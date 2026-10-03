//! The game itself: a level run in the VM, its input, its sound and its drawing.
//!
//! `grim-play <map>`: opens a window, runs the level in the VM and renders it.
//! With `--shot <file.png>` it renders a single frame without a window instead.
//!
//! With `--video <file.mp4> --seconds <n>` it plays without a window and records what it draws.
//! `--size <W>x<H>` opens the window at that size and keeps it there, which a tiling window
//! manager takes as a window to float rather than to fill the screen with.
//!
//! Controls: WASD to move, mouse to look, Shift to run, Tab to release the mouse, Escape for
//! the menu. With debug on, Delete switches to a free camera and pauses the game; leaving it
//! moves the player to the camera.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use grim_fs::FileSystem;

use grim_object::{ObjectKey, Value, World};
use grim_package::{Library, ObjectRef};
use grim_render::{Camera, DrawItem, MeshId, Renderer, Scene};
use std::collections::HashMap;
use grim_render_wgpu::WgpuRenderer;
use grim_vm::level::{LoadedLevel, Report};
use grim_vm::Vm;
use winit::application::ApplicationHandler;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorGrabMode, Window, WindowId};

/// Unreal's light colour: hue around the wheel, saturation 255 meaning white.
fn light_colour(hue: u8, saturation: u8) -> [f32; 3] {
    let h = hue as f32 / 255.0 * 6.0;
    let sector = h.floor() as i32;
    let f = h - sector as f32;
    let (r, g, b) = match sector.rem_euclid(6) {
        0 => (1.0, f, 0.0),
        1 => (1.0 - f, 1.0, 0.0),
        2 => (0.0, 1.0, f),
        3 => (0.0, 1.0 - f, 1.0),
        4 => (f, 0.0, 1.0),
        _ => (1.0, 0.0, 1.0 - f),
    };
    let s = saturation as f32 / 255.0;
    // The ramp from one corner of the wheel to the next is taken in the light's own scale,
    // not in the one the picture is shown in, which is what squaring it comes to. Measured
    // against a frame of the original in `EntryHall_hub`: the lit stone comes out with 0.603
    // of green to red against the original's 0.607, where the straight ramp gives 0.677 and
    // the whole hall turns yellow. The other roads that could have done it were ruled out by
    // the same measure: how the light that lands on a texel is brought back into range (it
    // never reaches the top, so it changes nothing), how it falls with distance (that moves
    // how bright it is, not its colour) and how much a lightmap brightens.
    let [r, g, b] = [r, g, b].map(|c| c * c);
    [r, g, b].map(|c| c + (1.0 - c) * s)
}

/// What the game's own bindings call a mouse button.
fn button_name(button: MouseButton) -> Option<&'static str> {
    Some(match button {
        MouseButton::Left => "LeftMouse",
        MouseButton::Right => "RightMouse",
        MouseButton::Middle => "MiddleMouse",
        _ => return None,
    })
}

/// The name `EInputKey` gives a key, without its `IK_`: what the bindings and the console go
/// by. Escape is left out: it reaches the console through the menu, which is how the game
/// opens it, and sending it twice would close it again.
fn key_name(key: KeyCode) -> Option<&'static str> {
    Some(match key {
        KeyCode::KeyA => "A",
        KeyCode::KeyB => "B",
        KeyCode::KeyC => "C",
        KeyCode::KeyD => "D",
        KeyCode::KeyE => "E",
        KeyCode::KeyF => "F",
        KeyCode::KeyG => "G",
        KeyCode::KeyH => "H",
        KeyCode::KeyI => "I",
        KeyCode::KeyJ => "J",
        KeyCode::KeyK => "K",
        KeyCode::KeyL => "L",
        KeyCode::KeyM => "M",
        KeyCode::KeyN => "N",
        KeyCode::KeyO => "O",
        KeyCode::KeyP => "P",
        KeyCode::KeyQ => "Q",
        KeyCode::KeyR => "R",
        KeyCode::KeyS => "S",
        KeyCode::KeyT => "T",
        KeyCode::KeyU => "U",
        KeyCode::KeyV => "V",
        KeyCode::KeyW => "W",
        KeyCode::KeyX => "X",
        KeyCode::KeyY => "Y",
        KeyCode::KeyZ => "Z",
        KeyCode::Digit0 => "0",
        KeyCode::Digit1 => "1",
        KeyCode::Digit2 => "2",
        KeyCode::Digit3 => "3",
        KeyCode::Digit4 => "4",
        KeyCode::Digit5 => "5",
        KeyCode::Digit6 => "6",
        KeyCode::Digit7 => "7",
        KeyCode::Digit8 => "8",
        KeyCode::Digit9 => "9",
        KeyCode::F1 => "F1",
        KeyCode::F2 => "F2",
        KeyCode::F3 => "F3",
        KeyCode::F4 => "F4",
        KeyCode::F5 => "F5",
        KeyCode::F6 => "F6",
        KeyCode::F7 => "F7",
        KeyCode::F8 => "F8",
        KeyCode::F9 => "F9",
        KeyCode::F10 => "F10",
        KeyCode::F11 => "F11",
        KeyCode::F12 => "F12",
        KeyCode::ArrowUp => "Up",
        KeyCode::ArrowDown => "Down",
        KeyCode::ArrowLeft => "Left",
        KeyCode::ArrowRight => "Right",
        KeyCode::Space => "SpaceBar",
        KeyCode::ShiftLeft | KeyCode::ShiftRight => "Shift",
        KeyCode::ControlLeft | KeyCode::ControlRight => "Ctrl",
        KeyCode::AltLeft | KeyCode::AltRight => "Alt",
        KeyCode::Enter | KeyCode::NumpadEnter => "Enter",
        KeyCode::Tab => "Tab",
        KeyCode::Backspace => "Backspace",
        KeyCode::Home => "Home",
        KeyCode::End => "End",
        KeyCode::PageUp => "PageUp",
        KeyCode::PageDown => "PageDown",
        KeyCode::Insert => "Insert",
        // The key left of the 1 (º on a Spanish keyboard): the console's `ConsoleKey`, 192.
        KeyCode::Backquote => "Tilde",
        _ => return None,
    })
}

/// `EInputKey` / `EInputAction`, as the scripts know them.
const IK_ESCAPE: u8 = 0x1B;
const IST_PRESS: u8 = 1;
const IST_RELEASE: u8 = 2;

/// `EDrawType`: what an actor draws itself as.
const DT_SPRITE: u8 = 1;
const DT_MESH: u8 = 2;
const DT_BRUSH: u8 = 3;

const WALK_SPEED: f32 = 600.0;
const MOUSE_SENSITIVITY: f32 = 12.0;
const MAX_PITCH: i32 = 16000;

/// An actor playing a skeletal animation: its own mesh, the skin that moves it and the
/// animation's keys.
struct Animated {
    mesh: MeshId,
    posed: grim_world::scene::PosedMesh,
    points: Vec<[f32; 3]>,
    /// What was posed last, to skip the work when nothing moved.
    last: Option<Vec<Option<grim_world::anim::Joint>>>,
}

/// Where a class keeps the properties the drawing reads first, looked up once per class.
#[derive(Clone, Copy)]
struct DrawSlots {
    hidden: Option<usize>,
    draw_type: Option<usize>,
    brush: Option<usize>,
    mesh: Option<usize>,
    location: Option<usize>,
    draw_scale: Option<usize>,
}

/// What those slots hold for one actor.
struct FirstLook {
    hidden: bool,
    draw_type: u8,
    brush: Option<ObjectKey>,
    mesh: Option<ObjectKey>,
    location: [f32; 3],
    draw_scale: f32,
}

/// One actor's draw data; `skins` must outlive the scene that borrows it.
struct ActorDraw {
    /// Which actor it draws, for the reports.
    actor: Option<ObjectKey>,
    /// A ball around what it draws, in world coordinates, for leaving it out when it is behind
    /// the camera or off to the side.
    centre: [f32; 3],
    radius: f32,
    mesh: MeshId,
    /// Baked light of a brush built from its own BSP; a mesh has none.
    lightmap: Option<grim_render::TextureId>,
    transform: grim_render::math::Mat4,
    alpha_test: Option<bool>,
    skins: Vec<Option<grim_render::TextureId>>,
    blend: Option<grim_render::Blend>,
    tint: [f32; 3],
    lights: [grim_render::MeshLight; grim_render::MESH_LIGHTS],
    env_map: bool,
}

pub struct Game {
    vm: Vm,
    level: LoadedLevel,
    report: Report,
    camera: Camera,
    keys: HashSet<KeyCode>,
    /// Mouse buttons held, by the name the bindings give them.
    buttons: HashSet<&'static str>,
    look: (f32, f32),
    grabbed: bool,
    window: Option<Arc<Window>>,
    renderer: Option<WgpuRenderer>,
    /// One mesh per zone of the level, with the ball it fills, and what each leaf can see.
    zone_meshes: Vec<(u8, MeshId, [f32; 3], f32)>,
    /// What posing the animated meshes took this frame, and how many were skinned again.
    skinning: (f32, u32),
    /// The surfaces Lumos brings out, drawn only while the player has it lit.
    lumos_mesh: Option<MeshId>,
    zone_visibility: Vec<u64>,
    /// The surfaces the sky shows through, drawn into the depth buffer only.
    backdrop_mesh: Option<MeshId>,
    /// Mirror surfaces with the plane each one reflects about.
    mirrors: Vec<(MeshId, [f32; 4])>,
    /// The `ScriptedTexture`s of the level, found once it has begun play.
    scripted_textures: Option<Vec<ObjectKey>>,
    /// The actors that carry a brush of their own (the movers), found once per level: which
    /// they are never changes, and looking for them among every actor twice a frame did.
    brush_actors: Option<Vec<ObjectKey>>,
    /// Ambient light of each zone, for the meshes standing in it.
    zone_ambients: grim_object::FastMap<u8, [f32; 3]>,
    /// Baked light of the level's surfaces, and the lights that made it.
    lightmap: Option<grim_render::TextureId>,
    /// The level's backdrop, and the actor it is seen from.
    sky_mesh: Option<MeshId>,
    lights: Vec<grim_world::lightmap::LightSample>,
    /// Ambient light of the level, for the meshes standing in it.
    ambient: [f32; 3],
    /// How many lights a surface takes, from the zone's `MaxLightCount`.
    light_count: usize,
    /// Fly around instead of playing, for looking at a map. While it is on the game is paused,
    /// and leaving it moves the player to where the camera ended up.
    free_camera: bool,
    /// Debug builds of the engine allow the free camera and the other inspection keys.
    debug: bool,
    /// The size the window asks for, in pixels, from `--size WxH`.
    window_size: Option<(u32, u32)>,
    /// Draw the shapes the collision uses over the scene.
    show_collision: bool,
    collision_mesh: Option<MeshId>,
    /// The client's movement and view axis scales, from the ini.
    /// Size of what is being drawn into, which the canvas reports to the scripts.
    viewport: (u32, u32),
    gamma: f32,
    /// Mesh per mesh object, and the actors that draw it.
    actor_meshes: grim_object::FastMap<ObjectKey, Option<MeshId>>,
    /// Actors that move their own copy of a mesh, with what it takes to pose it.
    animated: grim_object::FastMap<ObjectKey, Animated>,
    /// Mesh per brush of a `Mover`, keyed by the brush and its pivot.
    brush_meshes: grim_object::FastMap<ObjectKey, Option<MeshId>>,
    /// Middle of each brush in its own space, where it takes its light.
    brush_centres: grim_object::FastMap<ObjectKey, [f32; 3]>,
    /// Light worked out for each actor, and where it was standing at the time.
    tints: grim_object::FastMap<ObjectKey, ([f32; 3], [f32; 3], [grim_render::MeshLight; grim_render::MESH_LIGHTS])>,
    /// Material index of each section, and whether it came with a texture.
    mesh_sections: grim_object::FastMap<MeshId, Vec<u32>>,
    /// Mirror surfaces each brush carries, with the plane each one reflects about in the
    /// brush's own space.
    brush_mirrors: grim_object::FastMap<ObjectKey, Vec<(MeshId, [f32; 4])>>,
    /// How long the last frame took, for what the engine animates itself.
    frame_time: f32,
    /// Frame time smoothed over the last frames, for the debug overlay's reading to hold still.
    frame_average: f32,
    /// How many of the level's zones the last frame drew.
    zones_drawn: usize,
    /// What the last tick of the game itself took, in milliseconds.
    tick_cost: f32,
    /// What the last frame spent getting the scene together and drawing it, in milliseconds,
    /// with how many things went into it. The debug overlay shows them.
    frame_costs: (f32, f32, usize, usize),
    /// How big a ball each actor needs around it, from the mesh it was drawn with last time.
    draw_radii: grim_object::FastMap<ObjectKey, f32>,
    /// Where each class keeps what the drawing reads first.
    draw_slots: grim_object::FastMap<grim_object::ClassId, DrawSlots>,
    /// `GRIM_NO_CULL=1`: hands the renderer everything, to tell a drawing fault from one of
    /// leaving out what the camera cannot see.
    draw_everything: bool,
    /// How many meshes and sprites the last frame left out for being off screen, and how many
    /// The planes the mirrors of this frame reflect about, worked out before anything is
    /// drawn: what only a mirror shows must survive the culling.
    frame_mirrors: Vec<[f32; 4]>,
    /// mirror passes it drew and left out.
    frame_culled: (usize, usize, usize, usize),
    /// Lightmap of each brush built from its own BSP, which lights it like the level.
    brush_lightmaps: grim_object::FastMap<MeshId, grim_render::TextureId>,
    /// Where the centre of each static mesh's box sits in its own space, which is what the
    /// drawing puts at the actor's location.
    mesh_centres: grim_object::FastMap<MeshId, [f32; 3]>,
    /// How far each mesh reaches below the middle it was centred on.
    mesh_lows: grim_object::FastMap<MeshId, f32>,
    section_has_texture: grim_object::FastMap<MeshId, Vec<bool>>,
    last_frame: web_time::Instant,
    /// Where the browser leaves the renderer once it has made one.
    #[cfg(target_arch = "wasm32")]
    pending_renderer: std::rc::Rc<std::cell::RefCell<Option<Result<WgpuRenderer, String>>>>,
    map: String,
    textures: grim_world::scene::TextureCache,
}

const PACKAGE_MAGIC: [u8; 4] = [0xC1, 0x83, 0x2A, 0x9E];

/// Every Unreal package under `dir`, told by its first bytes rather than its extension, sorted.
fn package_files(fs: &dyn FileSystem, dir: &Path) -> Vec<PathBuf> {
    fs.files_under(dir).into_iter().filter(|p| fs.read_head(p, 4).is_ok_and(|h| h == PACKAGE_MAGIC)).collect()
}

/// The package of a map, by its name.
fn find_map(fs: &dyn FileSystem, root: &Path, map: &str) -> Result<PathBuf, String> {
    package_files(fs, &root.join("Maps"))
        .into_iter()
        .find(|p| p.file_stem().is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case(map)))
        .ok_or_else(|| format!("map {map} not found"))
}

/// Reads the persistent actors a save slot keeps for the levels its game has visited.
fn take_in_slot(vm: &mut Vm, dir: &Path) {
    let fs = vm.world.lib.fs().clone();
    let Ok(entries) = fs.read_dir(dir) else { return };
    let mut files: Vec<PathBuf> = entries
        .into_iter()
        .map(|e| e.path)
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().to_ascii_lowercase().ends_with("_pa.usa")))
        .collect();
    files.sort();
    for f in files {
        let Ok(bytes) = fs.read(&f) else { continue };
        match grim_save::read(&bytes, &vm.world) {
            Ok(level) => {
                println!("slot: {} actors kept for {}", level.actors.len(), level.name);
                vm.remember_level(level);
            }
            Err(e) => eprintln!("{}: {e}", f.display()),
        }
    }
}

impl Game {
    pub fn load(fs: Arc<dyn FileSystem>, root: &Path, map: &str, audio: Option<Box<dyn grim_audio::Audio>>) -> Result<Self, String> {
        // Unreal's render devices brighten the frame with a gamma ramp driven by the client's
        // `Brightness`. The offset is fitted against the game's own frames, where the shipped
        // 0.4 lands at 1.2: that matches their red to green balance across the whole range.
        let gamma_of = |b: f32| 0.8 + b;
        let mut world = World::new(Library::open_in(fs.clone(), root));
        for f in package_files(&*fs, root) {
            let pkg = world.lib.load_path(&f)?;
            for i in 0..pkg.exports.len() as u32 {
                if pkg.class_name(ObjectRef::Export(i)) == "Class" {
                    let key = ObjectKey { package: world.pkg_id(&pkg), export: i };
                    world.link_class(key).map_err(|e| e.0)?;
                }
            }
        }
        // A map by name, or the path of a package: a saved game is a package of its own, with
        // the level it holds inside it.
        let named = Path::new(map);
        let path = match fs.is_file(named) {
            true => named.to_path_buf(),
            false => find_map(&*fs, root, map)?,
        };

        let mut vm = Vm::new(world);
        vm.apply_config_defaults();
        // The sound goes in before the level does: a map that starts its music or an ambient
        // sound as it opens is heard from the first frame.
        vm.audio = audio;
        vm.apply_audio_settings();
        let brightness = vm
            .config
            .ini
            .get("WinDrv.WindowsClient", "Brightness")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.5);
        let gamma = gamma_of(brightness);
        let mut report = Report::default();
        // A save slot holds a `_pa` file per level the saved game has been in. Taking them all in
        // means walking back into one of those levels finds it as that game left it. The level
        // this save opens in needs none: it comes whole in the package.
        if let Some(dir) = path.parent() {
            take_in_slot(&mut vm, dir);
        }
        let level = vm.load_level(&path)?;
        vm.begin_play(&level, &mut report);
        vm.travel_post_accept(&mut report);

