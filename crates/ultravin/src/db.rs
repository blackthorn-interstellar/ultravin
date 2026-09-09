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

use crate::hash::IntMap;
use crate::matcher::PatternIndex;
use crate::tables::{check_header, validate_body};
use crate::tables::{
    ArchivedConversion, ArchivedDefaultValue, ArchivedElement, ArchivedEngineModel,
    ArchivedEngineModelPattern, ArchivedMakeModel, ArchivedPattern, ArchivedVSpecPattern,
    ArchivedVSpecSchema, ArchivedVSpecSchemaModel, ArchivedVSpecSchemaPattern,
    ArchivedVSpecSchemaYear, ArchivedVinSchema, ArchivedVpicData, ArchivedWmi, ArchivedWmiMake,
    ArchivedWmiVinSchema, HEADER_LEN,
};

/// 16-byte-aligned wrapper so `include_bytes!` (align 1) can be `rkyv::access`ed
/// in place. `HEADER_LEN` (64) is a multiple of 16, so the body at that offset
/// inherits the alignment of the whole blob.
#[repr(C, align(16))]
struct Aligned16<T: ?Sized>(T);

/// The artifact baked into the binary (a build product; see `build.rs`), forced
/// to 16-byte alignment so it can be accessed in place with zero copies.
static EMBEDDED: &Aligned16<[u8]> = &Aligned16(*include_bytes!(env!("ULTRAVIN_ARTIFACT")));

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
    /// Dense `element_id -> slice index` table (`-1` = absent), built once on first
    /// use. Resolution and projection repeatedly consult element metadata, so
    /// an O(1) index avoids searching the element table for every output row.
    element_index: OnceLock<Box<[i32]>>,
    /// Packed (lookup tag, numeric id) -> arena string id, initialized once.
    /// Winning-pass resolution repeatedly reads these immutable names.
    lookup_index: OnceLock<IntMap<u64, u32>>,
    /// Dense `element_id -> can this element contribute a pattern match` table
    /// (built once). Index construction uses these immutable element flags to
    /// exclude ineligible rows before they can reach the matching hot path.
    pattern_element_ok: OnceLock<Box<[bool]>>,
    /// One lazily compiled key index per schema, shared by all decoding threads.
    pattern_indexes: OnceLock<Box<[OnceLock<PatternIndex>]>>,
    /// A conversion producing Model can enable vehicle-spec pattern rows even
    /// when no regular/formula schema covers the candidate year.
    model_from_conversion: OnceLock<bool>,
    /// Packed WMI bytes -> archive row range. Year selection, core passes and
    /// error correction all consult the same WMI; avoid repeating string searches.
    wmi_index: OnceLock<IntMap<u64, (usize, usize)>>,
    /// The spec join first selects by make and model; keep those candidates in
    /// archive order so decoding need not scan every model of a manufacturer's
    /// every schema. Vehicle type, year and QC checks still run on each pass.
    spec_model_index: OnceLock<IntMap<(i32, i32), Vec<u32>>>,
}

// SAFETY: the archive is immutable, validated bytes; sharing `&Db` across threads
// only ever reads. The backing owns its buffer for the lifetime of the `Db`.
unsafe impl Send for Db {}
unsafe impl Sync for Db {}

