//! Switchy accepted-command journal with atomic canonical match updates.

use std::sync::Arc;

use crate::{
    AcceptedCommand, AccountId, CommandJournal, DeadlineId, DeadlineMillis, JournalError, MatchId,
    Participants, ScheduledDeadline,
};
use pwmtf_eight_ball::{MatchCommandResult, MatchConfiguration, MatchState, Player};
use pwmtf_protocol::{CommandEnvelope, CommandId};
use switchy_database::{
    Database, DatabaseValue,
    query::{Expression as _, FilterableQuery as _, SortDirection},
};

/// Durable Switchy command journal.
///
/// Each commit inserts the immutable accepted-command record and advances the
/// canonical match row in one transaction before returning.
pub struct SwitchyCommandJournal {
    db: Arc<dyn Database>,
}

impl SwitchyCommandJournal {
    /// Loads an unplayed durable match from its revision-zero canonical row.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the row is absent, malformed, no longer at
    /// revision zero, or fails snapshot/deadline integrity validation.
    pub async fn initial_match(
        &self,
        match_id: MatchId,
    ) -> Result<
        (
            Participants,
            pwmtf_eight_ball::MatchState,
            Option<ScheduledDeadline>,
        ),
        JournalError,
    > {
        let rows = self
            .db
            .select("matches")
            .where_eq("match_id", match_id.value().to_string())
            .execute(&*self.db)
            .await
            .map_err(|_| JournalError)?;
        let [row] = rows.as_slice() else {
            return Err(JournalError);
        };
        if integer(row, "canonical_revision").map_err(|_| JournalError)? != 0 {
            return Err(JournalError);
        }
        let participants = decode_participants(row).map_err(|_| JournalError)?;
        let snapshot = decode_bytes(&text(row, "canonical_snapshot").map_err(|_| JournalError)?)
            .map_err(|_| JournalError)?;
        let state = MatchState::from_bytes(&snapshot).map_err(|_| JournalError)?;
        validate_match_configuration(row, &state.configuration()).map_err(|_| JournalError)?;
        if state.checksum() != parse_u64(row, "canonical_checksum").map_err(|_| JournalError)? {
            return Err(JournalError);
        }
        let deadline = decode_match_deadline(row).map_err(|_| JournalError)?;
        if let Some(deadline) = deadline
            && (deadline.id.revision != 0 || deadline.id.player != state.active_player())
        {
            return Err(JournalError);
        }
        Ok((participants, state, deadline))
    }

    /// Returns all durable match identifiers in stable order.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] for malformed rows or database failures.
    pub async fn match_ids(&self) -> Result<Vec<MatchId>, JournalError> {
        let rows = self
            .db
            .select("matches")
            .sort("match_id", SortDirection::Asc)
            .execute(&*self.db)
            .await
            .map_err(|_| JournalError)?;
        rows.iter()
            .map(|row| {
                parse_u128(row, "match_id")
                    .map(MatchId::new)
                    .map_err(|_| JournalError)
            })
            .collect()
    }

    /// Resolves a durable match's participants from the canonical match row.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the match is absent or malformed.
    pub async fn participants(&self, match_id: MatchId) -> Result<Participants, JournalError> {
        let rows = self
            .db
            .select("matches")
            .where_eq("match_id", match_id.value().to_string())
            .execute(&*self.db)
            .await
            .map_err(|_| JournalError)?;
        let [row] = rows.as_slice() else {
            return Err(JournalError);
        };
        decode_participants(row).map_err(|_| JournalError)
    }

    /// Loads and validates the durable canonical head for readiness checks.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the match row, accepted-command tail,
    /// snapshot, checksum, configuration, participants, revision, or deadline
    /// is absent, malformed, or internally inconsistent.
    pub async fn canonical_head(
        &self,
        match_id: MatchId,
    ) -> Result<(Participants, u64, MatchState, Option<ScheduledDeadline>), JournalError> {
        load_canonical_head(&*self.db, match_id)
            .await
            .map_err(|_| JournalError)
    }

