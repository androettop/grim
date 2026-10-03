//! The game's sound in a browser.
//!
//! The mixing runs on the page's own thread, like everything else, so it is not left to the
//! browser to call it whenever it wants sound: that came in the middle of the frames and the
//! two starved each other. A timer mixes short blocks instead and queues them on Web Audio's
//! clock a little ahead of what is playing.

use grim_audio_cpal::{MusicFiles, OfflineAudio};
use wasm_bindgen::prelude::*;
use web_sys::{AudioContext, AudioContextState};

/// Frames mixed at a time.
const BLOCK: u32 = 1024;
/// How far ahead of what is heard the blocks are queued, in seconds: more than a slow frame,
/// so one does not cut the sound.
const LEAD: f64 = 0.15;
/// How often the queue is topped up, in milliseconds.
const PUMP_MS: i32 = 20;

/// Starts the sound and returns what the engine plays it through.
pub fn start(music: MusicFiles) -> Result<OfflineAudio, String> {
    let err = |e: JsValue| format!("{e:?}");
    let context = AudioContext::new().map_err(err)?;
    let rate = context.sample_rate();
    let audio = OfflineAudio::new(rate as u32, music);
    let mut mixer = audio.clone();
    let mut queued_until = 0.0f64;
    let mut block = vec![0.0f32; BLOCK as usize * 2];
    let (mut left, mut right) = (vec![0.0f32; BLOCK as usize], vec![0.0f32; BLOCK as usize]);
    let pump = Closure::<dyn FnMut()>::new(move || {
        // A context made without a click on the page starts suspended; any later call can
        // wake it once the player has clicked.
        if context.state() == AudioContextState::Suspended {
            let _ = context.resume();
            return;
        }
        let now = context.current_time();
        // Fallen behind (the tab was in the background): start again from now.
        if queued_until < now {
            queued_until = now + 0.02;
        }
        while queued_until < now + LEAD {
            mixer.render(&mut block);
            for (i, frame) in block.chunks_exact(2).enumerate() {
                left[i] = frame[0];
                right[i] = frame[1];
            }
            let Ok(buffer) = context.create_buffer(2, BLOCK, rate) else { return };
            let _ = buffer.copy_to_channel(&left, 0);
            let _ = buffer.copy_to_channel(&right, 1);
            let Ok(source) = context.create_buffer_source() else { return };
            source.set_buffer(Some(&buffer));
            let _ = source.connect_with_audio_node(&context.destination());
            let _ = source.start_with_when(queued_until);
            queued_until += BLOCK as f64 / rate as f64;
        }
    });
    let window = web_sys::window().ok_or("no window")?;
    window
        .set_interval_with_callback_and_timeout_and_arguments_0(pump.as_ref().unchecked_ref(), PUMP_MS)
        .map_err(err)?;
    // The pump runs for as long as the page is open.
    pump.forget();
    Ok(audio)
}
