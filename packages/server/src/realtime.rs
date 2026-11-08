//! Live session hub: per-match authorities, the account connection registry,
//! in-memory lobby presence, and the single `/ws` session loop.
//!
//! Concurrency model:
//!
//! * Every loaded match is one [`MatchUnit`] with its own async authority
//!   mutex, broadcast channel, cached head (revision/turn/deadline) and preview
//!   gate. No lock shared across matches is ever held across a database await;
//!   the registry maps are synchronous and only held for map operations.
//! * Each socket owns one bounded outbound queue. Per-match forwarder tasks and
//!   HTTP notification pushes enqueue into it; a full queue closes the slow
//!   connection, which then resynchronizes on reconnect.
//! * Lobby presence is a live connection holding an active `SubscribeLobby`.
//!   Each presence has a fresh identity, so durable explicit readiness rows
//!   keyed by that identity become inert as soon as the presence disappears
//!   (including across a process restart).

use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex as SyncMutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use axum::extract::ws::{Message, WebSocket};
use pwmtf_eight_ball::{MatchCommand, MatchStatus, Player, VersionedMatchCommand};
use pwmtf_protocol::{
    ClientMessage, CommandEnvelope, LastCommand, MAX_SNAPSHOT_BYTES, MatchStateMessage,
    RejectReason, SESSION_VERSION, ServerMessage, ShotPreview, SubscriptionKind, negotiate_version,
};
use tokio::sync::{Notify, broadcast, mpsc};

use crate::{
    AccountId, CommandError, ConnectionId, DeadlineMillis, HttpState, LobbyId, LobbyRecord,
    LobbyStatus, MatchId, MatchService, Participants, ScheduledDeadline, SwitchyCommandJournal,
    TransportError,
};

/// The client must send `Hello` within this interval after the upgrade.
pub const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
/// Connections silent for longer than this are closed (clients ping every 15 s).
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(45);
/// Maximum simultaneous match subscriptions per connection.
pub const MAX_MATCH_SUBSCRIPTIONS: usize = 16;
/// Maximum simultaneous lobby subscriptions per connection.
pub const MAX_LOBBY_SUBSCRIPTIONS: usize = 4;
/// Minimum interval between forwarded previews, except a cancel after a
/// non-cancel preview.
pub const PREVIEW_MIN_INTERVAL: Duration = Duration::from_millis(40);
const OUTBOUND_CAPACITY: usize = 128;
const MATCH_EVENT_CAPACITY: usize = 64;
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(5);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

