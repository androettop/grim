//! Loading a map into the VM and running its startup sequence.

use std::collections::BTreeMap;
use std::path::Path;

use grim_assets::{load_export, Asset};
use grim_object::{ObjectKey, Value};
use grim_package::ObjectRef;

use crate::vm::{Vm, VmError, VmErrorKind};

pub struct LoadedLevel {
    pub level: ObjectKey,
    pub actors: Vec<ObjectKey>,
}

/// Where a decal landed, in world space.
#[derive(Debug, Clone, Copy)]
pub struct Decal {
    pub at: [f32; 3],
    /// Normal of the surface it lies on.
    pub normal: [f32; 3],
}

/// A sequence of a mesh: its frame count, its rate in frames a second, and the script calls
/// it makes partway through.
#[derive(Debug, Clone)]
pub struct SeqInfo {
    pub frames: f32,
    pub rate: f32,
    pub notifies: Vec<(f32, grim_object::NameId)>,
}

/// Properties the engine owns: the save streams leave them out, and so do we.
const OWNED_BY_ENGINE: u32 = grim_object::types::cpf::NATIVE | grim_object::types::cpf::TRANSIENT;

/// The player and what it carries, between one level and the next.
#[derive(Default)]
pub struct Travelled {
    /// The player first, then the actors of its inventory in chain order.
    actors: Vec<TravelledActor>,
}

struct TravelledActor {
    /// Key in the level being left, to point at its new self what travelled naming it.
    key: ObjectKey,
    class: grim_object::ClassId,
    /// Name, index into a static array, and value of each `travel` property.
    props: Vec<(grim_object::NameId, u32, Value)>,
}

/// Steps one rotator component towards another by at most `step`, the short way round. Angles
/// wrap every 65536 units, so the difference is taken into a half turn either side.
///
/// A component that is turned comes out between 0 and 65535, as the original leaves it: its
/// saved games have pawns that walked their patrols facing yaw 40854 or 65526, never the
/// negative angles the same headings are asked for with. One already where it is wanted is
/// left as it is, however it is written (a roll of 65536 survives in them too).
fn turn_towards_unit(from: i32, to: i32, step: i32) -> i32 {
    if from == to {
        return from;
    }
    let mut delta = (to.wrapping_sub(from)) & 0xFFFF;
    if delta > 0x8000 {
        delta -= 0x10000;
    }
    if delta.abs() <= step.abs() || step == 0 {
        return to & 0xFFFF;
    }
    from.wrapping_add(step.abs() * delta.signum()) & 0xFFFF
}

/// One component of a rotator value, or zero.
fn as_int(v: Option<&Value>) -> i32 {
    match v {
        Some(Value::Int(i)) => *i,
        _ => 0,
    }
}

/// Whether two collision cylinders overlap.
fn overlap(a: ([f32; 3], f32, f32), b: ([f32; 3], f32, f32)) -> bool {
    let ((pa, ra, ha), (pb, rb, hb)) = (a, b);
    if (pa[2] - pb[2]).abs() > ha + hb {
        return false;
    }
    let (dx, dy) = (pa[0] - pb[0], pa[1] - pb[1]);
    dx * dx + dy * dy <= (ra + rb) * (ra + rb)
}

/// What the actors that never change collide and block with, kept for the whole level.
#[derive(Default)]
pub struct StaticCollision {
    pub roles: Vec<(ObjectKey, Role)>,
    pub colliders: Vec<Collider>,
    pub blockers: Vec<Blocker>,
}

/// `ECollideType`: the primitives `Actor` says an actor collides with.
pub const CT_BOX: u8 = 2;
pub const CT_ALIGNED_OVAL: u8 = 4;
pub const CT_ORIENTED_OVAL: u8 = 5;

/// What an actor overlaps others with: its cylinder, or the turned box it is when it is a
/// `CT_Box` (`radius` along its facing, `width` across it).
#[derive(Debug, Clone, Copy)]
pub struct Collider {
    pub actor: ObjectKey,
    pub at: [f32; 3],
    pub radius: f32,
    pub height: f32,
    pub boxed: Option<(f32, f32)>,
}

impl Collider {
    fn shape(&self) -> Option<Shape> {
        let (width, yaw) = self.boxed?;
        Some(Shape::Box { centre: self.at, half: [self.radius, width, self.height], yaw })
    }

    /// Whether two colliders overlap. A box is tested against the other one's footprint as
    /// the square it fills; two cylinders against each other.
    fn overlaps(&self, other: &Collider) -> bool {
        let reach = |c: &Collider| match c.shape() {
            Some(s) => {
                let (low, high) = s.bounds();
                [(high[0] - low[0]) / 2.0, (high[1] - low[1]) / 2.0, c.height]
            }
            None => [c.radius, c.radius, c.height],
        };
        match (self.shape(), other.shape()) {
            (None, None) => overlap((self.at, self.radius, self.height), (other.at, other.radius, other.height)),
            (Some(s), _) => s.holds(other.at, reach(other)),
            (None, Some(s)) => s.holds(self.at, reach(self)),
        }
    }
}

/// `RF_NotForClient | RF_NotForServer`: objects only the editor loads.
const EDITOR_ONLY: u32 = 0x0010_0000 | 0x0020_0000;

/// One frame of player input: the keys held and how far the mouse moved, named the way the
/// game's own bindings name them.
#[derive(Debug, Clone, Copy, Default)]
pub struct PlayerInput<'a> {
    pub held: &'a [&'a str],
    pub mouse: (f32, f32),
}

/// A latent move the engine drives while the state code waits: `MoveTo`, `MoveToward`,
/// `StrafeTo`, `StrafeFacing`, `TurnTo`, `TurnToward` and `WaitForLanding` all leave one of
/// these behind, and the tick advances it until it is done.
#[derive(Debug, Clone, Copy)]
pub struct MoveOrder {
    /// Where to go; `None` is a move that only turns.
    pub dest: Option<[f32; 3]>,
    /// An actor to go to, followed while it moves itself.
    pub target: Option<ObjectKey>,
    /// What to face while going there; `None` faces the way it moves.
    pub focus: Option<[f32; 3]>,
    pub focus_actor: Option<ObjectKey>,
    /// Fraction of `GroundSpeed` to move at, as `DesiredSpeed` means in the scripts.
    pub speed: f32,
    /// Gives up when it runs out, which is how Unreal keeps a blocked move from hanging.
    pub timer: f32,
    /// `WaitForLanding`: waits for the fall to end instead of moving.
    pub landing: bool,
    /// How far it still had to go when it was last looked at. Unreal's `MoveTo` ends when the
    /// pawn stops getting anywhere, so a move that gains no ground is over.
    pub nearest: f32,
}

/// Something that stands in the way of a move.
#[derive(Clone)]
pub struct Blocker {
    pub actor: ObjectKey,
    pub role: Role,
    pub shape: Shape,
    /// The box the shape lives in, in world axes. A move is tested against this first, which
    /// is what keeps a level's worth of blockers from costing a full test each.
    pub bounds: ([f32; 3], [f32; 3]),
}

/// What an actor stops and is stopped by, from its `bBlockActors`, `bBlockPlayers` and whether
/// it is a player.
#[derive(Debug, Clone, Copy)]
pub struct Role {
    pub block_actors: bool,
    pub block_players: bool,
    pub player: bool,
    /// `bCollideActors`: one that does not collide with actors is stopped by none of them,
    /// only by the level and the movers that are part of it.
    pub collides: bool,
}

impl Role {
    /// What an actor the tick did not gather is: it does not collide with actors, or it would
    /// have been gathered.
    pub const NONE: Role = Role { block_actors: false, block_players: false, player: false, collides: false };

    /// Whether a mover of this role is stopped by an actor of that one. Both have to agree:
    /// the blocker has to block the kind of thing the mover is, and the mover the kind of
    /// thing the blocker is. Hypothesis, from what the game shows: the jellybeans a chest
    /// throws out (`bBlockActors` false) fall through the chest (`bBlockActors` true) to
    /// the floor, where they came to rest on its box; and a `BlockPlayer` only stops Harry.
    pub fn stopped_by(&self, other: &Role) -> bool {
        if !self.collides || !other.collides {
            return false;
        }
        let theirs = if self.player { other.block_players } else { other.block_actors };
        let ours = if other.player { self.block_players } else { self.block_actors };
        theirs && ours
    }
}

/// The shape an actor blocks with.
#[derive(Clone)]
pub enum Shape {
    /// A box, turned about its up axis.
    Box { centre: [f32; 3], half: [f32; 3], yaw: f32 },
    /// A mover's own geometry, in the frame it is drawn in, with the box that geometry fills
    /// in its own axes.
    Brush {
        brush: ObjectKey,
        bsp: std::sync::Arc<grim_world::Bsp>,
        at: [f32; 3],
        axes: [[f32; 3]; 3],
        pivot: [f32; 3],
        local: ([f32; 3], [f32; 3]),
    },
}

impl Shape {
    /// Where a moving box meets this shape, and the surface it meets, in world axes.
    pub fn hit(&self, from: [f32; 3], to: [f32; 3], extent: [f32; 3]) -> Option<(f32, [f32; 3])> {
        match self {
            Shape::Box { centre, half, yaw } => grim_world::box_check_turned(from, to, *centre, *half, extent, *yaw),
            Shape::Brush { bsp, at, axes, pivot, .. } => {
                let into = |w: [f32; 3]| {
                    let d = grim_world::sub(w, *at);
                    [0, 1, 2].map(|i| grim_world::dot(d, axes[i]) + pivot[i])
                };
                let hit = bsp.box_check(into(from), into(to), extent)?;
                let n = hit.normal;
                let mut world = [0.0f32; 3];
                for c in 0..3 {
                    world[c] = axes[0][c] * n[0] + axes[1][c] * n[1] + axes[2][c] * n[2];
                }
                Some((hit.time, world))
            }
        }
    }

    /// The box this shape fills, in world axes.
    pub fn bounds(&self) -> ([f32; 3], [f32; 3]) {
        match self {
            Shape::Box { centre, half, yaw } => {
                let (sin, cos) = yaw.sin_cos();
                let reach = [
                    half[0] * cos.abs() + half[1] * sin.abs(),
                    half[0] * sin.abs() + half[1] * cos.abs(),
                    half[2],
                ];
                ([0, 1, 2].map(|i| centre[i] - reach[i]), [0, 1, 2].map(|i| centre[i] + reach[i]))
            }
            Shape::Brush { at, axes, pivot, local, .. } => {
                let (centre, half) = *local;
                let mut low = [f32::MAX; 3];
                let mut high = [f32::MIN; 3];
                for corner in 0..8 {
                    let sign = [corner & 1, (corner >> 1) & 1, (corner >> 2) & 1].map(|b| if b == 0 { -1.0 } else { 1.0 });
                    let point = [0, 1, 2].map(|i| centre[i] + half[i] * sign[i] - pivot[i]);
                    for c in 0..3 {
                        let world = at[c] + axes[0][c] * point[0] + axes[1][c] * point[1] + axes[2][c] * point[2];
                        low[c] = low[c].min(world);
                        high[c] = high[c].max(world);
                    }
                }
                (low, high)
            }
        }
    }

    /// Whether a box at a place is inside this shape.
    pub fn holds(&self, at: [f32; 3], extent: [f32; 3]) -> bool {
        match self {
            Shape::Box { centre, half, yaw } => {
                let d = [at[0] - centre[0], at[1] - centre[1]];
                let (sin, cos) = (-yaw).sin_cos();
                let local = [d[0] * cos - d[1] * sin, d[0] * sin + d[1] * cos, at[2] - centre[2]];
                (0..3).all(|i| local[i].abs() < half[i] + extent[i])
            }
            Shape::Brush { bsp, at: place, axes, pivot, .. } => {
                let d = grim_world::sub(at, *place);
                let local = [0, 1, 2].map(|i| grim_world::dot(d, axes[i]) + pivot[i]);
                bsp.box_solid(local, extent)
            }
        }
    }
}

/// An animation channel: an actor that poses part of its owner's skeleton.
#[derive(Debug, Clone, Copy)]
pub struct Channel {
    pub actor: ObjectKey,
    /// `EAnimType`: 0 replaces what the body's animation does with those bones, 1 combines.
    pub kind: u8,
    /// The bone it takes over, along with everything hanging from it.
    pub bone: grim_object::NameId,
    /// Whether it is posing those bones at the moment. A channel takes them over when it is
    /// given an animation of its own, and lets go of them when the body plays one that
    /// replaces what the channels do. Hypothesis: that is what `EAnimType` decides, and it is
    /// how Harry stops holding the casting pose once he stops casting: the channel keeps
    /// looping `CastAim`, and the body's next `AT_Replace` animation is what ends it.
    pub posing: bool,
}

/// Failures collected while running scripts, grouped for reporting.
#[derive(Default)]
pub struct Report {
    pub missing_natives: BTreeMap<String, usize>,
    /// Script error message → (count, example with trace).
    pub errors: BTreeMap<String, (usize, String)>,
    pub calls: usize,
}

impl Report {
    pub fn record(&mut self, e: VmError) {
        match &e.kind {
            VmErrorKind::MissingNative(n) => *self.missing_natives.entry(n.clone()).or_default() += 1,
            VmErrorKind::Script(m) => {
                let entry = self.errors.entry(m.clone()).or_insert((0, e.to_string()));
                entry.0 += 1;
            }
        }
    }
}

impl Vm {
    pub fn load_level(&mut self, path: &Path) -> Result<LoadedLevel, String> {
        let pkg = self.world.lib.load_path(path)?;
        let pid = self.world.pkg_id(&pkg);
        let idx = (0..pkg.exports.len() as u32)
            .find(|&i| pkg.class_name(ObjectRef::Export(i)) == "Level")
            .ok_or("no Level export")?;
        let Asset::Level(level) = load_export(&pkg, idx).map_err(|e| e.to_string())?.asset else {
            return Err("Level export is not a Level".into());
        };
        if let ObjectRef::Export(m) = level.model {
            if let Asset::Model(model) = load_export(&pkg, m).map_err(|e| e.to_string())?.asset {
                self.surfaces = model
                    .surfs
                    .iter()
                    .map(|s| (self.world.resolve(&pkg, s.texture).ok().flatten(), s.poly_flags))
                    .collect();
                self.bsp = Some(grim_world::Bsp::new(&model));
                self.zone_actors = model
                    .zones
                    .iter()
                    .map(|z| match z.zone_actor {
                        ObjectRef::Export(e) => Some(ObjectKey { package: pid, export: e }),
                        _ => None,
                    })
                    .collect();
            }
        }
        let actors = level
            .actors
            .iter()
            .filter_map(|r| match r {
                // Actors the editor keeps to itself are marked as not for the client or the
                // server, and the game never loads them.
                ObjectRef::Export(e) if pkg.exports[*e as usize].flags & EDITOR_ONLY == 0 => {
                    Some(ObjectKey { package: pid, export: *e })
                }
                _ => None,
            })
            .collect();
        let level_key = ObjectKey { package: pid, export: idx };
        self.actors = actors;
        self.level = Some(level_key);
        // The first actor slot holds the LevelInfo.
        self.level_info = self.actors.first().copied();
        let info = self.level_info;
        for a in self.actors.clone() {
            self.set_prop(a, "Level", Value::Object(info)).map_err(|e| e.to_string())?;
            self.set_prop(a, "XLevel", Value::Object(Some(level_key))).map_err(|e| e.to_string())?;
        }
        // The game keeps the map's file name on the level info, and its own code reads it from
        // there to name a save. A saved game brings it along, so it is only filled in when empty.
        if let Some(info) = info {
            if matches!(self.get_prop(info, "LevelEnterText"), Ok(Value::Str(s)) if s.is_empty()) {
                let name = format!("{}.unr", self.world.package(level_key.package).name);
                self.set_prop(info, "LevelEnterText", Value::Str(name)).map_err(|e| e.to_string())?;
            }
        }
        self.spawn_game_info().map_err(|e| e.to_string())?;
        Ok(LoadedLevel { level: level_key, actors: self.actors.clone() })
    }

    /// Creates the level's GameInfo (LevelInfo.DefaultGameType, else the ini's DefaultGame),
    /// then calls `InitGame`, before any actor begins play.
    fn spawn_game_info(&mut self) -> crate::VmResult<()> {
        let Some(info) = self.level_info else { return Ok(()) };
        // A saved game brings its own, already set up: `LevelInfo.Game` points at it, with the
        // replication info and player count the save had. Making another one lost them, and
        // the player logged into it came out with no `GameReplicationInfo`.
        if let Value::Object(Some(game)) = self.get_prop(info, "Game")? {
            if !self.is_deleted(game) {
                return Ok(());
            }
        }
        let class_key = match self.get_prop(info, "DefaultGameType")? {
            Value::Object(Some(k)) => Some(k),
            _ => {
                let name = self.config.ini.get("Engine.Engine", "DefaultGame").unwrap_or("Engine.GameInfo").to_string();
                match self.import_text(&grim_object::PropType::Class(None), &name) {
                    Some(Value::Object(k)) => k,
                    _ => None,
                }
            }
        };
        let Some(class_key) = class_key else { return crate::vm::fail("no game class") };
        let class = self.world.link_class(class_key).map_err(|e| crate::VmError::script(e.0))?;
        let game = self.new_object(class, None);
        self.set_prop(game, "Level", Value::Object(Some(info)))?;
        self.set_prop(game, "XLevel", Value::Object(self.level))?;
        self.actors.push(game);
        self.set_prop(info, "Game", Value::Object(Some(game)))?;
        self.call_event(game, "InitGame", vec![Value::Str(String::new()), Value::Str(String::new())])?;
        Ok(())
    }

    /// UE1 level startup: each phase runs over every actor before the next one starts.
    pub fn begin_play(&mut self, level: &LoadedLevel, report: &mut Report) {
        // Who the player is has to be known before anything begins play: the maps place Harry
        // themselves, and `HPawn.PreBeginPlay` reads `LevelInfo.PlayerHarryActor` to find him
        // (it logs "No Harry in Map!" when it is empty). No script ever assigns it.
        if let (Some(info), Some(harry)) = (self.level_info, self.harry_of(level)) {
            if let Err(e) = self.set_prop(info, "PlayerHarryActor", Value::Object(Some(harry))) {
                report.record(e);
            }
        }
        // A level that has already begun play is a saved game: `LevelInfo.bBegunPlay` is set
        // and every actor carries the state it was in. Unreal starts play only once — a saved
        // level is picked up where it was left, not started again. Starting it again ran every
        // actor's `PreBeginPlay` a second time, which is where `Pawn.Skill += Difficulty` came
        // out doubled on all 183 pawns of the first checkpoint, and sent every jellybean back
        // into the bounce it had finished long before the game was saved.
        let begun = self.level_info.is_some_and(|i| matches!(self.get_prop(i, "bBegunPlay"), Ok(Value::Bool(true))));
        if begun {
            for &a in &level.actors {
                if self.is_deleted(a) {
                    continue;
                }
                if let Err(e) = self.resume_state(a) {
                    report.record(e);
                }
            }
        } else {
            for event in ["PreBeginPlay", "BeginPlay", "PostBeginPlay", "SetInitialState"] {
                for &a in &level.actors {
                    if self.is_deleted(a) {
                        continue;
                    }
                    report.calls += 1;
                    if let Err(e) = self.call_event(a, event, vec![]) {
                        report.record(e);
                    }
                }
            }
            if let Some(info) = self.level_info {
                if let Err(e) = self.set_prop(info, "bBegunPlay", Value::Bool(true)) {
                    report.record(e);
                }
            }
        }
        // The player comes last: `GameInfo.Login` hands back a pawn the level already holds,
        // and it only finds one once the pawns have begun play and put themselves on the
        // level's list. Logging in before that made the game spawn a pawn of its own instead of
        // taking over the Harry the map places.
        self.attach_by_tag(report);
        self.login(report);
    }

    /// Puts actors on what their `AttachTag` names. That property of `Actor` is this game's own
    /// (the `Mover` one of the same name works the other way round, and `Mover.PostBeginPlay`
    /// reads it), and no script reads it: `StatueGregorySmarmy0` of `EntryHall_hub` names
    /// `StatueBase01` with it, the tag of `Mover33`, the plinth a Flipendo pushes back.
    /// Hypothesis: the engine bases the actor on the one so tagged as the level begins, so it
    /// rides with it; left where it was, the statue hung in the air as its plinth slid away.
    fn attach_by_tag(&mut self, report: &mut Report) {
        let none = self.world.names.intern("None");
        for a in self.actors.clone() {
            if self.is_deleted(a) || self.is_mover(a) {
                continue;
            }
            let Ok(Value::Name(tag)) = self.get_prop(a, "AttachTag") else { continue };
            if tag == none || self.world.names.str(tag).is_empty() {
                continue;
            }
            let base = self.actors.clone().into_iter().find(|&b| {
                b != a && !self.is_deleted(b) && matches!(self.get_prop(b, "Tag"), Ok(Value::Name(t)) if t == tag)
            });
            if let Some(base) = base {
                if let Err(e) = self.set_base(a, Some(base)) {
                    report.record(e);
                }
            }
        }
    }

    /// Puts an actor of a saved level back in the state it was saved in: the state it was in,
    /// where its state code had got to, and the latent function that code was waiting on.
    /// Unreal writes all three in front of the object's properties (the `StateFrame`), and
    /// reads them back instead of running the object's start-up again.
    fn resume_state(&mut self, a: ObjectKey) -> crate::VmResult<()> {
        // Only what was read from the package has a frame to read back.
        if a.package == crate::vm::TRANSIENT || a.package == grim_object::INTRINSIC {
            return Ok(());
        }
        let pkg = self.world.package(a.package).clone();
        let Ok(loaded) = grim_assets::load_export(&pkg, a.export) else { return Ok(()) };
        let Some(frame) = loaded.base.state_frame else { return Ok(()) };
        // The state it is in. A frame whose state is the class itself is in no state at all.
        let state = match self.world.resolve(&pkg, frame.state_node) {
            Ok(Some(k)) => self.world.link_state(k).ok(),
            _ => None,
        };
        // Where its state code is: the node whose code runs (the state a label was found in,
        // which may be above its own) and the byte offset in that code.
        let mut thread = None;
        if let (Some(offset), Ok(Some(k))) = (frame.code_offset, self.world.resolve(&pkg, frame.node)) {
            if offset >= 0 {
                if let Ok(node) = self.world.link_state(k) {
                    let code = self.world.state(node).code.clone();
                    match code.offsets.get(&(offset as u32)).copied() {
                        Some(pc) => thread = Some(crate::vm::StateThread { node, code, pc, latent: None }),
                        None => self.warn(format!("saved state code of {} resumes at {offset}, where no statement starts", self.world.path(k))),
                    }
                }
            }
        }
        // What that code was waiting on. The frame keeps a number for it, but not the
        // native's own: measured on the first checkpoint, 384 follows a `Sleep` (native 256)
        // and 385 a `FinishAnim` (native 261). What it waits on is read instead from the call
        // it stopped at, the statement just before the one it resumes at, which names itself.
        if let Some(t) = thread.as_mut() {
            if frame.latent_action != 0 {
                let call = t.pc.checked_sub(1).and_then(|i| t.code.statements.get(i)).map(|st| st.expr.clone());
                let name = match call {
                    Some(grim_assets::bytecode::Expr::Native { index, .. }) => {
                        self.world.functions.iter().find(|f| f.native == index).map(|f| self.world.names.str(f.name).to_string())
                    }
                    Some(grim_assets::bytecode::Expr::FinalFunction { function, .. }) => {
                        let owner = self.world.package(t.code.package.unwrap_or(a.package)).clone();
                        Some(owner.object_name(function).to_string())
                    }
                    _ => None,
                }
                .unwrap_or_default();
                t.latent = match name.as_str() {
                    // `Sleep` counts down in `LatentFloat`, which the save keeps.
                    "Sleep" => {
                        let left = match self.get_prop(a, "LatentFloat") {
                            Ok(Value::Float(v)) => v,
                            _ => 0.0,
                        };
                        Some(crate::vm::Latent::Sleep(left))
                    }
                    "FinishAnim" => Some(crate::vm::Latent::Wait("bAnimFinished", false)),
                    "FinishInterpolation" => Some(crate::vm::Latent::Wait("bInterpolating", true)),
                    // A move picks up again from what the pawn keeps of it: the movement
                    // natives leave where it is going in `Destination`, what it faces in
                    // `Focus` and whom it follows in `MoveTarget`, and the save has all three.
                    "MoveTo" | "StrafeTo" | "StrafeFacing" | "MoveToward" | "TurnTo" | "TurnToward" | "WaitForLanding" => {
                        self.resumed_move(a, &name)?;
                        Some(crate::vm::Latent::Move)
                    }
                    other => {
                        let class = self.class_of(a).map(|c| self.world.names.str(self.world.class(c).name).to_string()).unwrap_or_default();
                        let where_ = self.world.path(self.world.state(t.node).key);
                        self.warn(format!("saved state code of a {class} in {where_} waits on '{other}' ({}), which is not resumed", frame.latent_action));
                        None
                    }
                };
            }
        }
        let inst = self.instance(a)?;
        inst.state = state;
        inst.thread = thread;
        Ok(())
    }

