//! Pick the artifact `include_bytes!` bakes into the crate.
//!
//! The real artifact (`vpic.rkyv`, ~83 MB) is a build product: a pure function
//! of the pinned NHTSA dump, emitted by `vpic-import` and attached to every
//! GitHub release. It never lives in git or in the crates.io package, so this
//! script resolves it at build time, in order:
//!
//! 1. `ULTRAVIN_DATA` — an absolute path to a `vpic.rkyv`. Fully validated here
//!    so a truncated or corrupt file fails the build instead of becoming a bad
//!    `include_bytes!`.
//! 2. `data/vpic.rkyv` beside this manifest (the repo checkout's route; `make
//!    data` and the CI wheel jobs put it there).
//! 3. With the default `download-data` feature, on a released version: this
//!    version's GitHub release asset, fetched once into `~/.cache/ultravin` (or
//!    `ULTRAVIN_CACHE_DIR`), checked against the blake3 pinned in
//!    `data/manifest.json`, then validated like route 1. A dev build (version
//!    0.0.0) and docs.rs never download.
//! 4. Otherwise a tiny but valid *empty* artifact written to `OUT_DIR`, so a fresh
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
    println!("cargo:rerun-if-env-changed=ULTRAVIN_CACHE_DIR");
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let data_dir = manifest_dir.join("data");
    // Watching the directory (not the file) means the artifact *appearing* after
    // a placeholder build triggers a rebuild too. The dir is always present in a
    // checkout (data/.gitkeep) and in the package; a missing one has nothing to
    // watch.
    if data_dir.is_dir() {
        println!("cargo:rerun-if-changed={}", data_dir.display());
    }

    let artifact = match env_path("ULTRAVIN_DATA") {
        Some(p) => {
            assert!(
                p.is_absolute(),
                "ULTRAVIN_DATA must be an absolute path (build scripts do not run in your \
                 shell's directory), got {}",
                p.display()
            );
            println!("cargo:rerun-if-changed={}", p.display());
            validate(&p, "ULTRAVIN_DATA");
            p
        }
        None => {
            let local = data_dir.join("vpic.rkyv");
            if local.is_file() {
                local
            } else {
                download::fetch(&data_dir, &out_dir).unwrap_or_else(|| placeholder(&out_dir))
            }
        }
    };
    println!("cargo:rustc-env=ULTRAVIN_ARTIFACT={}", artifact.display());
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn read(path: &Path, what: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("{what}: cannot read {}: {e}", path.display()))
}

/// Refuse an artifact that is not a fully valid one. The embedded blob is loaded
/// *unchecked* at runtime (that is what keeps cold start cheap), so this
/// build-time pass is the only validation a file from outside the checkout ever
/// gets; the proofs are the same three `Db::from_bytes` runs
/// (`tables::validate_body`).
fn validate(path: &Path, what: &str) {
    if let Err(e) = validate_bytes(&read(path, what)) {
        panic!(
            "{what}: {} is not a usable ultravin artifact: {e}. Download vpic.rkyv from \
             the GitHub release matching this crate version and verify its blake3 against \
             that tag's vpic/manifest.json.",
            path.display()
        );
    }
}

fn validate_bytes(bytes: &[u8]) -> Result<(), String> {
    tables::check_header(bytes)?;
    let mut aligned = rkyv::util::AlignedVec::<16>::new();
    aligned.extend_from_slice(&bytes[tables::HEADER_LEN..]);
    let archived = tables::validate_body(&aligned)?;
    if archived.wmi.is_empty() {
        return Err("artifact is the empty placeholder, not real vPIC data".into());
    }
    Ok(())
}

