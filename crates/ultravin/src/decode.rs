//! The decode core, in the exact order of `spvindecode_core`: pattern pass,
//! layered sources, Formula Pattern, dedup, make, conversion, vehicle specs,
//! and defaults.

use std::borrow::Cow;
use std::cell::RefCell;
use std::cmp::Ordering;

use crate::db::Db;
use crate::hash::{ElementIndex, ElementSet, IntMap, IntSet};
use crate::matcher::like_match;
use crate::tables::{element_lookup_tag, is_exempt, ArchivedWmi, NULL_I32, NULL_I64};

/// A single decoding item (the `tblDecodingItem` ROW), pre-resolution.
///
/// `source` is a static literal for every source but Conversion, and `value` is
/// the `"XXX"` sentinel for every pattern/spec/default item (resolved lazily, and
/// only on the winning pass). These common literals borrow instead of allocating.
/// Resolved values, keys and attribute ids also borrow the archive until output
/// needs an owned string; losing passes and discarded rows avoid those copies.
#[derive(Debug, Clone)]
pub struct DecodingItem<'a> {
    pub created_on: i64, // NULL_I64 = none
    pub pattern_id: i32, // NULL_I32 = none
    pub keys: Cow<'a, str>,
    pub vin_schema_id: i32, // NULL_I32 = none
    pub wmi_id: i32,        // NULL_I32 = none
    pub element_id: i32,
    pub attribute_id: Cow<'a, str>,
    pub value: Cow<'a, str>,
    pub source: Cow<'static, str>,
    pub priority: i32,
    pub to_be_qced: bool,
}

/// Borrow already-uppercase archive keys; preserve ASCII-only casing on unusual data.
fn uppercase_key(key: &str) -> Cow<'_, str> {
    if key.bytes().any(|b| b.is_ascii_lowercase()) {
        Cow::Owned(key.to_ascii_uppercase())
    } else {
        Cow::Borrowed(key)
    }
}

/// Archive names commonly already have their required uppercase spelling.
/// Unicode names retain the full to_uppercase behavior, including expansions.
fn uppercase_name(name: &str) -> Cow<'_, str> {
    if name.is_ascii() && !name.bytes().any(|b| b.is_ascii_lowercase()) {
        Cow::Borrowed(name)
    } else {
        Cow::Owned(name.to_uppercase())
    }
}

/// Per-decode memo of the pattern-key scan, keyed by VIN schema id.
///
/// Which patterns of a schema a VIN's keys match depends only on the keys, not on
/// the model year — but [`crate::decode_full`] runs the core once per candidate
/// year (~1.5 passes per VIN over the parity corpus), and that scan is the single
/// hottest loop in a decode. Caching the hit list per schema makes the later
/// passes a replay instead of a rescan. Values are indices into
/// [`crate::db::Db::patterns`], avoiding another schema-range search on replay.
const MAX_RETAINED_PATTERN_SCHEMAS: usize = 128;
const MAX_RETAINED_PATTERN_HITS: usize = 4_096;

#[derive(Default)]
struct PatternScanStorage {
    schema_slots: IntMap<i32, usize>,
    hit_vectors: Vec<Vec<u32>>,
}

impl PatternScanStorage {
    fn clear(&mut self) {
        self.schema_slots.clear();
        for hits in &mut self.hit_vectors {
            hits.clear();
        }
    }

    fn retained_capacity(&self) -> usize {
        self.schema_slots.capacity()
            + self.hit_vectors.capacity()
            + self.hit_vectors.iter().map(Vec::capacity).sum::<usize>()
    }

    fn bound_retained_capacity(&mut self) {
        let total_hits = self.hit_vectors.iter().map(Vec::capacity).sum::<usize>();
        if self.schema_slots.capacity() > MAX_RETAINED_PATTERN_SCHEMAS
            || self.hit_vectors.capacity() > MAX_RETAINED_PATTERN_SCHEMAS
            || total_hits > MAX_RETAINED_PATTERN_HITS
        {
            *self = Self::default();
        }
    }
}

thread_local! {
    static PATTERN_SCAN_SCRATCH: RefCell<PatternScanStorage> = RefCell::new(PatternScanStorage::default());
}

pub struct PatternScan {
    storage: PatternScanStorage,
    used_vectors: usize,
}

impl Default for PatternScan {
    fn default() -> Self {
        let mut storage = PATTERN_SCAN_SCRATCH
            .try_with(|slot| {
                slot.try_borrow_mut()
                    .map(|mut slot| std::mem::take(&mut *slot))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        storage.clear();
        Self {
            storage,
            used_vectors: 0,
        }
    }
}

impl PatternScan {
    fn hits<'scan>(&'scan mut self, db: &Db, schema_id: i32, keys: &str) -> &'scan [u32] {
        let slot = match self.storage.schema_slots.get(&schema_id) {
            Some(&slot) => slot,
            None => {
                let slot = self.used_vectors;
                self.used_vectors += 1;
                if slot == self.storage.hit_vectors.len() {
                    self.storage.hit_vectors.push(Vec::new());
                }
                db.pattern_index(schema_id)
                    .expect("schema exists")
                    .hits_into(db, keys, &mut self.storage.hit_vectors[slot]);
                self.storage.schema_slots.insert(schema_id, slot);
                slot
            }
        };
        &self.storage.hit_vectors[slot]
    }
}

impl Drop for PatternScan {
    fn drop(&mut self) {
        self.storage.clear();
        self.storage.bound_retained_capacity();
        let _ = PATTERN_SCAN_SCRATCH.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                if slot.retained_capacity() < self.storage.retained_capacity() {
                    *slot = std::mem::take(&mut self.storage);
                }
            }
        });
    }
}

/// Output of the core pass.
pub struct CoreResult<'a> {
    pub items: Vec<DecodingItem<'a>>,
    pub wmi_found: bool,
}

