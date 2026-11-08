#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Authoritative match command processing and recovery boundaries.

use std::{collections::BTreeMap, future::Future};

use pwmtf_eight_ball::{MatchCommandResult, MatchState, Player};
use pwmtf_protocol::{CommandEnvelope, CommandId, SnapshotEnvelope};
use thiserror::Error;

mod command_store;
mod cookie;
mod http;
mod identity;
mod identity_store;
mod lobby;
mod lobby_store;
mod migrations;
mod oidc;
mod oidc_store;
mod profile_store;
mod projection_store;
mod realtime;
mod rematch_store;
mod serialized_database;
mod session_store;
mod social;
mod social_store;
mod token;

pub use command_store::SwitchyCommandJournal;
pub use cookie::{
    CookieError, DEVELOPMENT_SESSION_COOKIE_NAME, SESSION_COOKIE_NAME, SameSite,
    SessionCookiePolicy,
};
pub use http::{
    CANONICAL_ORIGIN, HttpState, OIDC_CALLBACK_PATH, TransportError, router as http_router,
};
pub use identity::{
    GoogleIdentity, IdentityError, IdentityJournal, IdentityJournalError, IdentityService,
    IdentityTransition, Session, SessionTokenHash,
};
pub use identity_store::{IdentityStoreError, account_for_google_identity, link_google_identity};

pub use lobby::{
    LobbyError, LobbyId, LobbyJournal, LobbyJournalError, LobbyRecord, LobbyService, LobbyStatus,
    LobbyTransition,
};
pub use lobby_store::{
    LobbyStoreError, cancel_lobby, clear_lobby_presence, clear_lobby_readiness, load_lobby,
    lobbies_for_account, lobby_readiness, lobby_ready, ready_lobby, release_lobby_presence,
    start_ready_lobby,
};
pub use migrations::{migrate, migrations};
pub use oidc::{GOOGLE_ISSUER, GoogleOidcClient, GoogleOidcError, OidcAttempt};
pub use oidc_store::{
    ClaimedOidcAttempt, NewOidcAttempt, OidcAttemptStoreError, claim_oidc_attempt,
    cleanup_oidc_attempts, create_oidc_attempt,
};
pub use profile_store::{ProfileStoreError, account_for_handle, assign_handle, handle_for_account};
pub use projection_store::{MatchSummary, ProjectionError, match_summary, rebuild_match_summaries};
pub use realtime::{
    HELLO_TIMEOUT, IDLE_TIMEOUT, MAX_LOBBY_SUBSCRIPTIONS, MAX_MATCH_SUBSCRIPTIONS,
    PREVIEW_MIN_INTERVAL,
};
pub use rematch_store::{
    PendingRematch, RematchStoreError, accept_rematch, offer_rematch, pending_rematches_for,
};
pub use serialized_database::SerializedDatabase;
pub use session_store::{
    SessionStoreError, cleanup_expired_sessions, create_session as create_stored_session,
    insert_session, resolve_session as resolve_stored_session,
    resolve_token as resolve_session_token, revoke_session as revoke_stored_session,
    revoke_token as revoke_session_token,
};
pub use social::{
    Challenge, ChallengeId, Handle, Invitation, InvitationId, InvitationTokenHash, SocialError,
    SocialJournal, SocialJournalError, SocialService, SocialTransition,
};
pub use social_store::{
    SocialStoreError, accept_challenge_into_lobby, create_challenge, create_invitation,
    generate_invitation, pending_challenges_for, redeem_invitation_into_lobby,
    redeem_invitation_token_into_lobby, revoke_invitation,
};
pub use token::{InvitationToken, SessionToken, TOKEN_BYTES, TOKEN_CHARACTERS, TokenError};

/// Stable authenticated account identifier supplied by the identity boundary.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AccountId(u128);

impl AccountId {
    /// Creates an account identifier from the verified identity projection.
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// Returns the stable numeric identity.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

/// Authoritative absolute deadline in server-clock milliseconds.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeadlineMillis(u64);

impl DeadlineMillis {
    /// Creates an absolute server-clock deadline.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Creates a deadline one turn after `now` using an immutable rules profile.
    #[must_use]
    pub const fn after_turn(now: Self, rules: pwmtf_eight_ball::RulesProfile) -> Self {
        Self(now.0.saturating_add(rules.turn_duration_millis()))
    }

    /// Creates the next turn deadline after an accepted command: the turn
    /// starts only once the authoritative shot playback has finished, so the
    /// due time is `now + playback_millis(physics, playback_ticks) + turn`.
    ///
    /// Placement, timeout, and concession commands report zero playback ticks.
    #[must_use]
    pub const fn after_playback_and_turn(
        now: Self,
        physics: pwmtf_billiards::PhysicsProfile,
        playback_ticks: u32,
        rules: pwmtf_eight_ball::RulesProfile,
    ) -> Self {
        Self::after_turn(
            Self(
                now.0
                    .saturating_add(physics.playback_millis(playback_ticks)),
            ),
            rules,
        )
    }

    /// Returns the absolute millisecond value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Stable identity of one scheduled turn deadline.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DeadlineId {
    /// Match revision for which this deadline was created.
    pub revision: u64,
    /// Player whose turn expires.
    pub player: Player,
}

/// Durable current deadline record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ScheduledDeadline {
    /// Stable exact-once identity.
    pub id: DeadlineId,
    /// Absolute server-clock due time.
    pub due_at: DeadlineMillis,
}

/// Stable identity of one authorized live connection.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ConnectionId(u128);

impl ConnectionId {
    /// Creates a connection identifier.
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// Returns the stable numeric connection identifier.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

/// Snapshot publication failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SnapshotError {
    /// Match does not exist.
    #[error("match not found")]
    MatchNotFound,
    /// Canonical snapshot exceeds the protocol bound.
    #[error("canonical snapshot exceeds protocol bound")]
    TooLarge,
}

