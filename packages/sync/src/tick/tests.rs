//! Input window rules and rollback edge cases, exercised with a tiny
//! non-physics game so the tick contract stays game-agnostic.

use std::collections::BTreeMap;

use super::{
    Advance, FramesUnavailable, InputReceipt, InputRefusal, Membership, MembershipError,
    ReplicaRequest, TickAuthority, TickAuthorityConfig, TickFrame, TickGame, TickReplica,
    TickReplicaConfig, TickReplicaError, lead_ticks,
};

/// Each seat steers a dot toward its target by at most two units per tick; a
/// running total makes the state depend on its whole history.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct Dots {
    dots: BTreeMap<u8, i32>,
    total: i64,
}

const VERSION: u8 = 1;

impl TickGame for Dots {
    type Seat = u8;
    type Input = i32;
    type DecodeError = &'static str;

    const TICKS_PER_SECOND: u32 = 60;

    fn neutral_input(&self, _seat: u8) -> i32 {
        0
    }

    fn step(&mut self, inputs: &[(u8, i32)]) {
        assert!(inputs.windows(2).all(|pair| pair[0].0 < pair[1].0));
        assert_eq!(inputs.len(), self.dots.len());
        for (seat, target) in inputs {
            let dot = self
                .dots
                .get_mut(seat)
                .expect("inputs only for seated seats");
            *dot += (*target - *dot).clamp(-2, 2);
            self.total += i64::from(*dot);
        }
    }

    fn seat_joined(&mut self, seat: u8) {
        self.dots.insert(seat, 0);
    }

    fn seat_left(&mut self, seat: u8) {
        self.dots.remove(&seat);
    }

    fn admits(&self, seat: u8) -> bool {
        seat < 4
    }

    fn checksum(&self) -> u64 {
        self.encode()
            .into_iter()
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![VERSION];
        bytes.extend(self.total.to_le_bytes());
        for (seat, dot) in &self.dots {
            bytes.push(*seat);
            bytes.extend(dot.to_le_bytes());
        }
        bytes
    }

    fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        let (version, rest) = bytes.split_first().ok_or("empty")?;
        if *version != VERSION {
            return Err("unknown version");
        }
        let (total, mut rest) = rest.split_at_checked(8).ok_or("truncated")?;
        let total = i64::from_le_bytes(total.try_into().map_err(|_| "truncated")?);
        let mut dots = BTreeMap::new();
        while let Some((seat, tail)) = rest.split_first() {
            let (dot, tail) = tail.split_at_checked(4).ok_or("truncated")?;
            dots.insert(
                *seat,
                i32::from_le_bytes(dot.try_into().map_err(|_| "truncated")?),
            );
            rest = tail;
        }
        Ok(Self { dots, total })
    }

    fn correction_magnitude(&self, before: &Self) -> u64 {
        self.dots
            .iter()
            .map(|(seat, dot)| u64::from(before.dots.get(seat).map_or(0, |old| dot.abs_diff(*old))))
            .max()
            .unwrap_or(0)
    }
}

fn config() -> TickAuthorityConfig {
    TickAuthorityConfig {
        input_window: 8,
        checksum_interval: 4,
        frame_history: 16,
    }
}

fn authority_with(seats: &[u8]) -> TickAuthority<Dots> {
    let mut authority = TickAuthority::open(Dots::default(), config());
    for seat in seats {
        authority.join(*seat).unwrap();
    }
    authority.advance();
    authority
}

fn replica_config() -> TickReplicaConfig {
    TickReplicaConfig {
        prediction_horizon: 6,
        max_buffered_frames: 32,
        spectator_delay: 2,
    }
}

fn replica(authority: &TickAuthority<Dots>, local: Option<u8>) -> TickReplica<Dots> {
    TickReplica::from_snapshot(&authority.snapshot(), local, replica_config()).unwrap()
}

