//! Independently verify a built vPIC artifact against its pinned BLAKE3 digest.

use std::env;
use std::fs;
use std::process::ExitCode;

use ultravin_core::tables::{FORMAT_VERSION, HEADER_LEN, MAGIC};

fn run() -> Result<String, String> {
    let mut args = env::args().skip(1);
    let path = args
        .next()
        .ok_or_else(|| "usage: verify-artifact <artifact> <expected-blake3>".to_string())?;
    let expected = args
        .next()
        .ok_or_else(|| "usage: verify-artifact <artifact> <expected-blake3>".to_string())?
        .to_ascii_lowercase();
    if args.next().is_some() {
        return Err("usage: verify-artifact <artifact> <expected-blake3>".to_string());
    }

    let data = fs::read(&path).map_err(|error| format!("cannot read {path}: {error}"))?;
    if data.len() < HEADER_LEN {
        return Err(format!("{path} is too small to contain an artifact header"));
    }
    if data[..MAGIC.len()] != MAGIC {
        return Err(format!("{path} has invalid artifact magic"));
    }
    let format = u16::from_le_bytes([data[8], data[9]]);
    if format != FORMAT_VERSION {
        return Err(format!(
            "{path} format {format} != {FORMAT_VERSION}; update the verifier for the new format"
        ));
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(&data[HEADER_LEN..]);
    hasher.update(&data[10..14]);
    let actual = hasher.finalize().to_hex().to_string();
    if actual != expected {
        return Err(format!(
            "{path} BLAKE3 mismatch: got {actual}, expected {expected}"
        ));
    }
    if data[14..46] != *hasher.finalize().as_bytes() {
        return Err(format!(
            "{path} header does not contain its computed BLAKE3"
        ));
    }

    Ok(actual)
}

fn main() -> ExitCode {
    match run() {
        Ok(actual) => {
            println!("artifact verified (independent re-hash): {actual}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("verify-artifact: {error}");
            ExitCode::FAILURE
        }
    }
}
