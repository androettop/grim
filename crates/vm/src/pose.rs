//! Where an actor's bones are.
//!
//! The pose belongs to the engine, not to whatever draws it: the scripts read bone positions
//! themselves, and `Pawn.WeaponLoc` and `WeaponRot` are native properties the engine fills from
//! the bone a pawn holds its weapon in.

use std::collections::HashMap;

use grim_object::{ObjectKey, Value};
use grim_world::anim::{blend_locals, Joint, Skin, Tracks};

use crate::vm::Vm;

/// The skeleton of one actor, with the animation it plays.
pub struct Pose {
    pub skin: Skin,
    pub tracks: Tracks,
    /// Sequence names, with their frame count and rate.
    pub sequences: Vec<(String, i32, f32)>,
    /// The pose held before the sequence being blended into, so the blend has a source.
    held: Vec<Option<Joint>>,
    /// Bumped whenever `held` changes, as part of what a remembered pose was worked out from.
    held_generation: u32,
    /// The last pose worked out and what it was worked out from: asked for again with nothing
    /// changed (the weapon bone every tick, the drawing, each bone a script reads) it is
    /// handed back instead of sampling every track again.
    memo: Option<(Vec<u32>, Vec<Option<Joint>>)>,
    /// Centre of the mesh's own box and the scale it is stored at: a mesh is drawn with that
    /// centre on the actor, so a bone has to be moved the same way to land where it is drawn.
    centre: [f32; 3],
    scale: [f32; 3],
    /// How far the bottom of the box is below that centre, already at the mesh's own scale:
    /// what an actor that stands on its feet is lowered by.
    low: f32,
    /// The bone the mesh itself names as the one holding a weapon, and the frame it puts the
    /// weapon in relative to that bone.
    weapon_bone: Option<usize>,
    weapon_adjust: grim_assets::mesh::Coords,
}

impl Vm {
    /// The actor's skeleton, read from its mesh the first time it is asked for.
    /// How many frames a sequence of an actor's mesh lasts. Its keys are on frames 0 to one
    /// short of that, so a caller that sets `AnimFrame` by hand to reach a pose rather than to
    /// play knows where the last key is.
    pub(crate) fn sequence_frames(&mut self, actor: ObjectKey, sequence: &str) -> Option<i32> {
        self.pose(actor)?;
        let pose = self.poses.get(&actor)?;
        pose.sequences.iter().find(|(n, ..)| n.eq_ignore_ascii_case(sequence)).map(|&(_, frames, _)| frames)
    }

    fn pose(&mut self, actor: ObjectKey) -> Option<&mut Pose> {
        let Ok(Value::Object(Some(mesh_key))) = self.get_prop(actor, "Mesh") else { return None };
        if !self.poses.contains_key(&actor) {
            let made = self.build_pose(mesh_key)?;
            self.poses.insert(actor, made);
        }
        self.poses.get_mut(&actor)
    }

    fn build_pose(&mut self, mesh_key: ObjectKey) -> Option<Pose> {
        let pkg = self.world.package(mesh_key.package).clone();
        let grim_assets::Asset::SkeletalMesh(mesh) = grim_assets::load_export(&pkg, mesh_key.export).ok()?.asset else {
            return None;
        };
        let skin = Skin::new(&pkg, &mesh);
        let handle = self.world.lib.resolve(&pkg, mesh.default_animation).ok()?;
        let grim_assets::Asset::Animation(anim) = grim_assets::load_export(&handle.package, handle.export).ok()?.asset
        else {
            return None;
        };
        let mut tracks = Tracks::new(&handle.package, &anim);
        tracks.bind(&skin.bone_names);
        let sequences =
            anim.anim_seqs.iter().map(|s| (handle.package.name(s.name).to_string(), s.num_frames, s.rate)).collect();
        let lod = &mesh.lod.mesh;
        let b = lod.primitive.bounding_box;
        let centre = [(b.min.x + b.max.x) / 2.0, (b.min.y + b.max.y) / 2.0, (b.min.z + b.max.z) / 2.0];
        let scale = [lod.scale.x, lod.scale.y, lod.scale.z];
        let low = (b.min.z - centre[2]) * scale[2];
        let weapon_bone = usize::try_from(mesh.weapon_bone_index).ok().filter(|&i| i < mesh.ref_skeleton.len());
        Some(Pose { skin, tracks, sequences, held: Vec::new(), held_generation: 0, memo: None, centre, scale, low, weapon_bone, weapon_adjust: mesh.weapon_adjust })
    }

