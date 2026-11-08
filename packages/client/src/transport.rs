//! Pure session-socket state machine, independent of web-sys and Bevy.
//!
//! [`SessionTransport`] owns the connection lifecycle, the version-two
//! handshake, clock synchronization, match/lobby subscriptions, prediction,
//! the single pending command, preview state, and shot playback scheduling.
//! Browser glue feeds it socket events and monotonic/wall clock readings and
//! performs the [`TransportAction`]s it queues. It never decides gameplay: the
//! server's `MatchState` is always applied as authority.

use pwmtf_game_domain::{
    CompletionReason, Foul, Group, MatchCommand, MatchCommandResult, MatchState, MatchStatus,
    Objective, PhysicsProfile, Player, PocketId, TableGeometry, Vector, VersionedShotCommand,
    VersionedTableState,
};
use pwmtf_protocol::{
    ClientMessage, CommandEnvelope, CommandId, MatchStateMessage, ProtocolError, RejectReason,
    SESSION_VERSION, ServerMessage, ShotPreview, SnapshotEnvelope, SubscriptionKind,
};
use thiserror::Error;

use crate::playback::ShotPlayback;
use crate::prediction::{PredictionError, PredictionState};

/// First reconnect delay after a failure.
pub const RETRY_BASE_MS: f64 = 250.0;
/// Upper bound on the exponential reconnect delay.
pub const RETRY_MAX_MS: f64 = 30_000.0;
/// Interval between client pings while ready.
pub const PING_INTERVAL_MS: f64 = 15_000.0;
/// Bound on socket establishment and on the `Hello`→`Welcome` handshake.
pub const HANDSHAKE_TIMEOUT_MS: f64 = 10_000.0;
/// A ready socket silent for this long is treated as dead.
pub const SILENCE_TIMEOUT_MS: f64 = 45_000.0;
/// Remote previews older than this are no longer displayed.
pub const REMOTE_PREVIEW_EXPIRY_MS: f64 = 650.0;
/// Minimum interval between repeated identical previews.
pub const PREVIEW_RESEND_MS: f64 = 45.0;
/// Interval between retransmissions of a cancel preview.
pub const CANCEL_RETRY_MS: f64 = 65.0;
/// A cancel preview is retransmitted for at most this long.
pub const CANCEL_EXPIRY_MS: f64 = 600.0;
const CLOCK_SAMPLES: usize = 8;

/// Session socket lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConnectionStatus {
    /// Not started, or deactivated by the page; no socket and no reconnects.
    Inactive,
    /// A socket is being established.
    Connecting,
    /// The socket is open and `Hello` was sent.
    AwaitingWelcome,
    /// The handshake completed.
    Ready,
    /// The previous socket failed; a reconnect is scheduled.
    Backoff,
}

/// Socket operation the browser glue must perform.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransportAction {
    /// Open a new same-origin `/ws` socket, replacing any previous one.
    Open,
    /// Close and forget the current socket without reporting its close.
    Close,
    /// Send one binary frame on the current socket.
    Send(Vec<u8>),
}

/// Page-facing event delivered to the JavaScript session listener.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransportEvent {
    /// Event name, such as `"match"`.
    pub name: &'static str,
    /// JSON detail object.
    pub detail: String,
}

/// Best current estimate of the server clock.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClockSync {
    /// Server Unix milliseconds minus client wall-clock milliseconds.
    pub offset_ms: f64,
    /// Half the round trip of the sample, bounding the offset error.
    pub uncertainty_ms: f64,
}

#[derive(Clone, Copy, Debug)]
struct PendingCommand {
    envelope: CommandEnvelope,
    /// Set after a socket loss; the command is resent with the same id once
    /// the post-reconnect state proves it was not applied.
    resend_on_state: bool,
}

/// Where the opponent's cue should be while they plan a shot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpponentCueTarget {
    /// Their latest aim this turn, if any has arrived.
    pub aim: Option<pwmtf_game_domain::Aim>,
    /// Drawn-back power; zero when they are not charging.
    pub power: pwmtf_game_domain::ShotPower,
    /// Cue-ball contact point for the guide.
    pub spin: pwmtf_game_domain::Spin,
}

#[derive(Debug, Default)]
struct PreviewState {
    remote: Option<(ShotPreview, f64)>,
    /// Latest accepted non-cancel preview: the opponent's resting aim after
    /// they pause or release, until the revision changes.
    resting: Option<ShotPreview>,
    last_received_sequence: Option<(u64, u32)>,
    remote_last: Option<(u64, u64, u32, bool)>,
    last_sent: Option<f64>,
    pending_cancel: Option<(ShotPreview, f64)>,
    active: Option<ShotPreview>,
    interrupted_gesture: Option<u32>,
}

impl PreviewState {
    const fn clear_remote(&mut self) {
        self.remote = None;
        self.resting = None;
        self.remote_last = None;
        self.last_received_sequence = None;
    }
}

/// What the most recent accepted shot did, for the shot summary.
#[derive(Clone, Debug, Eq, PartialEq)]
struct ShotSummary {
    revision: u64,
    shooter: Player,
    pocketed: Vec<u8>,
    fouls: Vec<Foul>,
    assigned_group: Option<Group>,
    kept_turn: bool,
    ball_in_hand: bool,
    reracked: bool,
}

impl ShotSummary {
    /// Replays an accepted shot on the state before it. Canonical physics is
    /// deterministic, so this reproduces the server's resolution exactly.
    fn replay(
        before: &MatchState,
        command: pwmtf_game_domain::VersionedMatchCommand,
        revision: u64,
    ) -> Option<Self> {
        let mut after = before.clone();
        let MatchCommandResult::Shot(resolution) = after.apply_command(command).ok()? else {
            return None;
        };
        let pocketed = if resolution.reracked {
            Vec::new()
        } else {
            before
                .table()
                .balls()
                .iter()
                .zip(after.table().balls())
                .filter(|(was, now)| !was.pocketed && now.pocketed)
                .map(|(ball, _)| ball.id.number())
                .collect()
        };
        Some(Self {
            revision,
            shooter: resolution.shooter,
            pocketed,
            fouls: resolution.fouls,
            assigned_group: resolution.assigned_group,
            kept_turn: resolution.next_player == Some(resolution.shooter),
            ball_in_hand: resolution.ball_in_hand,
            reracked: resolution.reracked,
        })
    }
}

struct MatchSession {
    match_id: u128,
    seat: Option<Player>,
    prediction: Option<PredictionState>,
    deadline_at_ms: Option<u64>,
    pending: Option<PendingCommand>,
    /// The local player's 8-ball call, valid only for the revision it was made at.
    called_pocket: Option<(u64, PocketId)>,
    last_shot: Option<ShotSummary>,
    /// Where every screen starts the cue this turn: the aim of the shot that
    /// led here, carried across placements and timeouts.
    turn_aim: Option<(u64, pwmtf_game_domain::Aim)>,
    playback: Option<ShotPlayback>,
}

impl MatchSession {
    const fn new(match_id: u128) -> Self {
        Self {
            match_id,
            seat: None,
            prediction: None,
            deadline_at_ms: None,
            pending: None,
            called_pocket: None,
            last_shot: None,
            turn_aim: None,
            playback: None,
        }
    }
}

/// Pure client session state machine; see the module documentation.
#[allow(clippy::struct_excessive_bools)]
pub struct SessionTransport {
    started: bool,
    active: bool,
    status: ConnectionStatus,
    retry_attempt: u32,
    retry_at: Option<f64>,
    handshake_deadline: Option<f64>,
    hello_sent: Option<(f64, f64)>,
    next_ping_at: Option<f64>,
    outstanding_ping: Option<(u64, f64, f64)>,
    ping_nonce: u64,
    last_received_at: f64,
    clock_samples: Vec<ClockSync>,
    ever_ready: bool,
    session: Option<MatchSession>,
    lobby_id: Option<u128>,
    previews: PreviewState,
    presentation_epoch: u64,
    playback_clock: Option<f64>,
    playback_finished: bool,
    actions: Vec<TransportAction>,
    events: Vec<TransportEvent>,
    last_connection_event: Option<&'static str>,
    last_match_event: Option<String>,
}

impl Default for SessionTransport {
    fn default() -> Self {
        Self {
            started: false,
            active: true,
            status: ConnectionStatus::Inactive,
            retry_attempt: 0,
            retry_at: None,
            handshake_deadline: None,
            hello_sent: None,
            next_ping_at: None,
            outstanding_ping: None,
            ping_nonce: 0,
            last_received_at: 0.0,
            clock_samples: Vec::new(),
            ever_ready: false,
            session: None,
            lobby_id: None,
            previews: PreviewState::default(),
            presentation_epoch: 0,
            playback_clock: None,
            playback_finished: false,
            actions: Vec::new(),
            events: Vec::new(),
            last_connection_event: None,
            last_match_event: None,
        }
    }
}

impl SessionTransport {
    // ----- lifecycle -------------------------------------------------------

    /// Starts the session: connects now and reconnects itself afterwards.
    pub fn start(&mut self, now: f64) {
        self.started = true;
        if self.active && self.status == ConnectionStatus::Inactive {
            self.open(now);
        }
    }

    /// Activates or deactivates the socket on page visibility/online changes.
    ///
    /// Deactivation closes the socket and pauses reconnects; activation
    /// reconnects immediately. A pending command survives both.
    pub fn set_active(&mut self, active: bool, now: f64) {
        self.active = active;
        if !active {
            if self.status != ConnectionStatus::Inactive {
                self.actions.push(TransportAction::Close);
                self.lose_socket();
                self.status = ConnectionStatus::Inactive;
                self.retry_at = None;
                self.emit_connection();
            }
            return;
        }
        if !self.started {
            return;
        }
        match self.status {
            ConnectionStatus::Inactive => {
                self.retry_attempt = 0;
                self.open(now);
            }
            ConnectionStatus::Backoff => self.open(now),
            ConnectionStatus::Connecting
            | ConnectionStatus::AwaitingWelcome
            | ConnectionStatus::Ready => {}
        }
    }

    /// Reports that the socket opened; sends `Hello`.
    pub fn socket_opened(&mut self, now: f64, wall: f64) {
        if self.status != ConnectionStatus::Connecting {
            return;
        }
        self.status = ConnectionStatus::AwaitingWelcome;
        self.handshake_deadline = Some(now + HANDSHAKE_TIMEOUT_MS);
        self.last_received_at = now;
        self.hello_sent = Some((now, wall));
        self.send(&ClientMessage::Hello {
            versions: vec![SESSION_VERSION],
        });
        self.emit_connection();
    }

