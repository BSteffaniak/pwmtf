//! A two-player cooperative pinball test game on planar physics shaped
//! bodies, synchronized by the tick mode of `pwmtf_sync` through a simulated
//! impaired network.
//!
//! Not a product game: it proves that a sloped playfield (world
//! acceleration), kinematic flippers pivoting within an angle range, kicking
//! bumpers and slingshots keep rollback synchronization converging under
//! latency, jitter, loss, reordering and duplication.

mod support;

use pwmtf_planar_physics::{
    Aabb, BodyDef, BodyId, Event, MICROS_PER_UNIT, PI_MICRORADIANS, Part, Region, SensorDef,
    SensorId, Shape, ShapedBodyDef, Vector, WallDef, World, WorldConfig,
};
use pwmtf_sync::TickGame;
use support::{HARSH, HarnessGame, MODERATE, assert_healthy, assert_replays_identically, run};

const M: i64 = MICROS_PER_UNIT;
const TPS: u32 = 60;
const HALF_WIDTH: i64 = 250_000;
const HALF_HEIGHT: i64 = 500_000;
const BALL_RADIUS: i64 = 13_500;
const FLIPPER_Y: i64 = -380_000;
const FLIPPER_X: i64 = 120_000;
const FLIPPER_HALF: i64 = 40_000;
/// Thirty degrees.
const SWING: i64 = PI_MICRORADIANS / 6;
const GAME_VERSION: u8 = 1;

/// A flipper button: held (1) or released (0).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Button(u8);

#[derive(Clone, Debug, Eq, PartialEq)]
struct Pinball {
    world: World,
    flippers: [BodyId; 2],
    ball: BodyId,
    drain: SensorId,
    bumps: u32,
    flips: u32,
    drains: u16,
}

impl Pinball {
    #[allow(clippy::too_many_lines)]
    fn new() -> Self {
        let mut world = World::new(WorldConfig {
            ticks_per_second: u16::try_from(TPS).unwrap(),
            max_substeps: 16,
            max_speed: 6 * M,
        })
        .unwrap();
        world.set_gravity(Vector::new(0, -1_200_000)).unwrap();
        world
            .add_bounds(
                Aabb::new(
                    Vector::new(-HALF_WIDTH, -HALF_HEIGHT),
                    Vector::new(HALF_WIDTH, HALF_HEIGHT),
                )
                .unwrap(),
                600_000,
                u32::MAX,
            )
            .unwrap();
        // Inlane guides ending above each flipper, inside its pivot, so the
        // ball rolls on down the flipper.
        for side in [-1_i64, 1] {
            world
                .add_wall(WallDef {
                    start: Vector::new(side * HALF_WIDTH, -270_000),
                    end: Vector::new(side * (FLIPPER_X - 15_000), FLIPPER_Y + 40_000),
                    restitution: 400_000,
                    layers: u32::MAX,
                })
                .unwrap();
        }
        // Left flipper extends right of its pivot and rests pointing down;
        // the right flipper mirrors it.
        let flippers = [-1_i64, 1].map(|side| {
            world
                .add_shaped_body(ShapedBodyDef {
                    angle: -side * -SWING,
                    angle_range: Some((-SWING, SWING)),
                    body: BodyDef {
                        restitution: 300_000,
                        ..BodyDef::kinematic(1, Vector::new(side * FLIPPER_X, FLIPPER_Y), 1)
                    },
                    ..ShapedBodyDef::kinematic(
                        Vector::new(side * FLIPPER_X, FLIPPER_Y),
                        1,
                        28_000_000,
                        vec![Part::at(
                            Shape::Capsule {
                                half_length: FLIPPER_HALF,
                                radius: 13_000,
                            },
                            Vector::new(-side * FLIPPER_HALF, 0),
                            0,
                        )],
                    )
                })
                .unwrap()
        });
        // Pop bumpers kick on every touch.
        for center in [
            Vector::new(-100_000, 200_000),
            Vector::new(100_000, 200_000),
            Vector::new(0, 320_000),
        ] {
            world
                .add_shaped_body(ShapedBodyDef::kinematic(
                    center,
                    1,
                    0,
                    vec![Part {
                        kick: 1_500_000,
                        ..Part::new(Shape::Disc { radius: 30_000 })
                    }],
                ))
                .unwrap();
        }
        // Slingshots above the inlanes: kicking capsules.
        for side in [-1_i64, 1] {
            world
                .add_shaped_body(ShapedBodyDef {
                    angle: side * -1_100_000,
                    ..ShapedBodyDef::kinematic(
                        Vector::new(side * 190_000, -170_000),
                        1,
                        0,
                        vec![Part {
                            kick: 1_000_000,
                            ..Part::new(Shape::Capsule {
                                half_length: 50_000,
                                radius: 14_000,
                            })
                        }],
                    )
                })
                .unwrap();
        }
        let ball = world
            .add_body(BodyDef {
                restitution: 600_000,
                damping: 20_000,
                ..BodyDef::dynamic(BALL_RADIUS, 80, Vector::ZERO)
            })
            .unwrap();
        let drain = world
            .add_sensor(SensorDef {
                region: Region::Rect(
                    Aabb::new(
                        Vector::new(-HALF_WIDTH, -HALF_HEIGHT),
                        Vector::new(HALF_WIDTH, -460_000),
                    )
                    .unwrap(),
                ),
                mask: u32::MAX,
            })
            .unwrap();
        let mut game = Self {
            world,
            flippers,
            ball,
            drain,
            bumps: 0,
            flips: 0,
            drains: 0,
        };
        game.serve();
        game
    }