    /// Where an actor's mesh is anchored: the world point its stored box centre is drawn at.
    /// The vertices of a mesh are kept with that centre taken out of them, so this is the
    /// translation of the matrix it is drawn with — and the point a bone has to be measured
    /// from, or a bone and the mesh it belongs to end up in different places.
    ///
    /// `centre` and `low` are the mesh's own, at the mesh's scale; `scale` is the actor's
    /// `DrawScale`. Two rules meet here: across the floor a mesh stands on its own origin,
    /// and in height an actor the engine moves stands on the bottom of its collision cylinder
    /// while one that only sits where the map put it stands on its origin there too.
    pub fn mesh_anchor(&mut self, actor: ObjectKey, centre: [f32; 3], low: f32, scale: f32) -> [f32; 3] {
        let mut at = self.get_prop(actor, "Location").and_then(|v| crate::vm::floats3(&v)).unwrap_or([0.0; 3]);
        let rotation = self.get_prop(actor, "Rotation").and_then(|v| crate::vm::ints3(&v)).unwrap_or([0; 3]);
        let axes = crate::natives::core::axes(rotation);
        // Across the floor a mesh stands on its own origin, not on the middle of its box: the
        // vertices were stored with that middle taken out of them, so putting it back is what
        // leaves the mesh where its actor is. Measured against what `Adv1Willow` puts around
        // its whomping willow by hand — the twelve `GenericColObj` that are the collision of
        // one tree's root arms — each of them lands 30 units from a bone of the skeleton this
        // way, against 64 with the middle of the box on the actor and 167 with the root bone
        // there.
        for c in 0..2 {
            at[c] += (axes[0][c] * centre[0] - axes[1][c] * centre[1]) * scale;
        }
        let moves = !matches!(self.get_prop(actor, "Physics"), Ok(Value::Byte(0)));
        let height = match self.get_prop(actor, "CollisionHeight") {
            Ok(Value::Float(h)) => h,
            _ => 0.0,
        };
        if moves && height > 0.0 {
            at[2] += -height - low * scale;
        } else {
            // Measured on the wall torches of `Adv1Willow`: their flames sit at the torch's
            // origin plus the offset the class gives (the original's save agrees to the last
            // digit), and only with the torch on its own origin does that land in the cage.
            // With the middle of its box there instead, the torch stood 18.6 units above its
            // flame.
            at[2] += (axes[0][2] * centre[0] - axes[1][2] * centre[1] + axes[2][2] * centre[2]) * scale;
        }
        // `PrePivot` moves the mesh off its anchor. Nothing in the maps sets it for a mesh; the
        // scripts do, to keep a mesh still while its cylinder changes (`harry.MountFinish`).
        if let Ok(pivot) = self.get_prop(actor, "PrePivot").and_then(|v| crate::vm::floats3(&v)) {
            for i in 0..3 {
                at[i] += pivot[i];
            }
        }
        at
    }

    /// Which sequence an actor is playing, how far into it in frames, how far it has blended
    /// into it from the pose it held (1 when done), and how long the sequence lasts if it
    /// loops.
    pub(crate) fn playing(&mut self, actor: ObjectKey) -> Option<(usize, f32, f32, Option<f32>)> {
        let Ok(Value::Name(seq)) = self.get_prop(actor, "AnimSequence") else { return None };
        let Ok(Value::Float(frame)) = self.get_prop(actor, "AnimFrame") else { return None };
        let name = self.world.names.str(seq).to_string();
        let alpha = match self.get_prop(actor, "TweenAlpha") {
            Ok(Value::Float(v)) => v.clamp(0.0, 1.0),
            _ => 1.0,
        };
        let pose = self.poses.get(&actor)?;
        let (index, frames) = pose
            .sequences
            .iter()
            .enumerate()
            .find(|(_, (n, ..))| n.eq_ignore_ascii_case(&name))
            .map(|(i, (_, f, _))| (i, *f))?;
        // A sequence lasts as many frames as it declares, and its keys stop one short of that:
        // Harry's 19-frame `StrafeRight` has keys on frames 0 to 18 and a track time of 19/30
        // of a second. So the actor's frame, which runs from 0 to 1 over the whole sequence,
        // spans every frame including the last — the one that carries the pose from the last
        // key back to the first, which only a loop plays through (one that does not loop stops
        // at `AnimLast`, its last key).
        let span = frames.max(1) as f32;
        let at = frame.clamp(0.0, 1.0) * span;
        let loops = matches!(self.get_prop(actor, "bAnimLoop"), Ok(Value::Bool(true)));
        Some((index, at, alpha, loops.then_some(span)))
    }