    /// Gives a pawn whose saved state code waits on a move that move again.
    fn resumed_move(&mut self, a: ObjectKey, latent: &str) -> crate::VmResult<()> {
        let point = |vm: &mut Self, prop: &str| vm.get_prop(a, prop).ok().and_then(|v| crate::vm::floats3(&v).ok());
        let target = match self.get_prop(a, "MoveTarget")? {
            Value::Object(Some(t)) if !self.is_deleted(t) => Some(t),
            _ => None,
        };
        let mut order = crate::natives::engine::order();
        match latent {
            "MoveTo" => order.dest = point(self, "Destination"),
            "StrafeTo" => (order.dest, order.focus) = (point(self, "Destination"), point(self, "Focus")),
            "StrafeFacing" => (order.dest, order.focus_actor) = (point(self, "Destination"), target),
            "MoveToward" => order.target = target,
            "TurnTo" => order.focus = point(self, "Focus"),
            "TurnToward" => order.focus_actor = target,
            _ => order.landing = true,
        }
        let order = crate::natives::engine::timed(self, a, order)?;
        self.start_move(a, order);
        Ok(())
    }

    /// The `harry` the map places, which is the actor the player takes over.
    fn harry_of(&mut self, level: &LoadedLevel) -> Option<ObjectKey> {
        let harry = self.class_named("harry")?;
        level.actors.iter().copied().find(|&a| self.class_of(a).is_ok_and(|c| self.world.is_child_of(c, harry)))
    }

    /// Logs a player in, like the engine does after the level begins play: `GameInfo.Login`
    /// creates the player pawn, then `PostLogin` and `Possess` run.
    fn login(&mut self, report: &mut Report) -> Option<ObjectKey> {
        let info = self.level_info?;
        let Ok(Value::Object(Some(game))) = self.get_prop(info, "Game") else { return None };
        let class_name = self.config.ini.get("URL", "Class").unwrap_or("Engine.PlayerPawn").to_string();
        let spawn_class = match self.import_text(&grim_object::PropType::Class(None), &class_name) {
            Some(v) => v,
            None => Value::Object(None),
        };
        let args = vec![Value::Str(String::new()), Value::Str(String::new()), Value::Str(String::new()), spawn_class];
        let player = match self.call_event(game, "Login", args) {
            Ok(Some(Value::Object(p))) => p,
            Ok(_) => None,
            Err(e) => {
                report.record(e);
                None
            }
        };
        // `LevelInfo.PlayerHarryActor` is this game's own addition to the level: no script ever
        // assigns it, and scripts all over read it to reach the player (`HPawn.PreBeginPlay`
        // logs "No Harry in Map!" when it is empty). Hypothesis: the engine fills it in with
        // the pawn it just logged in, which is the only thing that fits every reader.
        if let Some(p) = player {
            if let Err(e) = self.set_prop(info, "PlayerHarryActor", Value::Object(Some(p))) {
                report.record(e);
            }
        }
        // The engine starts the player it just logged in before telling the game about it:
        // `RestartPlayer` is what hands out the default inventory, the wand among it.
        if let Some(p) = player {
            if let Err(e) = self.call_event(game, "RestartPlayer", vec![Value::Object(Some(p))]) {
                report.record(e);
            }
        }
        if let Some(p) = player {
            for event in ["PostLogin", "Possess"] {
                let (target, args) = if event == "PostLogin" { (game, vec![Value::Object(Some(p))]) } else { (p, vec![]) };
                if let Err(e) = self.call_event(target, event, args) {
                    report.record(e);
                }
            }
        }
        self.player = player;
        if let Some(p) = player {
            if let Err(e) = self.attach_viewport(p) {
                report.record(e);
            }
            if let Err(e) = self.set_default_game_state(p) {
                report.record(e);
            }
            self.screen_actors_by_game_state(report);
        }
        player
    }

    /// What the player keeps when the game changes level: the properties the scripts mark as
    /// `travel`, which is where this game holds its state (the game state, health, the spell
    /// book, the cards), for the player and for every actor hanging off its inventory. This is
    /// UE1's `TRAVEL_Items`: the level is thrown away and the player is rebuilt with what it
    /// carried in the next one.
    pub fn travel_properties(&mut self) -> Travelled {
        let Some(player) = self.player else { return Travelled::default() };
        let mut keys = vec![player];
        // The inventory is a chain, each item naming the next. A chain that loops back on
        // itself would never end, so an item is only taken once.
        let mut next = self.inventory_of(player);
        while let Some(item) = next {
            if keys.contains(&item) {
                break;
            }
            keys.push(item);
            next = self.inventory_of(item);
        }
        let actors = keys
            .into_iter()
            .filter_map(|key| {
                let class = self.class_of(key).ok()?;
                let props = self.travel_values(key, class);
                Some(TravelledActor { key, class, props })
            })
            .collect();
        Travelled { actors }
    }

    fn inventory_of(&mut self, actor: ObjectKey) -> Option<ObjectKey> {
        match self.get_prop(actor, "Inventory") {
            Ok(Value::Object(next)) => next,
            _ => None,
        }
    }

    /// Every slot of every property a class marks `travel`. The spell book, the cards and the
    /// status are static arrays, so an element at a time is the only way to carry them whole.
    fn travel_values(&mut self, actor: ObjectKey, class: grim_object::ClassId) -> Vec<(grim_object::NameId, u32, Value)> {
        let props: Vec<_> = self
            .world
            .class(class)
            .props
            .iter()
            .filter(|p| p.flags & grim_object::types::cpf::TRAVEL != 0)
            .map(|p| (p.name, p.slot, p.array_dim))
            .collect();
        let mut out = Vec::new();
        for (name, slot, dim) in props {
            for i in 0..dim {
                let Ok(instance) = self.instance(actor) else { continue };
                if let Some(v) = instance.values.get((slot + i) as usize) {
                    out.push((name, i, v.clone()));
                }
            }
        }
        out
    }

