//! A two-player foosball test game on planar physics shaped bodies,
//! synchronized by the tick mode of `pwmtf_sync` through a simulated
//! impaired network.
//!
//! Not a product game: it proves that compound kinematic rods whose figures
//! slide together and spin on their own spindles, with rotating box
//! contacts, keep rollback synchronization converging under latency, jitter,
//! loss, reordering and duplication.

mod support;

use pwmtf_planar_physics::{
    Aabb, Attachment, BodyDef, BodyId, Event, MICROS_PER_UNIT, PI_MICRORADIANS, Part, Region,
    SensorDef, SensorId, Shape, ShapedBodyDef, Vector, World, WorldConfig,
};
use pwmtf_sync::TickGame;
use support::{HARSH, HarnessGame, MODERATE, assert_healthy, assert_replays_identically, run};

const M: i64 = MICROS_PER_UNIT;
const TPS: u32 = 60;
const HALF_LENGTH: i64 = 600_000;
const HALF_WIDTH: i64 = 350_000;
const GOAL_HALF_WIDTH: i64 = 100_000;
const BALL_RADIUS: i64 = 20_000;
const ROD_X: i64 = 300_000;
const SLIDE: i64 = 120_000;
const FIGURE_SPACING: i64 = 230_000;
const HANGING: i64 = PI_MICRORADIANS / 2;
const GAME_VERSION: u8 = 1;

/// A rod command: slide position in millimetres and spin angle in
/// milliradians. Device neutral: a drag, a stick or keys all produce it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Rod {
    slide: i16,
    spin: i16,
}

impl Rod {
    const fn hanging() -> Self {
        Self {
            slide: 0,
            spin: 1_571,
        }
    }

    fn slide(self) -> i64 {
        i64::from(self.slide).clamp(-120, 120) * 1_000
    }