    /// Creates a journal over an initialized shared Switchy database.
    #[must_use]
    pub fn new(db: Arc<dyn Database>) -> Self {
        Self { db }
    }
}

impl CommandJournal for SwitchyCommandJournal {
    async fn commit(&mut self, command: AcceptedCommand) -> Result<(), JournalError> {
        commit_command(&*self.db, &command)
            .await
            .map_err(|_| JournalError)
    }

    async fn load(&self, match_id: MatchId) -> Result<Vec<AcceptedCommand>, JournalError> {
        load_commands(&*self.db, match_id)
            .await
            .map_err(|_| JournalError)
    }
}

async fn commit_command(
    db: &dyn Database,
    command: &AcceptedCommand,
) -> Result<(), CommandStoreError> {
    let envelope =
        CommandEnvelope::from_bytes(&command.frame).map_err(|_| CommandStoreError::Malformed)?;
    if envelope.command_id != command.command_id
        || envelope.expected_revision.checked_add(1) != Some(command.revision)
    {
        return Err(CommandStoreError::Malformed);
    }
    let state =
        MatchState::from_bytes(&command.snapshot).map_err(|_| CommandStoreError::Malformed)?;
    if state.checksum() != command.checksum {
        return Err(CommandStoreError::Malformed);
    }
    validate_deadline(command.deadline, command.revision, &state)?;

    let deadline_revision = command
        .deadline
        .map(|value| to_i64(value.id.revision))
        .transpose()?;
    let deadline_due_at = command
        .deadline
        .map(|value| to_i64(value.due_at.value()))
        .transpose()?;
    // Switchy's Turso transactions start DEFERRED on a new connection. A
    // concurrent writer can invalidate a read snapshot before the first
    // insert; only that pre-write failure is safe to retry from a fresh tx.
    // Never retry the update or commit: the durability outcome may be unknown.
    for attempt in 0..5 {
        match commit_command_once(
            db,
            command,
            &state,
            &envelope,
            deadline_revision,
            deadline_due_at,
        )
        .await
        {
            Err(CommandStoreError::PreWriteBusy) if attempt < 4 => {
                tokio::time::sleep(std::time::Duration::from_millis(20 * (attempt + 1))).await;
            }
            Err(CommandStoreError::PreWriteBusy) => return Err(CommandStoreError::Database),
            result => return result,
        }
    }
    unreachable!("bounded retry loop returns on its final attempt")
}

fn pre_write_error(error: switchy_database::DatabaseError) -> CommandStoreError {
    let busy = matches!(
        &error,
        switchy_database::DatabaseError::Turso(switchy_database::turso::TursoDatabaseError::Turso(
            turso::Error::BusySnapshot(_) | turso::Error::Busy(_)
        ))
    );
    if busy {
        CommandStoreError::PreWriteBusy
    } else {
        error.into()
    }
}

fn pre_write_snapshot_error(error: switchy_database::DatabaseError) -> CommandStoreError {
    if matches!(
        &error,
        switchy_database::DatabaseError::Turso(switchy_database::turso::TursoDatabaseError::Turso(
            turso::Error::Busy(_) | turso::Error::BusySnapshot(_)
        ))
    ) {
        CommandStoreError::PreWriteBusy
    } else {
        error.into()
    }
}

