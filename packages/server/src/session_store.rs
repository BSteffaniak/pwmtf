//! Switchy query adapters for hash-only sessions.

use crate::{AccountId, Session, SessionToken, SessionTokenHash, TokenError};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;

/// Inserts a hash-only session record.
///
/// # Errors
///
/// Returns [`SessionStoreError`] for timestamp/account conversion overflow or
/// database failures.
pub async fn insert_session(
    db: &dyn Database,
    token_hash: SessionTokenHash,
    session: Session,
) -> Result<(), SessionStoreError> {
    if session.expires_at <= session.last_used_at {
        return Err(SessionStoreError::InvalidExpiration);
    }
    let encoded_hash = encode_hash(token_hash);
    let insert = db
        .insert("sessions")
        .value("session_hash", encoded_hash.clone())
        .value("account_id", session.account.value().to_string())
        .value("expires_at_ms", to_i64(session.expires_at)?)
        .value("last_used_at_ms", to_i64(session.last_used_at)?)
        .execute(db)
        .await;
    if let Err(error) = insert {
        let rows = db
            .select("sessions")
            .where_eq("session_hash", encoded_hash)
            .execute(db)
            .await?;
        let [row] = rows.as_slice() else {
            return Err(SessionStoreError::Database(error));
        };
        let account = row
            .get("account_id")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .and_then(|value| value.parse::<u128>().ok());
        let expires_at = row.get("expires_at_ms").and_then(|value| value.as_i64());
        let last_used_at = row.get("last_used_at_ms").and_then(|value| value.as_i64());
        if account == Some(session.account.value())
            && expires_at == Some(to_i64(session.expires_at)?)
            && last_used_at == Some(to_i64(session.last_used_at)?)
        {
            return Ok(());
        }
        return Err(SessionStoreError::TokenConflict);
    }
    Ok(())
}

/// Generates and inserts a hash-only session, returning its raw token exactly
/// once to the authenticated transport boundary.
///
/// # Errors
///
/// Returns [`SessionStoreError`] when secure generation or persistence fails.
pub async fn create_session(
    db: &dyn Database,
    session: Session,
) -> Result<SessionToken, SessionStoreError> {
    let token = SessionToken::generate()?;
    insert_session(db, token.hash(), session).await?;
    Ok(token)
}

/// Resolves an unexpired raw session token through its hash.
///
/// # Errors
///
/// Returns [`SessionStoreError`] for malformed tokens, stored values, or
/// database failures.
pub async fn resolve_token(
    db: &dyn Database,
    token: &str,
    now: u64,
) -> Result<Option<AccountId>, SessionStoreError> {
    let token = SessionToken::parse(token)?;
    resolve_session(db, token.hash(), now).await
}

/// Revokes a raw session token through its hash.
///
/// # Errors
///
/// Returns [`SessionStoreError`] for malformed tokens or database failures.
pub async fn revoke_token(db: &dyn Database, token: &str) -> Result<(), SessionStoreError> {
    let token = SessionToken::parse(token)?;
    revoke_session(db, token.hash()).await
}

/// Resolves an unexpired session hash and atomically advances last-use.
///
/// Contention before the first write is retried from a fresh Switchy
/// transaction. An ambiguous commit is never retried.
///
/// # Errors
///
/// Returns [`SessionStoreError`] for malformed stored values or database failures.
pub async fn resolve_session(
    db: &dyn Database,
    token_hash: SessionTokenHash,
    now: u64,
) -> Result<Option<AccountId>, SessionStoreError> {
    for attempt in 0..5 {
        // Only retry failures before the first successful write. A failed
        // commit is ambiguous: authentication must fail closed in that case.
        match resolve_session_once(db, token_hash, now).await {
            Err(SessionStoreError::PreWriteBusy) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await;
            }
            Err(SessionStoreError::PreWriteBusy) => return Err(SessionStoreError::Busy),
            result => return result,
        }
    }
    unreachable!("bounded retry loop returns on its final attempt")
}

