//! The accurate transcript after the call: each recorded track is decoded
//! to 16 kHz and run through Whisper (with Silero VAD, so silence is
//! skipped), then both speakers are merged into `transcript.md` and
//! `transcript.json`.
//!
//! Runs on a dedicated graphics card through Vulkan when there is one
//! (integrated Intel graphics are skipped: slow, and their drivers crashed
//! in tests), otherwise on the CPU, with a lowered priority so the computer
//! stays responsive.
//!
//! The app runs this in a child process (`recall --transcribe DIR
//! --machine`): a GPU driver crash then can't take the window down, and
//! pausing/cancelling is a signal to the child.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::Instant;

use anyhow::{Context, bail};
use gst::prelude::*;
use recall_core::call::{self, CallInfo};
use recall_core::pipelines::{self, quote};
use recall_core::speech;
use recall_core::transcript::{self, Segment};
use recall_core::whisper::{self, Transcript, WhisperModel};
use whisper_rs::{
    DtwMode, DtwModelPreset, DtwParameters, FullParams, SamplingStrategy, WhisperContext,
    WhisperContextParameters, WhisperVadContext, WhisperVadContextParams, WhisperVadParams,
};

use crate::models;

pub struct Settings {
    pub models_dir: PathBuf,
    pub model: WhisperModel,
    pub gpu: bool,
}

/// Whether `dir` is a finished call without a transcript (and no failed
/// attempt).
pub fn needs_transcript(dir: &Path) -> bool {
    read_info(dir).is_ok_and(|i| i.duration.is_some())
        && !dir.join(call::TRANSCRIPT_JSON).exists()
        && !dir.join(call::TRANSCRIPT_FAILED).exists()
}

/// Calls in `output_dir` that still need a transcript, oldest first.
pub fn pending(output_dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(output_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| needs_transcript(p))
        .collect();
    dirs.sort();
    dirs
}

fn read_info(dir: &Path) -> anyhow::Result<CallInfo> {
    let text = std::fs::read_to_string(dir.join(call::INFO_FILE))?;
    Ok(serde_json::from_str(&text)?)
}

/// Reports progress 0..=1 (called from Whisper's thread).
pub type Progress = Arc<dyn Fn(f64) + Send + Sync>;

/// Transcribes the call in `dir`.  Writes the files (or `transcript.failed`
/// with the error).
pub fn run(dir: &Path, settings: &Settings, progress: Progress) -> anyhow::Result<Transcript> {
    let _ = std::fs::remove_file(dir.join(call::TRANSCRIPT_FAILED));
    let result = transcribe(dir, settings, progress);
    match &result {
        Ok(t) => crate::log::write(&format!(
            "transcribed {}: {} segments",
            dir.display(),
            t.segments.len()
        )),
        Err(e) => {
            crate::log::call_error(dir, &format!("transcribing failed: {e:#}"));
            let _ = std::fs::write(dir.join(call::TRANSCRIPT_FAILED), format!("{e:#}\n"));
        }
    }
    result
}

/// whisper.cpp is compiled for CPUs with AVX2 and friends (see
/// `.cargo/config.toml`); on an older one it would die with SIGILL.
fn check_cpu() -> anyhow::Result<()> {
    #[cfg(target_arch = "x86_64")]
    {
        let missing: Vec<&str> = [
            ("SSE 4.2", is_x86_feature_detected!("sse4.2")),
            ("AVX", is_x86_feature_detected!("avx")),
            ("AVX2", is_x86_feature_detected!("avx2")),
            ("FMA", is_x86_feature_detected!("fma")),
            ("F16C", is_x86_feature_detected!("f16c")),
            ("BMI2", is_x86_feature_detected!("bmi2")),
        ]
        .into_iter()
        .filter(|(_, has)| !has)
        .map(|(name, _)| name)
        .collect();
        if !missing.is_empty() {
            bail!(
                "this processor is too old for Whisper transcripts (no {})",
                missing.join(", ")
            );
        }
    }
    Ok(())
}