const MAX_RETAINED_MATCHED_PATTERNS: usize = 256;
const MAX_RETAINED_SCHEMA_PRIORITIES: usize = 128;
const MAX_RETAINED_MAKE_IDS: usize = 64;

#[derive(Default)]
struct DecodeScratch {
    matched_patterns: Vec<(i32, u32)>,
    schema_yearfrom: IntMap<i32, i32>,
    distinct_makeids: Vec<i32>,
}

impl DecodeScratch {
    fn clear(&mut self) {
        self.matched_patterns.clear();
        self.schema_yearfrom.clear();
        self.distinct_makeids.clear();
    }

    fn bound_retained_capacity(&mut self) {
        if self.matched_patterns.capacity() > MAX_RETAINED_MATCHED_PATTERNS {
            self.matched_patterns = Vec::new();
        }
        if self.schema_yearfrom.capacity() > MAX_RETAINED_SCHEMA_PRIORITIES {
            self.schema_yearfrom = IntMap::default();
        }
        if self.distinct_makeids.capacity() > MAX_RETAINED_MAKE_IDS {
            self.distinct_makeids = Vec::new();
        }
    }
}

thread_local! {
    static DECODE_SCRATCH: RefCell<DecodeScratch> = RefCell::new(DecodeScratch::default());
}

struct DecodeScratchGuard {
    scratch: DecodeScratch,
}

impl DecodeScratchGuard {
    fn take() -> Self {
        let scratch = DECODE_SCRATCH
            .try_with(|slot| {
                slot.try_borrow_mut()
                    .map(|mut slot| std::mem::take(&mut *slot))
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        Self { scratch }
    }
}

impl std::ops::Deref for DecodeScratchGuard {
    type Target = DecodeScratch;

    fn deref(&self) -> &Self::Target {
        &self.scratch
    }
}

impl std::ops::DerefMut for DecodeScratchGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.scratch
    }
}

impl Drop for DecodeScratchGuard {
    fn drop(&mut self) {
        self.scratch.clear();
        self.scratch.bound_retained_capacity();
        let _ = DECODE_SCRATCH.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                if slot.matched_patterns.capacity() < self.scratch.matched_patterns.capacity() {
                    slot.matched_patterns = std::mem::take(&mut self.scratch.matched_patterns);
                }
                if slot.schema_yearfrom.capacity() < self.scratch.schema_yearfrom.capacity() {
                    slot.schema_yearfrom = std::mem::take(&mut self.scratch.schema_yearfrom);
                }
                if slot.distinct_makeids.capacity() < self.scratch.distinct_makeids.capacity() {
                    slot.distinct_makeids = std::mem::take(&mut self.scratch.distinct_makeids);
                }
            }
        });
    }
}

pub(crate) const DEFAULT_MODEL_YEAR_SOURCE: &str = "***X*|Y";

const FIRST_STATIC_MODEL_YEAR: i32 = 1900;
const STATIC_MODEL_YEAR_COUNT: usize = 300;

const fn static_model_year_texts() -> [[u8; 4]; STATIC_MODEL_YEAR_COUNT] {
    let mut texts = [[0; 4]; STATIC_MODEL_YEAR_COUNT];
    let mut index = 0;
    while index < STATIC_MODEL_YEAR_COUNT {
        let year = FIRST_STATIC_MODEL_YEAR as usize + index;
        texts[index] = [
            b'0' + (year / 1000) as u8,
            b'0' + ((year / 100) % 10) as u8,
            b'0' + ((year / 10) % 10) as u8,
            b'0' + (year % 10) as u8,
        ];
        index += 1;
    }
    texts
}

static MODEL_YEAR_TEXTS: [[u8; 4]; STATIC_MODEL_YEAR_COUNT] = static_model_year_texts();

fn static_model_year_text(year: i32) -> Option<&'static str> {
    let index = usize::try_from(year.checked_sub(FIRST_STATIC_MODEL_YEAR)?).ok()?;
    let bytes = MODEL_YEAR_TEXTS.get(index)?;
    Some(std::str::from_utf8(bytes).expect("static model years contain only ASCII digits"))
}

fn model_year_values(year: i32) -> (Cow<'static, str>, Cow<'static, str>) {
    if let Some(text) = static_model_year_text(year) {
        (Cow::Borrowed(text), Cow::Borrowed(text))
    } else {
        let text = year.to_string();
        (Cow::Owned(text.clone()), Cow::Owned(text))
    }
}

/// Stack-backed form of the at-most-14-byte VIN key used by the decode core.
pub(crate) struct VarKeys {
    bytes: [u8; 14],
    len: usize,
}

impl VarKeys {
    pub(crate) fn as_str(&self) -> &str {
        // `sanitize` makes the internal decode VIN ASCII before constructing this.
        std::str::from_utf8(&self.bytes[..self.len]).expect("sanitized VIN keys are ASCII")
    }
}

pub(crate) fn build_var_keys_stack(vin: &str) -> VarKeys {
    let b = vin.as_bytes();
    let mut out = VarKeys {
        bytes: [0; 14],
        len: 0,
    };
    if b.len() <= 3 {
        return out;
    }
    let end = b.len().min(8);
    let first = &b[3..end];
    out.bytes[..first.len()].copy_from_slice(first);
    out.len = first.len();
    if b.len() > 9 {
        out.bytes[out.len] = b'|';
        out.len += 1;
        let end2 = b.len().min(17);
        let second = &b[9..end2];
        out.bytes[out.len..out.len + second.len()].copy_from_slice(second);
        out.len += second.len();
    }
    out
}

/// `var_keys = vin[3..8] || ('|' || vin[9..17])` (1-based 4-8 and 10-17).
#[cfg(test)]
fn build_var_keys_reference(vin: &str) -> String {
    let b = vin.as_bytes();
    if b.len() <= 3 {
        return String::new();
    }
    let mut k = String::new();
    let end = b.len().min(8);
    k.push_str(&vin[3..end]);
    if b.len() > 9 {
        k.push('|');
        let end2 = b.len().min(17);
        k.push_str(&vin[9..end2]);
    }
    k
}

