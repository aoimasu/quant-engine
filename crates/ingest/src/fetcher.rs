//! The byte-transport port. The downloader's logic is written against [`Fetcher`] so it is fully
//! testable offline; the real network client ([`HttpFetcher`]) lives behind the `http` feature.

#[cfg(feature = "http")]
use std::io::Read;

use thiserror::Error;

/// A transport-level fetch failure.
#[derive(Debug, Error)]
pub enum FetchError {
    /// The resource does not exist (HTTP 404). Distinguished so the planner can treat a missing
    /// period as "no data" rather than a hard error.
    #[error("not found: {0}")]
    NotFound(String),

    /// Any other transport failure (DNS, TLS, timeout, non-404 status).
    #[error("transport error fetching {url}: {message}")]
    Transport {
        /// The URL that failed.
        url: String,
        /// A human-readable cause.
        message: String,
    },
}

/// Fetches the bytes at a URL. The single seam between the downloader and the network.
pub trait Fetcher {
    /// GET `url`, returning the response body.
    ///
    /// # Errors
    /// [`FetchError::NotFound`] for a 404; [`FetchError::Transport`] otherwise.
    fn get(&self, url: &str) -> Result<Vec<u8>, FetchError>;
}

/// A blocking HTTP fetcher over `ureq` (system TLS). Compiled only under the `http` feature.
#[cfg(feature = "http")]
pub struct HttpFetcher {
    agent: ureq::Agent,
}

#[cfg(feature = "http")]
impl HttpFetcher {
    /// A fetcher with sensible default timeouts.
    #[must_use]
    pub fn new() -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(60))
            .build();
        Self { agent }
    }
}

#[cfg(feature = "http")]
impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "http")]
impl Fetcher for HttpFetcher {
    fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
        match self.agent.get(url).call() {
            Ok(resp) => {
                let mut buf = Vec::new();
                resp.into_reader()
                    .read_to_end(&mut buf)
                    .map_err(|e| FetchError::Transport {
                        url: url.to_owned(),
                        message: e.to_string(),
                    })?;
                Ok(buf)
            }
            Err(ureq::Error::Status(404, _)) => Err(FetchError::NotFound(url.to_owned())),
            Err(e) => Err(FetchError::Transport {
                url: url.to_owned(),
                message: e.to_string(),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// An in-memory fetcher for tests: serves a fixed url→bytes map and counts hits.
    pub(crate) struct FakeFetcher {
        pub responses: HashMap<String, Vec<u8>>,
        pub hits: RefCell<HashMap<String, usize>>,
    }

    impl FakeFetcher {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                hits: RefCell::new(HashMap::new()),
            }
        }
    }

    impl Fetcher for FakeFetcher {
        fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
            *self.hits.borrow_mut().entry(url.to_owned()).or_insert(0) += 1;
            self.responses
                .get(url)
                .cloned()
                .ok_or_else(|| FetchError::NotFound(url.to_owned()))
        }
    }

    #[test]
    fn fake_serves_and_counts_and_404s() {
        let mut f = FakeFetcher::new();
        f.responses.insert("u".to_owned(), b"x".to_vec());
        assert_eq!(f.get("u").unwrap(), b"x");
        assert_eq!(f.get("u").unwrap(), b"x");
        assert_eq!(f.hits.borrow()["u"], 2);
        assert!(matches!(f.get("missing"), Err(FetchError::NotFound(_))));
    }
}
