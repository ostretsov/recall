//! Speech models: download (with checksum), unpack, load and keep loaded.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, bail};
use recall_core::call::human_size;
use recall_core::lang::{Language, ModelInfo, ModelKind};
use sha2::{Digest, Sha256};

/// Written into a model folder once it is completely unpacked.
const COMPLETE_MARKER: &str = ".recall-complete";

/// The model's folder (zip models) or file if it is installed completely.
pub fn installed(models_dir: &Path, info: &ModelInfo) -> Option<PathBuf> {
    let path = models_dir.join(info.name);
    match info.kind {
        ModelKind::Zip => path.join(COMPLETE_MARKER).exists().then_some(path),
        // Files get their final name only after the checksum matched.
        ModelKind::File => path.is_file().then_some(path),
    }
}

/// Which of `wanted` are not installed.
pub fn missing<'a>(models_dir: &Path, wanted: &[&'a ModelInfo]) -> Vec<&'a ModelInfo> {
    wanted
        .iter()
        .copied()
        .filter(|m| installed(models_dir, m).is_none())
        .collect()
}

/// Free bytes on the file system holding `path` (or its nearest existing
/// parent).
pub fn free_space(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: c is a valid C string and st a valid out pointer.
    (unsafe { libc::statvfs(c.as_ptr(), &mut st) } == 0)
        .then(|| st.f_bavail as u64 * st.f_frsize as u64)
}

/// Downloads and unpacks `info` into `models_dir`.  `progress(done, total)`
/// is called while downloading; setting `cancel` stops it.
pub fn download(
    info: &ModelInfo,
    models_dir: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> anyhow::Result<PathBuf> {
    download_from(info, info.url, models_dir, progress, cancel)
}

/// [`download`] from another URL (tests use a local server).
pub fn download_from(
    info: &ModelInfo,
    url: &str,
    models_dir: &Path,
    progress: &mut dyn FnMut(u64, u64),
    cancel: &AtomicBool,
) -> anyhow::Result<PathBuf> {
    if let Some(dir) = installed(models_dir, info) {
        return Ok(dir);
    }
    std::fs::create_dir_all(models_dir)
        .with_context(|| format!("creating {}", models_dir.display()))?;
    // Zip + unpacked model (about twice the zip) + margin.
    let need = match info.kind {
        ModelKind::Zip => info.size * 4,
        ModelKind::File => info.size + info.size / 2,
    };
    if let Some(free) = free_space(models_dir)
        && free < need
    {
        bail!(
            "not enough disk space for the {} model: {} free, {} needed",
            info.name,
            human_size(free),
            human_size(need)
        );
    }
    let zip_path = models_dir.join(format!(".{}.part", info.name));
    let tmp_dir = models_dir.join(format!(".{}.unpacking", info.name));
    let result = (|| {
        crate::log::write(&format!("downloading {url}"));
        let mut resp = ureq::get(url)
            .call()
            .with_context(|| format!("downloading {url}"))?;
        let total = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok()?.parse().ok())
            .unwrap_or(info.size);
        let mut reader = resp.body_mut().as_reader();
        let mut file = std::fs::File::create(&zip_path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 64 * 1024];
        let mut done = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                bail!("download cancelled");
            }
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            file.write_all(&buf[..n])?;
            done += n as u64;
            progress(done, total);
        }
        file.sync_all()?;
        let sum = hex(&hasher.finalize());
        if sum != info.sha256 {
            bail!(
                "the downloaded {} is damaged (checksum mismatch); try again",
                info.name
            );
        }
        if info.kind == ModelKind::File {
            let dest = models_dir.join(info.name);
            std::fs::rename(&zip_path, &dest)?;
            crate::log::write(&format!("installed {}", dest.display()));
            return Ok(dest);
        }
        let _ = std::fs::remove_dir_all(&tmp_dir);
        zip::ZipArchive::new(std::fs::File::open(&zip_path)?)?
            .extract(&tmp_dir)
            .context("unpacking the model")?;
        let unpacked = tmp_dir.join(info.name);
        if !unpacked.is_dir() {
            bail!("{} does not contain {}/", url, info.name);
        }
        std::fs::write(unpacked.join(COMPLETE_MARKER), info.sha256)?;
        let dest = models_dir.join(info.name);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(&unpacked, &dest)?;
        crate::log::write(&format!("installed {}", dest.display()));
        Ok(dest)
    })();
    let _ = std::fs::remove_file(&zip_path);
    let _ = std::fs::remove_dir_all(&tmp_dir);
    result
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Loaded Vosk models, shared by both speakers' recognizers (loading takes
/// a second or two and ~100 MB of memory, so each is loaded once).
#[derive(Clone, Default)]
pub struct VoskCache {
    loaded: Arc<Mutex<HashMap<Language, Arc<vosk::Model>>>>,
}

