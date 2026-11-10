use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use recall::ui::Args;
use recall_core::config::{Config, Paths};
use recall_core::lang::Language;
use recall_core::whisper::{self, WhisperModel};

const HELP: &str = "recall: records calls with live subtitles

Usage: recall [options]

  --config-dir DIR       settings folder (default ~/.config/recall)
  --control SOCK         accept control commands on a Unix socket
  --ctl SOCK JSON        send one command to a running recall, print the reply
  --download-model NAME  download a model: en, ru, es (live subtitles),
                         base, small, turbo (Whisper), vad
  --transcribe DIR       make the Whisper transcript of a recorded call
    --no-gpu             (with --transcribe) use the CPU only
    --machine            (with --transcribe) progress lines for the app
  --gpu                  print the graphics card Whisper would use
  --self-test            check the installation and exit
  --version
";

fn main() -> ExitCode {
    let mut args = Args {
        config_dir: None,
        control: None,
    };
    let all: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| all.iter().any(|a| a == f);
    let (no_gpu, machine) = (flag("--no-gpu"), flag("--machine"));
    let mut it = all.iter().cloned();
    while let Some(a) = it.next() {
        let mut value = |name: &str| {
            it.next().unwrap_or_else(|| {
                eprintln!("{name} needs a value");
                std::process::exit(2)
            })
        };
        match a.as_str() {
            "--config-dir" => args.config_dir = Some(PathBuf::from(value("--config-dir"))),
            "--control" => args.control = Some(PathBuf::from(value("--control"))),
            "--ctl" => {
                let sock = value("--ctl");
                let json = value("--ctl");
                return match recall::control::request(sock.as_ref(), &json) {
                    Ok(line) => {
                        print!("{line}");
                        ExitCode::SUCCESS
                    }
                    Err(e) => {
                        eprintln!("{e:#}");
                        ExitCode::FAILURE
                    }
                };
            }
            "--download-model" => {
                let code = value("--download-model");
                return download_model(&code, args.config_dir.clone());
            }
            "--transcribe" => {
                let dir = value("--transcribe");
                return transcribe(dir.as_ref(), args.config_dir.clone(), no_gpu, machine);
            }
            "--no-gpu" | "--machine" => {}
            "--gpu" => {
                vulkan_quiet();
                println!(
                    "{}",
                    recall::transcribe::pick_gpu().map_or("none".into(), |(_, n)| n)
                );
                return ExitCode::SUCCESS;
            }
            "--self-test" => {
                return if recall::selftest::run() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                };
            }
            "--version" => {
                println!("recall {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{HELP}");
                return ExitCode::from(2);
            }
        }
    }
    let code = recall::ui::run(args);
    if code == gtk::glib::ExitCode::SUCCESS {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn download_model(code: &str, config_dir: Option<PathBuf>) -> ExitCode {
    let info = match (Language::from_code(code), code) {
        (Some(lang), _) => lang.live_model(),
        (None, "vad") => &whisper::VAD_MODEL,
        (None, name) => match WhisperModel::ALL.into_iter().find(|m| m.label() == name) {
            Some(m) => m.info(),
            None => {
                eprintln!("unknown model {code}: use en, ru, es, base, small, turbo or vad");
                return ExitCode::from(2);
            }
        },
    };
    recall::log::init();
    let paths = Paths::from_env(config_dir);
    let config = Config::load(&paths.config_file()).unwrap_or_default();
    let dir = config.models_dir(&paths.data_home);
    let mut shown = 0;
    let r = recall::models::download(
        info,
        &dir,
        &mut |done, total| {
            let pct = done * 100 / total.max(1);
            if pct >= shown + 10 {
                shown = pct;
                eprintln!("{pct}%");
            }
        },
        &AtomicBool::new(false),
    );
    match r {
        Ok(p) => {
            println!("{}", p.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}

/// ggml prints the Vulkan devices it finds; not wanted on stdout.
fn vulkan_quiet() {
    recall::transcribe::init_logging();
}

fn transcribe(
    dir: &std::path::Path,
    config_dir: Option<PathBuf>,
    no_gpu: bool,
    machine: bool,
) -> ExitCode {
    // The app's log is the app's; the child writes its own.
    recall::log::init_named("transcribe");
    if let Err(e) = gst::init() {
        eprintln!("GStreamer: {e}");
        return ExitCode::FAILURE;
    }
    let paths = Paths::from_env(config_dir);
    let config = Config::load(&paths.config_file()).unwrap_or_default();
    let settings = recall::transcribe::Settings {
        models_dir: config.models_dir(&paths.data_home),
        model: config.transcript_model,
        gpu: config.gpu && !no_gpu,
    };
    let started = std::time::Instant::now();
    let shown = std::sync::Mutex::new(0u32);
    let progress: recall::transcribe::Progress = std::sync::Arc::new(move |p: f64| {
        let pct = (p * 100.0) as u32;
        let mut s = shown.lock().unwrap();
        if machine && pct > *s {
            *s = pct;
            println!("progress {p:.3}");
        } else if !machine && pct >= *s + 10 {
            *s = pct;
            eprintln!("{pct}%");
        }
    });
    let md = dir.join(recall_core::call::TRANSCRIPT_MD);
    match recall::transcribe::run(dir, &settings, progress) {
        Ok(t) => {
            if machine {
                println!("done {}", md.display());
            } else {
                eprintln!(
                    "{} segments in {:.1} s ({})",
                    t.segments.len(),
                    started.elapsed().as_secs_f64(),
                    t.model
                );
                println!("{}", md.display());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            if machine {
                println!("error {}", format!("{e:#}").replace('\n', " "));
            } else {
                eprintln!("{e:#}");
            }
            ExitCode::FAILURE
        }
    }
}
