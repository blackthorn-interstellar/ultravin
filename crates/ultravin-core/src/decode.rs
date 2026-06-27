//! The decode core, in the exact order of `spvindecode_core`: pattern pass,
//! layered sources, Formula Pattern, dedup, make, conversion, vehicle specs,
//! and defaults.

use std::cmp::Ordering;

use crate::db::Db;
use crate::matcher::{like_match, regex_match_cached};
use crate::tables::{element_lookup_tag, is_exempt, NULL_I32, NULL_I64};

/// A single decoding item (the `tblDecodingItem` ROW), pre-resolution.
#[derive(Debug, Clone)]
pub struct DecodingItem {
    pub created_on: i64, // NULL_I64 = none
    pub pattern_id: i32, // NULL_I32 = none
    pub keys: String,
    pub vin_schema_id: i32, // NULL_I32 = none
    pub wmi_id: i32,        // NULL_I32 = none
    pub element_id: i32,
    pub attribute_id: String,
    pub value: String,
    pub source: String,
    pub priority: i32,
    pub to_be_qced: bool,
}

/// Output of the core pass.
pub struct CoreResult {
    pub items: Vec<DecodingItem>,
    pub wmi_found: bool,
}

/// `var_keys = vin[3..8] || ('|' || vin[9..17])` (1-based 4-8 and 10-17).
pub fn build_var_keys(vin: &str) -> String {
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

/// Run the W1 decode core for `var_wmi` / `var_keys` / `model_year`.
pub fn decode_core(
    db: &Db,
    var_wmi: &str,
    var_keys: &str,
    model_year: Option<i32>,
    model_year_source: &str,
    now_micros: i64,
) -> CoreResult {
    let mut items: Vec<DecodingItem> = Vec::new();

    let Some(wmi) = db.wmi_by_str(var_wmi, now_micros) else {
        return CoreResult {
            items,
            wmi_found: false,
        };
    };
    let wmiid = wmi.id.to_native();

    // --- Pattern pass: collect matches, then order globally by Pattern.Id ASC.
    let vkb = var_keys.as_bytes();
    let mut matched: Vec<&crate::tables::ArchivedPattern> = Vec::new();
    for wvs in db.wmi_vinschema_for(wmiid) {
        if let Some(my) = model_year {
            let to = if wvs.yearto.to_native() == NULL_I32 {
                2999
            } else {
                wvs.yearto.to_native()
            };
            if my < wvs.yearfrom.to_native() || my > to {
                continue;
            }
        }
        let Some(vs) = db.vinschema_by_id(wvs.vinschemaid.to_native()) else {
            continue;
        };
        if vs.tobeqced {
            continue;
        }
        for p in db.patterns_for(wvs.vinschemaid.to_native()) {
            if matches!(p.elementid.to_native(), 26 | 27 | 29 | 39) {
                continue;
            }
            let Some(e) = db.element_by_id(p.elementid.to_native()) else {
                continue;
            };
            if !e.decode_present || e.isprivate {
                continue;
            }
            let hit = if p.has_bracket {
                let rid = p.keys_regex.to_native();
                regex_match_cached(rid, db.s(rid), var_keys)
            } else {
                like_match(vkb, db.s(p.keys.to_native()).as_bytes())
            };
            if hit {
                matched.push(p);
            }
        }
    }
    matched.sort_by_key(|p| p.id.to_native());
    for p in matched {
        items.push(DecodingItem {
            created_on: p.createdon_key.to_native(),
            pattern_id: p.id.to_native(),
            keys: db.s(p.keys.to_native()).to_ascii_uppercase(),
            vin_schema_id: p.vinschemaid.to_native(),
            wmi_id: wmiid,
            element_id: p.elementid.to_native(),
            attribute_id: db.s(p.attributeid.to_native()).to_string(),
            value: "XXX".to_string(),
            source: "Pattern".to_string(),
            priority: schema_year_from(db, wmiid, p.vinschemaid.to_native(), model_year),
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
                    attribute_id: db.s(child.attributeid.to_native()).to_string(),
                    value: "XXX".to_string(),
                    source: "EngineModelPattern".to_string(),
                    priority: 50,
                    to_be_qced: false,
                });
            }
        }
    }

    let wmi_upper = var_wmi.to_ascii_uppercase();

    // --- (b) VehType 39 (priority 100).
    let veh_type_id = wmi.vehicletypeid.to_native();
    if veh_type_id != NULL_I32 {
        if let Some(tag) = element_lookup_tag(39) {
            if let Some(name) = db.lookup(tag, veh_type_id) {
                items.push(DecodingItem {
                    created_on: wmi.createdon_key.to_native(),
                    pattern_id: NULL_I32,
                    keys: wmi_upper.clone(),
                    vin_schema_id: NULL_I32,
                    wmi_id: wmiid,
                    element_id: 39,
                    attribute_id: veh_type_id.to_string(),
                    value: name.to_uppercase(),
                    source: "VehType".to_string(),
                    priority: 100,
                    to_be_qced: false,
                });
            }
        }
    }

    // --- (c)/(d) Manufacturer Name 27 and Id 157 (priority 100).
    let mfr_id = wmi.manufacturerid.to_native();
    if mfr_id != NULL_I32 {
        let mfr_name = element_lookup_tag(27)
            .and_then(|t| db.lookup(t, mfr_id))
            .map(|n| n.to_uppercase())
            .unwrap_or_default();
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: wmi_upper.clone(),
            vin_schema_id: NULL_I32,
            wmi_id: wmiid,
            element_id: 27,
            attribute_id: mfr_id.to_string(),
            value: mfr_name,
            source: "Manuf. Name".to_string(),
            priority: 100,
            to_be_qced: false,
        });
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: wmi_upper.clone(),
            vin_schema_id: NULL_I32,
            wmi_id: wmiid,
            element_id: 157,
            attribute_id: mfr_id.to_string(),
            value: mfr_id.to_string(),
            source: "Manuf. Id".to_string(),
            priority: 100,
            to_be_qced: false,
        });
    }

    // --- (e) ModelYear 29 (priority 100).
    if let Some(my) = model_year {
        items.push(DecodingItem {
            created_on: NULL_I64,
            pattern_id: NULL_I32,
            keys: model_year_source.to_string(),
            vin_schema_id: NULL_I32,
            wmi_id: NULL_I32,
            element_id: 29,
            attribute_id: my.to_string(),
            value: my.to_string(),
            source: "ModelYear".to_string(),
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
        &wmi_upper,
        var_wmi,
        wmi.createdon_key.to_native(),
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
fn append_formula_patterns(
    db: &Db,
    items: &mut Vec<DecodingItem>,
    wmiid: i32,
    var_keys: &str,
    model_year: Option<i32>,
) {
    let formula_keys: String = var_keys
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    let fk = formula_keys.as_bytes();
    let mut seen_vs: std::collections::HashSet<i32> = std::collections::HashSet::new();
    let mut new_items: Vec<DecodingItem> = Vec::new();
    for wvs in db.wmi_vinschema_for(wmiid) {
        if let Some(my) = model_year {
            let to = if wvs.yearto.to_native() == NULL_I32 {
                2999
            } else {
                wvs.yearto.to_native()
            };
            if my < wvs.yearfrom.to_native() || my > to {
                continue;
            }
        }
        let vsid = wvs.vinschemaid.to_native();
        if !seen_vs.insert(vsid) {
            continue;
        }
        for p in db.patterns_for(vsid) {
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
            if !like_match(fk, keys.as_bytes()) {
                continue;
            }
            new_items.push(DecodingItem {
                created_on: p.createdon_key.to_native(),
                pattern_id: p.id.to_native(),
                keys: keys.to_string(),
                vin_schema_id: vsid,
                wmi_id: NULL_I32,
                element_id: p.elementid.to_native(),
                attribute_id: db.s(p.attributeid.to_native()).to_string(),
                value: formula_value(var_keys, keys),
                source: "Formula Pattern".to_string(),
                priority: 100,
                to_be_qced: false,
            });
        }
    }
    items.extend(new_items);
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

/// The Pattern source priority is `Wmi_VinSchema.YearFrom`. Find the YearFrom
/// for `(wmiid, vinschemaid)` matching the model year window.
fn schema_year_from(db: &Db, wmiid: i32, vinschemaid: i32, model_year: Option<i32>) -> i32 {
    for wvs in db.wmi_vinschema_for(wmiid) {
        if wvs.vinschemaid.to_native() != vinschemaid {
            continue;
        }
        if let Some(my) = model_year {
            let to = if wvs.yearto.to_native() == NULL_I32 {
                2999
            } else {
                wvs.yearto.to_native()
            };
            if my < wvs.yearfrom.to_native() || my > to {
                continue;
            }
        }
        return wvs.yearfrom.to_native();
    }
    0
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

fn len_no_star(keys: &str) -> usize {
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
    use std::collections::HashMap;
    // best index per element id (lowest under the comparator).
    let mut best: HashMap<i32, usize> = HashMap::new();
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
    let mut keep = vec![true; items.len()];
    for (i, it) in items.iter().enumerate() {
        if is_exempt(it.element_id) {
            continue;
        }
        if best.get(&it.element_id) != Some(&i) {
            keep[i] = false;
        }
    }
    let mut idx = 0;
    items.retain(|_| {
        let k = keep[idx];
        idx += 1;
        k
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
fn append_make(
    db: &Db,
    items: &mut Vec<DecodingItem>,
    wmiid: i32,
    wmi_upper: &str,
    var_wmi: &str,
    wmi_created: i64,
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
        let _ = wmi_upper;
        if let Ok(modelid) = model_attr.parse::<i32>() {
            for mm in db.makes_for_model(modelid) {
                let makeid = mm.makeid.to_native();
                let name = element_lookup_tag(26)
                    .and_then(|t| db.lookup(t, makeid))
                    .map(|n| n.to_uppercase())
                    .unwrap_or_default();
                items.push(DecodingItem {
                    created_on: NULL_I64,
                    pattern_id,
                    keys: keys.clone(),
                    vin_schema_id,
                    wmi_id: NULL_I32,
                    element_id: 26,
                    attribute_id: makeid.to_string(),
                    value: name,
                    source: "pattern - model".to_string(),
                    priority: 1000,
                    to_be_qced: false,
                });
            }
        }
    } else {
        // single distinct public make via wmi_make
        let makes = db.wmi_makes_for(wmiid);
        let mut distinct: Vec<i32> = makes.iter().map(|m| m.makeid.to_native()).collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() == 1 {
            let makeid = distinct[0];
            let name = element_lookup_tag(26)
                .and_then(|t| db.lookup(t, makeid))
                .map(|n| n.to_uppercase())
                .unwrap_or_default();
            items.push(DecodingItem {
                created_on: wmi_created,
                pattern_id: NULL_I32,
                keys: var_wmi.to_string(),
                vin_schema_id: NULL_I32,
                wmi_id: wmiid,
                element_id: 26,
                attribute_id: makeid.to_string(),
                value: name,
                source: "Make".to_string(),
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
fn append_conversions(db: &Db, items: &mut Vec<DecodingItem>) {
    struct Row<'a> {
        priority: i32,
        created_on: i64,
        conv_id: i32,
        to_elem: i32,
        formula: &'a str,
        value: String,
        keys: String,
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
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then(created_desc_nulls_first(a.created_on, b.created_on))
            .then(a.conv_id.cmp(&b.conv_id))
    });

    let mut present: std::collections::HashSet<i32> =
        items.iter().map(|it| it.element_id).collect();
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
            attribute_id: result.clone(),
            value: result,
            source,
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
fn append_vehicle_specs(
    db: &Db,
    items: &mut Vec<DecodingItem>,
    var_wmi: &str,
    model_year: Option<i32>,
) {
    use std::collections::{HashMap, HashSet};

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
        for s in db.vspecschemas_for_make(makeid) {
            let schema_id = s.id.to_native();
            if s.vehicletypeid.to_native() != veh_type || s.tobeqced {
                continue;
            }
            if !db
                .vspecschema_models_for(schema_id)
                .iter()
                .any(|m| m.modelid.to_native() == model_id)
            {
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
        let mut matched: HashSet<usize> = HashSet::new();
        for p in db.vspecpatterns_for(c.sp_id) {
            if !p.iskey {
                continue;
            }
            let attr = db.s(p.attributeid.to_native()).to_ascii_lowercase();
            let mut n = 0usize;
            for (i, it) in items.iter().enumerate() {
                if it.element_id == p.elementid.to_native()
                    && it.attribute_id.to_ascii_lowercase() == attr
                {
                    matched.insert(i);
                    n += 1;
                }
            }
            cnt_total += n.max(1);
        }
        cnt_total == matched.len()
    });

    // STEP 3: non-key attributes for elements not already decoded (exempt set
    // never blocks). Emit one tbl1 row per surviving non-key pattern.
    let decoded_nonexempt: HashSet<i32> = items
        .iter()
        .map(|it| it.element_id)
        .filter(|e| !SPEC_EXEMPT.contains(e))
        .collect();
    struct Tbl1 {
        schema_id: i32,
        sp_id: i32,
        element_id: i32,
        attribute_id: String,
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
                attribute_id: db.s(p.attributeid.to_native()).to_string(),
                changed_on: p.changedon_key.to_native(),
                tobeqced: c.tobeqced,
            });
        }
    }

    // STEP 4: dedup to one per element by latest ChangedOn. Ties (rare) break by
    // highest VSpecSchemaPattern id then highest schema id — deterministic.
    let mut best: HashMap<i32, usize> = HashMap::new();
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
            keys: String::new(),
            vin_schema_id: t.schema_id,
            wmi_id: NULL_I32,
            element_id: t.element_id,
            attribute_id: t.attribute_id.clone(),
            value: "XXX".to_string(),
            source: "Vehicle Specs".to_string(),
            priority: -100,
            to_be_qced: t.tobeqced,
        });
    }
}

/// DefaultValue (priority 10) for the decoded vehicle type, for absent elements.
fn append_default_values(db: &Db, items: &mut Vec<DecodingItem>) {
    let Some(veh) = items
        .iter()
        .find(|it| it.element_id == 39)
        .and_then(|it| it.attribute_id.parse::<i32>().ok())
    else {
        return;
    };
    let present: std::collections::HashSet<i32> = items.iter().map(|it| it.element_id).collect();
    let mut to_add: Vec<DecodingItem> = Vec::new();
    for dv in db.defaultvalues_for(veh) {
        let element_id = dv.elementid.to_native();
        if !dv.defaultvalue_present || present.contains(&element_id) {
            continue;
        }
        let default_str = db.s(dv.defaultvalue.to_native());
        let is_lookup = db
            .element_by_id(element_id)
            .map(|e| db.s(e.datatype.to_native()).eq_ignore_ascii_case("lookup"))
            .unwrap_or(false);
        let value = if is_lookup && default_str == "0" {
            "Not Applicable".to_string()
        } else {
            "XXX".to_string()
        };
        to_add.push(DecodingItem {
            created_on: dv.createdon_key.to_native(),
            pattern_id: NULL_I32,
            keys: String::new(),
            vin_schema_id: NULL_I32,
            wmi_id: NULL_I32,
            element_id,
            attribute_id: default_str.to_string(),
            value,
            source: "Default".to_string(),
            priority: 10,
            to_be_qced: false,
        });
    }
    items.extend(to_add);
}
