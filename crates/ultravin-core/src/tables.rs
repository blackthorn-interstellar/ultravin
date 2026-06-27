//! The single source of truth for the embedded artifact schema.
//!
//! Struct-of-arrays, all sorted `Vec`s — no `HashMap` ever reaches the archive,
//! so identical input bytes produce a byte-identical archive (stable blake3).
//! Strings live in one shared arena and are referenced by a `u32` offset id.
//!
//! Shared by `ultravin-build` (which constructs and serializes it) and
//! `ultravin-core::db` (which loads and queries it). One definition, one format.

use rkyv::{Archive, Deserialize, Serialize};

/// Sentinel for a NULL `i32` column (real ids are positive).
pub const NULL_I32: i32 = i32::MIN;
/// Sentinel for a NULL `i64` timestamp column.
pub const NULL_I64: i64 = i64::MIN;

/// Element ids exempt from per-element dedup (kept even when duplicated).
pub const EXEMPT_ELEMENTS: [i32; 8] = [114, 121, 129, 150, 154, 155, 169, 186];

/// `true` if `element_id` skips the dedup pass.
pub fn is_exempt(element_id: i32) -> bool {
    EXEMPT_ELEMENTS.contains(&element_id)
}

/// Lookup source tables, in a fixed canonical order; the index is the `tag`.
/// (NCSA views 96/97/98 are W2 — deferred.)
pub const LOOKUP_TABLES: &[&str] = &[
    "batterytype",
    "bedtype",
    "bodycab",
    "bodystyle",
    "destinationmarket",
    "drivetype",
    "entertainmentsystem",
    "fueltype",
    "grossvehicleweightrating",
    "make",
    "manufacturer",
    "model",
    "steering",
    "transmission",
    "vehicletype",
    "brakesystem",
    "airbaglocations",
    "wheelbasetype",
    "valvetraindesign",
    "engineconfiguration",
    "airbaglocfront",
    "fueldeliverytype",
    "airbaglocknee",
    "evdriveunit",
    "country",
    "pretensioner",
    "seatbeltsall",
    "adaptivecruisecontrol",
    "abs",
    "autobrake",
    "blindspotmonitoring",
    "ecs",
    "tractioncontrol",
    "forwardcollisionwarning",
    "lanedeparturewarning",
    "lanekeepsystem",
    "rearvisibilitycamera",
    "parkassist",
    "trailertype",
    "trailerbodytype",
    "coolingtype",
    "electrificationlevel",
    "chargerlevel",
    "turbo",
    "errorcode",
    "axleconfiguration",
    "busfloorconfigtype",
    "bustype",
    "custommotorcycletype",
    "motorcyclesuspensiontype",
    "motorcyclechassistype",
    "tpms",
    "dynamicbrakesupport",
    "pedestrianautomaticemergencybraking",
    "autoreversesystem",
    "automaticpedestrainalertingsound",
    "can_aacn",
    "edr",
    "keylessignition",
    "daytimerunninglight",
    "lowerbeamheadlamplightsource",
    "semiautomaticheadlampbeamswitching",
    "adaptivedrivingbeam",
    "rearcrosstrafficalert",
    "rearautomaticemergencybraking",
    "blindspotintervention",
    "lanecenteringassistance",
    "nonlanduse",
    "fueltanktype",
    "fueltankmaterial",
    "combinedbrakingsystem",
    "wheeliemitigation",
];