#[test]
fn authority_accepts_only_inputs_inside_the_window() {
    let mut authority = authority_with(&[0]);
    assert_eq!(authority.tick(), 1);
    assert_eq!(authority.submit(0, 1, 5), Ok(InputReceipt::Accepted));
    assert_eq!(authority.submit(0, 8, 5), Ok(InputReceipt::Accepted));
    assert_eq!(
        authority.submit(0, 9, 5),
        Err(InputRefusal::TooEarly { tick: 9, limit: 9 })
    );
    assert_eq!(
        authority.submit(0, 0, 5),
        Err(InputRefusal::Late {
            tick: 0,
            current: 1
        })
    );
    assert_eq!(authority.submit(7, 2, 5), Err(InputRefusal::NotSeated));
    authority.advance();
    // The window slides with the tick and never grows.
    assert_eq!(authority.submit(0, 9, 5), Ok(InputReceipt::Accepted));
    assert_eq!(
        authority.submit(0, 10, 5),
        Err(InputRefusal::TooEarly {
            tick: 10,
            limit: 10
        })
    );
    assert_eq!(
        authority.submit(0, 1, 5),
        Err(InputRefusal::Late {
            tick: 1,
            current: 2
        })
    );
}

#[test]
fn duplicate_inputs_are_idempotent_and_conflicts_refused() {
    let mut authority = authority_with(&[0]);
    assert_eq!(authority.submit(0, 3, 4), Ok(InputReceipt::Accepted));
    assert_eq!(authority.submit(0, 3, 4), Ok(InputReceipt::Duplicate));
    assert_eq!(authority.submit(0, 3, 9), Err(InputRefusal::Conflict));
    let results = authority.submit_batch(0, [(2, 1), (3, 4), (3, 6), (0, 1)]);
    assert_eq!(
        results,
        vec![
            Ok(InputReceipt::Accepted),
            Ok(InputReceipt::Duplicate),
            Err(InputRefusal::Conflict),
            Err(InputRefusal::Late {
                tick: 0,
                current: 1
            }),
        ]
    );
}

#[test]
fn missing_inputs_repeat_the_last_one_and_late_inputs_are_dropped() {
    let mut authority = authority_with(&[0, 1]);
    authority.submit(0, 1, 10).unwrap();
    let frame = authority.advance();
    assert_eq!(frame.tick, 1);
    // Seat 1 never sent anything: neutral input.
    assert_eq!(frame.inputs, vec![(0, 10), (1, 0)]);
    let frame = authority.advance();
    assert_eq!(frame.inputs, vec![(0, 10), (1, 0)]);
    // A late input for an already simulated tick changes nothing.
    assert!(matches!(
        authority.submit(1, 2, 50),
        Err(InputRefusal::Late { .. })
    ));
    let frame = authority.advance();
    assert_eq!(frame.inputs, vec![(0, 10), (1, 0)]);
    assert_eq!(authority.state().dots[&0], 6);
}

#[test]
fn membership_changes_apply_at_the_next_boundary() {
    let mut authority = TickAuthority::open(Dots::default(), config());
    assert_eq!(authority.join(9), Err(MembershipError::NotAdmitted));
    authority.join(0).unwrap();
    assert_eq!(authority.join(0), Err(MembershipError::AlreadySeated));
    assert_eq!(authority.leave(1), Err(MembershipError::NotSeated));
    // Inputs from a joining seat are accepted before the boundary.
    authority.submit(0, 0, 3).unwrap();
    assert!(authority.state().dots.is_empty());
    let frame = authority.advance();
    assert_eq!(frame.changes, vec![Membership::Join(0)]);
    assert_eq!(frame.inputs, vec![(0, 3)]);

    authority.submit(0, 3, 7).unwrap();
    authority.leave(0).unwrap();
    assert_eq!(authority.leave(0), Err(MembershipError::NotSeated));
    assert_eq!(authority.submit(0, 2, 1), Err(InputRefusal::NotSeated));
    // Rejoining at the same boundary resets to neutral, and the buffered
    // input from before leaving was discarded.
    authority.join(0).unwrap();
    let frame = authority.advance();
    assert_eq!(
        frame.changes,
        vec![Membership::Leave(0), Membership::Join(0)]
    );
    assert_eq!(frame.inputs, vec![(0, 0)]);
    assert_eq!(authority.advance().inputs, vec![(0, 0)]);
    assert_eq!(authority.advance().inputs, vec![(0, 0)]);
    assert_eq!(authority.seats().collect::<Vec<_>>(), vec![(0, 0)]);
}

