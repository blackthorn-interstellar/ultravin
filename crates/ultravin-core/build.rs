//! Ensure the embedded artifact exists before the crate is compiled.
//!
//! The real artifact (`data/vpic.rkyv`) is a build product, materialized by
//! `vpic-import --emit-artifact` (a pure function of the pinned dump). To let a
//! fresh checkout compile before the importer has run, this script writes a
//! tiny but valid *empty* artifact when one is absent. It never parses the
//! 320MB dump at build time.

#[path = "src/tables.rs"]
#[allow(dead_code)]
mod tables;

use std::path::Path;

fn main() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let path = Path::new(&manifest).join("data/vpic.rkyv");
    println!("cargo:rerun-if-changed=data/vpic.rkyv");

    if !path.exists() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let empty = tables::VpicData {
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
        let bytes = tables::serialize_artifact(&empty, 0);
        std::fs::write(&path, bytes).unwrap();
    }
}