/// Stable match identifier.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MatchId(u128);

impl MatchId {
    /// Creates a match identifier.
    #[must_use]
    pub const fn new(value: u128) -> Self {
        Self(value)
    }

    /// Returns the stable numeric identifier.
    #[must_use]
    pub const fn value(self) -> u128 {
        self.0
    }
}

/// Authorized match participant mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Participants {
    /// Account occupying player one.
    pub player_one: AccountId,
    /// Account occupying player two.
    pub player_two: AccountId,
}

impl Participants {
    /// Returns the seat occupied by `account`, if it is a participant.
    #[must_use]
    pub const fn player_for(self, account: AccountId) -> Option<Player> {
        if account.0 == self.player_one.0 {
            Some(Player::One)
        } else if account.0 == self.player_two.0 {
            Some(Player::Two)
        } else {
            None
        }
    }
}

/// Canonical command record that must become durable before acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedCommand {
    /// Match receiving the command.
    pub match_id: MatchId,
    /// New canonical revision after acceptance.
    pub revision: u64,
    /// Idempotency key.
    pub command_id: CommandId,
    /// Authenticated actor.
    pub actor: AccountId,
    /// Exact accepted wire frame.
    pub frame: Vec<u8>,
    /// Canonical command result needed for identical duplicate acknowledgement after recovery.
    pub result: MatchCommandResult,
    /// Complete resulting canonical aggregate snapshot.
    pub snapshot: Vec<u8>,
    /// Result checksum.
    pub checksum: u64,
    /// Next authoritative deadline, absent after completion.
    pub deadline: Option<ScheduledDeadline>,
}

/// Durable command journal adapter.
///
/// Implementations must commit the record atomically before returning success.
pub trait CommandJournal {
    /// Persists one accepted command and resulting canonical state.
    ///
    /// # Errors
    ///
    /// Returns an adapter-defined error when durability cannot be established.
    fn commit(
        &mut self,
        command: AcceptedCommand,
    ) -> impl Future<Output = Result<(), JournalError>> + Send;

    /// Loads durable accepted commands for one match in ascending revision order.
    ///
    /// # Errors
    ///
    /// Returns an adapter-defined error when durable records cannot be read or
    /// validated.
    fn load(
        &self,
        match_id: MatchId,
    ) -> impl Future<Output = Result<Vec<AcceptedCommand>, JournalError>> + Send;
}

/// Durable journal failure without backend-sensitive details.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("canonical command journal commit failed")]
pub struct JournalError;

/// Authoritative response for an accepted or previously accepted command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandAcknowledgement {
    /// Canonical revision after the command.
    pub revision: u64,
    /// Stable checksum after applying the command.
    pub checksum: u64,
    /// Canonical command result.
    pub result: MatchCommandResult,
    /// Whether this response was recovered from the idempotency cache.
    pub duplicate: bool,
    /// Next authoritative deadline, absent after completion.
    pub deadline: Option<ScheduledDeadline>,
    /// Fixed simulation ticks the accepted shot ran; zero for non-shot commands.
    pub playback_ticks: u32,
}

/// Authoritative command rejection.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CommandError {
    /// Match identifier is unknown.
    #[error("match not found")]
    MatchNotFound,
    /// Authenticated account is not a participant.
    #[error("account is not authorized for match")]
    Unauthorized,
    /// Command belongs to the other participant's turn or identity.
    #[error("account cannot apply this command")]
    WrongActor,
    /// Expected revision does not match canonical state.
    #[error("stale match revision: expected {expected}, actual {actual}")]
    StaleRevision {
        /// Revision in the command.
        expected: u64,
        /// Current canonical revision.
        actual: u64,
    },
    /// Loaded terminal match retains an impossible authoritative deadline.
    #[error("terminal match retains an authoritative deadline")]
    InvalidDeadlineState,
    /// Match is already terminal and cannot accept another command.
    #[error("match is already completed")]
    MatchCompleted,
    /// Canonical revision space is exhausted; no command was accepted.
    #[error("canonical match revision is exhausted")]
    RevisionExhausted,
    /// Same idempotency key was reused with different bytes or actor.
    #[error("idempotency key conflicts with an existing command")]
    IdempotencyConflict,
    /// Canonical domain rejected the transition.
    #[error("canonical match transition rejected")]
    Domain,
    /// Durable journal failed; no acknowledgement was produced.
    #[error(transparent)]
    Journal(#[from] JournalError),
}

/// Durable recovery failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RecoveryError {
    /// Durable records could not be loaded.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// Match has already been loaded into this authority.
    #[error("match is already loaded")]
    AlreadyLoaded,
    /// Durable initial deadline does not identify revision zero and the active player.
    #[error("durable initial deadline is invalid")]
    InvalidDeadline,
    /// Revisions are not contiguous from one.
    #[error("durable command revisions are invalid")]
    InvalidRevision,
    /// A command id appears more than once in the journal.
    #[error("durable journal contains duplicate command ids")]
    DuplicateCommand,
    /// Accepted wire frame is malformed or incompatible.
    #[error("durable journal contains an invalid command frame")]
    InvalidFrame,
    /// Canonical snapshot is malformed or incompatible.
    #[error("durable journal contains an invalid canonical snapshot")]
    InvalidSnapshot,
    /// Snapshot tail does not match canonical command replay.
    #[error("durable snapshot tail does not match canonical replay")]
    ReplayMismatch,
    /// Durable history contains a command after canonical match completion.
    #[error("durable journal contains a command after match completion")]
    CommandAfterCompletion,
    /// Snapshot checksum does not match the durable record.
    #[error("durable canonical snapshot checksum mismatch")]
    ChecksumMismatch,
}

struct MatchRuntime {
    participants: Participants,
    revision: u64,
    state: MatchState,
    deadline: Option<ScheduledDeadline>,
    accepted: BTreeMap<CommandId, StoredAcknowledgement>,
}

