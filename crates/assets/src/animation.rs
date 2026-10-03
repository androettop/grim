//! `Engine.Animation`, KnowWonder variant: skeletal animation with compressed keys stored in
//! global pools at the end of the object.
//!
//! ```text
//! RefBones   TArray<{FName, u32 flags, i32 parent}>
//! Moves      TArray<MotionChunk>
//!              vec RootSpeed3D, f32 TrackTime, i32 StartBone, u32 Flags,
//!              TArray<i32> BoneIndices, TArray<Track>
//!              Track = u32 flags, compact nrot, compact npos, compact ntime,
//!                      f32 pos_scale, f32 frame_time
//! AnimSeqs   TArray<{FName, TArray<FName> groups, i32 start, i32 frames,
//!                    TArray<{f32 time, FName fn}> notifys, f32 rate}>
//! RotKeys    TArray<[i16; 3]>  len == Σnrot over all tracks, in track order
//! PosKeys    TArray<[i16; 3]>  len == Σnpos
//! KeyFrames  TArray<u8>        len == Σntime; frame deltas, first 0
//! ```
//!
//! Verified on every animation of the game (see tests): nrot == ntime, npos ∈ {1, ntime},
//! last key frame == frames - 1, frame_time == 1 / rate, TrackTime == frames / rate.
//! Rotation keys store each quaternion component as `asin(q) * 2/π * 32767` (W implicit and
//! positive). Checked against 4087 single-key tracks vs the mesh reference pose: 0.015° median
//! error, sharp at that scale, and Σxyz² ≤ 1 (within quantization) on every key of the game. Position keys are scaled
//! by `pos_scale / 32767` (reproduces the reference pose). Both confirmed visually in grim-viewer,
//! applying rotations conjugated like the mesh reference skeleton.

use grim_package::property::{read_name, read_vector, Vector};
use grim_package::{NameIndex, Reader, Result};

use crate::model::{read_array, section};
use crate::{check, Ctx};

#[derive(Debug, Clone)]
pub struct NamedBone {
    pub name: NameIndex,
    pub flags: u32,
    pub parent_index: i32,
}

#[derive(Debug, Clone)]
pub struct Track {
    pub flags: u32,
    pub num_rot_keys: u32,
    pub num_pos_keys: u32,
    pub num_time_keys: u32,
    pub pos_scale: f32,
    pub frame_time: f32,
}

#[derive(Debug, Clone)]
pub struct MotionChunk {
    pub root_speed_3d: Vector,
    pub track_time: f32,
    pub start_bone: i32,
    pub flags: u32,
    pub bone_indices: Vec<i32>,
    pub tracks: Vec<Track>,
}

#[derive(Debug, Clone)]
pub struct AnimNotify {
    pub time: f32,
    pub function: NameIndex,
}

#[derive(Debug, Clone)]
pub struct AnimSeq {
    pub name: NameIndex,
    pub groups: Vec<NameIndex>,
    pub start_frame: i32,
    pub num_frames: i32,
    pub notifys: Vec<AnimNotify>,
    pub rate: f32,
}

#[derive(Debug, Clone)]
pub struct Animation {
    pub ref_bones: Vec<NamedBone>,
    pub moves: Vec<MotionChunk>,
    pub anim_seqs: Vec<AnimSeq>,
    pub rot_keys: Vec<[i16; 3]>,
    pub pos_keys: Vec<[i16; 3]>,
    pub key_frames: Vec<u8>,
}

/// Rotation key to quaternion `[x, y, z, w]`: `q = sin(v / 32767 * π/2)` per component.
pub fn decode_rotation(k: [i16; 3]) -> [f32; 4] {
    let [x, y, z] = k.map(|c| (c as f64 / 32767.0 * std::f64::consts::FRAC_PI_2).sin());
    let w = (1.0 - x * x - y * y - z * z).max(0.0).sqrt();
    [x as f32, y as f32, z as f32, w as f32]
}

pub fn decode_position(k: [i16; 3], pos_scale: f32) -> [f32; 3] {
    k.map(|v| v as f32 * pos_scale / 32767.0)
}

fn read_count_u32(r: &mut Reader) -> Result<u32> {
    Ok(r.compact_len()? as u32)
}

fn read_key(r: &mut Reader) -> Result<[i16; 3]> {
    Ok([r.i16()?, r.i16()?, r.i16()?])
}

impl Animation {
    pub fn parse(cx: &Ctx, r: &mut Reader) -> Result<Self> {
        let pkg = cx.pkg;
        let ref_bones = section(
            "ref_bones",
            read_array(r, 9, |r| Ok(NamedBone { name: read_name(pkg, r)?, flags: r.u32()?, parent_index: r.i32()? })),
        )?;
        let moves: Vec<MotionChunk> = section(
            "moves",
            read_array(r, 12 + 4 + 8 + 2, |r| {
                Ok(MotionChunk {
                    root_speed_3d: read_vector(r)?,
                    track_time: r.f32()?,
                    start_bone: r.i32()?,
                    flags: r.u32()?,
                    bone_indices: read_array(r, 4, |r| r.i32())?,
                    tracks: read_array(r, 4 + 3 + 8, |r| {
                        Ok(Track {
                            flags: r.u32()?,
                            num_rot_keys: read_count_u32(r)?,
                            num_pos_keys: read_count_u32(r)?,
                            num_time_keys: read_count_u32(r)?,
                            pos_scale: r.f32()?,
                            frame_time: r.f32()?,
                        })
                    })?,
                })
            }),
        )?;
        let anim_seqs = section(
            "anim_seqs",
            read_array(r, 1 + 1 + 8 + 1 + 4, |r| {
                Ok(AnimSeq {
                    name: read_name(pkg, r)?,
                    groups: read_array(r, 1, |r| read_name(pkg, r))?,
                    start_frame: r.i32()?,
                    num_frames: r.i32()?,
                    notifys: read_array(r, 5, |r| Ok(AnimNotify { time: r.f32()?, function: read_name(pkg, r)? }))?,
                    rate: r.f32()?,
                })
            }),
        )?;
        let tracks = || moves.iter().flat_map(|m| &m.tracks);
        let sum = |f: fn(&Track) -> u32| tracks().map(|t| f(t) as u64).sum::<u64>();
        let at = r.offset();
        let rot_keys = section("rot_keys", read_array(r, 6, read_key))?;
        check(at, "rot keys vs track counts", rot_keys.len() as u64, sum(|t| t.num_rot_keys))?;
        let at = r.offset();
        let pos_keys = section("pos_keys", read_array(r, 6, read_key))?;
        check(at, "pos keys vs track counts", pos_keys.len() as u64, sum(|t| t.num_pos_keys))?;
        let at = r.offset();
        let n = r.count(1)?;
        let key_frames = r.bytes(n)?.to_vec();
        check(at, "key frames vs track counts", key_frames.len() as u64, sum(|t| t.num_time_keys))?;
        Ok(Self { ref_bones, moves, anim_seqs, rot_keys, pos_keys, key_frames })
    }
}
