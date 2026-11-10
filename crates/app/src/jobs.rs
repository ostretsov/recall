//! Runs transcriptions one at a time in a child process
//! (`recall --transcribe DIR --machine`) and reports to the UI thread.
//!
//! A child keeps a GPU driver crash away from the window; if it dies that
//! way, the call is retried on the CPU.  While a new call is being recorded
//! the child is stopped (SIGSTOP) so the recording and live subtitles get
//! the computer; it continues afterwards.

use std::io::{BufRead, BufReader};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use recall_core::whisper::ChildLine;

#[derive(Debug)]
pub enum JobEvent {
    Progress {
        dir: PathBuf,
        fraction: f64,
    },
    Done {
        dir: PathBuf,
        result: Result<PathBuf, String>,
        crashed: bool,
        gpu: bool,
    },
}

pub struct Running {
    pub dir: PathBuf,
    pub pid: u32,
    pub gpu: bool,
}

/// Starts the child for `dir`; events go to `events`.
pub fn spawn(
    dir: &Path,
    config_dir: Option<&Path>,
    gpu: bool,
    events: async_channel::Sender<JobEvent>,
) -> anyhow::Result<Running> {
    let exe = std::env::current_exe()?;
    let mut cmd = Command::new(exe);
    if let Some(c) = config_dir {
        cmd.arg("--config-dir").arg(c);
    }
    cmd.arg("--transcribe").arg(dir).arg("--machine");
    if !gpu {
        cmd.arg("--no-gpu");
    }
    cmd.stdout(Stdio::piped())
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            // Die with the app; lower priority than the app.
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            libc::setpriority(libc::PRIO_PROCESS, 0, 10);
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped");
    let d = dir.to_path_buf();
    crate::log::write(&format!(
        "transcribing {} (pid {pid}, gpu {gpu})",
        d.display()
    ));
    std::thread::Builder::new()
        .name("transcribe".into())
        .spawn(move || {
            let mut result = Err("the transcriber stopped without a result".to_string());
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                match ChildLine::parse(&line) {
                    Some(ChildLine::Progress(f)) => {
                        let _ = events.send_blocking(JobEvent::Progress {
                            dir: d.clone(),
                            fraction: f,
                        });
                    }
                    Some(ChildLine::Done(p)) => result = Ok(PathBuf::from(p)),
                    Some(ChildLine::Error(e)) => result = Err(e),
                    None => {}
                }
            }
            let status = child.wait();
            // Killed by a signal we didn't send (we use SIGKILL to cancel).
            let crashed = status
                .as_ref()
                .ok()
                .and_then(|s| s.signal())
                .is_some_and(|sig| sig != libc::SIGKILL);
            crate::log::write(&format!(
                "transcriber for {} exited: {status:?}",
                d.display()
            ));
            let _ = events.send_blocking(JobEvent::Done {
                dir: d,
                result,
                crashed,
                gpu,
            });
        })?;
    Ok(Running {
        dir: dir.to_path_buf(),
        pid,
        gpu,
    })
}

pub fn signal(pid: u32, sig: libc::c_int) {
    // SAFETY: plain syscall on our own child.
    unsafe {
        libc::kill(pid as libc::pid_t, sig);
    }
}

/// The graphics card the transcriber would use (`recall --gpu`), run in a
/// child so Vulkan never initializes in the app.  `None`: no usable one.
pub fn detect_gpu() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let out = Command::new(exe)
        .arg("--gpu")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !name.is_empty() && name != "none").then_some(name)
}