#[derive(Clone)]
struct StoredAcknowledgement {
    actor: AccountId,
    frame: Vec<u8>,
    acknowledgement: CommandAcknowledgement,
}

/// In-process authoritative match service with injected durability.
pub struct MatchService<J> {
    journal: J,
    matches: BTreeMap<MatchId, MatchRuntime>,
}

impl<J: CommandJournal> MatchService<J> {
    /// Creates an empty authoritative service.
    #[must_use]
    pub const fn new(journal: J) -> Self {
        Self {
            journal,
            matches: BTreeMap::new(),
        }
    }

    /// Registers a canonically initialized match for injected test and adapter
    /// authorities. Production durable loading uses the private atomic initial
    /// loader so state and deadline cannot be published separately.
    ///
    /// Returns the previous state only if the identifier was already present.
    #[doc(hidden)]
    pub fn insert_match(
        &mut self,
        match_id: MatchId,
        participants: Participants,
        state: MatchState,
    ) -> Option<MatchState> {
        self.matches
            .insert(
                match_id,
                MatchRuntime {
                    participants,
                    revision: 0,
                    state,
                    deadline: None,
                    accepted: BTreeMap::new(),
                },
            )
            .map(|runtime| runtime.state)
    }

    fn load_initial_match(
        &mut self,
        match_id: MatchId,
        participants: Participants,
        state: MatchState,
        deadline: Option<ScheduledDeadline>,
    ) -> Result<(), RecoveryError> {
        if self.matches.contains_key(&match_id) {
            return Err(RecoveryError::AlreadyLoaded);
        }
        if let Some(deadline) = deadline
            && (deadline.id.revision != 0 || deadline.id.player != state.active_player())
        {
            return Err(RecoveryError::InvalidDeadline);
        }
        self.matches.insert(
            match_id,
            MatchRuntime {
                participants,
                revision: 0,
                state,
                deadline,
                accepted: BTreeMap::new(),
            },
        );
        Ok(())
    }

    /// Returns complete in-process match heads in stable identifier order.
    ///
    /// This snapshot lets operational verification release the authority lock
    /// before performing durable I/O, so readiness probes never block command
    /// processing while they inspect the database.
    #[must_use]
    pub fn match_heads(
        &self,
    ) -> Vec<(
        MatchId,
        Participants,
        u64,
        MatchState,
        Option<ScheduledDeadline>,
    )> {
        self.matches
            .iter()
            .map(|(match_id, runtime)| {
                (
                    *match_id,
                    runtime.participants,
                    runtime.revision,
                    runtime.state.clone(),
                    runtime.deadline,
                )
            })
            .collect()
    }

    /// Returns the authorized participants for one in-process match.
    #[must_use]
    pub fn participants(&self, match_id: MatchId) -> Option<Participants> {
        self.matches
            .get(&match_id)
            .map(|runtime| runtime.participants)
    }

    /// Returns the current canonical revision and state.
    #[must_use]
    pub fn match_state(&self, match_id: MatchId) -> Option<(u64, &MatchState)> {
        self.matches
            .get(&match_id)
            .map(|runtime| (runtime.revision, &runtime.state))
    }

    /// Returns a complete canonical snapshot for initial subscription or reconnect.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError`] when the match is unknown or its canonical
    /// snapshot exceeds the protocol bound.
    pub fn snapshot(&self, match_id: MatchId) -> Result<SnapshotEnvelope, SnapshotError> {
        let runtime = self
            .matches
            .get(&match_id)
            .ok_or(SnapshotError::MatchNotFound)?;
        SnapshotEnvelope::new(
            runtime.revision,
            runtime.state.checksum(),
            runtime.state.to_bytes(),
        )
        .map_err(|_| SnapshotError::TooLarge)
    }

    /// Returns the current authoritative deadline.
    #[must_use]
    pub fn deadline(&self, match_id: MatchId) -> Option<ScheduledDeadline> {
        self.matches
            .get(&match_id)
            .and_then(|runtime| runtime.deadline)
    }

    /// Returns current deadlines in stable match-identifier order.
    #[must_use]
    pub fn scheduled_deadlines(&self) -> Vec<(MatchId, ScheduledDeadline)> {
        self.matches
            .iter()
            .filter_map(|(match_id, runtime)| {
                runtime.deadline.map(|deadline| (*match_id, deadline))
            })
            .collect()
    }

    /// Returns the earliest current deadline with stable match-id tie breaking.
    #[must_use]
    pub fn next_deadline(&self) -> Option<(MatchId, ScheduledDeadline)> {
        self.scheduled_deadlines()
            .into_iter()
            .min_by_key(|(match_id, deadline)| (deadline.due_at, *match_id))
    }

    /// Applies every deadline due at or before authoritative time in stable
    /// deadline/match order.
    ///
    /// Each timeout passes through normal durable command acceptance. Stale or
    /// already-applied identities are harmless and omitted from the result.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] when a due timeout cannot be durably accepted.
    /// Previously accepted timeouts remain committed and later deadlines remain
    /// available for a subsequent poll.
    pub async fn poll_due_deadlines(
        &mut self,
        now: DeadlineMillis,
    ) -> Result<Vec<(MatchId, CommandAcknowledgement)>, CommandError> {
        let mut due = self
            .scheduled_deadlines()
            .into_iter()
            .filter(|(_, deadline)| deadline.due_at <= now)
            .collect::<Vec<_>>();
        due.sort_by_key(|(match_id, deadline)| (deadline.due_at, *match_id));
        let mut accepted = Vec::with_capacity(due.len());
        for (match_id, deadline) in due {
            if let Some(acknowledgement) = self.apply_due_deadline(match_id, deadline, now).await? {
                accepted.push((match_id, acknowledgement));
            }
        }
        Ok(accepted)
    }