/// The lookup source table backing a given element id (port of the
/// `CASE ElementId` in `felementattributevalue`). NCSA views (96/97/98)
/// return `None` (W2 — resolve to raw id).
pub fn lookup_table_for_element(element_id: i32) -> Option<&'static str> {
    Some(match element_id {
        2 => "batterytype",
        3 => "bedtype",
        4 => "bodycab",
        5 => "bodystyle",
        10 => "destinationmarket",
        15 => "drivetype",
        23 => "entertainmentsystem",
        24 | 66 => "fueltype",
        25 | 184 | 185 | 190 => "grossvehicleweightrating",
        26 => "make",
        27 => "manufacturer",
        28 => "model",
        36 => "steering",
        37 => "transmission",
        39 => "vehicletype",
        42 => "brakesystem",
        55 | 56 | 107 => "airbaglocations",
        60 => "wheelbasetype",
        62 => "valvetraindesign",
        64 => "engineconfiguration",
        65 => "airbaglocfront",
        67 => "fueldeliverytype",
        69 => "airbaglocknee",
        72 => "evdriveunit",
        75 => "country",
        78 => "pretensioner",
        79 => "seatbeltsall",
        81 => "adaptivecruisecontrol",
        86 => "abs",
        87 => "autobrake",
        88 => "blindspotmonitoring",
        99 => "ecs",
        100 => "tractioncontrol",
        101 => "forwardcollisionwarning",
        102 => "lanedeparturewarning",
        103 => "lanekeepsystem",
        104 => "rearvisibilitycamera",
        105 => "parkassist",
        116 => "trailertype",
        117 => "trailerbodytype",
        122 => "coolingtype",
        126 => "electrificationlevel",
        127 => "chargerlevel",
        135 => "turbo",
        143 => "errorcode",
        145 => "axleconfiguration",
        148 => "busfloorconfigtype",
        149 => "bustype",
        151 => "custommotorcycletype",
        152 => "motorcyclesuspensiontype",
        153 => "motorcyclechassistype",
        168 => "tpms",
        170 => "dynamicbrakesupport",
        171 => "pedestrianautomaticemergencybraking",
        172 => "autoreversesystem",
        173 => "automaticpedestrainalertingsound",
        174 => "can_aacn",
        175 => "edr",
        176 => "keylessignition",
        177 => "daytimerunninglight",
        178 => "lowerbeamheadlamplightsource",
        179 => "semiautomaticheadlampbeamswitching",
        180 => "adaptivedrivingbeam",
        183 => "rearcrosstrafficalert",
        192 => "rearautomaticemergencybraking",
        193 => "blindspotintervention",
        194 => "lanecenteringassistance",
        195 => "nonlanduse",
        200 => "fueltanktype",
        201 => "fueltankmaterial",
        202 => "combinedbrakingsystem",
        203 => "wheeliemitigation",
        _ => return None,
    })
}

/// The `tag` for a lookup table name, or `None` if it is not a lookup source.
pub fn tag_of_table(name: &str) -> Option<u16> {
    LOOKUP_TABLES
        .iter()
        .position(|t| *t == name)
        .map(|i| i as u16)
}

