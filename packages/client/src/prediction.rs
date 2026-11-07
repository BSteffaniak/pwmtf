//! Client-owned prediction, authoritative reconciliation, and interpolation.

use pwmtf_game_domain::{
    MatchCommand, MatchError, MatchState, Vector, VersionedMatchCommand, VersionedShotCommand,
};
use pwmtf_protocol::{CommandEnvelope, CommandId, ProtocolError, SnapshotEnvelope};
use thiserror::Error;

/// Predicted ball sample consumed only by presentation interpolation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BallSample {
    /// Stable canonical ball number.
    pub ball: u8,
    /// Previous authoritative/predicted horizontal micro-position.
    pub from_x: i64,
    /// Previous authoritative/predicted vertical micro-position.
    pub from_y: i64,
    /// Newly reconciled horizontal micro-position.
    pub to_x: i64,
    /// Newly reconciled vertical micro-position.
    pub to_y: i64,
}

/// Reconciliation disposition after receiving an authoritative snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reconciliation {
    /// Prediction exactly matched authoritative state.
    Confirmed,
    /// Prediction differed and presentation must interpolate to authority.
    Corrected,
    /// Snapshot advanced state without a local prediction.
    Advanced,
}

/// Browser-side prediction state. It never accepts or decides gameplay.
pub struct PredictionState {
    authoritative_revision: u64,
    authoritative: MatchState,
    predicted: MatchState,
    pending: Option<CommandId>,
    pending_playback_ticks: u32,
    interpolation: Vec<BallSample>,
}

impl PredictionState {
    /// Creates presentation state from a validated authoritative snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`PredictionError`] for unknown protocol/domain versions,
    /// malformed snapshots, or checksum mismatch.
    pub fn from_snapshot(snapshot: &SnapshotEnvelope) -> Result<Self, PredictionError> {
        let authoritative = decode_snapshot(snapshot)?;
        Ok(Self {
            authoritative_revision: snapshot.revision,
            predicted: authoritative.clone(),
            authoritative,
            pending: None,
            pending_playback_ticks: 0,
            interpolation: Vec::new(),
        })
    }

    /// Returns the last authoritative revision.
    #[must_use]
    pub const fn authoritative_revision(&self) -> u64 {
        self.authoritative_revision
    }

    /// Returns the last authoritative canonical state.
    #[must_use]
    pub const fn authoritative(&self) -> &MatchState {
        &self.authoritative
    }

    /// Returns presentation's current predicted state.
    #[must_use]
    pub const fn predicted(&self) -> &MatchState {
        &self.predicted
    }

    /// Returns whether local presentation has an unresolved predicted command.
    #[must_use]
    pub const fn has_pending_prediction(&self) -> bool {
        self.pending.is_some()
    }

    /// Returns the unresolved predicted command's identifier, if any.
    #[must_use]
    pub const fn pending_command(&self) -> Option<CommandId> {
        self.pending
    }

    /// Returns the fixed shot-playback ticks of the pending prediction, or
    /// zero without one or for a non-shot command.
    #[must_use]
    pub const fn pending_playback_ticks(&self) -> u32 {
        if self.pending.is_some() {
            self.pending_playback_ticks
        } else {
            0
        }
    }

    /// Returns interpolation samples created by the last correction.
    #[must_use]
    pub fn interpolation(&self) -> &[BallSample] {
        &self.interpolation
    }

    /// Predicts any supported state-changing command locally and creates its
    /// revision-bound transport envelope.
    ///
    /// A second command cannot be predicted until the pending command is
    /// reconciled, preventing the client from inventing an authority queue.
    ///
    /// # Errors
    ///
    /// Returns [`PredictionError::PendingCommand`] when unresolved work exists,
    /// or [`PredictionError::Domain`] when the canonical domain rejects local
    /// prediction.
    pub fn predict_command(
        &mut self,
        command_id: CommandId,
        command: VersionedMatchCommand,
    ) -> Result<CommandEnvelope, PredictionError> {
        if self.pending.is_some() {
            return Err(PredictionError::PendingCommand);
        }
        self.pending_playback_ticks = self.predicted.apply_command_timed(command)?.playback_ticks;
        self.pending = Some(command_id);
        Ok(CommandEnvelope::new(
            self.authoritative_revision,
            command_id,
            command,
        ))
    }

