//! The mixer's own arithmetic, on made-up samples: no game data is involved.

use grim_audio::{Kind, Listener, Mixer, Play, Samples, Volumes};

fn tone(rate: u32, frames: usize) -> Samples {
    Samples { rate, channels: 1, data: (0..frames).map(|i| if i % 2 == 0 { 16000 } else { -16000 }).collect() }
}

fn peak(out: &[f32]) -> f32 {
    out.iter().fold(0.0f32, |a, s| a.max(s.abs()))
}

#[test]
fn a_voice_sounds_and_then_ends() {
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 4410));
    mixer.play(Play::new(id));
    let mut out = vec![0.0; 4410 * 2];
    mixer.render(&mut out);
    assert!(peak(&out) > 0.1, "the voice should be heard");
    assert_eq!(mixer.voices(), 0, "a sound that has run out is dropped");
}

#[test]
fn a_slot_holds_one_sound_at_a_time() {
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 44100));
    let mut play = Play::new(id);
    play.slot = 3;
    mixer.play(play);
    mixer.play(play);
    assert_eq!(mixer.voices(), 1, "the new sound takes the slot");
    let mut no_override = play;
    no_override.no_override = true;
    assert!(mixer.play(no_override).is_none(), "and one that may not override is refused");
}

#[test]
fn distance_and_the_settings_turn_a_sound_down() {
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 44100));
    mixer.set_listener(Listener { at: [0.0; 3], forward: [1.0, 0.0, 0.0], right: [0.0, 1.0, 0.0] });
    let mut play = Play::new(id);
    play.radius = 100.0;
    play.at = [90.0, 0.0, 0.0];
    mixer.play(play);
    let mut far = vec![0.0; 2205 * 2];
    mixer.render(&mut far);
    let mut near_play = play;
    near_play.at = [5.0, 0.0, 0.0];
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 44100));
    near_play.sound = id;
    mixer.play(near_play);
    let mut near = vec![0.0; 2205 * 2];
    mixer.render(&mut near);
    assert!(peak(&near) > peak(&far) * 2.0, "what is close is louder: {} vs {}", peak(&near), peak(&far));

    // Turning the effects down turns the voice down with them.
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 44100));
    mixer.set_volumes(Volumes { master: 1.0, effects: 0.25, music: 1.0, speech: 1.0 });
    mixer.play(Play { kind: Kind::Effect, ..Play::new(id) });
    let mut quiet = vec![0.0; 2205 * 2];
    mixer.render(&mut quiet);
    assert!(peak(&quiet) < 0.2, "a quarter of the volume is a quarter as loud: {}", peak(&quiet));
}

#[test]
fn a_looping_voice_keeps_going() {
    let mut mixer = Mixer::new(44100);
    let id = mixer.add_sound(1, tone(44100, 441));
    let mut play = Play::new(id);
    play.looping = true;
    mixer.play(play);
    let mut out = vec![0.0; 44100 * 2];
    mixer.render(&mut out);
    assert_eq!(mixer.voices(), 1, "a loop outlives its own length");
    assert!(peak(&out) > 0.1);
}
