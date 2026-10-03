//! Posing and skinning a skeletal mesh.
//!
//! A mesh keeps its bind skeleton and, per bone, the influences it pulls: each one names a point
//! and its weight, and `local_points` holds that point in the bone's own space (one entry per
//! influence, checked on the game's meshes). Skinning is then the weighted sum of each
//! influence's local point put through its bone, with no inverse bind matrices.

use grim_assets::animation::{decode_position, decode_rotation, Animation};
use grim_assets::mesh::SkeletalMesh;

use crate::Vec3;

/// Rotation and translation, which is all a bone carries.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Joint {
    /// Quaternion `[x, y, z, w]`.
    pub rotation: [f32; 4],
    pub position: Vec3,
}

impl Joint {
    pub const IDENTITY: Self = Self { rotation: [0.0, 0.0, 0.0, 1.0], position: [0.0; 3] };

    /// Part way from one joint to another, for blending one animation into the next.
    pub fn blend(&self, other: &Joint, t: f32) -> Joint {
        Joint {
            rotation: slerp(self.rotation, other.rotation, t),
            position: [0, 1, 2].map(|i| self.position[i] + (other.position[i] - self.position[i]) * t),
        }
    }
}

/// Blends one pose into another; a bone only one of them poses is taken as it is.
pub fn blend_locals(from: &[Option<Joint>], to: &[Option<Joint>], t: f32) -> Vec<Option<Joint>> {
    (0..from.len().max(to.len()))
        .map(|b| match (from.get(b).copied().flatten(), to.get(b).copied().flatten()) {
            (Some(a), Some(c)) => Some(a.blend(&c, t)),
            (a, c) => c.or(a),
        })
        .collect()
}

impl Joint {

    /// Applies this joint to a point.
    /// The same turn and move as a 3x4 matrix: row `i` gives coordinate `i` of the moved point.
    pub fn matrix(&self) -> [[f32; 4]; 3] {
        let [x, y, z, w] = self.rotation;
        let (xx, yy, zz) = (x * x, y * y, z * z);
        let (xy, xz, yz) = (x * y, x * z, y * z);
        let (wx, wy, wz) = (w * x, w * y, w * z);
        let p = self.position;
        [
            [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy), p[0]],
            [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx), p[1]],
            [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy), p[2]],
        ]
    }

    pub fn apply(&self, p: Vec3) -> Vec3 {
        let [x, y, z, w] = self.rotation;
        // p + 2w(q × p) + 2(q × (q × p))
        let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
        let q = [x, y, z];
        let t = crate::scale(cross(q, p), 2.0);
        let r = [0, 1, 2].map(|i| p[i] + w * t[i] + cross(q, t)[i]);
        [0, 1, 2].map(|i| r[i] + self.position[i])
    }

    /// The joint that applies `self` after `other`.
    pub fn then(&self, parent: &Joint) -> Joint {
        Joint { rotation: mul_quat(parent.rotation, self.rotation), position: parent.apply(self.position) }
    }
}

fn mul_quat(a: [f32; 4], b: [f32; 4]) -> [f32; 4] {
    [
        a[3] * b[0] + a[0] * b[3] + a[1] * b[2] - a[2] * b[1],
        a[3] * b[1] - a[0] * b[2] + a[1] * b[3] + a[2] * b[0],
        a[3] * b[2] + a[0] * b[1] - a[1] * b[0] + a[2] * b[3],
        a[3] * b[3] - a[0] * b[0] - a[1] * b[1] - a[2] * b[2],
    ]
}

fn conjugate(q: [f32; 4]) -> [f32; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

fn slerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
    let mut dot = (0..4).map(|i| a[i] * b[i]).sum::<f32>();
    let mut b = b;
    if dot < 0.0 {
        b = b.map(|v| -v);
        dot = -dot;
    }
    if dot > 0.9995 {
        let out = [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t);
        let len = (out.iter().map(|v| v * v).sum::<f32>()).sqrt().max(1e-6);
        return out.map(|v| v / len);
    }
    let theta = dot.clamp(-1.0, 1.0).acos();
    let (s, st) = (theta.sin(), (theta * t).sin());
    let (wa, wb) = (((1.0 - t) * theta).sin() / s, st / s);
    [0, 1, 2, 3].map(|i| a[i] * wa + b[i] * wb)
}