    /// Drops the ball in from the top, alternating sides.
    fn serve(&mut self) {
        self.world
            .set_position(self.ball, Vector::new(0, 450_000))
            .unwrap();
        let drift = if self.drains.is_multiple_of(2) {
            300_000
        } else {
            -250_000
        };
        self.world
            .set_velocity(self.ball, Vector::new(drift, 0))
            .unwrap();
    }

    fn ball_position(&self) -> Vector {
        self.world.body(self.ball).unwrap().position()
    }

    fn rest_angle(seat: u8) -> i64 {
        if seat == 0 { -SWING } else { SWING }
    }
}

impl TickGame for Pinball {
    type Seat = u8;
    type Input = Button;
    type DecodeError = String;

    const TICKS_PER_SECOND: u32 = TPS;

    fn neutral_input(&self, _seat: u8) -> Button {
        Button(0)
    }

    fn step(&mut self, inputs: &[(u8, Button)]) {
        for (seat, button) in inputs {
            let angle = if button.0 == 0 {
                Self::rest_angle(*seat)
            } else {
                -Self::rest_angle(*seat)
            };
            let flipper = self.flippers[usize::from(*seat)];
            if button.0 != 0 && self.world.body(flipper).unwrap().angle_target() != angle {
                self.flips += 1;
            }
            self.world.set_angle_target(flipper, angle).unwrap();
        }
        let report = self.world.step();
        for event in report.events {
            match event {
                Event::Bumped { body, .. } if body == self.ball => self.bumps += 1,
                Event::SensorEnter { sensor, body }
                    if sensor == self.drain && body == self.ball =>
                {
                    self.drains += 1;
                    self.serve();
                }
                _ => {}
            }
        }
    }

    fn seat_left(&mut self, seat: u8) {
        self.world
            .set_angle_target(self.flippers[usize::from(seat)], Self::rest_angle(seat))
            .unwrap();
    }

    fn admits(&self, seat: u8) -> bool {
        seat < 2
    }

    fn checksum(&self) -> u64 {
        support::fnv(&self.encode())
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![GAME_VERSION];
        bytes.extend(self.bumps.to_le_bytes());
        bytes.extend(self.flips.to_le_bytes());
        bytes.extend(self.drains.to_le_bytes());
        bytes.extend(self.world.to_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let (version, rest) = bytes.split_first().ok_or("empty")?;
        if *version != GAME_VERSION {
            return Err(format!("unknown pinball version {version}"));
        }
        let (header, rest) = rest.split_at_checked(10).ok_or("truncated")?;
        let world = World::from_bytes(rest).map_err(|error| error.to_string())?;
        Ok(Self {
            world,
            bumps: u32::from_le_bytes([header[0], header[1], header[2], header[3]]),
            flips: u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
            drains: u16::from_le_bytes([header[8], header[9]]),
            ..Self::new()
        })
    }

    fn correction_magnitude(&self, before: &Self) -> u64 {
        self.world
            .bodies()
            .zip(before.world.bodies())
            .map(|((_, now), (_, then))| (now.position() - then.position()).length().unsigned_abs())
            .max()
            .unwrap_or(0)
    }
}

impl HarnessGame for Pinball {
    /// Holds the flipper while the ball is low on its side, with a little
    /// per-seat timing jitter, and lets a cradled ball roll off.
    fn bot(&self, seat: u8, tick: u64) -> Button {
        let ball = self.ball_position();
        if self.world.body(self.ball).unwrap().velocity().length() < 100_000 {
            return Button(0);
        }
        let own_side = if seat == 0 {
            ball.x < 20_000
        } else {
            ball.x > -20_000
        };
        let late = i64::try_from((tick + u64::from(seat) * 7) % 23).unwrap() * 2_000;
        Button(u8::from(own_side && ball.y < FLIPPER_Y + 70_000 - late))
    }

    fn corrupt(input: &mut Button) {
        input.0 ^= 1;
    }

    fn played(&self) -> bool {
        self.flips > 0 && self.bumps > 0
    }
}

const TABLE_DIAGONAL: u64 = 1_120_000;

#[test]
fn pinball_players_and_a_late_spectator_converge_under_moderate_impairment() {
    let outcome = run(Pinball::new(), MODERATE, 4, 3_600, None);
    assert_healthy(&outcome, "pinball moderate", 10, TABLE_DIAGONAL, 250_000);
    assert!(outcome.clients[2].stats.snapshots_adopted >= 1);
    let state = outcome.authority.state();
    println!(
        "pinball: {} bumps, {} flips, {} drains",
        state.bumps, state.flips, state.drains
    );
    assert!(state.flips > 10 && state.bumps > 20 && state.drains > 0);
}

#[test]
fn pinball_converges_under_harsh_impairment() {
    let outcome = run(Pinball::new(), HARSH, 5, 3_600, None);
    assert_healthy(&outcome, "pinball harsh", 50, TABLE_DIAGONAL, 300_000);
}

#[test]
fn a_corrupted_pinball_client_detects_the_desync_and_resynchronizes() {
    let outcome = run(Pinball::new(), MODERATE, 6, 2_400, Some(1_200));
    assert_healthy(&outcome, "pinball corrupted", 10, TABLE_DIAGONAL, 250_000);
    assert_eq!(outcome.clients[1].stats.desyncs_detected, 1);
}

#[test]
fn identical_pinball_impairment_replays_identically() {
    assert_replays_identically(Pinball::new);
}
