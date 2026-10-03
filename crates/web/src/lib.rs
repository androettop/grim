//! The game in a browser.
//!
//! The page does two things with this module. In a worker, it unpacks the disc image the
//! player picked ([`disc_languages`], [`unpack_disc`]): reads of a `File` can only be made
//! synchronously there. On the page itself it hands over the unpacked files ([`add_file`]) and
//! starts the game in its canvas ([`play`]).

#![cfg(target_arch = "wasm32")]

use std::path::Path;
use std::sync::Arc;

mod audio;

use grim_fs::{FileSystem, MemoryFs};
use wasm_bindgen::prelude::*;

/// Where the unpacked files sit in memory: the install's root.
const ROOT: &str = "HP2";

thread_local! {
    static FILES: Arc<MemoryFs> = Arc::new(MemoryFs::new());
}

#[wasm_bindgen(start)]
fn start() {
    console_error_panic_hook::set_once();
}

/// A `File` the player picked, read a stretch at a time.
struct BlobSource(web_sys::Blob);

impl grim_disc::Source for BlobSource {
    fn len(&self) -> u64 {
        self.0.size() as u64
    }

    fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<(), String> {
        let err = |e: JsValue| format!("reading the disc image at {offset}: {e:?}");
        let slice = self.0.slice_with_f64_and_f64(offset as f64, (offset + buf.len() as u64) as f64).map_err(err)?;
        let bytes = web_sys::FileReaderSync::new().and_then(|r| r.read_as_array_buffer(&slice)).map_err(err)?;
        let bytes = js_sys::Uint8Array::new(&bytes);
        if bytes.length() as usize != buf.len() {
            return Err(format!("reading the disc image at {offset}: {} of {} bytes", bytes.length(), buf.len()));
        }
        bytes.copy_to(buf);
        Ok(())
    }
}

fn open_disc(image: web_sys::Blob) -> Result<grim_disc::Disc, JsValue> {
    grim_disc::Disc::open(Arc::new(BlobSource(image))).map_err(|e| JsValue::from_str(&e))
}

/// The languages on a disc image, the one installed by default first, if it has one.
#[wasm_bindgen]
pub fn disc_languages(image: web_sys::Blob) -> Result<js_sys::Array, JsValue> {
    let disc = open_disc(image)?;
    let mut languages = disc.languages.clone();
    if disc.default_language().is_none() {
        languages.retain(|l| l != "int");
    }
    Ok(languages.iter().map(|l| JsValue::from_str(l)).collect())
}

/// Unpacks the game in `language` from a disc image, calling `on_file(path, bytes, done, total)`
/// for every file. The other languages' dialog, which only checks the parser, is left out.
#[wasm_bindgen]
pub fn unpack_disc(image: web_sys::Blob, language: &str, on_file: &js_sys::Function) -> Result<(), JsValue> {
    let disc = open_disc(image)?;
    disc.install(language, &|p| !p.starts_with("Extra"), &mut |path, data, done, total| {
        let args = js_sys::Array::of4(
            &JsValue::from_str(&path.to_string_lossy()),
            &js_sys::Uint8Array::from(&data[..]),
            &JsValue::from(done as u32),
            &JsValue::from(total as u32),
        );
        on_file.apply(&JsValue::NULL, &args).map(|_| ()).map_err(|e| format!("{e:?}"))
    })
    .map_err(|e| JsValue::from_str(&e))
}

/// Hands one unpacked file over, by its path in the install (`System/Core.u`).
#[wasm_bindgen]
pub fn add_file(path: &str, data: Vec<u8>) {
    FILES.with(|fs| fs.insert(Path::new(ROOT).join(path), data));
}

/// Loads `map` and plays it in the page's `<canvas id="grim">`.
#[wasm_bindgen]
pub fn play(map: &str, debug: bool) -> Result<(), JsValue> {
    let fs: Arc<dyn FileSystem> = FILES.with(|f| f.clone());
    let root = Path::new(ROOT);
    let music = grim_audio_cpal::MusicFiles { fs: fs.clone(), dir: root.join("Music") };
    let audio: Option<Box<dyn grim_audio::Audio>> = match audio::start(music) {
        Ok(a) => Some(Box::new(a)),
        Err(e) => {
            web_sys::console::warn_1(&format!("audio: {e}").into());
            None
        }
    };
    let mut game = grim_game::Game::load(fs, root, map, audio).map_err(|e| JsValue::from_str(&e))?;
    game.set_debug(debug);
    grim_game::run_in_browser(game);
    Ok(())
}
