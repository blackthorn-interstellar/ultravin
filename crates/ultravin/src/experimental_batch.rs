//! Experimental batch-owned storage for full native decode output.
//!
//! Each bounded shard owns one contiguous element vector. Projection writes
//! directly into it, avoiding the allocation of one `Vec<DecodedElement>` per
//! VIN. Header strings, error-code vectors, and computed `Cow::Owned` element
//! strings remain individually allocated. The existing decode APIs are unchanged.

use std::borrow::Cow;
use std::mem::size_of;
use std::ops::Range;

use rayon::prelude::*;
use serde::ser::SerializeStruct;

use crate::{BatchResults, Db, DecodeResult, DecodedElement, RawResult};

const DEFAULT_ROWS_PER_SHARD: usize = 256;

/// Tuning options for the experimental slab batch path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlabOptions {
    /// Maximum VINs decoded into one worker-owned element slab.
    pub rows_per_shard: usize,
}

impl Default for SlabOptions {
    fn default() -> Self {
        Self {
            rows_per_shard: DEFAULT_ROWS_PER_SHARD,
        }
    }
}

#[derive(Debug)]
struct StoredRow {
    input_index: usize,
    vin: String,
    wmi: String,
    descriptor: String,
    model_year: Option<i32>,
    error_codes: Vec<i32>,
    check_digit_valid: bool,
    corrected_vin: String,
    elements: Range<usize>,
}

#[derive(Debug)]
struct Shard<'db> {
    rows: Vec<StoredRow>,
    elements: Vec<DecodedElement<'db>>,
}

impl<'db> Shard<'db> {
    fn with_capacity(rows: usize, elements: usize) -> Self {
        Self {
            rows: Vec::with_capacity(rows),
            elements: Vec::with_capacity(elements),
        }
    }

    fn push(&mut self, input_index: usize, mut raw: RawResult<'db>) {
        crate::resolve::resolve_xxx(raw.db, &mut raw.items);
        let elements = crate::project_into(raw.db, raw.items, &mut self.elements);
        self.rows.push(StoredRow {
            input_index,
            vin: raw.vin,
            wmi: raw.wmi,
            descriptor: raw.descriptor,
            model_year: raw.model_year,
            error_codes: raw.error_codes,
            check_digit_valid: raw.check_digit_valid,
            corrected_vin: raw.corrected_vin,
            elements,
        });
    }
}

/// Full decode results whose large element lists are owned by bounded shards.
#[derive(Debug)]
pub struct SlabBatch<'db> {
    shards: Option<Vec<Shard<'db>>>,
    locations: Vec<(usize, usize)>,
}

impl<'db> SlabBatch<'db> {
    fn shards(&self) -> &[Shard<'db>] {
        self.shards.as_deref().unwrap_or_default()
    }

    pub fn len(&self) -> usize {
        self.locations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.locations.is_empty()
    }

    pub fn get(&self, index: usize) -> Option<SlabResultRef<'_, 'db>> {
        let &(shard, row) = self.locations.get(index)?;
        Some(SlabResultRef {
            shard: &self.shards()[shard],
            row: &self.shards()[shard].rows[row],
        })
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = SlabResultRef<'_, 'db>> + '_ {
        (0..self.len()).map(|index| self.get(index).expect("valid slab location"))
    }

