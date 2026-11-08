//! The authoritative side of tick synchronization.
//!
//! # Input window policy
//!
//! The authority's current tick `T` is the next tick it will simulate. An
//! input stamped for tick `t` from seat `s` is:
//!
//! - **late** when `t < T`: the tick was already simulated, so the input is
//!   dropped and refused with [`InputRefusal::Late`]. The seat's previous
//!   input was repeated for that tick and the frame says so; clients roll
//!   back to it.
//! - **too early** when `t >= T + input_window`: refused with
//!   [`InputRefusal::TooEarly`], bounding buffered memory to
//!   `input_window` ticks per seat.
//! - **accepted** otherwise, and buffered until tick `t` is simulated.
//!
//! The first input received for a `(seat, tick)` wins. An identical
//! retransmission is reported as [`InputReceipt::Duplicate`]; a different
//! input for the same slot is refused with [`InputRefusal::Conflict`].
//! When tick `T` is simulated, a seated seat with no buffered input repeats
//! the last input it contributed (its neutral input right after joining).

use std::collections::{BTreeMap, VecDeque};

use thiserror::Error;

use super::{Membership, TickFrame, TickGame, TickSnapshot};

type Frame<G> = TickFrame<<G as TickGame>::Seat, <G as TickGame>::Input>;

/// Tunable bounds for a [`TickAuthority`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickAuthorityConfig {
    /// How many ticks, starting at the current one, accept inputs. Must be
    /// at least one (and at most 65 536); larger values tolerate players
    /// further ahead.
    pub input_window: u64,
    /// A frame carries a checksum when its resulting tick is a multiple of
    /// this; `1` checksums every frame. Must be at least one.
    pub checksum_interval: u64,
    /// How many recent frames are kept for retransmission.
    pub frame_history: usize,
}

impl Default for TickAuthorityConfig {
    /// Two seconds of input window at 60 Hz, a checksum every half second at
    /// 60 Hz, and four seconds of retransmittable frames at 60 Hz.
    fn default() -> Self {
        Self {
            input_window: 120,
            checksum_interval: 30,
            frame_history: 240,
        }
    }
}

/// An input the authority accepted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InputReceipt {
    /// A new input, buffered for its tick.
    Accepted,
    /// An identical retransmission of an already buffered input.
    Duplicate,
}

/// Why the authority refused an input. Nothing changed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum InputRefusal {
    /// The tick was already simulated; the seat's previous input was used.
    #[error("input for tick {tick} is late; the authority is at tick {current}")]
    Late {
        /// The input's tick.
        tick: u64,
        /// The authority's current tick.
        current: u64,
    },
    /// The tick is beyond the input window.
    #[error("input for tick {tick} is too far ahead; ticks before {limit} are accepted")]
    TooEarly {
        /// The input's tick.
        tick: u64,
        /// The first tick refused.
        limit: u64,
    },
    /// The seat is not seated and is not joining.
    #[error("the seat is not seated")]
    NotSeated,
    /// A different input was already accepted for this seat and tick.
    #[error("a different input was already accepted for this seat and tick")]
    Conflict,
}

/// Why a membership change was refused. Nothing changed.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MembershipError {
    /// The seat is already seated or already joining.
    #[error("the seat is already seated")]
    AlreadySeated,
    /// The seat is not seated, or is already leaving.
    #[error("the seat is not seated")]
    NotSeated,
    /// The game does not admit this seat.
    #[error("the game does not admit this seat")]
    NotAdmitted,
}

/// The requested frames are no longer retained; send a snapshot instead.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("frames from tick {requested} are no longer retained; the oldest is {oldest}")]
pub struct FramesUnavailable {
    /// The first tick requested.
    pub requested: u64,
    /// The oldest retained frame's tick.
    pub oldest: u64,
}