fn transcribe(dir: &Path, settings: &Settings, progress: Progress) -> anyhow::Result<Transcript> {
    lower_priority();
    init_logging();
    check_cpu()?;
    let info = read_info(dir).context("reading call.json")?;
    let gpu = if settings.gpu { pick_gpu() } else { None };
    let model = settings.model.resolve(gpu.is_some());
    let model_info = model.info();
    let model_path = models::installed(&settings.models_dir, model_info)
        .with_context(|| format!("the Whisper model {} is not downloaded", model.label()))?;
    let vad_path = models::installed(&settings.models_dir, &whisper::VAD_MODEL);

    let started = Instant::now();
    let mut cparams = WhisperContextParameters::default();
    cparams.use_gpu(gpu.is_some());
    if let Some((id, _)) = &gpu {
        cparams.gpu_device(*id);
    }
    // DTW token times (attention-based) are accurate enough to tell which
    // turn a word belongs to; the plain ones drift by seconds.
    cparams.dtw_parameters(DtwParameters {
        mode: DtwMode::ModelPreset {
            model_preset: match model {
                WhisperModel::Base => DtwModelPreset::Base,
                WhisperModel::Auto | WhisperModel::Small => DtwModelPreset::Small,
                WhisperModel::Turbo => DtwModelPreset::LargeV3Turbo,
            },
        },
        ..DtwParameters::default()
    });
    let ctx = WhisperContext::new_with_params(&model_path, cparams)
        .context("loading the Whisper model")?;
    crate::log::write(&format!(
        "whisper {} loaded in {:.1} s on {}",
        model_info.name,
        started.elapsed().as_secs_f64(),
        gpu.as_ref().map_or("the CPU", |(_, n)| n.as_str())
    ));

    let mut vad = match &vad_path {
        Some(path) => {
            let mut vp = WhisperVadContextParams::new();
            vp.set_n_threads(threads());
            vp.set_use_gpu(false);
            Some(
                WhisperVadContext::new(&path.to_string_lossy(), vp)
                    .map_err(|e| anyhow::anyhow!("loading the voice detector: {e}"))?,
            )
        }
        None => None,
    };
    let eot = ctx.token_eot();

    // Decode and find speech in all tracks first: progress then follows
    // the amount of speech, which is what takes the time.
    let mut work = Vec::new();
    for speaker in &info.tracks {
        let file = pipelines::track_path(dir, *speaker);
        if !file.exists() {
            continue;
        }
        let audio = decode(&file).with_context(|| format!("reading {}", file.display()))?;
        let secs = audio.len() as f64 / pipelines::ASR_RATE as f64;
        let regions = match vad.as_mut() {
            Some(v) => speech_regions(v, &audio)?,
            None => vec![(0.0, secs)],
        };
        let windows = speech::pack(&regions);
        crate::log::write(&format!(
            "{speaker:?}: {secs:.0} s, {:.0} s of speech in {} windows",
            windows.iter().map(|w| w.len()).sum::<f64>(),
            windows.len()
        ));
        work.push((*speaker, audio, windows));
    }
    let total: f64 = work
        .iter()
        .flat_map(|(_, _, w)| w.iter().map(|x| x.len()))
        .sum::<f64>()
        .max(1.0);
    let mut done = 0.0;

    let mut segments = Vec::new();
    for (speaker, audio, windows) in &work {
        let t0 = Instant::now();
        let mut state = ctx.create_state().context("creating the Whisper state")?;
        for window in windows {
            let samples = window.samples(audio, pipelines::ASR_RATE);
            let mut params = full_params(info.language.code(), gpu.is_some());
            params.set_initial_prompt(info.language.style_prompt());
            let (p, before, len) = (progress.clone(), done, window.len());
            params.set_progress_callback_safe(move |pct: i32| {
                p((before + len * pct as f64 / 100.0) / total)
            });
            state
                .full(params, &samples)
                .map_err(|e| anyhow::anyhow!("Whisper failed: {e}"))?;
            for seg in state.as_iter() {
                let tokens: Vec<speech::Token> = (0..seg.n_tokens())
                    .filter_map(|i| seg.get_token(i))
                    .filter(|t| t.token_id() < eot)
                    .filter_map(|t| {
                        let d = t.token_data();
                        // The DTW time is the token's moment; -1 if unknown.
                        let (t0, t1) = if d.t_dtw >= 0 {
                            (d.t_dtw as f64 / 100.0, d.t_dtw as f64 / 100.0)
                        } else {
                            (d.t0 as f64 / 100.0, d.t1 as f64 / 100.0)
                        };
                        Some(speech::Token {
                            bytes: t.to_bytes().ok()?.to_vec(),
                            t0,
                            t1,
                        })
                    })
                    .collect();
                for line in speech::lines(window, &tokens) {
                    if let Some(text) = whisper::clean_text(&line.text) {
                        segments.push(Segment {
                            speaker: *speaker,
                            start: line.start,
                            end: line.end,
                            text,
                            words: vec![],
                        });
                    }
                }
            }
            done += window.len();
            progress(done / total);
        }
        crate::log::write(&format!(
            "{speaker:?}: transcribed in {:.1} s",
            t0.elapsed().as_secs_f64()
        ));
    }

    let segments = transcript::sorted(segments);
    let transcript = Transcript {
        title: info.title(),
        language: info.language,
        started: info.started,
        duration: info.duration,
        model: model_info.name.to_string(),
        segments,
    };
    write(dir, &transcript)?;
    Ok(transcript)
}

