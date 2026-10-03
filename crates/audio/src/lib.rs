//! Audio interface. Backends (cpal, a file writer, ...) implement [`Audio`]; the engine only
//! ever talks to this crate.
//!
//! The mixing itself is here, in [`Mixer`]: it is plain arithmetic over the samples the game
//! ships, so every backend shares it and only has to hand the result to a device or a file.

/// Sound slots, as `Engine.Actor.ESoundSlot` names them. One sound per slot per actor: a new
/// one in the same slot takes over from the one playing, which is how Unreal keeps an actor
/// from talking over itself.
pub const SLOT_NONE: u8 = 0;

/// Where the listener is and which way it faces.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Listener {
    pub at: [f32; 3],
    pub forward: [f32; 3],
    pub right: [f32; 3],
}

impl Default for Listener {
    fn default() -> Self {
        Self { at: [0.0; 3], forward: [1.0, 0.0, 0.0], right: [0.0, 1.0, 0.0] }
    }
}

/// How loud each kind of sound is, as the game's own settings name them. 1 is unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volumes {
    pub master: f32,
    pub effects: f32,
    pub music: f32,
    pub speech: f32,
}

impl Default for Volumes {
    fn default() -> Self {
        Self { master: 1.0, effects: 1.0, music: 1.0, speech: 1.0 }
    }
}

/// What `Actor.ModifySound` can change about a sound already playing, as
/// `Engine.Actor.ESoundParam` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundParam {
    Volume,
    Radius,
    Pitch,
}

/// What a sound counts as, for the settings that turn each kind up or down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Effect,
    Speech,
    Music,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SoundId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VoiceId(pub u32);

/// 16-bit PCM, which is what the game's packages hold once decoded.
#[derive(Debug, Clone)]
pub struct Samples {
    pub rate: u32,
    pub channels: u16,
    pub data: Vec<i16>,
}

/// One request to play a sound, in the terms `Actor.PlaySound` uses.
#[derive(Debug, Clone, Copy)]
pub struct Play {
    pub sound: SoundId,
    /// The actor it comes from, as an opaque number: the mixer only compares them.
    pub owner: u64,
    pub slot: u8,
    pub volume: f32,
    /// How far it carries. Zero means it is heard everywhere, at full volume.
    pub radius: f32,
    /// 1 is the sound's own speed. Unreal stores this as a byte where 64 is 1.
    pub pitch: f32,
    pub looping: bool,
    pub at: [f32; 3],
    /// Does not take over the slot if something is already playing in it.
    pub no_override: bool,
    pub kind: Kind,
}

impl Play {
    pub fn new(sound: SoundId) -> Self {
        Self {
            sound,
            owner: 0,
            slot: SLOT_NONE,
            volume: 1.0,
            radius: 0.0,
            pitch: 1.0,
            looping: false,
            at: [0.0; 3],
            no_override: false,
            kind: Kind::Effect,
        }
    }
}

/// What the engine asks of an audio backend.
pub trait Audio {
    /// Hands over a decoded sound; the same id is returned for the same `key` afterwards.
    fn add_sound(&mut self, key: u64, samples: Samples) -> SoundId;
    /// Whether that sound is already loaded.
    fn sound_of(&self, key: u64) -> Option<SoundId>;
    fn play(&mut self, play: Play) -> Option<VoiceId>;
    /// Moves what an actor is playing, so a sound follows whatever makes it.
    fn move_owner(&mut self, owner: u64, at: [f32; 3]);
    /// `StopSound`: silences an owner's voices, narrowed by slot or by which sound it is.
    fn stop(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, fade: f32);
    /// `ModifySound`: changes one thing about what an owner is already playing, and answers
    /// whether anything was. The game uses the answer to decide whether to start the sound.
    fn modify(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, param: SoundParam, value: f32) -> bool;
    fn set_listener(&mut self, listener: Listener);
    fn set_volumes(&mut self, volumes: Volumes);
    /// Starts a piece of music, by the name the scripts use. Answers a handle for `stop_music`.
    fn play_music(&mut self, song: &str, fade_in: f32) -> u32;
    fn stop_music(&mut self, handle: u32, fade_out: f32);
    fn stop_all_music(&mut self, fade_out: f32);
    /// How many voices are sounding, which is what a test or a debug overlay asks for.
    fn voices(&self) -> usize;
    /// What is sounding: owner, slot, sound, volume and pitch of each voice, for a diagnostic
    /// that has to say whether a sound the scripts asked for is actually playing.
    fn playing(&self) -> Vec<Sounding>;
    /// How loud an owner's voice in a slot is right now, over a window of `seconds`. The
    /// engine opens a talking character's mouth by it.
    fn loudness(&self, owner: u64, slot: u8, seconds: f32) -> f32;
    /// Silences every voice and every piece of music, which is what leaving a level does: what
    /// the old level was playing does not follow the player into the new one.
    fn stop_everything(&mut self) {
        let mut owners: Vec<u64> = self.playing().iter().map(|s| s.owner).collect();
        owners.dedup();
        for owner in owners {
            self.stop(owner, None, None, 0.0);
        }
        self.stop_all_music(0.0);
    }
}

