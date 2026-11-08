//! A two-player air-hockey test game on planar physics, synchronized by the
//! tick mode of `pwmtf_sync` through a simulated impaired network.
//!
//! Not a product game: it proves that pointer-driven kinematic mallets,
//! deterministic physics, prediction, rollback, late join and desync recovery
//! work together under latency, jitter, loss, reordering and duplication.

use std::collections::{BTreeMap, BTreeSet};

use pwmtf_planar_physics::{
    Aabb, BodyDef, BodyId, Event, MICROS_PER_UNIT, Region, SensorDef, SensorId, Vector, World,
    WorldConfig,
};
use pwmtf_sync::{
    InputReceipt, ReplicaRequest, TickAuthority, TickAuthorityConfig, TickFrame, TickGame,
    TickReplica, TickReplicaConfig, TickSnapshot,
};

const M: i64 = MICROS_PER_UNIT;
const TPS: u32 = 60;
const HALF_WIDTH: i64 = M;
const HALF_HEIGHT: i64 = M / 2;
const GOAL_HALF_WIDTH: i64 = M / 6;
const MALLET_RADIUS: i64 = 45_000;
const PUCK_RADIUS: i64 = 30_000;
const MALLET_SPEED: i64 = 6 * M;
const PUCK_LAYER: u32 = 1;
const MALLET_LAYER: u32 = 2;
const GAME_VERSION: u8 = 1;

/// A pointer target in millimetres: the device-neutral input for mouse,
/// touch or a gamepad-driven cursor alike.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Pointer {
    x: i16,
    y: i16,
}

impl Pointer {
    fn at(position: Vector) -> Self {
        let quantize = |micros: i64| {
            i16::try_from((micros / 1_000).clamp(-2_000, 2_000)).expect("clamped to i16")
        };
        Self {
            x: quantize(position.x),
            y: quantize(position.y),
        }
    }

    fn position(self) -> Vector {
        Vector::new(i64::from(self.x) * 1_000, i64::from(self.y) * 1_000)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct AirHockey {
    world: World,
    mallets: [BodyId; 2],
    puck: BodyId,
    goals: [SensorId; 2],
    score: [u16; 2],
}

fn home(seat: u8) -> Vector {
    Vector::new(if seat == 0 { -M * 3 / 4 } else { M * 3 / 4 }, 0)
}

impl AirHockey {
    fn new() -> Self {
        let mut world = World::new(WorldConfig {
            ticks_per_second: u16::try_from(TPS).unwrap(),
            max_substeps: 16,
            max_speed: 10 * M,
        })
        .unwrap();
        let arena = Aabb::new(
            Vector::new(-HALF_WIDTH, -HALF_HEIGHT),
            Vector::new(HALF_WIDTH, HALF_HEIGHT),
        )
        .unwrap();
        world.add_bounds(arena, 900_000, u32::MAX).unwrap();
        let halves = [
            Aabb::new(
                Vector::new(-HALF_WIDTH + MALLET_RADIUS, -HALF_HEIGHT + MALLET_RADIUS),
                Vector::new(-MALLET_RADIUS, HALF_HEIGHT - MALLET_RADIUS),
            )
            .unwrap(),
            Aabb::new(
                Vector::new(MALLET_RADIUS, -HALF_HEIGHT + MALLET_RADIUS),
                Vector::new(HALF_WIDTH - MALLET_RADIUS, HALF_HEIGHT - MALLET_RADIUS),
            )
            .unwrap(),
        ];
        let mallets = [0, 1].map(|seat| {
            world
                .add_body(BodyDef {
                    confine: Some(halves[usize::from(seat)]),
                    layers: MALLET_LAYER,
                    mask: PUCK_LAYER,
                    ..BodyDef::kinematic(MALLET_RADIUS, home(seat), MALLET_SPEED)
                })
                .unwrap()
        });
        let puck = world
            .add_body(BodyDef {
                restitution: 950_000,
                damping: 80_000,
                friction: M / 20,
                layers: PUCK_LAYER,
                mask: PUCK_LAYER | MALLET_LAYER,
                ..BodyDef::dynamic(PUCK_RADIUS, 15, Vector::ZERO)
            })
            .unwrap();
        let mouth = HALF_WIDTH - PUCK_RADIUS - 5_000;
        let goals = [-1, 1].map(|side: i64| {
            let (min_x, max_x) = if side < 0 {
                (-HALF_WIDTH, -mouth)
            } else {
                (mouth, HALF_WIDTH)
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
                    mask: PUCK_LAYER,
                })
                .unwrap()
        });
        let mut game = Self {
            world,
            mallets,
            puck,
            goals,
            score: [0; 2],
        };
        game.serve(0);
        game
    }

    /// Places the puck at center drifting toward `toward`.
    fn serve(&mut self, toward: u8) {
        self.world.set_position(self.puck, Vector::ZERO).unwrap();
        let drift = if toward == 0 { -M } else { M };
        self.world
            .set_velocity(self.puck, Vector::new(drift, M / 3))
            .unwrap();
    }

    fn mallet(&self, seat: u8) -> &pwmtf_planar_physics::Body {
        self.world.body(self.mallets[usize::from(seat)]).unwrap()
    }

    fn puck_position(&self) -> Vector {
        self.world.body(self.puck).unwrap().position()
    }
}

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

impl TickGame for AirHockey {
    type Seat = u8;
    type Input = Pointer;
    type DecodeError = String;