/// One bone's keys within a sequence.
#[derive(Debug, Clone, Default)]
struct Track {
    bone: usize,
    rotations: Vec<[f32; 4]>,
    positions: Vec<Vec3>,
    times: Vec<f32>,
}

/// An animation object with its key pools sliced per sequence and bone.
#[derive(Debug, Clone, Default)]
pub struct Tracks {
    sequences: Vec<Vec<Track>>,
    /// Bone of the animation for each bone of the mesh, matched by name.
    to_mesh: Vec<i32>,
    pub names: Vec<String>,
}

impl Tracks {
    /// Reads the key pools, which the tracks index one after another.
    pub fn new(pkg: &grim_package::Package, anim: &Animation) -> Self {
        let (mut rot, mut pos, mut time) = (0usize, 0usize, 0usize);
        let mut sequences = Vec::with_capacity(anim.moves.len());
        for m in &anim.moves {
            let mut tracks = Vec::with_capacity(m.tracks.len());
            for (k, t) in m.tracks.iter().enumerate() {
                let mut times = Vec::with_capacity(t.num_time_keys as usize);
                let mut at = 0.0;
                for j in 0..t.num_time_keys as usize {
                    at += anim.key_frames.get(time + j).copied().unwrap_or(1) as f32;
                    times.push(at);
                }
                let rotations = (0..t.num_rot_keys as usize)
                    .filter_map(|j| anim.rot_keys.get(rot + j).map(|&k| decode_rotation(k)))
                    .collect();
                let positions = (0..t.num_pos_keys as usize)
                    .filter_map(|j| anim.pos_keys.get(pos + j).map(|&k| decode_position(k, t.pos_scale)))
                    .collect();
                rot += t.num_rot_keys as usize;
                pos += t.num_pos_keys as usize;
                time += t.num_time_keys as usize;
                let bone = m.bone_indices.get(k).copied().unwrap_or(m.start_bone + k as i32);
                tracks.push(Track { bone: bone.max(0) as usize, rotations, positions, times });
            }
            sequences.push(tracks);
        }
        let names = anim.ref_bones.iter().map(|b| pkg.name(b.name).to_string()).collect();
        Self { sequences, to_mesh: Vec::new(), names }
    }

    /// Mesh bone each animation bone poses, `-1` where the mesh has none.
    pub fn to_mesh(&self) -> &[i32] {
        &self.to_mesh
    }

    /// Matches the animation's bones to a mesh's.
    ///
    /// Both come from the same rig and keep its order, but a mesh may carry bones the animation
    /// leaves out (an attachment point, say) and rigs repeat names -- several of the game's
    /// characters have two bones called `bip01 Head`. Looking a name up would then send two
    /// tracks to the same bone and leave another at its bind pose, which tears the mesh apart,
    /// so the two lists are walked together and only searched from scratch when they diverge.
    pub fn bind(&mut self, mesh_bones: &[String]) {
        let mut at = 0usize;
        self.to_mesh = self
            .names
            .iter()
            .map(|n| match mesh_bones.iter().skip(at).position(|m| m.eq_ignore_ascii_case(n)) {
                Some(offset) => {
                    at += offset + 1;
                    (at - 1) as i32
                }
                None => mesh_bones.iter().position(|m| m.eq_ignore_ascii_case(n)).map_or(-1, |i| i as i32),
            })
            .collect();
    }

