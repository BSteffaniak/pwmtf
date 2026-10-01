//! Geometric first-contact aiming aids, never authoritative shot simulation.
use crate::{
    BALL_RADIUS, CanonicalPresentation, PrototypeInput, canonical_to_world, sandbox::Sandbox,
};
use bevy::prelude::*;

use std::sync::atomic::{AtomicU8, Ordering};
static MODE: AtomicU8 = AtomicU8::new(1);
static UNLIMITED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub const GUIDE_LENGTH: f32 = 500.0;

pub fn unlimited_range() -> bool {
    UNLIMITED.load(Ordering::Relaxed)
}

/// Allows short projections beyond the visible incoming aim line.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_unlimited_aim_range(enabled: bool) {
    UNLIMITED.store(enabled, Ordering::Relaxed);
}

/// Selects geometric (0), short physics (1), or full physics (2) guides.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn set_aim_mode(mode: u8) {
    MODE.store(mode.min(2), Ordering::Relaxed);
}

fn circle_hit(origin: Vec2, direction: Vec2, center: Vec2, radius: f32) -> Option<f32> {
    let offset = center - origin;
    let along = offset.dot(direction);
    let perpendicular = along.mul_add(-along, offset.length_squared());
    if along <= 0.0 || perpendicular > radius * radius {
        return None;
    }
    let distance = along - (radius.mul_add(radius, -perpendicular)).max(0.0).sqrt();
    (distance >= 0.0).then_some(distance)
}

fn rail_hit(origin: Vec2, direction: Vec2, segments: &[(Vec2, Vec2)]) -> f32 {
    let mut nearest = 1400.0_f32;
    for &(a, b) in segments {
        let tangent = (b - a).normalize();
        let normal = Vec2::new(-tangent.y, tangent.x);
        let denominator = direction.dot(normal);
        if denominator.abs() > 0.0001 {
            for side in [-1.0_f32, 1.0] {
                let distance = side.mul_add(BALL_RADIUS, (a - origin).dot(normal)) / denominator;
                let along = (origin + direction * distance - a).dot(tangent);
                if distance >= 0.0 && along >= 0.0 && along <= a.distance(b) {
                    nearest = nearest.min(distance);
                }
            }
        }
        for endpoint in [a, b] {
            if let Some(distance) = circle_hit(origin, direction, endpoint, BALL_RADIUS) {
                nearest = nearest.min(distance);
            }
        }
    }
    nearest
}

fn first_hit(
    origin: Vec2,
    direction: Vec2,
    balls: &std::collections::BTreeMap<u8, Vec2>,
    rail_distance: f32,
) -> (f32, Option<Vec2>) {
    let mut distance = rail_distance;
    let mut target = None;
    for (&number, &center) in balls {
        if number == 0 {
            continue;
        }
        if let Some(hit) = circle_hit(origin, direction, center, BALL_RADIUS * 2.0)
            && hit < distance
        {
            distance = hit;
            target = Some(center);
        }
    }
    (distance, target)
}

