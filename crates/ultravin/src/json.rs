//! Full JSON output directly from resolved items. Constant metadata is escaped
//! once per database; each decode writes its variable fields into one buffer.

use std::borrow::Cow;

use crate::{public_decode, tables::ArchivedElement, Db, RawResult};

pub(crate) struct ElementJson {
    before_value: Vec<u8>,
    before_attribute: Vec<u8>,
    before_source: Vec<u8>,
    default: std::sync::OnceLock<(i64, Vec<u8>)>,
}

fn value(out: &mut Vec<u8>, value: impl serde::Serialize) {
    serde_json::to_writer(out, &value).expect("decode fields are infallibly serializable");
}

/// JSON only escapes ASCII control characters, quote and backslash. Inspect
/// eight bytes at a time, then copy ordinary UTF-8 strings directly. The general
/// serializer remains responsible for escaping whenever one of those bytes occurs.
fn string(out: &mut Vec<u8>, text: &str) {
    let (words, tail) = text.as_bytes().as_chunks::<8>();
    let escape = words
        .iter()
        .any(|word| needs_escape(u64::from_ne_bytes(*word)))
        || tail.iter().any(|&b| b < 0x20 || b == b'"' || b == b'\\');
    if escape {
        value(out, text);
    } else {
        out.push(b'"');
        out.extend_from_slice(text.as_bytes());
        out.push(b'"');
    }
}

fn needs_escape(word: u64) -> bool {
    let zero =
        |word: u64| word.wrapping_sub(0x0101_0101_0101_0101) & !word & 0x8080_8080_8080_8080 != 0;
    zero(word & 0xe0e0_e0e0_e0e0_e0e0)
        || zero(word ^ 0x2222_2222_2222_2222)
        || zero(word ^ 0x5c5c_5c5c_5c5c_5c5c)
}

impl ElementJson {
    pub(crate) fn new(db: &Db, e: &ArchivedElement) -> Self {
        let mut before_value = b"{\"group_name\":".to_vec();
        string(&mut before_value, db.s(e.groupname.to_native()));
        before_value.extend_from_slice(b",\"variable\":");
        string(&mut before_value, db.s(e.name.to_native()));
        before_value.extend_from_slice(b",\"value\":");

        let mut before_attribute = b",\"element_id\":".to_vec();
        value(&mut before_attribute, e.id.to_native());
        before_attribute.extend_from_slice(b",\"attribute_id\":");

        let mut before_source = b",\"code\":".to_vec();
        string(&mut before_source, db.s(e.code.to_native()));
        before_source.extend_from_slice(b",\"data_type\":");
        string(&mut before_source, db.s(e.datatype.to_native()));
        before_source.extend_from_slice(b",\"decode\":");
        string(&mut before_source, public_decode(db, e).unwrap());
        before_source.extend_from_slice(b",\"source\":");
        Self {
            default: std::sync::OnceLock::new(),
            before_value,
            before_attribute,
            before_source,
        }
    }
    /// Keep one complete "Not Applicable" default per element. Its timestamp
    /// is archive data, but can differ between vehicle types. A mismatch takes
    /// the ordinary encoder, so cache fill order never affects output.
    fn default(&self, created_on: i64) -> Option<&[u8]> {
        let (timestamp, bytes) = self.default.get_or_init(|| {
            let mut out = self.before_value.clone();
            out.extend_from_slice(b"\"Not Applicable\"");
            out.extend_from_slice(&self.before_attribute);
            out.extend_from_slice(b"\"0\"");
            out.extend_from_slice(&self.before_source);
            out.extend_from_slice(b"\"Default\"");
            out.extend_from_slice(NULL_PROVENANCE);
            value(&mut out, crate::opt_i64(created_on));
            out.extend_from_slice(NULL_WMI);
            (created_on, out)
        });
        (*timestamp == created_on).then_some(bytes)
    }
}

const NULL_PROVENANCE: &[u8] =
    b",\"pattern_id\":null,\"vin_schema_id\":null,\"keys\":\"\",\"created_on\":";
const NULL_WMI: &[u8] = b",\"wmi_id\":null,\"to_be_qced\":false}";