    /// Convert to the established owned shape. This intentionally recreates
    /// every per-VIN element vector and is useful as a compatibility/control path.
    pub fn into_owned(mut self) -> BatchResults<DecodeResult<'db>> {
        let shards = self.shards.take().unwrap_or_default();
        let output_len = std::mem::take(&mut self.locations).len();
        let converted: Vec<Vec<(usize, DecodeResult<'db>)>> = crate::install_batch_work(|| {
            shards
                .into_par_iter()
                .map(|shard| {
                    let mut elements = shard.elements.into_iter();
                    let rows = shard
                        .rows
                        .into_iter()
                        .map(|row| {
                            let element_count = row.elements.len();
                            let elements = elements.by_ref().take(element_count).collect();
                            (
                                row.input_index,
                                DecodeResult {
                                    vin: row.vin,
                                    wmi: row.wmi,
                                    descriptor: row.descriptor,
                                    model_year: row.model_year,
                                    error_codes: row.error_codes,
                                    check_digit_valid: row.check_digit_valid,
                                    corrected_vin: row.corrected_vin,
                                    elements,
                                },
                            )
                        })
                        .collect();
                    debug_assert!(elements.next().is_none());
                    rows
                })
                .collect()
        });
        let mut slots: Vec<Option<DecodeResult<'db>>> = (0..output_len).map(|_| None).collect();
        for shard in converted {
            for (input_index, result) in shard {
                slots[input_index] = Some(result);
            }
        }
        BatchResults::new(
            slots
                .into_iter()
                .map(|slot| slot.expect("every slab output slot was initialized"))
                .collect(),
        )
    }

    /// Capacity-backed bytes owned by this container, including retained String,
    /// error-code, and computed element-string buffers. Allocator metadata is excluded.
    pub fn allocated_bytes(&self) -> usize {
        let mut bytes = self.shards.as_ref().map_or(0, |v| v.capacity()) * size_of::<Shard<'_>>()
            + self.locations.capacity() * size_of::<(usize, usize)>();
        for shard in self.shards() {
            bytes += shard.rows.capacity() * size_of::<StoredRow>();
            bytes += shard.elements.capacity() * size_of::<DecodedElement<'_>>();
            for row in &shard.rows {
                bytes += row.vin.capacity()
                    + row.wmi.capacity()
                    + row.descriptor.capacity()
                    + row.corrected_vin.capacity()
                    + row.error_codes.capacity() * size_of::<i32>();
            }
            for element in &shard.elements {
                bytes += cow_capacity(&element.value)
                    + cow_capacity(&element.attribute_id)
                    + cow_capacity(&element.source)
                    + cow_capacity(&element.keys);
            }
        }
        bytes
    }
}

impl Drop for SlabBatch<'_> {
    fn drop(&mut self) {
        let Some(shards) = self.shards.take() else {
            return;
        };
        if self.locations.len() < 512 {
            drop(shards);
            return;
        }
        crate::install_batch_work(|| shards.into_par_iter().for_each(drop));
    }
}

// The owned/borrowed variant and String capacity are required for this estimate.
#[allow(clippy::ptr_arg)]
fn cow_capacity(value: &Cow<'_, str>) -> usize {
    match value {
        Cow::Borrowed(_) => 0,
        Cow::Owned(value) => value.capacity(),
    }
}

impl serde::Serialize for SlabBatch<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.iter())
    }
}

/// Borrowed view of one result in a [`SlabBatch`].
#[derive(Debug, Clone, Copy)]
pub struct SlabResultRef<'batch, 'db> {
    shard: &'batch Shard<'db>,
    row: &'batch StoredRow,
}

impl<'batch, 'db> SlabResultRef<'batch, 'db> {
    pub fn vin(self) -> &'batch str {
        &self.row.vin
    }
    pub fn wmi(self) -> &'batch str {
        &self.row.wmi
    }
    pub fn descriptor(self) -> &'batch str {
        &self.row.descriptor
    }
    pub fn model_year(self) -> Option<i32> {
        self.row.model_year
    }
    pub fn error_codes(self) -> &'batch [i32] {
        &self.row.error_codes
    }
    pub fn check_digit_valid(self) -> bool {
        self.row.check_digit_valid
    }
    pub fn corrected_vin(self) -> &'batch str {
        &self.row.corrected_vin
    }
    pub fn elements(self) -> &'batch [DecodedElement<'db>] {
        &self.shard.elements[self.row.elements.clone()]
    }

    pub fn to_owned(self) -> DecodeResult<'db> {
        DecodeResult {
            vin: self.row.vin.clone(),
            wmi: self.row.wmi.clone(),
            descriptor: self.row.descriptor.clone(),
            model_year: self.row.model_year,
            error_codes: self.row.error_codes.clone(),
            check_digit_valid: self.row.check_digit_valid,
            corrected_vin: self.row.corrected_vin.clone(),
            elements: self.elements().to_vec(),
        }
    }
}