/// Run the decode core using caller-owned item storage.
///
/// The returned [`CoreResult`] always owns the supplied vector, cleared before
/// use. This lets a worker recycle its allocation without retaining database
/// references between calls.
#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_core_into<'a>(
    db: &'a Db,
    var_wmi: &str,
    wmi: Option<&'a ArchivedWmi>,
    var_keys: &str,
    model_year: Option<i32>,
    model_year_source: &str,
    scan: &mut PatternScan,
    mut items: Vec<DecodingItem<'a>>,
) -> CoreResult<'a> {
    items.clear();
    let Some(wmi) = wmi else {
        return CoreResult {
            items,
            wmi_found: false,
        };
    };
    let wmiid = wmi.id.to_native();
    // Move owned scratch out instead of holding a RefCell borrow through the
    // decode. A nested decode therefore receives a fresh default scratch, and
    // the guard restores bounded capacity even while unwinding a panic.
    let mut scratch = DecodeScratchGuard::take();
    scratch.clear();

    // Pre-size for a typical decode (pattern matches + layered sources + specs +
    // defaults + the 6 corrections) so the hot push loops don't repeatedly realloc.
    items.reserve(96);

    // --- Pattern pass: collect matches, then order globally by Pattern.Id ASC.
    // Capture each year-eligible schema's `YearFrom` here (in slice order, first
    // wins) so the Pattern-source priority is one map lookup per matched pattern
    // instead of `schema_year_from` rescanning `wmi_vinschema` per pattern.
    for wvs in db.wmi_vinschema_for(wmiid) {
        if let Some(my) = model_year {
            if my < wvs.yearfrom.to_native() || my > wvs.yearto_or(2999) {
                continue;
            }
        }
        let sid = wvs.vinschemaid.to_native();
        scratch
            .schema_yearfrom
            .entry(sid)
            .or_insert(wvs.yearfrom.to_native());
        let Some(vs) = db.vinschema_by_id(sid) else {
            continue;
        };
        if vs.tobeqced {
            continue;
        }
        // Year-independent, so the first pass to reach this schema pays for the
        // scan and any later pass replays its hit list (see [`PatternScan`]).
        let hits = scan.hits(db, sid, var_keys);
        scratch.matched_patterns.extend(
            hits.iter()
                .map(|&index| (db.patterns()[index as usize].id.to_native(), index)),
        );
    }
    // Pattern ids are unique, so the order is total — no need for a stable sort
    // (which allocates a scratch buffer).
    scratch.matched_patterns.sort_unstable_by_key(|&(id, _)| id);
    for &(_, index) in &scratch.matched_patterns {
        let p = &db.patterns()[index as usize];
        items.push(DecodingItem {
            created_on: p.createdon_key.to_native(),
            pattern_id: p.id.to_native(),
            keys: uppercase_key(db.s(p.keys.to_native())),
            vin_schema_id: p.vinschemaid.to_native(),
            wmi_id: wmiid,
            element_id: p.elementid.to_native(),
            attribute_id: Cow::Borrowed(db.s(p.attributeid.to_native())),
            value: Cow::Borrowed("XXX"),
            source: Cow::Borrowed("Pattern"),
            priority: *scratch
                .schema_yearfrom
                .get(&p.vinschemaid.to_native())
                .unwrap_or(&0),
            to_be_qced: false,
        });
    }

    // --- (a) EngineModelPattern (priority 50).
    if let Some(idx) = pick_element18(&items) {
        let em_name = items[idx].attribute_id.trim().to_ascii_lowercase();
        let keys = items[idx].keys.clone();
        let pattern_id = items[idx].pattern_id;
        let vin_schema_id = items[idx].vin_schema_id;
        if let Some(em) = db.enginemodel_by_norm(&em_name) {
            for child in db.enginemodelpatterns_for(em.id.to_native()) {
                items.push(DecodingItem {
                    created_on: child.createdon_key.to_native(),
                    pattern_id,
                    keys: keys.clone(),
                    vin_schema_id,
                    wmi_id: wmiid,
                    element_id: child.elementid.to_native(),
                    attribute_id: Cow::Borrowed(db.s(child.attributeid.to_native())),
                    value: Cow::Borrowed("XXX"),
                    source: Cow::Borrowed("EngineModelPattern"),
                    priority: 50,
                    to_be_qced: false,
                });
            }
        }
    }

    let strings = db.wmi_strings(wmi);
    let wmi_upper = Cow::Borrowed(strings.wmi.as_str());

    // --- (b) VehType 39 (priority 100).
    if let Some((id, name)) = &strings.vehicle {
        items.push(DecodingItem {
            created_on: wmi.createdon_key.to_native(),
            pattern_id: NULL_I32,
            keys: wmi_upper.clone(),
            vin_schema_id: NULL_I32,
            wmi_id: wmiid,
            element_id: 39,
            attribute_id: Cow::Borrowed(id.as_str()),
            value: Cow::Borrowed(name.as_str()),
            source: Cow::Borrowed("VehType"),
            priority: 100,
            to_be_qced: false,
        });
    }

    // --- (c)/(d) Manufacturer Name 27 and Id 157 (priority 100).
    if let Some((id, name)) = &strings.manufacturer {
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: wmi_upper.clone(),
            vin_schema_id: NULL_I32,
            wmi_id: wmiid,
            element_id: 27,
            attribute_id: Cow::Borrowed(id.as_str()),
            value: Cow::Borrowed(name.as_str()),
            source: Cow::Borrowed("Manu. Name"),
            priority: 100,
            to_be_qced: false,
        });
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: wmi_upper,
            vin_schema_id: NULL_I32,
            wmi_id: wmiid,
            element_id: 157,
            attribute_id: Cow::Borrowed(id.as_str()),
            value: Cow::Borrowed(id.as_str()),
            source: Cow::Borrowed("Manu. Id"),
            priority: 100,
            to_be_qced: false,
        });
    }

    // --- (e) ModelYear 29 (priority 100).
    if let Some(my) = model_year {
        let (attribute_id, value) = model_year_values(my);
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: if model_year_source == DEFAULT_MODEL_YEAR_SOURCE {
                Cow::Borrowed(DEFAULT_MODEL_YEAR_SOURCE)
            } else {
                Cow::Owned(model_year_source.to_string())
            },
            vin_schema_id: NULL_I32,
            wmi_id: NULL_I32,
            element_id: 29,
            attribute_id,
            value,
            source: Cow::Borrowed("ModelYear"),
            priority: 100,
            to_be_qced: false,
        });
    }

    // --- Formula Pattern (priority 100): patterns whose keys carry `#` digit
    // placeholders; the matched VIN digits become the value directly.
    append_formula_patterns(db, &mut items, wmiid, var_keys, model_year);

    // --- Dedup (once).
    dedup_per_element(&mut items);

    // --- Make 26 (post-dedup, never re-deduped).
    append_make(
        db,
        &mut items,
        wmiid,
        db.s(wmi.wmi.to_native()),
        wmi.createdon_key.to_native(),
        &mut scratch.distinct_makeids,
    );

    // --- Conversion (priority 100): derive sibling elements via vpic.conversion.
    append_conversions(db, &mut items);

    // --- Vehicle Specs (priority -100): make/model/year/vehicletype matching.
    append_vehicle_specs(db, &mut items, var_wmi, model_year);

    // --- DefaultValue (priority 10).
    append_default_values(db, &mut items);

    CoreResult {
        items,
        wmi_found: true,
    }
}

