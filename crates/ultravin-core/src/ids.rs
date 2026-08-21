//! Projected batch decode: one typed column per requested `element_id`.
//!
//! The dataset path (see [`crate::parquet_io`]) decodes a chunk of rows and
//! keeps only the caller-named elements, emitted as parallel typed columns
//! instead of per-VIN element lists. Keying on the stable vPIC `element_id`
//! rather than the variable name means a monthly NHTSA dump that renames a
//! variable cannot silently break a pipeline.

use std::collections::HashMap;

use crate::db::Db;
use crate::hash::IntSet;
use crate::{decode_full, epoch_to_year, now_secs, public_decode};

/// Value type of a projected column, taken from the element's vPIC `data_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdsDType {
    /// `data_type` `lookup` or `string` — kept as text.
    Str,
    /// `data_type` `int`.
    Int,
    /// `data_type` `decimal`.
    Float,
}

/// Resolved request for one element id: its column type and output name.
#[derive(Debug, Clone)]
pub struct IdMeta {
    pub id: i32,
    pub dtype: IdsDType,
    /// The variable name (the column label in dataset output).
    pub name: String,
}

/// Validate requested element ids against the embedded archive.
///
/// An id that does not exist, is private, or never reaches decode output is a
/// caller mistake worth failing on up front — not a silent all-null column.
pub fn resolve_ids(db: &Db, ids: &[i32]) -> Result<Vec<IdMeta>, String> {
    let mut metas = Vec::with_capacity(ids.len());
    let mut seen = IntSet::<i32>::default();
    for &id in ids {
        if !seen.insert(id) {
            return Err(format!("element_id {id} requested more than once"));
        }
        let e = db.element_by_id(id).ok_or_else(|| {
            format!(
                "unknown element_id {id}; valid ids are in ultravin.ELEMENTS \
                 (pin element_id, not variable name)"
            )
        })?;
        if public_decode(db, e).is_none() {
            return Err(format!(
                "element_id {id} ({}) never appears in decode output",
                db.s(e.name.to_native())
            ));
        }
        let dtype = match db.s(e.datatype.to_native()) {
            "int" => IdsDType::Int,
            "decimal" => IdsDType::Float,
            _ => IdsDType::Str,
        };
        metas.push(IdMeta {
            id,
            dtype,
            name: db.s(e.name.to_native()).to_string(),
        });
    }
    Ok(metas)
}

/// Every public element id in archive order — the wide projection (`ids=None`).
pub fn all_public_ids(db: &Db) -> Vec<i32> {
    db.elements()
        .iter()
        .filter(|e| public_decode(db, e).is_some())
        .map(|e| e.id.to_native())
        .collect()
}

/// One projected column's values, typed by [`IdsDType`].
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnValues {
    Str(Vec<Option<String>>),
    Int(Vec<Option<i64>>),
    Float(Vec<Option<f64>>),
}