async fn commit_command_once(
    db: &dyn Database,
    command: &AcceptedCommand,
    state: &MatchState,
    envelope: &CommandEnvelope,
    deadline_revision: Option<i64>,
    deadline_due_at: Option<i64>,
) -> Result<(), CommandStoreError> {
    let tx = db.begin_transaction().await.map_err(pre_write_error)?;
    let matches = tx
        .select("matches")
        .where_eq("match_id", command.match_id.value().to_string())
        .execute(&*tx)
        .await
        .map_err(pre_write_error)?;
    let row = exactly_one(&matches)?;
    validate_match_configuration(row, &state.configuration())?;
    let current_revision = integer(row, "canonical_revision")?;
    if current_revision != to_i64(envelope.expected_revision)? {
        return Err(CommandStoreError::StaleRevision);
    }

    let id = command_id(command.match_id, command.command_id);
    tx.insert("accepted_commands")
        .value("accepted_command_id", id)
        .value("match_id", command.match_id.value().to_string())
        .value("revision", to_i64(command.revision)?)
        .value("command_id", encode_bytes(&command.command_id.bytes()))
        .value("actor_id", command.actor.value().to_string())
        .value("frame", encode_bytes(&command.frame))
        .value("result_payload", encode_bytes(&command.result.to_bytes()))
        .value("snapshot", encode_bytes(&command.snapshot))
        .value("checksum", checksum_i64(command.checksum))
        .value(
            "deadline_revision",
            command.deadline.map(|value| value.id.revision.to_string()),
        )
        .value(
            "deadline_player",
            command
                .deadline
                .map(|value| encode_player(value.id.player).to_string()),
        )
        .value(
            "deadline_at_ms",
            command
                .deadline
                .map(|value| value.due_at.value().to_string()),
        )
        .execute(&*tx)
        .await
        .map_err(pre_write_snapshot_error)?;

    let updated = tx
        .update("matches")
        .value("canonical_revision", to_i64(command.revision)?)
        .value("canonical_snapshot", encode_bytes(&command.snapshot))
        .value("canonical_checksum", checksum_i64(command.checksum))
        .value(
            "deadline_revision",
            deadline_revision.map_or(DatabaseValue::Null, DatabaseValue::Int64),
        )
        .value(
            "deadline_player",
            command.deadline.map_or(DatabaseValue::Null, |value| {
                DatabaseValue::Int64(encode_player(value.id.player))
            }),
        )
        .value(
            "deadline_at_ms",
            deadline_due_at.map_or(DatabaseValue::Null, DatabaseValue::Int64),
        )
        .where_eq("match_id", command.match_id.value().to_string())
        .where_eq("canonical_revision", current_revision)
        .execute(&*tx)
        .await?;
    if updated.len() != 1 {
        return Err(CommandStoreError::StaleRevision);
    }
    tx.commit().await?;
    Ok(())
}

async fn load_canonical_head(
    db: &dyn Database,
    match_id: MatchId,
) -> Result<(Participants, u64, MatchState, Option<ScheduledDeadline>), CommandStoreError> {
    let rows = db
        .select("matches")
        .where_eq("match_id", match_id.value().to_string())
        .execute(db)
        .await?;
    let row = exactly_one(&rows)?;
    let participants = decode_participants(row)?;
    let revision = unsigned_integer(row, "canonical_revision")?;
    let state = MatchState::from_bytes(&decode_bytes(&text(row, "canonical_snapshot")?)?)
        .map_err(|_| CommandStoreError::Malformed)?;
    if state.checksum() != parse_u64(row, "canonical_checksum")?
        || state.configuration() != pinned_match_configuration(row)?
    {
        return Err(CommandStoreError::Malformed);
    }
    let deadline = decode_match_deadline(row)?;
    validate_deadline(deadline, revision, &state)?;
    if revision == 0 {
        let command_rows = db
            .select("accepted_commands")
            .where_eq("match_id", match_id.value().to_string())
            .execute(db)
            .await?;
        if !command_rows.is_empty() {
            return Err(CommandStoreError::Malformed);
        }
    } else {
        let commands = load_commands(db, match_id).await?;
        let Some(last) = commands.last() else {
            return Err(CommandStoreError::Malformed);
        };
        if last.revision != revision
            || last.snapshot != state.to_bytes()
            || last.checksum != state.checksum()
            || last.deadline != deadline
        {
            return Err(CommandStoreError::Malformed);
        }
    }
    Ok((participants, revision, state, deadline))
}

