//! Shaped bodies: boxes, capsules, compounds, rotation, friction, kinematic
//! angle control, flippers, drives, acceleration fields and bumpers.

use crate::{
    Aabb, Attachment, BodyDef, BodyId, DISC_WORLD_STATE_VERSION, DecodeError, Drive, Event,
    FieldDef, MICROS_PER_UNIT, MILLIONTHS, PI_MICRORADIANS, Part, Region, Shape, ShapedBodyDef,
    TRIG_UNIT, Vector, WORLD_STATE_VERSION, WallDef, World, WorldConfig, WorldError, sin_cos,
};

const M: i64 = MICROS_PER_UNIT;
const QUARTER: i64 = PI_MICRORADIANS / 2;

fn config() -> WorldConfig {
    WorldConfig {
        ticks_per_second: 60,
        max_substeps: 16,
        max_speed: 12 * M,
    }
}

fn arena(world: &mut World, half_width: i64, half_height: i64) {
    world
        .add_bounds(
            Aabb::new(
                Vector::new(-half_width, -half_height),
                Vector::new(half_width, half_height),
            )
            .unwrap(),
            MILLIONTHS,
            u32::MAX,
        )
        .unwrap();
}

fn ball(position: Vector, velocity: Vector) -> BodyDef {
    BodyDef {
        velocity,
        ..BodyDef::dynamic(30_000, 10, position)
    }
}

fn rect(half_width: i64, half_height: i64) -> Shape {
    Shape::Rect {
        half_width,
        half_height,
    }
}

fn capsule(half_length: i64, radius: i64) -> Shape {
    Shape::Capsule {
        half_length,
        radius,
    }
}

/// Linear momentum and angular momentum about the origin of every dynamic
/// body (mass units times micro-units, scaled down for angular terms).
fn momenta(world: &World) -> (i128, i128, i128) {
    world
        .bodies()
        .filter(|(_, body)| body.kind() == crate::BodyKind::Dynamic)
        .fold((0, 0, 0), |(x, y, l), (_, body)| {
            let mass = i128::from(body.def().mass);
            let (p, v) = (body.position(), body.velocity());
            let orbital = mass
                * (i128::from(p.x) * i128::from(v.y) - i128::from(p.y) * i128::from(v.x))
                / i128::from(M);
            let spin =
                mass * i128::from(body.inertia_per_mass()) * i128::from(body.angular_velocity())
                    / (i128::from(M) * 1_000_000);
            (
                x + mass * i128::from(v.x),
                y + mass * i128::from(v.y),
                l + orbital + spin,
            )
        })
}

/// Twice the kinetic energy, linear plus rotational, in mass·µu²/s².
fn energy(world: &World) -> i128 {
    world
        .bodies()
        .filter(|(_, body)| body.kind() == crate::BodyKind::Dynamic)
        .map(|(_, body)| {
            let mass = i128::from(body.def().mass);
            let omega = i128::from(body.angular_velocity());
            mass * body.velocity().length_squared()
                + mass * i128::from(body.inertia_per_mass()) * omega * omega / 1_000_000_000_000
        })
        .sum()
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: i64) -> i64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let bound = u64::try_from(bound).unwrap();
        i64::try_from((self.0 >> 33) % (2 * bound + 1)).unwrap() - i64::try_from(bound).unwrap()
    }
}

#[test]
fn shaped_bodies_are_validated() {
    let mut world = World::new(config()).unwrap();
    let dynamic = |parts| ShapedBodyDef::dynamic(10, Vector::ZERO, parts);
    assert_eq!(
        world.add_shaped_body(dynamic(Vec::new())),
        Err(WorldError::InvalidParameter)
    );
    assert_eq!(
        world.add_shaped_body(dynamic(vec![Part::new(rect(0, 30_000))])),
        Err(WorldError::InvalidParameter)
    );
    // The thinnest part must be tunnelling-safe (25 mm at 12 m/s, 60 Hz).
    assert_eq!(
        world.add_shaped_body(dynamic(vec![Part::new(rect(100_000, 20_000))])),
        Err(WorldError::BodyTooSmall {
            radius: 20_000,
            minimum: 25_000
        })
    );
    // Kicks and spindles belong to kinematic bodies only.
    assert_eq!(
        world.add_shaped_body(dynamic(vec![Part {
            kick: M,
            ..Part::new(capsule(0, 30_000))
        }])),
        Err(WorldError::InvalidParameter)
    );
    assert_eq!(
        world.add_shaped_body(dynamic(vec![Part {
            attachment: Attachment::Spindle,
            ..Part::new(capsule(0, 30_000))
        }])),
        Err(WorldError::InvalidParameter)
    );
    let mut ranged = dynamic(vec![Part::new(capsule(0, 30_000))]);
    ranged.angle_range = Some((0, 1));
    assert_eq!(
        world.add_shaped_body(ranged),
        Err(WorldError::InvalidParameter)
    );
    let mut driven = ShapedBodyDef::kinematic(Vector::ZERO, M, M, vec![Part::new(rect(M, M))]);
    driven.drive = Some(Drive {
        acceleration: M,
        turn_rate: M,
        turn_acceleration: M,
        grip: M,
    });
    assert_eq!(
        world.add_shaped_body(driven),
        Err(WorldError::InvalidParameter)
    );
    // A valid body: radius is the bounding distance, the angular cap is
    // lowered so the tip never outruns the world's speed limit.
    let id = world
        .add_shaped_body(ShapedBodyDef {
            max_angular_speed: crate::MAX_ANGULAR_SPEED,
            ..dynamic(vec![Part::at(
                capsule(100_000, 30_000),
                Vector::new(0, 200_000),
                0,
            )])
        })
        .unwrap();
    let body = world.body(id).unwrap();
    assert_eq!(body.radius(), 330_000);
    assert_eq!(body.thickness(), 30_000);
    assert_eq!(body.max_angular_speed(), 12 * M * 1_000_000 / 330_000);
    // Plain discs reject shaped operations.
    let plain = world
        .add_body(ball(Vector::new(M, 0), Vector::ZERO))
        .unwrap();
    assert_eq!(world.set_angle(plain, 0), Err(WorldError::NotShaped));
    assert_eq!(world.set_angle_target(id, 0), Err(WorldError::NotKinematic));
    assert_eq!(world.set_drive(id, 0, 0), Err(WorldError::InvalidParameter));
    assert_eq!(
        world.set_wall_friction(crate::WallId(3), 0),
        Err(WorldError::UnknownWall)
    );
}

