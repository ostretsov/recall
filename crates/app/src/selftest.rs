//! `recall --self-test`: checks an installation without sound devices or a
//! network — GStreamer elements, the Vosk library, a short recording with
//! test tones through the real recorder, and (with a display) GTK.

use std::time::Duration;

use recall_core::deps;
use recall_core::lang::Language;
use recall_core::mic_channel;
use recall_core::pipelines::Source;
use recall_core::transcript::Speaker;

use crate::models::VoskCache;
use crate::recorder::{Event, Options, Recorder};

pub fn in_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(program).is_file()))
}

/// Prints one line per check; returns whether all passed.
pub fn run() -> bool {
    let mut ok = true;
    let mut check = |name: &str, result: anyhow::Result<String>| match result {
        Ok(detail) => println!(
            "ok    {name}{}",
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        ),
        Err(e) => {
            ok = false;
            println!("FAIL  {name}: {e:#}");
        }
    };
    check(
        "GStreamer",
        gst::init()
            .map(|_| gst::version_string().to_string())
            .map_err(Into::into),
    );
    vosk::set_log_level(vosk::LogLevel::Error);
    check("GStreamer elements", {
        let missing = deps::missing_packages(|e| gst::ElementFactory::find(e).is_some(), |_| true);
        if missing.is_empty() {
            Ok(format!("{} found", deps::ELEMENTS.len()))
        } else {
            Err(anyhow::anyhow!("missing; install {}", missing.join(" ")))
        }
    });
    // Calls into libvosk: a missing model is reported as None, not a crash.
    check(
        "Vosk library",
        match vosk::Model::new("/nonexistent/recall-self-test") {
            None => Ok("loaded".into()),
            Some(_) => Err(anyhow::anyhow!("loaded a model from nowhere")),
        },
    );
    check("recording", test_recording());
    // Vulkan loader + whisper.cpp; no graphics card is fine (CPU).
    crate::transcribe::init_logging();
    check(
        "transcripts",
        Ok(match crate::transcribe::pick_gpu() {
            Some((_, name)) => format!("on {name}"),
            None => "on the CPU (no dedicated graphics card)".into(),
        }),
    );
    if std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some() {
        check(
            "GTK",
            gtk::init().map(|_| String::new()).map_err(Into::into),
        );
    } else {
        println!("skip  GTK (no display)");
    }
    println!(
        "{}",
        if ok {
            "self-test passed"
        } else {
            "self-test FAILED"
        }
    );
    ok
}

/// Two seconds of test tones on both tracks through the real recorder.
fn test_recording() -> anyhow::Result<String> {
    let tmp = std::env::temp_dir().join(format!("recall-self-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let (tx, rx) = async_channel::unbounded();
    let rec = Recorder::start(
        Options {
            dir: tmp.join("call"),
            language: Language::En,
            mic: Source::Test,
            mic_channel: mic_channel::Mode::Auto,
            others: Source::Test,
            models_dir: tmp.join("models"),
            vosk: VoskCache::default(),
        },
        tx,
    )?;
    std::thread::sleep(Duration::from_secs(2));
    let finished = rec.stop()?;
    let mut heard = [false; 2];
    let mut errors = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        match ev {
            Event::Level { speaker, db } if db > -60.0 => heard[speaker as usize] = true,
            Event::Error(e) => errors.push(e),
            _ => {}
        }
    }
    let result = (|| {
        anyhow::ensure!(errors.is_empty(), "{}", errors.join("; "));
        for sp in Speaker::ALL {
            let f = recall_core::pipelines::track_path(&finished.dir, sp);
            let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
            anyhow::ensure!(size > 2000, "{} is {size} bytes", f.display());
            anyhow::ensure!(heard[sp as usize], "no sound reached {sp:?}'s recognizer");
        }
        Ok(format!("{:.1} s on both tracks", finished.duration))
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}
