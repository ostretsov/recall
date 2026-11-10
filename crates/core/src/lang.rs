//! Call languages and the speech models that go with them.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    #[default]
    En,
    Ru,
    Es,
}

impl Language {
    pub const ALL: [Language; 3] = [Language::En, Language::Ru, Language::Es];

    /// ISO 639-1 code, as used in settings, the control API and file names.
    pub fn code(self) -> &'static str {
        match self {
            Language::En => "en",
            Language::Ru => "ru",
            Language::Es => "es",
        }
    }

    /// Name in the language itself (shown in the language picker).
    pub fn name(self) -> &'static str {
        match self {
            Language::En => "English",
            Language::Ru => "Русский",
            Language::Es => "Español",
        }
    }

    pub fn from_code(code: &str) -> Option<Language> {
        Language::ALL
            .into_iter()
            .find(|l| l.code().eq_ignore_ascii_case(code.trim()))
    }

    /// A short punctuated sentence in the language, given to Whisper as
    /// the "previous text": without it Whisper sometimes writes whole
    /// passages in lowercase without punctuation.
    pub fn style_prompt(self) -> &'static str {
        match self {
            Language::En => "Hello, everyone. Let's begin the meeting.",
            Language::Ru => "Здравствуйте, коллеги. Давайте начнём встречу.",
            Language::Es => "Hola a todos. Empecemos la reunión.",
        }
    }

    /// The small Vosk model used for live subtitles.
    pub fn live_model(self) -> &'static ModelInfo {
        match self {
            Language::En => &VOSK_EN,
            Language::Ru => &VOSK_RU,
            Language::Es => &VOSK_ES,
        }
    }
}

/// How a model is packaged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelKind {
    /// A zip with one top-level directory `name` (Vosk).
    Zip,
    /// A single file, saved as `name` (Whisper, VAD).
    File,
}

/// A downloadable model.
#[derive(Debug, PartialEq, Eq)]
pub struct ModelInfo {
    /// Directory (zip) or file name in the models folder.
    pub name: &'static str,
    pub kind: ModelKind,
    pub url: &'static str,
    pub sha256: &'static str,
    /// Download size in bytes (for the progress bar and the disk check).
    pub size: u64,
}

impl ModelInfo {
    /// Size in MB for messages, e.g. "46 MB".
    pub fn size_label(&self) -> String {
        format!("{} MB", self.size.div_ceil(1_000_000))
    }
}

pub const VOSK_EN: ModelInfo = ModelInfo {
    kind: ModelKind::Zip,
    name: "vosk-model-small-en-us-0.15",
    url: "https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip",
    sha256: "30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498",
    size: 41_205_931,
};

pub const VOSK_RU: ModelInfo = ModelInfo {
    kind: ModelKind::Zip,
    name: "vosk-model-small-ru-0.22",
    url: "https://alphacephei.com/vosk/models/vosk-model-small-ru-0.22.zip",
    sha256: "961d5ff98a17f4aa6de69864d0aa71fa5bac682301d2b5d17a3f24c5c99a46d4",
    size: 46_236_750,
};

pub const VOSK_ES: ModelInfo = ModelInfo {
    kind: ModelKind::Zip,
    name: "vosk-model-small-es-0.42",
    url: "https://alphacephei.com/vosk/models/vosk-model-small-es-0.42.zip",
    sha256: "09b239888f633ef2f0b4e09736e3d9936acfd810bc65d53fad45261762c6511f",
    size: 39_817_833,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_round_trip() {
        for l in Language::ALL {
            assert_eq!(Language::from_code(l.code()), Some(l));
        }
        assert_eq!(Language::from_code(" RU "), Some(Language::Ru));
        assert_eq!(Language::from_code("de"), None);
    }

    #[test]
    fn models_are_distinct_and_consistent() {
        let models: Vec<_> = Language::ALL.iter().map(|l| l.live_model()).collect();
        for m in &models {
            assert!(m.url.ends_with(&format!("/{}.zip", m.name)));
            assert_eq!(m.sha256.len(), 64);
            assert!(m.size > 1_000_000);
        }
        assert_ne!(models[0], models[1]);
        assert_ne!(models[1], models[2]);
        assert_eq!(VOSK_RU.size_label(), "47 MB");
    }

    #[test]
    fn serde_uses_codes() {
        assert_eq!(serde_json::to_string(&Language::Es).unwrap(), "\"es\"");
        assert_eq!(
            serde_json::from_str::<Language>("\"ru\"").unwrap(),
            Language::Ru
        );
    }
}
