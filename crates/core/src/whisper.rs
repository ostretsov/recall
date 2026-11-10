//! The accurate after-call transcript: Whisper models and the transcript
//! files (`transcript.md`, `transcript.json`).

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::lang::{Language, ModelInfo, ModelKind};
use crate::transcript::Segment;

/// Whisper model sizes offered in the settings (`transcript_model`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WhisperModel {
    /// Turbo with a dedicated graphics card, small without.
    #[default]
    Auto,
    /// Fast, weakest (148 MB).
    Base,
    /// Good balance (190 MB, 5-bit quantized).
    Small,
    /// large-v3-turbo: best, needs a GPU to be quick (574 MB, 5-bit).
    Turbo,
}

impl WhisperModel {
    /// The concrete models (not `Auto`).
    pub const ALL: [WhisperModel; 3] =
        [WhisperModel::Base, WhisperModel::Small, WhisperModel::Turbo];

    /// The concrete model: `Auto` depends on whether a usable GPU exists.
    /// (Measured on a GTX 1050: turbo transcribes a 1-hour call in ~25 min;
    /// on a 4-core i5 CPU small takes about as long, turbo much longer.)
    pub fn resolve(self, gpu: bool) -> WhisperModel {
        match self {
            WhisperModel::Auto if gpu => WhisperModel::Turbo,
            WhisperModel::Auto => WhisperModel::Small,
            m => m,
        }
    }

    /// The model file (`Auto` counts as small: call [`Self::resolve`] first).
    pub fn info(self) -> &'static ModelInfo {
        match self {
            WhisperModel::Base => &WHISPER_BASE,
            WhisperModel::Auto | WhisperModel::Small => &WHISPER_SMALL,
            WhisperModel::Turbo => &WHISPER_TURBO,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            WhisperModel::Auto => "auto",
            WhisperModel::Base => "base",
            WhisperModel::Small => "small",
            WhisperModel::Turbo => "turbo",
        }
    }
}

#[cfg(test)]
const HF: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/";

pub const WHISPER_BASE: ModelInfo = ModelInfo {
    name: "ggml-base.bin",
    kind: ModelKind::File,
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
    sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe",
    size: 147_951_465,
};

pub const WHISPER_SMALL: ModelInfo = ModelInfo {
    name: "ggml-small-q5_1.bin",
    kind: ModelKind::File,
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small-q5_1.bin",
    sha256: "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
    size: 190_085_487,
};

pub const WHISPER_TURBO: ModelInfo = ModelInfo {
    name: "ggml-large-v3-turbo-q5_0.bin",
    kind: ModelKind::File,
    url: "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3-turbo-q5_0.bin",
    sha256: "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
    size: 574_041_195,
};

/// Silero voice activity detection: Whisper only sees the parts with speech
/// (faster, and no invented text in long silences).
pub const VAD_MODEL: ModelInfo = ModelInfo {
    name: "ggml-silero-v6.2.0.bin",
    kind: ModelKind::File,
    url: "https://huggingface.co/ggml-org/whisper-vad/resolve/main/ggml-silero-v6.2.0.bin",
    sha256: "2aa269b785eeb53a82983a20501ddf7c1d9c48e33ab63a41391ac6c9f7fb6987",
    size: 885_098,
};

/// Whether a Vulkan device (by name) is worth using for Whisper: dedicated
/// NVIDIA/AMD/Intel Arc cards are; integrated Intel graphics (slow, and
/// their driver crashed in tests) and software renderers are not.
pub fn usable_gpu(name: &str) -> bool {
    let n = name.to_lowercase();
    ![
        "llvmpipe",
        "lavapipe",
        "swiftshader",
        "hd graphics",
        "uhd graphics",
        "iris",
    ]
    .iter()
    .any(|bad| n.contains(bad))
}

/// The machine-readable transcript (`transcript.json`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Transcript {
    pub title: String,
    pub language: Language,
    pub started: DateTime<Local>,
    pub duration: Option<f64>,
    /// Model file that made it, e.g. "ggml-small-q5_1.bin".
    pub model: String,
    /// Both speakers, sorted by time.
    pub segments: Vec<Segment>,
}

/// A line from `recall --transcribe DIR --machine` (the app runs
/// transcription in a child process and reads these).
#[derive(Clone, Debug, PartialEq)]
pub enum ChildLine {
    Progress(f64),
    Done(String),
    Error(String),
}

impl ChildLine {
    pub fn parse(line: &str) -> Option<ChildLine> {
        let (kind, rest) = line.trim_end().split_once(' ')?;
        match kind {
            "progress" => rest
                .parse()
                .ok()
                .map(|p: f64| ChildLine::Progress(p.clamp(0.0, 1.0))),
            "done" => Some(ChildLine::Done(rest.to_string())),
            "error" => Some(ChildLine::Error(rest.to_string())),
            _ => None,
        }
    }
}

