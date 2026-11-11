//! Recording a call: the GStreamer pipeline (two tracks to Opus files) and
//! one live-subtitle thread per track (Vosk).
//!
//! Times are pipeline running time: seconds since recording started, the
//! same for both tracks, so "Me" and "Others" lines interleave correctly.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use gst::prelude::*;
use recall_core::call::{self, CallInfo};
use recall_core::lang::Language;
use recall_core::levels;
use recall_core::mic_channel::{self, Picker};
use recall_core::pipelines::{self, Source};
use recall_core::transcript::{self, Segment, Speaker, Word};

use crate::models::VoskCache;

/// What the recorder reports to the UI.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Text being recognized right now (replaces the previous partial).
    Partial { speaker: Speaker, text: String },
    /// A finished utterance.
    Segment(Segment),
    /// Level in dBFS, about ten times a second.
    Level { speaker: Speaker, db: f64 },
    /// Live subtitles for a track are off or on again.
    Subtitles {
        speaker: Speaker,
        status: SubtitleStatus,
    },
    /// Something the user should know; recording continues.
    Warning(String),
    /// Recording failed (device gone, disk full, ...).
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum SubtitleStatus {
    On,
    /// The language's model is not downloaded.
    ModelMissing,
    Failed(String),
}

pub struct Options {
    pub dir: PathBuf,
    pub language: Language,
    pub mic: Source,
    pub mic_channel: mic_channel::Mode,
    pub others: Source,
    pub models_dir: PathBuf,
    pub vosk: VoskCache,
}

/// Recognizer input is fed in pieces of this many samples (100 ms).
const CHUNK: usize = pipelines::ASR_RATE as usize / 10;
/// Partial results are asked for every this many chunks.
const PARTIAL_EVERY: u32 = 3;
/// If recognition falls this far behind (slow machine), audio is skipped
/// for subtitles (the recording itself is complete).
const MAX_LAG: f64 = 8.0;
/// How often a missing model is looked for again (it may get downloaded
/// during the call).
const MODEL_RECHECK: Duration = Duration::from_secs(2);

/// Summary of a finished recording.
#[derive(Debug)]
pub struct Finished {
    pub dir: PathBuf,
    pub duration: f64,
    pub segments: Vec<Segment>,
}

pub struct Recorder {
    pipeline: gst::Pipeline,
    asr_threads: Vec<JoinHandle<()>>,
    bus_thread: Option<JoinHandle<()>>,
    eos_rx: mpsc::Receiver<()>,
    stopping: Arc<AtomicBool>,
    started: Instant,
    info: CallInfo,
    dir: PathBuf,
    segments: Arc<Mutex<Vec<Segment>>>,
}

impl Recorder {
    pub fn start(opts: Options, events: async_channel::Sender<Event>) -> anyhow::Result<Recorder> {
        let others = crate::devices::resolve_others(opts.others.clone())?;
        let desc = pipelines::recording(&opts.mic, &others, &opts.dir)
            .context("both tracks are switched off (microphone = \"none\", others = \"none\")")?;
        std::fs::create_dir_all(&opts.dir)
            .with_context(|| format!("creating {}", opts.dir.display()))?;
        let tracks: Vec<Speaker> = [(Speaker::Me, &opts.mic), (Speaker::Others, &others)]
            .into_iter()
            .filter(|(_, s)| **s != Source::None)
            .map(|(sp, _)| sp)
            .collect();
        let info = CallInfo {
            started: chrono::Local::now(),
            duration: None,
            language: opts.language,
            tracks: tracks.clone(),
            recorder: format!("recall {}", env!("CARGO_PKG_VERSION")),
        };
        write_info(&opts.dir, &info)?;
        crate::log::write(&format!("recording to {}: {desc}", opts.dir.display()));

        let pipeline = gst::parse::launch(&desc)
            .context("building the recording pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
        // A pulsesrc clock stops while no sound arrives; the system clock never does.
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));

        if tracks.contains(&Speaker::Me)
            && let Some(proc_) = pipeline.by_name(pipelines::mic_processing_name())
            && let Some(pad) = proc_.static_pad("src")
        {
            install_mic_processing(&pad, opts.mic_channel);
        }

        let jsonl = Arc::new(Mutex::new(
            File::options()
                .create(true)
                .append(true)
                .open(opts.dir.join(call::LIVE_JSONL))?,
        ));
        let segments = Arc::new(Mutex::new(Vec::new()));
        let (eos_tx, eos_rx) = mpsc::channel();
        let stopping = Arc::new(AtomicBool::new(false));
        let bus_thread = {
            let bus = pipeline.bus().context("no bus")?;
            let pipeline = pipeline.downgrade();
            let stopping = stopping.clone();
            let events = events.clone();
            let dir = opts.dir.clone();
            std::thread::Builder::new()
                .name("bus".into())
                .spawn(move || bus_loop(bus, pipeline, stopping, eos_tx, events, dir))?
        };

