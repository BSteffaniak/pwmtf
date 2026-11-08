//! Smooth, always-present presentation of the opponent's cue.
//!
//! Planning previews arrive at a bounded cadence (roughly 20 per second), so
//! drawing each one directly looks stepped. The cue instead follows them with
//! a critically damped spring: it accelerates and settles like a hand moving
//! a real cue, never overshooting. Between gestures it rests at the last aim
//! with no power rather than disappearing.

use pwmtf_billiards::{Aim, ShotPower, Spin};

use crate::transport::OpponentCueTarget;

/// Time for the aim to settle on a new heading, in seconds.
const AIM_SMOOTH_SECONDS: f32 = 0.09;
/// Time for the drawn-back power to settle, in seconds.
const POWER_SMOOTH_SECONDS: f32 = 0.06;
/// Frames longer than this (a background tab) do not fling the cue.
const MAX_STEP_SECONDS: f32 = 0.1;
/// Heading before the opponent has aimed this session.
const DEFAULT_AIM: f32 = 0.0;

/// Rendered opponent cue for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpponentCueView {
    /// Heading in radians, in (-π, π].
    pub aim: f32,
    /// Drawn-back power in `0.0..=1.0`.
    pub power: f32,
    /// Cue-ball contact point for the guide, in `-1.0..=1.0` per axis.
    pub spin: (f32, f32),
}

/// Spring state carried between frames.
#[derive(Debug, Default)]
pub struct OpponentCueMotion {
    view: Option<OpponentCueView>,
    aim_velocity: f32,
    power_velocity: f32,
    /// Last heading shown, kept across turns so the cue reappears where the
    /// opponent left it instead of snapping to a default.
    last_aim: Option<f32>,
}

impl OpponentCueMotion {
    /// Advances the spring by `seconds` toward `target`; `None` hides the cue.
    pub fn update(
        &mut self,
        target: Option<OpponentCueTarget>,
        seconds: f32,
    ) -> Option<OpponentCueView> {
        let Some(target) = target else {
            self.view = None;
            self.aim_velocity = 0.0;
            self.power_velocity = 0.0;
            return None;
        };
        let target_aim = target
            .aim
            .map_or_else(|| self.last_aim.unwrap_or(DEFAULT_AIM), aim_radians);
        let target_power = power_fraction(target.power);
        let spin = spin_fraction(target.spin);
        let dt = seconds.clamp(0.0, MAX_STEP_SECONDS);
        let view = match self.view {
            // A cue that was hidden appears at its target; it never sweeps in.
            None => OpponentCueView {
                aim: target_aim,
                power: target_power,
                spin,
            },
            Some(current) => {
                // Follow the short way around, then keep the angle wrapped.
                let goal = current.aim + wrap_angle(target_aim - current.aim);
                OpponentCueView {
                    aim: wrap_angle(smooth_damp(
                        current.aim,
                        goal,
                        &mut self.aim_velocity,
                        AIM_SMOOTH_SECONDS,
                        dt,
                    )),
                    power: smooth_damp(
                        current.power,
                        target_power,
                        &mut self.power_velocity,
                        POWER_SMOOTH_SECONDS,
                        dt,
                    )
                    .clamp(0.0, 1.0),
                    spin,
                }
            }
        };
        self.view = Some(view);
        self.last_aim = Some(view.aim);
        Some(view)
    }
}

fn aim_radians(aim: Aim) -> f32 {
    wrap_angle(f32::from(aim.steps()) * std::f32::consts::TAU / f32::from(Aim::STEPS_PER_TURN))
}

fn power_fraction(power: ShotPower) -> f32 {
    f32::from(power.units()) / f32::from(ShotPower::MAX)
}

fn spin_fraction(spin: Spin) -> (f32, f32) {
    (
        f32::from(spin.side) / 10_000.0,
        f32::from(spin.vertical) / 10_000.0,
    )
}