    /// Points a value that travelled at the new level: a reference to something that travelled
    /// too becomes its new self, one to anything else in the old level is dropped, and one to an
    /// object of a package (a class, a texture, a sound) stands as it is, since packages outlive
    /// the level.
    fn rebind(&self, value: Value, map: &std::collections::HashMap<ObjectKey, ObjectKey>) -> Value {
        match value {
            Value::Object(Some(key)) if key.package == crate::vm::TRANSIENT => Value::Object(map.get(&key).copied()),
            Value::Struct(fields) => Value::Struct(fields.into_iter().map(|v| self.rebind(v, map)).collect()),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.rebind(v, map)).collect()),
            v => v,
        }
    }

    /// Rebuilds in the level just loaded what travelled into it: the player's own state, and an
    /// actor per item it carried, each told it arrived with `TravelPreAccept` once every one of
    /// them holds its state. `Inventory.TravelPreAccept` gives the item back to its owner, which
    /// is what links the inventory chain again.
    pub fn restore_travel_properties(&mut self, carried: Travelled, report: &mut Report) {
        let Some(player) = self.player else { return };
        let Some((first, items)) = carried.actors.split_first() else { return };
        let mut map = std::collections::HashMap::new();
        map.insert(first.key, player);
        let mut spawned = Vec::new();
        for item in items {
            match crate::natives::engine::spawn(self, player, item.class, Some(player), None, None, None) {
                Ok(Some(actor)) => {
                    map.insert(item.key, actor);
                    spawned.push(actor);
                }
                Ok(None) => {}
                Err(e) => report.record(e),
            }
        }
        for (actor, carried) in std::iter::once((player, first)).chain(spawned.iter().copied().zip(items)) {
            for (name, index, value) in carried.props.clone() {
                let value = self.rebind(value, &map);
                if let Err(e) = self.set_prop_id_at(actor, name, index, value) {
                    report.record(e);
                }
            }
        }
        for actor in &spawned {
            if let Err(e) = self.call_event(*actor, "TravelPreAccept", Vec::new()) {
                report.record(e);
            }
        }
        self.travelled = spawned;
        self.screen_actors_by_game_state(report);
    }

    /// The name of the level the VM is running, as a `_pa` stream spells it. The game keeps it in
    /// `LevelInfo.LevelEnterText`, which is what a saved game carries: the package of a save is
    /// called `Save0`, and the level inside it is still the map it was saved in.
    pub fn level_name(&mut self) -> String {
        if let Some(info) = self.level_info {
            if let Ok(Value::Str(name)) = self.get_prop(info, "LevelEnterText") {
                if !name.is_empty() {
                    return name;
                }
            }
        }
        match self.level {
            Some(level) => format!("{}.unr", self.world.package(level.package).name),
            None => String::new(),
        }
    }

    /// The actors this level keeps between visits, in the shape a save slot holds them: those
    /// with `bPersistent`, with every property the stream carries. An actor that was destroyed is
    /// simply not in the list, which is how the game remembers a jellybean was collected.
    pub fn persistent_actors(&mut self) -> grim_save::Level {
        let mut actors = Vec::new();
        for a in self.actors.clone() {
            if self.is_deleted(a) || !matches!(self.get_prop(a, "bPersistent"), Ok(Value::Bool(true))) {
                continue;
            }
            let Ok(class) = self.class_of(a) else { continue };
            let props = self.world.class(class).props.clone();
            let mut saved = Vec::new();
            for p in &props {
                if p.flags & OWNED_BY_ENGINE != 0 || matches!(p.ty, grim_object::PropType::Array(_)) {
                    continue;
                }
                for i in 0..p.array_dim {
                    let Ok(instance) = self.instance(a) else { continue };
                    let Some(value) = instance.values.get((p.slot + i) as usize).cloned() else { continue };
                    if let Some(value) = self.saved_value(&p.ty, &value) {
                        saved.push(grim_save::Prop { name: self.world.names.str(p.name).to_string(), index: i, value });
                    }
                }
            }
            let class_name = self.world.names.str(self.world.class(class).name).to_string();
            actors.push(grim_save::Actor { name: self.object_name(a), class: class_name, props: saved });
        }
        grim_save::Level { name: self.level_name(), unknown: [-1, 0, 0], actors }
    }

    /// One value as the stream writes it. Object and class references are recorded without what
    /// they point at, which is all the format keeps of them.
    fn saved_value(&self, ty: &grim_object::PropType, value: &Value) -> Option<grim_save::Saved> {
        use grim_object::PropType;
        use grim_save::Saved;
        Some(match (ty, value) {
            (PropType::Byte(_), Value::Byte(b)) => Saved::Byte(*b),
            (PropType::Int, Value::Int(i)) => Saved::Int(*i),
            (PropType::Bool, Value::Bool(b)) => Saved::Bool(*b),
            (PropType::Float, Value::Float(f)) => Saved::Float(*f),
            (PropType::Object(_), _) => Saved::Object,
            (PropType::Class(_), _) => Saved::Class,
            (PropType::Name, Value::Name(n)) => Saved::Name(self.world.names.str(*n).to_string()),
            (PropType::Str, Value::Str(s)) => Saved::Str { text: s.clone(), unicode: false },
            (PropType::Struct(id), Value::Struct(values)) => {
                let def = &self.world.structs[id.0 as usize];
                let mut fields = Vec::new();
                let mut at = 0;
                for f in &def.fields {
                    for _ in 0..f.array_dim {
                        fields.push(self.saved_value(&f.ty, values.get(at)?)?);
                        at += 1;
                    }
                }
                Saved::Struct { ty: self.world.names.str(def.name).to_string(), fields }
            }
            _ => return None,
        })
    }

    /// Keeps a level the way it was left, so that coming back to it finds it that way. The game
    /// writes this to the save slot on every level change; ours is held for the session until
    /// there is a slot to write into.
    pub fn remember_level(&mut self, level: grim_save::Level) {
        self.visited.insert(level.name.to_ascii_lowercase(), level);
    }

    /// Puts back what an earlier visit left behind, before the level begins play: every
    /// persistent actor the stream names gets its state, and a persistent actor it does not name
    /// was destroyed and is destroyed again without ever starting.
    pub fn restore_persistent_actors(&mut self, loaded: &LoadedLevel, report: &mut Report) {
        let key = self.level_name().to_ascii_lowercase();
        let Some(stream) = self.visited.get(&key).cloned() else { return };
        let mut by_name: BTreeMap<String, ObjectKey> = BTreeMap::new();
        for &a in &loaded.actors {
            by_name.insert(self.object_name(a).to_ascii_lowercase(), a);
        }
        let mut named = Vec::new();
        for record in &stream.actors {
            let Some(&actor) = by_name.get(&record.name.to_ascii_lowercase()) else {
                report.record(VmError::script(format!("{}: the level has no actor {}", stream.name, record.name)));
                continue;
            };
            named.push(actor);
            self.apply_record(actor, record, report);
        }
        for &a in &loaded.actors {
            if named.contains(&a) || self.is_deleted(a) {
                continue;
            }
            if matches!(self.get_prop(a, "bPersistent"), Ok(Value::Bool(true))) {
                let _ = self.set_prop(a, "bDeleteMe", Value::Bool(true));
                if let Ok(instance) = self.instance(a) {
                    instance.deleted = true;
                }
            }
        }
    }

    fn apply_record(&mut self, actor: ObjectKey, record: &grim_save::Actor, report: &mut Report) {
        let Ok(class) = self.class_of(actor) else { return };
        let class_name = self.world.names.str(self.world.class(class).name).to_string();
        if !class_name.eq_ignore_ascii_case(&record.class) {
            report.record(VmError::script(format!("{} was saved as {} and is a {class_name}", record.name, record.class)));
            return;
        }
        for prop in &record.props {
            let name = self.world.names.intern(&prop.name);
            let Some(def) = self.world.class(class).prop(name).cloned() else {
                report.record(VmError::script(format!("{}: {} has no property {}", record.name, class_name, prop.name)));
                continue;
            };
            if prop.index >= def.array_dim {
                report.record(VmError::script(format!("{}: {}[{}] is out of its {} slots", record.name, prop.name, prop.index, def.array_dim)));
                continue;
            }
            let slot = (def.slot + prop.index) as usize;
            let Ok(instance) = self.instance(actor) else { continue };
            let Some(current) = instance.values.get(slot).cloned() else { continue };
            if let Some(value) = self.restored_value(&def.ty, &prop.value, &current) {
                if let Ok(instance) = self.instance(actor) {
                    instance.values[slot] = value;
                }
            }
        }
    }

    /// The value to put back, or `None` to leave what the level itself put there: what the stream
    /// keeps of an object or a class reference is only that the property was there.
    fn restored_value(&mut self, ty: &grim_object::PropType, saved: &grim_save::Saved, current: &Value) -> Option<Value> {
        use grim_object::PropType;
        use grim_save::Saved;
        Some(match (ty, saved) {
            (PropType::Byte(_), Saved::Byte(b)) => Value::Byte(*b),
            (PropType::Int, Saved::Int(i)) => Value::Int(*i),
            (PropType::Bool, Saved::Bool(b)) => Value::Bool(*b),
            (PropType::Float, Saved::Float(f)) => Value::Float(*f),
            (PropType::Object(_) | PropType::Class(_), Saved::Object | Saved::Class) => return None,
            (PropType::Name, Saved::Name(n)) => Value::Name(self.world.names.intern(n)),
            (PropType::Str, Saved::Str { text, .. }) => Value::Str(text.clone()),
            (PropType::Struct(id), Saved::Struct { fields, .. }) => {
                let def = self.world.structs[id.0 as usize].fields.clone();
                let mut values = match current {
                    Value::Struct(v) => v.clone(),
                    _ => return None,
                };
                let mut at = 0;
                for f in &def {
                    for _ in 0..f.array_dim {
                        let (Some(saved), Some(current)) = (fields.get(at), values.get(at).cloned()) else { break };
                        if let Some(v) = self.restored_value(&f.ty, saved, &current) {
                            values[at] = v;
                        }
                        at += 1;
                    }
                }
                Value::Struct(values)
            }
            _ => return None,
        })
    }

    /// Tells the player and everything that travelled with it that they arrived. This is where
    /// the game builds what the level cannot hold for it: `harry.TravelPostAccept` spawns the
    /// wand when it finds itself with an empty inventory, and `Weapon.TravelPostAccept` gets
    /// its ammunition back.
    pub fn travel_post_accept(&mut self, report: &mut Report) {
        let Some(player) = self.player else { return };
        if let Err(e) = self.call_event(player, "TravelPostAccept", Vec::new()) {
            report.record(e);
        }
        for actor in std::mem::take(&mut self.travelled) {
            if self.is_deleted(actor) {
                continue;
            }
            if let Err(e) = self.call_event(actor, "TravelPostAccept", Vec::new()) {
                report.record(e);
            }
        }
    }

    /// Gives the player what the engine gives it: a viewport with the console class the ini
    /// names, and a canvas. The game's menus, HUD and text all go through them.
    fn attach_viewport(&mut self, player: ObjectKey) -> crate::VmResult<()> {
        // The scripts ask `Viewport(Player) != None` to tell a local player from a remote one,
        // and `Viewport` is a class the engine provides rather than the packages.
        let viewport = match self.spawn_engine_class("Engine.Player")? {
            Some(base) => {
                let class = self.world.native_class("Viewport", base);
                Some(self.new_object(class, None))
            }
            None => None,
        };
        let console_name = self.config.ini.get("Engine.Engine", "Console").unwrap_or("Engine.Console").to_string();
        let console = self.spawn_engine_object(&console_name)?;
        let canvas = self.spawn_engine_object("Engine.Canvas")?;
        if let (Some(viewport), Some(console)) = (viewport, console) {
            self.set_prop(viewport, "Actor", Value::Object(Some(player)))?;
            self.set_prop(viewport, "Console", Value::Object(Some(console)))?;
            self.set_prop(console, "Viewport", Value::Object(Some(viewport)))?;
            self.set_prop(player, "Player", Value::Object(Some(viewport)))?;
            self.call_event(console, "VideoChange", vec![])?;
        }
        if let (Some(canvas), Some(viewport)) = (canvas, viewport) {
            self.set_prop(canvas, "Viewport", Value::Object(Some(viewport)))?;
        }
        self.canvas = canvas;
        Ok(())
    }

    fn spawn_engine_object(&mut self, path: &str) -> crate::VmResult<Option<ObjectKey>> {
        Ok(self.spawn_engine_class(path)?.map(|class| self.new_object(class, None)))
    }

    fn spawn_engine_class(&mut self, path: &str) -> crate::VmResult<Option<grim_object::ClassId>> {
        let Some(Value::Object(Some(key))) = self.import_text(&grim_object::PropType::Class(None), path) else {
            return Ok(None);
        };
        Ok(Some(self.world.link_class(key).map_err(|e| crate::VmError::script(e.0))?))
    }

    /// Marks which actors take part in the game state the player is in, which the engine does
    /// rather than the scripts (`bInCurrentGameState` is "set by Engine"). An actor whose
    /// groups name states of the master list is in when any of them is the current one; one
    /// that names none is left as it is. Measured over the 2837 actors of an original save made
    /// in `GState115`: the 25 naming it are in, including a cauldron whose groups run from
    /// `GState070` to `GState115`; the 418 naming only others are out; of those naming no
    /// state, 2300 are in and 94 out, which a rule putting them all in cannot give.
    /// Actors are told about the change so they can put themselves away.
    pub fn screen_actors_by_game_state(&mut self, report: &mut Report) {
        let Some(player) = self.player else { return };
        let Ok(Value::Str(state)) = self.get_prop(player, "CurrentGameState") else { return };
        let Ok(Value::Str(list)) = self.get_prop(player, "GameStateMasterList") else { return };
        let tokens: Vec<String> = list.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        for a in self.actors.clone() {
            if self.is_deleted(a) {
                continue;
            }
            let groups = match self.get_prop(a, "Group") {
                Ok(Value::Name(n)) => self.world.names.str(n).to_string(),
                _ => String::new(),
            };
            let mut states = groups.split(',').map(str::trim).filter(|g| tokens.iter().any(|t| t.eq_ignore_ascii_case(g))).peekable();
            if states.peek().is_none() {
                continue;
            }
            let inside = states.any(|g| g.eq_ignore_ascii_case(&state));
            let was = matches!(self.get_prop(a, "bInCurrentGameState"), Ok(Value::Bool(true)));
            if was == inside {
                continue;
            }
            if let Err(e) = self.set_prop(a, "bInCurrentGameState", Value::Bool(inside)) {
                report.record(e);
                continue;
            }
            if let Err(e) = self.call_event(a, "OnResolveGameState", vec![]) {
                report.record(e);
            }
        }
    }

    /// The game advances through named game states, and actors check the current one to decide
    /// whether they take part. A level loaded without one leaves every actor active at once, so
    /// the first state of the player's master list is applied when none is set.
    fn set_default_game_state(&mut self, player: ObjectKey) -> crate::VmResult<()> {
        let current = match self.get_prop(player, "CurrentGameState")? {
            Value::Str(s) => s,
            _ => return Ok(()),
        };
        if !current.is_empty() && !current.eq_ignore_ascii_case("None") {
            return Ok(());
        }
        let Value::Str(list) = self.get_prop(player, "GameStateMasterList")? else { return Ok(()) };
        let Some(first) = list.split(',').next().map(str::to_string) else { return Ok(()) };
        if first.is_empty() {
            return Ok(());
        }
        self.call_event(player, "SetGameState", vec![Value::Str(first)])?;
        Ok(())
    }

    /// Falling actors, the only physics mode the engine simulates so far. Unreal drops
    /// `PHYS_Falling` actors (chests, pickups) onto the floor when the level starts.
    /// PHYS_Walking: the script sets `Acceleration`, the engine turns it into movement over
    /// the floor, keeps the actor on it and drops it when it walks off an edge.
    fn walk(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_WALKING: u8 = 1;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_WALKING)) {
            return Ok(());
        }
        // A latent move drives the actor itself, at the speed it was asked for.
        if self.moves.contains_key(&a) {
            return Ok(());
        }
        // The scripts give a direction; a pawn pushes along it with its own AccelRate.
        let mut accel = crate::vm::floats3(&self.get_prop_id(a, self.hot.acceleration)?)?;
        let size = (accel[0] * accel[0] + accel[1] * accel[1]).sqrt();
        // Below a unit there is nothing being asked for. The scripts let the direction they
        // hand over fade away rather than dropping it, so without this the actor reads a
        // millionth of a push as a push and walks on for as long as it takes to reach the wall.
        if size < 1.0 {
            accel = [0.0; 3];
        }
        let size = (accel[0] * accel[0] + accel[1] * accel[1]).sqrt();
        if size > 0.0 {
            let rate = match self.get_prop_id(a, self.hot.accel_rate)? {
                Value::Float(v) if v > 0.0 => v,
                _ => 500.0,
            };
            for v in accel.iter_mut().take(2) {
                *v *= rate / size;
            }
        }
        let mut velocity = crate::vm::floats3(&self.get_prop_id(a, self.hot.velocity)?)?;
        let mut ground_speed = match self.get_prop_id(a, self.hot.ground_speed)? {
            Value::Float(v) if v > 0.0 => v,
            _ => 320.0,
        };
        // Walking instead of running, which `bRun` asks for, covers a fraction of the ground.
        if matches!(self.get_prop_id(a, self.hot.b_run), Ok(Value::Byte(0))) {
            if let Ok(Value::Float(pct)) = self.get_prop_id(a, self.hot.walking_pct) {
                if pct > 0.0 {
                    ground_speed *= pct;
                }
            }
        }
        // Ground friction, as a zone property; the default matches Unreal's own.
        let friction = match self.get_prop_id(a, self.hot.ground_friction) {
            Ok(Value::Float(v)) if v > 0.0 => v,
            _ => 8.0,
        };
        // Hypothesis, from how the game feels against the original: Unreal turns the speed it
        // already carries towards the direction asked for, at the zone's friction, and pushes
        // along that direction on top; with nothing asked for it rubs the speed off at twice
        // that friction. The turning term is what makes a change of direction immediate
        // instead of a long curve, and the braking one what stops a walk where it is.
        let pushing = accel[0] * accel[0] + accel[1] * accel[1] > 1e-4;
        let speed_now = (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt();
        if pushing {
            let size = (accel[0] * accel[0] + accel[1] * accel[1]).sqrt();
            for i in 0..2 {
                let wanted = accel[i] / size * speed_now;
                velocity[i] += (wanted - velocity[i]) * (friction * dt).min(1.0);
                velocity[i] += accel[i] * dt;
            }
        } else {
            for v in velocity.iter_mut().take(2) {
                *v -= *v * (2.0 * friction * dt).min(1.0);
            }
            // Below a walking pace of its own there is nothing left to carry.
            if (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt() < 1.0 {
                velocity[0] = 0.0;
                velocity[1] = 0.0;
            }
        }
        velocity[2] = 0.0;
        let speed = (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt();
        if speed > ground_speed {
            for v in velocity.iter_mut().take(2) {
                *v *= ground_speed / speed;
            }
        }
        self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
        // Even standing still: what holds an actor up is the floor under it, and the step
        // looks for one. Without this a pawn left in the air never falls and never lands.
        self.step_walk(a, velocity, dt)
    }

    /// Moves a walking actor by `velocity` over `dt`: slides along what it bumps into, walks up
    /// small steps, stays on the floor going down them, and starts falling when there is none.
    fn step_walk(&mut self, a: ObjectKey, velocity: [f32; 3], dt: f32) -> crate::VmResult<()> {
        const PHYS_FALLING: u8 = 2;
        // How high a step the actor walks over instead of bumping into; the pawns carry it.
        let max_step = match self.get_prop_id(a, self.hot.max_step_height) {
            Ok(Value::Float(v)) if v > 0.0 => v,
            _ => 25.0,
        };
        let (radius, height) = match (self.get_prop_id(a, self.hot.collision_radius)?, self.get_prop_id(a, self.hot.collision_height)?) {
            (Value::Float(r), Value::Float(h)) => (r, h),
            _ => (0.0, 0.0),
        };
        let extent = [radius, radius, height];
        // Whatever put the actor inside the level, it leaves before it tries to walk.
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let from = self.unstuck(a, from, extent, max_step);
        let step = grim_world::scale(velocity, dt);
        let blockers = std::mem::take(&mut self.blockers);
        // Where it would not fit: inside the level, or inside something that stops it. A step
        // up tested against the level alone could lift a walker into a mover it stands on:
        // the fire crabs on `Mover21` in the original's save of `Adv3DungeonQuest` rose by
        // their whole step on the first tick, the search for the floor below starting inside
        // the brush and finding it straight away.
        let role = self.roles.get(&a).copied().unwrap_or(Role::NONE);
        let solid = |at: [f32; 3]| {
            self.bsp.as_ref().is_some_and(|b| b.box_solid(at, extent))
                || blockers.iter().any(|b| {
                    b.actor != a
                        && (matches!(b.shape, Shape::Brush { .. }) || role.stopped_by(&b.role))
                        && b.shape.holds(at, extent)
                })
        };
        // What it runs into is remembered, to be told about it once the move is done.
        let mut bumped = Vec::new();
        let mut moved = self.slide_bumping(a, from, step, extent, &blockers, &mut bumped);
        // Walking into something no higher than a step means walking up it: the same move is
        // tried again from a step higher, and the search for the floor below brings it back
        // down onto the stair. Without this a staircase is a wall.
        let covered = |p: [f32; 3]| ((p[0] - from[0]).powi(2) + (p[1] - from[1]).powi(2)).sqrt();
        let wanted = (step[0] * step[0] + step[1] * step[1]).sqrt();
        if wanted > 1e-3 && covered(moved) < wanted - 0.1 {
            let raised = grim_world::add(from, [0.0, 0.0, max_step]);
            if !solid(raised) {
                let over = self.slide_bumping(a, raised, step, extent, &blockers, &mut bumped);
                // Only if it ends somewhere it fits: a step up that lands inside the next
                // block buries the actor, and from there every trace starts in solid space.
                if covered(over) > covered(moved) + 0.1 && !solid(over) {
                    moved = over;
                }
            }
        }
        // Walking into something taller than a step is what starts a climb, once the step up
        // has been ruled out.
        let stopped = wanted > 1e-3 && covered(moved) < wanted - 0.1;
        let blocked_by = (stopped && self.switches.trace_move).then(|| (moved, bumped.first().copied()));
        // Walk up small steps, and stay on the floor when going down them. A walker reaches
        // down for the floor by the same step whether it is moving or not: Unreal keeps one
        // standing on what is within a step below it, and only what has nothing there falls.
        // Reaching no further than the feet made a player put down a few units above the floor
        // (the map's own start spots sit 8 units above it) fall on arrival, animation and all.
        let reach = max_step;
        // The search for the floor starts a step up so a rise is walked over rather than run
        // into, but only from somewhere the actor would fit: raising it into a ceiling would
        // find that as the floor and carry the actor up through it, a step every frame. Under
        // a low ceiling the whole step is not free, so it takes as much of it as is.
        let mut up = moved;
        // Only what is walking somewhere steps over anything: standing still, the floor is
        // looked for straight down, which is what the reach above already says.
        if wanted > 1e-4 {
            for part in [1.0, 0.75, 0.5, 0.25] {
                let raised = grim_world::add(moved, [0.0, 0.0, max_step * part]);
                if !solid(raised) {
                    up = raised;
                    break;
                }
            }
        }
        let down = grim_world::add(up, [0.0, 0.0, -(up[2] - moved[2]) - reach]);
        let floor = self.sweep(a, up, down, extent, &blockers);
        // What it is standing on. A mover carries whatever rides it, so it has to know.
        let standing_on = floor.and_then(|(_, _, on)| on);
        match floor {
            Some((time, floor_normal, _)) => {
                let mut landing = Self::stop_short(up, down, time);
                // A box sweep down past the edge of a step can miss the step's top and come to
                // rest on the floor under it, inside the step: at the top of `EntryHall_hub`'s
                // flight Harry's box, half over the last step, was put 16 units into it and
                // lifted back out the next frame, over and over. What leaves it inside the
                // level is corrected here, as the next frame would, before anything sees it.
                if self.inside_level(landing, extent) {
                    landing = self.free_above(landing, extent, max_step);
                }
                // A walker on the move keeps a little air under its feet: in the original's
                // saves Harry walking stands 2.50 units over the floor every time (13 saves of
                // the first twenty, on flat ground), where only resting against it would leave
                // the half unit the sweep stops short by.
                const FLOOR_GAP: f32 = 2.5;
                if wanted > 1e-4 {
                    let on_floor = grim_world::lerp(up, down, time);
                    let held = [landing[0], landing[1], on_floor[2] + FLOOR_GAP];
                    if held[2] > landing[2] && !solid(held) {
                        landing = held;
                    }
                }
                // Standing still, a walker stays where it is as long as there is a floor within
                // a step below it: Unreal does not pull it down onto one, it only asks that one
                // is there. The whomping willow of `Adv1Willow` is what shows it — the map puts
                // it at z -480.4 and the original's own save still has it there, 9.6 units
                // clear of the floor at -512, while being pulled down left it at -490.
                if wanted <= 1e-4 && moved[2] > landing[2] {
                    landing = moved;
                }
                // Up or down a step the walker is not lifted there at once. Unreal's walk moves
                // a pawn straight onto a step it can take, which shows as a jump on every stair;
                // this game's engine eases the height instead (as the user describes the
                // original). Only the height lags: the collision is on the step the whole time.
                // Hypothesis, the user's own reading of how it feels: the height goes up the way
                // the walker walks, reaching each step's height one step's length after meeting
                // it, so a flight is climbed in a straight line whatever its steps measure. The
                // length is the distance walked between the last two steps; on the first one,
                // with none measured yet, it closes in at the pawn's `GroundSpeed`.
                // Only a step is eased, never a slope: walking over hills and ramps the height
                // follows the ground as it goes. A step is a change of height more than the
                // ground under the walker explains — what its lean rises over the distance
                // walked — and more than the air the walker keeps under its feet.
                let walked = ((landing[0] - from[0]).powi(2) + (landing[1] - from[1]).powi(2)).sqrt();
                let lean = floor_normal[2].clamp(1e-3, 1.0);
                let slope_rise = walked * (1.0 - lean * lean).sqrt() / lean;
                let stepped = (landing[2] - from[2]).abs() > slope_rise + FLOOR_GAP;
                let before = self.stair_was.get(&a).copied().unwrap_or(0.0);
                let mut lag = if stepped { landing[2] - (from[2] - before) } else { before };
                let (since, rate) = self.stair_run.get(&a).copied().unwrap_or((f32::INFINITY, 0.0));
                let since = since + walked;
                // A step further from the last than twice the walker's width is taken as the
                // first of a new flight, not as one step's length. That limit is a judgement of
                // this engine's, not something the files give.
                let (since, rate) = if stepped {
                    (0.0, if since > 1.0 && since <= 4.0 * extent[0] { lag.abs() / since } else { 0.0 })
                } else {
                    (since, rate)
                };
                let close = if rate > 0.0 {
                    rate * walked
                } else {
                    match self.get_prop(a, "GroundSpeed") {
                        Ok(Value::Float(v)) if v > 0.0 => v * dt,
                        _ => 0.0,
                    }
                };
                if self.switches.trace_move && a == self.player.unwrap_or(a) {
                    eprintln!(
                        "stairs at {:.1},{:.1} from {:.1} to {:.1} gap {:.1} walked {:.1} since {since:.1} rate {rate:.3} close {close:.1}",
                        landing[0], landing[1], from[2], landing[2], lag, walked
                    );
                }
                lag -= lag.signum() * lag.abs().min(close);
                // More than a step apart is not a stair: the actor was put somewhere else.
                // `GRIM_NO_RAMP` leaves the steps as Unreal takes them, for comparing.
                if lag.abs() > max_step || self.switches.no_ramp {
                    lag = 0.0;
                }
                self.stair_run.insert(a, (since, rate));
                if lag.abs() > 0.01 {
                    self.stair_lag.insert(a, lag);
                } else {
                    self.stair_lag.remove(&a);
                }
                self.set_prop_id(a, self.hot.location, crate::vm::vector(landing[0], landing[1], landing[2] - lag))?;
            }
            None => {
                self.stair_lag.remove(&a);
                self.stair_run.remove(&a);
                self.set_prop_id(a, self.hot.location, crate::vm::vector(moved[0], moved[1], moved[2]))?;
                // Walking off the floor is what starts a fall; standing still over nothing is
                // not. Unreal looks for the floor as part of moving, so an actor that is not
                // going anywhere keeps whatever it has. The original's own save of
                // `Adv1Willow` is what says so: the flying Ford hangs at z 1049 with walking
                // physics and no floor within a thousand units, and the save still has it
                // walking and still has it there.
                if wanted > 1e-4 {
                    self.set_prop(a, "Physics", Value::Byte(PHYS_FALLING))?;
                }
            }
        }
        self.blockers = blockers;
        // A pawn that walks onto a mover rides it from then on, and one that walks off it
        // stops riding. `SetBase` is the game's own way of saying so, events and all.
        let was = match self.get_prop_id(a, self.hot.base) {
            Ok(Value::Object(o)) => o,
            _ => None,
        };
        if standing_on != was && (standing_on.is_some() || was.is_some_and(|w| self.is_mover(w))) {
            self.set_base(a, standing_on)?;
        }
        // What it ran into hears about it: a door that opens when somebody walks into it is
        // waiting for exactly that.
        for other in bumped {
            if self.is_deleted(a) || self.is_deleted(other) {
                continue;
            }
            self.call_event(other, "Bump", vec![Value::Object(Some(a))])?;
            self.call_event(a, "Bump", vec![Value::Object(Some(other))])?;
        }
        if let Some((at, by)) = blocked_by {
            let what = match by {
                Some(b) => self.object_name(b),
                None => "the level".to_string(),
            };
            let name = self.object_name(a);
            eprintln!("walk {name} stopped at {:?} by {what}", at.map(|v| v.round()));
        }
        // Climbing is something Harry does walking into a ledge face on: backing into one or
        // sliding along it does not start it.
        if stopped && self.facing(a, velocity)? {
            self.try_mount(a, velocity)?;
        }
        Ok(())
    }

    /// Whether a pawn could walk straight to an actor: its cylinder is carried along the line in
    /// steps of its own radius and put down on the ground under each one, and the first step
    /// that does not fit ends it. What a door asks before it opens for somebody.
    pub fn can_walk_straight_to(&mut self, a: ObjectKey, target: ObjectKey) -> crate::VmResult<bool> {
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let to = crate::vm::floats3(&self.get_prop_id(target, self.hot.location)?)?;
        let Some((_, radius, height)) = self.cylinder(a)? else { return Ok(true) };
        let extent = [radius, radius, height];
        let step_up = match self.get_prop(a, "MaxStepHeight") {
            Ok(Value::Float(v)) if v > 0.0 => v,
            _ => 0.0,
        };
        let flat = [to[0] - from[0], to[1] - from[1], 0.0];
        let distance = (flat[0] * flat[0] + flat[1] * flat[1]).sqrt();
        if distance <= radius {
            return Ok(true);
        }
        let steps = (distance / radius.max(1.0)).ceil() as u32;
        let mut at = from;
        for i in 1..=steps {
            let t = i as f32 / steps as f32;
            let mut next = [from[0] + flat[0] * t, from[1] + flat[1] * t, at[2]];
            // Up over a step, then down onto whatever is there: the same two moves a walk makes.
            if !self.actor_fits(a, next)? {
                next[2] += step_up;
                if !self.actor_fits(a, next)? {
                    return Ok(false);
                }
            }
            let below = [next[0], next[1], next[2] - height * 4.0];
            let blockers = std::mem::take(&mut self.blockers);
            let hit = self.sweep(a, next, below, extent, &blockers);
            self.blockers = blockers;
            match hit {
                Some((time, _, _)) => next[2] += (below[2] - next[2]) * time,
                // Nothing under it within four times its own height: a drop it cannot be asked
                // to walk off.
                None => return Ok(false),
            }
            at = next;
        }
        Ok(true)
    }

    /// Whether an actor's collision cylinder has room at a place, which is what a teleport asks
    /// before it moves one. An actor that does not collide with the world always fits.
    pub fn actor_fits(&mut self, a: ObjectKey, at: [f32; 3]) -> crate::VmResult<bool> {
        if !matches!(self.get_prop(a, "bCollideWorld"), Ok(Value::Bool(true))) {
            return Ok(true);
        }
        let Some((_, radius, height)) = self.cylinder(a)? else { return Ok(true) };
        let extent = [radius, radius, height];
        if self.bsp.as_ref().is_some_and(|b| b.box_solid(at, extent)) {
            if self.switches.trace_move {
                eprintln!("fits {}: no, the level at {:?}", self.object_name(a), at.map(|v| v.round()));
            }
            return Ok(false);
        }
        // Nor may it stand inside something that blocks the way.
        // An actor the tick did not gather (one that does not collide with actors, such as a
        // jellybean a student carries) still has its own say: taken as stopped by everything,
        // a bean put in a student's hand never fitted there.
        let role = match self.roles.get(&a).copied() {
            Some(r) => r,
            None => self.role(a),
        };
        let blocker = self
            .blockers
            .iter()
            .find(|b| {
                b.actor != a
                    && (matches!(b.shape, Shape::Brush { .. }) || role.stopped_by(&b.role))
                    && b.shape.holds(at, [radius, radius, height])
            })
            .map(|b| b.actor);
        if let (Some(b), true) = (blocker, self.switches.trace_move) {
            eprintln!("fits {}: no, {} is there at {:?}", self.object_name(a), self.object_name(b), at.map(|v| v.round()));
        }
        Ok(blocker.is_none())
    }

    /// Where an actor put at a place ends up: there if it fits, else pushed out of the level
    /// along each axis by as much as its collision box reaches into it, if that is enough.
    /// Measured on the `SmartStart`s: `Grounds_Night.SmartStart1`, where the player comes in
    /// from the willow, stands with Harry's cylinder 1.9 units into the floor, and the
    /// original still put him there (its save has him right above it, walked down onto the
    /// floor); refusing the move left him at the level's first start, across the map.
    pub fn find_spot(&mut self, a: ObjectKey, at: [f32; 3]) -> crate::VmResult<Option<[f32; 3]>> {
        if self.actor_fits(a, at)? {
            return Ok(Some(at));
        }
        let (Some((_, radius, height)), Some(bsp)) = (self.cylinder(a)?, self.bsp.as_ref()) else { return Ok(None) };
        let extent = [radius, radius, height];
        let mut spot = at;
        for axis in 0..3 {
            for sign in [-1.0f32, 1.0] {
                let mut end = at;
                end[axis] += sign * extent[axis];
                if let Some(hit) = bsp.line_check(at, end) {
                    let reach = (hit.location[axis] - at[axis]).abs();
                    spot[axis] -= sign * (extent[axis] - reach);
                }
            }
        }
        Ok(if spot != at && self.actor_fits(a, spot)? { Some(spot) } else { None })
    }

    /// Records a channel an actor was given, so the drawing can pose its bones.
    pub fn add_channel(&mut self, owner: ObjectKey, actor: ObjectKey, kind: u8, bone: grim_object::NameId) {
        let list = self.channels.entry(owner).or_default();
        list.retain(|c| c.actor != actor);
        list.push(Channel { actor, kind, bone, posing: false });
    }

    /// An animation played on a channel: it takes its bones over from now on.
    pub fn channel_plays(&mut self, actor: ObjectKey) {
        for list in self.channels.values_mut() {
            for c in list.iter_mut().filter(|c| c.actor == actor) {
                c.posing = true;
            }
        }
    }

    /// An animation the body plays that replaces what the channels do: they let go.
    pub fn release_channels(&mut self, owner: ObjectKey) {
        if let Some(list) = self.channels.get_mut(&owner) {
            for c in list {
                c.posing = false;
            }
        }
    }

    /// Channels of an actor, in the order they were created.
    pub fn channels_of(&self, owner: ObjectKey) -> &[Channel] {
        self.channels.get(&owner).map_or(&[], |v| v.as_slice())
    }

    /// Gives an actor a move for the tick to drive.
    pub fn start_move(&mut self, a: ObjectKey, order: MoveOrder) {
        self.moves.insert(a, order);
    }

    /// Advances the latent move an actor was given, if any: it turns towards what it must face
    /// and walks towards where it must go, and the state code resumes once it arrives.
    fn advance_move(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_WALKING: u8 = 1;
        const PHYS_FALLING: u8 = 2;
        let Some(mut order) = self.moves.get(&a).copied() else { return Ok(()) };
        order.timer -= dt;
        let expired = order.timer <= 0.0;
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;

        if order.landing {
            let landed = !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_FALLING));
            if landed || expired {
                self.moves.remove(&a);
            } else {
                self.moves.insert(a, order);
            }
            return Ok(());
        }

        // A move towards an actor follows it, so it keeps up with one that moves itself.
        let dest = match order.target {
            Some(t) if !self.is_deleted(t) => Some(crate::vm::floats3(&self.get_prop_id(t, self.hot.location)?)?),
            Some(_) => None,
            None => order.dest,
        };
        let focus = match order.focus_actor {
            Some(t) if !self.is_deleted(t) => Some(crate::vm::floats3(&self.get_prop_id(t, self.hot.location)?)?),
            Some(_) => None,
            None => order.focus,
        };
        if let Some(d) = dest {
            self.set_prop(a, "Destination", crate::vm::vector(d[0], d[1], d[2]))?;
            // Unreal's `MoveTo` ends when the pawn stops getting anywhere, not only when it
            // arrives. Several cutscenes send an actor at a mark it cannot reach in a straight
            // line and carry on regardless; waiting the timer out instead left them grinding
            // against a wall for a minute, or shuffling inside a wall for ever when whatever
            // holds them keeps putting them back. Sliding along a wall towards the mark still
            // gains ground, so that goes on.
            let left = ((d[0] - from[0]).powi(2) + (d[1] - from[1]).powi(2)).sqrt();
            if left >= order.nearest {
                self.moves.remove(&a);
                self.set_prop_id(a, self.hot.velocity, crate::vm::vector(0.0, 0.0, 0.0))?;
                self.set_prop(a, "Acceleration", crate::vm::vector(0.0, 0.0, 0.0))?;
                return Ok(());
            }
            order.nearest = left;
        }
        if let Some(f) = focus {
            self.set_prop(a, "Focus", crate::vm::vector(f[0], f[1], f[2]))?;
        }

        // What it faces: the focus if it was given one, else where it is going.
        let facing = focus.or(dest);
        let mut turned = true;
        if let Some(f) = facing {
            let to = grim_world::sub(f, from);
            if to[0] * to[0] + to[1] * to[1] > 1e-4 {
                // Kept between 0 and 65535, like everything the original turns (see
                // `turn_towards_unit`): its saves have the same yaw here as in `Rotation`.
                let want = crate::vm::rotation_towards(to)[1] & 0xFFFF;
                self.set_prop_id(a, self.hot.desired_rotation, crate::vm::rotator(0, want, 0))?;
                turned = self.turn_towards(a, want, dt)?;
            }
        }

        let arrived = match dest {
            None => turned,
            Some(d) => {
                let to = grim_world::sub(d, from);
                let flat = (to[0] * to[0] + to[1] * to[1]).sqrt();
                let speed = self.move_speed(a, order.speed)?;
                let step = speed * dt;
                if flat <= step.max(1.0) {
                    // The last step lands on the destination instead of orbiting around it.
                    self.set_prop_id(a, self.hot.location, crate::vm::vector(d[0], d[1], from[2]))?;
                    self.set_prop_id(a, self.hot.velocity, crate::vm::vector(0.0, 0.0, 0.0))?;
                    self.set_prop(a, "Acceleration", crate::vm::vector(0.0, 0.0, 0.0))?;
                    true
                } else {
                    let dir = [to[0] / flat, to[1] / flat, 0.0];
                    let velocity = grim_world::scale(dir, speed);
                    self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
                    self.set_prop(a, "Acceleration", crate::vm::vector(dir[0], dir[1], 0.0))?;
                    if matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_WALKING)) {
                        self.step_walk(a, velocity, dt)?;
                    } else {
                        let now = grim_world::add(from, grim_world::scale(velocity, dt));
                        self.set_prop_id(a, self.hot.location, crate::vm::vector(now[0], now[1], now[2]))?;
                    }
                    false
                }
            }
        };
        if self.switches.trace_move {
            let name = self.object_name(a);
            let left = dest.map(|d| {
                let to = grim_world::sub(d, from);
                (to[0] * to[0] + to[1] * to[1]).sqrt()
            });
            let physics = self.get_prop_id(a, self.hot.physics).ok();
            eprintln!(
                "move {name} ({physics:?}) at {:?} to {:?} left {:?} turned {turned} timer {:.1} {}",
                from.map(|v| v.round()),
                dest.map(|d| d.map(|v| v.round())),
                left.map(|v| v.round()),
                order.timer,
                if arrived { "arrived" } else if expired { "gave up" } else { "" }
            );
        }
        if arrived || expired {
            self.moves.remove(&a);
        } else {
            self.moves.insert(a, order);
        }
        Ok(())
    }

    /// Speed of a latent move: a fraction of the actor's own, as `DesiredSpeed` means.
    fn move_speed(&mut self, a: ObjectKey, fraction: f32) -> crate::VmResult<f32> {
        let ground = match self.get_prop_id(a, self.hot.ground_speed)? {
            Value::Float(v) if v > 0.0 => v,
            _ => 320.0,
        };
        Ok(ground * fraction.clamp(0.0, 1.0).max(0.01))
    }

    /// Turns an actor's yaw towards `want` at its own `RotationRate`; true once it is there.
    fn turn_towards(&mut self, a: ObjectKey, want: i32, dt: f32) -> crate::VmResult<bool> {
        let Value::Struct(rot) = self.get_prop(a, "Rotation")? else { return Ok(true) };
        let Some(Value::Int(yaw)) = rot.get(1).cloned() else { return Ok(true) };
        let rate = match self.get_prop_id(a, self.hot.rotation_rate)? {
            Value::Struct(r) => match r.get(1) {
                Some(Value::Int(v)) if *v > 0 => *v,
                _ => 60000,
            },
            _ => 60000,
        };
        // Yaw wraps at 65536: take the short way round.
        let delta = (((want - yaw) % 65536) + 98304) % 65536 - 32768;
        let step = (rate as f32 * dt) as i32;
        let (pitch, roll) = (as_int(rot.first()), as_int(rot.get(2)));
        if delta.abs() <= step {
            self.set_prop(a, "Rotation", crate::vm::rotator(pitch, want & 0xFFFF, roll))?;
            return Ok(true);
        }
        let yaw = (yaw + step * delta.signum()) & 0xFFFF;
        self.set_prop(a, "Rotation", crate::vm::rotator(pitch, yaw, roll))?;
        Ok(false)
    }

    /// PHYS_Interpolating: an interpolation manager walks its owner along a chain of
    /// `InterpolationPoint`s, each section a bezier whose control points the points carry.
    /// Reaching one is the point's own business: `InterpolateEnd` fires its events, holds the
    /// move for a pause and picks what comes next, so this only advances and places.
    /// Puts an actor on a base, with the events the scripts expect. What an actor stands on
    /// is what carries it when that moves.
    pub fn set_base(&mut self, a: ObjectKey, new: Option<ObjectKey>) -> crate::VmResult<()> {
        let old = match self.get_prop_id(a, self.hot.base)? {
            Value::Object(o) => o,
            _ => None,
        };
        if new == old {
            return Ok(());
        }
        if let Some(o) = old {
            self.call_event(o, "Detach", vec![Value::Object(Some(a))])?;
        }
        self.set_prop_id(a, self.hot.base, Value::Object(new))?;
        if let Some(n) = new {
            self.call_event(n, "Attach", vec![Value::Object(Some(a))])?;
        }
        self.call_event(a, "BaseChange", vec![])?;
        Ok(())
    }

    /// Whether a mover put where it is going would be inside somebody, and what the scripts
    /// want done about it. Whoever is riding it is not in the way: it carries them.
    fn encroaches(&mut self, a: ObjectKey, at: [f32; 3], turn: [i32; 3]) -> crate::VmResult<bool> {
        let Some(shape) = self.blocking_shape_at(a, at, turn) else { return Ok(false) };
        let mut blocked = false;
        for other in self.collidable_actors() {
            let Collider { actor: b, at: centre, radius, height, .. } = other;
            if b == a || self.is_deleted(b) {
                continue;
            }
            if matches!(self.get_prop(b, "Base"), Ok(Value::Object(Some(base))) if base == a) {
                continue;
            }
            // Only what stands in the way counts. A marker or a trigger sitting in a doorway
            // collides for its own purposes but is not something a door can be stopped by.
            let blocks = matches!(self.get_prop(b, "bBlockActors"), Ok(Value::Bool(true)))
                || matches!(self.get_prop(b, "bBlockPlayers"), Ok(Value::Bool(true)));
            if !blocks {
                continue;
            }
            if !shape.holds(centre, [radius, radius, height]) {
                continue;
            }
            if self.switches.trace_encroach {
                let class = |vm: &mut Self, k| {
                    vm.class_of(k).map(|c| vm.world.names.str(vm.world.class(c).name).to_string()).unwrap_or_default()
                };
                let (mover, hit) = (class(self, a), class(self, b));
                eprintln!("encroach {mover} on {hit} at {centre:?}");
            }
            self.call_event(b, "EncroachedBy", vec![Value::Object(Some(a))])?;
            if matches!(self.call_event(a, "EncroachingOn", vec![Value::Object(Some(b))])?, Some(Value::Bool(true))) {
                blocked = true;
            }
        }
        Ok(blocked)
    }

    /// The shape a mover would block with if it stood somewhere else.
    fn blocking_shape_at(&mut self, a: ObjectKey, at: [f32; 3], rotation: [i32; 3]) -> Option<Shape> {
        let Ok(Value::Object(Some(brush))) = self.get_prop(a, "Brush") else { return None };
        let bsp = self.brush_bsp(brush)?;
        let pivot = self.get_prop(a, "PrePivot").and_then(|v| crate::vm::floats3(&v)).unwrap_or([0.0; 3]);
        let local = self.brush_box(brush).unwrap_or(([0.0; 3], [f32::MAX / 4.0; 3]));
        Some(Shape::Brush { brush, bsp, at, axes: crate::natives::core::axes(rotation), pivot, local })
    }

    /// Whether an actor is a mover, which is what can carry another along with it.
    fn is_mover(&mut self, a: ObjectKey) -> bool {
        self.class_of(a).is_ok_and(|c| {
            let mut class = Some(c);
            while let Some(k) = class {
                if self.world.names.str(self.world.class(k).name).contains("Mover") {
                    return true;
                }
                class = self.world.class(k).super_;
            }
            false
        })
    }

    /// `PHYS_Trailer`: the actor rides its owner. Unreal puts it where the owner is, moved by
    /// its own `PrePivot` when `bTrailerPrePivot` asks for it, and turns it with the owner when
    /// `bTrailerSameRotation` does. The game hangs its attached effects off this
    /// (`HPawn.CreateAttachedParticleFX` sets the physics and the pivot), so without it the
    /// flying car's trail of smoke stays behind where the level started.
    fn trail(&mut self, a: ObjectKey) -> crate::VmResult<()> {
        const PHYS_TRAILER: u8 = 11;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_TRAILER)) {
            return Ok(());
        }
        let Value::Object(Some(owner)) = self.get_prop(a, "Owner")? else { return Ok(()) };
        if self.is_deleted(owner) {
            return Ok(());
        }
        let mut at = crate::vm::floats3(&self.get_prop_id(owner, self.hot.location)?)?;
        // `Actor.AttachToOwner` puts a trailer on one of its owner's bones: it sets `AnimBone`
        // to the bone's number plus one. The whomping willow's roots hang their blocking boxes
        // on their bones this way, so the way through opens as a root lifts; things carried
        // go in a hand the same way.
        if let Ok(Value::Byte(bone)) = self.get_prop(a, "AnimBone") {
            if bone > 0 {
                if let Some((joint, _)) = self.bone_index_world(owner, bone as usize - 1) {
                    at = joint;
                }
            }
        }
        let same_rotation = matches!(self.get_prop(a, "bTrailerSameRotation"), Ok(Value::Bool(true)));
        let rotation = crate::vm::ints3(&self.get_prop_id(owner, self.hot.rotation)?)?;
        if matches!(self.get_prop(a, "bTrailerPrePivot"), Ok(Value::Bool(true))) {
            let pivot = crate::vm::floats3(&self.get_prop(a, "PrePivot")?)?;
            // The pivot is in the owner's own frame, turned by its rotation, whether or not the
            // trailer shares that facing. Measured against the original's save of `Adv1Willow`:
            // the flame `FireHP2_bigtorch6` carries `PrePivot = (0, 15, 7)` on a torch at
            // (1259.47, -191.52, -372) facing yaw 16256, and the save has it at
            // (1244.47, -191.33, -365) — the turned pivot to the last digit, where taking the
            // pivot as it stands puts it 21 units away. Forty-four of the level's torch flames
            // were out by 20 to 30 units.
            let axes = crate::natives::core::axes(rotation);
            let offset = [0, 1, 2].map(|i| axes[0][i] * pivot[0] + axes[1][i] * pivot[1] + axes[2][i] * pivot[2]);
            at = [0, 1, 2].map(|i| at[i] + offset[i]);
        }
        self.set_prop_id(a, self.hot.location, crate::vm::vector(at[0], at[1], at[2]))?;
        if same_rotation {
            self.set_prop_id(a, self.hot.rotation, crate::vm::rotator(rotation[0], rotation[1], rotation[2]))?;
        }
        Ok(())
    }

    /// `PHYS_MovingBrush`: a mover on its way to a keyframe. `Mover.InterpolateTo` sets up
    /// where it comes from (`OldPos`/`OldRot`), where it is going (`BasePos + KeyPos[KeyNum]`)
    /// and how fast (`PhysRate`, one over the seconds it takes); the engine walks `PhysAlpha`
    /// from 0 to 1 and puts the brush between the two. Without this no door opens and no
    /// staircase moves, because the scripts only ever ask for the move.
    fn move_brush(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_MOVING_BRUSH: u8 = 9;
        const PHYS_NONE: u8 = 0;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_MOVING_BRUSH)) {
            return Ok(());
        }
        // A mover carries this physics from the moment it is made; what says it is on its way
        // is `bInterpolating`, which `InterpolateTo` sets along with where it comes from.
        // Without that check every mover is dragged to where its untouched keys point, which
        // is the origin of the map.
        if !matches!(self.get_prop(a, "bInterpolating"), Ok(Value::Bool(true))) {
            return Ok(());
        }
        let Ok(Value::Float(rate)) = self.get_prop(a, "PhysRate") else { return Ok(()) };
        let Ok(Value::Float(alpha)) = self.get_prop(a, "PhysAlpha") else { return Ok(()) };
        let Ok(Value::Byte(key)) = self.get_prop(a, "KeyNum") else { return Ok(()) };
        let from = crate::vm::floats3(&self.get_prop(a, "OldPos")?)?;
        let from_rot = crate::vm::ints3(&self.get_prop(a, "OldRot")?)?;
        let base = crate::vm::floats3(&self.get_prop(a, "BasePos")?)?;
        let base_rot = crate::vm::ints3(&self.get_prop(a, "BaseRot")?)?;
        let key_pos = crate::vm::floats3(&self.get_prop_at(a, "KeyPos", key as u32)?)?;
        let key_rot = crate::vm::ints3(&self.get_prop_at(a, "KeyRot", key as u32)?)?;
        let to = [0, 1, 2].map(|i| base[i] + key_pos[i]);
        let to_rot = [0, 1, 2].map(|i| base_rot[i] + key_rot[i]);

        // A mover with no time to take arrives at once. The game has them: `Mover0` and
        // `Mover50` of `Adv3DungeonQuest` carry `MoveTime = 0`, and `Mover.InterpolateTo` turns
        // that into a rate of nothing (it divides by the time). Creeping up on one by nothing a
        // tick never gets there, and `FinishInterpolation` would wait for ever.
        let alpha = if rate > 0.0 { (alpha + rate * dt).min(1.0) } else { 1.0 };
        // Hypothesis: `MV_GlideByTime`, which every mover of this game asks for, eases in and
        // out of the move; `MV_MoveByTime` runs at a steady pace. The doors of the game start
        // and stop softly, which is what the smooth step gives.
        const MV_MOVE_BY_TIME: u8 = 0;
        let glide = matches!(self.get_prop(a, "MoverGlideType"), Ok(Value::Byte(MV_MOVE_BY_TIME)));
        let t = if glide { alpha } else { alpha * alpha * (3.0 - 2.0 * alpha) };
        let at = [0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t);
        let turn = [0, 1, 2].map(|i| from_rot[i] + ((to_rot[i] - from_rot[i]) as f32 * t) as i32);
        // Anything in the way hears about it first. `Mover.EncroachingOn` is where the game
        // says what to do — stop, go back, crush or ignore — and a true answer means the move
        // does not happen this tick.
        if self.encroaches(a, at, turn)? {
            return Ok(());
        }
        // A mover that collides with the world stops where its brush meets the level, and the
        // engine leaves the wall's normal in `Mover.HitNormal`, a property of this game's that
        // no script writes. `GridMover`, the only mover that asks for `bCollideWorld`, is kept
        // inside its yard by it: `GridMover.Tick` reads the normal once the move is over and
        // `GetNewLocation` slides the next push along the wall. Without it the willow's blocks
        // went straight through the walls around them.
        if matches!(self.get_prop(a, "bCollideWorld"), Ok(Value::Bool(true))) {
            if let Some((centre, half)) = self.collision_box(a)? {
                let was = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
                let motion = [0, 1, 2].map(|i| at[i] - was[i]);
                let end = [0, 1, 2].map(|i| centre[i] + motion[i]);
                let hit = self.bsp.as_ref().and_then(|b| b.box_check(centre, end, half));
                if let Some(hit) = hit.filter(|h| grim_world::dot(h.normal, motion) < 0.0) {
                    if self.switches.trace_encroach {
                        eprintln!("mover {} meets the level at {:.2} of its step, normal {:?}", self.object_name(a), hit.time, hit.normal);
                    }
                    let stop = [0, 1, 2].map(|i| was[i] + motion[i] * hit.time);
                    self.set_prop_id(a, self.hot.location, crate::vm::vector(stop[0], stop[1], stop[2]))?;
                    self.set_prop(a, "HitNormal", crate::vm::vector(hit.normal[0], hit.normal[1], hit.normal[2]))?;
                    self.set_prop(a, "Physics", Value::Byte(PHYS_NONE))?;
                    self.set_prop(a, "bInterpolating", Value::Bool(false))?;
                    self.call_event(a, "KeyFrameReached", vec![])?;
                    return Ok(());
                }
            }
        }
        // Whatever rides it comes along: a staircase that moves takes with it whoever is
        // standing on it. The brush turns about its own place, so a rider turns around it too.
        let was = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let was_rot = crate::vm::ints3(&self.get_prop(a, "Rotation")?)?;
        let step = [0, 1, 2].map(|i| at[i] - was[i]);
        let spin = turn[1] - was_rot[1];
        if step.iter().any(|v| v.abs() > 1e-4) || spin != 0 {
            let angle = spin as f32 / 32768.0 * std::f32::consts::PI;
            let (sin, cos) = angle.sin_cos();
            for rider in self.actors.clone() {
                if self.is_deleted(rider) || rider == a {
                    continue;
                }
                if !matches!(self.get_prop(rider, "Base"), Ok(Value::Object(Some(b))) if b == a) {
                    continue;
                }
                let Ok(here) = self.get_prop(rider, "Location").and_then(|v| crate::vm::floats3(&v)) else { continue };
                let d = [here[0] - was[0], here[1] - was[1]];
                let turned = [at[0] + d[0] * cos - d[1] * sin, at[1] + d[0] * sin + d[1] * cos, here[2] + step[2]];
                self.set_prop(rider, "Location", crate::vm::vector(turned[0], turned[1], turned[2]))?;
                if spin != 0 {
                    if let Ok(r) = self.get_prop(rider, "Rotation").and_then(|v| crate::vm::ints3(&v)) {
                        self.set_prop(rider, "Rotation", crate::vm::rotator(r[0], r[1] + spin, r[2]))?;
                    }
                }
            }
        }
        self.set_prop_id(a, self.hot.location, crate::vm::vector(at[0], at[1], at[2]))?;
        self.set_prop(a, "Rotation", crate::vm::rotator(turn[0], turn[1], turn[2]))?;
        self.set_prop(a, "PhysAlpha", Value::Float(alpha))?;
        if alpha < 1.0 {
            return Ok(());
        }
        // Arrived: the brush stops and the scripts hear about it, which is what chains the
        // next leg of the move and what a `FinishInterpolation` is waiting for.
        self.set_prop(a, "Physics", Value::Byte(PHYS_NONE))?;
        self.set_prop(a, "bInterpolating", Value::Bool(false))?;
        self.call_event(a, "KeyFrameReached", vec![])?;
        Ok(())
    }

    fn interpolate(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_INTERPOLATING: u8 = 8;
        if !matches!(self.get_prop_id(a, self.hot.physics), Ok(Value::Byte(PHYS_INTERPOLATING))) {
            return Ok(());
        }
        // Only a manager has somewhere to go; the actor it moves just carries the mode.
        let Ok(Value::Object(Some(mut dest))) = self.get_prop(a, "Dest") else { return Ok(()) };
        let Ok(Value::Object(Some(owner))) = self.get_prop(a, "Owner") else { return Ok(()) };
        let Ok(Value::Float(rate)) = self.get_prop(a, "PhysRate") else { return Ok(()) };
        let Ok(Value::Float(alpha)) = self.get_prop(a, "PhysAlpha") else { return Ok(()) };

        // Waiting at a point: the pause runs down before the path goes on.
        let pause = match self.get_prop(a, "RemainingPause") {
            Ok(Value::Float(v)) => v,
            _ => 0.0,
        };
        let mut alpha = alpha;
        if pause > 0.0 {
            self.set_prop(a, "RemainingPause", Value::Float(pause - dt))?;
            if pause - dt > 0.0 {
                return Ok(());
            }
            let Some(next) = self.reached(a, dest)? else { return Ok(()) };
            (dest, alpha) = (next, 0.0);
        }

        // A spline move carries its speed in world units and the scripts ease it up and down
        // from a standstill, so a speed of zero means the move has not started rather than that
        // it has no speed. A plain interpolation gives its rate as a fraction of the section.
        let spline = matches!(self.get_prop(owner, "SplineManager"), Ok(Value::Object(Some(m))) if m == a);
        let step = match (spline, self.get_prop(owner, "IPSpeed")) {
            (true, Ok(Value::Float(speed))) => speed * dt / self.section_length(dest)?.max(1.0),
            (true, _) => 0.0,
            _ => rate * dt,
        };
        alpha += step;
        while alpha >= 1.0 {
            alpha -= 1.0;
            let Some(next) = self.reached(a, dest)? else { return Ok(()) };
            if next == dest {
                // A pause started here: the actor waits at the point it reached.
                self.set_prop(a, "PhysAlpha", Value::Float(1.0))?;
                return Ok(());
            }
            dest = next;
        }
        self.set_prop(a, "PhysAlpha", Value::Float(alpha))?;

        // A path that starts nowhere in particular runs from wherever the actor stood.
        let here = crate::vm::floats3(&self.get_prop(owner, "Location")?)?;
        match self.interp_from.get(&a) {
            Some((from, _)) if *from == dest => {}
            _ => {
                self.interp_from.insert(a, (dest, here));
            }
        }
        let start = self.interp_from.get(&a).map(|(_, p)| *p);
        let Some((at, facing)) = self.bezier_at(a, dest, alpha, start)? else { return Ok(()) };
        self.set_prop(owner, "Location", crate::vm::vector(at[0], at[1], at[2]))?;
        // Turning along the path is the point's to ask for, and the actor's to refuse: a camera
        // told to follow a spline without `align` aims at its own target instead.
        let ignores = matches!(self.get_prop(owner, "bInterpolating_IgnoreRot"), Ok(Value::Bool(true)))
            || matches!(self.get_prop(dest, "bDontAlterRotation"), Ok(Value::Bool(true)));
        if !ignores && matches!(self.get_prop(dest, "bFaceMoveDirection"), Ok(Value::Bool(true))) {
            let r = crate::vm::rotator(facing[0], facing[1], facing[2]);
            self.set_prop(owner, "Rotation", r)?;
        }
        Ok(())
    }

    /// Tells a point it was reached and reports where the manager goes next: the same point
    /// again while it pauses there, and `None` once the path is over.
    fn reached(&mut self, manager: ObjectKey, dest: ObjectKey) -> crate::VmResult<Option<ObjectKey>> {
        self.call_event(dest, "InterpolateEnd", vec![Value::Object(Some(manager)), Value::Bool(true)])?;
        // The path may have ended, which destroys the manager.
        if self.is_deleted(manager) {
            return Ok(None);
        }
        match self.get_prop(manager, "Dest")? {
            Value::Object(Some(next)) => Ok(Some(next)),
            _ => Ok(None),
        }
    }

    /// How long the section ending at `dest` is: the point carries it, as the editor measured
    /// it. `Pawn.SplineTick` reads the same field as `PhysAlpha * Dest.PathDist`, so it is the
    /// length of that one section and not a distance along the whole path.
    fn section_length(&mut self, dest: ObjectKey) -> crate::VmResult<f32> {
        Ok(match self.get_prop(dest, "PathDist") {
            Ok(Value::Float(v)) if v > 0.0 => v,
            // A section the editor never measured (the actor starts off the path): take the
            // straight line to it.
            _ => {
                let to = crate::vm::floats3(&self.get_prop(dest, "Location")?)?;
                match self.get_prop(dest, "Prev") {
                    Ok(Value::Object(Some(prev))) => {
                        let from = crate::vm::floats3(&self.get_prop(prev, "Location")?)?;
                        let d = grim_world::sub(to, from);
                        grim_world::dot(d, d).sqrt()
                    }
                    _ => 0.0,
                }
            }
        })
    }

    /// Point and facing at `alpha` of the section that ends at `dest`: a cubic bezier whose
    /// middle controls are the two points' own offsets, as `InterpolationPoint` describes them.
    fn bezier_at(
        &mut self,
        manager: ObjectKey,
        dest: ObjectKey,
        alpha: f32,
        start: Option<[f32; 3]>,
    ) -> crate::VmResult<Option<([f32; 3], [i32; 3])>> {
        let p3 = crate::vm::floats3(&self.get_prop(dest, "Location")?)?;
        // The manager remembers where the section started, which is not always the point
        // before this one along its path: a spline can be told to start anywhere.
        let prev = match self.get_prop(manager, "Last") {
            Ok(Value::Object(Some(last))) if last != dest => Some(last),
            _ => match self.get_prop(dest, "Prev") {
                Ok(Value::Object(Some(prev))) => Some(prev),
                _ => None,
            },
        };
        let p0 = match prev {
            Some(prev) => crate::vm::floats3(&self.get_prop(prev, "Location")?)?,
            None => match start {
                Some(p) => p,
                None => return Ok(None),
            },
        };
        // Each point carries the control of the section it starts and of the one it ends, so a
        // section takes the leaving tangent of the point before and the arriving one of this.
        let out = match prev {
            Some(prev) => crate::vm::floats3(&self.get_prop(prev, "StartControlPoint")?)?,
            None => [0.0; 3],
        };
        let into = crate::vm::floats3(&self.get_prop(dest, "EndControlPoint")?)?;
        let p1 = grim_world::add(p0, out);
        let p2 = grim_world::add(p3, into);
        let at = |t: f32| {
            let u = 1.0 - t;
            let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            [0, 1, 2].map(|i| p0[i] * a + p1[i] * b + p2[i] * c + p3[i] * d)
        };
        let here = at(alpha);
        // Facing comes from where it is heading next along the curve.
        let ahead = at((alpha + 0.01).min(1.0));
        Ok(Some((here, crate::vm::rotation_towards(grim_world::sub(ahead, here)))))
    }

    /// Advances the sequence an actor is playing and the blend into it.
    ///
    /// The numbers are the ones the original keeps, as its saved games show them: `AnimRate`
    /// is the fraction of the sequence played a second (`Rate` times the sequence's own rate
    /// over its frame count: 0.2479 for Harry's 121-frame idle at 30), a sequence that does
    /// not loop stops at `AnimLast`, its last key (1 - 1/frames), with the rate put back to
    /// zero, and `TweenAlpha` runs from 0 to 1 at `TweenRate` a second while the sequence
    /// itself already plays from its first frame. Every save caught mid-blend agrees to the
    /// last digit: `TweenAlpha / TweenRate` equals `AnimFrame / AnimRate`, and `TweenRate` is
    /// back at zero once the blend is over.
    fn animate(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        let Ok(Value::Float(rate)) = self.get_prop_id(a, self.hot.anim_rate) else { return Ok(()) };
        let tween = match self.get_prop(a, "TweenRate")? {
            Value::Float(v) if v > 0.0 => v,
            _ => 0.0,
        };
        if tween > 0.0 {
            let alpha = match self.get_prop(a, "TweenAlpha")? {
                Value::Float(v) => v,
                _ => 1.0,
            };
            let next = alpha + tween * dt;
            if next < 1.0 {
                self.set_prop(a, "TweenAlpha", Value::Float(next))?;
            } else {
                self.set_prop(a, "TweenAlpha", Value::Float(1.0))?;
                self.set_prop(a, "TweenRate", Value::Float(0.0))?;
                // Hypothesis: a plain `TweenAnim` has nothing to play once it has landed on
                // its pose, and what waits for it (`FinishAnim` after a tween) is let go then.
                if rate == 0.0 && !matches!(self.get_prop_id(a, self.hot.anim_finished)?, Value::Bool(true)) {
                    self.set_prop_id(a, self.hot.anim_finished, Value::Bool(true))?;
                    self.call_event(a, "AnimEnd", vec![])?;
                    return Ok(());
                }
            }
        }
        if rate == 0.0 {
            return Ok(());
        }
        let Ok(Value::Name(seq)) = self.get_prop_id(a, self.hot.anim_sequence) else { return Ok(()) };
        let notifies = match self.sequence_of(a, seq) {
            Some(info) => info.notifies,
            // A sequence its mesh does not have. The scripts wait for animations to end, so it
            // ends right away rather than holding a cutscene up for ever.
            None => {
                let name = self.world.names.str(seq).to_string();
                let mesh = match self.anim_mesh(a) {
                    Some(m) => self.world.path(m),
                    None => {
                        let class = self.class_of(a).map(|c| self.world.names.str(self.world.class(c).name).to_string()).unwrap_or_default();
                        let owner = match self.get_prop(a, "Owner") {
                            Ok(Value::Object(Some(o))) => self.object_name(o),
                            _ => "nobody".to_string(),
                        };
                        format!("a {class} of {owner} with no mesh")
                    }
                };
                self.warn(format!("no sequence '{name}' in {mesh}"));
                self.set_prop_id(a, self.hot.anim_rate, Value::Float(0.0))?;
                self.set_prop_id(a, self.hot.anim_finished, Value::Bool(true))?;
                self.call_event(a, "AnimEnd", vec![])?;
                return Ok(());
            }
        };
        let Ok(Value::Float(frame)) = self.get_prop_id(a, self.hot.anim_frame) else { return Ok(()) };
        let looping = matches!(self.get_prop_id(a, self.hot.anim_loop), Ok(Value::Bool(true)));
        let last = match self.get_prop_id(a, self.hot.anim_last)? {
            Value::Float(l) if l > 0.0 => l,
            _ => 1.0,
        };
        let mut next = frame + rate * dt;
        // A sequence can carry calls of its own, partway through: the mesh keeps the moment
        // and the name of a function, and the actor playing it is asked to run that function
        // when the animation reaches it. Harry's cast is one of these, which is why releasing
        // the wand plays the throw and nothing leaves it until the arm has come round.
        // Hypothesis: the moment is a fraction of the sequence, like `AnimFrame` itself.
        for &(at, call) in &notifies {
            if at > frame && at <= next {
                self.call_event_id(a, call, vec![])?;
            }
        }
        if looping {
            // A loop runs through its last frame, the one that carries the last key back to
            // the first, before it comes round.
            if next < 1.0 {
                return self.set_prop_id(a, self.hot.anim_frame, Value::Float(next));
            }
            next -= next.floor();
            self.set_prop_id(a, self.hot.anim_frame, Value::Float(next))?;
        } else {
            if next < last {
                return self.set_prop_id(a, self.hot.anim_frame, Value::Float(next));
            }
            self.set_prop_id(a, self.hot.anim_frame, Value::Float(last))?;
            self.set_prop_id(a, self.hot.anim_rate, Value::Float(0.0))?;
            self.set_prop_id(a, self.hot.anim_finished, Value::Bool(true))?;
        }
        self.call_event(a, "AnimEnd", vec![])?;
        Ok(())
    }

    /// The mesh whose sequences an actor plays. An anim channel has no mesh of its own: it
    /// plays the sequences of the actor it poses.
    fn anim_mesh(&mut self, a: ObjectKey) -> Option<ObjectKey> {
        match self.get_prop_id(a, self.hot.mesh) {
            Ok(Value::Object(Some(mesh))) => Some(mesh),
            _ => match self.get_prop(a, "Owner") {
                Ok(Value::Object(Some(owner))) => match self.get_prop_id(owner, self.hot.mesh) {
                    Ok(Value::Object(Some(mesh))) => Some(mesh),
                    _ => None,
                },
                _ => None,
            },
        }
    }

    /// What an actor's mesh says about a sequence. Read once per mesh: reaching for it means
    /// parsing the package the mesh lives in.
    pub(crate) fn sequence_of(&mut self, a: ObjectKey, seq: grim_object::NameId) -> Option<SeqInfo> {
        let mesh = self.anim_mesh(a)?;
        if !self.durations.contains_key(&mesh) {
            let mut table = std::collections::HashMap::new();
            let pkg = self.world.package(mesh.package).clone();
            let mut take = |pkg: &std::sync::Arc<grim_package::Package>,
                            seqs: Vec<(grim_package::NameIndex, i32, f32, Vec<(f32, grim_package::NameIndex)>)>,
                            names: &mut grim_object::Names| {
                for (name, frames, rate, notifys) in seqs {
                    if rate > 0.0 && frames > 0 {
                        let calls = notifys.iter().map(|&(time, f)| (time, names.intern(pkg.name(f)))).collect();
                        table.insert(names.intern(pkg.name(name)), SeqInfo { frames: frames as f32, rate, notifies: calls });
                    }
                }
            };
            if let Some(asset) = grim_assets::load_export(&pkg, mesh.export).ok().map(|o| o.asset) {
                match &asset {
                    // A skeletal mesh keeps its sequences in the animation object it points at,
                    // which is usually in another package.
                    grim_assets::Asset::SkeletalMesh(m) => {
                        let own =
                            m.lod.mesh.anim_seqs.iter().map(|s| (s.name, s.num_frames, s.rate, s.notifys.iter().map(|n| (n.time, n.function)).collect::<Vec<_>>())).collect();
                        take(&pkg, own, &mut self.world.names);
                        if let Ok(handle) = self.world.lib.resolve(&pkg, m.default_animation) {
                            if let Ok(o) = grim_assets::load_export(&handle.package, handle.export) {
                                if let grim_assets::Asset::Animation(anim) = o.asset {
                                    let seqs = anim
                                        .anim_seqs
                                        .iter()
                                        .map(|s| (s.name, s.num_frames, s.rate, s.notifys.iter().map(|n| (n.time, n.function)).collect::<Vec<_>>()))
                                        .collect();
                                    take(&handle.package, seqs, &mut self.world.names);
                                }
                            }
                        }
                    }
                    grim_assets::Asset::LodMesh(m) => {
                        let own =
                            m.mesh.anim_seqs.iter().map(|s| (s.name, s.num_frames, s.rate, s.notifys.iter().map(|n| (n.time, n.function)).collect::<Vec<_>>())).collect();
                        take(&pkg, own, &mut self.world.names);
                    }
                    _ => {}
                }
            }
            self.durations.insert(mesh, table);
        }
        self.durations.get(&mesh)?.get(&seq).cloned()
    }

    /// Fires `Touch` and `UnTouch` for an actor that just moved, as the engine does when the
    /// collision cylinders of two actors start or stop overlapping. `Touching` is what the
    /// scripts read to find what is on top of them.
    pub fn update_touching(&mut self, a: ObjectKey, collidable: &[Collider]) -> crate::VmResult<()> {
        if !matches!(self.get_prop_id(a, self.hot.collide_actors)?, Value::Bool(true)) {
            return Ok(());
        }
        let Some(here) = self.collider(a)? else { return Ok(()) };
        let role = self.roles.get(&a).copied().unwrap_or(Role::NONE);
        let mut now = Vec::new();
        for other in collidable {
            if other.actor == a {
                continue;
            }
            if !here.overlaps(other) {
                continue;
            }
            // What stops the other is run into, not touched: a mover and whatever stands on
            // it, or two pawns, meet as a bump. The original's saves have fire crabs riding
            // movers with nothing in their `Touching`, where overlapping alone put the mover
            // there.
            let theirs = self.roles.get(&other.actor).copied().unwrap_or(Role::NONE);
            let brush = matches!(self.get_prop_id(other.actor, self.hot.brush), Ok(Value::Object(Some(_))))
                || matches!(self.get_prop_id(a, self.hot.brush), Ok(Value::Object(Some(_))));
            if brush || role.stopped_by(&theirs) || theirs.stopped_by(&role) {
                continue;
            }
            now.push(other.actor);
        }
        let before = self.touching.get(&a).cloned().unwrap_or_default();
        for &b in &now {
            if !before.contains(&b) {
                if self.switches.trace_touch {
                    let name = |vm: &mut Self, k| {
                        vm.class_of(k).map(|c| vm.world.names.str(vm.world.class(c).name).to_string()).unwrap_or_default()
                    };
                    let (x, y) = (name(self, a), name(self, b));
                    eprintln!("touch {x} <-> {y}");
                }
                self.call_event(a, "Touch", vec![Value::Object(Some(b))])?;
                self.call_event(b, "Touch", vec![Value::Object(Some(a))])?;
            }
        }
        for &b in &before {
            if !now.contains(&b) {
                self.call_event(a, "UnTouch", vec![Value::Object(Some(b))])?;
                self.call_event(b, "UnTouch", vec![Value::Object(Some(a))])?;
            }
        }
        // Scripts read the first few through `Touching`, which keeps its slots: what stops
        // touching leaves a hole and what starts takes the first free one. The original's
        // saves have snails whose `Touching` holds the same trail segments as ours but in
        // other slots, which rebuilding the list in the order the actors are walked gave.
        let mut slots: Vec<Option<ObjectKey>> = (0..4u32)
            .map(|i| match self.get_prop_at(a, "Touching", i) {
                Ok(Value::Object(o)) => o.filter(|o| now.contains(o)),
                _ => None,
            })
            .collect();
        for &b in &now {
            if slots.contains(&Some(b)) {
                continue;
            }
            if let Some(free) = slots.iter_mut().find(|s| s.is_none()) {
                *free = Some(b);
            }
        }
        for (i, slot) in slots.into_iter().enumerate() {
            self.set_prop_at(a, "Touching", i as u32, Value::Object(slot))?;
        }
        self.touching.insert(a, now);
        Ok(())
    }

    /// Every actor that takes part in overlaps, with its shape, gathered once a tick.
    fn collidable_actors(&mut self) -> Vec<Collider> {
        let mut out = self.static_collision().colliders.clone();
        for i in 0..self.actors.len() {
            let a = self.actors[i];
            if self.is_deleted(a) || !matches!(self.get_prop_id(a, self.hot.collide_actors), Ok(Value::Bool(true))) {
                continue;
            }
            if matches!(self.get_prop_id(a, self.hot.is_static), Ok(Value::Bool(true))) {
                continue;
            }
            if let Ok(Some(c)) = self.collider(a) {
                out.push(c);
            }
        }
        out
    }

    /// An actor's overlap shape. A `CT_Box` is the same turned box it blocks with:
    /// `Trigger10` of `Adv1Willow`, which shuts the planks behind Harry, is 200 long and 8
    /// wide, and taken as a cylinder of radius 200 it reached back past the planks and shut
    /// them in front of him.
    fn collider(&mut self, a: ObjectKey) -> crate::VmResult<Option<Collider>> {
        let Some((at, radius, height)) = self.cylinder(a)? else { return Ok(None) };
        let boxed = match self.get_prop_id(a, self.hot.collision_width)? {
            Value::Float(w) if w > 0.0 && self.collide_type(a) == CT_BOX => {
                let rotation = self.rot3_id(a, self.hot.rotation).unwrap_or([0; 3]);
                Some((w, rotation[1] as f32 / 32768.0 * std::f32::consts::PI))
            }
            _ => None,
        };
        Ok(Some(Collider { actor: a, at, radius, height, boxed }))
    }

    /// Actors that stand in the way of something moving: the props, the movers and the blocking
    /// volumes the level is built with. An actor only blocks what it collides with at all.
    pub fn find_blocking_actors(&mut self) -> Vec<Blocker> {
        self.roles.clear();
        let fixed = self.static_collision();
        let mut out = fixed.blockers.clone();
        for &(a, role) in &fixed.roles {
            self.roles.insert(a, role);
        }
        for i in 0..self.actors.len() {
            let a = self.actors[i];
            if self.is_deleted(a) || !matches!(self.get_prop_id(a, self.hot.collide_actors), Ok(Value::Bool(true))) {
                continue;
            }
            if matches!(self.get_prop_id(a, self.hot.is_static), Ok(Value::Bool(true))) {
                continue;
            }
            let role = self.role(a);
            self.roles.insert(a, role);
            if !role.block_actors && !role.block_players {
                continue;
            }
            // What blocks the way only changes when it moves, and most of a level never does,
            // so a shape is built once and kept until its actor is somewhere else.
            let at = self.vec3_id(a, self.hot.location).unwrap_or([0.0; 3]);
            let rot = self.rot3_id(a, self.hot.rotation).unwrap_or([0; 3]);
            if let Some((place, blocker)) = self.blocker_cache.get(&a) {
                if *place == (at, rot) {
                    out.push(Blocker { role, ..blocker.clone() });
                    continue;
                }
            }
            if let Some(shape) = self.blocking_shape(a) {
                let bounds = shape.bounds();
                let blocker = Blocker { actor: a, role, shape, bounds };
                self.blocker_cache.insert(a, ((at, rot), blocker.clone()));
                out.push(blocker);
            }
        }
        out
    }

    /// The collision of the level's `bStatic` actors, worked out the first time it is asked
    /// for. Unreal's `bStatic` is an actor that "does not move or change over time"; over half
    /// of a map's actors carry it, and going over them every tick was most of a tick's
    /// collision work.
    fn static_collision(&mut self) -> std::sync::Arc<StaticCollision> {
        if let Some(found) = &self.static_collision {
            return found.clone();
        }
        let mut fixed = StaticCollision::default();
        for i in 0..self.actors.len() {
            let a = self.actors[i];
            if self.is_deleted(a)
                || !matches!(self.get_prop_id(a, self.hot.collide_actors), Ok(Value::Bool(true)))
                || !matches!(self.get_prop_id(a, self.hot.is_static), Ok(Value::Bool(true)))
            {
                continue;
            }
            let role = self.role(a);
            fixed.roles.push((a, role));
            if let Ok(Some(c)) = self.collider(a) {
                fixed.colliders.push(c);
            }
            if role.block_actors || role.block_players {
                if let Some(shape) = self.blocking_shape(a) {
                    let bounds = shape.bounds();
                    fixed.blockers.push(Blocker { actor: a, role, shape, bounds });
                }
            }
        }
        let fixed = std::sync::Arc::new(fixed);
        self.static_collision = Some(fixed.clone());
        fixed
    }

    /// An actor's say in what stops what.
    fn role(&mut self, a: ObjectKey) -> Role {
        let flag = |v: crate::VmResult<Value>| matches!(v, Ok(Value::Bool(true)));
        Role {
            block_actors: flag(self.get_prop_id(a, self.hot.block_actors)),
            block_players: flag(self.get_prop_id(a, self.hot.block_players)),
            player: flag(self.get_prop_id(a, self.hot.is_player)),
            collides: flag(self.get_prop_id(a, self.hot.collide_actors)),
        }
    }

    /// Puts a blocker where its actor is now. The list is built once at the start of a tick,
    /// and an actor put somewhere else in the middle of it — a cutscene teleports several in a
    /// row — was left standing where it had been. `00001PrivetIntroPart2` shows it: in one tick
    /// it sends `LockHarry` from `cutmark45` to `CutMark60` and then `Lockhart` to `cutmark42`,
    /// 23 units from where `LockHarry` no longer was; the teleport failed at every height it
    /// tried, and the walk after it held the scene still for twenty seconds.
    pub fn refresh_blocker(&mut self, a: ObjectKey) {
        let Some(i) = self.blockers.iter().position(|b| b.actor == a) else { return };
        match self.blocking_shape(a) {
            Some(shape) => {
                let bounds = shape.bounds();
                let role = self.role(a);
                self.blockers[i] = Blocker { actor: a, role, shape, bounds };
            }
            None => {
                self.blockers.remove(i);
            }
        }
    }

    /// What an actor blocks the way with. A mover is a piece of geometry, and what it stops is
    /// its own shape: a door standing open lets you through the doorway it leaves, and a
    /// staircase is climbed step by step. Everything else is the box it carries.
    fn blocking_shape(&mut self, a: ObjectKey) -> Option<Shape> {
        let at = crate::vm::floats3(&self.get_prop_id(a, self.hot.location).ok()?).ok()?;
        let rotation = self.get_prop(a, "Rotation").and_then(|v| crate::vm::ints3(&v)).unwrap_or([0; 3]);
        if let Ok(Value::Object(Some(brush))) = self.get_prop(a, "Brush") {
            if let Some(bsp) = self.brush_bsp(brush) {
                let pivot = self.get_prop(a, "PrePivot").and_then(|v| crate::vm::floats3(&v)).unwrap_or([0.0; 3]);
                // Without a box of its own there is nothing to reject it cheaply by, so it
                // takes one big enough to hold anything: it is then always looked at properly.
                let local = self.brush_box(brush).unwrap_or(([0.0; 3], [f32::MAX / 4.0; 3]));
                return Some(Shape::Brush { brush, bsp, at, axes: crate::natives::core::axes(rotation), pivot, local });
            }
        }
        let (centre, half) = self.collision_box(a).ok()??;
        // The game blocks the way with long thin boxes turned to face where they are needed,
        // so which way one points is part of where it is. A cylinder does not turn.
        let yaw = match self.collide_type(a) {
            CT_BOX | CT_ORIENTED_OVAL => rotation[1] as f32 / 32768.0 * std::f32::consts::PI,
            _ => 0.0,
        };
        Some(Shape::Box { centre, half, yaw })
    }

    /// The tree of a brush, built once. A mover's brush carries the same nodes as the level.
    fn brush_bsp(&mut self, brush: ObjectKey) -> Option<std::sync::Arc<grim_world::Bsp>> {
        if let Some(found) = self.brush_bsps.get(&brush) {
            return found.clone();
        }
        let pkg = self.world.package(brush.package).clone();
        let built = match grim_assets::load_export(&pkg, brush.export).ok().map(|l| l.asset) {
            Some(grim_assets::Asset::Model(m)) if !m.nodes.is_empty() => {
                Some(std::sync::Arc::new(grim_world::Bsp::new(&m)))
            }
            _ => None,
        };
        self.brush_bsps.insert(brush, built.clone());
        built
    }

    /// Where a move that ran into something ends: a fixed distance short of the surface
    /// rather than a fraction of the move. A fraction leaves a long move well clear and a
    /// short one all but touching, and a box left touching a wall starts its next trace inside
    /// it, which is how an actor ends up unable to move at all.
    fn stop_short(from: [f32; 3], to: [f32; 3], time: f32) -> [f32; 3] {
        /// How much room to leave against whatever was hit.
        const SKIN: f32 = 0.5;
        let length = grim_world::sub(to, from);
        let length = (length[0] * length[0] + length[1] * length[1] + length[2] * length[2]).sqrt();
        let back = if length > 1e-6 { SKIN / length } else { 1.0 };
        grim_world::lerp(from, to, (time - back).max(0.0))
    }

    /// Whether a box at a place is inside the level's geometry.
    pub fn inside_level(&self, at: [f32; 3], extent: [f32; 3]) -> bool {
        self.bsp.as_ref().is_some_and(|b| b.box_solid(at, extent))
    }

    /// Where an actor stands, made good: normally where it is, and when that is inside the
    /// level, the nearest way out, or failing that the last place it stood clear. Going back
    /// is what keeps an actor that ends up buried from being pushed on through the wall.
    fn unstuck(&mut self, a: ObjectKey, at: [f32; 3], extent: [f32; 3], limit: f32) -> [f32; 3] {
        if !self.inside_level(at, extent) {
            self.safe_spots.insert(a, at);
            return at;
        }
        let out = self.free_above(at, extent, limit);
        if out != at {
            return out;
        }
        match self.safe_spots.get(&a) {
            Some(&back) if !self.inside_level(back, extent) => back,
            _ => at,
        }
    }

    /// Lifts a place until the box at it is clear of the level, giving up beyond `limit`. Only
    /// upwards: pushing it sideways could put it through a thin wall, which is the very thing
    /// being avoided. Walking up a slope or a mover coming down on it is enough to bury one.
    fn free_above(&self, at: [f32; 3], extent: [f32; 3], limit: f32) -> [f32; 3] {
        if !self.inside_level(at, extent) {
            return at;
        }
        let mut lift = 1.0;
        while lift <= limit {
            let up = [at[0], at[1], at[2] + lift];
            if !self.inside_level(up, extent) {
                return up;
            }
            lift += 1.0;
        }
        at
    }

    /// The first thing a moving box runs into: the level itself, or one of the actors blocking
    /// the way. Gives back how far it got, the surface it met and the actor, if it was one.
    fn sweep(
        &self,
        mover: ObjectKey,
        from: [f32; 3],
        to: [f32; 3],
        extent: [f32; 3],
        blockers: &[Blocker],
    ) -> Option<(f32, [f32; 3], Option<ObjectKey>)> {
        self.sweep_past(mover, None, from, to, extent, blockers)
    }

    /// The same, leaving one more actor out of the way.
    fn sweep_past(
        &self,
        mover: ObjectKey,
        past: Option<ObjectKey>,
        from: [f32; 3],
        to: [f32; 3],
        extent: [f32; 3],
        blockers: &[Blocker],
    ) -> Option<(f32, [f32; 3], Option<ObjectKey>)> {
        let mut best = self.bsp.as_ref().and_then(|b| b.box_check(from, to, extent)).map(|h| (h.time, h.normal, None));
        // The box the move itself sweeps out, to throw away in six comparisons everything that
        // is nowhere near it: a level holds hundreds of blockers and a step touches a handful.
        let low = [0, 1, 2].map(|i| from[i].min(to[i]) - extent[i]);
        let high = [0, 1, 2].map(|i| from[i].max(to[i]) + extent[i]);
        let role = self.roles.get(&mover).copied().unwrap_or(Role::NONE);
        for blocker in blockers {
            if blocker.actor == mover || past == Some(blocker.actor) {
                continue;
            }
            // A mover's brush stops whatever it meets; everything else has to agree with what
            // is moving.
            if !matches!(blocker.shape, Shape::Brush { .. }) && !role.stopped_by(&blocker.role) {
                continue;
            }
            let (b_low, b_high) = blocker.bounds;
            if (0..3).any(|i| b_low[i] > high[i] || b_high[i] < low[i]) {
                continue;
            }
            if let Some((t, n)) = blocker.shape.hit(from, to, extent) {
                if best.as_ref().is_none_or(|b| t < b.0) {
                    best = Some((t, n, Some(blocker.actor)));
                }
            }
        }
        best
    }

    /// An actor's collision cylinder: centre and half extents, when it has one.
    /// Where an actor's collision box sits and how big it is. A mover is a piece of geometry
    /// rather than a prop, so its own brush says where it is solid; the default cylinder a
    /// mover carries is a 160 unit cube that has nothing to do with the door it draws.
    /// Hypothesis: the brush's box is taken as it is stored, without the actor's rotation.
    pub fn collision_box(&mut self, a: ObjectKey) -> crate::VmResult<Option<([f32; 3], [f32; 3])>> {
        let Some((at, ..)) = self.cylinder(a)? else { return Ok(None) };
        if let Ok(Value::Object(Some(brush))) = self.get_prop(a, "Brush") {
            if let Some((centre, half)) = self.brush_box(brush) {
                // The brush is stored around its own pivot and the actor stands at it, which
                // is the same road its drawing takes.
                let pivot = self.get_prop(a, "PrePivot").and_then(|v| crate::vm::floats3(&v)).unwrap_or([0.0; 3]);
                return Ok(Some(([0, 1, 2].map(|i| at[i] + centre[i] - pivot[i]), half)));
            }
        }
        let Some(half) = self.collision_extent(a)? else { return Ok(None) };
        Ok(Some((at, half)))
    }

    /// The box a brush model takes up, around its own origin.
    fn brush_box(&mut self, brush: ObjectKey) -> Option<([f32; 3], [f32; 3])> {
        if let Some(found) = self.brush_boxes.get(&brush) {
            return *found;
        }
        let pkg = self.world.package(brush.package).clone();
        let found = match grim_assets::load_export(&pkg, brush.export).ok().map(|l| l.asset) {
            Some(grim_assets::Asset::Model(m)) if m.primitive.bounding_box.is_valid => {
                let b = m.primitive.bounding_box;
                let centre = [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0, (b.min.z + b.max.z) / 2.0];
                let half = [(b.max.x - b.min.x) / 2.0, (b.max.y - b.min.y) / 2.0, (b.max.z - b.min.z) / 2.0];
                (half.iter().all(|&h| h > 0.0)).then_some((centre, half))
            }
            _ => None,
        };
        self.brush_boxes.insert(brush, found);
        found
    }

    /// The half extents of an actor's collision box. This engine gives an actor a width of its
    /// own besides its radius, so a table is a box rather than a circle; without one the
    /// footprint is square on the radius, which is what a cylinder amounts to here.
    ///
    /// Which one depends on `CollideType`, as `Actor` documents it: `CT_Box` is a box of
    /// `CollisionRadius` by `CollisionWidth` by `CollisionHeight`; the oval cylinders take
    /// `CollisionWidth` as their radius along X; the default cylinder uses it as a vertical
    /// offset instead, so for it, and for anything else, the footprint stands on the radius
    /// alone.
    pub fn collision_extent(&mut self, a: ObjectKey) -> crate::VmResult<Option<[f32; 3]>> {
        let Some((_, radius, height)) = self.cylinder(a)? else { return Ok(None) };
        let width = match self.get_prop_id(a, self.hot.collision_width) {
            Ok(Value::Float(w)) if w > 0.0 => w,
            _ => radius,
        };
        Ok(Some(match self.collide_type(a) {
            CT_BOX => [radius, width, height],
            CT_ALIGNED_OVAL | CT_ORIENTED_OVAL => [width, radius, height],
            _ => [radius, radius, height],
        }))
    }

    /// An actor's `CollideType`.
    pub fn collide_type(&mut self, a: ObjectKey) -> u8 {
        match self.get_prop_id(a, self.hot.collide_type) {
            Ok(Value::Byte(t)) => t,
            _ => 0,
        }
    }

    fn cylinder(&mut self, a: ObjectKey) -> crate::VmResult<Option<([f32; 3], f32, f32)>> {
        let (Value::Float(radius), Value::Float(height)) =
            (self.get_prop_id(a, self.hot.collision_radius)?, self.get_prop_id(a, self.hot.collision_height)?)
        else {
            return Ok(None);
        };
        if radius <= 0.0 && height <= 0.0 {
            return Ok(None);
        }
        let Some(at) = self.vec3_id(a, self.hot.location) else { return crate::vm::fail("Location") };
        Ok(Some((at, radius, height)))
    }

    /// `sweep`, except that a box starting inside the level is not held up by it. Every trace
    /// out of solid space meets a surface at once, so an actor that ends up inside something
    /// would stay there for ever: it could neither move nor fall, which is what being stuck in
    /// a stair or a wall looks like. What blocks actors still blocks it.
    fn sweep_out(
        &self,
        mover: ObjectKey,
        from: [f32; 3],
        to: [f32; 3],
        extent: [f32; 3],
        blockers: &[Blocker],
    ) -> Option<(f32, [f32; 3], Option<ObjectKey>)> {
        // Only a move that gets it out is let through. Ignoring the level for any move would
        // let an actor that is briefly inside something walk on through it.
        if !self.inside_level(from, extent) || self.inside_level(to, extent) {
            return self.sweep(mover, from, to, extent, blockers);
        }
        let mut best: Option<(f32, [f32; 3], Option<ObjectKey>)> = None;
        let role = self.roles.get(&mover).copied().unwrap_or(Role::NONE);
        for blocker in blockers.iter().filter(|b| b.actor != mover) {
            if !matches!(blocker.shape, Shape::Brush { .. }) && !role.stopped_by(&blocker.role) {
                continue;
            }
            if let Some((t, n)) = blocker.shape.hit(from, to, extent) {
                if best.as_ref().is_none_or(|b| t < b.0) {
                    best = Some((t, n, Some(blocker.actor)));
                }
            }
        }
        best
    }

    /// Moves `from` by `step`, sliding along whatever it runs into.
    /// Moves `from` by `step`, sliding along whatever it runs into.
    fn slide(&self, mover: ObjectKey, from: [f32; 3], step: [f32; 3], extent: [f32; 3], blockers: &[Blocker]) -> [f32; 3] {
        self.slide_bumping(mover, from, step, extent, blockers, &mut Vec::new())
    }

    /// Moves `from` by `step`, sliding along whatever it runs into, and says what it ran into
    /// on the way: an actor that is bumped hears about it, which is how a door opens when
    /// somebody walks into it.
    fn slide_bumping(
        &self,
        mover: ObjectKey,
        from: [f32; 3],
        step: [f32; 3],
        extent: [f32; 3],
        blockers: &[Blocker],
        bumped: &mut Vec<ObjectKey>,
    ) -> [f32; 3] {
        // Going nowhere runs into nothing.
        if step[0].abs() + step[1].abs() + step[2].abs() < 1e-6 {
            return from;
        }
        let mut at = from;
        let mut left = step;
        for _ in 0..3 {
            let to = grim_world::add(at, left);
            let hit = self.sweep_out(mover, at, to, extent, blockers);
            let Some((time, normal, met)) = hit else { return to };
            if let Some(met) = met {
                if !bumped.contains(&met) {
                    bumped.push(met);
                }
            }
            at = Self::stop_short(at, to, time);
            // Keep the part of the move that runs along the wall.
            let into = grim_world::dot(left, normal);
            left = grim_world::sub(grim_world::scale(left, 1.0 - time), grim_world::scale(normal, into * (1.0 - time)));
            if grim_world::dot(left, left) < 1e-4 {
                break;
            }
        }
        at
    }

    /// A ledge in front of an actor it could climb onto, as the offset from where it stands to
    /// where it would end up. The pawns carry `MaxMountHeight`, which only Harry has a value
    /// for (96.5), and the climb animations say the rest: they carry him 30 units forward, so
    /// that is where the top has to hold him.
    ///
    /// Hypothesis: `Pawn.Mount` is an event of the game's own engine, declared next to
    /// `Landed` and called by no script of the game, so the engine is what looks for the ledge.
    /// What it hands over is read back from `harry.Mounting`: the distance from here to there,
    /// out of which the state takes the 30 forward and the 32, 64 or 82 up that the animation
    /// itself covers, and moves what is left over by hand.
    fn mount_target(&mut self, a: ObjectKey, dir: [f32; 3]) -> crate::VmResult<Option<[f32; 3]>> {
        /// How far in front of itself the climb animations put the actor.
        const REACH: f32 = 30.0;
        /// How far up a surface has to point to be something to stand on.
        const FLOOR_NORMAL: f32 = 0.7;
        let max = match self.get_prop(a, "MaxMountHeight") {
            Ok(Value::Float(v)) if v > 0.0 => v,
            _ => return Ok(None),
        };
        let max_step = match self.get_prop_id(a, self.hot.max_step_height) {
            Ok(Value::Float(v)) if v > 0.0 => v,
            _ => 25.0,
        };
        let Some((from, radius, height)) = self.cylinder(a)? else { return Ok(None) };
        let extent = [radius, radius, height];
        let len = (dir[0] * dir[0] + dir[1] * dir[1]).sqrt();
        if len < 1e-3 {
            return Ok(None);
        }
        let dir = [dir[0] / len, dir[1] / len, 0.0];
        let Some(bsp) = self.bsp.as_ref() else { return Ok(None) };
        // What is climbed is the level itself. A chest or a table is an actor standing on the
        // floor, not a ledge of the place, and walking into one is not a climb.
        let ahead = [from[0] + dir[0] * REACH, from[1] + dir[1] * REACH, from[2]];
        if bsp.box_check(from, ahead, extent).is_none() {
            return Ok(None);
        }
        let feet = from[2] - height;
        // The face in front, read where it is: a line ahead just over what the walk steps up
        // on its own. The box that met the wall may have met it at an edge, whose bevel
        // carries none of the flags of the face beside it, and the climb was refused there.
        // On a ramp or a flight of stairs that line runs into the ground rising ahead before it
        // reaches the wall, so it is taken again from over the ground it met.
        let mut knee = feet + max_step + 1.0;
        let reach = REACH + radius;
        let mut face = None;
        for _ in 0..4 {
            let Some(hit) = bsp.line_check([from[0], from[1], knee], [from[0] + dir[0] * reach, from[1] + dir[1] * reach, knee]) else {
                break;
            };
            if hit.normal[2] <= FLOOR_NORMAL {
                face = Some(hit);
                break;
            }
            knee = hit.location[2] + max_step + 1.0;
        }
        let Some(face) = face else { return Ok(None) };
        // And only where the level says it can be climbed. Hypothesis, from comparing what the
        // game lets Harry up with what it does not: the surfaces of the stepped blocks in the
        // room the map's own states call `ClimbingWallRoom` (Adv3DungeonQuest) carry this flag
        // and nothing else does; the armour stands of `EntryHall_hub` and the statue plinth of
        // `Grounds_hub`, which the game does not let him climb, carry no flags at all; and
        // `PrivetDr`, where nothing is climbed, has not one surface with it. It is the bit
        // Unreal calls a special poly, which this game gave a meaning of its own.
        const PF_CLIMBABLE: u32 = 0x0000_1000;
        if bsp.flags_of(&face) & PF_CLIMBABLE == 0 {
            return Ok(None);
        }
        // The top of the ledge, straight down where the climb puts him: higher than he can
        // reach means the start of that line is inside the wall.
        let probe = [ahead[0], ahead[1], feet + max];
        if bsp.point_solid(probe) {
            return Ok(None);
        }
        let Some(top) = bsp.line_check(probe, [ahead[0], ahead[1], feet]) else { return Ok(None) };
        if top.normal[2] <= FLOOR_NORMAL {
            return Ok(None);
        }
        // Standing room on it is all that is asked. Asking for room at the full reach above
        // it, as this did, refused a low step under a ceiling or in front of a higher one.
        let landing = [ahead[0], ahead[1], top.location[2] + height];
        if bsp.box_solid(landing, extent) {
            return Ok(None);
        }
        let rise = top.location[2] - feet;
        // Below a step it is not a climb, and the walk already goes up it.
        if rise <= max_step || rise > max {
            return Ok(None);
        }
        Ok(Some(grim_world::sub(landing, from)))
    }

    /// Whether an actor is heading the way it faces, which is what walking forward means.
    fn facing(&mut self, a: ObjectKey, heading: [f32; 3]) -> crate::VmResult<bool> {
        let size = (heading[0] * heading[0] + heading[1] * heading[1]).sqrt();
        if size < 1e-3 {
            return Ok(false);
        }
        let rotation = crate::vm::ints3(&self.get_prop(a, "Rotation")?)?;
        let axes = crate::natives::core::axes(rotation);
        Ok((heading[0] * axes[0][0] + heading[1] * axes[0][1]) / size > 0.7)
    }

    /// Tells an actor to climb onto a ledge it is up against, if there is one it can reach.
    fn try_mount(&mut self, a: ObjectKey, dir: [f32; 3]) -> crate::VmResult<bool> {
        let Some(delta) = self.mount_target(a, dir)? else { return Ok(false) };
        if self.switches.trace_mount {
            let at = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
            eprintln!("mount from {at:?} by {delta:?}");
        }
        self.call_event(a, "Mount", vec![crate::vm::vector(delta[0], delta[1], delta[2])])?;
        Ok(true)
    }

    /// `PHYS_Projectile`: what a spell moves by. It carries on along its velocity, with no
    /// weight of its own, until it runs into something; what it ran into is told about it the
    /// way the scripts expect, which is how a spell opens a chest.
    fn project(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_PROJECTILE: u8 = 6;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_PROJECTILE)) {
            return Ok(());
        }
        let velocity = crate::vm::floats3(&self.get_prop_id(a, self.hot.velocity)?)?;
        if velocity.iter().all(|v| v.abs() < 1e-4) {
            return Ok(());
        }
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let to = grim_world::add(from, grim_world::scale(velocity, dt));
        let extent = match self.collision_extent(a)? {
            Some(e) => e,
            None => [0.0; 3],
        };
        // A spell is born inside the hand that casts it, so whoever owns it is not in its way:
        // without this it hits its caster on the frame it is spawned and never flies, which is
        // what `baseSpell.ProcessTouch` was being handed in `EntryHall_hub`.
        let owner = match self.get_prop(a, "Owner") {
            Ok(Value::Object(o)) => o,
            _ => None,
        };
        let blockers = std::mem::take(&mut self.blockers);
        let hit = self.sweep_past(a, owner, from, to, extent, &blockers);
        self.blockers = blockers;
        match hit {
            None => self.set_prop_id(a, self.hot.location, crate::vm::vector(to[0], to[1], to[2]))?,
            Some((time, n, other)) => {
                let stop = Self::stop_short(from, to, time);
                self.set_prop_id(a, self.hot.location, crate::vm::vector(stop[0], stop[1], stop[2]))?;
                let mover = other.filter(|&o| matches!(self.get_prop(o, "bIsMover"), Ok(Value::Bool(true))));
                match other {
                    // A mover is solid to it, like the level: they bump into each other, and
                    // the projectile hits it as a wall. `GridMover.BumpMove.Bump` is how a
                    // Flipendo pushes the willow's blocks (`baseSpell.HitWall` only makes the
                    // sparks for one); told as a touch, the spell stopped against the block and
                    // hung there.
                    Some(other) if mover.is_some() => {
                        self.call_event(a, "Bump", vec![Value::Object(Some(other))])?;
                        if !self.is_deleted(other) {
                            self.call_event(other, "Bump", vec![Value::Object(Some(a))])?;
                        }
                        if !self.is_deleted(a) {
                            self.call_event(a, "HitWall", vec![crate::vm::vector(n[0], n[1], n[2]), Value::Object(Some(other))])?;
                        }
                    }
                    Some(other) => {
                        // Both sides are told, which is what an overlap does: the projectile's
                        // own `Touch` is what calls `ProcessTouch`, and the other actor's is
                        // what fires whatever it is there for. A spell reaches a secret door
                        // through the `spellTrigger` beside it, and that only opens if the
                        // trigger itself hears about the touch.
                        self.call_event(a, "Touch", vec![Value::Object(Some(other))])?;
                        if !self.is_deleted(other) {
                            self.call_event(other, "Touch", vec![Value::Object(Some(a))])?;
                        }
                    }
                    None => {
                        let level = Value::Object(self.level_info);
                        self.call_event(a, "HitWall", vec![crate::vm::vector(n[0], n[1], n[2]), level])?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `PHYS_Flying`: the actor moves under its own `Acceleration`, with no gravity and
    /// nothing to stand on. Unreal turns the velocity towards the acceleration and lets it
    /// drift back when there is none, holds it under `AirSpeed`, and slides along whatever it
    /// runs into. The pixies, the quidditch players, Hedwig and Harry on a broom all fly.
    fn fly(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_FLYING: u8 = 4;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_FLYING)) {
            return Ok(());
        }
        let float_of = |v: crate::VmResult<Value>| match v {
            Ok(Value::Float(f)) => f,
            _ => 0.0,
        };
        let mut accel = crate::vm::floats3(&self.get_prop(a, "Acceleration")?)?;
        let rate = float_of(self.get_prop_id(a, self.hot.accel_rate));
        let size = grim_world::dot(accel, accel).sqrt();
        if rate > 0.0 && size > rate {
            accel = grim_world::scale(accel, rate / size);
        }
        let mut velocity = crate::vm::floats3(&self.get_prop_id(a, self.hot.velocity)?)?;
        // How quickly it gives up the way it was going for the way it is pushed. Unreal takes
        // this from the zone's own fluid friction; the flying pawns of this game are in air.
        let friction = 0.5;
        for i in 0..3 {
            velocity[i] += (accel[i] - velocity[i] * friction) * dt;
        }
        let top = float_of(self.get_prop(a, "AirSpeed"));
        if top > 0.0 {
            let speed = grim_world::dot(velocity, velocity).sqrt();
            if speed > top {
                velocity = grim_world::scale(velocity, top / speed);
            }
        }
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let step = grim_world::scale(velocity, dt);
        if grim_world::dot(step, step) < 1e-8 {
            self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
            return Ok(());
        }
        let extent = self.collision_extent(a)?.unwrap_or([0.0; 3]);
        let to = grim_world::add(from, step);
        let blockers = std::mem::take(&mut self.blockers);
        let hit = self.sweep_out(a, from, to, extent, &blockers);
        self.blockers = blockers;
        match hit {
            None => {
                self.set_prop_id(a, self.hot.location, crate::vm::vector(to[0], to[1], to[2]))?;
                self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
            }
            Some((time, n, _)) => {
                let stop = Self::stop_short(from, to, time);
                // What is left of the move carries on along the surface, as a fall does.
                let left = grim_world::sub(to, stop);
                let into = grim_world::dot(left, n);
                let along = grim_world::sub(left, grim_world::scale(n, into));
                let blockers = std::mem::take(&mut self.blockers);
                let at = self.slide(a, stop, along, extent, &blockers);
                self.blockers = blockers;
                self.set_prop_id(a, self.hot.location, crate::vm::vector(at[0], at[1], at[2]))?;
                let into = grim_world::dot(velocity, n);
                let slide = grim_world::sub(velocity, grim_world::scale(n, into));
                self.set_prop_id(a, self.hot.velocity, crate::vm::vector(slide[0], slide[1], slide[2]))?;
                let level = Value::Object(self.level_info);
                self.call_event(a, "HitWall", vec![crate::vm::vector(n[0], n[1], n[2]), level])?;
            }
        }
        Ok(())
    }

    /// Gives back the height the stairs took off the drawn position. Only what is drawn walks
    /// the slope; the frame starts by putting the actor back on the step it really stands on,
    /// so everything that moves or measures it works from the ground and not from the slope.
    fn leave_stairs(&mut self, a: ObjectKey) -> crate::VmResult<()> {
        let Some(lag) = self.stair_lag.remove(&a) else { return Ok(()) };
        self.stair_was.insert(a, lag);
        let at = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        self.set_prop_id(a, self.hot.location, crate::vm::vector(at[0], at[1], at[2] + lag))
    }

    fn fall(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        const PHYS_NONE: u8 = 0;
        const PHYS_WALKING: u8 = 1;
        const PHYS_FALLING: u8 = 2;
        /// How far up a surface has to point to be a floor rather than a wall.
        const FLOOR_NORMAL: f32 = 0.7;
        if !matches!(self.get_prop_id(a, self.hot.physics)?, Value::Byte(PHYS_FALLING)) {
            return Ok(());
        }
        let gravity = self.zone_gravity(a)?;
        let mut velocity = crate::vm::floats3(&self.get_prop_id(a, self.hot.velocity)?)?;
        // Steering in the air: what a pawn asks to accelerate by still counts, scaled down by
        // its `AirControl` (Harry's is 0.25), which is how a jump can be bent a little. Only
        // across: gravity alone decides the height. Hypothesis: the steering cannot take it
        // faster across than it walks, unless it was already going faster than that (a jump
        // off a run keeps its speed).
        let air = match self.get_prop(a, "AirControl") {
            Ok(Value::Float(c)) if c > 0.0 => c,
            _ => 0.0,
        };
        if air > 0.0 {
            if let Ok(accel) = self.get_prop(a, "Acceleration").and_then(|v| crate::vm::floats3(&v)) {
                let before = (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt();
                for i in 0..2 {
                    velocity[i] += accel[i] * air * dt;
                }
                let limit = match self.get_prop(a, "GroundSpeed") {
                    Ok(Value::Float(g)) if g > 0.0 => g.max(before),
                    _ => before,
                };
                let after = (velocity[0] * velocity[0] + velocity[1] * velocity[1]).sqrt();
                if after > limit && after > 0.0 {
                    for v in velocity.iter_mut().take(2) {
                        *v *= limit / after;
                    }
                }
            }
        }
        for i in 0..3 {
            velocity[i] += gravity[i] * dt;
        }
        let from = crate::vm::floats3(&self.get_prop_id(a, self.hot.location)?)?;
        let (radius, height) = match (self.get_prop_id(a, self.hot.collision_radius)?, self.get_prop_id(a, self.hot.collision_height)?) {
            (Value::Float(r), Value::Float(h)) => (r, h),
            _ => (0.0, 0.0),
        };
        let extent = [radius, radius, height];
        // Whatever put it inside the level, it leaves before it falls any further: a trace out
        // of solid space meets a surface at once and the fall would never end.
        let from = self.unstuck(a, from, extent, height);
        let to = grim_world::add(from, grim_world::scale(velocity, dt));
        let blockers = std::mem::take(&mut self.blockers);
        let hit = self.sweep_out(a, from, to, extent, &blockers);
        self.blockers = blockers;
        if self.switches.trace_fall {
            eprintln!("fall {from:?} -> {to:?} extent {extent:?} hit {:?}", hit.map(|h| (h.0, h.1, h.2.is_some())));
        }
        match hit {
            None => {
                self.set_prop_id(a, self.hot.location, crate::vm::vector(to[0], to[1], to[2]))?;
                self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
            }
            Some((time, mut n, mut on)) => {
                let mut stop = Self::stop_short(from, to, time);
                // Hypothesis: a fall ends on a floor, taken as a surface leaning up more than
                // sideways; a wall only sends it sliding, still falling. Jumping into one and
                // coming to rest against it is what told this apart.
                // What is left of the move carries on along a wall. Stopping dead at it
                // instead leaves an actor pressed against a wall going nowhere: the next
                // frame it runs into the same wall right away and never falls past it.
                // Down along the wall it may come onto a floor, and then it has landed: a jump
                // onto a flight of stairs meets a riser first and the step under it straight
                // after. Taken as the end of a slide and not as a landing, the fall went on for
                // ever, the riser met again every frame.
                if n[2] <= FLOOR_NORMAL {
                    let left = grim_world::sub(to, stop);
                    let into = grim_world::dot(left, n);
                    let along = grim_world::add(stop, grim_world::sub(left, grim_world::scale(n, into)));
                    let blockers = std::mem::take(&mut self.blockers);
                    let floor = self.sweep_out(a, stop, along, extent, &blockers).filter(|h| h.1[2] > FLOOR_NORMAL);
                    self.blockers = blockers;
                    if let Some((t, floor_normal, under)) = floor {
                        stop = Self::stop_short(stop, along, t);
                        n = floor_normal;
                        on = under;
                    }
                }
                if n[2] <= FLOOR_NORMAL {
                    let left = grim_world::sub(to, stop);
                    let into = grim_world::dot(left, n);
                    let along = grim_world::sub(left, grim_world::scale(n, into));
                    let blockers = std::mem::take(&mut self.blockers);
                    let mut at = self.slide(a, stop, along, extent, &blockers);
                    // Where two faces of a wall meet, the slide can catch on the edge of the next
                    // one and go nowhere, and a jump into such a seam hung in the air: every
                    // frame the push across met the edge again and took the fall with it. A
                    // slide that gets nowhere lets the fall go on by itself, straight down,
                    // which beside a flat wall meets nothing.
                    let moved = grim_world::sub(at, stop);
                    if grim_world::dot(moved, moved) < 1e-6 && along[2] < 0.0 {
                        at = self.slide(a, stop, [0.0, 0.0, along[2]], extent, &blockers);
                    }
                    self.blockers = blockers;
                    self.set_prop_id(a, self.hot.location, crate::vm::vector(at[0], at[1], at[2]))?;
                    // Hitting a wall on the way up is how the high ledges are taken: jump at
                    // one and the engine finds the top within reach and climbs it.
                    if self.facing(a, [-n[0], -n[1], 0.0])? && self.try_mount(a, [-n[0], -n[1], 0.0])? {
                        return Ok(());
                    }
                    let into = grim_world::dot(velocity, n);
                    let slide = grim_world::sub(velocity, grim_world::scale(n, into));
                    self.set_prop_id(a, self.hot.velocity, crate::vm::vector(slide[0], slide[1], slide[2]))?;
                    let level = Value::Object(self.level_info);
                    self.call_event(a, "HitWall", vec![crate::vm::vector(n[0], n[1], n[2]), level])?;
                    return Ok(());
                }
                self.set_prop_id(a, self.hot.location, crate::vm::vector(stop[0], stop[1], stop[2]))?;
                // A pawn does not come to rest on one that will not have it: asked with
                // `PawnCantStandOnMe`, the characters and creatures say so (`HPawn` answers its
                // `bCantStandOnMe`, which `HChar` sets and the props do not), and the one landing
                // is sent off with `JumpOffPawn`, a push up and to a side, still falling. So Harry
                // cannot stand on anybody's head, and still can on a chest.
                let is_pawn = |vm: &mut Self, k: ObjectKey| matches!(vm.get_prop(k, "bIsPawn"), Ok(Value::Bool(true)));
                if let Some(under) = on {
                    let refuses = is_pawn(self, a)
                        && is_pawn(self, under)
                        && matches!(self.call_event(under, "PawnCantStandOnMe", vec![])?, Some(Value::Bool(true)));
                    if refuses {
                        self.call_event(a, "JumpOffPawn", vec![])?;
                        return Ok(());
                    }
                }
                // `bBounce` says the actor does not land: it is told it hit and goes on
                // falling, and the script is what turns the velocity around.
                // `HProp.BounceIntoPlace.HitWall` halves it and mirrors it about the surface,
                // which is how the beans a chest throws settle on the floor.
                if matches!(self.get_prop(a, "bBounce"), Ok(Value::Bool(true))) {
                    self.set_prop_id(a, self.hot.velocity, crate::vm::vector(velocity[0], velocity[1], velocity[2]))?;
                    let level = Value::Object(self.level_info);
                    self.call_event(a, "HitWall", vec![crate::vm::vector(n[0], n[1], n[2]), level])?;
                    // Nothing ends the bounce: the original's saves have the jellybeans lying on
                    // the floor still falling, going nowhere at a hair under one unit a second
                    // (`BlueJellyBean2` of the seventh checkpoint, 193 beans across twelve
                    // saves), never landed. Taking one as landed once its bounce was spent
                    // turned them all into walkers the original never has.
                    return Ok(());
                }
                self.set_prop_id(a, self.hot.velocity, crate::vm::vector(0.0, 0.0, 0.0))?;
                // The engine picks the mode the actor lands in before telling it it landed, so
                // a script's `Landed` can still change it. One that walks goes on walking;
                // anything else rests where it fell.
                let walks = self.can_walk(a)?;
                self.set_prop(a, "Physics", Value::Byte(if walks { PHYS_WALKING } else { PHYS_NONE }))?;
                self.call_event(a, "Landed", vec![crate::vm::vector(n[0], n[1], n[2])])?;
            }
        }
        Ok(())
    }

    /// Whether the level is stopped, which `LevelInfo.Pauser` says by naming who stopped it.
    pub fn paused(&mut self) -> bool {
        let Some(info) = self.level_info else { return false };
        matches!(self.get_prop(info, "Pauser"), Ok(Value::Str(who)) if !who.is_empty())
    }

    /// Counts an actor's life down and destroys it when it runs out. `LifeSpan` is how the
    /// game gets rid of what it spawns for a moment: a burst of particles, a sound, a decal.
    fn expire(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        let Ok(Value::Float(left)) = self.get_prop(a, "LifeSpan") else { return Ok(()) };
        if left <= 0.0 {
            return Ok(());
        }
        let left = left - dt;
        self.set_prop(a, "LifeSpan", Value::Float(left.max(0.0)))?;
        if left <= 0.0 {
            self.call_event(a, "Expired", vec![])?;
            if !self.is_deleted(a) {
                self.call_event(a, "Destroyed", vec![])?;
                self.set_prop(a, "bDeleteMe", Value::Bool(true))?;
                self.instance(a)?.deleted = true;
            }
        }
        Ok(())
    }

    /// Turns an actor the way it wants to face. `bRotateToDesired` asks the engine to bring
    /// `Rotation` around to `DesiredRotation` at `RotationRate`, which is how the scripts
    /// steer a pawn: they set where it should look and the engine walks it there. With
    /// `bFixedRotationDir` it simply keeps turning at that rate.
    fn turn(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        // Whatever physics moves the actor: a pawn walks and turns at once, and what turns
        // it is this. Harry is the case: `harry.PlayerWalking.PlayerMove` never sets his
        // rotation itself, it points `DesiredRotation` where the camera looks and leaves the
        // turning to the engine. The original's save of the first checkpoint agrees from the
        // other side: every jellybean at rest there faces yaw 0, its `DesiredRotation`, where
        // turning only under `PHYS_Rotating` left them spinning.
        let fixed = matches!(self.get_prop_id(a, self.hot.fixed_rotation), Ok(Value::Bool(true)));
        let to_desired = matches!(self.get_prop_id(a, self.hot.rotate_to_desired), Ok(Value::Bool(true)));
        if !fixed && !to_desired {
            return Ok(());
        }
        let Ok(rate) = self.get_prop_id(a, self.hot.rotation_rate).and_then(|v| crate::vm::ints3(&v)) else { return Ok(()) };
        let Ok(mut rot) = self.get_prop(a, "Rotation").and_then(|v| crate::vm::ints3(&v)) else { return Ok(()) };
        let step = rate.map(|r| (r as f32 * dt) as i32);
        if fixed {
            for i in 0..3 {
                rot[i] = rot[i].wrapping_add(step[i]);
            }
        } else {
            let Ok(want) = self.get_prop_id(a, self.hot.desired_rotation).and_then(|v| crate::vm::ints3(&v)) else { return Ok(()) };
            // Pitch and yaw only. The original's saves keep rolls its pawns were never
            // turned back from: a jellybean falling with roll 65536 and Filch walking with
            // 65537, both wanting 0 and both able to turn.
            for i in 0..2 {
                rot[i] = turn_towards_unit(rot[i], want[i], step[i]);
            }
        }
        self.set_prop(a, "Rotation", crate::vm::rotator(rot[0], rot[1], rot[2]))
    }

    /// Puts the game in the state a cutscene belongs to, so the actors it names are the ones
    /// the game would have around it. An actor's `Group` lists the game states it takes part
    /// in, and the player's `GameStateMasterList` is the order they come in; a cutscene of a
    /// later state is played with the game moved to it. Answers the state it moved to.
    pub fn enter_cutscene_state(&mut self, cut: ObjectKey, report: &mut Report) -> crate::VmResult<Option<String>> {
        let Some(player) = self.player else { return Ok(None) };
        let Value::Str(list) = self.get_prop(player, "GameStateMasterList")? else { return Ok(None) };
        let known: Vec<String> = list.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        let groups = match self.get_prop(cut, "Group")? {
            Value::Name(n) => self.world.names.str(n).to_string(),
            _ => return Ok(None),
        };
        let Some(state) = groups.split(',').map(str::trim).find_map(|g| known.iter().find(|t| t.eq_ignore_ascii_case(g))).cloned() else {
            return Ok(None);
        };
        if matches!(self.get_prop(player, "CurrentGameState")?, Value::Str(ref now) if now.eq_ignore_ascii_case(&state)) {
            return Ok(None);
        }
        self.set_prop(player, "CurrentGameState", Value::Str(state.clone()))?;
        self.screen_actors_by_game_state(report);
        Ok(Some(state))
    }

    /// Puts the player where a cutscene stands, which is where the game has it when the
    /// cutscene starts: the ones that start on a bump or a trigger are written for a player who
    /// has walked into them, and playing one from wherever the level begins makes its
    /// `WalkTo`s cross walls. Answers whether it moved anyone.
    pub fn place_player_at_cutscene(&mut self, cut: ObjectKey) -> crate::VmResult<bool> {
        let bump = matches!(self.get_prop(cut, "bBumpStarts"), Ok(Value::Bool(true)));
        let trigger = matches!(self.get_prop(cut, "bTriggerStarts"), Ok(Value::Bool(true)));
        let Some(player) = self.player else { return Ok(false) };
        if !bump && !trigger {
            return Ok(false);
        }
        let at = crate::vm::floats3(&self.get_prop(cut, "Location")?)?;
        let (radius, height) = match self.cylinder(player)? {
            Some((_, r, h)) => (r, h),
            None => (17.0, 42.0),
        };
        // The cutscene's own spot may be in the floor: the player is dropped in from above it
        // and left on the first height where it fits.
        let extent = [radius, radius, height];
        let free = (0..16)
            .map(|i| [at[0], at[1], at[2] + i as f32 * 8.0])
            .find(|&p| !self.bsp.as_ref().is_some_and(|b| b.box_solid(p, extent)));
        let Some(spot) = free else { return Ok(false) };
        self.set_prop(player, "Location", crate::vm::vector(spot[0], spot[1], spot[2]))?;
        self.set_prop(player, "Velocity", crate::vm::vector(0.0, 0.0, 0.0))?;
        Ok(true)
    }

    /// Whether an actor moves on foot: `bCanWalk` is the pawns' own answer, and only they
    /// declare it.
    fn can_walk(&mut self, a: ObjectKey) -> crate::VmResult<bool> {
        Ok(matches!(self.get_prop(a, "bCanWalk"), Ok(Value::Bool(true))))
    }

    /// The region a point of the level is in: its zone's `ZoneInfo`, the BSP leaf and the
    /// zone number, as `PointRegion` lays them out. A zone the map gives no `ZoneInfo` of its
    /// own is the level's, which is what `LevelInfo` (a `ZoneInfo` itself) stands for.
    fn region_at(&self, p: [f32; 3]) -> Option<(Option<ObjectKey>, Value)> {
        let bsp = self.bsp.as_ref()?;
        if bsp.is_empty() {
            return None;
        }
        let (leaf, zone) = bsp.point_region(p);
        let actor = self.zone_actors.get(zone as usize).copied().flatten().or(self.level_info);
        Some((actor, Value::Struct(vec![Value::Object(actor), Value::Int(leaf), Value::Byte(zone)])))
    }

    /// Works out the zone an actor is in from where it stands, and for a pawn the zones of its
    /// feet and its eyes too (`FootRegion` at the bottom of its cylinder, `HeadRegion` at its
    /// `BaseEyeHeight`). Only the map's own actors had a `Region`, the one the editor stored:
    /// anything spawned had none, so a chocolate frog out of a chest read no `ZoneGravity`
    /// in `ComputeTrajectoryByTime` and jumped with no height at all, and every footstep read
    /// `bWaterZone` from nothing. With `events`, a change is announced the way the scripts
    /// expect: the old zone's `ActorLeaving`, the actor's `ZoneChange`, the new zone's
    /// `ActorEntered`, and a pawn's `FootZoneChange` and `HeadZoneChange`.
    pub fn update_zone(&mut self, a: ObjectKey, events: bool) -> crate::VmResult<()> {
        if self.bsp.is_none() || self.is_deleted(a) {
            return Ok(());
        }
        let Some(at) = self.vec3_id(a, self.hot.location) else { return Ok(()) };
        if self.zoned_at.get(&a) == Some(&at) {
            return Ok(());
        }
        self.zoned_at.insert(a, at);
        let zone_of = |v: &Value| match v {
            Value::Struct(r) => match r.first() {
                Some(Value::Object(z)) => *z,
                _ => None,
            },
            _ => None,
        };
        let Some((zone, region)) = self.region_at(at) else { return Ok(()) };
        let was = self.get_prop(a, "Region").map(|v| zone_of(&v)).unwrap_or(None);
        self.set_prop(a, "Region", region)?;
        if events && was != zone {
            if let Some(old) = was {
                self.call_event(old, "ActorLeaving", vec![Value::Object(Some(a))])?;
            }
            self.call_event(a, "ZoneChange", vec![Value::Object(zone)])?;
            if let Some(new) = zone {
                self.call_event(new, "ActorEntered", vec![Value::Object(Some(a))])?;
            }
        }
        if !matches!(self.get_prop(a, "FootRegion"), Ok(Value::Struct(_))) {
            return Ok(());
        }
        let height = match self.get_prop_id(a, self.hot.collision_height)? {
            Value::Float(h) => h,
            _ => 0.0,
        };
        let eye = match self.get_prop(a, "BaseEyeHeight") {
            Ok(Value::Float(e)) => e,
            _ => 0.0,
        };
        for (prop, dz, event) in [("FootRegion", -height, "FootZoneChange"), ("HeadRegion", eye, "HeadZoneChange")] {
            let Some((zone, region)) = self.region_at([at[0], at[1], at[2] + dz]) else { continue };
            let was = self.get_prop(a, prop).map(|v| zone_of(&v)).unwrap_or(None);
            self.set_prop(a, prop, region)?;
            if events && was != zone {
                self.call_event(a, event, vec![Value::Object(zone)])?;
            }
        }
        Ok(())
    }

    /// Gravity of the zone an actor is in, from its `ZoneInfo`.
    fn zone_gravity(&mut self, a: ObjectKey) -> crate::VmResult<[f32; 3]> {
        if let Ok(Value::Struct(region)) = self.get_prop(a, "Region") {
            if let Some(Value::Object(Some(zone))) = region.first() {
                if let Ok(v) = self.get_prop(*zone, "ZoneGravity") {
                    if let Ok(g) = crate::vm::floats3(&v) {
                        return Ok(g);
                    }
                }
            }
        }
        Ok([0.0, 0.0, -512.0])
    }

    /// One frame for every live actor: timer, state code, then `Tick`.
    /// Feeds a frame of input to the player and lets its scripts act on it, the way the engine
    /// fills the movement axes before calling `PlayerTick`.
    /// Hands the player what its input devices did this frame, through the game's own key
    /// bindings: they fill the raw axes (`aMouseX`, `aForward`, ...) and the player's
    /// `PlayerInput` smooths the mouse and remaps them into what it moves and looks with.
    pub fn player_input(&mut self, input: PlayerInput<'_>, dt: f32, report: &mut Report) {
        let Some(p) = self.player else { return };
        let mut axes: Vec<(String, f32)> = Vec::new();
        let mut buttons: Vec<String> = Vec::new();
        let mut commands: Vec<String> = Vec::new();
        let mut apply = |bindings: Vec<crate::config::Binding>, scale: f32, pressed: bool| {
            for binding in bindings {
                match binding {
                    crate::config::Binding::Axis(name, speed) => match axes.iter_mut().find(|(n, _)| *n == name) {
                        Some((_, value)) => *value += speed * scale,
                        None => axes.push((name, speed * scale)),
                    },
                    crate::config::Binding::Button(name) => buttons.push(name),
                    // A command runs once, when the key goes down.
                    crate::config::Binding::Command(name) if pressed => commands.push(name),
                    crate::config::Binding::Command(_) => {}
                }
            }
        };
        // The console sees the keys first and may keep them to itself, which is what a menu
        // being up means.
        const IST_PRESS: u8 = 1;
        const IST_RELEASE: u8 = 3;
        const IST_AXIS: u8 = 4;
        // A key the console took stays the console's for as long as it is held: with the
        // console open, typing a W or an A walked Harry about behind it, because only the
        // commands bound to the key were held back and not its axes.
        for key in input.held {
            if !self.held_keys.iter().any(|k| k == key) && self.console_key(key, IST_PRESS, 0.0, report) {
                self.taken_keys.push(key.to_string());
            }
        }
        for key in self.held_keys.clone() {
            if !input.held.contains(&key.as_str()) {
                self.console_key(&key, IST_RELEASE, 0.0, report);
                self.taken_keys.retain(|k| *k != key);
            }
        }
        let taken = self.taken_keys.clone();
        for (key, delta) in [("MouseX", input.mouse.0), ("MouseY", input.mouse.1)] {
            if delta != 0.0 {
                self.console_key(key, IST_AXIS, delta, report);
            }
        }
        // A held key drives its axis at the speed the ini gives it. Those speeds are in the
        // units of the device the game was written for, where a turn of the mouse counts a few
        // hundred; the mouse here is fed in pixels a second, which counts thousands. The keys
        // are brought into the same range so that turning with them is not a crawl beside the
        // mouse. Hypothesis: the factor is the one the player's own `MouseScale` applies to
        // the mouse and not to a key, read back from the scripts as 12.
        const KEY_AXIS: f32 = 12.0;
        for key in input.held {
            if taken.iter().any(|k| k == key) {
                continue;
            }
            let pressed = !self.held_keys.iter().any(|k| k == key);
            apply(self.config.bindings(key), KEY_AXIS, pressed);
        }
        self.held_keys = input.held.iter().map(|k| k.to_string()).collect();
        // The mouse moves its axes by how far it went. Those axes count per second, like the
        // ones a held key drives: the player smooths them and the camera multiplies the result
        // by the frame's own time again.
        apply(self.config.bindings("MouseX"), input.mouse.0 / dt.max(1e-3), false);
        apply(self.config.bindings("MouseY"), input.mouse.1 / dt.max(1e-3), false);

        // Every axis and button the bindings can reach starts cleared, or the last frame's push
        // would stay down.
        for (name, _) in self.axis_names() {
            if let Err(e) = self.set_prop(p, &name, Value::Float(0.0)) {
                report.record(e);
            }
        }
        for name in self.button_names() {
            if let Err(e) = self.set_prop(p, &name, Value::Byte(0)) {
                report.record(e);
            }
        }
        for (name, value) in axes {
            if let Err(e) = self.set_prop(p, &name, Value::Float(value)) {
                report.record(e);
            }
        }
        for name in buttons {
            if let Err(e) = self.set_prop(p, &name, Value::Byte(1)) {
                report.record(e);
            }
        }
        // `Jump`, `AltFire` and the rest are `exec` functions of the player; a key that names
        // one that this player does not have simply does nothing.
        for name in commands {
            if let Err(e) = self.call_event(p, &name, Vec::new()) {
                report.record(e);
            }
        }
        if let Err(e) = self.call_event(p, "PlayerInput", vec![Value::Float(dt)]) {
            report.record(e);
        }
        if let Err(e) = self.call_event_id(p, self.hot.player_tick, vec![Value::Float(dt)]) {
            report.record(e);
        }
        // `ViewFlash` is an event (flag 0x800) that no script of the game calls every tick:
        // `Engine.PlayerPawn`'s states no longer do, and the only caller is
        // `HPConsole.HandleFastForward`, once. Yet it is what brings `FlashFog.W` (the scene's
        // brightness, 0 by default) up to 1, and what keeps it at 0 while a skipped cutscene
        // runs (`harry.ViewFlash` with `bForceBlackScreen`). Hypothesis: the engine calls it
        // for the player after `PlayerTick`, as the older engine's script did.
        if let Err(e) = self.call_event(p, "ViewFlash", vec![Value::Float(dt)]) {
            report.record(e);
        }
    }

    /// Index of a key in `EInputKey`, which names them as the bindings do with an `IK_` in
    /// front. The console is given keys by that number.
    pub fn input_key_index(&mut self, key: &str) -> Option<u8> {
        let e = self.world.enums.iter().find(|e| self.world.names.str(e.name) == "EInputKey")?;
        let at = e
            .values
            .iter()
            .position(|&n| self.world.names.str(n).trim_start_matches("IK_").eq_ignore_ascii_case(key))?;
        u8::try_from(at).ok()
    }

    /// The console the player's viewport carries, which the game draws its menus through.
    pub fn console(&mut self) -> Option<ObjectKey> {
        let player = self.player?;
        let Ok(Value::Object(Some(viewport))) = self.get_prop(player, "Player") else { return None };
        match self.get_prop(viewport, "Console") {
            Ok(Value::Object(c)) => c,
            _ => None,
        }
    }

    /// Hands a key to the console the way the engine does, before the bindings see it, and
    /// says whether it took it: that is how Escape opens the menu and how the menus and the
    /// map follow the mouse, which reaches them as movement on the `MouseX` and `MouseY` keys.
    fn console_key(&mut self, key: &str, action: u8, delta: f32, report: &mut Report) -> bool {
        let (Some(console), Some(index)) = (self.console(), self.input_key_index(key)) else { return false };
        let args = vec![Value::Byte(index), Value::Byte(action), Value::Float(delta)];
        match self.call_event(console, "KeyEvent", args) {
            Ok(Some(Value::Bool(taken))) => taken,
            Ok(_) => false,
            Err(e) => {
                report.record(e);
                false
            }
        }
    }

    /// Keys `[Engine.Input]` binds. The section also holds the `Input` class's own config,
    /// which the ini writes indexed (`Aliases[8]=...`): a key of the keyboard never is.
    fn bound_keys(&self) -> Vec<String> {
        self.config.user.keys("Engine.Input").iter().filter(|(k, _)| !k.contains('[')).map(|(k, _)| k.to_string()).collect()
    }

    /// Axes any binding of this game can move, worked out from the bindings themselves.
    fn axis_names(&self) -> Vec<(String, f32)> {
        let mut out: Vec<(String, f32)> = Vec::new();
        for key in self.bound_keys() {
            for binding in self.config.bindings(&key) {
                if let crate::config::Binding::Axis(name, _) = binding {
                    if !out.iter().any(|(n, _)| *n == name) {
                        out.push((name, 0.0));
                    }
                }
            }
        }
        out
    }

    /// Buttons any binding of this game can hold down, worked out the same way.
    fn button_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for key in self.bound_keys() {
            for binding in self.config.bindings(&key) {
                if let crate::config::Binding::Button(name) = binding {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
        out
    }


    /// Keeps the sound in step with the level: every actor that is making a sound drags it
    /// along, and an actor with an `AmbientSound` loops it from where it stands, which is the
    /// engine's job rather than any script's.
    /// Opens the mouth of whoever is speaking.
    ///
    /// Nothing in the scripts moves a mouth: `Pawn.PlayFacialAnim` drives the brows alone
    /// (`brow_root`, `brow_1`..`brow_5`), and `mouth_root`, `mouth_rest`, `mouth_anim` and
    /// `mouth_pos_0`..`mouth_pos_5` appear only in the name table of `HPModels`, so the engine
    /// is what plays them. What it has to go on is the line itself: `Actor.CutCommand_Say`
    /// plays the dialog with `PlaySound(dlgSound, SLOT_Talk, ...)`.
    ///
    /// Hypothesis: the mouth is opened by how loud the line is at that moment. It is what the
    /// shape of the data asks for — `mouth_anim` is two keys, shut and open, which is a range
    /// to sit anywhere in rather than a thing to play, and the crowd meshes carry six numbered
    /// poses instead, which is the same range in steps. Named characters (Harry, Lockhart,
    /// Tom Riddle) have only `mouth_rest` and `mouth_anim`.
    fn tick_mouths(&mut self) {
        const SLOT_TALK: u8 = 5;
        // One frame's worth of sound gives one mouth position.
        const WINDOW: f32 = 1.0 / 30.0;
        let Some(audio) = self.audio.as_ref() else { return };
        let talking: Vec<ObjectKey> = audio
            .playing()
            .iter()
            .filter(|v| v.slot == SLOT_TALK)
            .map(|v| ObjectKey { package: grim_object::PkgId((v.owner >> 32) as u32), export: v.owner as u32 })
            .collect();
        if talking.is_empty() && self.mouths.is_empty() {
            return;
        }
        let mut who: Vec<ObjectKey> = self.mouths.keys().copied().collect();
        who.extend(talking.iter().copied().filter(|a| !self.mouths.contains_key(a)));
        for a in who {
            if self.is_deleted(a) {
                self.mouths.remove(&a);
                continue;
            }
            let loud = match talking.contains(&a) {
                true => self.audio.as_ref().map_or(0.0, |x| x.loudness(crate::natives::sound_owner(a), SLOT_TALK, WINDOW)),
                false => 0.0,
            };
            if let Err(e) = self.move_mouth(a, loud) {
                self.warn(format!("mouth: {:?}", e.kind));
            }
        }
    }

    /// Puts one speaker's mouth where that much loudness leaves it, giving them the channel
    /// that holds it the first time they speak.
    fn move_mouth(&mut self, a: ObjectKey, loud: f32) -> crate::VmResult<()> {
        let Some(anims) = crate::natives::engine::mesh_anims(self, a)? else { return Ok(()) };
        let named = |vm: &Vm, n: &str| vm.world.names.get(n).filter(|id| anims.sequences.contains(id));
        let has_mouth_root = anims.bones.iter().any(|&b| self.world.names.str(b).eq_ignore_ascii_case("mouth_root"));
        if !has_mouth_root {
            return Ok(());
        }
        // Silence rests the mouth; a line opens it as far as it is loud.
        let (seq, open) = if loud <= 0.0 {
            match named(self, "mouth_rest").or_else(|| named(self, "mouth_anim")) {
                Some(seq) => (seq, 0.0),
                None => return Ok(()),
            }
        } else if let Some(seq) = named(self, "mouth_anim") {
            (seq, loud)
        } else {
            // Six numbered poses: the same range in steps, widest at five.
            let step = (loud * 5.0).round().clamp(0.0, 5.0) as u32;
            match named(self, &format!("mouth_pos_{step}")) {
                Some(seq) => (seq, 0.0),
                None => return Ok(()),
            }
        };
        // `AnimFrame` runs from 0 to 1 over the whole sequence, whose last frame carries the
        // pose round to the first key again; a mouth is placed between the keys instead, so
        // fully open is the last key rather than the end of the sequence.
        let name = self.world.names.str(seq).to_string();
        let frames = self.sequence_frames(a, &name).unwrap_or(1).max(1) as f32;
        let frame = open * (frames - 1.0) / frames;
        let channel = match self.mouths.get(&a) {
            Some(&(channel, _)) if !self.is_deleted(channel) => channel,
            _ => {
                let Some(class) = self.class_named("AnimChannel") else { return Ok(()) };
                let Some(channel) = crate::natives::engine::spawn(self, a, class, Some(a), None, None, None)? else {
                    return Ok(());
                };
                let bone = self.world.names.intern("mouth_root");
                // `AT_Replace`: the mouth bones are the channel's alone while it holds them.
                self.add_channel(a, channel, 0, bone);
                channel
            }
        };
        if self.mouths.get(&a).map(|&(_, was)| was) != Some(seq) {
            self.set_prop(channel, "AnimSequence", Value::Name(seq))?;
            self.set_prop(channel, "bAnimLoop", Value::Bool(false))?;
            // The frame is set here, not played: the rate stays at zero so the tick leaves it.
            self.set_prop(channel, "AnimRate", Value::Float(0.0))?;
        }
        // Taking the bones over is what makes the channel show, and a body animation played
        // over the whole skeleton drops every channel, so it is taken back whenever it was.
        if !self.channels_of(a).iter().any(|c| c.actor == channel && c.posing) {
            self.channel_plays(channel);
        }
        self.set_prop(channel, "AnimFrame", Value::Float(frame))?;
        if self.switches.trace_anim {
            let who = self.object_name(a);
            eprintln!("mouth {who}: {name} at {frame:.2} (loudness {loud:.2})");
        }
        self.mouths.insert(a, (channel, seq));
        Ok(())
    }

    fn tick_audio(&mut self) {
        if self.audio.is_none() {
            return;
        }
        for a in self.actors.clone() {
            if self.is_deleted(a) {
                if let Some(voice) = self.ambient.remove(&a) {
                    let _ = voice;
                    if let Some(audio) = self.audio.as_mut() {
                        audio.stop(crate::natives::sound_owner(a), None, None, 0.0);
                    }
                }
                continue;
            }
            let at = self.get_prop_id(a, self.hot.location).and_then(|v| crate::vm::floats3(&v)).unwrap_or([0.0; 3]);
            if let Some(audio) = self.audio.as_mut() {
                audio.move_owner(crate::natives::sound_owner(a), at);
            }
            let ambient = match self.get_prop(a, "AmbientSound") {
                Ok(Value::Object(k)) => k,
                _ => None,
            };
            match (ambient, self.ambient.get(&a).copied()) {
                (Some(sound), None) => {
                    // `SLOT_Ambient` in Unreal's own list, so an ambient sound never fights
                    // with the effects the same actor plays.
                    const SLOT_AMBIENT: u8 = 4;
                    let volume = match self.get_prop(a, "SoundVolume") {
                        Ok(Value::Byte(b)) => b as f32 / 255.0,
                        _ => 1.0,
                    };
                    let radius = match self.get_prop(a, "SoundRadius") {
                        Ok(Value::Byte(b)) => b as f32 * 25.0,
                        _ => 0.0,
                    };
                    let pitch = match self.get_prop(a, "SoundPitch") {
                        Ok(Value::Byte(b)) if b > 0 => b as f32 / 64.0,
                        _ => 1.0,
                    };
                    if let Some(id) = crate::natives::sound_of(self, sound) {
                        let play = grim_audio::Play {
                            sound: id,
                            owner: crate::natives::sound_owner(a),
                            slot: SLOT_AMBIENT,
                            volume,
                            radius,
                            pitch,
                            looping: true,
                            at,
                            no_override: false,
                            kind: grim_audio::Kind::Effect,
                        };
                        if let Some(audio) = self.audio.as_mut() {
                            audio.play(play);
                        }
                        self.ambient.insert(a, sound);
                    }
                }
                (None, Some(_)) => {
                    self.ambient.remove(&a);
                    if let Some(audio) = self.audio.as_mut() {
                        audio.stop(crate::natives::sound_owner(a), Some(4), None, 0.0);
                    }
                }
                _ => {}
            }
        }
    }

    /// The physics modes that move an actor by themselves, in the order the tick applies them.
    /// `Actor.AutonomousPhysics` runs the same thing for one actor over one delta.
    pub fn physics_step(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        self.turn(a, dt)?;
        self.fall(a, dt)?;
        self.fly(a, dt)?;
        self.project(a, dt)?;
        Ok(())
    }

    /// The modes that follow something else and so have to run once that has moved: an actor
    /// riding its owner, a mover on its way, and an interpolating actor.
    pub fn late_physics_step(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        self.trail(a)?;
        self.move_brush(a, dt)?;
        self.interpolate(a, dt)?;
        Ok(())
    }

    pub fn tick(&mut self, dt: f32, report: &mut Report) {
        // A paused level does not move: `Pauser` names who asked for it, and the game sets it
        // when it opens a menu.
        if self.paused() {
            return;
        }
        // The level carries the clock the scripts read: how long they have waited, when
        // something last happened, how far an animation has come.
        if let Some(info) = self.level_info {
            let now = match self.get_prop(info, "TimeSeconds") {
                Ok(Value::Float(t)) => t,
                _ => 0.0,
            };
            if let Err(e) = self.set_prop(info, "TimeSeconds", Value::Float(now + dt)) {
                report.record(e);
            }
        }
        // The stairs' slope is only drawn: the frame starts with everyone back on the step
        // they really stand on, before anything measures where anyone is.
        self.stair_was.clear();
        for a in self.stair_lag.keys().copied().collect::<Vec<_>>() {
            if let Err(e) = self.leave_stairs(a) {
                report.record(e);
            }
        }
        let trace = self.switches.trace_tick;
        let mark = std::time::Instant::now();
        // Overlaps are only looked for around actors that moved, against the ones that collide.
        // What was destroyed leaves the list: every loop over the actors skips it anyway, and
        // a hub played for a while carries more destroyed actors than live ones (2758 in all
        // on the grand staircase after a minute, 883 of them colliding).
        let heap = &self.heap;
        self.actors.retain(|a| !heap.get(a).is_some_and(|i| i.deleted));
        let collidable = self.collidable_actors();
        self.trace_candidates = Some((collidable.iter().map(|c| c.actor).collect(), self.actors.len()));
        let t_collidable = mark.elapsed().as_secs_f32() * 1000.0;
        self.blockers = self.find_blocking_actors();
        let t_blockers = mark.elapsed().as_secs_f32() * 1000.0;
        if trace {
            eprintln!("TICK collidable {t_collidable:.2} ({} of them), blockers {:.2} ({} of them)", collidable.len(), t_blockers - t_collidable, self.blockers.len());
        }
        let mark = std::time::Instant::now();
        // With the trace on, where the actors' part of the tick goes: their scripts, their
        // physics and animation, and finding out what they touch and the zone they are in.
        let mut phases = [0.0f32; 3];
        // And which classes it goes to, scripts and movement apart.
        let mut by_class: std::collections::HashMap<grim_object::ClassId, [f32; 2]> = std::collections::HashMap::new();
        let clock = |on: bool| on.then(std::time::Instant::now);
        let lap = |t: Option<std::time::Instant>, into: &mut f32| {
            if let Some(t) = t {
                *into += t.elapsed().as_secs_f32() * 1000.0;
            }
        };
        for a in self.actors.clone() {
            if self.is_deleted(a) {
                continue;
            }
            // `bStatic` is the game's own word for an actor that never moves and runs no code
            // of its own: Unreal leaves those out of the tick altogether, and over half of a
            // map's actors carry it. They still block the way and are still drawn.
            if matches!(self.get_prop_id(a, self.hot.is_static), Ok(Value::Bool(true))) {
                continue;
            }
            let was = self.vec3_id(a, self.hot.location).unwrap_or([0.0; 3]);
            // The actor's own code runs first and its physics after: Unreal ticks an actor's
            // script (`Tick`, then its state code, then its timers) and only then moves it.
            // The original's save of the first checkpoint is what shows the order: a jellybean
            // at rest in `HProp.BounceIntoPlace` turns itself 30000 a second in its `Tick`, and
            // the engine turns it back to `DesiredRotation` at `RotationRate` (50000); every
            // one of them is saved facing yaw 0, which is where it ends only if the engine
            // turns after the script does.
            report.calls += 1;
            let t = clock(trace);
            if let Err(e) = self.call_event_id(a, self.hot.tick, vec![Value::Float(dt)]) {
                report.record(e);
            }
            if self.is_deleted(a) {
                continue;
            }
            if let Err(e) = self.run_state_code(a, dt) {
                report.record(e);
            }
            if let Err(e) = self.tick_timer(a, dt) {
                report.record(e);
            }
            if let Err(e) = self.expire(a, dt) {
                report.record(e);
            }
            let spent = t.map(|t| t.elapsed().as_secs_f32() * 1000.0);
            lap(t, &mut phases[0]);
            if let (Some(ms), Ok(class)) = (spent, self.class_of(a)) {
                by_class.entry(class).or_default()[0] += ms;
            }
            if self.is_deleted(a) {
                continue;
            }
            let t = clock(trace);
            // An actor without physics (most of a level) is neither moved nor turned by the
            // engine: it only follows a latent move and animates. Turning those towards
            // `DesiredRotation` too swung the Diffindo vines of `Grounds_hub`, placed upside down
            // with `bRotateToDesired` on and a desired rotation of zero, round until they lay
            // flat across their doorways; the original leaves them as the map puts them.
            let moving = !matches!(self.get_prop_id(a, self.hot.physics), Ok(Value::Byte(0)));
            let step = if moving { self.physics_step(a, dt) } else { Ok(()) };
            if let Err(e) = step {
                report.record(e);
            }
            if let Err(e) = self.advance_move(a, dt) {
                report.record(e);
            }
            if moving {
                if let Err(e) = self.walk(a, dt) {
                    report.record(e);
                }
            }
            if let Err(e) = self.animate(a, dt) {
                report.record(e);
            }
            if moving {
                if let Err(e) = self.late_physics_step(a, dt) {
                    report.record(e);
                }
            }
            let spent = t.map(|t| t.elapsed().as_secs_f32() * 1000.0);
            lap(t, &mut phases[1]);
            if let (Some(ms), Ok(class)) = (spent, self.class_of(a)) {
                by_class.entry(class).or_default()[1] += ms;
            }
            let t = clock(trace);
            let now = self.vec3_id(a, self.hot.location).unwrap_or([0.0; 3]);
            if now != was {
                if let Err(e) = self.update_zone(a, true) {
                    report.record(e);
                }
                if let Err(e) = self.update_touching(a, &collidable) {
                    report.record(e);
                }
            }
            lap(t, &mut phases[2]);
        }
        let t_actors = mark.elapsed().as_secs_f32() * 1000.0;
        let mark = std::time::Instant::now();
        // The bones move with the animations, and the particles the scripts hang off them are
        // placed from where they ended up. An animation that carries the actor moves it first,
        // so everything placed off a bone is placed where the actor has arrived.
        self.tick_root_motion(report);
        self.tick_bones(report);
        let t_bones = mark.elapsed().as_secs_f32() * 1000.0;
        let mark = std::time::Instant::now();
        self.tick_particles(dt, report);
        self.tick_audio();
        self.tick_mouths();
        let t_particles = mark.elapsed().as_secs_f32() * 1000.0;
        let mark = std::time::Instant::now();
        self.update_decals(report);
        self.trace_candidates = None;
        if trace {
            eprintln!(
                "TICK blockers {t_blockers:.1} actors {t_actors:.1} (scripts {:.1} moving {:.1} touch+zone {:.1}) bones {t_bones:.1} particles {t_particles:.1} decals {:.1}",
                phases[0],
                phases[1],
                phases[2],
                mark.elapsed().as_secs_f32() * 1000.0
            );
            for (i, label) in ["scripts", "moving"].into_iter().enumerate() {
                let mut costs: Vec<(f32, grim_object::ClassId)> = by_class.iter().map(|(&c, t)| (t[i], c)).collect();
                costs.sort_by(|a, b| b.0.total_cmp(&a.0));
                let top: Vec<String> =
                    costs.iter().take(6).map(|(ms, c)| format!("{} {ms:.2}", self.world.names.str(self.world.class(*c).name))).collect();
                eprintln!("TICK {label} by class: {}", top.join(", "));
            }
        }
    }

    /// Puts every decal back under what casts it. In Unreal the renderer does this as it draws
    /// each one, handing the script the light that throws the shadow; with none, the script's
    /// own fallback drops it straight down, which is what a shadow does under an overhead light.
    fn update_decals(&mut self, report: &mut Report) {
        if self.switches.no_decals {
            return;
        }
        let Some(decal) = self.class_named("Decal") else { return };
        for a in self.actors.clone() {
            if self.is_deleted(a) {
                continue;
            }
            let Ok(class) = self.class_of(a) else { continue };
            if !self.world.is_child_of(class, decal) {
                continue;
            }
            // In Unreal it is the drawing of what casts a decal that brings the decal along:
            // an actor that is not drawn leaves its own shadow untouched, and the shadow takes
            // itself off the wall on the next tick (`if (!bUpdated) DetachDecal()`). What is
            // hidden — a character the game state leaves out — casts nothing, and so does what
            // the frame never drew: a car high over the rooftops keeps no shadow on the street
            // once it is off the screen.
            let owner = match self.get_prop(a, "Owner") {
                Ok(Value::Object(Some(o))) => o,
                _ => continue,
            };
            if self.is_deleted(owner) || matches!(self.get_prop_id(owner, self.hot.hidden), Ok(Value::Bool(true))) {
                continue;
            }
            if self.drawn.as_ref().is_some_and(|d| !d.contains(&owner)) {
                continue;
            }
            // No light is handed over, so the script throws the shadow straight down. Handing
            // it the light that reaches the actor most put Harry's shadow on the wall beside
            // him whenever a torch hung at his height: the script throws it away from the light
            // (`ShadowDir = Normal(Loc - L.Location)`), and that is sideways. Which light, if
            // any, Unreal hands over is not in the data: the first checkpoint's save was written
            // at 0.1 seconds, before any of its ten shadows had been updated once.
            report.calls += 1;
            if let Err(e) = self.call_event(a, "Update", vec![Value::Object(None)]) {
                report.record(e);
            }
        }
    }

    fn tick_timer(&mut self, a: ObjectKey, dt: f32) -> crate::VmResult<()> {
        let Value::Float(rate) = self.get_prop_id(a, self.hot.timer_rate)? else { return Ok(()) };
        if rate <= 0.0 {
            return Ok(());
        }
        let Value::Float(counter) = self.get_prop(a, "TimerCounter")? else { return Ok(()) };
        let counter = counter + dt;
        if counter < rate {
            return self.set_prop(a, "TimerCounter", Value::Float(counter));
        }
        let looping = matches!(self.get_prop(a, "bTimerLoop")?, Value::Bool(true));
        self.set_prop(a, "TimerCounter", Value::Float(if looping { counter - rate } else { 0.0 }))?;
        if !looping {
            self.set_prop(a, "TimerRate", Value::Float(0.0))?;
        }
        self.call_event(a, "Timer", vec![])?;
        Ok(())
    }
}
