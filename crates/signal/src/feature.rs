//! Feature-vector assembly (QE-108): per-bar vectors of quantised indicator states, the rows WFO/DE
//! consume.
//!
//! Built on the QE-107 catalogue, so it inherits batch/streaming parity: [`FeatureAssembler::push`]
//! is the single path and [`assemble_batch`] is just the push loop — batch == streaming by
//! construction (AC).

use crate::indicator::{catalogue, CatalogueConfig, Indicator, QState, CATALOGUE_VERSION};
use crate::Sample;

/// Sentinel `u16` meaning "no state" (indicator not yet warm) in the byte codec.
const NONE_SENTINEL: u16 = u16::MAX;

/// The ordered contract a [`FeatureVector`] is interpreted against: indicator ids + lookbacks in
/// catalogue order, the catalogue version, and the per-indicator state count. The version + state
/// count are embedded in each encoded vector's header so a decode against a mismatching schema fails
/// loudly rather than silently mis-decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSchema {
    ids: Vec<String>,
    lookbacks: Vec<usize>,
    version: u32,
    num_states: u16,
}

impl FeatureSchema {
    fn from_specs(specs: impl IntoIterator<Item = crate::indicator::IndicatorSpec>) -> Self {
        let mut ids = Vec::new();
        let mut lookbacks = Vec::new();
        let mut num_states = 0u16;
        for spec in specs {
            num_states = spec.num_states; // uniform across the catalogue
            ids.push(spec.id);
            lookbacks.push(spec.lookback);
        }
        FeatureSchema {
            ids,
            lookbacks,
            version: CATALOGUE_VERSION,
            num_states,
        }
    }

    /// Derive the schema from the catalogue built with `cfg`.
    #[must_use]
    pub fn from_catalogue(cfg: &CatalogueConfig) -> Self {
        FeatureSchema::from_specs(catalogue(cfg).iter().map(|i| i.spec()))
    }

    /// Number of features (== catalogue size).
    #[must_use]
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Whether the schema is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The ordered indicator ids.
    #[must_use]
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// The ordered per-indicator lookbacks (parallel to [`ids`](Self::ids)). The genotype-derived
    /// "timescale" behaviour descriptor (QE-111) reads these for a genome's referenced features.
    #[must_use]
    pub fn lookbacks(&self) -> &[usize] {
        &self.lookbacks
    }

    /// The catalogue version this schema was built from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The per-indicator state count (uniform across the catalogue).
    #[must_use]
    pub fn num_states(&self) -> u16 {
        self.num_states
    }

    /// The maximum indicator lookback — the warmup before a vector can be complete.
    #[must_use]
    pub fn max_lookback(&self) -> usize {
        self.lookbacks.iter().copied().max().unwrap_or(0)
    }
}

/// One bar's feature vector: the quantised state of every catalogue indicator (in schema order),
/// `None` for any indicator not yet warm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureVector {
    /// The bar's open time (epoch-ms).
    pub time_ms: i64,
    /// One slot per indicator, in schema order.
    pub states: Vec<Option<QState>>,
}

