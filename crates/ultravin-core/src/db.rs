//! Loader and query surface over the embedded artifact.
//!
//! The artifact bytes are validated once (`rkyv::access`) then deserialized into
//! an owned [`VpicData`]; every accessor below is a binary-search / partition
//! over the totally-ordered arrays. The embedded backend (`include_bytes!`) and
//! the external `mmap` backend feed the *same* validated path, so they decode
//! identically by construction.

use std::sync::OnceLock;

use rkyv::rancor;

use crate::tables::{
    ArchivedVpicData, DefaultValue, Element, EngineModel, EngineModelPattern, LookupRow, MakeModel,
    Pattern, VinSchema, VpicData, Wmi, WmiMake, WmiVinSchema, FORMAT_VERSION, HEADER_LEN, MAGIC,
};

/// The artifact baked into the binary (a build product; see `build.rs`).
static EMBEDDED: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/vpic.rkyv"));

/// The decode database: owned, totally-ordered tables loaded from an artifact.
pub struct Db {
    data: VpicData,
}

impl Db {
    /// Validate and load an artifact byte buffer (header + rkyv body).
    pub fn from_bytes(bytes: &[u8]) -> Result<Db, String> {
        if bytes.len() < HEADER_LEN {
            return Err("artifact too small".into());
        }
        if bytes[..8] != MAGIC {
            return Err("bad artifact magic".into());
        }
        let fmt = u16::from_le_bytes([bytes[8], bytes[9]]);
        if fmt != FORMAT_VERSION {
            return Err(format!("artifact format {fmt} != {FORMAT_VERSION}"));
        }
        // 16-byte-align the rkyv body (header is 64 bytes; include_bytes! is align 1).
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes[HEADER_LEN..]);
        let archived = rkyv::access::<ArchivedVpicData, rancor::Error>(&aligned)
            .map_err(|e| format!("artifact validation failed: {e}"))?;
        let data = rkyv::deserialize::<VpicData, rancor::Error>(archived)
            .map_err(|e| format!("artifact deserialize failed: {e}"))?;
        Ok(Db { data })
    }

    /// The process-wide embedded database (loaded once).
    pub fn embedded() -> &'static Db {
        static DB: OnceLock<Db> = OnceLock::new();
        DB.get_or_init(|| Db::from_bytes(EMBEDDED).expect("embedded artifact is valid"))
    }

    /// `true` once a real (non-empty) artifact has been baked in.
    pub fn is_loaded(&self) -> bool {
        !self.data.wmi.is_empty()
    }

    /// Load an artifact from a file via memory map (external-data backend).
    #[cfg(feature = "external-data")]
    pub fn open(path: &std::path::Path) -> Result<Db, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let map = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
        Db::from_bytes(&map)
    }

    /// Resolve an arena string id.
    pub fn s(&self, id: u32) -> &str {
        self.data.s(id)
    }

    /// Find a public WMI by its string (binary search, public availability gated).
    pub fn wmi_by_str(&self, wmi: &str, now_micros: i64) -> Option<&Wmi> {
        let v = &self.data.wmi;
        let mut lo = v.partition_point(|w| self.data.s(w.wmi) < wmi);
        while lo < v.len() && self.data.s(v[lo].wmi) == wmi {
            let w = &v[lo];
            if w.publicavailabilitydate != crate::tables::NULL_I64
                && w.publicavailabilitydate <= now_micros
            {
                return Some(w);
            }
            lo += 1;
        }
        None
    }

    /// Any WMI row by string (ignoring availability) — for vehicle/truck type.
    pub fn wmi_any(&self, wmi: &str) -> Option<&Wmi> {
        let v = &self.data.wmi;
        let lo = v.partition_point(|w| self.data.s(w.wmi) < wmi);
        v.get(lo).filter(|w| self.data.s(w.wmi) == wmi)
    }

    /// Contiguous `wmi_vinschema` rows for a wmi id.
    pub fn wmi_vinschema_for(&self, wmiid: i32) -> &[WmiVinSchema] {
        slice_eq(&self.data.wmi_vinschema, wmiid, |r| r.wmiid)
    }

    /// Contiguous `pattern` rows for a vin schema id.
    pub fn patterns_for(&self, vinschemaid: i32) -> &[Pattern] {
        slice_eq(&self.data.pattern, vinschemaid, |p| p.vinschemaid)
    }

    pub fn vinschema_by_id(&self, id: i32) -> Option<&VinSchema> {
        let v = &self.data.vinschema;
        v.binary_search_by_key(&id, |r| r.id).ok().map(|i| &v[i])
    }

    pub fn element_by_id(&self, id: i32) -> Option<&Element> {
        let v = &self.data.element;
        v.binary_search_by_key(&id, |r| r.id).ok().map(|i| &v[i])
    }

    /// All elements with a non-empty Decode and not private — the output set.
    pub fn elements(&self) -> &[Element] {
        &self.data.element
    }

    pub fn makes_for_model(&self, modelid: i32) -> &[MakeModel] {
        slice_eq(&self.data.make_model, modelid, |r| r.modelid)
    }

    pub fn wmi_makes_for(&self, wmiid: i32) -> &[WmiMake] {
        slice_eq(&self.data.wmi_make, wmiid, |r| r.wmiid)
    }

    /// Engine model whose `lower(trim(name))` equals `norm`.
    pub fn enginemodel_by_norm(&self, norm: &str) -> Option<&EngineModel> {
        self.data
            .enginemodel
            .iter()
            .find(|em| self.data.s(em.name).trim().to_ascii_lowercase() == norm)
    }

    pub fn enginemodelpatterns_for(&self, emid: i32) -> &[EngineModelPattern] {
        slice_eq(&self.data.enginemodelpattern, emid, |r| r.enginemodelid)
    }

    pub fn defaultvalues_for(&self, vehicletypeid: i32) -> &[DefaultValue] {
        slice_eq(&self.data.defaultvalue, vehicletypeid, |r| r.vehicletypeid)
    }

    /// `true` if `vin` has a check-digit exception.
    pub fn vinexception_checkdigit(&self, vin: &str) -> bool {
        let v = &self.data.vinexception;
        let lo = v.partition_point(|r| self.data.s(r.vin) < vin);
        v.get(lo)
            .map(|r| self.data.s(r.vin) == vin && r.checkdigit)
            .unwrap_or(false)
    }

    /// Resolve a lookup (`tag`, numeric id) to its name.
    pub fn lookup(&self, tag: u16, id: i32) -> Option<&str> {
        let v: &[LookupRow] = &self.data.lookups;
        let lo = v.partition_point(|r| (r.tag, r.id) < (tag, id));
        v.get(lo)
            .filter(|r| r.tag == tag && r.id == id)
            .map(|r| self.data.s(r.name))
    }
}

/// Contiguous sub-slice of `v` (sorted by `key`) whose key equals `target`.
fn slice_eq<T, F: Fn(&T) -> i32>(v: &[T], target: i32, key: F) -> &[T] {
    let lo = v.partition_point(|r| key(r) < target);
    let hi = v.partition_point(|r| key(r) <= target);
    &v[lo..hi]
}