    /// The body's pose as the sequence it plays has it, blended with the pose it held when
    /// it was told to play it.
    fn body_locals(&mut self, actor: ObjectKey) -> Option<Vec<Option<Joint>>> {
        let Some((index, at, alpha, wrap)) = self.playing(actor) else {
            let pose = self.poses.get(&actor)?;
            return Some(vec![None; pose.skin.bind_local.len()]);
        };
        let pose = self.poses.get(&actor)?;
        let locals = pose.tracks.locals(index, at, pose.skin.bind_local.len(), wrap);
        if alpha < 1.0 && !pose.held.is_empty() {
            return Some(blend_locals(&pose.held, &locals, alpha));
        }
        Some(locals)
    }

    /// Keeps the pose an actor shows now as the one the next blend starts from. An actor
    /// never posed yet has nothing on screen to blend from and is left alone.
    pub(crate) fn freeze_pose(&mut self, actor: ObjectKey) {
        if !self.poses.contains_key(&actor) {
            return;
        }
        if let Some(mut locals) = self.body_locals(actor) {
            // What was on screen, root included: an animation that carried the actor held its
            // root bone still, and so does the pose the next one starts from. Kept with the
            // travel in it, the blend out of a climb drew Harry the whole climb above the
            // ledge he was already standing on, and brought him down over the blend.
            if let Some(start) = self.root_moves.get(&actor).and_then(|r| r.start) {
                if let Some(Some(root)) = locals.first_mut() {
                    root.position = start;
                }
            }
            if let Some(pose) = self.poses.get_mut(&actor) {
                pose.held = locals;
                pose.held_generation = pose.held_generation.wrapping_add(1);
            }
        }
    }

    /// The actor's bones in their own spaces, as the animation it plays has them this frame.
    /// Blending into a new sequence and the channels posing parts of the skeleton are both
    /// applied here, so everything that asks where a bone is gets the same answer.
    pub fn pose_locals(&mut self, actor: ObjectKey) -> Option<Vec<Option<Joint>>> {
        self.pose(actor)?;
        let key = self.pose_inputs(actor);
        if let Some((was, locals)) = self.poses.get(&actor).and_then(|p| p.memo.as_ref()) {
            if *was == key {
                return Some(locals.clone());
            }
        }
        let locals = self.work_out_pose(actor)?;
        if let Some(pose) = self.poses.get_mut(&actor) {
            pose.memo = Some((key, locals.clone()));
        }
        Some(locals)
    }

    /// Everything a pose is worked out from, as bits: the body's sequence, frame and blend, the
    /// pose it blends from, where an animation that carries the actor started, and what each
    /// channel posing it plays. Two equal lists make the same pose.
    fn pose_inputs(&mut self, actor: ObjectKey) -> Vec<u32> {
        let bits = |v: crate::VmResult<Value>| match v {
            Ok(Value::Float(f)) => f.to_bits(),
            Ok(Value::Name(n)) => n.0,
            Ok(Value::Bool(b)) => b as u32,
            _ => u32::MAX,
        };
        let mut key = vec![
            bits(self.get_prop_id(actor, self.hot.anim_sequence)),
            bits(self.get_prop_id(actor, self.hot.anim_frame)),
            bits(self.get_prop(actor, "TweenAlpha")),
            bits(self.get_prop_id(actor, self.hot.anim_loop)),
            self.poses.get(&actor).map_or(0, |p| p.held_generation),
        ];
        match self.root_moves.get(&actor).and_then(|r| r.start) {
            Some(start) => key.extend(start.map(f32::to_bits)),
            None => key.push(u32::MAX),
        }
        for channel in self.channels_of(actor).to_vec().into_iter().filter(|c| c.posing) {
            key.push(channel.bone.0);
            key.push(channel.kind as u32);
            key.push(bits(self.get_prop_id(channel.actor, self.hot.anim_sequence)));
            key.push(bits(self.get_prop_id(channel.actor, self.hot.anim_frame)));
            key.push(bits(self.get_prop_id(channel.actor, self.hot.anim_loop)));
        }
        key
    }