#[test]
fn checksums_follow_the_interval_and_frames_are_retained_boundedly() {
    let mut authority = authority_with(&[0]);
    let frames: Vec<_> = (0..30).map(|_| authority.advance()).collect();
    for frame in &frames {
        assert_eq!(frame.checksum.is_some(), (frame.tick + 1) % 4 == 0);
    }
    assert_eq!(authority.tick(), 31);
    assert_eq!(authority.frames_from(31), Ok(Vec::new()));
    let tail = authority.frames_from(20).unwrap();
    assert_eq!(tail.len(), 11);
    assert_eq!(tail[0].tick, 20);
    assert_eq!(&tail[..], &frames[19..]);
    assert_eq!(
        authority.frames_from(5),
        Err(FramesUnavailable {
            requested: 5,
            oldest: 15
        })
    );
}

#[test]
fn zero_bounds_are_raised_to_one() {
    let mut authority = TickAuthority::open(
        Dots::default(),
        TickAuthorityConfig {
            input_window: 0,
            checksum_interval: 0,
            frame_history: 0,
        },
    );
    authority.join(0).unwrap();
    assert_eq!(authority.submit(0, 0, 1), Ok(InputReceipt::Accepted));
    assert!(matches!(
        authority.submit(0, 1, 1),
        Err(InputRefusal::TooEarly { .. })
    ));
    assert!(authority.advance().checksum.is_some());
    assert!(authority.frames_from(0).is_err());
}

#[test]
fn a_spectator_asks_for_frames_lost_behind_its_jitter_buffer() {
    let mut authority = authority_with(&[0]);
    let mut spectator = TickReplica::<Dots>::from_snapshot(
        &authority.snapshot(),
        None,
        TickReplicaConfig {
            spectator_delay: 6,
            ..replica_config()
        },
    )
    .unwrap();
    let frames: Vec<_> = (0..14).map(|_| authority.advance()).collect();
    // Frames 0..=5 arrive (the jitter buffer), 6..=8 are lost, 9.. arrive.
    for frame in frames[..6].iter().chain(&frames[9..]) {
        spectator.receive(frame.clone());
    }
    assert_eq!(
        spectator.advance(None).confirmed,
        0,
        "the buffer is still filling"
    );
    assert_eq!(
        spectator.request(),
        Some(ReplicaRequest::Frames {
            from: frames[6].tick
        }),
        "the hole is reported even though the confirmed tick is buffered"
    );
    for frame in authority.frames_from(frames[6].tick).unwrap() {
        spectator.receive(frame);
    }
    assert_eq!(spectator.request(), None);
    let mut confirmed = 0;
    for _ in 0..20 {
        confirmed += spectator.advance(None).confirmed;
    }
    assert!(
        confirmed >= 8,
        "presentation resumes once the hole is filled"
    );
}

#[test]
fn spectator_converges_from_a_snapshot_after_its_jitter_buffer() {
    let mut authority = authority_with(&[0, 1]);
    for tick in 1..10 {
        authority.submit(0, tick, 20).unwrap();
        authority.advance();
    }
    let mut spectator = replica(&authority, None);
    assert_eq!(
        spectator.confirmed_state().checksum(),
        authority.state().checksum()
    );
    let mut frames = Vec::new();
    for _ in 0..6 {
        frames.push(authority.advance());
    }
    spectator.receive(frames[0].clone());
    spectator.receive(frames[1].clone());
    // Two frames buffered equals the delay: nothing is presented yet.
    assert_eq!(spectator.advance(None).confirmed, 0);
    spectator.receive(frames[2].clone());
    assert_eq!(spectator.advance(None).confirmed, 1);
    for frame in &frames[3..] {
        spectator.receive(frame.clone());
    }
    // Five buffered (beyond twice the delay): catch up to three behind.
    assert_eq!(spectator.advance(None).confirmed, 2);
    assert_eq!(spectator.advance(None).confirmed, 1);
    assert_eq!(spectator.advance(None).confirmed, 0);
    assert_eq!(spectator.confirmed_tick(), frames[4].tick);
    assert_eq!(spectator.predicted_tick(), spectator.confirmed_tick());
    for frame in &frames[3..] {
        assert!(!spectator.receive(frame.clone()));
    }
    let mut zero_delay = TickReplica::<Dots>::from_snapshot(
        &authority.snapshot(),
        None,
        TickReplicaConfig {
            spectator_delay: 0,
            ..replica_config()
        },
    )
    .unwrap();
    let frames: Vec<_> = (0..3).map(|_| authority.advance()).collect();
    for frame in frames {
        zero_delay.receive(frame);
    }
    assert_eq!(zero_delay.advance(None).confirmed, 3);
    assert_eq!(
        zero_delay.presented().checksum(),
        authority.state().checksum()
    );
}

