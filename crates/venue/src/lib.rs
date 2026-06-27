//! qe-venue — venue connectivity and adapters.
//!
//! QE-201 lands the venue-aware REST client: all REST ingress flows through a weighted rate-limit handler
//! ([`ratelimit`]) that backs off rather than dropping under pressure, layered over an ephemeral cache
//! ([`cache`]) for immutable closed-window historical responses and a [`RestTransport`](rest::RestTransport)
//! network seam. Time and backoff go through the [`Clock`](clock::Clock) seam so the retry loop is
//! deterministic in tests.
//!
//! QE-202 adds the wss half: [`stream`] (tiers, channels, subscriptions, gaps), the [`ws`] transport seam
//! ([`WsConnection`](ws::WsConnection)/[`WsConnector`](ws::WsConnector), pull-based for deterministic
//! tests), and the tier-partitioned [`ConnectionRegistry`](registry::ConnectionRegistry) that reconnects,
//! resubscribes, and reports gaps. The concrete async websocket adapter is deferred to the runtime wiring
//! (kept out of the core so the offline build stays dependency-light), mirroring the `http`-gated REST
//! transport.

pub mod cache;
pub mod clock;
pub mod ratelimit;
pub mod registry;
pub mod rest;
pub mod stream;
pub mod ws;

pub use cache::RestCache;
pub use clock::{Clock, SystemClock};
pub use ratelimit::{Acquire, RateLimiter};
pub use registry::{ConnectionRegistry, PumpOutcome};
pub use rest::{RestError, RestResponse, RestTransport, VenueRequest, VenueRestClient};
pub use stream::{Gap, StreamChannel, StreamMessage, StreamTier, Subscription};
pub use ws::{WsConnection, WsConnector, WsError, WsPoll};

#[cfg(feature = "http")]
pub use rest::HttpRestTransport;
