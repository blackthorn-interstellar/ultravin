//! Deterministic artifact builder: parse the decode-critical `COPY` tables from
//! the dump, sort each into a total order, intern strings first-seen over the
//! sorted traversal, and serialize the rkyv [`VpicData`] with a content-addressed
//! header. The output is a pure function of the dump bytes.

use std::collections::HashMap;
use std::path::Path;

use ultravin_core::sqlwild_to_regex;
use ultravin_core::tables::{
    serialize_artifact, tag_of_table, Conversion, DefaultValue, Element, EngineModel,
    EngineModelPattern, LookupRow, MakeModel, Pattern, VSpecPattern, VSpecSchema, VSpecSchemaModel,
    VSpecSchemaPattern, VSpecSchemaYear, VinException, VinSchema, VpicData, Wmi, WmiMake,
    WmiVinSchema, NULL_I32, NULL_I64,
};

/// Tables that get their own typed array (vs. the generic lookups).
const DEDICATED: &[&str] = &[
    "wmi",
    "wmi_vinschema",
    "vinschema",
    "pattern",
    "element",
    "make_model",
    "wmi_make",
    "enginemodel",
    "enginemodelpattern",
    "defaultvalue",
    "vinexception",
    "conversion",
    "vehiclespecschema",
    "vspecschemapattern",
    "vehiclespecpattern",
    "vehiclespecschema_model",
    "vehiclespecschema_year",
];

// Raw (pre-intern) rows — owned strings, parsed numerics.
struct RWmi {
    id: i32,
    wmi: String,
    manufacturerid: i32,
    makeid: i32,
    vehicletypeid: i32,
    trucktypeid: i32,
    pad: i64,
    createdon_key: i64,
}
struct RPattern {
    id: i32,
    vinschemaid: i32,
    keys: String,
    elementid: i32,
    attributeid: String,
    createdon_key: i64,
}
struct RElement {
    id: i32,
    name: String,
    code: String,
    isprivate: bool,
    groupname: String,
    datatype: String,
    decode: String,
    decode_present: bool,
    weight: i32,
}
struct REngineModel {
    id: i32,
    name: String,
}
struct REmp {
    id: i32,
    enginemodelid: i32,
    elementid: i32,
    attributeid: String,
    createdon_key: i64,
}
struct RDefault {
    id: i32,
    elementid: i32,
    vehicletypeid: i32,
    defaultvalue: String,
    defaultvalue_present: bool,
    createdon_key: i64,
}
struct RVinExc {
    vin: String,
    checkdigit: bool,
}
struct RConversion {
    id: i32,
    fromelementid: i32,
    toelementid: i32,
    formula: String,
}
struct RVSpecPattern {
    id: i32,
    vspecschemapatternid: i32,
    iskey: bool,
    elementid: i32,
    attributeid: String,
    changedon_key: i64,
}

#[derive(Default)]
pub struct ArtifactBuilder {
    wmi: Vec<RWmi>,
    wmi_vinschema: Vec<WmiVinSchema>,
    vinschema: Vec<VinSchema>,
    pattern: Vec<RPattern>,
    element: Vec<RElement>,
    make_model: Vec<MakeModel>,
    wmi_make: Vec<WmiMake>,
    enginemodel: Vec<REngineModel>,
    enginemodelpattern: Vec<REmp>,
    defaultvalue: Vec<RDefault>,
    vinexception: Vec<RVinExc>,
    conversion: Vec<RConversion>,
    vspecschema: Vec<VSpecSchema>,
    vspecschemapattern: Vec<VSpecSchemaPattern>,
    vspecpattern: Vec<RVSpecPattern>,
    vspecschemamodel: Vec<VSpecSchemaModel>,
    vspecschemayear: Vec<VSpecSchemaYear>,
    lookups: Vec<(u16, i32, String)>,
    cur: Option<Ctx>,
}

struct Ctx {
    table: String,
    cols: Vec<String>,
    tag: Option<u16>,
}

/// Column names from a `COPY vpic.t (a, b, c) FROM stdin;` line.
pub fn copy_columns(line: &str) -> Vec<String> {
    let Some(start) = line.find('(') else {
        return Vec::new();
    };
    let Some(end) = line.find(')') else {
        return Vec::new();
    };
    line[start + 1..end]
        .split(',')
        .map(|s| s.trim().to_string())
        .collect()
}

