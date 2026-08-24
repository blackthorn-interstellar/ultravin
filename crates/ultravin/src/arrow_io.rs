//! Arrow-native columnar decode: a `RecordBatch` of VINs in, a `RecordBatch` of
//! typed columns out. No file I/O and no source of its own — parquet row groups
//! ([`crate::parquet_io`]), an Arrow C stream handed over from pyarrow/polars,
//! and batches a caller built by hand all enter through the same door.
//!
//! One [`ArrowDecoder`] is built per input schema and then reused for every
//! batch of that stream: column resolution, projection validation, and the
//! output schema are settled once, so a chunk costs a decode and two casts.
//! Memory is O(batch) and flat in the input's width — only the VIN and
//! caller-year columns are read.

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{
    cast::AsArray, Array, ArrayRef, Float64Array, Int32Array, Int64Array, RecordBatch, StringArray,
};
use arrow_cast::cast;
use arrow_schema::{DataType, Field, Schema, SchemaRef};

use crate::db::Db;
use crate::ids::{decode_batch_ids, resolve_columns, ColumnSpec, ColumnValues, IdMeta, IdsDType};

/// `Io` covers a batch that will not cast or assemble; `Config` is a caller
/// mistake (missing/ambiguous columns, bad column requests) worth a
/// `ValueError`. [`crate::parquet_io`] reports its file-level failures through
/// the same enum, so one door's errors map to Python the way the other's do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrowError {
    Io(String),
    Config(String),
}