    /// The pose itself, sampled from the tracks.
    fn work_out_pose(&mut self, actor: ObjectKey) -> Option<Vec<Option<Joint>>> {
        // An actor playing nothing the mesh has stands in its reference pose, which is what the
        // editor shows and what a map is laid out against. Its bones have to be answerable too:
        // the second whomping willow of `Adv1Willow` carries `AnimSequence = Root2`, a bone
        // name and not a sequence, so nothing plays on it at all.
        let mut locals = self.body_locals(actor)?;
        // An animation that carries the actor keeps the mesh where the actor is: the travel of
        // the root bone has already been handed to the actor, so the pose holds it still.
        if let Some(start) = self.root_moves.get(&actor).and_then(|r| r.start) {
            if let Some(Some(root)) = locals.first_mut() {
                root.position = start;
            }
        }
        // Channels pose part of the skeleton over what the body is doing: a face while it talks,
        // the eyes while they blink, the head while it follows something.
        for channel in self.channels_of(actor).to_vec().into_iter().filter(|c| c.posing) {
            let (Ok(Value::Name(seq)), Ok(Value::Float(frame))) =
                (self.get_prop(channel.actor, "AnimSequence"), self.get_prop(channel.actor, "AnimFrame"))
            else {
                continue;
            };
            let name = self.world.names.str(seq).to_string();
            let bone = self.world.names.str(channel.bone).to_string();
            let Some(pose) = self.poses.get(&actor) else { continue };
            let Some((index, frames)) =
                pose.sequences.iter().enumerate().find(|(_, (n, ..))| n.eq_ignore_ascii_case(&name)).map(|(i, (_, f, _))| (i, *f))
            else {
                continue;
            };
            // The channel runs on its own frame and loops on its own, as the body does.
            let span = frames.max(1) as f32;
            let at = frame.clamp(0.0, 1.0) * span;
            let loops = matches!(self.get_prop(channel.actor, "bAnimLoop"), Ok(Value::Bool(true)));
            let Some(pose) = self.poses.get(&actor) else { continue };
            let over = pose.tracks.locals(index, at, pose.skin.bind_local.len(), loops.then_some(span));
            pose.skin.take_over(&mut locals, &over, &bone, channel.kind == 1);
        }
        Some(locals)
    }

    /// Every bone of an actor's mesh, by name.
    pub fn bone_names(&mut self, actor: ObjectKey) -> Vec<String> {
        match self.pose(actor) {
            Some(pose) => pose.skin.bone_names.clone(),
            None => Vec::new(),
        }
    }

    /// Where a bone of an actor is in the world, and which way it points.
    pub fn bone_world(&mut self, actor: ObjectKey, bone: &str) -> Option<([f32; 3], [i32; 3])> {
        let index = {
            let locals = self.pose_locals(actor)?;
            let _ = &locals;
            self.poses.get(&actor)?.skin.bone_names.iter().position(|b| b.eq_ignore_ascii_case(bone))?
        };
        self.joint_world(actor, index, grim_assets::mesh::Coords::IDENTITY)
    }

    /// Where a bone of an actor is in the world, by its index in the skeleton.
    pub(crate) fn bone_index_world(&mut self, actor: ObjectKey, index: usize) -> Option<([f32; 3], [i32; 3])> {
        self.pose_locals(actor)?;
        if index >= self.poses.get(&actor)?.skin.bone_names.len() {
            return None;
        }
        self.joint_world(actor, index, grim_assets::mesh::Coords::IDENTITY)
    }

