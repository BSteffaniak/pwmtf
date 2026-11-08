//! Client-owned prediction: authoritative snapshots feed a
//! [`pwmtf_sync::Replica`], which predicts the local player's one pending
//! command and replays every accepted shot.

use pwmtf_eight_ball::{MatchError, MatchState};
use pwmtf_protocol::{CommandEnvelope, CommandId, ProtocolError, SnapshotEnvelope};
use pwmtf_sync::{CommandKey, Pending, ReplicaError};
use thiserror::Error;

/// Client-side mirror of one match.
pub type MatchReplica = pwmtf_sync::Replica<MatchState>;

/// The revision-bound wire envelope for a pending command.
#[must_use]
pub const fn envelope(
    pending: &Pending<pwmtf_eight_ball::VersionedMatchCommand>,
) -> CommandEnvelope {
    CommandEnvelope::new(
        pending.expected_revision,
        CommandId::new(pending.key.0),
        pending.command,
    )
}

/// The replica key for a wire command identifier.
#[must_use]
pub const fn key(command_id: CommandId) -> CommandKey {
    CommandKey(command_id.bytes())
}

impl From<ReplicaError<MatchError>> for PredictionError {
    fn from(error: ReplicaError<MatchError>) -> Self {
        match error {
            ReplicaError::PendingCommand => Self::PendingCommand,
            ReplicaError::Ended => Self::Domain(MatchError::Completed),
            ReplicaError::Rejected(error) => Self::Domain(error),
        }
    }
}

