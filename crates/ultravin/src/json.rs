//! Full JSON output from a decoded result. Constant element metadata is
//! escaped once per database; each result writes its variable fields into one
//! buffer.

use std::mem::MaybeUninit;
use std::ops::Range;
use std::sync::{Arc, Mutex};

use crate::{public_decode, tables::ArchivedElement, Db, DecodeResult, NativeBatch};

pub(crate) struct ElementJson {
    before_value: Vec<u8>,
    before_attribute: Vec<u8>,
    before_source: Vec<u8>,
    default: std::sync::OnceLock<(Option<i64>, Vec<u8>)>,
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
    fn default(&self, created_on: Option<i64>) -> Option<&[u8]> {
        let (timestamp, bytes) = self.default.get_or_init(|| {
            let mut out = self.before_value.clone();
            out.extend_from_slice(b"\"Not Applicable\"");
            out.extend_from_slice(&self.before_attribute);
            out.extend_from_slice(b"\"0\"");
            out.extend_from_slice(&self.before_source);
            out.extend_from_slice(b"\"Default\"");
            out.extend_from_slice(NULL_PROVENANCE);
            value(&mut out, created_on);
            out.extend_from_slice(NULL_WMI);
            (created_on, out)
        });
        (*timestamp == created_on).then_some(bytes)
    }
}

const NULL_PROVENANCE: &[u8] =
    b",\"pattern_id\":null,\"vin_schema_id\":null,\"keys\":\"\",\"created_on\":";
const NULL_WMI: &[u8] = b",\"wmi_id\":null,\"to_be_qced\":false}";

