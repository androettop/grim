//! Bytecode interpreter: frames, places, calls, states and latent execution.

use std::collections::HashMap;
use std::sync::Arc;

use grim_assets::bytecode::{Cast, Expr};
use grim_object::types::{cpf, func, Code};
use grim_object::{ClassId, FuncId, NameId, ObjectKey, PkgId, PropType, StateId, Value, World};
use grim_package::ObjectRef;

use crate::natives::{NativeCall, NativeFn, Natives};
use crate::typing::{Scope, Ty, Typer};

/// `auto state` flag. Verified: no class declares more than one (0x1 is editable `state()`).
pub const STATE_AUTO: u32 = 0x2;

/// Package id used for objects created at runtime.
pub const TRANSIENT: PkgId = PkgId(u32::MAX);

#[derive(Debug, Clone)]
pub enum VmErrorKind {
    MissingNative(String),
    Script(String),
}

#[derive(Debug, Clone)]
pub struct VmError {
    pub kind: VmErrorKind,
    /// Innermost first.
    pub trace: Vec<String>,
}

impl VmError {
    pub fn script(msg: impl Into<String>) -> Self {
        Self { kind: VmErrorKind::Script(msg.into()), trace: Vec::new() }
    }
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            VmErrorKind::MissingNative(n) => write!(f, "native {n} not implemented")?,
            VmErrorKind::Script(m) => f.write_str(m)?,
        }
        for t in &self.trace {
            write!(f, "\n  in {t}")?;
        }
        Ok(())
    }
}

pub type VmResult<T> = Result<T, VmError>;

pub(crate) fn fail<T>(msg: impl Into<String>) -> VmResult<T> {
    Err(VmError::script(msg))
}