/// Speech regions (seconds) in 16 kHz audio.
fn speech_regions(vad: &mut WhisperVadContext, audio: &[f32]) -> anyhow::Result<Vec<(f64, f64)>> {
    let mut p = WhisperVadParams::new();
    p.set_threshold(0.5);
    p.set_min_speech_duration(250);
    p.set_min_silence_duration(300);
    p.set_max_speech_duration(speech::WINDOW as f32 - 2.0);
    p.set_speech_pad(150);
    let segs = vad
        .segments_from_samples(p, audio)
        .map_err(|e| anyhow::anyhow!("voice detection failed: {e}"))?;
    Ok(segs
        .map(|s| (s.start as f64 / 100.0, s.end as f64 / 100.0))
        .collect())
}

fn full_params<'a, 'b>(language: &'a str, gpu: bool) -> FullParams<'a, 'b> {
    // Beam search is better but several times slower: worth it on a GPU.
    let strategy = if gpu {
        SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        }
    } else {
        SamplingStrategy::Greedy { best_of: 1 }
    };
    let mut p = FullParams::new(strategy);
    p.set_language(Some(language));
    p.set_n_threads(threads());
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_special(false);
    p.set_print_timestamps(false);
    p.set_suppress_nst(true);
    p.set_token_timestamps(true);
    // whisper.cpp's own VAD option only works in whisper_full(), not with a
    // separate state (what whisper-rs uses): we pack speech ourselves.
    // (No abort callback: whisper-rs 0.16's set_abort_callback_safe passes
    // the closure with the wrong type and makes every encode fail.)
    p
}

/// CPU threads for Whisper: all cores but one (at least one).
fn threads() -> i32 {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_sub(1).max(1))
        .unwrap_or(2) as i32
}

/// Names of the Vulkan GPUs whisper.cpp can see.
pub fn gpu_names() -> Vec<String> {
    whisper_rs::vulkan::list_devices()
        .into_iter()
        .map(|d| d.name)
        .collect()
}

/// The graphics card to use: `RECALL_GPU_DEVICE=<index>` if set, else the
/// first dedicated one (see [`recall_core::whisper::usable_gpu`]).
pub fn pick_gpu() -> Option<(i32, String)> {
    let devices = whisper_rs::vulkan::list_devices();
    if let Some(id) = std::env::var("RECALL_GPU_DEVICE")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
    {
        return devices
            .into_iter()
            .find(|d| d.id == id)
            .map(|d| (d.id, d.name));
    }
    devices
        .into_iter()
        .find(|d| whisper::usable_gpu(&d.name))
        .map(|d| (d.id, d.name))
}

/// Nice 10 for this thread (and the worker threads it creates).
fn lower_priority() {
    // SAFETY: plain syscall; who = 0 is the calling thread on Linux.
    unsafe {
        libc::setpriority(libc::PRIO_PROCESS, 0, 10);
    }
}

/// whisper.cpp and ggml print to stderr; silence them (except with
/// `RECALL_DEBUG`).
pub fn init_logging() {
    static ONCE: Once = Once::new();
    if std::env::var_os("RECALL_DEBUG").is_none() {
        ONCE.call_once(whisper_rs::install_logging_hooks);
    }
}

/// The track as 16 kHz mono f32.
fn decode(file: &Path) -> anyhow::Result<Vec<f32>> {
    let desc = format!(
        "filesrc location={} ! oggdemux ! opusdec ! audioconvert ! audioresample \
         ! audio/x-raw,format=F32LE,rate={},channels=1 ! appsink name=out sync=false",
        quote(&file.to_string_lossy()),
        pipelines::ASR_RATE
    );
    let pipeline = gst::parse::launch(&desc)?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow::anyhow!("not a pipeline"))?;
    let sink = pipeline
        .by_name("out")
        .and_then(|e| e.downcast::<gst_app::AppSink>().ok())
        .context("no appsink")?;
    pipeline.set_state(gst::State::Playing)?;
    let mut out = Vec::new();
    while let Ok(sample) = sink.pull_sample() {
        if let Some(buf) = sample.buffer()
            && let Ok(map) = buf.map_readable()
        {
            out.extend(
                map.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|b| f32::from_le_bytes(*b)),
            );
        }
    }
    let error = pipeline.bus().and_then(|bus| {
        bus.pop_filtered(&[gst::MessageType::Error])
            .and_then(|m| match m.view() {
                gst::MessageView::Error(e) => Some(e.error().to_string()),
                _ => None,
            })
    });
    let _ = pipeline.set_state(gst::State::Null);
    if let Some(e) = error
        && out.is_empty()
    {
        bail!("{e}");
    }
    Ok(out)
}

fn write(dir: &Path, t: &Transcript) -> anyhow::Result<()> {
    let mut md = transcript::markdown(&t.title, t.language, &t.segments);
    md.push_str(&format!(
        "\n---\nTranscribed by recall with Whisper ({}).\n",
        t.model
    ));
    let json = serde_json::to_string_pretty(t)?;
    // Atomic: a half-written transcript.json would count as done.
    let tmp = dir.join(".transcript.json.tmp");
    std::fs::write(dir.join(call::TRANSCRIPT_MD), md)?;
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, dir.join(call::TRANSCRIPT_JSON))?;
    Ok(())
}