    /// Local joints of a mesh's bones for a sequence at `frame`, leaving untouched bones out.
    ///
    /// `wrap` is how long the sequence lasts in frames, for one that loops. The keys of a
    /// sequence stop one frame short of that: Harry's 19-frame `StrafeRight` has its keys on
    /// frames 0 to 18 and lasts 19/30 of a second, so the last frame is the one that carries
    /// the pose from the last key round to the first. Given it, that frame is interpolated
    /// like any other; without it the last key is simply held, which is what a sequence that
    /// only plays once does with its final frame.
    pub fn locals(&self, sequence: usize, frame: f32, bones: usize, wrap: Option<f32>) -> Vec<Option<Joint>> {
        let mut out = vec![None; bones];
        let Some(tracks) = self.sequences.get(sequence) else { return out };
        for t in tracks {
            if t.rotations.is_empty() {
                continue;
            }
            let mesh_bone = match self.to_mesh.get(t.bone) {
                Some(&i) if i >= 0 => i as usize,
                Some(_) => continue,
                None => t.bone,
            };
            if mesh_bone >= bones {
                continue;
            }
            let rotation = conjugate(sample(&t.rotations, &t.times, frame, wrap, slerp));
            let position = if t.positions.is_empty() {
                [0.0; 3]
            } else {
                sample(&t.positions, &t.times, frame, wrap, |a, b, u| [0, 1, 2].map(|i| a[i] + (b[i] - a[i]) * u))
            };
            out[mesh_bone] = Some(Joint { rotation, position });
        }
        out
    }
}

/// The key at `frame`, interpolating between the two it falls between. Past the last key, a
/// sequence that loops (`wrap`, its length in frames) carries on into the first one again.
fn sample<T: Copy>(keys: &[T], times: &[f32], frame: f32, wrap: Option<f32>, lerp: impl Fn(T, T, f32) -> T) -> T {
    if keys.len() == 1 {
        return keys[0];
    }
    let at = |i: usize| if times.len() == keys.len() { times[i] } else { i as f32 };
    let mut k = 0;
    while k + 1 < keys.len() && at(k + 1) <= frame {
        k += 1;
    }
    if k + 1 >= keys.len() {
        let last = at(k);
        return match wrap {
            Some(total) if total > last && frame > last => lerp(keys[k], keys[0], ((frame - last) / (total - last)).clamp(0.0, 1.0)),
            _ => keys[k],
        };
    }
    let span = at(k + 1) - at(k);
    let u = if span > 0.0 { (frame - at(k)) / span } else { 0.0 };
    lerp(keys[k], keys[k + 1], u.clamp(0.0, 1.0))
}

/// A skeletal mesh ready to be posed: its bind pose, hierarchy and influences.
#[derive(Debug, Clone, Default)]
pub struct Skin {
    pub parents: Vec<i32>,
    pub bind_local: Vec<Joint>,
    pub bone_names: Vec<String>,
    /// Per point: the bones pulling it, with the weight and the point in that bone's space.
    influences: Vec<Vec<(usize, f32, Vec3)>>,
    pub points: usize,
}

impl Skin {
    pub fn new(pkg: &grim_package::Package, mesh: &SkeletalMesh) -> Self {
        let parents = mesh.ref_skeleton.iter().map(|b| b.parent_index).collect();
        let bind_local = mesh
            .ref_skeleton
            .iter()
            .map(|b| {
                let q = b.bone_pos.orientation;
                let p = b.bone_pos.position;
                Joint { rotation: conjugate([q.x, q.y, q.z, q.w]), position: [p.x, p.y, p.z] }
            })
            .collect();
        let bone_names = mesh.ref_skeleton.iter().map(|b| pkg.name(b.name).to_string()).collect();
        let mut influences = vec![Vec::new(); mesh.points.len()];
        let local = &mesh.local_points;
        for (bone, index) in mesh.bone_weight_idx.iter().enumerate() {
            for k in index.weight_index as usize..(index.weight_index + index.number) as usize {
                let Some(w) = mesh.bone_weights.get(k) else { continue };
                let Some(list) = influences.get_mut(w.point_index as usize) else { continue };
                let at = local.get(k).map(|p| [p.x, p.y, p.z]).unwrap_or([0.0; 3]);
                list.push((bone, w.bone_weight as f32 / 65535.0, at));
            }
        }
        Self { parents, bind_local, bone_names, influences, points: mesh.points.len() }
    }

