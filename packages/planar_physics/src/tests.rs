use crate::{
    Aabb, BodyDef, BodyId, BodyKind, DecodeError, Event, MICROS_PER_UNIT, MILLIONTHS, Region,
    SensorDef, Vector, WORLD_STATE_VERSION, WallDef, World, WorldConfig, WorldError,
};

const M: i64 = MICROS_PER_UNIT;

fn config() -> WorldConfig {
    WorldConfig {
        ticks_per_second: 60,
        max_substeps: 16,
        max_speed: 12 * M,
    }
}

fn arena() -> Aabb {
    Aabb::new(Vector::new(-M, -M / 2), Vector::new(M, M / 2)).unwrap()
}

fn world_with_bounds() -> World {
    let mut world = World::new(config()).unwrap();
    world.add_bounds(arena(), MILLIONTHS, u32::MAX).unwrap();
    world
}

fn puck(position: Vector, velocity: Vector) -> BodyDef {
    BodyDef {
        velocity,
        ..BodyDef::dynamic(M / 25, 15, position)
    }
}

fn momentum(world: &World) -> (i128, i128) {
    world
        .bodies()
        .filter(|(_, body)| body.kind() == BodyKind::Dynamic)
        .fold((0, 0), |(x, y), (_, body)| {
            let mass = i128::from(body.def().mass);
            (
                x + mass * i128::from(body.velocity().x),
                y + mass * i128::from(body.velocity().y),
            )
        })
}

fn energy(world: &World) -> i128 {
    world
        .bodies()
        .filter(|(_, body)| body.kind() == BodyKind::Dynamic)
        .map(|(_, body)| i128::from(body.def().mass) * body.velocity().length_squared())
        .sum()
}

/// A small deterministic generator for test inputs.
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
fn configuration_and_parameters_are_validated() {
    assert_eq!(
        World::new(WorldConfig {
            ticks_per_second: 0,
            ..config()
        }),
        Err(WorldError::InvalidConfig)
    );
    assert_eq!(
        World::new(WorldConfig {
            max_speed: 0,
            ..config()
        }),
        Err(WorldError::InvalidConfig)
    );
    // 12 m/s at 60 Hz with 16 substeps: 12.5 mm per substep, so a disc
    // needs at least a 25 mm radius.
    assert_eq!(config().min_radius(), 25_000);
    let mut world = World::new(config()).unwrap();
    assert_eq!(
        world.add_body(BodyDef::dynamic(20_000, 1, Vector::ZERO)),
        Err(WorldError::BodyTooSmall {
            radius: 20_000,
            minimum: 25_000
        })
    );
    assert_eq!(
        world.add_body(BodyDef::dynamic(M / 25, 0, Vector::ZERO)),
        Err(WorldError::InvalidParameter)
    );
    assert_eq!(
        world.add_body(BodyDef {
            restitution: MILLIONTHS + 1,
            ..BodyDef::dynamic(M / 25, 1, Vector::ZERO)
        }),
        Err(WorldError::InvalidParameter)
    );
    assert_eq!(
        world.add_body(BodyDef::dynamic(M / 25, 1, Vector::new(i64::MAX, 0))),
        Err(WorldError::OutOfRange)
    );
    assert_eq!(
        world.add_wall(WallDef {
            start: Vector::ZERO,
            end: Vector::ZERO,
            restitution: 0,
            layers: 1
        }),
        Err(WorldError::DegenerateWall)
    );
    assert_eq!(
        Aabb::new(Vector::new(1, 0), Vector::ZERO),
        Err(WorldError::InvalidRegion)
    );
    let disc = world
        .add_body(BodyDef::dynamic(M / 25, 1, Vector::ZERO))
        .unwrap();
    assert_eq!(
        world.set_target(disc, Vector::ZERO),
        Err(WorldError::NotKinematic)
    );
    assert_eq!(
        world.set_target(BodyId(9), Vector::ZERO),
        Err(WorldError::UnknownBody)
    );
    // Speed caps never exceed the world's.
    world.set_velocity(disc, Vector::new(100 * M, 0)).unwrap();
    assert_eq!(world.body(disc).unwrap().velocity(), Vector::new(12 * M, 0));
}

