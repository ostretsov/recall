//! A recorded call on disk: one folder per call.
//!
//! ```text
//! ~/Recordings/Calls/2026-11-02 14-30 (en)/
//!   call.json        what was recorded (this module's `CallInfo`)
//!   me.opus          microphone track
//!   others.opus      what the computer played (the other people)
//!   live.jsonl       live subtitles, one segment per line, written as they come
//!   live.md          the live subtitles as a readable transcript (at stop)
//!   transcript.md    accurate transcript made by Whisper after the call
//!   transcript.json  the same with timings, machine-readable
//!   errors.log       what went wrong while recording or transcribing (only then)
//! ```

use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::lang::Language;
use crate::transcript::Speaker;

pub const INFO_FILE: &str = "call.json";
pub const LIVE_JSONL: &str = "live.jsonl";
pub const LIVE_MD: &str = "live.md";
pub const TRANSCRIPT_MD: &str = "transcript.md";
pub const TRANSCRIPT_JSON: &str = "transcript.json";
/// Written when transcribing failed (holds the error), so the call is not
/// retried at every start.
pub const TRANSCRIPT_FAILED: &str = "transcript.failed";

/// Problems with this call (recording or transcribing), one timestamped
/// line each.  Only exists when something went wrong.
pub const ERRORS_LOG: &str = "errors.log";

/// Appends a problem to the call's `errors.log`.
pub fn log_error(dir: &Path, time: DateTime<Local>, msg: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::options()
        .create(true)
        .append(true)
        .open(dir.join(ERRORS_LOG))?;
    let msg = msg.trim_end().replace('\n', "\n    ");
    writeln!(f, "{} {msg}", time.format("%Y-%m-%d %H:%M:%S"))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CallInfo {
    pub started: DateTime<Local>,
    /// Filled in when the recording stops (missing after a crash).
    #[serde(default)]
    pub duration: Option<f64>,
    pub language: Language,
    /// Recorded tracks.
    pub tracks: Vec<Speaker>,
    /// Which program wrote it ("recall 0.1.0").
    pub recorder: String,
}

impl CallInfo {
    /// "Call 2026-11-02 14:30" (transcript titles).
    pub fn title(&self) -> String {
        format!("Call {}", self.started.format("%Y-%m-%d %H:%M"))
    }
}

/// A new, unused folder for a call starting at `started`.
pub fn new_dir(output_dir: &Path, started: DateTime<Local>, language: Language) -> PathBuf {
    let base = format!("{} ({})", started.format("%Y-%m-%d %H-%M"), language.code());
    let mut dir = output_dir.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = output_dir.join(format!("{base} {n}"));
        n += 1;
    }
    dir
}

/// "21.4 GB" for free-space messages.
pub fn human_size(bytes: u64) -> String {
    const GB: f64 = 1e9;
    const MB: f64 = 1e6;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else {
        format!("{:.0} MB", b / MB)
    }
}

/// Recording is refused below this much free space, and a warning shown
/// while recording.
pub const MIN_FREE_BYTES: u64 = 500_000_000;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn folder_names_never_collide() {
        let tmp = tempfile::tempdir().unwrap();
        let t = Local.with_ymd_and_hms(2026, 11, 2, 14, 30, 5).unwrap();
        let a = new_dir(tmp.path(), t, Language::Ru);
        assert_eq!(a.file_name().unwrap(), "2026-11-02 14-30 (ru)");
        std::fs::create_dir(&a).unwrap();
        let b = new_dir(tmp.path(), t, Language::Ru);
        assert_eq!(b.file_name().unwrap(), "2026-11-02 14-30 (ru) 2");
    }

    #[test]
    fn info_round_trip() {
        let info = CallInfo {
            started: Local.with_ymd_and_hms(2026, 11, 2, 14, 30, 0).unwrap(),
            duration: None,
            language: Language::Es,
            tracks: vec![Speaker::Me, Speaker::Others],
            recorder: "recall 0.1.0".into(),
        };
        let back: CallInfo = serde_json::from_str(&serde_json::to_string(&info).unwrap()).unwrap();
        assert_eq!(back, info);
        assert_eq!(info.title(), "Call 2026-11-02 14:30");
    }

    #[test]
    fn errors_are_appended() {
        let tmp = tempfile::tempdir().unwrap();
        let t = Local.with_ymd_and_hms(2026, 11, 2, 14, 30, 5).unwrap();
        log_error(tmp.path(), t, "pipeline error: no device").unwrap();
        log_error(tmp.path(), t, "two\nlines\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(ERRORS_LOG)).unwrap(),
            "2026-11-02 14:30:05 pipeline error: no device\n2026-11-02 14:30:05 two\n    lines\n"
        );
        // No folder: an error, not a panic.
        assert!(log_error(&tmp.path().join("missing"), t, "x").is_err());
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(21_400_000_000), "21.4 GB");
        assert_eq!(human_size(450_000_000), "450 MB");
    }
}