#[test]
fn a_disc_bounces_off_a_rotated_box_along_its_face_normal() {
    let mut world = World::new(config()).unwrap();
    // A box turned 45 degrees: its lower-left face normal points to
    // (-1, -1)/√2. A disc falling straight down onto that face leaves
    // sideways to the left.
    let wedge = world
        .add_shaped_body(ShapedBodyDef {
            angle: PI_MICRORADIANS / 4,
            ..ShapedBodyDef::kinematic(Vector::ZERO, M, 0, vec![Part::new(rect(200_000, 200_000))])
        })
        .unwrap();
    let disc = world
        .add_body(ball(Vector::new(-100_000, 400_000), Vector::new(0, -2 * M)))
        .unwrap();
    let mut contact = None;
    for _ in 0..20 {
        for event in world.step().events {
            if let Event::PartContact { normal, point, .. } = event {
                contact.get_or_insert((normal, point));
            }
        }
    }
    let (normal, point) = contact.expect("the disc hit the box");
    let diagonal = TRIG_UNIT * 7_071 / 10_000;
    assert!(
        (normal.x + diagonal).abs() < TRIG_UNIT / 1_000,
        "{normal:?}"
    );
    assert!(
        (normal.y - diagonal).abs() < TRIG_UNIT / 1_000,
        "{normal:?}"
    );
    // The contact lies on the upper-left face (x + ... within the box's
    // half-diagonal along the normal).
    let distance = (i128::from(point.y) - i128::from(point.x)) * 7_071 / 10_000;
    assert!((distance - 200_000).abs() < 2_000, "{point:?}");
    let velocity = world.body(disc).unwrap().velocity();
    // Elastic reflection of (0, -2) off normal (-1, 1)/√2 is (-2, 0).
    assert!((velocity.x + 2 * M).abs() < 5_000, "{velocity:?}");
    assert!(velocity.y.abs() < 5_000, "{velocity:?}");
    // No penetration remains.
    let position = world.body(disc).unwrap().position();
    let separation = (i128::from(position.y) - i128::from(position.x)) * 7_071 / 10_000;
    assert!(separation >= 230_000 - 1_000, "{position:?}");
    assert_eq!(world.body(wedge).unwrap().position(), Vector::ZERO);
}

#[test]
fn shapes_at_maximum_speed_never_tunnel_through_thin_walls() {
    let mut random = Lcg(17);
    for round in 0..60 {
        let mut world = World::new(config()).unwrap();
        arena(&mut world, M, M / 2);
        world
            .add_wall(WallDef {
                start: Vector::new(0, -M / 2),
                end: Vector::new(0, M / 2),
                restitution: MILLIONTHS,
                layers: u32::MAX,
            })
            .unwrap();
        let shape = match round % 3 {
            0 => rect(60_000, 25_000),
            1 => capsule(50_000, 25_000),
            _ => Shape::Disc { radius: 25_000 },
        };
        let velocity = Vector::new(random.next(12 * M), random.next(12 * M));
        let velocity = velocity.scale(i128::from(12 * M), i128::from(velocity.length().max(1)));
        let start = Vector::new(
            if round % 2 == 0 { -M / 2 } else { M / 2 },
            random.next(M / 4),
        );
        let id = world
            .add_shaped_body(ShapedBodyDef {
                angular_velocity: random.next(30_000_000),
                angle: random.next(PI_MICRORADIANS),
                surface_friction: 300_000,
                body: BodyDef {
                    velocity,
                    ..BodyDef::dynamic(1, 10, start)
                },
                ..ShapedBodyDef::new(BodyDef::dynamic(1, 10, start), vec![Part::new(shape)])
            })
            .unwrap();
        world.set_wall_friction(crate::WallId(4), 500_000).unwrap();
        for _ in 0..300 {
            world.step();
            let position = world.body(id).unwrap().position();
            assert_eq!(
                position.x.signum(),
                start.x.signum(),
                "round {round}: crossed the divider at {position:?}"
            );
            assert!(
                position.x.abs() < M && position.y.abs() < M / 2,
                "round {round}: escaped at {position:?}"
            );
            for pose in world.part_poses(id) {
                assert_eq!(pose.center.x.signum(), start.x.signum());
            }
        }
    }
}

#[test]
fn off_center_hits_conserve_momentum_and_never_gain_energy() {
    let mut world = World::new(config()).unwrap();
    // A disc strikes the end of a resting capsule: the capsule both moves
    // and spins.
    let bar = world
        .add_shaped_body(ShapedBodyDef::dynamic(
            20,
            Vector::ZERO,
            vec![Part::new(capsule(200_000, 30_000))],
        ))
        .unwrap();
    let disc = world
        .add_body(ball(Vector::new(150_000, -400_000), Vector::new(0, 3 * M)))
        .unwrap();
    let before = (momenta(&world), energy(&world));
    let mut hit = false;
    for _ in 0..20 {
        hit |= world
            .step()
            .events
            .iter()
            .any(|event| matches!(event, Event::BodyContact { .. }));
    }
    assert!(hit);
    let after = (momenta(&world), energy(&world));
    let bar_body = world.body(bar).unwrap();
    assert!(bar_body.velocity().y > 0, "{:?}", bar_body.velocity());
    // Struck right of center from below: counter-clockwise spin.
    assert!(bar_body.angular_velocity() > 0);
    assert!(world.body(disc).unwrap().velocity().y < 3 * M);
    let tolerance = 30 * 1_000;
    assert!(
        (after.0.0 - before.0.0).abs() < tolerance,
        "{before:?} {after:?}"
    );
    assert!(
        (after.0.1 - before.0.1).abs() < tolerance,
        "{before:?} {after:?}"
    );
    // Angular momentum about the origin is conserved too.
    let angular = (after.0.2 - before.0.2).abs();
    assert!(angular < before.0.2.abs() / 500, "{before:?} {after:?}");
    assert!(
        after.1 <= before.1 + before.1 / 10_000,
        "{before:?} {after:?}"
    );
    // Elastic: nearly all the energy remains.
    assert!(after.1 > before.1 * 99 / 100, "{before:?} {after:?}");
}

