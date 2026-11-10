//! GStreamer pipeline descriptions (`gst-launch` syntax) for recording.
//!
//! Each speaker track is one branch: source → mono 48 kHz → tee →
//! (a) Opus in Ogg on disk (crash-safe: everything but the last moments
//! stays playable) and (b) 16 kHz S16 mono to an appsink for live speech
//! recognition.

use std::path::{Path, PathBuf};

use crate::transcript::Speaker;

/// Where a track's sound comes from (the `microphone` / `others` settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    /// The system default: default input for the mic, the default output's
    /// monitor for the others (resolved by the app before building).
    Default,
    /// A PulseAudio/PipeWire source by name.
    Pulse(String),
    /// An audio file played in real time (tests and demos).
    File(PathBuf),
    /// A quiet sine tone.
    Test,
    /// Not recorded.
    None,
}

impl Source {
    pub fn parse(s: &str) -> Source {
        let s = s.trim();
        match s {
            "" | "default" => Source::Default,
            "test" => Source::Test,
            "none" | "off" => Source::None,
            _ => match s.strip_prefix("file:") {
                Some(path) => Source::File(PathBuf::from(path)),
                None => Source::Pulse(s.to_string()),
            },
        }
    }
}

/// Opus bit rate per track: plenty for speech recognition (~14 MB/hour).
pub const OPUS_BITRATE: u32 = 32_000;
/// Recording sample rate.
pub const RATE: u32 = 48_000;
/// Speech recognition sample rate.
pub const ASR_RATE: u32 = 16_000;

/// Name of the appsink delivering 16 kHz mono S16 for `speaker`.
pub fn asr_sink_name(speaker: Speaker) -> String {
    format!("{}_asr", speaker.track_name())
}

/// Name of the element after which the mic's samples are processed
/// (channel pick); its src pad gets the probe.  Stereo S16 48 kHz there.
pub fn mic_processing_name() -> &'static str {
    "me_proc"
}

/// The audio file of `speaker`'s track in a call folder.
pub fn track_path(dir: &Path, speaker: Speaker) -> PathBuf {
    dir.join(format!("{}.opus", speaker.track_name()))
}

/// A `gst-launch` property value in double quotes.
pub fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn source_element(source: &Source, speaker: Speaker) -> Option<String> {
    let t = speaker.track_name();
    // Pin to the system clock: a pulsesrc clock stops when no sound arrives.
    let pulse = format!("pulsesrc client-name=recall-{t} provide-clock=false do-timestamp=true");
    Some(match source {
        Source::None => return None,
        Source::Default => pulse,
        Source::Pulse(dev) => format!("{pulse} device={}", quote(dev)),
        Source::Test => "audiotestsrc is-live=true wave=sine freq=440 volume=0.05".to_string(),
        // identity sync=true: play the file in real time, like a call.
        Source::File(p) => format!(
            "filesrc location={} ! decodebin ! audioconvert ! audioresample ! identity sync=true",
            quote(&p.to_string_lossy())
        ),
    })
}

/// One track's branch, or `None` when the source is `None`.
pub fn track(source: &Source, speaker: Speaker, file: &Path) -> Option<String> {
    let src = source_element(source, speaker)?;
    let t = speaker.track_name();
    // The mic is kept stereo up to the channel-pick probe.
    let pre = match speaker {
        Speaker::Me => format!(
            "audio/x-raw,format=S16LE,rate={RATE},channels=2 ! identity name={} ! audioconvert ! ",
            mic_processing_name()
        ),
        Speaker::Others => String::new(),
    };
    Some(format!(
        "{src} \
         ! queue name={t}_srcq leaky=downstream max-size-time=2000000000 max-size-buffers=0 max-size-bytes=0 \
         ! audioconvert ! audioresample ! {pre}audio/x-raw,format=S16LE,rate={RATE},channels=1 \
         ! tee name={t}_tee \
         {t}_tee. ! queue max-size-time=2000000000 max-size-buffers=0 max-size-bytes=0 \
         ! opusenc bitrate={OPUS_BITRATE} audio-type=voice ! oggmux \
         ! filesink location={} async=false \
         {t}_tee. ! queue max-size-time=0 max-size-buffers=0 max-size-bytes=0 \
         ! audioresample ! audio/x-raw,format=S16LE,rate={ASR_RATE},channels=1 \
         ! appsink name={} sync=false async=false emit-signals=false max-buffers=0 drop=false",
        quote(&file.to_string_lossy()),
        asr_sink_name(speaker),
    ))
}

