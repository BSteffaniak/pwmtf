//! Browser WebSocket lifecycle state independent of JavaScript/Bevy authority.

use pwmtf_protocol::{CommandEnvelope, PROTOCOL_VERSION, ProtocolError, SnapshotEnvelope};
use thiserror::Error;

use crate::prediction::{PredictionError, PredictionState, Reconciliation};

/// Browser transport lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionStatus {
    /// No live socket exists.
    Disconnected,
    /// Browser is establishing a socket.
    Connecting,
    /// Protocol negotiation is pending.
    Negotiating,
    /// Socket is authenticated, authorized, and initialized by a snapshot.
    Ready,
    /// Connection failed and should use bounded retry delay.
    Backoff,
}

/// Client-owned WebSocket lifecycle and authoritative snapshot ingestion.
pub struct BrowserTransport {
    status: ConnectionStatus,
    protocol_negotiated: bool,
    retry_attempt: u8,
    prediction: Option<PredictionState>,
    rejected_command: bool,
    local_player: Option<pwmtf_game_domain::Player>,
}

impl Default for BrowserTransport {
    fn default() -> Self {
        Self {
            status: ConnectionStatus::Disconnected,
            protocol_negotiated: false,
            retry_attempt: 0,
            prediction: None,
            rejected_command: false,
            local_player: None,
        }
    }
}

impl BrowserTransport {
    /// Returns current transport lifecycle.
    #[must_use]
    pub const fn status(&self) -> ConnectionStatus {
        self.status
    }

    /// Returns current canonical/predicted client state after initialization.
    #[must_use]
    pub const fn prediction(&self) -> Option<&PredictionState> {
        self.prediction.as_ref()
    }

    /// Returns the latest authoritative canonical match state.
    #[must_use]
    pub fn authoritative_state(&self) -> Option<&pwmtf_game_domain::MatchState> {
        self.prediction.as_ref().map(PredictionState::authoritative)
    }

    /// Returns the authenticated local seat, if established by the browser session.
    #[must_use]
    pub const fn local_player(&self) -> Option<pwmtf_game_domain::Player> {
        self.local_player
    }

    /// Returns whether one authoritative revision has already been ingested.
    #[must_use]
    pub fn has_revision(&self, revision: u64) -> bool {
        self.prediction.as_ref().is_some_and(|prediction| {
            prediction.authoritative_revision() == revision && !prediction.has_pending_prediction()
        })
    }

    /// Begins a new browser connection attempt.
    pub const fn connecting(&mut self) {
        self.status = ConnectionStatus::Connecting;
        self.protocol_negotiated = false;
        self.rejected_command = false;
    }

    /// Marks the browser socket open and returns the bounded negotiation offer.
    #[must_use]
    pub fn opened(&mut self) -> String {
        self.status = ConnectionStatus::Negotiating;
        PROTOCOL_VERSION.to_string()
    }

    /// Accepts only the exact supported protocol negotiation response.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::Negotiation`] for malformed or unsupported responses.
    pub fn negotiated(&mut self, response: &str) -> Result<(), TransportClientError> {
        if self.status != ConnectionStatus::Negotiating {
            return Err(TransportClientError::NotReady);
        }
        let version = response.parse::<u16>().map_err(|_| {
            self.disconnected();
            TransportClientError::Negotiation
        })?;
        if version != PROTOCOL_VERSION {
            self.disconnected();
            return Err(TransportClientError::Negotiation);
        }
        self.protocol_negotiated = true;
        self.retry_attempt = 0;
        Ok(())
    }

