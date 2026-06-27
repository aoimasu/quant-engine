//! Local raw-file cache, mirroring the remote layout under a configurable root directory.
//!
//! Each dump file is stored at `<root>/<relative_path>` alongside a `<relative_path>.sha256` sidecar
//! recording the verified digest. A cached file is trusted only when its bytes re-hash to the stored
//! digest — so a truncated or tampered file is re-fetched rather than served.

use std::path::{Path, PathBuf};

use crate::checksum::sha256_hex;
use crate::source::DumpFile;
use crate::IngestError;

/// A filesystem cache rooted at a configurable, volume-friendly directory (QE-013).
#[derive(Debug, Clone)]
pub struct RawCache {
    root: PathBuf,
}

impl RawCache {
    /// A cache rooted at `root` (created on first write).
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Absolute path of a dump file in the cache.
    #[must_use]
    pub fn path_of(&self, file: &DumpFile) -> PathBuf {
        self.root.join(file.relative_path())
    }

    fn sidecar_of(&self, file: &DumpFile) -> PathBuf {
        self.root.join(format!("{}.sha256", file.relative_path()))
    }

    /// Whether the file is present **and** its bytes re-hash to the stored sidecar digest. A missing
    /// file, missing sidecar, or hash mismatch all return `false` (→ the downloader will re-fetch).
    ///
    /// # Errors
    /// [`IngestError::Io`] only on an unexpected read error (not on plain absence).
    pub fn is_verified(&self, file: &DumpFile) -> Result<bool, IngestError> {
        let path = self.path_of(file);
        let sidecar = self.sidecar_of(file);
        if !path.exists() || !sidecar.exists() {
            return Ok(false);
        }
        let bytes = read(&path)?;
        let stored = read_to_string(&sidecar)?;
        Ok(stored.trim() == sha256_hex(&bytes))
    }

    /// Store `bytes` for `file` plus its `digest` sidecar (creating parent directories).
    ///
    /// # Errors
    /// [`IngestError::Io`] on a directory-creation or write failure.
    pub fn store(&self, file: &DumpFile, bytes: &[u8], digest: &str) -> Result<(), IngestError> {
        let path = self.path_of(file);
        if let Some(parent) = path.parent() {
            create_dir_all(parent)?;
        }
        write(&path, bytes)?;
        write(&self.sidecar_of(file), digest.as_bytes())?;
        Ok(())
    }

    /// Read a cached file's bytes.
    ///
    /// # Errors
    /// [`IngestError::Io`] if it is absent or unreadable.
    pub fn read(&self, file: &DumpFile) -> Result<Vec<u8>, IngestError> {
        read(&self.path_of(file))
    }
}

fn read(path: &Path) -> Result<Vec<u8>, IngestError> {
    std::fs::read(path).map_err(|source| IngestError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn read_to_string(path: &Path) -> Result<String, IngestError> {
    std::fs::read_to_string(path).map_err(|source| IngestError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn write(path: &Path, bytes: &[u8]) -> Result<(), IngestError> {
    std::fs::write(path, bytes).map_err(|source| IngestError::Io {
        path: path.display().to_string(),
        source,
    })
}

fn create_dir_all(path: &Path) -> Result<(), IngestError> {
    std::fs::create_dir_all(path).map_err(|source| IngestError::Io {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::{DataKind, Date, Period};
    use qe_domain::{InstrumentId, Resolution};

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

    #[test]
    fn store_then_verify_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = RawCache::new(tmp.path());
        let f = file();
        assert!(!cache.is_verified(&f).unwrap()); // absent

        let bytes = b"zip-bytes";
        cache.store(&f, bytes, &sha256_hex(bytes)).unwrap();
        assert!(cache.is_verified(&f).unwrap());
        assert_eq!(cache.read(&f).unwrap(), bytes);
    }

    #[test]
    fn tampered_file_is_not_verified() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = RawCache::new(tmp.path());
        let f = file();
        cache
            .store(&f, b"original", &sha256_hex(b"original"))
            .unwrap();
        // Corrupt the cached blob behind the cache's back.
        std::fs::write(cache.path_of(&f), b"tampered").unwrap();
        assert!(
            !cache.is_verified(&f).unwrap(),
            "a blob that no longer matches its sidecar must not be trusted"
        );
    }
}
