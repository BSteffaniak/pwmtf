//! The canonical fixed-point contact and cloth mechanics.
use super::{
    BallState, CushionAxis, PhysicsProfile, Scalar, SimulationEventKind, TableGeometry, Vector,
    VersionedShotCommand, VersionedTableState, integer_sqrt, mul_div, resolve_pockets,
    separate_overlap, squared_i128,
};

pub fn strike(ball: &mut BallState, command: VersionedShotCommand) {
    // Cue offset produces surface speed 5/2 times the translational impulse.
    ball.angular_velocity = Vector::from_micros(
        mul_div(ball.velocity.x.0, i64::from(command.spin.vertical), 4_000),
        mul_div(ball.velocity.y.0, i64::from(command.spin.vertical), 4_000),
    );
    ball.side_spin = Scalar::from_micros(mul_div(
        speed(ball.velocity),
        -i64::from(command.spin.side),
        4_000,
    ));
}

fn speed(v: Vector) -> i64 {
    integer_sqrt(squared_i128(v.x.0) + squared_i128(v.y.0))
}

pub fn advance(
    geometry: TableGeometry,
    profile: PhysicsProfile,
    state: &mut VersionedTableState,
    events: &mut Vec<SimulationEventKind>,
) {
    // Four substeps bound displacement at maximum shot speed, improving thin
    // contacts without changing the externally visible fixed-tick boundary.
    let substeps = 4;
    let frequency = i64::from(profile.ticks_per_second) * substeps;
    for _ in 0..substeps {
        for ball in state.balls.iter_mut().filter(|ball| !ball.pocketed) {
            ball.position.x.0 += ball.velocity.x.0 / frequency;
            ball.position.y.0 += ball.velocity.y.0 / frequency;
        }
        resolve_pockets(&mut state.balls, geometry, events);
        cushions(&mut state.balls, geometry, profile, events);

        // Revisit contacts so impulses propagate through a tight rack rather
        // than being limited to one identifier-ordered sweep.
        for _ in 0..4 {
            contacts(&mut state.balls, geometry, profile, events);
        }
        // Resolve contact before crossing the full-width pocket drop boundary.
        // This also catches displacement introduced by contact separation.
        resolve_pockets(&mut state.balls, geometry, events);
        for ball in state.balls.iter_mut().filter(|ball| !ball.pocketed) {
            cloth(ball, profile, frequency);
        }
    }
    for ball in state.balls.iter_mut().filter(|ball| ball.pocketed) {
        ball.angular_velocity = Vector::ZERO;
        ball.side_spin = Scalar::from_micros(0);
    }
}

fn cloth(ball: &mut BallState, profile: PhysicsProfile, frequency: i64) {
    let slip = Vector::from_micros(
        ball.velocity.x.0 - ball.angular_velocity.x.0,
        ball.velocity.y.0 - ball.angular_velocity.y.0,
    );
    let slipping = speed(slip);
    if slipping > 0 {
        // mu_slide=0.20; I=2/5 mR². Slip decays 7/2 times as
        // quickly as translation; clamp the impulse exactly at natural roll.
        let impulse = (1_962_000 / frequency).min(mul_div(slipping, 2, 7));
        if impulse > 0 {
            let dx = mul_div(slip.x.0, impulse, slipping);
            let dy = mul_div(slip.y.0, impulse, slipping);
            ball.velocity.x.0 -= dx;
            ball.velocity.y.0 -= dy;
            ball.angular_velocity.x.0 += mul_div(dx, 5, 2);
            ball.angular_velocity.y.0 += mul_div(dy, 5, 2);
        }
    }
    if slipping <= 7 {
        let velocity = speed(ball.velocity);
        let decrement = (profile.rolling_deceleration_per_second_squared.0 / frequency).max(1);
        if velocity <= decrement.max(profile.settling_speed_per_second.0) {
            ball.velocity = Vector::ZERO;
        } else {
            ball.velocity.x.0 = mul_div(ball.velocity.x.0, velocity - decrement, velocity);
            ball.velocity.y.0 = mul_div(ball.velocity.y.0, velocity - decrement, velocity);
        }
        ball.angular_velocity = ball.velocity;
    }
    // 10 rad/s² vertical spin decay at the standard ball radius.
    let decay = 285_750 / frequency;
    ball.side_spin.0 -= ball.side_spin.0.signum() * ball.side_spin.0.abs().min(decay);
}