/// Keeps an angle in (-π, π].
fn wrap_angle(angle: f32) -> f32 {
    let wrapped =
        (angle + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
    if wrapped <= -std::f32::consts::PI {
        wrapped + std::f32::consts::TAU
    } else {
        wrapped
    }
}

/// Critically damped spring step (Game Programming Gems 4, "Critically
/// Damped Ease-In/Ease-Out Smoothing"): no overshoot, continuous velocity.
#[allow(clippy::suboptimal_flops)]
fn smooth_damp(current: f32, target: f32, velocity: &mut f32, smooth_time: f32, dt: f32) -> f32 {
    let omega = 2.0 / smooth_time;
    let x = omega * dt;
    let decay = 1.0 / (1.0 + x + 0.48 * x * x + 0.235 * x * x * x);
    let change = current - target;
    let temp = (*velocity + omega * change) * dt;
    *velocity = (*velocity - omega * temp) * decay;
    let next = target + (change + temp) * decay;
    if next.is_finite() { next } else { target }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    fn target(aim: Option<u16>, power: u16) -> Option<OpponentCueTarget> {
        Some(OpponentCueTarget {
            aim: aim.map(|steps| Aim::new(steps).unwrap()),
            power: ShotPower::new(power).unwrap(),
            spin: Spin::CENTER,
        })
    }

    fn settle(motion: &mut OpponentCueMotion, goal: Option<OpponentCueTarget>) -> OpponentCueView {
        let mut view = None;
        for _ in 0..120 {
            view = motion.update(goal, 1.0 / 60.0);
        }
        view.unwrap()
    }

    #[test]
    fn appears_at_its_target_without_sweeping_in() {
        let mut motion = OpponentCueMotion::default();
        let view = motion.update(target(Some(2_500), 0), 1.0 / 60.0).unwrap();
        assert!((view.aim - std::f32::consts::FRAC_PI_2).abs() < 1e-4);
    }

    #[test]
    fn eases_toward_new_aim_without_overshooting() {
        let mut motion = OpponentCueMotion::default();
        motion.update(target(Some(0), 0), 1.0 / 60.0);
        let first = motion.update(target(Some(1_000), 0), 1.0 / 60.0).unwrap();
        let goal = aim_radians(Aim::new(1_000).unwrap());
        assert!(
            first.aim > 0.0 && first.aim < goal,
            "moves part way in one frame"
        );
        let mut previous = first.aim;
        for _ in 0..120 {
            let view = motion.update(target(Some(1_000), 0), 1.0 / 60.0).unwrap();
            assert!(
                view.aim >= previous - 1e-6 && view.aim <= goal + 1e-4,
                "monotonic, no overshoot"
            );
            previous = view.aim;
        }
        assert!((previous - goal).abs() < 1e-3);
    }

    #[test]
    fn turns_the_short_way_across_a_half_turn() {
        let mut motion = OpponentCueMotion::default();
        // Just under +π to just over -π is a tiny counter-clockwise turn.
        motion.update(target(Some(4_990), 0), 1.0 / 60.0);
        let view = motion.update(target(Some(5_010), 0), 1.0 / 60.0).unwrap();
        let start = aim_radians(Aim::new(4_990).unwrap());
        assert!(wrap_angle(view.aim - start).abs() < 0.02);
        let settled = settle(&mut motion, target(Some(5_010), 0));
        assert!(wrap_angle(settled.aim - aim_radians(Aim::new(5_010).unwrap())).abs() < 1e-3);
    }

    #[test]
    fn rests_at_last_aim_with_power_drawn_back() {
        let mut motion = OpponentCueMotion::default();
        settle(&mut motion, target(Some(1_200), 8_000));
        // A pause or release keeps the heading and eases the power to zero.
        let resting = settle(&mut motion, target(None, 0));
        assert!((resting.aim - aim_radians(Aim::new(1_200).unwrap())).abs() < 1e-3);
        assert!(resting.power.abs() < 1e-3);
    }

    #[test]
    fn hides_outside_the_opponent_turn_and_returns_where_it_left_off() {
        let mut motion = OpponentCueMotion::default();
        let before = settle(&mut motion, target(Some(3_000), 0));
        assert!(motion.update(None, 1.0 / 60.0).is_none());
        let back = motion.update(target(None, 0), 1.0 / 60.0).unwrap();
        assert!((back.aim - before.aim).abs() < 1e-5);
    }

    #[test]
    fn a_long_frame_does_not_fling_the_cue() {
        let mut motion = OpponentCueMotion::default();
        motion.update(target(Some(0), 0), 1.0 / 60.0);
        let view = motion.update(target(Some(1_000), 0), 30.0).unwrap();
        let goal = aim_radians(Aim::new(1_000).unwrap());
        assert!(view.aim >= 0.0 && view.aim <= goal + 1e-4);
    }
}
