//! Audio backend. The mixing is `grim_audio`'s; this crate finds the sound card, keeps the
//! mixer fed from the audio thread, and decodes the game's music (Ogg Vorbis files in `Music/`).
//!
//! It also offers an offline backend that mixes into a buffer instead of a device, which is
//! what a recording uses to put the game's sound into a video.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use grim_fs::FileSystem;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use grim_audio::{Audio, Kind, Listener, Mixer, Play, Samples, SoundId, SoundParam, Volumes};

/// Music is mixed as one more voice, with this owner so it can be stopped on its own.
const MUSIC_OWNER: u64 = u64::MAX;
const MUSIC_SLOT: u8 = 200;

/// Where the game's songs are: its `Music` folder, in whatever file system holds the game.
#[derive(Clone)]
pub struct MusicFiles {
    pub fs: Arc<dyn FileSystem>,
    pub dir: PathBuf,
}

/// Shared between the caller and the audio thread.
struct Shared {
    mixer: Mixer,
    /// Handle to voice, for `StopMusic`.
    music: Vec<(u32, grim_audio::VoiceId)>,
    next_music: u32,
}

/// Decodes an Ogg Vorbis file to 16-bit PCM.
pub fn decode_ogg(data: Vec<u8>) -> Result<Samples, String> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(data)), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("ogg");
    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;
    let track = format.default_track().ok_or("no track")?;
    let (id, rate, channels) = (
        track.id,
        track.codec_params.sample_rate.unwrap_or(44100),
        track.codec_params.channels.map_or(2, |c| c.count() as u16),
    );
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let mut data: Vec<i16> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // The end of the stream arrives as an error; anything else stops the decode with
            // what has been read so far, which still plays.
            Err(_) => break,
        };
        if packet.track_id() != id {
            continue;
        }
        let Ok(decoded) = decoder.decode(&packet) else { break };
        let mut buf = symphonia::core::audio::SampleBuffer::<i16>::new(decoded.capacity() as u64, *decoded.spec());
        buf.copy_interleaved_ref(decoded);
        data.extend_from_slice(buf.samples());
    }
    Ok(Samples { rate, channels, data })
}

/// Mixes into a buffer the caller asks for, with no device at all: what a recording uses. It
/// is shared, so the one that records can hand a copy to the engine and keep one to read the
/// mix from.
#[derive(Clone)]
pub struct OfflineAudio {
    shared: Arc<Mutex<Shared>>,
    music: MusicFiles,
}

impl OfflineAudio {
    pub fn new(rate: u32, music: MusicFiles) -> Self {
        let shared = Shared { mixer: Mixer::new(rate), music: Vec::new(), next_music: 1 };
        Self { shared: Arc::new(Mutex::new(shared)), music }
    }

    /// Mixes the next stretch of sound, interleaved stereo.
    pub fn render(&mut self, out: &mut [f32]) {
        match self.shared.lock() {
            Ok(mut s) => s.mixer.render(out),
            Err(_) => out.fill(0.0),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Shared) -> T) -> Option<T> {
        self.shared.lock().ok().map(|mut s| f(&mut s))
    }
}

