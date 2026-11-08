//! The authoritative side: revision-checked, idempotent command acceptance.
//!
//! Acceptance is split into [`Authority::submit`], which computes the result
//! without changing anything, and [`Authority::commit`], which publishes it.
//! A host that must make commands durable before acknowledging them persists
//! the [`Proposal`] between the two; an in-memory host commits immediately.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::game::{Applied, Authorization, ReplayGame};

/// An opaque, client-chosen idempotency key for one command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandKey(pub [u8; 16]);

/// The authoritative turn deadline: who must act by when.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadline<S> {
    /// Revision the deadline belongs to; it lapses if the revision moves on.
    pub revision: u64,
    /// The seat that must act.
    pub seat: S,
    /// When the turn expires, in the host's millisecond clock.
    pub due_at_ms: u64,
}

/// An accepted command and its acknowledgement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Accepted<G: ReplayGame> {
    /// The seat that issued the command.
    pub seat: G::Seat,
    /// The command.
    pub command: G::Command,
    /// Revision after the command.
    pub revision: u64,
    /// State checksum after the command.
    pub checksum: u64,
    /// What the command produced.
    pub outcome: G::Outcome,
    /// Fixed simulation ticks of motion the command plays back.
    pub playback_ticks: u32,
    /// The next turn deadline, starting after the motion ends.
    pub deadline: Option<Deadline<G::Seat>>,
}

/// A computed, not yet published, command acceptance.
#[derive(Clone, Debug)]
pub struct Proposal<G: ReplayGame> {
    /// The command's idempotency key.
    pub key: CommandKey,
    /// The resulting state.
    pub state: G,
    /// The acknowledgement to publish.
    pub accepted: Accepted<G>,
}

/// The result of submitting a command.
#[derive(Clone, Debug)]
pub enum Submission<G: ReplayGame> {
    /// A new command to persist (if the host requires durability) and commit.
    New(Box<Proposal<G>>),
    /// An identical retransmission of an already accepted command.
    Duplicate(Accepted<G>),
}

/// Why the authority refused a command. The state is unchanged.
#[derive(Debug, Error)]
pub enum Refusal<E> {
    /// The command was created against a different revision.
    #[error("stale revision: expected {expected}, actual {actual}")]
    StaleRevision {
        /// Revision in the command.
        expected: u64,
        /// Current revision.
        actual: u64,
    },
    /// The game has ended.
    #[error("the game has ended")]
    Ended,
    /// The seat may not issue this command now.
    #[error("seat may not issue this command: {0:?}")]
    Unauthorized(Authorization),
    /// The key was already used for a different command, seat or revision.
    #[error("idempotency key reused for a different command")]
    KeyConflict,
    /// The game rejected the command.
    #[error("the game rejected the command: {0:?}")]
    Rejected(E),
    /// The revision counter is exhausted.
    #[error("revision space exhausted")]
    RevisionExhausted,
}

/// Recovery found inconsistent records.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RestoreError {
    /// A deadline does not belong to the restored revision and active seat.
    #[error("deadline does not match the restored revision and active seat")]
    InvalidDeadline,
    /// The game has ended but a deadline remains.
    #[error("an ended game retains a deadline")]
    DeadlineAfterEnd,
}

/// Authoritative state of one game.
#[derive(Clone, Debug)]
pub struct Authority<G: ReplayGame> {
    revision: u64,
    state: G,
    deadline: Option<Deadline<G::Seat>>,
    accepted: BTreeMap<CommandKey, Accepted<G>>,
}

impl<G: ReplayGame> Authority<G> {
    /// Starts a new game at revision zero, scheduling its first turn's
    /// deadline from `now_ms` when turns are timed.
    #[must_use]
    pub fn open(state: G, now_ms: u64) -> Self {
        let deadline = next_deadline(&state, 0, now_ms, 0);
        Self {
            revision: 0,
            state,
            deadline,
            accepted: BTreeMap::new(),
        }
    }

