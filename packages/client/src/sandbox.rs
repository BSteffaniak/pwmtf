//! Disposable practice state; never participates in multiplayer transport.
use bevy::prelude::Resource;
use pwmtf_game_domain::{
    BallId, BallState, PhysicsProfile, RackSeed, TableGeometry, Vector, VersionedShotCommand,
    VersionedTableState, standard_rack,
};

use crate::playback::{PlaybackEnd, ShotPlayback};

#[derive(Resource)]
pub struct Sandbox {
    pub enabled: bool,
    pub table: VersionedTableState,
    pub moving: bool,
    playback: Option<ShotPlayback>,
    seed: u64,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self {
            enabled: practice_enabled(),
            table: rack(42),
            moving: false,
            playback: None,
            seed: 42,
        }
    }
}

// Browser URL inspection cannot be const, even though the native fallback is.
#[allow(clippy::missing_const_for_fn)]
fn practice_enabled() -> bool {
    #[cfg(target_arch = "wasm32")]
    {
        web_sys::window()
            .and_then(|window| window.location().search().ok())
            .is_some_and(|search| {
                !search
                    .trim_start_matches('?')
                    .split('&')
                    .any(|part| part.split('=').next() == Some("match"))
            })
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        true
    }
}

fn rack(seed: u64) -> VersionedTableState {
    standard_rack(TableGeometry::standard(), RackSeed::new(seed))
        .expect("standard practice rack is valid")
}

impl Sandbox {
    pub fn shoot(&mut self, command: VersionedShotCommand) {
        if !self.enabled || self.moving {
            return;
        }
        if let Ok(playback) = ShotPlayback::start(
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            self.table.clone(),
            command,
            None,
        ) {
            self.table = playback.table().clone();
            self.moving = !playback.is_finished();
            self.playback = self.moving.then_some(playback);
        }
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    /// Permanently disables practice once a multiplayer match is opened.
    pub fn disable(&mut self) {
        self.enabled = false;
        self.moving = false;
        self.playback = None;
    }

    pub fn update(&mut self, seconds: f64) {
        if !self.enabled || !self.moving {
            return;
        }
        let Some(playback) = &mut self.playback else {
            self.moving = false;
            return;
        };
        let finished = playback.advance(seconds.min(0.1));
        self.table = playback.table().clone();
        if finished {
            let end = playback.end();
            self.playback = None;
            self.moving = false;
            if end == Some(PlaybackEnd::Settled) {
                self.finish_rack_or_restore_cue();
            } else {
                self.reset();
            }
        }
    }

    /// Interpolates between adjacent physics ticks rather than chasing state.
    pub fn presentation_position(
        &self,
        id: pwmtf_game_domain::BallId,
    ) -> Option<bevy::prelude::Vec2> {
        if let Some(playback) = &self.playback {
            let sample = playback.sample(id)?;
            return Some(crate::micros_to_world(sample.x_micros, sample.y_micros));
        }
        Some(crate::canonical_to_world(self.table.ball(id)?.position))
    }

    fn reset(&mut self) {
        self.seed = self.seed.wrapping_add(1);
        self.table = rack(self.seed);
        self.playback = None;
        self.moving = false;
    }

    fn finish_rack_or_restore_cue(&mut self) {
        if self
            .table
            .balls()
            .iter()
            .all(|ball| ball.id == BallId::CUE || ball.pocketed)
        {
            self.reset();
            return;
        }
        if !self
            .table
            .ball(BallId::CUE)
            .is_some_and(|ball| ball.pocketed)
        {
            return;
        }
        let geometry = TableGeometry::standard();
        let diameter = geometry.ball_radius().micros() * 2;
        let mut balls = self.table.balls().to_vec();
        // Search the playable area deterministically, rejecting overlap through
        // the domain constructor rather than duplicating collision rules.
        for x in -20..=20 {
            for y in -9..=9 {
                balls[0] = BallState::stationary(
                    BallId::CUE,
                    Vector::from_micros(x * diameter, y * diameter),
                );
                if let Ok(table) =
                    VersionedTableState::new(self.table.version(), geometry, 0, balls.clone())
                {
                    self.table = table;
                    return;
                }
            }
        }
        self.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pwmtf_game_domain::{Aim, ShotPower, Spin};

    #[test]
    fn repeated_shots_settle_without_turns() {
        let mut sandbox = Sandbox::default();
        for _ in 0..3 {
            sandbox.shoot(VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(5_000).unwrap(),
                Spin::CENTER,
            ));
            assert!(sandbox.moving);
            for _ in 0..10_000 {
                sandbox.update(0.1);
                if !sandbox.moving {
                    break;
                }
            }
            assert!(!sandbox.moving);
            assert!(!sandbox.table.ball(BallId::CUE).unwrap().pocketed);
        }
    }

    #[test]
    fn scratch_restores_cue_and_clear_table_reracks() {
        let mut sandbox = Sandbox::default();
        let mut balls = sandbox.table.balls().to_vec();
        balls[0].pocketed = true;
        sandbox.table = VersionedTableState::new(
            sandbox.table.version(),
            TableGeometry::standard(),
            0,
            balls.clone(),
        )
        .unwrap();
        sandbox.finish_rack_or_restore_cue();
        assert!(!sandbox.table.ball(BallId::CUE).unwrap().pocketed);
        for ball in &mut balls {
            ball.pocketed = true;
        }
        sandbox.table =
            VersionedTableState::new(sandbox.table.version(), TableGeometry::standard(), 0, balls)
                .unwrap();
        sandbox.finish_rack_or_restore_cue();
        assert!(sandbox.table.balls().iter().all(|ball| !ball.pocketed));
        assert_eq!(sandbox.seed, 43);
    }
}