    /// Decodes an initial/reconnect snapshot or reconciles an update.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError`] for malformed framing, checksum/version
    /// rejection, or stale authoritative data.
    pub fn receive_snapshot(
        &mut self,
        bytes: &[u8],
    ) -> Result<Option<Reconciliation>, TransportClientError> {
        if !self.protocol_negotiated
            || !matches!(
                self.status,
                ConnectionStatus::Negotiating | ConnectionStatus::Ready
            )
        {
            return Err(TransportClientError::NotReady);
        }
        let snapshot = SnapshotEnvelope::from_bytes(bytes).map_err(|error| {
            self.disconnected();
            TransportClientError::Protocol(error)
        })?;
        if self.has_revision(snapshot.revision) {
            // A reconnect's initial snapshot may match the last accepted revision.
            // It still completes negotiation for the replacement socket.
            self.retry_attempt = 0;
            self.status = ConnectionStatus::Ready;
            return Ok(None);
        }
        let reconciliation = if let Some(prediction) = &mut self.prediction {
            prediction.reconcile(&snapshot).map(Some)
        } else {
            PredictionState::from_snapshot(&snapshot).map(|prediction| {
                self.prediction = Some(prediction);
                None
            })
        };
        let disposition = reconciliation.map_err(|error| {
            self.disconnected();
            TransportClientError::Prediction(error)
        })?;
        self.retry_attempt = 0;
        self.status = ConnectionStatus::Ready;
        Ok(disposition)
    }

    /// Sets the participant seat derived from authenticated durable membership.
    pub const fn set_local_player(&mut self, player: pwmtf_game_domain::Player) {
        self.local_player = Some(player);
    }

    /// Returns whether presentation has one unresolved predicted command.
    #[must_use]
    pub fn has_pending_prediction(&self) -> bool {
        self.prediction
            .as_ref()
            .is_some_and(PredictionState::has_pending_prediction)
    }

    /// Returns whether the local participant may submit a shot or placement.
    #[must_use]
    pub fn accepts_active_player_command(&self) -> bool {
        self.accepts_gameplay_commands()
            && !self.has_pending_prediction()
            && self.local_player.is_some_and(|player| {
                self.authoritative_state()
                    .is_some_and(|state| state.active_player() == player)
            })
    }

    /// Returns whether the authoritative match accepts live gameplay commands.
    #[must_use]
    pub fn accepts_gameplay_commands(&self) -> bool {
        self.status == ConnectionStatus::Ready
            && self.authoritative_state().is_some_and(|state| {
                matches!(state.status(), pwmtf_game_domain::MatchStatus::InProgress)
            })
    }

    /// Creates and locally predicts one cue-ball placement command.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] unless initialization
    /// completed, or a prediction/domain failure for invalid local state.
    pub fn predict_cue_ball_placement(
        &mut self,
        command_id: pwmtf_protocol::CommandId,
        position: pwmtf_game_domain::Vector,
    ) -> Result<CommandEnvelope, TransportClientError> {
        if !self.accepts_active_player_command() {
            return Err(TransportClientError::WrongTurn);
        }
        self.prediction
            .as_mut()
            .ok_or(TransportClientError::NotReady)?
            .predict_cue_ball_placement(command_id, position)
            .map_err(Into::into)
    }

    /// Creates and locally predicts one bounded shot command.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] unless initialization
    /// completed, or a prediction/domain failure for invalid local state.
    pub fn predict_shot(
        &mut self,
        command_id: pwmtf_protocol::CommandId,
        shot: pwmtf_game_domain::VersionedShotCommand,
        called_pocket: Option<pwmtf_game_domain::PocketId>,
    ) -> Result<CommandEnvelope, TransportClientError> {
        if !self.accepts_active_player_command() {
            return Err(TransportClientError::WrongTurn);
        }
        self.prediction
            .as_mut()
            .ok_or(TransportClientError::NotReady)?
            .predict_shot(command_id, shot, called_pocket)
            .map_err(Into::into)
    }

    /// Creates and locally predicts one concession command.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] unless initialization
    /// completed, or a prediction/domain failure for invalid local state.
    pub fn predict_concession(
        &mut self,
        command_id: pwmtf_protocol::CommandId,
        player: pwmtf_game_domain::Player,
    ) -> Result<CommandEnvelope, TransportClientError> {
        if !self.accepts_gameplay_commands() {
            return Err(TransportClientError::NotReady);
        }
        self.prediction
            .as_mut()
            .ok_or(TransportClientError::NotReady)?
            .predict_concession(command_id, player)
            .map_err(Into::into)
    }

    /// Marks the current predicted command rejected without treating an
    /// authoritative application rejection as a transport failure.
    pub fn command_rejected(&mut self) {
        if let Some(prediction) = &mut self.prediction {
            prediction.abandon_prediction();
        }
        self.rejected_command = true;
    }

    /// Returns and clears whether the latest command was rejected.
    pub const fn take_command_rejected(&mut self) -> bool {
        let rejected = self.rejected_command;
        self.rejected_command = false;
        rejected
    }