/// Formula Pattern insert (port of `spvindecode_core` L150-173). `formulaKeys`
/// is `var_keys` with every digit replaced by `#`; a pattern qualifies when its
/// `keys` contain a `#`, its element is not in {26,27,29,39}, and `formulaKeys
/// LIKE replace(keys,'*','_')||'%'`. The emitted value is the slice of
/// `var_keys` spanning the pattern's first-to-last `#`. No Decode/IsPrivate/
/// TobeQCed/PublicAvailability filtering (only `INNER JOIN Element`).
fn append_formula_patterns<'a>(
    db: &'a Db,
    items: &mut Vec<DecodingItem<'a>>,
    wmiid: i32,
    var_keys: &str,
    model_year: Option<i32>,
) {
    let formula_keys = std::cell::OnceCell::new();
    let mut seen_vs: IntSet<i32> = IntSet::default();
    for wvs in db.wmi_vinschema_for(wmiid) {
        if let Some(my) = model_year {
            if my < wvs.yearfrom.to_native() || my > wvs.yearto_or(2999) {
                continue;
            }
        }
        let vsid = wvs.vinschemaid.to_native();
        let index = db.pattern_index(vsid);
        let rows = index
            .map(|index| index.formula_rows.as_slice())
            .unwrap_or(&[]);
        // Formula patterns do not require a VinSchema row. Preserve that join
        // behavior for orphan schema ids, which have no slot in the lazy index.
        let unindexed = if index.is_none() {
            db.patterns_for(vsid)
        } else {
            &[]
        };
        if rows.is_empty() && unindexed.is_empty() || !seen_vs.insert(vsid) {
            continue;
        }
        for p in rows
            .iter()
            .map(|&i| &db.patterns()[i as usize])
            .chain(unindexed)
        {
            if matches!(p.elementid.to_native(), 26 | 27 | 29 | 39) {
                continue;
            }
            let keys = db.s(p.keys.to_native());
            if !keys.contains('#') {
                continue;
            }
            if db.element_by_id(p.elementid.to_native()).is_none() {
                continue;
            }
            // Most schemas have no formula rows. Build the substituted VIN
            // only when a surviving formula actually needs to match it.
            let fk = formula_keys
                .get_or_init(|| {
                    let mut text = String::with_capacity(var_keys.len());
                    text.extend(
                        var_keys
                            .chars()
                            .map(|c| if c.is_ascii_digit() { '#' } else { c }),
                    );
                    text
                })
                .as_bytes();
            if !like_match(fk, keys.as_bytes()) {
                continue;
            }
            items.push(DecodingItem {
                created_on: p.createdon_key.to_native(),
                pattern_id: p.id.to_native(),
                keys: Cow::Borrowed(keys),
                vin_schema_id: vsid,
                wmi_id: NULL_I32,
                element_id: p.elementid.to_native(),
                attribute_id: Cow::Borrowed(db.s(p.attributeid.to_native())),
                value: Cow::Owned(formula_value(var_keys, keys)),
                source: Cow::Borrowed("Formula Pattern"),
                priority: 100,
                to_be_qced: false,
            });
        }
    }
}

/// `SUBSTRING(var_keys, STRPOS(keys,'#'), last_hash - first_hash + 1)` — the
/// slice of `var_keys` covering the pattern's first-to-last `#` (1-based, port
/// of the L163 STRPOS/REVERSE expression).
fn formula_value(var_keys: &str, keys: &str) -> String {
    let kb = keys.as_bytes();
    let (Some(first), Some(last)) = (
        kb.iter().position(|&c| c == b'#'),
        kb.iter().rposition(|&c| c == b'#'),
    ) else {
        return String::new();
    };
    let vb = var_keys.as_bytes();
    if first >= vb.len() {
        return String::new();
    }
    let end = (last + 1).min(vb.len());
    String::from_utf8_lossy(&vb[first..end]).into_owned()
}

