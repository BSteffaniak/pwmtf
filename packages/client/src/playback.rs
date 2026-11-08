//! Fixed-tick shot playback shared by practice and multiplayer presentation.
//!
//! Playback re-runs the canonical integer solver tick by tick, so an accepted
//! shot replays bit-exactly with the server. It only drives presentation and
//! never decides gameplay.

use pwmtf_billiards::{
    BallId, PhysicsProfile, SimulationError, TableGeometry, VersionedShotCommand,
    VersionedTableState, advance_tick, start_shot,
};

/// Upper bound on ticks simulated by one [`ShotPlayback::advance`] call, so a
/// long-suspended tab catches up over several frames instead of one stall.
pub const MAX_TICKS_PER_ADVANCE: u32 = 480;

/// One ball's presentation sample between two adjacent playback ticks.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlaybackBall {
    /// Stable canonical ball number.
    pub number: u8,
    /// Sub-tick interpolated horizontal position in micro-metres.
    pub x_micros: f64,
    /// Sub-tick interpolated vertical position in micro-metres.
    pub y_micros: f64,
    /// Whether the ball has been pocketed by the current tick.
    pub pocketed: bool,
}

/// How a playback ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlaybackEnd {
    /// Every ball settled within the tick limit.
    Settled,
    /// The externally supplied tick limit was reached first.
    TickLimit,
    /// The canonical solver refused to continue.
    Failed,
}

/// Fixed-tick stepper over one in-flight canonical shot.
#[derive(Clone, Debug)]
pub struct ShotPlayback {
    physics: PhysicsProfile,
    geometry: TableGeometry,
    previous: VersionedTableState,
    current: VersionedTableState,
    accumulator: f64,
    ticks: u32,
    limit: u32,
    end: Option<PlaybackEnd>,
}

impl ShotPlayback {
    /// Starts playback of `shot` from a settled `table`.
    ///
    /// `limit` bounds the number of ticks played; `None` uses the profile's
    /// maximum. A zero-power shot or a zero limit is finished immediately,
    /// matching the server's zero `playback_ticks`.
    ///
    /// # Errors
    ///
    /// Returns [`SimulationError`] when the canonical solver cannot start the
    /// shot: an unsupported command version, a moving table, or an absent or
    /// pocketed cue ball.
    pub fn start(
        physics: PhysicsProfile,
        geometry: TableGeometry,
        table: VersionedTableState,
        shot: VersionedShotCommand,
        limit: Option<u32>,
    ) -> Result<Self, SimulationError> {
        let current = start_shot(physics, table, shot)?;
        let limit = limit
            .unwrap_or_else(|| physics.maximum_ticks())
            .min(physics.maximum_ticks());
        let end = (shot.power.units() == 0 || limit == 0).then_some(PlaybackEnd::Settled);
        Ok(Self {
            physics,
            geometry,
            previous: current.clone(),
            current,
            accumulator: 0.0,
            ticks: 0,
            limit,
            end,
        })
    }

    /// Returns whether playback has ended.
    #[must_use]
    pub const fn is_finished(&self) -> bool {
        self.end.is_some()
    }

    /// Returns how playback ended, if it has.
    #[must_use]
    pub const fn end(&self) -> Option<PlaybackEnd> {
        self.end
    }

    /// Returns the number of ticks played so far.
    #[must_use]
    pub const fn ticks(&self) -> u32 {
        self.ticks
    }

    /// Returns the tick limit selected at start.
    #[must_use]
    pub const fn limit(&self) -> u32 {
        self.limit
    }

    /// Returns the canonical table at the current tick.
    #[must_use]
    pub const fn table(&self) -> &VersionedTableState {
        &self.current
    }

    /// Advances exactly one fixed tick unless finished.
    ///
    /// Returns whether playback has finished after this call.
    pub fn advance_tick(&mut self) -> bool {
        if self.end.is_some() {
            return true;
        }
        match advance_tick(self.geometry, self.physics, self.current.clone()) {
            Ok(result) => {
                self.ticks = self.ticks.saturating_add(1);
                self.previous = std::mem::replace(&mut self.current, result.state);
                if result.settled {
                    self.end = Some(PlaybackEnd::Settled);
                } else if self.ticks >= self.limit {
                    self.end = Some(PlaybackEnd::TickLimit);
                }
            }
            Err(_) => self.end = Some(PlaybackEnd::Failed),
        }
        if self.end.is_some() {
            self.previous = self.current.clone();
            self.accumulator = 0.0;
        }
        self.end.is_some()
    }

    /// Advances by real elapsed `seconds` at the profile's fixed tick rate,
    /// simulating at most [`MAX_TICKS_PER_ADVANCE`] ticks per call.
    ///
    /// Non-finite or negative durations are ignored. Returns whether playback
    /// has finished after this call.
    pub fn advance(&mut self, seconds: f64) -> bool {
        if self.end.is_some() {
            return true;
        }
        if seconds.is_finite() && seconds > 0.0 {
            self.accumulator += seconds;
        }
        let step = 1.0 / f64::from(self.physics.ticks_per_second());
        let mut stepped = 0;
        while self.accumulator >= step && stepped < MAX_TICKS_PER_ADVANCE {
            self.accumulator -= step;
            stepped += 1;
            if self.advance_tick() {
                return true;
            }
        }
        // Never carry more than one tick of lag beyond the per-call budget's
        // remainder; the next call continues catching up from here.
        self.accumulator = self
            .accumulator
            .min(step * f64::from(MAX_TICKS_PER_ADVANCE));
        false
    }

