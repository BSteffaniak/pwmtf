//! A reproducible impaired-network harness for two-seat tick games on
//! planar physics, synchronized by the tick mode of `pwmtf_sync`: two
//! players from the start and a spectator joining late from a snapshot,
//! under latency, jitter, loss, reordering and duplication, with optional
//! deliberate corruption to prove desync detection and recovery.

use std::collections::{BTreeMap, BTreeSet};

use pwmtf_sync::{
    InputReceipt, ReplicaRequest, TickAuthority, TickAuthorityConfig, TickFrame, TickGame,
    TickReplica, TickReplicaConfig, TickSnapshot,
};

/// What a test game adds for the harness.
pub trait HarnessGame: TickGame<Seat = u8> {
    /// A scripted player's input from the presented state.
    fn bot(&self, seat: u8, tick: u64) -> Self::Input;
    /// Alters an input in transit so state silently diverges.
    fn corrupt(input: &mut Self::Input);
    /// Whether the game was actually played (something scored or moved).
    fn played(&self) -> bool;
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
pub struct Impairment {
    pub latency_ms: u64,
    pub jitter_ms: u64,
    pub loss_per_mille: u64,
    pub duplicate_per_mille: u64,
}

pub const MODERATE: Impairment = Impairment {
    latency_ms: 40,
    jitter_ms: 25,
    loss_per_mille: 50,
    duplicate_per_mille: 30,
};

pub const HARSH: Impairment = Impairment {
    latency_ms: 90,
    jitter_ms: 60,
    loss_per_mille: 150,
    duplicate_per_mille: 100,
};

#[derive(Clone, Debug)]
enum Message<I> {
    Inputs {
        client: usize,
        ack: u64,
        inputs: Vec<(u64, I)>,
        wants_snapshot: bool,
    },
    Frames {
        client: usize,
        frames: Vec<TickFrame<u8, I>>,
    },
    Snapshot {
        client: usize,
        snapshot: TickSnapshot<u8, I>,
    },
}

struct Network<I> {
    impairment: Impairment,
    random: Random,
    queue: BTreeMap<(u64, u64), Message<I>>,
    sequence: u64,
}

impl<I: Clone> Network<I> {
    fn send(&mut self, now: u64, message: Message<I>) {
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

    fn due(&mut self, now: u64) -> Vec<Message<I>> {
        let later = self.queue.split_off(&(now + 1, 0));
        std::mem::replace(&mut self.queue, later)
            .into_values()
            .collect()
    }
}

const SEND_INTERVAL_MS: u64 = 50;
const MAX_FRAMES_PER_MESSAGE: usize = 45;

#[derive(Default)]
pub struct Stats {
    pub predicted: u64,
    pub stalled: u64,
    pub corrections: u64,
    pub max_correction: u64,
    pub total_correction: u64,
    pub max_resimulated: u64,
    pub checksums_compared: u64,
    pub snapshots_adopted: u64,
    pub desyncs_detected: u64,
    accepted: BTreeSet<u64>,
}

pub struct Client<G: HarnessGame> {
    pub seat: Option<u8>,
    pub replica: Option<TickReplica<G>>,
    joins_at: u64,
    pub stats: Stats,
    pub corrupted: bool,
    last_confirmed: u64,
}

pub fn replica_config() -> TickReplicaConfig {
    TickReplicaConfig {
        prediction_horizon: 40,
        max_buffered_frames: 240,
        spectator_delay: 4,
    }
}

pub struct Outcome<G: HarnessGame> {
    pub clients: Vec<Client<G>>,
    pub authority: TickAuthority<G>,
}

/// Runs `ticks` of `game` with two bots and a spectator joining after
/// twenty seconds. With `corrupt_at`, player two receives a forged frame at
/// that tick.
#[allow(clippy::too_many_lines)]
pub fn run<G: HarnessGame>(
    game: G,
    impairment: Impairment,
    seed: u64,
    ticks: u64,
    corrupt_at: Option<u64>,
) -> Outcome<G> {
    let tps = u64::from(G::TICKS_PER_SECOND);
    let mut authority = TickAuthority::open(
        game,
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
    };
    let mut clients: Vec<Client<G>> = [Some(0), Some(1), None]
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
    let ahead =
        (impairment.latency_ms + impairment.jitter_ms + 2 * SEND_INTERVAL_MS) * tps / 1_000 + 2;
    let end_ms = ticks * 1_000 / tps;
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

        let clock_tick = now * tps / 1_000;
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
            let replica = clients[1].replica.as_mut().unwrap();
            let mut forged = authority.frames_from(server_tick - 1).unwrap()[0].clone();
            G::corrupt(&mut forged.inputs[0].1);
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
                Some(_) => target.saturating_sub(replica.predicted_tick()).min(3),
                None => 1,
            };
            for _ in 0..steps {
                let was_desynced = replica.is_desynced();
                let input = client
                    .seat
                    .map(|seat| replica.presented().bot(seat, replica.predicted_tick()));
                let advance = replica.advance(input);
                if replica.is_desynced() && !was_desynced {
                    client.stats.desyncs_detected += 1;
                }
                if advance.predicted {
                    client.stats.predicted += 1;
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

/// Asserts every client converged on the authority: no unresolved desync,
/// confirmation within a second, checksums compared throughout, rare stalls,
/// bounded re-simulation and corrections.
pub fn assert_healthy<G: HarnessGame>(
    outcome: &Outcome<G>,
    label: &str,
    max_missed_per_mille: u64,
    max_correction: u64,
    max_mean_correction: u64,
) {
    let tps = u64::from(G::TICKS_PER_SECOND);
    let server_tick = outcome.authority.tick();
    assert!(
        outcome.authority.state().played(),
        "{label}: the game must actually be played"
    );
    for (index, client) in outcome.clients.iter().enumerate() {
        let replica = client.replica.as_ref().expect("every client joined");
        let stats = &client.stats;
        let missed = server_tick - stats.accepted.range(..server_tick).count() as u64;
        println!(
            "{label} client {index}: confirmed {} / {server_tick}, predicted {}, stalled {}, \
             corrections {}, mean correction {} um, max correction {} um, max resimulated {}, \
             checksums compared {}, snapshots {}, desyncs {}, missed inputs {missed}",
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
        );
        assert!(!replica.is_desynced(), "{label}: client {index} desynced");
        assert!(!client.corrupted, "{label}: client {index} never resynced");
        assert!(
            server_tick - replica.confirmed_tick() < tps,
            "{label}: client {index} fell behind"
        );
        assert!(stats.checksums_compared > 100);
        if client.seat.is_some() {
            assert!(
                stats.stalled * 50 <= stats.predicted,
                "{label}: client {index} stalled too often"
            );
            assert!(stats.max_resimulated <= replica_config().prediction_horizon);
            assert!(stats.max_correction <= max_correction);
            assert!(
                stats.total_correction / stats.corrections.max(1) <= max_mean_correction,
                "{label}: client {index} corrections too large"
            );
            assert!(
                missed * 1_000 <= server_tick * max_missed_per_mille,
                "{label}: client {index} missed too many ticks"
            );
        }
    }
}

/// Two identical runs end in identical states everywhere.
pub fn assert_replays_identically<G: HarnessGame>(make: fn() -> G) {
    let first = run(make(), MODERATE, 9, 900, None);
    let second = run(make(), MODERATE, 9, 900, None);
    assert_eq!(
        first.authority.state().checksum(),
        second.authority.state().checksum()
    );
    for (left, right) in first.clients.iter().zip(&second.clients) {
        assert_eq!(
            left.replica
                .as_ref()
                .map(|replica| replica.presented().checksum()),
            right
                .replica
                .as_ref()
                .map(|replica| replica.presented().checksum())
        );
    }
}

/// FNV-1a, the checksum PWMTF uses for canonical encodings.
pub fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}
