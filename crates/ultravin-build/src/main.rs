//! vpic-import — deterministically extract the vPIC schema, stored procedures,
//! and a manifest from a NHTSA `.plain` dump so upstream changes stay diffable.
//!
//! It splits the dump into one file per object (tables/views/types/functions),
//! groups sequences/constraints, strips volatile `pg_dump` TOC/OID comments (so
//! month-to-month diffs show only real changes), counts `COPY` rows without
//! materializing the data, and writes a `manifest.json` pinning the dump sha256
//! plus per-table row counts. Output is a pure function of the input bytes.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use clap::Parser;
use serde::Serialize;
use sha2::{Digest, Sha256};

mod artifact;
use artifact::ArtifactBuilder;

#[derive(Parser)]
#[command(
    name = "vpic-import",
    about = "Deterministically extract vPIC schema, stored procedures, and manifest from a NHTSA .plain dump"
)]
struct Cli {
    /// Path to the dump: a `vPICList_lite_YYYY_MM.plain.zip` or an unpacked `.sql`.
    #[arg(long)]
    dump: PathBuf,
    /// Month label (YYYY_MM) recorded in the manifest and the source URL.
    #[arg(long)]
    month: String,
    /// Output directory for the committed text (schema/, procs/, manifest.json).
    #[arg(long, default_value = "vpic")]
    out: PathBuf,
    /// Path for the embedded rkyv artifact (a gitignored build product).
    #[arg(long, default_value = "crates/ultravin-core/data/vpic.rkyv")]
    emit_artifact: PathBuf,
}

#[derive(Serialize)]
struct Manifest {
    month: String,
    source_url: String,
    dump_file: String,
    dump_sha256: String,
    builder_version: String,
    table_count: usize,
    function_count: usize,
    total_rows: u64,
    tables: BTreeMap<String, u64>,
    functions: Vec<String>,
    artifact_blake3: String,
    artifact_bytes: usize,
}

/// Parse a `pg_dump` TOC header: `-- [Data for ]Name: <n>; Type: <t>; Schema:..`.
fn parse_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("-- ")?;
    let rest = rest.strip_prefix("Data for ").unwrap_or(rest);
    let rest = rest.strip_prefix("Name: ")?;
    let (name, after) = rest.split_once("; Type: ")?;
    let (typ, _) = after.split_once(';')?;
    Some((name.trim().to_string(), typ.trim().to_string()))
}

/// Volatile or structural comment lines we drop for clean diffs.
fn is_noise(line: &str) -> bool {
    line == "--"
        || line.starts_with("-- TOC entry")
        || line.starts_with("-- Dependencies")
        || line.starts_with("-- Dumped ")
}

/// Object name -> filesystem-safe basename (strip function args, quotes, case).
fn obj_basename(name: &str) -> String {
    let base = name.split('(').next().unwrap_or(name);
    base.trim()
        .trim_matches('"')
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .to_ascii_lowercase()
}

/// Table name from a `COPY vpic.<table> (...) FROM stdin;` line.
fn copy_table(line: &str) -> Option<String> {
    let rest = line.strip_prefix("COPY ")?;
    let tok = rest.split([' ', '(']).next()?;
    Some(tok.trim().trim_start_matches("vpic.").to_string())
}

fn sha256_file(path: &Path) -> Result<String, Box<dyn Error>> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Trim leading/trailing blank lines and return `None` if nothing remains.
fn trim_block(lines: &[String]) -> Option<String> {
    let start = lines.iter().position(|l| !l.trim().is_empty())?;
    let end = lines.iter().rposition(|l| !l.trim().is_empty())?;
    Some(lines[start..=end].join("\n"))
}