pub(crate) fn encode(mut result: RawResult<'_>) -> String {
    crate::resolve::resolve_xxx(result.db, &mut result.items);
    let order = crate::projection_order(result.db, &result.items);
    let mut out = Vec::with_capacity(order.len() * 400 + 512);
    out.extend_from_slice(b"{\"vin\":");
    string(&mut out, &result.vin);
    out.extend_from_slice(b",\"wmi\":");
    string(&mut out, &result.wmi);
    out.extend_from_slice(b",\"descriptor\":");
    string(&mut out, &result.descriptor);
    out.extend_from_slice(b",\"model_year\":");
    value(&mut out, result.model_year);
    out.extend_from_slice(b",\"error_codes\":");
    value(&mut out, &result.error_codes);
    out.extend_from_slice(b",\"check_digit_valid\":");
    value(&mut out, result.check_digit_valid);
    out.extend_from_slice(b",\"corrected_vin\":");
    string(&mut out, &result.corrected_vin);
    out.extend_from_slice(b",\"elements\":[");
    for (n, (_, i)) in order.into_iter().enumerate() {
        if n != 0 {
            out.push(b',');
        }
        let it = &result.items[i];
        let template = result.db.element_json(it.element_id);
        let null_provenance = it.pattern_id == crate::tables::NULL_I32
            && it.vin_schema_id == crate::tables::NULL_I32
            && it.wmi_id == crate::tables::NULL_I32
            && it.keys.is_empty()
            && !it.to_be_qced;
        if null_provenance
            && it.source == "Default"
            && it.value == "Not Applicable"
            && it.attribute_id == "0"
        {
            if let Some(default) = template.default(it.created_on) {
                out.extend_from_slice(default);
                continue;
            }
        }
        out.extend_from_slice(&template.before_value);
        string(&mut out, &crate::scrub_value(Cow::Borrowed(&it.value)));
        out.extend_from_slice(&template.before_attribute);
        string(&mut out, &it.attribute_id);
        out.extend_from_slice(&template.before_source);
        string(&mut out, &it.source);
        if null_provenance {
            out.extend_from_slice(NULL_PROVENANCE);
            value(&mut out, crate::opt_i64(it.created_on));
            out.extend_from_slice(NULL_WMI);
            continue;
        }
        out.extend_from_slice(b",\"pattern_id\":");
        value(&mut out, crate::opt_i32(it.pattern_id));
        out.extend_from_slice(b",\"vin_schema_id\":");
        value(&mut out, crate::opt_i32(it.vin_schema_id));
        out.extend_from_slice(b",\"keys\":");
        string(&mut out, &it.keys);
        out.extend_from_slice(b",\"created_on\":");
        value(&mut out, crate::opt_i64(it.created_on));
        out.extend_from_slice(b",\"wmi_id\":");
        value(&mut out, crate::opt_i32(it.wmi_id));
        out.extend_from_slice(b",\"to_be_qced\":");
        value(&mut out, it.to_be_qced);
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    String::from_utf8(out).expect("JSON strings and literals are UTF-8")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::DecodingItem;
    use crate::tables::{serialize_artifact, Element, VpicData, NULL_I32, NULL_I64};

    fn fixture(name: &str) -> Db {
        let strings = ["", name, "Group\"\\\n日本語", "text\t", "Y\"", "code\\"];
        let mut arena_bytes = Vec::new();
        let mut arena_offsets = vec![0];
        for s in strings {
            arena_bytes.extend_from_slice(s.as_bytes());
            arena_offsets.push(arena_bytes.len() as u32);
        }
        let data = VpicData {
            arena_bytes,
            arena_offsets,
            element: [1, 2, 3, 114, 5000]
                .into_iter()
                .map(|id| Element {
                    id,
                    name: 1,
                    code: 5,
                    groupname: 2,
                    datatype: 3,
                    decode: if id == 3 { 0 } else { 4 },
                    decode_present: true,
                    isprivate: id == 2,
                    weight: 0,
                })
                .collect(),
            wmi: vec![],
            wmi_vinschema: vec![],
            vinschema: vec![],
            pattern: vec![],
            make_model: vec![],
            wmi_make: vec![],
            enginemodel: vec![],
            enginemodelpattern: vec![],
            defaultvalue: vec![],
            vinexception: vec![],
            conversion: vec![],
            lookups: vec![],
            cover: vec![],
            vspecschema: vec![],
            vspecschemapattern: vec![],
            vspecpattern: vec![],
            vspecschemamodel: vec![],
            vspecschemayear: vec![],
        };
        Db::from_bytes(&serialize_artifact(&data, 1)).unwrap()
    }

    fn raw(db: &Db) -> RawResult<'_> {
        RawResult {
            db,
            vin: "\"\\\0\n日本語".into(),
            wmi: "é\t".into(),
            descriptor: "\r\n".into(),
            model_year: Some(-1980),
            error_codes: vec![1, 7, 400],
            check_digit_valid: false,
            corrected_vin: "\u{2028}".into(),
            items: [114, 5000, -1, 1, 2, 3, 114, 114]
                .into_iter()
                .enumerate()
                .map(|(i, id)| {
                    let mut item = DecodingItem {
                        created_on: if i % 2 == 0 { NULL_I64 } else { i64::MAX },
                        pattern_id: if i % 2 == 0 { NULL_I32 } else { -5 },
                        keys: Cow::Borrowed("key\"\\\0\n"),
                        vin_schema_id: i32::MAX,
                        wmi_id: NULL_I32,
                        element_id: id,
                        attribute_id: Cow::Borrowed("attr\t\r\n\"\\é"),
                        value: Cow::Owned(format!("value {i}\t\r\n\"\\\0\u{001f}日本語")),
                        source: Cow::Borrowed("source\"\\\n"),
                        priority: 0,
                        to_be_qced: i % 2 == 0,
                    };
                    if matches!(i, 0 | 6 | 7) {
                        item.pattern_id = NULL_I32;
                        item.vin_schema_id = NULL_I32;
                        item.keys = Cow::Borrowed("");
                        item.to_be_qced = false;
                        item.source = Cow::Borrowed("Default");
                        if i != 6 {
                            item.value = Cow::Borrowed("Not Applicable");
                            item.attribute_id = Cow::Borrowed("0");
                        }
                    }
                    item
                })
                .collect(),
        }
    }

    #[test]
    fn word_scan_matches_byte_scan() {
        let mut word = 20260910u64;
        for _ in 0..100_000 {
            word ^= word << 13;
            word ^= word >> 7;
            word ^= word << 17;
            assert_eq!(
                needs_escape(word),
                word.to_ne_bytes()
                    .iter()
                    .any(|&b| b < 0x20 || b == b'"' || b == b'\\')
            );
        }
    }

    #[test]
    fn string_writer_preserves_serde_escaping_at_every_word_boundary() {
        for c in (0..=127)
            .map(char::from)
            .chain("é日本語\u{2028}\u{1f600}".chars())
        {
            for position in 0..24 {
                let text = format!("{}{c}日本語tail", "x".repeat(position));
                let mut out = Vec::new();
                string(&mut out, &text);
                assert_eq!(out, serde_json::to_vec(&text).unwrap(), "{text:?}");
            }
        }
    }

    #[test]
    fn escaping_order_filtering_and_nullable_fields_match_serde() {
        let a = fixture("variable\"\\\0\n日本語");
        let b = fixture("a different database");
        // Repeated ids, private/absent elements, sparse ids, control characters,
        // extreme integers and different databases sharing the same element ids.
        for db in [&a, &b, &a] {
            assert_eq!(
                encode(raw(db)),
                serde_json::to_string(&raw(db).full()).unwrap()
            );
        }
        let mut empty = raw(&a);
        empty.items.clear();
        empty.error_codes.clear();
        empty.model_year = None;
        let actual = encode(empty);
        assert!(actual.ends_with("\"elements\":[]}"));
        assert!(actual.contains("\"model_year\":null,\"error_codes\":[]"));
    }

    #[test]
    fn full_json_matches_serde_at_a_fixed_clock() {
        let Some(db) = Db::try_embedded() else {
            return;
        };
        let mut vins = db.cover();
        vins.extend(
            [
                "",
                "é",
                "\"\\\0\t\n",
                "1HGCM82633A004352extra",
                "1HGCM82633A004352",
            ]
            .map(String::from),
        );
        vins.push("#".repeat(200));
        for vin in vins {
            for year in [None, Some(1980), Some(2003), Some(2026), Some(0)] {
                let decode = || crate::decode_items(db, &vin, 1_788_739_200_000_000, 2026, year);
                assert_eq!(
                    encode(decode()),
                    serde_json::to_string(&decode().full()).unwrap(),
                    "{vin:?}, {year:?}"
                );
            }
        }
    }

    #[test]
    fn templates_are_shared_safely_across_threads() {
        let db = fixture("a shared database");
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..20 {
                        assert_eq!(
                            encode(raw(&db)),
                            serde_json::to_string(&raw(&db).full()).unwrap()
                        );
                    }
                });
            }
        });
    }

    #[test]
    fn batch_json_preserves_exact_bytes_input_order_and_caller_years() {
        if Db::try_embedded().is_none() {
            return;
        }
        let inputs: Vec<String> = [
            "1HGCM82633A004352",
            "é\"\\\n",
            "1FTFW1ET5DFC10312",
            "1HGCM82633A004352",
            "",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let years = [None, Some(1980), Some(1995), Some(2026)];
        let parts: Vec<_> = inputs
            .iter()
            .enumerate()
            .map(|(i, vin)| crate::decode_json(vin, years.get(i).copied().flatten()))
            .collect();
        assert_eq!(
            crate::decode_batch_json(&inputs, Some(&years)),
            format!("[{}]", parts.join(","))
        );
        assert_eq!(crate::decode_batch_json(&[], None), "[]");
    }
}