    const TICKS_PER_SECOND: u32 = TPS;

    fn neutral_input(&self, seat: u8) -> Pointer {
        Pointer::at(home(seat))
    }

    fn step(&mut self, inputs: &[(u8, Pointer)]) {
        for (seat, pointer) in inputs {
            self.world
                .set_target(self.mallets[usize::from(*seat)], pointer.position())
                .unwrap();
        }
        let report = self.world.step();
        for event in report.events {
            if let Event::SensorEnter { sensor, body } = event
                && body == self.puck
            {
                let conceded = u8::from(sensor == self.goals[1]);
                self.score[usize::from(1 - conceded)] += 1;
                self.serve(conceded);
            }
        }
    }

    fn seat_joined(&mut self, seat: u8) {
        self.world
            .set_position(self.mallets[usize::from(seat)], home(seat))
            .unwrap();
    }

    fn seat_left(&mut self, seat: u8) {
        self.world
            .set_target(self.mallets[usize::from(seat)], home(seat))
            .unwrap();
    }

    fn admits(&self, seat: u8) -> bool {
        seat < 2
    }

    fn checksum(&self) -> u64 {
        fnv(&self.encode())
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![GAME_VERSION];
        bytes.extend(self.score[0].to_le_bytes());
        bytes.extend(self.score[1].to_le_bytes());
        bytes.extend(self.world.to_bytes());
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, String> {
        let (version, rest) = bytes.split_first().ok_or("empty")?;
        if *version != GAME_VERSION {
            return Err(format!("unknown air hockey version {version}"));
        }
        let (scores, rest) = rest.split_at_checked(4).ok_or("truncated")?;
        let world = World::from_bytes(rest).map_err(|error| error.to_string())?;
        let template = Self::new();
        Ok(Self {
            world,
            score: [
                u16::from_le_bytes([scores[0], scores[1]]),
                u16::from_le_bytes([scores[2], scores[3]]),
            ],
            ..template
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

/// `SplitMix64`, for reproducible impairment.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, bound: u64) -> u64 {
        if bound == 0 { 0 } else { self.next() % bound }
    }

    fn per_mille(&mut self, chance: u64) -> bool {
        self.below(1_000) < chance
    }
}

#[derive(Clone, Copy, Debug)]
struct Impairment {
    latency_ms: u64,
    jitter_ms: u64,
    loss_per_mille: u64,
    duplicate_per_mille: u64,
}

type Frame = TickFrame<u8, Pointer>;
type Snapshot = TickSnapshot<u8, Pointer>;

#[derive(Clone, Debug)]
enum Message {
    /// Client to server: batched unconfirmed inputs, the confirmed tick as an
    /// acknowledgement, and whether a snapshot is needed.
    Inputs {
        client: usize,
        ack: u64,
        inputs: Vec<(u64, Pointer)>,
        wants_snapshot: bool,
    },
    /// Server to client: contiguous frames from the client's acknowledgement.
    Frames { client: usize, frames: Vec<Frame> },
    /// Server to client: a full snapshot.
    Snapshot { client: usize, snapshot: Snapshot },
}

struct Network {
    impairment: Impairment,
    random: Random,
    queue: BTreeMap<(u64, u64), Message>,
    sequence: u64,
    sent: u64,
}

impl Network {
    fn send(&mut self, now: u64, message: Message) {
        self.sent += 1;
        let copies = if self.random.per_mille(self.impairment.duplicate_per_mille) {
            2
        } else {
            1
        };
        for _ in 0..copies {
            if self.random.per_mille(self.impairment.loss_per_mille) {
                continue;
            }
            let delay =
                self.impairment.latency_ms + self.random.below(self.impairment.jitter_ms + 1);
            self.sequence += 1;
            self.queue
                .insert((now + delay, self.sequence), message.clone());
        }
    }

    fn due(&mut self, now: u64) -> Vec<Message> {
        let later = self.queue.split_off(&(now + 1, 0));
        std::mem::replace(&mut self.queue, later)
            .into_values()
            .collect()
    }
}

const SEND_INTERVAL_MS: u64 = 50;
const MAX_FRAMES_PER_MESSAGE: usize = 45;

#[derive(Default)]
struct Stats {
    predicted: u64,
    stalled: u64,
    corrections: u64,
    max_correction: u64,
    total_correction: u64,
    max_resimulated: u64,
    checksums_compared: u64,
    snapshots_adopted: u64,
    desyncs_detected: u64,
    /// Ticks for which the authority received this player's input in time.
    accepted: BTreeSet<u64>,
}

struct Client {
    seat: Option<u8>,
    replica: Option<TickReplica<AirHockey>>,
    /// Joins (asks for a snapshot) at this time.
    joins_at: u64,
    stats: Stats,
    /// Corrupted on purpose and not yet resynchronized.
    corrupted: bool,
    last_confirmed: u64,
}

fn replica_config() -> TickReplicaConfig {
    TickReplicaConfig {
        prediction_horizon: 40,
        max_buffered_frames: 240,
        spectator_delay: 4,
    }
}

/// A scripted player: the pointer chases the presented puck from behind,
/// with a wobble, at a human hand's speed (three metres per second).
fn bot_input(game: &AirHockey, seat: u8, tick: u64) -> Pointer {
    let puck = game.puck_position();
    let behind = if seat == 0 { -M / 20 } else { M / 20 };
    let phase = i64::try_from(tick % 40).unwrap() - 20;
    let wobble = phase * (M / 400);
    let desired = Vector::new(puck.x + behind, puck.y + wobble);
    let previous = game.mallet(seat).target();
    let hand_speed = 3 * M / i64::from(TPS);
    Pointer::at(previous + (desired - previous).clamp_length(hand_speed))
}

struct Outcome {
    clients: Vec<Client>,
    authority: TickAuthority<AirHockey>,
}

#[allow(clippy::too_many_lines)]
fn run(impairment: Impairment, seed: u64, ticks: u64, corrupt_at: Option<u64>) -> Outcome {
    let mut authority = TickAuthority::open(
        AirHockey::new(),
        TickAuthorityConfig {
            input_window: 120,
            checksum_interval: 30,
            frame_history: 600,
        },
    );
    authority.join(0).unwrap();
    authority.join(1).unwrap();
    let initial = authority.snapshot();
    let mut network = Network {
        impairment,
        random: Random(seed),
        queue: BTreeMap::new(),
        sequence: 0,
        sent: 0,
    };
    let mut clients: Vec<Client> = [Some(0), Some(1), None]
        .into_iter()
        .map(|seat| Client {
            seat,
            replica: seat.map(|seat| {
                TickReplica::from_snapshot(&initial, Some(seat), replica_config()).unwrap()
            }),
            joins_at: if seat.is_some() { 0 } else { 20_000 },
            stats: Stats::default(),
            corrupted: false,
            last_confirmed: 0,
        })
        .collect();
    let mut history = BTreeMap::from([(0_u64, authority.state().checksum())]);
    let mut acks = [0_u64; 3];
    let mut snapshot_wanted = [false; 3];
    let mut last_snapshot_sent = [0_u64; 3];
    // Inputs must reach the authority before it simulates their tick: one
    // way latency, jitter and the batching interval, plus one more interval
    // so a single lost batch is repaired by the next, plus margin.
    let ahead = (impairment.latency_ms + impairment.jitter_ms + 2 * SEND_INTERVAL_MS)
        * u64::from(TPS)
        / 1_000
        + 2;
    let end_ms = ticks * 1_000 / u64::from(TPS);
    let mut server_tick = 0;
    for now in 0..=end_ms {
        for message in network.due(now) {
            match message {
                Message::Inputs {
                    client,
                    ack,
                    inputs,
                    wants_snapshot,
                } => {
                    acks[client] = acks[client].max(ack);
                    snapshot_wanted[client] |= wants_snapshot;
                    if let Some(seat) = clients[client].seat {
                        let ticks: Vec<u64> = inputs.iter().map(|(tick, _)| *tick).collect();
                        let results = authority.submit_batch(seat, inputs);
                        for (tick, result) in ticks.into_iter().zip(results) {
                            if result == Ok(InputReceipt::Accepted) {
                                clients[client].stats.accepted.insert(tick);
                            }
                        }
                    }
                }
                Message::Frames { client, frames } => {
                    if let Some(replica) = &mut clients[client].replica {
                        for frame in frames {
                            replica.receive(frame);
                        }
                    }
                }
                Message::Snapshot { client, snapshot } => {
                    let entry = &mut clients[client];
                    match &mut entry.replica {
                        Some(replica) => {
                            if replica.request() == Some(ReplicaRequest::Snapshot) {
                                replica.adopt(&snapshot).unwrap();
                                entry.stats.snapshots_adopted += 1;
                                entry.corrupted = false;
                            }
                        }
                        None => {
                            entry.replica = Some(
                                TickReplica::from_snapshot(&snapshot, entry.seat, replica_config())
                                    .unwrap(),
                            );
                            entry.stats.snapshots_adopted += 1;
                        }
                    }
                }
            }
        }

        let clock_tick = now * u64::from(TPS) / 1_000;
        let ticked = clock_tick > server_tick;
        while server_tick < clock_tick {
            authority.advance();
            server_tick = authority.tick();
            history.insert(server_tick, authority.state().checksum());
        }
        if !ticked {
            continue;
        }

        if corrupt_at == Some(server_tick) {
            // The newest frame, corrupted in transit, reaches player two
            // before the real one: its state silently diverges until the
            // next checksum.
            let replica = clients[1].replica.as_mut().unwrap();
            let mut forged = authority.frames_from(server_tick - 1).unwrap()[0].clone();
            forged.inputs[0].1.y = forged.inputs[0].1.y.wrapping_add(300);
            assert!(replica.receive(forged));
            clients[1].corrupted = true;
        }

        for (index, client) in clients.iter_mut().enumerate() {
            let Some(replica) = &mut client.replica else {
                if now >= client.joins_at && server_tick.is_multiple_of(15) {
                    network.send(
                        now,
                        Message::Inputs {
                            client: index,
                            ack: 0,
                            inputs: Vec::new(),
                            wants_snapshot: true,
                        },
                    );
                }
                continue;
            };
            let target = server_tick + ahead;
            let steps = match client.seat {
                // Catch up at most three ticks per host tick.
                Some(_) => target.saturating_sub(replica.predicted_tick()).min(3),
                None => 1,
            };
            for _ in 0..steps {
                let was_desynced = replica.is_desynced();
                let input = client
                    .seat
                    .map(|seat| bot_input(replica.presented(), seat, replica.predicted_tick()));
                let advance = replica.advance(input);
                if replica.is_desynced() && !was_desynced {
                    client.stats.desyncs_detected += 1;
                }
                if advance.predicted {
                    client.stats.predicted += 1;
                    let seat = client.seat.unwrap();
                    // Once seated, the local input is applied the moment it
                    // is made.
                    if replica.seats().any(|(seated, _)| seated == seat) {
                        let mallet = replica.presented().mallet(seat);
                        assert_eq!(mallet.target(), input.unwrap().position());
                    }
                } else if advance.stalled {
                    client.stats.stalled += 1;
                }
                if let Some(correction) = advance.correction {
                    client.stats.corrections += 1;
                    client.stats.total_correction += correction.magnitude;
                    client.stats.max_correction =
                        client.stats.max_correction.max(correction.magnitude);
                    client.stats.max_resimulated = client
                        .stats
                        .max_resimulated
                        .max(correction.ticks_resimulated);
                }
                if replica.confirmed_tick() != client.last_confirmed {
                    client.last_confirmed = replica.confirmed_tick();
                    if !client.corrupted {
                        client.stats.checksums_compared += 1;
                        assert_eq!(
                            replica.confirmed_state().checksum(),
                            history[&replica.confirmed_tick()],
                            "client {index} diverged at tick {}",
                            replica.confirmed_tick()
                        );
                    }
                }
            }
            if now.is_multiple_of(SEND_INTERVAL_MS) {
                network.send(
                    now,
                    Message::Inputs {
                        client: index,
                        ack: replica.confirmed_tick(),
                        inputs: replica.unconfirmed_inputs().collect(),
                        wants_snapshot: replica.request() == Some(ReplicaRequest::Snapshot),
                    },
                );
            }
        }

        if now.is_multiple_of(SEND_INTERVAL_MS) {
            for client in 0..clients.len() {
                if snapshot_wanted[client] && now >= last_snapshot_sent[client] + 200 {
                    snapshot_wanted[client] = false;
                    last_snapshot_sent[client] = now;
                    network.send(
                        now,
                        Message::Snapshot {
                            client,
                            snapshot: authority.snapshot(),
                        },
                    );
                    continue;
                }
                if clients[client].replica.is_none() {
                    continue;
                }
                match authority.frames_from(acks[client]) {
                    Ok(frames) if !frames.is_empty() => {
                        let frames = frames.into_iter().take(MAX_FRAMES_PER_MESSAGE).collect();
                        network.send(now, Message::Frames { client, frames });
                    }
                    Ok(_) => {}
                    Err(_) => snapshot_wanted[client] = true,
                }
            }
        }
    }
    Outcome { clients, authority }
}

/// The diagonal of the table: no presented position can jump further.
const TABLE_DIAGONAL: u64 = 2_236_068;

fn assert_healthy(
    outcome: &Outcome,
    label: &str,
    max_missed_per_mille: u64,
    max_mean_correction: u64,
) {
    let server_tick = outcome.authority.tick();
    let state = outcome.authority.state();
    assert!(
        state.score.iter().sum::<u16>() > 0 || state.puck_position() != Vector::ZERO,
        "{label}: the game must actually be played"
    );
    for (index, client) in outcome.clients.iter().enumerate() {
        let replica = client.replica.as_ref().expect("every client joined");
        let stats = &client.stats;
        println!(
            "{label} client {index}: confirmed {} / {server_tick}, predicted {}, stalled {}, \
             corrections {}, mean correction {} um, max correction {} um, max resimulated {}, \
             checksums compared {}, snapshots {}, desyncs {}, missed inputs {}",
            replica.confirmed_tick(),
            stats.predicted,
            stats.stalled,
            stats.corrections,
            stats.total_correction / stats.corrections.max(1),
            stats.max_correction,
            stats.max_resimulated,
            stats.checksums_compared,
            stats.snapshots_adopted,
            stats.desyncs_detected,
            missed_inputs(client, server_tick),
        );
        assert!(!replica.is_desynced(), "{label}: client {index} desynced");
        assert!(!client.corrupted, "{label}: client {index} never resynced");
        // Confirmation keeps up with the authority, within a second.
        assert!(
            server_tick - replica.confirmed_tick() < u64::from(TPS),
            "{label}: client {index} fell behind"
        );
        assert!(stats.checksums_compared > 100);
        if client.seat.is_some() {
            assert!(
                stats.stalled * 50 <= stats.predicted,
                "{label}: client {index} stalled too often"
            );
            assert!(stats.max_resimulated <= replica_config().prediction_horizon);
            assert!(stats.max_correction <= TABLE_DIAGONAL);
            // Mostly the puck and the remote mallet, which are predicted
            // from the remote seat's last confirmed pointer target.
            assert!(
                stats.total_correction / stats.corrections.max(1) <= max_mean_correction,
                "{label}: client {index} corrections too large"
            );
            // The local mallet depends only on local inputs, so it is
            // corrected only when an input missed its tick at the authority
            // (every copy lost or too late) and the previous one repeated.
            assert!(
                missed_inputs(client, server_tick) * 1_000 <= server_tick * max_missed_per_mille,
                "{label}: client {index} missed too many ticks"
            );
        }
    }
}

fn missed_inputs(client: &Client, server_tick: u64) -> u64 {
    server_tick - client.stats.accepted.range(..server_tick).count() as u64
}

const MODERATE: Impairment = Impairment {
    latency_ms: 40,
    jitter_ms: 25,
    loss_per_mille: 50,
    duplicate_per_mille: 30,
};

const HARSH: Impairment = Impairment {
    latency_ms: 90,
    jitter_ms: 60,
    loss_per_mille: 150,
    duplicate_per_mille: 100,
};

#[test]
fn players_and_a_late_spectator_converge_under_moderate_impairment() {
    let outcome = run(MODERATE, 1, 4_800, None);
    assert_healthy(&outcome, "moderate", 10, 250_000);
    // The spectator joined twenty seconds in, from a snapshot.
    assert!(outcome.clients[2].stats.snapshots_adopted >= 1);
}

#[test]
fn players_and_a_late_spectator_converge_under_harsh_impairment() {
    let outcome = run(HARSH, 2, 4_800, None);
    assert_healthy(&outcome, "harsh", 50, 300_000);
}

#[test]
fn a_corrupted_client_detects_the_desync_and_resynchronizes() {
    let outcome = run(MODERATE, 3, 3_600, Some(1_500));
    assert_healthy(&outcome, "corrupted", 10, 250_000);
    let victim = &outcome.clients[1].stats;
    assert_eq!(victim.desyncs_detected, 1);
    assert!(victim.snapshots_adopted >= 1);
    for (index, client) in outcome.clients.iter().enumerate() {
        if index != 1 {
            assert_eq!(client.stats.desyncs_detected, 0);
        }
    }
}

#[test]
fn identical_impairment_replays_identically() {
    let first = run(MODERATE, 9, 1_200, None);
    let second = run(MODERATE, 9, 1_200, None);
    assert_eq!(
        first.authority.state().checksum(),
        second.authority.state().checksum()
    );
    for (left, right) in first.clients.iter().zip(&second.clients) {
        let (left, right) = (left.replica.as_ref(), right.replica.as_ref());
        assert_eq!(
            left.map(|replica| replica.presented().checksum()),
            right.map(|replica| replica.presented().checksum())
        );
    }
}
