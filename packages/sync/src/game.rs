//! The contract a deterministic game implements to be synchronized.

use std::fmt::Debug;

/// A deterministic, turn-structured game whose commands resolve instantly on
/// the authority and replay identically on every client.
///
/// Implementations must be deterministic: applying the same command to equal
/// states yields equal states, outcomes and playback lengths on every
/// platform. Use integer or fixed-point arithmetic for anything that affects
/// state.
pub trait ReplayGame: Clone + Eq + Debug {
    /// Who may act; supplied by the host after authenticating a player.
    type Seat: Copy + Eq + Ord + Debug;
    /// A bounded, already-validated state-changing command.
    type Command: Copy + Eq + Debug;
    /// What applying a command produced, for acknowledgements and summaries.
    type Outcome: Clone + Eq + Debug;
    /// Why the game refused a command.
    type Error: Debug;
    /// Presentation of a command's motion, stepped by clients.
    type Playback: Playback;

    /// Whether the game still accepts commands.
    fn in_progress(&self) -> bool;

    /// The seat whose turn it is, or `None` once the game has ended.
    fn active_seat(&self) -> Option<Self::Seat>;

    /// Checks that `seat` may issue `command` now, before any work is done.
    ///
    /// # Errors
    ///
    /// Returns [`Authorization`] describing why the seat may not act.
    fn authorize(&self, seat: Self::Seat, command: &Self::Command) -> Result<(), Authorization>;

    /// Applies a command, returning its outcome and the fixed number of
    /// simulation ticks its motion takes to play back (zero for none).
    ///
    /// # Errors
    ///
    /// Returns the game's error when the command is illegal in this state.
    /// The state must be unchanged on error.
    fn apply(&mut self, command: Self::Command) -> Result<Applied<Self::Outcome>, Self::Error>;

    /// A stable checksum of the complete state, identical on every platform.
    fn checksum(&self) -> u64;

    /// How long a turn lasts in milliseconds, or `None` for untimed turns.
    fn turn_duration_millis(&self) -> Option<u64>;

    /// Real time, in milliseconds, that `ticks` of playback take.
    fn playback_millis(&self, ticks: u32) -> u64;

    /// The command the authority applies when a turn's deadline passes.
    fn timeout_command(&self) -> Self::Command;

    /// Whether clients should resolve `command` locally as soon as it is
    /// submitted. Return `false` for commands whose resolution is expensive
    /// (such as a full physics simulation): they are played back at once and
    /// resolved only if the authority's answer has not arrived by the end.
    fn predict_immediately(&self, command: &Self::Command) -> bool {
        let _ = command;
        true
    }

    /// Starts presenting `command`'s motion from this (pre-command) state.
    ///
    /// `ticks` is the authority's playback length when known; `None` plays
    /// until the motion naturally ends. Return `None` for commands without
    /// motion.
    fn start_playback(&self, command: &Self::Command, ticks: Option<u32>)
    -> Option<Self::Playback>;
}

/// A successfully applied command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Applied<O> {
    /// What the command produced.
    pub outcome: O,
    /// Fixed simulation ticks of motion to play back; zero for none.
    pub playback_ticks: u32,
}

/// Why a seat may not issue a command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Authorization {
    /// It is another seat's turn, or the command belongs to another seat.
    WrongSeat,
    /// The command is never accepted from a seat (for example, a timeout).
    NotPermitted,
}

/// Fixed-tick presentation of one command's motion.
pub trait Playback {
    /// Advances by real elapsed seconds; returns whether playback finished.
    fn advance(&mut self, seconds: f64) -> bool;

    /// Whether playback has finished.
    fn is_finished(&self) -> bool;
}