#[test]
fn boxes_collide_with_boxes_and_walls_and_settle() {
    let mut world = World::new(config()).unwrap();
    arena(&mut world, M, M);
    let crate_def = |position, velocity, angle| ShapedBodyDef {
        angle,
        angular_damping: 200_000,
        surface_friction: 400_000,
        body: BodyDef {
            velocity,
            restitution: 500_000,
            friction: M,
            ..BodyDef::dynamic(1, 30, position)
        },
        ..ShapedBodyDef::dynamic(30, position, vec![Part::new(rect(100_000, 60_000))])
    };
    let left = world
        .add_shaped_body(crate_def(
            Vector::new(-400_000, 0),
            Vector::new(3 * M, 0),
            0,
        ))
        .unwrap();
    let right = world
        .add_shaped_body(crate_def(
            Vector::new(300_000, 30_000),
            Vector::ZERO,
            400_000,
        ))
        .unwrap();
    let before = momenta(&world);
    let mut contact = false;
    for _ in 0..10 {
        contact |= world.step().events.iter().any(|event| {
            matches!(event, Event::BodyContact { first, second, .. } if *first == left && *second == right)
        });
    }
    assert!(contact);
    assert!(world.body(right).unwrap().velocity().x > M);
    assert!(world.body(right).unwrap().angular_velocity() != 0);
    let after = momenta(&world);
    // Friction acts over the ten ticks; compare loosely.
    assert!(after.0 <= before.0);
    for _ in 0..600 {
        world.step();
    }
    for id in [left, right] {
        let body = world.body(id).unwrap();
        assert_eq!(body.velocity(), Vector::ZERO, "{id:?} comes to rest");
        for pose in world.part_poses(id) {
            assert!(pose.center.x.abs() < M && pose.center.y.abs() < M);
        }
    }
    // At rest nothing moves: no jitter.
    let snapshot = world.clone();
    for _ in 0..120 {
        world.step();
    }
    for (id, body) in world.bodies() {
        let then = snapshot.body(id).unwrap();
        assert_eq!(body.position(), then.position());
        assert_eq!(body.angle(), then.angle());
    }
}

#[test]
fn friction_spins_a_ball_glancing_off_a_wall() {
    let mut world = World::new(config()).unwrap();
    let wall = world
        .add_wall(WallDef {
            start: Vector::new(-M, 0),
            end: Vector::new(M, 0),
            restitution: 800_000,
            layers: u32::MAX,
        })
        .unwrap();
    world.set_wall_friction(wall, 600_000).unwrap();
    let spinning = |world: &mut World, x: i64, friction: u32| {
        world
            .add_shaped_body(ShapedBodyDef {
                surface_friction: friction,
                body: BodyDef {
                    velocity: Vector::new(2 * M, -2 * M),
                    restitution: 800_000,
                    layers: 1,
                    mask: 1,
                    ..BodyDef::dynamic(1, 10, Vector::new(x, 200_000))
                },
                ..ShapedBodyDef::dynamic(
                    10,
                    Vector::new(x, 200_000),
                    vec![Part::new(Shape::Disc { radius: 30_000 })],
                )
            })
            .unwrap()
    };
    let grippy = spinning(&mut world, -500_000, 600_000);
    let slick = spinning(&mut world, 0, 0);
    for _ in 0..12 {
        world.step();
    }
    let (grippy, slick) = (world.body(grippy).unwrap(), world.body(slick).unwrap());
    // Both bounce up.
    assert!(grippy.velocity().y > 0 && slick.velocity().y > 0);
    // Without friction the tangential speed and spin are untouched.
    assert_eq!(slick.velocity().x, 2 * M);
    assert_eq!(slick.angular_velocity(), 0);
    // With friction the ball loses tangential speed and rolls: moving +x
    // along a floor below it spins clockwise (negative).
    assert!(
        grippy.velocity().x < 2 * M - M / 4,
        "{:?}",
        grippy.velocity()
    );
    assert!(grippy.angular_velocity() < 0);
    // A solid disc sticks once its contact point stops sliding:
    // v = -ω r, at 2/3 of the incoming tangential speed.
    let rolling = -grippy.angular_velocity() * 30_000 / 1_000_000;
    assert!(
        (rolling - grippy.velocity().x).abs() < 20_000,
        "{rolling} vs {:?}",
        grippy.velocity()
    );
}

/// A foosball-like rod: figures on spindles along a rod that slides in y.
fn rod_world() -> (World, BodyId, BodyId) {
    let mut world = World::new(config()).unwrap();
    let figure = |y| Part {
        attachment: Attachment::Spindle,
        ..Part::at(rect(80_000, 25_000), Vector::new(0, y), 0)
    };
    let rod = world
        .add_shaped_body(ShapedBodyDef {
            angle: QUARTER,
            ..ShapedBodyDef::kinematic(
                Vector::ZERO,
                2 * M,
                40_000_000,
                vec![figure(-300_000), figure(0), figure(300_000)],
            )
        })
        .unwrap();
    // The ball rests in front of the middle figure's upper half.
    let ball = world
        .add_body(ball(Vector::new(70_000, 50_000), Vector::ZERO))
        .unwrap();
    (world, rod, ball)
}