impl FeatureVector {
    /// Whether every indicator has produced a state (the row WFO/DE consumes).
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.states.iter().all(Option::is_some)
    }

    /// Encode to **self-describing**, deterministic bytes interpreted against `schema`:
    ///
    /// `[version u32 BE][num_states u16 BE][width u16 BE][time i64 BE]` then one `u16` (BE) per slot,
    /// with `0xFFFF` meaning `None`. The header lets [`from_bytes`] reject a vector encoded under a
    /// different catalogue version / state count, instead of silently mis-decoding (a same-lineage
    /// catalogue change otherwise reads as garbage).
    #[must_use]
    pub fn to_bytes(&self, schema: &FeatureSchema) -> Vec<u8> {
        let width = self.states.len() as u16;
        let mut out = Vec::with_capacity(16 + self.states.len() * 2);
        out.extend_from_slice(&schema.version().to_be_bytes());
        out.extend_from_slice(&schema.num_states().to_be_bytes());
        out.extend_from_slice(&width.to_be_bytes());
        out.extend_from_slice(&self.time_ms.to_be_bytes());
        for slot in &self.states {
            let code = slot.map_or(NONE_SENTINEL, QState::index);
            out.extend_from_slice(&code.to_be_bytes());
        }
        out
    }

    /// Decode bytes produced by [`to_bytes`], validating the header against `schema`.
    ///
    /// Returns `None` if the bytes are malformed, truncated, or the embedded version / state-count /
    /// width does not match `schema` — so a catalogue change can never be silently mis-read.
    #[must_use]
    pub fn from_bytes(bytes: &[u8], schema: &FeatureSchema) -> Option<FeatureVector> {
        let width = schema.len();
        if bytes.len() != 16 + width * 2 {
            return None;
        }
        let version = u32::from_be_bytes(bytes[0..4].try_into().ok()?);
        let num_states = u16::from_be_bytes(bytes[4..6].try_into().ok()?);
        let stored_width = u16::from_be_bytes(bytes[6..8].try_into().ok()?);
        if version != schema.version()
            || num_states != schema.num_states()
            || usize::from(stored_width) != width
        {
            return None;
        }
        let time_ms = i64::from_be_bytes(bytes[8..16].try_into().ok()?);
        let mut states = Vec::with_capacity(width);
        for i in 0..width {
            let off = 16 + i * 2;
            let code = u16::from_be_bytes(bytes[off..off + 2].try_into().ok()?);
            states.push(if code == NONE_SENTINEL {
                None
            } else {
                Some(QState::from_index(code))
            });
        }
        Some(FeatureVector { time_ms, states })
    }
}

/// Assembles per-bar feature vectors by driving the whole catalogue with one shared `update` path.
pub struct FeatureAssembler {
    catalogue: Vec<Box<dyn Indicator>>,
}

impl FeatureAssembler {
    /// Build an assembler over the catalogue configured by `cfg`.
    #[must_use]
    pub fn new(cfg: &CatalogueConfig) -> Self {
        FeatureAssembler {
            catalogue: catalogue(cfg),
        }
    }

    /// The schema this assembler produces vectors against.
    #[must_use]
    pub fn schema(&self) -> FeatureSchema {
        FeatureSchema::from_specs(self.catalogue.iter().map(|i| i.spec()))
    }

    /// Feed one sample (in time order) and assemble its feature vector.
    pub fn push(&mut self, sample: &Sample) -> FeatureVector {
        let states = self
            .catalogue
            .iter_mut()
            .map(|ind| ind.update(sample))
            .collect();
        FeatureVector {
            time_ms: sample.bar.open_time().millis(),
            states,
        }
    }

    /// Reset every indicator to its pre-warmup state.
    pub fn reset(&mut self) {
        for ind in &mut self.catalogue {
            ind.reset();
        }
    }
}