    fn spin(self) -> i64 {
        i64::from(self.spin).clamp(-3_142, 3_142) * 1_000
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Foosball {
    world: World,
    rods: [BodyId; 2],
    ball: BodyId,
    goals: [SensorId; 2],
    score: [u16; 2],
    kicks: u32,
}

/// Seat 0's rod stands left of center and attacks +x, seat 1's the mirror.
fn rod_x(seat: u8) -> i64 {
    if seat == 0 { -ROD_X } else { ROD_X }
}

impl Foosball {
    fn new() -> Self {
        let mut world = World::new(WorldConfig {
            ticks_per_second: u16::try_from(TPS).unwrap(),
            max_substeps: 16,
            max_speed: 8 * M,
        })
        .unwrap();
        world
            .add_bounds(
                Aabb::new(
                    Vector::new(-HALF_LENGTH, -HALF_WIDTH),
                    Vector::new(HALF_LENGTH, HALF_WIDTH),
                )
                .unwrap(),
                850_000,
                u32::MAX,
            )
            .unwrap();
        let rods = [0, 1].map(|seat| {
            let x = rod_x(seat);
            let figure = |y| Part {
                attachment: Attachment::Spindle,
                ..Part::at(
                    Shape::Rect {
                        half_width: 55_000,
                        half_height: 18_000,
                    },
                    Vector::new(0, y),
                    0,
                )
            };
            world
                .add_shaped_body(ShapedBodyDef {
                    angle: HANGING,
                    body: BodyDef {
                        confine: Some(
                            Aabb::new(Vector::new(x, -SLIDE), Vector::new(x, SLIDE)).unwrap(),
                        ),
                        restitution: 700_000,
                        ..BodyDef::kinematic(1, Vector::new(x, 0), 2 * M)
                    },
                    ..ShapedBodyDef::kinematic(
                        Vector::new(x, 0),
                        2 * M,
                        35_000_000,
                        vec![figure(-FIGURE_SPACING), figure(0), figure(FIGURE_SPACING)],
                    )
                })
                .unwrap()
        });
        let ball = world
            .add_body(BodyDef {
                restitution: 800_000,
                damping: 50_000,
                friction: M / 20,
                ..BodyDef::dynamic(BALL_RADIUS, 20, Vector::ZERO)
            })
            .unwrap();
        let goals = [-1_i64, 1].map(|side| {
            let (min_x, max_x) = if side < 0 {
                (-HALF_LENGTH, -HALF_LENGTH + BALL_RADIUS + 5_000)
            } else {
                (HALF_LENGTH - BALL_RADIUS - 5_000, HALF_LENGTH)
            };
            world
                .add_sensor(SensorDef {
                    region: Region::Rect(
                        Aabb::new(
                            Vector::new(min_x, -GOAL_HALF_WIDTH),
                            Vector::new(max_x, GOAL_HALF_WIDTH),
                        )
                        .unwrap(),
                    ),
                    mask: u32::MAX,
                })
                .unwrap()
        });
        let mut game = Self {
            world,
            rods,
            ball,
            goals,
            score: [0; 2],
            kicks: 0,
        };
        game.serve(0);
        game
    }

    fn serve(&mut self, toward: u8) {
        self.world.set_position(self.ball, Vector::ZERO).unwrap();
        let drift = if toward == 0 { -M / 2 } else { M / 2 };
        self.world
            .set_velocity(self.ball, Vector::new(drift, M / 5))
            .unwrap();
    }

    fn ball_position(&self) -> Vector {
        self.world.body(self.ball).unwrap().position()
    }
}

impl TickGame for Foosball {
    type Seat = u8;
    type Input = Rod;
    type DecodeError = String;

    const TICKS_PER_SECOND: u32 = TPS;

    fn neutral_input(&self, _seat: u8) -> Rod {
        Rod::hanging()
    }

    fn step(&mut self, inputs: &[(u8, Rod)]) {
        for (seat, rod) in inputs {
            let id = self.rods[usize::from(*seat)];
            self.world
                .set_target(id, Vector::new(rod_x(*seat), rod.slide()))
                .unwrap();
            self.world.set_angle_target(id, rod.spin()).unwrap();
        }
        let report = self.world.step();
        for event in report.events {
            match event {
                Event::SensorEnter { sensor, body } if body == self.ball => {
                    let conceded = u8::from(sensor == self.goals[1]);
                    self.score[usize::from(1 - conceded)] += 1;
                    self.serve(conceded);
                }
                Event::PartContact { first, .. } if self.rods.contains(&first) => {
                    self.kicks += 1;
                }
                _ => {}
            }
        }
        // A dead ball out of everyone's reach is served again.
        if self.world.body(self.ball).unwrap().velocity() == Vector::ZERO {
            self.serve(u8::from(self.kicks % 2 == 1));
        }
    }

    fn seat_joined(&mut self, seat: u8) {
        let id = self.rods[usize::from(seat)];
        self.world
            .set_position(id, Vector::new(rod_x(seat), 0))
            .unwrap();
        self.world.set_angle(id, HANGING).unwrap();
    }

    fn admits(&self, seat: u8) -> bool {
        seat < 2
    }

    fn checksum(&self) -> u64 {
        support::fnv(&self.encode())
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![GAME_VERSION];
        bytes.extend(self.score[0].to_le_bytes());
        bytes.extend(self.score[1].to_le_bytes());
        bytes.extend(self.kicks.to_le_bytes());
        bytes.extend(self.world.to_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let (version, rest) = bytes.split_first().ok_or("empty")?;
        if *version != GAME_VERSION {
            return Err(format!("unknown foosball version {version}"));
        }
        let (header, rest) = rest.split_at_checked(8).ok_or("truncated")?;
        let world = World::from_bytes(rest).map_err(|error| error.to_string())?;
        Ok(Self {
            world,
            score: [
                u16::from_le_bytes([header[0], header[1]]),
                u16::from_le_bytes([header[2], header[3]]),
            ],
            kicks: u32::from_le_bytes([header[4], header[5], header[6], header[7]]),
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

impl HarnessGame for Foosball {
    /// Tracks the ball with the middle figure's upper half and kicks when
    /// the ball is just in front of the rod.
    fn bot(&self, seat: u8, tick: u64) -> Rod {
        let ball = self.ball_position();
        let x = rod_x(seat);
        let forward = if seat == 0 { ball.x - x } else { x - ball.x };
        let wobble = i64::try_from(tick % 50).unwrap() * 2_000 - 50_000;
        // Pick the nearest figure and line its upper half up with the ball.
        let figure = [-FIGURE_SPACING, 0, FIGURE_SPACING]
            .into_iter()
            .min_by_key(|offset| (ball.y - offset).abs())
            .unwrap();
        let slide = (ball.y - figure - 35_000 + wobble).clamp(-SLIDE, SLIDE);
        let kicking = (0..90_000).contains(&forward);
        let spin = match (kicking, seat) {
            (false, _) => 1_571,
            (true, 0) => 0,
            (true, _) => 3_141,
        };
        Rod {
            slide: i16::try_from(slide / 1_000).unwrap(),
            spin,
        }
    }

    fn corrupt(input: &mut Rod) {
        input.slide = input.slide.wrapping_add(60);
        input.spin = input.spin.wrapping_add(900);
    }

    fn played(&self) -> bool {
        self.kicks > 0
            && (self.score.iter().sum::<u16>() > 0 || self.ball_position() != Vector::ZERO)
    }
}

/// The table's diagonal: no presented position can jump further.
const TABLE_DIAGONAL: u64 = 1_400_000;

#[test]
fn foosball_players_and_a_late_spectator_converge_under_moderate_impairment() {
    let outcome = run(Foosball::new(), MODERATE, 1, 3_600, None);
    assert_healthy(&outcome, "foosball moderate", 10, TABLE_DIAGONAL, 250_000);
    assert!(outcome.clients[2].stats.snapshots_adopted >= 1);
    let state = outcome.authority.state();
    println!("foosball: {} kicks, score {:?}", state.kicks, state.score);
    assert!(state.kicks > 10);
}

#[test]
fn foosball_converges_under_harsh_impairment() {
    let outcome = run(Foosball::new(), HARSH, 2, 3_600, None);
    assert_healthy(&outcome, "foosball harsh", 50, TABLE_DIAGONAL, 300_000);
}

#[test]
fn a_corrupted_foosball_client_detects_the_desync_and_resynchronizes() {
    let outcome = run(Foosball::new(), MODERATE, 3, 2_400, Some(1_200));
    assert_healthy(&outcome, "foosball corrupted", 10, TABLE_DIAGONAL, 250_000);
    assert_eq!(outcome.clients[1].stats.desyncs_detected, 1);
    assert!(outcome.clients[1].stats.snapshots_adopted >= 1);
    for (index, client) in outcome.clients.iter().enumerate() {
        if index != 1 {
            assert_eq!(client.stats.desyncs_detected, 0);
        }
    }
}

#[test]
fn identical_foosball_impairment_replays_identically() {
    assert_replays_identically(Foosball::new);
}