/// Unescape a single COPY text field (`\t \n \r \\`); `\N` is handled upstream.
fn unescape(s: &str) -> String {
    if !s.contains('\\') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some(other) => out.push(other),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn civil_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse `YYYY-MM-DD HH:MM:SS[.ffffff]` to epoch micros; `None` -> NULL sentinel.
fn parse_ts(s: Option<&str>) -> i64 {
    let Some(s) = s else { return NULL_I64 };
    let s = s.trim();
    let (date, time) = match s.split_once(' ') {
        Some(p) => p,
        None => (s, "00:00:00"),
    };
    let dp: Vec<&str> = date.split('-').collect();
    if dp.len() != 3 {
        return NULL_I64;
    }
    let (y, mo, d) = (
        dp[0].parse::<i64>().unwrap_or(0),
        dp[1].parse::<i64>().unwrap_or(0),
        dp[2].parse::<i64>().unwrap_or(0),
    );
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, ""),
    };
    let tp: Vec<&str> = hms.split(':').collect();
    let (h, mi, se) = (
        tp.first().and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
        tp.get(1).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
        tp.get(2).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0),
    );
    let mut micros_frac = 0i64;
    if !frac.is_empty() {
        let mut f = frac.to_string();
        f.truncate(6);
        while f.len() < 6 {
            f.push('0');
        }
        micros_frac = f.parse::<i64>().unwrap_or(0);
    }
    let days = civil_to_days(y, mo, d);
    (days * 86_400 + h * 3_600 + mi * 60 + se) * 1_000_000 + micros_frac
}

impl ArtifactBuilder {
    /// Begin a `COPY` block; records columns when the table is decode-critical.
    pub fn begin_copy(&mut self, table: &str, line: &str) {
        let tag = tag_of_table(table);
        if DEDICATED.contains(&table) || tag.is_some() {
            self.cur = Some(Ctx {
                table: table.to_string(),
                cols: copy_columns(line),
                tag,
            });
        } else {
            self.cur = None;
        }
    }