async fn load_commands(
    db: &dyn Database,
    match_id: MatchId,
) -> Result<Vec<AcceptedCommand>, CommandStoreError> {
    let matches = db
        .select("matches")
        .where_eq("match_id", match_id.value().to_string())
        .execute(db)
        .await?;
    let configuration = pinned_match_configuration(exactly_one(&matches)?)?;
    let rows = db
        .select("accepted_commands")
        .where_eq("match_id", match_id.value().to_string())
        .sort("revision", SortDirection::Asc)
        .execute(db)
        .await?;
    let records = rows
        .iter()
        .map(|row| decode_command(row, match_id))
        .collect::<Result<Vec<_>, _>>()?;
    for record in &records {
        let state =
            MatchState::from_bytes(&record.snapshot).map_err(|_| CommandStoreError::Malformed)?;
        if state.configuration() != configuration {
            return Err(CommandStoreError::Malformed);
        }
    }
    Ok(records)
}

fn decode_command(
    row: &switchy_database::Row,
    expected_match: MatchId,
) -> Result<AcceptedCommand, CommandStoreError> {
    let match_id = MatchId::new(parse_u128(row, "match_id")?);
    if match_id != expected_match {
        return Err(CommandStoreError::Malformed);
    }
    let frame = decode_bytes(&text(row, "frame")?)?;
    let envelope = CommandEnvelope::from_bytes(&frame).map_err(|_| CommandStoreError::Malformed)?;
    let command_id = CommandId::new(decode_array(&text(row, "command_id")?)?);
    if command_id != envelope.command_id {
        return Err(CommandStoreError::Malformed);
    }
    let revision = unsigned_integer(row, "revision")?;
    if envelope.expected_revision.checked_add(1) != Some(revision) {
        return Err(CommandStoreError::Malformed);
    }
    let snapshot = decode_bytes(&text(row, "snapshot")?)?;
    let checksum = parse_u64(row, "checksum")?;
    let state = MatchState::from_bytes(&snapshot).map_err(|_| CommandStoreError::Malformed)?;
    if state.checksum() != checksum {
        return Err(CommandStoreError::Malformed);
    }
    let result = MatchCommandResult::from_bytes(&decode_bytes(&text(row, "result_payload")?)?)
        .map_err(|_| CommandStoreError::Malformed)?;
    let deadline = decode_deadline(row)?;
    validate_deadline(deadline, revision, &state)?;
    Ok(AcceptedCommand {
        match_id,
        revision,
        command_id,
        actor: AccountId::new(parse_u128(row, "actor_id")?),
        frame,
        result,
        snapshot,
        checksum,
        deadline,
    })
}

fn decode_participants(row: &switchy_database::Row) -> Result<Participants, CommandStoreError> {
    let participants = Participants {
        player_one: AccountId::new(parse_u128(row, "player_one_id")?),
        player_two: AccountId::new(parse_u128(row, "player_two_id")?),
    };
    if participants.player_one == participants.player_two {
        Err(CommandStoreError::Malformed)
    } else {
        Ok(participants)
    }
}

fn pinned_match_configuration(
    row: &switchy_database::Row,
) -> Result<MatchConfiguration, CommandStoreError> {
    MatchConfiguration::from_bytes(&decode_bytes(&text(row, "match_configuration")?)?)
        .map_err(|_| CommandStoreError::Malformed)
}

fn validate_match_configuration(
    row: &switchy_database::Row,
    expected: &MatchConfiguration,
) -> Result<(), CommandStoreError> {
    let stored = pinned_match_configuration(row)?;
    if stored == *expected {
        Ok(())
    } else {
        Err(CommandStoreError::Malformed)
    }
}

fn validate_deadline(
    deadline: Option<ScheduledDeadline>,
    revision: u64,
    state: &pwmtf_eight_ball::MatchState,
) -> Result<(), CommandStoreError> {
    match (state.status(), deadline) {
        (pwmtf_eight_ball::MatchStatus::InProgress, Some(value))
            if value.id.revision == revision && value.id.player == state.active_player() =>
        {
            Ok(())
        }
        (pwmtf_eight_ball::MatchStatus::Completed(_), None) => Ok(()),
        _ => Err(CommandStoreError::Malformed),
    }
}