#[test]
fn a_spinning_rod_kicks_the_ball_faster_in_the_spin_direction() {
    let (mut world, rod, ball) = rod_world();
    // Hanging figures (along y) clear the ball.
    for _ in 0..5 {
        world.step();
    }
    assert_eq!(world.body(ball).unwrap().velocity(), Vector::ZERO);
    // Spin clockwise toward horizontal: the upper half of every figure
    // sweeps toward +x.
    world.set_angle_target(rod, 0).unwrap();
    let mut struck = None;
    for _ in 0..30 {
        for event in world.step().events {
            if let Event::PartContact {
                first, first_part, ..
            } = event
            {
                assert_eq!(first, rod);
                struck.get_or_insert(first_part);
            }
        }
    }
    assert_eq!(struck, Some(1), "the middle figure struck");
    let velocity = world.body(ball).unwrap().velocity();
    let rod_body = world.body(rod).unwrap();
    assert_eq!(rod_body.angle(), 0);
    // The figure's surface moved at ω·r ≈ 40 rad/s × ~0.06 m ≈ 2.4 m/s;
    // the ball leaves faster than that, toward +x.
    assert!(velocity.x > 2 * M, "{velocity:?}");
    assert!(velocity.x > velocity.y.abs() * 2, "{velocity:?}");
    // Every figure turned on its own spindle; the rod did not move.
    assert_eq!(rod_body.position(), Vector::ZERO);
    let poses = world.part_poses(rod);
    assert_eq!(poses[0].center, Vector::new(0, -300_000));
    assert_eq!(poses[2].center, Vector::new(0, 300_000));
    assert!(poses.iter().all(|pose| pose.angle == 0));
}

#[test]
fn a_sliding_rod_pushes_the_ball_and_spin_respects_its_speed() {
    let (mut world, rod, ball) = rod_world();
    world.set_target(rod, Vector::new(0, 2 * M)).unwrap();
    world.set_angle_target(rod, -QUARTER).unwrap();
    let mut previous = world.body(rod).unwrap().angle();
    for _ in 0..30 {
        world.step();
        let body = world.body(rod).unwrap();
        assert!(body.velocity().length() <= 2 * M);
        assert!((body.angle() - previous).abs() <= 40_000_000 / 60);
        previous = body.angle();
    }
    // Shortest way round from +90° to -90° is ambiguous; it turned half a
    // turn in either case and wrapped into range.
    assert!(world.body(rod).unwrap().angle().abs() >= QUARTER - 1);
    assert!(world.body(ball).unwrap().velocity() != Vector::ZERO);
}

/// A pinball table section: playfield sloping toward -y, a left flipper
/// pivoting at the origin.
fn flipper_world(ball_x: i64) -> (World, BodyId, BodyId) {
    let mut world = World::new(WorldConfig {
        ticks_per_second: 120,
        max_substeps: 16,
        max_speed: 8 * M,
    })
    .unwrap();
    world.set_gravity(Vector::new(0, -1_500_000)).unwrap();
    // Rest pointing down-right at -30°, raised at +30°, pivot at its left
    // end: the capsule is offset half its length along the body's x axis.
    let rest = -PI_MICRORADIANS / 6;
    let flipper = world
        .add_shaped_body(ShapedBodyDef {
            angle: rest,
            angle_range: Some((rest, -rest)),
            ..ShapedBodyDef::kinematic(
                Vector::ZERO,
                1,
                30_000_000,
                vec![Part::at(capsule(35_000, 10_000), Vector::new(35_000, 0), 0)],
            )
        })
        .unwrap();
    let ball = world
        .add_body(BodyDef {
            restitution: 500_000,
            ..BodyDef::dynamic(13_500, 80, Vector::new(ball_x, 60_000))
        })
        .unwrap();
    (world, flipper, ball)
}

#[test]
fn a_flipper_launches_a_resting_ball_up_the_playfield() {
    let (mut world, flipper, ball) = flipper_world(50_000);
    // The ball falls onto the lowered flipper and rolls down it, coming to
    // rest... wedge it with a post at the flipper tip.
    world
        .add_wall(WallDef {
            start: Vector::new(75_000, -60_000),
            end: Vector::new(75_000, 0),
            restitution: 0,
            layers: u32::MAX,
        })
        .unwrap();
    for _ in 0..240 {
        world.step();
    }
    let resting = world.body(ball).unwrap();
    assert!(
        resting.velocity().length() < 20_000,
        "the ball settles on the flipper: {:?}",
        resting.velocity()
    );
    world.set_angle_target(flipper, PI_MICRORADIANS).unwrap();
    assert_eq!(
        world.body(flipper).unwrap().angle_target(),
        PI_MICRORADIANS / 6,
        "clamped into the range"
    );
    let mut launched = Vector::ZERO;
    for _ in 0..12 {
        world.step();
        launched = world.body(ball).unwrap().velocity();
    }
    // Up the playfield and toward the far side, at several metres per
    // second: faster than the flipper's surface at the contact.
    assert!(launched.y > M, "{launched:?}");
    assert!(launched.length() > 2 * M, "{launched:?}");
    let raised = world.body(flipper).unwrap();
    assert_eq!(raised.angle(), PI_MICRORADIANS / 6);
    assert_eq!(raised.angular_velocity(), 0);
}