impl Db {
    /// Fully validate the archived body (untrusted input), then hold it.
    fn build(backing: Backing) -> Result<Db, String> {
        // Checked rkyv access (layout + alignment), then the arena UTF-8 proof the
        // hot-path `from_utf8_unchecked` in `s()` relies on, then the element-id
        // cap that bounds the dense `element_index` — see `tables::validate_body`.
        validate_body(backing.body())?;
        // SAFETY: just validated above; the borrow is converted to a raw pointer
        // into the buffer owned by `backing` (stable across the move below).
        let archive = unsafe {
            rkyv::access_unchecked::<ArchivedVpicData>(backing.body()) as *const ArchivedVpicData
        };
        Ok(Db {
            _backing: backing,
            archive,
            element_index: OnceLock::new(),
            lookup_index: OnceLock::new(),
            pattern_element_ok: OnceLock::new(),
            pattern_indexes: OnceLock::new(),
            model_from_conversion: OnceLock::new(),
            wmi_index: OnceLock::new(),
            spec_model_index: OnceLock::new(),
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
            element_index: OnceLock::new(),
            lookup_index: OnceLock::new(),
            pattern_element_ok: OnceLock::new(),
            pattern_indexes: OnceLock::new(),
            model_from_conversion: OnceLock::new(),
            wmi_index: OnceLock::new(),
            spec_model_index: OnceLock::new(),
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
    ///
    /// # Panics
    /// If this binary was built with the empty placeholder artifact (`build.rs`
    /// stubs one so a fresh checkout compiles before the importer has run). A
    /// placeholder decodes every VIN to "manufacturer not registered" — refusing
    /// loudly here beats silently serving wrong answers to a crate consumer who
    /// was never told the importer exists. Use [`Db::try_embedded`] to probe.
    pub fn embedded() -> &'static Db {
        let db = Db::embedded_raw();
        assert!(
            db.is_loaded(),
            "ultravin: this binary embeds the EMPTY placeholder artifact (no vpic.rkyv at \
             build time), so every decode would be wrong. Get real data: download \
             vpic.rkyv from the GitHub release matching this crate version and rebuild \
             with ULTRAVIN_DATA=/abs/path/vpic.rkyv (or Db::open it with the \
             external-data feature); in the repo, `make download && make data`."
        );
        db
    }

    /// The embedded database, or `None` when this binary carries only the empty
    /// placeholder artifact. The non-panicking form of [`Db::embedded`], for
    /// tests and callers that degrade gracefully.
    pub fn try_embedded() -> Option<&'static Db> {
        let db = Db::embedded_raw();
        db.is_loaded().then_some(db)
    }

    /// The embedded blob as-is, placeholder or not (loaded once). Crate-visible
    /// so in-crate tests can probe `is_loaded` and skip without tripping the
    /// [`Db::embedded`] refusal.
    pub(crate) fn embedded_raw() -> &'static Db {
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
    ///
    /// # Safety contract
    /// The returned [`Db`] holds a read-only [`memmap2::Mmap`] over `path` for its
    /// entire lifetime and treats those bytes as immutable, already-validated rkyv.
    /// The backing file must not be mutated or truncated while the `Db` lives —
    /// another writer changing bytes out from under the zero-copy views would cause
    /// torn reads or out-of-bounds slicing. A caller that cannot guarantee a stable
    /// file should load an owned copy with [`Db::from_bytes`] instead.
    ///
    /// Accepted TOCTOU (do not flag as a runtime vuln): mapping-after-validate lets
    /// a local writer mutate/truncate the file after `Db::build` checks it, faulting
    /// (SIGBUS) or tearing reads. This is an accepted opt-in risk, not a bug to fix —
    /// `open` is behind the non-default `external-data` cargo feature, is NOT compiled
    /// into the shipped Python wheel, and has no in-repo caller; only an embedder who
    /// enables the feature and owns the data file reaches it, and the contract above
    /// puts the immutability obligation on them.
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
    #[inline]
    pub fn s(&self, id: u32) -> &str {
        let a = self.a();
        let i = id as usize;
        let start = a.arena_offsets[i].to_native() as usize;
        let end = a.arena_offsets[i + 1].to_native() as usize;
        // SAFETY: the arena is valid UTF-8 at every declared offset — guaranteed by
        // the importer for the embedded blob and proven once by `validate_arena`
        // for untrusted artifacts. `s()` is the single hottest call in a decode;
        // re-validating UTF-8 here (per call) was ~13% of decode self-time.
        unsafe { std::str::from_utf8_unchecked(&a.arena_bytes[start..end]) }
    }

    /// Find the first WMI row published as of the supplied clock.
    pub fn wmi_by_str(&self, wmi: &str, now_micros: i64) -> Option<&ArchivedWmi> {
        self.wmi_rows(wmi).iter().find(|w| w.is_public(now_micros))
    }

    /// Any WMI row by string (ignoring availability) — for vehicle/truck type.
    pub fn wmi_any(&self, wmi: &str) -> Option<&ArchivedWmi> {
        self.wmi_rows(wmi).first()
    }

    /// All rows for one WMI, in archive order. Availability remains a per-call
    /// decision: a cached range must not freeze a future publication date.
    fn wmi_rows(&self, wmi: &str) -> &[ArchivedWmi] {
        let v = self.a().wmi.as_slice();
        if let Some(key) = packed_wmi(wmi) {
            let index = self.wmi_index.get_or_init(|| {
                let mut index: IntMap<u64, (usize, usize)> =
                    IntMap::with_capacity_and_hasher(v.len(), Default::default());
                for (i, row) in v.iter().enumerate() {
                    if let Some(key) = packed_wmi(self.s(row.wmi.to_native())) {
                        index.entry(key).or_insert((i, i + 1)).1 = i + 1;
                    }
                }
                index
            });
            return index.get(&key).map_or(&[], |&(start, end)| &v[start..end]);
        }
        // Normal WMIs have three or six characters. Retain the ordinary lookup
        // for longer strings supplied through the explicit-database API.
        let lo = v.partition_point(|w| self.s(w.wmi.to_native()) < wmi);
        let len = v[lo..].partition_point(|w| self.s(w.wmi.to_native()) <= wmi);
        &v[lo..lo + len]
    }

    /// Contiguous `wmi_vinschema` rows for a wmi id.
    pub fn wmi_vinschema_for(&self, wmiid: i32) -> &[ArchivedWmiVinSchema] {
        slice_eq(self.a().wmi_vinschema.as_slice(), wmiid, |r| {
            r.wmiid.to_native()
        })
    }

    /// Conservative preflight for a pass that needs a PatternId-bearing row to
    /// compete. Any year-eligible link is enough, including orphan/QC schemas:
    /// formula matching intentionally permits those. With no links, only a
    /// conversion to Model could enable the vehicle-spec source later in core.
    pub(crate) fn may_have_pattern_rows(&self, wmi: &str, year: Option<i32>, now: i64) -> bool {
        let Some(wmi) = self.wmi_by_str(wmi, now) else {
            return false;
        };
        self.wmi_vinschema_for(wmi.id.to_native()).iter().any(|r| {
            year.is_none_or(|year| year >= r.yearfrom.to_native() && year <= r.yearto_or(2999))
        }) || *self.model_from_conversion.get_or_init(|| {
            self.a()
                .conversion
                .iter()
                .any(|c| c.toelementid.to_native() == 28)
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

    pub(crate) fn pattern_index(&self, id: i32) -> Option<&PatternIndex> {
        let schemas = self.a().vinschema.as_slice();
        let i = schemas
            .binary_search_by_key(&id, |s| s.id.to_native())
            .ok()?;
        let indexes = self
            .pattern_indexes
            .get_or_init(|| (0..schemas.len()).map(|_| OnceLock::new()).collect());
        Some(indexes[i].get_or_init(|| {
            let patterns = self.patterns();
            let start = patterns.partition_point(|p| p.vinschemaid.to_native() < id);
            let end =
                start + patterns[start..].partition_point(|p| p.vinschemaid.to_native() <= id);
            PatternIndex::build(self, start as u32, &patterns[start..end])
        }))
    }

    pub fn element_by_id(&self, id: i32) -> Option<&ArchivedElement> {
        if id < 0 {
            return None;
        }
        let idx = self.element_index();
        let slot = *idx.get(id as usize)?;
        if slot < 0 {
            None
        } else {
            Some(&self.a().element.as_slice()[slot as usize])
        }
    }

    /// `element_id -> eligible for the pattern pass`: the element exists, has a
    /// public decode, and is not one of the four the pass skips (26/27/29/39 are
    /// added by their own later passes). Indexed by element id; ids past the end
    /// are absent, hence ineligible.
    pub fn pattern_element_ok(&self) -> &[bool] {
        self.pattern_element_ok.get_or_init(|| {
            let idx = self.element_index();
            let mut ok = vec![false; idx.len()].into_boxed_slice();
            for (id, slot) in idx.iter().enumerate() {
                if *slot < 0 || matches!(id, 26 | 27 | 29 | 39) {
                    continue;
                }
                let e = &self.a().element.as_slice()[*slot as usize];
                ok[id] = e.decode_present && !e.isprivate;
            }
            ok
        })
    }

    /// Lazily-built dense `element_id -> slice index` table (see field docs).
    fn element_index(&self) -> &[i32] {
        self.element_index.get_or_init(|| {
            let v = self.a().element.as_slice();
            let max = v.iter().map(|e| e.id.to_native()).max().unwrap_or(-1);
            let mut idx = vec![-1i32; (max + 1).max(0) as usize];
            for (i, e) in v.iter().enumerate() {
                let id = e.id.to_native();
                if id >= 0 {
                    idx[id as usize] = i as i32;
                }
            }
            idx.into_boxed_slice()
        })
    }

    /// The whole element table. The public output set — elements with a non-empty
    /// Decode and not private — is the subset `public_decode` keeps in the caller.
    pub fn elements(&self) -> &[ArchivedElement] {
        self.a().element.as_slice()
    }

    // --- Whole-table access. Decoding only ever needs keyed lookups; generating
    // test VINs needs to walk the tables, so these exist for `generate`. ---

    /// The baked behavioural cover: the smallest VIN set that exercises every
    /// decode behaviour this data month can reach.
    pub fn cover(&self) -> Vec<String> {
        self.a().cover.iter().map(|v| v.to_string()).collect()
    }

    /// Every WMI, sorted by (wmi string ASC, id ASC).
    pub fn wmis(&self) -> &[ArchivedWmi] {
        self.a().wmi.as_slice()
    }

    /// Every pattern, sorted by (vinschemaid ASC, id ASC).
    pub fn patterns(&self) -> &[crate::tables::ArchivedPattern] {
        self.a().pattern.as_slice()
    }

    /// Every VinException VIN, sorted by the VIN string.
    pub fn vinexceptions(&self) -> &[crate::tables::ArchivedVinException] {
        self.a().vinexception.as_slice()
    }

    /// Every engine model, sorted by id.
    pub fn enginemodels(&self) -> &[crate::tables::ArchivedEngineModel] {
        self.a().enginemodel.as_slice()
    }

    /// Every vehicle-spec schema, sorted by (makeid ASC, id ASC).
    pub fn vspecschemas(&self) -> &[crate::tables::ArchivedVSpecSchema] {
        self.a().vspecschema.as_slice()
    }

    /// Every DefaultValue row, sorted by (vehicletypeid ASC, id ASC).
    pub fn defaultvalues(&self) -> &[crate::tables::ArchivedDefaultValue] {
        self.a().defaultvalue.as_slice()
    }

    /// Lookup ids whose value matches `name` case-insensitively, for one table
    /// tag — the reverse of [`Db::lookup`], used to turn a make name into ids.
    pub fn lookup_ids_by_name(&self, tag: u16, name: &str) -> Vec<i32> {
        self.a()
            .lookups
            .iter()
            .filter(|r| {
                r.tag.to_native() == tag && self.s(r.name.to_native()).eq_ignore_ascii_case(name)
            })
            .map(|r| r.id.to_native())
            .collect()
    }

    pub fn makes_for_model(&self, modelid: i32) -> &[ArchivedMakeModel] {
        slice_eq(self.a().make_model.as_slice(), modelid, |r| {
            r.modelid.to_native()
        })
    }

    pub fn wmi_makes_for(&self, wmiid: i32) -> &[ArchivedWmiMake] {
        slice_eq(self.a().wmi_make.as_slice(), wmiid, |r| r.wmiid.to_native())
    }

    /// Engine model whose `lower(trim(name))` equals `norm` (already lowercased by
    /// the caller). Case-insensitive compare avoids allocating a lowercased copy of
    /// every row's name during the linear scan.
    pub fn enginemodel_by_norm(&self, norm: &str) -> Option<&ArchivedEngineModel> {
        self.a().enginemodel.iter().find(|em| {
            self.s(em.name.to_native())
                .trim()
                .eq_ignore_ascii_case(norm)
        })
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
        let mut out: Vec<i32> = Vec::new();
        for row in self.wmi_rows(wmi) {
            for m in self.wmi_makes_for(row.id.to_native()) {
                out.push(m.makeid.to_native());
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// All `Wmi.id`s whose string equals `wmi` (no availability filter), for the
    /// `fExtractValidCharsPerWmiYear` join (correction charset).
    pub fn wmi_ids_for_str(&self, wmi: &str) -> Vec<i32> {
        self.wmi_rows(wmi)
            .iter()
            .map(|w| w.id.to_native())
            .collect()
    }

    /// `VehicleSpecSchema` rows for a make id.
    pub fn vspecschemas_for_make(&self, makeid: i32) -> &[ArchivedVSpecSchema] {
        slice_eq(self.a().vspecschema.as_slice(), makeid, |r| {
            r.makeid.to_native()
        })
    }

    pub(crate) fn vspecschemas_for_make_model(
        &self,
        makeid: i32,
        modelid: i32,
    ) -> impl Iterator<Item = &ArchivedVSpecSchema> {
        let index = self.spec_model_index.get_or_init(|| {
            let mut index: IntMap<(i32, i32), Vec<u32>> = IntMap::default();
            for (i, schema) in self.vspecschemas().iter().enumerate() {
                let mut previous_model = None;
                for model in self.vspecschema_models_for(schema.id.to_native()) {
                    let id = model.modelid.to_native();
                    // The old join used `any`: duplicate model rows must not
                    // duplicate the schema. Model rows are sorted by model id.
                    if previous_model != Some(id) {
                        index
                            .entry((schema.makeid.to_native(), id))
                            .or_default()
                            .push(i as u32);
                        previous_model = Some(id);
                    }
                }
            }
            index
        });
        index
            .get(&(makeid, modelid))
            .into_iter()
            .flatten()
            .map(|&i| &self.vspecschemas()[i as usize])
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
        let key = (u64::from(tag) << 32) | u64::from(id as u32);
        self.lookup_index
            .get_or_init(|| {
                let rows = self.a().lookups.as_slice();
                let mut index = IntMap::with_capacity_and_hasher(rows.len(), Default::default());
                for row in rows {
                    let key = (u64::from(row.tag.to_native()) << 32)
                        | u64::from(row.id.to_native() as u32);
                    // The previous lower-bound search returns the first duplicate.
                    index.entry(key).or_insert(row.name.to_native());
                }
                index
            })
            .get(&key)
            .map(|&name| self.s(name))
    }
}

/// Keep length in the last byte so embedded NULs and short strings cannot alias.
fn packed_wmi(wmi: &str) -> Option<u64> {
    let bytes = wmi.as_bytes();
    if bytes.len() > 7 {
        return None;
    }
    let mut key = [0u8; 8];
    key[..bytes.len()].copy_from_slice(bytes);
    key[7] = bytes.len() as u8;
    Some(u64::from_le_bytes(key))
}

/// Contiguous sub-slice of `v` (sorted by `key`) whose key equals `target`.
fn slice_eq<T, F: Fn(&T) -> i32>(v: &[T], target: i32, key: F) -> &[T] {
    let lo = v.partition_point(|r| key(r) < target);
    // The upper bound can only lie in the suffix; searching `v[lo..]` halves the
    // comparison count of the second binary search on the large tables.
    let hi = lo + v[lo..].partition_point(|r| key(r) <= target);
    &v[lo..hi]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_index_matches_lower_bound_searches() {
        let Some(db) = Db::try_embedded() else { return };
        let rows = db.a().lookups.as_slice();
        let key = |r: &crate::tables::ArchivedLookupRow| (r.tag.to_native(), r.id.to_native());
        for row in rows {
            let (tag, id) = key(row);
            let first = rows.partition_point(|r| key(r) < (tag, id));
            assert_eq!(db.lookup(tag, id), Some(db.s(rows[first].name.to_native())));
        }
        for tag in [0, 1, 26, 27, 39, u16::MAX] {
            for id in [i32::MIN, -1, 0, 1, 100_000, i32::MAX] {
                let first = rows.partition_point(|r| key(r) < (tag, id));
                let expected = rows
                    .get(first)
                    .filter(|r| key(r) == (tag, id))
                    .map(|r| db.s(r.name.to_native()));
                assert_eq!(db.lookup(tag, id), expected, "tag {tag}, id {id}");
            }
        }
    }

    #[test]
    fn pass_preflight_keeps_orphan_schemas_and_conversion_models() {
        use crate::tables::{serialize_artifact, Conversion, VpicData, Wmi, WmiVinSchema};
        let mut data = VpicData {
            arena_bytes: b"ABC#x#".to_vec(),
            arena_offsets: vec![0, 0, 3, 6],
            wmi: vec![
                Wmi {
                    id: 1,
                    wmi: 1,
                    manufacturerid: 1,
                    makeid: 1,
                    vehicletypeid: 2,
                    trucktypeid: 0,
                    publicavailabilitydate: 100,
                    createdon_key: 0,
                },
                Wmi {
                    id: 2,
                    wmi: 1,
                    manufacturerid: 1,
                    makeid: 1,
                    vehicletypeid: 2,
                    trucktypeid: 0,
                    publicavailabilitydate: 0,
                    createdon_key: 0,
                },
            ],
            wmi_vinschema: vec![
                WmiVinSchema {
                    id: 1,
                    wmiid: 1,
                    vinschemaid: 900,
                    yearfrom: 2000,
                    yearto: 2000,
                },
                WmiVinSchema {
                    id: 2,
                    wmiid: 2,
                    vinschemaid: 901,
                    yearfrom: 2010,
                    yearto: 2010,
                },
            ],
            // Formula rows may join an orphan schema: do not require this table.
            vinschema: vec![],
            pattern: vec![],
            element: vec![],
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
        let load = |data: &VpicData| Db::from_bytes(&serialize_artifact(data, 1)).unwrap();
        let db = load(&data);
        assert!(!db.may_have_pattern_rows("ABC", Some(2010), -1));
        assert!(!db.may_have_pattern_rows("ABC", Some(2000), 50));
        assert!(db.may_have_pattern_rows("ABC", Some(2010), 50));
        assert!(db.may_have_pattern_rows("ABC", Some(2000), 100));
        assert!(!db.may_have_pattern_rows("ABC", Some(2010), 100));
        assert!(db.may_have_pattern_rows("ABC", None, 100));
        assert!(!db.may_have_pattern_rows("ABC", Some(1900), 100));
        assert!(!db.may_have_pattern_rows("unknown", None, 100));
        data.conversion.push(Conversion {
            id: 1,
            fromelementid: 39,
            toelementid: 28,
            formula: 2,
        });
        assert!(
            load(&data).may_have_pattern_rows("ABC", Some(1900), 100),
            "a conversion-produced Model can enable vehicle-spec pattern rows"
        );
        data.conversion[0].toelementid = 26;
        assert!(!load(&data).may_have_pattern_rows("ABC", Some(1900), 100));
        data.wmi_vinschema[0].yearto = crate::tables::NULL_I32;
        let db = load(&data);
        assert!(db.may_have_pattern_rows("ABC", Some(2999), 100));
        assert!(!db.may_have_pattern_rows("ABC", Some(3000), 100));
    }

    #[test]
    fn packed_wmis_preserve_length_and_embedded_nuls() {
        let strings = ["", "\0", "A", "A\0", "\0A", "ABC", "AB9DEF", "ABCDEFG", "é"];
        let keys: std::collections::HashSet<_> =
            strings.iter().map(|s| packed_wmi(s).unwrap()).collect();
        assert_eq!(keys.len(), strings.len());
        assert_eq!(packed_wmi("ABCDEFGH"), None);
        assert_eq!(packed_wmi("éééé"), None);
    }

    #[test]
    fn wmi_index_preserves_row_order_and_publication_boundaries() {
        let Some(db) = Db::try_embedded() else { return };
        let all = db.wmis();
        for row in all {
            let wmi = db.s(row.wmi.to_native());
            let start = all.partition_point(|w| db.s(w.wmi.to_native()) < wmi);
            let len = all[start..].partition_point(|w| db.s(w.wmi.to_native()) <= wmi);
            let expected = &all[start..start + len];
            let ids: Vec<_> = expected.iter().map(|w| w.id.to_native()).collect();
            assert_eq!(db.wmi_ids_for_str(wmi), ids, "{wmi}");
            assert!(std::ptr::eq(db.wmi_any(wmi).unwrap(), &expected[0]));
            let date = row.publicavailabilitydate.to_native();
            for now in [i64::MIN, 0, date.saturating_sub(1), date, i64::MAX] {
                let want = expected.iter().find(|w| w.is_public(now));
                assert_eq!(
                    db.wmi_by_str(wmi, now).map(|w| w as *const _),
                    want.map(|w| w as *const _),
                    "{wmi}, clock {now}"
                );
            }
        }
        for wmi in ["", "?", "ABC\0", "not a WMI", "a longer string", "éééé"] {
            let expected: Vec<_> = all
                .iter()
                .filter(|w| db.s(w.wmi.to_native()) == wmi)
                .map(|w| w.id.to_native())
                .collect();
            assert_eq!(db.wmi_ids_for_str(wmi), expected, "{wmi:?}");
        }
    }

    #[test]
    fn spec_model_index_preserves_the_original_join_and_order() {
        let Some(db) = Db::try_embedded() else { return };
        let mut pairs = std::collections::BTreeSet::new();
        for schema in db.vspecschemas() {
            for model in db.vspecschema_models_for(schema.id.to_native()) {
                pairs.insert((schema.makeid.to_native(), model.modelid.to_native()));
            }
        }
        pairs.insert((-1, -1));
        for (make, model) in pairs {
            let expected: Vec<i32> = db
                .vspecschemas_for_make(make)
                .iter()
                .filter(|s| {
                    db.vspecschema_models_for(s.id.to_native())
                        .iter()
                        .any(|m| m.modelid.to_native() == model)
                })
                .map(|s| s.id.to_native())
                .collect();
            let actual: Vec<i32> = db
                .vspecschemas_for_make_model(make, model)
                .map(|s| s.id.to_native())
                .collect();
            assert_eq!(actual, expected, "make {make}, model {model}");
        }
    }
}