impl std::fmt::Display for ArrowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArrowError::Io(m) | ArrowError::Config(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for ArrowError {}

/// A decode stream reaches the Arrow C data interface as a `RecordBatchReader`,
/// whose error type is the arrow ecosystem's. `Config` rides across as
/// `InvalidArgumentError` and back, so a caller mistake that only surfaces
/// mid-stream still reads as one on the far side instead of decaying to "I/O".
impl From<ArrowError> for arrow_schema::ArrowError {
    fn from(e: ArrowError) -> arrow_schema::ArrowError {
        match e {
            ArrowError::Config(m) => arrow_schema::ArrowError::InvalidArgumentError(m),
            ArrowError::Io(m) => arrow_schema::ArrowError::ComputeError(m),
        }
    }
}

impl From<arrow_schema::ArrowError> for ArrowError {
    fn from(e: arrow_schema::ArrowError) -> ArrowError {
        match e {
            arrow_schema::ArrowError::InvalidArgumentError(m) => ArrowError::Config(m),
            // The two variants `From<ArrowError>` produces map back verbatim, so
            // a message that crosses the boundary twice is unchanged.
            arrow_schema::ArrowError::ComputeError(m) => ArrowError::Io(m),
            other => ArrowError::Io(other.to_string()),
        }
    }
}

/// The decoded model year's output column name. Reserved: an input carrying it
/// already would be shadowed.
pub const DECODED_YEAR: &str = "decoded_model_year";

/// Year-column candidates tried by name (case-insensitive) — the common names
/// across NHTSA, fleet-telemetry, and registration exports.
pub(crate) const YEAR_NAMES: [&str; 5] = [
    "year",
    "model_year",
    "model_yr_num",
    "veh_mfg_yr",
    "sf_model_year",
];

/// Which input columns to read and which elements to emit.
#[derive(Debug, Clone, Default)]
pub struct ArrowOpts {
    /// Input VIN column name; `None` → autodetect by name (`vin`).
    pub vin: Option<String>,
    /// Input caller-year column name; `None` → autodetect by name (absence is
    /// fine — those rows just decode without the hint).
    pub year: Option<String>,
    /// Elements to project, by id or name, in output order.
    pub columns: Vec<ColumnSpec>,
    /// How to label the projected columns; defaults to the variable name.
    pub names: ColumnNames,
}

/// A decoder bound to one input schema: `RecordBatch` in, `RecordBatch` out.
///
/// Output layout is `[vin, year?, decoded_model_year, ..projected]` — the VIN
/// (and the caller-year column, when there is one) passed through, the decoded
/// model year, then one typed column per requested element. Row order and row
/// count equal the input's: an undecodable VIN is a row of nulls, never a raise
/// and never a dropped row.
#[derive(Debug)]
pub struct ArrowDecoder {
    vin_idx: usize,
    year_idx: Option<usize>,
    metas: Vec<IdMeta>,
    out_schema: SchemaRef,
}

impl ArrowDecoder {
    /// Bind a decoder to `in_schema`, resolving columns by name.
    ///
    /// The VIN column must resolve — explicitly named, or exactly one field
    /// called `vin` (case-insensitively). A caller-year column is optional: it is
    /// taken from [`YEAR_NAMES`] when not named, and its absence is not an error.
    pub fn new(in_schema: &SchemaRef, opts: &ArrowOpts) -> Result<ArrowDecoder, ArrowError> {
        let metas = resolve_columns(Db::embedded(), &opts.columns).map_err(ArrowError::Config)?;
        let vin_idx = vin_by_name(in_schema, opts.vin.as_deref())?.ok_or_else(|| {
            ArrowError::Config(
                "could not autodetect a VIN-like column — pass the column name explicitly"
                    .to_string(),
            )
        })?;
        let year_idx = year_by_name(in_schema, opts.year.as_deref())?;
        ArrowDecoder::with_columns(in_schema, vin_idx, year_idx, metas, opts.names)
    }

    /// Bind a decoder to `in_schema` with the columns already resolved — for a
    /// caller that found them some other way ([`crate::parquet_io`] falls back to
    /// sniffing values when the names give nothing).
    pub fn with_columns(
        in_schema: &SchemaRef,
        vin_idx: usize,
        year_idx: Option<usize>,
        metas: Vec<IdMeta>,
        names: ColumnNames,
    ) -> Result<ArrowDecoder, ArrowError> {
        if vin_idx >= in_schema.fields().len() {
            return Err(ArrowError::Config(format!(
                "VIN column index {vin_idx} is past the end of a {}-column schema",
                in_schema.fields().len()
            )));
        }
        // A VIN column holding numbers would be cast to text and decoded as
        // garbage — silently, since an undecodable VIN is a null row by design.
        let vin_type = in_schema.field(vin_idx).data_type();
        if !is_stringish(vin_type) {
            return Err(ArrowError::Config(format!(
                "VIN column {:?} holds {vin_type}, not text",
                in_schema.field(vin_idx).name()
            )));
        }
        // One column cannot be both the VIN and the caller year: the output would
        // carry its name twice, with the second write winning.
        if year_idx == Some(vin_idx) {
            return Err(ArrowError::Config(format!(
                "column {:?} was named as both the VIN and the caller-year column",
                in_schema.field(vin_idx).name()
            )));
        }
        if let Some(yi) = year_idx.filter(|&yi| yi >= in_schema.fields().len()) {
            return Err(ArrowError::Config(format!(
                "caller-year column index {yi} is past the end of a {}-column schema",
                in_schema.fields().len()
            )));
        }
        // A caller-year column that is not a number casts to nonsense rather than
        // failing: a Date32 becomes days-since-epoch, a Boolean becomes 0/1. Every
        // value then lands outside vPIC's [1980, current+2] window, so the hint is
        // discarded *and* error code 12 is stamped on every row — a whole-dataset
        // corruption that looks like a decode result. Floats are fine: that is what
        // pandas gives an integer column with a missing value, and the cast is safe
        // (`NaN` and out-of-range become null, i.e. no hint for that row).
        if let Some(yi) = year_idx {
            let year_type = in_schema.field(yi).data_type();
            if !is_yearish(year_type) {
                return Err(ArrowError::Config(format!(
                    "caller-year column {:?} holds {year_type}, not a number; \
                     cast it to an integer or float column first",
                    in_schema.field(yi).name()
                )));
            }
        }
        let out_schema = build_out_schema(in_schema, vin_idx, year_idx, &metas, names)?;
        Ok(ArrowDecoder {
            vin_idx,
            year_idx,
            metas,
            out_schema,
        })
    }

    /// The schema every batch this decoder emits carries.
    pub fn out_schema(&self) -> &SchemaRef {
        &self.out_schema
    }

    /// The resolved projection, in output order.
    pub fn columns(&self) -> &[IdMeta] {
        &self.metas
    }

    /// The input column indices this decoder reads.
    pub fn vin_index(&self) -> usize {
        self.vin_idx
    }

    /// The resolved caller-year column, if the input has one.
    pub fn year_index(&self) -> Option<usize> {
        self.year_idx
    }

    /// Decode + project one batch into [`out_schema`](Self::out_schema).
    ///
    /// A null VIN decodes as the empty string but stays null on the passthrough
    /// column. The batch must have the schema this decoder was built for; only
    /// the two columns it reads are checked.
    pub fn decode_batch(&self, batch: &RecordBatch) -> Result<RecordBatch, ArrowError> {
        if batch.num_columns() <= self.vin_idx
            || self.year_idx.is_some_and(|yi| batch.num_columns() <= yi)
        {
            return Err(ArrowError::Config(format!(
                "batch has {} columns, too few for the schema this decoder was built for",
                batch.num_columns()
            )));
        }
        let vin_arr = cast(batch.column(self.vin_idx), &DataType::Utf8)
            .map_err(|e| ArrowError::Io(format!("casting VIN column: {e}")))?;
        let vin = vin_arr.as_string::<i32>();
        let vins: Vec<String> = (0..vin.len())
            .map(|i| {
                if vin.is_null(i) {
                    String::new()
                } else {
                    vin.value(i).to_string()
                }
            })
            .collect();

        // The caller-year column is cast once and reused: the kernel reads it and
        // the passthrough emits it, both as Int32.
        let year_arr =
            match self.year_idx {
                None => None,
                Some(yi) => Some(cast(batch.column(yi), &DataType::Int32).map_err(|e| {
                    ArrowError::Io(format!("casting caller-year column to int: {e}"))
                })?),
            };
        let years: Option<Vec<Option<i32>>> = year_arr.as_ref().map(|a| {
            let y = a.as_primitive::<arrow_array::types::Int32Type>();
            (0..y.len())
                .map(|i| (!y.is_null(i)).then(|| y.value(i)))
                .collect()
        });

        let out = decode_batch_ids(&vins, years.as_deref(), &self.metas);

        let mut cols: Vec<ArrayRef> = Vec::with_capacity(self.metas.len() + 3);
        cols.push(vin_arr);
        cols.extend(year_arr);
        cols.push(Arc::new(Int32Array::from(out.model_year)));
        for col in out.columns {
            cols.push(match col {
                ColumnValues::Str(v) => Arc::new(StringArray::from(v)) as ArrayRef,
                ColumnValues::Int(v) => Arc::new(Int64Array::from(v)),
                ColumnValues::Float(v) => Arc::new(Float64Array::from(v)),
            });
        }
        RecordBatch::try_new(self.out_schema.clone(), cols)
            .map_err(|e| ArrowError::Io(format!("assembling output batch: {e}")))
    }
}

/// Whole numbers, including the dictionary encoding an integer column can
/// arrive under. This gates *autodetection* — which columns the parquet sniffer
/// is willing to guess are the caller year — so it stays narrow: a float column
/// is accepted when the caller names one ([`is_yearish`]) but never guessed at,
/// because guessing wider would turn any float column in the model-year range
/// into a second candidate and make a previously fine dataset ambiguous.
pub(crate) fn is_intish(dt: &DataType) -> bool {
    match dt {
        DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64 => true,
        DataType::Dictionary(_, inner) => is_intish(inner),
        _ => false,
    }
}

/// A dtype a caller-year column is allowed to hold: whole numbers, or the floats
/// pandas hands over for an integer column containing a missing value — the
/// single most common way a real year column arrives. Casting those is lossless
/// for the values that matter and yields null for `NaN`, which simply means "no
/// hint for this row".
///
/// Wider than [`is_intish`] on purpose; see there for why autodetect stays narrow.
pub(crate) fn is_yearish(dt: &DataType) -> bool {
    match dt {
        DataType::Float16 | DataType::Float32 | DataType::Float64 => true,
        DataType::Dictionary(_, inner) => is_yearish(inner),
        other => is_intish(other),
    }
}

/// Text, including the dictionary encoding pandas gives a categorical column —
/// the cast in [`ArrowDecoder::decode_batch`] flattens it either way, so column
/// detection has no reason to skip it.
pub(crate) fn is_stringish(dt: &DataType) -> bool {
    match dt {
        DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Binary
        | DataType::LargeBinary
        | DataType::BinaryView => true,
        DataType::Dictionary(_, inner) => is_stringish(inner),
        _ => false,
    }
}

pub(crate) fn col_index(schema: &SchemaRef, name: &str) -> Option<usize> {
    schema.fields().iter().position(|f| f.name() == name)
}

pub(crate) fn indices(schema: &SchemaRef, pred: impl Fn(&str) -> bool) -> Vec<usize> {
    schema
        .fields()
        .iter()
        .enumerate()
        .filter(|(_, f)| pred(f.name()))
        .map(|(i, _)| i)
        .collect()
}

fn names(schema: &SchemaRef, idx: &[usize]) -> String {
    idx.iter()
        .map(|&i| schema.field(i).name().as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Resolve the VIN column by name: the caller's if it named one, else the single
/// field called `vin`. `Ok(None)` = no name matched, which is the caller's cue to
/// look further (parquet sniffs values) or to give up.
pub(crate) fn vin_by_name(
    schema: &SchemaRef,
    named: Option<&str>,
) -> Result<Option<usize>, ArrowError> {
    if let Some(name) = named {
        return col_index(schema, name)
            .ok_or_else(|| ArrowError::Config(format!("no column named {name:?}")))
            .map(Some);
    }
    let found = indices(schema, |n| n.eq_ignore_ascii_case("vin"));
    match found.len() {
        1 => Ok(Some(found[0])),
        0 => Ok(None),
        _ => Err(ArrowError::Config(format!(
            "ambiguous VIN column: {} all match by name",
            names(schema, &found)
        ))),
    }
}

/// Resolve the caller-year column by name, over [`YEAR_NAMES`]. `Ok(None)` = no
/// name matched; unlike the VIN, having no year column at all is fine.
pub(crate) fn year_by_name(
    schema: &SchemaRef,
    named: Option<&str>,
) -> Result<Option<usize>, ArrowError> {
    if let Some(name) = named {
        return col_index(schema, name)
            .ok_or_else(|| ArrowError::Config(format!("no column named {name:?}")))
            .map(Some);
    }
    let mut found = Vec::new();
    for cand in YEAR_NAMES {
        found.extend(indices(schema, |n| n.eq_ignore_ascii_case(cand)));
    }
    match found.len() {
        1 => Ok(Some(found[0])),
        0 => Ok(None),
        _ => Err(ArrowError::Config(format!(
            "ambiguous caller-year column: {}",
            names(schema, &found)
        ))),
    }
}

/// Arrow field-metadata keys carried by every projected column, in both naming
/// modes: the stable vPIC `element_id` and the variable name it currently has.
///
/// Whichever of the two the column is *labelled* with, the other is the one a
/// consumer would otherwise have to re-derive — so both travel with the schema.
/// `ArrowWriter` embeds field metadata in the parquet footer, so they survive a
/// round-trip to disk.
pub const ELEMENT_ID_KEY: &str = "element_id";
pub const VARIABLE_KEY: &str = "variable";

/// The label prefix under [`ColumnNames::Id`].
const ATTR_PREFIX: &str = "attr_";

/// How to label the projected output columns.
///
/// This is a schema-drift decision, not a cosmetic one. NHTSA renames variables
/// between monthly data releases, so a pipeline whose columns are named after
/// them silently changes shape on a refresh; the `element_id` never moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColumnNames {
    /// The vPIC variable name (`Make`, `Displacement (L)`). Readable, and what a
    /// human wants when the output is the end of the road.
    #[default]
    Variable,
    /// `attr_<element_id>` (`attr_26`). Stable across data refreshes, and what a
    /// long-lived table or a downstream schema should be pinned to.
    Id,
}

impl ColumnNames {
    /// The output label for one projected element.
    fn label(self, m: &IdMeta) -> String {
        match self {
            ColumnNames::Variable => m.name.clone(),
            ColumnNames::Id => format!("{ATTR_PREFIX}{}", m.id),
        }
    }
}

/// The output schema: passthrough vin (+ year), [`DECODED_YEAR`], then one typed
/// field per projected element. A projection label colliding with a passthrough
/// name would silently merge two different things — refuse.
fn build_out_schema(
    in_schema: &SchemaRef,
    vin_idx: usize,
    year_idx: Option<usize>,
    metas: &[IdMeta],
    names: ColumnNames,
) -> Result<SchemaRef, ArrowError> {
    let mut fields: Vec<Field> = Vec::with_capacity(metas.len() + 3);
    fields.push(Field::new(
        in_schema.field(vin_idx).name().clone(),
        DataType::Utf8,
        true,
    ));
    let mut taken: Vec<String> = vec![in_schema.field(vin_idx).name().clone()];
    if let Some(yi) = year_idx {
        let name = in_schema.field(yi).name().clone();
        taken.push(name.clone());
        fields.push(Field::new(name, DataType::Int32, true));
    }
    if taken.iter().any(|t| t == DECODED_YEAR) {
        return Err(ArrowError::Config(format!(
            "input already has a column named {DECODED_YEAR}; remove or rename it"
        )));
    }
    taken.push(DECODED_YEAR.to_string());
    fields.push(Field::new(DECODED_YEAR, DataType::Int32, true));
    for m in metas {
        let dt = match m.dtype {
            IdsDType::Str => DataType::Utf8,
            IdsDType::Int => DataType::Int64,
            IdsDType::Float => DataType::Float64,
        };
        // The label depends on the naming mode, so the collision check has to be
        // on the label — an input column called `attr_26` collides under `Id`
        // naming exactly as one called `Make` does under `Variable`.
        let label = names.label(m);
        if taken.contains(&label) {
            return Err(ArrowError::Config(format!(
                "projected column {label:?} collides with a passthrough column of the same name; \
                 rename the input column or drop that element from the projection"
            )));
        }
        taken.push(label.clone());
        // Both keys, both modes: whichever the label spells out, the other is
        // what a consumer would otherwise have to re-derive.
        fields.push(Field::new(label, dt, true).with_metadata(HashMap::from([
            (ELEMENT_ID_KEY.to_string(), m.id.to_string()),
            (VARIABLE_KEY.to_string(), m.name.clone()),
        ])));
    }
    Ok(Arc::new(Schema::new(fields)))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAKE: i32 = 26; // lookup  -> Utf8
    const CYLINDERS: i32 = 9; // int     -> Int64
    const DISPLACEMENT_L: i32 = 13; // decimal -> Float64

    const HONDA: &str = "1HGCM82633A004352";
    const FORD: &str = "1FTFW1ET5DFC10312";

    fn loaded() -> bool {
        Db::try_embedded().is_some()
    }

    fn opts(columns: Vec<ColumnSpec>) -> ArrowOpts {
        ArrowOpts {
            columns,
            ..ArrowOpts::default()
        }
    }

    fn ids(list: &[i32]) -> Vec<ColumnSpec> {
        list.iter().copied().map(ColumnSpec::Id).collect()
    }

    fn batch(cols: Vec<(Field, ArrayRef)>) -> RecordBatch {
        let (fields, arrays): (Vec<_>, Vec<_>) = cols.into_iter().unzip();
        RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).expect("input batch")
    }

    fn utf8(name: &str, values: &[Option<&str>]) -> (Field, ArrayRef) {
        (
            Field::new(name, DataType::Utf8, true),
            Arc::new(StringArray::from(values.to_vec())),
        )
    }

    fn i32s(name: &str, values: &[Option<i32>]) -> (Field, ArrayRef) {
        (
            Field::new(name, DataType::Int32, true),
            Arc::new(Int32Array::from(values.to_vec())),
        )
    }

    fn out_names(b: &RecordBatch) -> Vec<String> {
        b.schema()
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect()
    }

    fn col_utf8(b: &RecordBatch, name: &str) -> Vec<Option<String>> {
        let a = b.column_by_name(name).expect(name).as_string::<i32>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i).to_string()))
            .collect()
    }

    fn col_i32(b: &RecordBatch, name: &str) -> Vec<Option<i32>> {
        let a = b
            .column_by_name(name)
            .expect(name)
            .as_primitive::<arrow_array::types::Int32Type>();
        (0..a.len())
            .map(|i| (!a.is_null(i)).then(|| a.value(i)))
            .collect()
    }

    #[test]
    fn a_hand_built_batch_decodes_into_typed_columns() {
        if !loaded() {
            return;
        }
        let input = batch(vec![
            utf8("vin", &[Some(HONDA), Some(FORD), None, Some("nope")]),
            i32s("year", &[None, None, Some(2010), None]),
        ]);
        let d = ArrowDecoder::new(
            &input.schema(),
            &opts(ids(&[MAKE, CYLINDERS, DISPLACEMENT_L])),
        )
        .expect("decoder");
        assert_eq!(d.vin_index(), 0);
        assert_eq!(d.year_index(), Some(1));

        let out = d.decode_batch(&input).expect("decode");
        assert_eq!(&out.schema(), d.out_schema());
        // Passthrough, decoded year, then the projection labelled by variable
        // name — which the archive owns, so read it back rather than pinning it.
        let mut want = vec![
            "vin".to_string(),
            "year".to_string(),
            DECODED_YEAR.to_string(),
        ];
        want.extend(d.columns().iter().map(|m| m.name.clone()));
        assert_eq!(out_names(&out), want);
        assert_eq!(d.columns()[0].name, "Make");
        // Row count and order are the input's, misses included.
        assert_eq!(out.num_rows(), 4);
        assert_eq!(
            col_utf8(&out, "vin"),
            [
                Some(HONDA.to_string()),
                Some(FORD.to_string()),
                None,
                Some("nope".to_string())
            ]
        );
        assert_eq!(col_utf8(&out, "Make")[0].as_deref(), Some("HONDA"));
        assert_eq!(col_i32(&out, DECODED_YEAR)[0], Some(2003));
        // A garbage VIN with no hint decodes to nothing; the passthrough year is
        // still handed to the decode, so row 2's null VIN keeps its 2010 hint.
        assert_eq!(col_i32(&out, DECODED_YEAR)[3], None);
        assert_eq!(col_utf8(&out, "Make")[3], None);
        assert_eq!(col_i32(&out, "year")[2], Some(2010));
        assert_eq!(col_i32(&out, DECODED_YEAR)[2], Some(2010));
        // Each projected column keeps the type its element declares.
        assert_eq!(
            out.column(4).data_type(),
            &DataType::Int64,
            "an int element projects to Int64"
        );
        assert_eq!(out.column(5).data_type(), &DataType::Float64);
    }

    #[test]
    fn id_naming_labels_the_projection_attr_element_id() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA)]), i32s("year", &[None])]);
        let named = ArrowOpts {
            names: ColumnNames::Id,
            ..opts(ids(&[MAKE, CYLINDERS]))
        };
        let d = ArrowDecoder::new(&input.schema(), &named).expect("decoder");
        // Passthrough columns keep their own names in both modes; only the
        // projection is renamed.
        assert_eq!(
            out_names(&d.decode_batch(&input).expect("decode")),
            ["vin", "year", DECODED_YEAR, "attr_26", "attr_9"]
        );
    }

    #[test]
    fn both_naming_modes_decode_to_the_same_values() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA), Some("nope")])]);
        let by_name = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE])))
            .expect("decoder")
            .decode_batch(&input)
            .expect("decode");
        let by_id = ArrowDecoder::new(
            &input.schema(),
            &ArrowOpts {
                names: ColumnNames::Id,
                ..opts(ids(&[MAKE]))
            },
        )
        .expect("decoder")
        .decode_batch(&input)
        .expect("decode");
        assert_eq!(col_utf8(&by_name, "Make"), col_utf8(&by_id, "attr_26"));
        assert_eq!(by_name.num_rows(), by_id.num_rows());
    }

    #[test]
    fn an_attr_label_collides_the_way_a_variable_name_does() {
        if !loaded() {
            return;
        }
        // A source column literally called `attr_26` would be shadowed by the
        // projection under `Id` naming, exactly as one called `Make` is under
        // `Variable` naming.
        let input = batch(vec![utf8("attr_26", &[Some(HONDA)])]);
        let named = ArrowOpts {
            vin: Some("attr_26".to_string()),
            names: ColumnNames::Id,
            ..opts(ids(&[MAKE]))
        };
        let err = ArrowDecoder::new(&input.schema(), &named).unwrap_err();
        assert!(
            format!("{err}").contains("collides with a passthrough"),
            "{err}"
        );
        // ...and under variable naming that same input is fine, because the
        // labels no longer clash.
        let fine = ArrowOpts {
            vin: Some("attr_26".to_string()),
            ..opts(ids(&[MAKE]))
        };
        assert!(ArrowDecoder::new(&input.schema(), &fine).is_ok());
    }

    #[test]
    fn every_projected_field_carries_its_element_id() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA)]), i32s("year", &[None])]);
        let d =
            ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE, CYLINDERS]))).expect("decoder");
        let schema = d.out_schema();
        // The passthrough and the decoded year are not elements, so they carry
        // nothing; each projected column carries the id it was resolved from.
        for name in ["vin", "year", DECODED_YEAR] {
            let (_, f) = schema.column_with_name(name).expect(name);
            assert!(f.metadata().is_empty(), "{name} should carry no metadata");
        }
        for m in d.columns() {
            let (_, f) = schema.column_with_name(&m.name).expect(&m.name);
            assert_eq!(f.metadata().get(ELEMENT_ID_KEY), Some(&m.id.to_string()));
            assert_eq!(f.metadata().get(VARIABLE_KEY), Some(&m.name));
        }
    }

    #[test]
    fn id_naming_still_carries_the_variable_name_in_metadata() {
        if !loaded() {
            return;
        }
        // The whole point of the pair: whichever key the label spells out, the
        // other is the one a consumer would otherwise have to re-derive.
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let d = ArrowDecoder::new(
            &input.schema(),
            &ArrowOpts {
                names: ColumnNames::Id,
                ..opts(ids(&[MAKE]))
            },
        )
        .expect("decoder");
        let (_, f) = d.out_schema().column_with_name("attr_26").expect("attr_26");
        assert_eq!(f.metadata().get(ELEMENT_ID_KEY), Some(&"26".to_string()));
        assert_eq!(f.metadata().get(VARIABLE_KEY), Some(&"Make".to_string()));
    }

    #[test]
    fn a_projection_can_be_named_instead_of_numbered() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let d = ArrowDecoder::new(
            &input.schema(),
            &opts(vec![ColumnSpec::Name("Make".into())]),
        )
        .expect("decoder");
        assert_eq!(d.year_index(), None, "no year column is not an error");
        let out = d.decode_batch(&input).expect("decode");
        assert_eq!(out_names(&out), ["vin", DECODED_YEAR, "Make"]);
        assert_eq!(col_utf8(&out, "Make"), [Some("HONDA".to_string())]);
    }

    #[test]
    fn the_caller_year_column_reaches_the_decode() {
        if !loaded() {
            return;
        }
        // 2013 is not this VIN's derivable year (2003) but is inside vPIC's
        // caller-year window, so the hint has to move the answer.
        let input = batch(vec![
            utf8("vin", &[Some(HONDA), Some(HONDA)]),
            i32s("model_year", &[Some(2013), None]),
        ]);
        let d = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).expect("decoder");
        let out = d.decode_batch(&input).expect("decode");
        assert_eq!(col_i32(&out, DECODED_YEAR), [Some(2013), Some(2003)]);
    }

    #[test]
    fn an_empty_projection_still_passes_the_rows_through() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA), Some("nope")])]);
        let d = ArrowDecoder::new(&input.schema(), &opts(Vec::new())).expect("decoder");
        let out = d.decode_batch(&input).expect("decode");
        assert_eq!(out_names(&out), ["vin", DECODED_YEAR]);
        assert_eq!(col_i32(&out, DECODED_YEAR), [Some(2003), None]);
    }

    #[test]
    fn a_zero_row_batch_is_a_zero_row_batch() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[])]);
        let d = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).expect("decoder");
        let out = d.decode_batch(&input).expect("decode");
        assert_eq!(out.num_rows(), 0);
        assert_eq!(out_names(&out), ["vin", DECODED_YEAR, "Make"]);
    }

    #[test]
    fn column_detection_is_by_name_only_here() {
        if !loaded() {
            return;
        }
        // Value sniffing is the parquet layer's job: a VIN-shaped column not
        // called "vin" is an error at this level, not a lucky guess.
        let input = batch(vec![utf8("chassis", &[Some(HONDA)])]);
        let err = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).unwrap_err();
        assert!(format!("{err}").contains("could not autodetect"), "{err}");
        // ...but naming it works.
        let named = ArrowOpts {
            vin: Some("chassis".to_string()),
            ..opts(ids(&[MAKE]))
        };
        let d = ArrowDecoder::new(&input.schema(), &named).expect("decoder");
        assert_eq!(
            col_utf8(&d.decode_batch(&input).unwrap(), "Make")[0].as_deref(),
            Some("HONDA")
        );
    }

    #[test]
    fn a_vin_column_that_is_not_text_is_refused() {
        if !loaded() {
            return;
        }
        let input = batch(vec![i32s("vin", &[Some(1)])]);
        let err = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).unwrap_err();
        assert!(format!("{err}").contains("not text"), "{err}");
    }

    #[test]
    fn a_caller_year_column_that_is_not_a_number_is_refused() {
        if !loaded() {
            return;
        }
        // A Date32 casts to days-since-epoch and a Boolean to 0/1; either way
        // every row falls outside vPIC's year window, so the hint is dropped and
        // error code 12 is stamped across the whole dataset. Refuse instead.
        for dt in [DataType::Date32, DataType::Boolean, DataType::Utf8] {
            let input = batch(vec![
                utf8("vin", &[Some(HONDA)]),
                (
                    Field::new("year", dt.clone(), true),
                    arrow_cast::cast(&Int32Array::from(vec![Some(1)]), &dt).expect("cast"),
                ),
            ]);
            let err = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).unwrap_err();
            let msg = format!("{err}");
            assert!(msg.contains("caller-year column \"year\""), "{msg}");
            assert!(msg.contains("not a number"), "{msg}");
        }
        // An integer column of any width is still fine.
        let ok = batch(vec![
            utf8("vin", &[Some(HONDA)]),
            i32s("year", &[Some(2013)]),
        ]);
        let d = ArrowDecoder::new(&ok.schema(), &opts(ids(&[MAKE]))).expect("decoder");
        assert_eq!(
            col_i32(&d.decode_batch(&ok).unwrap(), DECODED_YEAR),
            [Some(2013)]
        );
    }

    #[test]
    fn a_float_caller_year_column_decodes_and_nan_means_no_hint() {
        if !loaded() {
            return;
        }
        // pandas hands an integer column with a missing value back as float64, so
        // this is how a real year column most often arrives. The cast is safe:
        // 2013.0 is the hint, NaN is simply no hint for that row.
        let input = batch(vec![
            utf8("vin", &[Some(HONDA), Some(HONDA)]),
            (
                Field::new("year", DataType::Float64, true),
                Arc::new(Float64Array::from(vec![Some(2013.0), Some(f64::NAN)])) as ArrayRef,
            ),
        ]);
        let d = ArrowDecoder::new(&input.schema(), &opts(ids(&[MAKE]))).expect("decoder");
        let out = d.decode_batch(&input).expect("decode");
        // Row 0 takes the hint; row 1 falls back to the VIN's own 2003.
        assert_eq!(col_i32(&out, DECODED_YEAR), [Some(2013), Some(2003)]);
        // The passthrough column is emitted as Int32, with NaN carried through as
        // null rather than as some arbitrary integer.
        assert_eq!(col_i32(&out, "year"), [Some(2013), None]);
    }

    #[test]
    fn a_bad_column_request_is_a_config_error() {
        if !loaded() {
            return;
        }
        let input = batch(vec![utf8("vin", &[Some(HONDA)])]);
        let err =
            ArrowDecoder::new(&input.schema(), &opts(vec![ColumnSpec::Id(999_999)])).unwrap_err();
        assert!(matches!(err, ArrowError::Config(_)), "{err}");
        assert!(format!("{err}").contains("unknown element_id"), "{err}");
    }

    #[test]
    fn a_decoded_year_column_in_the_input_is_refused() {
        if !loaded() {
            return;
        }
        let input = batch(vec![
            utf8("vin", &[Some(HONDA)]),
            i32s(DECODED_YEAR, &[Some(2003)]),
        ]);
        let named = ArrowOpts {
            year: Some(DECODED_YEAR.to_string()),
            ..opts(ids(&[MAKE]))
        };
        let err = ArrowDecoder::new(&input.schema(), &named).unwrap_err();
        assert!(format!("{err}").contains("remove or rename it"), "{err}");
    }
}