    /// Returns the sub-tick fraction toward the next tick in `0.0..=1.0`.
    #[must_use]
    pub fn alpha(&self) -> f64 {
        if self.end.is_some() {
            return 1.0;
        }
        (self.accumulator * f64::from(self.physics.ticks_per_second())).clamp(0.0, 1.0)
    }

    /// Samples one ball between the previous and current ticks.
    ///
    /// A ball pocketed by the current tick is reported pocketed immediately,
    /// so presentation hides it on the tick it drops.
    #[must_use]
    pub fn sample(&self, id: BallId) -> Option<PlaybackBall> {
        let current = self.current.ball(id)?;
        let previous = self.previous.ball(id).unwrap_or(current);
        let alpha = self.alpha();
        #[allow(clippy::cast_precision_loss)]
        let lerp = |from: i64, to: i64| ((to - from) as f64).mul_add(alpha, from as f64);
        Some(PlaybackBall {
            number: id.number(),
            x_micros: lerp(previous.position.x.micros(), current.position.x.micros()),
            y_micros: lerp(previous.position.y.micros(), current.position.y.micros()),
            pocketed: current.pocketed,
        })
    }

    /// Samples every ball in canonical order.
    #[must_use]
    pub fn samples(&self) -> Vec<PlaybackBall> {
        self.current
            .balls()
            .iter()
            .filter_map(|ball| self.sample(ball.id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use pwmtf_billiards::{Aim, RackSeed, ShotPower, Spin};
    use pwmtf_eight_ball::{MatchCommand, MatchState, Player, RulesProfile, VersionedMatchCommand};

    use super::*;

    fn state() -> MatchState {
        MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap()
    }

    fn shot(power: u16) -> VersionedShotCommand {
        VersionedShotCommand::new(
            Aim::new(0).unwrap(),
            ShotPower::new(power).unwrap(),
            Spin::CENTER,
        )
    }

    #[test]
    fn playback_converges_with_authoritative_shot_at_the_reported_tick() {
        let initial = state();
        let mut authority = initial.clone();
        let applied = authority
            .apply_command_timed(VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: shot(8_000),
                called_pocket: None,
            }))
            .unwrap();
        assert!(applied.playback_ticks > 0);
        let mut playback = ShotPlayback::start(
            initial.physics(),
            initial.geometry(),
            initial.table().clone(),
            shot(8_000),
            Some(applied.playback_ticks),
        )
        .unwrap();
        let mut moved_mid_shot = false;
        while !playback.advance_tick() {
            moved_mid_shot |= playback.table().balls() != initial.table().balls()
                && playback.table().balls() != authority.table().balls();
        }
        assert!(moved_mid_shot);
        assert_eq!(playback.ticks(), applied.playback_ticks);
        assert_eq!(playback.end(), Some(PlaybackEnd::Settled));
        assert_eq!(playback.table().balls(), authority.table().balls());
    }

    #[test]
    fn zero_power_and_zero_limit_finish_immediately() {
        let initial = state();
        let playback = ShotPlayback::start(
            initial.physics(),
            initial.geometry(),
            initial.table().clone(),
            shot(0),
            None,
        )
        .unwrap();
        assert!(playback.is_finished());
        let playback = ShotPlayback::start(
            initial.physics(),
            initial.geometry(),
            initial.table().clone(),
            shot(5_000),
            Some(0),
        )
        .unwrap();
        assert!(playback.is_finished());
        assert_eq!(playback.ticks(), 0);
    }

    #[test]
    fn real_time_advance_uses_fixed_ticks_and_bounded_catch_up() {
        let initial = state();
        let mut playback = ShotPlayback::start(
            initial.physics(),
            initial.geometry(),
            initial.table().clone(),
            shot(10_000),
            None,
        )
        .unwrap();
        let tick = 1.0 / f64::from(initial.physics().ticks_per_second());
        assert!(!playback.advance(tick * 2.5));
        assert_eq!(playback.ticks(), 2);
        assert!(playback.alpha() > 0.4 && playback.alpha() < 0.6);
        assert!(!playback.advance(f64::NAN));
        assert!(!playback.advance(-1.0));
        assert_eq!(playback.ticks(), 2);
        playback.advance(1_000.0);
        assert!(playback.ticks() <= 2 + MAX_TICKS_PER_ADVANCE);
    }

    #[test]
    fn tick_limit_stops_before_settling() {
        let initial = state();
        let mut playback = ShotPlayback::start(
            initial.physics(),
            initial.geometry(),
            initial.table().clone(),
            shot(10_000),
            Some(3),
        )
        .unwrap();
        while !playback.advance_tick() {}
        assert_eq!(playback.ticks(), 3);
        assert_eq!(playback.end(), Some(PlaybackEnd::TickLimit));
        let cue = playback.sample(BallId::CUE).unwrap();
        assert!(!cue.pocketed);
        assert_eq!(playback.samples().len(), 16);
    }
}