    /// Restores a game from durable records: its current state, revision and
    /// deadline, and every previously accepted command for idempotency.
    ///
    /// # Errors
    ///
    /// Returns [`RestoreError`] when the deadline is inconsistent with the
    /// state.
    pub fn restore(
        state: G,
        revision: u64,
        deadline: Option<Deadline<G::Seat>>,
        accepted: impl IntoIterator<Item = (CommandKey, Accepted<G>)>,
    ) -> Result<Self, RestoreError> {
        if let Some(deadline) = deadline {
            if !state.in_progress() {
                return Err(RestoreError::DeadlineAfterEnd);
            }
            if deadline.revision != revision || state.active_seat() != Some(deadline.seat) {
                return Err(RestoreError::InvalidDeadline);
            }
        }
        Ok(Self {
            revision,
            state,
            deadline,
            accepted: accepted.into_iter().collect(),
        })
    }

    /// The current revision.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// The current authoritative state.
    #[must_use]
    pub const fn state(&self) -> &G {
        &self.state
    }

    /// The current turn deadline.
    #[must_use]
    pub const fn deadline(&self) -> Option<Deadline<G::Seat>> {
        self.deadline
    }

    /// A previously accepted command, by key.
    #[must_use]
    pub fn accepted(&self, key: &CommandKey) -> Option<&Accepted<G>> {
        self.accepted.get(key)
    }

    /// Computes the result of `seat` issuing `command` without changing any
    /// state. `now_ms` schedules the next deadline after the command's motion.
    ///
    /// # Errors
    ///
    /// Returns [`Refusal`] when the command cannot be accepted.
    pub fn submit(
        &self,
        key: CommandKey,
        seat: G::Seat,
        expected_revision: u64,
        command: G::Command,
        now_ms: u64,
    ) -> Result<Submission<G>, Refusal<G::Error>> {
        if let Some(accepted) = self.accepted.get(&key) {
            let same_revision = accepted.revision == expected_revision.wrapping_add(1);
            return if accepted.seat == seat && accepted.command == command && same_revision {
                Ok(Submission::Duplicate(accepted.clone()))
            } else {
                Err(Refusal::KeyConflict)
            };
        }
        if expected_revision != self.revision {
            return Err(Refusal::StaleRevision {
                expected: expected_revision,
                actual: self.revision,
            });
        }
        if !self.state.in_progress() {
            return Err(Refusal::Ended);
        }
        self.state
            .authorize(seat, &command)
            .map_err(Refusal::Unauthorized)?;
        let mut state = self.state.clone();
        let Applied {
            outcome,
            playback_ticks,
        } = state.apply(command).map_err(Refusal::Rejected)?;
        let revision = self
            .revision
            .checked_add(1)
            .ok_or(Refusal::RevisionExhausted)?;
        let deadline = next_deadline(&state, revision, now_ms, playback_ticks);
        let checksum = state.checksum();
        Ok(Submission::New(Box::new(Proposal {
            key,
            state,
            accepted: Accepted {
                seat,
                command,
                revision,
                checksum,
                outcome,
                playback_ticks,
                deadline,
            },
        })))
    }

    /// The timeout to submit when the current deadline is due at `now_ms`.
    #[must_use]
    pub fn due_timeout(&self, now_ms: u64) -> Option<(G::Seat, G::Command)> {
        self.deadline
            .filter(|deadline| deadline.due_at_ms <= now_ms && deadline.revision == self.revision)
            .map(|deadline| (deadline.seat, self.state.timeout_command()))
    }

    /// Publishes a proposal computed from the current revision.
    ///
    /// Proposals are only valid for the revision they were computed from; a
    /// host must not commit two proposals from the same revision.
    pub fn commit(&mut self, proposal: Proposal<G>) {
        debug_assert_eq!(proposal.accepted.revision, self.revision + 1);
        self.revision = proposal.accepted.revision;
        self.state = proposal.state;
        self.deadline = proposal.accepted.deadline;
        self.accepted.insert(proposal.key, proposal.accepted);
    }
}

/// The deadline for the turn after a command whose motion lasts
/// `playback_ticks`: the turn starts once the motion has played back.
fn next_deadline<G: ReplayGame>(
    state: &G,
    revision: u64,
    now_ms: u64,
    playback_ticks: u32,
) -> Option<Deadline<G::Seat>> {
    let duration = state.turn_duration_millis()?;
    let seat = state.active_seat().filter(|_| state.in_progress())?;
    Some(Deadline {
        revision,
        seat,
        due_at_ms: now_ms
            .saturating_add(state.playback_millis(playback_ticks))
            .saturating_add(duration),
    })
}