/// Authoritative state of one tick-synchronized game.
#[derive(Clone, Debug)]
pub struct TickAuthority<G: TickGame> {
    config: TickAuthorityConfig,
    state: G,
    tick: u64,
    /// Seated seats and the last input each contributed.
    seats: BTreeMap<G::Seat, G::Input>,
    /// Membership changes applied at the next boundary, in request order.
    changes: Vec<Membership<G::Seat>>,
    /// Buffered inputs; index `i` holds tick `tick + i`.
    pending: VecDeque<BTreeMap<G::Seat, G::Input>>,
    /// Recently emitted frames, oldest first.
    history: VecDeque<TickFrame<G::Seat, G::Input>>,
}

impl<G: TickGame> TickAuthority<G> {
    /// Starts a game at tick zero with nobody seated. Zero-valued bounds in
    /// `config` are raised to one.
    #[must_use]
    pub fn open(state: G, config: TickAuthorityConfig) -> Self {
        let config = TickAuthorityConfig {
            input_window: config.input_window.max(1),
            checksum_interval: config.checksum_interval.max(1),
            frame_history: config.frame_history,
        };
        let window = usize::try_from(config.input_window).unwrap_or(usize::MAX);
        Self {
            config,
            state,
            tick: 0,
            seats: BTreeMap::new(),
            changes: Vec::new(),
            pending: std::iter::repeat_with(BTreeMap::new)
                .take(window.min(1 << 16))
                .collect(),
            history: VecDeque::new(),
        }
    }

    /// The configuration in effect.
    #[must_use]
    pub const fn config(&self) -> TickAuthorityConfig {
        self.config
    }

    /// The canonical state at the current tick.
    #[must_use]
    pub const fn state(&self) -> &G {
        &self.state
    }