    /// Predicts a bounded shot locally and creates its revision-bound command.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::predict_command`].
    pub fn predict_shot(
        &mut self,
        command_id: CommandId,
        shot: VersionedShotCommand,
        called_pocket: Option<pwmtf_game_domain::PocketId>,
    ) -> Result<CommandEnvelope, PredictionError> {
        self.predict_command(
            command_id,
            VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot,
                called_pocket,
            }),
        )
    }

    /// Predicts an authoritative cue-ball placement command locally.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::predict_command`].
    pub fn predict_cue_ball_placement(
        &mut self,
        command_id: CommandId,
        position: Vector,
    ) -> Result<CommandEnvelope, PredictionError> {
        self.predict_command(
            command_id,
            VersionedMatchCommand::new(MatchCommand::PlaceCueBall { position }),
        )
    }

    /// Predicts an explicit concession command locally.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::predict_command`].
    pub fn predict_concession(
        &mut self,
        command_id: CommandId,
        player: pwmtf_game_domain::Player,
    ) -> Result<CommandEnvelope, PredictionError> {
        self.predict_command(
            command_id,
            VersionedMatchCommand::new(MatchCommand::Concede { player }),
        )
    }

    /// Reconciles a complete authoritative snapshot and creates correction
    /// interpolation samples when prediction diverged.
    ///
    /// # Errors
    ///
    /// Returns [`PredictionError`] for stale, malformed, unknown-version, or
    /// checksum-invalid snapshots.
    pub fn reconcile(
        &mut self,
        snapshot: &SnapshotEnvelope,
    ) -> Result<Reconciliation, PredictionError> {
        if snapshot.revision < self.authoritative_revision {
            return Err(PredictionError::StaleSnapshot);
        }
        let next = decode_snapshot(snapshot)?;
        if snapshot.revision == self.authoritative_revision {
            return if next.checksum() == self.authoritative.checksum() {
                Ok(Reconciliation::Advanced)
            } else {
                Err(PredictionError::RevisionConflict)
            };
        }
        let had_pending = self.pending.take().is_some();
        let disposition = if had_pending && self.predicted.checksum() == next.checksum() {
            Reconciliation::Confirmed
        } else if had_pending || self.predicted.checksum() != next.checksum() {
            Reconciliation::Corrected
        } else {
            Reconciliation::Advanced
        };
        self.interpolation = if disposition == Reconciliation::Corrected {
            interpolation_samples(&self.predicted, &next)
        } else {
            Vec::new()
        };
        self.authoritative_revision = snapshot.revision;
        self.authoritative = next.clone();
        self.predicted = next;
        Ok(disposition)
    }

    /// Drops local prediction and restores last authority after transport loss.
    pub fn abandon_prediction(&mut self) {
        self.predicted = self.authoritative.clone();
        self.pending = None;
        self.interpolation.clear();
    }
}

