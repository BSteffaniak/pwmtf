//! Spherical ball surfaces with presentation-only rolling orientation.
use bevy::{
    asset::{load_internal_asset, uuid_handle},
    prelude::*,
    render::render_resource::AsBindGroup,
    shader::ShaderRef,
    sprite_render::{AlphaMode2d, Material2d, Material2dPlugin},
};

use crate::{BALL_RADIUS, CanonicalBall};

const SHADER: Handle<Shader> = uuid_handle!("294e8038-a855-4478-b6b9-a4132b05c18e");

pub struct BallArtPlugin;

impl Plugin for BallArtPlugin {
    fn build(&self, app: &mut App) {
        load_internal_asset!(app, SHADER, "ball_art.wgsl", Shader::from_wgsl);
        app.add_plugins(Material2dPlugin::<BallMaterial>::default());
    }
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct BallMaterial {
    #[uniform(0)]
    orientation: Vec4,
    #[uniform(0)]
    number: Vec4,
}

impl Material2d for BallMaterial {
    fn fragment_shader() -> ShaderRef {
        SHADER.into()
    }

    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

#[derive(Component)]
pub struct RollingSurface {
    previous: Vec2,
    orientation: Quat,
    hidden: bool,
}

// Transparent surfaces need distinct sort keys even at identical XY positions.
// Keep all balls above the pockets (Z=3) and below the aiming guide (Z=6).
fn ball_depth(number: u8) -> f32 {
    f32::from(number).mul_add(0.01, 4.0)
}

pub fn spawn(
    commands: &mut Commands,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<BallMaterial>,
    presentation: &crate::CanonicalPresentation,
) {
    let mesh = meshes.add(Rectangle::from_size(Vec2::splat(BALL_RADIUS * 2.5)));
    for number in 0..=15 {
        let position = presentation.target[&number];
        commands.spawn((
            Mesh2d(mesh.clone()),
            MeshMaterial2d(materials.add(BallMaterial {
                orientation: Quat::IDENTITY.to_array().into(),
                number: Vec4::new(f32::from(number), 0.0, 0.0, 0.0),
            })),
            Transform::from_translation(position.extend(ball_depth(number))),
            CanonicalBall(number),
            RollingSurface {
                previous: position,
                orientation: Quat::IDENTITY,
                hidden: false,
            },
        ));
    }
}

fn roll(orientation: Quat, displacement: Vec2) -> Quat {
    let distance = displacement.length();
    if distance <= f32::EPSILON {
        return orientation;
    }
    let axis = Vec3::new(-displacement.y, displacement.x, 0.0) / distance;
    (Quat::from_axis_angle(axis, distance / BALL_RADIUS) * orientation).normalize()
}

#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
pub fn update(
    mut balls: Query<(
        &Transform,
        &Visibility,
        &MeshMaterial2d<BallMaterial>,
        &mut RollingSurface,
    )>,
    mut materials: ResMut<Assets<BallMaterial>>,
) {
    for (transform, visibility, material, mut surface) in &mut balls {
        let position = transform.translation.truncate();
        let hidden = *visibility == Visibility::Hidden;
        if !hidden && !surface.hidden {
            surface.orientation = roll(surface.orientation, position - surface.previous);
        }
        surface.previous = position;
        surface.hidden = hidden;
        if let Some(mut material) = materials.get_mut(&material.0) {
            material.orientation = surface.orientation.to_array().into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visual_radius_matches_projected_physics_on_both_axes() {
        use pwmtf_billiards::{TableGeometry, Vector};
        let radius = TableGeometry::standard().ball_radius().micros();
        let center = crate::canonical_to_world(Vector::ZERO);
        for offset in [
            Vector::from_micros(radius, 0),
            Vector::from_micros(0, radius),
        ] {
            let projected_radius = crate::canonical_to_world(offset).distance(center);
            assert!((projected_radius - BALL_RADIUS).abs() < 0.0001);
        }
    }

    #[test]
    fn canonical_rack_has_no_overlapping_visible_ball_bodies() {
        use pwmtf_billiards::{RackSeed, TableGeometry, standard_rack};
        let rack = standard_rack(TableGeometry::standard(), RackSeed::new(42)).unwrap();
        for (index, ball) in rack.balls().iter().enumerate() {
            for other in &rack.balls()[index + 1..] {
                let distance = crate::canonical_to_world(ball.position)
                    .distance(crate::canonical_to_world(other.position));
                assert!(distance + 0.0001 >= BALL_RADIUS * 2.0);
            }
        }
    }

    #[test]
    fn overlapping_balls_have_unique_stable_depths_below_the_aiming_guide() {
        // Every ball may occupy the same XY position; order must depend only on
        // its stable identity, not spawn order, movement, or material updates.
        let mut depths: Vec<_> = (0..=15)
            .rev()
            .map(|number| (number, ball_depth(number)))
            .collect();
        depths.sort_by(|left, right| left.1.total_cmp(&right.1));
        for (expected, (number, depth)) in (0..=15).zip(&depths) {
            assert_eq!(expected, *number);
            assert!(*depth > 3.0 && *depth < 6.0);
        }
        assert!(depths.windows(2).all(|pair| pair[0].1 < pair[1].1));
    }

    #[test]
    fn rolling_distance_and_direction_follow_sphere_geometry() {
        let quarter = BALL_RADIUS * std::f32::consts::FRAC_PI_2;
        let right = roll(Quat::IDENTITY, Vec2::new(quarter, 0.0));
        assert!((right * Vec3::Z - Vec3::X).length() < 0.0001);
        let up = roll(Quat::IDENTITY, Vec2::new(0.0, quarter));
        assert!((up * Vec3::Z - Vec3::Y).length() < 0.0001);
        let reversed = roll(right, Vec2::new(-quarter, 0.0));
        assert!((reversed * Vec3::Z - Vec3::Z).length() < 0.0001);
    }

    #[test]
    fn rolling_is_frame_partition_independent_and_stationary_balls_keep_orientation() {
        let full = roll(Quat::IDENTITY, Vec2::new(40.0, 20.0));
        let mut split = Quat::IDENTITY;
        for _ in 0..10 {
            split = roll(split, Vec2::new(4.0, 2.0));
        }
        assert!((full * Vec3::Z - split * Vec3::Z).length() < 0.0001);
        assert_eq!(roll(split, Vec2::ZERO), split);
    }
}