impl Audio for OfflineAudio {
    fn add_sound(&mut self, key: u64, samples: Samples) -> SoundId {
        self.with(|s| s.mixer.add_sound(key, samples)).unwrap_or(SoundId(u32::MAX))
    }
    fn sound_of(&self, key: u64) -> Option<SoundId> {
        self.with(|s| s.mixer.sound_of(key)).flatten()
    }
    fn play(&mut self, play: Play) -> Option<grim_audio::VoiceId> {
        self.with(|s| s.mixer.play(play)).flatten()
    }
    fn move_owner(&mut self, owner: u64, at: [f32; 3]) {
        self.with(|s| s.mixer.move_owner(owner, at));
    }
    fn stop(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, fade: f32) {
        self.with(|s| s.mixer.stop(owner, slot, sound, fade));
    }
    fn modify(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, param: SoundParam, value: f32) -> bool {
        self.with(|s| s.mixer.modify(owner, slot, sound, param, value)).unwrap_or(false)
    }
    fn playing(&self) -> Vec<grim_audio::Sounding> {
        self.shared.lock().map(|s| s.mixer.playing()).unwrap_or_default()
    }
    fn loudness(&self, owner: u64, slot: u8, seconds: f32) -> f32 {
        self.shared.lock().map(|s| s.mixer.loudness(owner, slot, seconds)).unwrap_or(0.0)
    }
    fn set_listener(&mut self, listener: Listener) {
        self.with(|s| s.mixer.set_listener(listener));
    }
    fn set_volumes(&mut self, volumes: Volumes) {
        self.with(|s| s.mixer.set_volumes(volumes));
    }
    fn play_music(&mut self, song: &str, fade_in: f32) -> u32 {
        let files = self.music.clone();
        self.with(|s| {
            let Some(id) = load_music(&mut s.mixer, &files, song) else { return 0 };
            let Shared { mixer, music, next_music } = s;
            start_music(mixer, music, next_music, id, fade_in)
        })
        .unwrap_or(0)
    }
    fn stop_music(&mut self, handle: u32, fade_out: f32) {
        self.with(|s| {
            let Shared { mixer, music, .. } = s;
            stop_music(mixer, music, handle, fade_out);
        });
    }
    fn stop_all_music(&mut self, fade_out: f32) {
        self.with(|s| {
            s.mixer.stop(MUSIC_OWNER, None, None, fade_out);
            s.music.clear();
        });
    }
    fn voices(&self) -> usize {
        self.with(|s| s.mixer.voices()).unwrap_or(0)
    }
}

/// The sound card, fed from cpal's own thread.
pub struct CpalAudio {
    shared: Arc<Mutex<Shared>>,
    music: MusicFiles,
    // Dropping the stream stops the sound, so it is kept alive here.
    _stream: cpal::Stream,
}

impl CpalAudio {
    pub fn open(music: MusicFiles) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no audio output device")?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let rate = config.sample_rate().0;
        let channels = config.channels() as usize;
        let shared = Arc::new(Mutex::new(Shared { mixer: Mixer::new(rate), music: Vec::new(), next_music: 1 }));
        let feed = shared.clone();
        let mut scratch: Vec<f32> = Vec::new();
        let stream = device
            .build_output_stream(
                &config.into(),
                move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
                    let frames = out.len() / channels.max(1);
                    scratch.resize(frames * 2, 0.0);
                    match feed.lock() {
                        Ok(mut shared) => shared.mixer.render(&mut scratch),
                        // A mixer left locked by a panicking caller must not take the sound
                        // card down with it: the device simply plays silence.
                        Err(_) => scratch.fill(0.0),
                    }
                    for (i, frame) in out.chunks_mut(channels.max(1)).enumerate() {
                        let (l, r) = (scratch[i * 2], scratch[i * 2 + 1]);
                        match frame.len() {
                            0 => {}
                            1 => frame[0] = (l + r) * 0.5,
                            _ => {
                                frame[0] = l;
                                frame[1] = r;
                                for s in &mut frame[2..] {
                                    *s = 0.0;
                                }
                            }
                        }
                    }
                },
                |e| eprintln!("audio: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self { shared, music, _stream: stream })
    }

    fn with<T>(&self, f: impl FnOnce(&mut Shared) -> T) -> Option<T> {
        self.shared.lock().ok().map(|mut s| f(&mut s))
    }
}

