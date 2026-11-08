//! Durable rematch offer/acceptance and linked-match creation.

use crate::{AccountId, DeadlineId, DeadlineMillis, MatchId, Participants, ScheduledDeadline};
use pwmtf_billiards::RackSeed;
use pwmtf_eight_ball::{MatchConfiguration, MatchState, RematchMetadata};
use switchy_database::{
    Database, DatabaseValue,
    query::{FilterableQuery as _, SortDirection},
};
use thiserror::Error;

/// Pending rematch offer visible to the opponent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingRematch {
    /// Completed match being rematched.
    pub previous_match_id: MatchId,
    /// Participant who created the offer.
    pub offered_by: AccountId,
}

/// Returns pending opponent rematch offers for an authenticated account.
///
/// # Errors
///
/// Returns [`RematchStoreError`] for malformed canonical or offer records and
/// database failures.
pub async fn pending_rematches_for(
    db: &dyn Database,
    actor: AccountId,
) -> Result<Vec<PendingRematch>, RematchStoreError> {
    let rows = db
        .select("rematch_offers")
        .sort("previous_match_id", SortDirection::Asc)
        .execute(db)
        .await?;
    let mut pending = Vec::new();
    for row in rows {
        if !is_null(&row, "accepted_match_id")? {
            continue;
        }
        let previous_match_id = MatchId::new(parse_u128(&row, "previous_match_id")?);
        let offered_by = account(&row, "offered_by_account_id")?;
        if offered_by == actor {
            continue;
        }
        let (_, participants, previous) = load_match(db, previous_match_id).await?;
        authorize(participants, actor)?;
        RematchMetadata::from_completed(previous_match_id.value(), &previous)
            .map_err(|_| RematchStoreError::NotCompleted)?;
        pending.push(PendingRematch {
            previous_match_id,
            offered_by,
        });
    }
    Ok(pending)
}

/// Creates or idempotently repeats one participant's rematch offer.
///
/// # Errors
///
/// Returns [`RematchStoreError`] unless the previous match is completed and the
/// actor is one of its durable participants, or when persistence fails.
pub async fn offer_rematch(
    db: &dyn Database,
    previous_match_id: MatchId,
    actor: AccountId,
) -> Result<(), RematchStoreError> {
    let tx = db.begin_transaction().await?;
    let (revision, participants, previous) = load_match(&*tx, previous_match_id).await?;
    if revision == 0 {
        return Err(RematchStoreError::Malformed);
    }
    authorize(participants, actor)?;
    RematchMetadata::from_completed(previous_match_id.value(), &previous)
        .map_err(|_| RematchStoreError::NotCompleted)?;
    let existing = tx
        .select("rematch_offers")
        .where_eq("previous_match_id", previous_match_id.value().to_string())
        .execute(&*tx)
        .await?;
    match existing.as_slice() {
        [] => {
            let insert = tx
                .insert("rematch_offers")
                .value("previous_match_id", previous_match_id.value().to_string())
                .value("offered_by_account_id", actor.value().to_string())
                .value("accepted_match_id", DatabaseValue::Null)
                .execute(&*tx)
                .await;
            if let Err(error) = insert {
                let current = tx
                    .select("rematch_offers")
                    .where_eq("previous_match_id", previous_match_id.value().to_string())
                    .execute(&*tx)
                    .await?;
                return match current.as_slice() {
                    [row]
                        if account(row, "offered_by_account_id")? == actor
                            && is_null(row, "accepted_match_id")? =>
                    {
                        Ok(())
                    }
                    [row] if !is_null(row, "accepted_match_id")? => {
                        Err(RematchStoreError::AlreadyAccepted)
                    }
                    [_] => Err(RematchStoreError::OfferExists),
                    [] => Err(RematchStoreError::Database(error)),
                    _ => Err(RematchStoreError::Malformed),
                };
            }
            tx.commit().await?;
            Ok(())
        }
        [row]
            if account(row, "offered_by_account_id")? == actor
                && is_null(row, "accepted_match_id")? =>
        {
            Ok(())
        }
        [row] if !is_null(row, "accepted_match_id")? => Err(RematchStoreError::AlreadyAccepted),
        [_] => Err(RematchStoreError::OfferExists),
        _ => Err(RematchStoreError::Malformed),
    }
}

