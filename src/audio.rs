//! Menu music and sound effects through SDL2_mixer.
//!
//! Audio is entirely optional. A missing file silences that one sound, and an
//! unusable sound device silences everything, but neither affects the menu.
//!
//! [`Audio`] belongs to the SDL session, so it is released with everything
//! else before a game starts. That matters most on plain ALSA, where a sound
//! device can be held by one program at a time.

use std::fmt::Display;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use sdl2::mixer::{self, Channel, Chunk, DEFAULT_FORMAT, InitFlag, MAX_VOLUME, Music};
use sdl2::{AudioSubsystem, Sdl};

use crate::config::AudioConfig;

// Opus is always 48 kHz, so this avoids resampling the recommended format.
const FREQUENCY: i32 = 48_000;
const STEREO: i32 = 2;
// About 43 ms. Slower machines underrun with less, and the effects are short
// enough that this latency goes unnoticed.
const CHUNK_SIZE: i32 = 2048;

/// Each effect has its own channel, and music plays outside the channels
/// altogether, so nothing interrupts anything else. Starting the selection
/// sound on a busy channel restarts it, so rapid scrolling never stacks up.
const SELECT_CHANNEL: Channel = Channel(0);
const LAUNCH_CHANNEL: Channel = Channel(1);
const CHANNEL_COUNT: i32 = 2;

const LOOP_FOREVER: i32 = -1;
const MUSIC_FADE_MS: i32 = 300;
/// The launch sound may hold the game back this long at most.
const MAX_LAUNCH_SOUND: Duration = Duration::from_secs(2);
const LAUNCH_POLL: Duration = Duration::from_millis(10);

/// Converts a 0-100 setting to SDL_mixer's volume range.
fn mixer_volume(percent: i64) -> i32 {
    percent.clamp(0, 100) as i32 * MAX_VOLUME / 100
}

fn any_sound_exists(config: &AudioConfig) -> bool {
    [&config.music, &config.select_sound, &config.launch_sound]
        .iter()
        .any(|path| path.is_file())
}

/// Loads one optional sound. Failure only ever costs that sound.
fn load<T, E: Display>(
    name: &str,
    path: &Path,
    loader: impl FnOnce(&Path) -> Result<T, E>,
) -> Option<T> {
    if !path.is_file() {
        log_info!("no {name}: {} not found", path.display());
        return None;
    }
    loader(path)
        .inspect_err(|e| log_warn!("cannot load {name} {}: {e}", path.display()))
        .ok()
}

pub struct Audio {
    music: Option<Music<'static>>,
    select: Option<Chunk>,
    launch: Option<Chunk>,
    _context: mixer::Sdl2MixerContext,
    _subsystem: AudioSubsystem,
}

impl Audio {
    /// Opens the sound device and loads every sound. Returns `None`, leaving
    /// audio untouched, if audio is disabled or there is nothing to play.
    pub fn open(sdl: &Sdl, config: &AudioConfig) -> Option<Self> {
        if !config.enabled {
            return None;
        }
        if !any_sound_exists(config) {
            log_info!("audio: no sound files found, running silently");
            return None;
        }
        let subsystem = sdl
            .audio()
            .inspect_err(|e| log_warn!("audio unavailable, running silently: {e}"))
            .ok()?;
        // Decoders load on demand, so no flags are needed: asking for specific
        // ones would fail outright if any single one were missing. This call
        // provides the handle that unloads them again.
        let context = mixer::init(InitFlag::empty())
            .inspect_err(|e| log_warn!("audio unavailable, running silently: {e}"))
            .ok()?;
        mixer::open_audio(FREQUENCY, DEFAULT_FORMAT, STEREO, CHUNK_SIZE)
            .inspect_err(|e| log_warn!("cannot open sound device, running silently: {e}"))
            .ok()?;
        mixer::allocate_channels(CHANNEL_COUNT);

        let effect = |name, path: &Path| {
            let mut chunk = load(name, path, |path| Chunk::from_file(path))?;
            chunk.set_volume(mixer_volume(config.effects_volume));
            Some(chunk)
        };
        Music::set_volume(mixer_volume(config.music_volume));
        let audio = Self {
            music: load("music", &config.music, |path| Music::from_file(path)),
            select: effect("selection sound", &config.select_sound),
            launch: effect("launch sound", &config.launch_sound),
            _context: context,
            _subsystem: subsystem,
        };
        // Listed only now, because decoders load when first needed.
        let decoders: Vec<_> = (0..mixer::get_music_decoders_number())
            .map(mixer::get_music_decoder)
            .collect();
        log_info!("audio ready, decoders loaded: {}", decoders.join(" "));
        Some(audio)
    }

