//! What the player picks up by walking into it, run over the game's own maps.

use std::path::{Path, PathBuf};

use grim_object::{ObjectKey, Value, World};
use grim_package::{Library, ObjectRef};
use grim_vm::level::Report;
use grim_vm::Vm;

fn linked_world(root: &Path) -> World {
    let mut world = World::new(Library::open(root));
    for f in grim_testkit::package_files(root) {
        let pkg = world.lib.load_path(&f).unwrap();
        for i in 0..pkg.exports.len() as u32 {
            if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                world.link_class(key).unwrap();
            }
        }
    }
    world
}

fn map(root: &Path, name: &str) -> PathBuf {
    grim_testkit::package_files(&root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(name)))
        .unwrap_or_else(|| panic!("map {name} not found"))
}

/// A jellybean is picked up by walking into it: `HProp.Touch` asks `CanPickupNow` and sends the
/// prop to its `PickupProp` state, which is what takes it off the floor and counts it.
#[test]
fn walking_into_a_jellybean_picks_it_up() {
    let root = grim_testkit::require_game_dir();
    let mut vm = Vm::new(linked_world(&root));
    vm.apply_config_defaults();
    let mut report = Report::default();

    let level = vm.load_level(&map(&root, "Entryhall_hub")).unwrap();
    vm.begin_play(&level, &mut report);
    let player = vm.player.expect("the level has a player");

    // A bean that is standing about, not one a chest still holds.
    let mut beans: Vec<ObjectKey> = Vec::new();
    for a in vm.actors.clone() {
        if vm.is_deleted(a) {
            continue;
        }
        let named = vm
            .class_of(a)
            .map(|c| vm.world.names.str(vm.world.class(c).name).to_lowercase().contains("jellybean"))
            .unwrap_or(false);
        if named {
            beans.push(a);
        }
    }
    assert!(!beans.is_empty(), "the level has no jellybeans");
    let bean = beans[0];
    let at = grim_vm::vm::floats3(&vm.get_prop(bean, "Location").unwrap()).unwrap();
    let before = match vm.call_event(player, "JellyBeansCount", vec![]) {
        Ok(Some(Value::Int(n))) => n,
        other => panic!("the player cannot count its beans: {other:?}"),
    };

    // Walk into it from a step away, standing on the same floor: the player's feet where the
    // bean's are. Dropping the player onto the bean's own middle would prove nothing — what the
    // game does is walk into it, and a prop blocks the player, so they never overlap.
    let float_of = |vm: &mut Vm, a: ObjectKey, name: &str| match vm.get_prop(a, name) {
        Ok(Value::Float(v)) => v,
        _ => 0.0,
    };
    let feet = at[2] - float_of(&mut vm, bean, "CollisionHeight");
    let z = feet + float_of(&mut vm, player, "CollisionHeight");
    let away = float_of(&mut vm, bean, "CollisionRadius") + float_of(&mut vm, player, "CollisionRadius") + 40.0;
    vm.set_prop(player, "Location", grim_vm::vm::vector(at[0] - away, at[1], z)).unwrap();
    // Walking is driven by `Acceleration`, which is what the player's input sets.
    let speed = float_of(&mut vm, player, "AccelRate").max(1.0);
    for _ in 0..60 {
        if vm.is_deleted(bean) {
            break;
        }
        vm.set_prop(player, "Acceleration", grim_vm::vm::vector(speed, 0.0, 0.0)).unwrap();
        vm.tick(1.0 / 30.0, &mut report);
    }

    // Picking one up takes it off the floor for good: it flies to the HUD and destroys itself.
    let state = vm.state_name(bean);
    let after = match vm.call_event(player, "JellyBeansCount", vec![]) {
        Ok(Some(Value::Int(n))) => n,
        other => panic!("the player cannot count its beans: {other:?}"),
    };
    assert!(vm.is_deleted(bean), "the jellybean is still on the floor: state {state:?}, beans {before} -> {after}");
}

/// A bean a chest throws out has to come to rest on the floor: `chestbronze.turnover.generateobject`
/// spawns it with a velocity and `SetPhysics(PHYS_Falling)`, and it bounces (`bBounce`) until it
/// settles. It stays falling, as the original's saves have every bean lying on a floor (going
/// nowhere at under a unit a second), but it has to be lying there, not hanging in the air.
#[test]
fn a_thrown_jellybean_lands_on_the_floor() {
    let root = grim_testkit::require_game_dir();
    let mut vm = Vm::new(linked_world(&root));
    vm.apply_config_defaults();
    let mut report = Report::default();

    let level = vm.load_level(&map(&root, "Entryhall_hub")).unwrap();
    vm.begin_play(&level, &mut report);
    let player = vm.player.expect("the level has a player");
    let from = grim_vm::vm::floats3(&vm.get_prop(player, "Location").unwrap()).unwrap();

    let class = vm.class_named("BlueJellyBean").expect("the game has blue jellybeans");
    let at = grim_vm::vm::vector(from[0], from[1], from[2] + 64.0);
    let bean = grim_vm::natives::engine::spawn(&mut vm, player, class, None, None, Some(at), None)
        .unwrap()
        .expect("the bean was spawned");
    // Thrown up and to one side, as a chest throws them.
    vm.set_prop(bean, "Velocity", grim_vm::vm::vector(120.0, 60.0, 260.0)).unwrap();
    vm.set_prop(bean, "Physics", Value::Byte(2)).unwrap();

    for _ in 0..150 {
        vm.tick(1.0 / 30.0, &mut report);
        if vm.is_deleted(bean) {
            break;
        }
    }
    assert!(!vm.is_deleted(bean), "the bean was taken before it landed");
    let end = grim_vm::vm::floats3(&vm.get_prop(bean, "Location").unwrap()).unwrap();
    let velocity = grim_vm::vm::floats3(&vm.get_prop(bean, "Velocity").unwrap()).unwrap();
    let speed = (velocity[0] * velocity[0] + velocity[1] * velocity[1] + velocity[2] * velocity[2]).sqrt();
    // Settled: what is left of the bounce is a hair, and after a frame more it is where it was.
    vm.tick(1.0 / 30.0, &mut report);
    let later = grim_vm::vm::floats3(&vm.get_prop(bean, "Location").unwrap()).unwrap();
    let drift = (0..3).map(|i| (later[i] - end[i]).abs()).fold(0.0f32, f32::max);
    assert!(drift < 1.0 && speed < 30.0, "the bean is still on its way after five seconds, at {end:?} going {speed}");
    // It has to come to rest on something, not hang in mid air: the floor it was thrown from is
    // right below, so it may not end up above where it started.
    assert!(
        end[2] <= from[2] + 8.0,
        "the bean settled in the air at {end:?}, above the floor it was thrown from ({from:?})"
    );
}