impl Audio for CpalAudio {
    fn add_sound(&mut self, key: u64, samples: Samples) -> SoundId {
        self.with(|s| s.mixer.add_sound(key, samples)).unwrap_or(SoundId(u32::MAX))
    }
    fn sound_of(&self, key: u64) -> Option<SoundId> {
        self.with(|s| s.mixer.sound_of(key)).flatten()
    }
    fn play(&mut self, play: Play) -> Option<grim_audio::VoiceId> {
        self.with(|s| s.mixer.play(play)).flatten()
    }
    fn move_owner(&mut self, owner: u64, at: [f32; 3]) {
        self.with(|s| s.mixer.move_owner(owner, at));
    }
    fn stop(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, fade: f32) {
        self.with(|s| s.mixer.stop(owner, slot, sound, fade));
    }
    fn modify(&mut self, owner: u64, slot: Option<u8>, sound: Option<SoundId>, param: SoundParam, value: f32) -> bool {
        self.with(|s| s.mixer.modify(owner, slot, sound, param, value)).unwrap_or(false)
    }
    fn playing(&self) -> Vec<grim_audio::Sounding> {
        self.shared.lock().map(|s| s.mixer.playing()).unwrap_or_default()
    }
    fn loudness(&self, owner: u64, slot: u8, seconds: f32) -> f32 {
        self.shared.lock().map(|s| s.mixer.loudness(owner, slot, seconds)).unwrap_or(0.0)
    }
    fn set_listener(&mut self, listener: Listener) {
        self.with(|s| s.mixer.set_listener(listener));
    }
    fn set_volumes(&mut self, volumes: Volumes) {
        self.with(|s| s.mixer.set_volumes(volumes));
    }
    fn play_music(&mut self, song: &str, fade_in: f32) -> u32 {
        let files = self.music.clone();
        self.with(|s| {
            let Some(id) = load_music(&mut s.mixer, &files, song) else { return 0 };
            let Shared { mixer, music, next_music } = s;
            start_music(mixer, music, next_music, id, fade_in)
        })
        .unwrap_or(0)
    }
    fn stop_music(&mut self, handle: u32, fade_out: f32) {
        self.with(|s| {
            let Shared { mixer, music, .. } = s;
            stop_music(mixer, music, handle, fade_out);
        });
    }
    fn stop_all_music(&mut self, fade_out: f32) {
        self.with(|s| {
            s.mixer.stop(MUSIC_OWNER, None, None, fade_out);
            s.music.clear();
        });
    }
    fn voices(&self) -> usize {
        self.with(|s| s.mixer.voices()).unwrap_or(0)
    }
}

/// Finds a song in the game's `Music` folder and loads it once. The scripts name it without an
/// extension, and the files are Ogg Vorbis.
fn load_music(mixer: &mut Mixer, files: &MusicFiles, song: &str) -> Option<SoundId> {
    let key = music_key(song);
    if let Some(id) = mixer.sound_of(key) {
        return Some(id);
    }
    let name = song.rsplit(['/', '\\']).next().unwrap_or(song);
    let stem = name.split('.').next().unwrap_or(name);
    let file = files
        .fs
        .read_dir(&files.dir)
        .ok()?
        .into_iter()
        .map(|e| e.path)
        .find(|p| p.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(stem)))?;
    let samples = decode_ogg(files.fs.read(&file).ok()?.to_vec()).ok()?;
    Some(mixer.add_sound(key, samples))
}

/// A song's key in the mixer, kept apart from the packages' sounds (which are keyed by object).
fn music_key(song: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in song.to_ascii_lowercase().bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    // The top bit keeps music keys away from the object keys the engine passes.
    h | 0x8000_0000_0000_0000
}

fn start_music(mixer: &mut Mixer, music: &mut Vec<(u32, grim_audio::VoiceId)>, next: &mut u32, id: SoundId, fade_in: f32) -> u32 {
    let mut play = Play::new(id);
    play.owner = MUSIC_OWNER;
    play.slot = MUSIC_SLOT;
    play.looping = true;
    play.kind = Kind::Music;
    let Some(voice) = mixer.play_faded(play, fade_in) else { return 0 };
    let handle = *next;
    *next += 1;
    music.push((handle, voice));
    handle
}

fn stop_music(mixer: &mut Mixer, music: &mut Vec<(u32, grim_audio::VoiceId)>, handle: u32, fade_out: f32) {
    if let Some(i) = music.iter().position(|(h, _)| *h == handle) {
        mixer.stop_voice(music[i].1, fade_out);
        music.remove(i);
    }
}