/// One sounding voice, as a diagnostic sees it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sounding {
    pub owner: u64,
    pub slot: u8,
    pub sound: SoundId,
    pub volume: f32,
    pub radius: f32,
    pub pitch: f32,
    pub looping: bool,
    pub kind: Kind,
}

/// A sound being played.
#[derive(Debug, Clone, Copy)]
struct Voice {
    id: VoiceId,
    sound: SoundId,
    owner: u64,
    slot: u8,
    volume: f32,
    radius: f32,
    pitch: f32,
    looping: bool,
    at: [f32; 3],
    kind: Kind,
    /// Where playback has reached, in source frames.
    cursor: f64,
    /// Fading out towards silence: seconds left and how long the fade is.
    fade_out: Option<(f32, f32)>,
    fade_in: Option<(f32, f32)>,
}

/// Mixes the voices into an output buffer. Backends own one of these and feed it to a device.
pub struct Mixer {
    pub rate: u32,
    sounds: Vec<Samples>,
    keys: Vec<(u64, SoundId)>,
    voices: Vec<Voice>,
    next_voice: u32,
    listener: Listener,
    volumes: Volumes,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self {
            rate,
            sounds: Vec::new(),
            keys: Vec::new(),
            voices: Vec::new(),
            next_voice: 0,
            listener: Listener::default(),
            volumes: Volumes::default(),
        }
    }

    pub fn add_sound(&mut self, key: u64, samples: Samples) -> SoundId {
        if let Some(id) = self.sound_of(key) {
            return id;
        }
        let id = SoundId(self.sounds.len() as u32);
        self.sounds.push(samples);
        self.keys.push((key, id));
        id
    }

    pub fn sound_of(&self, key: u64) -> Option<SoundId> {
        self.keys.iter().find(|(k, _)| *k == key).map(|(_, id)| *id)
    }

    pub fn play(&mut self, play: Play) -> Option<VoiceId> {
        if play.sound.0 as usize >= self.sounds.len() {
            return None;
        }
        // One sound per slot per actor, as Unreal does it: the new one takes the slot unless it
        // was asked not to. Slot 0 (`SLOT_None`) is the exception, where sounds pile up.
        if play.slot != SLOT_NONE {
            let busy = self.voices.iter().any(|v| v.owner == play.owner && v.slot == play.slot);
            if busy {
                if play.no_override {
                    return None;
                }
                self.voices.retain(|v| !(v.owner == play.owner && v.slot == play.slot));
            }
        }
        let id = VoiceId(self.next_voice);
        self.next_voice += 1;
        self.voices.push(Voice {
            id,
            sound: play.sound,
            owner: play.owner,
            slot: play.slot,
            volume: play.volume,
            radius: play.radius,
            pitch: play.pitch.max(0.01),
            looping: play.looping,
            at: play.at,
            kind: play.kind,
            cursor: 0.0,
            fade_out: None,
            fade_in: None,
        });
        Some(id)
    }

    pub fn move_owner(&mut self, owner: u64, at: [f32; 3]) {
        for v in self.voices.iter_mut().filter(|v| v.owner == owner) {
            v.at = at;
        }
    }

    pub fn stop(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, fade: f32) {
        for v in self.voices.iter_mut() {
            let mine = v.owner == owner && slot.is_none_or(|s| v.slot == s) && sound.is_none_or(|s| v.sound == s);
            if mine {
                if fade <= 0.0 {
                    v.fade_out = Some((0.0, 0.0));
                } else {
                    v.fade_out = Some((fade, fade));
                }
            }
        }
        self.voices.retain(|v| !matches!(v.fade_out, Some((0.0, 0.0))));
    }

    pub fn modify(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, param: SoundParam, value: f32) -> bool {
        let mut found = false;
        for v in self.voices.iter_mut() {
            if v.owner != owner || !slot.is_none_or(|s| v.slot == s) || !sound.is_none_or(|s| v.sound == s) {
                continue;
            }
            found = true;
            match param {
                SoundParam::Volume => v.volume = value,
                SoundParam::Radius => v.radius = value,
                SoundParam::Pitch => v.pitch = value.max(0.01),
            }
        }
        found
    }

    pub fn stop_voice(&mut self, voice: VoiceId, fade: f32) {
        if fade <= 0.0 {
            self.voices.retain(|v| v.id != voice);
            return;
        }
        for v in self.voices.iter_mut().filter(|v| v.id == voice) {
            v.fade_out = Some((fade, fade));
        }
    }

    pub fn set_listener(&mut self, listener: Listener) {
        self.listener = listener;
    }

    pub fn set_volumes(&mut self, volumes: Volumes) {
        self.volumes = volumes;
    }

    pub fn volumes(&self) -> Volumes {
        self.volumes
    }

    pub fn voices(&self) -> usize {
        self.voices.len()
    }

    pub fn playing(&self) -> Vec<Sounding> {
        self.voices
            .iter()
            .map(|v| Sounding {
                owner: v.owner,
                slot: v.slot,
                sound: v.sound,
                volume: v.volume,
                radius: v.radius,
                pitch: v.pitch,
                looping: v.looping,
                kind: v.kind,
            })
            .collect()
    }

    /// How loud what an owner is playing in a slot sounds right now, 0 to 1. A character's
    /// mouth is driven by this: the meshes carry a two-key `mouth_anim` (shut and open) and
    /// nothing in the scripts moves it, so the engine has to open it from the speech itself.
    /// A window of `seconds` around where playback has reached is measured, not one sample,
    /// so a frame's worth of sound gives one mouth position.
    pub fn loudness(&self, owner: u64, slot: u8, seconds: f32) -> f32 {
        let Some(v) = self.voices.iter().find(|v| v.owner == owner && v.slot == slot) else { return 0.0 };
        let Some(sound) = self.sounds.get(v.sound.0 as usize) else { return 0.0 };
        let channels = sound.channels.max(1) as usize;
        let frames = sound.data.len() / channels;
        if frames == 0 {
            return 0.0;
        }
        let window = ((seconds.max(0.0) * sound.rate as f32) as usize).clamp(1, frames);
        let at = (v.cursor as usize).min(frames.saturating_sub(1));
        let end = (at + window).min(frames);
        let mut peak = 0.0f32;
        for f in at..end {
            for c in 0..channels {
                peak = peak.max((sound.data[f * channels + c] as f32 / i16::MAX as f32).abs());
            }
        }
        peak.clamp(0.0, 1.0)
    }

    /// Starts a voice that fades in, for music.
    pub fn play_faded(&mut self, play: Play, fade_in: f32) -> Option<VoiceId> {
        let id = self.play(play)?;
        if fade_in > 0.0 {
            if let Some(v) = self.voices.iter_mut().find(|v| v.id == id) {
                v.fade_in = Some((fade_in, fade_in));
            }
        }
        Some(id)
    }


    /// Fills `out` (interleaved stereo) with what is sounding, and advances every voice.
    pub fn render(&mut self, out: &mut [f32]) {
        out.fill(0.0);
        let frames = out.len() / 2;
        let seconds = frames as f32 / self.rate as f32;
        let listener_rate = self.rate as f64;
        let mut done: Vec<VoiceId> = Vec::new();
        for v in self.voices.iter_mut() {
            let Some(sound) = self.sounds.get(v.sound.0 as usize) else {
                done.push(v.id);
                continue;
            };
            let channels = sound.channels.max(1) as usize;
            let source_frames = sound.data.len() / channels;
            if source_frames == 0 {
                done.push(v.id);
                continue;
            }
            let step = sound.rate as f64 / listener_rate * v.pitch as f64;
            // The volume this block starts and ends at, so a fade is smooth across it.
            let fade = |left: Option<(f32, f32)>, out: bool| -> (f32, f32) {
                match left {
                    Some((left, whole)) if whole > 0.0 => {
                        let a = (left / whole).clamp(0.0, 1.0);
                        let b = ((left - seconds) / whole).clamp(0.0, 1.0);
                        if out { (a, b) } else { (1.0 - a, 1.0 - b) }
                    }
                    Some(_) => (0.0, 0.0),
                    None => (1.0, 1.0),
                }
            };
            let (out_a, out_b) = fade(v.fade_out, true);
            let (in_a, in_b) = fade(v.fade_in, false);
            let (gain, pan) = placement(v, &self.listener, &self.volumes);
            // Equal-power panning: a sound to the side keeps its loudness as it crosses.
            let (left_gain, right_gain) = ((1.0 - pan).max(0.0).sqrt(), (1.0 + pan).max(0.0).sqrt());
            let mut cursor = v.cursor;
            let mut ended = false;
            for (i, frame) in out.chunks_mut(2).enumerate() {
                let t = i as f32 / frames.max(1) as f32;
                let envelope = (out_a + (out_b - out_a) * t) * (in_a + (in_b - in_a) * t);
                let index = cursor as usize;
                if index >= source_frames {
                    if v.looping {
                        cursor = 0.0;
                    } else {
                        ended = true;
                        break;
                    }
                }
                let index = (cursor as usize).min(source_frames - 1);
                let at = index * channels;
                let mono = if channels == 1 {
                    sound.data[at] as f32
                } else {
                    (sound.data[at] as f32 + sound.data[at + 1] as f32) * 0.5
                };
                let sample = mono / 32768.0 * gain * envelope;
                frame[0] += sample * left_gain;
                frame[1] += sample * right_gain;
                cursor += step;
            }
            v.cursor = cursor;
            let tick = |f: &mut Option<(f32, f32)>| {
                if let Some((left, _)) = f {
                    *left -= seconds;
                }
            };
            tick(&mut v.fade_out);
            tick(&mut v.fade_in);
            let ran_out = !v.looping && cursor >= source_frames as f64;
            if ended || ran_out || matches!(v.fade_out, Some((left, _)) if left <= 0.0) {
                done.push(v.id);
            }
            if matches!(v.fade_in, Some((left, _)) if left <= 0.0) {
                v.fade_in = None;
            }
        }
        self.voices.retain(|v| !done.contains(&v.id));
        // Nothing is allowed to clip the output; the loudest voice wins gracefully.
        for s in out.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }
    }
}

/// How loud a voice is and how it sits between the ears, from where it is and how far it
/// carries. Unreal fades a sound out linearly over its radius; a sound with no radius is heard
/// everywhere, which is what the interface sounds and the music use.
fn placement(v: &Voice, listener: &Listener, volumes: &Volumes) -> (f32, f32) {
    let kind = match v.kind {
        Kind::Effect => volumes.effects,
        Kind::Speech => volumes.speech,
        Kind::Music => volumes.music,
    };
    let gain = v.volume * kind * volumes.master;
    if v.radius <= 0.0 {
        return (gain, 0.0);
    }
    let to = [0, 1, 2].map(|i| v.at[i] - listener.at[i]);
    let distance = (to[0] * to[0] + to[1] * to[1] + to[2] * to[2]).sqrt();
    if distance >= v.radius {
        return (0.0, 0.0);
    }
    let near = 1.0 - distance / v.radius;
    let pan = if distance > 1.0 {
        let r = listener.right;
        (to[0] * r[0] + to[1] * r[1] + to[2] * r[2]) / distance
    } else {
        0.0
    };
    (gain * near, pan.clamp(-1.0, 1.0))
}