const fn power_stroke_length(power: f32, clearance: f32) -> f32 {
    power
        .clamp(0.0, 1.0)
        .mul_add(320.0, BALL_RADIUS)
        .min(clearance)
        .min(GUIDE_LENGTH)
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn draw(
    mut gizmos: Gizmos,
    presentation: Res<CanonicalPresentation>,
    input: Res<PrototypeInput>,
    sandbox: Res<Sandbox>,
    mut exact: ResMut<crate::exact_preview::ExactPreview>,
) {
    #[cfg(target_arch = "wasm32")]
    let remote = if !sandbox.enabled {
        crate::browser_transport::remote_preview()
    } else {
        None
    };
    #[cfg(target_arch = "wasm32")]
    let mut preview_input = crate::PrototypeInput::default();
    #[cfg(target_arch = "wasm32")]
    let has_remote = remote.is_some();
    #[cfg(target_arch = "wasm32")]
    let input = if let Some(preview) = remote {
        preview_input.aim_angle = f32::from(preview.aim.steps()) * std::f32::consts::TAU
            / f32::from(pwmtf_game_domain::Aim::STEPS_PER_TURN);
        preview_input.power =
            f32::from(preview.power.units()) / f32::from(pwmtf_game_domain::ShotPower::MAX);
        preview_input.spin = Vec2::new(
            f32::from(preview.spin.side) / 10_000.0,
            f32::from(preview.spin.vertical) / 10_000.0,
        );
        &preview_input
    } else {
        &*input
    };
    #[cfg(not(target_arch = "wasm32"))]
    let input = &*input;
    let enabled = sandbox.enabled && !sandbox.moving;
    #[cfg(target_arch = "wasm32")]
    let enabled =
        enabled || crate::browser_transport::accepts_active_player_command() || remote.is_some();
    if !enabled {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    if input.placing_cue_ball {
        return;
    }
    #[cfg(target_arch = "wasm32")]
    let show_power_stroke = has_remote || crate::shot_control_mode() == 1;
    #[cfg(not(target_arch = "wasm32"))]
    let show_power_stroke = crate::shot_control_mode() == 1;
    if enabled
        && show_power_stroke
        && let Some(&origin) = presentation.target.get(&0)
    {
        let direction = Vec2::from_angle(input.aim_angle);
        let segments: Vec<_> = pwmtf_game_domain::TableGeometry::standard()
            .cushion_segments()
            .into_iter()
            .map(|(a, b)| (canonical_to_world(a), canonical_to_world(b)))
            .collect();
        let (clearance, _) = first_hit(
            origin,
            direction,
            &presentation.target,
            rail_hit(origin, direction, &segments),
        );
        // The amber stroke is a bounded power indicator, not a claim about
        // the ball's eventual trajectory or the physics-accurate guide.
        let length = power_stroke_length(input.power, clearance);
        if length > BALL_RADIUS {
            gizmos.line_2d(
                origin + direction * BALL_RADIUS,
                origin + direction * length,
                Color::srgb(0.96, 0.59, 0.12),
            );
        }
    }
    let mode = MODE.load(Ordering::Relaxed);
    if mode != 0 && !(show_power_stroke && input.power <= 0.0) {
        let state = if sandbox.enabled {
            Some((
                sandbox.table.clone(),
                pwmtf_game_domain::PhysicsProfile::standard(),
                pwmtf_game_domain::TableGeometry::standard(),
            ))
        } else {
            #[cfg(target_arch = "wasm32")]
            {
                crate::browser_transport::preview_state()
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                None
            }
        };
        if let Some((table, physics, geometry)) = state {
            exact.draw(
                &mut gizmos,
                table,
                physics,
                geometry,
                crate::practice_shot(input),
                mode == 1,
            );
        }
        return;
    }
    let Some(&origin) = presentation.target.get(&0) else {
        return;
    };
    let direction = Vec2::from_angle(input.aim_angle);
    let segments: Vec<_> = pwmtf_game_domain::TableGeometry::standard()
        .cushion_segments()
        .into_iter()
        .map(|(a, b)| (canonical_to_world(a), canonical_to_world(b)))
        .collect();
    let (distance, target) = first_hit(
        origin,
        direction,
        &presentation.target,
        rail_hit(origin, direction, &segments).min(if unlimited_range() {
            f32::INFINITY
        } else {
            GUIDE_LENGTH
        }),
    );
    let contact = origin + direction * distance;
    let white = Color::srgba(0.96, 0.96, 0.86, 0.75);
    if distance > BALL_RADIUS {
        gizmos.line_2d(
            origin + direction * BALL_RADIUS,
            origin + direction * distance.min(GUIDE_LENGTH),
            white,
        );
    }
    let Some(target) = target else {
        return;
    };
    gizmos.circle_2d(contact, BALL_RADIUS, white);
    let normal = (target - contact).normalize_or_zero();
    let transfer = direction.dot(normal).clamp(0.0, 1.0);
    let tangent = direction - normal * transfer;
    let length = (transfer * 100.0).min(rail_hit(target, normal, &segments));
    if length > BALL_RADIUS {
        gizmos.line_2d(
            target + normal * BALL_RADIUS,
            target + normal * length,
            Color::srgb(0.95, 0.72, 0.25),
        );
    }
    if tangent.length() > 0.05 {
        let heading = tangent.normalize();
        let length = (tangent.length() * 80.0).min(rail_hit(contact, heading, &segments));
        if length > BALL_RADIUS {
            gizmos.line_2d(
                contact + heading * BALL_RADIUS,
                contact + heading * length,
                white,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn power_stroke_grows_and_stops_at_first_contact() {
        let low = power_stroke_length(0.2, 500.0);
        assert!(low > BALL_RADIUS);
        assert!(power_stroke_length(0.8, 500.0) > low);
        assert!(power_stroke_length(1.0, 90.0) <= 90.0);
        assert!(power_stroke_length(1.0, 900.0) <= GUIDE_LENGTH);
        assert!(power_stroke_length(0.0, 500.0) <= BALL_RADIUS);
    }

    #[test]
    fn nearest_ball_wins_and_balls_behind_are_ignored() {
        let balls = [
            (1, Vec2::new(100.0, 0.0)),
            (2, Vec2::new(50.0, 0.0)),
            (3, Vec2::new(-30.0, 0.0)),
        ]
        .into_iter()
        .collect();
        let (distance, target) = first_hit(Vec2::ZERO, Vec2::X, &balls, 200.0);
        assert!((distance - (BALL_RADIUS.mul_add(-2.0, 50.0))).abs() < 0.001);
        assert_eq!(target, Some(Vec2::new(50.0, 0.0)));
        assert_eq!(first_hit(Vec2::ZERO, Vec2::X, &balls, 10.0).1, None);
    }
    #[test]
    fn rail_contact_accounts_for_ball_radius() {
        let distance = rail_hit(
            Vec2::ZERO,
            Vec2::X,
            &[(Vec2::new(100.0, -50.0), Vec2::new(100.0, 50.0))],
        );
        assert!((distance - (100.0 - BALL_RADIUS)).abs() < 0.001);
        assert!(
            circle_hit(
                Vec2::ZERO,
                Vec2::X,
                Vec2::new(100.0, BALL_RADIUS * 3.0),
                BALL_RADIUS * 2.0
            )
            .is_none()
        );
    }
}