/// Atomically accepts an opponent's rematch offer and creates one linked match.
///
/// The new match preserves immutable rules/physics/table profiles, receives a
/// fresh rack seed, alternates the breaker, and links to the previous match.
///
/// # Errors
///
/// Returns [`RematchStoreError`] for missing, self-accepted, duplicate,
/// unauthorized, malformed, incompatible, or database records.
pub async fn accept_rematch(
    db: &dyn Database,
    previous_match_id: MatchId,
    new_match_id: MatchId,
    actor: AccountId,
    rack_seed: RackSeed,
    accepted_at: DeadlineMillis,
) -> Result<MatchState, RematchStoreError> {
    let tx = db.begin_transaction().await?;
    let (revision, participants, previous) = load_match(&*tx, previous_match_id).await?;
    if revision == 0 {
        return Err(RematchStoreError::Malformed);
    }
    authorize(participants, actor)?;
    let offer_rows = tx
        .select("rematch_offers")
        .where_eq("previous_match_id", previous_match_id.value().to_string())
        .execute(&*tx)
        .await?;
    let [offer] = offer_rows.as_slice() else {
        return Err(RematchStoreError::NotFound);
    };
    if !is_null(offer, "accepted_match_id")? {
        return Err(RematchStoreError::AlreadyAccepted);
    }
    if account(offer, "offered_by_account_id")? == actor {
        return Err(RematchStoreError::SelfAccept);
    }
    let metadata = RematchMetadata::from_completed(previous_match_id.value(), &previous)
        .map_err(|_| RematchStoreError::NotCompleted)?;
    let state = MatchState::new(
        previous.rules(),
        previous.physics(),
        previous.geometry(),
        rack_seed,
        metadata.breaker,
    )
    .map_err(|_| RematchStoreError::Malformed)?;
    let deadline = ScheduledDeadline {
        id: DeadlineId {
            revision: 0,
            player: state.active_player(),
        },
        // PWMTF only creates timed (standard-profile) matches.
        due_at: DeadlineMillis::after_turn(accepted_at, state.rules())
            .ok_or(RematchStoreError::Malformed)?,
    };
    tx.insert("matches")
        .value("match_id", new_match_id.value().to_string())
        .value("player_one_id", participants.player_one.value().to_string())
        .value("player_two_id", participants.player_two.value().to_string())
        .value("canonical_revision", 0_i64)
        .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
        .value("canonical_checksum", checksum_i64(state.checksum()))
        .value(
            "match_configuration",
            encode_bytes(&state.configuration().to_bytes()),
        )
        .value("deadline_revision", 0_i64)
        .value("deadline_player", encode_player(state.active_player()))
        .value("deadline_at_ms", to_i64(deadline.due_at.value())?)
        .value("previous_match_id", previous_match_id.value().to_string())
        .execute(&*tx)
        .await?;
    let updated = tx
        .update("rematch_offers")
        .value("accepted_match_id", new_match_id.value().to_string())
        .where_eq("previous_match_id", previous_match_id.value().to_string())
        .where_eq("accepted_match_id", DatabaseValue::Null)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(RematchStoreError::AlreadyAccepted);
    }
    tx.commit().await?;
    Ok(state)
}