/// Text Whisper is known to invent for silence or noise (learned from
/// subtitled videos).  A segment consisting only of one of these is dropped.
const HALLUCINATIONS: &[&str] = &[
    "thank you for watching",
    "thanks for watching",
    "please subscribe",
    "subtitles by the amara.org community",
    "продолжение следует",
    "субтитры создавал dimatorzok",
    "субтитры сделал dimatorzok",
    "редактор субтитров а.семкин корректор а.егорова",
    "спасибо за просмотр",
    "подписывайтесь на канал",
    "subtítulos realizados por la comunidad de amara.org",
    "gracias por ver el video",
    "¡gracias por ver!",
    "gracias por ver",
];

/// Cleans one Whisper segment's text; `None` if nothing real is left.
pub fn clean_text(text: &str) -> Option<String> {
    let t = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let norm: String = t
        .to_lowercase()
        .trim_matches(|c: char| c.is_ascii_punctuation() || c == '…' || c.is_whitespace())
        .to_string();
    if norm.is_empty()
        || HALLUCINATIONS
            .iter()
            .any(|h| norm == h.trim_matches(|c: char| c.is_ascii_punctuation()))
        // Bracketed sound descriptions: "[Music]", "(аплодисменты)", "*laughs*".
        || (t.starts_with(['[', '(', '*']) && t.ends_with([']', ')', '*']))
    {
        return None;
    }
    Some(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models() {
        for m in WhisperModel::ALL {
            let i = m.info();
            assert!(i.url.starts_with(HF) && i.url.ends_with(i.name));
            assert_eq!(i.kind, ModelKind::File);
            assert_eq!(i.sha256.len(), 64);
        }
        assert_eq!(WhisperModel::default(), WhisperModel::Auto);
        assert_eq!(WhisperModel::Auto.resolve(true), WhisperModel::Turbo);
        assert_eq!(WhisperModel::Auto.resolve(false), WhisperModel::Small);
        assert_eq!(WhisperModel::Base.resolve(true), WhisperModel::Base);
        assert_eq!(
            serde_json::from_str::<WhisperModel>("\"turbo\"").unwrap(),
            WhisperModel::Turbo
        );
        assert!(VAD_MODEL.url.ends_with(VAD_MODEL.name));
    }

    #[test]
    fn child_lines() {
        assert_eq!(
            ChildLine::parse("progress 0.425\n"),
            Some(ChildLine::Progress(0.425))
        );
        assert_eq!(
            ChildLine::parse("progress 7"),
            Some(ChildLine::Progress(1.0))
        );
        assert_eq!(
            ChildLine::parse("done /a b/transcript.md"),
            Some(ChildLine::Done("/a b/transcript.md".into()))
        );
        assert_eq!(
            ChildLine::parse("error no model"),
            Some(ChildLine::Error("no model".into()))
        );
        assert_eq!(ChildLine::parse("ggml_vulkan: Found 2"), None);
        assert_eq!(ChildLine::parse("progress x"), None);
    }

    #[test]
    fn gpu_choice() {
        assert!(usable_gpu("NVIDIA GeForce GTX 1050"));
        assert!(usable_gpu("AMD Radeon RX 6600 (RADV NAVI23)"));
        assert!(usable_gpu("Intel(R) Arc(tm) A750 Graphics (DG2)"));
        assert!(!usable_gpu("Intel(R) HD Graphics 630 (KBL GT2)"));
        assert!(!usable_gpu("Intel(R) UHD Graphics 620 (KBL GT2)"));
        assert!(!usable_gpu("Intel(R) Iris(R) Xe Graphics (TGL GT2)"));
        assert!(!usable_gpu("llvmpipe (LLVM 15.0.7, 256 bits)"));
    }

    #[test]
    fn cleaning() {
        assert_eq!(
            clean_text("  Hello   there. ").as_deref(),
            Some("Hello there.")
        );
        assert_eq!(clean_text(" Thank you for watching!"), None);
        assert_eq!(clean_text("Продолжение следует..."), None);
        assert_eq!(clean_text("Субтитры создавал DimaTorzok"), None);
        assert_eq!(clean_text("[Music]"), None);
        assert_eq!(clean_text("(аплодисменты)"), None);
        assert_eq!(clean_text(" ... "), None);
        // Real speech that merely contains such words stays.
        assert!(clean_text("Thank you for watching the demo, any questions?").is_some());
        assert_eq!(clean_text("¡Gracias por ver!"), None);
    }
}