/// Pick the element-18 item by (Priority DESC, CreatedOn DESC, id DESC).
fn pick_element18(items: &[DecodingItem]) -> Option<usize> {
    let mut best: Option<usize> = None;
    for (i, it) in items.iter().enumerate() {
        if it.element_id != 18 {
            continue;
        }
        match best {
            None => best = Some(i),
            Some(b) => {
                let cur = &items[b];
                let better = it.priority > cur.priority
                    || (it.priority == cur.priority
                        && created_desc_nulls_first(it.created_on, cur.created_on)
                            == Ordering::Less)
                    || (it.priority == cur.priority && it.created_on == cur.created_on && i > b);
                if better {
                    best = Some(i);
                }
            }
        }
    }
    best
}

/// CreatedOn DESC with NULLs first (i.e. `Less` == ranks earlier / wins).
fn created_desc_nulls_first(a: i64, b: i64) -> Ordering {
    match (a == NULL_I64, b == NULL_I64) {
        (true, true) => Ordering::Equal,
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        (false, false) => b.cmp(&a),
    }
}

/// Key length ignoring `*`, the third rung of [`dedup_cmp`]. `cover` reproduces
/// the same grouping, so both must count identically — hence one function.
pub(crate) fn len_no_star(keys: &str) -> usize {
    keys.chars().filter(|c| *c != '*').count()
}

/// Compare two keys as if their `[`/`]` were stripped, without allocating.
/// Keys are ASCII, so byte order matches `str`'s lexicographic ordering — this
/// is equivalent to `keys_no_brackets(a).cmp(&keys_no_brackets(b))`.
fn cmp_keys_no_brackets(a: &str, b: &str) -> Ordering {
    let mut ai = a.bytes().filter(|&c| c != b'[' && c != b']');
    let mut bi = b.bytes().filter(|&c| c != b'[' && c != b']');
    loop {
        match (ai.next(), bi.next()) {
            (Some(x), Some(y)) => match x.cmp(&y) {
                Ordering::Equal => continue,
                other => return other,
            },
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

/// RANK dedup: keep the best item per non-exempt element id.
fn dedup_per_element(items: &mut Vec<DecodingItem>) {
    // best index per element id (lowest under the comparator).
    let mut best = ElementIndex::default();
    for (i, it) in items.iter().enumerate() {
        if is_exempt(it.element_id) {
            continue;
        }
        match best.get(&it.element_id) {
            None => {
                best.insert(it.element_id, i);
            }
            Some(&b) => {
                if dedup_cmp(it, i, &items[b], b) == Ordering::Less {
                    best.insert(it.element_id, i);
                }
            }
        }
    }
    // Keep an item iff it is exempt or the chosen best for its element. `best`
    // stores original indices, so comparing the running original index needs no
    // separate keep-bitmap.
    let mut idx = 0;
    items.retain(|it| {
        let i = idx;
        idx += 1;
        is_exempt(it.element_id) || best.get(&it.element_id) == Some(&i)
    });
}

/// Dedup comparator: Priority DESC, CreatedOn DESC (NULLS FIRST), len_no_star
/// ASC, keys_no_brackets ASC, synthetic id (insertion order) ASC.
fn dedup_cmp(a: &DecodingItem, ai: usize, b: &DecodingItem, bi: usize) -> Ordering {
    b.priority
        .cmp(&a.priority)
        .then_with(|| created_desc_nulls_first(a.created_on, b.created_on))
        .then_with(|| len_no_star(&a.keys).cmp(&len_no_star(&b.keys)))
        .then_with(|| cmp_keys_no_brackets(&a.keys, &b.keys))
        .then_with(|| ai.cmp(&bi))
}

/// Make (element 26): pattern-model join (priority 1000), else single-WMI make.
fn append_make<'a>(
    db: &'a Db,
    items: &mut Vec<DecodingItem<'a>>,
    wmiid: i32,
    var_wmi: &'a str,
    wmi_created: i64,
    distinct_makeids: &mut Vec<i32>,
) {
    let model_item = items.iter().find(|it| it.element_id == 28).map(|it| {
        (
            it.attribute_id.clone(),
            it.pattern_id,
            it.keys.clone(),
            it.vin_schema_id,
        )
    });

    if let Some((model_attr, pattern_id, keys, vin_schema_id)) = model_item {
        if let Ok(modelid) = model_attr.parse::<i32>() {
            for mm in db.makes_for_model(modelid) {
                let makeid = mm.makeid.to_native();
                let name = element_lookup_tag(26)
                    .and_then(|t| db.lookup(t, makeid))
                    .map(uppercase_name)
                    .unwrap_or_default();
                items.push(DecodingItem {
                    created_on: NULL_I64,
                    pattern_id,
                    keys: keys.clone(),
                    vin_schema_id,
                    wmi_id: NULL_I32,
                    element_id: 26,
                    attribute_id: Cow::Owned(makeid.to_string()),
                    value: name,
                    source: Cow::Borrowed("pattern - model"),
                    priority: 1000,
                    to_be_qced: false,
                });
            }
        }
    } else {
        // single distinct public make via wmi_make
        let makes = db.wmi_makes_for(wmiid);
        distinct_makeids.extend(makes.iter().map(|m| m.makeid.to_native()));
        distinct_makeids.sort_unstable();
        distinct_makeids.dedup();
        if distinct_makeids.len() == 1 {
            let makeid = distinct_makeids[0];
            let name = element_lookup_tag(26)
                .and_then(|t| db.lookup(t, makeid))
                .map(uppercase_name)
                .unwrap_or_default();
            items.push(DecodingItem {
                created_on: wmi_created,
                pattern_id: NULL_I32,
                keys: Cow::Borrowed(var_wmi),
                vin_schema_id: NULL_I32,
                wmi_id: wmiid,
                element_id: 26,
                attribute_id: Cow::Owned(makeid.to_string()),
                value: name,
                source: Cow::Borrowed("Make"),
                priority: -100,
                to_be_qced: false,
            });
        }
    }
}

/// Conversion (priority 100): the `vpic.conversion` cursor loop. For each decoded
/// item whose `ElementId` is a conversion `FromElementId`, evaluate the formula
/// (`#x#` = the item's `AttributeId`) and emit the `ToElementId` — but only when
/// that target is not already present for this pass. The cursor order
/// (Priority DESC, CreatedOn DESC NULLS FIRST, conversion id ASC) decides which
/// source wins when several would produce the same target.
fn append_conversions<'a>(db: &'a Db, items: &mut Vec<DecodingItem<'a>>) {
    struct Row<'a> {
        priority: i32,
        created_on: i64,
        conv_id: i32,
        to_elem: i32,
        formula: &'a str,
        value: Cow<'a, str>,
        keys: Cow<'a, str>,
        pattern_id: i32,
        vin_schema_id: i32,
        wmi_id: i32,
    }

    // Snapshot the cursor rows before any insert (PostgreSQL evaluates the FOR
    // query once, so conversion-derived items never spawn further conversions).
    let mut rows: Vec<Row> = Vec::new();
    for it in items.iter() {
        for c in db.conversions_from(it.element_id) {
            rows.push(Row {
                priority: it.priority,
                created_on: it.created_on,
                conv_id: c.id.to_native(),
                to_elem: c.toelementid.to_native(),
                formula: db.s(c.formula.to_native()),
                value: it.attribute_id.clone(),
                keys: it.keys.clone(),
                pattern_id: it.pattern_id,
                vin_schema_id: it.vin_schema_id,
                wmi_id: it.wmi_id,
            });
        }
    }
    if rows.is_empty() {
        return;
    }
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(created_desc_nulls_first(a.created_on, b.created_on))
            .then(a.conv_id.cmp(&b.conv_id))
    });

    let mut present: ElementSet = items.iter().map(|it| it.element_id).collect();
    for r in rows {
        if !present.insert(r.to_elem) {
            continue;
        }
        let result = crate::conversion::eval(r.formula, &r.value);
        let source = conversion_source(r.conv_id, r.formula, &r.value);
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: r.pattern_id,
            keys: r.keys,
            vin_schema_id: r.vin_schema_id,
            wmi_id: r.wmi_id,
            element_id: r.to_elem,
            attribute_id: Cow::Owned(result.clone()),
            value: Cow::Owned(result),
            source: Cow::Owned(source),
            priority: 100,
            to_be_qced: false,
        });
    }
}

