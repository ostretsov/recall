//! Control API: one JSON command per line on a Unix socket (`--control SOCK`),
//! one JSON reply per line.  Every command runs the same code as the
//! corresponding button, so tests and scripts drive the real app.

use serde::{Deserialize, Serialize};

use crate::lang::Language;
use crate::transcript::{Segment, Speaker};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    /// The current [`State`].
    State,
    Start,
    Stop,
    /// Like the global shortcut.
    Toggle,
    Language {
        language: Language,
    },
    /// Subtitle text size in points.
    FontSize {
        size: f64,
    },
    /// Folder new calls are saved in (absolute; created if missing).
    OutputDir {
        dir: String,
    },
    /// Start recall when you log in (installed package only).
    Autostart {
        enabled: bool,
    },
    /// Opens or closes the settings popover (like the gear button).
    Settings {
        open: bool,
    },
    /// (Re)make the Whisper transcript of a call folder (default: the
    /// current/last call).
    Transcribe {
        #[serde(default)]
        dir: Option<String>,
    },
    /// Look for a newer release now (also in development builds); the
    /// banner offers it (see `State::update`).  Installing needs a click.
    CheckUpdate,
    /// Like the window's close button: hides the window while the top bar
    /// icon is there (the app keeps running), otherwise quits.
    Close,
    /// Shows the window again (like "Open recall" in the icon's menu).
    Show,
    /// Like "Quit" in the icon's menu: ends the app (a recording is saved
    /// first).
    Quit,
}

impl Command {
    pub fn parse(line: &str) -> Result<Command, String> {
        serde_json::from_str(line).map_err(|e| format!("bad command: {e}"))
    }
}

/// What the app shows, for `{"cmd":"state"}`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    pub recording: bool,
    pub language: Language,
    /// Seconds since recording started (0 when not recording).
    pub elapsed: f64,
    /// Folder of the current (or last) call.
    pub call_dir: Option<String>,
    /// Finished utterances of the current (or last) call.
    pub segments: Vec<Segment>,
    /// Text being recognized right now, per speaker.
    pub partial: Vec<(Speaker, String)>,
    /// Last level reading in dBFS, per speaker.
    pub levels: Vec<(Speaker, f64)>,
    /// The status line (errors, warnings, hints).
    pub status: String,
    /// Subtitle text size in points.
    pub font_size: f64,
    /// Call folder being transcribed now.
    pub transcribing: Option<String>,
    /// Its progress 0..1.
    pub transcript_progress: f64,
    /// Calls waiting for their transcript.
    pub transcript_queue: usize,
    /// The last finished `transcript.md`.
    pub last_transcript: Option<String>,
    /// Text of the "new version" banner ("" while hidden).
    #[serde(default)]
    pub update: String,
    /// The window is shown (false after it was closed to the top bar).
    #[serde(default)]
    pub window_visible: bool,
    /// Folder new calls are saved in.
    #[serde(default)]
    pub output_dir: String,
    /// The "start at login" setting.
    #[serde(default)]
    pub autostart: bool,
    /// The settings popover is open.
    #[serde(default)]
    pub settings_open: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<State>,
}

impl Reply {
    pub fn ok() -> Reply {
        Reply {
            ok: true,
            error: None,
            state: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Reply {
        Reply {
            ok: false,
            error: Some(msg.into()),
            state: None,
        }
    }

    pub fn state(state: State) -> Reply {
        Reply {
            state: Some(state),
            ..Reply::ok()
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("replies always serialize")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_commands() {
        assert_eq!(Command::parse(r#"{"cmd":"state"}"#), Ok(Command::State));
        assert_eq!(Command::parse(r#"{"cmd":"toggle"}"#), Ok(Command::Toggle));
        assert_eq!(
            Command::parse(r#"{"cmd":"language","language":"ru"}"#),
            Ok(Command::Language {
                language: Language::Ru
            })
        );
        assert!(Command::parse(r#"{"cmd":"language","language":"xx"}"#).is_err());
        assert!(Command::parse("nonsense").is_err());
        assert_eq!(
            Command::parse(r#"{"cmd":"font_size","size":16}"#),
            Ok(Command::FontSize { size: 16.0 })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"transcribe"}"#),
            Ok(Command::Transcribe { dir: None })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"check_update"}"#),
            Ok(Command::CheckUpdate)
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"output_dir","dir":"/tmp/calls"}"#),
            Ok(Command::OutputDir {
                dir: "/tmp/calls".into()
            })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"settings","open":true}"#),
            Ok(Command::Settings { open: true })
        );
        assert_eq!(
            Command::parse(r#"{"cmd":"autostart","enabled":true}"#),
            Ok(Command::Autostart { enabled: true })
        );
        assert_eq!(Command::parse(r#"{"cmd":"close"}"#), Ok(Command::Close));
        assert_eq!(Command::parse(r#"{"cmd":"show"}"#), Ok(Command::Show));
        assert_eq!(Command::parse(r#"{"cmd":"quit"}"#), Ok(Command::Quit));
    }

    #[test]
    fn replies() {
        assert_eq!(Reply::ok().to_line(), r#"{"ok":true}"#);
        assert_eq!(Reply::error("no").to_line(), r#"{"ok":false,"error":"no"}"#);
        let r = Reply::state(State {
            recording: true,
            ..State::default()
        });
        let back: Reply = serde_json::from_str(&r.to_line()).unwrap();
        assert!(back.state.unwrap().recording);
    }
}