    /// Starts looping the background music.
    pub fn start_music(&self) {
        if let Some(music) = &self.music {
            if let Err(e) = music.play(LOOP_FOREVER) {
                log_warn!("cannot play music: {e}");
            }
        }
    }

    pub fn play_select(&self) {
        if let Some(sound) = &self.select {
            if let Err(e) = SELECT_CHANNEL.play(sound, 0) {
                log_warn!("cannot play selection sound: {e}");
            }
        }
    }

    /// Fades the music out and plays the launch sound, returning when the
    /// sound has finished or `MAX_LAUNCH_SOUND` has passed.
    pub fn finish_for_launch(&self) {
        SELECT_CHANNEL.halt();
        let playing = self.launch.as_ref().is_some_and(|sound| {
            LAUNCH_CHANNEL
                .play(sound, 0)
                .inspect_err(|e| log_warn!("cannot play launch sound: {e}"))
                .is_ok()
        });
        if !playing {
            Music::halt();
            return;
        }
        if let Err(e) = Music::fade_out(MUSIC_FADE_MS) {
            log_warn!("cannot fade out music: {e}");
        }
        let deadline = Instant::now() + MAX_LAUNCH_SOUND;
        while LAUNCH_CHANNEL.is_playing() && Instant::now() < deadline {
            thread::sleep(LAUNCH_POLL);
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        Music::halt();
        Channel::all().halt();
        // Sounds must be freed while the mixer is still open, and the fields
        // would otherwise only drop after this function returns.
        self.music = None;
        self.select = None;
        self.launch = None;
        mixer::close_audio();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn config(music: PathBuf, select: PathBuf, launch: PathBuf) -> AudioConfig {
        AudioConfig {
            music,
            select_sound: select,
            launch_sound: launch,
            ..AudioConfig::default()
        }
    }

    #[test]
    fn volume_spans_the_mixer_range() {
        assert_eq!(mixer_volume(0), 0);
        assert_eq!(mixer_volume(50), MAX_VOLUME / 2);
        assert_eq!(mixer_volume(100), MAX_VOLUME);
    }

    #[test]
    fn volume_cannot_leave_the_mixer_range() {
        assert_eq!(mixer_volume(i64::MAX), MAX_VOLUME);
        assert_eq!(mixer_volume(-1), 0);
    }

    #[test]
    fn missing_sounds_need_no_sound_device() {
        let missing = PathBuf::from("/no/such/sound.opus");
        let settings = config(missing.clone(), missing.clone(), missing);
        assert!(!any_sound_exists(&settings));
        assert!(!any_sound_exists(&config("".into(), "".into(), "".into())));
    }

    #[test]
    fn one_existing_sound_is_enough() {
        let dir = tempfile::tempdir().unwrap();
        let sound = dir.path().join("select.opus");
        fs::write(&sound, "").unwrap();
        let missing = PathBuf::from("/no/such/sound.opus");
        assert!(any_sound_exists(&config(missing.clone(), sound, missing)));
    }

    #[test]
    fn a_directory_is_not_a_sound() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_path_buf();
        assert!(!any_sound_exists(&config(path.clone(), path.clone(), path)));
    }

    #[test]
    fn load_reports_missing_and_unreadable_files_as_none() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("absent.opus");
        let loaded: Option<()> = load("test", &missing, |_| -> Result<(), String> {
            panic!("a missing file must not reach the decoder")
        });
        assert!(loaded.is_none());

        let invalid = dir.path().join("invalid.opus");
        fs::write(&invalid, "not audio").unwrap();
        let loaded: Option<()> = load("test", &invalid, |_| Err("unsupported format"));
        assert!(loaded.is_none());
    }
}
