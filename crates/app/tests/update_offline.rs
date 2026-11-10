//! The update check and download against a local HTTP server (no internet).

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;

use recall_core::update::{self, Version};
use sha2::{Digest, Sha256};

/// Serves `routes` (path → body) on 127.0.0.1 until the test ends; the base URL.
fn serve(routes: Vec<(String, Vec<u8>)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut first = String::new();
            reader.read_line(&mut first).unwrap();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
            }
            let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
            let body = routes.iter().find(|(p, _)| *p == path).map(|(_, b)| b);
            let (status, body) = match body {
                Some(b) => ("200 OK", b.clone()),
                None => ("404 Not Found", b"{\"message\":\"Not Found\"}".to_vec()),
            };
            let _ = write!(
                stream,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    base
}

fn release_json(base: &str, deb: &[u8], digest: &str) -> String {
    format!(
        r#"{{"tag_name":"v9.1.0","html_url":"https://example.org/v9.1.0","draft":false,"prerelease":false,
            "assets":[{{"name":"recall_9.1.0_amd64.deb","size":{},"digest":"sha256:{digest}",
                        "browser_download_url":"{base}/recall_9.1.0_amd64.deb"}}]}}"#,
        deb.len()
    )
}

fn hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn finds_downloads_and_verifies_a_newer_release() {
    let deb: Vec<u8> = (0..700_000u32).map(|i| (i % 251) as u8).collect();
    // The download URL must be https (parse_release refuses others), so the
    // release JSON is checked here and the download goes to the local server.
    let base = serve(vec![
        (
            "/latest".into(),
            release_json("https://x", &deb, &hex(&deb)).into_bytes(),
        ),
        ("/recall_9.1.0_amd64.deb".into(), deb.clone()),
    ]);
    let mut r = recall::update::fetch_newer_from(&format!("{base}/latest"), "0.1.0")
        .unwrap()
        .expect("9.1.0 is newer");
    assert_eq!(r.version, Version(9, 1, 0));
    assert_eq!(r.deb_size, deb.len() as u64);
    assert_eq!(r.sha256.as_deref(), Some(hex(&deb).as_str()));

    // Same version: nothing to offer.
    assert!(
        recall::update::fetch_newer_from(&format!("{base}/latest"), "9.1.0")
            .unwrap()
            .is_none()
    );

    r.deb_url = format!("{base}/recall_9.1.0_amd64.deb");
    let dir = tempfile::tempdir().unwrap();
    let mut progress = Vec::new();
    let path = recall::update::download(&r, dir.path(), |n| progress.push(n)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), deb);
    assert_eq!(progress.last().copied(), Some(deb.len() as u64));
    assert!(progress.windows(2).all(|w| w[0] < w[1]));

    // A damaged download is refused.
    r.sha256 = Some("0".repeat(64));
    let err = recall::update::download(&r, dir.path(), |_| {}).unwrap_err();
    assert!(format!("{err:#}").contains("SHA-256"), "{err:#}");

    // A short download too.
    r.sha256 = None;
    r.deb_size += 1;
    let err = recall::update::download(&r, dir.path(), |_| {}).unwrap_err();
    assert!(format!("{err:#}").contains("incomplete"), "{err:#}");
}

#[test]
fn unreachable_or_missing_release_is_an_error() {
    let base = serve(vec![]);
    let err = recall::update::fetch_newer_from(&format!("{base}/latest"), "0.1.0").unwrap_err();
    assert!(format!("{err:#}").contains("404"), "{err:#}");
}

#[test]
fn restart_script_waits_for_the_process() {
    // The script polls `kill -0 PID`; with a finished PID it goes straight on.
    let child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    let mut child = child;
    child.wait().unwrap();
    let script = update::restart_script(pid).replace("exec /usr/bin/recall", "echo started");
    let out = std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .arg("recall")
        .arg("--config-dir")
        .arg("/x")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "started --config-dir /x\n"
    );
}
