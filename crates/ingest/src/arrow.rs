//! Arrow record-batch + IPC serialisation of the fused corpus (QE-104), behind the `arrow`
//! feature.
//!
//! The fused corpus is a fixed grid of `slots` plus one nullable `Float64` column per
//! [`CanonicalSeries`] (holes → null). [`corpus_to_record_batch`] builds the batch with a fixed
//! schema/column order; [`corpus_to_ipc`] writes it to Arrow IPC stream bytes — **byte-reproducible**
//! for fixed inputs (Arrow IPC embeds no clock/random state), the Arrow counterpart of AC #1.
//!
//! Values are exact `rust_decimal` upstream; Arrow has no 96-bit decimal in this minimal stack, so
//! the serialised column is `Float64` (a transport/interop convenience). Exact money stays in the
//! [`FusedCorpus`] itself — the Arrow batch is the interchange artefact, not the source of truth.

use std::sync::Arc;

use arrow_array::{ArrayRef, Float64Array, Int64Array, RecordBatch};
use arrow_ipc::writer::StreamWriter;
use arrow_schema::{ArrowError, DataType, Field, Schema};
use rust_decimal::prelude::ToPrimitive;

use crate::canonical::CanonicalSeries;
use crate::fuse::FusedCorpus;

/// The Arrow schema of a fused corpus: an `Int64` `slot_ms` column followed by one nullable
/// `Float64` column per canonical series, in [`CanonicalSeries::ALL`] order.
#[must_use]
pub fn corpus_schema() -> Schema {
    let mut fields: Vec<Field> = Vec::with_capacity(1 + CanonicalSeries::ALL.len());
    fields.push(Field::new("slot_ms", DataType::Int64, false));
    for series in CanonicalSeries::ALL {
        fields.push(Field::new(series.as_str(), DataType::Float64, true));
    }
    Schema::new(fields)
}

/// Build a fixed-schema [`RecordBatch`] from the fused corpus (holes → null `Float64`).
///
/// Note: a value whose exact `Decimal` cannot be represented as `f64` (`to_f64` → `None`, e.g. an
/// extreme magnitude) also maps to null, indistinguishable from a hole in the Arrow column. This is
/// an interchange-only concern — the exact value is preserved in [`FusedCorpus`], the source of
/// truth; for real perp/spot prices `to_f64` does not fail.
///
/// # Errors
/// [`ArrowError`] if the column arrays cannot be assembled into a batch (shape mismatch).
pub fn corpus_to_record_batch(corpus: &FusedCorpus) -> Result<RecordBatch, ArrowError> {
    let schema = Arc::new(corpus_schema());
    let mut columns: Vec<ArrayRef> = Vec::with_capacity(1 + CanonicalSeries::ALL.len());
    columns.push(Arc::new(Int64Array::from(corpus.slots.clone())));

    for series in CanonicalSeries::ALL {
        let cells = corpus
            .column(series)
            .map(|c| c.cells.as_slice())
            .unwrap_or(&[]);
        let values: Vec<Option<f64>> = cells
            .iter()
            .map(|cell| cell.value().and_then(|d| d.to_f64()))
            .collect();
        columns.push(Arc::new(Float64Array::from(values)));
    }

    RecordBatch::try_new(schema, columns)
}

/// Serialise the fused corpus to Arrow IPC stream bytes — the byte-reproducible interchange
/// artefact.
///
/// # Errors
/// [`ArrowError`] if batch construction or IPC writing fails.
pub fn corpus_to_ipc(corpus: &FusedCorpus) -> Result<Vec<u8>, ArrowError> {
    let batch = corpus_to_record_batch(corpus)?;
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut writer = StreamWriter::try_new(&mut buf, &batch.schema())?;
        writer.write(&batch)?;
        writer.finish()?;
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, InstrumentId, Price, Qty, Resolution, TimeInterval, Timestamp};
    use rust_decimal::Decimal;

    use crate::derive::Adjustment;
    use crate::fuse::{fuse, FusionInput};

    const MIN: i64 = 60_000;

    fn close_bar(t_ms: i64, close: i64) -> Bar {
        let c = Price::new(Decimal::from(close)).unwrap();
        Bar::new(
            Timestamp::from_millis(t_ms),
            Resolution::M1,
            c,
            c,
            c,
            c,
            Qty::new(Decimal::ONE).unwrap(),
            1,
        )
        .unwrap()
    }

    fn sample_corpus() -> FusedCorpus {
        let input = FusionInput {
            instrument: InstrumentId::new("BTCUSDT").unwrap(),
            window: TimeInterval::new(Timestamp::from_millis(0), Timestamp::from_millis(3 * MIN))
                .unwrap(),
            resolution: Resolution::M1,
            max_gap_ms: 0,
            adjustment: Adjustment::IDENTITY,
            perp_partitions: vec![vec![close_bar(0, 101), close_bar(2 * MIN, 103)]],
            spot_partitions: vec![vec![close_bar(0, 100)]],
            funding: vec![(0, Decimal::from(1))],
            premium_index: vec![],
            futures_metrics: vec![],
        };
        fuse(&input).unwrap()
    }

    #[test]
    fn schema_has_slot_plus_one_column_per_series() {
        let schema = corpus_schema();
        assert_eq!(schema.fields().len(), 1 + CanonicalSeries::ALL.len());
        assert_eq!(schema.field(0).name(), "slot_ms");
        assert_eq!(schema.field(1).name(), CanonicalSeries::ALL[0].as_str());
    }

    #[test]
    fn record_batch_shape_matches_grid() {
        let corpus = sample_corpus();
        let batch = corpus_to_record_batch(&corpus).unwrap();
        assert_eq!(batch.num_rows(), corpus.slots.len());
        assert_eq!(batch.num_columns(), 1 + CanonicalSeries::ALL.len());
    }

    #[test]
    fn ipc_bytes_are_byte_reproducible() {
        let corpus = sample_corpus();
        let a = corpus_to_ipc(&corpus).unwrap();
        let b = corpus_to_ipc(&corpus).unwrap();
        assert_eq!(
            a, b,
            "Arrow IPC bytes must be reproducible for fixed inputs"
        );
        assert!(!a.is_empty());
    }
}
