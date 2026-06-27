//! The W1 decode core: pattern pass + layered sources + dedup + make + defaults,
//! in the exact order of `spvindecode_core`. Conversion / Formula Pattern /
//! Vehicle Specs are W2 and deliberately omitted.

use std::cmp::Ordering;

use crate::db::Db;
use crate::matcher::{like_match, regex_match};
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
    pub pattern_count: usize,
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
            pattern_count: 0,
        };
    };
    let wmiid = wmi.id;

    // --- Pattern pass: collect matches, then order globally by Pattern.Id ASC.
    let vkb = var_keys.as_bytes();
    let mut matched: Vec<&crate::tables::Pattern> = Vec::new();
    for wvs in db.wmi_vinschema_for(wmiid) {
        if let Some(my) = model_year {
            let to = if wvs.yearto == NULL_I32 {
                2999
            } else {
                wvs.yearto
            };
            if my < wvs.yearfrom || my > to {
                continue;
            }
        }
        let Some(vs) = db.vinschema_by_id(wvs.vinschemaid) else {
            continue;
        };
        if vs.tobeqced {
            continue;
        }
        for p in db.patterns_for(wvs.vinschemaid) {
            if matches!(p.elementid, 26 | 27 | 29 | 39) {
                continue;
            }
            let Some(e) = db.element_by_id(p.elementid) else {
                continue;
            };
            if !e.decode_present || e.isprivate {
                continue;
            }
            let hit = if p.has_bracket {
                regex_match(db.s(p.keys_regex), var_keys)
            } else {
                like_match(vkb, db.s(p.keys).as_bytes())
            };
            if hit {
                matched.push(p);
            }
        }
    }
    matched.sort_by_key(|p| p.id);
    let pattern_count = matched.len();
    for p in matched {
        items.push(DecodingItem {
            created_on: p.createdon_key,
            pattern_id: p.id,
            keys: db.s(p.keys).to_ascii_uppercase(),
            vin_schema_id: p.vinschemaid,
            wmi_id: wmiid,
            element_id: p.elementid,
            attribute_id: db.s(p.attributeid).to_string(),
            value: "XXX".to_string(),
            source: "Pattern".to_string(),
            priority: schema_year_from(db, wmiid, p.vinschemaid, model_year),
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
            for child in db.enginemodelpatterns_for(em.id) {
                items.push(DecodingItem {
                    created_on: child.createdon_key,
                    pattern_id,
                    keys: keys.clone(),
                    vin_schema_id,
                    wmi_id: wmiid,
                    element_id: child.elementid,
                    attribute_id: db.s(child.attributeid).to_string(),
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
    if wmi.vehicletypeid != NULL_I32 {
        if let Some(tag) = element_lookup_tag(39) {
            if let Some(name) = db.lookup(tag, wmi.vehicletypeid) {
                items.push(DecodingItem {
                    created_on: wmi.createdon_key,
                    pattern_id: NULL_I32,
                    keys: wmi_upper.clone(),
                    vin_schema_id: NULL_I32,
                    wmi_id: wmiid,
                    element_id: 39,
                    attribute_id: wmi.vehicletypeid.to_string(),
                    value: name.to_ascii_uppercase(),
                    source: "VehType".to_string(),
                    priority: 100,
                    to_be_qced: false,
                });
            }
        }
    }

    // --- (c)/(d) Manufacturer Name 27 and Id 157 (priority 100).
    let mfr_id = wmi.manufacturerid;
    if mfr_id != NULL_I32 {
        let mfr_name = element_lookup_tag(27)
            .and_then(|t| db.lookup(t, mfr_id))
            .map(|n| n.to_ascii_uppercase())
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

    // --- Dedup (once).
    dedup_per_element(&mut items);

    // --- Make 26 (post-dedup, never re-deduped).
    append_make(
        db,
        &mut items,
        wmiid,
        &wmi_upper,
        var_wmi,
        wmi.createdon_key,
    );

    // --- DefaultValue (priority 10).
    append_default_values(db, &mut items);

    CoreResult {
        items,
        wmi_found: true,
        pattern_count,
    }
}

/// The Pattern source priority is `Wmi_VinSchema.YearFrom`. Find the YearFrom
/// for `(wmiid, vinschemaid)` matching the model year window.
fn schema_year_from(db: &Db, wmiid: i32, vinschemaid: i32, model_year: Option<i32>) -> i32 {
    for wvs in db.wmi_vinschema_for(wmiid) {
        if wvs.vinschemaid != vinschemaid {
            continue;
        }
        if let Some(my) = model_year {
            let to = if wvs.yearto == NULL_I32 {
                2999
            } else {
                wvs.yearto
            };
            if my < wvs.yearfrom || my > to {
                continue;
            }
        }
        return wvs.yearfrom;
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

fn keys_no_brackets(keys: &str) -> String {
    keys.replace(['[', ']'], "")
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
        .then_with(|| keys_no_brackets(&a.keys).cmp(&keys_no_brackets(&b.keys)))
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
                let name = element_lookup_tag(26)
                    .and_then(|t| db.lookup(t, mm.makeid))
                    .map(|n| n.to_ascii_uppercase())
                    .unwrap_or_default();
                items.push(DecodingItem {
                    created_on: NULL_I64,
                    pattern_id,
                    keys: keys.clone(),
                    vin_schema_id,
                    wmi_id: NULL_I32,
                    element_id: 26,
                    attribute_id: mm.makeid.to_string(),
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
        let mut distinct: Vec<i32> = makes.iter().map(|m| m.makeid).collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() == 1 {
            let makeid = distinct[0];
            let name = element_lookup_tag(26)
                .and_then(|t| db.lookup(t, makeid))
                .map(|n| n.to_ascii_uppercase())
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
        if !dv.defaultvalue_present || present.contains(&dv.elementid) {
            continue;
        }
        let default_str = db.s(dv.defaultvalue);
        let is_lookup = db
            .element_by_id(dv.elementid)
            .map(|e| db.s(e.datatype).eq_ignore_ascii_case("lookup"))
            .unwrap_or(false);
        let value = if is_lookup && default_str == "0" {
            "Not Applicable".to_string()
        } else {
            "XXX".to_string()
        };
        to_add.push(DecodingItem {
            created_on: dv.createdon_key,
            pattern_id: NULL_I32,
            keys: String::new(),
            vin_schema_id: NULL_I32,
            wmi_id: NULL_I32,
            element_id: dv.elementid,
            attribute_id: default_str.to_string(),
            value,
            source: "Default".to_string(),
            priority: 10,
            to_be_qced: false,
        });
    }
    items.extend(to_add);
}
