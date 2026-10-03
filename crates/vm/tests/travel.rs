//! What the player carries from one level to the next, run over the game's own maps.

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

/// Every slot of the spell book that holds a spell, as (slot, spell class).
fn book(vm: &mut Vm, player: ObjectKey) -> Vec<(u32, ObjectKey)> {
    (0..32)
        .filter_map(|i| match vm.get_prop_at(player, "SpellBook", i) {
            Ok(Value::Object(Some(c))) => Some((i, c)),
            _ => None,
        })
        .collect()
}

fn object(vm: &mut Vm, obj: ObjectKey, prop: &str) -> Option<ObjectKey> {
    match vm.get_prop(obj, prop) {
        Ok(Value::Object(o)) => o,
        _ => None,
    }
}

#[test]
fn the_player_carries_its_spell_book_and_its_wand_to_the_next_level() {
    let root = grim_testkit::require_game_dir();
    let mut vm = Vm::new(linked_world(&root));
    vm.apply_config_defaults();
    let mut report = Report::default();

    let level = vm.load_level(&map(&root, "Entryhall_hub")).unwrap();
    vm.begin_play(&level, &mut report);
    let player = vm.player.expect("the level has a player");
    vm.travel_post_accept(&mut report);

    // Something worth carrying, and more than the level hands out by itself: spells go in the
    // book by name, the way the game's own code puts them there.
    let from_level = book(&mut vm, player);
    for spell in ["Flipendo", "Lumos", "Alohomora", "Skurge", "Rictusempra", "Diffindo", "Spongify"] {
        vm.call_event(player, "AddToSpellBookByString", vec![Value::Str(spell.to_string())]).unwrap();
    }
    let spells = book(&mut vm, player);
    assert!(spells.len() > from_level.len(), "the spells did not reach the book: {spells:?}");
    let wand = object(&mut vm, player, "Weapon").expect("the player arrives with a wand");
    let wand_class = vm.class_of(wand).unwrap();
    let health = vm.get_prop(player, "Health").unwrap();

    let carried = vm.travel_properties();
    vm.reset_heap();
    let level = vm.load_level(&map(&root, "Grandstaircase_hub")).unwrap();
    vm.begin_play(&level, &mut report);
    vm.restore_travel_properties(carried, &mut report);
    vm.travel_post_accept(&mut report);

    let player = vm.player.expect("the next level has a player too");
    assert_eq!(book(&mut vm, player), spells, "the spell book did not travel whole");
    assert_eq!(vm.get_prop(player, "Health").unwrap(), health);
    let wand = object(&mut vm, player, "Weapon").expect("the wand did not travel");
    assert_eq!(vm.class_of(wand).unwrap(), wand_class);
    // `Inventory.TravelPreAccept` gives the item back to its owner, which links the chain again.
    assert_eq!(object(&mut vm, player, "Inventory"), Some(wand), "the wand is not in the inventory");
    assert_eq!(object(&mut vm, wand, "Owner"), Some(player));
}

/// What a level keeps between visits: a persistent actor that was destroyed does not come back,
/// and one that changed is found changed.
#[test]
fn a_level_is_found_the_way_it_was_left() {
    let root = grim_testkit::require_game_dir();
    let mut vm = Vm::new(linked_world(&root));
    vm.apply_config_defaults();
    let mut report = Report::default();
    let path = map(&root, "Entryhall_hub");

    let level = vm.load_level(&path).unwrap();
    vm.begin_play(&level, &mut report);
    let before = vm.persistent_actors();
    assert!(before.actors.len() > 10, "the level has hardly any persistent actors: {}", before.actors.len());

    // Collect a jellybean and find a secret area, the two things a visit leaves behind.
    let bean = before.actors.iter().find(|a| a.class.to_ascii_lowercase().contains("bean")).expect("a jellybean").name.clone();
    let marker = before
        .actors
        .iter()
        .find(|a| a.class.eq_ignore_ascii_case("SecretAreaMarker"))
        .expect("a secret area marker")
        .name
        .clone();
    let find = |vm: &mut Vm, name: &str| {
        let keys: Vec<ObjectKey> = vm.actors.clone();
        keys.into_iter().find(|&k| vm.object_name(k).eq_ignore_ascii_case(name)).unwrap()
    };
    let bean_key = find(&mut vm, &bean);
    vm.call_event(bean_key, "Destroy", vec![]).unwrap();
    assert!(vm.is_deleted(bean_key), "the jellybean was not destroyed");
    let marker_key = find(&mut vm, &marker);
    vm.set_prop(marker_key, "bFound", Value::Bool(true)).unwrap();

    let leaving = vm.persistent_actors();
    assert_eq!(leaving.actors.len(), before.actors.len() - 1, "the collected jellybean is still in the stream");
    vm.remember_level(leaving);

    // Come back to it.
    vm.reset_heap();
    let level = vm.load_level(&path).unwrap();
    vm.restore_persistent_actors(&level, &mut report);
    vm.begin_play(&level, &mut report);

    let bean_key = find(&mut vm, &bean);
    assert!(vm.is_deleted(bean_key), "the jellybean came back");
    let marker_key = find(&mut vm, &marker);
    assert_eq!(vm.get_prop(marker_key, "bFound").unwrap(), Value::Bool(true), "the secret area was forgotten");
    assert_eq!(vm.persistent_actors().actors.len(), before.actors.len() - 1);
}
