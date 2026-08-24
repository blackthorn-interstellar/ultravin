//! Pick the artifact `include_bytes!` bakes into the crate.
//!
//! The real artifact (`vpic.rkyv`, ~82 MB) is a build product: a pure function
//! of the pinned NHTSA dump, emitted by `vpic-import` and attached to every
//! GitHub release. It never lives in git or in the crates.io package, so this
//! script resolves it at build time, in order:
//!
//! 1. `ULTRAVIN_DATA` — an absolute path to a `vpic.rkyv` (the crates.io
//!    consumer's route). Fully validated here so a truncated or corrupt file
//!    fails the build instead of becoming a bad `include_bytes!`.
//! 2. `data/vpic.rkyv` beside this manifest (the repo checkout's route; `make
//!    data` and the CI wheel jobs put it there).
//! 3. Otherwise a tiny but valid *empty* artifact written to `OUT_DIR`, so a fresh
//!    checkout, docs.rs and `cargo publish --verify` all compile. `Db::embedded()`
//!    refuses to serve it at runtime; `Db::try_embedded()` reports `None`.
//!
//! The chosen path reaches `src/db.rs` through the `ULTRAVIN_ARTIFACT` env var.
//! Nothing here ever parses the 320 MB dump.

#[path = "src/tables.rs"]
#[allow(dead_code)]
mod tables;

use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=ULTRAVIN_DATA");
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let data_dir = manifest.join("data");
    // Watching the directory (not the file) means the artifact *appearing* after
    // a placeholder build triggers a rebuild too. The dir is always present in a
    // checkout (data/.gitkeep) and in the package; a missing one has nothing to
    // watch.
    if data_dir.is_dir() {
        println!("cargo:rerun-if-changed={}", data_dir.display());
    }

    let artifact = match std::env::var_os("ULTRAVIN_DATA").filter(|v| !v.is_empty()) {
        Some(p) => {
            let p = PathBuf::from(p);
            assert!(
                p.is_absolute(),
                "ULTRAVIN_DATA must be an absolute path (build scripts do not run in your \
                 shell's directory), got {}",
                p.display()
            );
            println!("cargo:rerun-if-changed={}", p.display());
            validate(&p);
            p
        }
        None => {
            let local = data_dir.join("vpic.rkyv");
            if local.is_file() {
                local
            } else {
                placeholder(&out_dir)
            }
        }
    };
    println!("cargo:rustc-env=ULTRAVIN_ARTIFACT={}", artifact.display());
}

/// Refuse a caller-supplied artifact that is not a fully valid one. The embedded
/// blob is loaded *unchecked* at runtime (that is what keeps cold start cheap),
/// so this build-time pass is the only validation such a file ever gets; the
/// proofs are the same three `Db::from_bytes` runs (see `tables::validate_body`).
fn validate(path: &Path) {
    let bytes = std::fs::read(path)
        .unwrap_or_else(|e| panic!("ULTRAVIN_DATA: cannot read {}: {e}", path.display()));
    let check = || -> Result<(), String> {
        tables::check_header(&bytes)?;
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes[tables::HEADER_LEN..]);
        let archived = tables::validate_body(&aligned)?;
        if archived.wmi.is_empty() {
            return Err("artifact is the empty placeholder, not real vPIC data".into());
        }
        Ok(())
    };
    if let Err(e) = check() {
        panic!(
            "ULTRAVIN_DATA={} is not a usable ultravin artifact: {e}. Download vpic.rkyv \
             from the GitHub release matching this crate version and verify its blake3 \
             against that tag's vpic/manifest.json.",
            path.display()
        );
    }
}

/// Write the empty placeholder artifact into `OUT_DIR` and return its path.
fn placeholder(out_dir: &Path) -> PathBuf {
    // Cargo hides this for non-path dependencies; Db::embedded()'s runtime
    // refusal is the real guard. This is the compile-time heads-up.
    println!(
        "cargo:warning=ultravin: no vpic.rkyv (ULTRAVIN_DATA unset, data/vpic.rkyv \
         absent) — embedding an EMPTY placeholder artifact. Decoding will refuse at \
         runtime; point ULTRAVIN_DATA at a vpic.rkyv from a GitHub release, or in the \
         repo run `make data`."
    );
    let empty = tables::VpicData {
        cover: Vec::new(),
        arena_bytes: vec![0],
        arena_offsets: vec![0, 0],
        wmi: Vec::new(),
        wmi_vinschema: Vec::new(),
        vinschema: Vec::new(),
        pattern: Vec::new(),
        element: Vec::new(),
        make_model: Vec::new(),
        wmi_make: Vec::new(),
        enginemodel: Vec::new(),
        enginemodelpattern: Vec::new(),
        defaultvalue: Vec::new(),
        vinexception: Vec::new(),
        conversion: Vec::new(),
        lookups: Vec::new(),
        vspecschema: Vec::new(),
        vspecschemapattern: Vec::new(),
        vspecpattern: Vec::new(),
        vspecschemamodel: Vec::new(),
        vspecschemayear: Vec::new(),
    };
    let path = out_dir.join("vpic.rkyv");
    std::fs::write(&path, tables::serialize_artifact(&empty, 0)).unwrap();
    path
}
