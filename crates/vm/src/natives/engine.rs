//! `Engine` natives. Those needing subsystems that do not exist yet (collision, audio,
//! animation playback) are explicit stubs that record a warning in the run report.

use grim_assets::{load_export, Asset};
use grim_object::{NameId, ObjectKey, Value};
use grim_package::ObjectRef;

use super::{MeshAnims, NativeCall, Natives};
use crate::level::MoveOrder;
use crate::vm::{vector, Latent, Vm, VmError, VmResult};

type R = VmResult<Value>;

fn class_arg(vm: &mut Vm, c: &NativeCall, i: usize) -> VmResult<Option<grim_object::ClassId>> {
    match c.obj(i)? {
        Some(k) => Ok(Some(vm.world.link_class(k).map_err(|e| VmError::script(e.0))?)),
        None => Ok(None),
    }
}

/// The mesh an actor's animations play on. An `AnimChannel` is an invisible actor with no mesh
/// of its own (its class says so: "default class for aux anim channels"); what it animates is the
/// skeleton of the actor that owns it, which is how the eye blink and facial channels drive a
/// character's face. Looking the sequence up on the channel itself found no mesh and dropped
/// every one of those animations.
pub fn anim_mesh(vm: &mut Vm, actor: ObjectKey) -> VmResult<Option<ObjectKey>> {
    if let Value::Object(Some(mesh)) = vm.get_prop(actor, "Mesh")? {
        return Ok(Some(mesh));
    }
    let Some(channel) = vm.class_named("AnimChannel") else { return Ok(None) };
    let mut at = actor;
    // An owner chain, since a channel may hang off another channel; a short guard keeps a
    // level whose owners loop from hanging the engine.
    for _ in 0..8 {
        if !vm.class_of(at).is_ok_and(|c| vm.world.is_child_of(c, channel)) {
            return Ok(None);
        }
        match vm.get_prop(at, "Owner")? {
            Value::Object(Some(owner)) => {
                if let Value::Object(Some(mesh)) = vm.get_prop(owner, "Mesh")? {
                    return Ok(Some(mesh));
                }
                at = owner;
            }
            _ => return Ok(None),
        }
    }
    Ok(None)
}

/// Animation sequences and bones of the mesh an actor displays.
pub fn mesh_anims(vm: &mut Vm, actor: ObjectKey) -> VmResult<Option<MeshAnims>> {
    let Some(mesh) = anim_mesh(vm, actor)? else { return Ok(None) };
    if let Some(a) = vm.anim_cache.get(&mesh) {
        return Ok(Some(a.clone()));
    }
    let pkg = vm.world.package(mesh.package).clone();
    let asset = load_export(&pkg, mesh.export).map_err(|e| VmError::script(e.to_string()))?.asset;
    let mut out = MeshAnims::default();
    let anim = match asset {
        Asset::SkeletalMesh(m) => {
            out.bones = m.ref_skeleton.iter().map(|b| vm.world.name(&pkg, b.name)).collect();
            vm.world.resolve(&pkg, m.default_animation).map_err(|e| VmError::script(e.0))?
        }
        Asset::LodMesh(m) => {
            out.sequences = m.mesh.anim_seqs.iter().map(|s| vm.world.name(&pkg, s.name)).collect();
            None
        }
        _ => None,
    };
    if let Some(ak) = anim {
        let apkg = vm.world.package(ak.package).clone();
        if let Asset::Animation(a) = load_export(&apkg, ak.export).map_err(|e| VmError::script(e.to_string()))?.asset {
            out.sequences = a.anim_seqs.iter().map(|s| vm.world.name(&apkg, s.name)).collect();
        }
    }
    vm.anim_cache.insert(mesh, out.clone());
    Ok(Some(out))
}

/// The group a mesh's animation sequence belongs to, as the mesh itself records it.
fn anim_group(vm: &mut Vm, actor: grim_object::ObjectKey, seq: grim_object::NameId) -> Option<grim_object::NameId> {
    let mesh = anim_mesh(vm, actor).ok()??;
    let pkg = vm.world.package(mesh.package).clone();
    let asset = grim_assets::load_export(&pkg, mesh.export).ok()?.asset;
    let seqs = match &asset {
        grim_assets::Asset::SkeletalMesh(m) => &m.lod.mesh.anim_seqs,
        grim_assets::Asset::LodMesh(m) => &m.mesh.anim_seqs,
        _ => return None,
    };
    let wanted = vm.world.names.str(seq).to_string();
    let group = seqs.iter().find(|s| pkg.name(s.name).eq_ignore_ascii_case(&wanted)).map(|s| pkg.name(s.group).to_string())?;
    Some(vm.world.names.intern(&group))
}

/// `PlayAnim(Sequence, Rate, TweenTime)`: the sequence runs at its own rate times `Rate`.
/// `EAnimType`: an animation either replaces what the channels are posing or combines with it.
const AT_REPLACE: u8 = 0;

fn set_anim(vm: &mut Vm, c: &NativeCall, looping: bool) -> R {
    let seq = c.name(0)?;
    let rate = if c.has(1) { c.float(1)? } else { 1.0 };
    let tween = if c.has(2) { c.float(2)? } else { 0.0 };
    // `LoopAnim` takes a minimum rate the others do not, so its later arguments sit one along.
    let (kind_arg, bone_arg) = if looping { (4, 5) } else { (3, 4) };
    let kind = if c.has(kind_arg) { c.byte(kind_arg)? } else { AT_REPLACE };
    // The bone named `Move` is no bone: it asks for the animation to carry the actor with it.
    let root_bone = if c.has(bone_arg) { Some(c.name(bone_arg)?) } else { None };
    let carries = root_bone.is_some_and(|b| vm.world.names.str(b).eq_ignore_ascii_case("Move"));
    // Whether this animation carries the actor is recorded once the pose on screen has been
    // kept: that pose holds the root bone of an animation that carried the actor still, and
    // dropping the record first let the blend out of a climb start from the root the whole
    // climb up, above the ledge Harry already stood on.
    let root_move = |vm: &mut Vm| {
        if carries {
            vm.root_moves.insert(c.target, crate::pose::RootMove::new(seq));
        } else {
            vm.root_moves.remove(&c.target);
        }
    };
    let whole_body = root_bone.is_none() || carries;
    vm.channel_plays(c.target);
    if whole_body && kind == AT_REPLACE {
        vm.release_channels(c.target);
    }
    // Asking again for the loop already running leaves it where it is: the scripts call this
    // every time they decide what a pawn is doing, and restarting would hold it at its first
    // frame for as long as the blend lasts.
    if looping
        && matches!(vm.get_prop(c.target, "AnimSequence"), Ok(Value::Name(playing)) if playing == seq)
        && matches!(vm.get_prop(c.target, "bAnimLoop"), Ok(Value::Bool(true)))
    {
        root_move(vm);
        let per_second = vm.sequence_of(c.target, seq).map_or(0.0, |i| i.rate / i.frames.max(1.0));
        vm.set_prop(c.target, "AnimRate", Value::Float(rate * per_second))?;
        return Ok(Value::Int(0));
    }
    // A sequence the mesh does not have leaves the actor playing what it already was. Unreal
    // does the same: its `PlayAnim` looks the sequence up first and only then writes
    // `AnimSequence`, logging the miss. Taking the name anyway left characters in the pose the
    // mesh is stored in, arms straight out, whenever a cutscene asked for an animation their
    // own set does not carry (`Lockhart` and `Normal`, for one).
    if !mesh_anims(vm, c.target)?.is_some_and(|m| m.sequences.contains(&seq)) {
        let name = vm.world.names.str(seq).to_string();
        let mesh = match anim_mesh(vm, c.target)? {
            Some(m) => vm.world.path(m),
            None => {
                let class = vm.class_of(c.target).map(|k| vm.world.names.str(vm.world.class(k).name).to_string()).unwrap_or_default();
                let owner = match vm.get_prop(c.target, "Owner")? {
                    Value::Object(Some(o)) => vm.object_name(o),
                    _ => "nobody".to_string(),
                };
                format!("a {class} of {owner} with no mesh")
            }
        };
        root_move(vm);
        vm.warn(format!("no sequence '{name}' in {mesh}"));
        // What asked for it is not left waiting: the scripts wait for an animation to end
        // (`FinishAnim` in `Pawn.DoingCutAnimate`, among others), and one that never started
        // would hold a cutscene up for ever. It is over before it began. No `AnimEnd` is raised
        // for it, though: `harry.PlayerWalking.AnimEnd` answers with `PlayInAir`, which asks
        // again for `AnimFalling` — a name that is no sequence until the fall sets one — and
        // the two called each other until the call depth ran out.
        vm.set_prop_id(c.target, vm.hot.anim_finished, Value::Bool(true))?;
        return Ok(Value::Int(0));
    }
    if vm.switches.trace_anim {
        let name = vm.world.names.str(seq).to_string();
        let class = vm
            .class_of(c.target)
            .map(|k| vm.world.names.str(vm.world.class(k).name).to_string())
            .unwrap_or_default();
        let who = vm.object_name(c.target);
        let now = match vm.level_info.map(|i| vm.get_prop(i, "TimeSeconds")) {
            Some(Ok(Value::Float(t))) => t,
            _ => 0.0,
        };
        eprintln!("anim {now:8.2} {class} {who}: {name} rate {rate} tween {tween} looping {looping}");
    }
    let (frames, seq_rate) = match vm.sequence_of(c.target, seq) {
        Some(info) => (info.frames.max(1.0), info.rate),
        None => (1.0, 0.0),
    };
    // The pose on screen is where a blend starts from, so it is kept before anything changes.
    if tween > 0.0 {
        vm.freeze_pose(c.target);
    }
    root_move(vm);
    vm.set_prop(c.target, "AnimSequence", Value::Name(seq))?;
    vm.set_prop(c.target, "AnimFrame", Value::Float(0.0))?;
    vm.set_prop(c.target, "AnimLast", Value::Float(1.0 - 1.0 / frames))?;
    vm.set_prop(c.target, "bAnimLoop", Value::Bool(looping))?;
    vm.set_prop(c.target, "bAnimFinished", Value::Bool(false))?;
    vm.set_prop(c.target, "AnimRate", Value::Float(rate * seq_rate / frames))?;
    set_tween(vm, c.target, tween)?;
    Ok(Value::Int(0))
}

/// Starts the blend into what an actor was just told to play: `TweenAlpha` runs from 0 to 1
/// over `TweenTime` seconds, and no time at all switches at once.
fn set_tween(vm: &mut Vm, actor: ObjectKey, time: f32) -> VmResult<()> {
    let blending = time > 0.0;
    vm.set_prop(actor, "TweenRate", Value::Float(if blending { 1.0 / time } else { 0.0 }))?;
    vm.set_prop(actor, "TweenAlpha", Value::Float(if blending { 0.0 } else { 1.0 }))?;
    Ok(())
}