/// Client prediction/reconciliation failure.
#[derive(Debug, Error)]
pub enum PredictionError {
    /// Another local command still awaits authority.
    #[error("a predicted command is already pending")]
    PendingCommand,
    /// Snapshot revision moved backwards.
    #[error("authoritative snapshot is stale")]
    StaleSnapshot,
    /// Snapshot reused a revision with different canonical state.
    #[error("authoritative snapshot conflicts at the current revision")]
    RevisionConflict,
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

fn decode_snapshot(snapshot: &SnapshotEnvelope) -> Result<MatchState, PredictionError> {
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

fn interpolation_samples(from: &MatchState, to: &MatchState) -> Vec<BallSample> {
    from.table()
        .balls()
        .iter()
        .zip(to.table().balls())
        .filter(|(from, to)| from.id == to.id && from.position != to.position)
        .map(|(from, to)| BallSample {
            ball: from.id.number(),
            from_x: from.position.x.micros(),
            from_y: from.position.y.micros(),
            to_x: to.position.x.micros(),
            to_y: to.position.y.micros(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_game_domain::{
        Aim, PhysicsProfile, Player, RackSeed, RulesProfile, ShotPower, Spin, TableGeometry,
        VersionedMatchCommand,
    };
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

    fn initial() -> SnapshotEnvelope {
        let state = match_state();
        SnapshotEnvelope::new(0, state.checksum(), state.to_bytes()).unwrap()
    }

    fn server_snapshot(service: &MatchService<ImpairedJournal>) -> SnapshotEnvelope {
        service.snapshot(MATCH_ID).unwrap()
    }

    fn command(revision: u64, id: u8, command: MatchCommand) -> CommandEnvelope {
        CommandEnvelope::new(
            revision,
            CommandId::new([id; 16]),
            VersionedMatchCommand::new(command),
        )
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn complete_match_converges_under_impairment_and_process_restart() {
        block_on(async {
            let journal = ImpairedJournal::default();
            let mut server = MatchService::new(journal.clone());
            server.insert_match(MATCH_ID, PARTICIPANTS, match_state());
            let initial = server_snapshot(&server);
            let mut shooter = PredictionState::from_snapshot(&initial).unwrap();
            let mut opponent = PredictionState::from_snapshot(&initial).unwrap();

            let shot = VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(0).unwrap(),
                Spin::CENTER,
            );
            let shot_frame = shooter
                .predict_shot(CommandId::new([1; 16]), shot, None)
                .unwrap();
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

            let authoritative_one = server_snapshot(&server);
            assert_eq!(
                shooter.reconcile(&authoritative_one).unwrap(),
                Reconciliation::Confirmed
            );
            assert_eq!(
                opponent.reconcile(&authoritative_one).unwrap(),
                Reconciliation::Corrected
            );
            assert!(matches!(
                opponent.reconcile(&initial),
                Err(PredictionError::StaleSnapshot)
            ));

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
            let authoritative_two = server_snapshot(&server);

            // Reordering may deliver revision two before revision one.
            assert_eq!(
                opponent.reconcile(&authoritative_two).unwrap(),
                Reconciliation::Corrected
            );
            assert!(matches!(
                opponent.reconcile(&authoritative_one),
                Err(PredictionError::StaleSnapshot)
            ));

            // Reconnect uses a complete snapshot instead of stale presentation work.
            shooter.abandon_prediction();
            assert_eq!(
                shooter.reconcile(&authoritative_two).unwrap(),
                Reconciliation::Corrected
            );

            let mut restarted = MatchService::new(journal.clone());
            assert_eq!(
                restarted.recover_match(MATCH_ID, PARTICIPANTS).await,
                Ok(Some(2))
            );
            let after_restart = server_snapshot(&restarted);
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

            let concession = command(
                2,
                3,
                MatchCommand::Concede {
                    player: Player::One,
                },
            );
            restarted
                .apply_at(
                    MATCH_ID,
                    PARTICIPANTS.player_one,
                    concession,
                    DeadlineMillis::new(600),
                )
                .await
                .unwrap();
            let completed = server_snapshot(&restarted);
            assert!(matches!(
                MatchState::from_bytes(&completed.snapshot)
                    .unwrap()
                    .status(),
                pwmtf_game_domain::MatchStatus::Completed(_)
            ));
            assert_eq!(
                shooter.reconcile(&completed).unwrap(),
                Reconciliation::Corrected
            );
            assert_eq!(
                opponent.reconcile(&completed).unwrap(),
                Reconciliation::Corrected
            );
            assert_eq!(shooter.predicted().checksum(), completed.checksum);
            assert_eq!(opponent.predicted().checksum(), completed.checksum);
        });
    }

    #[test]
    fn concession_uses_revision_bound_prediction_path() {
        let state = MatchState::from_bytes(&initial().snapshot).unwrap();
        let snapshot = SnapshotEnvelope::new(4, state.checksum(), state.to_bytes()).unwrap();
        let mut prediction = PredictionState::from_snapshot(&snapshot).unwrap();
        let envelope = prediction
            .predict_concession(CommandId::new([11; 16]), Player::One)
            .unwrap();
        assert_eq!(envelope.expected_revision, 4);
        assert!(matches!(
            envelope.command.command,
            MatchCommand::Concede {
                player: Player::One
            }
        ));
        assert!(matches!(
            prediction.predicted().status(),
            pwmtf_game_domain::MatchStatus::Completed(_)
        ));
    }

    #[test]
    fn cue_ball_placement_uses_prediction_path() {
        let mut state = MatchState::from_bytes(&initial().snapshot).unwrap();
        state.timeout_turn().unwrap();
        let snapshot = SnapshotEnvelope::new(1, state.checksum(), state.to_bytes()).unwrap();
        let mut prediction = PredictionState::from_snapshot(&snapshot).unwrap();
        let initial_position = prediction
            .predicted()
            .table()
            .balls()
            .iter()
            .find(|ball| ball.id == pwmtf_game_domain::BallId::CUE)
            .unwrap()
            .position;
        let envelope = prediction
            .predict_cue_ball_placement(CommandId::new([9; 16]), initial_position)
            .unwrap();
        assert!(matches!(
            envelope.command.command,
            MatchCommand::PlaceCueBall { position } if position == initial_position
        ));
        assert!(!prediction.predicted().ball_in_hand());
    }

    #[test]
    fn same_revision_authority_does_not_clear_unaccepted_prediction() {
        let mut prediction = PredictionState::from_snapshot(&initial()).unwrap();
        prediction
            .predict_shot(
                CommandId::new([10; 16]),
                VersionedShotCommand::new(
                    Aim::new(0).unwrap(),
                    ShotPower::new(0).unwrap(),
                    Spin::CENTER,
                ),
                None,
            )
            .unwrap();
        assert!(prediction.has_pending_prediction());
        assert_eq!(
            prediction.reconcile(&initial()).unwrap(),
            Reconciliation::Advanced
        );
        assert!(prediction.has_pending_prediction());
    }

    #[test]
    fn divergent_authority_corrects_prediction_and_stale_snapshots_fail() {
        let mut prediction = PredictionState::from_snapshot(&initial()).unwrap();
        prediction
            .predict_shot(
                CommandId::new([8; 16]),
                VersionedShotCommand::new(
                    Aim::new(0).unwrap(),
                    ShotPower::new(1_000).unwrap(),
                    Spin::CENTER,
                ),
                None,
            )
            .unwrap();
        let mut authority = MatchState::from_bytes(&initial().snapshot).unwrap();
        authority
            .apply_command(VersionedMatchCommand::new(MatchCommand::Timeout))
            .unwrap();
        let snapshot =
            SnapshotEnvelope::new(1, authority.checksum(), authority.to_bytes()).unwrap();
        assert_eq!(
            prediction.reconcile(&snapshot).unwrap(),
            Reconciliation::Corrected
        );
        assert_eq!(prediction.predicted(), &authority);
        assert_eq!(
            prediction.reconcile(&initial()).unwrap_err().to_string(),
            PredictionError::StaleSnapshot.to_string()
        );
    }
}