    /// Applies a due deadline exactly once when its revision/player identity is current.
    ///
    /// Stale or already-applied deadline identities are harmless no-ops.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] if the synthetic timeout cannot be durably accepted.
    pub async fn apply_due_deadline(
        &mut self,
        match_id: MatchId,
        deadline: ScheduledDeadline,
        now: DeadlineMillis,
    ) -> Result<Option<CommandAcknowledgement>, CommandError> {
        let runtime = self
            .matches
            .get(&match_id)
            .ok_or(CommandError::MatchNotFound)?;
        if !matches!(
            runtime.state.status(),
            pwmtf_eight_ball::MatchStatus::InProgress
        ) {
            return if runtime.deadline.is_none() {
                Ok(None)
            } else {
                Err(CommandError::InvalidDeadlineState)
            };
        }
        if runtime.deadline != Some(deadline)
            || deadline.due_at > now
            || runtime.revision != deadline.id.revision
            || runtime.state.active_player() != deadline.id.player
        {
            return Ok(None);
        }
        let actor = match deadline.id.player {
            Player::One => runtime.participants.player_one,
            Player::Two => runtime.participants.player_two,
        };
        let envelope = CommandEnvelope::new(
            deadline.id.revision,
            timeout_command_id(deadline.id),
            pwmtf_eight_ball::VersionedMatchCommand::new(pwmtf_eight_ball::MatchCommand::Timeout),
        );
        self.apply_at(match_id, actor, envelope, now)
            .await
            .map(Some)
    }

    /// Restores one match from its durable canonical command records.
    ///
    /// The latest record's complete snapshot becomes canonical state; every
    /// record is validated for contiguous revisions, match identity, unique
    /// idempotency keys, and snapshot checksum before publication.
    ///
    /// # Errors
    ///
    /// Returns [`RecoveryError`] when durable records cannot be loaded or fail
    /// canonical integrity checks.
    pub async fn recover_match(
        &mut self,
        match_id: MatchId,
        participants: Participants,
    ) -> Result<Option<u64>, RecoveryError> {
        let records = self.journal.load(match_id).await?;
        self.restore_match(match_id, participants, records)
    }

    /// Validates and publishes already-loaded durable records for one match.
    fn restore_match(
        &mut self,
        match_id: MatchId,
        participants: Participants,
        records: Vec<AcceptedCommand>,
    ) -> Result<Option<u64>, RecoveryError> {
        if records.is_empty() {
            return Ok(None);
        }
        let mut accepted = BTreeMap::new();
        let mut previous_revision = 0_u64;
        let mut state: Option<MatchState> = None;
        let mut records_last_deadline = None;
        let mut terminal_revision = None;
        for record in records {
            if terminal_revision.is_some() {
                return Err(RecoveryError::CommandAfterCompletion);
            }
            if record.match_id != match_id
                || previous_revision.checked_add(1) != Some(record.revision)
            {
                return Err(RecoveryError::InvalidRevision);
            }
            if accepted.contains_key(&record.command_id) {
                return Err(RecoveryError::DuplicateCommand);
            }
            let envelope = CommandEnvelope::from_bytes(&record.frame)
                .map_err(|_| RecoveryError::InvalidFrame)?;
            if envelope.expected_revision != previous_revision
                || envelope.command_id != record.command_id
            {
                return Err(RecoveryError::InvalidFrame);
            }
            let restored = MatchState::from_bytes(&record.snapshot)
                .map_err(|_| RecoveryError::InvalidSnapshot)?;
            let mut replayed = if let Some(state) = state.take() {
                state
            } else {
                let configuration = restored.configuration();
                MatchState::new(
                    configuration.rules(),
                    configuration.physics(),
                    configuration.geometry(),
                    configuration.rack_seed(),
                    configuration.breaker(),
                )
                .map_err(|_| RecoveryError::InvalidSnapshot)?
            };
            let replayed_result = replayed
                .apply_command_timed(envelope.command)
                .map_err(|_| RecoveryError::ReplayMismatch)?;
            if replayed_result.result != record.result || replayed != restored {
                return Err(RecoveryError::ReplayMismatch);
            }
            if restored.checksum() != record.checksum {
                return Err(RecoveryError::ChecksumMismatch);
            }
            let acknowledgement = CommandAcknowledgement {
                revision: record.revision,
                checksum: record.checksum,
                result: record.result.clone(),
                duplicate: false,
                deadline: record.deadline,
                playback_ticks: replayed_result.playback_ticks,
            };
            accepted.insert(
                record.command_id,
                StoredAcknowledgement {
                    actor: record.actor,
                    frame: record.frame,
                    acknowledgement,
                },
            );
            previous_revision = record.revision;
            records_last_deadline = record.deadline;
            terminal_revision = matches!(
                restored.status(),
                pwmtf_eight_ball::MatchStatus::Completed(_)
            )
            .then_some(record.revision);
            state = Some(restored);
        }
        let state = state.ok_or(RecoveryError::InvalidSnapshot)?;
        if self.matches.contains_key(&match_id) {
            return Err(RecoveryError::AlreadyLoaded);
        }
        self.matches.insert(
            match_id,
            MatchRuntime {
                participants,
                revision: previous_revision,
                state,
                deadline: records_last_deadline,
                accepted,
            },
        );
        Ok(Some(previous_revision))
    }

    #[cfg(test)]
    async fn apply_for_test(
        &mut self,
        match_id: MatchId,
        actor: AccountId,
        envelope: CommandEnvelope,
    ) -> Result<CommandAcknowledgement, CommandError> {
        self.apply_at(match_id, actor, envelope, DeadlineMillis::new(0))
            .await
    }