    /// Reports that the socket closed or errored, or that a send failed.
    pub fn socket_closed(&mut self, now: f64) {
        if matches!(
            self.status,
            ConnectionStatus::Inactive | ConnectionStatus::Backoff
        ) {
            return;
        }
        self.fail(now);
    }

    /// Reports a protocol violation, such as a text frame; closes and backs off.
    pub fn protocol_violation(&mut self, now: f64) {
        self.socket_closed(now);
    }

    /// Processes timers: handshake/silence timeouts, pings and reconnects.
    pub fn poll(&mut self, now: f64, wall: f64) {
        match self.status {
            ConnectionStatus::Connecting | ConnectionStatus::AwaitingWelcome => {
                if self.handshake_deadline.is_some_and(|at| now >= at) {
                    self.fail(now);
                }
            }
            ConnectionStatus::Ready => {
                if now - self.last_received_at >= SILENCE_TIMEOUT_MS {
                    self.fail(now);
                } else if self.next_ping_at.is_some_and(|at| now >= at) {
                    self.ping_nonce = self.ping_nonce.wrapping_add(1);
                    self.outstanding_ping = Some((self.ping_nonce, now, wall));
                    self.next_ping_at = Some(now + PING_INTERVAL_MS);
                    self.send(&ClientMessage::Ping {
                        nonce: self.ping_nonce,
                    });
                }
            }
            ConnectionStatus::Backoff => {
                if self.active && self.retry_at.is_some_and(|at| now >= at) {
                    self.open(now);
                }
            }
            ConnectionStatus::Inactive => {}
        }
    }

    /// Returns the monotonic time at which [`Self::poll`] must next run.
    #[must_use]
    pub fn next_wakeup(&self) -> Option<f64> {
        match self.status {
            ConnectionStatus::Connecting | ConnectionStatus::AwaitingWelcome => {
                self.handshake_deadline
            }
            ConnectionStatus::Ready => {
                let silence = self.last_received_at + SILENCE_TIMEOUT_MS;
                Some(self.next_ping_at.map_or(silence, |ping| ping.min(silence)))
            }
            ConnectionStatus::Backoff => self.retry_at.filter(|_| self.active),
            ConnectionStatus::Inactive => None,
        }
    }

    /// Returns the delay before the next reconnect after `attempt` failures.
    #[must_use]
    pub fn retry_delay_ms(attempt: u32) -> f64 {
        let exponent = attempt.saturating_sub(1).min(16);
        (RETRY_BASE_MS * f64::from(1_u32 << exponent)).min(RETRY_MAX_MS)
    }

    fn open(&mut self, now: f64) {
        self.status = ConnectionStatus::Connecting;
        self.retry_at = None;
        self.handshake_deadline = Some(now + HANDSHAKE_TIMEOUT_MS);
        self.actions.push(TransportAction::Open);
        self.emit_connection();
    }

    fn fail(&mut self, now: f64) {
        self.actions.push(TransportAction::Close);
        self.lose_socket();
        self.retry_attempt = self.retry_attempt.saturating_add(1);
        self.status = ConnectionStatus::Backoff;
        self.retry_at = Some(now + Self::retry_delay_ms(self.retry_attempt));
        self.emit_connection();
    }

    fn lose_socket(&mut self) {
        self.handshake_deadline = None;
        self.hello_sent = None;
        self.next_ping_at = None;
        self.outstanding_ping = None;
        self.previews = PreviewState {
            interrupted_gesture: self.previews.interrupted_gesture,
            ..PreviewState::default()
        };
        if let Some(pending) = self.session.as_mut().and_then(|s| s.pending.as_mut()) {
            pending.resend_on_state = true;
        }
    }

    fn send(&mut self, message: &ClientMessage) -> bool {
        match message.to_bytes() {
            Ok(frame) => {
                self.actions.push(TransportAction::Send(frame));
                true
            }
            Err(_) => false,
        }
    }

    fn record_clock_sample(&mut self, sent_mono: f64, sent_wall: f64, now: f64, server_ms: u64) {
        let rtt = (now - sent_mono).max(0.0);
        #[allow(clippy::cast_precision_loss)]
        let server = server_ms as f64;
        let sample = ClockSync {
            offset_ms: server - (sent_wall + rtt / 2.0),
            uncertainty_ms: rtt / 2.0,
        };
        if self.clock_samples.len() == CLOCK_SAMPLES {
            self.clock_samples.remove(0);
        }
        self.clock_samples.push(sample);
    }

    // ----- inbound frames --------------------------------------------------

    /// Processes one binary server frame.
    ///
    /// Malformed frames, unexpected handshake order, an unsupported version,
    /// or an invalid authoritative snapshot close the socket and back off.
    pub fn receive(&mut self, bytes: &[u8], now: f64) {
        if !matches!(
            self.status,
            ConnectionStatus::AwaitingWelcome | ConnectionStatus::Ready
        ) {
            return;
        }
        self.last_received_at = now;
        let Ok(message) = ServerMessage::from_bytes(bytes) else {
            self.fail(now);
            return;
        };
        if self.status == ConnectionStatus::AwaitingWelcome {
            match message {
                ServerMessage::Welcome {
                    version: SESSION_VERSION,
                    server_now_ms,
                } => self.welcomed(now, server_now_ms),
                _ => self.fail(now),
            }
            return;
        }
        match message {
            ServerMessage::Welcome { .. } => self.fail(now),
            ServerMessage::MatchState(state) => {
                if self.receive_match_state(state).is_err() {
                    self.fail(now);
                }
            }
            ServerMessage::Rejected {
                match_id,
                command_id,
                reason,
            } => self.receive_rejection(match_id, command_id, reason),
            ServerMessage::Preview { match_id, preview } => {
                self.receive_preview(match_id, preview, now);
            }
            ServerMessage::Pong {
                nonce,
                server_now_ms,
            } => {
                if let Some((expected, sent_mono, sent_wall)) = self.outstanding_ping
                    && expected == nonce
                {
                    self.outstanding_ping = None;
                    self.record_clock_sample(sent_mono, sent_wall, now, server_now_ms);
                    self.emit_match();
                }
            }
            ServerMessage::LobbyChanged { lobby_id } => {
                self.push_event("lobby-changed", format!("{{\"lobby_id\":\"{lobby_id}\"}}"));
            }
            ServerMessage::MatchStarted { lobby_id, match_id } => self.push_event(
                "match-started",
                format!("{{\"lobby_id\":\"{lobby_id}\",\"match_id\":\"{match_id}\"}}"),
            ),
            ServerMessage::SocialChanged => self.push_event("social-changed", "{}".to_owned()),
            ServerMessage::SubscriptionDenied { kind, id } => {
                let kind = match kind {
                    SubscriptionKind::Match => {
                        if self.subscribed_match() == Some(id) {
                            self.session = None;
                            self.last_match_event = None;
                        }
                        "match"
                    }
                    SubscriptionKind::Lobby => {
                        if self.lobby_id == Some(id) {
                            self.lobby_id = None;
                        }
                        "lobby"
                    }
                };
                self.push_event(
                    "subscription-denied",
                    format!("{{\"kind\":\"{kind}\",\"id\":\"{id}\"}}"),
                );
            }
        }
    }

    fn welcomed(&mut self, now: f64, server_now_ms: u64) {
        if let Some((sent_mono, sent_wall)) = self.hello_sent.take() {
            self.record_clock_sample(sent_mono, sent_wall, now, server_now_ms);
        }
        self.status = ConnectionStatus::Ready;
        self.ever_ready = true;
        self.retry_attempt = 0;
        self.handshake_deadline = None;
        self.next_ping_at = Some(now + PING_INTERVAL_MS);
        if let Some(match_id) = self.subscribed_match() {
            self.send(&ClientMessage::SubscribeMatch { match_id });
        }
        if let Some(lobby_id) = self.lobby_id {
            self.send(&ClientMessage::SubscribeLobby { lobby_id });
        }
        self.emit_connection();
        self.emit_match();
    }

