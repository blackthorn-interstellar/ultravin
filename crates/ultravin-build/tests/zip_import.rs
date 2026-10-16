use std::fs;
use std::io::{Cursor, Write};
use std::path::Path;
use std::process::{Command, Output};

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

fn import(dump: &Path, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vpic-import"))
        .arg("--dump")
        .arg(dump)
        .args(["--month", "2026_10", "--out"])
        .arg(out)
        .arg("--emit-artifact")
        .arg(out.join("data/vpic.rkyv"))
        .output()
        .unwrap()
}

#[test]
fn stored_and_deflated_dumps_match_plain_sql_and_keep_first_entry_name() {
    let root = std::env::temp_dir().join(format!("ultravin-zip-roundtrip-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let sql =
        b"SET search_path TO vpic;\nCOPY vpic.country (id, name) FROM stdin;\n1\tFrance\n\\.\n";
    let name = "données.sql";
    let plain = root.join(name);
    fs::write(&plain, sql).unwrap();
    let baseline = root.join("plain");
    let result = import(&plain, &baseline);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let expected: serde_json::Value =
        serde_json::from_slice(&fs::read(baseline.join("manifest.json")).unwrap()).unwrap();
    for (label, method) in [
        ("stored", CompressionMethod::Stored),
        ("deflated", CompressionMethod::Deflated),
    ] {
        let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
        zip.start_file(name, opts).unwrap();
        zip.write_all(sql).unwrap();
        zip.start_file("ignored.sql", opts).unwrap();
        zip.write_all(b"COPY vpic.wrong (id) FROM stdin;\n1\n\\.\n")
            .unwrap();
        let dump = root.join(format!("{label}.zip"));
        fs::write(&dump, zip.finish().unwrap().into_inner()).unwrap();
        let out = root.join(label);
        let result = import(&dump, &out);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("manifest.json")).unwrap()).unwrap();
        for key in [
            "dump_file",
            "tables",
            "total_rows",
            "artifact_blake3",
            "artifact_bytes",
        ] {
            assert_eq!(manifest[key], expected[key], "{label}: {key}");
        }
        assert_eq!(
            fs::read(out.join("data/vpic.rkyv")).unwrap(),
            fs::read(baseline.join("data/vpic.rkyv")).unwrap()
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn invalid_and_empty_archives_fail_before_writing_output() {
    let root = std::env::temp_dir().join(format!("ultravin-zip-invalid-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let empty = ZipWriter::new(Cursor::new(Vec::new()))
        .finish()
        .unwrap()
        .into_inner();
    for (label, bytes) in [("invalid", b"not a zip".to_vec()), ("empty", empty)] {
        let dump = root.join(format!("{label}.zip"));
        fs::write(&dump, bytes).unwrap();
        let out = root.join(label);
        let result = import(&dump, &out);
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("vpic-import error:"));
        assert!(!out.exists());
    }
    fs::remove_dir_all(root).unwrap();
}