/// Durable rematch lifecycle failure.
#[derive(Debug, Error)]
pub enum RematchStoreError {
    /// Match or offer does not exist.
    #[error("rematch record not found")]
    NotFound,
    /// Previous match has no terminal result.
    #[error("previous match is not completed")]
    NotCompleted,
    /// Actor is not a match participant.
    #[error("rematch operation is not authorized")]
    Unauthorized,
    /// Existing opponent offer must be accepted rather than overwritten.
    #[error("opponent rematch offer already exists")]
    OfferExists,
    /// Offer has already created a linked match.
    #[error("rematch offer already accepted")]
    AlreadyAccepted,
    /// Offer creator cannot accept their own offer.
    #[error("cannot accept own rematch offer")]
    SelfAccept,
    /// Stored canonical record is malformed or incompatible.
    #[error("stored rematch record is malformed")]
    Malformed,
    /// Numeric value exceeds portable schema bounds.
    #[error("rematch value is out of range")]
    Overflow,
    /// Database operation failed.
    #[error("rematch storage failed")]
    Database(#[from] switchy_database::DatabaseError),
}

async fn load_match(
    db: &dyn Database,
    match_id: MatchId,
) -> Result<(u64, Participants, MatchState), RematchStoreError> {
    let rows = db
        .select("matches")
        .where_eq("match_id", match_id.value().to_string())
        .execute(db)
        .await?;
    let [row] = rows.as_slice() else {
        return Err(RematchStoreError::NotFound);
    };
    let revision = integer(row, "canonical_revision")?;
    let snapshot = decode_bytes(&text(row, "canonical_snapshot")?)?;
    let state = MatchState::from_bytes(&snapshot).map_err(|_| RematchStoreError::Malformed)?;
    let configuration =
        MatchConfiguration::from_bytes(&decode_bytes(&text(row, "match_configuration")?)?)
            .map_err(|_| RematchStoreError::Malformed)?;
    if state.configuration() != configuration {
        return Err(RematchStoreError::Malformed);
    }
    let checksum = parse_u64(row, "canonical_checksum")?;
    if state.checksum() != checksum {
        return Err(RematchStoreError::Malformed);
    }
    let participants = Participants {
        player_one: AccountId::new(parse_u128(row, "player_one_id")?),
        player_two: AccountId::new(parse_u128(row, "player_two_id")?),
    };
    if participants.player_one == participants.player_two {
        return Err(RematchStoreError::Malformed);
    }
    Ok((revision, participants, state))
}

const fn authorize(participants: Participants, actor: AccountId) -> Result<(), RematchStoreError> {
    if actor.value() == participants.player_one.value()
        || actor.value() == participants.player_two.value()
    {
        Ok(())
    } else {
        Err(RematchStoreError::Unauthorized)
    }
}

fn account(row: &switchy_database::Row, column: &str) -> Result<AccountId, RematchStoreError> {
    Ok(AccountId::new(parse_u128(row, column)?))
}

fn text(row: &switchy_database::Row, column: &str) -> Result<String, RematchStoreError> {
    row.get(column)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(RematchStoreError::Malformed)
}

fn is_null(row: &switchy_database::Row, column: &str) -> Result<bool, RematchStoreError> {
    use switchy_database::query::Expression as _;
    row.get(column)
        .map(|value| value.is_null())
        .ok_or(RematchStoreError::Malformed)
}

fn parse_u128(row: &switchy_database::Row, column: &str) -> Result<u128, RematchStoreError> {
    text(row, column)?
        .parse()
        .map_err(|_| RematchStoreError::Malformed)
}

fn parse_u64(row: &switchy_database::Row, column: &str) -> Result<u64, RematchStoreError> {
    let value = row.get(column).ok_or(RematchStoreError::Malformed)?;
    if let Some(value) = value.as_i64() {
        return Ok(u64::from_ne_bytes(value.to_ne_bytes()));
    }
    if let Some(value) = value.as_u64() {
        return Ok(value);
    }
    value
        .as_str()
        .ok_or(RematchStoreError::Malformed)?
        .parse()
        .map_err(|_| RematchStoreError::Malformed)
}

fn integer(row: &switchy_database::Row, column: &str) -> Result<u64, RematchStoreError> {
    let value = row.get(column).ok_or(RematchStoreError::Malformed)?;
    if let Some(value) = value.as_i64() {
        return u64::try_from(value).map_err(|_| RematchStoreError::Malformed);
    }
    value.as_u64().ok_or(RematchStoreError::Malformed)
}

fn to_i64(value: u64) -> Result<i64, RematchStoreError> {
    i64::try_from(value).map_err(|_| RematchStoreError::Overflow)
}

const fn encode_player(player: pwmtf_eight_ball::Player) -> i64 {
    match player {
        pwmtf_eight_ball::Player::One => 1,
        pwmtf_eight_ball::Player::Two => 2,
    }
}

fn encode_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn decode_bytes(value: &str) -> Result<Vec<u8>, RematchStoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(RematchStoreError::Malformed);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok((hex(pair[0])? << 4) | hex(pair[1])?))
        .collect()
}

const fn hex(value: u8) -> Result<u8, RematchStoreError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(RematchStoreError::Malformed),
    }
}