#[test]
fn local_input_is_presented_immediately_and_correct_predictions_do_not_roll_back() {
    let mut authority = authority_with(&[0, 1]);
    let mut player = replica(&authority, Some(0));
    let advance = player.advance(Some(10));
    assert!(advance.predicted);
    assert_eq!(player.predicted_tick(), 2);
    assert_eq!(player.presented().dots[&0], 2);
    assert_eq!(player.confirmed_state().dots[&0], 0);
    assert_eq!(
        player.unconfirmed_inputs().collect::<Vec<_>>(),
        vec![(1, 10)]
    );
    player.advance(None);
    assert_eq!(
        player.unconfirmed_inputs().collect::<Vec<_>>(),
        vec![(1, 10), (2, 10)]
    );
    for (tick, input) in player.unconfirmed_inputs() {
        authority.submit(0, tick, input).unwrap();
    }
    player.receive(authority.advance());
    player.receive(authority.advance());
    let advance = player.advance(Some(10));
    assert_eq!(advance.confirmed, 2);
    assert_eq!(advance.correction, None);
    assert_eq!(player.confirmed_tick(), 3);
    assert_eq!(
        player.confirmed_state().checksum(),
        authority.state().checksum()
    );
    assert_eq!(
        player.unconfirmed_inputs().collect::<Vec<_>>(),
        vec![(3, 10)]
    );
}

#[test]
fn remote_input_changes_roll_back_and_resimulate() {
    let mut authority = authority_with(&[0, 1]);
    let mut player = replica(&authority, Some(0));
    for _ in 0..4 {
        player.advance(Some(0));
    }
    let presented_remote = player.presented().dots[&1];
    assert_eq!(presented_remote, 0);
    // The remote seat moved from tick 1 onward.
    for tick in 1..5 {
        authority.submit(1, tick, 100).unwrap();
        authority.submit(0, tick, 0).unwrap();
    }
    player.receive(authority.advance());
    let advance = player.advance(Some(0));
    let correction = advance.correction.expect("misprediction rolls back");
    assert_eq!(correction.ticks_resimulated, 3);
    // Remote seat now predicted with its confirmed input for every tick.
    assert_eq!(correction.magnitude, 8);
    assert_eq!(player.presented().dots[&1], 10);
    for _ in 0..3 {
        player.receive(authority.advance());
    }
    let advance = player.advance(Some(0));
    assert_eq!(advance.correction, None);
    assert_eq!(player.confirmed_tick(), 5);
    assert_eq!(
        player.confirmed_state().checksum(),
        authority.state().checksum()
    );
}

#[test]
fn late_local_input_is_corrected_to_the_authority() {
    let mut authority = authority_with(&[0]);
    let mut player = replica(&authority, Some(0));
    player.advance(Some(50));
    // The input never arrives: the authority repeats neutral.
    let frame = authority.advance();
    assert_eq!(frame.inputs, vec![(0, 0)]);
    assert!(matches!(
        authority.submit(0, 1, 50),
        Err(InputRefusal::Late { .. })
    ));
    player.receive(frame);
    let advance = player.advance(Some(50));
    assert_eq!(
        advance
            .correction
            .map(|correction| correction.ticks_resimulated),
        Some(0)
    );
    assert_eq!(player.confirmed_state().dots[&0], 0);
    assert_eq!(player.presented().dots[&0], 2);
}

#[test]
fn prediction_stalls_at_the_horizon_and_requests_frames() {
    let authority = authority_with(&[0, 1]);
    let mut player = replica(&authority, Some(0));
    assert_eq!(player.request(), None);
    for _ in 0..6 {
        assert!(player.advance(Some(1)).predicted);
    }
    let stalled = player.advance(Some(1));
    assert_eq!(
        stalled,
        Advance {
            stalled: true,
            ..Advance::default()
        }
    );
    assert_eq!(player.predicted_tick(), 7);
    assert_eq!(player.request(), Some(ReplicaRequest::Frames { from: 1 }));
}

