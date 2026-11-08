//! Rebuildable match-summary projection derived from canonical snapshots.

use crate::{AccountId, MatchId, Participants};
use pwmtf_eight_ball::{CompletionReason, MatchConfiguration, MatchState, MatchStatus, Player};
use switchy_database::{Database, query::FilterableQuery as _};
use thiserror::Error;

/// One rebuildable completed-match summary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatchSummary {
    /// Canonical match identifier.
    pub match_id: MatchId,
    /// Winning account.
    pub winner: AccountId,
    /// Canonical completion reason.
    pub reason: CompletionReason,
    /// Canonical revision projected.
    pub revision: u64,
    /// Canonical checksum projected.
    pub checksum: u64,
}

/// Rebuilds all match summaries exclusively from canonical match snapshots.
///
/// Existing summaries are deleted first. In-progress matches intentionally
/// produce no row, so this projection remains disposable and reconstructible.
///
/// # Errors
///
/// Returns [`ProjectionError`] for malformed canonical rows, unsupported
/// snapshots, inconsistent checksums, overflow, or database failures.
pub async fn rebuild_match_summaries(
    db: &dyn Database,
) -> Result<Vec<MatchSummary>, ProjectionError> {
    let tx = db.begin_transaction().await?;
    tx.delete("match_summaries").execute(&*tx).await?;
    let rows = tx.select("matches").execute(&*tx).await?;
    let mut summaries = Vec::new();
    for row in rows {
        let match_id = MatchId::new(parse_u128(&row, "match_id")?);
        let participants = decode_participants(&row)?;
        let revision = unsigned_integer(&row, "canonical_revision")?;
        let snapshot = decode_bytes(&text(&row, "canonical_snapshot")?)?;
        let checksum = parse_u64(&row, "canonical_checksum")?;
        let state = MatchState::from_bytes(&snapshot).map_err(|_| ProjectionError::Malformed)?;
        let configuration =
            MatchConfiguration::from_bytes(&decode_bytes(&text(&row, "match_configuration")?)?)
                .map_err(|_| ProjectionError::Malformed)?;
        if state.configuration() != configuration || state.checksum() != checksum {
            return Err(ProjectionError::Malformed);
        }
        let MatchStatus::Completed(outcome) = state.status() else {
            continue;
        };
        if revision == 0 {
            return Err(ProjectionError::Malformed);
        }
        let winner = match outcome.winner {
            Player::One => participants.player_one,
            Player::Two => participants.player_two,
        };
        let summary = MatchSummary {
            match_id,
            winner,
            reason: outcome.reason,
            revision,
            checksum,
        };
        tx.insert("match_summaries")
            .value("match_id", match_id.value().to_string())
            .value("winner_account_id", winner.value().to_string())
            .value("completion_reason", encode_reason(outcome.reason))
            .value("canonical_revision", to_i64(revision)?)
            .value("canonical_checksum", checksum_i64(checksum))
            .execute(&*tx)
            .await?;
        summaries.push(summary);
    }
    summaries.sort_by_key(|summary| summary.match_id);
    tx.commit().await?;
    Ok(summaries)
}

/// Loads one disposable match-summary projection row.
///
/// # Errors
///
/// Returns [`ProjectionError`] for malformed or duplicate records and database
/// failures.
pub async fn match_summary(
    db: &dyn Database,
    match_id: MatchId,
) -> Result<Option<MatchSummary>, ProjectionError> {
    let rows = db
        .select("match_summaries")
        .where_eq("match_id", match_id.value().to_string())
        .execute(db)
        .await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => {
            let winner = AccountId::new(parse_u128(row, "winner_account_id")?);
            let revision = unsigned_integer(row, "canonical_revision")?;
            if revision == 0 {
                return Err(ProjectionError::Malformed);
            }
            Ok(Some(MatchSummary {
                match_id,
                winner,
                reason: decode_reason(&text(row, "completion_reason")?)?,
                revision,
                checksum: parse_u64(row, "canonical_checksum")?,
            }))
        }
        _ => Err(ProjectionError::Malformed),
    }
}

/// Rebuildable projection failure.
#[derive(Debug, Error)]
pub enum ProjectionError {
    /// Canonical or projection row is malformed/incompatible.
    #[error("match projection record is malformed")]
    Malformed,
    /// Numeric value exceeds portable database bounds.
    #[error("match projection value is out of range")]
    Overflow,
    /// Database operation failed.
    #[error("match projection storage failed")]
    Database(#[from] switchy_database::DatabaseError),
}

const fn encode_reason(reason: CompletionReason) -> &'static str {
    match reason {
        CompletionReason::LegalEightBall => "legal-eight-ball",
        CompletionReason::IllegalEightBall => "illegal-eight-ball",
        CompletionReason::Concession => "concession",
    }
}