fn lock<T>(mutex: &SyncMutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One complete authoritative state publication for a match.
#[derive(Debug)]
pub struct StateEvent {
    revision: u64,
    checksum: u64,
    deadline_at_ms: Option<u64>,
    server_now_ms: u64,
    last_command: Option<LastCommand>,
    snapshot: Vec<u8>,
}

impl StateEvent {
    fn message(&self, match_id: MatchId, seat: Player) -> ServerMessage {
        ServerMessage::MatchState(MatchStateMessage {
            match_id: match_id.value(),
            seat,
            revision: self.revision,
            checksum: self.checksum,
            deadline_at_ms: self.deadline_at_ms,
            server_now_ms: self.server_now_ms,
            last_command: self.last_command,
            snapshot: self.snapshot.clone(),
        })
    }
}

#[derive(Clone, Debug)]
enum MatchEvent {
    State(Arc<StateEvent>),
    Preview { from: Player, preview: ShotPreview },
}

#[derive(Clone, Copy, Debug)]
struct MatchHead {
    revision: u64,
    active_player: Player,
    in_progress: bool,
    deadline: Option<ScheduledDeadline>,
}

#[derive(Default)]
struct PreviewGate {
    last: Option<(Instant, bool)>,
    order: Option<(u64, u32)>,
}

/// Outcome of one live command for the sending connection.
pub enum CommandOutcome {
    /// Newly accepted; every subscriber receives the broadcast `MatchState`
    /// with `last_command`. The sender also enqueues this state directly, in
    /// order, so an immediate retransmit's duplicate reply at the same
    /// revision cannot overtake (and suppress) the transition.
    Accepted(Arc<StateEvent>),
    /// Identical retransmit of an already accepted command.
    Duplicate(Arc<StateEvent>),
    /// Refused; the sender receives `Rejected` followed by this state.
    Rejected(RejectReason, Arc<StateEvent>),
}

/// Per-match authoritative unit.
pub struct MatchUnit {
    match_id: MatchId,
    participants: Participants,
    authority: tokio::sync::Mutex<MatchService<SwitchyCommandJournal>>,
    head: SyncMutex<MatchHead>,
    events: broadcast::Sender<MatchEvent>,
    previews: SyncMutex<PreviewGate>,
}

impl MatchUnit {
    fn new(
        match_id: MatchId,
        participants: Participants,
        service: MatchService<SwitchyCommandJournal>,
    ) -> Result<Self, TransportError> {
        let head = head_of(&service, match_id)?;
        let (events, _) = broadcast::channel(MATCH_EVENT_CAPACITY);
        Ok(Self {
            match_id,
            participants,
            authority: tokio::sync::Mutex::new(service),
            head: SyncMutex::new(head),
            events,
            previews: SyncMutex::new(PreviewGate::default()),
        })
    }

    pub const fn participants(&self) -> Participants {
        self.participants
    }

    pub fn deadline(&self) -> Option<ScheduledDeadline> {
        lock(&self.head).deadline
    }

    /// Returns whether the canonical match still accepts turns.
    pub fn in_progress(&self) -> bool {
        lock(&self.head).in_progress
    }

    fn refresh_head(&self, service: &MatchService<SwitchyCommandJournal>) {
        if let Ok(head) = head_of(service, self.match_id) {
            *lock(&self.head) = head;
        }
    }

    fn state_event(
        &self,
        service: &MatchService<SwitchyCommandJournal>,
        last_command: Option<LastCommand>,
    ) -> Result<Arc<StateEvent>, TransportError> {
        let (revision, state) = service
            .match_state(self.match_id)
            .ok_or(TransportError::MatchNotFound)?;
        let snapshot = state.to_bytes();
        if snapshot.len() > MAX_SNAPSHOT_BYTES {
            return Err(TransportError::Internal);
        }
        Ok(Arc::new(StateEvent {
            revision,
            checksum: state.checksum(),
            deadline_at_ms: service
                .deadline(self.match_id)
                .map(|deadline| deadline.due_at.value()),
            server_now_ms: crate::http::unix_millis()?,
            last_command,
            snapshot,
        }))
    }

    /// Returns the current complete state without a transition description.
    pub async fn current_state(&self) -> Result<Arc<StateEvent>, TransportError> {
        let service = self.authority.lock().await;
        self.state_event(&service, None)
    }

    /// Returns a gap-free event receiver together with the state it follows.
    async fn subscribe(
        &self,
    ) -> Result<(broadcast::Receiver<MatchEvent>, Arc<StateEvent>), TransportError> {
        let service = self.authority.lock().await;
        let receiver = self.events.subscribe();
        let state = self.state_event(&service, None)?;
        drop(service);
        Ok((receiver, state))
    }

    /// Returns the in-process canonical head for readiness verification.
    pub async fn head(
        &self,
    ) -> Option<(
        Participants,
        u64,
        pwmtf_eight_ball::MatchState,
        Option<ScheduledDeadline>,
    )> {
        let service = self.authority.lock().await;
        service.match_heads().into_iter().next().map(
            |(_, participants, revision, state, deadline)| {
                (participants, revision, state, deadline)
            },
        )
    }

    /// Applies one live client command with durable commit before any
    /// publication.
    ///
    /// `Timeout` is server-only: a client-originated `Timeout` is refused as
    /// [`RejectReason::IllegalMove`], and a command identifier inside the
    /// reserved synthetic-timeout namespace is refused as
    /// [`RejectReason::Malformed`], so a client can neither end a turn nor
    /// pre-empt a future timeout's idempotency key.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::InvalidSubscription`] when the account is not
    /// a participant, and clock/encoding failures from state publication.
    pub async fn apply(
        &self,
        account: AccountId,
        envelope: CommandEnvelope,
        now: DeadlineMillis,
    ) -> Result<CommandOutcome, TransportError> {
        let actor = self
            .participants
            .player_for(account)
            .ok_or(TransportError::InvalidSubscription)?;
        let mut service = self.authority.lock().await;
        let reserved = if matches!(envelope.command.command, MatchCommand::Timeout) {
            Some(RejectReason::IllegalMove)
        } else if crate::is_timeout_command_id(envelope.command_id) {
            Some(RejectReason::Malformed)
        } else {
            None
        };
        if let Some(reason) = reserved {
            return Ok(CommandOutcome::Rejected(
                reason,
                self.state_event(&service, None)?,
            ));
        }
        match service
            .apply_at(self.match_id, account, envelope, now)
            .await
        {
            Ok(acknowledgement) if acknowledgement.duplicate => {
                Ok(CommandOutcome::Duplicate(self.state_event(&service, None)?))
            }
            Ok(acknowledgement) => {
                self.refresh_head(&service);
                let event = self.state_event(
                    &service,
                    Some(LastCommand {
                        command_id: envelope.command_id,
                        actor,
                        command: envelope.command,
                        playback_ticks: acknowledgement.playback_ticks,
                    }),
                )?;
                let _ = self.events.send(MatchEvent::State(Arc::clone(&event)));
                drop(service);
                Ok(CommandOutcome::Accepted(event))
            }
            Err(error) => Ok(CommandOutcome::Rejected(
                reject_reason(error),
                self.state_event(&service, None)?,
            )),
        }
    }

    /// Applies this match's deadline exactly once when it is due, publishing
    /// the resulting state with a `Timeout` transition description.
    ///
    /// Returns whether a timeout was accepted.
    pub async fn apply_due_deadline(&self, now: DeadlineMillis) -> Result<bool, TransportError> {
        let mut service = self.authority.lock().await;
        let Some(deadline) = service.deadline(self.match_id) else {
            return Ok(false);
        };
        if deadline.due_at > now {
            return Ok(false);
        }
        let Some(acknowledgement) = service
            .apply_due_deadline(self.match_id, deadline, now)
            .await
            .map_err(|_| TransportError::Recovery)?
        else {
            return Ok(false);
        };
        self.refresh_head(&service);
        let event = self.state_event(
            &service,
            Some(LastCommand {
                command_id: crate::timeout_command_id(deadline.id),
                actor: deadline.id.player,
                command: VersionedMatchCommand::new(MatchCommand::Timeout),
                playback_ticks: acknowledgement.playback_ticks,
            }),
        )?;
        let _ = self.events.send(MatchEvent::State(event));
        drop(service);
        Ok(true)
    }

    /// The single preview validation helper: the sender must hold the active
    /// seat of an in-progress match at the preview's revision before the turn
    /// deadline, respect the rate limit, and keep a monotonic sequence. Valid
    /// previews are published for the other participant only.
    fn forward_preview(
        &self,
        seat: Player,
        preview: ShotPreview,
        now_ms: u64,
        at: Instant,
    ) -> bool {
        let head = *lock(&self.head);
        if head.revision != preview.revision
            || !head.in_progress
            || head.active_player != seat
            || head
                .deadline
                .is_none_or(|deadline| deadline.due_at.value() <= now_ms)
        {
            return false;
        }
        let mut gate = lock(&self.previews);
        let rate_ok = gate.last.is_none_or(|(last, canceled)| {
            at.saturating_duration_since(last) >= PREVIEW_MIN_INTERVAL
                || (preview.cancel && !canceled)
        });
        let order_ok = gate.order.is_none_or(|(revision, sequence)| {
            revision != preview.revision || preview.sequence > sequence
        });
        if !rate_ok || !order_ok {
            return false;
        }
        gate.last = Some((at, preview.cancel));
        gate.order = Some((preview.revision, preview.sequence));
        let _ = self.events.send(MatchEvent::Preview {
            from: seat,
            preview,
        });
        drop(gate);
        true
    }
}

fn head_of(
    service: &MatchService<SwitchyCommandJournal>,
    match_id: MatchId,
) -> Result<MatchHead, TransportError> {
    let (revision, state) = service
        .match_state(match_id)
        .ok_or(TransportError::MatchNotFound)?;
    Ok(MatchHead {
        revision,
        active_player: state.active_player(),
        in_progress: matches!(state.status(), MatchStatus::InProgress),
        deadline: service.deadline(match_id),
    })
}

const fn reject_reason(error: CommandError) -> RejectReason {
    match error {
        CommandError::StaleRevision { .. } => RejectReason::StaleRevision,
        CommandError::WrongActor => RejectReason::WrongTurn,
        CommandError::MatchCompleted => RejectReason::MatchCompleted,
        CommandError::Domain => RejectReason::IllegalMove,
        CommandError::IdempotencyConflict => RejectReason::IdempotencyConflict,
        CommandError::Unauthorized => RejectReason::NotSubscribed,
        CommandError::MatchNotFound
        | CommandError::InvalidDeadlineState
        | CommandError::RevisionExhausted
        | CommandError::Journal(_) => RejectReason::Unavailable,
    }
}

struct Outbound {
    message: ServerMessage,
    broadcast: bool,
}

/// Handle used to push messages into one connection's bounded outbound queue.
#[derive(Clone)]
pub struct ConnectionHandle {
    outbound: mpsc::Sender<Outbound>,
    close: Arc<Notify>,
}

impl ConnectionHandle {
    /// Enqueues a message; a full queue closes the connection.
    fn push(&self, message: ServerMessage, broadcast: bool) -> bool {
        match self.outbound.try_send(Outbound { message, broadcast }) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.close.notify_one();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}

#[derive(Clone, Copy)]
struct Presence {
    account: AccountId,
}

#[derive(Default)]
struct LobbyLive {
    presences: BTreeMap<ConnectionId, Presence>,
}

/// Process-wide registry of live match units, connections and lobby presence.
#[derive(Default)]
pub struct Hub {
    matches: SyncMutex<BTreeMap<MatchId, Arc<MatchUnit>>>,
    connections: SyncMutex<BTreeMap<AccountId, BTreeMap<ConnectionId, ConnectionHandle>>>,
    lobbies: SyncMutex<BTreeMap<LobbyId, LobbyLive>>,
    lobby_locks: SyncMutex<BTreeMap<LobbyId, Arc<tokio::sync::Mutex<()>>>>,
}

impl Hub {
    pub fn unit(&self, match_id: MatchId) -> Option<Arc<MatchUnit>> {
        lock(&self.matches).get(&match_id).cloned()
    }

    /// Inserts a freshly loaded unit unless another loader won the race.
    fn insert_unit(&self, unit: MatchUnit) -> Arc<MatchUnit> {
        Arc::clone(
            lock(&self.matches)
                .entry(unit.match_id)
                .or_insert_with(|| Arc::new(unit)),
        )
    }

    /// Returns every loaded unit in stable match-identifier order.
    pub fn units(&self) -> Vec<Arc<MatchUnit>> {
        lock(&self.matches).values().cloned().collect()
    }

    pub fn match_ids(&self) -> Vec<MatchId> {
        lock(&self.matches).keys().copied().collect()
    }

    fn register(&self, account: AccountId, connection: ConnectionId, handle: ConnectionHandle) {
        lock(&self.connections)
            .entry(account)
            .or_default()
            .insert(connection, handle);
    }

    fn unregister(&self, account: AccountId, connection: ConnectionId) {
        let mut connections = lock(&self.connections);
        if let Some(handles) = connections.get_mut(&account) {
            handles.remove(&connection);
            if handles.is_empty() {
                connections.remove(&account);
            }
        }
    }

    /// Pushes one notification to every live connection of each account.
    pub fn notify_accounts(&self, accounts: &[AccountId], message: &ServerMessage) {
        let handles = {
            let connections = lock(&self.connections);
            accounts
                .iter()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter_map(|account| connections.get(account))
                .flat_map(|handles| handles.values().cloned())
                .collect::<Vec<_>>()
        };
        for handle in handles {
            handle.push(message.clone(), false);
        }
    }

    /// Returns the per-lobby lock serializing presence, readiness and start.
    fn lobby_lock(&self, lobby_id: LobbyId) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(lock(&self.lobby_locks).entry(lobby_id).or_default())
    }

    /// Drops an empty lobby entry and its unused lock. The emptiness check and
    /// the removal happen under one map lock so a concurrent join cannot lose
    /// its freshly inserted presence.
    fn prune_lobby(&self, lobby_id: LobbyId) {
        let empty = {
            let mut lobbies = lock(&self.lobbies);
            let empty = lobbies
                .get(&lobby_id)
                .is_none_or(|live| live.presences.is_empty());
            if empty {
                lobbies.remove(&lobby_id);
            }
            empty
        };
        if empty {
            let mut locks = lock(&self.lobby_locks);
            if locks
                .get(&lobby_id)
                .is_some_and(|entry| Arc::strong_count(entry) == 1)
            {
                locks.remove(&lobby_id);
            }
        }
    }

    /// Returns the live presence identities of a lobby.
    pub fn lobby_presences(&self, lobby_id: LobbyId) -> BTreeSet<ConnectionId> {
        lock(&self.lobbies)
            .get(&lobby_id)
            .map(|live| live.presences.keys().copied().collect())
            .unwrap_or_default()
    }

    /// Returns one account's live presence identities in a lobby.
    pub fn account_presences(&self, lobby_id: LobbyId, account: AccountId) -> Vec<ConnectionId> {
        lock(&self.lobbies)
            .get(&lobby_id)
            .map(|live| {
                live.presences
                    .iter()
                    .filter(|(_, presence)| presence.account == account)
                    .map(|(id, _)| *id)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns whether an account has at least one live presence in a lobby.
    pub fn account_present(&self, lobby_id: LobbyId, account: AccountId) -> bool {
        !self.account_presences(lobby_id, account).is_empty()
    }
}

impl HttpState {
    /// Returns the loaded authority unit for a match, recovering it from
    /// durable records on first access.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Recovery`] when durable records are absent,
    /// fail integrity validation, or name different participants.
    pub async fn match_unit(
        &self,
        match_id: MatchId,
        participants: Participants,
    ) -> Result<Arc<MatchUnit>, TransportError> {
        if let Some(unit) = self.hub.unit(match_id) {
            return if unit.participants() == participants {
                Ok(unit)
            } else {
                Err(TransportError::Recovery)
            };
        }
        let journal = SwitchyCommandJournal::new(Arc::clone(&self.db));
        let records = crate::CommandJournal::load(&journal, match_id)
            .await
            .map_err(|_| TransportError::Recovery)?;
        let mut service = MatchService::new(SwitchyCommandJournal::new(Arc::clone(&self.db)));
        if records.is_empty() {
            let (stored, state, deadline) = journal
                .initial_match(match_id)
                .await
                .map_err(|_| TransportError::Recovery)?;
            if stored != participants {
                return Err(TransportError::Recovery);
            }
            service
                .load_initial_match(match_id, participants, state, deadline)
                .map_err(|_| TransportError::Recovery)?;
        } else {
            service
                .restore_match(match_id, participants, records)
                .map_err(|_| TransportError::Recovery)?
                .ok_or(TransportError::Recovery)?;
        }
        let unit = self
            .hub
            .insert_unit(MatchUnit::new(match_id, participants, service)?);
        if unit.participants() == participants {
            Ok(unit)
        } else {
            Err(TransportError::Recovery)
        }
    }

    /// Authorizes an account from durable participants and loads the match.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::MatchNotFound`] for unknown matches,
    /// [`TransportError::InvalidSubscription`] for non-participants, and
    /// [`TransportError::Recovery`] for durable integrity failures.
    pub async fn authorized_match_unit(
        &self,
        match_id: MatchId,
        account: AccountId,
    ) -> Result<Arc<MatchUnit>, TransportError> {
        let participants = match self.hub.unit(match_id) {
            Some(unit) => unit.participants(),
            None => SwitchyCommandJournal::new(Arc::clone(&self.db))
                .participants(match_id)
                .await
                .map_err(|_| TransportError::MatchNotFound)?,
        };
        if participants.player_for(account).is_none() {
            return Err(TransportError::InvalidSubscription);
        }
        self.match_unit(match_id, participants).await
    }

    async fn join_lobby(
        &self,
        lobby_id: LobbyId,
        account: AccountId,
        handle: &ConnectionHandle,
    ) -> Result<Option<ConnectionId>, TransportError> {
        let record = crate::load_lobby(&*self.db, lobby_id)
            .await?
            .ok_or(TransportError::SocialNotFound)?;
        if record.participants.player_for(account).is_none() {
            return Err(TransportError::InvalidSubscription);
        }
        let presence = {
            let lobby_lock = self.hub.lobby_lock(lobby_id);
            let _guard = lobby_lock.lock().await;
            let record = crate::load_lobby(&*self.db, lobby_id)
                .await?
                .ok_or(TransportError::SocialNotFound)?;
            match record.status {
                LobbyStatus::Started { match_id } => {
                    handle.push(
                        ServerMessage::MatchStarted {
                            lobby_id: lobby_id.value(),
                            match_id: match_id.value(),
                        },
                        false,
                    );
                    return Ok(None);
                }
                LobbyStatus::Cancelled { .. } => {
                    handle.push(
                        ServerMessage::LobbyChanged {
                            lobby_id: lobby_id.value(),
                        },
                        false,
                    );
                    return Ok(None);
                }
                LobbyStatus::Waiting => {}
            }
            let presence = ConnectionId::new(crate::http::random_u128()?);
            lock(&self.hub.lobbies)
                .entry(lobby_id)
                .or_default()
                .presences
                .insert(presence, Presence { account });
            presence
        };
        self.notify_lobby_changed(record);
        // The presence is live and must reach the session so shutdown releases
        // it; a failed start attempt is retried on the next ready/subscribe.
        if let Err(error) = self.try_start_lobby(lobby_id).await {
            eprintln!("lobby start attempt failed: {error}");
        }
        Ok(Some(presence))
    }

    /// Removes one lobby presence and its readiness row, notifying members.
    async fn leave_lobby(&self, lobby_id: LobbyId, presence: ConnectionId) {
        {
            let lobby_lock = self.hub.lobby_lock(lobby_id);
            let _guard = lobby_lock.lock().await;
            if let Some(live) = lock(&self.hub.lobbies).get_mut(&lobby_id) {
                live.presences.remove(&presence);
            }
            if let Err(error) = crate::release_lobby_presence(&*self.db, presence).await {
                eprintln!("lobby presence release failed: {error}");
            }
        }
        self.hub.prune_lobby(lobby_id);
        if let Ok(Some(record)) = crate::load_lobby(&*self.db, lobby_id).await {
            self.notify_lobby_changed(record);
        }
    }

    pub fn notify_lobby_changed(&self, record: LobbyRecord) {
        self.hub.notify_accounts(
            &[
                record.participants.player_one,
                record.participants.player_two,
            ],
            &ServerMessage::LobbyChanged {
                lobby_id: record.id.value(),
            },
        );
    }

    pub fn notify_social(&self, accounts: &[AccountId]) {
        self.hub
            .notify_accounts(accounts, &ServerMessage::SocialChanged);
    }

    pub fn notify_match_started(
        &self,
        participants: Participants,
        lobby_id: Option<LobbyId>,
        match_id: MatchId,
    ) {
        let accounts = [participants.player_one, participants.player_two];
        self.hub.notify_accounts(
            &accounts,
            &ServerMessage::MatchStarted {
                lobby_id: lobby_id.map_or(0, LobbyId::value),
                match_id: match_id.value(),
            },
        );
        self.notify_social(&accounts);
    }

    /// Records explicit readiness for every live presence of `actor`, then
    /// starts the match atomically if both participants are present and ready.
    ///
    /// # Errors
    ///
    /// Returns [`TransportError::Lobby`] for missing, consumed, unauthorized,
    /// unconnected (no live presence), malformed, or database lobby failures,
    /// and [`TransportError::Internal`] for clock or randomness failures.
    pub async fn ready_lobby_member(
        &self,
        lobby_id: LobbyId,
        actor: AccountId,
    ) -> Result<LobbyRecord, TransportError> {
        let outcome = self.ready_lobby_locked(lobby_id, actor).await;
        // Every exit path drops an unused lock entry, so refused requests for
        // arbitrary lobby identifiers cannot grow the lock map.
        self.hub.prune_lobby(lobby_id);
        let (record, started) = outcome?;
        if started {
            self.after_lobby_started(record).await;
        } else {
            self.notify_lobby_changed(record);
        }
        Ok(record)
    }

    async fn ready_lobby_locked(
        &self,
        lobby_id: LobbyId,
        actor: AccountId,
    ) -> Result<(LobbyRecord, bool), TransportError> {
        let lobby_lock = self.hub.lobby_lock(lobby_id);
        let _guard = lobby_lock.lock().await;
        let presences = self.hub.account_presences(lobby_id, actor);
        let record = crate::ready_lobby(
            &*self.db,
            lobby_id,
            actor,
            &presences,
            crate::http::unix_millis()?,
        )
        .await?;
        Ok(self
            .start_if_ready_locked(record)
            .await?
            .map_or((record, false), |started| (started, true)))
    }

    async fn try_start_lobby(&self, lobby_id: LobbyId) -> Result<(), TransportError> {
        let started = {
            let lobby_lock = self.hub.lobby_lock(lobby_id);
            let _guard = lobby_lock.lock().await;
            let Some(record) = crate::load_lobby(&*self.db, lobby_id).await? else {
                return Ok(());
            };
            if record.status != LobbyStatus::Waiting {
                return Ok(());
            }
            self.start_if_ready_locked(record).await?
        };
        if let Some(record) = started {
            self.after_lobby_started(record).await;
        }
        Ok(())
    }

    /// Starts a waiting lobby when both participants have a live, ready
    /// presence. The caller holds the lobby lock.
    async fn start_if_ready_locked(
        &self,
        record: LobbyRecord,
    ) -> Result<Option<LobbyRecord>, TransportError> {
        let live = self.hub.lobby_presences(record.id);
        if !crate::lobby_ready(&*self.db, record.id, &live).await? {
            return Ok(None);
        }
        let now = crate::http::unix_millis()?;
        let state = pwmtf_eight_ball::MatchState::new(
            pwmtf_eight_ball::RulesProfile::standard(),
            pwmtf_billiards::PhysicsProfile::standard(),
            pwmtf_billiards::TableGeometry::standard(),
            pwmtf_billiards::RackSeed::new(crate::http::random_u64()?),
            Player::One,
        )
        .map_err(|_| TransportError::Internal)?;
        let deadline = ScheduledDeadline {
            id: crate::DeadlineId {
                revision: 0,
                player: state.active_player(),
            },
            due_at: DeadlineMillis::after_turn(DeadlineMillis::new(now), state.rules()),
        };
        let match_id = MatchId::new(crate::http::random_u128()?);
        let started =
            crate::start_ready_lobby(&*self.db, record.id, match_id, &state, deadline, &live)
                .await?;
        lock(&self.hub.lobbies).remove(&record.id);
        Ok(Some(started))
    }

    async fn after_lobby_started(&self, record: LobbyRecord) {
        let LobbyStatus::Started { match_id } = record.status else {
            return;
        };
        if let Err(error) = self.match_unit(match_id, record.participants).await {
            eprintln!("started match failed to load: {error}");
        }
        if let Err(error) = crate::clear_lobby_readiness(&*self.db, record.id).await {
            eprintln!("started lobby readiness cleanup failed: {error}");
        }
        self.hub.prune_lobby(record.id);
        self.notify_match_started(record.participants, Some(record.id), match_id);
    }

    /// Drops every in-memory presence of a lobby after it stops waiting.
    pub async fn retire_lobby_presence(&self, lobby_id: LobbyId) {
        lock(&self.hub.lobbies).remove(&lobby_id);
        if let Err(error) = crate::clear_lobby_readiness(&*self.db, lobby_id).await {
            eprintln!("retired lobby readiness cleanup failed: {error}");
        }
        self.hub.prune_lobby(lobby_id);
    }
}

struct MatchSubscription {
    unit: Arc<MatchUnit>,
    seat: Player,
    forwarder: tokio::task::JoinHandle<()>,
}

struct Session {
    account: AccountId,
    connection: ConnectionId,
    handle: ConnectionHandle,
    matches: BTreeMap<MatchId, MatchSubscription>,
    lobbies: BTreeMap<LobbyId, Option<ConnectionId>>,
    delivered: BTreeMap<MatchId, u64>,
}

impl Session {
    fn push(&self, message: ServerMessage) -> bool {
        self.handle.push(message, false)
    }

    async fn handle_message(&mut self, state: &HttpState, message: ClientMessage) -> bool {
        match message {
            ClientMessage::Hello { .. } => false,
            ClientMessage::Ping { nonce } => crate::http::unix_millis().is_ok_and(|now| {
                self.push(ServerMessage::Pong {
                    nonce,
                    server_now_ms: now,
                })
            }),
            ClientMessage::SubscribeMatch { match_id } => {
                self.subscribe_match(state, MatchId::new(match_id)).await
            }
            ClientMessage::UnsubscribeMatch { match_id } => {
                let match_id = MatchId::new(match_id);
                if let Some(subscription) = self.matches.remove(&match_id) {
                    subscription.forwarder.abort();
                }
                self.delivered.remove(&match_id);
                true
            }
            ClientMessage::SubscribeLobby { lobby_id } => {
                self.subscribe_lobby(state, LobbyId::new(lobby_id)).await
            }
            ClientMessage::UnsubscribeLobby { lobby_id } => {
                let lobby_id = LobbyId::new(lobby_id);
                if let Some(Some(presence)) = self.lobbies.remove(&lobby_id) {
                    state.leave_lobby(lobby_id, presence).await;
                }
                true
            }
            ClientMessage::Command { match_id, envelope } => {
                self.command(MatchId::new(match_id), envelope).await
            }
            ClientMessage::Preview { match_id, preview } => {
                if let Some(subscription) = self.matches.get(&MatchId::new(match_id))
                    && let Ok(now_ms) = crate::http::unix_millis()
                {
                    subscription.unit.forward_preview(
                        subscription.seat,
                        preview,
                        now_ms,
                        Instant::now(),
                    );
                }
                true
            }
        }
    }

    async fn subscribe_match(&mut self, state: &HttpState, match_id: MatchId) -> bool {
        if let Some(subscription) = self.matches.get(&match_id) {
            return subscription
                .unit
                .current_state()
                .await
                .is_ok_and(|event| self.push(event.message(match_id, subscription.seat)));
        }
        let denied = ServerMessage::SubscriptionDenied {
            kind: SubscriptionKind::Match,
            id: match_id.value(),
        };
        if self.matches.len() >= MAX_MATCH_SUBSCRIPTIONS {
            return self.push(denied);
        }
        let Ok(unit) = state.authorized_match_unit(match_id, self.account).await else {
            return self.push(denied);
        };
        let Some(seat) = unit.participants().player_for(self.account) else {
            return self.push(denied);
        };
        let Ok((receiver, event)) = unit.subscribe().await else {
            return false;
        };
        if !self.push(event.message(match_id, seat)) {
            return false;
        }
        let forwarder = tokio::spawn(forward_match_events(
            Arc::clone(&unit),
            seat,
            receiver,
            self.handle.clone(),
        ));
        self.matches.insert(
            match_id,
            MatchSubscription {
                unit,
                seat,
                forwarder,
            },
        );
        true
    }

    async fn subscribe_lobby(&mut self, state: &HttpState, lobby_id: LobbyId) -> bool {
        let denied = ServerMessage::SubscriptionDenied {
            kind: SubscriptionKind::Lobby,
            id: lobby_id.value(),
        };
        if let Some(Some(_)) = self.lobbies.get(&lobby_id) {
            return self.push(ServerMessage::LobbyChanged {
                lobby_id: lobby_id.value(),
            });
        }
        if !self.lobbies.contains_key(&lobby_id) && self.lobbies.len() >= MAX_LOBBY_SUBSCRIPTIONS {
            return self.push(denied);
        }
        match state.join_lobby(lobby_id, self.account, &self.handle).await {
            Ok(presence) => {
                self.lobbies.insert(lobby_id, presence);
                true
            }
            Err(
                TransportError::SocialNotFound
                | TransportError::InvalidSubscription
                | TransportError::Lobby(_),
            ) => self.push(denied),
            Err(_) => false,
        }
    }

    async fn command(&self, match_id: MatchId, envelope: CommandEnvelope) -> bool {
        let Some(subscription) = self.matches.get(&match_id) else {
            return self.push(ServerMessage::Rejected {
                match_id: match_id.value(),
                command_id: envelope.command_id,
                reason: RejectReason::NotSubscribed,
            });
        };
        let Ok(now) = crate::http::unix_millis() else {
            return false;
        };
        let seat = subscription.seat;
        match subscription
            .unit
            .apply(self.account, envelope, DeadlineMillis::new(now))
            .await
        {
            // Marked as a broadcast so the forwarder's copy is deduplicated
            // by the per-match revision filter.
            Ok(CommandOutcome::Accepted(event)) => {
                self.handle.push(event.message(match_id, seat), true)
            }
            Ok(CommandOutcome::Duplicate(event)) => self.push(event.message(match_id, seat)),
            Ok(CommandOutcome::Rejected(reason, event)) => {
                self.push(ServerMessage::Rejected {
                    match_id: match_id.value(),
                    command_id: envelope.command_id,
                    reason,
                }) && self.push(event.message(match_id, seat))
            }
            Err(_) => false,
        }
    }

    /// Returns whether an outbound message should be written, applying
    /// subscription filtering and per-match revision monotonicity for
    /// broadcast states (direct replies are always written).
    fn admit(&mut self, outbound: &Outbound) -> bool {
        match &outbound.message {
            ServerMessage::MatchState(state) => {
                let match_id = MatchId::new(state.match_id);
                if !self.matches.contains_key(&match_id) {
                    return false;
                }
                let delivered = self.delivered.get(&match_id).copied();
                if outbound.broadcast
                    && delivered.is_some_and(|revision| state.revision <= revision)
                {
                    return false;
                }
                self.delivered.insert(
                    match_id,
                    delivered.map_or(state.revision, |revision| revision.max(state.revision)),
                );
                true
            }
            ServerMessage::Preview { match_id, .. } => {
                self.matches.contains_key(&MatchId::new(*match_id))
            }
            _ => true,
        }
    }

    async fn shutdown(self, state: &HttpState) {
        for subscription in self.matches.into_values() {
            subscription.forwarder.abort();
        }
        for (lobby_id, presence) in self.lobbies {
            if let Some(presence) = presence {
                state.leave_lobby(lobby_id, presence).await;
            }
        }
        state.hub.unregister(self.account, self.connection);
    }
}

async fn forward_match_events(
    unit: Arc<MatchUnit>,
    seat: Player,
    mut receiver: broadcast::Receiver<MatchEvent>,
    handle: ConnectionHandle,
) {
    loop {
        let message = match receiver.recv().await {
            Ok(MatchEvent::State(event)) => event.message(unit.match_id, seat),
            Ok(MatchEvent::Preview { from, preview }) if from != seat => ServerMessage::Preview {
                match_id: unit.match_id.value(),
                preview,
            },
            Ok(MatchEvent::Preview { .. }) => continue,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let Ok(event) = unit.current_state().await else {
                    handle.close.notify_one();
                    break;
                };
                event.message(unit.match_id, seat)
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        if !handle.push(message, true) {
            break;
        }
    }
}

async fn send(socket: &mut WebSocket, message: &ServerMessage) -> bool {
    match message.to_bytes() {
        Ok(bytes) => socket.send(Message::Binary(bytes.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn close(mut socket: WebSocket) {
    let _ = tokio::time::timeout(CLOSE_TIMEOUT, socket.send(Message::Close(None))).await;
}

/// Waits for the mandatory `Hello` and returns the negotiated version.
async fn handshake(socket: &mut WebSocket) -> Option<u16> {
    let deadline = tokio::time::Instant::now() + HELLO_TIMEOUT;
    loop {
        let message = tokio::time::timeout_at(deadline, socket.recv())
            .await
            .ok()??
            .ok()?;
        match message {
            Message::Binary(bytes) => {
                let ClientMessage::Hello { versions } = ClientMessage::from_bytes(&bytes).ok()?
                else {
                    return None;
                };
                return negotiate_version(&versions, &[SESSION_VERSION]).ok();
            }
            Message::Ping(_) | Message::Pong(_) => {}
            Message::Text(_) | Message::Close(_) => return None,
        }
    }
}

/// Runs one authenticated `/ws` session until it closes.
pub async fn run_session(
    state: Arc<HttpState>,
    mut socket: WebSocket,
    account: AccountId,
    connection: ConnectionId,
) {
    let Some(version) = handshake(&mut socket).await else {
        close(socket).await;
        return;
    };
    let Ok(server_now_ms) = crate::http::unix_millis() else {
        close(socket).await;
        return;
    };
    if !send(
        &mut socket,
        &ServerMessage::Welcome {
            version,
            server_now_ms,
        },
    )
    .await
    {
        return;
    }
    let (outbound, mut queue) = mpsc::channel(OUTBOUND_CAPACITY);
    let close_signal = Arc::new(Notify::new());
    let handle = ConnectionHandle {
        outbound,
        close: Arc::clone(&close_signal),
    };
    state.hub.register(account, connection, handle.clone());
    let mut session = Session {
        account,
        connection,
        handle,
        matches: BTreeMap::new(),
        lobbies: BTreeMap::new(),
        delivered: BTreeMap::new(),
    };
    let mut last_inbound = Instant::now();
    let mut idle_check = tokio::time::interval(IDLE_CHECK_INTERVAL);
    loop {
        tokio::select! {
            () = close_signal.notified() => break,
            outbound = queue.recv() => {
                let Some(outbound) = outbound else { break };
                if session.admit(&outbound) && !send(&mut socket, &outbound.message).await {
                    break;
                }
            }
            _ = idle_check.tick() => {
                if last_inbound.elapsed() > IDLE_TIMEOUT {
                    break;
                }
            }
            message = socket.recv() => {
                let Some(Ok(message)) = message else { break };
                last_inbound = Instant::now();
                match message {
                    Message::Binary(bytes) => {
                        let Ok(message) = ClientMessage::from_bytes(&bytes) else { break };
                        if !session.handle_message(&state, message).await {
                            break;
                        }
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    Message::Text(_) | Message::Close(_) => break,
                }
            }
        }
    }
    session.shutdown(&state).await;
    close(socket).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_errors_map_to_stable_reject_reasons() {
        assert_eq!(
            reject_reason(CommandError::StaleRevision {
                expected: 1,
                actual: 2,
            }),
            RejectReason::StaleRevision
        );
        assert_eq!(
            reject_reason(CommandError::WrongActor),
            RejectReason::WrongTurn
        );
        assert_eq!(
            reject_reason(CommandError::MatchCompleted),
            RejectReason::MatchCompleted
        );
        assert_eq!(
            reject_reason(CommandError::Domain),
            RejectReason::IllegalMove
        );
        assert_eq!(
            reject_reason(CommandError::IdempotencyConflict),
            RejectReason::IdempotencyConflict
        );
        assert_eq!(
            reject_reason(CommandError::Journal(crate::JournalError)),
            RejectReason::Unavailable
        );
        assert_eq!(
            reject_reason(CommandError::RevisionExhausted),
            RejectReason::Unavailable
        );
    }

    #[test]
    fn full_outbound_queue_closes_the_connection() {
        let (outbound, _queue) = mpsc::channel(1);
        let close = Arc::new(Notify::new());
        let handle = ConnectionHandle {
            outbound,
            close: Arc::clone(&close),
        };
        assert!(handle.push(ServerMessage::SocialChanged, false));
        assert!(!handle.push(ServerMessage::SocialChanged, false));
        tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap()
            .block_on(async {
                tokio::time::timeout(Duration::from_secs(1), close.notified())
                    .await
                    .unwrap();
            });
    }
}
