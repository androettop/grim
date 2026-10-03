//! Reads the player's own copy of the game: the music it ships must decode.

#[test]
fn the_games_music_decodes() {
    let Some(root) = grim_testkit::game_dir() else { return };
    let dir = root.join("Music");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut files: Vec<_> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "ogg")).collect();
    files.sort();
    assert!(!files.is_empty(), "no music in {}", dir.display());
    // Decoding all 141 would read 64MB; the first few prove the format is read.
    for path in files.iter().take(3) {
        let pcm = grim_audio_cpal::decode_ogg(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert!(pcm.rate >= 8000, "{}: rate {}", path.display(), pcm.rate);
        assert!(pcm.channels >= 1);
        assert!(pcm.data.len() > pcm.rate as usize, "{}: only {} samples", path.display(), pcm.data.len());
    }
}

#[test]
fn a_song_of_the_game_can_be_played() {
    use grim_audio::Audio;
    let Some(root) = grim_testkit::game_dir() else { return };
    let dir = root.join("Music");
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.path().file_name().map(|n| n.to_string_lossy().to_string()))
        .filter(|n| n.ends_with(".ogg"))
        .collect();
    names.sort();
    let Some(song) = names.first() else { return };
    let mut audio = grim_audio_cpal::OfflineAudio::new(44100, &dir);
    // The scripts name a song the way the maps store it, extension and all.
    let handle = audio.play_music(song, 0.0);
    assert!(handle != 0, "{song} should have started");
    let mut out = vec![0.0f32; 44100 * 2];
    audio.render(&mut out);
    let peak = out.iter().fold(0.0f32, |a, s| a.max(s.abs()));
    assert!(peak > 0.01, "{song} should be heard: peak {peak}");
    audio.stop_all_music(0.0);
    let mut silence = vec![0.0f32; 4410 * 2];
    audio.render(&mut silence);
    assert!(silence.iter().all(|s| *s == 0.0), "stopping the music should leave silence");
}
