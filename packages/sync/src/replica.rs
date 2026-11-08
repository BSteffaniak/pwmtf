//! The client side: authoritative state, one predicted command, and playback.
//!
//! A replica never decides outcomes. It mirrors the authority's states,
//! predicts the local player's single pending command, and replays every
//! accepted command's motion with the same deterministic game code, so the
//! shooter sees their shot at once and observers see it in full.

use thiserror::Error;

use crate::authority::CommandKey;
use crate::game::{Playback, ReplayGame};

/// Longest stretch of playback advanced in one call, in milliseconds. A slow
/// frame delays motion slightly instead of skipping its start.
pub const MAX_PLAYBACK_STEP_MS: f64 = 50.0;

/// The local player's command awaiting the authority.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Pending<C> {
    /// The command's idempotency key; retransmissions reuse it.
    pub key: CommandKey,
    /// The command.
    pub command: C,
    /// The revision it was created against.
    pub expected_revision: u64,
    resolved: bool,
    resend_on_state: bool,
}

/// The authority's description of the transition into a received state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Remote<S, C> {
    /// The command's idempotency key.
    pub key: CommandKey,
    /// The seat that issued it.
    pub seat: S,
    /// The command.
    pub command: C,
    /// Fixed simulation ticks of motion it plays back.
    pub playback_ticks: u32,
}

/// An accepted transition, reported to the host once its motion has played.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Transition<G: ReplayGame> {
    /// Revision after the transition.
    pub revision: u64,
    /// The state before it.
    pub before: G,
    /// The seat that issued it.
    pub seat: G::Seat,
    /// The command.
    pub command: G::Command,
}

/// What receiving an authoritative state changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Received<C> {
    /// The revision advanced.
    pub revision_changed: bool,
    /// The pending command was dropped because authority moved on without it.
    pub superseded: bool,
    /// The pending command must be retransmitted (same key) now.
    pub resend: Option<Pending<C>>,
}

impl<C> Default for Received<C> {
    fn default() -> Self {
        Self {
            revision_changed: false,
            superseded: false,
            resend: None,
        }
    }
}

/// Why a command could not be submitted.
#[derive(Debug, Error)]
pub enum ReplicaError<E> {
    /// A command is already awaiting the authority.
    #[error("a command is already pending")]
    PendingCommand,
    /// The game has ended.
    #[error("the game has ended")]
    Ended,
    /// Local prediction rejected the command.
    #[error("local prediction rejected the command: {0:?}")]
    Rejected(E),
}

/// Client-side mirror of one game.
#[derive(Clone, Debug)]
pub struct Replica<G: ReplayGame> {
    revision: u64,
    authoritative: G,
    predicted: G,
    pending: Option<Pending<G::Command>>,
    playback: Option<G::Playback>,
    playback_clock_ms: Option<f64>,
    playback_ended: bool,
    unsettled: Option<Transition<G>>,
    settled: Option<Transition<G>>,
}

impl<G: ReplayGame> Replica<G> {
    /// Mirrors an authoritative state.
    #[must_use]
    pub fn new(revision: u64, state: G) -> Self {
        Self {
            revision,
            predicted: state.clone(),
            authoritative: state,
            pending: None,
            playback: None,
            playback_clock_ms: None,
            playback_ended: false,
            unsettled: None,
            settled: None,
        }
    }

    /// The last authoritative revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The last authoritative state.
    #[must_use]
    pub const fn authoritative(&self) -> &G {
        &self.authoritative
    }

    /// The state to present when nothing is playing: the authoritative state
    /// with the pending command applied once resolved.
    #[must_use]
    pub const fn predicted(&self) -> &G {
        &self.predicted
    }

    /// The command awaiting the authority, if any.
    #[must_use]
    pub const fn pending(&self) -> Option<&Pending<G::Command>> {
        self.pending.as_ref()
    }

    /// Whether the pending command has not been resolved locally yet.
    #[must_use]
    pub fn has_unresolved(&self) -> bool {
        self.pending.is_some_and(|pending| !pending.resolved)
    }

    /// The motion currently playing back, if any.
    #[must_use]
    pub const fn playback(&self) -> Option<&G::Playback> {
        self.playback.as_ref()
    }

    /// Submits the local player's command: it is predicted (or deferred, see
    /// [`ReplayGame::predict_immediately`]) and its motion starts playing.
    /// Returns the revision to send it against.
    ///
    /// # Errors
    ///
    /// Returns [`ReplicaError`] when a command is pending, the game ended, or
    /// local prediction rejects the command.
    pub fn submit(
        &mut self,
        key: CommandKey,
        command: G::Command,
    ) -> Result<u64, ReplicaError<G::Error>> {
        if self.pending.is_some() {
            return Err(ReplicaError::PendingCommand);
        }
        if !self.predicted.in_progress() {
            return Err(ReplicaError::Ended);
        }
        let resolved = self.authoritative.predict_immediately(&command);
        if resolved {
            let mut predicted = self.predicted.clone();
            predicted.apply(command).map_err(ReplicaError::Rejected)?;
            self.predicted = predicted;
        }
        self.playback = self
            .authoritative
            .start_playback(&command, None)
            .filter(|playback| !playback.is_finished());
        self.playback_clock_ms = None;
        self.pending = Some(Pending {
            key,
            command,
            expected_revision: self.revision,
            resolved,
            resend_on_state: false,
        });
        Ok(self.revision)
    }