fn decode_match_deadline(
    row: &switchy_database::Row,
) -> Result<Option<ScheduledDeadline>, CommandStoreError> {
    let revision = row_u64(row, "deadline_revision")?;
    let player = row_u64(row, "deadline_player")?;
    let due_at = row_u64(row, "deadline_at_ms")?;
    match (revision, player, due_at) {
        (None, None, None) => Ok(None),
        (Some(revision), Some(player), Some(due_at)) => Ok(Some(ScheduledDeadline {
            id: DeadlineId {
                revision,
                player: decode_player(player)?,
            },
            due_at: DeadlineMillis::new(due_at),
        })),
        _ => Err(CommandStoreError::Malformed),
    }
}

fn decode_deadline(
    row: &switchy_database::Row,
) -> Result<Option<ScheduledDeadline>, CommandStoreError> {
    let revision = row_u64(row, "deadline_revision")?;
    let player = row_u64(row, "deadline_player")?;
    let due_at = row_u64(row, "deadline_at_ms")?;
    match (revision, player, due_at) {
        (None, None, None) => Ok(None),
        (Some(revision), Some(player), Some(due_at)) => Ok(Some(ScheduledDeadline {
            id: DeadlineId {
                revision,
                player: decode_player(player)?,
            },
            due_at: DeadlineMillis::new(due_at),
        })),
        _ => Err(CommandStoreError::Malformed),
    }
}

/// Durable command persistence failure without retaining backend details.
#[derive(Debug)]
enum CommandStoreError {
    Database,
    PreWriteBusy,
    Malformed,
    StaleRevision,
    Overflow,
}

impl From<switchy_database::DatabaseError> for CommandStoreError {
    fn from(_: switchy_database::DatabaseError) -> Self {
        Self::Database
    }
}

fn exactly_one(
    rows: &[switchy_database::Row],
) -> Result<&switchy_database::Row, CommandStoreError> {
    match rows {
        [row] => Ok(row),
        _ => Err(CommandStoreError::Malformed),
    }
}

fn integer(row: &switchy_database::Row, column: &str) -> Result<i64, CommandStoreError> {
    row.get(column)
        .and_then(|value| value.as_i64())
        .ok_or(CommandStoreError::Malformed)
}

fn unsigned_integer(row: &switchy_database::Row, column: &str) -> Result<u64, CommandStoreError> {
    u64::try_from(integer(row, column)?).map_err(|_| CommandStoreError::Malformed)
}

fn text(row: &switchy_database::Row, column: &str) -> Result<String, CommandStoreError> {
    row.get(column)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or(CommandStoreError::Malformed)
}

fn row_u64(row: &switchy_database::Row, column: &str) -> Result<Option<u64>, CommandStoreError> {
    let value = row.get(column).ok_or(CommandStoreError::Malformed)?;
    if value.is_null() {
        return Ok(None);
    }
    if let Some(value) = value.as_i64() {
        return u64::try_from(value)
            .map(Some)
            .map_err(|_| CommandStoreError::Malformed);
    }
    if let Some(value) = value.as_str() {
        return parse_text_u64(value).map(Some);
    }
    Err(CommandStoreError::Malformed)
}

fn parse_u128(row: &switchy_database::Row, column: &str) -> Result<u128, CommandStoreError> {
    text(row, column)?
        .parse()
        .map_err(|_| CommandStoreError::Malformed)
}

fn parse_u64(row: &switchy_database::Row, column: &str) -> Result<u64, CommandStoreError> {
    let value = row.get(column).ok_or(CommandStoreError::Malformed)?;
    if let Some(value) = value.as_i64() {
        return Ok(u64::from_ne_bytes(value.to_ne_bytes()));
    }
    value
        .as_str()
        .ok_or(CommandStoreError::Malformed)
        .and_then(parse_text_u64)
}

fn parse_text_u64(value: &str) -> Result<u64, CommandStoreError> {
    value.parse().map_err(|_| CommandStoreError::Malformed)
}

fn to_i64(value: u64) -> Result<i64, CommandStoreError> {
    i64::try_from(value).map_err(|_| CommandStoreError::Overflow)
}

