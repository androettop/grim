//! Validation over the real game files (see AGENTS.md). Run with `cargo test --release`.

use std::collections::BTreeSet;

use grim_assets::animation::{decode_position, decode_rotation};
use grim_assets::sound::SOUND_HAS_TRAILER;
use grim_assets::{load_export, Asset};
use grim_package::{ObjectRef, Package};


#[test]
fn every_export_of_every_package() {
    let root = grim_testkit::require_game_dir();
    let files = grim_testkit::package_files(&root);
    assert!(files.len() >= 133, "expected the full game, found {} packages", files.len());

    let mut failures = Vec::new();
    let mut unparsed = BTreeSet::new();
    let (mut anims, mut sounds, mut skels, mut exports) = (0, 0, 0, 0);
    let mut fail = |msg: String| {
        if failures.len() < 50 {
            failures.push(msg);
        }
    };
    for f in &files {
        let rel = grim_testkit::display_path(&root, f);
        let pkg = match Package::open(f).unwrap() {
            Ok(p) => p,
            Err(e) => {
                fail(format!("{rel}: {e}"));
                continue;
            }
        };
        let layout = pkg.layout();
        if !layout.gaps.is_empty() || !layout.overlaps.is_empty() {
            fail(format!("{rel}: gaps {:?} overlaps {:?}", layout.gaps, layout.overlaps));
        }
        for i in 0..pkg.exports.len() as u32 {
            let name = || format!("{rel} {}", pkg.path_name(ObjectRef::Export(i)));
            let o = match load_export(&pkg, i) {
                Ok(o) => o,
                Err(e) => {
                    fail(format!("{}: {e}", name()));
                    continue;
                }
            };
            exports += 1;
            match &o.asset {
                Asset::Unparsed(_) => {
                    unparsed.insert(o.class.clone());
                }
                Asset::Animation(a) => {
                    anims += 1;
                    check_animation(a).unwrap_or_else(|e| fail(format!("{}: {e}", name())))
                }
                Asset::Sound(s) => {
                    sounds += 1;
                    check_sound(&pkg, s).unwrap_or_else(|e| fail(format!("{}: {e}", name())))
                }
                Asset::SkeletalMesh(m) => {
                    skels += 1;
                    check_skeletal(m).unwrap_or_else(|e| fail(format!("{}: {e}", name())))
                }
                _ => {}
            }
        }
    }
    eprintln!("checked {exports} exports: {anims} animations, {sounds} sounds, {skels} skeletal meshes");
    assert!(exports > 100_000 && anims >= 368 && sounds >= 8195 && skels >= 390, "the game files look incomplete");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
    assert!(unparsed.is_empty(), "classes left unparsed: {unparsed:?}");
}

fn check_animation(a: &grim_assets::Animation) -> Result<(), String> {
    let (mut rot, mut pos, mut time) = (0usize, 0usize, 0usize);
    if a.moves.len() != a.anim_seqs.len() {
        return Err(format!("{} moves vs {} sequences", a.moves.len(), a.anim_seqs.len()));
    }
    for (m, seq) in a.moves.iter().zip(&a.anim_seqs) {
        if (m.track_time - seq.num_frames as f32 / seq.rate).abs() > 1e-3 {
            return Err(format!("TrackTime {} vs frames/rate {}", m.track_time, seq.num_frames as f32 / seq.rate));
        }
        for t in &m.tracks {
            let (nr, np, nt) = (t.num_rot_keys as usize, t.num_pos_keys as usize, t.num_time_keys as usize);
            if nr != nt || !(np == 1 || np == nt) {
                return Err(format!("track key counts rot {nr} pos {np} time {nt}"));
            }
            if (t.frame_time - 1.0 / seq.rate).abs() > 1e-6 {
                return Err(format!("frame_time {} vs 1/rate {}", t.frame_time, 1.0 / seq.rate));
            }
            let frames = &a.key_frames[time..time + nt];
            if nt > 1 && (frames[0] != 0 || frames.iter().map(|&d| d as i32).sum::<i32>() != seq.num_frames - 1) {
                return Err(format!("key frames {frames:?} for {} frames", seq.num_frames));
            }
            for &k in &a.rot_keys[rot..rot + nr] {
                let xyz2: f64 = k.iter().map(|&c| (c as f64 / 32767.0 * std::f64::consts::FRAC_PI_2).sin().powi(2)).sum();
                // Rounding each component to half a step (π/2 / 32767) can add up to ~1.7e-4.
                if xyz2 > 1.0 + 2e-4 {
                    return Err(format!("rotation key {k:?} is not a unit quaternion (xyz² = {xyz2})"));
                }
                let q = decode_rotation(k);
                let n = q.iter().map(|v| v * v).sum::<f32>();
                if (n - 1.0).abs() > 1e-4 || q[3] < -1e-6 {
                    return Err(format!("rotation key {k:?} -> {q:?}"));
                }
            }
            for &k in &a.pos_keys[pos..pos + np] {
                let p = decode_position(k, t.pos_scale);
                let m = p.iter().fold(0f32, |m, v| m.max(v.abs()));
                if m > t.pos_scale * 1.0001 {
                    return Err(format!("position key {k:?} exceeds pos_scale {}", t.pos_scale));
                }
            }
            rot += nr;
            pos += np;
            time += nt;
        }
    }
    Ok(())
}

fn check_sound(pkg: &Package, s: &grim_assets::Sound) -> Result<(), String> {
    if (s.flags & SOUND_HAS_TRAILER != 0) != s.trailer.is_some() {
        return Err("trailer presence does not match flags".into());
    }
    match pkg.name(s.format) {
        "XA" => {
            if s.sample_rate == 0 || (s.duration - s.sample_count as f32 / s.sample_rate as f32).abs() > 1e-4 {
                return Err(format!("duration {} vs {} samples @ {}", s.duration, s.sample_count, s.sample_rate));
            }
            if s.data.len() != (s.sample_count as usize).div_ceil(28) * 15 {
                return Err(format!("{} bytes for {} samples", s.data.len(), s.sample_count));
            }
            let pcm = grim_assets::sound::decode_xa(s).map_err(|e| e.to_string())?;
            if pcm.len() != s.sample_count as usize {
                return Err(format!("decoded {} of {} samples", pcm.len(), s.sample_count));
            }
        }
        "wav" => {
            if !s.data.starts_with(b"RIFF") || s.sample_rate != 0 {
                return Err("wav sound without RIFF header or with XA fields".into());
            }
        }
        other => return Err(format!("unknown sound format {other}")),
    }
    Ok(())
}

fn check_skeletal(m: &grim_assets::SkeletalMesh) -> Result<(), String> {
    if m.bone_weight_idx.len() != m.ref_skeleton.len() {
        return Err(format!("{} weight ranges for {} bones", m.bone_weight_idx.len(), m.ref_skeleton.len()));
    }
    if m.local_points.len() != m.bone_weights.len() {
        return Err(format!("{} local points for {} weights", m.local_points.len(), m.bone_weights.len()));
    }
    for w in &m.bone_weight_idx {
        if w.weight_index as usize + w.number as usize > m.bone_weights.len() {
            return Err(format!("weight range {w:?} out of bounds"));
        }
    }
    if m.bone_weights.iter().any(|w| w.point_index as usize >= m.points.len()) {
        return Err("weight references a missing point".into());
    }
    Ok(())
}