    /// Bind pose in world space, used when nothing is playing.
    pub fn bind_world(&self) -> Vec<Joint> {
        self.world(&vec![None; self.bind_local.len()])
    }

    /// World joints from local ones, falling back to the bind pose where a bone has no key.
    pub fn world(&self, locals: &[Option<Joint>]) -> Vec<Joint> {
        let mut out: Vec<Joint> = Vec::with_capacity(self.bind_local.len());
        for b in 0..self.bind_local.len() {
            let local = locals.get(b).copied().flatten().unwrap_or(self.bind_local[b]);
            let parent = self.parents.get(b).copied().unwrap_or(-1);
            // A bone whose parent is itself, or comes later, is a root.
            let joint = match parent {
                p if b == 0 || p < 0 || p as usize >= b => local,
                p => local.then(&out[p as usize]),
            };
            out.push(joint);
        }
        out
    }

    /// Lets another pose take over a bone and everything hanging from it, which is what an
    /// animation channel does: `combine` applies it on top of what is there, else it replaces.
    pub fn take_over(&self, locals: &mut [Option<Joint>], over: &[Option<Joint>], bone: &str, combine: bool) {
        let Some(root) = self.bone_names.iter().position(|b| b.eq_ignore_ascii_case(bone)) else { return };
        for b in root..self.bind_local.len() {
            if b != root && !self.descends_from(b, root) {
                continue;
            }
            let Some(Some(joint)) = over.get(b).copied() else { continue };
            locals[b] = Some(match (combine, locals[b]) {
                (true, Some(base)) => Joint { rotation: mul_quat(base.rotation, joint.rotation), position: base.position },
                _ => joint,
            });
        }
    }

    /// Whether a bone hangs from another. Bones come after their parent, so the walk ends.
    fn descends_from(&self, mut bone: usize, root: usize) -> bool {
        while let Some(&parent) = self.parents.get(bone) {
            if parent < 0 || parent as usize >= bone {
                return false;
            }
            if parent as usize == root {
                return true;
            }
            bone = parent as usize;
        }
        false
    }

    /// Points of the mesh put through a pose.
    pub fn deform(&self, world: &[Joint], out: &mut Vec<Vec3>) {
        out.clear();
        out.reserve(self.points);
        // Each bone as a matrix, worked out once: a point then costs nine products per bone
        // that moves it instead of turning it by a quaternion, and a mesh is thousands of them.
        let matrices: Vec<[[f32; 4]; 3]> = world.iter().map(Joint::matrix).collect();
        for list in &self.influences {
            let mut acc = [0.0f32; 3];
            for &(bone, weight, at) in list {
                let Some(m) = matrices.get(bone) else { continue };
                for i in 0..3 {
                    acc[i] += (m[i][0] * at[0] + m[i][1] * at[1] + m[i][2] * at[2] + m[i][3]) * weight;
                }
            }
            out.push(acc);
        }
    }
}

#[cfg(test)]
mod matrix_tests {
    use super::Joint;

    /// A bone's matrix moves a point where the bone itself does.
    #[test]
    fn a_joints_matrix_moves_points_as_the_joint_does() {
        let mut seed = 0x1234_5678u32;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        };
        for _ in 0..200 {
            let q = [next(), next(), next(), next()];
            let len = q.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-3);
            let joint = Joint { rotation: q.map(|v| v / len), position: [next() * 50.0, next() * 50.0, next() * 50.0] };
            let p = [next() * 30.0, next() * 30.0, next() * 30.0];
            let m = joint.matrix();
            let by_matrix = [0, 1, 2].map(|i| m[i][0] * p[0] + m[i][1] * p[1] + m[i][2] * p[2] + m[i][3]);
            let by_joint = joint.apply(p);
            for i in 0..3 {
                assert!((by_matrix[i] - by_joint[i]).abs() < 1e-3, "{by_matrix:?} against {by_joint:?}");
            }
        }
    }
}
