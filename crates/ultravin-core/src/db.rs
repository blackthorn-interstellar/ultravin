//! Loader and query surface over the embedded artifact.
//!
//! The artifact bytes are validated **once** (`rkyv::access`) and then held as-is;
//! every accessor returns a reference *into the archived buffer* (true zero-copy)
//! — no owned [`crate::tables::VpicData`] is ever materialized. Loading is just a
//! validate + pointer compute, so cold-start does not pay a ~75 MB deserialize.
//!
//! Archived integers are little-endian wrappers (`rkyv::rend`); accessors call
//! `.to_native()` at the comparison/return boundary. Both backends (embedded
//! `include_bytes!` and external `mmap`) feed the same validated archived bytes,
//! so they decode identically by construction.

use std::sync::OnceLock;

use rkyv::rancor;

use crate::tables::{
    ArchivedConversion, ArchivedDefaultValue, ArchivedElement, ArchivedEngineModel,
    ArchivedEngineModelPattern, ArchivedMakeModel, ArchivedPattern, ArchivedVSpecPattern,
    ArchivedVSpecSchema, ArchivedVSpecSchemaModel, ArchivedVSpecSchemaPattern,
    ArchivedVSpecSchemaYear, ArchivedVinSchema, ArchivedVpicData, ArchivedWmi, ArchivedWmiMake,
    ArchivedWmiVinSchema, FORMAT_VERSION, HEADER_LEN, MAGIC, NULL_I64,
};

/// 16-byte-aligned wrapper so `include_bytes!` (align 1) can be `rkyv::access`ed
/// in place. `HEADER_LEN` (64) is a multiple of 16, so the body at that offset
/// inherits the alignment of the whole blob.
#[repr(C, align(16))]
struct Aligned16<T: ?Sized>(T);

/// The artifact baked into the binary (a build product; see `build.rs`), forced
/// to 16-byte alignment so it can be accessed in place with zero copies.
static EMBEDDED: &Aligned16<[u8]> = &Aligned16(*include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/data/vpic.rkyv"
)));

/// Owner of the validated archived bytes (keeps the backing memory alive).
enum Backing {
    /// The process-static embedded blob; nothing to own.
    Static,
    /// An owned, 16-aligned copy of the rkyv body (header stripped).
    Owned(rkyv::util::AlignedVec<16>),
    /// A memory-mapped artifact file (header included; body at `HEADER_LEN`).
    #[cfg(feature = "external-data")]
    Mmap(memmap2::Mmap),
}

impl Backing {
    /// The 16-aligned rkyv body bytes (no header).
    fn body(&self) -> &[u8] {
        match self {
            Backing::Static => &EMBEDDED.0[HEADER_LEN..],
            Backing::Owned(v) => &v[..],
            #[cfg(feature = "external-data")]
            Backing::Mmap(m) => &m[HEADER_LEN..],
        }
    }
}

/// The decode database: validated archived bytes plus a pointer to the root.
///
/// The pointer references the heap/static buffer owned by `_backing`; that buffer
/// never moves once allocated (moving `Db` only moves the small owner handle), so
/// the pointer stays valid for the lifetime of the `Db`.
pub struct Db {
    _backing: Backing,
    archive: *const ArchivedVpicData,
}

// SAFETY: the archive is immutable, validated bytes; sharing `&Db` across threads
// only ever reads. The backing owns its buffer for the lifetime of the `Db`.
unsafe impl Send for Db {}
unsafe impl Sync for Db {}

/// Validate magic + format on the *full* artifact bytes (header + body).
fn check_header(bytes: &[u8]) -> Result<(), String> {
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
    Ok(())
}

impl Db {
    /// Fully validate the archived body (untrusted input), then hold it.
    fn build(backing: Backing) -> Result<Db, String> {
        // Checked access validates layout + alignment; we discard the borrow.
        rkyv::access::<ArchivedVpicData, rancor::Error>(backing.body())
            .map_err(|e| format!("artifact validation failed: {e}"))?;
        // SAFETY: just validated above; the borrow is converted to a raw pointer
        // into the buffer owned by `backing` (stable across the move below).
        let archive = unsafe {
            rkyv::access_unchecked::<ArchivedVpicData>(backing.body()) as *const ArchivedVpicData
        };
        Ok(Db {
            _backing: backing,
            archive,
        })
    }

    /// Hold the archived body of a *trusted* artifact without the O(n) full
    /// validation pass. Used only for the embedded blob, whose integrity is
    /// identical to the binary's own (built deterministically by our importer and
    /// gated by the frozen-corpus + parity tests); skipping the ~75 MB validation
    /// walk is what brings cold-start under target.
    ///
    /// # Safety
    /// `backing.body()` must be a valid rkyv archive of `ArchivedVpicData` at
    /// 16-byte alignment — guaranteed for the embedded artifact.
    unsafe fn build_trusted(backing: Backing) -> Db {
        let archive =
            rkyv::access_unchecked::<ArchivedVpicData>(backing.body()) as *const ArchivedVpicData;
        Db {
            _backing: backing,
            archive,
        }
    }