/// The `tag` (index into `lookups`) for an element id.
pub fn element_lookup_tag(element_id: i32) -> Option<u16> {
    lookup_table_for_element(element_id).and_then(tag_of_table)
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct Wmi {
    pub id: i32,
    pub wmi: u32,
    pub manufacturerid: i32,
    pub makeid: i32,
    pub vehicletypeid: i32,
    pub trucktypeid: i32,
    pub publicavailabilitydate: i64,
    pub createdon_key: i64,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct WmiVinSchema {
    pub id: i32,
    pub wmiid: i32,
    pub vinschemaid: i32,
    pub yearfrom: i32,
    pub yearto: i32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct VinSchema {
    pub id: i32,
    pub tobeqced: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct Pattern {
    pub id: i32,
    pub vinschemaid: i32,
    pub keys: u32,
    pub keys_regex: u32,
    pub elementid: i32,
    pub attributeid: u32,
    pub createdon_key: i64,
    pub specificity: u8,
    pub has_bracket: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct Element {
    pub id: i32,
    pub name: u32,
    pub code: u32,
    pub isprivate: bool,
    pub groupname: u32,
    pub datatype: u32,
    pub decode: u32,
    pub decode_present: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct MakeModel {
    pub makeid: i32,
    pub modelid: i32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct WmiMake {
    pub wmiid: i32,
    pub makeid: i32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct EngineModel {
    pub id: i32,
    pub name: u32,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct EngineModelPattern {
    pub id: i32,
    pub enginemodelid: i32,
    pub elementid: i32,
    pub attributeid: u32,
    pub createdon_key: i64,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct DefaultValue {
    pub id: i32,
    pub elementid: i32,
    pub vehicletypeid: i32,
    pub defaultvalue: u32,
    pub defaultvalue_present: bool,
    pub createdon_key: i64,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct VinException {
    pub vin: u32,
    pub checkdigit: bool,
}

#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct LookupRow {
    pub tag: u16,
    pub id: i32,
    pub name: u32,
}

/// The root archive. Every `Vec` is totally ordered (see field comments) so the
/// serialized bytes are a pure function of the dump.
#[derive(Archive, Serialize, Deserialize, Debug, Clone)]
pub struct VpicData {
    /// Shared string arena: `str(id) = bytes[offsets[id]..offsets[id+1]]`.
    pub arena_bytes: Vec<u8>,
    pub arena_offsets: Vec<u32>,
    /// SORTED by (wmi string ASC, id ASC).
    pub wmi: Vec<Wmi>,
    /// SORTED by (wmiid ASC, id ASC).
    pub wmi_vinschema: Vec<WmiVinSchema>,
    /// SORTED by id ASC.
    pub vinschema: Vec<VinSchema>,
    /// SORTED by (vinschemaid ASC, id ASC).
    pub pattern: Vec<Pattern>,
    /// SORTED by id ASC.
    pub element: Vec<Element>,
    /// SORTED by (modelid ASC, makeid ASC).
    pub make_model: Vec<MakeModel>,
    /// SORTED by (wmiid ASC, makeid ASC).
    pub wmi_make: Vec<WmiMake>,
    /// SORTED by id ASC.
    pub enginemodel: Vec<EngineModel>,
    /// SORTED by (enginemodelid ASC, id ASC).
    pub enginemodelpattern: Vec<EngineModelPattern>,
    /// SORTED by (vehicletypeid ASC, id ASC).
    pub defaultvalue: Vec<DefaultValue>,
    /// SORTED by vin string ASC.
    pub vinexception: Vec<VinException>,
    /// SORTED by (tag ASC, id ASC).
    pub lookups: Vec<LookupRow>,
}

/// Artifact magic bytes.
pub const MAGIC: [u8; 8] = *b"ULTRAVIN";
/// On-disk format version.
pub const FORMAT_VERSION: u16 = 1;
/// Fixed header length prepended to the rkyv buffer.
pub const HEADER_LEN: usize = 64;

/// Serialize `data` into a self-describing artifact: a 64-byte header
/// (magic, format/builder version, blake3, root offset) followed by the rkyv
/// buffer. Deterministic: identical `data` + `builder_version` => identical bytes.
pub fn serialize_artifact(data: &VpicData, builder_version: u32) -> Vec<u8> {
    let body = rkyv::to_bytes::<rkyv::rancor::Error>(data).expect("rkyv serialize");
    let mut hasher = blake3::Hasher::new();
    hasher.update(&body);
    hasher.update(&builder_version.to_le_bytes());
    let hash = hasher.finalize();
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&builder_version.to_le_bytes());
    out.extend_from_slice(hash.as_bytes()); // 32 bytes -> ends at 46
    out.extend_from_slice(&(HEADER_LEN as u64).to_le_bytes()); // root offset -> ends at 54
    out.resize(HEADER_LEN, 0);
    out.extend_from_slice(&body);
    out
}

/// The blake3 digest recorded in an artifact header, as lowercase hex.
pub fn artifact_blake3_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(64);
    for b in &bytes[14..46] {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

impl VpicData {
    /// Resolve an arena string id to its `&str`.
    pub fn s(&self, id: u32) -> &str {
        let i = id as usize;
        let start = self.arena_offsets[i] as usize;
        let end = self.arena_offsets[i + 1] as usize;
        // Arena bytes are valid UTF-8 by construction (interned from &str).
        std::str::from_utf8(&self.arena_bytes[start..end]).unwrap_or("")
    }
}