#[test]
fn a_fast_ball_never_tunnels_through_a_swinging_flipper() {
    let mut random = Lcg(5);
    for round in 0..40 {
        let (mut world, flipper, ball) = flipper_world(40_000);
        let speed = 8 * M;
        let aim = Vector::new(random.next(M), -M).clamp_length(M);
        let aim = aim.scale(i128::from(speed), i128::from(aim.length().max(1)));
        world
            .set_position(ball, Vector::new(40_000, 150_000))
            .unwrap();
        world.set_velocity(ball, aim).unwrap();
        if round % 2 == 0 {
            world.set_angle_target(flipper, PI_MICRORADIANS).unwrap();
        }
        let above = |world: &World| {
            // Which side of the flipper's core line the ball's center is.
            let body = world.body(flipper).unwrap();
            let (sin, cos) = sin_cos(body.angle());
            let p = world.body(ball).unwrap().position();
            let along = i128::from(p.x) * i128::from(cos) + i128::from(p.y) * i128::from(sin);
            let across = i128::from(p.y) * i128::from(cos) - i128::from(p.x) * i128::from(sin);
            (along, across)
        };
        let mut crossed = false;
        for _ in 0..30 {
            let (along_before, across_before) = above(&world);
            world.step();
            let (along_after, across_after) = above(&world);
            let over = |along: i128| along >= 0 && along <= 70_000 * i128::from(TRIG_UNIT);
            if over(along_before) && over(along_after) && across_before > 0 && across_after < 0 {
                crossed = true;
            }
        }
        assert!(
            !crossed,
            "round {round}: the ball passed through the flipper"
        );
    }
}

/// A rink with `count` carts in a row facing +x.
fn rink(count: usize) -> (World, Vec<BodyId>) {
    let mut world = World::new(WorldConfig {
        ticks_per_second: 60,
        max_substeps: 12,
        max_speed: 6 * M,
    })
    .unwrap();
    arena(&mut world, 4 * M, 3 * M);
    let carts = (0..count)
        .map(|index| {
            let y = i64::try_from(index).unwrap() * 700_000 - 2 * M;
            world
                .add_shaped_body(ShapedBodyDef {
                    angular_damping: 0,
                    max_angular_speed: 6_000_000,
                    drive: Some(Drive {
                        acceleration: 3 * M,
                        turn_rate: 2_500_000,
                        turn_acceleration: 15_000_000,
                        grip: 8 * M,
                    }),
                    body: BodyDef {
                        restitution: 700_000,
                        friction: 800_000,
                        max_speed: 3 * M,
                        ..BodyDef::dynamic(1, 120, Vector::new(-2 * M, y))
                    },
                    ..ShapedBodyDef::dynamic(
                        120,
                        Vector::new(-2 * M, y),
                        vec![Part::new(capsule(150_000, 220_000))],
                    )
                })
                .unwrap()
        })
        .collect();
    (world, carts)
}

#[test]
fn carts_accelerate_steer_brake_and_hold_their_line() {
    let (mut world, carts) = rink(1);
    let cart = carts[0];
    world.set_drive(cart, 1_000_000, 0).unwrap();
    for _ in 0..30 {
        world.step();
    }
    let body = world.body(cart).unwrap();
    // Half a second at 3 m/s² less rolling friction: about 1.1 m/s.
    assert!(body.velocity().x > 1_000_000 && body.velocity().x < 1_300_000);
    assert_eq!(body.velocity().y, 0);
    assert_eq!(body.angle(), 0);
    // Steer left while driving: the heading turns and the velocity follows
    // it (grip), instead of sliding on straight like a puck.
    world.set_drive(cart, 1_000_000, 1_000_000).unwrap();
    for _ in 0..30 {
        world.step();
    }
    let body = world.body(cart).unwrap();
    assert!(body.angle() > 900_000, "{}", body.angle());
    let (sin, cos) = sin_cos(body.angle());
    let velocity = body.velocity();
    let sideways = (i128::from(velocity.y) * i128::from(cos)
        - i128::from(velocity.x) * i128::from(sin))
        / i128::from(TRIG_UNIT);
    // Grip corrects the sideways slip each tick; what remains is one tick
    // of turning (speed × turn rate / tick rate ≈ 90 mm/s), where a puck
    // would keep its whole original heading.
    assert!(sideways.abs() < 110_000, "{sideways} {velocity:?}");
    // Full reverse throttle brakes to a stop well within a second.
    world.set_drive(cart, -1_000_000, 0).unwrap();
    let mut stopped = None;
    for tick in 0..60 {
        world.step();
        let body = world.body(cart).unwrap();
        let (sin, cos) = sin_cos(body.angle());
        let forward = i128::from(body.velocity().x) * i128::from(cos)
            + i128::from(body.velocity().y) * i128::from(sin);
        if forward <= 0 {
            stopped.get_or_insert(tick);
        }
    }
    assert!(stopped.is_some_and(|tick| tick < 40), "{stopped:?}");
    // Released, the cart coasts to rest and stays exactly still.
    world.set_drive(cart, 0, 0).unwrap();
    for _ in 0..240 {
        world.step();
    }
    let rest = *world.body(cart).unwrap();
    assert_eq!(rest.velocity(), Vector::ZERO);
    assert_eq!(rest.angular_velocity(), 0);
    for _ in 0..120 {
        world.step();
    }
    assert_eq!(*world.body(cart).unwrap(), rest);
}