impl VoskCache {
    /// The live model for `lang`: `Ok(None)` if not downloaded.
    pub fn get(
        &self,
        lang: Language,
        models_dir: &Path,
    ) -> anyhow::Result<Option<Arc<vosk::Model>>> {
        let mut loaded = self.loaded.lock().unwrap();
        if let Some(m) = loaded.get(&lang) {
            return Ok(Some(m.clone()));
        }
        let Some(dir) = installed(models_dir, lang.live_model()) else {
            return Ok(None);
        };
        let started = std::time::Instant::now();
        let model = vosk::Model::new(dir.to_string_lossy())
            .with_context(|| format!("loading the speech model {}", dir.display()))?;
        crate::log::write(&format!(
            "loaded {} in {:.1} s",
            dir.display(),
            started.elapsed().as_secs_f64()
        ));
        let model = Arc::new(model);
        loaded.insert(lang, model.clone());
        Ok(Some(model))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// Serves `body` once over HTTP on localhost; returns the URL.
    fn serve_once(body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/m.zip", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 4096];
            let _ = s.read(&mut req);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).unwrap();
            s.write_all(&body).unwrap();
        });
        url
    }

    fn test_zip(name: &str) -> Vec<u8> {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        w.add_directory(format!("{name}/"), opts).unwrap();
        w.start_file(format!("{name}/README"), opts).unwrap();
        w.write_all(b"model").unwrap();
        w.finish().unwrap().into_inner()
    }

    fn info_for(body: &[u8]) -> ModelInfo {
        let sum: &'static str = Box::leak(hex(&Sha256::digest(body)).into_boxed_str());
        ModelInfo {
            name: "test-model",
            kind: ModelKind::Zip,
            url: "unused",
            sha256: sum,
            size: body.len() as u64,
        }
    }

    #[test]
    fn download_verify_unpack() {
        let dir = tempfile::tempdir().unwrap();
        let body = test_zip("test-model");
        let info = info_for(&body);
        assert!(installed(dir.path(), &info).is_none());
        let mut last = (0, 0);
        let path = download_from(
            &info,
            &serve_once(body.clone()),
            dir.path(),
            &mut |d, t| last = (d, t),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(last, (body.len() as u64, body.len() as u64));
        assert_eq!(
            std::fs::read_to_string(path.join("README")).unwrap(),
            "model"
        );
        assert_eq!(installed(dir.path(), &info), Some(path));
        // Only the model folder is left behind.
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn single_file_model() {
        let dir = tempfile::tempdir().unwrap();
        let body = b"ggml model bytes".to_vec();
        let mut info = info_for(&body);
        info.kind = ModelKind::File;
        info.name = "ggml-test.bin";
        assert_eq!(missing(dir.path(), &[&info]).len(), 1);
        let path = download_from(
            &info,
            &serve_once(body.clone()),
            dir.path(),
            &mut |_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(path, dir.path().join("ggml-test.bin"));
        assert_eq!(std::fs::read(&path).unwrap(), body);
        assert!(missing(dir.path(), &[&info]).is_empty());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn damaged_download_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let body = test_zip("test-model");
        let mut info = info_for(&body);
        info.sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
        let err = download_from(
            &info,
            &serve_once(body),
            dir.path(),
            &mut |_, _| {},
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(err.to_string().contains("checksum"), "{err}");
        assert!(installed(dir.path(), &info).is_none());
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "no leftovers"
        );
    }

    #[test]
    fn free_space_of_missing_path_uses_parent() {
        assert!(free_space(Path::new("/tmp/does/not/exist/yet")).unwrap() > 0);
    }
}
