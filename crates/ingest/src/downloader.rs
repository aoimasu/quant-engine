//! The download orchestration: idempotent, resumable, checksum-verified fetching of dump files.

use crate::cache::RawCache;
use crate::checksum::{parse_checksum_file, sha256_hex};
use crate::fetcher::{FetchError, Fetcher};
use crate::source::DumpFile;
use crate::IngestError;

/// What happened to one file during a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOutcome {
    /// Already present and digest-verified — nothing fetched (idempotent / resumed).
    Skipped,
    /// Fetched and verified on the first attempt.
    Fetched,
    /// First transfer was corrupt; re-fetched and verified.
    Refetched,
    /// The period has no published dump (HTTP 404) — not an error. Binance does not publish every
    /// period for every instrument (e.g. the not-yet-closed current month).
    Missing,
}

/// Aggregate result of syncing a target list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    /// Files already present + verified.
    pub skipped: usize,
    /// Files fetched cleanly.
    pub fetched: usize,
    /// Files that needed a re-fetch after a corrupt first transfer.
    pub refetched: usize,
    /// Periods with no published dump (404) — absent, not failed.
    pub missing: usize,
    /// Files that could not be obtained (relative_path, reason).
    pub failed: Vec<(String, String)>,
}

impl SyncReport {
    fn record(&mut self, outcome: FileOutcome) {
        match outcome {
            FileOutcome::Skipped => self.skipped += 1,
            FileOutcome::Fetched => self.fetched += 1,
            FileOutcome::Refetched => self.refetched += 1,
            FileOutcome::Missing => self.missing += 1,
        }
    }
}

/// Downloads dump files through a [`Fetcher`] into a [`RawCache`], verifying every transfer.
pub struct Downloader<F: Fetcher> {
    fetcher: F,
    cache: RawCache,
    base_url: String,
}

impl<F: Fetcher> Downloader<F> {
    /// A downloader over `fetcher`, caching under `cache`, fetching from `base_url` (no trailing
    /// slash, e.g. [`crate::source::DEFAULT_BASE_URL`]).
    pub fn new(fetcher: F, cache: RawCache, base_url: impl Into<String>) -> Self {
        Self {
            fetcher,
            cache,
            base_url: base_url.into(),
        }
    }

    /// Ensure one file is present and verified, fetching only if needed.
    ///
    /// Idempotent: a file already present with a matching digest is [`FileOutcome::Skipped`]
    /// (AC #1). A corrupt transfer (digest mismatch) is rejected and re-fetched once
    /// ([`FileOutcome::Refetched`]); a persistently bad file errors with
    /// [`IngestError::ChecksumMismatch`] and is **not** cached (AC #2).
    ///
    /// # Errors
    /// [`IngestError`] on a transport failure, a missing checksum sidecar, an unparseable checksum,
    /// a persistent digest mismatch, or a cache IO error.
    pub fn sync_file(&self, file: &DumpFile) -> Result<FileOutcome, IngestError> {
        if self.cache.is_verified(file)? {
            return Ok(FileOutcome::Skipped);
        }

        // Fetch the expected digest once (it is tiny and authoritative). A 404 here means the period
        // is simply not published → `Missing`, not a failure.
        let Some(checksum_bytes) = self.fetch_opt(&file.checksum_url(&self.base_url))? else {
            return Ok(FileOutcome::Missing);
        };
        let checksum_text = String::from_utf8_lossy(&checksum_bytes);
        let expected = parse_checksum_file(&checksum_text)
            .ok_or_else(|| IngestError::ChecksumUnparseable(file.checksum_relative_path()))?;

        // Try, then retry once on a corrupt transfer.
        for attempt in 0..2 {
            let Some(bytes) = self.fetch_opt(&file.url(&self.base_url))? else {
                return Ok(FileOutcome::Missing);
            };
            if sha256_hex(&bytes) == expected {
                self.cache.store(file, &bytes, &expected)?;
                return Ok(if attempt == 0 {
                    FileOutcome::Fetched
                } else {
                    FileOutcome::Refetched
                });
            }
            // else: corrupt — discard (never cached) and retry.
        }
        Err(IngestError::ChecksumMismatch(file.relative_path()))
    }

    /// Sync a target list, accumulating a [`SyncReport`]. Resumable: each file independently checks
    /// the cache, so a re-run after an interruption skips everything already done. A per-file failure
    /// is recorded and the run continues.
    pub fn sync_all(&self, files: &[DumpFile]) -> SyncReport {
        let mut report = SyncReport::default();
        for file in files {
            match self.sync_file(file) {
                Ok(outcome) => report.record(outcome),
                Err(e) => report.failed.push((file.relative_path(), e.to_string())),
            }
        }
        report
    }

