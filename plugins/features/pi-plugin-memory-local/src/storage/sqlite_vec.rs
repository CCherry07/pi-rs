//! Private process-wide registration for the statically linked `sqlite-vec`
//! extension.
//!
//! `sqlite-vec` exposes its SQLite entry point with an erased C signature, so
//! registering it with `rusqlite` requires one audited conversion. Unsafe code
//! is denied crate-wide and allowed only in `register_once` below.

use std::sync::OnceLock;

use rusqlite::auto_extension::{RawAutoExtension, register_auto_extension};

static REGISTRATION: OnceLock<Result<(), RegistrationError>> = OnceLock::new();

/// Register `sqlite-vec` for every SQLite connection opened by this process.
///
/// Registration is attempted once. Later calls return the same result, so a
/// failure cannot be silently retried into a partially configured process.
pub(super) fn register() -> Result<(), RegistrationError> {
    REGISTRATION.get_or_init(register_once).clone()
}

#[allow(unsafe_code)]
fn register_once() -> Result<(), RegistrationError> {
    // SAFETY: sqlite-vec documents `sqlite3_vec_init` as its SQLite extension
    // entry point. The exported Rust symbol has an erased zero-argument type,
    // but the linked C implementation has exactly SQLite's RawAutoExtension
    // calling convention. The function pointer remains valid for the process
    // lifetime because sqlite-vec is statically linked into this provider crate.
    let extension = unsafe {
        std::mem::transmute::<*const (), RawAutoExtension>(
            sqlite_vec::sqlite3_vec_init as *const (),
        )
    };
    // SAFETY: `extension` is the process-lifetime sqlite-vec initializer
    // described above. This module never resets or cancels the registration.
    unsafe { register_auto_extension(extension) }
        .map_err(|error| RegistrationError::Register(error.to_string()))
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(super) enum RegistrationError {
    #[error("cannot register sqlite-vec: {0}")]
    Register(String),
}

#[cfg(test)]
mod tests {
    use rusqlite::{Connection, params};

    #[test]
    fn registration_loads_vec0_and_cosine_knn() {
        super::register().expect("sqlite-vec registration");
        let connection = Connection::open_in_memory().expect("in-memory SQLite");
        let version = connection
            .query_row("SELECT vec_version()", [], |row| row.get::<_, String>(0))
            .expect("sqlite-vec version");
        assert!(!version.is_empty());
        connection
            .execute_batch(
                "CREATE VIRTUAL TABLE vectors USING vec0(
                     id TEXT PRIMARY KEY,
                     embedding FLOAT[2] distance_metric=cosine,
                     scope_key TEXT
                 );",
            )
            .expect("vec0 table");
        connection
            .execute(
                "INSERT INTO vectors(id, embedding, scope_key) VALUES (?1, ?2, ?3)",
                params!["near", f32_blob(&[1.0, 0.0]), "project:a"],
            )
            .expect("near vector");
        connection
            .execute(
                "INSERT INTO vectors(id, embedding, scope_key) VALUES (?1, ?2, ?3)",
                params!["far", f32_blob(&[0.0, 1.0]), "project:a"],
            )
            .expect("far vector");

        let nearest = connection
            .query_row(
                "SELECT id FROM vectors
                 WHERE embedding MATCH ?1 AND k = 1 AND scope_key IN (?2)
                 ORDER BY distance",
                params![f32_blob(&[0.9, 0.1]), "project:a"],
                |row| row.get::<_, String>(0),
            )
            .expect("nearest vector");
        assert_eq!(nearest, "near");
    }

    fn f32_blob(vector: &[f32]) -> Vec<u8> {
        vector
            .iter()
            .flat_map(|value| value.to_ne_bytes())
            .collect()
    }
}