/// `left('Conversion ' || id || ': ' || replace(formula,'#x#',value), 50)`.
fn conversion_source(conv_id: i32, formula: &str, value: &str) -> String {
    let full = format!("Conversion {conv_id}: {}", formula.replace("#x#", value));
    full.chars().take(50).collect()
}

/// Element ids that never block a non-key spec in STEP 3 even when already
/// decoded (the proc's `ElementId NOT IN (1,114,...)` carve-out — note it
/// includes element 1, unlike the dedup-exempt list).
const SPEC_EXEMPT: [i32; 9] = [1, 114, 121, 129, 150, 154, 155, 169, 186];

/// Vehicle Specs (priority -100): the `spvindecode_core` spec sub-pass.
///
/// Runs once per pass after Conversion (only when a WMI was found). Selects
/// candidate `VSpecSchemaPattern`s by make/vehicletype/model/year, keeps only
/// those whose every `IsKey` pattern matches a decoded item of this pass, then
/// emits each non-key spec attribute for an element not already decoded
/// (modulo [`SPEC_EXEMPT`]), deduped to one row per element by latest ChangedOn.
fn append_vehicle_specs<'a>(
    db: &'a Db,
    items: &mut Vec<DecodingItem<'a>>,
    var_wmi: &str,
    model_year: Option<i32>,
) {
    // STEP 0: tVehicleType (element 39) and var_modelId (element 28). Either NULL
    // => the candidate join matches nothing, so no specs are produced.
    let Some(veh_type) = items
        .iter()
        .find(|it| it.element_id == 39)
        .and_then(|it| it.attribute_id.parse::<i32>().ok())
    else {
        return;
    };
    let Some(model_id) = items
        .iter()
        .find(|it| it.element_id == 28)
        .and_then(|it| it.attribute_id.parse::<i32>().ok())
    else {
        return;
    };

    // STEP 1: candidate (VSpecSchemaPattern id, schema id, tobeqced). A schema
    // qualifies on make in {wmi's makes}, vehicletype, a model row == model_id,
    // year (no year rows match any, else exact), tobeqced gate, and having >=1
    // key pattern. (includeNotPublicilyAvailable is false here, as in W1.)
    let makeids = db.makeids_for_wmi_str(var_wmi);
    struct Candidate {
        sp_id: i32,
        schema_id: i32,
        tobeqced: bool,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for &makeid in &makeids {
        for s in db.vspecschemas_for_make_model(makeid, model_id) {
            let schema_id = s.id.to_native();
            if s.vehicletypeid.to_native() != veh_type || s.tobeqced {
                continue;
            }
            let years = db.vspecschema_years_for(schema_id);
            let year_ok = match (years.is_empty(), model_year) {
                (true, _) => true,
                (false, Some(my)) => years.iter().any(|y| y.year.to_native() == my),
                (false, None) => false,
            };
            if !year_ok {
                continue;
            }
            for sp in db.vspecschemapatterns_for(schema_id) {
                if db
                    .vspecpatterns_for(sp.id.to_native())
                    .iter()
                    .any(|p| p.iskey)
                {
                    candidates.push(Candidate {
                        sp_id: sp.id.to_native(),
                        schema_id,
                        tobeqced: s.tobeqced,
                    });
                }
            }
        }
    }

    // STEP 2: key elimination. Keep a candidate iff cntTotal == cntMatch, where
    // cntTotal sums max(matches,1) over its key patterns (the left-join null-row)
    // and cntMatch is the count of distinct decoded items any key pattern matched.
    candidates.retain(|c| {
        let mut cnt_total = 0usize;
        let mut matched: IntSet<usize> = IntSet::default();
        for p in db.vspecpatterns_for(c.sp_id) {
            if !p.iskey {
                continue;
            }
            // Case-insensitive compare against the raw arena attribute id — no
            // lowercased copy of it (or of each candidate item) per comparison.
            let attr = db.s(p.attributeid.to_native());
            let mut n = 0usize;
            for (i, it) in items.iter().enumerate() {
                if it.element_id == p.elementid.to_native()
                    && it.attribute_id.eq_ignore_ascii_case(attr)
                {
                    matched.insert(i);
                    n += 1;
                }
            }
            cnt_total += n.max(1);
        }
        cnt_total == matched.len()
    });
    if candidates.is_empty() {
        return;
    }

    // STEP 3: non-key attributes for elements not already decoded (exempt set
    // never blocks). Emit one tbl1 row per surviving non-key pattern.
    let decoded_nonexempt: ElementSet = items
        .iter()
        .map(|it| it.element_id)
        .filter(|e| !SPEC_EXEMPT.contains(e))
        .collect();
    struct Tbl1<'a> {
        schema_id: i32,
        sp_id: i32,
        element_id: i32,
        attribute_id: Cow<'a, str>,
        changed_on: i64,
        tobeqced: bool,
    }
    let mut tbl1: Vec<Tbl1> = Vec::new();
    for c in &candidates {
        for p in db.vspecpatterns_for(c.sp_id) {
            if p.iskey || decoded_nonexempt.contains(&p.elementid.to_native()) {
                continue;
            }
            tbl1.push(Tbl1 {
                schema_id: c.schema_id,
                sp_id: c.sp_id,
                element_id: p.elementid.to_native(),
                attribute_id: Cow::Borrowed(db.s(p.attributeid.to_native())),
                changed_on: p.changedon_key.to_native(),
                tobeqced: c.tobeqced,
            });
        }
    }

    // STEP 4: dedup to one per element by latest ChangedOn. Ties (rare) break by
    // highest VSpecSchemaPattern id then highest schema id — deterministic.
    let mut best = ElementIndex::default();
    for (i, t) in tbl1.iter().enumerate() {
        match best.get(&t.element_id) {
            None => {
                best.insert(t.element_id, i);
            }
            Some(&b) => {
                let cur = &tbl1[b];
                let better = (t.changed_on, t.sp_id, t.schema_id)
                    > (cur.changed_on, cur.sp_id, cur.schema_id);
                if better {
                    best.insert(t.element_id, i);
                }
            }
        }
    }

    // STEP 5: emit the surviving spec items (value 'XXX', source 'Vehicle Specs').
    let mut keep: Vec<usize> = best.into_values().collect();
    keep.sort_unstable();
    for i in keep {
        let t = &tbl1[i];
        items.push(DecodingItem {
            created_on: t.changed_on,
            pattern_id: t.sp_id,
            keys: Cow::Borrowed(""),
            vin_schema_id: t.schema_id,
            wmi_id: NULL_I32,
            element_id: t.element_id,
            attribute_id: t.attribute_id.clone(),
            value: Cow::Borrowed("XXX"),
            source: Cow::Borrowed("Vehicle Specs"),
            priority: -100,
            to_be_qced: t.tobeqced,
        });
    }
}

