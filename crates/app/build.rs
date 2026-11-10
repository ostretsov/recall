//! Links against libvosk.so from `vendor/vosk` (scripts/fetch-vosk.sh) or
//! `$VOSK_LIB_DIR`.  Development builds find it at run time through an
//! rpath to that folder; the .deb build sets `RECALL_RPATH=/opt/recall/lib`,
//! where the package bundles it.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=VOSK_LIB_DIR");
    println!("cargo:rerun-if-env-changed=RECALL_RPATH");
    let dir = std::env::var_os("VOSK_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../vendor/vosk"));
    let lib = dir.join("libvosk.so");
    if !lib.exists() {
        panic!(
            "{} not found: run scripts/fetch-vosk.sh (or set VOSK_LIB_DIR)",
            lib.display()
        );
    }
    let dir = dir.canonicalize().expect("vosk dir");
    println!("cargo:rerun-if-changed={}", lib.display());
    println!("cargo:rustc-link-search=native={}", dir.display());
    let rpath = std::env::var("RECALL_RPATH").unwrap_or_else(|_| dir.display().to_string());
    println!("cargo:rustc-link-arg=-Wl,-rpath,{rpath}");
}
