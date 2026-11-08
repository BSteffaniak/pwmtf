#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Transport-free, deterministic synchronization in two modes: command
//! replay for turn-based games ([`ReplayGame`], [`Authority`], [`Replica`])
//! and tick-stamped inputs with client rollback for continuous real-time
//! games ([`TickGame`], [`TickAuthority`], [`TickReplica`]).
//!
//! See the crate README for both models. This crate owns no transport,
//! persistence, identity or rendering; hosts supply those.

mod authority;
mod game;
mod replica;
mod tick;

pub use authority::{
    Accepted, Authority, CommandKey, Deadline, Proposal, Refusal, RestoreError, Submission,
};
pub use game::{Applied, Authorization, Playback, ReplayGame};
pub use replica::{
    MAX_PLAYBACK_STEP_MS, Pending, Received, Remote, Replica, ReplicaError, Transition,
};
pub use tick::{
    Advance, Correction, FramesUnavailable, InputReceipt, InputRefusal, Membership,
    MembershipError, ReplicaRequest, TickAuthority, TickAuthorityConfig, TickFrame, TickGame,
    TickReplica, TickReplicaConfig, TickReplicaError, TickSnapshot, lead_ticks,
};

#[cfg(test)]
mod tests;