/// Append `result` to `out` as the JSON object `serde_json` writes for it.
/// `db` is the database that decoded it.
pub(crate) fn encode_into(db: &Db, result: &DecodeResult<'_>, out: &mut Vec<u8>) {
    out.extend_from_slice(VIN_KEY);
    string(out, &result.vin);
    out.extend_from_slice(b",\"wmi\":");
    string(out, &result.wmi);
    out.extend_from_slice(b",\"descriptor\":");
    string(out, &result.descriptor);
    out.extend_from_slice(b",\"model_year\":");
    value(out, result.model_year);
    out.extend_from_slice(b",\"error_codes\":");
    value(out, &result.error_codes);
    out.extend_from_slice(b",\"check_digit_valid\":");
    value(out, result.check_digit_valid);
    out.extend_from_slice(b",\"corrected_vin\":");
    string(out, &result.corrected_vin);
    out.extend_from_slice(b",\"elements\":[");
    for (n, it) in result.elements.iter().enumerate() {
        if n != 0 {
            out.push(b',');
        }
        let template = db.element_json(it.element_id);
        let null_provenance = it.pattern_id.is_none()
            && it.vin_schema_id.is_none()
            && it.wmi_id.is_none()
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
        string(out, &it.value);
        out.extend_from_slice(&template.before_attribute);
        string(out, &it.attribute_id);
        out.extend_from_slice(&template.before_source);
        string(out, &it.source);
        if null_provenance {
            out.extend_from_slice(NULL_PROVENANCE);
            value(out, it.created_on);
            out.extend_from_slice(NULL_WMI);
            continue;
        }
        out.extend_from_slice(b",\"pattern_id\":");
        value(out, it.pattern_id);
        out.extend_from_slice(b",\"vin_schema_id\":");
        value(out, it.vin_schema_id);
        out.extend_from_slice(b",\"keys\":");
        string(out, &it.keys);
        out.extend_from_slice(b",\"created_on\":");
        value(out, it.created_on);
        out.extend_from_slice(b",\"wmi_id\":");
        value(out, it.wmi_id);
        out.extend_from_slice(b",\"to_be_qced\":");
        value(out, it.to_be_qced);
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
}

/// [`encode_into`] a new string.
pub(crate) fn encode(db: &Db, result: &DecodeResult<'_>) -> String {
    let mut out = Vec::with_capacity(result.elements.len() * 400 + 512);
    encode_into(db, result, &mut out);
    String::from_utf8(out).expect("JSON strings and literals are UTF-8")
}

/// How every JSON object of a result begins, full or flat.
const VIN_KEY: &[u8] = b"{\"vin\":";

/// Result ids whose text a stream worker remembers across its batches.
const REMEMBERED: usize = 256;

/// One object of a batch call's JSON, `text[start..end]` for a row decoded
/// for itself. A row sharing another row's result writes `{"vin":`, its own
/// `vin`, then that row's text from `rest`, just past its `vin`.
pub(crate) struct Row {
    vin: Option<Vin>,
    text: Arc<Vec<u8>>,
    start: usize,
    rest: usize,
    end: usize,
}

impl Row {
    /// The row of `result` whose whole object is `text`.
    pub(crate) fn whole(result: &DecodeResult<'_>, text: Vec<u8>) -> Self {
        let end = text.len();
        Self::within(result, Arc::new(text), 0..end)
    }

    /// The row of `result` whose whole object is `text[span]`.
    fn within(result: &DecodeResult<'_>, text: Arc<Vec<u8>>, span: Range<usize>) -> Self {
        let rest = span.start + VIN_KEY.len() + Vin::new(&result.vin).as_bytes().len();
        debug_assert_eq!(&text[span.start..span.start + VIN_KEY.len()], VIN_KEY);
        debug_assert_eq!(
            &text[span.start + VIN_KEY.len()..rest],
            Vin::new(&result.vin).as_bytes()
        );
        Self {
            vin: None,
            text,
            start: span.start,
            rest,
            end: span.end,
        }
    }

    /// A row of `result` that shares this row's text after the `vin`.
    fn sharing(&self, result: &DecodeResult<'_>) -> Self {
        Self {
            vin: Some(Vin::new(&result.vin)),
            ..self.text_only()
        }
    }

    /// This row's text, for sharing.
    fn text_only(&self) -> Self {
        Self {
            vin: None,
            text: Arc::clone(&self.text),
            ..*self
        }
    }

    /// The object after its `vin`.
    fn after_vin(&self) -> &[u8] {
        &self.text[self.rest..self.end]
    }

    fn len(&self) -> usize {
        match &self.vin {
            None => self.end - self.start,
            Some(vin) => VIN_KEY.len() + vin.as_bytes().len() + self.end - self.rest,
        }
    }

    /// Write the row and then `end` into `out`, exactly its length plus one.
    fn write(&self, out: &mut [MaybeUninit<u8>], end: u8) {
        let (out, last) = out.split_at_mut(out.len() - 1);
        last[0].write(end);
        let Some(vin) = &self.vin else {
            out.write_copy_of_slice(&self.text[self.start..self.end]);
            return;
        };
        let (key, out) = out.split_at_mut(VIN_KEY.len());
        key.write_copy_of_slice(VIN_KEY);
        let (quoted, out) = out.split_at_mut(vin.as_bytes().len());
        quoted.write_copy_of_slice(vin.as_bytes());
        out.write_copy_of_slice(self.after_vin());
    }
}

/// A `vin` as a JSON string, in place when it is short.
enum Vin {
    Short(u8, [u8; 30]),
    Long(Box<[u8]>),
}

impl Vin {
    fn new(vin: &str) -> Self {
        let bytes = vin.as_bytes();
        if bytes.len() <= 28 && !bytes.iter().any(|&b| b < 0x20 || b == b'"' || b == b'\\') {
            let mut short = [0; 30];
            short[0] = b'"';
            short[1..=bytes.len()].copy_from_slice(bytes);
            short[bytes.len() + 1] = b'"';
            return Self::Short(bytes.len() as u8 + 2, short);
        }
        let mut quoted = Vec::new();
        string(&mut quoted, vin);
        Self::Long(quoted.into_boxed_slice())
    }

    fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Short(len, bytes) => &bytes[..usize::from(*len)],
            Self::Long(bytes) => bytes,
        }
    }
}

/// Whether `result` is one the result cache stores, so its id can recur.
fn reusable(result: &DecodeResult<'_>) -> bool {
    result.error_codes == [0] && result.corrected_vin.is_empty()
}

/// The JSON a stream's workers write, once per result id they see: two rows
/// with one id differ only in `vin` ([`NativeBatch::result_ids`]), so a row
/// whose id appeared earlier in its batch, or that its worker remembers from
/// an earlier batch, copies that row's text after the `vin` instead of
/// encoding again. Each worker takes a [`Memo`] for its batch and puts it
/// back after.
#[derive(Default)]
pub(crate) struct Rows {
    memos: Mutex<Vec<Memo>>,
}

