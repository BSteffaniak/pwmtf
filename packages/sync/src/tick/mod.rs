//! Tick-stamped input synchronization with client prediction and rollback.
//!
//! Continuous real-time games (air hockey, foosball, carts) advance a fixed
//! number of simulation ticks per second whether or not anyone acts. Every
//! participant contributes one small, quantized input per tick, and a
//! deterministic [`TickGame::step`] turns the complete, seat-ordered input set
//! into the next state.
//!
//! - [`TickAuthority`] owns the canonical state. It buffers inputs stamped
//!   with the tick they are meant for, advances exactly one tick each time the
//!   host calls [`TickAuthority::advance`], and emits a [`TickFrame`] holding
//!   every input it used, membership changes and a periodic checksum.
//! - [`TickReplica`] mirrors it on a client. A player applies its own input
//!   immediately, predicts ahead of confirmed ticks, and rolls back and
//!   re-simulates whenever a confirmed frame disagrees with its prediction. A
//!   spectator presents confirmed ticks after a small jitter buffer.
//!
//! The host owns the clock (when to advance), transport, framing and identity
//! (mapping a connection to a seat).

mod authority;
mod replica;

#[cfg(test)]
mod tests;

use std::fmt::Debug;

pub use authority::{
    FramesUnavailable, InputReceipt, InputRefusal, MembershipError, TickAuthority,
    TickAuthorityConfig,
};
pub use replica::{
    Advance, Correction, ReplicaRequest, TickReplica, TickReplicaConfig, TickReplicaError,
};

/// A deterministic game advanced in fixed ticks from per-seat inputs.
///
/// Implementations must be deterministic and platform independent: equal
/// states stepped with equal inputs yield equal states and checksums on every
/// platform (native and WASM). Use integer or fixed-point arithmetic for
/// everything that affects state, and never depend on hash-map iteration
/// order.
pub trait TickGame: Clone + Debug {
    /// Who contributes inputs; supplied by the host after authenticating.
    type Seat: Copy + Eq + Ord + Debug;
    /// One seat's input for one tick.
    ///
    /// Inputs must be small, bounded, quantized and device neutral: describe
    /// intent such as a pointer target position or axis values, never a key
    /// press, so mouse, keyboard and touch produce the same representation.
    /// Hosts validate and quantize inputs before submitting them.
    type Input: Copy + Eq + Debug;
    /// Why encoded state could not be decoded.
    type DecodeError: Debug;

    /// Fixed simulation ticks per second.
    const TICKS_PER_SECOND: u32;

    /// The input a seat contributes before it has sent any, such as a mallet
    /// resting at home.
    fn neutral_input(&self, seat: Self::Seat) -> Self::Input;

    /// Advances exactly one tick.
    ///
    /// `inputs` holds exactly one entry per seated seat, sorted ascending by
    /// seat. A seat with no input for this tick contributes its previous one
    /// (or [`TickGame::neutral_input`] right after joining).
    fn step(&mut self, inputs: &[(Self::Seat, Self::Input)]);

    /// Applies a seat joining at a tick boundary, before that tick's step.
    fn seat_joined(&mut self, seat: Self::Seat) {
        let _ = seat;
    }

    /// Applies a seat leaving at a tick boundary, before that tick's step.
    fn seat_left(&mut self, seat: Self::Seat) {
        let _ = seat;
    }

    /// Whether `seat` may join this game; the authority refuses others.
    fn admits(&self, seat: Self::Seat) -> bool {
        let _ = seat;
        true
    }

    /// A stable checksum of the complete state, identical on every platform.
    fn checksum(&self) -> u64;

    /// Encodes the complete state. The encoding must carry its own version.
    fn encode(&self) -> Vec<u8>;

    /// Decodes a state produced by [`TickGame::encode`].
    ///
    /// # Errors
    ///
    /// Returns the game's error for malformed bytes or an unknown version.
    fn decode(bytes: &[u8]) -> Result<Self, Self::DecodeError>;

    /// How far presentation visibly moved between `before` and `self`, two
    /// states for the same tick, in game units (for example the largest body
    /// displacement in micrometres). Hosts use it to decide how much to
    /// smooth a rollback correction. The default reports `1` when the states
    /// differ and `0` otherwise.
    fn correction_magnitude(&self, before: &Self) -> u64 {
        u64::from(self.checksum() != before.checksum())
    }
}

/// A change in who is seated, applied at a tick boundary before the step.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Membership<S> {
    /// The seat joins, contributing its neutral input until it sends one.
    Join(S),
    /// The seat leaves.
    Leave(S),
}

/// Everything the authority used to advance from `tick` to `tick + 1`.
///
/// Frames are the broadcast unit. Applying contiguous frames to a snapshot
/// reproduces the authority's state exactly.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickFrame<S, I> {
    /// The tick this frame advances from.
    pub tick: u64,
    /// Membership changes applied at the boundary, in order, before the step.
    pub changes: Vec<Membership<S>>,
    /// One input per seated seat, ascending by seat.
    pub inputs: Vec<(S, I)>,
    /// The state checksum after the step, present every checksum interval.
    pub checksum: Option<u64>,
}

/// The authority's complete state at a tick boundary, for late joins and
/// resynchronization.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickSnapshot<S, I> {
    /// The tick of the state: the next frame to apply is for this tick.
    pub tick: u64,
    /// Seated seats with the last input each contributed, ascending by seat.
    pub seats: Vec<(S, I)>,
    /// The game's versioned [`TickGame::encode`] bytes.
    pub state: Vec<u8>,
}

/// Ticks a player should predict ahead of the newest confirmed tick so its
/// inputs reach the authority before it simulates them: a full round trip
/// plus jitter, rounded up, plus one tick of margin.
#[must_use]
pub const fn lead_ticks(round_trip_millis: u64, jitter_millis: u64, ticks_per_second: u32) -> u64 {
    let millis = round_trip_millis.saturating_add(jitter_millis);
    let ticks = millis
        .saturating_mul(ticks_per_second as u64)
        .div_ceil(1_000);
    ticks.saturating_add(1)
}