fn contacts(
    balls: &mut [BallState],
    geometry: TableGeometry,
    profile: PhysicsProfile,
    events: &mut Vec<SimulationEventKind>,
) {
    let diameter = geometry.ball_radius.0 * 2;
    for index in 0..balls.len() {
        let (first_slice, rest) = balls.split_at_mut(index + 1);
        let first = &mut first_slice[index];
        if first.pocketed {
            continue;
        }
        for second in rest.iter_mut().filter(|ball| !ball.pocketed) {
            let dx = second.position.x.0 - first.position.x.0;
            let dy = second.position.y.0 - first.position.y.0;
            let squared = squared_i128(dx) + squared_i128(dy);
            if squared > squared_i128(diameter) {
                continue;
            }
            let distance = integer_sqrt(squared).max(1);
            let relative = i128::from(second.velocity.x.0 - first.velocity.x.0) * i128::from(dx)
                + i128::from(second.velocity.y.0 - first.velocity.y.0) * i128::from(dy);
            if relative < 0 && squared > 0 {
                let numerator =
                    -relative * i128::from(1_000_000 + profile.collision_restitution_millionths);
                let denominator = 2 * squared * 1_000_000;
                // Multiply BEFORE division; preserve sub-millimetre/s nudges.
                let ix = i64::try_from(numerator * i128::from(dx) / denominator).unwrap_or(0);
                let iy = i64::try_from(numerator * i128::from(dy) / denominator).unwrap_or(0);
                first.velocity.x.0 -= ix;
                first.velocity.y.0 -= iy;
                second.velocity.x.0 += ix;
                second.velocity.y.0 += iy;
                let tangent = mul_div(second.velocity.x.0 - first.velocity.x.0, -dy, distance)
                    + mul_div(second.velocity.y.0 - first.velocity.y.0, dx, distance)
                    - first.side_spin.0
                    - second.side_spin.0;
                let normal_impulse = speed(Vector::from_micros(ix, iy));
                let friction = (tangent / 7).clamp(-normal_impulse / 20, normal_impulse / 20);
                let tx = mul_div(friction, -dy, distance);
                let ty = mul_div(friction, dx, distance);
                first.velocity.x.0 += tx;
                first.velocity.y.0 += ty;
                second.velocity.x.0 -= tx;
                second.velocity.y.0 -= ty;
                first.side_spin.0 += mul_div(friction, 5, 2);
                second.side_spin.0 += mul_div(friction, 5, 2);
                events.push(SimulationEventKind::BallContact {
                    first: first.id,
                    second: second.id,
                });
            }
            separate_overlap(first, second, diameter, dx, dy, squared);
        }
    }
}