    /// Marks the pending command for retransmission once the next state at
    /// its revision proves it was not applied (call after a reconnect).
    pub const fn request_resend(&mut self) {
        if let Some(pending) = self.pending.as_mut() {
            pending.resend_on_state = true;
        }
    }

    /// Drops the pending command and any playback after a rejection or a
    /// lost connection. Returns whether playback was interrupted.
    pub fn abandon(&mut self) -> bool {
        self.pending = None;
        self.predicted = self.authoritative.clone();
        let stopped = self.playback.take().is_some();
        if stopped {
            self.playback_ended = true;
        }
        stopped
    }

    /// Applies an authoritative state and the transition that produced it.
    pub fn receive(
        &mut self,
        revision: u64,
        state: G,
        last: Option<Remote<G::Seat, G::Command>>,
    ) -> Received<G::Command> {
        let mut received = Received::default();
        match revision.cmp(&self.revision) {
            // A delayed state from before a later broadcast.
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Equal => {
                if state.checksum() != self.authoritative.checksum() {
                    // Authority replaced this revision: adopt it outright.
                    received.superseded = self.pending.take().is_some();
                    if self.playback.take().is_some() {
                        self.playback_ended = true;
                    }
                    self.authoritative = state.clone();
                    self.predicted = state;
                    received.revision_changed = true;
                }
                if let Some(pending) = self.pending.as_mut()
                    && pending.resend_on_state
                    && pending.expected_revision == revision
                {
                    pending.resend_on_state = false;
                    received.resend = Some(*pending);
                }
            }
            std::cmp::Ordering::Greater => {
                received.revision_changed = true;
                let previous = self.revision;
                self.unsettled = last
                    .filter(|_| revision == previous + 1)
                    .map(|last| Transition {
                        revision,
                        before: self.authoritative.clone(),
                        seat: last.seat,
                        command: last.command,
                    });
                // Without the transition's key, recognizing our own command
                // needs its locally resolved outcome.
                if last.is_none() {
                    self.resolve_pending();
                }
                let ours = self.pending.is_some_and(|pending| {
                    last.map_or_else(
                        || {
                            revision == pending.expected_revision + 1
                                && self.predicted.checksum() == state.checksum()
                        },
                        |last| last.key == pending.key,
                    )
                });
                if self.pending.is_some() && !ours {
                    received.superseded = true;
                    self.pending = None;
                    if self.playback.take().is_some() {
                        self.playback_ended = true;
                    }
                }
                if ours {
                    // The submit-time playback shows exactly this transition.
                    self.pending = None;
                } else if revision == previous + 1
                    && let Some(last) = last
                    && last.playback_ticks > 0
                {
                    self.playback = self
                        .authoritative
                        .start_playback(&last.command, Some(last.playback_ticks))
                        .filter(|playback| !playback.is_finished());
                    self.playback_clock_ms = None;
                    if self.playback.is_none() {
                        self.playback_ended = true;
                    }
                } else if self.playback.take().is_some() {
                    // A gap or motionless transition: never keep drawing an
                    // older replay over newer authority.
                    self.playback_ended = true;
                }
                self.revision = revision;
                self.authoritative = state.clone();
                self.predicted = state;
            }
        }
        if self.playback.is_none() {
            self.settle();
        }
        received
    }

    /// Advances playback to monotonic time `now_ms`. Returns `true` once when
    /// playback ends or is interrupted, so presentation can snap to state.
    pub fn advance(&mut self, now_ms: f64) -> bool {
        let elapsed = self.playback_clock_ms.map_or(0.0, |previous| {
            (now_ms - previous).clamp(0.0, MAX_PLAYBACK_STEP_MS)
        });
        self.playback_clock_ms = Some(now_ms);
        if let Some(playback) = self.playback.as_mut()
            && playback.advance(elapsed / 1_000.0)
        {
            self.playback = None;
            self.playback_ended = true;
            self.settle();
        }
        std::mem::take(&mut self.playback_ended)
    }

    /// Takes the most recent transition whose motion has finished playing.
    pub const fn take_settled(&mut self) -> Option<Transition<G>> {
        self.settled.take()
    }

    /// Work deferred while motion played: report the transition, and resolve
    /// the pending command if the authority has not answered yet.
    fn settle(&mut self) {
        if let Some(transition) = self.unsettled.take() {
            self.settled = Some(transition);
        }
        self.resolve_pending();
    }

    fn resolve_pending(&mut self) {
        if let Some(pending) = self.pending.as_mut()
            && !pending.resolved
        {
            pending.resolved = true;
            let mut predicted = self.authoritative.clone();
            if predicted.apply(pending.command).is_ok() {
                self.predicted = predicted;
            }
        }
    }
}
