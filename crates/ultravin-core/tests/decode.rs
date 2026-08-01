//! Integration tests for the W1 decode against the embedded artifact, plus an
//! artifact integrity (determinism) assertion. These are skipped gracefully when
//! only the empty placeholder artifact is present (no dump imported yet).

use ultravin_core::{decode, Db};

fn loaded() -> bool {
    Db::try_embedded().is_some()
}

fn value(vin: &str, element_id: i32) -> Option<String> {
    decode(vin, None)
        .elements
        .into_iter()
        .find(|e| e.element_id == element_id)
        .map(|e| e.value)
}

#[test]
fn canonical_honda() {
    if !loaded() {
        eprintln!("skip: artifact not built");
        return;
    }
    let r = decode("1HGCM82633A004352", None);
    let make = r.elements.iter().find(|e| e.element_id == 26).unwrap();
    assert_eq!(make.value, "HONDA");
    assert_eq!(make.source, "pattern - model");
    assert_eq!(value("1HGCM82633A004352", 28).as_deref(), Some("Accord"));
    assert_eq!(r.model_year, Some(2003));
    assert_eq!(value("1HGCM82633A004352", 18).as_deref(), Some("J30A4"));
    assert_eq!(
        value("1HGCM82633A004352", 39).as_deref(),
        Some("PASSENGER CAR")
    );
    assert_eq!(r.error_codes, vec![0]);
}

#[test]
fn smoke_error_codes() {
    if !loaded() {
        return;
    }
    let cases: &[(&str, &[i32])] = &[
        ("1HGCM82633A004352", &[0]),
        ("1FTFW1ET5DFC10312", &[1]),
        ("5UXWX7C5XBA123456", &[1]),
        ("3C6TRVAG6JE100000", &[1]),
        ("SAL00000000000000", &[1, 8, 11, 400]),
        ("ZZZCM82633A004352", &[1, 7]),
    ];
    for (vin, codes) in cases {
        assert_eq!(&decode(vin, None).error_codes, codes, "codes for {vin}");
    }
}

#[test]
fn unknown_wmi_short_circuits() {
    if !loaded() {
        return;
    }
    let r = decode("ZZZCM82633A004352", None);
    // Only the 6 Corrections pseudo-elements survive an err-7 short circuit.
    assert!(r.elements.iter().all(|e| e.source == "Corrections"));
    assert_eq!(
        value("ZZZCM82633A004352", 196).as_deref(),
        Some("ZZZCM826*3A")
    );
    assert_eq!(value("ZZZCM82633A004352", 142).as_deref(), Some(""));
}

#[test]
fn single_wmi_make_fallback() {
    if !loaded() {
        return;
    }
    // No Model -> Make resolves via the single-WMI fallback (source "Make").
    let r = decode("SAL00000000000000", None);
    let make = r.elements.iter().find(|e| e.element_id == 26).unwrap();
    assert_eq!(make.value, "LAND ROVER");
    assert_eq!(make.source, "Make");
    assert!(r.elements.iter().all(|e| e.element_id != 28));
}

#[test]
fn artifact_blake3_matches_manifest() {
    // Determinism / integrity: the embedded artifact's header digest equals the
    // value recorded in the committed manifest (when a real artifact is present).
    if !loaded() {
        return;
    }
    let art = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/data/vpic.rkyv")).unwrap();
    let hex = ultravin_core::tables::artifact_blake3_hex(&art);
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vpic/manifest.json"
    ))
    .unwrap();
    assert!(
        manifest.contains(&hex),
        "artifact blake3 {hex} not found in manifest"
    );
}

#[test]
fn embedded_loader_is_consistent() {
    // The embedded bytes loaded twice decode identically (loader is pure).
    if !loaded() {
        return;
    }
    let art = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/data/vpic.rkyv")).unwrap();
    let db = Db::from_bytes(&art).unwrap();
    let a = ultravin_core::decode_with(
        db_ref(&db),
        "1HGCM82633A004352",
        1_750_000_000_000_000,
        2026,
    );
    let b = decode("1HGCM82633A004352", None);
    let map = |r: &ultravin_core::DecodeResult<'_>| {
        let mut v: Vec<_> = r
            .elements
            .iter()
            .map(|e| (e.element_id, e.value.clone(), e.source.to_string()))
            .collect();
        v.sort();
        v
    };
    assert_eq!(map(&a), map(&b));
}

fn db_ref(db: &Db) -> &Db {
    db
}

// Caller-year expectations below are oracle-verified against
// `vpic.spvindecode(vin, false, year)` on the pinned 2026_07 dump, and use only
// years whose in/out-of-window status cannot change as `current_year` grows
// (the window is [1980, current_year + 2]).

#[test]
fn caller_year_matching_the_vin_changes_nothing() {
    if !loaded() {
        return;
    }
    assert_eq!(
        decode("1HGCM82633A004352", Some(2003)),
        decode("1HGCM82633A004352", None)
    );
}

#[test]
fn caller_year_pass_can_win_best_of() {
    if !loaded() {
        return;
    }
    // 1995 differs from the VIN-implied 2003, so it runs as its own pass and
    // the +10000 model-year bonus carries it through the best-pass scoring.
    let r = decode("1HGCM82633A004352", Some(1995));
    assert_eq!(r.model_year, Some(1995));
    assert_eq!(r.error_codes, vec![3, 12, 14]);
}

#[test]
fn out_of_window_caller_year_still_flags_error_12() {
    if !loaded() {
        return;
    }
    // 1979 is below the [1980, current_year + 2] window: no caller-year pass,
    // but the proc still compares it against the decoded year for error 12.
    let r = decode("1HGCM82633A004352", Some(1979));
    assert_eq!(r.model_year, Some(2003));
    assert_eq!(r.error_codes, vec![0, 12]);
}

#[test]
fn batch_years_thread_per_vin() {
    if !loaded() {
        return;
    }
    let vins = ["1HGCM82633A004352", "1HGCM82633A004352"].map(String::from);
    let years = [None, Some(1995)];
    let batch = ultravin_core::decode_batch(&vins, Some(&years));
    assert_eq!(batch[0], decode(&vins[0], None));
    assert_eq!(batch[1], decode(&vins[1], Some(1995)));
    // No years slice decodes exactly like all-None.
    assert_eq!(ultravin_core::decode_batch(&vins, None)[1], batch[0]);
}
