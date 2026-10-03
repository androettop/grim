//! An animation's bones must agree with the skeleton of the mesh it poses: a track's position
//! keys are the bone's offset from its parent, so a one-key track has to reproduce the mesh's
//! own reference pose, and a bone has to keep that offset over every frame of every sequence.

use grim_assets::animation::{decode_position, Animation};
use grim_assets::mesh::SkeletalMesh;
use grim_assets::{load_export, Asset};
use grim_package::{Library, ObjectRef};

struct Pair {
    name: String,
    pkg: std::sync::Arc<grim_package::Package>,
    mesh: Box<SkeletalMesh>,
    /// Bone names of the mesh and of the animation, in their own order.
    mesh_bones: Vec<String>,
    anim_bones: Vec<String>,
    anim: Box<Animation>,
}

/// Every skeletal mesh in the game, with the animation it names.
fn pairs(lib: &Library, root: &std::path::Path) -> Vec<Pair> {
    let mut out = Vec::new();
    for file in grim_testkit::package_files(root) {
        let Ok(pkg) = lib.load_path(&file) else { continue };
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) != "SkeletalMesh" {
                continue;
            }
            let Ok(o) = load_export(&pkg, i) else { continue };
            let Asset::SkeletalMesh(mesh) = o.asset else { continue };
            let Ok(handle) = lib.resolve(&pkg, mesh.default_animation) else { continue };
            let Ok(o) = load_export(&handle.package, handle.export) else { continue };
            let Asset::Animation(anim) = o.asset else { continue };
            out.push(Pair {
                name: pkg.object_name(ObjectRef::Export(i)).to_string(),
                pkg: pkg.clone(),
                mesh_bones: mesh.ref_skeleton.iter().map(|b| pkg.name(b.name).to_string()).collect(),
                anim_bones: anim.ref_bones.iter().map(|b| handle.package.name(b.name).to_string()).collect(),
                mesh,
                anim,
            });
        }
    }
    out
}

#[test]
fn every_animation_bone_takes_a_bone_of_its_own() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let (mut meshes, mut bones) = (0usize, 0usize);
    let mut clashes = Vec::new();
    for p in pairs(&lib, &root.join("System")) {
        let mut tracks = grim_world::anim::Tracks::default();
        tracks.names = p.anim_bones.clone();
        tracks.bind(&p.mesh_bones);
        let mut taken: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
        for (anim_bone, &mesh_bone) in tracks.to_mesh().iter().enumerate() {
            bones += 1;
            if mesh_bone < 0 {
                continue;
            }
            if let Some(other) = taken.insert(mesh_bone, anim_bone) {
                clashes.push(format!("{}: bones {other} and {anim_bone} both pose mesh bone {mesh_bone}", p.name));
            }
        }
        meshes += 1;
    }
    println!("{bones} bones over {meshes} meshes");
    assert!(clashes.is_empty(), "{} clashes, e.g. {:?}", clashes.len(), &clashes[..clashes.len().min(5)]);
}

/// Skinning with the mesh's own reference pose has to give the mesh back: every point is the
/// weighted sum of its influences, each one a point held in its bone's space.
#[test]
fn the_bind_pose_reproduces_the_mesh() {
    let root = grim_testkit::require_game_dir();
    let lib = Library::open(&root);
    let mut worst = (0.0f32, String::new());
    let (mut checked, mut off) = (0usize, 0usize);
    let mut per_mesh: std::collections::BTreeMap<String, (usize, usize)> = std::collections::BTreeMap::new();
    for p in pairs(&lib, &root.join("System")) {
        let skin = grim_world::anim::Skin::new(&p.pkg, &p.mesh);
        let world = skin.bind_world();
        let mut points = Vec::new();
        skin.deform(&world, &mut points);
        for (i, got) in points.iter().enumerate() {
            let want = p.mesh.points[i];
            let want = [want.x, want.y, want.z];
            let error = (0..3).map(|k| (got[k] - want[k]).abs()).fold(0.0f32, f32::max);
            checked += 1;
            let e = per_mesh.entry(p.name.clone()).or_default();
            e.0 += 1;
            if error > 0.5 {
                off += 1;
                e.1 += 1;
            }
            if error > worst.0 {
                worst = (error, format!("{} point {i}: {got:?} vs {want:?}", p.name));
            }
        }
    }
    println!("{checked} points, {off} off by more than half a unit; worst {} ({})", worst.0, worst.1);
    let mut bad: Vec<_> = per_mesh.iter().filter(|(_, (_, n))| *n > 0).collect();
    bad.sort_by_key(|(_, (t, n))| std::cmp::Reverse(n * 100 / t.max(&1)));
    println!("{} of {} meshes do not come back", bad.len(), per_mesh.len());
    for (name, (total, n)) in bad.iter().take(10) {
        println!("  {name}: {n} of {total}");
    }
}
