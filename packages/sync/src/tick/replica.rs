//! The client side of tick synchronization: prediction, rollback and resync.
//!
//! A replica holds the last **confirmed** state (every authoritative frame
//! before [`TickReplica::confirmed_tick`] applied) and, for a player, a
//! bounded queue of **predicted** states up to
//! [`TickReplica::predicted_tick`].
//!
//! - Each [`TickReplica::advance`] first integrates contiguous received
//!   frames. A frame whose inputs match what was predicted for that tick
//!   promotes the predicted state without re-simulating. A frame that
//!   differs (a remote seat changed input, the local input arrived late, or a
//!   seat joined or left) rolls back: the frame is applied to the confirmed
//!   state and every later tick is re-simulated with the local player's own
//!   inputs and every remote seat's newest confirmed input.
//! - A player then stamps its input for the predicted tick and steps once, so
//!   its own input is visible immediately. Prediction stops at
//!   [`TickReplicaConfig::prediction_horizon`] ticks ahead of confirmation.
//! - A spectator never predicts. It keeps
//!   [`TickReplicaConfig::spectator_delay`] frames buffered as a jitter
//!   buffer and presents confirmed state, catching up when far behind.
//! - Frames that carry a checksum are verified after they are confirmed. A
//!   mismatch marks the replica desynchronized: it stops integrating and
//!   asks for a snapshot through [`TickReplica::request`].

use std::collections::{BTreeMap, VecDeque};

use thiserror::Error;

use super::{Membership, TickFrame, TickGame, TickSnapshot};

/// Tunable bounds for a [`TickReplica`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TickReplicaConfig {
    /// Most ticks a player predicts beyond the confirmed tick; it also bounds
    /// re-simulation work per rollback and retained predicted states.
    pub prediction_horizon: u64,
    /// Most out-of-order frames buffered ahead of the confirmed tick. A frame
    /// further ahead means the replica fell too far behind and needs a
    /// snapshot.
    pub max_buffered_frames: u64,
    /// Frames a spectator keeps buffered before presenting them.
    pub spectator_delay: u64,
}

impl Default for TickReplicaConfig {
    /// Half a second of prediction at 60 Hz, four seconds of reorder buffer
    /// at 60 Hz and a 50 ms spectator jitter buffer at 60 Hz.
    fn default() -> Self {
        Self {
            prediction_horizon: 30,
            max_buffered_frames: 240,
            spectator_delay: 3,
        }
    }
}

/// What a replica needs from the authority. Hosts rate-limit requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplicaRequest {
    /// Retransmit frames from this tick onward.
    Frames {
        /// The first missing tick.
        from: u64,
    },
    /// Send a fresh snapshot; frames cannot repair this replica.
    Snapshot,
}

/// A rollback that changed what was presented.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Correction {
    /// Ticks re-simulated after the rollback.
    pub ticks_resimulated: u64,
    /// [`TickGame::correction_magnitude`] between the presented state before
    /// and after. Approximate when confirmation overtook the prediction, as
    /// the two states then belong to different ticks.
    pub magnitude: u64,
}

/// What one [`TickReplica::advance`] did.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Advance {
    /// Frames confirmed during this call.
    pub confirmed: u64,
    /// The predicted tick advanced by one with the local input.
    pub predicted: bool,
    /// A player could not predict further: the horizon was reached or the
    /// replica is awaiting a snapshot.
    pub stalled: bool,
    /// A rollback changed the presented state.
    pub correction: Option<Correction>,
}

/// Why a snapshot could not be adopted. The replica is unchanged.
#[derive(Debug, Error)]
pub enum TickReplicaError<E> {
    /// The game could not decode the snapshot's state.
    #[error("snapshot state could not be decoded: {0:?}")]
    Decode(E),
}

#[derive(Clone, Debug)]
struct Predicted<G: TickGame> {
    inputs: Vec<(G::Seat, G::Input)>,
    state: G,
}