    /// Applies a command and schedules the next turn deadline from authoritative
    /// time. The next turn starts after the accepted shot's authoritative
    /// playback, so the deadline is `now + playback + turn duration`.
    ///
    /// # Errors
    ///
    /// Returns [`CommandError`] for unknown matches, unauthorized/wrong actors,
    /// stale revisions, conflicting idempotency reuse, domain rejection, or a
    /// journal failure. Journal failure leaves in-memory canonical state and
    /// revision unchanged.
    #[allow(clippy::too_many_lines)]
    pub async fn apply_at(
        &mut self,
        match_id: MatchId,
        actor: AccountId,
        envelope: CommandEnvelope,
        now: DeadlineMillis,
    ) -> Result<CommandAcknowledgement, CommandError> {
        let runtime = self
            .matches
            .get_mut(&match_id)
            .ok_or(CommandError::MatchNotFound)?;
        let player = runtime
            .participants
            .player_for(actor)
            .ok_or(CommandError::Unauthorized)?;
        let frame = envelope.to_bytes();
        if let Some(stored) = runtime.accepted.get(&envelope.command_id) {
            if stored.actor == actor && stored.frame == frame {
                let mut acknowledgement = stored.acknowledgement.clone();
                acknowledgement.duplicate = true;
                return Ok(acknowledgement);
            }
            return Err(CommandError::IdempotencyConflict);
        }
        if envelope.expected_revision != runtime.revision {
            return Err(CommandError::StaleRevision {
                expected: envelope.expected_revision,
                actual: runtime.revision,
            });
        }
        if !matches!(
            runtime.state.status(),
            pwmtf_eight_ball::MatchStatus::InProgress
        ) {
            return Err(CommandError::MatchCompleted);
        }
        if command_requires_active_player(envelope.command.command)
            && runtime.state.active_player() != player
        {
            return Err(CommandError::WrongActor);
        }
        if let pwmtf_eight_ball::MatchCommand::Concede {
            player: command_player,
        } = envelope.command.command
            && command_player != player
        {
            return Err(CommandError::WrongActor);
        }

        let mut candidate = runtime.state.clone();
        let pwmtf_eight_ball::AppliedCommand {
            result,
            playback_ticks,
        } = candidate
            .apply_command_timed(envelope.command)
            .map_err(|_| CommandError::Domain)?;
        let revision = runtime
            .revision
            .checked_add(1)
            .ok_or(CommandError::RevisionExhausted)?;
        let checksum = candidate.checksum();
        let deadline = if matches!(
            candidate.status(),
            pwmtf_eight_ball::MatchStatus::InProgress
        ) {
            Some(ScheduledDeadline {
                id: DeadlineId {
                    revision,
                    player: candidate.active_player(),
                },
                due_at: DeadlineMillis::after_playback_and_turn(
                    now,
                    candidate.physics(),
                    playback_ticks,
                    candidate.rules(),
                ),
            })
        } else {
            None
        };
        let acknowledgement = CommandAcknowledgement {
            revision,
            checksum,
            result: result.clone(),
            duplicate: false,
            deadline,
            playback_ticks,
        };
        self.journal
            .commit(AcceptedCommand {
                match_id,
                revision,
                command_id: envelope.command_id,
                actor,
                frame: frame.clone(),
                result,
                snapshot: candidate.to_bytes(),
                checksum,
                deadline,
            })
            .await?;
        runtime.state = candidate;
        runtime.revision = revision;
        runtime.deadline = deadline;
        runtime.accepted.insert(
            envelope.command_id,
            StoredAcknowledgement {
                actor,
                frame,
                acknowledgement: acknowledgement.clone(),
            },
        );
        Ok(acknowledgement)
    }
}

/// Deterministic idempotency key of the synthetic timeout for one deadline.
///
/// Recovery and retried scheduler polls derive the same key, so a timeout is
/// accepted at most once per deadline identity.
#[must_use]
pub const fn timeout_command_id(deadline: DeadlineId) -> CommandId {
    let mut id = [0_u8; 16];
    let revision = deadline.revision.to_be_bytes();
    let mut index = 0;
    while index < revision.len() {
        id[index] = revision[index];
        index += 1;
    }
    id[8] = match deadline.player {
        Player::One => 1,
        Player::Two => 2,
    };
    CommandId::new(id)
}

/// Returns whether a command identifier lies in the reserved namespace of
/// [`timeout_command_id`] (byte 8 names a seat and bytes 9..16 are zero).
///
/// Live client commands carrying such an identifier are refused so a client
/// cannot pre-empt a future synthetic timeout's idempotency key.
#[must_use]
pub fn is_timeout_command_id(command_id: CommandId) -> bool {
    let bytes = command_id.bytes();
    matches!(bytes[8], 1 | 2) && bytes[9..].iter().all(|byte| *byte == 0)
}