        // Start before the recognizer threads pull: an appsink that is not
        // running yet answers a pull with "no more samples".
        if let Err(e) = pipeline.set_state(gst::State::Playing) {
            stopping.store(true, Ordering::Relaxed);
            let _ = pipeline.set_state(gst::State::Null);
            let _ = bus_thread.join();
            crate::log::call_error(&opts.dir, &format!("cannot start recording: {e}"));
            bail!("cannot start recording: {e}");
        }
        let mut asr_threads = Vec::new();
        for speaker in &tracks {
            let sink = pipeline
                .by_name(&pipelines::asr_sink_name(*speaker))
                .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
                .context("speech recognition sink missing")?;
            let ctx = AsrContext {
                speaker: *speaker,
                language: opts.language,
                models_dir: opts.models_dir.clone(),
                vosk: opts.vosk.clone(),
                pipeline: pipeline.downgrade(),
                dir: opts.dir.clone(),
                events: events.clone(),
                jsonl: jsonl.clone(),
                segments: segments.clone(),
            };
            asr_threads.push(
                std::thread::Builder::new()
                    .name(format!("asr-{}", speaker.track_name()))
                    .spawn(move || asr_loop(sink, ctx))?,
            );
        }

        let recorder = Recorder {
            pipeline,
            asr_threads,
            bus_thread: Some(bus_thread),
            eos_rx,
            stopping,
            started: Instant::now(),
            info,
            dir: opts.dir,
            segments,
        };
        Ok(recorder)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn elapsed(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    /// Finishes the files and the subtitles.  Blocks for up to a few
    /// seconds: call it off the UI thread.
    pub fn stop(mut self) -> anyhow::Result<Finished> {
        let duration = self.elapsed();
        self.pipeline.send_event(gst::event::Eos::new());
        if self.eos_rx.recv_timeout(Duration::from_secs(5)).is_err() {
            crate::log::call_error(
                &self.dir,
                "stop: the recording did not finish within 5 s; closed anyway (the end of the files may be cut)",
            );
        }
        self.stopping.store(true, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
        for t in self.asr_threads.drain(..) {
            let _ = t.join();
        }
        if let Some(t) = self.bus_thread.take() {
            let _ = t.join();
        }
        self.info.duration = Some(duration);
        write_info(&self.dir, &self.info)?;
        let segments = transcript::sorted(self.segments.lock().unwrap().clone());
        std::fs::write(
            self.dir.join(call::LIVE_MD),
            transcript::markdown(&self.info.title(), self.info.language, &segments),
        )?;
        crate::log::write(&format!(
            "stopped after {duration:.1} s, {} segments",
            segments.len()
        ));
        Ok(Finished {
            dir: self.dir.clone(),
            duration,
            segments,
        })
    }
}

fn write_info(dir: &Path, info: &CallInfo) -> anyhow::Result<()> {
    std::fs::write(
        dir.join(call::INFO_FILE),
        serde_json::to_string_pretty(info)?,
    )?;
    Ok(())
}

fn bus_loop(
    bus: gst::Bus,
    pipeline: gst::glib::WeakRef<gst::Pipeline>,
    stopping: Arc<AtomicBool>,
    eos_tx: mpsc::Sender<()>,
    events: async_channel::Sender<Event>,
    dir: PathBuf,
) {
    while !stopping.load(Ordering::Relaxed) {
        let Some(msg) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) else {
            continue;
        };
        use gst::MessageView;
        match msg.view() {
            MessageView::Eos(_) => {
                let _ = eos_tx.send(());
            }
            MessageView::Error(e) => {
                let src = e.src().map(|s| s.name().to_string()).unwrap_or_default();
                let text = format!("{src}: {} ({:?})", e.error(), e.debug());
                crate::log::call_error(&dir, &format!("pipeline error: {text}"));
                let _ =
                    events.send_blocking(Event::Error(user_error(&src, &e.error().to_string())));
                // Let a stop in progress finish without waiting for EOS.
                let _ = eos_tx.send(());
            }
            MessageView::Warning(w) => {
                crate::log::write(&format!(
                    "pipeline warning: {} ({:?})",
                    w.error(),
                    w.debug()
                ));
            }
            MessageView::Latency(_) => {
                if let Some(p) = pipeline.upgrade() {
                    let _ = p.recalculate_latency();
                }
            }
            _ => {}
        }
    }
}

/// A short message for the status line.
fn user_error(element: &str, error: &str) -> String {
    let what = if element.contains("recall-me") || element.starts_with("me") {
        "Microphone: "
    } else if element.contains("recall-others") || element.starts_with("others") {
        "Sound output: "
    } else if element.contains("filesink") {
        "Saving: "
    } else {
        ""
    };
    format!("Recording stopped. {what}{error}")
}

/// Turns the (stereo) mic into the voice in both channels, picking the
/// channel that carries it (see `mic_channel`).
fn install_mic_processing(pad: &gst::Pad, mode: mic_channel::Mode) {
    /// 50 ms of stereo frames at 48 kHz per picker reading.
    const READING_FRAMES: usize = pipelines::RATE as usize / 20;
    struct St {
        picker: Picker,
        sum: [f64; 2],
        frames: usize,
    }
    let st = Mutex::new(St {
        picker: Picker::new(mode),
        sum: [0.0; 2],
        frames: 0,
    });
    pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if let Some(gst::PadProbeData::Buffer(buffer)) = &mut info.data
            && let Ok(mut map) = buffer.make_mut().map_writable()
        {
            let stereo: Vec<i16> = map
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| i16::from_le_bytes(*b))
                .collect();
            let mut st = st.lock().unwrap();
            for f in stereo.as_chunks::<2>().0 {
                for (sum, x) in st.sum.iter_mut().zip(f) {
                    *sum += (*x as f64 / 32768.0).powi(2);
                }
                st.frames += 1;
                if st.frames >= READING_FRAMES {
                    let n = st.frames as f64;
                    let db = st.sum.map(|s| 10.0 * (s / n).log10());
                    if let Some(p) = st.picker.feed(&db) {
                        crate::log::write(&format!("microphone channel: {p:?}"));
                    }
                    st.sum = [0.0; 2];
                    st.frames = 0;
                }
            }
            let mono = pipelines::mix_to_mono(&stereo, st.picker.current().gains());
            drop(st);
            for (frame, x) in map.as_chunks_mut::<4>().0.iter_mut().zip(mono) {
                let [lo, hi] = x.to_le_bytes();
                *frame = [lo, hi, lo, hi];
            }
        }
        gst::PadProbeReturn::Ok
    });
}