/// Write the empty placeholder artifact into `OUT_DIR` and return its path.
fn placeholder(out_dir: &Path) -> PathBuf {
    // Cargo hides this for non-path dependencies; Db::embedded()'s runtime
    // refusal is the real guard. This is the compile-time heads-up.
    println!(
        "cargo:warning=ultravin: no vpic.rkyv (ULTRAVIN_DATA unset, data/vpic.rkyv absent, \
         nothing to download) — embedding an EMPTY placeholder artifact. Decoding will \
         refuse at runtime. In the repo run `make data`; as a dependency, build a released \
         version with the download-data feature or set ULTRAVIN_DATA."
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

/// Route 3: this version's release asset, cached per machine.
#[cfg(feature = "download-data")]
mod download {
    use super::{env_path, read, validate_bytes, Path, PathBuf};

    /// Hard ceiling on the download; the artifact is ~83 MB, so anything near
    /// this is not the file we asked for.
    const MAX_BYTES: u64 = 512 * 1024 * 1024;

    /// The artifact for this crate version, from the cache or the GitHub release.
    /// `None` when this build must not download: a dev checkout (0.0.0) or docs.rs.
    pub fn fetch(data_dir: &Path, out_dir: &Path) -> Option<PathBuf> {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap();
        if version == "0.0.0" || std::env::var_os("DOCS_RS").is_some() {
            return None;
        }
        let want = pinned_blake3(&data_dir.join("manifest.json"));
        let dir = cache_dir().unwrap_or_else(|| out_dir.to_path_buf());
        let path = dir.join(format!("vpic-{want}.rkyv"));
        // A deleted cache file must re-run this script, not break include_bytes!.
        println!("cargo:rerun-if-changed={}", path.display());
        if path.is_file() {
            if artifact_blake3(&read(&path, "cached artifact")) == want {
                return Some(path);
            }
            println!(
                "cargo:warning=ultravin: cached {} failed its blake3 check; re-downloading",
                path.display()
            );
        }

        let url = format!(
            "{}/releases/download/v{version}/vpic.rkyv",
            std::env::var("CARGO_PKG_REPOSITORY").unwrap()
        );
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("ultravin: cannot create cache dir {}: {e}", dir.display()));
        let tmp = dir.join(format!("vpic-{want}.rkyv.part-{}", std::process::id()));
        if let Err(e) = download(&url, &tmp) {
            let _ = std::fs::remove_file(&tmp);
            panic!(
                "ultravin: could not download the vPIC data artifact from {url}: {e}\n\
                 The crate embeds this ~83 MB file once per machine (cache: {}). Either fix \
                 network access and rebuild, download it yourself and set \
                 ULTRAVIN_DATA=/abs/path/vpic.rkyv, or build with --no-default-features \
                 and load it at runtime via Db::open (feature external-data).",
                dir.display()
            );
        }
        let bytes = read(&tmp, "downloaded artifact");
        let got = artifact_blake3(&bytes);
        if got != want {
            let _ = std::fs::remove_file(&tmp);
            panic!(
                "ultravin: {url} has blake3 {got} but this crate version pins {want}; \
                 refusing to embed it"
            );
        }
        if let Err(e) = validate_bytes(&bytes) {
            let _ = std::fs::remove_file(&tmp);
            panic!("ultravin: {url} is not a usable artifact: {e}");
        }
        std::fs::rename(&tmp, &path)
            .unwrap_or_else(|e| panic!("ultravin: cannot move {} into place: {e}", tmp.display()));
        Some(path)
    }

    /// `ULTRAVIN_CACHE_DIR`, else the platform's user cache directory, else
    /// `None` (the caller falls back to `OUT_DIR`, which persists only as long
    /// as the target dir does).
    fn cache_dir() -> Option<PathBuf> {
        if let Some(d) = env_path("ULTRAVIN_CACHE_DIR") {
            return Some(d);
        }
        if let Some(x) = env_path("XDG_CACHE_HOME") {
            return Some(x.join("ultravin"));
        }
        if let Some(h) = env_path("HOME") {
            return Some(h.join(".cache").join("ultravin"));
        }
        env_path("LOCALAPPDATA").map(|l| l.join("ultravin"))
    }

    /// `artifact_blake3` from the crate's copy of the data manifest. A missing
    /// or malformed copy is a packaging bug, not a user error.
    fn pinned_blake3(manifest: &Path) -> String {
        let text = String::from_utf8(read(manifest, "data manifest")).unwrap();
        let key = "\"artifact_blake3\"";
        let hex = text
            .find(key)
            .and_then(|i| {
                let rest = &text[i + key.len()..];
                let start = rest.find('"')? + 1;
                rest.get(start..start + 64)
            })
            .filter(|h| h.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or_else(|| {
                panic!(
                    "ultravin: {} has no artifact_blake3 (packaging bug)",
                    manifest.display()
                )
            });
        hex.to_ascii_lowercase()
    }

    /// The manifest's hash scheme: blake3 over the rkyv body followed by the
    /// header's builder-version bytes — see `tables::serialize_artifact`.
    fn artifact_blake3(bytes: &[u8]) -> String {
        if bytes.len() < super::tables::HEADER_LEN {
            return String::new();
        }
        let mut h = blake3::Hasher::new();
        h.update(&bytes[super::tables::HEADER_LEN..]);
        h.update(&bytes[10..14]);
        h.finalize().to_hex().to_string()
    }

    fn download(url: &str, to: &Path) -> Result<(), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(std::time::Duration::from_secs(30)))
            .user_agent(concat!(
                "ultravin/",
                env!("CARGO_PKG_VERSION"),
                " (build script)"
            ))
            .build()
            .into();
        let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
        let mut body = resp.body_mut().with_config().limit(MAX_BYTES).reader();
        let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
        std::io::copy(&mut body, &mut file).map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(not(feature = "download-data"))]
mod download {
    use super::{Path, PathBuf};

    pub fn fetch(_data_dir: &Path, _out_dir: &Path) -> Option<PathBuf> {
        None
    }
}