#[test]
fn carts_bump_each_other_and_the_boards_believably() {
    let (mut world, carts) = rink(2);
    let (striker, target) = (carts[0], carts[1]);
    // Line the target up across the striker's path, broadside.
    world.set_position(target, Vector::new(0, -2 * M)).unwrap();
    world.set_angle(target, QUARTER).unwrap();
    world.set_drive(striker, 1_000_000, 0).unwrap();
    let before = momenta(&world);
    let mut bumped = None;
    for tick in 0..90 {
        let report = world.step();
        if report.events.iter().any(|event| {
            matches!(event, Event::BodyContact { first, second, .. } if *first == striker && *second == target)
        }) {
            bumped.get_or_insert(tick);
            world.set_drive(striker, 0, 0).unwrap();
        }
    }
    assert!(bumped.is_some(), "the carts collided");
    let (s, t) = (world.body(striker).unwrap(), world.body(target).unwrap());
    // The struck cart is shoved along +x (sliding sideways against its
    // grip) while the striker slows; nothing crosses through.
    assert!(t.position().x > 100_000, "{:?}", t.position());
    assert!(s.position().x < t.position().x - 400_000);
    let after = momenta(&world);
    assert!(after.0 < before.0 + i128::from(120 * 3 * M));
    // Drive into the boards: the cart stops at the board without passing.
    world.set_drive(striker, 1_000_000, 0).unwrap();
    world
        .set_position(striker, Vector::new(3 * M, 2 * M))
        .unwrap();
    world.set_active(target, false).unwrap();
    for _ in 0..240 {
        world.step();
        let pose = world.part_poses(striker)[0];
        assert!(pose.center.x <= 4 * M - 220_000 + 1_000);
    }
    let pressed = world.body(striker).unwrap();
    assert!(pressed.position().x > 4 * M - 400_000);
    assert!(
        pressed.velocity().length() < M / 4,
        "{:?}",
        pressed.velocity()
    );
}

#[test]
fn idle_carts_in_contact_do_not_jitter() {
    let (mut world, carts) = rink(8);
    // Pack them touching in a column.
    for (index, cart) in carts.iter().enumerate() {
        let y = i64::try_from(index).unwrap() * 440_000 - 2 * M;
        world.set_position(*cart, Vector::new(0, y)).unwrap();
    }
    for _ in 0..240 {
        world.step();
    }
    let settled = world.clone();
    for _ in 0..240 {
        world.step();
    }
    for cart in &carts {
        assert_eq!(
            world.body(*cart).unwrap().position(),
            settled.body(*cart).unwrap().position()
        );
        assert_eq!(world.body(*cart).unwrap().velocity(), Vector::ZERO);
    }
}

#[test]
fn a_tilt_rolls_marbles_downhill_and_fields_add_regionally() {
    let mut world = World::new(config()).unwrap();
    arena(&mut world, 2 * M, M);
    let marble = |world: &mut World, y| {
        world
            .add_body(BodyDef {
                restitution: 300_000,
                layers: 1,
                mask: 1,
                ..BodyDef::dynamic(30_000, 5, Vector::new(0, y))
            })
            .unwrap()
    };
    let unboosted = marble(&mut world, -M / 2);
    let boosted = marble(&mut world, M / 2);
    world.set_gravity(Vector::new(M, 0)).unwrap();
    let field = world
        .add_field(FieldDef {
            region: Region::Rect(Aabb::new(Vector::new(-2 * M, 0), Vector::new(2 * M, M)).unwrap()),
            acceleration: Vector::new(M, 0),
            mask: 1,
        })
        .unwrap();
    for _ in 0..60 {
        world.step();
    }
    // One second at 1 m/s²: 1 m/s and half a metre (each tick adds the
    // tick's share, 16 667 µm/s, exactly).
    let unboosted_body = world.body(unboosted).unwrap();
    assert_eq!(unboosted_body.velocity(), Vector::new(60 * 16_667, 0));
    assert!((unboosted_body.position().x - M / 2).abs() < 10_000);
    assert_eq!(
        world.body(boosted).unwrap().velocity(),
        Vector::new(60 * 33_333, 0)
    );
    // Tilt the other way, per tick, and switch the booster off: the
    // marbles turn around and come to rest against the left board.
    world.set_field_acceleration(field, Vector::ZERO).unwrap();
    for tick in 0..600 {
        let tilt = if tick < 300 { -2 * M } else { -M };
        world.set_gravity(Vector::new(tilt, 0)).unwrap();
        world.step();
    }
    for id in [unboosted, boosted] {
        let body = world.body(id).unwrap();
        assert_eq!(body.position().x, -2 * M + 30_000, "{:?}", body.position());
        assert_eq!(body.velocity(), Vector::ZERO);
    }
}

fn bumper_world() -> (World, BodyId, BodyId) {
    let mut world = World::new(config()).unwrap();
    let bumper = world
        .add_shaped_body(ShapedBodyDef::kinematic(
            Vector::ZERO,
            1,
            0,
            vec![Part {
                kick: 2 * M,
                ..Part::new(Shape::Disc { radius: 100_000 })
            }],
        ))
        .unwrap();
    let ball = world
        .add_body(BodyDef {
            restitution: 500_000,
            ..ball(Vector::new(-500_000, 0), Vector::new(M, 0))
        })
        .unwrap();
    (world, bumper, ball)
}

#[test]
fn bumpers_kick_once_per_contact_and_report_it() {
    let (mut world, bumper, ball) = bumper_world();
    let mut kicks = Vec::new();
    for tick in 0..60 {
        for event in world.step().events {
            if let Event::Bumped {
                bumper: id,
                body,
                normal,
                speed,
                part,
            } = event
            {
                kicks.push((tick, id, body, normal, speed, part));
            }
        }
    }
    assert_eq!(kicks.len(), 1, "{kicks:?}");
    let (_, id, body, normal, speed, part) = kicks[0];
    assert_eq!((id, body, part, speed), (bumper, ball, 0, 2 * M));
    assert_eq!(normal, Vector::new(-TRIG_UNIT, 0));
    // Bounce (half of 1 m/s) plus the 2 m/s kick.
    let velocity = world.body(ball).unwrap().velocity();
    assert!((velocity.x + 2_500_000).abs() < 5_000, "{velocity:?}");
    // Sent back in, it fires again: once per contact.
    world.set_position(ball, Vector::new(-500_000, 0)).unwrap();
    world.set_velocity(ball, Vector::new(M, 0)).unwrap();
    let again = (0..60)
        .flat_map(|_| world.step().events)
        .filter(|event| matches!(event, Event::Bumped { .. }))
        .count();
    assert_eq!(again, 1);
}

