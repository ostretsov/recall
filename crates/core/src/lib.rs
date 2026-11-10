//! recall's logic that can be tested without a display, sound devices or
//! speech models.  The GTK/GStreamer/Vosk side lives in `crates/app`.

pub mod call;
pub mod config;
pub mod control;
pub mod deps;
pub mod hotkey;
pub mod lang;
pub mod levels;
pub mod mic_channel;
pub mod pipelines;
pub mod speech;
pub mod transcript;
pub mod whisper;
