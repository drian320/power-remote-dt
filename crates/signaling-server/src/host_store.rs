//! Persistent host_id ↔ pubkey store backed by SQLite.

use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Mutex;

pub struct HostStore {
    conn: Mutex<Connection>,
}

#[derive(thiserror::Error, Debug)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("pubkey mismatch for host_id")]
    PubkeyMismatch,
    #[error("failed to allocate a unique host_id after retries")]
    AllocationExhausted,
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS hosts (
    host_id TEXT PRIMARY KEY,
    pubkey_b64 TEXT NOT NULL,
    registered_at INTEGER NOT NULL
)";

impl HostStore {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute(SCHEMA, [])?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        conn.execute(SCHEMA, [])?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// When `host_id` is `None`, allocate a fresh 9-digit ID. When `Some`,
    /// verify the stored pubkey matches; if no row exists, insert as a
    /// first-time registration. Returns the effective host_id in dashed form
    /// (e.g. `"123-456-789"`) when the stored ID is exactly 9 digits;
    /// otherwise returns the stored ID verbatim.
    pub fn allocate_or_verify(
        &self,
        host_id: Option<&str>,
        pubkey_b64: &str,
    ) -> Result<String, StoreError> {
        let key = pubkey_b64.trim();
        let now = now_secs();
        let conn = self.conn.lock().unwrap();
        match host_id {
            Some(id) => {
                // Only normalize dashes for 9-digit numeric IDs. Opaque
                // strings ("w1-test", "alice-desktop") keep dashes so the
                // ws.rs DashMap key matches what clients send in Connect.
                let stripped = id.replace('-', "");
                let id_normalized =
                    if stripped.len() == 9 && stripped.chars().all(|c| c.is_ascii_digit()) {
                        stripped
                    } else {
                        id.to_string()
                    };
                let existing: Option<String> = conn
                    .query_row(
                        "SELECT pubkey_b64 FROM hosts WHERE host_id = ?1",
                        params![id_normalized],
                        |r| r.get(0),
                    )
                    .ok();
                match existing {
                    Some(stored) if stored.trim() == key => Ok(display_format(&id_normalized)),
                    Some(_) => Err(StoreError::PubkeyMismatch),
                    None => {
                        conn.execute(
                            "INSERT INTO hosts (host_id, pubkey_b64, registered_at) VALUES (?1, ?2, ?3)",
                            params![id_normalized, key, now],
                        )?;
                        Ok(display_format(&id_normalized))
                    }
                }
            }
            None => {
                for _ in 0..5 {
                    let id = random_9digit();
                    let res = conn.execute(
                        "INSERT INTO hosts (host_id, pubkey_b64, registered_at) VALUES (?1, ?2, ?3)",
                        params![id, key, now],
                    );
                    match res {
                        Ok(_) => return Ok(display_format(&id)),
                        Err(rusqlite::Error::SqliteFailure(e, _))
                            if e.code == rusqlite::ErrorCode::ConstraintViolation =>
                        {
                            continue;
                        }
                        Err(e) => return Err(StoreError::Sqlite(e)),
                    }
                }
                Err(StoreError::AllocationExhausted)
            }
        }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_9digit() -> String {
    use rand_core::{OsRng, RngCore};
    let n: u32 = 100_000_000 + (OsRng.next_u32() % 900_000_000);
    format!("{n:09}")
}

/// Insert dashes at positions 3 and 6 when `id` is exactly 9 ASCII digits.
/// Otherwise return `id` unchanged.
fn display_format(id: &str) -> String {
    if id.len() == 9 && id.chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &id[0..3], &id[3..6], &id[6..9])
    } else {
        id.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_new_id() {
        let s = HostStore::open_in_memory().unwrap();
        let id = s.allocate_or_verify(None, "AAA").unwrap();
        assert_eq!(id.len(), 11, "expected 9 digits + 2 dashes: {id}");
    }

    #[test]
    fn verify_same_pubkey_reuses_id() {
        let s = HostStore::open_in_memory().unwrap();
        let id = s.allocate_or_verify(None, "AAA").unwrap();
        let id2 = s.allocate_or_verify(Some(&id), "AAA").unwrap();
        assert_eq!(id, id2);
    }

    #[test]
    fn verify_different_pubkey_is_mismatch() {
        let s = HostStore::open_in_memory().unwrap();
        let id = s.allocate_or_verify(None, "AAA").unwrap();
        let err = s.allocate_or_verify(Some(&id), "BBB").unwrap_err();
        assert!(matches!(err, StoreError::PubkeyMismatch));
    }

    #[test]
    fn first_time_register_with_9digit_input() {
        let s = HostStore::open_in_memory().unwrap();
        let id = s.allocate_or_verify(Some("987654321"), "AAA").unwrap();
        assert_eq!(id, "987-654-321");
        let id2 = s.allocate_or_verify(Some("987-654-321"), "AAA").unwrap();
        assert_eq!(id2, "987-654-321");
    }

    #[test]
    fn first_time_register_with_opaque_string() {
        let s = HostStore::open_in_memory().unwrap();
        let id = s.allocate_or_verify(Some("alice-desktop"), "AAA").unwrap();
        // Opaque strings preserve their dashes verbatim (not 9-digit numeric).
        assert_eq!(id, "alice-desktop");
        // Re-register with same pubkey reuses the same ID.
        let id2 = s.allocate_or_verify(Some("alice-desktop"), "AAA").unwrap();
        assert_eq!(id2, "alice-desktop");
    }
}