    // One cohesive transition: reconcile, playback, pending command and summary.
    #[allow(clippy::too_many_lines)]
    fn receive_match_state(&mut self, message: MatchStateMessage) -> Result<(), PredictionError> {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.match_id == message.match_id)
        else {
            return Ok(());
        };
        let envelope = SnapshotEnvelope::new(message.revision, message.checksum, message.snapshot)?;
        session.seat = Some(message.seat);
        session.deadline_at_ms = message.deadline_at_ms;
        let Some(prediction) = session.prediction.as_mut() else {
            session.prediction = Some(PredictionState::from_snapshot(&envelope)?);
            self.previews.clear_remote();
            self.emit_match();
            return Ok(());
        };
        let local = prediction.authoritative_revision();
        let mut superseded = false;
        let mut revision_changed = false;
        match message.revision.cmp(&local) {
            // Delayed state from before a later broadcast; authority already moved on.
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => {
                match prediction.reconcile(&envelope) {
                    Ok(_) => {}
                    Err(PredictionError::RevisionConflict) => {
                        *prediction = PredictionState::from_snapshot(&envelope)?;
                        superseded = session.pending.take().is_some();
                        session.playback = None;
                        self.playback_finished = true;
                        revision_changed = true;
                    }
                    Err(error) => return Err(error),
                }
                if let Some(pending) = session.pending.as_mut()
                    && pending.resend_on_state
                    && pending.envelope.expected_revision == message.revision
                {
                    pending.resend_on_state = false;
                    let resend = ClientMessage::Command {
                        match_id: session.match_id,
                        envelope: pending.envelope,
                    };
                    if let Ok(frame) = resend.to_bytes() {
                        self.actions.push(TransportAction::Send(frame));
                    }
                }
            }
            std::cmp::Ordering::Greater => {
                revision_changed = true;
                session.turn_aim = match message.last_command.map(|last| last.command.command) {
                    Some(MatchCommand::PlayShot { shot, .. }) => Some((message.revision, shot.aim)),
                    // The cue does not move during a placement or timeout.
                    Some(_) if message.revision == local + 1 => session
                        .turn_aim
                        .filter(|(at, _)| *at == local)
                        .map(|(_, aim)| (message.revision, aim)),
                    _ => None,
                };
                session.last_shot = message
                    .last_command
                    .filter(|_| message.revision == local + 1)
                    .and_then(|last| {
                        ShotSummary::replay(
                            prediction.authoritative(),
                            last.command,
                            message.revision,
                        )
                    });
                let ours = session.pending.is_some_and(|pending| {
                    message.last_command.map_or_else(
                        || {
                            message.revision == pending.envelope.expected_revision + 1
                                && prediction.predicted().checksum() == message.checksum
                        },
                        |last| last.command_id == pending.envelope.command_id,
                    )
                });
                if session.pending.is_some() && !ours {
                    prediction.abandon_prediction();
                    session.pending = None;
                    session.playback = None;
                    self.playback_finished = true;
                    superseded = true;
                }
                if ours {
                    // The shooter's release-time playback describes exactly
                    // this transition, so it keeps running.
                    session.pending = None;
                } else if message.revision == local + 1
                    && let Some(last) = message.last_command
                    && last.playback_ticks > 0
                    && let MatchCommand::PlayShot { shot, .. } = last.command.command
                {
                    let authority = prediction.authoritative();
                    session.playback = ShotPlayback::start(
                        authority.physics(),
                        authority.geometry(),
                        authority.table().clone(),
                        shot,
                        Some(last.playback_ticks),
                    )
                    .ok()
                    .filter(|playback| !playback.is_finished());
                    self.playback_clock = None;
                    if session.playback.is_none() {
                        self.playback_finished = true;
                    }
                } else if session.playback.take().is_some() {
                    // Gap, reconnect, placement or timeout: an older replay
                    // must not keep drawing over newer authoritative state.
                    self.playback_finished = true;
                }
                prediction.reconcile(&envelope)?;
            }
        }
        if revision_changed {
            self.previews.clear_remote();
        }
        if superseded {
            self.emit_rejection(message.match_id, "superseded");
        }
        self.emit_match();
        Ok(())
    }

    fn receive_rejection(&mut self, match_id: u128, command_id: CommandId, reason: RejectReason) {
        let Some(session) = self
            .session
            .as_mut()
            .filter(|session| session.match_id == match_id)
        else {
            return;
        };
        if session
            .pending
            .is_some_and(|pending| pending.envelope.command_id == command_id)
        {
            session.pending = None;
            if let Some(prediction) = session.prediction.as_mut() {
                prediction.abandon_prediction();
            }
            if session.playback.take().is_some() {
                self.playback_finished = true;
            }
        }
        self.emit_rejection(match_id, reject_reason_label(reason));
        self.emit_match();
    }

    fn receive_preview(&mut self, match_id: u128, preview: ShotPreview, now: f64) {
        if self.subscribed_match() != Some(match_id) || !self.remote_preview_eligible(&preview) {
            return;
        }
        let state = &mut self.previews;
        if state
            .last_received_sequence
            .is_some_and(|(revision, sequence)| {
                revision == preview.revision && preview.sequence <= sequence
            })
        {
            return;
        }
        state.last_received_sequence = Some((preview.revision, preview.sequence));
        let permitted = state
            .remote_last
            .is_none_or(|(revision, gesture, _, canceled)| {
                revision != preview.revision
                    || preview.cancel
                    || gesture != preview.gesture
                    || !canceled
            });
        if !permitted {
            return;
        }
        state.remote_last = Some((
            preview.revision,
            preview.gesture,
            preview.sequence,
            preview.cancel,
        ));
        state.remote = Some((preview, now));
        if !preview.cancel {
            state.resting = Some(preview);
        }
    }

    fn remote_preview_eligible(&self, preview: &ShotPreview) -> bool {
        let Some(session) = &self.session else {
            return false;
        };
        self.status == ConnectionStatus::Ready
            && session.pending.is_none()
            && session.playback.is_none()
            && session.prediction.as_ref().is_some_and(|prediction| {
                let state = prediction.authoritative();
                prediction.authoritative_revision() == preview.revision
                    && matches!(state.status(), MatchStatus::InProgress)
                    && session
                        .seat
                        .is_some_and(|seat| seat != state.active_player())
            })
    }

    // ----- subscriptions ---------------------------------------------------

    /// Subscribes to one match, replacing any previous match subscription and
    /// resetting all match presentation state. Re-subscribing to the current
    /// match is a no-op.
    pub fn subscribe_match(&mut self, match_id: u128) {
        if self.subscribed_match() == Some(match_id) {
            return;
        }
        self.unsubscribe_match();
        self.session = Some(MatchSession::new(match_id));
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
        if self.status == ConnectionStatus::Ready {
            self.send(&ClientMessage::SubscribeMatch { match_id });
        }
    }

    /// Drops the current match subscription and its pending work.
    pub fn unsubscribe_match(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        if self.status == ConnectionStatus::Ready {
            self.send(&ClientMessage::UnsubscribeMatch {
                match_id: session.match_id,
            });
        }
        self.previews = PreviewState::default();
        self.last_match_event = None;
        self.playback_clock = None;
        self.presentation_epoch = self.presentation_epoch.wrapping_add(1);
    }

    /// Subscribes to one lobby (presence), replacing any previous lobby.
    pub fn subscribe_lobby(&mut self, lobby_id: u128) {
        if self.lobby_id == Some(lobby_id) {
            return;
        }
        self.unsubscribe_lobby();
        self.lobby_id = Some(lobby_id);
        if self.status == ConnectionStatus::Ready {
            self.send(&ClientMessage::SubscribeLobby { lobby_id });
        }
    }

    /// Drops the current lobby subscription.
    pub fn unsubscribe_lobby(&mut self) {
        if let Some(lobby_id) = self.lobby_id.take()
            && self.status == ConnectionStatus::Ready
        {
            self.send(&ClientMessage::UnsubscribeLobby { lobby_id });
        }
    }

    // ----- commands --------------------------------------------------------

    /// Predicts and sends one shot; the shooter's playback starts immediately
    /// from the authoritative table.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] without a ready socket and
    /// initialized match, [`TransportClientError::WrongTurn`] unless the local
    /// seat is active with no pending command or playback, or a prediction
    /// failure when the domain refuses the shot locally.
    pub fn submit_shot(
        &mut self,
        command_id: CommandId,
        shot: VersionedShotCommand,
        called_pocket: Option<PocketId>,
    ) -> Result<(), TransportClientError> {
        self.require_active_player()?;
        let session = self
            .session
            .as_mut()
            .ok_or(TransportClientError::NotReady)?;
        let prediction = session
            .prediction
            .as_mut()
            .ok_or(TransportClientError::NotReady)?;
        let authority = prediction.authoritative();
        let (physics, geometry, table) = (
            authority.physics(),
            authority.geometry(),
            authority.table().clone(),
        );
        let envelope = prediction.predict_shot(command_id, shot, called_pocket)?;
        let ticks = prediction.pending_playback_ticks();
        session.playback = ShotPlayback::start(physics, geometry, table, shot, Some(ticks))
            .ok()
            .filter(|playback| !playback.is_finished());
        self.playback_clock = None;
        self.previews.pending_cancel = None;
        self.previews.active = None;
        self.previews.last_sent = None;
        self.dispatch(envelope);
        Ok(())
    }

    /// Predicts and sends one cue-ball placement.
    ///
    /// # Errors
    ///
    /// Same conditions as [`Self::submit_shot`].
    pub fn submit_cue_ball_placement(
        &mut self,
        command_id: CommandId,
        position: Vector,
    ) -> Result<(), TransportClientError> {
        self.require_active_player()?;
        let envelope = self
            .prediction_mut()
            .ok_or(TransportClientError::NotReady)?
            .predict_cue_ball_placement(command_id, position)?;
        self.dispatch(envelope);
        Ok(())
    }

    /// Predicts and sends one explicit concession.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] unless the socket is ready
    /// and the match in progress, [`TransportClientError::WrongTurn`] for a
    /// seat other than the local one, or a prediction failure (including an
    /// already pending command).
    pub fn submit_concession(
        &mut self,
        command_id: CommandId,
        player: Player,
    ) -> Result<(), TransportClientError> {
        if !self.accepts_gameplay_commands() {
            return Err(TransportClientError::NotReady);
        }
        if self.seat() != Some(player) {
            return Err(TransportClientError::WrongTurn);
        }
        let envelope = self
            .prediction_mut()
            .ok_or(TransportClientError::NotReady)?
            .predict_concession(command_id, player)?;
        self.dispatch(envelope);
        Ok(())
    }

    fn require_active_player(&self) -> Result<(), TransportClientError> {
        if !self.accepts_gameplay_commands() {
            return Err(TransportClientError::NotReady);
        }
        if !self.accepts_active_player_command() {
            return Err(TransportClientError::WrongTurn);
        }
        Ok(())
    }

    fn dispatch(&mut self, envelope: CommandEnvelope) {
        let Some(session) = self.session.as_mut() else {
            return;
        };
        session.pending = Some(PendingCommand {
            envelope,
            resend_on_state: false,
        });
        let match_id = session.match_id;
        if !self.send(&ClientMessage::Command { match_id, envelope }) {
            // Unencodable frames cannot occur for validated envelopes; resend
            // after the next authoritative state rather than losing the command.
            if let Some(pending) = self.session.as_mut().and_then(|s| s.pending.as_mut()) {
                pending.resend_on_state = true;
            }
        }
        self.emit_match();
    }

    fn prediction_mut(&mut self) -> Option<&mut PredictionState> {
        self.session.as_mut()?.prediction.as_mut()
    }

    // ----- playback --------------------------------------------------------

    /// Advances any running shot playback to monotonic time `now`.
    ///
    /// Returns `true` exactly once when a playback finishes or is cut short,
    /// so presentation snaps to the authoritative state.
    pub fn advance_playback(&mut self, now: f64) -> bool {
        let elapsed = self
            .playback_clock
            .map_or(0.0, |previous| (now - previous).max(0.0));
        self.playback_clock = Some(now);
        if let Some(session) = self.session.as_mut()
            && let Some(playback) = session.playback.as_mut()
            && playback.advance(elapsed / 1_000.0)
        {
            session.playback = None;
            self.playback_finished = true;
            self.emit_match();
        }
        std::mem::take(&mut self.playback_finished)
    }

    /// Returns the running shot playback, if any.
    #[must_use]
    pub fn playback(&self) -> Option<&ShotPlayback> {
        self.session.as_ref()?.playback.as_ref()
    }

    /// Returns whether a shot is currently playing back.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.playback().is_some()
    }

    // ----- previews --------------------------------------------------------

    /// Sends one locally quantized, non-authoritative planning update.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::WrongTurn`] unless the local seat may
    /// act now.
    pub fn send_preview(
        &mut self,
        value: ShotPreview,
        now: f64,
    ) -> Result<(), TransportClientError> {
        if !self.accepts_active_player_command() {
            return Err(TransportClientError::WrongTurn);
        }
        let match_id = self
            .subscribed_match()
            .ok_or(TransportClientError::NotReady)?;
        if !value.cancel {
            self.previews.pending_cancel = None;
            self.previews.interrupted_gesture = None;
        }
        self.send(&ClientMessage::Preview {
            match_id,
            preview: value,
        });
        self.previews.last_sent = Some(now);
        self.previews.active = (!value.cancel).then_some(value);
        if value.cancel {
            self.previews.pending_cancel = Some((value, now));
        }
        Ok(())
    }

    /// Repeats the latest preview at a bounded cadence so an idle drag stays
    /// visible. Returns whether a frame was queued.
    ///
    /// # Errors
    ///
    /// Returns [`TransportClientError::NotReady`] without a match.
    pub fn resend_preview(
        &mut self,
        value: ShotPreview,
        now: f64,
    ) -> Result<bool, TransportClientError> {
        if !self.accepts_active_player_command() {
            return Ok(false);
        }
        if self
            .previews
            .last_sent
            .is_some_and(|at| now - at < PREVIEW_RESEND_MS)
        {
            return Ok(false);
        }
        let match_id = self
            .subscribed_match()
            .ok_or(TransportClientError::NotReady)?;
        self.send(&ClientMessage::Preview {
            match_id,
            preview: value,
        });
        self.previews.last_sent = Some(now);
        self.previews.active = Some(value);
        Ok(true)
    }

    /// Cancels a charged preview before the browser suspends rendering.
    pub fn cancel_preview_on_interruption(&mut self, now: f64) {
        if let Some(mut value) = self.previews.active.take() {
            self.previews.interrupted_gesture = Some(value.sequence);
            value.cancel = true;
            value.sequence = value.sequence.saturating_add(1);
            // Expiry remains the fallback when transport is unavailable.
            let _ = self.send_preview(value, now);
        }
    }

    /// Returns the interrupted preview sequence until the held pointer is
    /// released. Callers must not reuse that sequence for a new gesture.
    #[must_use]
    pub const fn interrupted_preview_sequence(&self) -> Option<u32> {
        self.previews.interrupted_gesture
    }

    /// Clears the interruption latch after the held pointer is released.
    pub const fn rearm_interrupted_gesture(&mut self) {
        self.previews.interrupted_gesture = None;
    }

    /// Retransmits a cancel briefly to overcome loss, then expires. Returns
    /// the transmitted sequence so the next gesture cannot reuse it.
    pub fn retry_pending_cancel(&mut self, now: f64) -> Option<u32> {
        let (cancel, started) = self.previews.pending_cancel?;
        if now - started > CANCEL_EXPIRY_MS {
            self.previews.pending_cancel = None;
            return None;
        }
        if !self
            .previews
            .last_sent
            .is_some_and(|at| now - at > CANCEL_RETRY_MS)
        {
            return None;
        }
        let mut cancel = cancel;
        cancel.sequence = cancel.sequence.saturating_add(1);
        if self.send_preview(cancel, now).is_ok() {
            self.previews.pending_cancel = Some((cancel, started));
            return Some(cancel.sequence);
        }
        self.previews.pending_cancel = None;
        None
    }

    /// Drops a canceled preview when a new gesture or a shot supersedes it.
    pub const fn forget_pending_cancel(&mut self) {
        self.previews.pending_cancel = None;
    }

    /// Returns the opponent's current planning preview, if still valid.
    pub fn remote_preview(&mut self, now: f64) -> Option<ShotPreview> {
        let valid = self.previews.remote.is_some_and(|(value, at)| {
            now - at <= REMOTE_PREVIEW_EXPIRY_MS && self.remote_preview_eligible(&value)
        });
        if !valid {
            self.previews.remote = None;
        }
        self.previews
            .remote
            .and_then(|(value, _)| (!value.cancel).then_some(value))
    }

    /// Whether the local player must call a pocket for the 8-ball now.
    #[must_use]
    pub fn calling_eight(&self) -> bool {
        self.accepts_active_player_command()
            && self.session.as_ref().is_some_and(|session| {
                session
                    .seat
                    .zip(session.prediction.as_ref())
                    .is_some_and(|(seat, prediction)| {
                        prediction.authoritative().objective(seat) == Objective::EightBall
                    })
            })
    }

    /// Records the local player's 8-ball call for this turn.
    pub fn set_called_pocket(&mut self, pocket: PocketId) {
        if !self.calling_eight() {
            return;
        }
        let Some(session) = self.session.as_mut() else {
            return;
        };
        let Some(revision) = session
            .prediction
            .as_ref()
            .map(PredictionState::authoritative_revision)
        else {
            return;
        };
        session.called_pocket = Some((revision, pocket));
        self.emit_match();
    }

    /// The local player's 8-ball call for the current turn, if made.
    #[must_use]
    pub fn called_pocket(&self) -> Option<PocketId> {
        let session = self.session.as_ref()?;
        let revision = session.prediction.as_ref()?.authoritative_revision();
        session
            .called_pocket
            .filter(|(called_at, _)| *called_at == revision)
            .map(|(_, pocket)| pocket)
    }

    /// Where the cue starts this turn, identically on both players' screens:
    /// the aim of the shot that led here, or, without one (a fresh rack or a
    /// reconnect), from the cue ball toward the table's center.
    #[must_use]
    pub fn turn_start_aim(&self) -> Option<pwmtf_game_domain::Aim> {
        let session = self.session.as_ref()?;
        let prediction = session.prediction.as_ref()?;
        let revision = prediction.authoritative_revision();
        if let Some((at, aim)) = session.turn_aim
            && at == revision
        {
            return Some(aim);
        }
        let cue = prediction
            .authoritative()
            .table()
            .ball(pwmtf_game_domain::BallId::CUE)?;
        #[allow(clippy::cast_precision_loss)]
        let angle = (-(cue.position.y.micros() as f64)).atan2(-(cue.position.x.micros() as f64));
        let turns = angle.rem_euclid(std::f64::consts::TAU) / std::f64::consts::TAU;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let steps = (turns * f64::from(pwmtf_game_domain::Aim::STEPS_PER_TURN)).round() as u16
            % pwmtf_game_domain::Aim::STEPS_PER_TURN;
        pwmtf_game_domain::Aim::new(steps).ok()
    }

    /// Asks the page to prompt for a pocket call after a blocked release.
    pub fn request_pocket_call(&mut self) {
        self.push_event("hint", "{\"reason\":\"call-pocket\"}".to_owned());
    }

    /// Returns where the opponent's cue belongs during their turn: their live
    /// planning when it is fresh, otherwise resting at their last aim with no
    /// power. `None` outside the opponent's turn, so the cue is never shown
    /// for a player who cannot act.
    pub fn opponent_cue_target(&mut self, now: f64) -> Option<OpponentCueTarget> {
        let revision = self.opponent_turn_revision()?;
        if let Some(live) = self.remote_preview(now) {
            return Some(OpponentCueTarget {
                aim: Some(live.aim),
                power: live.power,
                spin: live.spin,
            });
        }
        let resting = self
            .previews
            .resting
            .filter(|preview| preview.revision == revision);
        Some(OpponentCueTarget {
            aim: resting
                .map(|preview| preview.aim)
                .or_else(|| self.turn_start_aim()),
            power: pwmtf_game_domain::ShotPower::new(0).ok()?,
            spin: resting.map_or(pwmtf_game_domain::Spin::CENTER, |preview| preview.spin),
        })
    }

    /// The authoritative revision while the opponent may act and this client
    /// is connected, idle, and not replaying a shot.
    fn opponent_turn_revision(&self) -> Option<u64> {
        let session = self.session.as_ref()?;
        if self.status != ConnectionStatus::Ready
            || session.pending.is_some()
            || session.playback.is_some()
        {
            return None;
        }
        let prediction = session.prediction.as_ref()?;
        let state = prediction.authoritative();
        (matches!(state.status(), MatchStatus::InProgress)
            && session
                .seat
                .is_some_and(|seat| seat != state.active_player()))
        .then(|| prediction.authoritative_revision())
    }

    // ----- queries ---------------------------------------------------------

    /// Returns the lifecycle status.
    #[must_use]
    pub const fn status(&self) -> ConnectionStatus {
        self.status
    }

    /// Returns the subscribed match identifier.
    #[must_use]
    pub fn subscribed_match(&self) -> Option<u128> {
        self.session.as_ref().map(|session| session.match_id)
    }

    /// Returns the subscribed lobby identifier.
    #[must_use]
    pub const fn subscribed_lobby(&self) -> Option<u128> {
        self.lobby_id
    }

    /// Returns the local seat reported by the server.
    #[must_use]
    pub fn seat(&self) -> Option<Player> {
        self.session.as_ref()?.seat
    }

    /// Returns prediction state after the first authoritative `MatchState`.
    #[must_use]
    pub fn prediction(&self) -> Option<&PredictionState> {
        self.session.as_ref()?.prediction.as_ref()
    }

    /// Returns the latest authoritative canonical match state.
    #[must_use]
    pub fn authoritative_state(&self) -> Option<&MatchState> {
        self.prediction().map(PredictionState::authoritative)
    }

    /// Returns the latest authoritative revision.
    #[must_use]
    pub fn authoritative_revision(&self) -> Option<u64> {
        self.prediction()
            .map(PredictionState::authoritative_revision)
    }

    /// Returns the predicted state's checksum.
    #[must_use]
    pub fn predicted_checksum(&self) -> Option<u64> {
        self.prediction()
            .map(|prediction| prediction.predicted().checksum())
    }

    /// Returns whether one command awaits authority.
    #[must_use]
    pub fn has_pending_command(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|session| session.pending.is_some())
    }

    /// Returns the pending command's identifier.
    #[must_use]
    pub fn pending_command_id(&self) -> Option<CommandId> {
        self.session
            .as_ref()?
            .pending
            .map(|pending| pending.envelope.command_id)
    }

    /// Returns the authoritative turn deadline in server Unix milliseconds.
    #[must_use]
    pub fn deadline_at_ms(&self) -> Option<u64> {
        self.session.as_ref()?.deadline_at_ms
    }

    /// Returns whether the socket is ready and the match is in progress.
    #[must_use]
    pub fn accepts_gameplay_commands(&self) -> bool {
        self.status == ConnectionStatus::Ready
            && self
                .authoritative_state()
                .is_some_and(|state| matches!(state.status(), MatchStatus::InProgress))
    }

    /// Returns whether the local seat may shoot or place now: ready, in
    /// progress, local turn, no pending command, and no playback running.
    #[must_use]
    pub fn accepts_active_player_command(&self) -> bool {
        self.accepts_gameplay_commands()
            && !self.has_pending_command()
            && !self.is_playing()
            && self.seat().is_some_and(|seat| {
                self.authoritative_state()
                    .is_some_and(|state| state.active_player() == seat)
            })
    }

    /// Returns whether predicted state grants ball-in-hand.
    #[must_use]
    pub fn ball_in_hand(&self) -> bool {
        self.prediction()
            .is_some_and(|prediction| prediction.predicted().ball_in_hand())
    }

    /// Returns an isolated solver input for optional exact aiming previews.
    #[must_use]
    pub fn preview_state(&self) -> Option<(VersionedTableState, PhysicsProfile, TableGeometry)> {
        let state = self.prediction()?.predicted();
        Some((state.table().clone(), state.physics(), state.geometry()))
    }

    /// Returns a counter that changes whenever match presentation must reset.
    #[must_use]
    pub const fn presentation_epoch(&self) -> u64 {
        self.presentation_epoch
    }

    /// Returns the best clock estimate (lowest-uncertainty recent sample).
    #[must_use]
    pub fn clock(&self) -> Option<ClockSync> {
        self.clock_samples
            .iter()
            .copied()
            .min_by(|a, b| a.uncertainty_ms.total_cmp(&b.uncertainty_ms))
    }

    // ----- outbox ----------------------------------------------------------

    /// Drains queued socket actions in order.
    pub fn take_actions(&mut self) -> Vec<TransportAction> {
        std::mem::take(&mut self.actions)
    }

    /// Drains queued page events in order.
    pub fn take_events(&mut self) -> Vec<TransportEvent> {
        std::mem::take(&mut self.events)
    }

    /// Re-queues the current `connection` and `match` events, for a listener
    /// registered after they were first emitted.
    pub fn replay_state_events(&mut self) {
        self.last_connection_event = None;
        self.last_match_event = None;
        if self.started {
            self.emit_connection();
        }
        self.emit_match();
    }

    fn push_event(&mut self, name: &'static str, detail: String) {
        self.events.push(TransportEvent { name, detail });
    }

    const fn connection_label(&self) -> &'static str {
        match self.status {
            ConnectionStatus::Inactive => "inactive",
            ConnectionStatus::Ready => "ready",
            ConnectionStatus::Backoff => "reconnecting",
            ConnectionStatus::Connecting | ConnectionStatus::AwaitingWelcome => {
                if self.ever_ready || self.retry_attempt > 0 {
                    "reconnecting"
                } else {
                    "connecting"
                }
            }
        }
    }

    fn emit_connection(&mut self) {
        let label = self.connection_label();
        if self.last_connection_event != Some(label) {
            self.last_connection_event = Some(label);
            self.push_event("connection", format!("{{\"status\":\"{label}\"}}"));
        }
    }

    fn emit_rejection(&mut self, match_id: u128, reason: &str) {
        self.push_event(
            "rejected",
            format!("{{\"match_id\":\"{match_id}\",\"reason\":\"{reason}\"}}"),
        );
    }

    fn emit_match(&mut self) {
        let Some(detail) = self.match_detail() else {
            return;
        };
        if self.last_match_event.as_deref() != Some(detail.as_str()) {
            self.last_match_event = Some(detail.clone());
            self.push_event("match", detail);
        }
    }

    fn match_detail(&self) -> Option<String> {
        let session = self.session.as_ref()?;
        let seat = session.seat?;
        let prediction = session.prediction.as_ref()?;
        let state = prediction.authoritative();
        let (active_player, winner, reason) = match state.status() {
            MatchStatus::InProgress => (player_number(state.active_player()), 0, ""),
            MatchStatus::Completed(outcome) => (
                0,
                player_number(outcome.winner),
                match outcome.reason {
                    CompletionReason::Concession => "concession",
                    CompletionReason::LegalEightBall => "legal-eight-ball",
                    CompletionReason::IllegalEightBall => "illegal-eight-ball",
                },
            ),
        };
        let deadline = session
            .deadline_at_ms
            .map_or_else(|| "null".to_owned(), |deadline| deadline.to_string());
        let clock = self.clock().unwrap_or(ClockSync {
            offset_ms: 0.0,
            uncertainty_ms: 0.0,
        });
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (offset, uncertainty) = (
            clock.offset_ms.round() as i64,
            clock.uncertainty_ms.ceil().max(0.0) as u64,
        );
        let opponent = match seat {
            Player::One => Player::Two,
            Player::Two => Player::One,
        };
        let pocketed = state
            .table()
            .balls()
            .iter()
            .filter(|ball| ball.pocketed && ball.id.number() != 0)
            .map(|ball| ball.id.number().to_string())
            .collect::<Vec<_>>()
            .join(",");
        let called = self.called_pocket().map_or_else(
            || "null".to_owned(),
            |pocket| format!("\"{}\"", pocket_label(pocket)),
        );
        let last_shot = session
            .last_shot
            .as_ref()
            .map_or_else(|| "null".to_owned(), shot_summary_json);
        Some(format!(
            "{{\"match_id\":\"{}\",\"revision\":{},\"seat\":{},\"active_player\":{},\
             \"winner\":{},\"completion_reason\":\"{}\",\"ball_in_hand\":{},\
             \"pending\":{},\"playing\":{},\"deadline_at_ms\":{},\
             \"server_offset_ms\":{},\"clock_uncertainty_ms\":{},\
             \"my_target\":\"{}\",\"opponent_target\":\"{}\",\"on_break\":{},\
             \"pocketed\":[{}],\"calling_eight\":{},\"called_pocket\":{},\"last_shot\":{}}}",
            session.match_id,
            prediction.authoritative_revision(),
            player_number(seat),
            active_player,
            winner,
            reason,
            state.ball_in_hand(),
            session.pending.is_some(),
            session.playback.is_some(),
            deadline,
            offset,
            uncertainty,
            objective_label(state.objective(seat)),
            objective_label(state.objective(opponent)),
            state.break_pending(),
            pocketed,
            self.calling_eight(),
            called,
            last_shot,
        ))
    }
}