    /// Feed one data row line (tab-separated COPY body).
    pub fn feed(&mut self, line: &str) {
        let Some(ctx) = self.cur.as_ref() else { return };
        let fields: Vec<&str> = line.split('\t').collect();
        let get = |name: &str| -> Option<String> {
            ctx.cols
                .iter()
                .position(|c| c == name)
                .and_then(|i| fields.get(i))
                .and_then(|v| if *v == "\\N" { None } else { Some(unescape(v)) })
        };
        let geti = |name: &str| get(name).and_then(|v| v.parse::<i32>().ok());
        let geti_or = |name: &str, d: i32| geti(name).unwrap_or(d);
        let created_key = |c: &str, u: &str| {
            let upd = get(u);
            if upd.is_some() {
                parse_ts(upd.as_deref())
            } else {
                parse_ts(get(c).as_deref())
            }
        };

        match ctx.table.as_str() {
            "wmi" => self.wmi.push(RWmi {
                id: geti_or("id", 0),
                wmi: get("wmi").unwrap_or_default(),
                manufacturerid: geti_or("manufacturerid", NULL_I32),
                makeid: geti_or("makeid", NULL_I32),
                vehicletypeid: geti_or("vehicletypeid", NULL_I32),
                trucktypeid: geti_or("trucktypeid", NULL_I32),
                pad: parse_ts(get("publicavailabilitydate").as_deref()),
                createdon_key: created_key("createdon", "updatedon"),
            }),
            "wmi_vinschema" => self.wmi_vinschema.push(WmiVinSchema {
                id: geti_or("id", 0),
                wmiid: geti_or("wmiid", 0),
                vinschemaid: geti_or("vinschemaid", 0),
                yearfrom: geti_or("yearfrom", 0),
                yearto: geti_or("yearto", NULL_I32),
            }),
            "vinschema" => self.vinschema.push(VinSchema {
                id: geti_or("id", 0),
                tobeqced: get("tobeqced").as_deref() == Some("t"),
            }),
            "pattern" => self.pattern.push(RPattern {
                id: geti_or("id", 0),
                vinschemaid: geti_or("vinschemaid", 0),
                keys: get("keys").unwrap_or_default(),
                elementid: geti_or("elementid", 0),
                attributeid: get("attributeid").unwrap_or_default(),
                createdon_key: created_key("createdon", "updatedon"),
            }),
            "element" => {
                let decode = get("decode");
                self.element.push(RElement {
                    id: geti_or("id", 0),
                    name: get("name").unwrap_or_default(),
                    code: get("code").unwrap_or_default(),
                    isprivate: get("isprivate").as_deref() == Some("t"),
                    groupname: get("groupname").unwrap_or_default(),
                    datatype: get("datatype").unwrap_or_default(),
                    decode_present: decode.is_some(),
                    decode: decode.unwrap_or_default(),
                    weight: geti_or("weight", NULL_I32),
                })
            }
            "make_model" => self.make_model.push(MakeModel {
                makeid: geti_or("makeid", 0),
                modelid: geti_or("modelid", 0),
            }),
            "wmi_make" => self.wmi_make.push(WmiMake {
                wmiid: geti_or("wmiid", 0),
                makeid: geti_or("makeid", 0),
            }),
            "enginemodel" => self.enginemodel.push(REngineModel {
                id: geti_or("id", 0),
                name: get("name").unwrap_or_default(),
            }),
            "enginemodelpattern" => self.enginemodelpattern.push(REmp {
                id: geti_or("id", 0),
                enginemodelid: geti_or("enginemodelid", 0),
                elementid: geti_or("elementid", 0),
                attributeid: get("attributeid").unwrap_or_default(),
                createdon_key: created_key("createdon", "updatedon"),
            }),
            "defaultvalue" => {
                let dv = get("defaultvalue");
                self.defaultvalue.push(RDefault {
                    id: geti_or("id", 0),
                    elementid: geti_or("elementid", 0),
                    vehicletypeid: geti_or("vehicletypeid", 0),
                    defaultvalue_present: dv.is_some(),
                    defaultvalue: dv.unwrap_or_default(),
                    createdon_key: created_key("createdon", "updatedon"),
                })
            }
            "vinexception" => self.vinexception.push(RVinExc {
                vin: get("vin").unwrap_or_default(),
                checkdigit: get("checkdigit").as_deref() == Some("t"),
            }),
            "conversion" => self.conversion.push(RConversion {
                id: geti_or("id", 0),
                fromelementid: geti_or("fromelementid", 0),
                toelementid: geti_or("toelementid", 0),
                formula: get("formula").unwrap_or_default(),
            }),
            "vehiclespecschema" => self.vspecschema.push(VSpecSchema {
                id: geti_or("id", 0),
                makeid: geti_or("makeid", NULL_I32),
                vehicletypeid: geti_or("vehicletypeid", NULL_I32),
                tobeqced: get("tobeqced").as_deref() == Some("t"),
            }),
            "vspecschemapattern" => self.vspecschemapattern.push(VSpecSchemaPattern {
                id: geti_or("id", 0),
                schemaid: geti_or("schemaid", 0),
            }),
            "vehiclespecpattern" => self.vspecpattern.push(RVSpecPattern {
                id: geti_or("id", 0),
                vspecschemapatternid: geti_or("vspecschemapatternid", 0),
                iskey: get("iskey").as_deref() == Some("t"),
                elementid: geti_or("elementid", 0),
                attributeid: get("attributeid").unwrap_or_default(),
                changedon_key: created_key("createdon", "updatedon"),
            }),
            "vehiclespecschema_model" => self.vspecschemamodel.push(VSpecSchemaModel {
                schemaid: geti_or("vehiclespecschemaid", 0),
                modelid: geti_or("modelid", 0),
            }),
            "vehiclespecschema_year" => self.vspecschemayear.push(VSpecSchemaYear {
                schemaid: geti_or("vehiclespecschemaid", 0),
                year: geti_or("year", 0),
            }),
            _ => {}
        }

        // Lookup tables also feed the generic (tag,id,name) array.
        if let Some(tag) = ctx.tag {
            if let (Some(id), Some(name)) = (geti("id"), get("name")) {
                self.lookups.push((tag, id, name));
            }
        }
    }