#[test]
fn reordered_duplicated_and_missing_frames_are_handled() {
    let mut authority = authority_with(&[0, 1]);
    let mut player = replica(&authority, Some(0));
    let frames: Vec<_> = (0..5).map(|_| authority.advance()).collect();
    assert!(player.receive(frames[2].clone()));
    assert!(!player.receive(frames[2].clone()));
    assert!(player.receive(frames[1].clone()));
    assert_eq!(player.request(), Some(ReplicaRequest::Frames { from: 1 }));
    assert_eq!(player.advance(Some(0)).confirmed, 0);
    let retransmitted = authority.frames_from(player.confirmed_tick()).unwrap();
    for frame in retransmitted {
        player.receive(frame);
    }
    assert_eq!(player.request(), None);
    let advance = player.advance(Some(0));
    assert_eq!(advance.confirmed, 5);
    assert_eq!(player.confirmed_tick(), 6);
    assert_eq!(
        player.confirmed_state().checksum(),
        authority.state().checksum()
    );
    assert!(!player.receive(frames[0].clone()));
}

#[test]
fn checksum_mismatch_is_detected_and_a_snapshot_resynchronizes() {
    let mut authority = authority_with(&[0, 1]);
    let mut player = replica(&authority, Some(0));
    let mut frames: Vec<TickFrame<u8, i32>> = (0..3).map(|_| authority.advance()).collect();
    assert_eq!(frames[2].tick, 3);
    assert!(frames[2].checksum.is_some());
    // Corrupt an input in transit: the state diverges silently until the
    // next checksum.
    frames[1].inputs[1].1 = 77;
    for frame in frames {
        player.receive(frame);
    }
    player.advance(Some(0));
    assert!(player.is_desynced());
    assert_eq!(player.confirmed_tick(), 4);
    assert_eq!(player.request(), Some(ReplicaRequest::Snapshot));
    let stalled = player.advance(Some(0));
    assert!(stalled.stalled && !stalled.predicted);

    let broken = super::TickSnapshot {
        state: vec![9],
        ..authority.snapshot()
    };
    assert!(matches!(
        player.adopt(&broken),
        Err(TickReplicaError::Decode("unknown version"))
    ));
    assert!(player.is_desynced());

    player.receive(authority.advance());
    player.adopt(&authority.snapshot()).unwrap();
    assert!(!player.is_desynced());
    assert_eq!(player.request(), None);
    assert_eq!(player.confirmed_tick(), authority.tick());
    for _ in 0..8 {
        player.receive(authority.advance());
        player.advance(Some(0));
    }
    assert!(!player.is_desynced());
    assert_eq!(
        player.confirmed_state().checksum(),
        authority.state().checksum()
    );
}

#[test]
fn frames_beyond_the_reorder_buffer_request_a_snapshot() {
    let mut authority = authority_with(&[0]);
    let mut spectator = replica(&authority, None);
    let frames: Vec<_> = (0..40).map(|_| authority.advance()).collect();
    assert!(!spectator.receive(frames[39].clone()));
    assert_eq!(spectator.request(), Some(ReplicaRequest::Snapshot));
    spectator.adopt(&authority.snapshot()).unwrap();
    assert_eq!(spectator.request(), None);
    assert_eq!(spectator.confirmed_tick(), 41);
}

#[test]
fn a_joining_player_predicts_once_its_seat_is_confirmed() {
    let mut authority = authority_with(&[0]);
    authority.join(1).unwrap();
    let mut joiner = replica(&authority, Some(1));
    assert_eq!(joiner.seats().count(), 1);
    joiner.advance(Some(30));
    // Not seated yet: the stamped input is kept but not predicted.
    assert!(!joiner.presented().dots.contains_key(&1));
    for (tick, input) in joiner.unconfirmed_inputs() {
        assert_eq!(authority.submit(1, tick, input), Ok(InputReceipt::Accepted));
    }
    joiner.receive(authority.advance());
    joiner.advance(Some(30));
    assert_eq!(joiner.seats().collect::<Vec<_>>(), vec![(0, 0), (1, 30)]);
    assert_eq!(joiner.confirmed_state().dots[&1], 2);
    assert_eq!(joiner.presented().dots[&1], 4);
}

#[test]
fn lead_covers_a_round_trip_and_jitter() {
    assert_eq!(lead_ticks(0, 0, 60), 1);
    assert_eq!(lead_ticks(100, 0, 60), 7);
    assert_eq!(lead_ticks(100, 40, 60), 10);
    assert!(lead_ticks(u64::MAX, u64::MAX, 60) >= u64::MAX / 1_000);
}
