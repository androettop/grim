//! Validation over the real game files. Run with `cargo test --release`.

use grim_object::{ObjectKey, Value, World};
use grim_package::property::{PropertyValue, StructValue};
use grim_package::{Library, ObjectRef};

const DEFINITION_CLASSES: &[&str] = &[
    "Class", "Function", "State", "Struct", "Enum", "Const", "TextBuffer", "ByteProperty",
    "IntProperty", "BoolProperty", "FloatProperty", "ObjectProperty", "ClassProperty",
    "NameProperty", "StrProperty", "StructProperty", "ArrayProperty",
];

fn floats(v: &Value) -> Vec<f32> {
    match v {
        Value::Struct(f) => f.iter().flat_map(floats).collect(),
        Value::Float(x) => vec![*x],
        Value::Int(x) => vec![*x as f32],
        Value::Byte(x) => vec![*x as f32],
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn link_every_class_and_instantiate_every_object() {
    let root = grim_testkit::require_game_dir();
    let mut world = World::new(Library::open(&root));
    let mut failures = Vec::new();
    let (mut objects, mut cross_checked) = (0usize, 0usize);
    for f in grim_testkit::package_files(&root) {
        let pkg = world.lib.load_path(&f).unwrap();
        for i in 0..pkg.exports.len() as u32 {
            let class = pkg.class_name(ObjectRef::Export(i)).to_string();
            if class == "Class" {
                let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                if let Err(e) = world.link_class(key) {
                    failures.push(e.0);
                }
                continue;
            }
            if DEFINITION_CLASSES.contains(&class.as_str()) {
                continue;
            }
            objects += 1;
            let o = match world.instantiate(&pkg, i) {
                Ok(o) => o,
                Err(e) => {
                    failures.push(e.0);
                    continue;
                }
            };
            // The generic struct decoder must agree with the hand-written ones of grim-package.
            let loaded = grim_assets::load_export(&pkg, i).unwrap();
            let def = world.classes[o.class.0 as usize].clone();
            for tag in loaded.base.properties.iter().flatten() {
                let PropertyValue::Struct { value, .. } = &tag.value else { continue };
                let want: Vec<f32> = match value {
                    StructValue::Vector(v) => vec![v.x, v.y, v.z],
                    StructValue::Rotator(r) => vec![r.pitch as f32, r.yaw as f32, r.roll as f32],
                    StructValue::Color(c) => vec![c.r as f32, c.g as f32, c.b as f32, c.a as f32],
                    StructValue::Plane(p) => vec![p.x, p.y, p.z, p.w],
                    StructValue::Scale(s) => vec![s.scale.x, s.scale.y, s.scale.z, s.sheer_rate, s.sheer_axis as f32],
                    _ => continue,
                };
                let name = world.names.get(pkg.name(tag.name)).unwrap();
                let p = def.prop(name).unwrap();
                let got = floats(&o.values[(p.slot + tag.array_index) as usize]);
                // Bitwise: some saved vectors hold uninitialized data, NaN included.
                let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
                if bits(&got) != bits(&want) {
                    failures.push(format!("{}: {} generic {got:?} vs specific {want:?}", world.path(o.key), pkg.name(tag.name)));
                }
                cross_checked += 1;
            }
        }
    }
    eprintln!("{objects} objects, {cross_checked} struct values cross-checked");
    assert!(objects > 100_000 && cross_checked > 10_000, "the game files look incomplete");
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures[..failures.len().min(30)].join("\n"));
}