const fn objective_label(objective: Objective) -> &'static str {
    match objective {
        Objective::OpenTable => "open",
        Objective::Group(Group::Solids) => "solids",
        Objective::Group(Group::Stripes) => "stripes",
        Objective::EightBall => "eight",
    }
}

/// Stable pocket names shared with the page.
#[must_use]
pub const fn pocket_label(pocket: PocketId) -> &'static str {
    match pocket {
        PocketId::TopLeft => "top-left",
        PocketId::TopCenter => "top-center",
        PocketId::TopRight => "top-right",
        PocketId::BottomLeft => "bottom-left",
        PocketId::BottomCenter => "bottom-center",
        PocketId::BottomRight => "bottom-right",
    }
}

fn shot_summary_json(summary: &ShotSummary) -> String {
    let pocketed = summary
        .pocketed
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let fouls = summary
        .fouls
        .iter()
        .map(|foul| {
            format!(
                "\"{}\"",
                match foul {
                    Foul::Scratch => "scratch",
                    Foul::NoObjectContact => "no-contact",
                    Foul::WrongFirstContact => "wrong-first-contact",
                    Foul::NoRailAfterContact => "no-rail",
                }
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let group = summary.assigned_group.map_or("null", |group| match group {
        Group::Solids => "\"solids\"",
        Group::Stripes => "\"stripes\"",
    });
    format!(
        "{{\"revision\":{},\"shooter\":{},\"pocketed\":[{}],\"fouls\":[{}],\
         \"assigned_group\":{},\"kept_turn\":{},\"ball_in_hand\":{},\"reracked\":{}}}",
        summary.revision,
        player_number(summary.shooter),
        pocketed,
        fouls,
        group,
        summary.kept_turn,
        summary.ball_in_hand,
        summary.reracked,
    )
}

const fn player_number(player: Player) -> u8 {
    match player {
        Player::One => 1,
        Player::Two => 2,
    }
}

/// Returns the stable event label for a server rejection reason.
#[must_use]
pub const fn reject_reason_label(reason: RejectReason) -> &'static str {
    match reason {
        RejectReason::Malformed => "malformed",
        RejectReason::StaleRevision => "stale-revision",
        RejectReason::WrongTurn => "wrong-turn",
        RejectReason::MatchCompleted => "match-completed",
        RejectReason::IllegalMove => "illegal-move",
        RejectReason::IdempotencyConflict => "idempotency-conflict",
        RejectReason::Unavailable => "unavailable",
        RejectReason::NotSubscribed => "not-subscribed",
    }
}

/// Client session operation failure.
#[derive(Debug, Error)]
pub enum TransportClientError {
    /// The socket is not ready or the match has no authoritative state yet.
    #[error("match session is not ready")]
    NotReady,
    /// The local seat may not act now (not its turn, pending, or playing).
    #[error("local participant may not act now")]
    WrongTurn,
    /// A wire frame could not be encoded or decoded.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// Local prediction failed.
    #[error(transparent)]
    Prediction(#[from] PredictionError),
}

#[cfg(test)]
mod tests {
    use pwmtf_game_domain::{
        Aim, BallId, MatchCommand, RackSeed, RulesProfile, ShotPower, Spin, VersionedMatchCommand,
    };
    use pwmtf_protocol::LastCommand;

    use super::*;

    const MATCH: u128 = 77;
    const LOBBY: u128 = 5;

    fn initial_state() -> MatchState {
        MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap()
    }

    fn shot(power: u16) -> VersionedShotCommand {
        VersionedShotCommand::new(
            Aim::new(0).unwrap(),
            ShotPower::new(power).unwrap(),
            Spin::CENTER,
        )
    }

    fn state_frame(
        state: &MatchState,
        revision: u64,
        seat: Player,
        last_command: Option<LastCommand>,
    ) -> Vec<u8> {
        ServerMessage::MatchState(MatchStateMessage {
            match_id: MATCH,
            seat,
            revision,
            checksum: state.checksum(),
            deadline_at_ms: Some(1_000_000),
            server_now_ms: 0,
            last_command,
            snapshot: state.to_bytes(),
        })
        .to_bytes()
        .unwrap()
    }

    fn frame(message: &ServerMessage) -> Vec<u8> {
        message.to_bytes().unwrap()
    }

    fn sent(actions: &[TransportAction]) -> Vec<ClientMessage> {
        actions
            .iter()
            .filter_map(|action| match action {
                TransportAction::Send(bytes) => Some(ClientMessage::from_bytes(bytes).unwrap()),
                _ => None,
            })
            .collect()
    }

    fn event_names(events: &[TransportEvent]) -> Vec<&'static str> {
        events.iter().map(|event| event.name).collect()
    }

    /// Opens, handshakes, and initializes a match at `revision`.
    fn ready(seat: Player, state: &MatchState, revision: u64) -> SessionTransport {
        let mut transport = SessionTransport::default();
        transport.subscribe_match(MATCH);
        transport.start(0.0);
        transport.socket_opened(10.0, 1_000.0);
        transport.receive(
            &frame(&ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 5_000,
            }),
            30.0,
        );
        transport.receive(&state_frame(state, revision, seat, None), 40.0);
        transport.take_actions();
        transport.take_events();
        transport
    }

    fn reconnect(transport: &mut SessionTransport, now: f64) {
        transport.socket_closed(now);
        let retry = transport.next_wakeup().unwrap();
        transport.poll(retry, 0.0);
        transport.socket_opened(retry + 1.0, 0.0);
        transport.receive(
            &frame(&ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 0,
            }),
            retry + 2.0,
        );
    }

    fn accepted_shot(state: &MatchState, power: u16) -> (MatchState, u32) {
        let mut next = state.clone();
        let applied = next
            .apply_command_timed(VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: shot(power),
                called_pocket: None,
            }))
            .unwrap();
        (next, applied.playback_ticks)
    }

    #[test]
    fn handshake_sends_hello_then_subscriptions_and_reports_ready() {
        let mut transport = SessionTransport::default();
        transport.subscribe_match(MATCH);
        transport.subscribe_lobby(LOBBY);
        assert!(transport.take_actions().is_empty());
        transport.start(0.0);
        assert_eq!(transport.status(), ConnectionStatus::Connecting);
        assert_eq!(transport.take_actions(), vec![TransportAction::Open]);
        transport.socket_opened(5.0, 1_000.0);
        assert_eq!(transport.status(), ConnectionStatus::AwaitingWelcome);
        assert_eq!(
            sent(&transport.take_actions()),
            vec![ClientMessage::Hello {
                versions: vec![SESSION_VERSION]
            }]
        );
        transport.receive(
            &frame(&ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 2_000,
            }),
            25.0,
        );
        assert_eq!(transport.status(), ConnectionStatus::Ready);
        assert_eq!(
            sent(&transport.take_actions()),
            vec![
                ClientMessage::SubscribeMatch { match_id: MATCH },
                ClientMessage::SubscribeLobby { lobby_id: LOBBY },
            ]
        );
        let events = transport.take_events();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.name == "connection")
                .map(|event| event.detail.as_str())
                .collect::<Vec<_>>(),
            vec!["{\"status\":\"connecting\"}", "{\"status\":\"ready\"}"]
        );
        // Welcome: RTT 20 ms → offset = 2000 - (1000 + 10), uncertainty 10.
        let clock = transport.clock().unwrap();
        assert!((clock.offset_ms - 990.0).abs() < f64::EPSILON);
        assert!((clock.uncertainty_ms - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn handshake_fails_closed_on_wrong_version_wrong_order_timeout_and_garbage() {
        for case in 0..4 {
            let mut transport = SessionTransport::default();
            transport.start(0.0);
            transport.socket_opened(1.0, 0.0);
            transport.take_actions();
            match case {
                0 => transport.receive(
                    &frame(&ServerMessage::Welcome {
                        version: 1,
                        server_now_ms: 0,
                    }),
                    2.0,
                ),
                1 => transport.receive(&frame(&ServerMessage::SocialChanged), 2.0),
                2 => transport.poll(1.0 + HANDSHAKE_TIMEOUT_MS, 0.0),
                _ => transport.receive(&[0xEE, 1, 2], 2.0),
            }
            assert_eq!(transport.status(), ConnectionStatus::Backoff, "case {case}");
            assert!(transport.take_actions().contains(&TransportAction::Close));
        }
    }

    #[test]
    fn backoff_is_bounded_exponential_and_resets_after_welcome() {
        assert!((SessionTransport::retry_delay_ms(1) - 250.0).abs() < f64::EPSILON);
        assert!((SessionTransport::retry_delay_ms(2) - 500.0).abs() < f64::EPSILON);
        assert!((SessionTransport::retry_delay_ms(8) - 30_000.0).abs() < f64::EPSILON);
        assert!((SessionTransport::retry_delay_ms(1_000) - 30_000.0).abs() < f64::EPSILON);
        let mut transport = SessionTransport::default();
        transport.start(0.0);
        transport.socket_closed(100.0);
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
        assert_eq!(transport.next_wakeup(), Some(350.0));
        transport.poll(349.0, 0.0);
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
        transport.poll(350.0, 0.0);
        assert_eq!(transport.status(), ConnectionStatus::Connecting);
        transport.socket_closed(400.0);
        assert_eq!(transport.next_wakeup(), Some(900.0));
        transport.poll(900.0, 0.0);
        transport.socket_opened(901.0, 0.0);
        transport.receive(
            &frame(&ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 0,
            }),
            902.0,
        );
        transport.socket_closed(1_000.0);
        assert_eq!(transport.next_wakeup(), Some(1_250.0));
        assert!(
            transport
                .take_events()
                .iter()
                .any(|event| event.detail == "{\"status\":\"reconnecting\"}")
        );
    }

    #[test]
    fn inactive_closes_and_pauses_reconnects_then_resumes_immediately() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        transport.set_active(false, 100.0);
        assert_eq!(transport.status(), ConnectionStatus::Inactive);
        assert_eq!(transport.take_actions(), vec![TransportAction::Close]);
        assert_eq!(transport.next_wakeup(), None);
        transport.poll(1_000_000.0, 0.0);
        assert!(transport.take_actions().is_empty());
        transport.set_active(true, 200.0);
        assert_eq!(transport.status(), ConnectionStatus::Connecting);
        assert_eq!(transport.take_actions(), vec![TransportAction::Open]);
        assert_eq!(
            event_names(&transport.take_events()),
            vec!["connection", "connection"]
        );
    }

    #[test]
    fn pings_on_interval_and_pong_refines_clock_offset() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        assert_eq!(transport.next_wakeup(), Some(30.0 + PING_INTERVAL_MS));
        let at = 30.0 + PING_INTERVAL_MS;
        transport.poll(at, 50_000.0);
        let pings = sent(&transport.take_actions());
        let [ClientMessage::Ping { nonce }] = pings.as_slice() else {
            panic!("expected one ping, got {pings:?}");
        };
        // Wrong nonce is ignored.
        transport.receive(
            &frame(&ServerMessage::Pong {
                nonce: nonce + 1,
                server_now_ms: 0,
            }),
            at + 2.0,
        );
        transport.receive(
            &frame(&ServerMessage::Pong {
                nonce: *nonce,
                server_now_ms: 60_003,
            }),
            at + 6.0,
        );
        // RTT 6 → offset = 60003 - (50000 + 3) = 10000, uncertainty 3 (< Welcome's 10).
        let clock = transport.clock().unwrap();
        assert!((clock.offset_ms - 10_000.0).abs() < f64::EPSILON);
        assert!((clock.uncertainty_ms - 3.0).abs() < f64::EPSILON);
        let events = transport.take_events();
        assert!(events.iter().any(|event| event.name == "match"
            && event.detail.contains("\"server_offset_ms\":10000")
            && event.detail.contains("\"clock_uncertainty_ms\":3")));
    }

    #[test]
    fn silent_socket_is_closed_and_reconnected() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        transport.poll(40.0 + SILENCE_TIMEOUT_MS, 0.0);
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
    }

    #[test]
    fn reconnect_resubscribes_match_and_lobby() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        transport.subscribe_lobby(LOBBY);
        transport.take_actions();
        reconnect(&mut transport, 100.0);
        let messages = sent(&transport.take_actions());
        assert!(messages.contains(&ClientMessage::SubscribeMatch { match_id: MATCH }));
        assert!(messages.contains(&ClientMessage::SubscribeLobby { lobby_id: LOBBY }));
        transport.unsubscribe_lobby();
        transport.unsubscribe_match();
        assert_eq!(
            sent(&transport.take_actions()),
            vec![
                ClientMessage::UnsubscribeLobby { lobby_id: LOBBY },
                ClientMessage::UnsubscribeMatch { match_id: MATCH },
            ]
        );
    }

    #[test]
    fn pending_command_is_resent_with_same_id_after_reconnect_at_unchanged_revision() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 3);
        let id = CommandId::new([1; 16]);
        transport.submit_shot(id, shot(5_000), None).unwrap();
        let first = sent(&transport.take_actions());
        let [ClientMessage::Command { envelope, .. }] = first.as_slice() else {
            panic!("expected command");
        };
        let original = *envelope;
        assert_eq!(original.command_id, id);
        assert_eq!(original.expected_revision, 3);
        reconnect(&mut transport, 100.0);
        transport.take_actions();
        assert!(transport.has_pending_command());
        transport.receive(&state_frame(&state, 3, Player::One, None), 500.0);
        assert_eq!(
            sent(&transport.take_actions()),
            vec![ClientMessage::Command {
                match_id: MATCH,
                envelope: original
            }]
        );
        // A second same-revision state does not resend again.
        transport.receive(&state_frame(&state, 3, Player::One, None), 501.0);
        assert!(sent(&transport.take_actions()).is_empty());
        // Duplicate-retransmit reply (no last_command) confirms by checksum.
        let (next, _) = accepted_shot(&state, 5_000);
        transport.receive(&state_frame(&next, 4, Player::One, None), 600.0);
        assert!(!transport.has_pending_command());
        assert!(
            !event_names(&transport.take_events()).contains(&"rejected"),
            "confirmation is not a rejection"
        );
    }

    #[test]
    fn pending_command_is_superseded_when_revision_moved_by_another_command() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        transport
            .submit_shot(CommandId::new([2; 16]), shot(5_000), None)
            .unwrap();
        assert!(transport.is_playing());
        reconnect(&mut transport, 50.0);
        transport.take_actions();
        transport.take_events();
        let mut timed_out = state;
        timed_out.timeout_turn().unwrap();
        transport.receive(
            &state_frame(
                &timed_out,
                1,
                Player::One,
                Some(LastCommand {
                    command_id: CommandId::new([9; 16]),
                    actor: Player::One,
                    command: VersionedMatchCommand::new(MatchCommand::Timeout),
                    playback_ticks: 0,
                }),
            ),
            60.0,
        );
        assert!(!transport.has_pending_command());
        assert!(!transport.is_playing());
        assert!(sent(&transport.take_actions()).is_empty());
        let events = transport.take_events();
        assert!(events.iter().any(|event| event.name == "rejected"
            && event.detail == format!("{{\"match_id\":\"{MATCH}\",\"reason\":\"superseded\"}}")));
        assert_eq!(transport.predicted_checksum(), Some(timed_out.checksum()));
        assert!(transport.advance_playback(70.0), "presentation must snap");
    }

    #[test]
    fn confirmation_by_command_id_keeps_shooter_playback_running() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        let id = CommandId::new([3; 16]);
        transport.submit_shot(id, shot(8_000), None).unwrap();
        assert!(transport.is_playing(), "shooter playback starts at release");
        assert!(!transport.accepts_active_player_command());
        let (next, ticks) = accepted_shot(&state, 8_000);
        assert_eq!(transport.playback().unwrap().limit(), ticks);
        transport.receive(
            &state_frame(
                &next,
                1,
                Player::One,
                Some(LastCommand {
                    command_id: id,
                    actor: Player::One,
                    command: VersionedMatchCommand::new(MatchCommand::PlayShot {
                        shot: shot(8_000),
                        called_pocket: None,
                    }),
                    playback_ticks: ticks,
                }),
            ),
            50.0,
        );
        assert!(!transport.has_pending_command());
        assert!(transport.is_playing());
        assert_eq!(transport.authoritative_revision(), Some(1));
        transport.advance_playback(100.0);
        let duration = PhysicsProfile::standard().playback_millis(ticks);
        #[allow(clippy::cast_precision_loss)]
        let end = 100.0 + duration as f64 + 50.0;
        let mut now = 100.0;
        let mut finished = false;
        while now < end && !finished {
            now += 16.0;
            finished = transport.advance_playback(now);
        }
        assert!(finished);
        assert!(!transport.is_playing());
    }

    #[test]
    fn every_rejection_reason_is_reported_and_abandons_prediction() {
        for reason in RejectReason::ALL {
            let state = initial_state();
            let mut transport = ready(Player::One, &state, 0);
            let id = CommandId::new([4; 16]);
            transport.submit_shot(id, shot(5_000), None).unwrap();
            transport.take_events();
            transport.receive(
                &frame(&ServerMessage::Rejected {
                    match_id: MATCH,
                    command_id: id,
                    reason,
                }),
                50.0,
            );
            assert!(!transport.has_pending_command());
            assert!(!transport.is_playing());
            assert_eq!(transport.predicted_checksum(), Some(state.checksum()));
            assert_eq!(transport.status(), ConnectionStatus::Ready);
            let events = transport.take_events();
            let label = reject_reason_label(reason);
            assert!(events.iter().any(|event| event.name == "rejected"
                && event.detail.contains(&format!("\"reason\":\"{label}\""))));
            assert!(transport.accepts_active_player_command());
        }
    }

    #[test]
    fn accepted_shot_sets_summary_and_shared_turn_start_aim() {
        let state = initial_state();
        let (next, ticks) = accepted_shot(&state, 8_000);
        let last = LastCommand {
            command_id: CommandId::new([6; 16]),
            actor: Player::One,
            command: VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: shot(8_000),
                called_pocket: None,
            }),
            playback_ticks: ticks,
        };
        let mut observer = ready(Player::Two, &state, 0);
        let mut shooter = ready(Player::One, &state, 0);
        let before = observer.turn_start_aim();
        assert_eq!(
            before,
            shooter.turn_start_aim(),
            "both screens agree before any shot"
        );
        observer.take_events();
        observer.receive(&state_frame(&next, 1, Player::Two, Some(last)), 50.0);
        shooter.receive(&state_frame(&next, 1, Player::One, Some(last)), 50.0);
        // The next turn starts with the cue where the shot left it, on both screens.
        assert_eq!(observer.turn_start_aim(), Some(shot(8_000).aim));
        assert_eq!(shooter.turn_start_aim(), Some(shot(8_000).aim));
        let detail = observer
            .take_events()
            .into_iter()
            .rfind(|event| event.name == "match")
            .unwrap()
            .detail;
        assert!(
            detail.contains("\"last_shot\":{\"revision\":1,\"shooter\":1,"),
            "{detail}"
        );
        assert!(detail.contains("\"on_break\":false"), "{detail}");
    }

    #[test]
    fn pocket_calls_only_while_on_the_eight_and_reset_each_turn() {
        let state = initial_state();
        let mut shooter = ready(Player::One, &state, 0);
        // Open table: no call is taken.
        assert!(!shooter.calling_eight());
        shooter.set_called_pocket(PocketId::TopLeft);
        assert_eq!(shooter.called_pocket(), None);
        // A call recorded at an earlier revision is not valid for this turn.
        if let Some(session) = shooter.session.as_mut() {
            session.called_pocket = Some((7, PocketId::TopLeft));
        }
        assert_eq!(shooter.called_pocket(), None);
        shooter.take_events();
        shooter.request_pocket_call();
        assert!(
            shooter
                .take_events()
                .iter()
                .any(|event| event.name == "hint" && event.detail.contains("call-pocket"))
        );
    }

    #[test]
    fn observer_plays_back_only_from_the_immediately_preceding_revision() {
        let state = initial_state();
        let (next, ticks) = accepted_shot(&state, 8_000);
        let last = LastCommand {
            command_id: CommandId::new([5; 16]),
            actor: Player::One,
            command: VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: shot(8_000),
                called_pocket: None,
            }),
            playback_ticks: ticks,
        };
        let mut observer = ready(Player::Two, &state, 0);
        observer.receive(&state_frame(&next, 1, Player::Two, Some(last)), 50.0);
        assert!(observer.is_playing());
        assert!(!observer.accepts_active_player_command(), "input blocked");
        let playback = observer.playback().unwrap();
        assert_eq!(playback.limit(), ticks);
        assert_eq!(playback.table().balls(), {
            let started =
                pwmtf_game_domain::start_shot(state.physics(), state.table().clone(), shot(8_000))
                    .unwrap();
            started.balls().to_vec()
        });
        assert!(
            observer
                .take_events()
                .iter()
                .any(|event| event.name == "match" && event.detail.contains("\"playing\":true"))
        );

        // A gap (local revision 0, incoming 2) snaps instead.
        let mut gapped = ready(Player::Two, &state, 0);
        gapped.receive(&state_frame(&next, 2, Player::Two, Some(last)), 50.0);
        assert!(!gapped.is_playing());
        assert_eq!(gapped.predicted_checksum(), Some(next.checksum()));

        // Reconnect states carry no last command and never play back.
        let mut reconnected = ready(Player::Two, &state, 0);
        reconnected.receive(&state_frame(&next, 1, Player::Two, None), 50.0);
        assert!(!reconnected.is_playing());

        // Placement/timeout never play back.
        let mut timed_out = state.clone();
        timed_out.timeout_turn().unwrap();
        let mut timeout_observer = ready(Player::Two, &state, 0);
        timeout_observer.receive(
            &state_frame(
                &timed_out,
                1,
                Player::Two,
                Some(LastCommand {
                    command: VersionedMatchCommand::new(MatchCommand::Timeout),
                    playback_ticks: 0,
                    ..last
                }),
            ),
            50.0,
        );
        assert!(!timeout_observer.is_playing());
    }

    #[test]
    fn newer_state_without_playback_cuts_a_stale_replay_short() {
        let state = initial_state();
        let (next, ticks) = accepted_shot(&state, 8_000);
        let mut observer = ready(Player::Two, &state, 0);
        observer.receive(
            &state_frame(
                &next,
                1,
                Player::Two,
                Some(LastCommand {
                    command_id: CommandId::new([6; 16]),
                    actor: Player::One,
                    command: VersionedMatchCommand::new(MatchCommand::PlayShot {
                        shot: shot(8_000),
                        called_pocket: None,
                    }),
                    playback_ticks: ticks,
                }),
            ),
            50.0,
        );
        assert!(observer.is_playing());
        assert!(!observer.advance_playback(60.0));
        // The tab missed a timeout and a further shot; the reconnect state
        // jumps two revisions with no transition description.
        let mut later = next;
        later.timeout_turn().unwrap();
        observer.receive(&state_frame(&later, 3, Player::Two, None), 70.0);
        assert!(!observer.is_playing());
        assert!(observer.advance_playback(80.0));
        assert_eq!(observer.predicted_checksum(), Some(later.checksum()));
    }

    #[test]
    fn observer_playback_converges_to_the_server_table() {
        let state = initial_state();
        let (next, ticks) = accepted_shot(&state, 8_000);
        let mut observer = ready(Player::Two, &state, 0);
        observer.receive(
            &state_frame(
                &next,
                1,
                Player::Two,
                Some(LastCommand {
                    command_id: CommandId::new([6; 16]),
                    actor: Player::One,
                    command: VersionedMatchCommand::new(MatchCommand::PlayShot {
                        shot: shot(8_000),
                        called_pocket: None,
                    }),
                    playback_ticks: ticks,
                }),
            ),
            50.0,
        );
        let mut playback = observer.playback().unwrap().clone();
        let mut pocketed_seen = std::collections::BTreeSet::new();
        while !playback.advance_tick() {
            for ball in playback.samples() {
                if ball.pocketed {
                    pocketed_seen.insert(ball.number);
                }
            }
        }
        assert_eq!(playback.table().balls(), next.table().balls());
        for ball in next.table().balls() {
            if ball.pocketed && ball.id != BallId::CUE {
                assert!(
                    pocketed_seen.contains(&ball.id.number())
                        || playback.sample(ball.id).unwrap().pocketed
                );
            }
        }
    }

    #[test]
    fn inactive_or_unready_participants_cannot_submit() {
        let state = initial_state();
        let mut observer = ready(Player::Two, &state, 0);
        assert!(observer.accepts_gameplay_commands());
        assert!(!observer.accepts_active_player_command());
        assert!(matches!(
            observer.submit_shot(CommandId::new([7; 16]), shot(1), None),
            Err(TransportClientError::WrongTurn)
        ));
        assert!(matches!(
            observer.submit_concession(CommandId::new([7; 16]), Player::One),
            Err(TransportClientError::WrongTurn)
        ));
        observer
            .submit_concession(CommandId::new([7; 16]), Player::Two)
            .unwrap();
        let mut offline = SessionTransport::default();
        offline.subscribe_match(MATCH);
        assert!(matches!(
            offline.submit_shot(CommandId::new([8; 16]), shot(1), None),
            Err(TransportClientError::NotReady)
        ));
        let mut completed = state;
        completed.concede(Player::Two).unwrap();
        let mut terminal = ready(Player::One, &completed, 1);
        assert!(!terminal.accepts_gameplay_commands());
        assert!(matches!(
            terminal.submit_concession(CommandId::new([9; 16]), Player::One),
            Err(TransportClientError::NotReady)
        ));
        let events = {
            terminal.replay_state_events();
            terminal.take_events()
        };
        assert!(events.iter().any(|event| {
            event.name == "match"
                && event.detail.contains("\"active_player\":0")
                && event.detail.contains("\"winner\":1")
                && event
                    .detail
                    .contains("\"completion_reason\":\"concession\"")
        }));
    }

    #[test]
    fn cue_ball_placement_is_predicted_and_sent() {
        let mut state = initial_state();
        state.timeout_turn().unwrap();
        let mut transport = ready(Player::Two, &state, 1);
        assert!(transport.ball_in_hand());
        let cue = state.table().ball(BallId::CUE).unwrap().position;
        transport
            .submit_cue_ball_placement(CommandId::new([3; 16]), cue)
            .unwrap();
        assert!(!transport.ball_in_hand());
        assert!(!transport.is_playing());
        let messages = sent(&transport.take_actions());
        assert!(matches!(
            messages.as_slice(),
            [ClientMessage::Command { envelope, .. }] if envelope.expected_revision == 1
        ));
    }

    #[test]
    fn match_event_shape_and_emit_only_on_change() {
        let state = initial_state();
        let mut transport = SessionTransport::default();
        transport.subscribe_match(MATCH);
        transport.start(0.0);
        transport.socket_opened(0.0, 1_000.0);
        transport.receive(
            &frame(&ServerMessage::Welcome {
                version: SESSION_VERSION,
                server_now_ms: 1_000,
            }),
            0.0,
        );
        transport.receive(&state_frame(&state, 0, Player::One, None), 1.0);
        let events = transport.take_events();
        let matches: Vec<_> = events
            .iter()
            .filter(|event| event.name == "match")
            .collect();
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].detail,
            format!(
                "{{\"match_id\":\"{MATCH}\",\"revision\":0,\"seat\":1,\"active_player\":1,\
                 \"winner\":0,\"completion_reason\":\"\",\"ball_in_hand\":false,\
                 \"pending\":false,\"playing\":false,\"deadline_at_ms\":1000000,\
                 \"server_offset_ms\":0,\"clock_uncertainty_ms\":0,\
                 \"my_target\":\"open\",\"opponent_target\":\"open\",\"on_break\":true,\
                 \"pocketed\":[],\"calling_eight\":false,\"called_pocket\":null,\"last_shot\":null}}"
            )
        );
        transport.receive(&state_frame(&state, 0, Player::One, None), 2.0);
        assert!(transport.take_events().is_empty());
    }

    #[test]
    fn lobby_social_and_denial_events_are_forwarded() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        transport.subscribe_lobby(LOBBY);
        for message in [
            ServerMessage::LobbyChanged { lobby_id: LOBBY },
            ServerMessage::MatchStarted {
                lobby_id: LOBBY,
                match_id: u128::MAX,
            },
            ServerMessage::SocialChanged,
            ServerMessage::SubscriptionDenied {
                kind: SubscriptionKind::Match,
                id: MATCH,
            },
            ServerMessage::SubscriptionDenied {
                kind: SubscriptionKind::Lobby,
                id: LOBBY,
            },
        ] {
            transport.receive(&frame(&message), 50.0);
        }
        let details: Vec<_> = transport
            .take_events()
            .into_iter()
            .map(|event| format!("{} {}", event.name, event.detail))
            .collect();
        assert_eq!(
            details,
            vec![
                format!("lobby-changed {{\"lobby_id\":\"{LOBBY}\"}}"),
                format!(
                    "match-started {{\"lobby_id\":\"{LOBBY}\",\"match_id\":\"{}\"}}",
                    u128::MAX
                ),
                "social-changed {}".to_owned(),
                format!("subscription-denied {{\"kind\":\"match\",\"id\":\"{MATCH}\"}}"),
                format!("subscription-denied {{\"kind\":\"lobby\",\"id\":\"{LOBBY}\"}}"),
            ]
        );
        assert_eq!(transport.subscribed_match(), None);
        assert_eq!(transport.subscribed_lobby(), None);
    }

    fn preview(revision: u64, sequence: u32, cancel: bool) -> ShotPreview {
        ShotPreview {
            revision,
            gesture: 11,
            sequence,
            aim: Aim::new(10).unwrap(),
            power: ShotPower::new(100).unwrap(),
            spin: Spin::CENTER,
            cancel,
        }
    }

    #[test]
    fn remote_previews_are_ordered_expire_and_require_observer_seat() {
        let state = initial_state();
        let mut observer = ready(Player::Two, &state, 0);
        let receive = |transport: &mut SessionTransport, value: ShotPreview, now: f64| {
            transport.receive(
                &frame(&ServerMessage::Preview {
                    match_id: MATCH,
                    preview: value,
                }),
                now,
            );
        };
        receive(&mut observer, preview(0, 2, false), 100.0);
        assert_eq!(observer.remote_preview(100.0), Some(preview(0, 2, false)));
        // Older sequence ignored.
        receive(&mut observer, preview(0, 1, false), 110.0);
        assert_eq!(observer.remote_preview(110.0), Some(preview(0, 2, false)));
        // Cancel hides it.
        receive(&mut observer, preview(0, 3, true), 120.0);
        assert_eq!(observer.remote_preview(120.0), None);
        // Same gesture cannot resume after cancel.
        receive(&mut observer, preview(0, 4, false), 130.0);
        assert_eq!(observer.remote_preview(130.0), None);
        // Wrong revision ignored, and expiry clears.
        let mut fresh = ready(Player::Two, &state, 0);
        receive(&mut fresh, preview(1, 1, false), 100.0);
        assert_eq!(fresh.remote_preview(100.0), None);
        receive(&mut fresh, preview(0, 1, false), 100.0);
        assert_eq!(
            fresh.remote_preview(100.0 + REMOTE_PREVIEW_EXPIRY_MS + 1.0),
            None
        );
        // The active shooter never displays a remote preview.
        let mut shooter = ready(Player::One, &state, 0);
        receive(&mut shooter, preview(0, 1, false), 100.0);
        assert_eq!(shooter.remote_preview(100.0), None);
    }

    #[test]
    fn opponent_cue_stays_for_their_whole_turn() {
        let state = initial_state();
        let mut observer = ready(Player::Two, &state, 0);
        let receive = |transport: &mut SessionTransport, value: ShotPreview, now: f64| {
            transport.receive(
                &frame(&ServerMessage::Preview {
                    match_id: MATCH,
                    preview: value,
                }),
                now,
            );
        };
        // Visible before the opponent has aimed at all, at the turn-start aim
        // that their own screen uses too.
        let idle = observer.opponent_cue_target(100.0).unwrap();
        assert_eq!(idle.aim, observer.turn_start_aim());
        assert!(idle.aim.is_some());
        assert_eq!(idle.power.units(), 0);
        // A live preview drives it.
        let mut charged = preview(0, 1, false);
        charged.power = pwmtf_game_domain::ShotPower::new(4_000).unwrap();
        receive(&mut observer, charged, 110.0);
        let live = observer.opponent_cue_target(120.0).unwrap();
        assert_eq!(live.aim, Some(charged.aim));
        assert_eq!(live.power.units(), 4_000);
        // After expiry, and after a release cancel, it rests at the last aim
        // with no power instead of disappearing.
        let expired = observer
            .opponent_cue_target(110.0 + REMOTE_PREVIEW_EXPIRY_MS + 1.0)
            .unwrap();
        assert_eq!((expired.aim, expired.power.units()), (Some(charged.aim), 0));
        receive(&mut observer, preview(0, 2, true), 900.0);
        let released = observer.opponent_cue_target(910.0).unwrap();
        assert_eq!(
            (released.aim, released.power.units()),
            (Some(charged.aim), 0)
        );
        // Never shown to the player whose turn it is.
        let mut shooter = ready(Player::One, &state, 0);
        assert_eq!(shooter.opponent_cue_target(100.0), None);
    }

    #[test]
    fn local_previews_send_resend_cancel_and_interrupt() {
        let state = initial_state();
        let mut shooter = ready(Player::One, &state, 0);
        shooter.send_preview(preview(0, 1, false), 100.0).unwrap();
        assert_eq!(
            sent(&shooter.take_actions()),
            vec![ClientMessage::Preview {
                match_id: MATCH,
                preview: preview(0, 1, false)
            }]
        );
        assert!(!shooter.resend_preview(preview(0, 2, false), 120.0).unwrap());
        assert!(shooter.resend_preview(preview(0, 2, false), 150.0).unwrap());
        shooter.take_actions();
        shooter.cancel_preview_on_interruption(200.0);
        assert_eq!(shooter.interrupted_preview_sequence(), Some(2));
        let cancel = sent(&shooter.take_actions());
        assert!(matches!(
            cancel.as_slice(),
            [ClientMessage::Preview { preview, .. }] if preview.cancel && preview.sequence == 3
        ));
        assert_eq!(shooter.retry_pending_cancel(240.0), None);
        assert_eq!(shooter.retry_pending_cancel(270.0), Some(4));
        assert_eq!(
            shooter.retry_pending_cancel(200.0 + CANCEL_EXPIRY_MS + 1.0),
            None
        );
        shooter.rearm_interrupted_gesture();
        assert_eq!(shooter.interrupted_preview_sequence(), None);
        let mut observer = ready(Player::Two, &state, 0);
        assert!(matches!(
            observer.send_preview(preview(0, 1, false), 0.0),
            Err(TransportClientError::WrongTurn)
        ));
    }

    #[test]
    fn subscribe_match_resets_presentation_and_ignores_other_matches() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        let epoch = transport.presentation_epoch();
        transport.subscribe_match(MATCH);
        assert_eq!(transport.presentation_epoch(), epoch);
        transport.subscribe_match(MATCH + 1);
        assert_ne!(transport.presentation_epoch(), epoch);
        assert_eq!(
            sent(&transport.take_actions()),
            vec![
                ClientMessage::UnsubscribeMatch { match_id: MATCH },
                ClientMessage::SubscribeMatch {
                    match_id: MATCH + 1
                },
            ]
        );
        assert!(transport.prediction().is_none());
        transport.receive(&state_frame(&state, 0, Player::One, None), 50.0);
        assert!(transport.prediction().is_none(), "other match ignored");
    }

    #[test]
    fn invalid_snapshot_checksum_fails_closed() {
        let state = initial_state();
        let mut transport = ready(Player::One, &state, 0);
        let bad = ServerMessage::MatchState(MatchStateMessage {
            match_id: MATCH,
            seat: Player::One,
            revision: 1,
            checksum: state.checksum() ^ 1,
            deadline_at_ms: None,
            server_now_ms: 0,
            last_command: None,
            snapshot: state.to_bytes(),
        });
        transport.receive(&frame(&bad), 50.0);
        assert_eq!(transport.status(), ConnectionStatus::Backoff);
    }
}
