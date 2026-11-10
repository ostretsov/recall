//! Transcript segments (who said what, when) and how they are written out.
//!
//! Times are seconds from the start of the recording.  "Me" is the
//! microphone track, "Others" is what the computer played (the other people
//! in the call); they are recognized separately, so the speaker is always
//! known exactly.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use crate::lang::Language;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Speaker {
    Me,
    Others,
}

impl Speaker {
    pub const ALL: [Speaker; 2] = [Speaker::Me, Speaker::Others];

    pub fn label(self) -> &'static str {
        match self {
            Speaker::Me => "Me",
            Speaker::Others => "Others",
        }
    }

    /// File stem of this speaker's audio track ("me", "others").
    pub fn track_name(self) -> &'static str {
        match self {
            Speaker::Me => "me",
            Speaker::Others => "others",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub word: String,
    pub start: f64,
    pub end: f64,
    /// Recognizer confidence 0..1.
    pub conf: f32,
}

/// One utterance: what one speaker said between two pauses.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Segment {
    pub speaker: Speaker,
    pub start: f64,
    pub end: f64,
    pub text: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub words: Vec<Word>,
}

impl Segment {
    /// From recognized words; `offset` is added to their times (the
    /// recognizer counts from where it was last reset).  `None` if there is
    /// no text.
    pub fn from_words(
        speaker: Speaker,
        text: &str,
        words: Vec<Word>,
        offset: f64,
        fallback_time: f64,
    ) -> Option<Segment> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let words: Vec<Word> = words
            .into_iter()
            .map(|w| Word {
                start: w.start + offset,
                end: w.end + offset,
                ..w
            })
            .collect();
        let (start, end) = match (words.first(), words.last()) {
            (Some(f), Some(l)) => (f.start, l.end),
            _ => (fallback_time, fallback_time),
        };
        Some(Segment {
            speaker,
            start,
            end,
            text: text.to_string(),
            words,
        })
    }

    /// One line of `live.jsonl`.
    pub fn to_json_line(&self) -> String {
        serde_json::to_string(self).expect("segments always serialize")
    }
}

/// Reads a `.jsonl` file's segments, skipping lines that don't parse (a
/// crash can leave a half-written last line).
pub fn parse_jsonl(text: &str) -> Vec<Segment> {
    text.lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Sorts segments of both speakers by time.
pub fn sorted(mut segments: Vec<Segment>) -> Vec<Segment> {
    segments.sort_by(|a, b| a.start.total_cmp(&b.start));
    segments
}

/// "0:03:07" (hours always shown, so columns line up in long calls).
pub fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
}

/// Consecutive segments of one speaker closer than this are one paragraph.
pub const PARAGRAPH_GAP: f64 = 3.0;

/// A readable Markdown transcript.
pub fn markdown(title: &str, language: Language, segments: &[Segment]) -> String {
    let mut out = format!("# {title}\n\nLanguage: {}\n\n", language.name());
    let mut prev: Option<&Segment> = None;
    for seg in segments {
        let continues =
            prev.is_some_and(|p| p.speaker == seg.speaker && seg.start - p.end < PARAGRAPH_GAP);
        if continues {
            out.push(' ');
            out.push_str(&seg.text);
        } else {
            if prev.is_some() {
                out.push_str("\n\n");
            }
            let _ = write!(
                out,
                "**[{}] {}:** {}",
                clock(seg.start),
                seg.speaker.label(),
                seg.text
            );
        }
        prev = Some(seg);
    }
    if prev.is_some() {
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(speaker: Speaker, start: f64, end: f64, text: &str) -> Segment {
        Segment {
            speaker,
            start,
            end,
            text: text.into(),
            words: vec![],
        }
    }

    #[test]
    fn segment_from_words_applies_offset() {
        let words = vec![
            Word {
                word: "hola".into(),
                start: 0.5,
                end: 0.9,
                conf: 1.0,
            },
            Word {
                word: "amigo".into(),
                start: 1.0,
                end: 1.6,
                conf: 0.8,
            },
        ];
        let s = Segment::from_words(Speaker::Others, " hola amigo ", words, 10.0, 0.0).unwrap();
        assert_eq!((s.start, s.end), (10.5, 11.6));
        assert_eq!(s.text, "hola amigo");
        assert_eq!(s.words[1].start, 11.0);
        assert!(Segment::from_words(Speaker::Me, "  ", vec![], 0.0, 0.0).is_none());
        let s = Segment::from_words(Speaker::Me, "x", vec![], 0.0, 7.0).unwrap();
        assert_eq!((s.start, s.end), (7.0, 7.0));
    }

    #[test]
    fn jsonl_round_trip_skips_broken_lines() {
        let a = seg(Speaker::Me, 1.0, 2.0, "привет");
        let b = seg(Speaker::Others, 2.5, 3.0, "hi");
        let text = format!(
            "{}\n{}\n{{\"speaker\":\"me\",\"sta",
            a.to_json_line(),
            b.to_json_line()
        );
        assert_eq!(parse_jsonl(&text), vec![a, b]);
    }

    #[test]
    fn clock_format() {
        assert_eq!(clock(0.0), "0:00:00");
        assert_eq!(clock(187.9), "0:03:07");
        assert_eq!(clock(3725.0), "1:02:05");
        assert_eq!(clock(-3.0), "0:00:00");
    }

    #[test]
    fn markdown_groups_paragraphs() {
        let segs = sorted(vec![
            seg(Speaker::Others, 5.0, 6.0, "fine thanks"),
            seg(Speaker::Me, 0.0, 1.0, "hello"),
            seg(Speaker::Me, 2.0, 3.0, "how are you"),
            seg(Speaker::Others, 20.0, 21.0, "bye"),
        ]);
        let md = markdown("Call", Language::En, &segs);
        assert_eq!(
            md,
            "# Call\n\nLanguage: English\n\n\
             **[0:00:00] Me:** hello how are you\n\n\
             **[0:00:05] Others:** fine thanks\n\n\
             **[0:00:20] Others:** bye\n"
        );
        assert_eq!(
            markdown("Empty", Language::Ru, &[]),
            "# Empty\n\nLanguage: Русский\n\n"
        );
    }
}