    /// Validate and load an artifact byte buffer (header + rkyv body).
    pub fn from_bytes(bytes: &[u8]) -> Result<Db, String> {
        check_header(bytes)?;
        // 16-byte-align the rkyv body (input alignment is unknown).
        let mut aligned = rkyv::util::AlignedVec::<16>::new();
        aligned.extend_from_slice(&bytes[HEADER_LEN..]);
        Db::build(Backing::Owned(aligned))
    }

    /// The process-wide embedded database (loaded once).
    pub fn embedded() -> &'static Db {
        static DB: OnceLock<Db> = OnceLock::new();
        DB.get_or_init(|| {
            check_header(&EMBEDDED.0).expect("embedded artifact header is valid");
            // SAFETY: the embedded artifact is a trusted, deterministically built
            // blob baked into this binary; its body is a valid 16-aligned archive.
            unsafe { Db::build_trusted(Backing::Static) }
        })
    }

    /// `true` once a real (non-empty) artifact has been baked in.
    pub fn is_loaded(&self) -> bool {
        !self.a().wmi.is_empty()
    }

    /// Load an artifact from a file via memory map (external-data backend).
    #[cfg(feature = "external-data")]
    pub fn open(path: &std::path::Path) -> Result<Db, String> {
        let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        let map = unsafe { memmap2::Mmap::map(&file).map_err(|e| e.to_string())? };
        check_header(&map)?;
        Db::build(Backing::Mmap(map))
    }

    /// The archived root (zero-copy view into the validated buffer).
    #[inline]
    fn a(&self) -> &ArchivedVpicData {
        // SAFETY: pointer is valid for as long as `self` (see struct docs).
        unsafe { &*self.archive }
    }

    /// Resolve an arena string id.
    pub fn s(&self, id: u32) -> &str {
        let a = self.a();
        let i = id as usize;
        let start = a.arena_offsets[i].to_native() as usize;
        let end = a.arena_offsets[i + 1].to_native() as usize;
        // Arena bytes are valid UTF-8 by construction (interned from &str).
        std::str::from_utf8(&a.arena_bytes[start..end]).unwrap_or("")
    }

    /// Find a public WMI by its string (binary search, public availability gated).
    pub fn wmi_by_str(&self, wmi: &str, now_micros: i64) -> Option<&ArchivedWmi> {
        let v = self.a().wmi.as_slice();
        let mut lo = v.partition_point(|w| self.s(w.wmi.to_native()) < wmi);
        while lo < v.len() && self.s(v[lo].wmi.to_native()) == wmi {
            let w = &v[lo];
            let pad = w.publicavailabilitydate.to_native();
            if pad != NULL_I64 && pad <= now_micros {
                return Some(w);
            }
            lo += 1;
        }
        None
    }

    /// Any WMI row by string (ignoring availability) — for vehicle/truck type.
    pub fn wmi_any(&self, wmi: &str) -> Option<&ArchivedWmi> {
        let v = self.a().wmi.as_slice();
        let lo = v.partition_point(|w| self.s(w.wmi.to_native()) < wmi);
        v.get(lo).filter(|w| self.s(w.wmi.to_native()) == wmi)
    }

    /// Contiguous `wmi_vinschema` rows for a wmi id.
    pub fn wmi_vinschema_for(&self, wmiid: i32) -> &[ArchivedWmiVinSchema] {
        slice_eq(self.a().wmi_vinschema.as_slice(), wmiid, |r| {
            r.wmiid.to_native()
        })
    }

    /// Contiguous `pattern` rows for a vin schema id.
    pub fn patterns_for(&self, vinschemaid: i32) -> &[ArchivedPattern] {
        slice_eq(self.a().pattern.as_slice(), vinschemaid, |p| {
            p.vinschemaid.to_native()
        })
    }

    pub fn vinschema_by_id(&self, id: i32) -> Option<&ArchivedVinSchema> {
        let v = self.a().vinschema.as_slice();
        v.binary_search_by(|r| r.id.to_native().cmp(&id))
            .ok()
            .map(|i| &v[i])
    }

    pub fn element_by_id(&self, id: i32) -> Option<&ArchivedElement> {
        let v = self.a().element.as_slice();
        v.binary_search_by(|r| r.id.to_native().cmp(&id))
            .ok()
            .map(|i| &v[i])
    }

    /// All elements with a non-empty Decode and not private — the output set.
    pub fn elements(&self) -> &[ArchivedElement] {
        self.a().element.as_slice()
    }

    pub fn makes_for_model(&self, modelid: i32) -> &[ArchivedMakeModel] {
        slice_eq(self.a().make_model.as_slice(), modelid, |r| {
            r.modelid.to_native()
        })
    }

    pub fn wmi_makes_for(&self, wmiid: i32) -> &[ArchivedWmiMake] {
        slice_eq(self.a().wmi_make.as_slice(), wmiid, |r| r.wmiid.to_native())
    }

    /// Engine model whose `lower(trim(name))` equals `norm`.
    pub fn enginemodel_by_norm(&self, norm: &str) -> Option<&ArchivedEngineModel> {
        self.a()
            .enginemodel
            .iter()
            .find(|em| self.s(em.name.to_native()).trim().to_ascii_lowercase() == norm)
    }

    pub fn enginemodelpatterns_for(&self, emid: i32) -> &[ArchivedEngineModelPattern] {
        slice_eq(self.a().enginemodelpattern.as_slice(), emid, |r| {
            r.enginemodelid.to_native()
        })
    }

    pub fn defaultvalues_for(&self, vehicletypeid: i32) -> &[ArchivedDefaultValue] {
        slice_eq(self.a().defaultvalue.as_slice(), vehicletypeid, |r| {
            r.vehicletypeid.to_native()
        })
    }

    /// `true` if `vin` has a check-digit exception.
    pub fn vinexception_checkdigit(&self, vin: &str) -> bool {
        let v = self.a().vinexception.as_slice();
        let lo = v.partition_point(|r| self.s(r.vin.to_native()) < vin);
        v.get(lo)
            .map(|r| self.s(r.vin.to_native()) == vin && r.checkdigit)
            .unwrap_or(false)
    }

    /// Conversions whose `FromElementId` equals `from_element_id` (`vpic.conversion`).
    pub fn conversions_from(
        &self,
        from_element_id: i32,
    ) -> impl Iterator<Item = &ArchivedConversion> {
        self.a()
            .conversion
            .iter()
            .filter(move |c| c.fromelementid.to_native() == from_element_id)
    }

    /// All make ids linked (via `Wmi_Make`) to any `Wmi` row whose string equals
    /// `wmi` (no public-availability filter, matching the spec candidate join).
    pub fn makeids_for_wmi_str(&self, wmi: &str) -> Vec<i32> {
        let v = self.a().wmi.as_slice();
        let mut i = v.partition_point(|w| self.s(w.wmi.to_native()) < wmi);
        let mut out: Vec<i32> = Vec::new();
        while i < v.len() && self.s(v[i].wmi.to_native()) == wmi {
            for m in self.wmi_makes_for(v[i].id.to_native()) {
                out.push(m.makeid.to_native());
            }
            i += 1;
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// All `Wmi.id`s whose string equals `wmi` (no availability filter), for the
    /// `fExtractValidCharsPerWmiYear` join (correction charset).
    pub fn wmi_ids_for_str(&self, wmi: &str) -> Vec<i32> {
        let v = self.a().wmi.as_slice();
        let mut i = v.partition_point(|w| self.s(w.wmi.to_native()) < wmi);
        let mut out = Vec::new();
        while i < v.len() && self.s(v[i].wmi.to_native()) == wmi {
            out.push(v[i].id.to_native());
            i += 1;
        }
        out
    }

    /// `VehicleSpecSchema` rows for a make id.
    pub fn vspecschemas_for_make(&self, makeid: i32) -> &[ArchivedVSpecSchema] {
        slice_eq(self.a().vspecschema.as_slice(), makeid, |r| {
            r.makeid.to_native()
        })
    }

    /// `VSpecSchemaPattern` rows for a schema id.
    pub fn vspecschemapatterns_for(&self, schemaid: i32) -> &[ArchivedVSpecSchemaPattern] {
        slice_eq(self.a().vspecschemapattern.as_slice(), schemaid, |r| {
            r.schemaid.to_native()
        })
    }

    /// `VehicleSpecPattern` rows for a `VSpecSchemaPattern` id.
    pub fn vspecpatterns_for(&self, vspid: i32) -> &[ArchivedVSpecPattern] {
        slice_eq(self.a().vspecpattern.as_slice(), vspid, |r| {
            r.vspecschemapatternid.to_native()
        })
    }

    /// `VehicleSpecSchema_Model` rows for a schema id.
    pub fn vspecschema_models_for(&self, schemaid: i32) -> &[ArchivedVSpecSchemaModel] {
        slice_eq(self.a().vspecschemamodel.as_slice(), schemaid, |r| {
            r.schemaid.to_native()
        })
    }

    /// `VehicleSpecSchema_Year` rows for a schema id.
    pub fn vspecschema_years_for(&self, schemaid: i32) -> &[ArchivedVSpecSchemaYear] {
        slice_eq(self.a().vspecschemayear.as_slice(), schemaid, |r| {
            r.schemaid.to_native()
        })
    }

    /// Resolve a lookup (`tag`, numeric id) to its name.
    pub fn lookup(&self, tag: u16, id: i32) -> Option<&str> {
        let v = self.a().lookups.as_slice();
        let lo = v.partition_point(|r| (r.tag.to_native(), r.id.to_native()) < (tag, id));
        v.get(lo)
            .filter(|r| r.tag.to_native() == tag && r.id.to_native() == id)
            .map(|r| self.s(r.name.to_native()))
    }
}

/// Contiguous sub-slice of `v` (sorted by `key`) whose key equals `target`.
fn slice_eq<T, F: Fn(&T) -> i32>(v: &[T], target: i32, key: F) -> &[T] {
    let lo = v.partition_point(|r| key(r) < target);
    let hi = v.partition_point(|r| key(r) <= target);
    &v[lo..hi]
}
