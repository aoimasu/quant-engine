//! Stream model for the Market/Realtime wss tiers.
//!
//! A [`Subscription`] names a venue stream (`btcusdt@kline_5m`, `btcusdt@markPrice@1s`); its
//! [`StreamChannel`] carries the expected message cadence used for gap detection. Tiers
//! ([`StreamTier`]) are the partition key for the connection registry.

use qe_domain::{InstrumentId, Resolution};

/// markPrice@1s cadence in milliseconds.
pub const MARK_PRICE_CADENCE_MS: i64 = 1_000;

/// depth20@100ms cadence in milliseconds.
pub const DEPTH20_CADENCE_MS: i64 = 100;

/// The wss tier a stream belongs to — the registry's partition key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StreamTier {
    /// Market data (Hedge-Planner path): kline + markPrice.
    Market,
    /// Realtime tier (Edge gateway path): bookTicker + depth20@100ms + aggTrade.
    Realtime,
}

impl StreamTier {
    /// Every tier, for iterating the registry partitions.
    pub const ALL: [StreamTier; 2] = [StreamTier::Market, StreamTier::Realtime];
}

/// A wss channel — Market-tier (kline / markPrice) or Realtime-tier (bookTicker / depth20 / aggTrade).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamChannel {
    /// Kline/candles at a resolution (5m/30m/4h). Market tier.
    Kline(Resolution),
    /// Mark price at 1-second cadence. Market tier.
    MarkPrice,
    /// Best bid/ask top-of-book updates (`bookTicker`). Realtime tier; event-driven (no fixed cadence).
    BookTicker,
    /// 20-level order-book snapshots at 100 ms (`depth20@100ms`). Realtime tier.
    Depth20,
    /// Aggregated trades (`aggTrade`). Realtime tier; event-driven (no fixed cadence).
    AggTrade,
}

impl StreamChannel {
    /// Expected inter-message spacing (ms) for fixed-cadence channels, or `None` for **event-driven**
    /// channels (`bookTicker`, `aggTrade`) where a time-based gap is undefined. When `Some(ms)`, a spacing
    /// larger than `ms` between consecutive messages is a discontinuity; when `None`, the registry does no
    /// cadence-based gap detection (an outage still surfaces via `PumpOutcome.reconnected`).
    #[must_use]
    pub fn cadence_ms(self) -> Option<i64> {
        match self {
            StreamChannel::Kline(res) => Some(i64::from(res.minutes()) * 60_000),
            StreamChannel::MarkPrice => Some(MARK_PRICE_CADENCE_MS),
            StreamChannel::Depth20 => Some(DEPTH20_CADENCE_MS),
            StreamChannel::BookTicker | StreamChannel::AggTrade => None,
        }
    }

    /// The venue stream suffix (`kline_5m`, `markPrice@1s`, `bookTicker`, `depth20@100ms`, `aggTrade`).
    #[must_use]
    pub fn suffix(self) -> String {
        match self {
            StreamChannel::Kline(res) => format!("kline_{}", res.as_str()),
            StreamChannel::MarkPrice => "markPrice@1s".to_owned(),
            StreamChannel::BookTicker => "bookTicker".to_owned(),
            StreamChannel::Depth20 => "depth20@100ms".to_owned(),
            StreamChannel::AggTrade => "aggTrade".to_owned(),
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

    /// Convenience: a bookTicker (best bid/ask) subscription. Realtime tier.
    #[must_use]
    pub fn book_ticker(instrument: InstrumentId) -> Self {
        Self::new(instrument, StreamChannel::BookTicker)
    }

    /// Convenience: a depth20@100ms order-book subscription. Realtime tier.
    #[must_use]
    pub fn depth20(instrument: InstrumentId) -> Self {
        Self::new(instrument, StreamChannel::Depth20)
    }

    /// Convenience: an aggTrade subscription. Realtime tier.
    #[must_use]
    pub fn agg_trade(instrument: InstrumentId) -> Self {
        Self::new(instrument, StreamChannel::AggTrade)
    }

    /// The tier this subscription belongs to. Kline + markPrice are Market-tier (the Hedge-Planner data
    /// path); bookTicker + depth20 + aggTrade are Realtime-tier (the Edge gateway path).
    #[must_use]
    pub fn tier(&self) -> StreamTier {
        match self.channel {
            StreamChannel::Kline(_) | StreamChannel::MarkPrice => StreamTier::Market,
            StreamChannel::BookTicker | StreamChannel::Depth20 | StreamChannel::AggTrade => {
                StreamTier::Realtime
            }
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
        assert_eq!(
            StreamChannel::Kline(Resolution::M5).cadence_ms(),
            Some(300_000)
        );
        assert_eq!(
            StreamChannel::Kline(Resolution::H4).cadence_ms(),
            Some(14_400_000)
        );
        assert_eq!(StreamChannel::MarkPrice.cadence_ms(), Some(1_000));

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

    // --- QE-203: Realtime-tier streams ---

    #[test]
    fn realtime_channels_have_venue_correct_stream_names() {
        assert_eq!(
            Subscription::book_ticker(inst()).stream_name(),
            "btcusdt@bookTicker"
        );
        assert_eq!(
            Subscription::depth20(inst()).stream_name(),
            "btcusdt@depth20@100ms"
        );
        assert_eq!(
            Subscription::agg_trade(inst()).stream_name(),
            "btcusdt@aggTrade"
        );
    }

    #[test]
    fn realtime_channels_are_realtime_tier() {
        assert_eq!(
            Subscription::book_ticker(inst()).tier(),
            StreamTier::Realtime
        );
        assert_eq!(Subscription::depth20(inst()).tier(), StreamTier::Realtime);
        assert_eq!(Subscription::agg_trade(inst()).tier(), StreamTier::Realtime);
        // The Market-tier channels must not be mis-routed by the new arm.
        assert_eq!(
            Subscription::kline(inst(), Resolution::M5).tier(),
            StreamTier::Market
        );
        assert_eq!(Subscription::mark_price(inst()).tier(), StreamTier::Market);
    }

    #[test]
    fn event_driven_channels_have_no_cadence_but_depth_does() {
        // Event-driven: no time-defined hole.
        assert_eq!(StreamChannel::BookTicker.cadence_ms(), None);
        assert_eq!(StreamChannel::AggTrade.cadence_ms(), None);
        // depth20 is genuinely 100 ms.
        assert_eq!(
            StreamChannel::Depth20.cadence_ms(),
            Some(DEPTH20_CADENCE_MS)
        );
        assert_eq!(StreamChannel::Depth20.cadence_ms(), Some(100));
    }
}