/// Work a latent native left pending; state code resumes once it completes.
#[derive(Debug, Clone, PartialEq)]
pub enum Latent {
    Sleep(f32),
    /// Waits while a boolean property of the actor holds that value, which is how the engine's
    /// latent functions wait for something it drives itself.
    Wait(&'static str, bool),
    /// Waits for a move the engine drives (`MoveTo`, `TurnTo`, ...) to finish.
    Move,
    /// Completes on the next tick. Placeholder for latents whose subsystem does not exist yet.
    NextTick(String),
}

#[derive(Debug, Clone)]
pub struct StateThread {
    /// State the running code belongs to: the one whose label was jumped to, which may be
    /// above the object's own state in the chain.
    pub node: StateId,
    pub code: Arc<Code>,
    pub pc: usize,
    pub latent: Option<Latent>,
}

#[derive(Debug, Clone)]
pub struct Instance {
    pub class: ClassId,
    pub name: NameId,
    pub values: Vec<Value>,
    pub state: Option<StateId>,
    pub thread: Option<StateThread>,
    /// Bumped on every state change, so running state code can notice it was replaced.
    pub state_generation: u32,
    pub deleted: bool,
    /// Events the object has switched off with `Disable`. Unreal keeps this as a mask over the
    /// 64 probe names; here it is the names themselves, which is the same thing for the
    /// notifications the engine sends. Almost always empty.
    pub disabled: Vec<NameId>,
}

/// A writable location.
#[derive(Debug, Clone)]
pub enum Place {
    Local(usize),
    Field { obj: ObjectKey, slot: usize },
    /// Slot of a struct value.
    Sub(Box<Place>, usize),
    /// Element of a dynamic array.
    Elem(Box<Place>, usize),
    /// A value that is not a variable (e.g. a function result); writes are discarded.
    Temp(Box<Value>),
}

struct IterState {
    rows: Vec<Vec<Value>>,
    next: usize,
    outs: Vec<(usize, Place)>,
    body_pc: usize,
    /// Where the loop goes once it runs out: past the `IteratorPop` its end offset names. That
    /// pop is for a `break` or `return` leaving the loop early; taking it at the natural end too
    /// pops the loop around it, which `harry.DisplayFirstErrorMessages` (two nested
    /// `AllActors`) showed as "IteratorNext without iterator".
    end_pc: usize,
}

pub struct Frame {
    pub obj: ObjectKey,
    pub func: Option<FuncId>,
    pub locals: Vec<Value>,
    pub code: Arc<Code>,
    pub pc: usize,
    switch: Option<Value>,
    iters: Vec<IterState>,
}

enum Flow {
    Next,
    Jump(u32),
    Return(Value),
    Stop,
    GotoLabel(NameId),
}

/// Per-function data the interpreter needs on every call.
pub struct FuncInfo {
    pub path: String,
    pub flags: u32,
    /// Local slot of each parameter, in call order.
    pub params: Vec<usize>,
    pub out: Vec<bool>,
    pub ret: Option<usize>,
    pub zero: Vec<Value>,
    pub code: Arc<Code>,
    pub native: Option<NativeFn>,
}

pub struct Vm {
    pub world: World,
    pub(crate) heap: grim_object::FastMap<ObjectKey, Instance>,
    next_transient: u32,
    natives_by_index: grim_object::FastMap<u16, FuncId>,
    natives: Natives,
    infos: grim_object::FastMap<FuncId, Arc<FuncInfo>>,
    struct_by_name: grim_object::FastMap<NameId, grim_object::StructId>,
    depth: u32,
    /// Latent request made by the last native call, consumed by the state code runner.
    pub latent: Option<Latent>,
    pub log: Vec<String>,
    /// The console asked the game to end (`exit`, `quit`).
    pub quit: bool,
    /// The console's `hideactors`: the world is drawn without its actors until `showactors`.
    pub hide_actors: bool,
    pub warnings: HashMap<String, usize>,
    /// Argument presence for the next native call (set by `invoke`).
    present: Vec<bool>,
    rng: u64,
    /// Actors of the running level, spawned ones appended.
    pub actors: Vec<ObjectKey>,
    pub level_info: Option<ObjectKey>,
    pub level: Option<ObjectKey>,
    /// Player pawn created by `GameInfo.Login`.
    pub player: Option<ObjectKey>,
    /// Animation sequence names per mesh.
    pub anim_cache: grim_object::FastMap<ObjectKey, crate::natives::MeshAnims>,
    pub config: crate::config::Config,
    /// BSP of the loaded level.
    pub bsp: Option<grim_world::Bsp>,
    /// The `ZoneInfo` of each zone of the level's BSP, by zone number.
    pub zone_actors: Vec<Option<ObjectKey>>,
    /// Where each actor was when its zone was last worked out.
    pub(crate) zoned_at: grim_object::FastMap<ObjectKey, [f32; 3]>,
    /// What stops what, for every actor that collides, gathered with the blockers each tick.
    pub(crate) roles: grim_object::FastMap<ObjectKey, crate::level::Role>,
    /// While a tick runs: the actors that collided when it began, and how many actors there
    /// were then. A trace only has to look at those and at whatever was spawned since.
    pub(crate) trace_candidates: Option<(Vec<ObjectKey>, usize)>,
    /// `GRIM_PROFILE_SCRIPTS`: for each function called, how long its calls took (with what
    /// they called) and how many there were.
    profile: Option<grim_object::FastMap<FuncId, (f64, u64)>>,
    /// What each surface of the level is made of: its texture and its poly flags, which the
    /// scripts ask for to tell what they just hit.
    pub surfaces: Vec<(Option<ObjectKey>, u32)>,
    /// Actors each actor is currently overlapping, so `Touch` and `UnTouch` fire once each.
    pub touching: grim_object::FastMap<ObjectKey, Vec<ObjectKey>>,
    /// Logs calls whose path contains this, for following what the game's scripts do.
    pub trace_func: Option<String>,
    /// Where the scripts are at the moment a native runs, so a warning from inside one can say
    /// which code asked for it. It is set on every call, so it holds only what a frame carries.
    site: Option<(ObjectKey, Option<FuncId>)>,
    /// Which bit of a state's `ProbeMask` each event the engine sends stands for.
    probe_bit: grim_object::FastMap<NameId, u32>,
    /// The debug switches the hot loops ask about, read from the environment once: asking the
    /// environment itself takes a lock and a scan of every variable, and these sit in loops
    /// that run thousands of times a tick.
    pub(crate) switches: Switches,
    /// Names of the properties the per-frame loops read, interned once.
    pub hot: HotNames,
    /// Keys the player held last frame, to tell a press from a key still down.
    pub held_keys: Vec<String>,
    /// Held keys whose press the console kept: they reach no binding until they are let go.
    pub taken_keys: Vec<String>,
    /// The `Canvas` the game draws its menus and HUD through.
    pub canvas: Option<ObjectKey>,
    /// Rectangles the scripts drew this frame.
    pub canvas_quads: Vec<crate::canvas::CanvasQuad>,
    /// Actors the scripts drew over the world this frame, through `Canvas.DrawActor`.
    pub canvas_actors: Vec<ObjectKey>,
    /// What the renderer drew last frame, when something is drawing at all. In Unreal a decal
    /// is brought along by the drawing of what casts it, so only what was on the screen keeps
    /// its shadow on the ground. `None` means nothing is drawing — a headless run — and every
    /// actor that is not hidden counts as drawn.
    pub drawn: Option<grim_object::FastSet<ObjectKey>>,
    pub(crate) fonts: grim_object::FastMap<ObjectKey, crate::canvas::FontMetrics>,
    /// A `ScriptedTexture`'s picture, which its `NotifyActor` draws into each frame. The engine
    /// hands it to the renderer; nothing else in the game reads it back.
    pub scripted: grim_object::FastMap<ObjectKey, crate::canvas::Picture>,
    /// Decoded textures, for the drawing the engine does on the processor rather than on the
    /// card: the font pages a scripted texture writes its text with.
    pub(crate) pictures: grim_object::FastMap<ObjectKey, Option<grim_assets::texture::Rgba>>,
    /// How long each sequence of a mesh lasts, read from the mesh once.
    pub(crate) durations: grim_object::FastMap<ObjectKey, HashMap<NameId, crate::level::SeqInfo>>,
    /// Where an interpolation's current section started, for paths whose first point has
    /// nothing before it: the move runs from wherever the actor was.
    pub(crate) interp_from: grim_object::FastMap<ObjectKey, (ObjectKey, [f32; 3])>,
    /// Latent moves in flight, one per actor.
    pub(crate) moves: grim_object::FastMap<ObjectKey, crate::level::MoveOrder>,
    /// Animation channels each actor was given, with the bone each one takes over.
    pub(crate) channels: grim_object::FastMap<ObjectKey, Vec<crate::level::Channel>>,
    /// Skeletons of the actors that have one, posed by what they play.
    pub poses: crate::pose::Poses,
    /// The last place each moving actor stood clear of the level, to go back to if it ends up
    /// inside something.
    pub(crate) safe_spots: grim_object::FastMap<ObjectKey, [f32; 3]>,
    /// How far below the step it stands on a walker is drawn, so a flight of stairs is
    /// walked as the slope it looks like. The physics adds it back before it moves anything.
    pub(crate) stair_lag: grim_object::FastMap<ObjectKey, f32>,
    /// How far each walker's height still trailed its step when this tick began: `stair_lag`
    /// is emptied as the tick starts, so it cannot say it.
    pub(crate) stair_was: grim_object::FastMap<ObjectKey, f32>,
    /// Per walker on stairs: how far it has walked since its last step up or down, and how
    /// much height it makes up per unit walked, which is one step's height over one step's
    /// length.
    pub(crate) stair_run: grim_object::FastMap<ObjectKey, (f32, f32)>,
    /// Animations that carry the actor with them, by actor.
    pub(crate) root_moves: grim_object::FastMap<ObjectKey, crate::pose::RootMove>,
    /// The actors that travelled into this level with the player, waiting to be told they
    /// arrived. UE1 gives every travelling actor `TravelPostAccept`, not only the player.
    pub(crate) travelled: Vec<ObjectKey>,
    /// What each reference of the bytecode resolves to.
    resolved: grim_object::FastMap<(PkgId, ObjectRef), ObjectKey>,
    /// The shape each actor blocks the way with, and where it was when it was worked out.
    pub(crate) blocker_cache: grim_object::FastMap<ObjectKey, (([f32; 3], [i32; 3]), crate::level::Blocker)>,
    /// What the level's `bStatic` actors collide and block with, worked out once: they never
    /// move and never change, so a tick only has to look at the rest.
    pub(crate) static_collision: Option<std::sync::Arc<crate::level::StaticCollision>>,
    /// What each property reference of the bytecode points at, worked out once.
    prop_defs: grim_object::FastMap<(PkgId, ObjectRef), grim_object::PropDef>,
    /// Room for a call's locals, handed back when it returns so the next call takes it again.
    spare_locals: Vec<Vec<Value>>,
    /// What a name resolves to for a class in a state, kept because the chains never change.
    found_functions: grim_object::FastMap<(ClassId, Option<StateId>, NameId), Option<FuncId>>,
    /// Class ids looked up by name, for the few the engine itself needs to know about.
    named_classes: HashMap<String, Option<ClassId>>,
    /// How big each mesh is drawn, read from the mesh once: the shadows ask for it every frame.
    pub(crate) mesh_extents: grim_object::FastMap<ObjectKey, [f32; 3]>,
    /// The box the actor's mesh fills in the pose it is drawn in, in the mesh's own axes. The
    /// drawing fills it in as it poses each one; what has none falls back to the stored box.
    pub pose_extents: grim_object::FastMap<ObjectKey, [f32; 3]>,
    /// Where each decal landed: a shadow is an actor that sticks to the surface below whatever
    /// casts it, and this is the spot the engine threw it at.
    pub decals: grim_object::FastMap<ObjectKey, crate::level::Decal>,
    /// Each level the game has been in, the way it was left, by level name in lower case. This is
    /// what a save slot keeps in its `<Map>_pa.usa` files, and it outlives the level it describes.
    pub(crate) visited: std::collections::HashMap<String, grim_save::Level>,
    /// Actors that block the way, gathered once a tick: the movement of every actor is tested
    /// against them, and walking the whole level for each one is what the tick cannot afford.
    pub(crate) blockers: Vec<crate::level::Blocker>,
    /// The box each brush takes up, read from its model once.
    pub(crate) brush_boxes: grim_object::FastMap<ObjectKey, Option<([f32; 3], [f32; 3])>>,
    /// The tree of each brush that blocks the way, built once.
    pub(crate) brush_bsps: grim_object::FastMap<ObjectKey, Option<std::sync::Arc<grim_world::Bsp>>>,
    /// Live particles of each `ParticleFX` of the level.
    pub particles: grim_object::FastMap<ObjectKey, crate::particles::Emitter>,
    /// `ParticleFX` itself, looked up once.
    pub(crate) particle_class: Option<ClassId>,
    /// Where the game's sound goes. Nothing without one: the tools that only run the scripts
    /// have no device and no need for one.
    pub audio: Option<Box<dyn grim_audio::Audio>>,
    /// Sounds handed to the backend already, by the object they came from.
    pub(crate) sounds: grim_object::FastMap<ObjectKey, grim_audio::SoundId>,
    /// The `AmbientSound` each actor is looping, so it is started once and stopped when it goes.
    pub(crate) ambient: grim_object::FastMap<ObjectKey, ObjectKey>,
    /// The channel that holds each speaker's mouth, and the sequence it is holding it in.
    pub(crate) mouths: grim_object::FastMap<ObjectKey, (ObjectKey, grim_object::NameId)>,
}

/// Property names the per-frame loops use, so they never intern a string again.
/// The debug switches, read once when the VM starts.
pub struct Switches {
    pub trace_call: Option<String>,
    pub no_ramp: bool,
    pub no_decals: bool,
    pub trace_anim: bool,
    pub trace_move: bool,
    pub trace_probe: bool,
    pub trace_touch: bool,
    pub trace_fall: bool,
    pub trace_mount: bool,
    pub trace_encroach: bool,
    pub trace_quads: bool,
    pub trace_tick: bool,
}

#[derive(Debug, Clone, Copy)]
pub struct HotNames {
    pub location: NameId,
    pub velocity: NameId,
    pub acceleration: NameId,
    pub physics: NameId,
    pub collide_actors: NameId,
    pub collision_radius: NameId,
    pub collision_height: NameId,
    pub timer_rate: NameId,
    pub tick: NameId,
    pub player_tick: NameId,
    pub anim_rate: NameId,
    pub anim_sequence: NameId,
    pub anim_frame: NameId,
    pub anim_last: NameId,
    pub anim_loop: NameId,
    pub anim_finished: NameId,
    pub mesh: NameId,
    /// Read for every actor on every frame by the drawing.
    pub rotation: NameId,
    pub draw_type: NameId,
    pub draw_scale: NameId,
    pub hidden: NameId,
    pub brush: NameId,
    pub style: NameId,
    pub corona: NameId,
    pub fixed_rotation: NameId,
    pub rotate_to_desired: NameId,
    pub block_actors: NameId,
    pub block_players: NameId,
    pub is_player: NameId,
    pub is_mover: NameId,
    pub collision_width: NameId,
    pub collide_type: NameId,
    pub is_static: NameId,
    pub max_step_height: NameId,
    pub ground_speed: NameId,
    pub accel_rate: NameId,
    pub b_run: NameId,
    pub walking_pct: NameId,
    pub ground_friction: NameId,
    pub base: NameId,
    pub desired_rotation: NameId,
    pub rotation_rate: NameId,
    /// Native properties of `Core.Object`: the layout has a slot, only the engine fills it.
    pub name_prop: NameId,
    pub class_prop: NameId,
}

const MAX_DEPTH: u32 = 250;

impl Vm {
    pub fn new(world: World) -> Self {
        let config = crate::config::Config::load(world.lib.fs().clone(), world.lib.root());
        let natives_by_index = (0..world.functions.len())
            .filter(|&i| world.functions[i].native != 0)
            .map(|i| (world.functions[i].native, FuncId(i as u32)))
            .collect();
        let struct_by_name: grim_object::FastMap<_, _> = world.structs.iter().enumerate().map(|(i, s)| (s.name, grim_object::StructId(i as u32))).collect();
        let mut vm = Self {
            world,
            heap: grim_object::FastMap::default(),
            next_transient: 0,
            natives_by_index,
            natives: Natives::standard(),
            infos: grim_object::FastMap::default(),
            struct_by_name,
            depth: 0,
            latent: None,
            log: Vec::new(),
            quit: false,
            hide_actors: false,
            warnings: HashMap::new(),
            present: Vec::new(),
            rng: 0x2545_F491_4F6C_DD1D,
            actors: Vec::new(),
            level_info: None,
            level: None,
            player: None,
            anim_cache: grim_object::FastMap::default(),
            config,
            bsp: None,
            zone_actors: Vec::new(),
            zoned_at: grim_object::FastMap::default(),
            roles: grim_object::FastMap::default(),
            trace_candidates: None,
            profile: std::env::var_os("GRIM_PROFILE_SCRIPTS").map(|_| grim_object::FastMap::default()),
            surfaces: Vec::new(),
            touching: grim_object::FastMap::default(),
            trace_func: std::env::var("GRIM_TRACE_FUNC").ok(),
            site: None,
            probe_bit: grim_object::FastMap::default(),
            switches: Switches {
                trace_call: std::env::var("GRIM_TRACE_CALL").ok(),
                no_ramp: std::env::var_os("GRIM_NO_RAMP").is_some(),
                no_decals: std::env::var_os("GRIM_NO_DECALS").is_some(),
                trace_anim: std::env::var_os("GRIM_TRACE_ANIM").is_some(),
                trace_move: std::env::var_os("GRIM_TRACE_MOVE").is_some(),
                trace_probe: std::env::var_os("GRIM_TRACE_PROBE").is_some(),
                trace_touch: std::env::var_os("GRIM_TRACE_TOUCH").is_some(),
                trace_fall: std::env::var_os("GRIM_TRACE_FALL").is_some(),
                trace_mount: std::env::var_os("GRIM_TRACE_MOUNT").is_some(),
                trace_encroach: std::env::var_os("GRIM_TRACE_ENCROACH").is_some(),
                trace_quads: std::env::var_os("GRIM_DEBUG_QUADS").is_some(),
                trace_tick: std::env::var_os("GRIM_TRACE_TICK").is_some(),
            },
            hot: HotNames {
                location: NameId(0),
                velocity: NameId(0),
                acceleration: NameId(0),
                physics: NameId(0),
                collide_actors: NameId(0),
                collision_radius: NameId(0),
                collision_height: NameId(0),
                timer_rate: NameId(0),
                tick: NameId(0),
                player_tick: NameId(0),
                anim_rate: NameId(0),
                anim_sequence: NameId(0),
                anim_frame: NameId(0),
                anim_last: NameId(0),
                anim_loop: NameId(0),
                anim_finished: NameId(0),
                mesh: NameId(0),
                rotation: NameId(0),
                draw_type: NameId(0),
                draw_scale: NameId(0),
                hidden: NameId(0),
                brush: NameId(0),
                style: NameId(0),
                corona: NameId(0),
                fixed_rotation: NameId(0),
                rotate_to_desired: NameId(0),
                block_actors: NameId(0),
                block_players: NameId(0),
                is_player: NameId(0),
                is_mover: NameId(0),
                collision_width: NameId(0),
                collide_type: NameId(0),
                is_static: NameId(0),
                max_step_height: NameId(0),
                ground_speed: NameId(0),
                accel_rate: NameId(0),
                b_run: NameId(0),
                walking_pct: NameId(0),
                ground_friction: NameId(0),
                base: NameId(0),
                desired_rotation: NameId(0),
                rotation_rate: NameId(0),
                name_prop: NameId(0),
                class_prop: NameId(0),
            },
            held_keys: Vec::new(),
            taken_keys: Vec::new(),
            blockers: Vec::new(),
            brush_boxes: grim_object::FastMap::default(),
            brush_bsps: grim_object::FastMap::default(),
            poses: HashMap::new(),
            root_moves: grim_object::FastMap::default(),
            travelled: Vec::new(),
            decals: grim_object::FastMap::default(),
            mesh_extents: grim_object::FastMap::default(),
            pose_extents: grim_object::FastMap::default(),
            named_classes: HashMap::new(),
            found_functions: grim_object::FastMap::default(),
            spare_locals: Vec::new(),
            prop_defs: grim_object::FastMap::default(),
            blocker_cache: grim_object::FastMap::default(),
            static_collision: None,
            resolved: grim_object::FastMap::default(),
            visited: std::collections::HashMap::new(),
            stair_lag: grim_object::FastMap::default(),
            stair_was: grim_object::FastMap::default(),
            stair_run: grim_object::FastMap::default(),
            safe_spots: grim_object::FastMap::default(),
            particles: grim_object::FastMap::default(),
            particle_class: None,
            audio: None,
            sounds: grim_object::FastMap::default(),
            ambient: grim_object::FastMap::default(),
            mouths: grim_object::FastMap::default(),
            canvas: None,
            canvas_quads: Vec::new(),
            canvas_actors: Vec::new(),
            drawn: None,
            fonts: grim_object::FastMap::default(),
            scripted: grim_object::FastMap::default(),
            pictures: grim_object::FastMap::default(),
            durations: grim_object::FastMap::default(),
            interp_from: grim_object::FastMap::default(),
            moves: grim_object::FastMap::default(),
            channels: grim_object::FastMap::default(),
        };
        // Which events a state listens for is a bitmask it carries (`ProbeMask`), and which bit
        // stands for which event is not written down anywhere: it comes out of the game itself.
        // Over the 709 states of the game, taking the bits common to every state that declares
        // an event leaves exactly one bit per event, and only two bits of all those the states
        // use are left without a name (one is `Mover.StandOpenTimed`'s `Attach`, the other is
        // in `PlayerPawn.PlayerSwimming`, for a function the engine never sends).
        for (bit, name) in [
            (6, "Trigger"),
            (7, "UnTrigger"),
            (8, "Timer"),
            (9, "HitWall"),
            (11, "Landed"),
            (12, "ZoneChange"),
            (13, "Touch"),
            (14, "UnTouch"),
            (15, "Bump"),
            (16, "BeginState"),
            (17, "EndState"),
            (24, "AnimEnd"),
            (26, "InterpolateEnd"),
            (36, "Tick"),
            (37, "PlayerTick"),
        ] {
            let id = vm.world.names.intern(name);
            vm.probe_bit.insert(id, bit);
        }
        vm.hot = HotNames {
            location: vm.world.names.intern("Location"),
            velocity: vm.world.names.intern("Velocity"),
            acceleration: vm.world.names.intern("Acceleration"),
            physics: vm.world.names.intern("Physics"),
            collide_actors: vm.world.names.intern("bCollideActors"),
            collision_radius: vm.world.names.intern("CollisionRadius"),
            collision_height: vm.world.names.intern("CollisionHeight"),
            timer_rate: vm.world.names.intern("TimerRate"),
            tick: vm.world.names.intern("Tick"),
            player_tick: vm.world.names.intern("PlayerTick"),
            anim_rate: vm.world.names.intern("AnimRate"),
            anim_sequence: vm.world.names.intern("AnimSequence"),
            anim_frame: vm.world.names.intern("AnimFrame"),
            anim_last: vm.world.names.intern("AnimLast"),
            anim_loop: vm.world.names.intern("bAnimLoop"),
            anim_finished: vm.world.names.intern("bAnimFinished"),
            mesh: vm.world.names.intern("Mesh"),
            rotation: vm.world.names.intern("Rotation"),
            draw_type: vm.world.names.intern("DrawType"),
            draw_scale: vm.world.names.intern("DrawScale"),
            hidden: vm.world.names.intern("bHidden"),
            brush: vm.world.names.intern("Brush"),
            style: vm.world.names.intern("Style"),
            corona: vm.world.names.intern("bCorona"),
            fixed_rotation: vm.world.names.intern("bFixedRotationDir"),
            rotate_to_desired: vm.world.names.intern("bRotateToDesired"),
            block_actors: vm.world.names.intern("bBlockActors"),
            block_players: vm.world.names.intern("bBlockPlayers"),
            is_player: vm.world.names.intern("bIsPlayer"),
            is_mover: vm.world.names.intern("bIsMover"),
            collision_width: vm.world.names.intern("CollisionWidth"),
            collide_type: vm.world.names.intern("CollideType"),
            is_static: vm.world.names.intern("bStatic"),
            max_step_height: vm.world.names.intern("MaxStepHeight"),
            ground_speed: vm.world.names.intern("GroundSpeed"),
            accel_rate: vm.world.names.intern("AccelRate"),
            b_run: vm.world.names.intern("bRun"),
            walking_pct: vm.world.names.intern("WalkingPct"),
            ground_friction: vm.world.names.intern("ZoneGroundFriction"),
            base: vm.world.names.intern("Base"),
            desired_rotation: vm.world.names.intern("DesiredRotation"),
            rotation_rate: vm.world.names.intern("RotationRate"),
            name_prop: vm.world.names.intern("Name"),
            class_prop: vm.world.names.intern("Class"),
        };
        vm
    }