#[test]
fn equal_discs_exchange_velocity_and_conserve_momentum() {
    let mut world = World::new(config()).unwrap();
    world
        .add_body(puck(Vector::new(-M / 2, 0), Vector::new(2 * M, 0)))
        .unwrap();
    world
        .add_body(puck(Vector::new(M / 2, 0), Vector::ZERO))
        .unwrap();
    let before = (momentum(&world), energy(&world));
    let mut contacts = 0;
    for _ in 0..60 {
        let report = world.step();
        contacts += report
            .events
            .iter()
            .filter(|event| matches!(event, Event::BodyContact { .. }))
            .count();
    }
    assert_eq!(contacts, 1);
    let (first, second) = (world.bodies[0].velocity(), world.bodies[1].velocity());
    assert!(first.x.abs() < 10, "striker stops: {first:?}");
    assert!(
        (second.x - 2 * M).abs() < 10,
        "target takes over: {second:?}"
    );
    let after = (momentum(&world), energy(&world));
    assert!((after.0.0 - before.0.0).abs() <= 15 * 10);
    assert_eq!(after.0.1, 0);
    assert!(after.1 <= before.1);
}

#[test]
fn oblique_collisions_of_unequal_masses_roughly_conserve_momentum() {
    let mut world = World::new(config()).unwrap();
    world
        .add_body(BodyDef {
            mass: 40,
            ..puck(Vector::new(-M / 2, 10_000), Vector::new(3 * M, 0))
        })
        .unwrap();
    world
        .add_body(BodyDef {
            mass: 10,
            ..puck(Vector::new(0, -20_000), Vector::new(0, M / 2))
        })
        .unwrap();
    let before = (momentum(&world), energy(&world));
    for _ in 0..30 {
        world.step();
    }
    let after = (momentum(&world), energy(&world));
    let tolerance = 50 * 100;
    assert!(
        (after.0.0 - before.0.0).abs() < tolerance,
        "{before:?} {after:?}"
    );
    assert!(
        (after.0.1 - before.0.1).abs() < tolerance,
        "{before:?} {after:?}"
    );
    assert!(
        after.1 <= before.1 + before.1 / 1_000_000,
        "{before:?} {after:?}"
    );
    // Both moved off their original lines.
    assert!(world.bodies[1].velocity().x > 0);
}

#[test]
fn discs_at_maximum_speed_never_tunnel_through_walls() {
    let mut random = Lcg(7);
    for _ in 0..200 {
        let mut world = world_with_bounds();
        let velocity = Vector::new(random.next(12 * M), random.next(12 * M)).clamp_length(12 * M);
        let velocity = if velocity.length() < 11 * M {
            velocity.scale(i128::from(12 * M), i128::from(velocity.length().max(1)))
        } else {
            velocity
        };
        let id = world
            .add_body(puck(
                Vector::new(random.next(M / 2), random.next(M / 4)),
                velocity,
            ))
            .unwrap();
        // A thin interior divider too.
        world
            .add_wall(WallDef {
                start: Vector::new(0, -M / 2),
                end: Vector::new(0, M / 2),
                restitution: MILLIONTHS,
                layers: u32::MAX,
            })
            .unwrap();
        let side = world.body(id).unwrap().position().x.signum();
        for _ in 0..600 {
            let report = world.step();
            assert_eq!(report.substeps, 10);
            let body = world.body(id).unwrap();
            let position = body.position();
            assert!(
                position.x.abs() <= M - body.radius() && position.y.abs() <= M / 2 - body.radius(),
                "escaped the arena at {position:?}"
            );
            assert_eq!(position.x.signum(), side, "crossed the divider");
        }
    }
}

#[test]
fn kinematic_paddle_transfers_momentum_and_respects_limits() {
    let mut world = world_with_bounds();
    let half = Aabb::new(Vector::new(-M, -M / 2), Vector::new(0, M / 2)).unwrap();
    let paddle = world
        .add_body(BodyDef {
            confine: Some(half),
            ..BodyDef::kinematic(M / 20, Vector::new(-M / 2, 0), 6 * M)
        })
        .unwrap();
    let disc = world
        .add_body(puck(Vector::new(-M / 4, 0), Vector::ZERO))
        .unwrap();
    // Aim beyond the confinement: the paddle stops at the center line.
    world.set_target(paddle, Vector::new(M, 0)).unwrap();
    let mut hit = None;
    for _ in 0..30 {
        let report = world.step();
        let moved = world.body(paddle).unwrap().velocity();
        assert!(moved.length() <= 6 * M);
        if let Some(event) = report
            .events
            .iter()
            .find(|event| matches!(event, Event::BodyContact { .. }))
        {
            hit.get_or_insert(*event);
        }
    }
    assert_eq!(world.body(paddle).unwrap().position(), Vector::new(0, 0));
    let Some(Event::BodyContact {
        first,
        second,
        speed,
    }) = hit
    else {
        panic!("the paddle never struck the disc");
    };
    assert_eq!((first, second), (paddle, disc));
    assert!(speed > 5 * M);
    // A heavy-handed strike sends the disc off faster than the paddle.
    let disc_velocity = world.body(disc).unwrap().velocity();
    assert!(disc_velocity.x.abs() > 6 * M, "{disc_velocity:?}");
}