async fn resolve_session_once(
    db: &dyn Database,
    token_hash: SessionTokenHash,
    now: u64,
) -> Result<Option<AccountId>, SessionStoreError> {
    let tx = db.begin_transaction().await.map_err(pre_write_error)?;
    let encoded_hash = encode_hash(token_hash);
    let rows = tx
        .select("sessions")
        .where_eq("session_hash", encoded_hash.clone())
        .execute(&*tx)
        .await
        .map_err(pre_write_error)?;
    if rows.len() > 1 {
        return Err(SessionStoreError::Malformed);
    }
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let expires_at = row
        .get("expires_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(SessionStoreError::Malformed)?;
    let last_used_at = row
        .get("last_used_at_ms")
        .and_then(|value| value.as_i64())
        .ok_or(SessionStoreError::Malformed)?;
    let now = to_i64(now)?;
    if expires_at <= last_used_at || now < last_used_at {
        return Err(SessionStoreError::Malformed);
    }
    if expires_at <= now {
        tx.delete("sessions")
            .where_eq("session_hash", encoded_hash)
            .where_eq("last_used_at_ms", last_used_at)
            .execute(&*tx)
            .await
            .map_err(pre_write_snapshot_error)?;
        tx.commit().await?;
        return Ok(None);
    }
    let account = row
        .get("account_id")
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(SessionStoreError::Malformed)?
        .parse::<u128>()
        .map_err(|_| SessionStoreError::Malformed)?;
    let updated = tx
        .update("sessions")
        .value("last_used_at_ms", now)
        .where_eq("session_hash", encoded_hash)
        .where_eq("last_used_at_ms", last_used_at)
        .execute(&*tx)
        .await
        .map_err(pre_write_snapshot_error)?;
    if updated.len() != 1 {
        let current = tx
            .select("sessions")
            .where_eq("session_hash", encode_hash(token_hash))
            .execute(&*tx)
            .await?;
        let [current] = current.as_slice() else {
            return Err(SessionStoreError::ConcurrentUse);
        };
        let current_account = current
            .get("account_id")
            .and_then(|value| value.as_str().map(ToOwned::to_owned))
            .ok_or(SessionStoreError::Malformed)?
            .parse::<u128>()
            .map_err(|_| SessionStoreError::Malformed)?;
        let current_expiry = current
            .get("expires_at_ms")
            .and_then(|value| value.as_i64())
            .ok_or(SessionStoreError::Malformed)?;
        let current_last_used = current
            .get("last_used_at_ms")
            .and_then(|value| value.as_i64())
            .ok_or(SessionStoreError::Malformed)?;
        if current_account == account && current_expiry == expires_at && current_last_used == now {
            return Ok(Some(AccountId::new(account)));
        }
        return Err(SessionStoreError::ConcurrentUse);
    }
    tx.commit().await?;
    Ok(Some(AccountId::new(account)))
}

fn pre_write_error(error: switchy_database::DatabaseError) -> SessionStoreError {
    if matches!(
        &error,
        switchy_database::DatabaseError::Turso(switchy_database::turso::TursoDatabaseError::Turso(
            turso::Error::Busy(_) | turso::Error::BusySnapshot(_)
        ))
    ) {
        SessionStoreError::PreWriteBusy
    } else {
        error.into()
    }
}

fn pre_write_snapshot_error(error: switchy_database::DatabaseError) -> SessionStoreError {
    if matches!(
        &error,
        switchy_database::DatabaseError::Turso(switchy_database::turso::TursoDatabaseError::Turso(
            turso::Error::Busy(_) | turso::Error::BusySnapshot(_)
        ))
    ) {
        SessionStoreError::PreWriteBusy
    } else {
        error.into()
    }
}

/// Deletes every expired session by server-clock time.
///
/// # Errors
///
/// Returns [`SessionStoreError`] when the cutoff exceeds the portable schema or
/// the cleanup query fails.
pub async fn cleanup_expired_sessions(
    db: &dyn Database,
    now: u64,
) -> Result<(), SessionStoreError> {
    db.delete("sessions")
        .where_lte("expires_at_ms", to_i64(now)?)
        .execute(db)
        .await?;
    Ok(())
}

/// Revokes a session by deleting only its hash-indexed record.
///
/// # Errors
///
/// Returns [`SessionStoreError::Database`] on query failure.
pub async fn revoke_session(
    db: &dyn Database,
    token_hash: SessionTokenHash,
) -> Result<(), SessionStoreError> {
    db.delete("sessions")
        .where_eq("session_hash", encode_hash(token_hash))
        .execute(db)
        .await?;
    Ok(())
}

/// Session query adapter failure.
#[derive(Debug, Error)]
pub enum SessionStoreError {
    /// Switchy query failed.
    #[error(transparent)]
    Database(#[from] switchy_database::DatabaseError),
    /// A bounded retry exhausted pre-write Turso contention.
    #[error("session storage is busy")]
    Busy,
    /// Retryable contention before a transaction's first successful write.
    #[error("session storage is busy before write")]
    PreWriteBusy,
    /// Token generation or canonical parsing failed.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// Session expiration is not later than its creation/use time.
    #[error("session expiration is invalid")]
    InvalidExpiration,
    /// Numeric value cannot be represented by the portable schema.
    #[error("session value exceeds portable schema bounds")]
    Overflow,
    /// Session token hash already identifies different durable data.
    #[error("session token hash conflict")]
    TokenConflict,
    /// Concurrent session use changed the record during resolution.
    #[error("session was used concurrently")]
    ConcurrentUse,
    /// Stored record is malformed.
    #[error("stored session is malformed")]
    Malformed,
}

fn encode_hash(hash: SessionTokenHash) -> String {
    let mut output = String::with_capacity(64);
    for byte in hash.bytes() {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn to_i64(value: u64) -> Result<i64, SessionStoreError> {
    i64::try_from(value).map_err(|_| SessionStoreError::Overflow)
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;

    use super::*;

    #[test]
    fn pre_write_turso_contention_is_retryable_but_other_errors_are_not() {
        use switchy_database::{DatabaseError, turso::TursoDatabaseError};
        let busy =
            || DatabaseError::Turso(TursoDatabaseError::Turso(turso::Error::Busy(String::new())));
        let stale = || {
            DatabaseError::Turso(TursoDatabaseError::Turso(turso::Error::BusySnapshot(
                String::new(),
            )))
        };
        assert!(matches!(
            pre_write_error(busy()),
            SessionStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(stale()),
            SessionStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(busy()),
            SessionStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(DatabaseError::NoRow),
            SessionStoreError::Database(_)
        ));
    }

    #[test]
    fn duplicate_session_insert_is_idempotent_but_conflicts_fail() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            crate::migrate(&*db).await.expect("schema migrates");
            let hash = SessionTokenHash::new([9; 32]);
            let session = Session {
                account: AccountId::new(7),
                expires_at: 100,
                last_used_at: 0,
            };
            insert_session(&*db, hash, session).await.unwrap();
            insert_session(&*db, hash, session).await.unwrap();
            assert!(matches!(
                insert_session(
                    &*db,
                    hash,
                    Session {
                        account: AccountId::new(8),
                        ..session
                    },
                )
                .await,
                Err(SessionStoreError::TokenConflict)
            ));
            assert_eq!(db.select("sessions").execute(&*db).await.unwrap().len(), 1);
        });
    }

    #[test]
    fn generated_session_persists_only_hash_and_resolves_raw_token() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            crate::migrate(&*db).await.expect("schema migrates");
            let token = create_session(
                &*db,
                Session {
                    account: AccountId::new(9),
                    expires_at: 100,
                    last_used_at: 0,
                },
            )
            .await
            .unwrap();
            let stored = db
                .select("sessions")
                .execute(&*db)
                .await
                .unwrap()
                .remove(0)
                .get("session_hash")
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap();
            assert_ne!(stored, token.expose());
            assert_eq!(
                resolve_token(&*db, token.expose(), 50).await.unwrap(),
                Some(AccountId::new(9))
            );
            revoke_token(&*db, token.expose()).await.unwrap();
            assert_eq!(resolve_token(&*db, token.expose(), 50).await.unwrap(), None);
        });
    }

    #[test]
    fn concurrent_same_timestamp_session_use_is_idempotent() {
        block_on(async {
            let db: std::sync::Arc<dyn Database> = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens")
                .into();
            crate::migrate(&*db).await.expect("schema migrates");
            let hash = SessionTokenHash::new([8; 32]);
            insert_session(
                &*db,
                hash,
                Session {
                    account: AccountId::new(7),
                    expires_at: 100,
                    last_used_at: 0,
                },
            )
            .await
            .unwrap();
            let first_db = std::sync::Arc::clone(&db);
            let second_db = std::sync::Arc::clone(&db);
            let (first, second) = futures_lite::future::zip(
                async move { resolve_session(&*first_db, hash, 50).await },
                async move { resolve_session(&*second_db, hash, 50).await },
            )
            .await;
            assert_eq!(first.unwrap(), Some(AccountId::new(7)));
            assert_eq!(second.unwrap(), Some(AccountId::new(7)));
        });
    }

    #[test]
    fn scheduled_cleanup_removes_all_expired_sessions() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            crate::migrate(&*db).await.expect("schema migrates");
            for (hash, expires_at) in [([1; 32], 100), ([2; 32], 101), ([3; 32], 99)] {
                insert_session(
                    &*db,
                    SessionTokenHash::new(hash),
                    Session {
                        account: AccountId::new(42),
                        expires_at,
                        last_used_at: 0,
                    },
                )
                .await
                .unwrap();
            }
            cleanup_expired_sessions(&*db, 100).await.unwrap();
            let rows = db.select("sessions").execute(&*db).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(
                rows[0]
                    .get("expires_at_ms")
                    .and_then(|value| value.as_i64()),
                Some(101)
            );
        });
    }

    #[test]
    fn switchy_session_round_trip_and_revocation() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .expect("in-memory Turso opens");
            crate::migrate(&*db).await.expect("schema migrates");
            let hash = SessionTokenHash::new([5; 32]);
            assert!(matches!(
                insert_session(
                    &*db,
                    SessionTokenHash::new([4; 32]),
                    Session {
                        account: AccountId::new(42),
                        expires_at: 100,
                        last_used_at: 100,
                    },
                )
                .await,
                Err(SessionStoreError::InvalidExpiration)
            ));
            db.insert("sessions")
                .value("session_hash", encode_hash(SessionTokenHash::new([3; 32])))
                .value("account_id", "42")
                .value("expires_at_ms", 100_i64)
                .value("last_used_at_ms", 100_i64)
                .execute(&*db)
                .await
                .unwrap();
            assert!(matches!(
                resolve_session(&*db, SessionTokenHash::new([3; 32]), 50).await,
                Err(SessionStoreError::Malformed)
            ));
            revoke_session(&*db, SessionTokenHash::new([3; 32]))
                .await
                .unwrap();
            db.insert("sessions")
                .value("session_hash", encode_hash(SessionTokenHash::new([2; 32])))
                .value("account_id", "42")
                .value("expires_at_ms", 200_i64)
                .value("last_used_at_ms", 100_i64)
                .execute(&*db)
                .await
                .unwrap();
            assert!(matches!(
                resolve_session(&*db, SessionTokenHash::new([2; 32]), 50).await,
                Err(SessionStoreError::Malformed)
            ));
            revoke_session(&*db, SessionTokenHash::new([2; 32]))
                .await
                .unwrap();
            insert_session(
                &*db,
                hash,
                Session {
                    account: AccountId::new(42),
                    expires_at: 100,
                    last_used_at: 0,
                },
            )
            .await
            .expect("session inserts");
            assert_eq!(
                resolve_session(&*db, hash, 50).await.unwrap(),
                Some(AccountId::new(42))
            );
            let row = db.select("sessions").execute(&*db).await.unwrap().remove(0);
            assert_eq!(
                row.get("last_used_at_ms").and_then(|value| value.as_i64()),
                Some(50)
            );
            assert_eq!(resolve_session(&*db, hash, 100).await.unwrap(), None);
            assert!(
                db.select("sessions")
                    .execute(&*db)
                    .await
                    .unwrap()
                    .is_empty()
            );
            revoke_session(&*db, hash).await.unwrap();
            assert_eq!(resolve_session(&*db, hash, 50).await.unwrap(), None);
        });
    }
}