struct AsrContext {
    speaker: Speaker,
    language: Language,
    models_dir: PathBuf,
    vosk: VoskCache,
    pipeline: gst::glib::WeakRef<gst::Pipeline>,
    /// The call's folder (for its `errors.log`).
    dir: PathBuf,
    events: async_channel::Sender<Event>,
    jsonl: Arc<Mutex<File>>,
    segments: Arc<Mutex<Vec<Segment>>>,
}

/// A recognizer and the recording time its own clock starts at.
struct Live {
    rec: vosk::Recognizer,
    offset: f64,
    _model: Arc<vosk::Model>,
    chunks: u32,
    partial: String,
}

/// Pulls 16 kHz mono from the appsink until EOS, measures levels and runs
/// the recognizer.
fn asr_loop(sink: gst_app::AppSink, ctx: AsrContext) {
    let mut live: Option<Live> = None;
    let mut status: Option<SubtitleStatus> = None;
    let mut next_model_check = Instant::now();
    let mut chunk: Vec<i16> = Vec::with_capacity(CHUNK * 2);
    let mut chunk_start = 0.0;
    let mut lagging = false;
    while let Ok(sample) = sink.pull_sample() {
        let Some(buffer) = sample.buffer() else {
            continue;
        };
        let pts = buffer.pts().map(|t| t.seconds_f64()).unwrap_or(chunk_start);
        let Ok(map) = buffer.map_readable() else {
            continue;
        };
        let samples: Vec<i16> = map
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        drop(map);

        // Fallen behind real time?  Skip audio (for subtitles only).
        let now = ctx
            .pipeline
            .upgrade()
            .and_then(|p| p.current_running_time())
            .map(|t| t.seconds_f64());
        if let Some(now) = now
            && now - pts > MAX_LAG
        {
            if !lagging {
                lagging = true;
                crate::log::call_error(
                    &ctx.dir,
                    &format!(
                        "subtitles {:?}: recognition {:.1} s behind; skipping audio (the recording is complete)",
                        ctx.speaker,
                        now - pts
                    ),
                );
                let _ = ctx.events.send_blocking(Event::Warning(
                    "Subtitles are falling behind (the computer is busy); some speech is skipped. The recording itself is complete.".into(),
                ));
            }
            if let Some(l) = live.take() {
                finish(l, &ctx, pts);
            }
            chunk.clear();
            continue;
        }
        lagging = false;

        if chunk.is_empty() {
            chunk_start = pts;
        }
        chunk.extend_from_slice(&samples);
        if chunk.len() < CHUNK {
            continue;
        }
        let _ = ctx.events.send_blocking(Event::Level {
            speaker: ctx.speaker,
            db: levels::rms_db(&chunk),
        });
        if live.is_none() && Instant::now() >= next_model_check {
            next_model_check = Instant::now() + MODEL_RECHECK;
            live = start_recognizer(&ctx, chunk_start, &mut status);
        }
        if let Some(l) = live.as_mut() {
            feed(l, &chunk, &ctx, chunk_start);
        }
        chunk.clear();
    }
    if let Some(mut l) = live {
        if !chunk.is_empty() {
            feed(&mut l, &chunk, &ctx, chunk_start);
        }
        finish(l, &ctx, chunk_start);
    }
}