const fn command_requires_active_player(command: pwmtf_eight_ball::MatchCommand) -> bool {
    matches!(
        command,
        pwmtf_eight_ball::MatchCommand::PlayShot { .. }
            | pwmtf_eight_ball::MatchCommand::PlaceCueBall { .. }
    )
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use pwmtf_billiards::{PhysicsProfile, RackSeed, TableGeometry};
    use pwmtf_eight_ball::{MatchCommand, RulesProfile, VersionedMatchCommand};

    use super::*;

    #[derive(Default)]
    struct MemoryJournal {
        records: Vec<AcceptedCommand>,
        fail: bool,
    }

    impl CommandJournal for MemoryJournal {
        async fn commit(&mut self, command: AcceptedCommand) -> Result<(), JournalError> {
            if self.fail {
                Err(JournalError)
            } else {
                self.records.push(command);
                Ok(())
            }
        }

        async fn load(&self, match_id: MatchId) -> Result<Vec<AcceptedCommand>, JournalError> {
            if self.fail {
                Err(JournalError)
            } else {
                Ok(self
                    .records
                    .iter()
                    .filter(|record| record.match_id == match_id)
                    .cloned()
                    .collect())
            }
        }
    }

    fn service() -> MatchService<MemoryJournal> {
        let mut service = MatchService::new(MemoryJournal::default());
        service.insert_match(
            MatchId::new(9),
            Participants {
                player_one: AccountId::new(1),
                player_two: AccountId::new(2),
            },
            MatchState::new(
                RulesProfile::standard(),
                PhysicsProfile::standard(),
                TableGeometry::standard(),
                RackSeed::new(42),
                Player::One,
            )
            .unwrap(),
        );
        service
    }

    fn envelope(revision: u64, id: u8, command: MatchCommand) -> CommandEnvelope {
        CommandEnvelope::new(
            revision,
            CommandId::new([id; 16]),
            VersionedMatchCommand::new(command),
        )
    }

    #[test]
    fn snapshots_support_initial_load_and_reconnect_convergence() {
        block_on(async {
            let mut service = service();
            let initial = service.snapshot(MatchId::new(9)).unwrap();
            assert_eq!(initial.revision, 0);
            assert_eq!(
                pwmtf_eight_ball::MatchState::from_bytes(&initial.snapshot)
                    .unwrap()
                    .checksum(),
                initial.checksum
            );
            service
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 7, MatchCommand::Timeout),
                )
                .await
                .unwrap();
            let reconnect = service.snapshot(MatchId::new(9)).unwrap();
            assert_eq!(reconnect.revision, 1);
            assert_ne!(reconnect.checksum, initial.checksum);
            let restored = pwmtf_eight_ball::MatchState::from_bytes(&reconnect.snapshot).unwrap();
            assert_eq!(restored.active_player(), Player::Two);
            assert_eq!(restored.checksum(), reconnect.checksum);
        });
    }

    #[test]
    fn deadline_after_turn_uses_the_pinned_profile_duration_and_saturates() {
        assert_eq!(
            DeadlineMillis::after_turn(DeadlineMillis::new(7), RulesProfile::standard()).value(),
            7 + RulesProfile::standard().turn_duration_millis()
        );
        assert_eq!(
            DeadlineMillis::after_turn(DeadlineMillis::new(u64::MAX), RulesProfile::standard())
                .value(),
            u64::MAX
        );
    }

    #[test]
    fn scheduler_polls_all_due_matches_in_stable_order() {
        block_on(async {
            let mut service = service();
            service.insert_match(
                MatchId::new(10),
                Participants {
                    player_one: AccountId::new(3),
                    player_two: AccountId::new(4),
                },
                MatchState::new(
                    RulesProfile::standard(),
                    PhysicsProfile::standard(),
                    TableGeometry::standard(),
                    RackSeed::new(43),
                    Player::One,
                )
                .unwrap(),
            );
            service
                .apply_at(
                    MatchId::new(10),
                    AccountId::new(3),
                    envelope(0, 11, MatchCommand::Timeout),
                    DeadlineMillis::new(10),
                )
                .await
                .unwrap();
            service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 12, MatchCommand::Timeout),
                    DeadlineMillis::new(10),
                )
                .await
                .unwrap();
            assert_eq!(service.next_deadline().unwrap().0, MatchId::new(9));
            assert!(
                service
                    .poll_due_deadlines(DeadlineMillis::new(30_009))
                    .await
                    .unwrap()
                    .is_empty()
            );
            let applied = service
                .poll_due_deadlines(DeadlineMillis::new(30_010))
                .await
                .unwrap();
            assert_eq!(
                applied
                    .iter()
                    .map(|(match_id, _)| *match_id)
                    .collect::<Vec<_>>(),
                vec![MatchId::new(9), MatchId::new(10)]
            );
            assert_eq!(service.match_state(MatchId::new(9)).unwrap().0, 2);
            assert_eq!(service.match_state(MatchId::new(10)).unwrap().0, 2);
        });
    }

    #[test]
    fn shot_deadline_starts_after_authoritative_playback() {
        block_on(async {
            let mut service = service();
            let shot = envelope(
                0,
                30,
                MatchCommand::PlayShot {
                    shot: pwmtf_billiards::VersionedShotCommand::new(
                        pwmtf_billiards::Aim::new(0).unwrap(),
                        pwmtf_billiards::ShotPower::new(pwmtf_billiards::ShotPower::MAX).unwrap(),
                        pwmtf_billiards::Spin::CENTER,
                    ),
                    called_pocket: None,
                },
            );
            let acknowledgement = service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    shot,
                    DeadlineMillis::new(1_000),
                )
                .await
                .unwrap();
            assert!(acknowledgement.playback_ticks > 0);
            let playback =
                PhysicsProfile::standard().playback_millis(acknowledgement.playback_ticks);
            assert!(playback > 0);
            assert_eq!(
                acknowledgement.deadline.unwrap().due_at.value(),
                1_000 + playback + RulesProfile::standard().turn_duration_millis()
            );
            assert_eq!(
                service.journal.records[0].deadline,
                acknowledgement.deadline
            );
            let duplicate = service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    shot,
                    DeadlineMillis::new(9_000),
                )
                .await
                .unwrap();
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.deadline, acknowledgement.deadline);
            let mut recovered = MatchService::new(MemoryJournal {
                records: service.journal.records.clone(),
                fail: false,
            });
            recovered
                .recover_match(
                    MatchId::new(9),
                    Participants {
                        player_one: AccountId::new(1),
                        player_two: AccountId::new(2),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                recovered.deadline(MatchId::new(9)),
                acknowledgement.deadline
            );
        });
    }

    #[test]
    fn timeout_command_ids_are_deterministic_per_deadline() {
        let id = timeout_command_id(DeadlineId {
            revision: 0x0102,
            player: Player::Two,
        });
        let mut expected = [0_u8; 16];
        expected[6] = 1;
        expected[7] = 2;
        expected[8] = 2;
        assert_eq!(id, CommandId::new(expected));
    }

    #[test]
    fn reserved_timeout_namespace_covers_every_synthetic_timeout_id() {
        for revision in [0, 1, u64::MAX] {
            for player in [Player::One, Player::Two] {
                assert!(is_timeout_command_id(timeout_command_id(DeadlineId {
                    revision,
                    player
                })));
            }
        }
        assert!(!is_timeout_command_id(CommandId::new([7; 16])));
        let mut seatless = [0_u8; 16];
        seatless[8] = 3;
        assert!(!is_timeout_command_id(CommandId::new(seatless)));
    }

    #[test]
    fn terminal_match_with_deadline_fails_scheduler_closed() {
        block_on(async {
            let mut service = service();
            service
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(
                        0,
                        25,
                        MatchCommand::Concede {
                            player: Player::One,
                        },
                    ),
                )
                .await
                .unwrap();
            let impossible = ScheduledDeadline {
                id: DeadlineId {
                    revision: 1,
                    player: Player::One,
                },
                due_at: DeadlineMillis::new(1),
            };
            service.matches.get_mut(&MatchId::new(9)).unwrap().deadline = Some(impossible);
            assert_eq!(
                service
                    .apply_due_deadline(MatchId::new(9), impossible, DeadlineMillis::new(1))
                    .await,
                Err(CommandError::InvalidDeadlineState)
            );
            assert_eq!(service.journal.records.len(), 1);
        });
    }

    #[test]
    fn deadline_is_durable_and_applies_exactly_once() {
        block_on(async {
            let mut service = service();
            let first = service
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 5, MatchCommand::Timeout),
                    DeadlineMillis::new(1_000),
                )
                .await
                .unwrap();
            let deadline = first.deadline.unwrap();
            assert_eq!(deadline.due_at, DeadlineMillis::new(31_000));
            assert_eq!(service.journal.records[0].deadline, Some(deadline));
            assert_eq!(
                service
                    .apply_due_deadline(MatchId::new(9), deadline, DeadlineMillis::new(30_999))
                    .await
                    .unwrap(),
                None
            );
            let applied = service
                .apply_due_deadline(MatchId::new(9), deadline, DeadlineMillis::new(31_000))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(applied.revision, 2);
            assert_eq!(
                service
                    .match_state(MatchId::new(9))
                    .unwrap()
                    .1
                    .active_player(),
                Player::One
            );
            assert_eq!(
                service
                    .apply_due_deadline(MatchId::new(9), deadline, DeadlineMillis::new(40_000))
                    .await
                    .unwrap(),
                None
            );
            assert_eq!(service.journal.records.len(), 2);
        });
    }

    #[test]
    fn recovery_restores_deadline_for_exact_once_application() {
        block_on(async {
            let mut original = service();
            let acknowledgement = original
                .apply_at(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 6, MatchCommand::Timeout),
                    DeadlineMillis::new(500),
                )
                .await
                .unwrap();
            let deadline = acknowledgement.deadline.unwrap();
            let mut recovered = MatchService::new(MemoryJournal {
                records: original.journal.records.clone(),
                fail: false,
            });
            recovered
                .recover_match(
                    MatchId::new(9),
                    Participants {
                        player_one: AccountId::new(1),
                        player_two: AccountId::new(2),
                    },
                )
                .await
                .unwrap();
            assert_eq!(recovered.deadline(MatchId::new(9)), Some(deadline));
            assert!(
                recovered
                    .apply_due_deadline(MatchId::new(9), deadline, deadline.due_at)
                    .await
                    .unwrap()
                    .is_some()
            );
            assert_eq!(recovered.journal.records.len(), 2);
        });
    }

    #[test]
    fn durable_recovery_restores_state_revision_and_idempotency() {
        block_on(async {
            let mut original = service();
            let command = envelope(0, 8, MatchCommand::Timeout);
            let first = original
                .apply_for_test(MatchId::new(9), AccountId::new(1), command)
                .await
                .unwrap();
            let journal = MemoryJournal {
                records: original.journal.records.clone(),
                fail: false,
            };
            let expected = original.match_state(MatchId::new(9)).unwrap().1.clone();
            let mut recovered = MatchService::new(journal);
            assert_eq!(
                recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Ok(Some(1))
            );
            let (revision, state) = recovered.match_state(MatchId::new(9)).unwrap();
            assert_eq!(revision, 1);
            assert_eq!(state, &expected);
            assert_eq!(
                recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Err(RecoveryError::AlreadyLoaded)
            );
            let duplicate = recovered
                .apply_for_test(MatchId::new(9), AccountId::new(1), command)
                .await
                .unwrap();
            assert_eq!(duplicate.result, first.result);
            assert!(duplicate.duplicate);
            assert_eq!(recovered.journal.records.len(), 1);
        });
    }

    #[test]
    fn recovery_rejects_durable_commands_after_completion() {
        block_on(async {
            let mut original = service();
            original
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(
                        0,
                        23,
                        MatchCommand::Concede {
                            player: Player::One,
                        },
                    ),
                )
                .await
                .unwrap();
            let mut extra = original.journal.records[0].clone();
            extra.revision = 2;
            extra.command_id = CommandId::new([24; 16]);
            extra.frame = envelope(1, 24, MatchCommand::Timeout).to_bytes();
            let mut recovered = MatchService::new(MemoryJournal {
                records: vec![original.journal.records[0].clone(), extra],
                fail: false,
            });
            assert_eq!(
                recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Err(RecoveryError::CommandAfterCompletion)
            );
            assert!(recovered.match_state(MatchId::new(9)).is_none());
        });
    }

    #[test]
    fn recovery_replays_snapshot_tail_and_rejects_divergence() {
        block_on(async {
            let mut original = service();
            original
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 12, MatchCommand::Timeout),
                )
                .await
                .unwrap();
            original
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(2),
                    envelope(1, 13, MatchCommand::Timeout),
                )
                .await
                .unwrap();
            let mut first_divergence = original.journal.records.clone();
            first_divergence[0].snapshot = first_divergence[1].snapshot.clone();
            first_divergence[0].checksum = MatchState::from_bytes(&first_divergence[0].snapshot)
                .unwrap()
                .checksum();
            let mut first_recovered = MatchService::new(MemoryJournal {
                records: first_divergence,
                fail: false,
            });
            assert_eq!(
                first_recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Err(RecoveryError::ReplayMismatch)
            );
            let mut records = original.journal.records.clone();
            records[1].snapshot = records[0].snapshot.clone();
            records[1].checksum = MatchState::from_bytes(&records[1].snapshot)
                .unwrap()
                .checksum();
            let mut recovered = MatchService::new(MemoryJournal {
                records,
                fail: false,
            });
            assert_eq!(
                recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Err(RecoveryError::ReplayMismatch)
            );
            assert!(recovered.match_state(MatchId::new(9)).is_none());
        });
    }

    #[test]
    fn corrupted_recovery_records_fail_closed() {
        block_on(async {
            let mut original = service();
            original
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 9, MatchCommand::Timeout),
                )
                .await
                .unwrap();
            let mut records = original.journal.records.clone();
            records[0].checksum ^= 1;
            let mut recovered = MatchService::new(MemoryJournal {
                records,
                fail: false,
            });
            assert_eq!(
                recovered
                    .recover_match(
                        MatchId::new(9),
                        Participants {
                            player_one: AccountId::new(1),
                            player_two: AccountId::new(2),
                        },
                    )
                    .await,
                Err(RecoveryError::ChecksumMismatch)
            );
            assert!(recovered.match_state(MatchId::new(9)).is_none());
        });
    }

    #[test]
    fn acknowledgement_happens_after_durable_commit() {
        block_on(async {
            let mut service = service();
            let acknowledgement = service
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(0, 1, MatchCommand::Timeout),
                )
                .await
                .unwrap();
            assert_eq!(acknowledgement.revision, 1);
            assert!(!acknowledgement.duplicate);
            assert_eq!(service.journal.records.len(), 1);
            let (revision, state) = service.match_state(MatchId::new(9)).unwrap();
            assert_eq!(revision, 1);
            assert_eq!(state.active_player(), Player::Two);
            assert_eq!(service.journal.records[0].snapshot, state.to_bytes());
        });
    }

    #[test]
    fn completed_match_rejects_further_commands_without_commit() {
        block_on(async {
            let mut service = service();
            service
                .apply_for_test(
                    MatchId::new(9),
                    AccountId::new(1),
                    envelope(
                        0,
                        21,
                        MatchCommand::Concede {
                            player: Player::One,
                        },
                    ),
                )
                .await
                .unwrap();
            let before = service.match_state(MatchId::new(9)).unwrap().1.clone();
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(2),
                        envelope(1, 22, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::MatchCompleted)
            );
            assert_eq!(service.journal.records.len(), 1);
            assert_eq!(service.match_state(MatchId::new(9)).unwrap().1, &before);
        });
    }

    #[test]
    fn exhausted_revision_fails_closed_without_mutation_or_commit() {
        block_on(async {
            let mut service = service();
            let runtime = service.matches.get_mut(&MatchId::new(9)).unwrap();
            runtime.revision = u64::MAX;
            let before = runtime.state.clone();
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(1),
                        envelope(u64::MAX, 1, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::RevisionExhausted)
            );
            let (revision, after) = service.match_state(MatchId::new(9)).unwrap();
            assert_eq!(revision, u64::MAX);
            assert_eq!(after, &before);
            assert!(service.journal.records.is_empty());
        });
    }

    #[test]
    fn journal_failure_does_not_mutate_or_acknowledge() {
        block_on(async {
            let mut service = service();
            service.journal.fail = true;
            let before = service.match_state(MatchId::new(9)).unwrap().1.clone();
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(1),
                        envelope(0, 1, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::Journal(JournalError))
            );
            let (revision, after) = service.match_state(MatchId::new(9)).unwrap();
            assert_eq!(revision, 0);
            assert_eq!(after, &before);
        });
    }

    #[test]
    fn duplicates_are_harmless_and_conflicts_fail() {
        block_on(async {
            let mut service = service();
            let command = envelope(0, 3, MatchCommand::Timeout);
            let first = service
                .apply_for_test(MatchId::new(9), AccountId::new(1), command)
                .await
                .unwrap();
            let duplicate = service
                .apply_for_test(MatchId::new(9), AccountId::new(1), command)
                .await
                .unwrap();
            assert_eq!(first.revision, duplicate.revision);
            assert!(duplicate.duplicate);
            assert_eq!(service.journal.records.len(), 1);
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(1),
                        envelope(1, 3, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::IdempotencyConflict)
            );
        });
    }

    #[test]
    fn authorization_revision_and_active_player_are_enforced() {
        block_on(async {
            let mut service = service();
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(3),
                        envelope(0, 1, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::Unauthorized)
            );
            assert!(matches!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(1),
                        envelope(7, 2, MatchCommand::Timeout),
                    )
                    .await,
                Err(CommandError::StaleRevision { actual: 0, .. })
            ));
            assert_eq!(
                service
                    .apply_for_test(
                        MatchId::new(9),
                        AccountId::new(2),
                        envelope(
                            0,
                            4,
                            MatchCommand::PlayShot {
                                shot: pwmtf_billiards::VersionedShotCommand::new(
                                    pwmtf_billiards::Aim::new(0).unwrap(),
                                    pwmtf_billiards::ShotPower::new(0).unwrap(),
                                    pwmtf_billiards::Spin::CENTER,
                                ),
                                called_pocket: None,
                            },
                        ),
                    )
                    .await,
                Err(CommandError::WrongActor)
            );
        });
    }
}