/// Spawns an actor: defaults, placement, then UE1's spawn events.
pub fn spawn(vm: &mut Vm, spawner: ObjectKey, class: grim_object::ClassId, owner: Option<ObjectKey>, tag: Option<NameId>, loc: Option<Value>, rot: Option<Value>) -> VmResult<Option<ObjectKey>> {
    let a = vm.new_object(class, None);
    let level = (vm.level_info, vm.level);
    vm.set_prop(a, "Level", Value::Object(level.0))?;
    vm.set_prop(a, "XLevel", Value::Object(level.1))?;
    vm.set_prop(a, "Owner", Value::Object(owner))?;
    let tag = tag.unwrap_or(vm.world.class(class).name);
    vm.set_prop(a, "Tag", Value::Name(tag))?;
    let loc = match loc { Some(l) => l, None => vm.get_prop(spawner, "Location")? };
    let rot = match rot { Some(r) => r, None => vm.get_prop(spawner, "Rotation")? };
    vm.set_prop(a, "Location", loc)?;
    vm.set_prop(a, "Rotation", rot)?;
    vm.actors.push(a);
    // It starts in the zone it was put in. Hypothesis: nothing is told about that first one;
    // the scripts only hear about changes of zone after this.
    vm.update_zone(a, false)?;
    for event in ["Spawned", "PreBeginPlay", "BeginPlay", "PostBeginPlay", "SetInitialState"] {
        if vm.is_deleted(a) {
            return Ok(None);
        }
        vm.call_event(a, event, vec![])?;
    }
    Ok(if vm.is_deleted(a) { None } else { Some(a) })
}

/// How big an actor is drawn, in world axes: the box of the pose the drawing left, or the box
/// the mesh was stored with, times `DrawScale`. An actor with no mesh falls back to what it
/// collides with, which is all the engine knows about its size. Turning it to the world's axes
/// is what "how big it is drawn" means for an actor that has turned: an arm across the view
/// counts as width. Measured against the shadows of an original saved game, whose `DrawScale`
/// the engine had worked out from this: 0.691 for Harry and 0.715 for Snape.
fn render_extent(vm: &mut Vm, a: ObjectKey) -> VmResult<[f32; 3]> {
    let scale = match vm.get_prop(a, "DrawScale") {
        Ok(Value::Float(v)) if v > 0.0 => v,
        _ => 1.0,
    };
    let stored = match vm.get_prop(a, "Mesh")? {
        Value::Object(Some(mesh)) => vm.mesh_extent(mesh),
        _ => None,
    };
    if let Some(size) = vm.pose_extents.get(&a).copied().or(stored) {
        let [_, yaw, _] = crate::vm::ints3(&vm.get_prop(a, "Rotation")?)?;
        let turn = yaw as f32 * std::f32::consts::PI / 32768.0;
        let (sin, cos) = turn.sin_cos();
        let (x, y) = (size[0] * scale, size[1] * scale);
        return Ok([x * cos.abs() + y * sin.abs(), x * sin.abs() + y * cos.abs(), size[2] * scale]);
    }
    Ok(match vm.collision_extent(a)? {
        Some(half) => [half[0] * 2.0, half[1] * 2.0, half[2] * 2.0],
        None => [0.0; 3],
    })
}

/// Collision cylinder of an actor.
fn cylinder(vm: &mut Vm, a: ObjectKey) -> VmResult<([f32; 3], f32, f32)> {
    let loc = crate::vm::floats3(&vm.get_prop(a, "Location")?)?;
    let Value::Float(r) = vm.get_prop(a, "CollisionRadius")? else { return crate::vm::fail("CollisionRadius") };
    let Value::Float(h) = vm.get_prop(a, "CollisionHeight")? else { return crate::vm::fail("CollisionHeight") };
    Ok((loc, r, h))
}

/// A segment swept by a box, with the box it all fits in.
struct Reach {
    start: [f32; 3],
    end: [f32; 3],
    extent: [f32; 3],
    low: [f32; 3],
    high: [f32; 3],
}

impl Reach {
    fn of(start: [f32; 3], end: [f32; 3], extent: [f32; 3]) -> Self {
        let low = [0, 1, 2].map(|i| start[i].min(end[i]) - extent[i]);
        let high = [0, 1, 2].map(|i| start[i].max(end[i]) + extent[i]);
        Reach { start, end, extent, low, high }
    }
}

/// Where a trace meets an actor that collides, if it does. What is nowhere near the segment is
/// left out by where it stands and how big it is before its shape is worked out: the camera
/// traces a few times a tick, and a hub has over a thousand actors. A brush's box lies about
/// its own pivot, so a brush is always looked at properly. The shape turns with the actor only
/// when its `CollideType` says it does.
fn actor_hit(vm: &mut Vm, a: ObjectKey, reach: &Reach) -> VmResult<Option<(f32, [f32; 3])>> {
    if vm.is_deleted(a) || !matches!(vm.get_prop_id(a, vm.hot.collide_actors)?, Value::Bool(true)) {
        return Ok(None);
    }
    if !matches!(vm.get_prop_id(a, vm.hot.brush), Ok(Value::Object(Some(_)))) {
        let Some(at) = vm.vec3_id(a, vm.hot.location) else { return Ok(None) };
        let float = |v: VmResult<Value>| match v {
            Ok(Value::Float(f)) => f.max(0.0),
            _ => 0.0,
        };
        let radius = float(vm.get_prop_id(a, vm.hot.collision_radius));
        let across = radius.max(float(vm.get_prop_id(a, vm.hot.collision_width))) * std::f32::consts::SQRT_2;
        let half = [across, across, float(vm.get_prop_id(a, vm.hot.collision_height))];
        if (0..3).any(|k| at[k] + half[k] < reach.low[k] || at[k] - half[k] > reach.high[k]) {
            return Ok(None);
        }
    }
    let Some((centre, half)) = vm.collision_box(a)? else { return Ok(None) };
    let yaw = match vm.collide_type(a) {
        crate::level::CT_BOX | crate::level::CT_ORIENTED_OVAL => {
            vm.rot3_id(a, vm.hot.rotation).map_or(0.0, |r| r[1] as f32 / 32768.0 * std::f32::consts::PI)
        }
        _ => 0.0,
    };
    Ok(grim_world::box_check_turned(reach.start, reach.end, centre, half, reach.extent, yaw))
}

/// World and actor trace. Returns the hit actor (the LevelInfo when the world was hit).
fn trace(vm: &mut Vm, from: ObjectKey, start: [f32; 3], end: [f32; 3], extent: [f32; 3], actors: bool) -> VmResult<Option<(ObjectKey, [f32; 3], [f32; 3])>> {
    let mut best: Option<(f32, ObjectKey, [f32; 3], [f32; 3])> = None;
    if let Some(bsp) = &vm.bsp {
        if let Some(h) = bsp.box_check(start, end, extent) {
            if let Some(level) = vm.level_info {
                best = Some((h.time, level, h.location, h.normal));
            }
        }
    }
    // The world a trace always meets is the level and its movers: a door, a secret wall or
    // a Lumos block stops the camera's own traces (`BaseCam` asks with `bTraceActors` off)
    // as much as the level does. Other actors only when asked for.
    let reach = Reach::of(start, end, extent);
    for a in vm.trace_candidates() {
        if a == from {
            continue;
        }
        if !actors && !matches!(vm.get_prop_id(a, vm.hot.is_mover), Ok(Value::Bool(true))) {
            continue;
        }
        {
            if let Some((t, n)) = actor_hit(vm, a, &reach)? {
                if best.as_ref().is_none_or(|b| t < b.0) {
                    best = Some((t, a, grim_world::lerp(start, end, t), n));
                }
            }
        }
    }
    Ok(best.map(|(_, a, l, n)| (a, l, n)))
}