    /// Cancels local prediction after a lifecycle loss and enters retry backoff.
    pub fn disconnected(&mut self) {
        if let Some(prediction) = &mut self.prediction {
            prediction.abandon_prediction();
        }
        if self.status != ConnectionStatus::Backoff {
            self.retry_attempt = self.retry_attempt.saturating_add(1).min(8);
        }
        self.status = ConnectionStatus::Backoff;
        self.protocol_negotiated = false;
        self.rejected_command = false;
    }

    /// Returns bounded exponential retry delay, capped at 30 seconds.
    #[must_use]
    pub fn retry_delay_ms(&self) -> u64 {
        250_u64
            .saturating_mul(1_u64 << self.retry_attempt.min(7))
            .min(30_000)
    }
}

/// Browser transport lifecycle failure.
#[derive(Debug, Error)]
pub enum TransportClientError {
    /// Protocol negotiation failed closed.
    #[error("WebSocket protocol negotiation failed")]
    Negotiation,
    /// Command was attempted before authoritative initialization.
    #[error("WebSocket is not ready")]
    NotReady,
    /// Command was attempted when the local participant was not active.
    #[error("local participant is not the active player")]
    WrongTurn,
    /// Wire frame was malformed or incompatible.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Snapshot/prediction reconciliation failed.
    #[error(transparent)]
    Prediction(#[from] PredictionError),
}

#[cfg(test)]
mod tests {
    use pwmtf_game_domain::{
        MatchState, PhysicsProfile, Player, RackSeed, RulesProfile, TableGeometry,
    };

    use super::*;

    fn snapshot(revision: u64) -> Vec<u8> {
        let state = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        SnapshotEnvelope::new(revision, state.checksum(), state.to_bytes())
            .unwrap()
            .to_bytes()
    }

    #[test]
    fn lifecycle_negotiates_initializes_and_backs_off() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        assert_eq!(transport.opened(), PROTOCOL_VERSION.to_string());
        assert_eq!(transport.status(), ConnectionStatus::Negotiating);
        transport.negotiated("1").unwrap();
        assert_eq!(transport.status(), ConnectionStatus::Negotiating);
        assert_eq!(transport.receive_snapshot(&snapshot(0)).unwrap(), None);
        assert_eq!(transport.status(), ConnectionStatus::Ready);
        assert!(transport.accepts_gameplay_commands());
        transport.disconnected();
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
        assert_eq!(transport.retry_delay_ms(), 500);
        transport.disconnected();
        assert_eq!(transport.retry_delay_ms(), 500);
    }