#[test]
fn a_paddle_cannot_squeeze_a_disc_through_a_wall() {
    let mut world = world_with_bounds();
    let paddle = world
        .add_body(BodyDef::kinematic(M / 20, Vector::new(M / 2, 0), 12 * M))
        .unwrap();
    let disc = world
        .add_body(puck(Vector::new(M - M / 10, 0), Vector::ZERO))
        .unwrap();
    for tick in 0..240 {
        let wobble = if tick % 2 == 0 { M / 50 } else { -M / 50 };
        world
            .set_target(paddle, Vector::new(2 * M, wobble))
            .unwrap();
        world.step();
        let position = world.body(disc).unwrap().position();
        assert!(position.x <= M - M / 25, "squeezed out at {position:?}");
        assert!(position.y.abs() <= M / 2 - M / 25);
    }
}

#[test]
fn sensors_report_entry_and_exit_once() {
    let mut world = world_with_bounds();
    let goal = world
        .add_sensor(SensorDef {
            region: Region::Rect(
                Aabb::new(Vector::new(M / 4, -M / 8), Vector::new(M / 2, M / 8)).unwrap(),
            ),
            mask: 1,
        })
        .unwrap();
    let circle = world
        .add_sensor(SensorDef {
            region: Region::Circle {
                center: Vector::new(-M / 2, 0),
                radius: M / 10,
            },
            mask: 1,
        })
        .unwrap();
    let moving = world
        .add_body(BodyDef {
            layers: 1,
            ..puck(Vector::ZERO, Vector::new(M, 0))
        })
        .unwrap();
    let resting = world
        .add_body(BodyDef {
            layers: 1,
            ..puck(Vector::new(-M / 2, M / 20), Vector::ZERO)
        })
        .unwrap();
    let ignored = world
        .add_body(BodyDef {
            layers: 2,
            ..puck(Vector::new(M / 3, M / 3), Vector::ZERO)
        })
        .unwrap();
    let mut events = Vec::new();
    for _ in 0..42 {
        events.extend(
            world.step().events.into_iter().filter(|event| {
                matches!(event, Event::SensorEnter { .. } | Event::SensorExit { .. })
            }),
        );
    }
    assert_eq!(
        events,
        vec![
            Event::SensorEnter {
                sensor: circle,
                body: resting
            },
            Event::SensorEnter {
                sensor: goal,
                body: moving
            },
            Event::SensorExit {
                sensor: goal,
                body: moving
            },
        ]
    );
    assert!(world.is_sensing(circle, resting));
    assert!(!world.is_sensing(goal, ignored));
    world.set_active(resting, false).unwrap();
    let report = world.step();
    assert!(report.events.contains(&Event::SensorExit {
        sensor: circle,
        body: resting
    }));
}

#[test]
fn friction_and_damping_bring_discs_to_rest() {
    let mut world = World::new(config()).unwrap();
    let rolling = world
        .add_body(BodyDef {
            friction: M,
            ..puck(Vector::new(-M / 2, 0), Vector::new(M, 0))
        })
        .unwrap();
    let damped = world
        .add_body(BodyDef {
            damping: MILLIONTHS / 2,
            ..puck(Vector::new(-M / 2, M), Vector::new(M, 0))
        })
        .unwrap();
    for _ in 0..60 {
        world.step();
    }
    // One metre per second against one metre per second squared: stopped
    // after a second, half a metre later.
    assert_eq!(world.body(rolling).unwrap().velocity(), Vector::ZERO);
    let rest = world.body(rolling).unwrap().position().x;
    assert!(rest.abs() < M / 50, "{rest}");
    let speed = world.body(damped).unwrap().velocity().x;
    assert!(speed < 650_000 && speed > 550_000, "{speed}");
}

