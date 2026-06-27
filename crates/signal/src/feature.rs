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
/// catalogue order, plus the catalogue version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureSchema {
    ids: Vec<String>,
    lookbacks: Vec<usize>,
    version: u32,
}

impl FeatureSchema {
    /// Derive the schema from the catalogue built with `cfg`.
    #[must_use]
    pub fn from_catalogue(cfg: &CatalogueConfig) -> Self {
        let cat = catalogue(cfg);
        let mut ids = Vec::with_capacity(cat.len());
        let mut lookbacks = Vec::with_capacity(cat.len());
        for ind in &cat {
            let spec = ind.spec();
            ids.push(spec.id);
            lookbacks.push(spec.lookback);
        }
        FeatureSchema {
            ids,
            lookbacks,
            version: CATALOGUE_VERSION,
        }
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

    /// The catalogue version this schema was built from.
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
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

    /// Encode to compact, deterministic bytes: `i64` time (BE) then one `u16` (BE) per slot, with
    /// `0xFFFF` meaning `None`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + self.states.len() * 2);
        out.extend_from_slice(&self.time_ms.to_be_bytes());
        for slot in &self.states {
            let code = slot.map_or(NONE_SENTINEL, QState::index);
            out.extend_from_slice(&code.to_be_bytes());
        }
        out
    }

    /// Decode `width` slots from bytes produced by [`to_bytes`]. Returns `None` on a length mismatch
    /// or a state index `>= NONE_SENTINEL` other than the sentinel.
    #[must_use]
    pub fn from_bytes(bytes: &[u8], width: usize) -> Option<FeatureVector> {
        if bytes.len() != 8 + width * 2 {
            return None;
        }
        let time_ms = i64::from_be_bytes(bytes[0..8].try_into().ok()?);
        let mut states = Vec::with_capacity(width);
        for i in 0..width {
            let off = 8 + i * 2;
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
        let mut ids = Vec::with_capacity(self.catalogue.len());
        let mut lookbacks = Vec::with_capacity(self.catalogue.len());
        for ind in &self.catalogue {
            let spec = ind.spec();
            ids.push(spec.id);
            lookbacks.push(spec.lookback);
        }
        FeatureSchema {
            ids,
            lookbacks,
            version: CATALOGUE_VERSION,
        }
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
        let v = FeatureVector {
            time_ms: 1_700_000_000_000,
            states: vec![
                Some(QState::from_index(3)),
                None,
                Some(QState::from_index(0)),
            ],
        };
        let bytes = v.to_bytes();
        let back = FeatureVector::from_bytes(&bytes, 3).unwrap();
        assert_eq!(back, v);
        // Width mismatch fails.
        assert!(FeatureVector::from_bytes(&bytes, 2).is_none());
    }

    #[test]
    fn complete_vector_round_trips_through_bytes() {
        let cfg = CatalogueConfig::default();
        let samples = series(80);
        let width = FeatureSchema::from_catalogue(&cfg).len();
        let v = assemble_batch(&cfg, &samples).pop().unwrap();
        assert!(v.is_complete());
        assert_eq!(FeatureVector::from_bytes(&v.to_bytes(), width).unwrap(), v);
    }
}
