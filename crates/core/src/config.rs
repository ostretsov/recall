//! Settings: `~/.config/recall/config.toml` (or `--config-dir DIR`).
//!
//! Every key is optional; unknown keys are ignored so older versions can read
//! newer files.  The app writes the file back when settings change in the UI
//! (e.g. the last chosen language).

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::lang::Language;
use crate::whisper::WhisperModel;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Language of the call (the last one chosen).
    pub language: Language,
    /// Where call folders go.  Empty: `~/Recordings/Calls`.
    pub output_dir: String,
    /// "default" (the system's default input), a PulseAudio source name,
    /// "file:/path/to/audio" (played in real time; for tests), "test" (a
    /// tone) or "none".
    pub microphone: String,
    /// "auto", "both", "left" or "right": which input channel carries the
    /// voice (a Focusrite Scarlett puts input 1 on the left only).
    pub microphone_channel: String,
    /// "default" (what the default output device plays, i.e. the other
    /// people in the call), a PulseAudio source name (e.g. a `.monitor`),
    /// "file:...", "test" or "none".
    pub others: String,
    /// Global shortcut that starts/stops recording, e.g. "Ctrl+Alt+R".
    /// Empty: none.
    pub hotkey: String,
    /// Where speech models are stored.  Empty: `~/.local/share/recall/models`.
    pub models_dir: String,
    /// Subtitle text size in points (Ctrl+plus / Ctrl+minus in the window).
    pub font_size: f64,
    /// Make an accurate transcript with Whisper after each call.
    pub transcribe: bool,
    /// Whisper model: "auto" (turbo with a graphics card, else small),
    /// "base", "small" or "turbo".
    pub transcript_model: WhisperModel,
    /// Use the graphics card (Vulkan) for Whisper when there is one.
    pub gpu: bool,
}

/// Subtitle font sizes in points: smallest, largest, default.
pub const FONT_MIN: f64 = 8.0;
pub const FONT_MAX: f64 = 40.0;
pub const FONT_DEFAULT: f64 = 13.0;

/// A font size within [`FONT_MIN`]..=[`FONT_MAX`], in whole points
/// (anything unusable gives the default).
pub fn font_size(pt: f64) -> f64 {
    if pt.is_finite() {
        pt.round().clamp(FONT_MIN, FONT_MAX)
    } else {
        FONT_DEFAULT
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            language: Language::En,
            output_dir: String::new(),
            microphone: "default".into(),
            microphone_channel: "auto".into(),
            others: "default".into(),
            hotkey: "Ctrl+Alt+R".into(),
            models_dir: String::new(),
            font_size: FONT_DEFAULT,
            transcribe: true,
            transcript_model: WhisperModel::default(),
            gpu: true,
        }
    }
}

impl Config {
    /// Reads `path`; a missing file gives the defaults.
    pub fn load(path: &Path) -> anyhow::Result<Config> {
        match std::fs::read_to_string(path) {
            Ok(text) => Config::parse(&text).with_context(|| format!("reading {}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn parse(text: &str) -> anyhow::Result<Config> {
        Ok(toml::from_str(text)?)
    }

    /// Writes atomically (temporary file + rename).
    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn output_dir(&self, home: &Path) -> PathBuf {
        expand(&self.output_dir, home).unwrap_or_else(|| home.join("Recordings").join("Calls"))
    }

    pub fn models_dir(&self, data_home: &Path) -> PathBuf {
        expand(&self.models_dir, data_home)
            .unwrap_or_else(|| data_home.join("recall").join("models"))
    }
}

/// Standard locations, from the environment (XDG base directories).
#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config_dir: PathBuf,
    /// `$XDG_DATA_HOME` (`~/.local/share`).
    pub data_home: PathBuf,
}

impl Paths {
    pub fn new(
        home: PathBuf,
        config_dir_override: Option<PathBuf>,
        xdg_config_home: Option<PathBuf>,
        xdg_data_home: Option<PathBuf>,
    ) -> Paths {
        let config_dir = config_dir_override.unwrap_or_else(|| {
            xdg_config_home
                .filter(|p| p.is_absolute())
                .unwrap_or_else(|| home.join(".config"))
                .join("recall")
        });
        let data_home = xdg_data_home
            .filter(|p| p.is_absolute())
            .unwrap_or_else(|| home.join(".local").join("share"));
        Paths {
            home,
            config_dir,
            data_home,
        }
    }

    pub fn from_env(config_dir_override: Option<PathBuf>) -> Paths {
        let var = |k| {
            std::env::var_os(k)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        Paths::new(
            var("HOME").unwrap_or_else(|| PathBuf::from("/tmp")),
            config_dir_override,
            var("XDG_CONFIG_HOME"),
            var("XDG_DATA_HOME"),
        )
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

/// `~/x` and absolute paths; relative ones are taken relative to `base`.
/// Empty means "not set".
fn expand(value: &str, base: &Path) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix("~/") {
        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        return Some(home.join(rest));
    }
    let p = PathBuf::from(value);
    Some(if p.is_absolute() { p } else { base.join(p) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_sizes() {
        assert_eq!(Config::default().font_size, 13.0);
        assert_eq!(Config::parse("font_size = 18.0").unwrap().font_size, 18.0);
        assert_eq!(font_size(15.4), 15.0);
        assert_eq!(font_size(2.0), FONT_MIN);
        assert_eq!(font_size(99.0), FONT_MAX);
        assert_eq!(font_size(f64::NAN), FONT_DEFAULT);
    }

    #[test]
    fn empty_file_gives_defaults() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn partial_file_and_unknown_keys() {
        let c = Config::parse("language = \"ru\"\nfuture_key = 3\n").unwrap();
        assert_eq!(c.language, Language::Ru);
        assert_eq!(c.microphone, "default");
        assert_eq!(c.hotkey, "Ctrl+Alt+R");
    }

    #[test]
    fn bad_language_is_an_error() {
        assert!(Config::parse("language = \"xx\"").is_err());
    }

    #[test]
    fn save_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/config.toml");
        assert_eq!(Config::load(&path).unwrap(), Config::default());
        let c = Config {
            language: Language::Es,
            microphone: "file:/tmp/a.wav".into(),
            ..Config::default()
        };
        c.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), c);
    }

    #[test]
    fn directories() {
        let home = Path::new("/home/u");
        let data = Path::new("/home/u/.local/share");
        let c = Config::default();
        assert_eq!(c.output_dir(home), Path::new("/home/u/Recordings/Calls"));
        assert_eq!(
            c.models_dir(data),
            Path::new("/home/u/.local/share/recall/models")
        );
        let c = Config {
            output_dir: "/data/calls".into(),
            models_dir: "m".into(),
            ..Config::default()
        };
        assert_eq!(c.output_dir(home), Path::new("/data/calls"));
        assert_eq!(c.models_dir(data), Path::new("/home/u/.local/share/m"));
    }

    #[test]
    fn xdg_paths() {
        let p = Paths::new("/h".into(), None, None, None);
        assert_eq!(p.config_file(), Path::new("/h/.config/recall/config.toml"));
        assert_eq!(p.data_home, Path::new("/h/.local/share"));
        let p = Paths::new(
            "/h".into(),
            None,
            Some("/x/conf".into()),
            Some("relative".into()),
        );
        assert_eq!(p.config_dir, Path::new("/x/conf/recall"));
        assert_eq!(
            p.data_home,
            Path::new("/h/.local/share"),
            "relative XDG ignored"
        );
        let p = Paths::new("/h".into(), Some("/t/c".into()), None, None);
        assert_eq!(p.config_file(), Path::new("/t/c/config.toml"));
    }
}