fn write_file(path: &Path, body: &str) -> Result<(), Box<dyn Error>> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut f = File::create(path)?;
    f.write_all(body.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

struct Importer {
    out: PathBuf,
    tables: BTreeMap<String, u64>,
    functions: BTreeSet<String>,
    sequences: Vec<String>,
    constraints: Vec<String>,
    misc: Vec<String>,
}

impl Importer {
    /// Route a finished object's body to the right committed file/bucket.
    fn finalize(&mut self, typ: &str, base: &str, body: &[String]) -> Result<(), Box<dyn Error>> {
        let Some(text) = trim_block(body) else {
            return Ok(());
        };
        let upper = typ.to_ascii_uppercase();
        if upper.starts_with("FUNCTION") || upper.starts_with("PROCEDURE") {
            self.functions.insert(base.to_string());
            write_file(&self.out.join("procs").join(format!("{base}.sql")), &text)?;
        } else if upper == "TABLE" {
            write_file(
                &self.out.join("schema/tables").join(format!("{base}.sql")),
                &text,
            )?;
        } else if upper == "VIEW" {
            write_file(
                &self.out.join("schema/views").join(format!("{base}.sql")),
                &text,
            )?;
        } else if upper == "TYPE" {
            write_file(
                &self.out.join("schema/types").join(format!("{base}.sql")),
                &text,
            )?;
        } else if upper == "TABLE DATA" {
            // data handled by the COPY counter; nothing to write
        } else if upper.contains("SEQUENCE") || upper == "DEFAULT" {
            self.sequences.push(text);
        } else if upper.contains("CONSTRAINT") || upper == "INDEX" || upper == "TRIGGER" {
            self.constraints.push(text);
        } else {
            self.misc.push(text);
        }
        Ok(())
    }
}

fn run(cli: &Cli) -> Result<(), Box<dyn Error>> {
    let dump_sha256 = sha256_file(&cli.dump)?;

    // Obtain a line reader over the SQL and the logical dump file name.
    let is_zip = cli.dump.extension().and_then(|e| e.to_str()) == Some("zip");
    let (reader, dump_file): (Box<dyn BufRead>, String) = if is_zip {
        let mut archive = zip::ZipArchive::new(File::open(&cli.dump)?)?;
        let entry = archive.by_index(0)?;
        let name = entry.name().to_string();
        // Read the entry fully (the SQL part is ~320MB; the zip reader is not Send).
        let mut buf = Vec::new();
        let mut e = entry;
        e.read_to_end(&mut buf)?;
        (Box::new(BufReader::new(std::io::Cursor::new(buf))), name)
    } else {
        let name = cli
            .dump
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("dump.sql")
            .to_string();
        (Box::new(BufReader::new(File::open(&cli.dump)?)), name)
    };

    // Clear prior output so deleted objects don't linger.
    for sub in ["schema", "procs"] {
        let p = cli.out.join(sub);
        if p.exists() {
            fs::remove_dir_all(&p)?;
        }
    }

    let mut imp = Importer {
        out: cli.out.clone(),
        tables: BTreeMap::new(),
        functions: BTreeSet::new(),
        sequences: Vec::new(),
        constraints: Vec::new(),
        misc: Vec::new(),
    };
    let mut preamble: Vec<String> = Vec::new();
    let mut cur: Option<(String, String)> = None; // (type, basename)
    let mut body: Vec<String> = Vec::new();
    let mut data_mode: Option<(String, u64)> = None; // (table, row count)
    let mut builder = ArtifactBuilder::default();

    for line in reader.lines() {
        let line = line?;
        if let Some((table, count)) = data_mode.as_mut() {
            if line == "\\." {
                imp.tables.insert(table.clone(), *count);
                builder.end_copy();
                data_mode = None;
            } else {
                *count += 1;
                builder.feed(&line);
            }
            continue;
        }
        if line.starts_with("COPY ") && line.trim_end().ends_with("FROM stdin;") {
            if let Some(table) = copy_table(&line) {
                builder.begin_copy(&table, &line);
                data_mode = Some((table, 0));
            }
            continue;
        }
        if let Some((name, typ)) = parse_header(&line) {
            if let Some((ptyp, pbase)) = cur.take() {
                imp.finalize(&ptyp, &pbase, &body)?;
            }
            cur = Some((typ, obj_basename(&name)));
            body = Vec::new();
            continue;
        }
        if is_noise(&line) {
            continue;
        }
        if cur.is_some() {
            body.push(line);
        } else {
            preamble.push(line);
        }
    }
    if let Some((ptyp, pbase)) = cur.take() {
        imp.finalize(&ptyp, &pbase, &body)?;
    }

    // Grouped/preamble files.
    if let Some(text) = trim_block(&preamble) {
        write_file(&cli.out.join("schema/_preamble.sql"), &text)?;
    }
    if !imp.sequences.is_empty() {
        write_file(
            &cli.out.join("schema/sequences.sql"),
            &imp.sequences.join("\n\n"),
        )?;
    }
    if !imp.constraints.is_empty() {
        write_file(
            &cli.out.join("schema/constraints.sql"),
            &imp.constraints.join("\n\n"),
        )?;
    }
    if !imp.misc.is_empty() {
        write_file(&cli.out.join("schema/_misc.sql"), &imp.misc.join("\n\n"))?;
    }

    // Build the embedded artifact (deterministic content-addressed product).
    let builder_version: u32 = env!("CARGO_PKG_VERSION")
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let (artifact_bytes, artifact_blake3) =
        artifact::write_artifact(builder, &cli.emit_artifact, builder_version)?;

    let total_rows: u64 = imp.tables.values().sum();
    let manifest = Manifest {
        month: cli.month.clone(),
        source_url: format!(
            "https://vpic.nhtsa.dot.gov/Downloads/vPICList_lite_{}.plain.zip",
            cli.month
        ),
        dump_file,
        dump_sha256,
        builder_version: env!("CARGO_PKG_VERSION").to_string(),
        table_count: imp.tables.len(),
        function_count: imp.functions.len(),
        total_rows,
        tables: imp.tables.clone(),
        functions: imp.functions.iter().cloned().collect(),
        artifact_blake3,
        artifact_bytes,
    };
    write_file(
        &cli.out.join("manifest.json"),
        &serde_json::to_string_pretty(&manifest)?,
    )?;

    eprintln!(
        "vpic-import: {} tables ({} rows), {} functions; sha256={}",
        manifest.table_count, manifest.total_rows, manifest.function_count, manifest.dump_sha256
    );
    Ok(())
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("vpic-import error: {e}");
        std::process::exit(1);
    }
}
