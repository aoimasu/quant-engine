//! Shared LMDB plumbing for the market and synthetic stores: the single `unsafe` env-open and the
//! schema version check.

use std::path::Path;

use heed::types::Str;
use heed::{Database, Env, EnvOpenOptions, RwTxn};

use crate::StorageError;

/// The `meta` sub-database name and the schema-version key, shared by both stores.
pub(crate) const DB_META: &str = "meta";
pub(crate) const KEY_SCHEMA_VERSION: &str = "schema_version";

/// Open (creating the directory if needed) an LMDB environment at `path`.
///
/// This is the crate's **only** `unsafe` call.
///
/// # Errors
/// [`StorageError`] on I/O or an LMDB failure.
pub(crate) fn open_env(
    path: impl AsRef<Path>,
    map_size: usize,
    max_dbs: u32,
) -> Result<Env, StorageError> {
    std::fs::create_dir_all(&path)?;
    // SAFETY: `EnvOpenOptions::open` is `unsafe` because LMDB memory-maps the database file and the
    // caller must ensure no other mapping mutates it unsoundly. We uphold this: a single process
    // owns this exclusive on-disk path via one `Env`, and the mapping is never handed to foreign
    // code — the standard, sound usage of an embedded LMDB store. (See each store's `open` for the
    // single-open-per-path caller contract.)
    #[allow(unsafe_code)]
    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(map_size)
            .max_dbs(max_dbs)
            .open(path)?
    };
    Ok(env)
}

/// Record `expected` on a fresh store, or reject a store whose recorded version differs.
///
/// # Errors
/// [`StorageError::SchemaMismatch`] if a different version is recorded; [`StorageError::SchemaCorrupt`]
/// if the record is unparseable; or an LMDB error.
pub(crate) fn check_or_init_schema(
    meta: &Database<Str, Str>,
    wtxn: &mut RwTxn,
    expected: u32,
) -> Result<(), StorageError> {
    match meta.get(wtxn, KEY_SCHEMA_VERSION)? {
        Some(found_str) => {
            let found: u32 = found_str
                .parse()
                .map_err(|_| StorageError::SchemaCorrupt(found_str.to_owned()))?;
            if found != expected {
                return Err(StorageError::SchemaMismatch { expected, found });
            }
        }
        None => {
            meta.put(wtxn, KEY_SCHEMA_VERSION, &expected.to_string())?;
        }
    }
    Ok(())
}

/// Read the recorded schema version from a store's `meta` db.
///
/// # Errors
/// [`StorageError`] on an LMDB failure or a corrupt/missing record.
pub(crate) fn read_schema_version(
    env: &Env,
    meta: &Database<Str, Str>,
) -> Result<u32, StorageError> {
    let rtxn = env.read_txn()?;
    match meta.get(&rtxn, KEY_SCHEMA_VERSION)? {
        Some(v) => v
            .parse()
            .map_err(|_| StorageError::SchemaCorrupt(v.to_owned())),
        None => Err(StorageError::SchemaCorrupt("missing".to_owned())),
    }
}