fn start_recognizer(
    ctx: &AsrContext,
    offset: f64,
    status: &mut Option<SubtitleStatus>,
) -> Option<Live> {
    let mut report = |s: SubtitleStatus| {
        if status.as_ref() != Some(&s) {
            if let SubtitleStatus::Failed(e) = &s {
                crate::log::call_error(&ctx.dir, &format!("subtitles {:?}: {e}", ctx.speaker));
            }
            *status = Some(s.clone());
            let _ = ctx.events.send_blocking(Event::Subtitles {
                speaker: ctx.speaker,
                status: s,
            });
        }
    };
    let model = match ctx.vosk.get(ctx.language, &ctx.models_dir) {
        Ok(Some(m)) => m,
        Ok(None) => {
            report(SubtitleStatus::ModelMissing);
            return None;
        }
        Err(e) => {
            report(SubtitleStatus::Failed(format!("{e:#}")));
            return None;
        }
    };
    let Some(mut rec) = vosk::Recognizer::new(&model, pipelines::ASR_RATE as f32) else {
        report(SubtitleStatus::Failed(
            "cannot create the recognizer".into(),
        ));
        return None;
    };
    rec.set_words(true);
    report(SubtitleStatus::On);
    Some(Live {
        rec,
        offset,
        _model: model,
        chunks: 0,
        partial: String::new(),
    })
}

fn feed(l: &mut Live, chunk: &[i16], ctx: &AsrContext, time: f64) {
    match l.rec.accept_waveform(chunk) {
        Ok(vosk::DecodingState::Finalized) => {
            let seg = l.rec.result().single().and_then(|r| {
                Segment::from_words(ctx.speaker, r.text, words(&r.result), l.offset, time)
            });
            emit_final(l, seg, ctx);
        }
        Ok(vosk::DecodingState::Running) => {
            l.chunks += 1;
            if l.chunks.is_multiple_of(PARTIAL_EVERY) {
                let text = l.rec.partial_result().partial.trim().to_string();
                if text != l.partial {
                    l.partial = text.clone();
                    let _ = ctx.events.send_blocking(Event::Partial {
                        speaker: ctx.speaker,
                        text,
                    });
                }
            }
        }
        Ok(vosk::DecodingState::Failed) | Err(_) => {
            crate::log::write(&format!("{:?}: recognizer rejected audio", ctx.speaker));
        }
    }
}

/// Flushes the last utterance.
fn finish(mut l: Live, ctx: &AsrContext, time: f64) {
    let seg =
        l.rec.final_result().single().and_then(|r| {
            Segment::from_words(ctx.speaker, r.text, words(&r.result), l.offset, time)
        });
    emit_final(&mut l, seg, ctx);
}

fn emit_final(l: &mut Live, seg: Option<Segment>, ctx: &AsrContext) {
    if !l.partial.is_empty() {
        l.partial.clear();
        let _ = ctx.events.send_blocking(Event::Partial {
            speaker: ctx.speaker,
            text: String::new(),
        });
    }
    let Some(seg) = seg else { return };
    {
        let mut f = ctx.jsonl.lock().unwrap();
        let _ = writeln!(f, "{}", seg.to_json_line());
        let _ = f.flush();
    }
    ctx.segments.lock().unwrap().push(seg.clone());
    let _ = ctx.events.send_blocking(Event::Segment(seg));
}

fn words(ws: &[vosk::Word]) -> Vec<Word> {
    ws.iter()
        .map(|w| Word {
            word: w.word.to_string(),
            start: w.start as f64,
            end: w.end as f64,
            conf: w.conf,
        })
        .collect()
}