    /// Drops every live object (e.g. before loading another map).
    pub fn reset_heap(&mut self) {
        self.heap.clear();
        self.next_transient = 0;
        self.actors.clear();
        self.level_info = None;
        self.level = None;
        self.player = None;
        self.bsp = None;
        self.zone_actors.clear();
        self.zoned_at.clear();
        self.moves.clear();
        self.interp_from.clear();
        self.channels.clear();
        self.mouths.clear();
        self.particles.clear();
        self.poses.clear();
        self.root_moves.clear();
        self.stair_lag.clear();
        self.stair_was.clear();
        self.stair_run.clear();
        self.safe_spots.clear();
        self.travelled.clear();
        self.decals.clear();
        self.blocker_cache.clear();
        self.static_collision = None;
        self.pose_extents.clear();
        // Everything below holds actors of the level being left. Kept over a level change, the
        // first overlap of the new level sends `UnTouch` to an actor that no longer exists
        // (`HGame.orangesnail.UnTouch` on a second visit), and the drawing reads a canvas that
        // was thrown away with the level that made it.
        self.touching.clear();
        self.blockers.clear();
        // Sounds themselves outlive a level (they belong to the packages), but what was
        // playing does not: the actors making it are gone.
        self.ambient.clear();
        if let Some(audio) = self.audio.as_mut() {
            audio.stop_all_music(0.0);
        }
        self.canvas = None;
        self.canvas_quads.clear();
        self.canvas_actors.clear();
    }

    /// What stands in the way of a move this tick, for whatever wants to show it.
    pub fn blockers(&self) -> &[crate::level::Blocker] {
        &self.blockers
    }

    pub fn is_deleted(&self, key: ObjectKey) -> bool {
        self.heap.get(&key).is_some_and(|i| i.deleted)
    }