    /// Fetch `url`, mapping a 404 to `Ok(None)` (absent, not an error) and any other transport
    /// failure to [`IngestError::Transport`].
    fn fetch_opt(&self, url: &str) -> Result<Option<Vec<u8>>, IngestError> {
        match self.fetcher.get(url) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(FetchError::NotFound(_)) => Ok(None),
            Err(FetchError::Transport { url, message }) => {
                Err(IngestError::Transport { url, message })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{DataKind, Date, Period};
    use qe_domain::{InstrumentId, Resolution};
    use std::cell::RefCell;
    use std::collections::HashMap;

    /// A fake fetcher with a url→bytes map. Optionally returns corrupt bytes for the *first* N hits
    /// of a given url, to exercise the corrupt-then-refetch path. Counts hits per url.
    struct ScriptedFetcher {
        responses: HashMap<String, Vec<u8>>,
        corrupt_first: RefCell<HashMap<String, usize>>,
        hits: RefCell<HashMap<String, usize>>,
    }

    impl ScriptedFetcher {
        fn new() -> Self {
            Self {
                responses: HashMap::new(),
                corrupt_first: RefCell::new(HashMap::new()),
                hits: RefCell::new(HashMap::new()),
            }
        }
        fn put(&mut self, url: String, bytes: Vec<u8>) {
            self.responses.insert(url, bytes);
        }
        fn hits(&self, url: &str) -> usize {
            self.hits.borrow().get(url).copied().unwrap_or(0)
        }
    }

    impl Fetcher for ScriptedFetcher {
        fn get(&self, url: &str) -> Result<Vec<u8>, FetchError> {
            *self.hits.borrow_mut().entry(url.to_owned()).or_insert(0) += 1;
            let bytes = self
                .responses
                .get(url)
                .cloned()
                .ok_or_else(|| FetchError::NotFound(url.to_owned()))?;
            // If this url is scripted to be corrupt for its first N hits, flip a byte.
            let mut cf = self.corrupt_first.borrow_mut();
            if let Some(remaining) = cf.get_mut(url) {
                if *remaining > 0 {
                    *remaining -= 1;
                    let mut bad = bytes.clone();
                    bad.push(0xFF); // wrong bytes → wrong digest
                    return Ok(bad);
                }
            }
            Ok(bytes)
        }
    }

    const BASE: &str = "https://data.binance.vision";

    fn file() -> DumpFile {
        DumpFile::new(
            InstrumentId::new("BTCUSDT").unwrap(),
            DataKind::Klines(Resolution::M5),
            Period::Daily(Date {
                year: 2020,
                month: 1,
                day: 7,
            }),
        )
    }

    /// Wire a fetcher to serve `bytes` (and its correct checksum sidecar) for `file`.
    fn serve(fetcher: &mut ScriptedFetcher, f: &DumpFile, bytes: &[u8]) {
        let digest = sha256_hex(bytes);
        fetcher.put(f.url(BASE), bytes.to_vec());
        fetcher.put(
            f.checksum_url(BASE),
            format!("{digest}  file.zip").into_bytes(),
        );
    }

    #[test]
    fn fetches_then_reruns_skip_everything() {
        // AC #1: a clean fetch, then a re-run fetches nothing already present + verified.
        let tmp = tempfile::tempdir().unwrap();
        let f = file();
        let mut fetcher = ScriptedFetcher::new();
        serve(&mut fetcher, &f, b"the-zip-bytes");

        let dl = Downloader::new(fetcher, RawCache::new(tmp.path()), BASE);

        let r1 = dl.sync_all(std::slice::from_ref(&f));
        assert_eq!((r1.fetched, r1.skipped), (1, 0));
        assert!(r1.failed.is_empty());
        let hits_after_first = dl.fetcher.hits(&f.url(BASE));

        // Re-run: skipped, and the file url is not hit again.
        let r2 = dl.sync_all(std::slice::from_ref(&f));
        assert_eq!((r2.fetched, r2.skipped), (0, 1));
        assert_eq!(
            dl.fetcher.hits(&f.url(BASE)),
            hits_after_first,
            "a verified file must not be re-fetched"
        );
    }

    #[test]
    fn corrupt_transfer_is_rejected_and_refetched() {
        // AC #2: the first transfer is corrupt → rejected, then re-fetched to success.
        let tmp = tempfile::tempdir().unwrap();
        let f = file();
        let mut fetcher = ScriptedFetcher::new();
        serve(&mut fetcher, &f, b"good-bytes");
        fetcher.corrupt_first.borrow_mut().insert(f.url(BASE), 1); // corrupt once

        let dl = Downloader::new(fetcher, RawCache::new(tmp.path()), BASE);
        let outcome = dl.sync_file(&f).expect("should recover via re-fetch");
        assert_eq!(outcome, FileOutcome::Refetched);
        assert_eq!(dl.fetcher.hits(&f.url(BASE)), 2, "one corrupt + one good");
        // The recovered file is now cached and verified.
        assert!(dl.cache.is_verified(&f).unwrap());
    }

    #[test]
    fn missing_period_is_not_a_failure() {
        // A 404 (no dump published for this period) → Missing, not an error, and nothing cached.
        let tmp = tempfile::tempdir().unwrap();
        let f = file();
        let fetcher = ScriptedFetcher::new(); // serves nothing → every url 404s
        let dl = Downloader::new(fetcher, RawCache::new(tmp.path()), BASE);

        assert_eq!(dl.sync_file(&f).unwrap(), FileOutcome::Missing);
        let report = dl.sync_all(std::slice::from_ref(&f));
        assert_eq!(report.missing, 1);
        assert!(report.failed.is_empty());
        assert!(!dl.cache.is_verified(&f).unwrap());
    }

    #[test]
    fn persistently_corrupt_file_errors_and_is_not_cached() {
        let tmp = tempfile::tempdir().unwrap();
        let f = file();
        let mut fetcher = ScriptedFetcher::new();
        serve(&mut fetcher, &f, b"good-bytes");
        fetcher.corrupt_first.borrow_mut().insert(f.url(BASE), 9); // always corrupt

        let dl = Downloader::new(fetcher, RawCache::new(tmp.path()), BASE);
        let err = dl.sync_file(&f).unwrap_err();
        assert!(matches!(err, IngestError::ChecksumMismatch(_)));
        assert!(
            !dl.cache.is_verified(&f).unwrap(),
            "a file that never verified must not be cached"
        );
    }
}
