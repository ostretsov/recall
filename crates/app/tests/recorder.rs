//! The real recorder with test sources (no sound devices needed).

use std::time::Duration;

use recall::models::VoskCache;
use recall::recorder::{Event, Options, Recorder, SubtitleStatus};
use recall_core::lang::Language;
use recall_core::mic_channel;
use recall_core::pipelines::Source;
use recall_core::transcript::Speaker;

#[test]
fn records_both_tracks_with_levels() {
    gst::init().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let (tx, rx) = async_channel::unbounded();
    let rec = Recorder::start(
        Options {
            dir: tmp.path().join("call"),
            language: Language::Ru,
            mic: Source::Test,
            mic_channel: mic_channel::Mode::Auto,
            others: Source::Test,
            models_dir: tmp.path().join("no-models"),
            vosk: VoskCache::default(),
        },
        tx,
    )
    .unwrap();
    std::thread::sleep(Duration::from_millis(1500));
    let finished = rec.stop().unwrap();
    let events: Vec<Event> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    for sp in Speaker::ALL {
        let levels: Vec<f64> = events
            .iter()
            .filter_map(|e| match e {
                Event::Level { speaker, db } if *speaker == sp => Some(*db),
                _ => None,
            })
            .collect();
        assert!(
            levels.len() >= 10,
            "{sp:?}: {} level readings",
            levels.len()
        );
        // A 0.05 sine: about -29 dBFS.
        let last = *levels.last().unwrap();
        assert!((-32.0..-26.0).contains(&last), "{sp:?} level {last}");
        assert!(events.contains(&Event::Subtitles {
            speaker: sp,
            status: SubtitleStatus::ModelMissing
        }));
        let file = recall_core::pipelines::track_path(&finished.dir, sp);
        assert!(std::fs::metadata(&file).unwrap().len() > 2000, "{file:?}");
    }
    assert!(
        !events.iter().any(|e| matches!(e, Event::Error(_))),
        "{events:?}"
    );
    assert!(finished.dir.join("call.json").exists());
    assert!(finished.dir.join("live.md").exists());
}