impl ColumnValues {
    pub fn len(&self) -> usize {
        match self {
            ColumnValues::Str(v) => v.len(),
            ColumnValues::Int(v) => v.len(),
            ColumnValues::Float(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A chunk of projected decodes: one row per input VIN, columns in request order.
///
/// Row alignment with the inputs is the contract: an undecodable VIN yields a
/// row of nulls (`model_year` included when even the year pass found nothing),
/// never a raise and never a dropped row.
#[derive(Debug, Clone, PartialEq)]
pub struct IdsBatch {
    pub model_year: Vec<Option<i32>>,
    pub columns: Vec<ColumnValues>,
}

/// Parse one decoded value into its column's type. Empty → null; a value that
/// does not parse as the declared numeric type is data the decoder should not
/// have emitted, but a bulk job must not die on it — null and move on.
fn parse_value(dtype: IdsDType, value: &str) -> Option<CellValue> {
    if value.is_empty() {
        return None;
    }
    Some(match dtype {
        IdsDType::Str => CellValue::Str(value.to_string()),
        IdsDType::Int => CellValue::Int(value.parse::<i64>().ok()?),
        IdsDType::Float => CellValue::Float(value.parse::<f64>().ok()?),
    })
}

/// Internal sum type so one extraction loop can fill any column kind.
#[derive(Debug, Clone)]
enum CellValue {
    Str(String),
    Int(i64),
    Float(f64),
}

/// Decode every input in parallel over the shared archive, projecting each
/// result down to the requested element columns. Output order matches `inputs`
/// and per-row semantics equal [`crate::decode`] with the matching year.
///
/// First occurrence wins for the repeat-exempt elements (the free-text notes):
/// a dataset column holds one value per row by construction, and the notes are
/// provenance, not competing facts.
pub fn decode_batch_ids(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    metas: &[IdMeta],
) -> IdsBatch {
    use rayon::prelude::*;

    let secs = now_secs();
    let now_micros = secs * 1_000_000;
    let current_year = epoch_to_year(secs);
    let db = Db::embedded();
    // element_id -> column index; filled once, read from every rayon task.
    let index: HashMap<i32, usize, crate::hash::FxBuildHasher> =
        metas.iter().enumerate().map(|(i, m)| (m.id, i)).collect();

    let mut model_year = vec![None; inputs.len()];
    // One flat buffer of `stride` cells per row rather than a `Vec` per row: at
    // 65k rows a chunk, the per-row allocation was the dominant cost here. The
    // `max(1)` keeps a row one cell wide when nothing is projected, because
    // `par_chunks_mut(0)` panics and a zero-width buffer would zip to no rows
    // at all — leaving `model_year` undecoded.
    let stride = metas.len().max(1);
    // Outer `Option` = "this element was already seen for this row", so the true
    // first occurrence wins for the repeat-exempt elements even when it is empty
    // (which is null, not "unfilled"). Inner `Option` = the value itself.
    let mut slots: Vec<Option<Option<CellValue>>> = vec![None; inputs.len() * stride];

    crate::batch_pool().install(|| {
        inputs
            .par_iter()
            .enumerate()
            .zip(&mut model_year)
            .zip(slots.par_chunks_mut(stride))
            .for_each(|(((i, vin), my), row)| {
                let r = decode_full(
                    db,
                    vin,
                    now_micros,
                    current_year,
                    years.and_then(|ys| ys.get(i)).copied().flatten(),
                );
                *my = r.model_year;
                for e in &r.elements {
                    if let Some(&ci) = index.get(&e.element_id) {
                        if row[ci].is_none() {
                            row[ci] = Some(parse_value(metas[ci].dtype, &e.value));
                        }
                    }
                }
            });
    });

    // Unwrap the per-row slots into typed columns (column-major output).
    let columns = metas
        .iter()
        .enumerate()
        .map(|(ci, m)| match m.dtype {
            IdsDType::Str => ColumnValues::Str(
                slots
                    .chunks_mut(stride)
                    .map(|row| match row[ci].take().flatten() {
                        Some(CellValue::Str(s)) => Some(s),
                        _ => None,
                    })
                    .collect(),
            ),
            IdsDType::Int => ColumnValues::Int(
                slots
                    .chunks_mut(stride)
                    .map(|row| match row[ci].take().flatten() {
                        Some(CellValue::Int(v)) => Some(v),
                        _ => None,
                    })
                    .collect(),
            ),
            IdsDType::Float => ColumnValues::Float(
                slots
                    .chunks_mut(stride)
                    .map(|row| match row[ci].take().flatten() {
                        Some(CellValue::Float(v)) => Some(v),
                        _ => None,
                    })
                    .collect(),
            ),
        })
        .collect();

    IdsBatch {
        model_year,
        columns,
    }
}

/// Top of the model-year window the parquet layer's year sniffer accepts.
pub fn year_upper_bound() -> i32 {
    epoch_to_year(now_secs()) + 2
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Element ids used below, with their vPIC `data_type` in parentheses. Ids
    /// are the stable key by contract — that is the whole reason this module
    /// projects on them instead of variable names.
    const MAKE: i32 = 26; // lookup  -> Str
    const CYLINDERS: i32 = 9; // int     -> Int
    const DISPLACEMENT_L: i32 = 13; // decimal -> Float

    const HONDA: &str = "1HGCM82633A004352";

    /// These tests need real data; a placeholder artifact decodes nothing, so
    /// they skip the way the rest of the crate's tests do.
    fn db() -> Option<&'static Db> {
        Db::try_embedded()
    }

    fn strs(c: &ColumnValues) -> &[Option<String>] {
        match c {
            ColumnValues::Str(v) => v,
            other => panic!("expected a Str column, got {other:?}"),
        }
    }

    #[test]
    fn resolve_ids_maps_data_type_to_column_type() {
        let Some(d) = db() else {
            return;
        };
        let metas = resolve_ids(d, &[MAKE, CYLINDERS, DISPLACEMENT_L]).unwrap();
        assert_eq!(metas.len(), 3);
        assert_eq!(metas[0].dtype, IdsDType::Str);
        assert_eq!(metas[0].name, "Make");
        assert_eq!(metas[1].dtype, IdsDType::Int);
        assert_eq!(metas[2].dtype, IdsDType::Float);
        // Request order is output order.
        assert_eq!(
            metas.iter().map(|m| m.id).collect::<Vec<_>>(),
            [MAKE, CYLINDERS, DISPLACEMENT_L]
        );
    }

    #[test]
    fn resolve_ids_rejects_an_unknown_id() {
        let Some(d) = db() else {
            return;
        };
        let err = resolve_ids(d, &[MAKE, 999_999]).unwrap_err();
        assert!(err.contains("unknown element_id 999999"), "{err}");
    }

    #[test]
    fn resolve_ids_rejects_a_repeated_id() {
        let Some(d) = db() else {
            return;
        };
        let err = resolve_ids(d, &[MAKE, CYLINDERS, MAKE]).unwrap_err();
        assert!(err.contains("more than once"), "{err}");
    }

    #[test]
    fn resolve_ids_rejects_an_element_that_never_reaches_output() {
        let Some(d) = db() else {
            return;
        };
        // Which ids are private/undecoded shifts with each vPIC dump, so pick
        // one from the archive rather than pinning a number that will rot.
        let hidden = d
            .elements()
            .iter()
            .find(|e| public_decode(d, e).is_none())
            .map(|e| e.id.to_native())
            .expect("the archive has at least one non-public element");
        let err = resolve_ids(d, &[hidden]).unwrap_err();
        assert!(err.contains("never appears in decode output"), "{err}");
        assert!(!all_public_ids(d).contains(&hidden));
    }

    #[test]
    fn all_public_ids_is_exactly_what_resolve_accepts() {
        let Some(d) = db() else {
            return;
        };
        let ids = all_public_ids(d);
        assert!(
            ids.len() > 100,
            "suspiciously few public ids: {}",
            ids.len()
        );
        assert_eq!(resolve_ids(d, &ids).unwrap().len(), ids.len());
    }

    #[test]
    fn decode_batch_ids_row_matches_a_single_decode() {
        if db().is_none() {
            return;
        }
        // Mixed corpus: a clean hit, an unregistered WMI, garbage, and empty —
        // with and without a caller year.
        let vins: Vec<String> = ["1FTFW1ET5DFC10312", HONDA, "ZZZCM82633A004352", "", "nope"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let years = [None, Some(2003), None, Some(2010), None];
        let metas = resolve_ids(Db::embedded(), &[MAKE, CYLINDERS, DISPLACEMENT_L]).unwrap();
        let batch = decode_batch_ids(&vins, Some(&years), &metas);

        assert_eq!(batch.model_year.len(), vins.len());
        assert_eq!(batch.columns.len(), 3);
        for c in &batch.columns {
            assert_eq!(c.len(), vins.len());
        }
        for (i, vin) in vins.iter().enumerate() {
            let r = crate::decode(vin, years[i]);
            assert_eq!(batch.model_year[i], r.model_year, "model_year for {vin:?}");
            for (ci, m) in metas.iter().enumerate() {
                let want = r
                    .elements
                    .iter()
                    .find(|e| e.element_id == m.id)
                    .map(|e| e.value.as_str())
                    .filter(|v| !v.is_empty());
                let what = format!("element {} for {vin:?}", m.id);
                match &batch.columns[ci] {
                    ColumnValues::Str(v) => assert_eq!(v[i].as_deref(), want, "{what}"),
                    ColumnValues::Int(v) => {
                        assert_eq!(v[i], want.and_then(|s| s.parse().ok()), "{what}");
                    }
                    ColumnValues::Float(v) => {
                        assert_eq!(v[i], want.and_then(|s| s.parse().ok()), "{what}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_bad_vin_is_a_null_row_not_a_raise() {
        if db().is_none() {
            return;
        }
        let vins = vec![String::new(), "nope".to_string(), HONDA.to_string()];
        let metas = resolve_ids(Db::embedded(), &[MAKE, CYLINDERS, DISPLACEMENT_L]).unwrap();
        let batch = decode_batch_ids(&vins, None, &metas);
        for (ci, _) in metas.iter().enumerate() {
            for row in 0..2 {
                let null = match &batch.columns[ci] {
                    ColumnValues::Str(v) => v[row].is_none(),
                    ColumnValues::Int(v) => v[row].is_none(),
                    ColumnValues::Float(v) => v[row].is_none(),
                };
                assert!(null, "column {ci} row {row} should be null");
            }
        }
        assert_eq!(batch.model_year[0], None);
        assert_eq!(batch.model_year[2], Some(2003));
        assert_eq!(strs(&batch.columns[0])[2].as_deref(), Some("HONDA"));
        assert_eq!(
            match &batch.columns[1] {
                ColumnValues::Int(v) => v[2],
                _ => unreachable!(),
            },
            Some(6)
        );
    }

    #[test]
    fn the_caller_year_reaches_the_decode() {
        if db().is_none() {
            return;
        }
        // 2013 is not this VIN's derivable year (2003) but is inside vPIC's
        // caller-year window, so the hint has to move the answer — otherwise
        // `years` is being dropped on the floor.
        let vins = vec![HONDA.to_string()];
        let metas = resolve_ids(Db::embedded(), &[MAKE]).unwrap();
        let hinted = decode_batch_ids(&vins, Some(&[Some(2013)]), &metas);
        let plain = decode_batch_ids(&vins, None, &metas);
        assert_eq!(plain.model_year, vec![Some(2003)]);
        assert_eq!(hinted.model_year, vec![Some(2013)]);
    }

    #[test]
    fn an_empty_projection_still_yields_aligned_rows() {
        if db().is_none() {
            return;
        }
        let vins = vec![HONDA.to_string(), "nope".to_string()];
        let batch = decode_batch_ids(&vins, None, &[]);
        assert!(batch.columns.is_empty());
        assert_eq!(batch.model_year, vec![Some(2003), None]);
    }
}