/// The whole recording pipeline: one branch per recorded track.
pub fn recording(mic: &Source, others: &Source, dir: &Path) -> Option<String> {
    let parts: Vec<String> = [(mic, Speaker::Me), (others, Speaker::Others)]
        .into_iter()
        .filter_map(|(src, sp)| track(src, sp, &track_path(dir, sp)))
        .collect();
    (!parts.is_empty()).then(|| parts.join("  "))
}

/// Mixes interleaved stereo S16 to mono with per-channel weights.
pub fn mix_to_mono(stereo: &[i16], gains: [f32; 2]) -> Vec<i16> {
    stereo
        .as_chunks::<2>()
        .0
        .iter()
        .map(|[l, r]| {
            (*l as f32 * gains[0] + *r as f32 * gains[1])
                .round()
                .clamp(-32768.0, 32767.0) as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_sources() {
        assert_eq!(Source::parse("default"), Source::Default);
        assert_eq!(Source::parse(""), Source::Default);
        assert_eq!(Source::parse("none"), Source::None);
        assert_eq!(Source::parse("test"), Source::Test);
        assert_eq!(
            Source::parse("file:/tmp/a b.wav"),
            Source::File("/tmp/a b.wav".into())
        );
        assert_eq!(
            Source::parse("alsa_output.x.monitor"),
            Source::Pulse("alsa_output.x.monitor".into())
        );
    }

    #[test]
    fn quoting() {
        assert_eq!(quote(r#"a "b"\c"#), r#""a \"b\"\\c""#);
    }

    #[test]
    fn mic_branch_has_processing_and_both_outputs() {
        let d = track(&Source::Default, Speaker::Me, Path::new("/c/me.opus")).unwrap();
        assert!(d.starts_with("pulsesrc client-name=recall-me provide-clock=false"));
        assert!(!d.contains("device="));
        assert!(d.contains("channels=2 ! identity name=me_proc"));
        assert!(d.contains("filesink location=\"/c/me.opus\""));
        assert!(d.contains("appsink name=me_asr"));
        assert!(d.contains("rate=16000,channels=1"));
    }

    #[test]
    fn others_branch() {
        let d = track(
            &Source::Pulse("sink.monitor".into()),
            Speaker::Others,
            Path::new("/c/others.opus"),
        )
        .unwrap();
        assert!(d.contains("device=\"sink.monitor\""));
        assert!(!d.contains("_proc"));
        assert!(d.contains("appsink name=others_asr"));
        assert!(track(&Source::None, Speaker::Others, Path::new("/x")).is_none());
    }

    #[test]
    fn file_source_plays_in_real_time() {
        let d = track(
            &Source::File("/a.wav".into()),
            Speaker::Others,
            Path::new("/o"),
        )
        .unwrap();
        assert!(d.starts_with("filesrc location=\"/a.wav\" ! decodebin"));
        assert!(d.contains("identity sync=true"));
    }

    #[test]
    fn whole_pipeline() {
        let dir = Path::new("/calls/x");
        let p = recording(&Source::Test, &Source::Test, dir).unwrap();
        assert!(p.contains("/calls/x/me.opus") && p.contains("/calls/x/others.opus"));
        assert_eq!(p.matches("audiotestsrc").count(), 2);
        assert!(recording(&Source::None, &Source::None, dir).is_none());
        let only_others = recording(&Source::None, &Source::Test, dir).unwrap();
        assert!(!only_others.contains("me_asr"));
    }

    #[test]
    fn mono_mix() {
        assert_eq!(
            mix_to_mono(&[100, -100, 30000, 30000], [1.0, 0.0]),
            vec![100, 30000]
        );
        assert_eq!(mix_to_mono(&[100, 300], [0.5, 0.5]), vec![200]);
        assert_eq!(mix_to_mono(&[32767, 32767], [1.0, 1.0]), vec![32767]);
    }
}
