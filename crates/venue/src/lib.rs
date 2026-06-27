//! qe-venue — venue connectivity and adapters.
//!
//! QE-201 lands the venue-aware REST client: all REST ingress flows through a weighted rate-limit handler
//! ([`ratelimit`]) that backs off rather than dropping under pressure, layered over an ephemeral cache
//! ([`cache`]) for immutable closed-window historical responses and a [`RestTransport`](rest::RestTransport)
//! network seam. Time and backoff go through the [`Clock`](clock::Clock) seam so the retry loop is
//! deterministic in tests.

pub mod cache;
pub mod clock;
pub mod ratelimit;
pub mod rest;

pub use cache::RestCache;
pub use clock::{Clock, SystemClock};
pub use ratelimit::{Acquire, RateLimiter};
pub use rest::{RestError, RestResponse, RestTransport, VenueRequest, VenueRestClient};

#[cfg(feature = "http")]
pub use rest::HttpRestTransport;