pub fn register(n: &mut Natives) {
    macro_rules! nat {
        ($path:literal, |$vm:ident, $c:ident| $body:expr) => {
            n.add($path, |$vm: &mut Vm, $c: &mut NativeCall| -> R {
                let _ = (&$vm, &$c);
                $body
            });
        };
    }

    nat!("Engine.Actor.IsSoftwareRendering", |vm, c| Ok(Value::Bool(false)));

    nat!("Engine.Actor.AllActors", |vm, c| {
        let base = class_arg(vm, c, 0)?;
        let tag = if c.has(2) { Some(c.name(2)?) } else { None };
        let mut rows = Vec::new();
        for a in vm.actors.clone() {
            if vm.is_deleted(a) {
                continue;
            }
            let ac = vm.class_of(a)?;
            if base.is_some_and(|b| !vm.world.is_child_of(ac, b)) {
                continue;
            }
            if let Some(t) = tag {
                if vm.get_prop(a, "Tag")? != Value::Name(t) {
                    continue;
                }
            }
            let mut row = c.args.clone();
            row[1] = Value::Object(Some(a));
            rows.push(row);
        }
        c.iterations = Some(rows);
        Ok(Value::Int(0))
    });

    // `RadiusActors(class, out Actor, Radius, optional Loc)`: what stands within a distance of a
    // point, the caller's own location by default. A radius of zero is no limit, as the calls
    // that leave it out show (`harry.AdjustAim` iterates every visible pawn).
    nat!("Engine.Actor.RadiusActors", |vm, c| {
        let base = class_arg(vm, c, 0)?;
        let radius = c.float(2)?;
        let from = match c.has(3) {
            true => c.vec(3)?,
            false => crate::vm::floats3(&vm.get_prop(c.target, "Location")?).unwrap_or([0.0; 3]),
        };
        let rows = actors_near(vm, c, base, radius, from, Visibility::Any)?;
        c.iterations = Some(rows);
        Ok(Value::Int(0))
    });
    // The same, but only what can be seen from there: the level's own geometry hides an actor,
    // and a hidden one is not iterated at all.
    nat!("Engine.Actor.VisibleActors", |vm, c| {
        let base = class_arg(vm, c, 0)?;
        let radius = if c.has(2) { c.float(2)? } else { 0.0 };
        let from = match c.has(3) {
            true => c.vec(3)?,
            false => crate::vm::floats3(&vm.get_prop(c.target, "Location")?).unwrap_or([0.0; 3]),
        };
        let rows = actors_near(vm, c, base, radius, from, Visibility::Seen { skip_hidden: true })?;
        c.iterations = Some(rows);
        Ok(Value::Int(0))
    });
    // `VisibleCollidingActors(class, out Actor, Radius, Loc, bIgnoreHidden)`: what an explosion
    // reaches (`Actor.HurtRadius`), so only actors that take part in collision count.
    nat!("Engine.Actor.VisibleCollidingActors", |vm, c| {
        let base = class_arg(vm, c, 0)?;
        let radius = if c.has(2) { c.float(2)? } else { 0.0 };
        let from = match c.has(3) {
            true => c.vec(3)?,
            false => crate::vm::floats3(&vm.get_prop(c.target, "Location")?).unwrap_or([0.0; 3]),
        };
        let skip_hidden = c.has(4) && c.bool(4)?;
        let rows = actors_near(vm, c, base, radius, from, Visibility::Colliding { skip_hidden })?;
        c.iterations = Some(rows);
        Ok(Value::Int(0))
    });

    nat!("Engine.Pawn.AddPawn", |vm, c| {
        let Some(info) = vm.level_info else { return Ok(Value::Int(0)) };
        // The list holds each pawn once. A saved game carries its own list in its properties and
        // its pawns begin play again when it is loaded, so `Pawn.PreBeginPlay` asks for pawns
        // that are already in it: adding one twice closes the list into a ring, and the game's
        // own walks over it (`Pawn.Destroyed` tells every pawn about a death) never end.
        let mut cur = match vm.get_prop(info, "PawnList")? { Value::Object(o) => o, _ => None };
        let mut guard = 0;
        while let Some(p) = cur {
            if p == c.target {
                return Ok(Value::Int(0));
            }
            cur = match vm.get_prop(p, "nextPawn")? { Value::Object(o) => o, _ => None };
            guard += 1;
            if guard > 100_000 {
                return crate::vm::fail("PawnList cycle");
            }
        }
        let head = vm.get_prop(info, "PawnList")?;
        vm.set_prop(c.target, "nextPawn", head)?;
        vm.set_prop(info, "PawnList", Value::Object(Some(c.target)))?;
        Ok(Value::Int(0))
    });

    nat!("Engine.Actor.SetCollision", |vm, c| {
        for (i, p) in ["bCollideActors", "bBlockActors", "bBlockPlayers"].into_iter().enumerate() {
            if c.has(i) {
                vm.set_prop(c.target, p, Value::Bool(c.bool(i)?))?;
            }
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.SetCollisionSize", |vm, c| {
        // The cylinder changes size around where the actor is; the actor does not move.
        // `harry.MountFinish` relies on it: it halves the collision and lowers `PrePivot` by
        // the new half height, which is what keeps the mesh (aligned on the bottom of the
        // cylinder) where it was drawn while the cylinder's bottom goes up. Moving the actor
        // down as well counted the change twice: Harry came off a ledge drawn above it.
        vm.set_prop(c.target, "CollisionRadius", Value::Float(c.float(0)?))?;
        vm.set_prop(c.target, "CollisionHeight", Value::Float(c.float(1)?))?;
        if c.has(2) {
            vm.set_prop(c.target, "CollisionWidth", Value::Float(c.float(2)?))?;
        }
        Ok(Value::Bool(true))
    });

    nat!("Engine.Actor.Move", |vm, c| {
        let d = c.vec(0)?;
        let (l, r, h) = cylinder(vm, c.target)?;
        let end = grim_world::add(l, d);
        let collides = matches!(vm.get_prop(c.target, "bCollideWorld")?, Value::Bool(true));
        let hit = if collides { vm.bsp.as_ref().and_then(|b| b.box_check(l, end, [r, r, h])) } else { None };
        match hit {
            None => {
                vm.set_prop(c.target, "Location", vector(end[0], end[1], end[2]))?;
                Ok(Value::Bool(true))
            }
            Some(hit) => {
                let stop = grim_world::lerp(l, end, (hit.time - 0.001).max(0.0));
                vm.set_prop(c.target, "Location", vector(stop[0], stop[1], stop[2]))?;
                let level = Value::Object(vm.level_info);
                let n = hit.normal;
                vm.call_event(c.target, "HitWall", vec![vector(n[0], n[1], n[2]), level])?;
                Ok(Value::Bool(false))
            }
        }
    });
    // Unreal refuses a move into solid geometry and says so, which is what the scripts lean on:
    // `SetLocation2` raises an actor a little at a time until one of the tries is taken.
    // `MoveSmooth` is `Move` that keeps going along a wall instead of stopping against it.
    nat!("Engine.Actor.MoveSmooth", |vm, c| {
        let d = c.vec(0)?;
        let (l, r, h) = cylinder(vm, c.target)?;
        let collides = matches!(vm.get_prop(c.target, "bCollideWorld")?, Value::Bool(true));
        let extent = [r, r, h];
        let step = |vm: &Vm, from: [f32; 3], delta: [f32; 3]| -> (bool, [f32; 3], [f32; 3]) {
            let end = grim_world::add(from, delta);
            let hit = if collides { vm.bsp.as_ref().and_then(|b| b.box_check(from, end, extent)) } else { None };
            match hit {
                None => (false, end, [0.0; 3]),
                Some(hit) => (true, grim_world::lerp(from, end, (hit.time - 0.001).max(0.0)), hit.normal),
            }
        };
        let (blocked, mut at, normal) = step(vm, l, d);
        if blocked {
            // What is left of the move, with the part going into the wall taken out of it.
            let left = grim_world::sub(grim_world::add(l, d), at);
            let into = grim_world::dot(left, normal);
            let slide = grim_world::sub(left, grim_world::scale(normal, into));
            at = step(vm, at, slide).1;
        }
        vm.set_prop(c.target, "Location", vector(at[0], at[1], at[2]))?;
        if blocked {
            let level = Value::Object(vm.level_info);
            vm.call_event(c.target, "HitWall", vec![vector(normal[0], normal[1], normal[2]), level])?;
        }
        Ok(Value::Bool(!blocked))
    });
    nat!("Engine.Actor.SetLocation", |vm, c| {
        let Some(at) = vm.find_spot(c.target, crate::vm::floats3(&c.args[0])?)? else {
            return Ok(Value::Bool(false));
        };
        vm.set_prop(c.target, "Location", vector(at[0], at[1], at[2]))?;
        vm.refresh_blocker(c.target);
        vm.update_zone(c.target, true)?;
        Ok(Value::Bool(true))
    });
    nat!("Engine.Actor.SetRotation", |vm, c| {
        let v = c.args[0].clone();
        vm.set_prop(c.target, "Rotation", v)?;
        Ok(Value::Bool(true))
    });

    nat!("Engine.Actor.SetTimer", |vm, c| {
        vm.set_prop(c.target, "TimerRate", Value::Float(c.float(0)?))?;
        vm.set_prop(c.target, "bTimerLoop", Value::Bool(c.bool(1)?))?;
        vm.set_prop(c.target, "TimerCounter", Value::Float(0.0))?;
        Ok(Value::Int(0))
    });

    nat!("Engine.Actor.Sleep", |vm, c| {
        vm.latent = Some(Latent::Sleep(c.float(0)?));
        Ok(Value::Int(0))
    });

    nat!("Engine.Actor.HasAnim", |vm, c| {
        let seq = c.name(0)?;
        Ok(Value::Bool(mesh_anims(vm, c.target)?.is_some_and(|m| m.sequences.contains(&seq))))
    });
    nat!("Engine.Actor.BonePos", |vm, c| {
        let bone = vm.world.names.str(c.name(0)?).to_string();
        let at = vm.bone_world(c.target, &bone).map(|(p, _)| p).unwrap_or_default();
        Ok(vector(at[0], at[1], at[2]))
    });
    nat!("Engine.Actor.BoneRot", |vm, c| {
        let bone = vm.world.names.str(c.name(0)?).to_string();
        let r = vm.bone_world(c.target, &bone).map(|(_, r)| r).unwrap_or_default();
        Ok(crate::vm::rotator(r[0], r[1], r[2]))
    });
    nat!("Engine.Actor.BoneName", |vm, c| {
        let index = c.int(0)?;
        let name = mesh_anims(vm, c.target)?.and_then(|m| m.bones.get(index.max(0) as usize).copied());
        Ok(Value::Name(name.unwrap_or_else(|| vm.world.names.intern("None"))))
    });
    nat!("Engine.Actor.BoneNumber", |vm, c| {
        let bone = c.name(0)?;
        let idx = mesh_anims(vm, c.target)?.and_then(|m| m.bones.iter().position(|&b| b == bone));
        Ok(Value::Int(idx.map(|i| i as i32).unwrap_or(-1)))
    });
    nat!("Engine.Actor.PlayAnim", |vm, c| set_anim(vm, c, false));
    nat!("Engine.Actor.LoopAnim", |vm, c| set_anim(vm, c, true));
    // --- Canvas: the game's own drawing -------------------------------------------------
    nat!("Engine.Canvas.DrawTile", |vm, c| {
        let texture = c.obj(0)?;
        let size = [c.float(1)?, c.float(2)?];
        let uv = [c.float(3)?, c.float(4)?, c.float(5)?, c.float(6)?];
        vm.canvas_draw_tile(texture, size, uv)?;
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.DrawTileClipped", |vm, c| {
        let texture = c.obj(0)?;
        let size = [c.float(1)?, c.float(2)?];
        let uv = [c.float(3)?, c.float(4)?, c.float(5)?, c.float(6)?];
        vm.canvas_draw_tile_clipped(texture, size, uv)?;
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.DrawText", |vm, c| {
        let text = c.str(0)?.to_string();
        let newline = !c.has(1) || c.bool(1)?;
        vm.canvas_draw_text(&text, newline)?;
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.DrawTextClipped", |vm, c| {
        let text = c.str(0)?.to_string();
        vm.canvas_draw_text_clipped(&text)?;
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.StrLen", |vm, c| {
        let text = c.str(0)?.to_string();
        let size = vm.canvas_text_size(&text);
        c.args[1] = Value::Float(size[0]);
        c.args[2] = Value::Float(size[1]);
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.TextSize", |vm, c| {
        let text = c.str(0)?.to_string();
        let size = vm.canvas_text_size(&text);
        c.args[1] = Value::Float(size[0]);
        c.args[2] = Value::Float(size[1]);
        Ok(Value::Int(0))
    });
    // `DrawActor(A, WireFrame, ClearZ)`: the actor is drawn over the finished world, from the
    // same point of view, so nothing standing in front of it can hide it. It is how the game
    // puts what Harry carries on the screen (`HProp.RenderHud`) and how a weapon shows itself
    // (`Inventory.RenderOverlays`). Wireframe is not drawn: no class of the game asks for it.
    nat!("Engine.Canvas.DrawActor", |vm, c| {
        if let Some(actor) = c.obj(0)? {
            vm.canvas_actors.push(actor);
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.DrawClippedActor", |vm, c| {
        if let Some(actor) = c.obj(0)? {
            vm.canvas_actors.push(actor);
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Canvas.DrawPortal", |vm, _c| {
        vm.warn("DrawPortal: portals are not drawn into the canvas yet");
        Ok(Value::Int(0))
    });

    nat!("Engine.Actor.FinishInterpolation", |vm, _c| {
        // The interpolation manager moves the actor and clears the flag when it is done.
        vm.latent = Some(Latent::Wait("bInterpolating", true));
        Ok(Value::Int(0))
    });

    nat!("Engine.LevelInfo.GetLocalURL", |vm, _c| {
        // The map the game is playing, in the form the scripts expect of a URL.
        let map = vm
            .level
            .map(|l| vm.world.package(l.package).name.clone())
            .unwrap_or_default();
        Ok(Value::Str(format!("{map}.unr")))
    });
    nat!("Engine.LevelInfo.GetAddressURL", |_vm, _c| Ok(Value::Str(String::new())));

    nat!("Engine.Actor.GetAnimGroup", |vm, c| {
        let seq = c.name(0)?;
        Ok(Value::Name(anim_group(vm, c.target, seq).unwrap_or(seq)))
    });
    nat!("Engine.Actor.IsAnimating", |vm, c| {
        let playing = matches!(vm.get_prop(c.target, "AnimRate")?, Value::Float(r) if r != 0.0)
            && !matches!(vm.get_prop(c.target, "bAnimFinished")?, Value::Bool(true));
        Ok(Value::Bool(playing))
    });
    nat!("Engine.Actor.FinishAnim", |vm, c| {
        // Blocks the state code until the sequence being played runs out. A loop is told to
        // stop at its end, or it never would: the original's saves have ten chocolate frogs
        // waiting in `holdstill`'s `FinishAnim` right after its `LoopAnim`, every one of them
        // with `bAnimLoop` back to false. Keeping the loop left the frog of a chest sitting
        // there for good.
        if matches!(vm.get_prop(c.target, "bAnimLoop")?, Value::Bool(true)) {
            vm.set_prop(c.target, "bAnimLoop", Value::Bool(false))?;
        }
        vm.latent = Some(Latent::Wait("bAnimFinished", false));
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.TweenAnim", |vm, c| {
        let seq = c.name(0)?;
        let time = c.float(1)?;
        // Tweening only blends into the sequence's first frame; it never plays on from there.
        if time > 0.0 {
            vm.freeze_pose(c.target);
        }
        vm.set_prop(c.target, "AnimSequence", Value::Name(seq))?;
        vm.set_prop(c.target, "AnimFrame", Value::Float(0.0))?;
        vm.set_prop(c.target, "bAnimLoop", Value::Bool(false))?;
        vm.set_prop(c.target, "bAnimFinished", Value::Bool(time <= 0.0))?;
        vm.set_prop(c.target, "AnimRate", Value::Float(0.0))?;
        set_tween(vm, c.target, time)?;
        Ok(Value::Int(0))
    });
    // An anim channel is an actor of its own that plays a sequence over part of its owner's
    // skeleton: the bone it is given and everything hanging from it, which is how a face moves
    // while the body keeps its own animation.
    nat!("Engine.Actor.CreateAnimChannel", |vm, c| {
        let Some(class) = class_arg(vm, c, 0)? else { return Ok(Value::Object(None)) };
        let kind = c.byte(1)?;
        let bone = c.name(2)?;
        let Some(channel) = spawn(vm, c.target, class, Some(c.target), None, None, None)? else {
            return Ok(Value::Object(None));
        };
        vm.add_channel(c.target, channel, kind, bone);
        Ok(Value::Object(Some(channel)))
    });

    nat!("Engine.Actor.SetPhysics", |vm, c| {
        vm.set_prop(c.target, "Physics", Value::Byte(c.byte(0)?))?;
        Ok(Value::Int(0))
    });
    // Latent movement: each of these leaves a `MoveOrder` behind and the tick advances it,
    // resuming the state code once the actor is there.
    nat!("Engine.Pawn.MoveTo", |vm, c| {
        let dest = c.vec(0)?;
        let speed = if c.has(1) { c.float(1)? } else { 1.0 };
        start_move(vm, c.target, MoveOrder { dest: Some(dest), speed, ..order() })
    });
    nat!("Engine.Pawn.MoveToward", |vm, c| {
        let Some(target) = c.obj(0)? else { return Ok(Value::Int(0)) };
        let speed = if c.has(1) { c.float(1)? } else { 1.0 };
        start_move(vm, c.target, MoveOrder { target: Some(target), speed, ..order() })
    });
    // Strafing moves the same way but keeps facing what it was given.
    nat!("Engine.Pawn.StrafeTo", |vm, c| {
        let dest = c.vec(0)?;
        let focus = c.vec(1)?;
        start_move(vm, c.target, MoveOrder { dest: Some(dest), focus: Some(focus), ..order() })
    });
    nat!("Engine.Pawn.StrafeFacing", |vm, c| {
        let dest = c.vec(0)?;
        start_move(vm, c.target, MoveOrder { dest: Some(dest), focus_actor: c.obj(1)?, ..order() })
    });
    nat!("Engine.Pawn.TurnTo", |vm, c| {
        let focus = c.vec(0)?;
        start_move(vm, c.target, MoveOrder { focus: Some(focus), ..order() })
    });
    nat!("Engine.Pawn.TurnToward", |vm, c| {
        let Some(target) = c.obj(0)? else { return Ok(Value::Int(0)) };
        start_move(vm, c.target, MoveOrder { focus_actor: Some(target), ..order() })
    });
    nat!("Engine.Pawn.WaitForLanding", |vm, c| {
        start_move(vm, c.target, MoveOrder { landing: true, timer: 4.0, ..order() })
    });
    // `StopWaiting`: ends the wait the pawn is in, so its state code carries on. A mover that
    // has finished moving calls it on every pawn that was waiting for it (`Mover.FinishNotify`),
    // which is how somebody riding a lift goes on with what it was doing.
    // `GetURLMap`: the map the game is running, as the URL spells it.
    nat!("Engine.Actor.GetURLMap", |vm, c| Ok(Value::Str(vm.level_name())));
    // `GetMapName(NameEnding, MapName, Dir)`: the map next to `MapName` among the ones installed,
    // going round from the last to the first. That is how the shortcut list walks them:
    // `ShortCutListLevels.Reset` asks for the first with an empty name and steps forward until
    // it is handed the first again, so stopping at the last left it asking for ever when F4
    // opened the window. `NameEnding` keeps only the names that end with it.
    nat!("Engine.Actor.GetMapName", |vm, c| {
        let (ending, name, dir) = (c.str(0)?.to_string(), c.str(1)?.to_string(), c.int(2)?);
        let dir_path = vm.world.lib.root().join("Maps");
        let mut maps: Vec<String> = match std::fs::read_dir(&dir_path) {
            Ok(entries) => entries
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.to_ascii_lowercase().ends_with(".unr"))
                .filter(|n| ending.is_empty() || n.to_ascii_lowercase().ends_with(&ending.to_ascii_lowercase()))
                .collect(),
            Err(_) => Vec::new(),
        };
        maps.sort_by_key(|n| n.to_ascii_lowercase());
        if maps.is_empty() {
            return Ok(Value::Str(String::new()));
        }
        let at = maps.iter().position(|m| m.eq_ignore_ascii_case(&name));
        let next = match (at, dir) {
            // No name to start from: the first one, whichever way it was asked for.
            (None, _) => 0,
            (Some(i), d) if d > 0 => (i + 1) % maps.len(),
            (Some(i), d) if d < 0 => (i + maps.len() - 1) % maps.len(),
            (Some(i), _) => i,
        };
        Ok(Value::Str(maps[next].clone()))
    });
    // `AutonomousPhysics(DeltaSeconds)`: move the actor by its own physics over that long, which
    // is the same step the tick takes. The cheat fly mode drives the player with it.
    nat!("Engine.Actor.AutonomousPhysics", |vm, c| {
        let dt = c.float(0)?;
        let target = c.target;
        vm.physics_step(target, dt)?;
        vm.late_physics_step(target, dt)?;
        Ok(Value::Int(0))
    });
    // `ClientTravel(URL, TravelType, bItems)`: the engine is told to go to another level. In a
    // game of one player that is the same thing the scripts do through `LevelInfo.NextURL`, which
    // is what the engine watches to change level.
    nat!("Engine.PlayerPawn.ClientTravel", |vm, c| {
        let url = c.str(0)?.to_string();
        let Some(info) = vm.level_info else { return Ok(Value::Int(0)) };
        vm.set_prop(info, "NextURL", Value::Str(url))?;
        vm.set_prop(info, "NextSwitchCountdown", Value::Float(0.0))?;
        Ok(Value::Int(0))
    });
    // `actorReachable(anActor)`: whether the pawn could walk straight there. A door asks it of
    // whoever triggers it before opening (`Mover.HandleTriggerDoor`, for anything within 500
    // units). The answer is the walk itself: the pawn's own cylinder is carried along the line
    // in steps of its radius, put down on whatever ground is under each one, and any step that
    // does not fit ends it. Nothing is pathfound — this native is the direct question.
    nat!("Engine.Pawn.actorReachable", |vm, c| {
        let Some(target) = c.obj(0)? else { return Ok(Value::Bool(false)) };
        let target = target;
        let me = c.target;
        Ok(Value::Bool(vm.can_walk_straight_to(me, target)?))
    });
    nat!("Engine.Pawn.StopWaiting", |vm, c| {
        vm.moves.remove(&c.target);
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.SetOwner", |vm, c| {
        let new = c.obj(0)?;
        let old = match vm.get_prop(c.target, "Owner")? { Value::Object(o) => o, _ => None };
        if let Some(o) = old {
            vm.call_event(o, "LostChild", vec![Value::Object(Some(c.target))])?;
        }
        vm.set_prop(c.target, "Owner", Value::Object(new))?;
        if let Some(n) = new {
            vm.call_event(n, "GainedChild", vec![Value::Object(Some(c.target))])?;
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.GetWorldCollisionBox", |vm, c| {
        let l = crate::vm::floats3(&vm.get_prop(c.target, "Location")?)?;
        // `bVisual` asks for the box the actor is *drawn* in rather than the one it collides
        // with. Hypothesis, from the flag's own name and from the only other native that
        // measures the same thing (`GetRenderExtent`): it is that extent, halved, around the
        // actor's location.
        let half = if c.has(0) && c.bool(0)? {
            render_extent(vm, c.target)?.map(|v| v / 2.0)
        } else {
            let Some(half) = vm.collision_extent(c.target)? else { return crate::vm::fail("no collision size") };
            half
        };
        Ok(Value::Struct(vec![
            vector(l[0] - half[0], l[1] - half[1], l[2] - half[2]),
            vector(l[0] + half[0], l[1] + half[1], l[2] + half[2]),
            Value::Byte(1),
        ]))
    });
    // How big the actor is drawn, which is what a shadow sizes itself by. The mesh's own box
    // times the scale it is drawn at; an actor without a mesh falls back to what it collides
    // with, which is all the engine knows about its size.
    nat!("Engine.Actor.GetRenderExtent", |vm, c| {
        let size = render_extent(vm, c.target)?;
        Ok(vector(size[0], size[1], size[2]))
    });
    nat!("Engine.Actor.GetSoundDuration", |vm, c| {
        let Some(k) = c.obj(0)? else { return Ok(Value::Float(0.0)) };
        let pkg = vm.world.package(k.package).clone();
        match load_export(&pkg, k.export).map_err(|e| VmError::script(e.to_string()))?.asset {
            Asset::Sound(s) => Ok(Value::Float(s.duration)),
            _ => crate::vm::fail("GetSoundDuration of a non-sound"),
        }
    });
    nat!("Engine.Actor.StopSound", |vm, c| {
        let sound = if c.has(0) { c.obj(0)? } else { None };
        let slot = if c.has(1) { Some(c.byte(1)?) } else { None };
        let fade = if c.has(2) { c.float(2)? } else { 0.0 };
        let id = match sound {
            Some(k) => sound_id(vm, k),
            None => None,
        };
        let owner = sound_key(c.target);
        if let Some(audio) = vm.audio.as_mut() {
            audio.stop(owner, slot, id, fade);
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.Trace", |vm, c| {
        let end = c.vec(2)?;
        let start = if c.has(3) { c.vec(3)? } else { crate::vm::floats3(&vm.get_prop(c.target, "Location")?)? };
        let actors = c.has(4) && c.bool(4)?;
        let extent = if c.has(5) { c.vec(5)? } else { [0.0; 3] };
        Ok(match trace(vm, c.target, start, end, extent, actors)? {
            Some((a, loc, norm)) => {
                c.args[0] = vector(loc[0], loc[1], loc[2]);
                c.args[1] = vector(norm[0], norm[1], norm[2]);
                Value::Object(Some(a))
            }
            None => {
                c.args[0] = vector(end[0], end[1], end[2]);
                c.args[1] = vector(0.0, 0.0, 0.0);
                Value::Object(None)
            }
        })
    });
    nat!("Engine.Actor.FastTrace", |vm, c| {
        let end = c.vec(0)?;
        let start = if c.has(1) { c.vec(1)? } else { crate::vm::floats3(&vm.get_prop(c.target, "Location")?)? };
        Ok(Value::Bool(vm.bsp.as_ref().and_then(|b| b.line_check(start, end)).is_none()))
    });
    nat!("Engine.Actor.TraceActors", |vm, c| {
        let base = class_arg(vm, c, 0)?;
        let end = c.vec(4)?;
        let start = if c.has(5) { c.vec(5)? } else { crate::vm::floats3(&vm.get_prop(c.target, "Location")?)? };
        let extent = if c.has(6) { c.vec(6)? } else { [0.0; 3] };
        let mut rows: Vec<(f32, Vec<Value>)> = Vec::new();
        // Hypothesis: this engine's `TraceActors` reports the level itself as its `LevelInfo`.
        // `BaseCam.CheckCollisionWithWorld` keeps the camera out of the walls with nothing but
        // this iterator, and tests what it hands back with `IsA('LevelInfo')`, which stock
        // Unreal could never satisfy.
        if let (Some(bsp), Some(level)) = (&vm.bsp, vm.level_info) {
            if let Some(hit) = bsp.box_check(start, end, extent) {
                let mut row = c.args.clone();
                row[1] = Value::Object(Some(level));
                row[2] = vector(hit.location[0], hit.location[1], hit.location[2]);
                row[3] = vector(hit.normal[0], hit.normal[1], hit.normal[2]);
                rows.push((hit.time, row));
            }
        }
        let reach = Reach::of(start, end, extent);
        for a in vm.trace_candidates() {
            if a == c.target || vm.is_deleted(a) {
                continue;
            }
            let Some((t, n)) = actor_hit(vm, a, &reach)? else { continue };
            let ac = vm.class_of(a)?;
            if base.is_some_and(|b| !vm.world.is_child_of(ac, b)) {
                continue;
            }
            {
                let p = grim_world::lerp(start, end, t);
                let mut row = c.args.clone();
                row[1] = Value::Object(Some(a));
                row[2] = vector(p[0], p[1], p[2]);
                row[3] = vector(n[0], n[1], n[2]);
                rows.push((t, row));
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        c.iterations = Some(rows.into_iter().map(|(_, r)| r).collect());
        Ok(Value::Int(0))
    });
    nat!("Engine.Pawn.LineOfSightTo", |vm, c| {
        let Some(other) = c.obj(0)? else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(line_of_sight(vm, c.target, other)?))
    });
    nat!("Engine.Pawn.CanSee", |vm, c| {
        let Some(other) = c.obj(0)? else { return Ok(Value::Bool(false)) };
        if matches!(vm.get_prop(other, "bHidden")?, Value::Bool(true)) {
            return Ok(Value::Bool(false));
        }
        // Peripheral vision: cosine of the angle between the view direction and the target.
        let (me, other_loc) = (crate::vm::floats3(&vm.get_prop(c.target, "Location")?)?, crate::vm::floats3(&vm.get_prop(other, "Location")?)?);
        let dir = grim_world::sub(other_loc, me);
        let len = grim_world::dot(dir, dir).sqrt();
        if len > 1e-3 {
            let rot = crate::vm::ints3(&vm.get_prop(c.target, "Rotation")?)?;
            let facing = super::core::axes(rot)[0];
            let cos = grim_world::dot(facing, grim_world::scale(dir, 1.0 / len));
            let periph = match vm.get_prop(c.target, "PeripheralVision")? { Value::Float(p) => p, _ => 0.0 };
            if cos < periph {
                return Ok(Value::Bool(false));
            }
        }
        Ok(Value::Bool(line_of_sight(vm, c.target, other)?))
    });
    // `PlayerCanSeeMe`: whether the player has me in view. What is out of sight is what the
    // engine's own classes clear away quietly (`Inventory.Pickup.Timer`, `Fragment.Dying.Timer`,
    // `Carcass.Dead.Timer`), so a wrong answer either leaves litter behind or makes it vanish
    // while being watched. The view is the player's own: the half angle of `FovAngle` around
    // where it looks, and nothing of the level in between.
    nat!("Engine.Actor.PlayerCanSeeMe", |vm, c| {
        let Some(player) = vm.player else { return Ok(Value::Bool(false)) };
        let me = crate::vm::floats3(&vm.get_prop(c.target, "Location")?)?;
        let eye = crate::vm::floats3(&vm.get_prop(player, "Location")?)?;
        let dir = grim_world::sub(me, eye);
        let len = grim_world::dot(dir, dir).sqrt();
        if len > 1e-3 {
            // A player looks where its `ViewRotation` points, which a cutscene camera moves on
            // its own; `Rotation` is only where the body faces.
            let rot = match vm.get_prop(player, "ViewRotation") {
                Ok(v) => crate::vm::ints3(&v)?,
                Err(_) => crate::vm::ints3(&vm.get_prop(player, "Rotation")?)?,
            };
            let facing = super::core::axes(rot)[0];
            let fov = match vm.get_prop(player, "FovAngle")? { Value::Float(f) if f > 0.0 => f, _ => 90.0 };
            let cos = grim_world::dot(facing, grim_world::scale(dir, 1.0 / len));
            if cos < (fov.to_radians() / 2.0).cos() {
                return Ok(Value::Bool(false));
            }
        }
        let target = c.target;
        Ok(Value::Bool(line_of_sight(vm, player, target)?))
    });
    nat!("Engine.Actor.SetBase", |vm, c| {
        let new = c.obj(0)?;
        vm.set_base(c.target, new)?;
        Ok(Value::Int(0))
    });
    nat!("Engine.Pawn.RemovePawn", |vm, c| {
        let Some(info) = vm.level_info else { return Ok(Value::Int(0)) };
        let next = vm.get_prop(c.target, "nextPawn")?;
        let mut prev: Option<ObjectKey> = None;
        let mut cur = match vm.get_prop(info, "PawnList")? { Value::Object(o) => o, _ => None };
        let mut guard = 0;
        while let Some(p) = cur {
            if p == c.target {
                match prev {
                    None => vm.set_prop(info, "PawnList", next)?,
                    Some(pp) => vm.set_prop(pp, "nextPawn", next)?,
                }
                break;
            }
            prev = Some(p);
            cur = match vm.get_prop(p, "nextPawn")? { Value::Object(o) => o, _ => None };
            guard += 1;
            if guard > 100_000 {
                return crate::vm::fail("PawnList cycle");
            }
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Decal.DetachDecal", |vm, c| {
        vm.decals.remove(&c.target);
        Ok(Value::Int(0))
    });
    // A decal sticks to the world: it is thrown from where the actor stands along a direction
    // until it meets a surface, and lies there. The scripts drop it if nothing was found, which
    // is why this hands back the level when it lands.
    nat!("Engine.Decal.AttachDecal", |vm, c| {
        let distance = c.float(0)?;
        // A decal is thrown against the way it faces: a shadow's rotation is set from the
        // direction the light comes from, so it points up under an overhead light and the
        // shadow falls the other way. The `DecalDir` argument is not that direction either:
        // the scripts pass `vect(1,0,0)` with it for a shadow that is not oriented, which would
        // throw it sideways.
        let [pitch, yaw, _] = crate::vm::ints3(&vm.get_prop(c.target, "Rotation")?)?;
        let (p, y) = (pitch as f32 * std::f32::consts::PI / 32768.0, yaw as f32 * std::f32::consts::PI / 32768.0);
        let dir = [-p.cos() * y.cos(), -p.cos() * y.sin(), -p.sin()];
        let length = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
        vm.decals.remove(&c.target);
        if length <= 0.0 || distance <= 0.0 {
            return Ok(Value::Object(None));
        }
        let mut from = crate::vm::floats3(&vm.get_prop(c.target, "Location")?)?;
        // What casts the shadow may be drawn below where it really stands: walking up a flight
        // of stairs an actor is let down towards the slope the steps stand for. Thrown from
        // there the shadow starts inside the step and catches its face, standing on end; it is
        // thrown from where the actor actually is instead.
        if let Value::Object(Some(owner)) = vm.get_prop(c.target, "Owner")? {
            if let Some(&lag) = vm.stair_lag.get(&owner) {
                from[2] += lag;
            }
        }
        let step = [dir[0] / length, dir[1] / length, dir[2] / length];
        // A shadow lies on the ground. A face the throw only grazes is no ground, so the throw
        // carries on past it: on a staircase the tread is what it is looking for, not the riser.
        // Whatever throws the decal may have its middle inside something it is standing against
        // — the game lets a pawn lean a little into a wall. A throw that starts in solid space
        // is stopped where it began, on the face it started in, so it backs out first.
        if let Some(bsp) = vm.bsp.as_ref() {
            for _ in 0..8 {
                if !bsp.box_solid(from, [0.0; 3]) {
                    break;
                }
                from = [0, 1, 2].map(|i| from[i] - dir[i] / length * 8.0);
            }
        }
        let mut start = from;
        let mut left = distance;
        for _ in 0..6 {
            let to = [0, 1, 2].map(|i| start[i] + step[i] * left);
            // The level's own tree and the movers': a shadow on a staircase that slides is on
            // the staircase, and that geometry belongs to the mover, not to the level.
            let mut hit = vm.bsp.as_ref().and_then(|b| b.line_check(start, to)).map(|h| (h.time, h.location, h.normal));
            for blocker in &vm.blockers {
                if !matches!(blocker.shape, crate::level::Shape::Brush { .. }) {
                    continue;
                }
                if let Some((time, normal)) = blocker.shape.hit(start, to, [0.0; 3]) {
                    if hit.as_ref().is_none_or(|h| time < h.0) {
                        hit = Some((time, grim_world::lerp(start, to, time), normal));
                    }
                }
            }
            let Some(hit) = hit.map(|(time, location, normal)| grim_world::Hit { time, location, normal, node: -1, surf: -1 }) else {
                return Ok(Value::Object(None));
            };
            let facing = -(hit.normal[0] * step[0] + hit.normal[1] * step[1] + hit.normal[2] * step[2]);
            if facing > 0.3 {
                vm.decals.insert(c.target, crate::level::Decal { at: hit.location, normal: hit.normal });
                return Ok(Value::Object(vm.level_info));
            }
            let gone = (0..3).map(|i| (hit.location[i] - start[i]).powi(2)).sum::<f32>().sqrt();
            left -= gone + 1.0;
            if left <= 0.0 {
                break;
            }
            // Out of the face it grazed and on along the throw: what stopped it is a wall the
            // thrower is leaning into, and the ground it is looking for is to the side of it.
            start = [0, 1, 2].map(|i| hit.location[i] + hit.normal[i] * 2.0 + step[i]);
        }
        Ok(Value::Object(None))
    });
    // The same native is declared on `Actor`, `Console` and `PlayerPawn`; which one a call
    // reaches depends on the class it is made on, so all three run the same command.
    nat!("Engine.PlayerPawn.ConsoleCommand", |vm, c| Ok(Value::Str(console_command(vm, c.str(0)?.to_string()).unwrap_or_default())));
    nat!("Engine.Actor.ConsoleCommand", |vm, c| Ok(Value::Str(console_command(vm, c.str(0)?.to_string()).unwrap_or_default())));
    // `Console.ConsoleCommand` answers whether the command was understood.
    nat!("Engine.Console.ConsoleCommand", |vm, c| Ok(Value::Bool(console_command(vm, c.str(0)?.to_string()).is_some())));
    nat!("Engine.Actor.PlayMusic", |vm, c| {
        let (song, fade) = (c.str(0)?.to_string(), if c.has(1) { c.float(1)? } else { 0.0 });
        let handle = match vm.audio.as_mut() {
            Some(audio) => audio.play_music(&song, fade),
            None => 0,
        };
        Ok(Value::Int(handle as i32))
    });
    nat!("Engine.Actor.StopMusic", |vm, c| {
        let (handle, fade) = (c.int(0)?, if c.has(1) { c.float(1)? } else { 0.0 });
        if let Some(audio) = vm.audio.as_mut() {
            audio.stop_music(handle.max(0) as u32, fade);
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.StopAllMusic", |vm, c| {
        let fade = if c.has(0) { c.float(0)? } else { 0.0 };
        if let Some(audio) = vm.audio.as_mut() {
            audio.stop_all_music(fade);
        }
        Ok(Value::Int(0))
    });
    nat!("Engine.Actor.PlaySound", |vm, c| {
        let target = c.target;
        play_sound(vm, target, c, target)
    });
    // `TraceTexture(End, Start, out Flags)`: what the world is made of where the line lands.
    nat!("Engine.Actor.TraceTexture", |vm, c| {
        let (end, start) = (c.vec(0)?, c.vec(1)?);
        let Some(hit) = vm.bsp.as_ref().and_then(|b| b.line_check(start, end)) else { return Ok(Value::Object(None)) };
        let Some(&(texture, flags)) = usize::try_from(hit.surf).ok().and_then(|i| vm.surfaces.get(i)) else {
            return Ok(Value::Object(None));
        };
        c.args[2] = Value::Int(flags as i32);
        Ok(Value::Object(texture))
    });
    nat!("Engine.Actor.GetCurrentKeyState", |vm, c| {
        let key = c.byte(0)?;
        let held = vm.held_keys.clone();
        Ok(Value::Bool(held.iter().any(|k| vm.input_key_index(k) == Some(key))))
    });
    // `ModifySound(parameter, Value, Sound, Slot)`: changes one thing about a sound the actor is
    // already playing and answers whether it was. `FlyingCarHarry.UpdateBroomSound` reads that
    // answer to tell "keep the engine going at this volume" from "start it".
    nat!("Engine.Actor.ModifySound", |vm, c| {
        let (param, value) = (c.byte(0)?, c.float(1)?);
        let sound = if c.has(2) { c.obj(2)? } else { None };
        let slot = if c.has(3) { Some(c.byte(3)?) } else { None };
        let param = match param {
            0 => grim_audio::SoundParam::Volume,
            // A radius is a byte in Unreal units of 25 world units, as `PlaySound` reads it.
            1 => grim_audio::SoundParam::Radius,
            2 => grim_audio::SoundParam::Pitch,
            other => return crate::vm::fail(format!("ModifySound: unknown sound parameter {other}")),
        };
        let value = match param {
            grim_audio::SoundParam::Radius => value * 25.0,
            _ => value,
        };
        let id = match sound {
            Some(sound) => match sound_id(vm, sound) {
                Some(id) => Some(id),
                None => return Ok(Value::Bool(false)),
            },
            None => None,
        };
        let owner = sound_key(c.target);
        let Some(audio) = vm.audio.as_mut() else { return Ok(Value::Bool(false)) };
        Ok(Value::Bool(audio.modify(owner, slot, id, param, value)))
    });
    // `MakeNoise` has nothing to tell in this game: no class overrides `Pawn.HearNoise`, and
    // nothing reads the noise Unreal records on a pawn (`noise1time`, `noise2time`). What the
    // game does instead is call `HPawn.PawnHearHarryNoise` itself, right beside `MakeNoise`
    // (`harry.PlayFootStep`, `harry.PlayLandedSound`).
    nat!("Engine.Actor.MakeNoise", |vm, c| Ok(Value::Int(0)));
    // The same sound, but attached to whoever owns the actor: a weapon's sound follows the
    // pawn holding it.
    nat!("Engine.Actor.PlayOwnedSound", |vm, c| {
        let target = c.target;
        let owner = match vm.get_prop(target, "Owner")? {
            Value::Object(Some(o)) => o,
            _ => target,
        };
        play_sound(vm, target, c, owner)
    });

    nat!("Engine.Actor.Spawn", |vm, c| {
        let Some(class) = class_arg(vm, c, 0)? else { return Ok(Value::Object(None)) };
        let owner = if c.has(1) { c.obj(1)? } else { None };
        let tag = if c.has(2) { Some(c.name(2)?) } else { None };
        let loc = if c.has(3) { Some(c.args[3].clone()) } else { None };
        let rot = if c.has(4) { Some(c.args[4].clone()) } else { None };
        Ok(Value::Object(spawn(vm, c.target, class, owner, tag, loc, rot)?))
    });
    nat!("Engine.Actor.Destroy", |vm, c| {
        if vm.is_deleted(c.target) {
            return Ok(Value::Bool(true));
        }
        vm.call_event(c.target, "Destroyed", vec![])?;
        vm.set_prop(c.target, "bDeleteMe", Value::Bool(true))?;
        vm.instance(c.target)?.deleted = true;
        Ok(Value::Bool(true))
    });

    nat!("Core.Object.DynamicLoadObject", |vm, c| {
        let name = c.str(0)?.to_string();
        let may_fail = c.has(2) && c.bool(2)?;
        let want = class_arg(vm, c, 1)?;
        let Some(pkg_name) = name.split('.').next() else { return Ok(Value::Object(None)) };
        // A package may hold several objects of the same name: a fire texture and the palette
        // it looks its heat up in are both `HP2_Menu.Icons.WandSpark`. The class asked for is
        // what tells them apart, so it decides which one is picked rather than only checking
        // the one that came first.
        let wanted_class = want.map(|w| vm.world.names.str(vm.world.class(w).name).to_string());
        let found = match vm.world.lib.load(pkg_name) {
            Ok(pkg) => {
                let last = name.rsplit('.').next().unwrap_or(&name).to_string();
                let named: Vec<u32> = (0..pkg.exports.len() as u32)
                    .filter(|&i| pkg.path_name(ObjectRef::Export(i)).eq_ignore_ascii_case(&name))
                    .chain(
                        (0..pkg.exports.len() as u32)
                            .filter(|&i| pkg.object_name(ObjectRef::Export(i)).eq_ignore_ascii_case(&last)),
                    )
                    .collect();
                let fits = |i: &&u32| match &wanted_class {
                    // The class a package records is the leaf name of the object's class.
                    Some(w) => {
                        let key = ObjectKey { package: vm.world.pkg_id(&pkg), export: **i };
                        w == "Class" && pkg.class_name(ObjectRef::Export(**i)) == "Class"
                            || vm.class_of(key).is_ok_and(|c| want.is_some_and(|w| vm.world.is_child_of(c, w)))
                    }
                    None => true,
                };
                named.iter().find(fits).or_else(|| named.first()).map(|&i| (pkg, i))
            }
            Err(_) => None,
        };
        let Some((pkg, i)) = found else {
            if !may_fail {
                vm.warn(format!("DynamicLoadObject: {name} not found"));
            }
            return Ok(Value::Object(None));
        };
        let key = ObjectKey { package: vm.world.pkg_id(&pkg), export: i };
        if let Some(want) = want {
            let is_class_request = vm.world.names.str(vm.world.class(want).name) == "Class";
            let ok = if is_class_request {
                pkg.class_name(ObjectRef::Export(i)) == "Class"
            } else {
                let cls = vm.class_of(key)?;
                vm.world.is_child_of(cls, want)
            };
            if !ok {
                vm.warn("DynamicLoadObject: object of the wrong class");
                return Ok(Value::Object(None));
            }
        }
        Ok(Value::Object(Some(key)))
    });
}

/// Sight check: the eyes of `from` against the target and the top of its cylinder.
fn line_of_sight(vm: &mut Vm, from: ObjectKey, other: ObjectKey) -> VmResult<bool> {
    let mut eye = crate::vm::floats3(&vm.get_prop(from, "Location")?)?;
    if let Value::Float(h) = vm.get_prop(from, "BaseEyeHeight")? {
        eye[2] += h;
    }
    let (target, _, th) = cylinder(vm, other)?;
    let clear = |vm: &Vm, to: [f32; 3]| vm.bsp.as_ref().and_then(|b| b.line_check(eye, to)).is_none();
    Ok(clear(vm, target) || clear(vm, [target[0], target[1], target[2] + th]))
}


/// Hands a sound of a package to the audio backend, decoding it the first time. The key the
/// backend keeps is the object itself, so the same sound is never decoded twice.
pub fn sound_id(vm: &mut Vm, key: ObjectKey) -> Option<grim_audio::SoundId> {
    if let Some(&id) = vm.sounds.get(&key) {
        return Some(id);
    }
    let pkg = vm.world.package(key.package).clone();
    let Asset::Sound(sound) = load_export(&pkg, key.export).ok()?.asset else { return None };
    let pcm = grim_assets::sound::decode(&pkg, &sound).ok()?;
    let audio = vm.audio.as_mut()?;
    let id = audio.add_sound(sound_key(key), grim_audio::Samples { rate: pcm.rate, channels: pcm.channels, data: pcm.samples });
    vm.sounds.insert(key, id);
    Some(id)
}

/// An object as one number, which is all the backend needs to tell sounds and owners apart.
pub fn sound_key(key: ObjectKey) -> u64 {
    ((key.package.0 as u64) << 32) | key.export as u64
}

/// Plays a sound the way `Actor.PlaySound` describes it: the actor's own `SoundRadius`,
/// `SoundVolume` and `SoundPitch` unless the call overrides them. Unreal's byte pitch has 64
/// for the sound's own speed, and its radius is in world units.
fn play_sound(vm: &mut Vm, actor: ObjectKey, c: &mut NativeCall, owner: ObjectKey) -> VmResult<Value> {
    let Some(sound) = c.obj(0)? else { return Ok(Value::Int(0)) };
    if vm.audio.is_none() {
        return Ok(Value::Int(0));
    }
    let Some(id) = sound_id(vm, sound) else { return Ok(Value::Int(0)) };
    let byte_scale = |v: Value, scale: f32| match v {
        Value::Byte(b) => b as f32 / scale,
        Value::Float(f) => f,
        Value::Int(i) => i as f32 / scale,
        _ => 1.0,
    };
    let slot = if c.has(1) { c.byte(1)? } else { grim_audio::SLOT_NONE };
    // A sound played without saying how loud takes the actor's `TransientSoundVolume`, which is
    // what Unreal keeps for the sounds an actor plays; `SoundVolume` is its ambient sound's.
    // Read from the ambient one, the same sound came out at whatever volume the actor's
    // ambient hum was set to, loud from one and quiet from the next.
    let volume = if c.has(2) { c.float(2)? } else { byte_scale(vm.get_prop(actor, "TransientSoundVolume")?, 1.0) };
    let no_override = c.has(3) && c.bool(3)?;
    let radius = match (c.has(4), vm.get_prop(actor, "TransientSoundRadius")?) {
        (true, _) => c.float(4)?,
        (false, Value::Float(r)) if r > 0.0 => r,
        // Hypothesis: with no transient radius of its own an actor's sound reaches as far as
        // its `SoundRadius` says, a byte in units of 25 world units.
        _ => byte_scale(vm.get_prop(actor, "SoundRadius")?, 1.0) * 25.0,
    };
    // `TransientSoundPitch` is 64 for a sound's own speed, as the byte pitches are.
    let pitch = if c.has(5) {
        c.float(5)?
    } else {
        match vm.get_prop(actor, "TransientSoundPitch")? {
            Value::Float(p) => p / 64.0,
            v => byte_scale(v, 64.0),
        }
    };
    let flat = c.has(6) && c.bool(6)?;
    let looping = c.has(7) && c.bool(7)?;
    let at = crate::vm::floats3(&vm.get_prop(actor, "Location")?).unwrap_or([0.0; 3]);
    // Speech is turned up and down on its own in the game's settings; the dialog lives in its
    // own package, which is what tells it apart from an effect.
    let kind = if vm.world.package(sound.package).name.eq_ignore_ascii_case("AllDialog") {
        grim_audio::Kind::Speech
    } else {
        grim_audio::Kind::Effect
    };
    let play = grim_audio::Play {
        sound: id,
        owner: sound_key(owner),
        slot,
        volume: volume.clamp(0.0, 4.0),
        radius: if flat { 0.0 } else { radius },
        pitch: if pitch > 0.0 { pitch } else { 1.0 },
        looping,
        at,
        no_override,
        kind,
    };
    if let Some(audio) = vm.audio.as_mut() {
        audio.play(play);
    }
    Ok(Value::Int(0))
}

/// A property value as UnrealScript writes it: what `GetPropertyText` hands back and
/// `SetPropertyText` reads. Only the shapes the game's own menus pass around are written out;
/// a struct keeps its field names, as the text form does.
fn property_text(vm: &Vm, v: &Value) -> String {
    match v {
        Value::Int(i) => i.to_string(),
        Value::Byte(b) => b.to_string(),
        Value::Bool(b) => (if *b { "True" } else { "False" }).to_string(),
        Value::Float(f) => format!("{f:.6}"),
        Value::Str(s) => s.clone(),
        Value::Name(n) => vm.world.names.str(*n).to_string(),
        Value::Object(None) => "None".to_string(),
        Value::Object(Some(k)) => vm.world.path(*k),
        Value::Array(a) => a.iter().map(|x| property_text(vm, x)).collect::<Vec<_>>().join(","),
        Value::Struct(f) => format!("({})", f.iter().map(|x| property_text(vm, x)).collect::<Vec<_>>().join(",")),
    }
}

/// Text drawn into a texture rather than onto the screen: the house point scores of the entrance
/// hall are four `ScriptedTexture`s their actor writes into (`CeremonySTextures.RenderTexture`).
pub fn register_scripted_texture(n: &mut Natives) {
    n.add("Engine.ScriptedTexture.TextSize", |vm: &mut Vm, c: &mut NativeCall| -> R {
        let text = c.str(0)?.to_string();
        let font = c.obj(3)?;
        let size = match font.and_then(|f| vm.font_metrics(f)) {
            Some(m) => m.size(&text),
            None => [0.0; 2],
        };
        c.args[1] = Value::Float(size[0]);
        c.args[2] = Value::Float(size[1]);
        Ok(Value::Int(0))
    });
    n.add("Engine.ScriptedTexture.DrawColoredText", |vm: &mut Vm, c: &mut NativeCall| -> R {
        let (x, y) = (c.float(0)?, c.float(1)?);
        let text = c.str(2)?.to_string();
        let Some(font) = c.obj(3)? else { return Ok(Value::Int(0)) };
        let colour = match &c.args[4] {
            Value::Struct(f) => {
                let byte = |i: usize| match f.get(i) {
                    Some(Value::Byte(b)) => *b,
                    _ => 255,
                };
                [byte(0), byte(1), byte(2), byte(3)]
            }
            _ => [255; 4],
        };
        let target = c.target;
        vm.scripted_draw_text(target, [x, y], &text, font, colour);
        Ok(Value::Int(0))
    });
}

pub fn register_config(n: &mut Natives) {
    // `GetPropertyText(PropName)` / `SetPropertyText(PropName, PropValue)`: an object's property
    // by name, as text. The game's own window code builds controls this way
    // (`UWindowBase.BuildObjectWithProperties`).
    n.add("Core.Object.GetPropertyText", |vm: &mut Vm, c: &mut NativeCall| -> R {
        let name = c.str(0)?.to_string();
        let target = c.target;
        match vm.get_prop(target, &name) {
            Ok(v) => Ok(Value::Str(property_text(vm, &v))),
            Err(_) => Ok(Value::Str(String::new())),
        }
    });
    n.add("Core.Object.SetPropertyText", |vm: &mut Vm, c: &mut NativeCall| -> R {
        let (name, text) = (c.str(0)?.to_string(), c.str(1)?.to_string());
        let target = c.target;
        let class = vm.class_of(target)?;
        let id = vm.world.names.intern(&name);
        let Some(ty) = vm.world.class(class).prop(id).map(|p| p.ty.clone()) else {
            return Ok(Value::Int(0));
        };
        if let Some(v) = vm.import_text(&ty, &text) {
            vm.set_prop(target, &name, v)?;
        }
        Ok(Value::Int(0))
    });
    n.add("Core.Object.Localize", |vm: &mut Vm, c: &mut NativeCall| -> R {
        let (section, key, package) = (c.str(0)?.to_string(), c.str(1)?.to_string(), c.str(2)?.to_string());
        Ok(Value::Str(vm.config.localize(&section, &key, &package).unwrap_or_else(|| {
            // Scripts probe for keys (e.g. a cutscene thread asks for `thread_0`..`thread_19`
            // until one is missing), so misses are expected. The section says which are which.
            vm.warn(format!("Localize: missing key in [{section}] of {package}"));
            format!("<?{}?{package}.{section}.{key}?>", vm.config.language)
        })))
    });
    n.add("Core.Object.GetLanguage", |vm: &mut Vm, _c: &mut NativeCall| -> R { Ok(Value::Str(vm.config.language.clone())) });
}

/// A latent move with nothing asked of it yet.
pub(crate) fn order() -> MoveOrder {
    MoveOrder { dest: None, target: None, focus: None, focus_actor: None, speed: 1.0, timer: 5.0, landing: false, nearest: f32::INFINITY }
}

/// Hands a move to the engine and leaves the state code waiting for it.
fn start_move(vm: &mut Vm, a: ObjectKey, o: MoveOrder) -> R {
    let o = timed(vm, a, o)?;
    vm.start_move(a, o);
    vm.latent = Some(Latent::Move);
    Ok(Value::Int(0))
}

/// A move with the time it is given before it gives up: twice what it would need at its
/// speed, so one that is blocked does not leave the state code waiting for ever.
pub(crate) fn timed(vm: &mut Vm, a: ObjectKey, mut o: MoveOrder) -> VmResult<MoveOrder> {
    let dest = match (o.dest, o.target) {
        (Some(d), _) => Some(d),
        (None, Some(t)) => crate::vm::floats3(&vm.get_prop(t, "Location")?).ok(),
        _ => None,
    };
    if let Some(d) = dest {
        let from = crate::vm::floats3(&vm.get_prop(a, "Location")?)?;
        let to = grim_world::sub(d, from);
        let distance = grim_world::dot(to, to).sqrt();
        let speed = match vm.get_prop(a, "GroundSpeed")? {
            Value::Float(v) if v > 0.0 => v * o.speed.clamp(0.01, 1.0),
            _ => 320.0,
        };
        o.timer = 2.0 * distance / speed + 1.0;
    }
    Ok(o)
}

/// Runs a command as a call to an `exec` function of the same name, which is how Unreal's
/// console reaches the game: the console itself is asked first, then the player, its HUD and
/// the game info. The words after the name become the arguments, read as the parameters'
/// types ask; the last string parameter takes the rest of the line.
fn exec_function(vm: &mut Vm, command: &str) -> bool {
    let command = command.trim();
    let (verb, mut rest) = command.split_once(char::is_whitespace).unwrap_or((command, ""));
    if verb.is_empty() {
        return false;
    }
    let name = vm.world.names.intern(verb);
    let player = vm.player;
    let hud = player.and_then(|p| match vm.get_prop(p, "myHUD") {
        Ok(Value::Object(h)) => h,
        _ => None,
    });
    let game = vm.level_info.and_then(|i| match vm.get_prop(i, "Game") {
        Ok(Value::Object(g)) => g,
        _ => None,
    });
    let targets = [vm.console(), player, hud, game];
    for target in targets.into_iter().flatten() {
        let Ok(Some(f)) = vm.find_function(target, name, false) else { continue };
        let def = vm.world.function(f).clone();
        if def.flags & grim_object::func::EXEC == 0 {
            continue;
        }
        let mut args = Vec::new();
        for (n, &i) in def.params.iter().enumerate() {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            let last = n + 1 == def.params.len();
            let word = match (&def.locals[i].ty, last) {
                (grim_object::PropType::Str, true) => std::mem::take(&mut rest),
                _ => {
                    let (w, r) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                    rest = r;
                    w
                }
            };
            args.push(match &def.locals[i].ty {
                grim_object::PropType::Int => Value::Int(word.parse().unwrap_or(0)),
                grim_object::PropType::Float => Value::Float(word.parse().unwrap_or(0.0)),
                grim_object::PropType::Byte(_) => Value::Byte(word.parse().unwrap_or(0)),
                grim_object::PropType::Bool => Value::Bool(matches!(word.to_ascii_lowercase().as_str(), "1" | "true")),
                grim_object::PropType::Name => Value::Name(vm.world.names.intern(word)),
                grim_object::PropType::Str => Value::Str(word.to_string()),
                _ => Value::Object(None),
            });
        }
        let verb_name = vm.world.names.str(def.name).to_string();
        if let Err(e) = vm.call_event(target, &verb_name, args) {
            vm.warn(format!("ConsoleCommand {command}: {e}"));
        }
        return true;
    }
    false
}

/// The class a console command names, by its own name or with its package in front
/// (`HGame.HChar`), and the property of it that the command names.
fn class_property(vm: &mut Vm, class: &str, property: &str) -> Option<(grim_object::ClassId, grim_object::PropDef)> {
    let short = class.rsplit('.').next().unwrap_or(class);
    let id = vm.class_named(short)?;
    let prop = vm.world.class(id).props.iter().find(|p| vm.world.names.str(p.name).eq_ignore_ascii_case(property)).cloned()?;
    Some((id, prop))
}

/// `set <class> <property> <value>`: the value, read as the property's text form, goes to
/// every object of that class or one below it, and to those classes' defaults, so what is
/// spawned later has it too. `set harry drawscale 2`, `set statusitemjellybeans ncount 500`.
fn set_command(vm: &mut Vm, command: &str) -> Option<String> {
    let mut words = command.split_whitespace().skip(1);
    let (class, property) = (words.next()?, words.next()?);
    let text: Vec<&str> = words.collect();
    let (id, prop) = class_property(vm, class, property)?;
    let value = vm.import_text(&prop.ty, &text.join(" "))?;
    let slot = prop.slot as usize;
    for c in 0..vm.world.classes.len() {
        let c = grim_object::ClassId(c as u32);
        if vm.world.is_child_of(c, id) {
            if let Some(v) = vm.world.classes[c.0 as usize].defaults.get_mut(slot) {
                *v = value.clone();
            }
        }
    }
    let keys: Vec<ObjectKey> = vm.heap.keys().copied().collect();
    for k in keys {
        if vm.class_of(k).is_ok_and(|c| vm.world.is_child_of(c, id)) {
            if let Ok(inst) = vm.instance(k) {
                if let Some(v) = inst.values.get_mut(slot) {
                    *v = value.clone();
                }
            }
        }
    }
    Some(String::new())
}

/// `get <class> <property>`: the class's default value, in its text form, which is what the
/// console prints back.
fn get_command(vm: &mut Vm, command: &str) -> Option<String> {
    let mut words = command.split_whitespace().skip(1);
    let (class, property) = (words.next()?, words.next()?);
    let (id, prop) = class_property(vm, class, property)?;
    let value = vm.world.class(id).defaults.get(prop.slot as usize).cloned()?;
    let text = property_text(vm, &value);
    vm.log.push(text.clone());
    Some(text)
}

/// A console command, run for whichever class it was called on. `None` means the engine does not
/// know the command yet; a command that returns nothing answers with an empty string.
fn console_command(vm: &mut Vm, command: String) -> Option<String> {
    vm.log.push(format!("ConsoleCommand: {command}"));
    // `get`/`set ini:<Section>.<Key> <Property>` is how the game's own options menu reads
    // and writes its settings: the ini entry names a class, and the property is read from
    // that class's own section. Everything else is still only logged.
    // `SETVOLUMES MUSIC=x SOUND=y` is what the options menu's sliders change as they move;
    // the `set ini:` that follows is what makes it stick.
    if let Some(rest) = command.strip_prefix("SETVOLUMES").or_else(|| command.strip_prefix("setvolumes")) {
        let device = vm.config.ini.get("Engine.Engine", "AudioDevice").unwrap_or_default().to_string();
        for pair in rest.split_whitespace() {
            let Some((what, value)) = pair.split_once('=') else { continue };
            let key = match what.to_ascii_uppercase().as_str() {
                "MUSIC" => "MusicVolume",
                "SOUND" | "EFFECTS" => "SoundVolume",
                _ => continue,
            };
            if !device.is_empty() {
                vm.config.change(device.as_str(), key, value);
            }
        }
        vm.apply_audio_settings();
        return Some(String::new());
    }
    let mut words = command.split_whitespace();
    let (verb, target) = (words.next().unwrap_or_default().to_ascii_lowercase(), words.next().unwrap_or_default().to_string());
    let property = words.next().unwrap_or_default().to_string();
    let value = words.next().unwrap_or_default().to_string();
    if let Some(path) = target.strip_prefix("ini:").or_else(|| target.strip_prefix("INI:")) {
        let (section, key) = match path.rsplit_once('.') {
            Some(pair) => pair,
            None => (path, ""),
        };
        // Without a property it is the ini entry itself that is being read. With one, the
        // entry names the class whose own section holds it; an entry that names nothing
        // leaves the setting where it was asked for rather than in a nameless section.
        let (section, key) = match property.is_empty() {
            true => (section.to_string(), key.to_string()),
            false => match vm.config.ini.get(section, key) {
                Some(class) if !class.is_empty() => (class.to_string(), property),
                _ => (format!("{section}.{key}"), property),
            },
        };
        match verb.as_str() {
            "get" => return Some(vm.config.ini.get(&section, &key).unwrap_or_default().to_string()),
            "set" => {
                vm.config.change(&section, &key, &value);
                vm.apply_audio_settings();
                return Some(String::new());
            }
            _ => {}
        }
    }
    if exec_function(vm, &command) {
        return Some(String::new());
    }
    match verb.as_str() {
        "set" => return set_command(vm, &command),
        "get" => return get_command(vm, &command),
        "exit" | "quit" => {
            vm.quit = true;
            return Some(String::new());
        }
        "hideactors" | "showactors" => {
            vm.hide_actors = verb == "hideactors";
            return Some(String::new());
        }
        _ => {}
    }
    // `open <map>` changes level: the engine travels once the level asks it to, which is
    // what `NextURL` does.
    if verb == "open" && !target.is_empty() {
        if let Some(info) = vm.level_info {
            let _ = vm.set_prop(info, "NextURL", Value::Str(target));
            let _ = vm.set_prop(info, "NextSwitchCountdown", Value::Float(0.0));
        }
        return Some(String::new());
    }
    vm.warn("ConsoleCommand: console commands not executed yet");
    None
}

/// What the three distance iterators ask of an actor besides its class and how far it is.
enum Visibility {
    Any,
    Seen { skip_hidden: bool },
    Colliding { skip_hidden: bool },
}

/// The rows of a `RadiusActors`-shaped iterator: every actor of the class within `radius` of
/// `from`, in the order the level holds them.
fn actors_near(
    vm: &mut Vm,
    c: &NativeCall,
    base: Option<grim_object::ClassId>,
    radius: f32,
    from: [f32; 3],
    want: Visibility,
) -> VmResult<Vec<Vec<Value>>> {
    let mut rows = Vec::new();
    for a in vm.actors.clone() {
        if vm.is_deleted(a) {
            continue;
        }
        let ac = vm.class_of(a)?;
        if base.is_some_and(|b| !vm.world.is_child_of(ac, b)) {
            continue;
        }
        let at = crate::vm::floats3(&vm.get_prop(a, "Location")?).unwrap_or([0.0; 3]);
        let d = [0, 1, 2].map(|i| at[i] - from[i]);
        if radius > 0.0 && d[0] * d[0] + d[1] * d[1] + d[2] * d[2] > radius * radius {
            continue;
        }
        let hidden = matches!(want, Visibility::Seen { skip_hidden: true } | Visibility::Colliding { skip_hidden: true });
        if hidden && matches!(vm.get_prop(a, "bHidden")?, Value::Bool(true)) {
            continue;
        }
        if matches!(want, Visibility::Colliding { .. }) && !matches!(vm.get_prop(a, "bCollideActors")?, Value::Bool(true)) {
            continue;
        }
        if !matches!(want, Visibility::Any) {
            // Only the level's own geometry hides an actor here: actors do not block the line,
            // which is what makes a crowd of pawns all visible to the one aiming at them.
            let blocked = vm.bsp.as_ref().is_some_and(|b| b.line_check(from, at).is_some());
            if blocked {
                continue;
            }
        }
        let mut row = c.args.clone();
        row[1] = Value::Object(Some(a));
        rows.push(row);
    }
    Ok(rows)
}