    /// The current tick: the next one [`TickAuthority::advance`] simulates.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Seated seats and the last input each contributed, ascending by seat.
    pub fn seats(&self) -> impl Iterator<Item = (G::Seat, G::Input)> + '_ {
        self.seats.iter().map(|(seat, input)| (*seat, *input))
    }

    /// Whether `seat` is seated, counting changes pending at the next
    /// boundary.
    #[must_use]
    pub fn is_seated(&self, seat: G::Seat) -> bool {
        self.changes
            .iter()
            .rev()
            .find_map(|change| match change {
                Membership::Join(joined) if *joined == seat => Some(true),
                Membership::Leave(left) if *left == seat => Some(false),
                _ => None,
            })
            .unwrap_or_else(|| self.seats.contains_key(&seat))
    }

    /// Seats `seat` at the next tick boundary.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipError::AlreadySeated`] when the seat is seated or
    /// joining, and [`MembershipError::NotAdmitted`] when the game refuses it.
    pub fn join(&mut self, seat: G::Seat) -> Result<(), MembershipError> {
        if self.is_seated(seat) {
            return Err(MembershipError::AlreadySeated);
        }
        if !self.state.admits(seat) {
            return Err(MembershipError::NotAdmitted);
        }
        self.changes.push(Membership::Join(seat));
        Ok(())
    }

    /// Unseats `seat` at the next tick boundary, discarding its buffered
    /// inputs.
    ///
    /// # Errors
    ///
    /// Returns [`MembershipError::NotSeated`] when the seat is not seated or
    /// is already leaving.
    pub fn leave(&mut self, seat: G::Seat) -> Result<(), MembershipError> {
        if !self.is_seated(seat) {
            return Err(MembershipError::NotSeated);
        }
        self.changes.push(Membership::Leave(seat));
        for slot in &mut self.pending {
            slot.remove(&seat);
        }
        Ok(())
    }

    /// Buffers `seat`'s input for `tick` according to the module's input
    /// window policy.
    ///
    /// # Errors
    ///
    /// Returns [`InputRefusal`] when the input is late, too early, conflicts
    /// with an accepted input, or the seat is not seated.
    pub fn submit(
        &mut self,
        seat: G::Seat,
        tick: u64,
        input: G::Input,
    ) -> Result<InputReceipt, InputRefusal> {
        if !self.is_seated(seat) {
            return Err(InputRefusal::NotSeated);
        }
        if tick < self.tick {
            return Err(InputRefusal::Late {
                tick,
                current: self.tick,
            });
        }
        let limit = self.tick.saturating_add(self.pending.len() as u64);
        let Some(slot) = usize::try_from(tick - self.tick)
            .ok()
            .and_then(|offset| self.pending.get_mut(offset))
        else {
            return Err(InputRefusal::TooEarly { tick, limit });
        };
        match slot.get(&seat) {
            Some(existing) if *existing == input => Ok(InputReceipt::Duplicate),
            Some(_) => Err(InputRefusal::Conflict),
            None => {
                slot.insert(seat, input);
                Ok(InputReceipt::Accepted)
            }
        }
    }

    /// Buffers a batch of `seat`'s inputs, returning each result in order.
    pub fn submit_batch(
        &mut self,
        seat: G::Seat,
        inputs: impl IntoIterator<Item = (u64, G::Input)>,
    ) -> Vec<Result<InputReceipt, InputRefusal>> {
        inputs
            .into_iter()
            .map(|(tick, input)| self.submit(seat, tick, input))
            .collect()
    }

    /// Simulates the current tick: applies pending membership changes, steps
    /// with every seat's buffered (or repeated) input, and returns the frame
    /// to broadcast.
    pub fn advance(&mut self) -> TickFrame<G::Seat, G::Input> {
        let changes = std::mem::take(&mut self.changes);
        for change in &changes {
            match *change {
                Membership::Join(seat) => {
                    let neutral = self.state.neutral_input(seat);
                    self.seats.insert(seat, neutral);
                    self.state.seat_joined(seat);
                }
                Membership::Leave(seat) => {
                    self.seats.remove(&seat);
                    self.state.seat_left(seat);
                }
            }
        }
        let mut buffered = self.pending.pop_front().unwrap_or_default();
        self.pending.push_back(BTreeMap::new());
        for (seat, last) in &mut self.seats {
            if let Some(input) = buffered.remove(seat) {
                *last = input;
            }
        }
        let inputs: Vec<_> = self.seats.iter().map(|(s, i)| (*s, *i)).collect();
        self.state.step(&inputs);
        let tick = self.tick;
        self.tick += 1;
        let checksum = self
            .tick
            .is_multiple_of(self.config.checksum_interval)
            .then(|| self.state.checksum());
        let frame = TickFrame {
            tick,
            changes,
            inputs,
            checksum,
        };
        if self.config.frame_history > 0 {
            if self.history.len() == self.config.frame_history {
                self.history.pop_front();
            }
            self.history.push_back(frame.clone());
        }
        frame
    }

    /// Retained frames from tick `from` up to the current tick, for
    /// retransmission to a client that missed some. Returns an empty list
    /// when `from` is at or beyond the current tick.
    ///
    /// # Errors
    ///
    /// Returns [`FramesUnavailable`] when frames from `from` are no longer
    /// retained; the client needs a [`TickAuthority::snapshot`].
    pub fn frames_from(&self, from: u64) -> Result<Vec<Frame<G>>, FramesUnavailable> {
        if from >= self.tick {
            return Ok(Vec::new());
        }
        let oldest = self.history.front().map_or(self.tick, |frame| frame.tick);
        if from < oldest {
            return Err(FramesUnavailable {
                requested: from,
                oldest,
            });
        }
        let skip = usize::try_from(from - oldest).unwrap_or(usize::MAX);
        Ok(self.history.iter().skip(skip).cloned().collect())
    }

    /// The complete state at the current tick boundary. Membership changes
    /// still pending appear in the next frame.
    #[must_use]
    pub fn snapshot(&self) -> TickSnapshot<G::Seat, G::Input> {
        TickSnapshot {
            tick: self.tick,
            seats: self.seats().collect(),
            state: self.state.encode(),
        }
    }
}