/// Assemble a feature vector for every sample (batch) — the `push` loop, identical to streaming.
#[must_use]
pub fn assemble_batch(cfg: &CatalogueConfig, samples: &[Sample]) -> Vec<FeatureVector> {
    let mut asm = FeatureAssembler::new(cfg);
    samples.iter().map(|s| asm.push(s)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_domain::{Bar, Price, Qty, Resolution, Timestamp};
    use rust_decimal::Decimal;

    const MIN: i64 = 60_000;

    fn series(n: usize) -> Vec<Sample> {
        (0..n)
            .map(|i| {
                let i = i as i64;
                let base = 100 + (i % 7) * 3 + i / 5;
                let bar = Bar::new(
                    Timestamp::from_millis(i * 5 * MIN),
                    Resolution::M5,
                    Price::new(Decimal::from(base)).unwrap(),
                    Price::new(Decimal::from(base + 6)).unwrap(),
                    Price::new(Decimal::from(base - 6)).unwrap(),
                    Price::new(Decimal::from(base + (i % 3) - 1)).unwrap(),
                    Qty::new(Decimal::from(10 + (i % 5))).unwrap(),
                    1,
                )
                .unwrap();
                Sample {
                    bar,
                    funding: Some(Decimal::new((i % 5) - 2, 4)),
                    open_interest: Some(Decimal::from(1000 + i * 7)),
                    premium: Some(Decimal::new((i % 3) - 1, 4)),
                }
            })
            .collect()
    }

    #[test]
    fn schema_matches_catalogue() {
        let cfg = CatalogueConfig::default();
        let schema = FeatureSchema::from_catalogue(&cfg);
        assert_eq!(schema.len(), catalogue(&cfg).len());
        assert!(schema.len() >= 20);
        assert_eq!(schema.version(), CATALOGUE_VERSION);
        assert!(schema.ids().contains(&"rsi_14".to_owned()));
    }

    #[test]
    fn ac_batch_equals_streaming() {
        let cfg = CatalogueConfig::default();
        let samples = series(80);

        let batch = assemble_batch(&cfg, &samples);

        let mut asm = FeatureAssembler::new(&cfg);
        let streamed: Vec<FeatureVector> = samples.iter().map(|s| asm.push(s)).collect();

        assert_eq!(batch, streamed);
        // Reproducible across a second run.
        assert_eq!(batch, assemble_batch(&cfg, &samples));
    }

    #[test]
    fn vectors_become_complete_after_max_lookback() {
        let cfg = CatalogueConfig::default();
        let samples = series(80);
        let schema = FeatureSchema::from_catalogue(&cfg);
        let vectors = assemble_batch(&cfg, &samples);

        // Before max_lookback no vector is complete; at/after it, they are.
        assert!(!vectors[schema.max_lookback() - 2].is_complete());
        assert!(vectors[schema.max_lookback() - 1].is_complete());
        assert!(vectors.last().unwrap().is_complete());
    }

    #[test]
    fn byte_codec_round_trips() {
        let real = FeatureSchema::from_catalogue(&CatalogueConfig::default());
        // A 3-wide toy schema sharing the version + state count.
        let toy = FeatureSchema {
            ids: vec!["a".into(), "b".into(), "c".into()],
            lookbacks: vec![1, 1, 1],
            version: real.version(),
            num_states: real.num_states(),
        };
        let v = FeatureVector {
            time_ms: 1_700_000_000_000,
            states: vec![
                Some(QState::from_index(3)),
                None,
                Some(QState::from_index(0)),
            ],
        };
        let bytes = v.to_bytes(&toy);
        assert_eq!(FeatureVector::from_bytes(&bytes, &toy).unwrap(), v);
        // Width mismatch (real catalogue schema) fails.
        assert!(FeatureVector::from_bytes(&bytes, &real).is_none());
    }

    #[test]
    fn decode_rejects_state_count_mismatch() {
        // Same catalogue size + width but a different num_states must reject the blob (no silent
        // mis-decode under an unchanged lineage).
        let cfg = CatalogueConfig::default();
        let schema = FeatureSchema::from_catalogue(&cfg);
        let v = assemble_batch(&cfg, &series(80)).pop().unwrap();
        let bytes = v.to_bytes(&schema);

        let other = FeatureSchema::from_catalogue(&CatalogueConfig { states: 9 });
        assert_eq!(other.len(), schema.len());
        assert_ne!(other.num_states(), schema.num_states());
        assert!(FeatureVector::from_bytes(&bytes, &other).is_none());
    }

    #[test]
    fn complete_vector_round_trips_through_bytes() {
        let cfg = CatalogueConfig::default();
        let schema = FeatureSchema::from_catalogue(&cfg);
        let v = assemble_batch(&cfg, &series(80)).pop().unwrap();
        assert!(v.is_complete());
        assert_eq!(
            FeatureVector::from_bytes(&v.to_bytes(&schema), &schema).unwrap(),
            v
        );
    }
}