fn decode_reason(value: &str) -> Result<CompletionReason, ProjectionError> {
    match value {
        "legal-eight-ball" => Ok(CompletionReason::LegalEightBall),
        "illegal-eight-ball" => Ok(CompletionReason::IllegalEightBall),
        "concession" => Ok(CompletionReason::Concession),
        _ => Err(ProjectionError::Malformed),
    }
}

fn decode_participants(row: &switchy_database::Row) -> Result<Participants, ProjectionError> {
    let participants = Participants {
        player_one: AccountId::new(parse_u128(row, "player_one_id")?),
        player_two: AccountId::new(parse_u128(row, "player_two_id")?),
    };
    if participants.player_one == participants.player_two {
        Err(ProjectionError::Malformed)
    } else {
        Ok(participants)
    }
}

fn text(row: &switchy_database::Row, column: &str) -> Result<String, ProjectionError> {
    row.get(column)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(ProjectionError::Malformed)
}

fn parse_u128(row: &switchy_database::Row, column: &str) -> Result<u128, ProjectionError> {
    text(row, column)?
        .parse()
        .map_err(|_| ProjectionError::Malformed)
}

fn parse_u64(row: &switchy_database::Row, column: &str) -> Result<u64, ProjectionError> {
    let value = row.get(column).ok_or(ProjectionError::Malformed)?;
    if let Some(value) = value.as_i64() {
        return Ok(u64::from_ne_bytes(value.to_ne_bytes()));
    }
    let text = value.as_str().ok_or(ProjectionError::Malformed)?;
    text.parse::<u64>()
        .or_else(|_| {
            text.parse::<i64>()
                .map(|signed| u64::from_ne_bytes(signed.to_ne_bytes()))
        })
        .map_err(|_| ProjectionError::Malformed)
}

fn unsigned_integer(row: &switchy_database::Row, column: &str) -> Result<u64, ProjectionError> {
    row.get(column)
        .and_then(|value| value.as_i64())
        .and_then(|value| u64::try_from(value).ok())
        .ok_or(ProjectionError::Malformed)
}

fn to_i64(value: u64) -> Result<i64, ProjectionError> {
    i64::try_from(value).map_err(|_| ProjectionError::Overflow)
}

fn decode_bytes(value: &str) -> Result<Vec<u8>, ProjectionError> {
    if !value.len().is_multiple_of(2) {
        return Err(ProjectionError::Malformed);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| Ok((hex(pair[0])? << 4) | hex(pair[1])?))
        .collect()
}

const fn hex(value: u8) -> Result<u8, ProjectionError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(ProjectionError::Malformed),
    }
}

const fn checksum_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_billiards::{PhysicsProfile, RackSeed, TableGeometry};
    use pwmtf_eight_ball::{MatchCommand, RulesProfile, VersionedMatchCommand};

    use super::*;

    fn encode_bytes(bytes: &[u8]) -> String {
        let mut output = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            use std::fmt::Write as _;
            write!(output, "{byte:02x}").unwrap();
        }
        output
    }

    #[test]
    fn projection_is_disposable_and_rebuilds_from_canonical_snapshot() {
        block_on(async {
            let db = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap();
            crate::migrate(&*db).await.unwrap();
            let mut state = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            state
                .apply_command(VersionedMatchCommand::new(MatchCommand::Concede {
                    player: Player::One,
                }))
                .unwrap();
            db.insert("matches")
                .value("match_id", "9")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 1_i64)
                .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
                .value("canonical_checksum", checksum_i64(state.checksum()))
                .value(
                    "match_configuration",
                    encode_bytes(&state.configuration().to_bytes()),
                )
                .value("deadline_revision", Option::<i64>::None)
                .value("deadline_player", Option::<i64>::None)
                .value("deadline_at_ms", Option::<i64>::None)
                .execute(&*db)
                .await
                .unwrap();
            let rebuilt = rebuild_match_summaries(&*db).await.unwrap();
            assert_eq!(rebuilt.len(), 1);
            assert_eq!(rebuilt[0].winner, AccountId::new(2));
            assert_eq!(rebuilt[0].reason, CompletionReason::Concession);
            db.update("matches")
                .value("canonical_revision", 0_i64)
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();
            assert!(matches!(
                rebuild_match_summaries(&*db).await,
                Err(ProjectionError::Malformed)
            ));
            db.update("matches")
                .value("canonical_revision", 1_i64)
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();
            db.delete("match_summaries").execute(&*db).await.unwrap();
            assert_eq!(match_summary(&*db, MatchId::new(9)).await.unwrap(), None);
            rebuild_match_summaries(&*db).await.unwrap();
            assert_eq!(
                match_summary(&*db, MatchId::new(9)).await.unwrap(),
                Some(rebuilt[0])
            );
            db.update("match_summaries")
                .value("canonical_revision", 0_i64)
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();
            assert!(matches!(
                match_summary(&*db, MatchId::new(9)).await,
                Err(ProjectionError::Malformed)
            ));
        });
    }
}