/// A client's view of a tick-synchronized game.
#[derive(Clone, Debug)]
pub struct TickReplica<G: TickGame> {
    config: TickReplicaConfig,
    local: Option<G::Seat>,
    confirmed: G,
    confirmed_tick: u64,
    /// Confirmed seats and the last input each contributed.
    seats: BTreeMap<G::Seat, G::Input>,
    /// Received frames at or after the confirmed tick.
    buffered: BTreeMap<u64, TickFrame<G::Seat, G::Input>>,
    /// Entry `i` advanced from `confirmed_tick + i`.
    predicted: VecDeque<Predicted<G>>,
    /// The local player's stamped inputs not yet confirmed.
    local_inputs: BTreeMap<u64, G::Input>,
    last_local: Option<G::Input>,
    desynced: bool,
    overflowed: bool,
}

impl<G: TickGame> TickReplica<G> {
    /// Starts a replica from an authority snapshot. `local` is the player's
    /// seat, or `None` for a spectator. A player whose join is still pending
    /// may start before its seat appears; its inputs take effect once the
    /// join is confirmed.
    ///
    /// # Errors
    ///
    /// Returns [`TickReplicaError::Decode`] when the snapshot's state cannot
    /// be decoded.
    pub fn from_snapshot(
        snapshot: &TickSnapshot<G::Seat, G::Input>,
        local: Option<G::Seat>,
        config: TickReplicaConfig,
    ) -> Result<Self, TickReplicaError<G::DecodeError>> {
        let confirmed = G::decode(&snapshot.state).map_err(TickReplicaError::Decode)?;
        Ok(Self {
            config,
            local,
            confirmed,
            confirmed_tick: snapshot.tick,
            seats: snapshot.seats.iter().copied().collect(),
            buffered: BTreeMap::new(),
            predicted: VecDeque::new(),
            local_inputs: BTreeMap::new(),
            last_local: None,
            desynced: false,
            overflowed: false,
        })
    }

    /// The configuration in effect.
    #[must_use]
    pub const fn config(&self) -> TickReplicaConfig {
        self.config
    }

    /// The local player's seat, or `None` for a spectator.
    #[must_use]
    pub const fn local_seat(&self) -> Option<G::Seat> {
        self.local
    }

    /// The newest confirmed state.
    #[must_use]
    pub const fn confirmed_state(&self) -> &G {
        &self.confirmed
    }

    /// The tick of [`TickReplica::confirmed_state`]: frames before it have
    /// been applied.
    #[must_use]
    pub const fn confirmed_tick(&self) -> u64 {
        self.confirmed_tick
    }

    /// The tick of [`TickReplica::presented`].
    #[must_use]
    pub fn predicted_tick(&self) -> u64 {
        self.confirmed_tick + self.predicted.len() as u64
    }

    /// The state to render: the newest prediction for a player, the
    /// confirmed state for a spectator.
    #[must_use]
    pub fn presented(&self) -> &G {
        self.predicted
            .back()
            .map_or(&self.confirmed, |predicted| &predicted.state)
    }