        // Start where the player is, or at the first PlayerStart.
        let mut camera = Camera { far: 131_072.0, ..Camera::default() };
        if let Some(p) = vm.player.or_else(|| find_actor(&mut vm, "PlayerStart")) {
            if let Ok(v) = vm.get_prop(p, "Location") {
                if let Ok(loc) = grim_vm::vm::floats3(&v) {
                    camera.position = [loc[0], loc[1], loc[2] + 64.0];
                }
            }
        }
        Ok(Self {
            vm,
            level,
            report,
            camera,
            keys: HashSet::new(),
            buttons: HashSet::new(),
            look: (0.0, 0.0),
            grabbed: false,
            window: None,
            renderer: None,
            zone_meshes: Vec::new(),
            skinning: (0.0, 0),
            lumos_mesh: None,
            zone_visibility: Vec::new(),
            backdrop_mesh: None,
            mirrors: Vec::new(),
            brush_actors: None,
            scripted_textures: None,
            zone_ambients: grim_object::FastMap::default(),
            lightmap: None,
            sky_mesh: None,
            lights: Vec::new(),
            ambient: [0.0; 3],
            light_count: 6,
            free_camera: std::env::var_os("GRIM_FREE_CAMERA").is_some(),
            debug: std::env::var_os("GRIM_NO_DEBUG").is_none(),
            window_size: None,
            show_collision: std::env::var_os("GRIM_SHOW_COLLISION").is_some(),
            collision_mesh: None,
            viewport: (960, 720),
            gamma,
            actor_meshes: grim_object::FastMap::default(),
            animated: grim_object::FastMap::default(),
            brush_meshes: grim_object::FastMap::default(),
            brush_centres: grim_object::FastMap::default(),
            tints: grim_object::FastMap::default(),
            mesh_sections: grim_object::FastMap::default(),
            mesh_centres: grim_object::FastMap::default(),
            mesh_lows: grim_object::FastMap::default(),
            frame_time: 1.0 / 30.0,
            frame_average: 1.0 / 30.0,
            zones_drawn: 0,
            tick_cost: 0.0,
            frame_costs: (0.0, 0.0, 0, 0),
            frame_culled: (0, 0, 0, 0),
            frame_mirrors: Vec::new(),
            draw_everything: std::env::var_os("GRIM_NO_CULL").is_some(),
            draw_radii: grim_object::FastMap::default(),
            draw_slots: grim_object::FastMap::default(),
            brush_lightmaps: grim_object::FastMap::default(),
            brush_mirrors: grim_object::FastMap::default(),
            section_has_texture: grim_object::FastMap::default(),
            last_frame: web_time::Instant::now(),
            #[cfg(target_arch = "wasm32")]
            pending_renderer: Default::default(),
            map: map.to_string(),
            textures: grim_world::scene::TextureCache::new(),
        })
    }

    /// Turns the debug mode, with its overlay and its inspection keys, on or off.
    pub fn set_debug(&mut self, on: bool) {
        self.debug = on;
    }

    /// The map the level asks to change to, if it has. Unreal leaves the next URL on the level
    /// info and the engine takes it from there once the countdown is done.
    fn pending_travel(&mut self) -> Option<String> {
        let info = self.vm.level_info?;
        let url = match self.vm.get_prop(info, "NextURL") {
            Ok(Value::Str(url)) if !url.is_empty() => url,
            _ => return None,
        };
        let left = match self.vm.get_prop(info, "NextSwitchCountdown") {
            Ok(Value::Float(v)) => v,
            _ => 0.0,
        };
        (left <= 0.0).then_some(url)
    }

    /// What the screen shows while a level loads: black, as the original's does, rather than
    /// the last frame of the level being left.
    fn show_black(&mut self) {
        if let Some(r) = self.renderer.as_mut() {
            let scene = Scene { clear_color: [0.0, 0.0, 0.0, 1.0], ..Default::default() };
            if let Err(e) = r.render(&scene) {
                eprintln!("render: {e}");
            }
        }
    }

    /// The frame left on the screen while a level loads. The engine tells the level it is
    /// loading (`LevelAction` 1, `LEVACT_Loading`) and the player to show the loading screen,
    /// and draws once: `baseConsole.DrawLevelAction` then puts up the loading picture and its
    /// text, with nothing of the world behind it. No script switches `bShowLoadingScreen` on,
    /// yet `harry.TravelPostAccept` switches it off once the new level is in ("Turn OFF the
    /// loading screen"), and it travels: hypothesis, the engine sets it when a travel starts.
    fn show_loading(&mut self) {
        if let Some(info) = self.vm.level_info {
            let _ = self.vm.set_prop(info, "LevelAction", Value::Byte(1));
        }
        if let Some(p) = self.vm.player {
            let _ = self.vm.set_prop(p, "bShowLoadingScreen", Value::Bool(true));
        }
        let overlay = self.draw_overlay();
        if let Some(r) = self.renderer.as_mut() {
            let scene = Scene { clear_color: [0.0, 0.0, 0.0, 1.0], overlay, ..Default::default() };
            if let Err(e) = r.render(&scene) {
                eprintln!("render: {e}");
            }
        }
    }

    /// Builds what every actor of the level draws with before its first frame: its mesh, its
    /// skins and its texture. Left to the first time each is drawn, the level fills in piece
    /// by piece in front of the player; the original has it all ready when the screen comes
    /// back from black.
    fn precache(&mut self) {
        for a in self.vm.actors.clone() {
            if let Ok(Value::Object(Some(mesh))) = self.vm.get_prop(a, "Mesh") {
                self.actor_mesh(mesh);
            }
            self.actor_skins(a);
            if let Ok(Value::Object(Some(k))) = self.vm.get_prop(a, "Texture") {
                if let Some(renderer) = self.renderer.as_mut() {
                    let pkg = self.vm.world.package(k.package).clone();
                    self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, k.export));
                }
            }
        }
    }

    /// Changes level: the player keeps what it travels with, everything else is left behind.
    fn travel(&mut self, url: &str) -> Result<(), String> {
        self.show_loading();
        // `Map.unr?options#tag`: only the map is ours to open.
        let map = url.split(['?', '#']).next().unwrap_or(url);
        let map = map.strip_suffix(".unr").unwrap_or(map).to_string();
        // The engine warns the player before it leaves: `harry.PreClientTravel` notes the
        // level being left in `PreviousLevelName`, which `TravelPostAccept` matches against
        // the new level's `SmartStart`s to put the player at the door it came through.
        if let Some(p) = self.vm.player {
            if let Err(e) = self.vm.call_event(p, "PreClientTravel", Vec::new()) {
                self.report.record(e);
            }
        }
        // Nothing the old level was playing goes on in the new one: its music and its sounds
        // stop with it, or every level change piled another track on top.
        if let Some(audio) = self.vm.audio.as_mut() {
            audio.stop_everything();
        }
        let carried = self.vm.travel_properties();
        // The level being left is kept the way it stands, so that what was collected in it stays
        // collected when the player comes back.
        let leaving = self.vm.persistent_actors();
        self.vm.remember_level(leaving);
        let (fs, root) = (self.vm.world.lib.fs().clone(), self.vm.world.lib.root().to_path_buf());
        let path = find_map(&*fs, &root, &map)?;
        self.vm.reset_heap();
        self.report = Report::default();
        self.level = self.vm.load_level(&path)?;
        let mut report = std::mem::take(&mut self.report);
        // Before the level begins play, so that what was destroyed on an earlier visit never
        // starts and what changed is already changed when the scripts first look at it.
        self.vm.restore_persistent_actors(&self.level, &mut report);
        self.vm.begin_play(&self.level, &mut report);
        self.vm.restore_travel_properties(carried, &mut report);
        self.vm.travel_post_accept(&mut report);
        self.report = report;
        self.map = map;

        // Nothing of the old level's drawing is worth keeping; its meshes go with it.
        if let Some(r) = self.renderer.as_mut() {
            r.clear_meshes();
        }
        self.zone_meshes.clear();
        self.lumos_mesh = None;
        self.zone_visibility.clear();
        self.backdrop_mesh = None;
        self.sky_mesh = None;
        self.lightmap = None;
        self.mirrors.clear();
        self.scripted_textures = None;
        self.brush_actors = None;
        self.lights.clear();
        self.actor_meshes.clear();
        self.animated.clear();
        self.brush_meshes.clear();
        self.brush_centres.clear();
        self.brush_lightmaps.clear();
        self.brush_mirrors.clear();
        self.tints.clear();
        self.mesh_sections.clear();
        self.section_has_texture.clear();
        self.build_scene_resources()?;
        self.precache();
        self.update_view();
        println!("travelled to {}", self.map);
        Ok(())
    }

    fn build_scene_resources(&mut self) -> Result<(), String> {
        let pkg = self.vm.world.package(self.level.level.package).clone();
        let (_, model) = grim_world::scene::level_model(&pkg).ok_or("map has no Model")?;
        self.light_count = self.zone_light_count();
        let samples = self.light_samples(&model);
        let ambient = self.zone_ambient(&model);
        self.lights = samples.values().copied().collect();
        // Meshes are not tied to a surface, so they take the level's most common ambient.
        self.ambient = ambient.iter().fold([0.0f32; 3], |a, v| [a[0] + v[0], a[1] + v[1], a[2] + v[2]]).map(|v| v / ambient.len().max(1) as f32);
        let zone_pan = self.zone_pan(&model);
        let pieces = grim_world::lightmap::Pieces::Surfs(&model);
        let baked = grim_world::lightmap::bake(&pieces, &ambient, self.light_count, &|r| match r {
            ObjectRef::Export(e) => samples.get(&e).copied(),
            _ => None,
        });
        if let Some(file) = std::env::var_os("GRIM_DEBUG_LIGHTMAP") {
            // How much light each surface ended up with, brightest first: a surface drawn far
            // brighter than it should be is either lit wrongly or lit by nothing at all, and
            // the two look the same on screen (a piece with no lightmap is drawn fully lit).
            let mut lit: Vec<(f32, usize)> = Vec::new();
            for (surf, place) in baked.placements.iter().enumerate() {
                if !place.valid {
                    if pieces.map(surf).is_some() {
                        let texture = model.surfs.get(surf).map(|s| pkg.path_name(s.texture)).unwrap_or_default();
                        println!("lit surf {surf}: no place in the atlas, so drawn fully lit  {texture}");
                    }
                    continue;
                }
                let mut sum = 0.0;
                for y in place.y..place.y + place.height {
                    for x in place.x..place.x + place.width {
                        let at = ((y * baked.width + x) * 4) as usize;
                        sum += baked.rgba[at..at + 3].iter().map(|&v| v as f32).sum::<f32>() / 3.0;
                    }
                }
                lit.push((sum / (place.width * place.height).max(1) as f32, surf));
            }
            lit.sort_by(|a, b| b.0.total_cmp(&a.0));
            for (mean, surf) in lit.iter().take(12) {
                let texture = model.surfs.get(*surf).map(|s| pkg.path_name(s.texture)).unwrap_or_default();
                println!("lit surf {surf}: {mean:.0}/255  {texture}");
            }
            write_png(file.to_string_lossy().as_ref(), baked.width, baked.height, &baked.rgba)?;
        }
        let Some(renderer) = self.renderer.as_mut() else { return Ok(()) };
        // The lightmap atlas is never minified far enough to want smaller copies, and every
        // surface has its own patch of it: shrinking one would bleed into its neighbours.
        self.lightmap = Some(renderer.create_texture(grim_render::TextureData {
            width: baked.width,
            height: baked.height,
            rgba: &baked.rgba,
            mips: &[],
        }));
        // The sky is its own piece of the level, drawn as the backdrop behind everything.
        let sky_zone = grim_world::scene::sky_zone_of(&pkg, &model);
        // One mesh per zone of the map. The engine draws a zone only when the one the eye is
        // in can see it, which is what the map's own leaves say, so the level goes to the
        // renderer a few rooms at a time instead of whole.
        let zones: std::collections::BTreeSet<u8> = model.nodes.iter().map(|n| n.i_zone[grim_world::FRONT]).collect();
        if std::env::var_os("GRIM_DEBUG_ZONES").is_some() {
            let actors: Vec<String> = model
                .zones
                .iter()
                .map(|z| match z.zone_actor {
                    grim_package::ObjectRef::Export(e) => format!("{}", pkg.path_name(grim_package::ObjectRef::Export(e))),
                    other => format!("{other:?}"),
                })
                .collect();
            println!("sky zone {sky_zone:?}; zones with geometry {zones:?}; zone actors {actors:?}");
        }
        let (mut vertices, mut sections, mut sliding) = (0, 0, 0);
        for zone in zones {
            if Some(zone) == sky_zone {
                continue;
            }
            let part = grim_world::scene::Part::WorldZone { zone, sky: sky_zone };
            let data = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, Some(&baked), part, &zone_pan);
            if data.vertices.is_empty() {
                continue;
            }
            vertices += data.vertices.len();
            sections += data.sections.len();
            sliding += data.sections.iter().filter(|s| s.pan != [0.0; 2]).count();
            // The ball the zone fills, for leaving out the rooms the camera is not looking at.
            let mut low = [f32::MAX; 3];
            let mut high = [f32::MIN; 3];
            for v in &data.vertices {
                for i in 0..3 {
                    low[i] = low[i].min(v.position[i]);
                    high[i] = high[i].max(v.position[i]);
                }
            }
            let centre = [0, 1, 2].map(|i| (low[i] + high[i]) / 2.0);
            let radius = (0..3).map(|i| (high[i] - low[i]) / 2.0).fold(0.0f32, |a, h| a + h * h).sqrt();
            self.zone_meshes.push((zone, renderer.create_mesh(&data), centre, radius));
        }
        let part = grim_world::scene::Part::Lumos { sky: sky_zone };
        let lumos = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, Some(&baked), part, &zone_pan);
        if !lumos.vertices.is_empty() {
            self.lumos_mesh = Some(renderer.create_mesh(&lumos));
        }
        // What each leaf of the tree can see, for the zone the eye is in to say what is drawn.
        self.zone_visibility = model.leaves.iter().map(|l| l.visible_zones).collect();
        println!(
            "level mesh: {vertices} vertices, {sections} sections ({sliding} sliding) in {} zones; {} lights in a {}x{} lightmap",
            self.zone_meshes.len(),
            self.lights.len(),
            baked.width,
            baked.height,
        );
        let found = grim_world::scene::mirror_surfaces(&model);
        if std::env::var_os("GRIM_TRACE_MIRROR").is_some() {
            eprintln!("{} mirror surfaces", found.len());
        }
        for (surf, plane) in found {
            let part = grim_world::scene::Part::Mirror(surf);
            let m = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, Some(&baked), part, &zone_pan);
            if !m.vertices.is_empty() {
                if std::env::var_os("GRIM_TRACE_MIRROR").is_some() {
                    let n = m.vertices.len() as f32;
                    let c = m.vertices.iter().fold([0.0f32; 3], |a, v| [0, 1, 2].map(|i| a[i] + v.position[i] / n));
                    eprintln!("mirror surf {surf} plane {plane:?} centre {:?}", c.map(|x| x.round()));
                }
                self.mirrors.push((renderer.create_mesh(&m), plane));
            }
        }
        let part = grim_world::scene::Part::Backdrop { sky: sky_zone };
        let backdrop = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, Some(&baked), part, &zone_pan);
        if !backdrop.vertices.is_empty() {
            self.backdrop_mesh = Some(renderer.create_mesh(&backdrop));
        }
        if let Some(zone) = sky_zone {
            let part = grim_world::scene::Part::Sky(zone);
            let sky = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, Some(&baked), part, &zone_pan);
            if !sky.vertices.is_empty() {
                self.sky_mesh = Some(renderer.create_mesh(&sky));
            }
        }
        Ok(())
    }

    /// How many lights a zone applies to a surface (`ZoneInfo.MaxLightCount`). The level's own
    /// info holds it for the default zone, which is the one every map here uses.
    fn zone_light_count(&mut self) -> usize {
        let zone = self
            .level
            .actors
            .iter()
            .copied()
            .find(|&a| self.vm.class_of(a).is_ok_and(|c| self.vm.world.names.str(self.vm.world.class(c).name) == "LevelInfo"));
        match zone.map(|z| self.vm.get_prop(z, "MaxLightCount")) {
            Some(Ok(Value::Int(n))) if n > 0 => n as usize,
            _ => 6,
        }
    }

    /// Byte property of an actor, zero when it has none.
    fn byte_prop(&mut self, actor: ObjectKey, name: &str) -> u8 {
        match self.vm.get_prop(actor, name) {
            Ok(Value::Byte(b)) => b,
            _ => 0,
        }
    }

    /// The lights the level's BSP refers to, by export index.
    fn light_samples(&mut self, model: &grim_assets::model::Model) -> HashMap<u32, grim_world::lightmap::LightSample> {
        let package = self.level.level.package;
        let mut out = HashMap::new();
        for r in model.lights.clone() {
            let ObjectRef::Export(export) = r else { continue };
            if out.contains_key(&export) {
                continue;
            }
            let actor = ObjectKey { package, export };
            if std::env::var_os("GRIM_DEBUG_LIGHTS").is_some() {
                let class = self.vm.class_of(actor).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                println!(
                    "light {class}: type {} effect {} brightness {} radius {} hue {} sat {} inner {} source {} glow {}",
                    self.byte_prop(actor, "LightType"),
                    self.byte_prop(actor, "LightEffect"),
                    self.byte_prop(actor, "LightBrightness"),
                    self.byte_prop(actor, "LightRadius"),
                    self.byte_prop(actor, "LightHue"),
                    self.byte_prop(actor, "LightSaturation"),
                    self.byte_prop(actor, "LightRadiusInner"),
                    self.byte_prop(actor, "LightSource"),
                    self.byte_prop(actor, "AmbientGlow"),
                );
                println!(
                    "  cone {} period {} phase {} rotation {:?}",
                    self.byte_prop(actor, "LightCone"),
                    self.byte_prop(actor, "LightPeriod"),
                    self.byte_prop(actor, "LightPhase"),
                    self.vm.get_prop(actor, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).unwrap_or_default(),
                );
            }
            // LT_None leaves the light off; its bits are still stored.
            if self.byte_prop(actor, "LightType") == 0 {
                continue;
            }
            let Ok(position) = self.vm.get_prop(actor, "Location").and_then(|v| grim_vm::vm::floats3(&v)) else { continue };
            let brightness = self.byte_prop(actor, "LightBrightness") as f32 / 255.0;
            let colour = light_colour(self.byte_prop(actor, "LightHue"), self.byte_prop(actor, "LightSaturation"));
            // LightRadius counts in blocks of 25 world units, and LightRadiusInner is the
            // fraction of that the light holds at full brightness.
            // How far a light reaches: `LightRadiusInner` in blocks of 25 world units, times
            // the light's own `DrawScale`, with `LightRadius` as the core it holds at full
            // brightness. `LightRadiusInner` is this game's own addition to `Actor` and is the
            // larger of the two (220 against 6 in `FlyingFordCutScene`, 100 against 32 in
            // `EntryHall_hub`), which is what makes it the outer reach.
            //
            // Measured against the lights each map hands its surfaces (`grim lightbits <map>`,
            // distance over reach): with `LightRadiusInner * 25 * DrawScale` not one of
            // `FlyingFordCutScene`'s 283 pairs falls outside, and the furthest sits at 0.98 of
            // it — the editor listed lights right up to that reach and never past it. Taking
            // `LightRadius` instead leaves 283 of them beyond, and the flat sheet of cloud the
            // car flies over stays black instead of the lit lavender of the original.
            //
            // Hypothesis. A light is never drawn in the game (they are `bHidden`), so its
            // `DrawScale` has nothing else to do, and the maps only make sense with it: in
            // `FlyingFordCutScene` every light carries `DrawScale = 15` and sits up to 800
            // units from the ground it is listed against with a radius of 6 (150 units), so
            // without it the whole landscape the car flies over stays black. Measured over the
            // lights each map hands its surfaces, the distances fit the scaled radius as well
            // as or better than the plain one (`grim lightbits <map>`), and the maps whose
            // lights carry a `DrawScale` below 1 are the ones that only exist for their corona,
            // with `LightBrightness` zero.
            let scale = match self.vm.get_prop(actor, "DrawScale") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            let core = self.byte_prop(actor, "LightRadius") as f32 * 25.0 * scale;
            let outer = self.byte_prop(actor, "LightRadiusInner") as f32 * 25.0 * scale;
            let radius = if outer > core { outer } else { core };
            let inner = if outer > core { core } else { 0.0 };
            let color = colour.map(|c| c * brightness);
            // LE_Spotlight (12) points along the actor's rotation; LightCone is its width.
            let cone = (self.byte_prop(actor, "LightEffect") == 12)
                .then(|| {
                    let rotation = self.vm.get_prop(actor, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).unwrap_or_default();
                    let half = self.byte_prop(actor, "LightCone") as f32 / 255.0 * std::f32::consts::FRAC_PI_2;
                    (grim_render::math::axes(rotation)[0], half.cos())
                });
            // LE_NonIncidence (13) lights a surface whatever way it faces.
            let ignores_angle = self.byte_prop(actor, "LightEffect") == 13;
            // A dark light takes light away: this game's night maps carve their shadows with them.
            let dark = matches!(self.vm.get_prop(actor, "bDarkLight"), Ok(Value::Bool(true)));
            let special = matches!(self.vm.get_prop(actor, "bSpecialLit"), Ok(Value::Bool(true)));
            out.insert(export, grim_world::lightmap::LightSample { position, color, radius, inner, cone, ignores_angle, dark, special });
        }
        out
    }

    /// Ambient light of each surface, taken from the `ZoneInfo` of the zone in front of it.
    /// How fast each zone slides the textures of its `PF_AutoUPan`/`PF_AutoVPan` surfaces, in
    /// texels a second. Only six of the game's maps set it; the rest leave it at zero, and
    /// their marked surfaces stand still, which is what the original does with them too.
    fn zone_pan(&mut self, model: &grim_assets::model::Model) -> Vec<[f32; 2]> {
        let package = self.level.level.package;
        let mut out = vec![[0.0f32; 2]; model.zones.len()];
        for (i, zone) in model.zones.iter().enumerate() {
            let ObjectRef::Export(export) = zone.zone_actor else { continue };
            let actor = ObjectKey { package, export };
            let speed = |game: &mut Game, name: &str| match game.vm.get_prop(actor, name) {
                Ok(Value::Float(v)) => v,
                _ => 0.0,
            };
            out[i] = [speed(self, "TexUPanSpeed"), speed(self, "TexVPanSpeed")];
        }
        out
    }

    fn zone_ambient(&mut self, model: &grim_assets::model::Model) -> Vec<[f32; 3]> {
        self.zone_ambients.clear();
        let package = self.level.level.package;
        let mut zone_of = vec![0usize; model.surfs.len()];
        for node in &model.nodes {
            if node.i_surf >= 0 {
                if let Some(z) = zone_of.get_mut(node.i_surf as usize) {
                    *z = node.i_zone[grim_world::FRONT] as usize;
                }
            }
        }
        let mut by_zone: HashMap<usize, [f32; 3]> = HashMap::new();
        for (i, zone) in model.zones.iter().enumerate() {
            let ObjectRef::Export(export) = zone.zone_actor else { continue };
            let actor = ObjectKey { package, export };
            let brightness = self.byte_prop(actor, "AmbientBrightness") as f32 / 255.0;
            let colour = light_colour(self.byte_prop(actor, "AmbientHue"), self.byte_prop(actor, "AmbientSaturation"));
            let ambient = [colour[0] * brightness, colour[1] * brightness, colour[2] * brightness];
            by_zone.insert(i, ambient);
            self.zone_ambients.insert(i as u8, ambient);
        }
        if std::env::var_os("GRIM_DEBUG_LIGHTS").is_some() {
            for (i, zone) in model.zones.iter().enumerate() {
                let ObjectRef::Export(export) = zone.zone_actor else { continue };
                let actor = ObjectKey { package, export };
                let class = self.vm.class_of(actor).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                println!(
                    "zone {i} {class}: ambient brightness {} hue {} sat {} -> {:?}",
                    self.byte_prop(actor, "AmbientBrightness"),
                    self.byte_prop(actor, "AmbientHue"),
                    self.byte_prop(actor, "AmbientSaturation"),
                    by_zone.get(&i),
                );
            }
        }
        zone_of.iter().map(|z| by_zone.get(z).copied().unwrap_or([0.0; 3])).collect()
    }

    /// An actor's light, kept until it moves: working it out traces every light in range
    /// against the level, which is far too much to redo for every actor of every frame.
    fn cached_light(&mut self, actor: ObjectKey, at: [f32; 3], shadowed: bool) -> ([f32; 3], [grim_render::MeshLight; grim_render::MESH_LIGHTS]) {
        const MOVED: f32 = 16.0;
        if let Some((was, tint, lights)) = self.tints.get(&actor) {
            let d = grim_world::sub(at, *was);
            if grim_world::dot(d, d) < MOVED * MOVED {
                return (*tint, *lights);
            }
        }
        let (tint, lights) = self.light_at(at, shadowed);
        self.tints.insert(actor, (at, tint, lights));
        (tint, lights)
    }

    /// Light reaching a point, as a mesh takes it: the strongest lights shade it by the angle
    /// of each vertex and the rest light it evenly. Every light is traced against the level, so
    /// meshes stand in the level's shadows.
    /// Ambient light where a point stands: a mesh takes it from the zone it is in, the same
    /// one a surface there would take.
    fn ambient_at(&self, point: [f32; 3]) -> [f32; 3] {
        let zone = self.vm.bsp.as_ref().map(|b| b.point_region(point).1);
        zone.and_then(|z| self.zone_ambients.get(&z).copied()).unwrap_or(self.ambient)
    }

    fn light_at(&self, point: [f32; 3], shadowed: bool) -> ([f32; 3], [grim_render::MeshLight; grim_render::MESH_LIGHTS]) {
        let mut reaching: Vec<(f32, grim_render::MeshLight)> = Vec::new();
        for light in &self.lights {
            // `bSpecialLit` reaches surfaces marked `PF_SpecialLit` and nothing else, actors
            // included: `Engine.Actor` says so of the flag ("Only affects special-lit
            // surfaces"). The flying car's sky is the case that shows it: its ground is lit
            // apart by fifteen `SpecialLit` lights, and pouring those into every actor as well
            // left the cloud sprites at a tint of (0.99, 0.98, 1.43) — white, over a night sky.
            if light.special {
                continue;
            }
            let to_light = grim_world::sub(light.position, point);
            let distance = grim_world::dot(to_light, to_light).sqrt();
            if distance >= light.radius {
                continue;
            }
            // A brush actor sits inside its own geometry, where every trace is blocked, so
            // only meshes are shadowed against the level.
            if shadowed && self.vm.bsp.as_ref().is_some_and(|b| b.line_check(light.position, point).is_some()) {
                continue;
            }
            let intensity = light.falloff(distance) * light.spread(to_light, distance);
            let color = light.color.map(|c| c * intensity);
            let direction = grim_world::scale(to_light, 1.0 / distance.max(1e-3));
            reaching.push((color[0] + color[1] + color[2], grim_render::MeshLight { direction, color }));
        }
        reaching.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut lights = [grim_render::MeshLight::default(); grim_render::MESH_LIGHTS];
        for (slot, (_, light)) in lights.iter_mut().zip(&reaching) {
            *slot = *light;
        }
        // Only the few strongest lights reach an actor, the same count a zone gives its
        // surfaces (`ZoneInfo.MaxLightCount`). What is left over is dropped rather than piled
        // into the fill: with the reach the maps really use, a night sky holds hundreds of
        // small lights and adding them all turned every sprite and every mesh white.
        let mut fill = self.ambient_at(point);
        let spare = self.light_count.saturating_sub(grim_render::MESH_LIGHTS);
        for (_, light) in reaching.iter().skip(grim_render::MESH_LIGHTS).take(spare) {
            for (f, c) in fill.iter_mut().zip(light.color) {
                *f += c;
            }
        }
        (fill, lights)
    }

    /// The backdrop and where it is seen from: Unreal looks at the sky from its zone's own
    /// actor, with the camera's rotation, so it turns with the view but never moves closer.
    fn sky_view(&mut self) -> Option<(grim_render::DrawItem<'static>, [f32; 3])> {
        let mesh = self.sky_mesh?;
        let zone = self.vm.actors.clone().into_iter().find(|&a| {
            self.vm
                .class_of(a)
                .is_ok_and(|c| self.vm.world.names.str(self.vm.world.class(c).name).contains("SkyZoneInfo"))
        })?;
        let at = self.vm.get_prop(zone, "Location").and_then(|v| grim_vm::vm::floats3(&v)).ok()?;
        let item = grim_render::DrawItem {
            mesh,
            transform: grim_render::math::IDENTITY,
            alpha_test: None,
            skins: &[],
            blend: None,
            tint: [1.0; 3],
            lights: Default::default(),
            env_map: false,
            lightmap: self.lightmap,
        };
        Some((item, at))
    }

    /// Where the game says the view is: its own `PlayerCalcView` decides, which is how the
    /// third person camera follows the player.
    fn update_view(&mut self) {
        self.update_listener();
        let Some(p) = self.vm.player else { return };
        let out = self.vm.call_with_out(p, "PlayerCalcView", vec![Value::Object(None), grim_vm::vm::vector(0.0, 0.0, 0.0), grim_vm::vm::rotator(0, 0, 0)]);
        if let Ok(Some(values)) = out {
            if let (Ok(at), Ok(rot)) = (grim_vm::vm::floats3(&values[1]), grim_vm::vm::ints3(&values[2])) {
                self.camera.position = at;
                self.camera.rotation = rot;
                return;
            }
        }
        // Without one, look through the pawn's own eyes.
        if let Ok(loc) = self.vm.get_prop(p, "Location").and_then(|v| grim_vm::vm::floats3(&v)) {
            let eye = match self.vm.get_prop(p, "EyeHeight") {
                Ok(Value::Float(h)) => h,
                _ => 0.0,
            };
            self.camera.position = [loc[0], loc[1], loc[2] + eye];
        }
        let view = self.vm.get_prop(p, "ViewRotation").or_else(|_| self.vm.get_prop(p, "Rotation"));
        if let Ok(rot) = view.and_then(|v| grim_vm::vm::ints3(&v)) {
            self.camera.rotation = rot;
        }
    }

    /// The sound is heard from where the view is: the same place and the same axes the scene
    /// is drawn from, so what is on the left of the screen is on the left of the ears.
    fn update_listener(&mut self) {
        if self.vm.audio.is_none() {
            return;
        }
        let [forward, right, _] = grim_render::math::axes(self.camera.rotation);
        let at = self.camera.position;
        if let Some(audio) = self.vm.audio.as_mut() {
            audio.set_listener(grim_audio::Listener { at, forward, right });
        }
    }

    /// Runs the game's own drawing: the console renders the menus, the HUD and its text into
    /// the canvas, and every rectangle it asks for is drawn over the scene.
    fn draw_overlay(&mut self) -> Vec<grim_render::Quad> {
        let (Some(canvas), Some(player)) = (self.vm.canvas, self.vm.player) else { return Vec::new() };
        let (width, height) = self.viewport;
        for (name, value) in [("SizeX", width), ("SizeY", height)] {
            let _ = self.vm.set_prop(canvas, name, Value::Int(value as i32));
        }
        for (name, value) in [("ClipX", width as f32), ("ClipY", height as f32)] {
            let _ = self.vm.set_prop(canvas, name, Value::Float(value));
        }
        for name in ["CurX", "CurY", "OrgX", "OrgY"] {
            let _ = self.vm.set_prop(canvas, name, Value::Float(0.0));
        }
        self.vm.canvas_quads.clear();
        self.vm.canvas_actors.clear();
        let console = self.console();
        // The console's idea of the screen: no script writes `FrameX`/`FrameY`, they only read
        // them to place the loading and pause messages (`baseConsole.PrintActionMessageInUpperLeft`
        // puts its text at a quarter across). The engine keeps them at the viewport's size.
        if let Some(console) = console {
            let _ = self.vm.set_prop(console, "FrameX", Value::Float(width as f32));
            let _ = self.vm.set_prop(console, "FrameY", Value::Float(height as f32));
        }
        // The engine draws in this order: the pawn prepares, the HUD draws over the world, and
        // the console puts its messages and menus on top.
        for event in ["PreRender", "PostRender"] {
            if let Err(e) = self.vm.call_event(player, event, vec![Value::Object(Some(canvas))]) {
                self.report.record(e);
            }
        }
        if let Some(console) = console {
            if let Err(e) = self.vm.call_event(console, "PostRender", vec![Value::Object(Some(canvas))]) {
                self.report.record(e);
            }
        }
        if self.debug {
            self.draw_debug_overlay(canvas, width as f32, height as f32);
        }
        let quads = std::mem::take(&mut self.vm.canvas_quads);
        if std::env::var_os("GRIM_DEBUG_QUADS").is_some() {
            for q in &quads {
                let name = q.texture.map(|k| {
                    let pkg = self.vm.world.package(k.package).clone();
                    format!("{}.{}", pkg.name, pkg.object_name(grim_package::ObjectRef::Export(k.export)))
                });
                println!("quad rect {:?} texture {name:?}", q.rect.map(|v| v.round()));
            }
        }
        let Some(renderer) = self.renderer.as_mut() else { return Vec::new() };
        let mut out = Vec::with_capacity(quads.len());
        for q in quads {
            let texture = q.texture.and_then(|k| {
                let pkg = self.vm.world.package(k.package).clone();
                self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, k.export))
            });
            // The scripts give texture rectangles in texels; the renderer keeps its sampling
            // inside them. Squeezing the rectangle half a texel from each side instead left a
            // tile drawn one texel to a pixel (every letter of a font) resampled at a slightly
            // different size, which scrambled small text.
            let mut rect = q.rect;
            let (id, uv, texture_masked) = match texture {
                Some((id, w, h, masked)) => {
                    // Drawn one texel to a pixel, a tile sits on whole pixels: half a pixel off,
                    // the filter would blend each texel with its neighbour, and text blurs.
                    if (q.rect[2] - q.uv[2]).abs() < 1e-3 && (q.rect[3] - q.uv[3]).abs() < 1e-3 {
                        rect[0] = rect[0].round();
                        rect[1] = rect[1].round();
                    }
                    (Some(id), [q.uv[0] / w, q.uv[1] / h, q.uv[2] / w, q.uv[3] / h], masked)
                }
                None => (None, [0.0, 0.0, 1.0, 1.0], false),
            };
            // The canvas style says how it goes over the scene.
            let blend = match q.style {
                grim_vm::canvas::STY_TRANSLUCENT => grim_render::Blend::Translucent,
                grim_vm::canvas::STY_MODULATED => grim_render::Blend::Modulated,
                _ => grim_render::Blend::Opaque,
            };
            // A masked texture keeps its transparent texels out whatever the style asks for.
            let masked = q.masked || texture_masked;
            out.push(grim_render::Quad { rect, uv, texture: id, color: q.color, masked, blend });
        }
        out
    }

    /// Escape asks the game to open its menu. Its own UI is not drawn yet, so this only runs
    /// the script side of it.
    /// Sends a key to the game the way the engine does: the console sees it first and may take
    /// it, which is how the menus open and how typing reaches them.
    fn key_to_game(&mut self, key: u8, pressed: bool) -> bool {
        let Some(console) = self.console() else { return false };
        let action = Value::Byte(if pressed { IST_PRESS } else { IST_RELEASE });
        let args = vec![Value::Byte(key), action, Value::Float(0.0)];
        match self.vm.call_event(console, "KeyEvent", args) {
            Ok(Some(Value::Bool(taken))) => taken,
            Ok(_) => false,
            Err(e) => {
                self.report.record(e);
                false
            }
        }
    }

    fn show_menu(&mut self) {
        if self.key_to_game(IK_ESCAPE, true) {
            return;
        }
        let Some(p) = self.vm.player else { return };
        if let Err(e) = self.vm.call_event(p, "ShowMenu", vec![]) {
            self.report.record(e);
        }
    }

    /// The console the player's viewport holds.
    fn console(&mut self) -> Option<ObjectKey> {
        let player = self.vm.player?;
        let Ok(Value::Object(Some(viewport))) = self.vm.get_prop(player, "Player") else { return None };
        match self.vm.get_prop(viewport, "Console") {
            Ok(Value::Object(c)) => c,
            _ => None,
        }
    }

    fn update(&mut self, dt: f32) {
        self.frame_time = dt;
        // A tenth of each new frame, so the number on screen does not jump about.
        self.frame_average += (dt - self.frame_average) * 0.1;
        let started = web_time::Instant::now();
        self.update_inner(dt);
        self.tick_cost = started.elapsed().as_secs_f32() * 1000.0;
    }

    fn update_inner(&mut self, dt: f32) {
        // The game's own debug mode goes with the engine's: it is what lets F4 open the
        // shortcut window (`HPConsole.KeyEvent` asks `bDebugMode` before `ShortCut`).
        if self.debug {
            if let Some(console) = self.console() {
                if matches!(self.vm.get_prop(console, "bDebugMode"), Ok(Value::Bool(false))) {
                    let _ = self.vm.set_prop(console, "bDebugMode", Value::Bool(true));
                }
            }
        }
        // A menu takes the pointer for itself, and the game takes it back when the menu goes.
        let free = self.pointer_free();
        if free == self.grabbed {
            self.grab(!free);
        }
        if !self.free_camera {
            return self.update_player(dt);
        }
        // Free camera: the game's clock is stopped while it is on.
        // Look.
        let (dx, dy) = std::mem::take(&mut self.look);
        self.camera.rotation[1] = self.camera.rotation[1].wrapping_add((dx * MOUSE_SENSITIVITY) as i32);
        self.camera.rotation[0] = (self.camera.rotation[0] - (dy * MOUSE_SENSITIVITY) as i32).clamp(-MAX_PITCH, MAX_PITCH);

        // Move along the camera axes.
        let [forward, right, _] = grim_render::math::axes(self.camera.rotation);
        let mut delta = [0.0f32; 3];
        let mut add = |v: [f32; 3], k: f32| {
            for i in 0..3 {
                delta[i] += v[i] * k;
            }
        };
        let speed = if self.keys.contains(&KeyCode::ShiftLeft) { WALK_SPEED * 4.0 } else { WALK_SPEED } * dt;
        if self.keys.contains(&KeyCode::KeyW) {
            add(forward, speed);
        }
        if self.keys.contains(&KeyCode::KeyS) {
            add(forward, -speed);
        }
        if self.keys.contains(&KeyCode::KeyD) {
            add(right, speed);
        }
        if self.keys.contains(&KeyCode::KeyA) {
            add(right, -speed);
        }
        if self.keys.contains(&KeyCode::Space) {
            add([0.0, 0.0, 1.0], speed);
        }
        if self.keys.contains(&KeyCode::ControlLeft) {
            add([0.0, 0.0, 1.0], -speed);
        }
        self.camera.position = grim_render::math::add(self.camera.position, delta);
    }

    /// Leaves the free camera: the player picks up where the camera is looking.
    /// Gives Harry every spell, the way the game's own code does: `AddToSpellBookByString`
    /// puts one in his book by name, and the book is one of the things he carries from level
    /// to level. `HGame.harry.AddToSpellBookByString` is where the names come from.
    fn grant_spells(&mut self) {
        const SPELLS: [&str; 10] = [
            "Flipendo",
            "Lumos",
            "Alohomora",
            "Skurge",
            "Rictusempra",
            "Diffindo",
            "Spongify",
            "DuelRictusempra",
            "DuelMimblewimble",
            "DuelExpelliarmus",
        ];
        let Some(player) = self.vm.player else { return };
        let mut given = Vec::new();
        for spell in SPELLS {
            if self.vm.call_event(player, "AddToSpellBookByString", vec![Value::Str(spell.to_string())]).is_ok() {
                given.push(spell);
            }
        }
        // The book has a slot per spell type; count what ended up in it.
        let held = (0..32)
            .filter(|&i| matches!(self.vm.get_prop_at(player, "SpellBook", i), Ok(Value::Object(Some(_)))))
            .count();
        println!("spells: {} ({held} in the book)", given.join(", "));
    }

    fn teleport_player(&mut self) {
        let Some(p) = self.vm.player else { return };
        let eye = match self.vm.get_prop(p, "EyeHeight") {
            Ok(Value::Float(h)) => h,
            _ => 0.0,
        };
        let at = self.camera.position;
        let _ = self.vm.set_prop(p, "Location", grim_vm::vm::vector(at[0], at[1], at[2] - eye));
        let _ = self.vm.set_prop(p, "Velocity", grim_vm::vm::vector(0.0, 0.0, 0.0));
        let rotation = self.camera.rotation;
        let yaw = grim_vm::vm::rotator(0, rotation[1], 0);
        let _ = self.vm.set_prop(p, "Rotation", yaw);
        let view = grim_vm::vm::rotator(rotation[0], rotation[1], rotation[2]);
        let _ = self.vm.set_prop(p, "ViewRotation", view);
        // Let it settle on whatever is under it.
        let _ = self.vm.set_prop(p, "Physics", Value::Byte(2));
    }

    /// Plays the game: the keys become the movement axes the scripts read, and the view comes
    /// from the player pawn itself.
    fn update_player(&mut self, dt: f32) {
        let (dx, dy) = std::mem::take(&mut self.look);
        let held: Vec<&str> = self.keys.iter().filter_map(|k| key_name(*k)).chain(self.buttons.iter().copied()).collect();
        let input = grim_vm::level::PlayerInput { held: &held, mouse: (dx, -dy) };
        let started = web_time::Instant::now();
        self.vm.player_input(input, dt, &mut self.report);
        let t_input = started.elapsed().as_secs_f32() * 1000.0;
        let started = web_time::Instant::now();
        self.vm.tick(dt, &mut self.report);
        let t_tick = started.elapsed().as_secs_f32() * 1000.0;
        let started = web_time::Instant::now();
        // The console is not an actor, so the engine ticks it itself: its window system needs it.
        if let Some(console) = self.console() {
            if let Err(e) = self.vm.call_event(console, "Tick", vec![Value::Float(dt)]) {
                self.report.record(e);
            }
        }
        let t_console = started.elapsed().as_secs_f32() * 1000.0;
        if let Some(url) = self.pending_travel() {
            if let Err(e) = self.travel(&url) {
                eprintln!("travel: {e}");
            }
        }
        let started = web_time::Instant::now();
        self.update_view();
        if std::env::var_os("GRIM_TRACE_TICK").is_some() {
            eprintln!(
                "PLAY input {t_input:.2} tick {t_tick:.2} console {t_console:.2} view {:.2}",
                started.elapsed().as_secs_f32() * 1000.0
            );
        }
    }

    /// Mesh of an actor's `Mesh` property, uploaded on first use.
    fn actor_mesh(&mut self, mesh_key: ObjectKey) -> Option<MeshId> {
        if let Some(m) = self.actor_meshes.get(&mesh_key) {
            return *m;
        }
        let renderer = self.renderer.as_mut()?;
        let pkg = self.vm.world.package(mesh_key.package).clone();
        let data = grim_world::scene::actor_mesh(renderer, &self.vm.world.lib, &pkg, mesh_key.export, &mut self.textures);
        let id = data.filter(|d| !d.data.vertices.is_empty()).map(|d| {
            let id = renderer.create_mesh(&d.data);
            self.mesh_sections.insert(id, d.data.sections.iter().map(|s| s.material).collect());
            self.section_has_texture.insert(id, d.data.sections.iter().map(|s| s.texture.is_some()).collect());
            self.mesh_centres.insert(id, d.centre);
            self.mesh_lows.insert(id, d.low);
            id
        });
        self.actor_meshes.insert(mesh_key, id);
        id
    }

    /// The mesh an actor draws while it animates: its own copy, posed from the sequence it is
    /// playing. Actors that are not animating share the mesh in its reference pose.
    fn animated_mesh(&mut self, actor: ObjectKey, mesh_key: ObjectKey) -> Option<MeshId> {
        let started = web_time::Instant::now();
        let mesh = self.pose_animated(actor, mesh_key);
        self.skinning.0 += started.elapsed().as_secs_f32() * 1000.0;
        mesh
    }

    fn pose_animated(&mut self, actor: ObjectKey, mesh_key: ObjectKey) -> Option<MeshId> {
        if !matches!(self.vm.get_prop(actor, "AnimSequence"), Ok(Value::Name(_))) {
            return None;
        }
        if !self.animated.contains_key(&actor) {
            let made = self.build_animated(mesh_key)?;
            self.animated.insert(actor, made);
        }
        // The pose itself belongs to the engine, which also answers the scripts asking where a
        // bone is; the drawing only skins the mesh with it.
        let locals = self.vm.pose_locals(actor)?;
        let world = self.vm.poses.get(&actor)?.skin.world(&locals);
        let animated = self.animated.get_mut(&actor)?;
        // Skinned again only when a bone moved: a blend moves them with the frame held, and so
        // does a face talking over a body that stands still.
        if animated.last.as_ref() == Some(&locals) {
            return Some(animated.mesh);
        }
        animated.last = Some(locals);
        self.skinning.1 += 1;
        let animated = self.animated.get_mut(&actor)?;
        let mut points = std::mem::take(&mut animated.points);
        self.vm.poses.get(&actor)?.skin.deform(&world, &mut points);
        let animated = self.animated.get_mut(&actor)?;
        animated.posed.pose(&points);
        animated.points = points;
        // How big the actor comes out in the pose it was just put in. The scripts ask the
        // engine for this (`GetRenderExtent`), and a shadow sizes itself by it.
        let mut lo = [f32::MAX; 3];
        let mut hi = [f32::MIN; 3];
        for v in &animated.posed.data.vertices {
            for i in 0..3 {
                lo[i] = lo[i].min(v.position[i]);
                hi[i] = hi[i].max(v.position[i]);
            }
        }
        let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
        // Where the skin ends up against where its bones are: both are in the mesh's own space
        // with the middle of its box taken out, so a mesh drawn away from its own skeleton
        // shows as a gap here.
        if std::env::var("GRIM_DEBUG_PLACEMENT").is_ok_and(|w| self.vm.object_name(actor).to_lowercase().contains(&w.to_lowercase())) {
            let mut blo = [f32::MAX; 3];
            let mut bhi = [f32::MIN; 3];
            for j in &world {
                for i in 0..3 {
                    blo[i] = blo[i].min(j.position[i]);
                    bhi[i] = bhi[i].max(j.position[i]);
                }
            }
            // How far each bone is from the nearest skinned point: a skeleton the skin does
            // not follow shows as a bone with nothing around it.
            let centre = [0, 1, 2].map(|i| (lo[i] + hi[i]) / 2.0 - (blo[i] + bhi[i]) / 2.0);
            let mut worst = (0.0f32, 0usize);
            for (b, j) in world.iter().enumerate() {
                let p = [0, 1, 2].map(|i| j.position[i] + centre[i]);
                let near = animated
                    .posed
                    .data
                    .vertices
                    .iter()
                    .map(|v| (0..3).map(|i| (v.position[i] - p[i]).powi(2)).sum::<f32>())
                    .fold(f32::MAX, f32::min)
                    .sqrt();
                if near > worst.0 {
                    worst = (near, b);
                }
            }
            println!("SKIN bones vs points: worst bone {} is {:.0} from any point", worst.1, worst.0);
            println!(
                "SKIN {}: skin box {:?}..{:?} middle {:?}; bones {:?}..{:?} middle {:?}",
                self.vm.object_name(actor),
                lo.map(|v| v.round()),
                hi.map(|v| v.round()),
                [0, 1, 2].map(|i| ((lo[i] + hi[i]) / 2.0).round()),
                blo.map(|v| v.round()),
                bhi.map(|v| v.round()),
                [0, 1, 2].map(|i| ((blo[i] + bhi[i]) / 2.0).round()),
            );
        }
        let mesh = animated.mesh;
        let data = animated.posed.data.clone();
        if size.iter().all(|v| v.is_finite()) {
            self.vm.pose_extents.insert(actor, size);
        }
        self.renderer.as_mut()?.update_mesh(mesh, &data);
        Some(mesh)
    }

    /// Builds an actor's own copy of a skeletal mesh, with its skin and animation keys.
    fn build_animated(&mut self, mesh_key: ObjectKey) -> Option<Animated> {
        let pkg = self.vm.world.package(mesh_key.package).clone();
        let renderer = self.renderer.as_mut()?;
        let posed = grim_world::scene::actor_mesh(renderer, &self.vm.world.lib, &pkg, mesh_key.export, &mut self.textures)?;
        if posed.data.vertices.is_empty() {
            return None;
        }
        let mesh = renderer.create_mesh(&posed.data);
        // The middle its vertices were stored around, as the static meshes keep it: without it
        // an animated mesh was drawn with that middle on its actor while its bones were placed
        // by the mesh's own origin, so the two came apart. The whomping willow showed it — its
        // box middle is 12.3 units off in Y and it is drawn ten times over, so the tree stood
        // 123 units to one side of the skeleton the map's own collision is built around.
        self.mesh_centres.insert(mesh, posed.centre);
        self.mesh_lows.insert(mesh, posed.low);
        self.mesh_sections.insert(mesh, posed.data.sections.iter().map(|s| s.material).collect());
        self.section_has_texture.insert(mesh, posed.data.sections.iter().map(|s| s.texture.is_some()).collect());
        Some(Animated { mesh, posed, points: Vec::new(), last: None })
    }

    /// Mesh of a brush actor (`Mover`), uploaded on first use.
    /// A brush's points are kept in its own frame, around its pivot, while the lights that lit
    /// it stand in the world. Baking its light means bringing them across.
    fn lights_in_brush(
        &mut self,
        model: &grim_assets::model::Model,
        at: [f32; 3],
        rotation: [i32; 3],
        pivot: [f32; 3],
    ) -> HashMap<u32, grim_world::lightmap::LightSample> {
        let axes = grim_render::math::axes(rotation);
        let into = |w: [f32; 3], translate: bool| {
            let d = if translate { grim_world::sub(w, at) } else { w };
            let local = [0, 1, 2].map(|i| grim_world::dot(d, axes[i]));
            if translate { [0, 1, 2].map(|i| local[i] + pivot[i]) } else { local }
        };
        let mut samples = self.light_samples(model);
        for sample in samples.values_mut() {
            sample.position = into(sample.position, true);
            if let Some((axis, cos)) = sample.cone {
                sample.cone = Some((into(axis, false), cos));
            }
        }
        samples
    }

    fn brush_mesh(&mut self, brush: ObjectKey, pivot: [f32; 3], at: [f32; 3], rotation: [i32; 3]) -> Option<MeshId> {
        if let Some(m) = self.brush_meshes.get(&brush) {
            return *m;
        }
        let pkg = self.vm.world.package(brush.package).clone();
        let model = match grim_assets::load_export(&pkg, brush.export).ok()?.asset {
            grim_assets::Asset::Model(m) => *m,
            _ => return None,
        };
        // A mover's brush is a piece of level geometry of its own: it carries the same nodes
        // and surfaces as the world, so it is built the same way. What it shows, though, comes
        // from the polygons the editor kept, because its tree holds only one surface per
        // plane: the front of a secret door in `EntryHall_hub` is one surface and three
        // polygons (stone, portrait, stone), and the tree alone would paint the portrait over
        // all of it. Its light is baked per polygon too. `GRIM_BRUSH_POLYS` leaves the tree
        // out and draws the polygons on their own, unlit.
        if std::env::var_os("GRIM_BRUSH_POLYS").is_none() && !model.nodes.is_empty() {
            let polys = grim_world::scene::brush_polys(&pkg, &model);
            let pieces = grim_world::lightmap::Pieces::of(&model, (!polys.is_empty()).then_some(&polys));
            let has_light = (0..pieces.count()).any(|i| pieces.map(i).is_some());
            let baked = has_light.then(|| {
                let samples = self.lights_in_brush(&model, at, rotation, pivot);
                // A brush has no zones of its own, so every piece of it takes the ambient
                // where the brush stands.
                let ambient = vec![self.ambient_at(at); pieces.count().max(1)];
                grim_world::lightmap::bake(&pieces, &ambient, self.light_count, &|r| match r {
                    ObjectRef::Export(e) => samples.get(&e).copied(),
                    _ => None,
                })
            });
            let renderer = self.renderer.as_mut()?;
            let light = baked.as_ref().map(|b| {
                renderer.create_texture(grim_render::TextureData { width: b.width, height: b.height, rgba: &b.rgba, mips: &[] })
            });
            // The mirrored surfaces of the brush are left out of its body: each one is built
            // on its own and drawn reflecting, like the level's own mirrors.
            let mut mirrors = Vec::new();
            for (surf, plane) in grim_world::scene::mirror_surfaces(&model) {
                let part = grim_world::scene::Part::Mirror(surf);
                let mut m = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, baked.as_ref(), part, &[]);
                for v in &mut m.vertices {
                    for i in 0..3 {
                        v.position[i] -= pivot[i];
                    }
                }
                if m.vertices.is_empty() {
                    continue;
                }
                // The plane moves with the brush: its distance follows the pivot taken out.
                let d = plane[3] - (plane[0] * pivot[0] + plane[1] * pivot[1] + plane[2] * pivot[2]);
                if std::env::var_os("GRIM_TRACE_MIRROR").is_some() {
                    let n = m.vertices.len() as f32;
                    let c = m.vertices.iter().fold([0.0f32; 3], |a, v| [0, 1, 2].map(|i| a[i] + v.position[i] / n));
                    eprintln!("  mirror of {} around its pivot at {:?}, plane {plane:?}", self.vm.object_name(brush), c.map(|x| x.round()));
                }
                let id = renderer.create_mesh(&m);
                self.mesh_sections.insert(id, m.sections.iter().map(|s| s.material).collect());
                self.section_has_texture.insert(id, m.sections.iter().map(|s| s.texture.is_some()).collect());
                mirrors.push((id, [plane[0], plane[1], plane[2], d]));
            }
            if std::env::var_os("GRIM_TRACE_MIRROR").is_some() && !mirrors.is_empty() {
                eprintln!("brush {} has {} mirror surfaces, pivot {:?}", self.vm.object_name(brush), mirrors.len(), pivot.map(|v| v.round()));
            }
            self.brush_mirrors.insert(brush, mirrors);
            let part = grim_world::scene::Part::Brush;
            let mut d = grim_world::scene::level_mesh(renderer, &self.vm.world.lib, &pkg, &model, &mut self.textures, baked.as_ref(), part, &[]);
            // The brush is stored around its own pivot, and the actor is placed at it.
            for v in &mut d.vertices {
                for i in 0..3 {
                    v.position[i] -= pivot[i];
                }
            }
            if !d.vertices.is_empty() {
                let id = renderer.create_mesh(&d);
                self.mesh_sections.insert(id, d.sections.iter().map(|s| s.material).collect());
                self.section_has_texture.insert(id, d.sections.iter().map(|s| s.texture.is_some()).collect());
                if let Some(light) = light {
                    self.brush_lightmaps.insert(id, light);
                }
                if std::env::var_os("GRIM_TRACE_BRUSH").is_some() {
                    let lit = (0..pieces.count()).filter(|&i| pieces.map(i).is_some()).count();
                    let found = self.lights_in_brush(&model, at, rotation, pivot).len();
                    eprintln!(
                        "brush {} surfaces, {} nodes, {} polygons, {lit} pieces with a lightmap, {} light refs, {found} found",
                        model.surfs.len(),
                        model.nodes.len(),
                        polys.len(),
                        model.lights.len()
                    );
                    if let Some(b) = baked.as_ref() {
                        let lit = b.rgba.chunks(4).filter(|c| c[0] > 8 || c[1] > 8 || c[2] > 8).count();
                        eprintln!("  atlas {}x{}, {lit} lit texels", b.width, b.height);
                    }
                    let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                    for v in &d.vertices {
                        for i in 0..3 {
                            lo[i] = lo[i].min(v.position[i]);
                            hi[i] = hi[i].max(v.position[i]);
                        }
                    }
                    eprintln!("brush bsp mesh {} verts, box {:?}..{:?}", d.vertices.len(), lo.map(|x| x.round()), hi.map(|x| x.round()));
                }
                let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
                for v in &d.vertices {
                    for i in 0..3 {
                        lo[i] = lo[i].min(v.position[i]);
                        hi[i] = hi[i].max(v.position[i]);
                    }
                }
                self.brush_centres.insert(brush, [0, 1, 2].map(|i| (lo[i] + hi[i]) / 2.0));
                self.brush_meshes.insert(brush, Some(id));
                return Some(id);
            }
        }
        // A brush with no tree of its own is drawn straight from its polygons, unlit.
        let renderer = self.renderer.as_mut()?;
        let light = None;
        let part = grim_world::scene::Part::Brush;
        let data = grim_world::scene::brush_mesh(renderer, &self.vm.world.lib, &pkg, &model, pivot, &mut self.textures, None, part);
        let mut centre = [0.0f32; 3];
        let id = data.map(|d| {
            let (mut lo, mut hi) = ([f32::MAX; 3], [f32::MIN; 3]);
            for v in &d.vertices {
                for i in 0..3 {
                    lo[i] = lo[i].min(v.position[i]);
                    hi[i] = hi[i].max(v.position[i]);
                }
            }
            centre = [0, 1, 2].map(|i| (lo[i] + hi[i]) / 2.0);
            let id = renderer.create_mesh(&d);
            self.mesh_sections.insert(id, d.sections.iter().map(|s| s.material).collect());
            self.section_has_texture.insert(id, d.sections.iter().map(|s| s.texture.is_some()).collect());
            if let Some(light) = light {
                self.brush_lightmaps.insert(id, light);
            }
            id
        });
        self.brush_centres.insert(brush, centre);
        self.brush_meshes.insert(brush, id);
        id
    }

    /// Textures an actor puts over its mesh materials (`MultiSkins`, else `Skin`).
    fn actor_skins(&mut self, actor: ObjectKey) -> Vec<Option<grim_render::TextureId>> {
        let mut keys: Vec<Option<ObjectKey>> = (0..8)
            .map(|i| match self.vm.get_prop_at(actor, "MultiSkins", i) {
                Ok(Value::Object(k)) => k,
                _ => None,
            })
            .collect();
        if keys.iter().all(Option::is_none) {
            if let Ok(Value::Object(Some(skin))) = self.vm.get_prop(actor, "Skin") {
                keys.fill(Some(skin));
            }
        }
        let Some(renderer) = self.renderer.as_mut() else { return Vec::new() };
        keys.into_iter()
            .map(|k| {
                let k = k?;
                let pkg = self.vm.world.package(k.package).clone();
                self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, k.export)).map(|(id, ..)| id)
            })
            .collect()
    }

    /// The live particles of every emitter, as sprites facing the camera. They are handed over
    /// sorted by texture, which is how the renderer draws them in one go.
    fn particle_sprites(&mut self) -> Vec<grim_render::Sprite> {
        let emitters: Vec<ObjectKey> = self.vm.particles.keys().copied().collect();
        let mut out = Vec::new();
        for a in emitters {
            if self.vm.is_deleted(a) {
                continue;
            }
            // A whole system is dropped at once when none of it can be seen: the ball is the
            // farthest particle from the emitter, so one straggler keeps the system in.
            let at = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
            let reach = match self.vm.particles.get(&a) {
                Some(state) if !state.particles.is_empty() => Some(
                    state
                        .particles
                        .iter()
                        .map(|p| {
                            let d = grim_render::math::sub(p.position, at);
                            let size = p.shade().1;
                            grim_render::math::dot(d, d).sqrt() + size[0].max(size[1])
                        })
                        .fold(0.0f32, f32::max),
                ),
                _ => None,
            };
            if let Some(reach) = reach {
                self.draw_radii.insert(a, reach);
                // What only a mirror shows is kept: this is the cheap test that decides
                // whether the actor is posed at all, so a reflection would otherwise lose it.
                let planes = self.frame_mirrors.clone();
                if !self.in_view(at, reach) && !self.seen_in_a_mirror(&planes, at, reach) {
                    continue;
                }
            }
            let texture = match self.vm.get_prop_at(a, "Textures", 0) {
                Ok(Value::Object(Some(k))) => {
                    let pkg = self.vm.world.package(k.package).clone();
                    let Some(renderer) = self.renderer.as_mut() else { continue };
                    self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, k.export)).map(|(id, ..)| id)
                }
                _ => None,
            };
            // Styles: 3 is STY_Translucent, 4 is STY_Modulated. The emitters all ask for one.
            let blend = match self.vm.get_prop(a, "Style") {
                Ok(Value::Byte(4)) => grim_render::Blend::Modulated,
                _ => grim_render::Blend::Translucent,
            };
            let Some(state) = self.vm.particles.get(&a) else { continue };
            for p in &state.particles {
                let (color, size) = p.shade();
                out.push(grim_render::Sprite { through: false, position: p.position, size, spin: p.spin, color, texture, blend, facing: None });
            }
        }
        out.sort_by_key(|s| (s.texture.map(|t| t.0), s.blend));
        out
    }

    /// Actors drawn as a picture facing the camera. The game shows the symbol of the spell it
    /// has a target for this way: a sprite carrying one of `SpellShapes.SpellFX`, which are
    /// textures the engine draws itself.
    fn actor_sprites(&mut self) -> Vec<grim_render::Sprite> {
        let mut out = Vec::new();
        for a in self.vm.actors.clone() {
            if self.vm.is_deleted(a) {
                continue;
            }
            // A light with `bCorona` shows its `Skin` as the glow around it. The light itself
            // is hidden (every light of the game is), and the glow is the only thing of it
            // that is drawn: the lamps of the stairs of `EntryHall_hub` and the candles carry
            // one each. Nothing draws it when something stands between it and the eye.
            // The same one-look-up-front as the meshes: a sprite is either a glow or an actor
            // drawn as a picture, and everything else is passed over without reading more.
            let Some(look) = self.first_look(a) else { continue };
            let corona = matches!(self.vm.get_prop_id(a, self.vm.hot.corona), Ok(Value::Bool(true)));
            if !corona && (look.hidden || look.draw_type != DT_SPRITE) {
                continue;
            }
            let from = if corona { self.vm.get_prop(a, "Skin") } else { self.vm.get_prop(a, "Texture") };
            let Ok(Value::Object(Some(key))) = from else { continue };
            let Ok(position) = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)) else { continue };
            if corona && self.vm.bsp.as_ref().is_some_and(|b| b.line_check(self.camera.position, position).is_some()) {
                continue;
            }
            let scale = match self.vm.get_prop(a, "DrawScale") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            let pkg = self.vm.world.package(key.package).clone();
            let Some(renderer) = self.renderer.as_mut() else { continue };
            let Some((texture, width, height, _)) =
                self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, key.export))
            else {
                continue;
            };
            // A glow adds its light whatever style the light carries: every light of the game
            // that has one is `STY_Modulated`, which would darken the wall behind it instead.
            let blend = match self.vm.get_prop(a, "Style") {
                Ok(Value::Byte(4)) if !corona => grim_render::Blend::Modulated,
                _ => grim_render::Blend::Translucent,
            };
            let glow = match self.vm.get_prop(a, "ScaleGlow") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            // A glow takes the colour of the light it belongs to: the lamps of the stairs are
            // hue 25 with saturation 20, which is the warm orange they show in the original.
            // Their `LightBrightness` is zero — they light nothing and are only there for the
            // glow — so how strong it is drawn cannot come from there.
            let colour = if corona {
                let tint = light_colour(self.byte_prop(a, "LightHue"), self.byte_prop(a, "LightSaturation"));
                let strength = glow * CORONA_GLOW;
                [tint[0] * strength, tint[1] * strength, tint[2] * strength, 1.0]
            } else {
                // A sprite takes the light of the place it stands in, as any other actor does,
                // unless it lights itself. Drawn at full brightness instead, the cloud banks of
                // the flying car's night sky (`MovingClouds`, drawn normally) came out white and
                // took the scene's blue with them. One drawn translucent or modulated is a glow
                // laid over the scene and is not lit, as a mesh in those styles is not: the spell
                // symbol (`GestureSprite`, translucent) turned red under a warm light where the
                // original shows it magenta.
                let unlit = matches!(self.vm.get_prop(a, "bUnlit"), Ok(Value::Bool(true))) || matches!(self.vm.get_prop(a, "Style"), Ok(Value::Byte(3 | 4)));
                let ambient_glow = self.byte_prop(a, "AmbientGlow") as f32 / 255.0;
                let tint = match unlit {
                    true => [1.0; 3],
                    false => {
                        let (tint, _) = self.cached_light(a, position, true);
                        tint.map(|c| c.max(ambient_glow))
                    }
                };
                [tint[0] * glow, tint[1] * glow, tint[2] * glow, 1.0]
            };
            // How big and how strong a glow is drawn is not in the files. Every corona of the
            // game carries only where it is, its colour and a `DrawScale` of 0.1 to 0.3 over a
            // picture 64 texels wide; its `LightBrightness` is zero, the ini files only say
            // whether coronas are drawn at all, and no script of the game mentions them. So
            // both are the engine's own, set by eye against the original: eight times the
            // usual size of a sprite, at half strength.
            const CORONA_SIZE: f32 = 8.0;
            const CORONA_GLOW: f32 = 0.5;
            let scale = if corona { scale * CORONA_SIZE } else { scale };
            if std::env::var_os("GRIM_DEBUG_SPRITES").is_some() {
                println!(
                    "  sprite {} at {position:?} size {:?} colour {colour:?} blend {blend:?}",
                    self.vm.object_name(a),
                    [width * scale, height * scale]
                );
            }
            out.push(grim_render::Sprite {
                // A glow is hidden by the trace above, not by what is drawn in front of it:
                // the lantern's own glass stands between the light and the eye.
                through: corona,
                position,
                size: [width * scale, height * scale],
                spin: 0.0,
                color: colour,
                texture: Some(texture),
                blend,
                facing: None,
            });
        }
        out.extend(self.decal_sprites());
        out
    }

    /// The decals that landed on the world: a shadow is one of them, thrown down from whatever
    /// casts it. Each lies on the surface it met, with the picture and the size its actor
    /// carries, and modulates what is already drawn there.
    fn decal_sprites(&mut self) -> Vec<grim_render::Sprite> {
        let mut out = Vec::new();
        if std::env::var_os("GRIM_NO_DECALS").is_some() {
            return out;
        }
        for (actor, decal) in self.vm.decals.clone() {
            if self.vm.is_deleted(actor) {
                continue;
            }
            let Ok(Value::Object(Some(key))) = self.vm.get_prop(actor, "Texture") else { continue };
            let scale = match self.vm.get_prop(actor, "DrawScale") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            let pkg = self.vm.world.package(key.package).clone();
            let Some(renderer) = self.renderer.as_mut() else { continue };
            let Some((texture, width, height, _)) =
                self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, key.export))
            else {
                continue;
            };
            let blend = match self.vm.get_prop(actor, "Style") {
                Ok(Value::Byte(4)) => grim_render::Blend::Modulated,
                _ => grim_render::Blend::Translucent,
            };
            // Far enough off the floor not to fight the surface for the depth test, near enough
            // to stay on it where the ground slopes.
            const LIFT: f32 = 2.0;
            let position = [
                decal.at[0] + decal.normal[0] * LIFT,
                decal.at[1] + decal.normal[1] * LIFT,
                decal.at[2] + decal.normal[2] * LIFT,
            ];
            out.push(grim_render::Sprite {
                through: false,
                position,
                size: [width * scale, height * scale],
                spin: 0.0,
                color: [1.0, 1.0, 1.0, 1.0],
                texture: Some(texture),
                blend,
                facing: Some(decal.normal),
            });
        }
        out
    }

    /// What the drawing reads about every actor before it decides whether to draw it at all.
    fn first_look(&mut self, a: ObjectKey) -> Option<FirstLook> {
        let class = self.vm.class_of(a).ok()?;
        let slots = match self.draw_slots.get(&class) {
            Some(s) => *s,
            None => {
                let hot = self.vm.hot;
                let of = |vm: &Vm, name| vm.world.class(class).prop(name).map(|p| p.slot as usize);
                let slots = DrawSlots {
                    hidden: of(&self.vm, hot.hidden),
                    draw_type: of(&self.vm, hot.draw_type),
                    brush: of(&self.vm, hot.brush),
                    mesh: of(&self.vm, hot.mesh),
                    location: of(&self.vm, hot.location),
                    draw_scale: of(&self.vm, hot.draw_scale),
                };
                self.draw_slots.insert(class, slots);
                slots
            }
        };
        let values = &self.vm.instance(a).ok()?.values;
        let at = |slot: Option<usize>| slot.and_then(|s| values.get(s));
        Some(FirstLook {
            hidden: matches!(at(slots.hidden), Some(Value::Bool(true))),
            draw_type: match at(slots.draw_type) {
                Some(Value::Byte(b)) => *b,
                _ => 0,
            },
            brush: match at(slots.brush) {
                Some(Value::Object(Some(k))) => Some(*k),
                _ => None,
            },
            mesh: match at(slots.mesh) {
                Some(Value::Object(Some(k))) => Some(*k),
                _ => None,
            },
            location: match at(slots.location) {
                Some(v) => grim_vm::vm::floats3(v).unwrap_or([0.0; 3]),
                None => [0.0; 3],
            },
            draw_scale: match at(slots.draw_scale) {
                Some(Value::Float(v)) if *v > 0.0 => *v,
                _ => 1.0,
            },
        })
    }

    /// Whether a ball is worth drawing: what falls outside what the camera can see is left out
    /// before anything is asked of the renderer. Each side of the view is a plane through the
    /// camera, and the ball is dropped only when it lies wholly behind one of them — a ball
    /// that reaches across the camera is kept, which is what the room the player stands in does.
    fn in_view(&self, centre: [f32; 3], radius: f32) -> bool {
        if self.draw_everything || !radius.is_finite() {
            return true;
        }
        use grim_render::math::{dot, sub};
        let [forward, right, up] = grim_render::math::axes(self.camera.rotation);
        let to = sub(centre, self.camera.position);
        let depth = dot(to, forward);
        if depth < -radius || depth > self.camera.far + radius {
            return false;
        }
        let half_y = self.camera.fov_y_deg.to_radians() / 2.0;
        let aspect = self.viewport.0 as f32 / self.viewport.1.max(1) as f32;
        let half_x = (half_y.tan() * aspect).atan();
        // Each plane's inward normal, from the angle the view opens at.
        let sides = [
            (half_x, right, 1.0),
            (half_x, right, -1.0),
            (half_y, up, 1.0),
            (half_y, up, -1.0),
        ];
        for (half, axis, sign) in sides {
            let (sin, cos) = half.sin_cos();
            let normal = [0, 1, 2].map(|i| forward[i] * sin - axis[i] * cos * sign);
            if dot(to, normal) < -radius {
                return false;
            }
        }
        true
    }

    /// What the debug mode puts over the frame: how fast the frame is going, on the right. The
    /// script log is the game's own to show: its console prints it in green in debug mode. It is
    /// drawn with the game's own canvas, so it goes through the same font and the same overlay
    /// as everything else on screen.
    fn draw_debug_overlay(&mut self, canvas: ObjectKey, width: f32, height: f32) {
        const MARGIN: f32 = 6.0;
        let font = match self.vm.get_prop(canvas, "SmallFont") {
            Ok(v @ Value::Object(Some(_))) => v,
            _ => return,
        };
        let set = |vm: &mut grim_vm::Vm, name: &str, v: Value| {
            let _ = vm.set_prop(canvas, name, v);
        };
        set(&mut self.vm, "Font", font);
        set(&mut self.vm, "bCenter", Value::Bool(false));
        set(&mut self.vm, "Style", Value::Byte(grim_vm::canvas::STY_TRANSLUCENT));
        set(&mut self.vm, "OrgX", Value::Float(0.0));
        set(&mut self.vm, "OrgY", Value::Float(0.0));
        set(&mut self.vm, "ClipX", Value::Float(width));
        set(&mut self.vm, "ClipY", Value::Float(height));
        let white = Value::Struct(vec![Value::Byte(255), Value::Byte(255), Value::Byte(255), Value::Byte(0)]);
        set(&mut self.vm, "DrawColor", white);

        // How fast it is going, on the right: frames a second and how long each one takes.
        let fps = if self.frame_average > 0.0 { 1.0 / self.frame_average } else { 0.0 };
        let (gather, draw, items, sprites) = self.frame_costs;
        let lines = [
            format!("{fps:.0} fps   {:.1} ms", self.frame_average * 1000.0),
            format!("game {:.1} ms", self.tick_cost),
            format!("scene {gather:.1} ms   draw {draw:.1} ms"),
            format!("{items} meshes   {sprites} sprites"),
            format!("{} + {} left out", self.frame_culled.0, self.frame_culled.1),
            format!("{} mirrors ({} left out)", self.frame_culled.2, self.frame_culled.3),
            format!("{} of {} zones", self.zones_drawn, self.zone_meshes.len()),
            format!("{} actors", self.vm.actors.len()),
        ];
        let mut y = MARGIN;
        for text in lines {
            let size = self.vm.canvas_text_size(&text);
            set(&mut self.vm, "CurX", Value::Float((width - size[0] - MARGIN).max(0.0)));
            set(&mut self.vm, "CurY", Value::Float(y));
            let _ = self.vm.canvas_draw_text(&text, true);
            y += size[1].max(1.0);
        }
        self.draw_axes(canvas, width, height);
    }

    /// The world's three axes in the bottom left corner, turned the way the camera is looking:
    /// X red, Y green, Z blue, the bright arm towards the positive side and the dim one
    /// towards the negative. Which way round a map's coordinates run is the first thing any
    /// question about where something is drawn comes down to.
    fn draw_axes(&mut self, canvas: ObjectKey, width: f32, height: f32) {
        /// How long an arm is drawn, in pixels.
        const ARM: f32 = 42.0;
        /// How thick the line is.
        const DOT: f32 = 2.0;
        let origin = [MARGIN_AXES + ARM, height - MARGIN_AXES - ARM];
        const MARGIN_AXES: f32 = 16.0;
        let [forward, right, up] = grim_render::math::axes(self.camera.rotation);
        // Where a world direction points on the screen: along the camera's own right and up,
        // with the picture's y running down. What is straight towards the eye draws as a dot.
        let on_screen = |v: [f32; 3]| [grim_render::math::dot(v, right), -grim_render::math::dot(v, up)];
        let axes: [([f32; 3], [f32; 4], &str); 3] = [
            ([1.0, 0.0, 0.0], [1.0, 0.25, 0.25, 1.0], "x"),
            ([0.0, 1.0, 0.0], [0.3, 1.0, 0.3, 1.0], "y"),
            ([0.0, 0.0, 1.0], [0.4, 0.55, 1.0, 1.0], "z"),
        ];
        let _ = forward;
        for (dir, colour, name) in axes {
            let d = on_screen(dir);
            for (sign, shade) in [(1.0f32, 1.0f32), (-1.0, 0.35)] {
                let tip = [origin[0] + d[0] * ARM * sign, origin[1] + d[1] * ARM * sign];
                let steps = ARM as i32;
                for k in 0..=steps {
                    let t = k as f32 / steps as f32;
                    self.vm.canvas_quads.push(grim_vm::canvas::CanvasQuad {
                        rect: [origin[0] + (tip[0] - origin[0]) * t - DOT / 2.0, origin[1] + (tip[1] - origin[1]) * t - DOT / 2.0, DOT, DOT],
                        uv: [0.0; 4],
                        texture: None,
                        color: [colour[0] * shade, colour[1] * shade, colour[2] * shade, 1.0],
                        masked: false,
                        style: grim_vm::canvas::STY_NORMAL,
                    });
                }
                // The letter at the far end, with a minus in front of it on the dim side.
                let label = if sign > 0.0 { name.to_string() } else { format!("-{name}") };
                let size = self.vm.canvas_text_size(&label);
                let _ = self.vm.set_prop(canvas, "CurX", Value::Float((tip[0] - size[0] / 2.0).clamp(0.0, width - size[0])));
                let _ = self.vm.set_prop(canvas, "CurY", Value::Float((tip[1] - size[1] / 2.0).clamp(0.0, height - size[1])));
                let colour = Value::Struct(vec![
                    Value::Byte((colour[0] * shade * 255.0) as u8),
                    Value::Byte((colour[1] * shade * 255.0) as u8),
                    Value::Byte((colour[2] * shade * 255.0) as u8),
                    Value::Byte(0),
                ]);
                let _ = self.vm.set_prop(canvas, "DrawColor", colour);
                let _ = self.vm.canvas_draw_text(&label, true);
            }
        }
    }

    /// The shapes the collision actually uses, as a cage of thin bars: what blocks the way
    /// this tick and the player's own box. It is rebuilt every frame into one mesh, because
    /// the movers carry theirs around with them.
    fn collision_draw(&mut self) -> Option<ActorDraw> {
        /// How thick a bar of the cage is drawn, in world units.
        const BAR: f32 = 1.5;
        let mut edges: Vec<([f32; 3], [f32; 3])> = Vec::new();
        let corners = |centre: [f32; 3], half: [f32; 3], yaw: f32| -> [[f32; 3]; 8] {
            let (sin, cos) = yaw.sin_cos();
            [0, 1, 2, 3, 4, 5, 6, 7].map(|k: usize| {
                let s = [k & 1, (k >> 1) & 1, (k >> 2) & 1].map(|b| if b == 0 { -1.0 } else { 1.0 });
                let d = [half[0] * s[0], half[1] * s[1]];
                [centre[0] + d[0] * cos - d[1] * sin, centre[1] + d[0] * sin + d[1] * cos, centre[2] + half[2] * s[2]]
            })
        };
        const CAGE: [(usize, usize); 12] =
            [(0, 1), (1, 3), (3, 2), (2, 0), (4, 5), (5, 7), (7, 6), (6, 4), (0, 4), (1, 5), (2, 6), (3, 7)];
        for blocker in self.vm.blockers() {
            match &blocker.shape {
                grim_vm::level::Shape::Box { centre, half, yaw } => {
                    let c = corners(*centre, *half, *yaw);
                    edges.extend(CAGE.map(|(a, b)| (c[a], c[b])));
                }
                grim_vm::level::Shape::Brush { brush, at, axes, pivot, .. } => {
                    // A mover blocks with its own faces, so those are what is drawn.
                    let (at, axes, pivot, brush) = (*at, *axes, *pivot, *brush);
                    let out = |local: [f32; 3]| {
                        let l = [0, 1, 2].map(|i| local[i] - pivot[i]);
                        [0, 1, 2].map(|c| at[c] + axes[0][c] * l[0] + axes[1][c] * l[1] + axes[2][c] * l[2])
                    };
                    let pkg = self.vm.world.package(brush.package).clone();
                    let Some(grim_assets::Asset::Model(model)) = grim_assets::load_export(&pkg, brush.export).ok().map(|l| l.asset)
                    else {
                        continue;
                    };
                    for node in &model.nodes {
                        if node.num_vertices < 3 {
                            continue;
                        }
                        let point = |k: i32| -> Option<[f32; 3]> {
                            let vert = model.verts.get((node.i_vert_pool + k) as usize)?;
                            let p = model.points.get(vert.p_vertex.max(0) as usize)?;
                            Some(out([p.x, p.y, p.z]))
                        };
                        for k in 0..node.num_vertices as i32 {
                            let next = (k + 1) % node.num_vertices as i32;
                            if let (Some(a), Some(b)) = (point(k), point(next)) {
                                edges.push((a, b));
                            }
                        }
                    }
                }
            }
        }
        // And the player's own box, which is what all of it is tested against.
        if let Some(p) = self.vm.player {
            if let (Ok(at), Ok(Value::Float(radius)), Ok(Value::Float(height))) = (
                self.vm.get_prop(p, "Location").and_then(|v| grim_vm::vm::floats3(&v)),
                self.vm.get_prop(p, "CollisionRadius"),
                self.vm.get_prop(p, "CollisionHeight"),
            ) {
                let c = corners(at, [radius, radius, height], 0.0);
                edges.extend(CAGE.map(|(a, b)| (c[a], c[b])));
            }
        }
        // Every edge becomes a thin bar of eight corners, so the cage draws with the ordinary
        // mesh pipeline instead of needing lines of its own.
        let mut data = grim_render::MeshData::default();
        for (from, to) in edges {
            let along = grim_world::sub(to, from);
            let length = grim_world::dot(along, along).sqrt();
            if length < 1e-3 {
                continue;
            }
            let n = grim_world::scale(along, 1.0 / length);
            let cross = |a: [f32; 3], b: [f32; 3]| [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]];
            let helper = if n[2].abs() < 0.9 { [0.0, 0.0, 1.0] } else { [1.0, 0.0, 0.0] };
            let across = cross(n, helper);
            let u = grim_world::scale(across, BAR / grim_world::dot(across, across).sqrt().max(1e-3));
            let side = cross(n, u);
            let v = grim_world::scale(side, BAR / grim_world::dot(side, side).sqrt().max(1e-3));
            let first = data.vertices.len() as u32;
            for end in [from, to] {
                for (a, b) in [(1.0, 1.0), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0)] {
                    let p = [0, 1, 2].map(|i| end[i] + u[i] * a + v[i] * b);
                    data.vertices.push(grim_render::Vertex { position: p, uv: [0.0; 2], normal: n, color: [1.0; 3], uv2: [0.0; 2] });
                }
            }
            for k in 0..4u32 {
                let (a, b) = (k, (k + 1) % 4);
                data.indices.extend([first + a, first + b, first + 4 + b, first + a, first + 4 + b, first + 4 + a]);
            }
        }
        if data.vertices.is_empty() {
            return None;
        }
        data.sections.push(grim_render::Section {
            first_index: 0,
            index_count: data.indices.len() as u32,
            texture: None,
            masked: false,
            material: 0,
            two_sided: true,
            blend: grim_render::Blend::Translucent,
            pan: [0.0; 2],
        });
        let renderer = self.renderer.as_mut()?;
        let mesh = match self.collision_mesh {
            Some(mesh) => {
                renderer.update_mesh(mesh, &data);
                mesh
            }
            None => {
                let mesh = renderer.create_mesh(&data);
                self.collision_mesh = Some(mesh);
                mesh
            }
        };
        Some(ActorDraw {
            centre: [0.0; 3],
            radius: f32::INFINITY,
            actor: None,
            mesh,
            lightmap: None,
            transform: grim_render::math::IDENTITY,
            alpha_test: None,
            skins: Vec::new(),
            blend: Some(grim_render::Blend::Translucent),
            tint: [0.2, 1.0, 0.4],
            lights: Default::default(),
            env_map: false,
        })
    }

    /// Draw data for every visible actor that has a mesh.
    fn actor_draws(&mut self) -> Vec<ActorDraw> {
        let mut items = Vec::new();
        // What ends up drawn, for the engine: a decal is brought along by the drawing of what
        // casts it, so a shadow only stays on the ground while its owner is on the screen.
        let mut drawn = grim_object::FastSet::default();
        let hidden: Vec<String> =
            std::env::var("GRIM_HIDE").unwrap_or_default().split(',').filter(|w| !w.is_empty()).map(|w| w.to_lowercase()).collect();

        for a in self.vm.actors.clone() {
            if self.vm.is_deleted(a) {
                continue;
            }
            // Everything the first look at an actor needs, read in one go by the slots its
            // class keeps them in: over a couple of thousand actors a frame, looking each
            // property up by name is most of what putting the frame together costs.
            let Some(look) = self.first_look(a) else { continue };
            // What the scripts draw over the world is drawn even when it is hidden in it: a
            // prop Harry carries takes itself out of the world and puts itself on the screen
            // (`HProp.RenderHud` through `Canvas.DrawActor`).
            let over_the_world = self.vm.canvas_actors.contains(&a);
            if (look.hidden && !over_the_world) || (look.draw_type != DT_MESH && look.draw_type != DT_BRUSH) {
                continue;
            }
            let draw_type = look.draw_type;
            // GRIM_NO_MOVERS leaves the movers out of the drawing, to tell what belongs to
            // them apart from what the level itself holds.
            if draw_type == DT_BRUSH && std::env::var_os("GRIM_NO_MOVERS").is_some() {
                continue;
            }
            let brush = look.brush;
            // Off screen is decided before anything is built: posing a mesh, working out what
            // lights it and looking its skins up is most of what a frame costs.
            if brush.is_none() {
                let (mesh_key, at, draw_scale) = (look.mesh, look.location, look.draw_scale);
                let reach = mesh_key
                    .and_then(|k| self.vm.mesh_extent(k))
                    .map_or(f32::INFINITY, |size| size.into_iter().fold(0.0f32, f32::max) * draw_scale);
                // What only a mirror shows is kept. This is the test that decides whether the
                // actor is posed at all, so a reflection would otherwise never see it: Dobby
                // stands off to the side of the shot that frames Harry's mirror.
                let planes = std::mem::take(&mut self.frame_mirrors);
                let shown = self.in_view(at, reach) || self.seen_in_a_mirror(&planes, at, reach);
                self.frame_mirrors = planes;
                if !shown {
                    continue;
                }
            }
            let pivot = self.vm.get_prop(a, "PrePivot").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
            let mesh = match brush {
                Some(b) => {
                    let at = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
                    let rot = self.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).unwrap_or([0; 3]);
                    self.brush_mesh(b, pivot, at, rot)
                }
                None => match self.vm.get_prop(a, "Mesh") {
                    // An actor playing a sequence moves its own copy of the mesh.
                    Ok(Value::Object(Some(k))) => self.animated_mesh(a, k).or_else(|| self.actor_mesh(k)),
                    _ => None,
                },
            };
            let Some(mesh) = mesh else { continue };
            // GRIM_HIDE=<name>,<name> leaves those actors out of the drawing, which is how
            // what is behind one of them gets looked at: the light the map baked under the
            // whomping willow says where the tree stood when the level was lit.
            if hidden.iter().any(|w| self.vm.object_name(a).to_lowercase().contains(w)) {
                continue;
            }
            // The console's `hideactors` leaves the level alone, with what the HUD draws.
            if self.vm.hide_actors && !over_the_world {
                continue;
            }
            drawn.insert(a);
            let Ok(mut location) = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)) else { continue };
            let Ok(rotation) = self.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)) else { continue };
            // A brush is lit at the middle of its own geometry, not at its pivot.
            let light_point = match brush.and_then(|b| self.brush_centres.get(&b).copied()) {
                Some(c) => {
                    let [f, r, u] = grim_render::math::axes(rotation);
                    [0, 1, 2].map(|i| location[i] + f[i] * c[0] + r[i] * c[1] + u[i] * c[2])
                }
                None => location,
            };
            let scale = match self.vm.get_prop(a, "DrawScale") {
                Ok(Value::Float(s)) if s > 0.0 => s,
                _ => 1.0,
            };
            // An actor the engine moves stands on the bottom of its own collision cylinder;
            // one that only sits where the map put it keeps the middle of its box on its
            // location. Hypothesis, measured over the 2878 props of twelve maps that have a
            // floor under them: of those with physics of their own, 626 of 634 land within
            // two units on the cylinder against 8 on the box, and of those with none, 466 of
            // 726 go the other way. It is what leaves the armour of `EntryHall_hub` standing
            // on its plinth instead of sunk 13 units into it.
            // Where the mesh is anchored, by the one rule the engine has for it: across the
            // floor a mesh stands on its own origin, and in height an actor the engine moves
            // stands on the bottom of its collision cylinder while one that only sits where
            // the map put it keeps the middle of its box. A brush is placed by its own pivot
            // and keeps out of it.
            if brush.is_none() {
                let centre = self.mesh_centres.get(&mesh).copied().unwrap_or([0.0; 3]);
                let low = self.mesh_lows.get(&mesh).copied().unwrap_or(0.0);
                location = self.vm.mesh_anchor(a, centre, low, scale);
            }
            // Style 2 is STY_Masked: it turns masking on for the whole actor, but never off
            // for materials that already ask for it.
            let masked = matches!(self.vm.get_prop(a, "Style"), Ok(Value::Byte(2)));
            // Styles: 3 is STY_Translucent, 4 is STY_Modulated.
            let blend = match self.vm.get_prop(a, "Style") {
                Ok(Value::Byte(3)) => Some(grim_render::Blend::Translucent),
                Ok(Value::Byte(4)) => Some(grim_render::Blend::Modulated),
                _ => None,
            };
            let env_map = matches!(self.vm.get_prop(a, "bMeshEnviroMap"), Ok(Value::Bool(true)));
            let mut skins = self.actor_skins(a);
            // A mesh drawn as an environment map shows the actor's own `Texture` wrapped
            // around it, over every material, not its skins: the ectoplasm's picture is
            // `EctoplasmFX`, which it carries in `Texture`, and with its skins wrapped by the
            // normals instead it came out as a patchwork of whatever its materials held.
            if env_map {
                if let Ok(Value::Object(Some(k))) = self.vm.get_prop(a, "Texture") {
                    if let Some(renderer) = self.renderer.as_mut() {
                        let pkg = self.vm.world.package(k.package).clone();
                        if let Some((id, ..)) = self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, k.export)) {
                            skins = vec![Some(id); skins.len().max(8)];
                        }
                    }
                }
            }
            // Unlit actors keep their texture's own colour.
            let unlit = matches!(self.vm.get_prop(a, "bUnlit"), Ok(Value::Bool(true))) || blend.is_some();
            let (mut tint, lights) = if unlit {
                ([1.0; 3], Default::default())
            } else {
                self.cached_light(a, light_point, brush.is_none())
            };
            // An actor lights itself with AmbientGlow, and ScaleGlow scales the whole of it.
            let glow = self.byte_prop(a, "AmbientGlow") as f32 / 255.0;
            let scale_glow = match self.vm.get_prop(a, "ScaleGlow") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            tint = tint.map(|c| (c.max(glow) * scale_glow).min(1.0));
            let baked = self.brush_lightmaps.get(&mesh).copied();
            // What a lightmap lights needs no light of its own.
            let tint = if baked.is_some() { [1.0; 3] } else { tint };
            // A ball that holds whatever is drawn: the mesh's own box, or the whole world for
            // a brush, whose size is not known here and which is better drawn than dropped.
            let radius = match brush {
                Some(_) => f32::INFINITY,
                None => match self.vm.get_prop(a, "Mesh") {
                    Ok(Value::Object(Some(key))) => match self.vm.mesh_extent(key) {
                        Some(size) => size.into_iter().fold(0.0f32, f32::max) * scale,
                        None => f32::INFINITY,
                    },
                    _ => f32::INFINITY,
                },
            };
            items.push(ActorDraw {
                actor: Some(a),
                centre: location,
                radius,
                mesh,
                lightmap: baked,
                // Brush polygons are already in world axes; meshes are stored mirrored on Y.
                transform: match brush {
                    Some(_) => grim_render::math::place(location, rotation),
                    None => grim_render::math::transform(location, rotation, [scale; 3]),
                },
                alpha_test: masked.then_some(true),
                skins,
                blend,
                tint,
                lights,
                env_map,
            });
        }
        items.extend(self.weapon_draws(&drawn));
        self.vm.drawn = Some(drawn);
        items
    }

    /// The mirrors the movers of the level carry, with where each one stands.
    /// Whether a mirror shows a ball the camera itself cannot see: the reflection is drawn from
    /// behind the glass, so what stands in front of a mirror is in the picture even when it is
    /// off the edge of the screen.
    fn seen_in_a_mirror(&self, planes: &[[f32; 4]], centre: [f32; 3], radius: f32) -> bool {
        planes.iter().any(|p| {
            let n = [p[0], p[1], p[2]];
            let away = grim_render::math::dot(n, centre) - p[3];
            let mirrored = [0, 1, 2].map(|i| centre[i] - 2.0 * away * n[i]);
            self.in_view(mirrored, radius)
        })
    }

    /// The planes every mirror of the level reflects about, the level's own and the ones the
    /// movers carry. Gathered before the culling so what only a mirror shows is kept.
    /// A `ScriptedTexture` is drawn by the game's own scripts: the engine asks its `NotifyActor`
    /// to fill it in, and hands the result to the renderer. The house point scores of the
    /// entrance hall are four of these (`CeremonySTextures`).
    fn draw_scripted_textures(&mut self) {
        // Which objects they are is settled when the level begins play, so the heap is walked
        // for them once rather than every frame.
        let textures = match &self.scripted_textures {
            Some(t) => t.clone(),
            None => {
                let found = self.vm.objects_of_class("ScriptedTexture");
                self.scripted_textures = Some(found.clone());
                found
            }
        };
        for texture in textures {
            let Ok(Value::Object(Some(actor))) = self.vm.get_prop(texture, "NotifyActor") else { continue };
            self.vm.scripted_clear(texture);
            if let Err(e) = self.vm.call_event(actor, "RenderTexture", vec![Value::Object(Some(texture))]) {
                self.report.record(e);
            }
            let Some(picture) = self.vm.scripted.get(&texture).filter(|p| p.dirty) else { continue };
            let (width, height, rgba) = (picture.width, picture.height, picture.rgba.clone());
            let pkg = self.vm.world.package(texture.package).clone();
            let Some(renderer) = self.renderer.as_mut() else { continue };
            let Some((id, _, _, _)) = self.textures.get_key(renderer, &self.vm.world.lib, (&pkg, texture.export)) else {
                continue;
            };
            renderer.update_texture(id, grim_render::TextureData { width, height, rgba: &rgba, mips: &[] });
            if let Some(picture) = self.vm.scripted.get_mut(&texture) {
                picture.dirty = false;
            }
        }
    }

    fn mirror_planes(&mut self) -> Vec<[f32; 4]> {
        let mut planes: Vec<[f32; 4]> = self.mirrors.iter().map(|(_, plane)| *plane).collect();
        for (_, plane, _, location, rotation) in self.brush_mirror_draws() {
            let [f, r, u] = grim_render::math::axes(rotation);
            let n = [
                f[0] * plane[0] + r[0] * plane[1] + u[0] * plane[2],
                f[1] * plane[0] + r[1] * plane[1] + u[1] * plane[2],
                f[2] * plane[0] + r[2] * plane[1] + u[2] * plane[2],
            ];
            let d = plane[3] + n[0] * location[0] + n[1] * location[1] + n[2] * location[2];
            planes.push([n[0], n[1], n[2], d]);
        }
        planes
    }

    fn brush_mirror_draws(&mut self) -> Vec<(MeshId, [f32; 4], grim_render::math::Mat4, [f32; 3], [i32; 3])> {
        let mut out = Vec::new();
        let movers = match &self.brush_actors {
            Some(found) => found.clone(),
            None => {
                let found: Vec<ObjectKey> = self
                    .vm
                    .actors
                    .clone()
                    .into_iter()
                    .filter(|&a| matches!(self.vm.get_prop_id(a, self.vm.hot.brush), Ok(Value::Object(Some(_)))))
                    .collect();
                self.brush_actors = Some(found.clone());
                found
            }
        };
        for a in movers {
            if self.vm.is_deleted(a) || matches!(self.vm.get_prop(a, "bHidden"), Ok(Value::Bool(true))) {
                continue;
            }
            let Ok(Value::Object(Some(brush))) = self.vm.get_prop(a, "Brush") else { continue };
            let Some(mirrors) = self.brush_mirrors.get(&brush) else { continue };
            if mirrors.is_empty() {
                continue;
            }
            let (Ok(location), Ok(rotation)) = (
                self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)),
                self.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)),
            ) else {
                continue;
            };
            let transform = grim_render::math::place(location, rotation);
            for &(mesh, plane) in mirrors {
                out.push((mesh, plane, transform, location, rotation));
            }
        }
        out
    }

    /// What a pawn holds. The weapon actor itself draws nothing once it is carried, because
    /// `Inventory.BecomeItem` gives it the mesh of the first person view, which this game has
    /// none of; the one seen from outside is `ThirdPersonMesh`, put on the weapon bone.
    ///
    /// Only the pawns drawn this frame are looked at: a weapon is drawn in its holder's hand.
    fn weapon_draws(&mut self, holders: &grim_object::FastSet<ObjectKey>) -> Vec<ActorDraw> {
        let mut items = Vec::new();
        for a in holders.iter().copied().collect::<Vec<_>>() {
            if self.vm.is_deleted(a) {
                continue;
            }
            let Ok(Value::Object(Some(weapon))) = self.vm.get_prop(a, "Weapon") else { continue };
            if matches!(self.vm.get_prop(weapon, "bHidden"), Ok(Value::Bool(true))) {
                continue;
            }
            let Ok(Value::Object(Some(mesh_key))) = self.vm.get_prop(weapon, "ThirdPersonMesh") else { continue };
            let (Ok(at), Ok(rot)) = (
                self.vm.get_prop(a, "WeaponLoc").and_then(|v| grim_vm::vm::floats3(&v)),
                self.vm.get_prop(a, "WeaponRot").and_then(|v| grim_vm::vm::ints3(&v)),
            ) else {
                continue;
            };
            let Some(mesh) = self.actor_mesh(mesh_key) else { continue };
            let scale = match self.vm.get_prop(weapon, "ThirdPersonScale") {
                Ok(Value::Float(v)) if v > 0.0 => v,
                _ => 1.0,
            };
            // A mesh drawn on its own is placed by the centre of its box, which is how a prop
            // rests on the floor; one held in a hand goes by its own origin instead. The wand
            // is modelled from the grip down 20 units of -Z, which is exactly the offset
            // `baseWand.GetWandEndPoint` takes from the weapon bone to reach its tip.
            // Its vertices were stored with that centre taken out of them, so putting the
            // mesh's own origin on the bone means putting it back.
            let centre = self.mesh_centres.get(&mesh).copied().unwrap_or([0.0; 3]);
            let axes = grim_vm::natives::core::axes(rot);
            let mut at = at;
            for c in 0..3 {
                at[c] += (axes[0][c] * centre[0] - axes[1][c] * centre[1] + axes[2][c] * centre[2]) * scale;
            }
            let (tint, lights) = self.light_at(at, false);
            let radius = match self.vm.get_prop(weapon, "Mesh") {
                Ok(Value::Object(Some(key))) => match self.vm.mesh_extent(key) {
                    Some(size) => size.into_iter().fold(0.0f32, f32::max) * scale,
                    None => f32::INFINITY,
                },
                _ => f32::INFINITY,
            };
            items.push(ActorDraw {
                actor: Some(weapon),
                centre: at,
                radius,
                mesh,
                lightmap: None,
                transform: grim_render::math::transform(at, rot, [scale; 3]),
                alpha_test: None,
                skins: self.actor_skins(weapon),
                blend: None,
                tint,
                lights,
                env_map: false,
            });
        }
        items
    }

    /// For actors with a mesh: where the floor is under them, where their mesh sits, and the
    /// offset that would drop the mesh onto that floor.
    fn report_placement(&mut self) {
        let filter = std::env::var("GRIM_DEBUG_PLACEMENT").unwrap_or_default();
        let mut shown = 0;
        for a in self.vm.actors.clone() {
            let Ok(Value::Object(Some(mesh_key))) = self.vm.get_prop(a, "Mesh") else { continue };
            let Ok(loc) = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)) else { continue };
            let class = self.vm.class_of(a).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
            if filter != "1" && !class.to_lowercase().contains(&filter.to_lowercase()) {
                continue;
            }
            let scale = match self.vm.get_prop(a, "DrawScale") {
                Ok(Value::Float(s)) if s > 0.0 => s,
                _ => 1.0,
            };
            let height = match self.vm.get_prop(a, "CollisionHeight") {
                Ok(Value::Float(h)) => h,
                _ => 0.0,
            };
            // Mesh extent in Z, from its reference pose.
            let pkg = self.vm.world.package(mesh_key.package).clone();
            let Some(data) = self.renderer.as_mut().map(|r| grim_world::scene::actor_mesh(r, &self.vm.world.lib, &pkg, mesh_key.export, &mut self.textures)) else { continue };
            let Some(data) = data else { continue };
            let min_z = data.data.vertices.iter().fold(f32::MAX, |lo, v| lo.min(v.position[2]));
            // What the mesh had taken out of it to be centred, which the origin rule puts back.
            let center = data.centre[2];
            let floor = self
                .vm
                .bsp
                .as_ref()
                .and_then(|b| b.line_check(loc, [loc[0], loc[1], loc[2] - 4096.0]))
                .map(|h| h.location[2]);
            let Some(floor) = floor else { continue };
            let feet = loc[2] + min_z * scale;
            // Three ways of putting a mesh on its actor, against the floor under it: with the
            // middle of its box on the actor (what the engine does), with its own origin
            // there (Unreal's own way), and standing on the bottom of the collision cylinder.
            println!(
                "PLACE {class:22} box {:7.1} origin {:7.1} cylinder {:7.1} sideways {:6.1} height {height:5.0} scale {scale:4.1} physics {:?} movable {:?} static {:?} collideworld {:?}",
                feet - floor,
                feet - floor + center * scale,
                loc[2] - height - floor,
                // How far centring the mesh on its box would carry it across the floor.
                (data.centre[0].powi(2) + data.centre[1].powi(2)).sqrt() * scale,
                self.vm.get_prop(a, "Physics"),
                self.vm.get_prop(a, "bMovable"),
                self.vm.get_prop(a, "bStatic"),
                self.vm.get_prop(a, "bCollideWorld"),
            );
            shown += 1;
            if shown >= 400 {
                break;
            }
        }
    }

    /// Counts actor sections that would draw untextured, for diagnosing skins.
    fn report_untextured(&mut self, draws: &[ActorDraw]) {
        let (mut sections, mut untextured) = (0, 0);
        let mut named: Vec<String> = Vec::new();
        for d in draws {
            for (i, s) in self.mesh_sections.get(&d.mesh).into_iter().flatten().enumerate() {
                sections += 1;
                let skin = d.skins.get(*s as usize).copied().flatten();
                if skin.is_none() && !self.section_has_texture[&d.mesh][i] {
                    untextured += 1;
                    if let Some(a) = d.actor {
                        let name = self
                            .vm
                            .class_of(a)
                            .map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string())
                            .unwrap_or_default();
                        if !named.contains(&name) {
                            named.push(name);
                        }
                    }
                }
            }
        }
        println!("{} actor draws, {sections} sections, {untextured} without texture: {named:?}", draws.len());
    }

    /// The surface the camera looks at, with the lights its lightmap holds.
    fn report_surface_ahead(&mut self) {
        let pkg = self.vm.world.package(self.level.level.package).clone();
        let Some((_, model)) = grim_world::scene::level_model(&pkg) else { return };
        let [forward, right, up] = grim_render::math::axes(self.camera.rotation);
        // GRIM_DEBUG_SURFACE=x,y aims at that pixel of a 1280x720 frame instead of the centre.
        let pixel: Vec<f32> = std::env::var("GRIM_DEBUG_SURFACE")
            .unwrap_or_default()
            .split(',')
            .filter_map(|v| v.parse().ok())
            .collect();
        let mut dir = forward;
        if let [x, y] = pixel[..] {
            let half = (self.camera.fov_y_deg.to_radians() / 2.0).tan();
            let (sx, sy) = ((2.0 * x / 1280.0 - 1.0) * half * 1280.0 / 720.0, (1.0 - 2.0 * y / 720.0) * half);
            dir = grim_render::math::add(forward, grim_render::math::add(grim_render::math::scale(right, sx), grim_render::math::scale(up, sy)));
        }
        let end = grim_render::math::add(self.camera.position, grim_render::math::scale(dir, 8192.0));
        let Some(hit) = self.vm.bsp.as_ref().and_then(|b| b.line_check(self.camera.position, end)) else {
            println!("nothing ahead");
            return;
        };
        let Some(surf) = model.surfs.get(hit.surf.max(0) as usize) else { return };
        let texture = pkg.object_name(surf.texture).to_string();
        let lm = model.light_map.get(surf.i_light_map.max(0) as usize);
        let mut lights = Vec::new();
        if let Some(lm) = lm.filter(|_| surf.i_light_map >= 0) {
            let mut k = lm.i_light_actors.max(0) as usize;
            while model.lights.get(k).is_some_and(|r| !r.is_null()) {
                if let ObjectRef::Export(e) = model.lights[k] {
                    let actor = ObjectKey { package: self.level.level.package, export: e };
                    let loc = self.vm.get_prop(actor, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or_default();
                    let distance = grim_world::dot(grim_world::sub(loc, hit.location), grim_world::sub(loc, hit.location)).sqrt();
                    let class = self.vm.class_of(actor).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                    lights.push(format!(
                        "export {e} {class}: type {} brightness {} radius {} ({:.0} units away, reach {:.0})",
                        self.byte_prop(actor, "LightType"),
                        self.byte_prop(actor, "LightBrightness"),
                        self.byte_prop(actor, "LightRadius"),
                        distance,
                        self.byte_prop(actor, "LightRadius") as f32 * 25.0,
                    ));
                }
                k += 1;
            }
        }
        if let Some(lm) = lm.filter(|_| surf.i_light_map >= 0) {
            let stride = (lm.u_clamp as usize).div_ceil(8);
            let mut k = lm.i_light_actors.max(0) as usize;
            let mut index = 0;
            println!("  lightmap {}x{} at offset {}", lm.u_clamp, lm.v_clamp, lm.data_offset);
            while model.lights.get(k).is_some_and(|r| !r.is_null()) {
                let start = lm.data_offset.max(0) as usize + index * stride * lm.v_clamp.max(0) as usize;
                let end = start + stride * lm.v_clamp.max(0) as usize;
                let set: u32 = model.light_bits[start.min(model.light_bits.len())..end.min(model.light_bits.len())]
                    .iter()
                    .map(|b| b.count_ones())
                    .sum();
                println!("    light {index}: {set} of {} texels lit", lm.u_clamp * lm.v_clamp);
                k += 1;
                index += 1;
            }
        }
        println!(
            "surface {} at {:?}: texture {texture} flags {:08x} lightmap {} ({} lights)",
            hit.surf, hit.location, surf.poly_flags, surf.i_light_map, lights.len()
        );
        for l in lights.iter().take(8) {
            println!("  {l}");
        }
    }

    fn render(&mut self) {
        let started = web_time::Instant::now();
        if self.zone_meshes.is_empty() {
            return;
        }
        // The textures the engine draws itself move on before anything is drawn with them.
        let tt = web_time::Instant::now();
        if let Some(renderer) = self.renderer.as_mut() {
            self.textures.tick(renderer, self.frame_time);
        }
        let t_textures = tt.elapsed().as_secs_f32() * 1000.0;
        let tt = web_time::Instant::now();
        self.draw_scripted_textures();
        let t_scripted = tt.elapsed().as_secs_f32() * 1000.0;
        let tt = web_time::Instant::now();
        self.frame_mirrors = self.mirror_planes();
        let t_mirrors = tt.elapsed().as_secs_f32() * 1000.0;
        let t0 = web_time::Instant::now();
        self.skinning = (0.0, 0);
        let mut draws = self.actor_draws();
        let t_draws = t0.elapsed().as_secs_f32() * 1000.0;
        if self.show_collision {
            draws.extend(self.collision_draw());
        }
        // What the camera cannot see is not handed to the renderer at all. The level itself
        // still goes whole: its geometry is one mesh, and splitting it by the zones the map
        // carries is a job of its own.
        let seen = draws.len();
        // What a mirror shows has to be kept even when the camera cannot see it itself: the
        // reflection is drawn from the other side of the glass, so an actor standing in front
        // of a mirror but off the edge of the screen is still in the picture. Dobby talking to
        // Harry in front of the bedroom mirror was being dropped here.
        let planes: Vec<[f32; 4]> = self.frame_mirrors.clone();
        // What the scripts drew over the world goes in its own list: it is not part of the
        // scene, so it is neither culled against the camera nor hidden behind anything.
        let over: Vec<ActorDraw> = match self.vm.canvas_actors.is_empty() {
            true => Vec::new(),
            false => {
                let wanted = self.vm.canvas_actors.clone();
                let (over, rest): (Vec<ActorDraw>, Vec<ActorDraw>) =
                    draws.into_iter().partition(|d| d.actor.is_some_and(|a| wanted.contains(&a)));
                draws = rest;
                over
            }
        };
        draws.retain(|d| self.in_view(d.centre, d.radius) || self.seen_in_a_mirror(&planes, d.centre, d.radius));
        let culled_meshes = seen - draws.len();
        if std::env::var_os("GRIM_DEBUG_DRAWS").is_some() {
            let names: Vec<String> = draws
                .iter()
                .filter_map(|d| d.actor)
                .map(|a| self.vm.object_name(a))
                .collect();
            println!("drawing {} of {seen}: {names:?}", draws.len());
        }
        if std::env::var_os("GRIM_DEBUG_SKINS").is_some() {
            self.report_untextured(&draws);
        }
        if std::env::var_os("GRIM_DEBUG_STATE").is_some() {
            let (mut inside, mut outside) = (0, 0);
            let state = self.vm.player.and_then(|p| self.vm.get_prop(p, "CurrentGameState").ok());
            for a in self.vm.actors.clone() {
                match self.vm.get_prop(a, "bInCurrentGameState") {
                    Ok(Value::Bool(true)) => inside += 1,
                    Ok(Value::Bool(false)) => outside += 1,
                    _ => {}
                }
            }
            println!("game state {state:?}: {inside} actors in state, {outside} out");
        }
        if std::env::var_os("GRIM_DEBUG_SURFACE").is_some() {
            self.report_surface_ahead();
        }
        if std::env::var_os("GRIM_DEBUG_PLACEMENT").is_some() {
            self.report_placement();
        }
        if std::env::var_os("GRIM_DEBUG_PLAYER").is_some() {
            if let Some(info) = self.vm.level_info {
                if let Ok(Value::Object(Some(game))) = self.vm.get_prop(info, "Game") {
                    let class = self.vm.class_of(game).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                    println!("game info class: {class}");
                }
            }
            if let Some(p) = self.vm.player {
                let class = self.vm.class_of(p).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                let state = self.vm.instance(p).ok().and_then(|i| i.state).map(|st| self.vm.world.names.str(self.vm.world.states[st.0 as usize].name).to_string());
                println!("player {class} state {state:?}");
                for prop in ["Location", "Physics", "myHUD", "Player", "bShowMenu", "Scoring", "HUDType"] {
                    println!("  {prop} = {:?}", self.vm.get_prop(p, prop));
                }
            } else {
                println!("no player");
            }
            // Why Login did or did not reuse a pawn that is already in the level.
            if let Some(info) = self.vm.level_info {
                let mut cur = match self.vm.get_prop(info, "PawnList") {
                    Ok(Value::Object(o)) => o,
                    _ => None,
                };
                let mut n = 0;
                while let Some(a) = cur {
                    let class = self.vm.class_of(a).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                    if Some(a) == self.vm.player || n < 3 {
                        let pri = self.vm.get_prop(a, "PlayerReplicationInfo");
                        let name = match &pri {
                            Ok(Value::Object(Some(k))) => self.vm.get_prop(*k, "PlayerName").ok(),
                            _ => None,
                        };
                        println!(
                            "  pawnlist {class}: Player {:?} bIsPlayer {:?} PRI {:?} name {:?}",
                            self.vm.get_prop(a, "Player"),
                            self.vm.get_prop(a, "bIsPlayer"),
                            pri.map(|v| matches!(v, Value::Object(Some(_)))),
                            name,
                        );
                    }
                    n += 1;
                    cur = match self.vm.get_prop(a, "nextPawn") {
                        Ok(Value::Object(o)) => o,
                        _ => None,
                    };
                    if n > 4000 {
                        break;
                    }
                }
                println!("  pawnlist length {n}");
            }
            // Pawns in the level, to see who the player is meant to be following.
            for a in self.vm.actors.clone() {
                let Ok(Value::Bool(true)) = self.vm.get_prop(a, "bIsPawn") else { continue };
                let class = self.vm.class_of(a).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                let loc = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or_default();
                let state = self.vm.instance(a).ok().and_then(|i| i.state).map(|st| self.vm.world.names.str(self.vm.world.states[st.0 as usize].name).to_string());
                println!("  pawn {class} at {loc:?} state {state:?}");
            }
        }
        if let Ok(tag) = std::env::var("GRIM_DEBUG_TAG") {
            for a in self.vm.actors.clone() {
                let name = match self.vm.get_prop(a, "Tag") {
                    Ok(Value::Name(n)) => self.vm.world.names.str(n).to_string(),
                    _ => continue,
                };
                if !name.eq_ignore_ascii_case(&tag) {
                    continue;
                }
                let class = self.vm.class_of(a).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                println!(
                    "tag {name}: {class} at {:?} facing {:?} scale {:?} lowest {:?} frame {:?} rate {:?} hidden {:?} style {:?} opacity {:?} drawtype {:?} mesh {:?} state {:?}",
                    self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or_default().map(|c| c.round()),
                    self.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).unwrap_or_default(),
                    self.vm.get_prop(a, "DrawScale"),
                    // Where the mesh reaches, as it is drawn: the lowest of its own points
                    // put where the actor stands.
                    self.animated.get(&a).map(|m| {
                        let low = m.posed.data.vertices.iter().map(|v| v.position[2]).fold(f32::MAX, f32::min);
                        let scale = match self.vm.get_prop(a, "DrawScale") {
                            Ok(Value::Float(s)) if s != 0.0 => s,
                            _ => 1.0,
                        };
                        let at = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or_default();
                        (at[2] + low * scale).round()
                    }),
                    self.vm.get_prop(a, "AnimFrame"),
                    self.vm.get_prop(a, "AnimRate"),
                    self.vm.get_prop(a, "bHidden"),
                    self.vm.get_prop(a, "Style"),
                    self.vm.get_prop(a, "Opacity"),
                    self.vm.get_prop(a, "DrawType"),
                    self.vm.get_prop(a, "Mesh").map(|m| matches!(m, Value::Object(Some(_)))),
                    self.vm.instance(a).ok().and_then(|i| i.state).map(|st| self.vm.world.names.str(self.vm.world.states[st.0 as usize].name).to_string()),
                );
            }
        }
        if std::env::var_os("GRIM_DEBUG_BLEND").is_some() {
            let mut shown = 0;
            for a in self.vm.actors.clone() {
                let style = match self.vm.get_prop(a, "Style") {
                    Ok(Value::Byte(b)) if b >= 3 => b,
                    _ => continue,
                };
                let filter = std::env::var("GRIM_DEBUG_BLEND").unwrap_or_default();
                let Ok(loc) = self.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)) else { continue };
                let class = self.vm.class_of(a).map(|c| self.vm.world.names.str(self.vm.world.class(c).name).to_string()).unwrap_or_default();
                if filter != "1" && !class.to_lowercase().contains(&filter.to_lowercase()) {
                    continue;
                }
                println!("style {style}: {class:?} at {loc:?}");
                shown += 1;
                if shown >= 400 {
                    break;
                }
            }
        }
        // The level goes in a zone at a time: the tree says which leaf the eye is in and the
        // leaf says which zones it can see, so the rooms behind a wall are never drawn. A map
        // with no zoning of its own keeps its single zone, which is everything.
        let at_leaf = self.vm.bsp.as_ref().and_then(|b| b.leaf_at(self.camera.position));
        let seen = match at_leaf {
            Some((leaf, zone)) => {
                let visible = self.zone_visibility.get(leaf.max(0) as usize).copied().unwrap_or(u64::MAX);
                visible | (1u64 << zone)
            }
            None => u64::MAX,
        };
        if std::env::var_os("GRIM_DEBUG_ZONES").is_some() {
            println!("camera leaf {at_leaf:?} sees {seen:#x}");
            for (zone, _, centre, radius) in self.zone_meshes.clone() {
                println!(
                    "  zone {zone} centre {:?} radius {radius} seen {} in view {}",
                    centre.map(|v| v.round()),
                    seen & (1u64 << zone) != 0,
                    self.in_view(centre, radius)
                );
            }
        }
        let mut items: Vec<DrawItem> = self
            .zone_meshes
            .iter()
            .filter(|(zone, _, centre, radius)| {
                // A zone is drawn when the eye's own zone can see it and it is in front of the
                // camera, or when a mirror shows it. The maps ship with every zone marked
                // visible from every other, so what actually keeps the room behind you out is
                // the second test.
                (seen & (1u64 << *zone) != 0) && (self.in_view(*centre, *radius) || self.seen_in_a_mirror(&planes, *centre, *radius))
            })
            .map(|(.., mesh, _, _)| DrawItem {
                mesh: *mesh,
                transform: grim_render::math::IDENTITY,
                alpha_test: None,
                skins: &[],
                blend: None,
                tint: [1.0; 3],
                lights: Default::default(),
                env_map: false,
                lightmap: self.lightmap,
            })
            .collect();
        // Hypothesis: lit, the surfaces show whole. The original works out an alpha for them
        // from `fLumosRadius` too, which is not known yet.
        let lumos_on = self.vm.player.is_some_and(|p| matches!(self.vm.get_prop(p, "bLumosOn"), Ok(Value::Bool(true))));
        let zones_drawn = items.len();
        if let (true, Some(mesh)) = (lumos_on, self.lumos_mesh) {
            items.push(DrawItem {
                mesh,
                transform: grim_render::math::IDENTITY,
                alpha_test: None,
                skins: &[],
                blend: None,
                tint: [1.0; 3],
                lights: Default::default(),
                env_map: false,
                lightmap: self.lightmap,
            });
        }
        if std::env::var_os("GRIM_DEBUG_ZONES").is_some() {
            println!("  zones drawn this frame: {zones_drawn}");
        }
        items.extend(draws.iter().map(|d| DrawItem {
            mesh: d.mesh,
            transform: d.transform,
            alpha_test: d.alpha_test,
            skins: &d.skins,
            blend: d.blend,
            tint: d.tint,
            lights: d.lights,
            env_map: d.env_map,
            lightmap: d.lightmap,
        }));
        let t3 = web_time::Instant::now();
        // GRIM_NO_OVERLAY leaves out what the game draws over the world, to see the world alone
        // while a title card or a menu would cover it.
        let overlay = if std::env::var_os("GRIM_NO_OVERLAY").is_some() { Vec::new() } else { self.draw_overlay() };
        let t_overlay = t3.elapsed().as_secs_f32() * 1000.0;
        let sky = self.sky_view();
        let backdrop = self.backdrop_mesh.map(|mesh| DrawItem {
            mesh,
            transform: grim_render::math::IDENTITY,
            alpha_test: None,
            skins: &[],
            blend: None,
            tint: [1.0; 3],
            lights: Default::default(),
            env_map: false,
            lightmap: None,
        });
        let mirrors = self
            .mirrors
            .iter()
            .map(|&(mesh, plane)| {
                let item = DrawItem {
                    mesh,
                    transform: grim_render::math::IDENTITY,
                    alpha_test: None,
                    skins: &[],
                    blend: None,
                    tint: [1.0; 3],
                    lights: Default::default(),
                    env_map: false,
                    lightmap: self.lightmap,
                };
                (item, plane)
            })
            .collect::<Vec<_>>();
        let mut mirrors = mirrors;
        // The mirrors a mover carries travel with it: the surface is drawn where the mover
        // stands and the plane it reflects about is taken there too.
        for (mesh, plane, transform, location, rotation) in self.brush_mirror_draws() {
            let [f, r, u] = grim_render::math::axes(rotation);
            let n = [
                f[0] * plane[0] + r[0] * plane[1] + u[0] * plane[2],
                f[1] * plane[0] + r[1] * plane[1] + u[1] * plane[2],
                f[2] * plane[0] + r[2] * plane[1] + u[2] * plane[2],
            ];
            let d = plane[3] + n[0] * location[0] + n[1] * location[1] + n[2] * location[2];
            let item = DrawItem {
                mesh,
                transform,
                alpha_test: None,
                skins: &[],
                blend: None,
                tint: [1.0; 3],
                lights: Default::default(),
                env_map: false,
                lightmap: None,
            };
            mirrors.push((item, [n[0], n[1], n[2], d]));
        }
        // A mirror only shows anything to an eye in front of it, and each one costs a whole
        // pass over the scene, so the ones the camera is behind are dropped.
        let mirror_passes = mirrors.len();
        mirrors.retain(|(_, plane)| {
            if self.draw_everything {
                return true;
            }
            let c = self.camera.position;
            plane[0] * c[0] + plane[1] * c[1] + plane[2] * c[2] - plane[3] > 0.0
        });
        let culled_mirrors = mirror_passes - mirrors.len();
        let t1 = web_time::Instant::now();
        let mut sprites = self.particle_sprites();
        let t_particles = t1.elapsed().as_secs_f32() * 1000.0;
        let t2 = web_time::Instant::now();
        sprites.extend(self.actor_sprites());
        let t_sprites = t2.elapsed().as_secs_f32() * 1000.0;
        let seen_sprites = sprites.len();
        sprites.retain(|s| self.in_view(s.position, s.size[0].max(s.size[1])));
        let culled_sprites = seen_sprites - sprites.len();
        let (built, drawn) = (items.len(), sprites.len());
        let mirrors_drawn = mirrors.len();
        let Some(renderer) = self.renderer.as_mut() else { return };
        let foreground: Vec<DrawItem> = over
            .iter()
            .map(|d| DrawItem {
                mesh: d.mesh,
                transform: d.transform,
                alpha_test: d.alpha_test,
                skins: &d.skins,
                blend: d.blend,
                tint: d.tint,
                lights: d.lights,
                env_map: d.env_map,
                lightmap: d.lightmap,
            })
            .collect();
        // The clock the sliding textures run on is the level's own, so it stops with the game.
        let time = match self.vm.level_info.map(|i| self.vm.get_prop(i, "TimeSeconds")) {
            Some(Ok(Value::Float(t))) => t,
            _ => 0.0,
        };
        // The player's screen flash, kept by its own scripts (`PlayerPawn.ViewFlash`). In this
        // game `FlashFog` is a plane whose `W` is how much of the world shows: 0 when a level
        // starts, faded up to 1 by `ConstantGlowFog`, and held at 0 while a skipped cutscene
        // runs, which is the black behind its hourglass.
        // The free camera is not the player's view, and it stops the clock the fade runs on.
        let flash = match self.vm.player.filter(|_| !self.free_camera).map(|p| self.vm.get_prop(p, "FlashFog")) {
            Some(Ok(Value::Struct(f))) => {
                let v = |i: usize| match f.get(i) {
                    Some(Value::Float(x)) => *x,
                    _ => 0.0,
                };
                [v(3).max(0.0), v(0), v(1), v(2)]
            }
            _ => [1.0, 0.0, 0.0, 0.0],
        };
        let scene = Scene {
            camera: self.camera,
            items,
            time,
            flash,
            gamma: self.gamma,
            overlay,
            sky,
            backdrop,
            mirrors,
            sprites,
            foreground,
            ..Default::default()
        };
        let gathered = started.elapsed().as_secs_f32() * 1000.0;
        if let Err(e) = renderer.render(&scene) {
            eprintln!("render: {e}");
        }
        let total = started.elapsed().as_secs_f32() * 1000.0;
        self.frame_costs = (gathered, total - gathered, built, drawn);
        self.frame_culled = (culled_meshes, culled_sprites, mirrors_drawn, culled_mirrors);
        self.zones_drawn = zones_drawn;
        if std::env::var_os("GRIM_TRACE_FRAME").is_some() {
            eprintln!(
                "FRAME textures {t_textures:.1} scripted {t_scripted:.1} mirrors {t_mirrors:.1} actors {t_draws:.1} (posing {:.1} for {}) particles {t_particles:.1} sprites {t_sprites:.1} overlay {t_overlay:.1} rest {:.1} render {:.1}",
                self.skinning.0,
                self.skinning.1,
                gathered - t_textures - t_scripted - t_mirrors - t_draws - t_particles - t_sprites - t_overlay,
                total - gathered
            );
        }
    }

    /// Whether the game wants the pointer to itself: with a menu up it does not, because the
    /// player is pointing at it.
    fn pointer_free(&mut self) -> bool {
        if self.vm.paused() {
            return true;
        }
        // A window of the game's own can be up with the game running behind it, such as the
        // shortcut window of its debug mode: the console says so with `bUWindowActive`.
        match self.console() {
            Some(console) => matches!(self.vm.get_prop(console, "bUWindowActive"), Ok(Value::Bool(true))),
            None => false,
        }
    }

    fn grab(&mut self, grabbed: bool) {
        self.grabbed = grabbed;
        if let Some(w) = &self.window {
            let mode = if grabbed { CursorGrabMode::Locked } else { CursorGrabMode::None };
            if w.set_cursor_grab(mode).is_err() && grabbed {
                let _ = w.set_cursor_grab(CursorGrabMode::Confined);
            }
            // The game draws its own cursor, so the system one never shows inside the window.
            w.set_cursor_visible(false);
        }
    }
}