/// Client prediction/reconciliation failure.
#[derive(Debug, Error)]
pub enum PredictionError {
    /// Another local command still awaits authority.
    #[error("a predicted command is already pending")]
    PendingCommand,
    /// Snapshot checksum does not match decoded canonical state.
    #[error("authoritative snapshot checksum is invalid")]
    Checksum,
    /// Snapshot framing/version is invalid.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Canonical snapshot or prediction failed.
    #[error(transparent)]
    Domain(#[from] MatchError),
    /// Canonical snapshot payload is malformed or unknown.
    #[error("authoritative snapshot payload is invalid")]
    Snapshot,
}

/// Decodes and validates an authoritative snapshot.
///
/// # Errors
///
/// Returns [`PredictionError`] for unknown protocol/domain versions,
/// malformed snapshots, or checksum mismatch.
pub fn decode_snapshot(snapshot: &SnapshotEnvelope) -> Result<MatchState, PredictionError> {
    if snapshot.protocol_version != pwmtf_protocol::PROTOCOL_VERSION {
        return Err(ProtocolError::UnsupportedProtocol(snapshot.protocol_version).into());
    }
    let state =
        MatchState::from_bytes(&snapshot.snapshot).map_err(|_| PredictionError::Snapshot)?;
    if state.checksum() != snapshot.checksum {
        return Err(PredictionError::Checksum);
    }
    Ok(state)
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_billiards::{
        Aim, PhysicsProfile, RackSeed, ShotPower, Spin, TableGeometry, VersionedShotCommand,
    };
    use pwmtf_eight_ball::{MatchCommand, Player, RulesProfile, VersionedMatchCommand};
    use pwmtf_server::{
        AcceptedCommand, AccountId, CommandJournal, DeadlineMillis, JournalError, MatchId,
        MatchService, Participants,
    };

    use super::*;

    #[derive(Clone, Default)]
    struct ImpairedJournal {
        records: std::sync::Arc<std::sync::Mutex<Vec<AcceptedCommand>>>,
    }

    impl ImpairedJournal {
        fn record_count(&self) -> usize {
            self.records.lock().expect("journal lock poisoned").len()
        }
    }

    impl CommandJournal for ImpairedJournal {
        async fn commit(&mut self, command: AcceptedCommand) -> Result<(), JournalError> {
            self.records.lock().map_err(|_| JournalError)?.push(command);
            Ok(())
        }

        async fn load(&self, match_id: MatchId) -> Result<Vec<AcceptedCommand>, JournalError> {
            Ok(self
                .records
                .lock()
                .map_err(|_| JournalError)?
                .iter()
                .filter(|record| record.match_id == match_id)
                .cloned()
                .collect())
        }
    }

    const MATCH_ID: MatchId = MatchId::new(91);
    const PARTICIPANTS: Participants = Participants {
        player_one: AccountId::new(1),
        player_two: AccountId::new(2),
    };

    fn match_state() -> MatchState {
        MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap()
    }

    fn replica(snapshot: &SnapshotEnvelope) -> MatchReplica {
        MatchReplica::new(snapshot.revision, decode_snapshot(snapshot).unwrap())
    }

    fn receive(replica: &mut MatchReplica, snapshot: &SnapshotEnvelope) {
        replica.receive(snapshot.revision, decode_snapshot(snapshot).unwrap(), None);
    }

    fn command(revision: u64, id: u8, command: MatchCommand) -> CommandEnvelope {
        CommandEnvelope::new(
            revision,
            CommandId::new([id; 16]),
            VersionedMatchCommand::new(command),
        )
    }

    fn shot(power: u16) -> VersionedMatchCommand {
        VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(power).unwrap(),
                Spin::CENTER,
            ),
            called_pocket: None,
        })
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn complete_match_converges_under_impairment_and_process_restart() {
        block_on(async {
            let journal = ImpairedJournal::default();
            let mut server = MatchService::new(journal.clone());
            server.insert_match(MATCH_ID, PARTICIPANTS, match_state());
            let initial = server.snapshot(MATCH_ID).unwrap();
            let mut shooter = replica(&initial);
            let mut opponent = replica(&initial);

            shooter
                .submit(key(CommandId::new([1; 16])), shot(0))
                .unwrap();
            let shot_frame = envelope(shooter.pending().unwrap());
            assert_eq!(shot_frame.expected_revision, 0);
            let accepted = server
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_one,
                    shot_frame,
                    DeadlineMillis::new(100),
                )
                .await
                .unwrap();
            assert_eq!(accepted.revision, 1);

            // Duplication is idempotent even when its acknowledgement is lost.
            let duplicate = server
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_one,
                    shot_frame,
                    DeadlineMillis::new(180),
                )
                .await
                .unwrap();
            assert!(duplicate.duplicate);
            assert_eq!(journal.record_count(), 1);

            // Without a transition description the shooter recognizes its
            // shot by resolving it locally, reproducing the server exactly.
            let authoritative_one = server.snapshot(MATCH_ID).unwrap();
            receive(&mut shooter, &authoritative_one);
            assert!(shooter.pending().is_none());
            receive(&mut opponent, &authoritative_one);
            // A delayed older snapshot is ignored.
            receive(&mut opponent, &initial);
            assert_eq!(opponent.revision(), 1);

            let timeout = command(1, 2, MatchCommand::Timeout);
            server
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_two,
                    timeout,
                    DeadlineMillis::new(250),
                )
                .await
                .unwrap();
            let authoritative_two = server.snapshot(MATCH_ID).unwrap();
            // Reordering may deliver revision two before revision one.
            receive(&mut opponent, &authoritative_two);
            receive(&mut opponent, &authoritative_one);
            assert_eq!(opponent.revision(), 2);
            receive(&mut shooter, &authoritative_two);

            let mut restarted = MatchService::new(journal.clone());
            assert_eq!(
                restarted.recover_match(MATCH_ID, PARTICIPANTS).await,
                Ok(Some(2))
            );
            let after_restart = restarted.snapshot(MATCH_ID).unwrap();
            assert_eq!(after_restart, authoritative_two);

            // Retransmission after restart remains exactly-once.
            let duplicate_after_restart = restarted
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_two,
                    timeout,
                    DeadlineMillis::new(400),
                )
                .await
                .unwrap();
            assert!(duplicate_after_restart.duplicate);
            assert_eq!(journal.record_count(), 2);
            assert_eq!(shooter.predicted().checksum(), after_restart.checksum);
            assert_eq!(opponent.predicted().checksum(), after_restart.checksum);

            shooter
                .submit(
                    key(CommandId::new([3; 16])),
                    VersionedMatchCommand::new(MatchCommand::Concede {
                        player: Player::One,
                    }),
                )
                .unwrap();
            restarted
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_one,
                    envelope(shooter.pending().unwrap()),
                    DeadlineMillis::new(600),
                )
                .await
                .unwrap();
            let completed = restarted.snapshot(MATCH_ID).unwrap();
            assert!(matches!(
                decode_snapshot(&completed).unwrap().status(),
                pwmtf_eight_ball::MatchStatus::Completed(_)
            ));
            receive(&mut shooter, &completed);
            receive(&mut opponent, &completed);
            assert!(shooter.pending().is_none());
            assert_eq!(shooter.predicted().checksum(), completed.checksum);
            assert_eq!(opponent.predicted().checksum(), completed.checksum);
        });
    }

    #[test]
    fn concession_and_placement_resolve_immediately() {
        let state = match_state();
        let mut prediction = MatchReplica::new(4, state.clone());
        let concede = VersionedMatchCommand::new(MatchCommand::Concede {
            player: Player::One,
        });
        assert_eq!(
            prediction
                .submit(key(CommandId::new([11; 16])), concede)
                .unwrap(),
            4
        );
        assert!(matches!(
            prediction.predicted().status(),
            pwmtf_eight_ball::MatchStatus::Completed(_)
        ));

        let mut state = state;
        state.timeout_turn().unwrap();
        let mut prediction = MatchReplica::new(1, state);
        let position = prediction
            .predicted()
            .table()
            .ball(pwmtf_billiards::BallId::CUE)
            .unwrap()
            .position;
        prediction
            .submit(
                key(CommandId::new([9; 16])),
                VersionedMatchCommand::new(MatchCommand::PlaceCueBall { position }),
            )
            .unwrap();
        assert!(!prediction.predicted().ball_in_hand());
    }

    #[test]
    fn same_revision_authority_does_not_clear_unaccepted_prediction() {
        let mut prediction = MatchReplica::new(0, match_state());
        prediction
            .submit(key(CommandId::new([10; 16])), shot(0))
            .unwrap();
        prediction.receive(0, match_state(), None);
        assert!(prediction.pending().is_some());
    }

    #[test]
    fn divergent_authority_is_adopted_without_resolving_the_shot() {
        let mut prediction = MatchReplica::new(0, match_state());
        prediction
            .submit(key(CommandId::new([8; 16])), shot(1_000))
            .unwrap();
        let mut authority = match_state();
        authority
            .apply_command(VersionedMatchCommand::new(MatchCommand::Timeout))
            .unwrap();
        let received = prediction.receive(1, authority.clone(), None);
        assert!(received.superseded);
        assert_eq!(prediction.predicted(), &authority);
        assert!(!prediction.has_unresolved());
    }

    #[test]
    fn snapshots_with_a_wrong_checksum_are_refused() {
        let state = match_state();
        let snapshot = SnapshotEnvelope::new(0, state.checksum() ^ 1, state.to_bytes()).unwrap();
        assert!(matches!(
            decode_snapshot(&snapshot),
            Err(PredictionError::Checksum)
        ));
    }
}