    /// Where the weapon of an actor sits, as its own mesh says: the mesh names the bone that
    /// holds one and the frame the weapon takes inside it.
    pub fn weapon_world(&mut self, actor: ObjectKey) -> Option<([f32; 3], [i32; 3])> {
        let (index, adjust) = {
            self.pose_locals(actor)?;
            let pose = self.poses.get(&actor)?;
            (pose.weapon_bone?, pose.weapon_adjust)
        };
        self.joint_world(actor, index, adjust)
    }

    /// One posed joint, put through a frame of its own, in world axes.
    fn joint_world(&mut self, actor: ObjectKey, index: usize, adjust: grim_assets::mesh::Coords) -> Option<([f32; 3], [i32; 3])> {
        let locals = self.pose_locals(actor)?;
        let pose = self.poses.get(&actor)?;
        let joint = *pose.skin.world(&locals).get(index)?;
        let (centre, mesh_scale, low) = (pose.centre, pose.scale, pose.low);
        let rotation = crate::vm::ints3(&self.get_prop(actor, "Rotation").ok()?).ok()?;
        let draw_scale = match self.get_prop(actor, "DrawScale") {
            Ok(Value::Float(s)) if s != 0.0 => s,
            _ => 1.0,
        };
        let location = self.mesh_anchor(actor, centre, low, draw_scale);
        let axes = crate::natives::core::axes(rotation);
        let o = adjust.origin;
        // A mesh is drawn with the centre of its own box on the actor and mirrored along Y,
        // and a bone has to take the same road to land where the mesh does.
        let to_world = |p: [f32; 3], translate: bool| {
            let m = [0, 1, 2].map(|i| (p[i] - if translate { centre[i] } else { 0.0 }) * mesh_scale[i] * draw_scale);
            let mut out = if translate { location } else { [0.0; 3] };
            for c in 0..3 {
                out[c] += axes[0][c] * m[0] - axes[1][c] * m[1] + axes[2][c] * m[2];
            }
            out
        };
        let at = to_world(joint.apply([o.x, o.y, o.z]), true);
        // Its axes, for the rotator the scripts turn offsets by and the drawing places it
        // with. Mirroring the mesh along Y leaves the frame this builds with its up along the
        // bone's own -Z, which is the way round the game wants: `GetWandEndPoint` reads the
        // wand's tip 20 units along -Z of it, and a hand at rest then holds the wand upright.
        let origin = joint.apply([0.0; 3]);
        let dir = |v: grim_package::property::Vector| {
            let turned = joint.apply([v.x, v.y, v.z]);
            to_world([0, 1, 2].map(|c| turned[c] - origin[c]), false)
        };
        Some((at, crate::vm::rotation_of_axes(dir(adjust.x_axis), dir(adjust.y_axis))))
    }

    /// Moves the actors whose animation carries them. Called once the frame has advanced, so
    /// the step is the travel of the root bone between the last frame and this one.
    pub fn tick_root_motion(&mut self, report: &mut crate::level::Report) {
        for a in self.root_moves.keys().copied().collect::<Vec<_>>() {
            if self.is_deleted(a) {
                self.root_moves.remove(&a);
                continue;
            }
            // Playing something else means the animation that asked for this is over.
            let playing = self.get_prop(a, "AnimSequence");
            let wanted = self.root_moves.get(&a).map(|r| r.sequence);
            if !matches!((playing, wanted), (Ok(Value::Name(s)), Some(w)) if s == w) {
                self.root_moves.remove(&a);
                continue;
            }
            if let Err(e) = self.root_step(a) {
                report.record(e);
            }
        }
    }