    /// End the current `COPY` block.
    pub fn end_copy(&mut self) {
        self.cur = None;
    }

    /// Sort, intern, serialize. Returns (artifact bytes, blake3 hex).
    pub fn build(mut self, builder_version: u32) -> (Vec<u8>, String) {
        // --- Total-order sorts.
        self.wmi
            .sort_by(|a, b| a.wmi.cmp(&b.wmi).then(a.id.cmp(&b.id)));
        self.wmi_vinschema
            .sort_by(|a, b| a.wmiid.cmp(&b.wmiid).then(a.id.cmp(&b.id)));
        self.vinschema.sort_by_key(|v| v.id);
        self.pattern
            .sort_by(|a, b| a.vinschemaid.cmp(&b.vinschemaid).then(a.id.cmp(&b.id)));
        self.element.sort_by_key(|e| e.id);
        self.make_model
            .sort_by(|a, b| a.modelid.cmp(&b.modelid).then(a.makeid.cmp(&b.makeid)));
        self.wmi_make
            .sort_by(|a, b| a.wmiid.cmp(&b.wmiid).then(a.makeid.cmp(&b.makeid)));
        self.enginemodel.sort_by_key(|e| e.id);
        self.enginemodelpattern
            .sort_by(|a, b| a.enginemodelid.cmp(&b.enginemodelid).then(a.id.cmp(&b.id)));
        self.defaultvalue
            .sort_by(|a, b| a.vehicletypeid.cmp(&b.vehicletypeid).then(a.id.cmp(&b.id)));
        self.vinexception.sort_by(|a, b| a.vin.cmp(&b.vin));
        self.conversion.sort_by_key(|c| c.id);
        self.lookups
            .sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        self.vspecschema
            .sort_by(|a, b| a.makeid.cmp(&b.makeid).then(a.id.cmp(&b.id)));
        self.vspecschemapattern
            .sort_by(|a, b| a.schemaid.cmp(&b.schemaid).then(a.id.cmp(&b.id)));
        self.vspecpattern.sort_by(|a, b| {
            a.vspecschemapatternid
                .cmp(&b.vspecschemapatternid)
                .then(a.id.cmp(&b.id))
        });
        self.vspecschemamodel
            .sort_by(|a, b| a.schemaid.cmp(&b.schemaid).then(a.modelid.cmp(&b.modelid)));
        self.vspecschemayear
            .sort_by(|a, b| a.schemaid.cmp(&b.schemaid).then(a.year.cmp(&b.year)));

        // --- First-seen interner over the sorted traversal in fixed table order.
        let mut intern = Interner::new();

        let wmi: Vec<Wmi> = self
            .wmi
            .iter()
            .map(|r| Wmi {
                id: r.id,
                wmi: intern.get(&r.wmi),
                manufacturerid: r.manufacturerid,
                makeid: r.makeid,
                vehicletypeid: r.vehicletypeid,
                trucktypeid: r.trucktypeid,
                publicavailabilitydate: r.pad,
                createdon_key: r.createdon_key,
            })
            .collect();

        let pattern: Vec<Pattern> = self
            .pattern
            .iter()
            .map(|r| {
                let keys = intern.get(&r.keys);
                let has_bracket = r.keys.contains('[');
                // Only bracket-class patterns are matched via regex; plain
                // patterns use LIKE and never read `keys_regex`. Skip interning
                // the derived regex for them so it doesn't bloat the arena.
                let keys_regex = if has_bracket {
                    intern.get(&sqlwild_to_regex(&r.keys))
                } else {
                    0
                };
                let attributeid = intern.get(&r.attributeid);
                Pattern {
                    id: r.id,
                    vinschemaid: r.vinschemaid,
                    keys,
                    keys_regex,
                    elementid: r.elementid,
                    attributeid,
                    createdon_key: r.createdon_key,
                    specificity: r.keys.chars().filter(|c| *c != '*').count() as u8,
                    has_bracket,
                }
            })
            .collect();

        let element: Vec<Element> = self
            .element
            .iter()
            .map(|r| Element {
                id: r.id,
                name: intern.get(&r.name),
                code: intern.get(&r.code),
                isprivate: r.isprivate,
                groupname: intern.get(&r.groupname),
                datatype: intern.get(&r.datatype),
                decode: intern.get(&r.decode),
                decode_present: r.decode_present,
                weight: r.weight,
            })
            .collect();

        let enginemodel: Vec<EngineModel> = self
            .enginemodel
            .iter()
            .map(|r| EngineModel {
                id: r.id,
                name: intern.get(&r.name),
            })
            .collect();

        let enginemodelpattern: Vec<EngineModelPattern> = self
            .enginemodelpattern
            .iter()
            .map(|r| EngineModelPattern {
                id: r.id,
                enginemodelid: r.enginemodelid,
                elementid: r.elementid,
                attributeid: intern.get(&r.attributeid),
                createdon_key: r.createdon_key,
            })
            .collect();

        let defaultvalue: Vec<DefaultValue> = self
            .defaultvalue
            .iter()
            .map(|r| DefaultValue {
                id: r.id,
                elementid: r.elementid,
                vehicletypeid: r.vehicletypeid,
                defaultvalue: intern.get(&r.defaultvalue),
                defaultvalue_present: r.defaultvalue_present,
                createdon_key: r.createdon_key,
            })
            .collect();

        let vinexception: Vec<VinException> = self
            .vinexception
            .iter()
            .map(|r| VinException {
                vin: intern.get(&r.vin),
                checkdigit: r.checkdigit,
            })
            .collect();

        let conversion: Vec<Conversion> = self
            .conversion
            .iter()
            .map(|r| Conversion {
                id: r.id,
                fromelementid: r.fromelementid,
                toelementid: r.toelementid,
                formula: intern.get(&r.formula),
            })
            .collect();

        let lookups: Vec<LookupRow> = self
            .lookups
            .iter()
            .map(|(tag, id, name)| LookupRow {
                tag: *tag,
                id: *id,
                name: intern.get(name),
            })
            .collect();

        let vspecpattern: Vec<VSpecPattern> = self
            .vspecpattern
            .iter()
            .map(|r| VSpecPattern {
                id: r.id,
                vspecschemapatternid: r.vspecschemapatternid,
                iskey: r.iskey,
                elementid: r.elementid,
                attributeid: intern.get(&r.attributeid),
                changedon_key: r.changedon_key,
            })
            .collect();

        let data = VpicData {
            arena_bytes: intern.bytes,
            arena_offsets: intern.offsets,
            wmi,
            wmi_vinschema: self.wmi_vinschema,
            vinschema: self.vinschema,
            pattern,
            element,
            make_model: self.make_model,
            wmi_make: self.wmi_make,
            enginemodel,
            enginemodelpattern,
            defaultvalue,
            vinexception,
            conversion,
            lookups,
            vspecschema: self.vspecschema,
            vspecschemapattern: self.vspecschemapattern,
            vspecpattern,
            vspecschemamodel: self.vspecschemamodel,
            vspecschemayear: self.vspecschemayear,
        };

        let bytes = serialize_artifact(&data, builder_version);
        let hex = ultravin_core::tables::artifact_blake3_hex(&bytes);
        (bytes, hex)
    }
}

struct Interner {
    map: HashMap<String, u32>,
    bytes: Vec<u8>,
    offsets: Vec<u32>,
}

impl Interner {
    fn new() -> Self {
        Interner {
            map: HashMap::new(),
            bytes: Vec::new(),
            offsets: vec![0],
        }
    }
    fn get(&mut self, s: &str) -> u32 {
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let id = (self.offsets.len() - 1) as u32;
        self.bytes.extend_from_slice(s.as_bytes());
        self.offsets.push(self.bytes.len() as u32);
        self.map.insert(s.to_string(), id);
        id
    }
}

/// Build the artifact and write it to `path` (creating parents).
pub fn write_artifact(
    builder: ArtifactBuilder,
    path: &Path,
    builder_version: u32,
) -> std::io::Result<(usize, String)> {
    let (bytes, hex) = builder.build(builder_version);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, &bytes)?;
    Ok((bytes.len(), hex))
}