const fn checksum_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_billiards::{PhysicsProfile, TableGeometry};
    use pwmtf_eight_ball::{MatchCommand, Player, RulesProfile, VersionedMatchCommand};

    use super::*;

    async fn database() -> Box<dyn Database> {
        let db = switchy_database_connection::builder()
            .turso()
            .with_in_memory()
            .build()
            .await
            .unwrap();
        crate::migrate(&*db).await.unwrap();
        db
    }

    async fn insert_completed_match(db: &dyn Database, match_id: MatchId) -> MatchState {
        insert_completed_match_at_revision(db, match_id, 1).await
    }

    async fn insert_completed_match_at_revision(
        db: &dyn Database,
        match_id: MatchId,
        revision: u64,
    ) -> MatchState {
        let mut previous = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        previous
            .apply_command(VersionedMatchCommand::new(MatchCommand::Concede {
                player: Player::Two,
            }))
            .unwrap();
        db.insert("matches")
            .value("match_id", match_id.value().to_string())
            .value("player_one_id", "1")
            .value("player_two_id", "2")
            .value("canonical_revision", i64::try_from(revision).unwrap())
            .value("canonical_snapshot", encode_bytes(&previous.to_bytes()))
            .value("canonical_checksum", checksum_i64(previous.checksum()))
            .value(
                "match_configuration",
                encode_bytes(&previous.configuration().to_bytes()),
            )
            .value("deadline_revision", DatabaseValue::Null)
            .value("deadline_player", DatabaseValue::Null)
            .value("deadline_at_ms", DatabaseValue::Null)
            .value("previous_match_id", DatabaseValue::Null)
            .execute(db)
            .await
            .unwrap();
        previous
    }

    #[test]
    fn concurrent_opposing_rematch_offers_create_one_offer() {
        block_on(async {
            let db: std::sync::Arc<dyn Database> = database().await.into();
            insert_completed_match(&*db, MatchId::new(6)).await;
            let first_db = std::sync::Arc::clone(&db);
            let second_db = std::sync::Arc::clone(&db);
            let (first, second) = futures_lite::future::zip(
                async move { offer_rematch(&*first_db, MatchId::new(6), AccountId::new(1)).await },
                async move { offer_rematch(&*second_db, MatchId::new(6), AccountId::new(2)).await },
            )
            .await;
            assert!(first.is_ok() ^ second.is_ok());
            assert_eq!(
                db.select("rematch_offers")
                    .execute(&*db)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        });
    }

    #[test]
    fn completed_snapshot_at_revision_zero_cannot_offer_a_rematch() {
        block_on(async {
            let db = database().await;
            insert_completed_match_at_revision(&*db, MatchId::new(7), 0).await;
            assert!(matches!(
                offer_rematch(&*db, MatchId::new(7), AccountId::new(1)).await,
                Err(RematchStoreError::Malformed)
            ));
            assert!(
                db.select("rematch_offers")
                    .execute(&*db)
                    .await
                    .unwrap()
                    .is_empty()
            );
        });
    }

    #[test]
    fn pending_rematches_are_visible_only_to_the_opponent() {
        block_on(async {
            let db = database().await;
            insert_completed_match(&*db, MatchId::new(8)).await;
            offer_rematch(&*db, MatchId::new(8), AccountId::new(1))
                .await
                .unwrap();
            assert!(
                pending_rematches_for(&*db, AccountId::new(1))
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                pending_rematches_for(&*db, AccountId::new(2))
                    .await
                    .unwrap(),
                vec![PendingRematch {
                    previous_match_id: MatchId::new(8),
                    offered_by: AccountId::new(1),
                }]
            );
            assert!(matches!(
                pending_rematches_for(&*db, AccountId::new(3)).await,
                Err(RematchStoreError::Unauthorized)
            ));
        });
    }

    #[test]
    fn accepted_rematch_is_linked_single_use_and_alternates_breaker() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            crate::migrate(&*db).await.unwrap();
            let mut previous = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            previous
                .apply_command(VersionedMatchCommand::new(MatchCommand::Concede {
                    player: Player::Two,
                }))
                .unwrap();
            db.insert("matches")
                .value("match_id", "9")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 1_i64)
                .value("canonical_snapshot", encode_bytes(&previous.to_bytes()))
                .value("canonical_checksum", checksum_i64(previous.checksum()))
                .value(
                    "match_configuration",
                    encode_bytes(&previous.configuration().to_bytes()),
                )
                .value("deadline_revision", DatabaseValue::Null)
                .value("deadline_player", DatabaseValue::Null)
                .value("deadline_at_ms", DatabaseValue::Null)
                .value("previous_match_id", DatabaseValue::Null)
                .execute(&*db)
                .await
                .unwrap();
            offer_rematch(&*db, MatchId::new(9), AccountId::new(1))
                .await
                .unwrap();
            let rematch = accept_rematch(
                &*db,
                MatchId::new(9),
                MatchId::new(10),
                AccountId::new(2),
                RackSeed::new(99),
                DeadlineMillis::new(30_000),
            )
            .await
            .unwrap();
            assert_eq!(rematch.breaker(), Player::Two);
            assert_eq!(rematch.rack_seed(), RackSeed::new(99));
            let row = db
                .select("matches")
                .where_eq("match_id", "10")
                .execute(&*db)
                .await
                .unwrap()
                .remove(0);
            assert_eq!(text(&row, "previous_match_id").unwrap(), "9");
            assert_eq!(integer(&row, "deadline_at_ms").unwrap(), 60_000);
            assert!(matches!(
                accept_rematch(
                    &*db,
                    MatchId::new(9),
                    MatchId::new(11),
                    AccountId::new(2),
                    RackSeed::new(100),
                    DeadlineMillis::new(30_000),
                )
                .await,
                Err(RematchStoreError::AlreadyAccepted)
            ));
        });
    }
}