    /// Uniform random float in [0, 1). xorshift64*: deterministic across runs.
    pub fn frand(&mut self) -> f32 {
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        let v = self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (v >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Name of the state an object is in, for diagnostics.
    pub fn state_name(&mut self, obj: ObjectKey) -> Option<String> {
        let state = self.instance(obj).ok()?.state?;
        Some(self.world.names.str(self.world.states[state.0 as usize].name).to_string())
    }

    /// Hands the audio backend the volumes the game's own settings hold. The options menu
    /// writes them through `set ini:`, so they take effect as soon as a slider moves.
    pub fn apply_audio_settings(&mut self) {
        let Some(device) = self.config.ini.get("Engine.Engine", "AudioDevice").map(str::to_string) else { return };
        let read = |ini: &crate::config::Ini, key: &str, fallback: f32| -> f32 {
            ini.get(&device, key).and_then(|v| v.parse::<f32>().ok()).map(|v| if v > 1.0 { v / 255.0 } else { v }).unwrap_or(fallback)
        };
        let effects = read(&self.config.ini, "SoundVolume", 1.0);
        let volumes = grim_audio::Volumes {
            master: 1.0,
            effects,
            music: read(&self.config.ini, "MusicVolume", 1.0),
            // The game turns speech up and down with the effects; it has no setting of its own.
            speech: effects,
        };
        if let Some(audio) = self.audio.as_mut() {
            audio.set_volumes(volumes);
        }
    }

    pub fn warn(&mut self, w: impl Into<String>) {
        *self.warnings.entry(w.into()).or_default() += 1;
    }

    /// A warning from inside a native, named with the script that called it.
    pub fn warn_here(&mut self, w: impl std::fmt::Display) {
        let where_ = match self.site {
            Some((obj, func)) => {
                let frame = Frame { obj, func, locals: Vec::new(), code: Default::default(), pc: 0, switch: None, iters: Vec::new() };
                self.frame_path(&frame)
            }
            None => "the engine".to_string(),
        };
        self.warn(format!("{w} in {where_}"));
    }

    /// The function a frame is running, for a warning to say where it happened.
    fn frame_path(&self, f: &Frame) -> String {
        if let Some(info) = f.func.and_then(|id| self.infos.get(&id)) {
            return info.path.clone();
        }
        // State code has no function to name: the class and the state it is in say where it is.
        match self.heap.get(&f.obj) {
            Some(inst) => {
                let class = self.world.names.str(self.world.class(inst.class).name);
                match inst.state.map(|s| self.world.names.str(self.world.state(s).name)) {
                    Some(state) => format!("{class}.{state} state code"),
                    None => format!("{class} state code"),
                }
            }
            None => "state code".to_string(),
        }
    }

    // ------------------------------------------------------------------ heap

    /// The live object for `key`, materialized from its package on first access.
    pub fn instance(&mut self, key: ObjectKey) -> VmResult<&mut Instance> {
        if !self.heap.contains_key(&key) {
            if key.package == TRANSIENT {
                return fail(format!("transient object {} does not exist", key.export));
            }
            if key.package == grim_object::INTRINSIC {
                // Default object of an intrinsic class: no script properties.
                let class = ClassId(key.export);
                let name = self.world.class(class).name;
                self.heap.insert(key, Instance { class, name, values: Vec::new(), state: None, thread: None, state_generation: 0, deleted: false, disabled: Vec::new() });
                return Ok(self.heap.get_mut(&key).unwrap());
            }
            let pkg = self.world.package(key.package).clone();
            let name = self.world.names.intern(pkg.object_name(ObjectRef::Export(key.export)));
            let inst = if pkg.class_name(ObjectRef::Export(key.export)) == "Class" {
                // The default object of a class shares the class key.
                let class = self.world.link_class(key).map_err(|e| VmError::script(e.0))?;
                Instance {
                    class,
                    name,
                    values: self.world.class(class).defaults.clone(),
                    state: None,
                    thread: None,
                    state_generation: 0,
                    deleted: false,
                    disabled: Vec::new(),
                }
            } else {
                let o = self.world.instantiate(&pkg, key.export).map_err(|e| VmError::script(e.0))?;
                // What the object named and the game could not load, kept as a warning rather
                // than thrown away: the reference is empty and the object exists all the same.
                for missing in std::mem::take(&mut self.world.unresolved) {
                    self.warn(format!("cannot load {missing}"));
                }
                Instance { class: o.class, name, values: o.values, state: None, thread: None, state_generation: 0, deleted: false, disabled: Vec::new() }
            };
            self.heap.insert(key, inst);
            self.fill_object_props(key);
        }
        Ok(self.heap.get_mut(&key).unwrap())
    }

    /// `Object.Name` and `Object.Class` have a slot in every layout, but the scripts cannot
    /// write them: the object system does, as it creates the object.
    fn fill_object_props(&mut self, key: ObjectKey) {
        let Some(inst) = self.heap.get(&key) else { return };
        let (class, name) = (inst.class, inst.name);
        let class_key = self.world.class(class).key;
        let slot = |w: &World, n: NameId| w.class(class).prop(n).map(|p| p.slot as usize);
        if let Some(s) = slot(&self.world, self.hot.name_prop) {
            if let Some(v) = self.heap.get_mut(&key).and_then(|i| i.values.get_mut(s)) {
                *v = Value::Name(name);
            }
        }
        if let Some(s) = slot(&self.world, self.hot.class_prop) {
            if let Some(v) = self.heap.get_mut(&key).and_then(|i| i.values.get_mut(s)) {
                *v = Value::Object(class_key);
            }
        }
    }

    /// How big a mesh is, in its own units: the size of the box it was stored with, times the
    /// scale it is stored at. Read from the mesh once and kept, because the shadows and the
    /// drawing ask for it every frame.
    pub fn mesh_extent(&mut self, mesh: ObjectKey) -> Option<[f32; 3]> {
        if let Some(size) = self.mesh_extents.get(&mesh).copied() {
            return Some(size);
        }
        let pkg = self.world.package(mesh.package).clone();
        let loaded = grim_assets::load_export(&pkg, mesh.export).ok()?;
        let lod = match &loaded.asset {
            grim_assets::Asset::LodMesh(m) => &m.mesh,
            grim_assets::Asset::SkeletalMesh(m) => &m.lod.mesh,
            _ => return None,
        };
        let b = lod.bounding_box;
        let size = [
            (b.max.x - b.min.x) * lod.scale.x.abs(),
            (b.max.y - b.min.y) * lod.scale.y.abs(),
            (b.max.z - b.min.z) * lod.scale.z.abs(),
        ];
        self.mesh_extents.insert(mesh, size);
        Some(size)
    }

    /// The class of that name, whatever package declares it.
    pub fn class_named(&mut self, name: &str) -> Option<ClassId> {
        if let Some(&id) = self.named_classes.get(name) {
            return id;
        }
        let id = self
            .world
            .classes
            .iter()
            .position(|c| self.world.names.str(c.name).eq_ignore_ascii_case(name))
            .map(|i| ClassId(i as u32));
        self.named_classes.insert(name.to_string(), id);
        id
    }

    pub fn class_of(&mut self, key: ObjectKey) -> VmResult<ClassId> {
        Ok(self.instance(key)?.class)
    }

    /// Creates a new object of `class` from its defaults.
    pub fn new_object(&mut self, class: ClassId, name: Option<NameId>) -> ObjectKey {
        let key = ObjectKey { package: TRANSIENT, export: self.next_transient };
        self.next_transient += 1;
        let name = name.unwrap_or_else(|| {
            let n = format!("{}{}", self.world.names.str(self.world.class(class).name), key.export);
            self.world.names.intern(&n)
        });
        let values = self.world.class(class).defaults.clone();
        self.heap.insert(key, Instance { class, name, values, state: None, thread: None, state_generation: 0, deleted: false, disabled: Vec::new() });
        self.fill_object_props(key);
        key
    }

    pub fn class_key(&self, class: ClassId) -> Option<ObjectKey> {
        self.world.class(class).key
    }

    /// Reads a property of an object by name.
    /// Reads a property by its interned name, which the hot loops keep around: interning one
    /// lowercases the string, and that allocation adds up over a level's worth of actors.
    pub fn get_prop_id(&mut self, obj: ObjectKey, n: NameId) -> VmResult<Value> {
        // An object already in the heap is read with one look-up: this is the hottest path of
        // the engine, taken thousands of times a tick.
        if let Some(inst) = self.heap.get(&obj) {
            if let Some(p) = self.world.class(inst.class).prop(n) {
                return Ok(inst.values[p.slot as usize].clone());
            }
        }
        let class = self.class_of(obj)?;
        let slot = self.world.class(class).prop(n).map(|p| p.slot as usize).ok_or_else(|| {
            let name = self.world.names.str(n).to_string();
            VmError::script(format!("{} has no property {name}", self.world.names.str(self.world.class(class).name)))
        })?;
        Ok(self.instance(obj)?.values[slot].clone())
    }

    /// The actors a trace has to look at: every one, or while a tick runs the ones that
    /// collided when it began and those spawned since. Actors are a few thousand once a hub
    /// has been played for a while (the destroyed stay in the list), and a camera's trace went
    /// over all of them to find the few it could meet.
    pub(crate) fn trace_candidates(&self) -> Vec<ObjectKey> {
        match &self.trace_candidates {
            Some((colliding, seen)) => {
                let mut out = colliding.clone();
                out.extend_from_slice(self.actors.get(*seen..).unwrap_or(&[]));
                out
            }
            None => self.actors.clone(),
        }
    }

    /// What `GRIM_PROFILE_SCRIPTS` gathered, the costliest first: path, milliseconds with what
    /// each call called, and calls. The totals start again from here.
    pub fn take_script_profile(&mut self) -> Vec<(String, f64, u64)> {
        let Some(profile) = self.profile.as_mut() else { return Vec::new() };
        let taken: Vec<(FuncId, (f64, u64))> = profile.drain().collect();
        let mut out: Vec<(String, f64, u64)> = taken.into_iter().map(|(f, (ms, n))| (self.func_info(f).path.clone(), ms, n)).collect();
        out.sort_by(|a, b| b.1.total_cmp(&a.1));
        out
    }

    /// A vector property read in place, without copying the value out: the location of every
    /// actor is asked for several times a tick.
    pub fn vec3_id(&mut self, obj: ObjectKey, n: NameId) -> Option<[f32; 3]> {
        if let Some(inst) = self.heap.get(&obj) {
            let p = self.world.class(inst.class).prop(n)?;
            return floats3(&inst.values[p.slot as usize]).ok();
        }
        self.get_prop_id(obj, n).ok().and_then(|v| floats3(&v).ok())
    }

    /// A rotator property read in place, the same way.
    pub fn rot3_id(&mut self, obj: ObjectKey, n: NameId) -> Option<[i32; 3]> {
        if let Some(inst) = self.heap.get(&obj) {
            let p = self.world.class(inst.class).prop(n)?;
            return ints3(&inst.values[p.slot as usize]).ok();
        }
        self.get_prop_id(obj, n).ok().and_then(|v| ints3(&v).ok())
    }

    pub fn set_prop_id(&mut self, obj: ObjectKey, n: NameId, v: Value) -> VmResult<()> {
        if let Some(inst) = self.heap.get_mut(&obj) {
            if let Some(p) = self.world.class(inst.class).prop(n) {
                inst.values[p.slot as usize] = v;
                return Ok(());
            }
        }
        let class = self.class_of(obj)?;
        let slot = self.world.class(class).prop(n).map(|p| p.slot as usize).ok_or_else(|| {
            let name = self.world.names.str(n).to_string();
            VmError::script(format!("{} has no property {name}", self.world.names.str(self.world.class(class).name)))
        })?;
        self.instance(obj)?.values[slot] = v;
        Ok(())
    }

    pub fn get_prop(&mut self, obj: ObjectKey, name: &str) -> VmResult<Value> {
        let n = self.world.names.intern(name);
        self.get_prop_id(obj, n)
    }

    /// Reads one element of a static array property.
    pub fn get_prop_at(&mut self, obj: ObjectKey, name: &str, index: u32) -> VmResult<Value> {
        let class = self.class_of(obj)?;
        let n = self.world.names.intern(name);
        let prop = self.world.class(class).prop(n).cloned().ok_or_else(|| {
            VmError::script(format!("{} has no property {name}", self.world.names.str(self.world.class(class).name)))
        })?;
        if index >= prop.array_dim {
            return fail(format!("{name}[{index}] out of {} slots", prop.array_dim));
        }
        Ok(self.instance(obj)?.values[(prop.slot + index) as usize].clone())
    }

    pub fn set_prop(&mut self, obj: ObjectKey, name: &str, v: Value) -> VmResult<()> {
        let n = self.world.names.intern(name);
        self.set_prop_id(obj, n, v)
    }

    pub fn set_prop_at(&mut self, obj: ObjectKey, name: &str, index: u32, v: Value) -> VmResult<()> {
        let n = self.world.names.intern(name);
        self.set_prop_id_at(obj, n, index, v)
    }

    /// Writes one element of a static array property by its interned name.
    pub fn set_prop_id_at(&mut self, obj: ObjectKey, n: NameId, index: u32, v: Value) -> VmResult<()> {
        let class = self.class_of(obj)?;
        let name = self.world.names.str(n).to_string();
        let prop = self.world.class(class).prop(n).cloned().ok_or_else(|| {
            VmError::script(format!("{} has no property {name}", self.world.names.str(self.world.class(class).name)))
        })?;
        if index >= prop.array_dim {
            return fail(format!("{name}[{index}] out of {} slots", prop.array_dim));
        }
        self.instance(obj)?.values[(prop.slot + index) as usize] = v;
        Ok(())
    }

    /// Every live object of a class, by name: for looking into what the game built.
    pub fn objects_of_class(&mut self, name: &str) -> Vec<ObjectKey> {
        let keys: Vec<ObjectKey> = self.heap.keys().copied().collect();
        keys.into_iter()
            .filter(|&k| {
                self.class_of(k).is_ok_and(|c| {
                    let n = self.world.class(c).name;
                    self.world.names.str(n).eq_ignore_ascii_case(name)
                })
            })
            .collect()
    }

    pub fn object_name(&mut self, obj: ObjectKey) -> String {
        match self.instance(obj) {
            Ok(i) => {
                let n = i.name;
                self.world.names.str(n).to_string()
            }
            Err(_) => "<missing>".into(),
        }
    }

    // ------------------------------------------------------------------ functions

    pub fn func_info(&mut self, f: FuncId) -> Arc<FuncInfo> {
        if let Some(i) = self.infos.get(&f) {
            return i.clone();
        }
        let fd = self.world.function(f).clone();
        let path = self.world.path(fd.key);
        let mut zero = Vec::with_capacity(fd.slots as usize);
        for l in &fd.locals {
            for _ in 0..l.array_dim {
                zero.push(self.world.zero(&l.ty));
            }
        }
        let info = Arc::new(FuncInfo {
            native: if fd.flags & func::NATIVE != 0 { self.natives.get(&path) } else { None },
            path,
            flags: fd.flags,
            params: fd.params.iter().map(|&i| fd.locals[i].slot as usize).collect(),
            out: fd.params.iter().map(|&i| fd.locals[i].flags & cpf::OUT_PARM != 0).collect(),
            ret: fd.ret.map(|i| fd.locals[i].slot as usize),
            zero,
            code: fd.code.clone(),
        });
        self.infos.insert(f, info.clone());
        info
    }

    /// Function a virtual call reaches: current state (and its parents), then the class chain.
    pub fn find_function(&mut self, obj: ObjectKey, name: NameId, global: bool) -> VmResult<Option<FuncId>> {
        let inst = self.instance(obj)?;
        let (start_state, start_class) = (if global { None } else { inst.state }, inst.class);
        // Walking the state and class chains for a name is done for every actor on every tick,
        // and neither chain changes while the game runs: what was found once is kept.
        let key = (start_class, start_state, name);
        if let Some(&found) = self.found_functions.get(&key) {
            return Ok(found);
        }
        let found = self.search_function(start_state, start_class, name);
        self.found_functions.insert(key, found);
        Ok(found)
    }

    fn search_function(&self, state: Option<StateId>, class: ClassId, name: NameId) -> Option<FuncId> {
        let (mut state, mut class) = (state, Some(class));
        while let Some(s) = state {
            let sd = self.world.state(s);
            if let Some(&f) = sd.functions.get(&name) {
                return Some(f);
            }
            state = sd.super_;
        }
        while let Some(c) = class {
            let cd = self.world.class(c);
            if let Some(&f) = cd.functions.get(&name) {
                return Some(f);
            }
            class = cd.super_;
        }
        None
    }

    /// The `auto state` of the most derived class that declares one.
    pub fn auto_state(&self, mut class: ClassId) -> Option<StateId> {
        loop {
            let c = self.world.class(class);
            if let Some(&s) = c.states.values().find(|&&s| self.world.state(s).flags & STATE_AUTO != 0) {
                return Some(s);
            }
            class = c.super_?;
        }
    }

    /// The state that holds `label`, searching `state` and then the states it extends, and the
    /// label's code offset. A state that redeclares one without labels keeps its parent's code:
    /// `HChar.patrol` only carries a `Stop`, and the patrol code is the one in `HPawn.patrol`.
    fn find_label(&self, state: StateId, label: NameId) -> Option<(StateId, u32)> {
        let mut state = Some(state);
        while let Some(s) = state {
            let sd = self.world.state(s);
            if let Some(&off) = sd.labels.get(&label) {
                return Some((s, off));
            }
            state = sd.super_;
        }
        None
    }

    pub fn find_state(&self, mut class: ClassId, name: NameId) -> Option<StateId> {
        loop {
            let c = self.world.class(class);
            if let Some(&s) = c.states.get(&name) {
                return Some(s);
            }
            class = c.super_?;
        }
    }

    /// Whether the object has switched this notification off with `Disable`.
    fn event_disabled(&mut self, obj: ObjectKey, n: NameId) -> bool {
        if self.heap.get(&obj).is_some_and(|i| i.disabled.contains(&n)) {
            return true;
        }
        !self.listens_for(obj, n)
    }

    /// Whether an actor's current state listens for an event. A state says so with two masks:
    /// `ProbeMask` for what it adds to what its class already listens for, and `IgnoreMask` for
    /// what it silences (`ignores Touch;`). An event with no bit of its own is always sent.
    fn listens_for(&mut self, obj: ObjectKey, n: NameId) -> bool {
        let Some(&bit) = self.probe_bit.get(&n) else { return true };
        let Some(inst) = self.heap.get(&obj) else { return true };
        let (class, state) = (inst.class, inst.state);
        let cd = self.world.class(class);
        let (mut probe, mut ignore) = (cd.probe_mask, cd.ignore_mask);
        if let Some(s) = state {
            let sd = self.world.state(s);
            probe |= sd.probe_mask;
            ignore &= sd.ignore_mask;
        }
        let listens = (probe & ignore) >> bit & 1 == 1;
        if !listens && self.switches.trace_probe {
            let class = self.world.names.str(self.world.class(class).name).to_string();
            let event = self.world.names.str(n).to_string();
            self.warn(format!("probe: {class} does not listen for {event} in {:?}", state.map(|s| self.world.names.str(self.world.state(s).name).to_string())));
        }
        listens
    }

    /// Calls an event (a function looked up by name) if the object defines it.
    /// Calls an event by its interned name, for the ones sent every frame.
    pub fn call_event_id(&mut self, obj: ObjectKey, n: NameId, args: Vec<Value>) -> VmResult<Option<Value>> {
        if self.event_disabled(obj, n) {
            return Ok(None);
        }
        match self.find_function(obj, n, false)? {
            Some(f) => Ok(Some(self.call_with_values(obj, f, args)?.0)),
            None => Ok(None),
        }
    }

    /// Calls a function and hands back its arguments, for the ones that answer through `out`
    /// parameters.
    pub fn call_with_out(&mut self, obj: ObjectKey, name: &str, args: Vec<Value>) -> VmResult<Option<Vec<Value>>> {
        let n = self.world.names.intern(name);
        match self.find_function(obj, n, false)? {
            Some(f) => Ok(Some(self.call_with_values(obj, f, args)?.1)),
            None => Ok(None),
        }
    }

    pub fn call_event(&mut self, obj: ObjectKey, name: &str, args: Vec<Value>) -> VmResult<Option<Value>> {
        let n = self.world.names.intern(name);
        if self.event_disabled(obj, n) {
            return Ok(None);
        }
        match self.find_function(obj, n, false)? {
            Some(f) => Ok(Some(self.call_with_values(obj, f, args)?.0)),
            None => Ok(None),
        }
    }

    /// Calls `f` on `obj` with already evaluated arguments; returns the result and the final
    /// argument values (for out parameters).
    pub fn call_with_values(&mut self, obj: ObjectKey, f: FuncId, mut args: Vec<Value>) -> VmResult<(Value, Vec<Value>)> {
        let info = self.func_info(f);
        if let Some(wanted) = self.switches.trace_call.clone() {
            if info.path.to_lowercase().contains(&wanted.to_lowercase()) {
                eprintln!("call {}", info.path);
            }
        }
        if args.len() > info.params.len() {
            return fail(format!("{}: {} arguments for {} parameters", info.path, args.len(), info.params.len()));
        }
        while args.len() < info.params.len() {
            args.push(info.zero[info.params[args.len()]].clone());
        }
        if self.depth >= MAX_DEPTH {
            return fail(format!("{}: call depth limit reached", info.path));
        }
        self.depth += 1;
        // GRIM_TRACE_FUNC=<text>,<text> logs every call whose path contains one of them, with
        // its arguments and the level clock.
        if let Some(filter) = self.trace_func.as_deref() {
            if filter.split(',').any(|f| !f.is_empty() && info.path.contains(f)) {
                let shown: Vec<String> = args.iter().map(|a| format!("{a:?}")).collect();
                let now = match self.level_info.map(|i| self.get_prop(i, "TimeSeconds")) {
                    Some(Ok(Value::Float(t))) => t,
                    _ => 0.0,
                };
                eprintln!(
                    "{:indent$}{now:8.2} [{}:{}] {}({})",
                    "",
                    obj.package.0,
                    obj.export,
                    info.path,
                    shown.join(", "),
                    indent = self.depth as usize
                );
            }
        }
        let started = self.profile.is_some().then(web_time::Instant::now);
        let r = self.call_inner(obj, f, &info, args);
        if let (Some(t), Some(profile)) = (started, self.profile.as_mut()) {
            let entry = profile.entry(f).or_default();
            entry.0 += t.elapsed().as_secs_f64() * 1000.0;
            entry.1 += 1;
        }
        self.depth -= 1;
        r.map_err(|mut e| {
            e.trace.push(info.path.clone());
            e
        })
    }

    fn call_inner(&mut self, obj: ObjectKey, f: FuncId, info: &FuncInfo, args: Vec<Value>) -> VmResult<(Value, Vec<Value>)> {
        if info.flags & func::NATIVE != 0 {
            let Some(native) = info.native else {
                return Err(VmError { kind: VmErrorKind::MissingNative(info.path.clone()), trace: Vec::new() });
            };
            let present = std::mem::take(&mut self.present);
            let mut call = NativeCall { target: obj, func: f, args, present, iterations: None };
            let ret = native(self, &mut call)?;
            return Ok((ret, call.args));
        }
        // The locals of a call are the function's zeroed ones. The room they take is handed
        // back when the call returns and taken again by the next: a tick makes thousands of
        // calls, and asking the allocator for each one of them is what that costs.
        let mut locals = self.spare_locals.pop().unwrap_or_default();
        locals.clear();
        locals.extend_from_slice(&info.zero);
        let mut frame = Frame {
            obj,
            func: Some(f),
            locals,
            code: info.code.clone(),
            pc: 0,
            switch: None,
            iters: Vec::new(),
        };
        for (i, v) in args.into_iter().enumerate() {
            frame.locals[info.params[i]] = v;
        }
        let ret = match self.run(&mut frame)? {
            Flow::Return(v) => v,
            _ => info.ret.map(|r| frame.locals[r].clone()).unwrap_or(Value::Int(0)),
        };
        let outs: Vec<Value> = info.params.iter().map(|&p| frame.locals[p].clone()).collect();
        self.spare_locals.push(std::mem::take(&mut frame.locals));
        Ok((ret, outs))
    }

    /// Evaluates call arguments in the caller's frame, calls, and writes out parameters back.
    fn invoke(&mut self, caller: &mut Frame, target: ObjectKey, f: FuncId, args: &[Expr]) -> VmResult<Value> {
        self.site = Some((caller.obj, caller.func));
        let info = self.func_info(f);
        // GRIM_TRACE_CALL=<part of a path> says when the scripts reach a function, which is
        // how to tell a path that is not taken from one that is.
        if let Some(wanted) = self.switches.trace_call.clone() {
            if info.path.to_lowercase().contains(&wanted.to_lowercase()) {
                eprintln!("call {}", info.path);
            }
        }
        // `&&` / `||` wrap their second operand in a Skip: evaluate it only when needed.
        if let [a, Expr::Skip { expr: bexpr, .. }] = args {
            let and = info.path == "Core.Object.AndAnd_BoolBool";
            if and || info.path == "Core.Object.OrOr_BoolBool" {
                let a = self.eval_bool(caller, a, caller.obj)?;
                if a != and {
                    return Ok(Value::Bool(a));
                }
                return Ok(Value::Bool(self.eval_bool(caller, bexpr, caller.obj)?));
            }
        }
        let mut vals = Vec::with_capacity(info.params.len());
        let mut outs = Vec::new();
        let mut present = vec![false; info.params.len()];
        for (i, a) in args.iter().enumerate() {
            if i >= info.params.len() {
                return fail(format!("{}: too many arguments", info.path));
            }
            present[i] = !matches!(a, Expr::Nothing);
            match a {
                Expr::Nothing => vals.push(info.zero[info.params[i]].clone()),
                a if info.out[i] => {
                    let p = self.place(caller, a, caller.obj)?;
                    vals.push(self.read(caller, &p)?);
                    outs.push((i, p));
                }
                a => vals.push(self.eval(caller, a, caller.obj)?),
            }
        }
        self.present = present;
        let (ret, out_vals) = self.call_with_values(target, f, vals)?;
        for (i, p) in outs {
            self.write(caller, &p, out_vals[i].clone())?;
        }
        Ok(ret)
    }

    fn callee(&mut self, frame: &Frame, e: &Expr, target: ObjectKey) -> VmResult<FuncId> {
        let pkg = frame.code.package.unwrap();
        match e {
            Expr::FinalFunction { function, .. } => {
                let k = self.resolve(pkg, *function)?;
                self.world.link_function(k).map_err(|e| VmError::script(e.0))
            }
            Expr::VirtualFunction { name, .. } | Expr::GlobalFunction { name, .. } => {
                let p = self.world.package(pkg).clone();
                let n = self.world.name(&p, *name);
                let global = matches!(e, Expr::GlobalFunction { .. });
                self.find_function(target, n, global)?.ok_or_else(|| {
                    VmError::script(format!("function {} not found", self.world.names.str(n)))
                })
            }
            Expr::Native { index, .. } => self
                .natives_by_index
                .get(index)
                .copied()
                .ok_or_else(|| VmError::script(format!("no native function with index {index}"))),
            _ => unreachable!(),
        }
    }

    /// What a reference in a package's tables points at. The bytecode asks for the same few
    /// over and over, and the answer never changes.
    fn resolve(&mut self, pkg: PkgId, r: ObjectRef) -> VmResult<ObjectKey> {
        if let Some(&found) = self.resolved.get(&(pkg, r)) {
            return Ok(found);
        }
        let found = self
            .world
            .resolve_in(pkg, r)
            .map_err(|e| VmError::script(e.0))?
            .ok_or_else(|| VmError::script("null reference in bytecode"))?;
        self.resolved.insert((pkg, r), found);
        Ok(found)
    }

    // ------------------------------------------------------------------ execution

    fn run(&mut self, f: &mut Frame) -> VmResult<Flow> {
        // The code of a frame does not change while it runs, so it is taken hold of once
        // rather than on every statement.
        let code = f.code.clone();
        while f.pc < code.statements.len() {
            let st = &code.statements[f.pc];
            f.pc += 1;
            match self.exec(f, &st.expr)? {
                Flow::Next => {}
                Flow::Jump(off) => f.pc = Self::target(f, off)?,
                other => return Ok(other),
            }
        }
        Ok(Flow::Next)
    }

    fn target(f: &Frame, off: u32) -> VmResult<usize> {
        f.code.offsets.get(&off).copied().ok_or_else(|| VmError::script(format!("jump to {off:#x} is not a statement")))
    }

    fn exec(&mut self, f: &mut Frame, e: &Expr) -> VmResult<Flow> {
        let obj = f.obj;
        Ok(match e {
            Expr::Jump(t) => Flow::Jump(*t as u32),
            Expr::JumpIfNot { target, cond } => {
                if self.eval_bool(f, cond, obj)? {
                    Flow::Next
                } else {
                    Flow::Jump(*target as u32)
                }
            }
            Expr::Switch { value, .. } => {
                f.switch = Some(self.eval(f, value, obj)?);
                Flow::Next
            }
            // A case that matches leaves the switch behind: the cases that follow are skipped
            // rather than compared again, which is what lets two of them share one body. The
            // game needs it: `HProp.TickPickupOrDropOff` writes `case 2: case 3:` over the same
            // code, and comparing again at the second one sent every jellybean pickup to the
            // switch's `default` ("ERROR: Pickup type not recognized") instead of finishing.
            Expr::Case { target, value } => match (value, f.switch.as_ref()) {
                (None, _) | (Some(_), None) => Flow::Next,
                (Some(v), Some(_)) => {
                    let v = self.eval(f, v, obj)?;
                    let taken = match (&v, f.switch.as_ref()) {
                        // Strings compare without case, which is how the game's own scripts
                        // switch on commands written in any case.
                        (Value::Str(a), Some(Value::Str(b))) => a.eq_ignore_ascii_case(b),
                        (a, b) => Some(a) == b,
                    };
                    if taken {
                        f.switch = None;
                        Flow::Next
                    } else {
                        Flow::Jump(*target as u32)
                    }
                }
            },
            Expr::Return(v) => match v.as_ref() {
                Expr::Nothing => Flow::Return(match f.func {
                    Some(func) => {
                        let info = self.func_info(func);
                        info.ret.map(|r| f.locals[r].clone()).unwrap_or(Value::Int(0))
                    }
                    None => Value::Int(0),
                }),
                v => Flow::Return(self.eval(f, v, obj)?),
            },
            Expr::Stop => Flow::Stop,
            Expr::Nothing | Expr::LabelTable(_) => Flow::Next,
            Expr::GotoLabel(l) => match self.eval(f, l, obj)? {
                Value::Name(n) => Flow::GotoLabel(n),
                v => return fail(format!("goto label {v:?}")),
            },
            Expr::Assert { line, cond } => {
                if !self.eval_bool(f, cond, obj)? {
                    return fail(format!("assertion failed at line {line}"));
                }
                Flow::Next
            }
            Expr::Iterator { expr, skip } => {
                let (rows, outs) = self.start_iterator(f, expr)?;
                let mut end_pc = Self::target(f, *skip as u32)?;
                if matches!(f.code.statements.get(end_pc).map(|s| &s.expr), Some(Expr::IteratorPop)) {
                    end_pc += 1;
                }
                if rows.is_empty() {
                    self.end_iterator(f, &outs)?;
                    f.pc = end_pc;
                } else {
                    let first = rows[0].clone();
                    for (i, p) in &outs {
                        self.write(f, p, first[*i].clone())?;
                    }
                    f.iters.push(IterState { rows, next: 1, outs, body_pc: f.pc, end_pc });
                }
                Flow::Next
            }
            Expr::IteratorNext => {
                let Some(it) = f.iters.last_mut() else { return fail("IteratorNext without iterator") };
                if it.next < it.rows.len() {
                    let row = it.rows[it.next].clone();
                    it.next += 1;
                    let (outs, body) = (it.outs.clone(), it.body_pc);
                    for (i, p) in &outs {
                        self.write(f, p, row[*i].clone())?;
                    }
                    f.pc = body;
                } else {
                    let (outs, end) = (it.outs.clone(), it.end_pc);
                    f.iters.pop();
                    self.end_iterator(f, &outs)?;
                    f.pc = end;
                }
                Flow::Next
            }
            Expr::IteratorPop => {
                f.iters.pop();
                Flow::Next
            }
            other => {
                self.eval(f, other, obj)?;
                Flow::Next
            }
        })
    }

    /// An iterator that runs out leaves no actor behind: Unreal clears the variable it writes
    /// each actor into when the list ends, and only a `break` leaves one in it. Keeping the
    /// last one instead makes a search that found nothing look like it found the last actor of
    /// the level (`HPawn.patrol.startup` took a cutscene camera for its first patrol point).
    fn end_iterator(&mut self, f: &mut Frame, outs: &[(usize, Place)]) -> VmResult<()> {
        for (_, p) in outs {
            if matches!(self.read(f, p)?, Value::Object(_)) {
                self.write(f, p, Value::Object(None))?;
            }
        }
        Ok(())
    }

    fn start_iterator(&mut self, f: &mut Frame, e: &Expr) -> VmResult<(Vec<Vec<Value>>, Vec<(usize, Place)>)> {
        // `foreach Something.AllActors(...)` reaches the iterator through a context, and it is
        // that object the iterator runs on.
        let (obj, e) = match e {
            Expr::Context { object, expr, .. } => match self.eval(f, object, f.obj)? {
                Value::Object(Some(o)) => (o, expr.as_ref()),
                _ => return Ok((Vec::new(), Vec::new())),
            },
            _ => (f.obj, e),
        };
        let (Expr::FinalFunction { args, .. } | Expr::VirtualFunction { args, .. } | Expr::Native { args, .. }) = e else {
            return fail("iterator over a non-call expression");
        };
        let fid = self.callee(f, e, obj)?;
        let info = self.func_info(fid);
        let mut vals = Vec::new();
        let mut outs = Vec::new();
        for (i, a) in args.iter().enumerate() {
            match a {
                Expr::Nothing => vals.push(info.zero[info.params[i]].clone()),
                a if info.out[i] => {
                    let p = self.place(f, a, obj)?;
                    vals.push(self.read(f, &p)?);
                    outs.push((i, p));
                }
                a => vals.push(self.eval(f, a, obj)?),
            }
        }
        while vals.len() < info.params.len() {
            vals.push(info.zero[info.params[vals.len()]].clone());
        }
        let Some(native) = info.native else {
            return Err(VmError { kind: VmErrorKind::MissingNative(info.path.clone()), trace: Vec::new() });
        };
        let present = args.iter().map(|a| !matches!(a, Expr::Nothing)).chain(std::iter::repeat(false)).take(vals.len()).collect();
        let mut call = NativeCall { target: obj, func: fid, args: vals, present, iterations: Some(Vec::new()) };
        native(self, &mut call)?;
        Ok((call.iterations.unwrap_or_default(), outs))
    }

    /// Names what an expression reads, for a warning to say what was accessed on nothing.
    fn read_name(&mut self, f: &Frame, e: &Expr) -> String {
        match e {
            Expr::InstanceVariable(r) | Expr::LocalVariable(r) | Expr::DefaultVariable(r) => match self.prop_of(f, *r) {
                Ok(p) => self.world.names.str(p.name).to_string(),
                Err(_) => "?".to_string(),
            },
            Expr::BoolVariable(inner) | Expr::StructMember { expr: inner, .. } => self.read_name(f, inner),
            Expr::ArrayElement { array, .. } | Expr::DynArrayElement { array, .. } => self.read_name(f, array),
            Expr::VirtualFunction { name, .. } => match f.code.package {
                Some(p) => format!("{}()", self.world.package(p).name(*name)),
                None => "?()".to_string(),
            },
            _ => "?".to_string(),
        }
    }

    fn eval_bool(&mut self, f: &mut Frame, e: &Expr, target: ObjectKey) -> VmResult<bool> {
        match self.eval(f, e, target)? {
            Value::Bool(b) => Ok(b),
            v => fail(format!("expected bool, got {v:?}")),
        }
    }

    pub fn eval(&mut self, f: &mut Frame, e: &Expr, target: ObjectKey) -> VmResult<Value> {
        let pkg = f.code.package.unwrap();
        Ok(match e {
            Expr::LocalVariable(_)
            | Expr::InstanceVariable(_)
            | Expr::DefaultVariable(_)
            | Expr::ArrayElement { .. }
            | Expr::DynArrayElement { .. }
            | Expr::StructMember { .. }
            | Expr::BoolVariable(_) => {
                let p = self.place(f, e, target)?;
                self.read(f, &p)?
            }
            Expr::Context { object, expr, .. } => match self.eval(f, object, target)? {
                Value::Object(Some(o)) => self.eval(f, expr, o)?,
                Value::Object(None) => {
                    let (path, what) = (self.frame_path(f), self.read_name(f, expr));
                    self.warn(format!("Accessed None in {path} reading {what}"));
                    self.zero_of(f, e)
                }
                v => return fail(format!("context on non-object {v:?}")),
            },
            Expr::ClassContext { object, expr, .. } => match self.eval(f, object, target)? {
                Value::Object(Some(c)) => self.eval(f, expr, c)?,
                Value::Object(None) => {
                    let path = self.frame_path(f);
                    self.warn(format!("Accessed None class in {path}"));
                    self.zero_of(f, e)
                }
                v => return fail(format!("class context on {v:?}")),
            },
            Expr::Let(a, b) | Expr::LetBool(a, b) => {
                let p = self.place(f, a, target)?;
                let v = self.eval(f, b, target)?;
                self.write(f, &p, v)?;
                Value::Int(0)
            }
            Expr::FinalFunction { args, .. } | Expr::VirtualFunction { args, .. } | Expr::GlobalFunction { args, .. } | Expr::Native { args, .. } => {
                let fid = self.callee(f, e, target)?;
                self.invoke(f, target, fid, args)?
            }
            Expr::IntConst(i) => Value::Int(*i),
            Expr::FloatConst(x) => Value::Float(*x),
            Expr::StringConst(s) | Expr::UnicodeStringConst(s) => Value::Str(s.clone()),
            Expr::ObjectConst(r) => Value::Object(Some(self.resolve(pkg, *r)?)),
            Expr::NameConst(n) => {
                let p = self.world.package(pkg).clone();
                Value::Name(self.world.name(&p, *n))
            }
            Expr::RotationConst(r) => rotator(r.pitch, r.yaw, r.roll),
            Expr::VectorConst(v) => vector(v.x, v.y, v.z),
            Expr::ByteConst(b) => Value::Byte(*b),
            Expr::IntConstByte(b) => Value::Int(*b as i32),
            Expr::IntZero => Value::Int(0),
            Expr::IntOne => Value::Int(1),
            Expr::True => Value::Bool(true),
            Expr::False => Value::Bool(false),
            Expr::NoObject => Value::Object(None),
            Expr::SelfRef => Value::Object(Some(target)),
            Expr::DynArrayLength(a) => match self.eval(f, a, target)? {
                Value::Array(v) => Value::Int(v.len() as i32),
                v => return fail(format!("length of non-array {v:?}")),
            },
            Expr::Skip { expr, .. } => self.eval(f, expr, target)?,
            Expr::EatString(s) => {
                self.eval(f, s, target)?;
                Value::Int(0)
            }
            Expr::StructCmpEq { a, b, .. } | Expr::StructCmpNe { a, b, .. } => {
                let eq = self.eval(f, a, target)? == self.eval(f, b, target)?;
                Value::Bool(eq == matches!(e, Expr::StructCmpEq { .. }))
            }
            Expr::DynamicCast { class, expr } => {
                let ck = self.resolve(pkg, *class)?;
                let want = self.world.link_class(ck).map_err(|e| VmError::script(e.0))?;
                match self.eval(f, expr, target)? {
                    Value::Object(Some(o)) => {
                        let c = self.class_of(o)?;
                        Value::Object(if self.world.is_child_of(c, want) { Some(o) } else { None })
                    }
                    v => v,
                }
            }
            Expr::MetaCast { class, expr } => {
                let ck = self.resolve(pkg, *class)?;
                let want = self.world.link_class(ck).map_err(|e| VmError::script(e.0))?;
                match self.eval(f, expr, target)? {
                    Value::Object(Some(c)) => {
                        let cid = self.world.class_id(c).or_else(|| self.world.link_class(c).ok());
                        Value::Object(cid.filter(|&cid| self.world.is_child_of(cid, want)).map(|_| c))
                    }
                    v => v,
                }
            }
            Expr::New { outer, name, flags, class } => {
                // `new` takes an optional outer, name and flags; left out they come as Nothing.
                let optional = |e: &Expr, vm: &mut Self, f: &mut Frame| match e {
                    Expr::Nothing => Ok(Value::Object(None)),
                    e => vm.eval(f, e, target),
                };
                optional(outer, self, f)?;
                let name = match optional(name, self, f)? {
                    Value::Str(s) if !s.is_empty() => Some(self.world.names.intern(&s)),
                    _ => None,
                };
                optional(flags, self, f)?;
                match self.eval(f, class, target)? {
                    Value::Object(Some(c)) => {
                        let cid = self.world.link_class(c).map_err(|e| VmError::script(e.0))?;
                        Value::Object(Some(self.new_object(cid, name)))
                    }
                    v => return fail(format!("new with class {v:?}")),
                }
            }
            Expr::Cast { cast, expr } => {
                let v = self.eval(f, expr, target)?;
                self.cast(*cast, v)?
            }
            other => return fail(format!("expression not supported here: {other:?}")),
        })
    }

    fn cast(&mut self, c: Cast, v: Value) -> VmResult<Value> {
        use Value::*;
        Ok(match (c, v) {
            (Cast::ByteToInt, Byte(b)) => Int(b as i32),
            (Cast::ByteToFloat, Byte(b)) => Float(b as f32),
            (Cast::IntToByte, Int(i)) => Byte(i as u8),
            (Cast::IntToFloat, Int(i)) => Float(i as f32),
            (Cast::BoolToInt, Bool(b)) => Int(b as i32),
            (Cast::FloatToByte, Float(x)) => Byte(x as i32 as u8),
            (Cast::FloatToInt, Float(x)) => Int(x as i32),
            (Cast::StringToInt, Str(s)) => Int(crate::text::parse_int(&s)),
            (Cast::StringToBool, Str(s)) => Bool(crate::text::parse_bool(&s)),
            (Cast::StringToFloat, Str(s)) => Float(crate::text::parse_float(&s)),
            (Cast::StringToName, Str(s)) => Name(self.world.names.intern(&s)),
            (Cast::ByteToString, Byte(b)) => Str(b.to_string()),
            (Cast::IntToString, Int(i)) => Str(i.to_string()),
            (Cast::BoolToString, Bool(b)) => Str(if b { "True" } else { "False" }.into()),
            (Cast::FloatToString, Float(x)) => Str(crate::text::float_to_string(x)),
            (Cast::NameToString, Name(n)) => Str(self.world.names.str(n).to_string()),
            (Cast::ObjectToString, Object(o)) => Str(match o {
                None => "None".into(),
                Some(k) => self.object_name(k),
            }),
            (Cast::VectorToString, v) => {
                let [x, y, z] = floats3(&v)?;
                Str(format!("{},{},{}", crate::text::float_to_string(x), crate::text::float_to_string(y), crate::text::float_to_string(z)))
            }
            (Cast::RotatorToString, v) => {
                let [p, y, r] = ints3(&v)?;
                Str(format!("{p},{y},{r}"))
            }
            (Cast::RotatorToVector, v) => {
                let [p, y, _] = ints3(&v)?;
                let (p, y) = (p as f32 * std::f32::consts::PI / 32768.0, y as f32 * std::f32::consts::PI / 32768.0);
                vector(p.cos() * y.cos(), p.cos() * y.sin(), p.sin())
            }
            (Cast::VectorToRotator, v) => {
                let [pitch, yaw, roll] = rotation_towards(floats3(&v)?);
                rotator(pitch, yaw, roll)
            }
            (c, v) => return fail(format!("cast {c:?} applied to {v:?}")),
        })
    }

    /// Typed zero for a context expression whose object was None.
    fn zero_of(&mut self, f: &Frame, e: &Expr) -> Value {
        // The context's size byte is the result size: 0 means no value (a void call).
        if let Expr::Context { size: 0, .. } | Expr::ClassContext { size: 0, .. } = e {
            return Value::Int(0);
        }
        let class = self.heap.get(&f.obj).map(|i| i.class);
        let (Some(class), Some(package)) = (class, f.code.package) else { return Value::Int(0) };
        let scope = Scope { package, class, function: f.func };
        let natives = self.natives_by_index.clone();
        let ty = Typer { world: &mut self.world, natives: &natives }.ty(&scope, e);
        if ty.is_none() {
            let kind = format!("{e:?}");
            let kind = kind.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("").to_string();
            self.warn(format!("untyped zero for {kind} through None"));
        }
        match ty {
            Some(Ty::Byte) => Value::Byte(0),
            Some(Ty::Int) | None => Value::Int(0),
            Some(Ty::Bool) => Value::Bool(false),
            Some(Ty::Float) => Value::Float(0.0),
            Some(Ty::Object) | Some(Ty::Class) => Value::Object(None),
            Some(Ty::Name) => Value::Name(self.world.names.intern("None")),
            Some(Ty::Str) => Value::Str(String::new()),
            Some(Ty::Array) => Value::Array(Vec::new()),
            Some(Ty::Struct(n)) => match self.struct_by_name.get(&n) {
                Some(&s) => self.world.zero(&PropType::Struct(s)),
                None => Value::Int(0),
            },
        }
    }

    // ------------------------------------------------------------------ places

    /// The property a piece of bytecode names. Resolving the reference and looking the
    /// property up is done for every variable the scripts touch, and neither answer ever
    /// changes, so each one is worked out once.
    fn prop_of(&mut self, f: &Frame, r: ObjectRef) -> VmResult<grim_object::PropDef> {
        let pkg = f.code.package.unwrap();
        if let Some(found) = self.prop_defs.get(&(pkg, r)) {
            return Ok(found.clone());
        }
        let k = self.resolve(pkg, r)?;
        let def = self
            .world
            .prop(k)
            .cloned()
            .ok_or_else(|| VmError::script(format!("{} is not a linked property", self.world.path(k))))?;
        self.prop_defs.insert((pkg, r), def.clone());
        Ok(def)
    }

    pub fn place(&mut self, f: &mut Frame, e: &Expr, target: ObjectKey) -> VmResult<Place> {
        Ok(match e {
            Expr::LocalVariable(r) => Place::Local(self.prop_of(f, *r)?.slot as usize),
            Expr::InstanceVariable(r) => Place::Field { obj: target, slot: self.prop_of(f, *r)?.slot as usize },
            Expr::DefaultVariable(r) => {
                let class = self.class_of(target)?;
                let obj = self.class_key(class).ok_or_else(|| VmError::script("default of an intrinsic class"))?;
                Place::Field { obj, slot: self.prop_of(f, *r)?.slot as usize }
            }
            Expr::ArrayElement { index, array } => {
                let i = match self.eval(f, index, f.obj)? {
                    Value::Int(i) => i,
                    Value::Byte(b) => b as i32,
                    v => return fail(format!("array index {v:?}")),
                };
                let dim = self.array_dim(f, array)?;
                let i = if i < 0 || i as u32 >= dim {
                    self.warn("Accessed array out of bounds");
                    i.clamp(0, dim as i32 - 1)
                } else {
                    i
                } as usize;
                match self.place(f, array, target)? {
                    Place::Local(s) => Place::Local(s + i),
                    Place::Field { obj, slot } => Place::Field { obj, slot: slot + i },
                    Place::Sub(b, s) => Place::Sub(b, s + i),
                    other => return fail(format!("static array element of {other:?}")),
                }
            }
            Expr::DynArrayElement { index, array } => {
                let i = match self.eval(f, index, f.obj)? {
                    Value::Int(i) if i >= 0 => i as usize,
                    v => return fail(format!("dynamic array index {v:?}")),
                };
                Place::Elem(Box::new(self.place(f, array, target)?), i)
            }
            Expr::StructMember { member, expr } => {
                let slot = self.prop_of(f, *member)?.slot as usize;
                Place::Sub(Box::new(self.place(f, expr, target)?), slot)
            }
            Expr::Context { object, expr, .. } => match self.eval(f, object, target)? {
                Value::Object(Some(o)) => self.place(f, expr, o)?,
                Value::Object(None) => {
                    let (path, what) = (self.frame_path(f), self.read_name(f, expr));
                    self.warn(format!("Accessed None in {path} reading {what}"));
                    Place::Temp(Box::new(self.zero_of(f, e)))
                }
                v => return fail(format!("context on non-object {v:?}")),
            },
            Expr::BoolVariable(inner) => self.place(f, inner, target)?,
            other => Place::Temp(Box::new(self.eval(f, other, target)?)),
        })
    }

    fn array_dim(&mut self, f: &Frame, e: &Expr) -> VmResult<u32> {
        match e {
            Expr::LocalVariable(r) | Expr::InstanceVariable(r) | Expr::DefaultVariable(r) => Ok(self.prop_of(f, *r)?.array_dim),
            Expr::StructMember { member, .. } => Ok(self.prop_of(f, *member)?.array_dim),
            Expr::Context { expr, .. } => self.array_dim(f, expr),
            other => fail(format!("static array element of {other:?}")),
        }
    }

    pub fn read(&mut self, f: &Frame, p: &Place) -> VmResult<Value> {
        Ok(match p {
            Place::Local(i) => f.locals[*i].clone(),
            Place::Field { obj, slot } => self.instance(*obj)?.values[*slot].clone(),
            Place::Sub(b, i) => match self.read(f, b)? {
                Value::Struct(v) => v.get(*i).cloned().ok_or_else(|| VmError::script("struct slot out of range"))?,
                v => return fail(format!("member of non-struct {v:?}")),
            },
            Place::Elem(b, i) => match self.read(f, b)? {
                Value::Array(v) => match v.get(*i) {
                    Some(x) => x.clone(),
                    None => return fail(format!("dynamic array index {i} out of {}", v.len())),
                },
                v => return fail(format!("element of non-array {v:?}")),
            },
            Place::Temp(v) => (**v).clone(),
        })
    }

    pub fn write(&mut self, f: &mut Frame, p: &Place, v: Value) -> VmResult<()> {
        match p {
            Place::Local(i) => f.locals[*i] = v,
            Place::Field { obj, slot } => self.instance(*obj)?.values[*slot] = v,
            Place::Sub(b, i) => {
                let mut s = self.read(f, b)?;
                match &mut s {
                    Value::Struct(fields) if *i < fields.len() => fields[*i] = v,
                    other => return fail(format!("member write into {other:?}")),
                }
                self.write(f, b, s)?;
            }
            Place::Elem(b, i) => {
                let mut a = self.read(f, b)?;
                match &mut a {
                    Value::Array(items) if *i < items.len() => items[*i] = v,
                    Value::Array(items) if *i == items.len() => items.push(v),
                    other => return fail(format!("dynamic array write at {i} into {other:?}")),
                }
                self.write(f, b, a)?;
            }
            Place::Temp(_) => {}
        }
        Ok(())
    }

    // ------------------------------------------------------------------ states

    /// `GotoState`: EndState on the old state, BeginState on the new one, and state code
    /// restarts at `label` (default `Begin`).
    pub fn goto_state(&mut self, obj: ObjectKey, state: Option<NameId>, label: Option<NameId>) -> VmResult<()> {
        let class = self.class_of(obj)?;
        let new = match state {
            Some(n) if self.world.names.str(n).eq_ignore_ascii_case("Auto") => self.auto_state(class),
            Some(n) if self.world.names.str(n) != "None" => {
                let s = self.find_state(class, n);
                if s.is_none() {
                    self.warn("GotoState to an unknown state");
                }
                s
            }
            _ => None,
        };
        let old = self.instance(obj)?.state;
        if old != new {
            if old.is_some() {
                self.call_event(obj, "EndState", vec![])?;
            }
            self.instance(obj)?.state = new;
        }
        let begin = self.world.names.intern("Begin");
        let label = label.unwrap_or(begin);
        let thread = new.and_then(|s| {
            let (node, off) = self.find_label(s, label)?;
            let sd = self.world.state(node);
            let pc = *sd.code.offsets.get(&off)?;
            Some(StateThread { node, code: sd.code.clone(), pc, latent: None })
        });
        let inst = self.instance(obj)?;
        inst.thread = thread;
        inst.state_generation += 1;
        if old != new && new.is_some() {
            self.call_event(obj, "BeginState", vec![])?;
        }
        Ok(())
    }

    /// Advances `obj`'s state code: waits for a pending latent action, then runs until the
    /// next latent call, `stop`, or the end of the code.
    pub fn run_state_code(&mut self, obj: ObjectKey, dt: f32) -> VmResult<()> {
        let Some(mut t) = self.instance(obj)?.thread.clone() else { return Ok(()) };
        match &mut t.latent {
            Some(Latent::Sleep(left)) => {
                *left -= dt;
                if *left > 0.0 {
                    self.instance(obj)?.thread = Some(t);
                    return Ok(());
                }
            }
            Some(Latent::Wait(prop, while_value)) => {
                if matches!(self.get_prop(obj, prop), Ok(Value::Bool(v)) if v == *while_value) {
                    self.instance(obj)?.thread = Some(t);
                    return Ok(());
                }
            }
            Some(Latent::Move) => {
                if self.moves.contains_key(&obj) {
                    self.instance(obj)?.thread = Some(t);
                    return Ok(());
                }
            }
            Some(Latent::NextTick(_)) | None => {}
        }
        t.latent = None;
        let mut node = t.node;
        let generation = self.instance(obj)?.state_generation;
        let mut frame = Frame { obj, func: None, locals: Vec::new(), code: t.code.clone(), pc: t.pc, switch: None, iters: Vec::new() };
        loop {
            if frame.pc >= frame.code.statements.len() {
                self.instance(obj)?.thread = None;
                return Ok(());
            }
            let code = frame.code.clone();
            let st = &code.statements[frame.pc];
            frame.pc += 1;
            self.latent = None;
            let flow = self.exec(&mut frame, &st.expr)?;
            if self.instance(obj)?.state_generation != generation {
                return Ok(()); // GotoState replaced this thread.
            }
            match flow {
                Flow::Next => {}
                Flow::Jump(off) => frame.pc = Self::target(&frame, off)?,
                Flow::Stop | Flow::Return(_) => {
                    self.instance(obj)?.thread = None;
                    return Ok(());
                }
                Flow::GotoLabel(n) => {
                    let (owner, off) = self.find_label(node, n).ok_or_else(|| VmError::script("unknown label"))?;
                    if owner != node {
                        node = owner;
                        frame.code = self.world.state(owner).code.clone();
                    }
                    frame.pc = Self::target(&frame, off)?;
                }
            }
            if let Some(l) = self.latent.take() {
                self.instance(obj)?.thread = Some(StateThread { node, code: frame.code.clone(), pc: frame.pc, latent: Some(l) });
                return Ok(());
            }
        }
    }
}

pub fn vector(x: f32, y: f32, z: f32) -> Value {
    Value::Struct(vec![Value::Float(x), Value::Float(y), Value::Float(z)])
}

/// Unreal rotator looking along a direction, in the engine's 1/65536 turns.
///
/// The angle is worked out in double precision and divided by pi as a float, then cut short
/// rather than rounded, and not brought into 0..65535. That is what the original's saves hold:
/// every `ActorShadow` pointed straight down, at `rotator(vect(0,0,1))` of `-ShadowDir`, is
/// saved at pitch 16383 and yaw -32767 — a quarter turn falling just short of 16384, and the
/// turn of the zero vector's negative zeros just short of -32768.
pub fn rotation_towards(d: [f32; 3]) -> [i32; 3] {
    let [x, y, z] = d.map(|v| v as f64);
    let units = |a: f64| (a * 32768.0 / std::f32::consts::PI as f64) as i32;
    [units(z.atan2((x * x + y * y).sqrt())), units(y.atan2(x)), 0]
}

/// Unreal rotator whose axes are the ones given: the direction it faces and the one to its
/// right. Inverts the engine's own `axes`, so a rotator built this way keeps the roll.
pub fn rotation_of_axes(forward: [f32; 3], right: [f32; 3]) -> [i32; 3] {
    let [pitch, yaw, _] = rotation_towards(forward);
    let a = |u: i32| u as f32 * std::f32::consts::PI / 32768.0;
    let (sp, cp) = a(pitch).sin_cos();
    let (sy, cy) = a(yaw).sin_cos();
    // The right and up axes the same pitch and yaw give with no roll.
    let flat_right = [-sy, cy, 0.0];
    let flat_up = [-sp * cy, -sp * sy, cp];
    let dot = |a: [f32; 3], b: [f32; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let roll = (-dot(right, flat_up)).atan2(dot(right, flat_right));
    [pitch, yaw, (roll * 32768.0 / std::f32::consts::PI) as i32]
}

pub fn rotator(p: i32, y: i32, r: i32) -> Value {
    Value::Struct(vec![Value::Int(p), Value::Int(y), Value::Int(r)])
}

pub fn floats3(v: &Value) -> VmResult<[f32; 3]> {
    match v {
        Value::Struct(f) if f.len() == 3 => match (&f[0], &f[1], &f[2]) {
            (Value::Float(x), Value::Float(y), Value::Float(z)) => Ok([*x, *y, *z]),
            _ => fail(format!("not a vector: {v:?}")),
        },
        _ => fail(format!("not a vector: {v:?}")),
    }
}

pub fn ints3(v: &Value) -> VmResult<[i32; 3]> {
    match v {
        Value::Struct(f) if f.len() == 3 => match (&f[0], &f[1], &f[2]) {
            (Value::Int(x), Value::Int(y), Value::Int(z)) => Ok([*x, *y, *z]),
            _ => fail(format!("not a rotator: {v:?}")),
        },
        _ => fail(format!("not a rotator: {v:?}")),
    }
}