fn scrimmage() -> (World, [BodyId; 3]) {
    let mut world = world_with_bounds();
    let left = world
        .add_body(BodyDef::kinematic(M / 20, Vector::new(-M / 2, 0), 8 * M))
        .unwrap();
    let right = world
        .add_body(BodyDef::kinematic(M / 20, Vector::new(M / 2, 0), 8 * M))
        .unwrap();
    let disc = world
        .add_body(BodyDef {
            restitution: 900_000,
            damping: 50_000,
            ..puck(Vector::ZERO, Vector::new(M, M / 3))
        })
        .unwrap();
    world
        .add_sensor(SensorDef {
            region: Region::Circle {
                center: Vector::ZERO,
                radius: M / 5,
            },
            mask: u32::MAX,
        })
        .unwrap();
    (world, [left, right, disc])
}

fn run_scrimmage(world: &mut World, ids: [BodyId; 3], seed: u64, ticks: u32) -> Vec<Event> {
    let mut random = Lcg(seed);
    let mut events = Vec::new();
    for _ in 0..ticks {
        world
            .set_target(ids[0], Vector::new(random.next(M), random.next(M / 2)))
            .unwrap();
        world
            .set_target(ids[1], Vector::new(random.next(M), random.next(M / 2)))
            .unwrap();
        events.extend(world.step().events);
    }
    events
}

#[test]
fn long_runs_are_deterministic() {
    let (mut first, ids) = scrimmage();
    let (mut second, _) = scrimmage();
    let first_events = run_scrimmage(&mut first, ids, 11, 10_000);
    let second_events = run_scrimmage(&mut second, ids, 11, 10_000);
    assert_eq!(first_events, second_events);
    assert!(first_events.len() > 100);
    assert_eq!(first.checksum(), second.checksum());
    assert_eq!(first, second);
    assert_eq!(first.tick(), 10_000);
    // Pinned so a platform or solver change cannot slip by unnoticed.
    assert_eq!(first.checksum(), GOLDEN_SCRIMMAGE_CHECKSUM);
}

const GOLDEN_SCRIMMAGE_CHECKSUM: u64 = 0xe7fa_0e8a_e363_803b;

#[test]
fn encoding_round_trips_and_continues_identically() {
    let (mut world, ids) = scrimmage();
    run_scrimmage(&mut world, ids, 3, 500);
    let bytes = world.to_bytes();
    assert_eq!(bytes[..2], WORLD_STATE_VERSION.to_le_bytes());
    let mut restored = World::from_bytes(&bytes).unwrap();
    assert_eq!(restored, world);
    assert_eq!(restored.checksum(), world.checksum());
    let original = run_scrimmage(&mut world, ids, 4, 500);
    let continued = run_scrimmage(&mut restored, ids, 4, 500);
    assert_eq!(original, continued);
    assert_eq!(restored, world);

    let mut wrong_version = bytes.clone();
    wrong_version[0] = 9;
    assert_eq!(
        World::from_bytes(&wrong_version),
        Err(DecodeError::UnsupportedVersion(9))
    );
    assert_eq!(
        World::from_bytes(&bytes[..bytes.len() - 1]),
        Err(DecodeError::Truncated)
    );
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        World::from_bytes(&trailing),
        Err(DecodeError::TrailingBytes)
    );
    let mut zero_rate = bytes;
    zero_rate[2] = 0;
    zero_rate[3] = 0;
    assert_eq!(
        World::from_bytes(&zero_rate),
        Err(DecodeError::Invalid(WorldError::InvalidConfig))
    );
}

#[test]
fn contacts_merge_within_a_tick_in_canonical_order() {
    let mut world = world_with_bounds();
    let paddle = world
        .add_body(BodyDef::kinematic(M / 20, Vector::new(M - M / 5, 0), M))
        .unwrap();
    let disc = world
        .add_body(puck(Vector::new(M - M / 10, 0), Vector::new(4 * M, 0)))
        .unwrap();
    world.set_target(paddle, Vector::new(M, 0)).unwrap();
    let mut seen = Vec::new();
    for _ in 0..20 {
        let report = world.step();
        let mut pairs: Vec<_> = report
            .events
            .iter()
            .filter_map(|event| match event {
                Event::BodyContact { first, second, .. } => Some((0, first.0, second.0)),
                Event::WallContact { body, wall, .. } => Some((1, body.0, wall.0)),
                _ => None,
            })
            .collect();
        let total = pairs.len();
        pairs.dedup();
        assert_eq!(pairs.len(), total, "contacts are merged per pair");
        seen.extend(pairs);
    }
    assert!(seen.contains(&(0, paddle.0, disc.0)));
    assert!(seen.iter().any(|pair| pair.0 == 1));
}