    #[test]
    fn reconnect_at_same_revision_completes_negotiation() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        transport.receive_snapshot(&snapshot(0)).unwrap();
        transport.disconnected();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        assert_eq!(transport.status(), ConnectionStatus::Negotiating);
        assert_eq!(transport.receive_snapshot(&snapshot(0)).unwrap(), None);
        assert_eq!(transport.status(), ConnectionStatus::Ready);
        assert!(transport.accepts_gameplay_commands());
    }

    #[test]
    fn ready_transport_predicts_cue_ball_placement() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        transport.receive_snapshot(&snapshot(0)).unwrap();
        transport.set_local_player(Player::Two);
        let mut state =
            MatchState::from_bytes(&SnapshotEnvelope::from_bytes(&snapshot(0)).unwrap().snapshot)
                .unwrap();
        state.timeout_turn().unwrap();
        let advanced = SnapshotEnvelope::new(1, state.checksum(), state.to_bytes())
            .unwrap()
            .to_bytes();
        transport.receive_snapshot(&advanced).unwrap();
        let cue_position = state
            .table()
            .balls()
            .iter()
            .find(|ball| ball.id == pwmtf_game_domain::BallId::CUE)
            .unwrap()
            .position;
        let command = transport
            .predict_cue_ball_placement(pwmtf_protocol::CommandId::new([3; 16]), cue_position)
            .unwrap();
        assert_eq!(command.expected_revision, 1);
        assert!(!transport.prediction().unwrap().predicted().ball_in_hand());
    }

    #[test]
    fn inactive_participant_cannot_predict_active_player_commands() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        transport.receive_snapshot(&snapshot(0)).unwrap();
        transport.set_local_player(Player::Two);
        assert!(transport.accepts_gameplay_commands());
        assert!(!transport.accepts_active_player_command());
        let shot = pwmtf_game_domain::VersionedShotCommand::new(
            pwmtf_game_domain::Aim::new(0).unwrap(),
            pwmtf_game_domain::ShotPower::new(1).unwrap(),
            pwmtf_game_domain::Spin::CENTER,
        );
        assert!(matches!(
            transport.predict_shot(pwmtf_protocol::CommandId::new([7; 16]), shot, None),
            Err(TransportClientError::WrongTurn)
        ));
    }

    #[test]
    fn terminal_authority_disables_all_gameplay_prediction() {
        let mut state = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        state.concede(Player::Two).unwrap();
        let terminal = SnapshotEnvelope::new(1, state.checksum(), state.to_bytes())
            .unwrap()
            .to_bytes();
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        transport.receive_snapshot(&terminal).unwrap();
        transport.set_local_player(Player::One);
        assert_eq!(transport.status(), ConnectionStatus::Ready);
        assert!(!transport.accepts_gameplay_commands());
        let shot = pwmtf_game_domain::VersionedShotCommand::new(
            pwmtf_game_domain::Aim::new(0).unwrap(),
            pwmtf_game_domain::ShotPower::new(1).unwrap(),
            pwmtf_game_domain::Spin::CENTER,
        );
        assert!(matches!(
            transport.predict_shot(pwmtf_protocol::CommandId::new([5; 16]), shot, None),
            Err(TransportClientError::WrongTurn)
        ));
        assert!(matches!(
            transport.predict_concession(pwmtf_protocol::CommandId::new([6; 16]), Player::One,),
            Err(TransportClientError::NotReady)
        ));
    }

    #[test]
    fn authoritative_command_rejection_abandons_prediction_without_disconnect() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        transport.receive_snapshot(&snapshot(0)).unwrap();
        transport.set_local_player(Player::One);
        let shot = pwmtf_game_domain::VersionedShotCommand::new(
            pwmtf_game_domain::Aim::new(0).unwrap(),
            pwmtf_game_domain::ShotPower::new(1).unwrap(),
            pwmtf_game_domain::Spin::CENTER,
        );
        transport
            .predict_shot(pwmtf_protocol::CommandId::new([8; 16]), shot, None)
            .unwrap();
        assert!(transport.has_pending_prediction());
        assert!(!transport.accepts_active_player_command());
        assert!(matches!(
            transport.predict_shot(pwmtf_protocol::CommandId::new([9; 16]), shot, None),
            Err(TransportClientError::WrongTurn)
        ));
        assert!(transport.prediction().unwrap().has_pending_prediction());
        transport.command_rejected();
        assert_eq!(transport.status(), ConnectionStatus::Ready);
        assert!(!transport.has_pending_prediction());
        assert!(transport.take_command_rejected());
        assert!(!transport.take_command_rejected());
    }

    #[test]
    fn duplicate_authoritative_revision_is_harmless_after_confirmation() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        assert_eq!(transport.receive_snapshot(&snapshot(0)).unwrap(), None);
        assert!(transport.has_revision(0));
        assert_eq!(transport.receive_snapshot(&snapshot(0)).unwrap(), None);
        assert_eq!(transport.status(), ConnectionStatus::Ready);
    }

    #[test]
    fn unsupported_negotiation_and_malformed_snapshots_fail_closed() {
        let mut transport = BrowserTransport::default();
        let _ = transport.opened();
        assert!(matches!(
            transport.negotiated("2"),
            Err(TransportClientError::Negotiation)
        ));
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
        assert_eq!(transport.retry_delay_ms(), 500);
        transport.connecting();
        let _ = transport.opened();
        transport.negotiated("1").unwrap();
        assert!(transport.receive_snapshot(&[0, 1]).is_err());
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
    }

    #[test]
    fn snapshot_before_negotiation_does_not_initialize_transport() {
        let mut transport = BrowserTransport::default();
        transport.connecting();
        let _ = transport.opened();
        assert!(matches!(
            transport.receive_snapshot(&snapshot(0)),
            Err(TransportClientError::NotReady)
        ));
    }
}