impl serde::Serialize for SlabResultRef<'_, '_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut result = serializer.serialize_struct("DecodeResult", 8)?;
        result.serialize_field("vin", &self.row.vin)?;
        result.serialize_field("wmi", &self.row.wmi)?;
        result.serialize_field("descriptor", &self.row.descriptor)?;
        result.serialize_field("model_year", &self.row.model_year)?;
        result.serialize_field("error_codes", &self.row.error_codes)?;
        result.serialize_field("check_digit_valid", &self.row.check_digit_valid)?;
        result.serialize_field("corrected_vin", &self.row.corrected_vin)?;
        result.serialize_field("elements", &self.elements())?;
        result.end()
    }
}

/// Decode using the embedded database and batch-owned element slabs.
pub fn decode_batch_slab(inputs: &[String], years: Option<&[Option<i32>]>) -> SlabBatch<'static> {
    Db::embedded().decode_batch_slab(inputs, years)
}

/// [`decode_batch_slab`] at an explicit instant.
pub fn decode_batch_slab_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
) -> SlabBatch<'static> {
    Db::embedded().decode_batch_slab_at(inputs, years, now_micros)
}

/// Configurable form of [`decode_batch_slab_at`]. A zero shard size is treated
/// as one row so caller-supplied tuning cannot panic.
pub fn decode_batch_slab_with_options_at(
    inputs: &[String],
    years: Option<&[Option<i32>]>,
    now_micros: i64,
    options: SlabOptions,
) -> SlabBatch<'static> {
    Db::embedded().decode_batch_slab_with_options_at(inputs, years, now_micros, options)
}