#[test]
fn a_ball_pressed_against_a_bumper_fires_it_only_once() {
    let (mut world, _, ball) = bumper_world();
    // A field pushes the ball into the bumper continuously; the kick throws
    // it out, the field brings it back: each return is a new contact, but
    // while touching it never refires.
    world.set_gravity(Vector::new(20 * M, 0)).unwrap();
    let mut fired_ticks = Vec::new();
    for tick in 0..120 {
        let count = world
            .step()
            .events
            .iter()
            .filter(|event| matches!(event, Event::Bumped { .. }))
            .count();
        assert!(count <= 1);
        if count == 1 {
            fired_ticks.push(tick);
        }
    }
    assert!(fired_ticks.len() >= 2);
    assert!(fired_ticks.windows(2).all(|pair| pair[1] - pair[0] > 3));
    assert!(world.body(ball).unwrap().position().x < -100_000);
}

/// A mixed table exercising every new feature at once.
#[allow(clippy::too_many_lines)]
fn carnival() -> (World, Vec<BodyId>) {
    let mut world = World::new(WorldConfig {
        ticks_per_second: 60,
        max_substeps: 16,
        max_speed: 8 * M,
    })
    .unwrap();
    arena(&mut world, 2 * M, M);
    for wall in 0..4 {
        world
            .set_wall_friction(crate::WallId(wall), 300_000)
            .unwrap();
    }
    world.set_gravity(Vector::new(0, -400_000)).unwrap();
    world
        .add_field(FieldDef {
            region: Region::Circle {
                center: Vector::new(M, 0),
                radius: 300_000,
            },
            acceleration: Vector::new(-M, M),
            mask: u32::MAX,
        })
        .unwrap();
    let mut ids = Vec::new();
    ids.push(
        world
            .add_shaped_body(ShapedBodyDef {
                angle_range: Some((-QUARTER, QUARTER)),
                ..ShapedBodyDef::kinematic(
                    Vector::new(-M, -M / 2),
                    2 * M,
                    20_000_000,
                    vec![
                        Part::at(capsule(150_000, 30_000), Vector::new(150_000, 0), 0),
                        Part {
                            kick: M,
                            ..Part::at(Shape::Disc { radius: 40_000 }, Vector::new(-60_000, 0), 0)
                        },
                    ],
                )
            })
            .unwrap(),
    );
    ids.push(
        world
            .add_shaped_body(ShapedBodyDef::kinematic(
                Vector::new(M, -M / 2),
                3 * M,
                30_000_000,
                vec![
                    Part {
                        attachment: Attachment::Spindle,
                        ..Part::at(rect(90_000, 30_000), Vector::new(0, -200_000), 0)
                    },
                    Part {
                        attachment: Attachment::Spindle,
                        ..Part::at(rect(90_000, 30_000), Vector::new(0, 200_000), 0)
                    },
                ],
            ))
            .unwrap(),
    );
    for index in 0..3_i64 {
        let position = Vector::new(-M + index * 600_000, M / 2);
        ids.push(
            world
                .add_shaped_body(ShapedBodyDef {
                    surface_friction: 400_000,
                    angular_damping: 100_000,
                    drive: Some(Drive {
                        acceleration: 2 * M,
                        turn_rate: 3_000_000,
                        turn_acceleration: 10_000_000,
                        grip: 5 * M,
                    }),
                    body: BodyDef {
                        restitution: 600_000,
                        friction: 300_000,
                        ..BodyDef::dynamic(1, 100, position)
                    },
                    ..ShapedBodyDef::dynamic(
                        100,
                        position,
                        vec![Part::new(capsule(80_000, 60_000))],
                    )
                })
                .unwrap(),
        );
    }
    for index in 0..4_i64 {
        let position = Vector::new(-M / 2 + index * 300_000, 0);
        ids.push(
            world
                .add_shaped_body(ShapedBodyDef {
                    surface_friction: 500_000,
                    body: BodyDef {
                        velocity: Vector::new(M, index * 300_000),
                        restitution: 800_000,
                        damping: 20_000,
                        ..BodyDef::dynamic(1, 10, position)
                    },
                    ..ShapedBodyDef::dynamic(
                        10,
                        position,
                        vec![Part::new(if index % 2 == 0 {
                            Shape::Disc { radius: 35_000 }
                        } else {
                            rect(45_000, 35_000)
                        })],
                    )
                })
                .unwrap(),
        );
    }
    ids.push(
        world
            .add_body(BodyDef {
                velocity: Vector::new(-2 * M, M),
                restitution: 900_000,
                ..BodyDef::dynamic(30_000, 8, Vector::new(0, -M / 2))
            })
            .unwrap(),
    );
    (world, ids)
}

fn run_carnival(world: &mut World, ids: &[BodyId], seed: u64, ticks: u32) -> Vec<Event> {
    let mut random = Lcg(seed);
    let mut events = Vec::new();
    for _ in 0..ticks {
        world
            .set_angle_target(ids[0], random.next(QUARTER))
            .unwrap();
        world
            .set_target(
                ids[1],
                Vector::new(M + random.next(M / 4), random.next(M / 2)),
            )
            .unwrap();
        world
            .set_angle_target(ids[1], random.next(PI_MICRORADIANS))
            .unwrap();
        for cart in &ids[2..5] {
            let throttle = i32::try_from(random.next(1_000_000)).unwrap();
            let steer = i32::try_from(random.next(1_000_000)).unwrap();
            world.set_drive(*cart, throttle, steer).unwrap();
        }
        world
            .set_gravity(Vector::new(random.next(300_000), -400_000))
            .unwrap();
        events.extend(world.step().events);
    }
    events
}

