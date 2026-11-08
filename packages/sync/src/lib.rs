#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Transport-free, deterministic command-replay synchronization.
//!
//! See the crate README for the model. This crate owns no transport,
//! persistence, identity or rendering; hosts supply those.

mod authority;
mod game;
mod replica;

pub use authority::{
    Accepted, Authority, CommandKey, Deadline, Proposal, Refusal, RestoreError, Submission,
};
pub use game::{Applied, Authorization, Playback, ReplayGame};
pub use replica::{
    MAX_PLAYBACK_STEP_MS, Pending, Received, Remote, Replica, ReplicaError, Transition,
};

#[cfg(test)]
mod tests;
