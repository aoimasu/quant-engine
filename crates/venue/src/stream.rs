//! Stream model for the Market/Realtime wss tiers.
//!
//! A [`Subscription`] names a venue stream (`btcusdt@kline_5m`, `btcusdt@markPrice@1s`); its
//! [`StreamChannel`] carries the expected message cadence used for gap detection. Tiers
//! ([`StreamTier`]) are the partition key for the connection registry.

use qe_domain::{InstrumentId, Resolution};

/// markPrice@1s cadence in milliseconds.
pub const MARK_PRICE_CADENCE_MS: i64 = 1_000;

/// The wss tier a stream belongs to — the registry's partition key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamTier {
    /// Market data: kline + markPrice (this ticket).
    Market,
    /// Realtime/private tier (QE-203) — partitioned here, populated later.
    Realtime,
}

impl StreamTier {
    /// Every tier, for iterating the registry partitions.
    pub const ALL: [StreamTier; 2] = [StreamTier::Market, StreamTier::Realtime];
}

/// A Market-tier channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamChannel {
    /// Kline/candles at a resolution (5m/30m/4h on the Market tier).
    Kline(Resolution),
    /// Mark price at 1-second cadence.
    MarkPrice,
}

impl StreamChannel {
    /// Expected inter-message spacing (ms): the resolution length for kline, 1s for markPrice. A larger
    /// gap than this between consecutive messages is a discontinuity.
    #[must_use]
    pub fn cadence_ms(self) -> i64 {
        match self {
            StreamChannel::Kline(res) => i64::from(res.minutes()) * 60_000,
            StreamChannel::MarkPrice => MARK_PRICE_CADENCE_MS,
        }
    }

    /// The venue stream suffix (`kline_5m`, `markPrice@1s`).
    #[must_use]
    pub fn suffix(self) -> String {
        match self {
            StreamChannel::Kline(res) => format!("kline_{}", res.as_str()),
            StreamChannel::MarkPrice => "markPrice@1s".to_owned(),
        }
    }
}

/// A subscription to one venue stream.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Subscription {
    /// The instrument.
    pub instrument: InstrumentId,
    /// The channel.
    pub channel: StreamChannel,
}

impl Subscription {
    /// A subscription to `channel` for `instrument`.
    #[must_use]
    pub fn new(instrument: InstrumentId, channel: StreamChannel) -> Self {
        Self {
            instrument,
            channel,
        }
    }

    /// Convenience: a kline subscription.
    #[must_use]
    pub fn kline(instrument: InstrumentId, resolution: Resolution) -> Self {
        Self::new(instrument, StreamChannel::Kline(resolution))
    }

    /// Convenience: a markPrice@1s subscription.
    #[must_use]
    pub fn mark_price(instrument: InstrumentId) -> Self {
        Self::new(instrument, StreamChannel::MarkPrice)
    }

    /// The tier this subscription belongs to. Kline + markPrice are Market-tier.
    #[must_use]
    pub fn tier(&self) -> StreamTier {
        match self.channel {
            StreamChannel::Kline(_) | StreamChannel::MarkPrice => StreamTier::Market,
        }
    }

    /// The venue stream name (lower-cased symbol + channel suffix), e.g. `btcusdt@kline_5m`.
    #[must_use]
    pub fn stream_name(&self) -> String {
        format!(
            "{}@{}",
            self.instrument.as_str().to_lowercase(),
            self.channel.suffix()
        )
    }
}

/// One decoded stream update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamMessage {
    /// The subscription it belongs to.
    pub subscription: Subscription,
    /// The venue event time (epoch ms).
    pub event_time_ms: i64,
    /// The raw payload (decoded downstream).
    pub payload: String,
}

/// A detected discontinuity in a stream — `to_ms − from_ms` of data was missed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gap {
    /// The stream name the gap is on.
    pub stream: String,
    /// Last event time seen before the hole.
    pub from_ms: i64,
    /// First event time seen after the hole.
    pub to_ms: i64,
}

impl Gap {
    /// The missed span in milliseconds.
    #[must_use]
    pub fn missed_ms(&self) -> i64 {
        self.to_ms - self.from_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst() -> InstrumentId {
        InstrumentId::new("BTCUSDT").unwrap()
    }

    #[test]
    fn cadence_and_stream_names_match_the_venue_forms() {
        assert_eq!(StreamChannel::Kline(Resolution::M5).cadence_ms(), 300_000);
        assert_eq!(
            StreamChannel::Kline(Resolution::H4).cadence_ms(),
            14_400_000
        );
        assert_eq!(StreamChannel::MarkPrice.cadence_ms(), 1_000);

        assert_eq!(
            Subscription::kline(inst(), Resolution::M5).stream_name(),
            "btcusdt@kline_5m"
        );
        assert_eq!(
            Subscription::mark_price(inst()).stream_name(),
            "btcusdt@markPrice@1s"
        );
    }

    #[test]
    fn kline_and_mark_price_are_market_tier() {
        assert_eq!(
            Subscription::kline(inst(), Resolution::M30).tier(),
            StreamTier::Market
        );
        assert_eq!(Subscription::mark_price(inst()).tier(), StreamTier::Market);
    }
}