impl Db {
    pub fn decode_batch_slab<'db>(
        &'db self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
    ) -> SlabBatch<'db> {
        self.decode_batch_slab_with_options_at(
            inputs,
            years,
            crate::now_micros(),
            SlabOptions::default(),
        )
    }

    pub fn decode_batch_slab_at<'db>(
        &'db self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
    ) -> SlabBatch<'db> {
        self.decode_batch_slab_with_options_at(inputs, years, now_micros, SlabOptions::default())
    }

    pub fn decode_batch_slab_with_options_at<'db>(
        &'db self,
        inputs: &[String],
        years: Option<&[Option<i32>]>,
        now_micros: i64,
        options: SlabOptions,
    ) -> SlabBatch<'db> {
        let rows_per_shard = options.rows_per_shard.max(1);
        let current_year = crate::current_year_at(now_micros);
        crate::install_batch_work(|| {
            let order = crate::locality_order(inputs);
            let shards: Vec<_> = order
                .par_chunks(rows_per_shard)
                .map(|chunk| {
                    // Retain only this bounded shard's native winners long enough
                    // to count public rows. Presence depends solely on element
                    // metadata, so XXX resolution and projection sorting still run
                    // exactly once when each result is consumed below.
                    let raws: Vec<_> = chunk
                        .iter()
                        .map(|&input_index| {
                            let input_index = input_index as usize;
                            let raw = crate::decode_items(
                                self,
                                &inputs[input_index],
                                now_micros,
                                current_year,
                                crate::year_at(years, input_index),
                            );
                            (input_index, raw)
                        })
                        .collect();
                    let element_count = raws
                        .iter()
                        .map(|(_, raw)| {
                            raw.items
                                .iter()
                                .filter(|item| raw.db.output_sort_key(item.element_id).is_some())
                                .count()
                        })
                        .sum();
                    let mut shard = Shard::with_capacity(raws.len(), element_count);
                    let initial_capacity = shard.elements.capacity();
                    for (input_index, raw) in raws {
                        shard.push(input_index, raw);
                    }
                    debug_assert_eq!(shard.elements.len(), element_count);
                    debug_assert_eq!(shard.elements.capacity(), initial_capacity);
                    shard
                })
                .collect();
            let mut locations = vec![(0, 0); inputs.len()];
            for (shard_index, shard) in shards.iter().enumerate() {
                for (row_index, row) in shard.rows.iter().enumerate() {
                    locations[row.input_index] = (shard_index, row_index);
                }
            }
            SlabBatch {
                shards: Some(shards),
                locations,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_owned_conversion_serialization_and_order_match_full_batch() {
        if !Db::embedded().is_loaded() {
            return;
        }
        let edge_inputs = [
            "1M8GDM9AXKP042788".to_string(),
            "1FMAA50A91A111111".to_string(),
            "5UXWX7C5*BA".to_string(),
            "1m8gdm9axkp042788".to_string(),
            "éVIN".to_string(),
            "".to_string(),
        ];
        let inputs: Vec<_> = (0..259)
            .map(|index| edge_inputs[index % edge_inputs.len()].clone())
            .collect();
        // Deliberately shorter than inputs; missing entries mean no caller year.
        let years = [None, Some(2001), Some(2013)];
        let now = 1_767_225_600_000_000;
        let expected = crate::decode_batch_at(&inputs, Some(&years), now);
        for rows_per_shard in [1, 64, 256] {
            let actual = Db::embedded().decode_batch_slab_with_options_at(
                &inputs,
                Some(&years),
                now,
                SlabOptions { rows_per_shard },
            );
            assert_eq!(actual.len(), inputs.len());
            assert_eq!(
                actual
                    .iter()
                    .map(SlabResultRef::to_owned)
                    .collect::<Vec<_>>(),
                expected
            );
            assert_eq!(
                serde_json::to_value(&actual).expect("serialize slab"),
                serde_json::to_value(&expected).expect("serialize owned")
            );
            assert!(actual.allocated_bytes() > 0);

            let converted = Db::embedded()
                .decode_batch_slab_with_options_at(
                    &inputs,
                    Some(&years),
                    now,
                    SlabOptions { rows_per_shard },
                )
                .into_owned();
            assert_eq!(converted, expected);
        }
    }

    #[test]
    fn empty_batch_and_zero_shard_size_are_well_defined() {
        if !Db::embedded().is_loaded() {
            return;
        }
        let batch = Db::embedded().decode_batch_slab_with_options_at(
            &[],
            None,
            0,
            SlabOptions { rows_per_shard: 0 },
        );
        assert!(batch.is_empty());
        assert!(batch.get(0).is_none());
        assert_eq!(
            serde_json::to_string(&batch).expect("serialize empty slab"),
            "[]"
        );
    }

    #[test]
    fn external_database_batch_borrows_only_while_database_is_alive() {
        let bytes = std::fs::read(env!("ULTRAVIN_ARTIFACT")).expect("read built artifact");
        let db = Db::from_bytes(&bytes).expect("load external database");
        if !db.is_loaded() {
            return;
        }
        let inputs = vec!["1M8GDM9AXKP042788".to_string(), "éVIN".to_string()];
        let expected = inputs
            .iter()
            .map(|input| db.decode_at(input, None, 1_767_225_600_000_000))
            .collect::<Vec<_>>();
        let batch = db.decode_batch_slab_at(&inputs, None, 1_767_225_600_000_000);
        assert_eq!(
            batch
                .iter()
                .map(SlabResultRef::to_owned)
                .collect::<Vec<_>>(),
            expected
        );
        assert_eq!(
            serde_json::to_value(&batch).expect("serialize external slab"),
            serde_json::to_value(&expected).expect("serialize external owned")
        );
    }
}