fn cushions(
    balls: &mut [BallState],
    geometry: TableGeometry,
    profile: PhysicsProfile,
    events: &mut Vec<SimulationEventKind>,
) {
    let segments = geometry.cushion_segments();
    let radius = geometry.ball_radius.0;
    for ball in balls.iter_mut().filter(|ball| !ball.pocketed) {
        for &(a, b) in &segments {
            let sx = b.x.0 - a.x.0;
            let sy = b.y.0 - a.y.0;
            let length_squared = squared_i128(sx) + squared_i128(sy);
            let projection = (i128::from(ball.position.x.0 - a.x.0) * i128::from(sx)
                + i128::from(ball.position.y.0 - a.y.0) * i128::from(sy))
            .clamp(0, length_squared);
            let closest_x =
                a.x.0 + i64::try_from(i128::from(sx) * projection / length_squared).unwrap_or(0);
            let closest_y =
                a.y.0 + i64::try_from(i128::from(sy) * projection / length_squared).unwrap_or(0);
            let dx = ball.position.x.0 - closest_x;
            let dy = ball.position.y.0 - closest_y;
            let squared = squared_i128(dx) + squared_i128(dy);
            if squared >= squared_i128(radius) || squared == 0 {
                continue;
            }
            let distance = integer_sqrt(squared).max(1);
            let normal =
                mul_div(ball.velocity.x.0, dx, distance) + mul_div(ball.velocity.y.0, dy, distance);
            ball.position.x.0 = closest_x + mul_div(dx, radius + 2, distance);
            ball.position.y.0 = closest_y + mul_div(dy, radius + 2, distance);
            if normal >= 0 {
                continue;
            }
            let impulse = mul_div(
                -normal,
                1_000_000 + i64::from(profile.cushion_restitution_millionths),
                1_000_000,
            );
            let tangent = mul_div(ball.velocity.x.0, -dy, distance)
                + mul_div(ball.velocity.y.0, dx, distance)
                - ball.side_spin.0;
            let friction = mul_div(tangent, 2, 7).clamp(-impulse / 5, impulse / 5);
            ball.velocity.x.0 += mul_div(impulse, dx, distance) + mul_div(friction, dy, distance);
            ball.velocity.y.0 += mul_div(impulse, dy, distance) - mul_div(friction, dx, distance);
            ball.side_spin.0 += mul_div(friction, 5, 2);
            events.push(SimulationEventKind::CushionContact {
                ball: ball.id,
                axis: if dx.abs() > dy.abs() {
                    CushionAxis::Horizontal
                } else {
                    CushionAxis::Vertical
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BallId;
    #[test]
    fn rolling_resistance_and_spin_snapshot_are_calibrated() {
        let mut ball = BallState::stationary(BallId::CUE, Vector::ZERO);
        ball.velocity.x.0 = 1_000_000;
        ball.angular_velocity = ball.velocity;
        ball.side_spin.0 = 100_000;
        let state = VersionedTableState::new(2, TableGeometry::standard(), 0, vec![ball]).unwrap();
        assert_eq!(
            VersionedTableState::from_bytes(TableGeometry::standard(), &state.to_bytes()).unwrap(),
            state
        );
        for _ in 0..960 {
            cloth(&mut ball, PhysicsProfile::standard(), 960);
        }
        assert!((ball.velocity.x.0 - 900_000).abs() < 1000);
        assert_eq!(ball.angular_velocity, ball.velocity);
        assert_eq!(ball.side_spin.0, 0);
    }

    #[test]
    fn follow_and_draw_act_on_cue_ball_after_impact() {
        for sign in [-1, 1] {
            let mut cue = BallState::stationary(BallId::CUE, Vector::ZERO);
            cue.angular_velocity.x.0 = sign * 500_000;
            for _ in 0..100 {
                cloth(&mut cue, PhysicsProfile::standard(), 960);
            }
            assert_eq!(cue.velocity.x.0.signum(), sign);
            assert!(cue.velocity.x.0.abs() > 100_000);
        }
    }

    #[test]
    fn collision_response_is_independent_of_identifier_order() {
        let geometry = TableGeometry::standard();
        let mut a = BallState::stationary(BallId::CUE, Vector::ZERO);
        let mut b = BallState::stationary(
            BallId::new(1).unwrap(),
            Vector::from_micros(geometry.ball_radius.0 * 2, 0),
        );
        a.velocity = Vector::from_micros(1_000_000, 200_000);
        a.side_spin.0 = 300_000;
        b.side_spin.0 = -100_000;
        let mut forward = vec![a, b];
        let mut reversed = vec![b, a];
        contacts(
            &mut forward,
            geometry,
            PhysicsProfile::standard(),
            &mut Vec::new(),
        );
        contacts(
            &mut reversed,
            geometry,
            PhysicsProfile::standard(),
            &mut Vec::new(),
        );
        assert_eq!(forward[0], reversed[1]);
        assert_eq!(forward[1], reversed[0]);
    }

    #[test]
    fn cushions_retain_normal_restitution_and_respond_to_english() {
        let geometry = TableGeometry::standard();
        let mut cue = BallState::stationary(
            BallId::CUE,
            Vector::from_micros(geometry.half_width.0 - 20_000, 200_000),
        );
        cue.velocity.x.0 = 1_000_000;
        cue.side_spin.0 = 300_000;
        cushions(
            std::slice::from_mut(&mut cue),
            geometry,
            PhysicsProfile::standard(),
            &mut Vec::new(),
        );
        assert_eq!(cue.velocity.x.0, -820_000);
        assert!(cue.velocity.y.0 < 0);
        assert!(cue.side_spin.0 < 300_000);
    }

    #[test]
    fn rolling_half_metre_per_second_travels_over_a_metre_before_stopping() {
        let geometry = TableGeometry::standard();
        let mut ball = BallState::stationary(BallId::CUE, Vector::from_micros(-800_000, 200_000));
        ball.velocity.x.0 = 500_000;
        ball.angular_velocity = ball.velocity;
        let mut state =
            VersionedTableState::new(crate::TABLE_STATE_VERSION, geometry, 0, vec![ball]).unwrap();
        let mut seconds = 0.0;
        for tick in 1..=2400 {
            let result = crate::advance_tick(geometry, PhysicsProfile::standard(), state).unwrap();
            state = result.state;
            if tick == 240 {
                assert!((state.balls[0].velocity.x.0 - 400_000).abs() < 1000);
            }
            if result.settled {
                seconds = f64::from(tick) / 240.0;
                break;
            }
        }
        let distance = state.balls[0].position.x.0 + 800_000;
        assert!((4.8..5.1).contains(&seconds), "stopped at {seconds}s");
        assert!(
            (1_230_000..1_260_000).contains(&distance),
            "travelled {distance} micrometres"
        );
    }

    #[test]
    fn retired_physics_and_table_versions_are_rejected() {
        let current = PhysicsProfile::standard();
        assert!(
            PhysicsProfile::new(
                1,
                current.ticks_per_second,
                current.maximum_ticks,
                current.maximum_speed_per_second,
                current.rolling_deceleration_per_second_squared,
                current.cushion_restitution_millionths,
                current.collision_restitution_millionths,
                current.settling_speed_per_second
            )
            .is_err()
        );
        let state =
            crate::standard_rack(TableGeometry::standard(), crate::RackSeed::new(42)).unwrap();
        let mut bytes = state.to_bytes();
        bytes[..2].copy_from_slice(&1_u16.to_be_bytes());
        assert!(VersionedTableState::from_bytes(TableGeometry::standard(), &bytes).is_err());
    }

    #[test]
    fn pocket_mouth_accepts_center_shots_and_jaws_reject_off_center_shots() {
        let geometry = TableGeometry::standard();
        let profile = PhysicsProfile::standard();
        let mut centered = BallState::stationary(BallId::CUE, Vector::from_micros(0, 600_000));
        centered.velocity.y.0 = 500_000;
        cushions(
            std::slice::from_mut(&mut centered),
            geometry,
            profile,
            &mut Vec::new(),
        );
        assert_eq!(centered.velocity.y.0, 500_000);
        let mut clipped = BallState::stationary(BallId::CUE, Vector::from_micros(60_000, 615_000));
        clipped.velocity.y.0 = 500_000;
        cushions(
            std::slice::from_mut(&mut clipped),
            geometry,
            profile,
            &mut Vec::new(),
        );
        assert!(clipped.velocity.y.0 < 0);
        assert!(clipped.velocity.x.0 < 0);
    }

    #[test]
    fn every_pocket_accepts_a_centered_approach_and_roundtrips_until_capture() {
        let geometry = TableGeometry::standard();
        for pocket in crate::PocketId::ALL {
            let center = crate::pocket_position(pocket, geometry);
            let x = center.x.0.signum();
            let y = center.y.0.signum();
            let mut ball = BallState::stationary(
                BallId::CUE,
                Vector::from_micros(center.x.0 - x * 100_000, center.y.0 - y * 100_000),
            );
            ball.velocity = Vector::from_micros(x * 500_000, y * 500_000);
            let mut state =
                VersionedTableState::new(crate::TABLE_STATE_VERSION, geometry, 0, vec![ball])
                    .unwrap();
            for _ in 0..100 {
                state = crate::advance_tick(geometry, PhysicsProfile::standard(), state)
                    .unwrap()
                    .state;
                assert_eq!(
                    VersionedTableState::from_bytes(geometry, &state.to_bytes()).unwrap(),
                    state
                );
                if state.balls[0].pocketed {
                    break;
                }
            }
            assert!(state.balls[0].pocketed, "missed {pocket:?}");
        }
    }

    #[test]
    fn swept_pocket_approaches_never_escape_the_board() {
        let geometry = TableGeometry::standard();
        for pocket in crate::PocketId::ALL {
            let center = crate::pocket_position(pocket, geometry);
            for offset in (-100_000..=100_000).step_by(20_000) {
                for sideways in [-2_000_000, 0, 2_000_000] {
                    let x = center.x.0.signum();
                    let y = center.y.0.signum();
                    let position = Vector::from_micros(
                        center.x.0 - x * 160_000 + offset,
                        center.y.0 - y * 160_000,
                    );
                    let mut ball = BallState::stationary(BallId::CUE, position);
                    ball.velocity = Vector::from_micros(x * 5_000_000 + sideways, y * 5_000_000);
                    let Ok(mut state) = VersionedTableState::new(
                        crate::TABLE_STATE_VERSION,
                        geometry,
                        0,
                        vec![ball],
                    ) else {
                        continue;
                    };
                    for _ in 0..240 {
                        state = crate::advance_tick(geometry, PhysicsProfile::standard(), state)
                            .unwrap()
                            .state;
                        let ball = state.balls[0];
                        if ball.pocketed {
                            break;
                        }
                        assert!(
                            ball.position.x.0.abs() <= geometry.half_width.0
                                && ball.position.y.0.abs() <= geometry.half_height.0,
                            "escaped {pocket:?}, offset={offset}, lateral={sideways}: {:?}",
                            ball.position
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn soft_contact_transfers_momentum_without_truncation() {
        let geometry = TableGeometry::standard();
        let mut balls = vec![
            BallState::stationary(BallId::CUE, Vector::ZERO),
            BallState::stationary(
                BallId::new(1).unwrap(),
                Vector::from_micros(geometry.ball_radius.0 * 2, 0),
            ),
        ];
        balls[0].velocity.x.0 = 20_000;
        contacts(
            &mut balls,
            geometry,
            PhysicsProfile::standard(),
            &mut Vec::new(),
        );
        assert_eq!(balls[0].velocity.x.0, 400);
        assert_eq!(balls[1].velocity.x.0, 19_600);
    }
    #[test]
    fn sliding_center_hit_transitions_to_five_sevenths_speed() {
        let mut ball = BallState::stationary(BallId::CUE, Vector::ZERO);
        ball.velocity.x.0 = 1_000_000;
        for _ in 0..150 {
            cloth(&mut ball, PhysicsProfile::standard(), 960);
        }
        assert!((ball.velocity.x.0 - 714_286).abs() < 4_000);
        assert!((ball.velocity.x.0 - ball.angular_velocity.x.0).abs() <= 7);
    }
}