    /// Confirmed seats and their newest confirmed inputs, ascending by seat.
    pub fn seats(&self) -> impl Iterator<Item = (G::Seat, G::Input)> + '_ {
        self.seats.iter().map(|(seat, input)| (*seat, *input))
    }

    /// Whether a checksum mismatch was detected; the replica waits for
    /// [`TickReplica::adopt`].
    #[must_use]
    pub const fn is_desynced(&self) -> bool {
        self.desynced
    }

    /// The local player's stamped inputs the authority has not confirmed,
    /// ascending by tick. Hosts send them, batched, in every input message,
    /// so a lost message is repaired by the next one.
    pub fn unconfirmed_inputs(&self) -> impl Iterator<Item = (u64, G::Input)> + '_ {
        self.local_inputs
            .range(self.confirmed_tick..)
            .map(|(tick, input)| (*tick, *input))
    }

    /// Buffers an authoritative frame. Returns whether it was new; frames
    /// already confirmed or already buffered are ignored, and a frame beyond
    /// the reorder buffer makes the replica request a snapshot.
    pub fn receive(&mut self, frame: TickFrame<G::Seat, G::Input>) -> bool {
        if frame.tick < self.confirmed_tick || self.buffered.contains_key(&frame.tick) {
            return false;
        }
        if frame.tick - self.confirmed_tick >= self.config.max_buffered_frames {
            self.overflowed = true;
            return false;
        }
        self.buffered.insert(frame.tick, frame);
        true
    }

    /// What the replica needs from the authority, if anything.
    #[must_use]
    pub fn request(&self) -> Option<ReplicaRequest> {
        if self.desynced || self.overflowed {
            return Some(ReplicaRequest::Snapshot);
        }
        // A hole anywhere after the contiguous run (not only at the confirmed
        // tick): a spectator's jitter buffer can hold frames before the hole.
        let contiguous = self.contiguous_frames();
        let gap = self
            .buffered
            .last_key_value()
            .is_some_and(|(last, _)| *last >= self.confirmed_tick + contiguous);
        let starved = self.local.is_some() && self.predicted.len() as u64 >= self.horizon();
        if gap {
            return Some(ReplicaRequest::Frames {
                from: self.confirmed_tick + contiguous,
            });
        }
        starved.then_some(ReplicaRequest::Frames {
            from: self.confirmed_tick,
        })
    }

    /// Replaces all state with an authority snapshot, clearing a desync.
    /// Buffered frames and local inputs at or after the snapshot's tick are
    /// kept; prediction restarts from the snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`TickReplicaError::Decode`] when the snapshot's state cannot
    /// be decoded.
    pub fn adopt(
        &mut self,
        snapshot: &TickSnapshot<G::Seat, G::Input>,
    ) -> Result<(), TickReplicaError<G::DecodeError>> {
        self.confirmed = G::decode(&snapshot.state).map_err(TickReplicaError::Decode)?;
        self.confirmed_tick = snapshot.tick;
        self.seats = snapshot.seats.iter().copied().collect();
        self.buffered = self.buffered.split_off(&snapshot.tick);
        self.local_inputs = self.local_inputs.split_off(&snapshot.tick);
        self.predicted.clear();
        self.desynced = false;
        self.overflowed = false;
        Ok(())
    }

    /// Advances one host tick: integrates received frames (rolling back when
    /// they contradict the prediction), then, for a player, stamps `input`
    /// for the predicted tick and predicts one step. `None` repeats the
    /// previous local input. Spectators pass `None`.
    ///
    /// A stalled player's `input` is not stamped; the next call supplies a
    /// fresh one.
    pub fn advance(&mut self, input: Option<G::Input>) -> Advance {
        let mut result = Advance::default();
        if self.desynced {
            result.stalled = self.local.is_some();
            return result;
        }
        let Some(local) = self.local else {
            result.confirmed = self.integrate(Some(self.spectator_frames()));
            return result;
        };
        let p_before = self.predicted_tick();
        let (confirmed, rollback) = self.integrate_player();
        result.confirmed = confirmed;
        if let Some(before) = rollback {
            let target = p_before.max(self.confirmed_tick);
            while self.predicted_tick() < target {
                self.predict_one();
            }
            result.correction = Some(Correction {
                ticks_resimulated: target - self.confirmed_tick,
                magnitude: self.presented().correction_magnitude(&before),
            });
        }
        if self.desynced || self.predicted.len() as u64 >= self.horizon() {
            result.stalled = true;
            return result;
        }
        let input = input
            .or(self.last_local)
            .or_else(|| self.seats.get(&local).copied())
            .unwrap_or_else(|| self.confirmed.neutral_input(local));
        self.last_local = Some(input);
        self.local_inputs.insert(self.predicted_tick(), input);
        self.predict_one();
        result.predicted = true;
        result
    }

    const fn horizon(&self) -> u64 {
        if self.config.prediction_horizon == 0 {
            1
        } else {
            self.config.prediction_horizon
        }
    }

    /// Buffered frames contiguous from the confirmed tick.
    fn contiguous_frames(&self) -> u64 {
        let mut available = 0;
        while self
            .buffered
            .contains_key(&(self.confirmed_tick + available))
        {
            available += 1;
        }
        available
    }

    /// How many contiguous frames a spectator presents this tick: none while
    /// the jitter buffer fills, one in steady state, more to catch up.
    fn spectator_frames(&self) -> u64 {
        let available = self.contiguous_frames();
        let delay = self.config.spectator_delay;
        if available <= delay {
            0
        } else if available <= delay.saturating_mul(2) {
            1
        } else {
            (available - delay.saturating_mul(2) + 1).min(available)
        }
    }

    /// Confirms up to `limit` contiguous frames without prediction. Returns
    /// how many were confirmed.
    fn integrate(&mut self, limit: Option<u64>) -> u64 {
        let mut count = 0;
        while limit.is_none_or(|limit| count < limit) && !self.desynced {
            let Some(frame) = self.buffered.remove(&self.confirmed_tick) else {
                break;
            };
            self.apply_confirmed(&frame);
            count += 1;
        }
        count
    }

    /// Confirms every contiguous frame, promoting matching predictions.
    /// Returns how many were confirmed and, when a rollback discarded the
    /// prediction, the state that was presented before it.
    fn integrate_player(&mut self) -> (u64, Option<G>) {
        let mut count = 0;
        let mut before = None;
        while !self.desynced {
            let Some(frame) = self.buffered.remove(&self.confirmed_tick) else {
                break;
            };
            match self.predicted.pop_front() {
                Some(front) if frame.changes.is_empty() && front.inputs == frame.inputs => {
                    self.record_confirmed(&frame);
                    self.confirmed = front.state;
                    self.finish_confirmed(&frame);
                }
                front => {
                    if let Some(front) = front {
                        before = Some(
                            self.predicted
                                .back()
                                .map_or(front.state, |last| last.state.clone()),
                        );
                        self.predicted.clear();
                    }
                    self.apply_confirmed(&frame);
                }
            }
            count += 1;
        }
        self.local_inputs = self.local_inputs.split_off(&self.confirmed_tick);
        (count, before)
    }

    /// Applies a frame's membership changes and inputs to the confirmed
    /// state.
    fn apply_confirmed(&mut self, frame: &TickFrame<G::Seat, G::Input>) {
        for change in &frame.changes {
            match *change {
                Membership::Join(seat) => self.confirmed.seat_joined(seat),
                Membership::Leave(seat) => self.confirmed.seat_left(seat),
            }
        }
        self.confirmed.step(&frame.inputs);
        self.record_confirmed(frame);
        self.finish_confirmed(frame);
    }

    /// Updates confirmed seats and their last inputs from a frame, which
    /// lists every seat seated after its membership changes.
    fn record_confirmed(&mut self, frame: &TickFrame<G::Seat, G::Input>) {
        self.seats.clear();
        self.seats.extend(frame.inputs.iter().copied());
    }

    /// Advances the confirmed tick and verifies the frame's checksum.
    fn finish_confirmed(&mut self, frame: &TickFrame<G::Seat, G::Input>) {
        self.confirmed_tick = frame.tick + 1;
        if frame
            .checksum
            .is_some_and(|checksum| checksum != self.confirmed.checksum())
        {
            self.desynced = true;
        }
    }

    /// The inputs predicted for `tick`: the local player's stamped input and
    /// every remote seat's newest confirmed input.
    fn predicted_inputs(&self, tick: u64) -> Vec<(G::Seat, G::Input)> {
        self.seats
            .iter()
            .map(|(seat, input)| {
                let input = if Some(*seat) == self.local {
                    self.local_inputs.get(&tick).copied().unwrap_or(*input)
                } else {
                    *input
                };
                (*seat, input)
            })
            .collect()
    }

    fn predict_one(&mut self) {
        let inputs = self.predicted_inputs(self.predicted_tick());
        let mut state = self.presented().clone();
        state.step(&inputs);
        self.predicted.push_back(Predicted { inputs, state });
    }
}