#[test]
fn mixed_long_runs_are_deterministic() {
    let (mut first, ids) = carnival();
    let (mut second, _) = carnival();
    let first_events = run_carnival(&mut first, &ids, 21, 6_000);
    let second_events = run_carnival(&mut second, &ids, 21, 6_000);
    assert_eq!(first_events, second_events);
    for kind in [
        |event: &Event| matches!(event, Event::PartContact { .. }),
        |event: &Event| matches!(event, Event::PartWallContact { .. }),
        |event: &Event| matches!(event, Event::Bumped { .. }),
        |event: &Event| matches!(event, Event::BodyContact { .. }),
    ] {
        assert!(first_events.iter().any(kind));
    }
    assert_eq!(first, second);
    assert_eq!(first.checksum(), second.checksum());
    // Every body stayed on the table.
    for (_, body) in first.bodies() {
        assert!(body.position().x.abs() <= 2 * M && body.position().y.abs() <= M);
    }
    // Pinned so a platform or solver change cannot slip by unnoticed.
    assert_eq!(first.checksum(), GOLDEN_CARNIVAL_CHECKSUM);
}

const GOLDEN_CARNIVAL_CHECKSUM: u64 = 0x0ee9_6cac_9c15_0bcb;

#[test]
fn extended_encoding_round_trips_and_continues_identically() {
    let (mut world, ids) = carnival();
    run_carnival(&mut world, &ids, 3, 400);
    let bytes = world.to_bytes();
    assert_eq!(bytes[..2], WORLD_STATE_VERSION.to_le_bytes());
    let mut restored = World::from_bytes(&bytes).unwrap();
    assert_eq!(restored, world);
    assert_eq!(restored.to_bytes(), bytes);
    let original = run_carnival(&mut world, &ids, 4, 400);
    let continued = run_carnival(&mut restored, &ids, 4, 400);
    assert_eq!(original, continued);
    assert_eq!(restored, world);

    let mut future = bytes.clone();
    future[..2].copy_from_slice(&3_u16.to_le_bytes());
    assert_eq!(
        World::from_bytes(&future),
        Err(DecodeError::UnsupportedVersion(3))
    );
    for cut in [bytes.len() - 1, bytes.len() / 2, 40] {
        assert_eq!(
            World::from_bytes(&bytes[..cut]),
            Err(DecodeError::Truncated)
        );
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        World::from_bytes(&trailing),
        Err(DecodeError::TrailingBytes)
    );
    // Flipping every single byte never panics and never yields a world that
    // encodes differently from what was decoded.
    for index in 0..bytes.len() {
        let mut corrupt = bytes.clone();
        corrupt[index] ^= 0x5a;
        if let Ok(decoded) = World::from_bytes(&corrupt) {
            let again = World::from_bytes(&decoded.to_bytes()).unwrap();
            assert_eq!(again, decoded);
        }
    }
}

#[test]
fn malformed_extended_payloads_are_rejected() {
    let mut world = World::new(config()).unwrap();
    let id = world
        .add_shaped_body(ShapedBodyDef::kinematic(
            Vector::ZERO,
            M,
            0,
            vec![Part {
                kick: M,
                ..Part::new(capsule(50_000, 30_000))
            }],
        ))
        .unwrap();
    let bytes = world.to_bytes();
    assert_eq!(bytes[..2], WORLD_STATE_VERSION.to_le_bytes());
    // Layout: version, config (11), tick (8), gravity (16), walls (2),
    // bodies (2), the plain record (99 bytes), then the shape flag.
    let flag = 2 + 11 + 8 + 16 + 2 + 2 + 99;
    assert_eq!(bytes[flag], 1);
    let mut unknown_flag = bytes.clone();
    unknown_flag[flag] = 7;
    assert_eq!(
        World::from_bytes(&unknown_flag),
        Err(DecodeError::UnknownTag)
    );
    // The part's shape tag follows the rotation block (8·4 + 4 + 4 + 1 +
    // 8 + 1 + 4 + 4) and the part count.
    let shape = flag + 1 + 58 + 1;
    assert_eq!(bytes[shape], 2);
    let mut unknown_shape = bytes.clone();
    unknown_shape[shape] = 9;
    assert_eq!(
        World::from_bytes(&unknown_shape),
        Err(DecodeError::UnknownTag)
    );
    // A zero-radius capsule fails validation.
    let mut thin = bytes.clone();
    thin[shape + 9..shape + 17].copy_from_slice(&0_i64.to_le_bytes());
    assert_eq!(
        World::from_bytes(&thin),
        Err(DecodeError::Invalid(WorldError::InvalidParameter))
    );
    // A kicking contact naming a missing body is refused.
    let mut touching = bytes;
    let count = touching.len() - 4;
    touching[count..].copy_from_slice(&1_u32.to_le_bytes());
    touching.extend(id.index().to_le_bytes());
    touching.push(0);
    touching.extend(9_u16.to_le_bytes());
    assert_eq!(
        World::from_bytes(&touching),
        Err(DecodeError::Invalid(WorldError::UnknownBody))
    );
}

#[test]
fn disc_only_worlds_keep_the_original_encoding() {
    let mut world = World::new(config()).unwrap();
    arena(&mut world, M, M);
    world
        .add_body(ball(Vector::ZERO, Vector::new(M, 0)))
        .unwrap();
    let bytes = world.to_bytes();
    assert_eq!(bytes[..2], DISC_WORLD_STATE_VERSION.to_le_bytes());
    assert_eq!(World::from_bytes(&bytes).unwrap(), world);
    // Any extension switches to version 2, and back when removed.
    world.set_gravity(Vector::new(0, -1)).unwrap();
    assert_eq!(world.to_bytes()[..2], WORLD_STATE_VERSION.to_le_bytes());
    assert_eq!(World::from_bytes(&world.to_bytes()).unwrap(), world);
    world.set_gravity(Vector::ZERO).unwrap();
    assert_eq!(world.to_bytes(), bytes);
}