    /// One frame of an animation that carries the actor.
    fn root_step(&mut self, a: ObjectKey) -> crate::VmResult<()> {
        self.pose(a);
        let Some((index, at, _, wrap)) = self.playing(a) else { return Ok(()) };
        let Some(pose) = self.poses.get(&a) else { return Ok(()) };
        let bones = pose.skin.bind_local.len();
        let Some(Some(root)) = pose.tracks.locals(index, at, bones, wrap).into_iter().next() else { return Ok(()) };
        let (centre_scale, draw_scale) = (pose.scale, match self.get_prop(a, "DrawScale") {
            Ok(Value::Float(s)) if s != 0.0 => s,
            _ => 1.0,
        });
        let here = root.position;
        let Some(entry) = self.root_moves.get_mut(&a) else { return Ok(()) };
        if entry.start.is_none() {
            // The first frame is where the mesh is held; the travel is measured from it.
            entry.start = Some(here);
        }
        let last = entry.last.replace(here);
        let Some(last) = last else { return Ok(()) };
        let step = [0, 1, 2].map(|i| (here[i] - last[i]) * centre_scale[i] * draw_scale);
        // A mesh is drawn mirrored along Y, so the step the root bone takes has to be turned
        // the same way before it means anything in the world.
        let rotation = crate::vm::ints3(&self.get_prop(a, "Rotation")?)?;
        let axes = crate::natives::core::axes(rotation);
        let mut delta = [0.0f32; 3];
        for c in 0..3 {
            delta[c] = axes[0][c] * step[0] - axes[1][c] * step[1] + axes[2][c] * step[2];
        }
        if delta.iter().all(|v| v.abs() < 1e-5) {
            return Ok(());
        }
        // The travel is not swept against the world. A climb lifts the actor over the very
        // ledge it is standing against, so a move that stopped at the wall would never get
        // over it; the script shrinks the collision for the climb instead, which is what keeps
        // it from catching on anything else. Hypothesis, as is the rest of this.
        let at = crate::vm::floats3(&self.get_prop(a, "Location")?)?;
        let to = [0, 1, 2].map(|i| at[i] + delta[i]);
        self.set_prop(a, "Location", crate::vm::vector(to[0], to[1], to[2]))?;
        Ok(())
    }

    /// Fills the bone-driven native properties of every actor that has them: a pawn carries
    /// where its weapon sits, which is how the scripts put the wand's effects on its tip.
    ///
    /// Only for what was drawn, when something draws: the original works the bones out as it
    /// poses a mesh to draw it, and its saves show it — in the fifth checkpoint Hermione's
    /// `WeaponLoc` is 1700 units from where she stands and two students' are on the far side
    /// of the castle, the places they were last seen. Posing every pawn of a hub every tick
    /// was a quarter of the tick.
    pub fn tick_bones(&mut self, report: &mut crate::level::Report) {
        for a in self.actors.clone() {
            if self.is_deleted(a) || !matches!(self.get_prop(a, "WeaponLoc"), Ok(Value::Struct(_))) {
                continue;
            }
            if self.drawn.as_ref().is_some_and(|d| !d.contains(&a)) && Some(a) != self.player {
                continue;
            }
            let Some((at, rot)) = self.weapon_world(a) else { continue };
            for (name, value) in
                [("WeaponLoc", crate::vm::vector(at[0], at[1], at[2])), ("WeaponRot", crate::vm::rotator(rot[0], rot[1], rot[2]))]
            {
                if let Err(e) = self.set_prop(a, name, value) {
                    report.record(e);
                }
            }
        }
    }

    /// Drops the skeletons of a level that is going away.
    pub fn clear_poses(&mut self) {
        self.poses.clear();
    }
}

/// Skeletons by actor.
pub type Poses = HashMap<ObjectKey, Pose>;

/// An animation whose root bone carries the actor rather than the mesh.
///
/// Hypothesis: `PlayAnim`'s fifth argument names a bone, and the climbs pass `'Move'`, which
/// no skeleton of the game has. What they do have is a root bone that travels: `climb32` lifts
/// it 31.7 units and pushes it 26.4 forward, `climb64` 63.4 and 31.0, `climb96end` 82.4 and
/// 23.8. Those are exactly the amounts `harry.Mounting` takes off the distance it was given
/// before moving what is left over by hand (32, 64, 82, and 30 forward), so the animation is
/// meant to cover them: the name asks the engine to move the actor by the root bone.
#[derive(Clone, Copy)]
pub struct RootMove {
    /// The sequence it belongs to; anything else cancels it.
    pub sequence: grim_object::NameId,
    /// Where the root bone stood when it was last sampled, in the mesh's own space.
    last: Option<[f32; 3]>,
    /// Where it stands at the first frame, which is where the pose holds it.
    start: Option<[f32; 3]>,
}

impl RootMove {
    pub fn new(sequence: grim_object::NameId) -> Self {
        Self { sequence, last: None, start: None }
    }
}