const fn encode_player(player: Player) -> i64 {
    match player {
        Player::One => 1,
        Player::Two => 2,
    }
}

const fn decode_player(value: u64) -> Result<Player, CommandStoreError> {
    match value {
        1 => Ok(Player::One),
        2 => Ok(Player::Two),
        _ => Err(CommandStoreError::Malformed),
    }
}

fn command_id(match_id: MatchId, command_id: CommandId) -> String {
    format!("{}:{}", match_id.value(), encode_bytes(&command_id.bytes()))
}

fn encode_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(output, "{byte:02x}").expect("writing to String cannot fail");
    }
    output
}

fn decode_array(value: &str) -> Result<[u8; 16], CommandStoreError> {
    decode_bytes(value)?
        .try_into()
        .map_err(|_| CommandStoreError::Malformed)
}

fn decode_bytes(value: &str) -> Result<Vec<u8>, CommandStoreError> {
    if !value.len().is_multiple_of(2) {
        return Err(CommandStoreError::Malformed);
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = hex(pair[0])?;
            let low = hex(pair[1])?;
            Ok((high << 4) | low)
        })
        .collect()
}

const fn hex(value: u8) -> Result<u8, CommandStoreError> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        _ => Err(CommandStoreError::Malformed),
    }
}

const fn checksum_i64(value: u64) -> i64 {
    i64::from_ne_bytes(value.to_ne_bytes())
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_billiards::{PhysicsProfile, RackSeed, TableGeometry};
    use pwmtf_eight_ball::{MatchCommand, MatchState, RulesProfile, VersionedMatchCommand};

    use super::*;

    #[test]
    fn pre_write_contention_is_retryable_but_non_contention_is_not() {
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
            CommandStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(stale()),
            CommandStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(busy()),
            CommandStoreError::PreWriteBusy
        ));
        assert!(matches!(
            pre_write_snapshot_error(DatabaseError::NoRow),
            CommandStoreError::Database
        ));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn pinned_configuration_rejects_mutated_match_rows_and_history() {
        block_on(async {
            let db: Arc<dyn Database> = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap()
                .into();
            crate::migrate(&*db).await.unwrap();
            let state = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            db.insert("matches")
                .value("match_id", "9")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 0_i64)
                .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
                .value("canonical_checksum", checksum_i64(state.checksum()))
                .value(
                    "match_configuration",
                    encode_bytes(&state.configuration().to_bytes()),
                )
                .value("deadline_revision", DatabaseValue::Null)
                .value("deadline_player", DatabaseValue::Null)
                .value("deadline_at_ms", DatabaseValue::Null)
                .execute(&*db)
                .await
                .unwrap();
            let alternate = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(99),
                Player::One,
            )
            .unwrap();
            db.update("matches")
                .value(
                    "match_configuration",
                    encode_bytes(&alternate.configuration().to_bytes()),
                )
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();
            let journal = SwitchyCommandJournal::new(Arc::clone(&db));
            assert_eq!(
                journal.initial_match(MatchId::new(9)).await,
                Err(JournalError)
            );
            db.update("matches")
                .value(
                    "match_configuration",
                    encode_bytes(&state.configuration().to_bytes()),
                )
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();
            let participants = crate::Participants {
                player_one: AccountId::new(1),
                player_two: AccountId::new(2),
            };
            let command = CommandEnvelope::new(
                0,
                CommandId::new([7; 16]),
                VersionedMatchCommand::new(MatchCommand::Timeout),
            );
            let mut service = crate::MatchService::new(SwitchyCommandJournal::new(Arc::clone(&db)));
            service.insert_match(MatchId::new(9), participants, state);
            service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    command,
                    DeadlineMillis::new(100),
                )
                .await
                .unwrap();

            let alternate = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(99),
                Player::One,
            )
            .unwrap();
            db.update("matches")
                .value(
                    "match_configuration",
                    encode_bytes(&alternate.configuration().to_bytes()),
                )
                .where_eq("match_id", "9")
                .execute(&*db)
                .await
                .unwrap();

            let journal = SwitchyCommandJournal::new(Arc::clone(&db));
            assert_eq!(journal.load(MatchId::new(9)).await, Err(JournalError));
        });
    }

    #[test]
    fn canonical_head_rejects_a_durable_row_diverging_from_its_command_tail() {
        block_on(async {
            let db: Arc<dyn Database> = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap()
                .into();
            crate::migrate(&*db).await.unwrap();
            let state = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            db.insert("matches")
                .value("match_id", "19")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 0_i64)
                .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
                .value("canonical_checksum", checksum_i64(state.checksum()))
                .value(
                    "match_configuration",
                    encode_bytes(&state.configuration().to_bytes()),
                )
                .value("deadline_revision", 0_i64)
                .value("deadline_player", 1_i64)
                .value("deadline_at_ms", 100_i64)
                .execute(&*db)
                .await
                .unwrap();
            let journal = SwitchyCommandJournal::new(Arc::clone(&db));
            assert_eq!(journal.canonical_head(MatchId::new(19)).await.unwrap().1, 0);

            db.update("matches")
                .value("canonical_revision", 1_i64)
                .where_eq("match_id", "19")
                .execute(&*db)
                .await
                .unwrap();
            assert!(journal.canonical_head(MatchId::new(19)).await.is_err());
        });
    }

    #[test]
    fn switchy_journal_commits_before_ack_and_recovers_idempotency_and_deadline() {
        block_on(async {
            let db: Arc<dyn Database> = switchy_database_connection::builder()
                .turso()
                .with_in_memory()
                .build()
                .await
                .unwrap()
                .into();
            crate::migrate(&*db).await.unwrap();
            let state = MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap();
            db.insert("matches")
                .value("match_id", "9")
                .value("player_one_id", "1")
                .value("player_two_id", "2")
                .value("canonical_revision", 0_i64)
                .value("canonical_snapshot", encode_bytes(&state.to_bytes()))
                .value("canonical_checksum", checksum_i64(state.checksum()))
                .value(
                    "match_configuration",
                    encode_bytes(&state.configuration().to_bytes()),
                )
                .value("deadline_revision", DatabaseValue::Null)
                .value("deadline_player", DatabaseValue::Null)
                .value("deadline_at_ms", DatabaseValue::Null)
                .execute(&*db)
                .await
                .unwrap();

            let participants = crate::Participants {
                player_one: AccountId::new(1),
                player_two: AccountId::new(2),
            };
            let command = CommandEnvelope::new(
                0,
                CommandId::new([7; 16]),
                VersionedMatchCommand::new(MatchCommand::Timeout),
            );
            let mut service = crate::MatchService::new(SwitchyCommandJournal::new(Arc::clone(&db)));
            service.insert_match(MatchId::new(9), participants, state);
            let accepted = service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    command,
                    DeadlineMillis::new(100),
                )
                .await
                .unwrap();
            assert_eq!(accepted.revision, 1);
            assert_eq!(
                db.select("accepted_commands")
                    .execute(&*db)
                    .await
                    .unwrap()
                    .len(),
                1
            );

            let loaded = load_commands(&*db, MatchId::new(9)).await;
            assert!(loaded.is_ok(), "loaded records: {loaded:?}");
            let journal = SwitchyCommandJournal::new(Arc::clone(&db));
            let mut recovered = crate::MatchService::new(journal);
            assert_eq!(
                recovered.recover_match(MatchId::new(9), participants).await,
                Ok(Some(1))
            );
            assert_eq!(recovered.deadline(MatchId::new(9)), accepted.deadline);
            let duplicate = recovered
                .apply_for_test(MatchId::new(9), AccountId::new(1), command)
                .await
                .unwrap();
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.revision, accepted.revision);
            assert_eq!(
                db.select("accepted_commands")
                    .execute(&*db)
                    .await
                    .unwrap()
                    .len(),
                1
            );
        });
    }
}