/// DefaultValue (priority 10) for the decoded vehicle type, for absent elements.
fn append_default_values<'a>(db: &'a Db, items: &mut Vec<DecodingItem<'a>>) {
    let Some(veh) = items
        .iter()
        .find(|it| it.element_id == 39)
        .and_then(|it| it.attribute_id.parse::<i32>().ok())
    else {
        return;
    };
    let present: ElementSet = items.iter().map(|it| it.element_id).collect();
    for dv in db.default_templates_for(veh) {
        if present.contains(&dv.element_id) {
            continue;
        }
        items.push(DecodingItem {
            created_on: dv.created_on,
            pattern_id: NULL_I32,
            keys: Cow::Borrowed(""),
            vin_schema_id: NULL_I32,
            wmi_id: NULL_I32,
            element_id: dv.element_id,
            attribute_id: Cow::Borrowed(db.s(dv.attribute_id)),
            value: Cow::Borrowed(if dv.not_applicable {
                "Not Applicable"
            } else {
                "XXX"
            }),
            source: Cow::Borrowed("Default"),
            priority: 10,
            to_be_qced: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_keys_preserve_ascii_uppercase_semantics() {
        for key in ["", "ABC*|12", "abc[De]", "éaŁ", "日本語"] {
            assert_eq!(uppercase_key(key), key.to_ascii_uppercase());
        }
        assert!(matches!(uppercase_key("ABC*"), Cow::Borrowed(_)));
        assert!(matches!(uppercase_key("AbC*"), Cow::Owned(_)));
    }

    #[test]
    fn stack_var_keys_match_the_public_owned_result_at_every_length() {
        let vin = "1HGCM82633A004352EXTRA";
        for len in 0..=vin.len() {
            let input = &vin[..len];
            assert_eq!(
                build_var_keys_stack(input).as_str(),
                build_var_keys_reference(input)
            );
        }
        assert_eq!(build_var_keys_stack(vin).as_str(), "CM826|3A004352");
        assert_eq!(build_var_keys_stack("1HG").as_str(), "");
        assert_eq!(build_var_keys_stack("1HGCM8263").as_str(), "CM826");
    }

    #[test]
    fn static_model_year_text_matches_formatting_and_preserves_fallback_ownership() {
        for year in 1900..=2199 {
            let expected = year.to_string();
            assert_eq!(static_model_year_text(year), Some(expected.as_str()));
            let (attribute_id, value) = model_year_values(year);
            assert!(matches!(attribute_id, Cow::Borrowed(_)));
            assert!(matches!(value, Cow::Borrowed(_)));
            assert_eq!(attribute_id, expected);
            assert_eq!(value, expected);
        }

        for year in [i32::MIN, -1, 0, 1899, 2200, i32::MAX] {
            assert_eq!(static_model_year_text(year), None);
            let expected = year.to_string();
            let (attribute_id, value) = model_year_values(year);
            assert!(matches!(attribute_id, Cow::Owned(_)));
            assert!(matches!(value, Cow::Owned(_)));
            assert_eq!(attribute_id, expected);
            assert_eq!(value, expected);
        }
    }

    #[test]
    fn nested_and_panicking_scratch_users_do_not_hold_tls_borrows() {
        DECODE_SCRATCH.with(|slot| *slot.borrow_mut() = DecodeScratch::default());
        let mut outer = DecodeScratchGuard::take();
        outer.matched_patterns.reserve(32);
        {
            let mut nested = DecodeScratchGuard::take();
            assert!(nested.matched_patterns.is_empty());
            nested.distinct_makeids.reserve(8);
        }
        let panic = std::panic::catch_unwind(|| {
            let mut scratch = DecodeScratchGuard::take();
            scratch.schema_yearfrom.reserve(16);
            panic!("injected scratch user panic");
        });
        assert!(panic.is_err());
        drop(outer);
        let restored = DecodeScratchGuard::take();
        assert!(restored.matched_patterns.capacity() >= 32);
        assert!(restored.schema_yearfrom.capacity() >= 16);
        assert!(restored.distinct_makeids.capacity() >= 8);
    }

    #[test]
    fn scratch_reuse_preserves_duplicate_priority_and_output_order() {
        let embedded = Db::embedded();
        let artifact = std::fs::read(env!("ULTRAVIN_ARTIFACT")).expect("database artifact");
        let loaded = Db::from_bytes(&artifact).expect("independently loaded database");
        let now = 1_788_220_800_000_000;
        for vin in [
            "1FMAA50A91A111111",
            "1HGCM82633A004352",
            "5YJSA1E26HF000337",
        ] {
            let expected = embedded.decode_at(vin, None, now);
            for db in [embedded, &loaded, embedded, &loaded] {
                assert_eq!(db.decode_at(vin, None, now), expected);
            }
        }
    }

    #[test]
    fn oversized_scratch_capacity_is_not_retained() {
        {
            let mut scratch = DecodeScratchGuard::take();
            scratch
                .matched_patterns
                .reserve(MAX_RETAINED_MATCHED_PATTERNS + 1);
            scratch
                .schema_yearfrom
                .reserve(MAX_RETAINED_SCHEMA_PRIORITIES + 1);
            scratch.distinct_makeids.reserve(MAX_RETAINED_MAKE_IDS + 1);
        }
        let scratch = DecodeScratchGuard::take();
        assert!(scratch.matched_patterns.capacity() <= MAX_RETAINED_MATCHED_PATTERNS);
        assert!(scratch.schema_yearfrom.capacity() <= MAX_RETAINED_SCHEMA_PRIORITIES);
        assert!(scratch.distinct_makeids.capacity() <= MAX_RETAINED_MAKE_IDS);
    }

    #[test]
    fn pattern_scan_pool_is_reentrant_and_bounds_retained_hits() {
        PATTERN_SCAN_SCRATCH.with(|slot| *slot.borrow_mut() = PatternScanStorage::default());
        let outer = PatternScan::default();
        {
            let nested = PatternScan::default();
            assert!(nested.storage.schema_slots.is_empty());
            assert!(nested.storage.hit_vectors.is_empty());
        }
        drop(outer);
        {
            let mut oversized = PatternScan::default();
            oversized
                .storage
                .schema_slots
                .reserve(MAX_RETAINED_PATTERN_SCHEMAS + 1);
            oversized.storage.hit_vectors.push(Vec::new());
            oversized.storage.hit_vectors[0].reserve(MAX_RETAINED_PATTERN_HITS + 1);
        }
        let restored = PatternScan::default();
        assert!(restored.storage.schema_slots.capacity() <= MAX_RETAINED_PATTERN_SCHEMAS);
        assert!(
            restored
                .storage
                .hit_vectors
                .iter()
                .map(Vec::capacity)
                .sum::<usize>()
                <= MAX_RETAINED_PATTERN_HITS
        );
    }

    #[test]
    fn core_into_returns_caller_storage_on_missing_wmi() {
        let mut items = Vec::with_capacity(123);
        items.push(DecodingItem {
            created_on: 0,
            pattern_id: 0,
            keys: Cow::Borrowed("stale"),
            vin_schema_id: 0,
            wmi_id: 0,
            element_id: 0,
            attribute_id: Cow::Borrowed("stale"),
            value: Cow::Borrowed("stale"),
            source: Cow::Borrowed("stale"),
            priority: 0,
            to_be_qced: false,
        });
        let pointer = items.as_ptr();
        let mut scan = PatternScan::default();
        let result = decode_core_into(
            Db::embedded(),
            "___",
            None,
            "",
            None,
            DEFAULT_MODEL_YEAR_SOURCE,
            &mut scan,
            items,
        );
        assert!(!result.wmi_found);
        assert!(result.items.is_empty());
        assert_eq!(result.items.as_ptr(), pointer);
        assert!(result.items.capacity() >= 123);
    }
}

#[cfg(test)]
mod uppercase_name_tests {
    use super::*;

    #[test]
    fn names_preserve_unicode_uppercase_and_borrow_ascii_when_possible() {
        for name in [
            "",
            "HONDA",
            "Passenger Car",
            "Straße",
            "école",
            "İstanbul",
            "日本語",
        ] {
            assert_eq!(uppercase_name(name), name.to_uppercase());
        }
        assert!(matches!(uppercase_name("HONDA"), Cow::Borrowed(_)));
        assert!(matches!(uppercase_name("Straße"), Cow::Owned(_)));
    }
}