/// A worker's text of [`REMEMBERED`] result ids, and a buffer to encode in.
struct Memo {
    table: Box<[Option<(u64, Row)>]>,
    scratch: Vec<u8>,
}

/// Where a batch row's text comes from.
enum Source {
    /// A row of an earlier batch with its id.
    Earlier(Row),
    /// Encoded by this row, at this span of the batch's text.
    Encoded(Range<usize>),
    /// The same as this earlier row of the batch.
    Repeat(usize),
}

/// A scratch buffer larger than this is not kept for the next batch.
pub(crate) const MAX_SCRATCH: usize = 16 << 20;

impl Rows {
    fn take(&self) -> Memo {
        let pooled = self.memos.lock().unwrap_or_else(|p| p.into_inner()).pop();
        pooled.unwrap_or_else(|| Memo {
            table: (0..REMEMBERED).map(|_| None).collect(),
            scratch: Vec::new(),
        })
    }

    fn put(&self, mut memo: Memo) {
        memo.scratch.clear();
        if memo.scratch.capacity() > MAX_SCRATCH {
            memo.scratch = Vec::new();
        }
        self.memos
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(memo);
    }

    /// Each row of `batch` with its input index; `encode` appends a whole
    /// object, `vin` first. The batch's new text is one allocation of its
    /// own size that its rows share.
    pub(crate) fn of(
        &self,
        batch: &NativeBatch<'_, '_>,
        encode: impl Fn(&DecodeResult<'_>, &mut Vec<u8>),
    ) -> Vec<(usize, Row)> {
        let ids = batch.result_ids();
        let mut memo = self.take();
        // The ids this batch encoded, each with its row.
        let mut here = [(u64::MAX, 0); 64];
        let mut sources = Vec::with_capacity(batch.len());
        for (row, result) in batch.iter().enumerate() {
            let id = ids.get(row).copied();
            let repeat = id.and_then(|id| Some(here[id as usize % 64]).filter(|h| h.0 == id));
            let source = if let Some(earlier) = id.and_then(|id| remembered(&memo.table, id)) {
                Source::Earlier(earlier.sharing(result))
            } else if let Some((_, first)) = repeat {
                Source::Repeat(first)
            } else {
                let start = memo.scratch.len();
                encode(result, &mut memo.scratch);
                if let Some(id) = id {
                    here[id as usize % 64] = (id, row);
                }
                Source::Encoded(start..memo.scratch.len())
            };
            sources.push(source);
        }
        // One exact copy of the batch's text: cheaper than spare capacity.
        let text = Arc::new(memo.scratch.clone());
        let mut out: Vec<(usize, Row)> = Vec::with_capacity(batch.len());
        let rows = batch.input_indices().zip(batch.iter()).zip(sources);
        for (row, ((index, result), source)) in rows.enumerate() {
            let built = match source {
                Source::Earlier(row) => row,
                Source::Repeat(first) => out[first].1.sharing(result),
                Source::Encoded(span) => {
                    let built = Row::within(result, Arc::clone(&text), span);
                    if let Some(&id) = ids.get(row) {
                        if reusable(result) {
                            memo.table[id as usize % REMEMBERED] = Some((id, built.text_only()));
                        }
                    }
                    built
                }
            };
            out.push((index, built));
        }
        self.put(memo);
        out
    }
}

/// Append the JSON Lines of `batch` to `out`, each object made by `encode`
/// (`vin` first) and ended by a newline. A row whose id appeared earlier in
/// the batch is `{"vin":`, its own `vin`, then that row's text after its
/// `vin`, with no encoding. Ids are not remembered across batches: on sorted
/// input that measured 13% slower than within a batch alone.
pub(crate) fn lines_into(
    batch: &NativeBatch<'_, '_>,
    encode: impl Fn(&DecodeResult<'_>, &mut Vec<u8>),
    out: &mut Vec<u8>,
) {
    let ids = batch.result_ids();
    // Each id this batch encoded, with its text after the `vin`.
    let mut here = [(u64::MAX, 0, 0); 64];
    for (row, result) in batch.iter().enumerate() {
        let Some(&id) = ids.get(row) else {
            encode(result, out);
            out.push(b'\n');
            continue;
        };
        let slot = &mut here[id as usize % 64];
        let vin = Vin::new(&result.vin);
        if slot.0 == id {
            let (rest, end) = (slot.1, slot.2);
            out.extend_from_slice(VIN_KEY);
            out.extend_from_slice(vin.as_bytes());
            out.extend_from_within(rest..end);
        } else {
            let start = out.len();
            encode(result, out);
            let rest = start + VIN_KEY.len() + vin.as_bytes().len();
            debug_assert_eq!(&out[start..start + VIN_KEY.len()], VIN_KEY);
            debug_assert_eq!(&out[start + VIN_KEY.len()..rest], vin.as_bytes());
            *slot = (id, rest, out.len());
        }
        out.push(b'\n');
    }
}

/// The text `table` remembers for `id`.
fn remembered(table: &[Option<(u64, Row)>], id: u64) -> Option<&Row> {
    match &table[id as usize % REMEMBERED] {
        Some((seen, row)) if *seen == id => Some(row),
        _ => None,
    }
}

/// The rows of `batches`, each with its input index, as a JSON array in
/// input order, or with `lines` as one object per line, each ending in a
/// newline. Every index below the row count must appear exactly once. The
/// decoder pool copies a batch's rows into place, then frees their text.
pub(crate) fn join(batches: Vec<Vec<(usize, Row)>>, lines: bool) -> String {
    use rayon::prelude::*;

    let rows = batches.iter().map(Vec::len).sum::<usize>();
    if rows == 0 {
        return if lines { String::new() } else { "[]".into() };
    }
    // Each row's offset in the output: first its length, then the sum of
    // the lengths before it.
    let mut at = vec![usize::MAX; rows];
    for &(index, ref row) in batches.iter().flatten() {
        assert_eq!(at[index], usize::MAX, "input {index} has two rows");
        at[index] = row.len() + 1;
    }
    let open = usize::from(!lines);
    let mut len = open;
    for offset in &mut at {
        len += std::mem::replace(offset, len);
    }
    let mut out = Vec::<u8>::with_capacity(len);
    let base = Base(out.spare_capacity_mut().as_mut_ptr());
    let write = |(index, row): (usize, Row)| {
        let end = match (lines, index + 1 == rows) {
            (true, _) => b'\n',
            (false, false) => b',',
            (false, true) => b']',
        };
        // SAFETY: every index below `rows` has one row (checked above), so
        // the spans `at[index]..at[index] + row.len() + 1` tile `open..len`
        // without overlap, inside the capacity, and nothing else uses them.
        let place = unsafe { std::slice::from_raw_parts_mut(base.at(at[index]), row.len() + 1) };
        row.write(place, end);
    };
    crate::install_batch_work(|| {
        batches.into_par_iter().for_each(|batch| {
            batch.into_par_iter().with_min_len(64).for_each(write);
        });
    });
    if !lines {
        out.spare_capacity_mut()[0].write(b'[');
    }
    // SAFETY: the rows and the opening bracket wrote all of `0..len`.
    unsafe { out.set_len(len) };
    debug_assert!(std::str::from_utf8(&out).is_ok());
    // SAFETY: every row is UTF-8 (a quoted `str`, and a UTF-8 object cut
    // after the ASCII quote that ends its `vin`), as are the framing bytes.
    unsafe { String::from_utf8_unchecked(out) }
}

/// The start of an output buffer the decoder pool writes rows into.
#[derive(Clone, Copy)]
struct Base(*mut MaybeUninit<u8>);

// SAFETY: threads write through it only to disjoint spans (see `join`).
unsafe impl Send for Base {}
unsafe impl Sync for Base {}

impl Base {
    /// The byte at `offset`, whose span the caller writes alone.
    fn at(self, offset: usize) -> *mut MaybeUninit<u8> {
        self.0.wrapping_add(offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::DecodingItem;
    use crate::tables::{serialize_artifact, Element, VpicData, NULL_I32, NULL_I64};
    use crate::RawResult;
    use std::borrow::Cow;

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
            defaults: None,
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
                encode(db, &raw(db).full()),
                serde_json::to_string(&raw(db).full()).unwrap()
            );
        }
        let mut empty = raw(&a);
        empty.items.clear();
        empty.error_codes.clear();
        empty.model_year = None;
        let actual = encode(&a, &empty.full());
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
                    encode(db, &decode().full()),
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
                            encode(&db, &raw(&db).full()),
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