impl Game {
    /// Once there is something to draw with: what the level draws is built before the first
    /// frame, and the pointer is taken.
    fn renderer_ready(&mut self, event_loop: &ActiveEventLoop, renderer: Result<WgpuRenderer, String>) {
        match renderer {
            Ok(r) => self.renderer = Some(r),
            Err(e) => {
                eprintln!("renderer: {e}");
                event_loop.exit();
                return;
            }
        }
        if let Some(size) = self.window.as_ref().map(|w| w.inner_size()) {
            if let Some(r) = self.renderer.as_mut() {
                r.resize(size.width.max(1), size.height.max(1));
            }
            self.viewport = (size.width.max(1), size.height.max(1));
        }
        self.show_black();
        if let Err(e) = self.build_scene_resources() {
            eprintln!("scene: {e}");
            event_loop.exit();
        }
        self.precache();
        self.grab(true);
    }
}

/// The id of the canvas a page gives the game to draw in.
#[cfg(target_arch = "wasm32")]
pub const WEB_CANVAS: &str = "grim";

/// Plays a loaded game in the page's canvas, for as long as the page is open.
#[cfg(target_arch = "wasm32")]
pub fn run_in_browser(game: Game) {
    use winit::platform::web::EventLoopExtWebSys;
    let event_loop = EventLoop::new().expect("cannot create the event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.spawn_app(game);
}

fn find_actor(vm: &mut Vm, class_name: &str) -> Option<ObjectKey> {
    let name = vm.world.names.intern(class_name);
    vm.actors.clone().into_iter().find(|&a| vm.class_of(a).is_ok_and(|c| vm.world.class(c).name == name))
}

impl ApplicationHandler for Game {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let mut attrs = Window::default_attributes().with_title(format!("grim-engine — {}", self.map));
        // A size of its own, and one that cannot be changed: under a tiling window manager
        // (Hyprland) a window that cannot be resized has the same smallest and largest size,
        // which is what makes it float at that size instead of filling the screen.
        if let Some((w, h)) = self.window_size {
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(w, h)).with_resizable(false);
        }
        // In a browser the window is the page's canvas.
        #[cfg(target_arch = "wasm32")]
        {
            use wasm_bindgen::JsCast;
            use winit::platform::web::WindowAttributesExtWebSys;
            let canvas = web_sys::window()
                .and_then(|w| w.document())
                .and_then(|d| d.get_element_by_id(WEB_CANVAS))
                .and_then(|e| e.dyn_into::<web_sys::HtmlCanvasElement>().ok());
            attrs = attrs.with_canvas(canvas).with_prevent_default(true);
        }
        let window = Arc::new(event_loop.create_window(attrs).expect("cannot create the window"));
        let size = window.inner_size();
        self.viewport = (size.width.max(1), size.height.max(1));
        self.window = Some(window.clone());
        #[cfg(not(target_arch = "wasm32"))]
        self.renderer_ready(event_loop, WgpuRenderer::for_window(window, size.width, size.height));
        // The browser hands the GPU over asynchronously; the frames pick the renderer up once
        // it is there.
        #[cfg(target_arch = "wasm32")]
        {
            let slot = self.pending_renderer.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let r = WgpuRenderer::for_window(window, size.width, size.height).await;
                *slot.borrow_mut() = Some(r);
            });
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        #[cfg(target_arch = "wasm32")]
        if self.renderer.is_none() {
            let ready = self.pending_renderer.borrow_mut().take();
            match ready {
                Some(r) => self.renderer_ready(event_loop, r),
                None => {
                    if let WindowEvent::RedrawRequested = event {
                        return;
                    }
                }
            }
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(r) = self.renderer.as_mut() {
                    r.resize(size.width, size.height);
                }
                self.viewport = (size.width.max(1), size.height.max(1));
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let PhysicalKey::Code(code) = event.physical_key else { return };
                match event.state {
                    ElementState::Pressed => {
                        // What the key writes goes to the console as well, as `KeyType`: it
                        // is how the typing line and the game's windows get their text.
                        if let (Some(text), Some(console)) = (event.text.clone(), self.console()) {
                            for c in text.chars().filter(|c| !c.is_control() && (*c as u32) < 256) {
                                if let Err(e) = self.vm.call_event(console, "KeyType", vec![Value::Byte(c as u8)]) {
                                    self.report.record(e);
                                }
                            }
                        }
                        match code {
                            KeyCode::Delete if self.debug => {
                                self.free_camera = !self.free_camera;
                                if !self.free_camera {
                                    self.teleport_player();
                                }
                                println!("free camera {}", if self.free_camera { "on, game paused" } else { "off" });
                            }
                            KeyCode::F8 if self.debug => {
                                self.show_collision = !self.show_collision;
                                println!("collision shapes {}", if self.show_collision { "shown" } else { "hidden" });
                            }
                            KeyCode::F9 if self.debug => self.grant_spells(),
                            KeyCode::Escape => {
                                self.grab(false);
                                self.show_menu();
                            }
                            KeyCode::Tab => {
                                let g = self.grabbed;
                                self.grab(!g);
                            }
                            _ => {}
                        }
                        self.keys.insert(code);
                    }
                    ElementState::Released => {
                        self.keys.remove(&code);
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if let Some(name) = button_name(button) {
                    match state {
                        ElementState::Pressed => {
                            self.buttons.insert(name);
                        }
                        ElementState::Released => {
                            self.buttons.remove(name);
                        }
                    }
                }
                // Clicking back into the window takes the pointer again, unless a menu wants it.
                if state == ElementState::Pressed && !self.pointer_free() {
                    self.grab(true);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                // The game draws its own cursor and keeps where it is in the console, in the
                // same pixels it draws with, so the pointer is put there rather than nudged.
                if let Some(console) = self.console() {
                    // The menus live in a space of their own, which their root window states
                    // and the drawing scales up to the viewport; the pointer is given in it.
                    let root = match self.vm.get_prop(console, "Root") {
                        Ok(Value::Object(Some(r))) => Some(r),
                        _ => None,
                    };
                    let size_of = |vm: &mut Vm, name: &str| match root.map(|r| vm.get_prop(r, name)) {
                        Some(Ok(Value::Float(v))) if v > 0.0 => v as f64,
                        _ => 0.0,
                    };
                    let (mw, mh) = (size_of(&mut self.vm, "WinWidth"), size_of(&mut self.vm, "WinHeight"));
                    let size = self.window.as_ref().map(|w| w.inner_size());
                    let (sw, sh) = size.map_or((mw, mh), |s| (s.width as f64, s.height as f64));
                    if mw > 0.0 && mh > 0.0 {
                        let x = (position.x / sw.max(1.0)) * mw;
                        let y = (position.y / sh.max(1.0)) * mh;
                        let _ = self.vm.set_prop(console, "MouseX", Value::Float(x as f32));
                        let _ = self.vm.set_prop(console, "MouseY", Value::Float(y as f32));
                    }
                }
            }
            WindowEvent::RedrawRequested => {
                let now = web_time::Instant::now();
                let dt = (now - self.last_frame).as_secs_f32().min(MAX_TICK);
                self.last_frame = now;
                self.update(dt);
                // The console's `exit` or `quit`.
                if self.vm.quit {
                    event_loop.exit();
                    return;
                }
                self.render();
            }
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event {
            if self.grabbed {
                self.look.0 += delta.0 as f32;
                self.look.1 += delta.1 as f32;
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }
}

fn write_png(out: &str, width: u32, height: u32, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().map_err(|e| e.to_string())?.write_image_data(rgba).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Plays the game without a window and writes what it draws as a video, frame by frame
/// through ffmpeg. Useful to watch a cutscene without sitting in front of it.
/// A scripted run of the player's controls, so a recording can show what a key does.
/// `GRIM_DEBUG_DRIVE="W@0-14 Ctrl@3 Alt@5-12 mouse=6@5-14"`: each entry is a key held over a
/// span of seconds (`@t` alone is a tap), and `mouse=<x>` or `mouseY=<y>` pushes the mouse.
#[derive(Default)]
struct Script {
    keys: Vec<(String, f32, f32)>,
    mouse: Vec<(f32, f32, f32, f32)>,
    /// Where to put the menu cursor, in the pixels the game draws with.
    cursor: Vec<([f32; 2], f32, f32)>,
}

#[cfg(not(target_arch = "wasm32"))]
impl Script {
    fn from_env() -> Self {
        let Ok(text) = std::env::var("GRIM_DEBUG_DRIVE") else { return Self::default() };
        let mut script = Self::default();
        for entry in text.split_whitespace() {
            let (what, span) = entry.split_once('@').unwrap_or((entry, "0-1e9"));
            let (from, to) = match span.split_once('-') {
                Some((a, b)) => (a.parse().unwrap_or(0.0), b.parse().unwrap_or(f32::MAX)),
                // A tap has to last long enough for a frame to see it go down.
                None => (span.parse().unwrap_or(0.0), span.parse().unwrap_or(0.0) + 0.15),
            };
            match what.split_once('=') {
                Some((axis, at)) if axis.eq_ignore_ascii_case("cursor") => {
                    let v: Vec<f32> = at.split(',').filter_map(|c| c.parse().ok()).collect();
                    if v.len() == 2 {
                        script.cursor.push(([v[0], v[1]], from, to));
                    }
                }
                Some((axis, push)) if axis.eq_ignore_ascii_case("mouse") => {
                    script.mouse.push((push.parse().unwrap_or(0.0), 0.0, from, to))
                }
                Some((axis, push)) if axis.eq_ignore_ascii_case("mouseY") => {
                    script.mouse.push((0.0, push.parse().unwrap_or(0.0), from, to))
                }
                _ => script.keys.push((what.to_string(), from, to)),
            }
        }
        script
    }

    fn held(&self, at: f32) -> Vec<String> {
        self.keys.iter().filter(|(_, f, t)| at >= *f && at < *t).map(|(k, _, _)| k.clone()).collect()
    }

    /// Where the cursor should be at that moment, if the script says.
    fn cursor(&self, at: f32) -> Option<[f32; 2]> {
        self.cursor.iter().find(|(_, f, t)| at >= *f && at < *t).map(|(p, ..)| *p)
    }

    fn mouse(&self, at: f32) -> (f32, f32) {
        self.mouse.iter().filter(|(_, _, f, t)| at >= *f && at < *t).fold((0.0, 0.0), |a, m| (a.0 + m.0, a.1 + m.1))
    }
}

/// The longest step the game is ticked by, however long the frame took: 2.5 frames a second,
/// Unreal's own floor. It matters most right after a level loads: the load's time reaches the
/// first tick of the new level as this much, and a cutscene that starts with the level
/// (`CutScene.Idle` sleeps 0.2 before `Play`) is already running when the fade from black
/// begins, instead of the level showing itself for a moment before it. Hypothesis: the value
/// is Unreal's; the game's own scripts only show that such a step must reach past 0.2.
pub const MAX_TICK: f32 = 0.4;

/// How many samples a second a recording mixes at.
pub const RECORD_RATE: u32 = 44100;

#[cfg(not(target_arch = "wasm32"))]
fn record(game: &mut Game, out: &str, seconds: f32, mut audio: Option<grim_audio_cpal::OfflineAudio>) -> Result<(), String> {
    if std::env::var_os("GRIM_DEBUG_SPELLS").is_some() {
        game.grant_spells();
    }
    // GRIM_DEBUG_SET turns on one of the game's own switches: `SpellCursor.bDebugMode=1`
    // follows a path of objects from the player, like GRIM_DEBUG_PROPS reads one.
    if let Ok(list) = std::env::var("GRIM_DEBUG_SET") {
        for entry in list.split(',') {
            let Some((path, value)) = entry.split_once('=') else { continue };
            let mut at = game.vm.player;
            let mut names = path.split('.').peekable();
            while let Some(name) = names.next() {
                if names.peek().is_none() {
                    if let Some(o) = at {
                        // The value takes the type the property already has.
                        let v = match (game.vm.get_prop(o, name), value.trim()) {
                            (Ok(Value::Byte(_)), n) => Value::Byte(n.parse().unwrap_or(0)),
                            (Ok(Value::Int(_)), n) => Value::Int(n.parse().unwrap_or(0)),
                            (Ok(Value::Str(_)), t) => Value::Str(t.to_string()),
                            (_, "0" | "false") => Value::Bool(false),
                            (_, "1" | "true") => Value::Bool(true),
                            (_, other) => Value::Float(other.parse().unwrap_or(0.0)),
                        };
                        let _ = game.vm.set_prop(o, name, v);
                    }
                    break;
                }
                at = match at.map(|o| game.vm.get_prop(o, name)) {
                    Some(Ok(Value::Object(k))) => k,
                    _ => None,
                };
            }
        }
    }
    // GRIM_DEBUG_START puts the player somewhere else, to reach what is being looked at
    // without walking there first: `x,y,z[,yaw[,when]]`. The map's own opening moves the
    // player itself, so `when` (seconds into the recording) waits for it to be done.
    let start: Vec<f32> = std::env::var("GRIM_DEBUG_START")
        .map(|at| at.split(',').filter_map(|c| c.trim().parse().ok()).collect())
        .unwrap_or_default();
    let place = |game: &mut Game| {
        if let (Some(p), true) = (game.vm.player, start.len() >= 3) {
            let _ = game.vm.set_prop(p, "Location", grim_vm::vm::vector(start[0], start[1], start[2]));
            if let Some(&yaw) = start.get(3) {
                for name in ["Rotation", "DesiredRotation"] {
                    let _ = game.vm.set_prop(p, name, grim_vm::vm::rotator(0, yaw as i32, 0));
                }
            }
        }
    };
    place(game);
    let placed_at = start.get(4).copied().unwrap_or(0.0);
    let mut placed = placed_at <= 0.0;
    use std::io::Write;
    const RATE: u32 = 30;
    let (width, height) = (960, 720);
    game.renderer = Some(WgpuRenderer::offscreen(width, height)?);
    game.viewport = (width, height);
    game.build_scene_resources()?;
    game.precache();
    // With sound, the picture goes to a file of its own first and the two are put together at
    // the end; ffmpeg cannot take both from the same pipe.
    let silent = format!("{out}.video.mp4");
    let pcm_path = format!("{out}.pcm");
    let video_out = if audio.is_some() { silent.clone() } else { out.to_string() };
    let mut pcm = match audio {
        Some(_) => Some(std::io::BufWriter::new(std::fs::File::create(&pcm_path).map_err(|e| e.to_string())?)),
        None => None,
    };
    let mut ffmpeg = std::process::Command::new("ffmpeg")
        .args([
            "-loglevel", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "rgba",
            "-s", &format!("{width}x{height}"),
            "-r", &RATE.to_string(),
            "-i", "-",
            "-c:v", "libx264", "-pix_fmt", "yuv420p", "-preset", "veryfast",
            &video_out,
        ])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start ffmpeg: {e}"))?;
    let mut input = ffmpeg.stdin.take().ok_or("ffmpeg has no stdin")?;
    let frames = (seconds * RATE as f32) as u32;
    let dt = 1.0 / RATE as f32;
    let mut after_load: Option<f32> = None;
    let mut command: Option<(String, f32)> = std::env::var("GRIM_DEBUG_COMMAND").ok().map(|v| match v.rsplit_once('@') {
        Some((line, when)) => (line.to_string(), when.parse().unwrap_or(0.0)),
        None => (v, 0.0),
    });
    // GRIM_DEBUG_DRIVE plays a script of key presses while it records, to see the movement,
    // the animations and the effects that go with them.
    let script = Script::from_env();
    // GRIM_DEBUG_GOTO=<actor>:<state>@<seconds> sends an actor into one of its states partway
    // through, to see what it does there without playing up to it: `ChestWood0:turnover@2`
    // opens a chest. `<actor>:!<event>` calls an event on it instead, with the player as the
    // instigator: `Mover24:!Trigger@40` raises a Lumos ledge.
    let mut goto: Option<(String, String, f32)> = std::env::var("GRIM_DEBUG_GOTO").ok().and_then(|v| {
        let (who, rest) = v.split_once(':')?;
        let (state, when) = rest.split_once('@').unwrap_or((rest, "0"));
        Some((who.to_string(), state.to_string(), when.parse().unwrap_or(0.0)))
    });
    for frame in 0..frames {
        let at = frame as f32 / RATE as f32;
        if !placed && at >= placed_at {
            place(game);
            placed = true;
        }
        if goto.as_ref().is_some_and(|g| at >= g.2) {
            let (who, state, _) = goto.take().unwrap_or_default();
            let target = game.vm.actors.clone().into_iter().find(|&a| game.vm.object_name(a).eq_ignore_ascii_case(&who));
            match target {
                Some(a) => {
                    let result = match state.strip_prefix('!') {
                        Some(event) => {
                            let player = Value::Object(game.vm.player);
                            game.vm.call_event(a, event, vec![player.clone(), player]).map(|_| ())
                        }
                        None => {
                            let name = game.vm.world.names.intern(&state);
                            game.vm.goto_state(a, Some(name), None)
                        }
                    };
                    if let Err(e) = result {
                        eprintln!("goto: {e}");
                    }
                }
                None => eprintln!("goto: no actor {who}"),
            }
        }
        // GRIM_DEBUG_COMMAND="<command>@<seconds>" types a line into the console partway
        // through, which is how a recording can show what a console command does.
        if command.as_ref().is_some_and(|c| at >= c.1) {
            let (line, _) = command.take().unwrap_or_default();
            if let Some(console) = game.console() {
                if let Err(e) = game.vm.call_event(console, "ConsoleCommand", vec![Value::Str(line)]) {
                    eprintln!("command: {e}");
                }
            }
        }
        let held = script.held(at);
        let held: Vec<&str> = held.iter().map(|s| s.as_str()).collect();
        if let (Some(p), Some(console)) = (script.cursor(at), game.console()) {
            let _ = game.vm.set_prop(console, "MouseX", Value::Float(p[0]));
            let _ = game.vm.set_prop(console, "MouseY", Value::Float(p[1]));
        }
        // The frame after a level loads is ticked by the time the load took, as a window's is.
        let dt = after_load.take().unwrap_or(dt);
        game.frame_time = dt;
        let input_state = grim_vm::level::PlayerInput { held: &held, mouse: script.mouse(at) };
        game.vm.player_input(input_state, dt, &mut game.report);
        game.vm.tick(dt, &mut game.report);
        if let Some(console) = game.console() {
            let _ = game.vm.call_event(console, "Tick", vec![Value::Float(dt)]);
        }
        if let Some(url) = game.pending_travel() {
            let started = web_time::Instant::now();
            game.travel(&url)?;
            after_load = Some(started.elapsed().as_secs_f32().min(MAX_TICK));
        }
        // The free camera stays where it was put: a recording of a place rather than of the
        // player, which is what `--at` and `--look` are for.
        if !game.free_camera {
            game.update_view();
        }
        // GRIM_DEBUG_CAM follows the view and says whether it ended up inside the level;
        // GRIM_DEBUG_PROPS names properties of the player to print along with it.
        if std::env::var_os("GRIM_DEBUG_CAM").is_some() && frame % std::env::var("GRIM_DEBUG_EVERY").ok().and_then(|v| v.parse::<u32>().ok()).unwrap_or(15) == 0 {
            let solid = game.vm.bsp.as_ref().map(|b| b.point_solid(game.camera.position));
            let at = game.vm.player.and_then(|p| game.vm.get_prop(p, "Location").and_then(|v| grim_vm::vm::floats3(&v)).ok());
            let watched = std::env::var("GRIM_DEBUG_PROPS").unwrap_or_default();
            let state = game.vm.player.map(|p| game.vm.state_name(p));
            // Whether the player's own box is inside the level, which is what every way of
            // getting stuck or falling out of the world starts as.
            let buried = game.vm.player.map(|p| {
                let at = game.vm.get_prop(p, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
                let r = match game.vm.get_prop(p, "CollisionRadius") { Ok(Value::Float(v)) => v, _ => 0.0 };
                let h = match game.vm.get_prop(p, "CollisionHeight") { Ok(Value::Float(v)) => v, _ => 0.0 };
                game.vm.inside_level(at, [r, r, h])
            });
            let mut props = String::new();
            for path in watched.split(',').filter(|n| !n.is_empty()) {
                // A path steps through objects: `Weapon.bHidden` is the player's weapon's.
                let mut at = game.vm.player;
                let mut v = None;
                for name in path.split('.') {
                    v = at.and_then(|o| game.vm.get_prop(o, name).ok());
                    at = match &v {
                        Some(Value::Object(k)) => *k,
                        _ => None,
                    };
                }
                // An object says little by its key: name the class it is of.
                let class = match &v {
                    Some(Value::Object(Some(k))) => game
                        .vm
                        .class_of(*k)
                        .ok()
                        .map(|c| format!("({})", game.vm.world.names.str(game.vm.world.class(c).name))),
                    _ => None,
                };
                props.push_str(&format!(" {path} {v:?}{}", class.unwrap_or_default()));
            }
            // GRIM_DEBUG_ACTOR=<name>,<name> follows named actors: where they are, whether
            // they are drawn, and what they are playing. It is what a scene where somebody
            // vanishes or stands in a T-pose needs.
            if let Ok(names) = std::env::var("GRIM_DEBUG_ACTOR") {
                let wanted: Vec<&str> = names.split(',').filter(|n| !n.is_empty()).collect();
                // `near` follows whatever stands close to the camera instead of a named actor.
                let near = wanted.iter().any(|w| w.eq_ignore_ascii_case("near"));
                for a in game.vm.actors.clone() {
                    let name = game.vm.object_name(a);
                    let at = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
                    let close = {
                        let d = [0, 1, 2].map(|i| at[i] - game.camera.position[i]);
                        (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt() < 500.0
                            && matches!(game.vm.get_prop(a, "Mesh"), Ok(Value::Object(Some(_))))
                    };
                    // `*` follows every actor, which is what comparing a whole level against
                    // the original's own save needs.
                    let all = wanted.iter().any(|w| *w == "*");
                    let named = wanted.iter().any(|w| name.eq_ignore_ascii_case(w) || name.to_lowercase().starts_with(&w.to_lowercase()));
                    if !(all || named || (near && close)) {
                        continue;
                    }
                    let get = |game: &mut Game, p: &str| game.vm.get_prop(a, p).map(|v| format!("{v:?}")).unwrap_or_else(|_| "-".into());
                    let at = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
                    let (hidden, draw, mesh, scale) = (get(game, "bHidden"), get(game, "DrawType"), get(game, "Mesh"), get(game, "DrawScale"));
                    let (seq, rate, frame) = (get(game, "AnimSequence"), get(game, "AnimRate"), get(game, "AnimFrame"));
                    let state = game.vm.state_name(a).unwrap_or_default();
                    let deleted = game.vm.is_deleted(a);
                    println!(
                        "  actor {name} at [{:.2}, {:.2}, {:.2}] deleted {deleted} hidden {hidden} draw {draw} state {state} seq {seq} rate {rate} frame {frame} mesh {mesh} scale {scale}",
                        at[0], at[1], at[2]
                    );
                }
            }
            if std::env::var_os("GRIM_DEBUG_CHANNELS").is_some() {
                if let Some(p) = game.vm.player {
                    for ch in game.vm.channels_of(p).to_vec() {
                        let seq = game.vm.get_prop(ch.actor, "AnimSequence").ok();
                        let frame = game.vm.get_prop(ch.actor, "AnimFrame").ok();
                        let bone = game.vm.world.names.str(ch.bone).to_string();
                        let class = game.vm.class_of(ch.actor).ok().map(|c| game.vm.world.names.str(game.vm.world.class(c).name).to_string());
                        let seq = seq.and_then(|v| match v { Value::Name(n) => Some(game.vm.world.names.str(n).to_string()), _ => None });
                        let state = game.vm.state_name(ch.actor);
                        println!("  channel {class:?} bone {bone} kind {} seq {seq:?} frame {frame:?} state {state:?}", ch.kind);
                    }
                    let seq = game.vm.get_prop(p, "AnimSequence").ok();
                    println!("  body seq {seq:?}");
                }
            }
            // GRIM_DEBUG_BONE=<bone>,<actor>.<bone>: where a bone of the player, or of a named
            // actor, has ended up in the world. Comparing one against what the map puts there
            // is how a mesh drawn in the wrong place or facing the wrong way is caught.
            if let Ok(bones) = std::env::var("GRIM_DEBUG_BONE") {
                for want in bones.split(',').filter(|b| !b.is_empty()) {
                    let (who, bone) = match want.split_once('.') {
                        Some((who, bone)) => (Some(who), bone),
                        None => (None, want),
                    };
                    let actors: Vec<ObjectKey> = match who {
                        None => game.vm.player.into_iter().collect(),
                        Some(name) => game
                            .vm
                            .actors
                            .clone()
                            .into_iter()
                            .filter(|&a| game.vm.object_name(a).to_lowercase().starts_with(&name.to_lowercase()))
                            .collect(),
                    };
                    for a in actors {
                        // `*` for the bone prints every one of them, which is what comparing a
                        // mesh against what the map puts around it needs.
                        if bone == "*" {
                            for name in game.vm.bone_names(a) {
                                if let Some((at, _)) = game.vm.bone_world(a, &name) {
                                    println!("  bone {name} of {} at {:?}", game.vm.object_name(a), at.map(|x| x.round()));
                                }
                            }
                            continue;
                        }
                        let at = game.vm.bone_world(a, bone);
                        let loc = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).unwrap_or([0.0; 3]);
                        let rot = game.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).unwrap_or([0; 3]);
                        println!(
                            "  bone {} of {} at {:?} (actor at {:?} rotation {rot:?})",
                            bone,
                            game.vm.object_name(a),
                            at.map(|(v, _)| v.map(|x| x.round())),
                            loc.map(|x| x.round())
                        );
                    }
                }
            }
            if let Ok(want) = std::env::var("GRIM_DEBUG_ACTORS") {
                for a in game.vm.actors.clone() {
                    if game.vm.is_deleted(a) {
                        continue;
                    }
                    let Ok(class) = game.vm.class_of(a) else { continue };
                    let name = game.vm.world.names.str(game.vm.world.class(class).name).to_string();
                    if !name.to_lowercase().contains(&want.to_lowercase()) {
                        continue;
                    }
                    let at = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).ok();
                    let state = game.vm.state_name(a);
                    let turn = game.vm.get_prop(a, "Rotation").and_then(|v| grim_vm::vm::ints3(&v)).ok();
                    let extra = std::env::var("GRIM_DEBUG_ACTOR_PROPS").unwrap_or_default();
                    let mut props = String::new();
                    for want in extra.split(',').filter(|n| !n.is_empty()) {
                        let (name, index) = match want.split_once('[') {
                            Some((n, i)) => (n, i.trim_end_matches(']').parse().unwrap_or(0)),
                            None => (want, 0),
                        };
                        props.push_str(&format!(" {want}={:?}", game.vm.get_prop_at(a, name, index)));
                    }
                    println!(
                        "  actor {name} at {:?} yaw {:?} state {state:?}{props}",
                        at.map(|v| v.map(|x| x.round())),
                        turn.map(|r| r[1])
                    );
                }
            }
            // GRIM_DEBUG_AUDIO names what is sounding, which is how a sound the scripts asked
            // for is told apart from one the mixer never started.
            if std::env::var_os("GRIM_DEBUG_AUDIO").is_some() {
                if let Some(audio) = game.vm.audio.as_ref() {
                    for v in audio.playing() {
                        println!(
                            "  sound {:?} slot {} owner {} volume {:.2} radius {:.0} pitch {:.2}{}",
                            v.kind, v.slot, v.owner, v.volume, v.radius, v.pitch,
                            if v.looping { " looping" } else { "" }
                        );
                    }
                }
            }
            if std::env::var_os("GRIM_DEBUG_FX").is_some() {
                let keys: Vec<ObjectKey> = game.vm.particles.keys().copied().collect();
                for a in keys {
                    let n = game.vm.particles.get(&a).map(|e| e.particles.len()).unwrap_or(0);
                    let class = game.vm.class_of(a).map(|c| game.vm.world.names.str(game.vm.world.class(c).name).to_string());
                    let at = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).ok();
                    let _ = &game.vm;
                    let emit = game.vm.get_prop(a, "bEmit").ok();
                    let life = game.vm.get_prop(a, "LifeSpan").ok();
                    let sizes: Vec<String> = game
                        .vm
                        .particles
                        .get(&a)
                        .map(|e| e.particles.iter().take(4).map(|p| format!("{:.0}x{:.0}", p.shade().1[0], p.shade().1[1])).collect())
                        .unwrap_or_default();
                    println!(
                        "  fx {class:?} {n} particles at {:?} emit {emit:?} life {life:?} sizes {}",
                        at.map(|v| v.map(|x| x.round())),
                        sizes.join(" ")
                    );
                }
            }
            println!(
                "{:.1}s player {:?} camera {:?} solid {solid:?} buried {buried:?} state {state:?}{props}",
                frame as f32 / RATE as f32,
                at.map(|v| v.map(|x| x.round())),
                game.camera.position.map(|v| v.round()),
            );
        }
        if std::env::var_os("GRIM_DEBUG_LOG").is_some() {
            for line in game.vm.log.drain(..) {
                println!("log: {line}");
            }
        }
        game.render();
        let frame = game.renderer.as_mut().and_then(|r| r.capture()).ok_or("no frame captured")?;
        input.write_all(&frame.rgba).map_err(|e| format!("ffmpeg stopped: {e}"))?;
        // One frame's worth of sound, mixed from what the tick set going.
        if let (Some(audio), Some(pcm)) = (audio.as_mut(), pcm.as_mut()) {
            let samples = (RECORD_RATE / RATE) as usize * 2;
            let mut block = vec![0.0f32; samples];
            audio.render(&mut block);
            let bytes: Vec<u8> = block.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
            pcm.write_all(&bytes).map_err(|e| format!("cannot write the sound: {e}"))?;
        }
    }
    drop(input);
    ffmpeg.wait().map_err(|e| e.to_string())?;
    if let Some(pcm) = pcm.take() {
        drop(pcm);
        let mix = std::process::Command::new("ffmpeg")
            .args([
                "-loglevel", "error", "-y",
                "-i", &silent,
                "-f", "s16le", "-ar", &RECORD_RATE.to_string(), "-ac", "2", "-i", &pcm_path,
                "-c:v", "copy", "-c:a", "aac", "-shortest", out,
            ])
            .status()
            .map_err(|e| format!("cannot put the sound and the picture together: {e}"))?;
        if !mix.success() {
            return Err("ffmpeg could not put the sound and the picture together".to_string());
        }
        let _ = std::fs::remove_file(&silent);
        let _ = std::fs::remove_file(&pcm_path);
    }
    println!("wrote {out}: {frames} frames at {RATE} fps");
    for (n, c) in &game.report.missing_natives {
        println!("missing native {n} x{c}");
    }
    for (_, (c, example)) in &game.report.errors {
        println!("script error x{c}: {example}");
    }
    let mut warnings: Vec<_> = game.vm.warnings.iter().collect();
    warnings.sort_by_key(|entry| std::cmp::Reverse(*entry.1));
    for (what, n) in warnings.iter().take(12) {
        println!("warning x{n}: {what}");
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Renders one frame without a window and writes it as a PNG.
fn shot(game: &mut Game, out: &str) -> Result<(), String> {
    // The game's own interface is laid out for 4:3, so that is what a shot uses.
    let (width, height) = (960, 720);
    game.renderer = Some(WgpuRenderer::offscreen(width, height)?);
    game.build_scene_resources()?;
    game.precache();
    if std::env::var_os("GRIM_DEBUG_VIEW").is_some() {
        game.update_view();
    }
    game.render();
    // GRIM_DEBUG_WALK=<ticks> drives the player forward, to check that input reaches it.
    if let Ok(n) = std::env::var("GRIM_DEBUG_WALK").unwrap_or_default().parse::<u32>() {
        let run = std::env::var_os("GRIM_DEBUG_RUN").is_some();
        let held: &[&str] = if run { &["W", "Shift"] } else { &["W"] };
        let input = grim_vm::level::PlayerInput { held, mouse: (0.0, 0.0) };
        for i in 0..n {
            game.vm.player_input(input, 1.0 / 30.0, &mut game.report);
            game.vm.tick(1.0 / 30.0, &mut game.report);
            if i == n - 1 {
                let log = game.vm.log.len();
                println!("script log lines: {log}");
                for line in game.vm.log.iter().rev().take(12).rev() {
                    println!("  {line}");
                }
            }
            if i % 10 == 0 {
                if let Some(p) = game.vm.player {
                    println!(
                        "tick {i}: player {:?} velocity {:?} accel {:?}",
                        game.vm.get_prop(p, "Location").and_then(|v| grim_vm::vm::floats3(&v)),
                        game.vm.get_prop(p, "Velocity").and_then(|v| grim_vm::vm::floats3(&v)),
                        game.vm.get_prop(p, "Acceleration").and_then(|v| grim_vm::vm::floats3(&v)),
                    );
                }
            }
        }
    }
    // GRIM_DEBUG_FRAMES=<n> times a tick and a frame, to see where the time goes.
    if let Ok(n) = std::env::var("GRIM_DEBUG_FRAMES").unwrap_or_default().parse::<u32>() {
        let input = grim_vm::level::PlayerInput::default();
        let _ = game.vm.take_script_profile();
        let start = web_time::Instant::now();
        for _ in 0..n {
            game.vm.player_input(input, 1.0 / 60.0, &mut game.report);
            game.vm.tick(1.0 / 60.0, &mut game.report);
        }
        let ticked = start.elapsed();
        // GRIM_PROFILE_SCRIPTS: where the scripts' part of those ticks went, per tick.
        for (path, ms, calls) in game.vm.take_script_profile().into_iter().take(40) {
            println!("  {:8.3} ms {:7.1} calls  {path}", ms / n as f64, calls as f64 / n as f64);
        }
        let start = web_time::Instant::now();
        for _ in 0..n {
            game.render();
        }
        println!(
            "{n} frames: tick {:.1} ms each, render {:.1} ms each",
            ticked.as_secs_f32() * 1000.0 / n as f32,
            start.elapsed().as_secs_f32() * 1000.0 / n as f32
        );
    }
    // GRIM_DEBUG_PLAY=<frames> plays that many frames before the shot, so what the game fades
    // in (its hud, its subtitles) has time to appear.
    if let Ok(frames) = std::env::var("GRIM_DEBUG_PLAY").unwrap_or_default().parse::<u32>() {
        let input = grim_vm::level::PlayerInput::default();
        let mut shown = 0u32;
        // GRIM_DEBUG_TRIGGER=<tag> fires what the level's own triggers would, to reach a part
        // of the game without playing through everything before it.
        if let Ok(tag) = std::env::var("GRIM_DEBUG_TRIGGER") {
            let want = game.vm.world.names.intern(&tag);
            for a in game.vm.actors.clone() {
                if !matches!(game.vm.get_prop(a, "Tag"), Ok(Value::Name(n)) if n == want) {
                    continue;
                }
                let player = Value::Object(game.vm.player);
                if let Err(e) = game.vm.call_event(a, "Trigger", vec![player.clone(), player]) {
                    eprintln!("trigger: {e}");
                }
            }
        }
        for _ in 0..frames {
            game.vm.player_input(input, 1.0 / 30.0, &mut game.report);
            game.vm.tick(1.0 / 30.0, &mut game.report);
            if let Some(console) = game.console() {
                let _ = game.vm.call_event(console, "Tick", vec![Value::Float(1.0 / 30.0)]);
            }
            if let Some(url) = game.pending_travel() {
                print!("at {:.1}s ", shown as f32 / 30.0);
                if let Err(e) = game.travel(&url) {
                    eprintln!("travel: {e}");
                }
            }
            shown += 1;
            game.update_view();
            // GRIM_DEBUG_FAST plays without drawing, to reach a later part of the game quickly.
            if std::env::var_os("GRIM_DEBUG_FAST").is_none() {
                game.render();
            }
            // GRIM_DEBUG_CAMERA follows what the view is attached to, once a second.
            if std::env::var_os("GRIM_DEBUG_CAMERA").is_some() {
                if shown % 30 == 0 {
                    let target = game.vm.player.and_then(|p| match game.vm.get_prop(p, "ViewTarget") {
                        Ok(Value::Object(t)) => t,
                        _ => None,
                    });
                    let at = target.and_then(|t| game.vm.get_prop(t, "Location").and_then(|v| grim_vm::vm::floats3(&v)).ok());
                    let class = target.and_then(|t| game.vm.class_of(t).ok()).map(|c| game.vm.world.names.str(game.vm.world.class(c).name).to_string());
                    let physics = target.map(|t| game.vm.get_prop(t, "Physics"));
                    println!(
                        "t={:.1}s camera {:?} | viewtarget {:?} at {:?} physics {:?}",
                        shown as f32 / 30.0,
                        game.camera.position.map(|v| v.round()),
                        class,
                        at.map(|v| v.map(|x| x.round())),
                        physics
                    );
                }
            }
        }
    }
    // GRIM_DEBUG_MENU opens the game's menu before the frame, to check it draws.
    if std::env::var_os("GRIM_DEBUG_MENU").is_some() {
        game.show_menu();
        // GRIM_DEBUG_PAGE=<name> turns the menu book to that page.
        if let Ok(page) = std::env::var("GRIM_DEBUG_PAGE") {
            let books: Vec<ObjectKey> = game.vm.objects_of_class("FEBook");
            for book in books {
                let r = game.vm.call_event(book, "OpenBook", vec![Value::Str(page.clone())]);
                println!("OpenBook({page}) -> {r:?}");
            }
        }
        // The menu animates in, so it needs a few seconds before it is worth looking at.
        let frames: u32 = std::env::var("GRIM_DEBUG_MENU").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
        for _ in 0..frames {
            game.vm.tick(1.0 / 30.0, &mut game.report);
            if let Some(console) = game.console() {
                let _ = game.vm.call_event(console, "Tick", vec![Value::Float(1.0 / 30.0)]);
            }
            game.render();
        }
        game.render();
    }
    for (msg, (n, example)) in game.report.errors.iter().take(6) {
        println!("script error x{n}: {msg}\n{example}");
    }
    for (native, n) in game.report.missing_natives.iter().take(6) {
        println!("missing native x{n}: {native}");
    }
    let mut warnings: Vec<_> = game.vm.warnings.iter().collect();
    warnings.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (warning, n) in warnings.iter().take(10) {
        println!("warning x{n}: {warning}");
    }
    // GRIM_DEBUG_CUTMARKS prints every actor the cutscenes can name, with where it stands.
    if std::env::var_os("GRIM_DEBUG_CUTMARKS").is_some() {
        for a in game.vm.actors.clone() {
            let Ok(Value::Str(cut)) = game.vm.get_prop(a, "CutName") else { continue };
            if cut.is_empty() {
                continue;
            }
            let at = game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).map(|v| v.map(|x| x.round()));
            println!("cut subject {cut:?} at {at:?}");
        }
    }
    // GRIM_DEBUG_SPLINE=<tag> prints the interpolation path with that tag, as the map holds it.
    if let Ok(tag) = std::env::var("GRIM_DEBUG_SPLINE") {
        for a in game.vm.actors.clone() {
            let class = game.vm.class_of(a).map(|c| game.vm.world.names.str(game.vm.world.class(c).name).to_string()).unwrap_or_default();
            if !class.contains("InterpolationPoint") {
                continue;
            }
            let name = |game: &mut Game, prop: &str| match game.vm.get_prop(a, prop) {
                Ok(Value::Name(n)) => game.vm.world.names.str(n).to_string(),
                other => format!("{other:?}"),
            };
            let tag_of = name(game, "Tag");
            if !tag.is_empty() && !tag_of.eq_ignore_ascii_case(&tag) {
                continue;
            }
            let own = name(game, "Name");
            println!(
                "point {own} tag {tag_of} position {:?} at {:?} dist {:?} in {:?} out {:?} prev {:?} next {:?}",
                game.vm.get_prop(a, "Position"),
                game.vm.get_prop(a, "Location").and_then(|v| grim_vm::vm::floats3(&v)).map(|v| v.map(|x| x.round())),
                game.vm.get_prop(a, "PathDist"),
                game.vm.get_prop(a, "StartControlPoint").and_then(|v| grim_vm::vm::floats3(&v)).map(|v| v.map(|x| x.round())),
                game.vm.get_prop(a, "EndControlPoint").and_then(|v| grim_vm::vm::floats3(&v)).map(|v| v.map(|x| x.round())),
                game.vm.get_prop(a, "Prev").map(|v| matches!(v, Value::Object(Some(_)))),
                game.vm.get_prop(a, "Next").map(|v| matches!(v, Value::Object(Some(_)))),
            );
        }
    }
    if std::env::var_os("GRIM_DEBUG_LOG").is_some() {
        for line in game.vm.log.iter() {
            println!("{line}");
        }
    }
    let image = game.renderer.as_mut().and_then(|r| r.capture()).ok_or("no frame captured")?;
    write_png(out, image.width, image.height, &image.rgba)?;
    println!("wrote {out} at {:?} rotation {:?}", game.camera.position, game.camera.rotation);
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// Starts one named cutscene: the game state it belongs to is said to be the current one (what
/// `OnResolveGameState` decides), and then it is played.
fn play_cutscene(game: &mut Game, name: &str) -> Result<(), String> {
    let all = game.vm.objects_of_class("CutScene");
    let cut = all
        .iter()
        .copied()
        .find(|&c| game.vm.object_name(c).eq_ignore_ascii_case(name))
        .ok_or_else(|| format!("no cutscene '{name}' in this map"))?;
    // Only this one runs: the map's own opening cutscene otherwise holds the camera and the
    // characters this one wants to capture.
    for other in all.iter().copied().filter(|&c| c != cut) {
        let _ = game.vm.call_event(other, "ForceFinish", vec![]);
        let _ = game.vm.set_prop(other, "bInCurrentGameState", Value::Bool(false));
        let _ = game.vm.call_event(other, "OnResolveGameState", vec![]);
    }
    let mut report = grim_vm::level::Report::default();
    if let Ok(Some(state)) = game.vm.enter_cutscene_state(cut, &mut report) {
        println!("game state set to {state} for this cutscene");
    }
    let _ = game.vm.place_player_at_cutscene(cut);
    game.vm.set_prop(cut, "bInCurrentGameState", Value::Bool(true)).map_err(|e| e.to_string())?;
    game.vm.call_event(cut, "OnResolveGameState", vec![]).map_err(|e| e.to_string())?;
    game.vm.call_event(cut, "Play", vec![]).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
/// What `grim-play` does with its command line.
pub fn main_cli() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut map = "PrivetDr".to_string();
    let (mut out, mut at, mut look, mut video) = (None, None, None, None);
    let mut cut = None;
    let (mut ticks, mut seconds) = (60u32, 20.0f32);
    let mut trace = None;
    let mut size: Option<(u32, u32)> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--shot" => out = it.next().cloned(),
            "--video" => video = it.next().cloned(),
            "--seconds" => seconds = it.next().and_then(|v| v.parse().ok()).unwrap_or(20.0),
            "--at" => at = it.next().cloned(),
            "--look" => look = it.next().cloned(),
            "--ticks" => ticks = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--trace" => trace = it.next().cloned(),
            "--cut" => cut = it.next().cloned(),
            "--size" => size = it.next().and_then(|v| v.split_once('x')).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?))),
            other => map = other.to_string(),
        }
    }
    let root = grim_testkit::require_game_dir();
    let fs: Arc<dyn FileSystem> = Arc::new(grim_fs::NativeFs);
    let music = grim_audio_cpal::MusicFiles { fs: fs.clone(), dir: root.join("Music") };
    let mut recorded: Option<grim_audio_cpal::OfflineAudio> = None;
    // A recording mixes the sound into a file next to the picture; a window plays it. Nothing
    // else needs a device, and `GRIM_NO_AUDIO` asks for silence.
    let audio: Option<Box<dyn grim_audio::Audio>> = if std::env::var_os("GRIM_NO_AUDIO").is_some() || out.is_some() {
        None
    } else if video.is_some() {
        let offline = grim_audio_cpal::OfflineAudio::new(RECORD_RATE, music);
        recorded = Some(offline.clone());
        Some(Box::new(offline))
    } else {
        match grim_audio_cpal::CpalAudio::open(music) {
            Ok(device) => Some(Box::new(device)),
            Err(e) => {
                eprintln!("audio: {e}");
                None
            }
        }
    };
    let mut game = match Game::load(fs, &root, &map, audio) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    // Only when asked for: the level is already loaded by now, so overwriting this would throw
    // away what GRIM_TRACE_FUNC was tracing through the startup.
    if trace.is_some() {
        game.vm.trace_func = trace;
    }
    let numbers = |s: &str| -> Vec<f32> { s.split(',').filter_map(|v| v.trim().parse().ok()).collect() };
    if let Some(v) = at.as_deref().map(numbers) {
        if v.len() == 3 {
            game.camera.position = [v[0], v[1], v[2]];
        }
    }
    if let Some(v) = look.as_deref().map(numbers) {
        if v.len() >= 2 {
            game.camera.rotation = [v[0] as i32, v[1] as i32, v.get(2).copied().unwrap_or(0.0) as i32];
        }
    }
    // `--cut <name>` plays one of the map's cutscenes from the start, the way the game does
    // when the state it belongs to is the one being played, so a recording can watch it.
    if let Some(name) = cut {
        if let Err(e) = play_cutscene(&mut game, &name) {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
    if let Some(file) = video {
        if let Err(e) = record(&mut game, &file, seconds, recorded) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    if let Some(out) = out {
        // Let the level settle (falling props reach the floor) before the frame.
        for _ in 0..ticks {
            let mut report = std::mem::take(&mut game.report);
            game.vm.tick(1.0 / 30.0, &mut report);
            game.report = report;
        }
        if let Err(e) = shot(&mut game, &out) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return;
    }
    // The same as pressing F9, for a recording or a screenshot that needs the spells.
    if std::env::var_os("GRIM_DEBUG_SPELLS").is_some() {
        game.grant_spells();
    }
    println!("{} actors; Tab releases the mouse, Escape opens the menu", game.level.actors.len());
    if game.debug {
        println!(
            "debug keys: Delete is the free camera, F8 shows the collision shapes, F9 gives Harry every spell \
             (GRIM_NO_DEBUG=1 turns them off)"
        );
    }
    game.window_size = size;
    let event_loop = EventLoop::new().expect("cannot create the event loop");
    event_loop.set_control_flow(ControlFlow::Poll);
    event_loop.run_app(&mut game).expect("event loop failed");
}
