//! Recoverability classification for venue connectivity errors (QE-421).
//!
//! Adopts the shared [`qe_error`] taxonomy so the runtime's live order loop can disposition **any**
//! venue-path error uniformly (halt-vs-retry-vs-skip) instead of reading each enum's prose. The mappings
//! mirror each type's documented recovery semantics: rate-limit / transient network / (re)connect failures
//! are **retryable**; a non-retryable REST failure and an exhausted retry budget are **fatal** (halt).

use qe_error::{Classified, ErrorClass};

use crate::rest::RestError;
use crate::userdata::UserDataError;
use crate::ws::WsError;

impl Classified for RestError {
    fn class(&self) -> ErrorClass {
        match self {
            // Back off and retry — the venue told us to wait, or the failure is transient.
            RestError::RateLimited { .. } | RestError::Transient(_) => ErrorClass::Transient,
            // Non-retryable (4xx/parse) or the retry budget is spent: the request cannot complete.
            RestError::Fatal(_) | RestError::Exhausted { .. } => ErrorClass::Fatal,
        }
    }
}

impl Classified for WsError {
    fn class(&self) -> ErrorClass {
        // Every websocket failure is a connectivity fault the registry recovers from by reconnecting +
        // resubscribing — transient, never fatal.
        match self {
            WsError::Connect(_) | WsError::Subscribe(_) | WsError::Closed => ErrorClass::Transient,
        }
    }
}

impl Classified for UserDataError {
    fn class(&self) -> ErrorClass {
        // The user-data session renews the listen key and re-snapshots position truth on reconnect, so a
        // key/connect/snapshot failure is recoverable by re-establishing the stream — transient.
        match self {
            UserDataError::ListenKey(_)
            | UserDataError::Connect(_)
            | UserDataError::Snapshot(_) => ErrorClass::Transient,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qe_error::Disposition;

    /// Compile-time proof every venue connectivity error type is `Classified`.
    fn _assert_classified<T: Classified>() {}
    fn _venue_error_types_are_classified() {
        _assert_classified::<RestError>();
        _assert_classified::<WsError>();
        _assert_classified::<UserDataError>();
    }

    /// Exhaustive: every `RestError` variant maps to the expected disposition.
    #[test]
    fn rest_error_dispositions() {
        assert_eq!(
            RestError::RateLimited { retry_after_ms: 10 }.disposition(),
            Disposition::Retry
        );
        assert_eq!(
            RestError::Transient("5xx".into()).disposition(),
            Disposition::Retry
        );
        assert_eq!(
            RestError::Fatal("400".into()).disposition(),
            Disposition::Halt
        );
        assert_eq!(
            RestError::Exhausted {
                attempts: 5,
                last: "timeout".into()
            }
            .disposition(),
            Disposition::Halt
        );
    }

    /// Exhaustive: every `WsError` variant retries (connectivity is recoverable).
    #[test]
    fn ws_error_dispositions() {
        for e in [
            WsError::Connect("x".into()),
            WsError::Subscribe("x".into()),
            WsError::Closed,
        ] {
            assert_eq!(e.disposition(), Disposition::Retry);
        }
    }

    /// Exhaustive: every `UserDataError` variant retries (re-establish the stream).
    #[test]
    fn userdata_error_dispositions() {
        for e in [
            UserDataError::ListenKey("x".into()),
            UserDataError::Connect("x".into()),
            UserDataError::Snapshot("x".into()),
        ] {
            assert_eq!(e.disposition(), Disposition::Retry);
        }
    }
}
