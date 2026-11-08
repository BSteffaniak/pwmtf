//! Exercises authority and replica with a small non-billiards game, proving
//! the contract is game-agnostic.

use crate::{
    Applied, Authority, Authorization, CommandKey, Deadline, Playback, Received, Refusal, Remote,
    ReplayGame, Replica, RestoreError, Submission,
};

/// Two players take turns rolling a marble forward; the first past the goal
/// wins. A roll's motion lasts one tick per unit of distance.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Marble {
    position: u32,
    turn: u8,
    winner: Option<u8>,
    timed: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Move {
    Roll(u8),
    Pass,
    Timeout,
}

const GOAL: u32 = 30;
const TICKS_PER_SECOND: u32 = 240;

impl Marble {
    const fn new(timed: bool) -> Self {
        Self {
            position: 0,
            turn: 0,
            winner: None,
            timed,
        }
    }
}

struct Rolling {
    remaining: u32,
    accumulator: f64,
}

impl Playback for Rolling {
    fn advance(&mut self, seconds: f64) -> bool {
        self.accumulator += seconds;
        let step = 1.0 / f64::from(TICKS_PER_SECOND);
        while self.remaining > 0 && self.accumulator >= step {
            self.accumulator -= step;
            self.remaining -= 1;
        }
        self.remaining == 0
    }

    fn is_finished(&self) -> bool {
        self.remaining == 0
    }
}

impl std::fmt::Debug for Rolling {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Rolling({})", self.remaining)
    }
}

impl Clone for Rolling {
    fn clone(&self) -> Self {
        Self {
            remaining: self.remaining,
            accumulator: self.accumulator,
        }
    }
}

impl ReplayGame for Marble {
    type Seat = u8;
    type Command = Move;
    type Outcome = u32;
    type Error = &'static str;
    type Playback = Rolling;

    fn in_progress(&self) -> bool {
        self.winner.is_none()
    }

    fn active_seat(&self) -> Option<u8> {
        self.in_progress().then_some(self.turn)
    }

    fn authorize(&self, seat: u8, command: &Move) -> Result<(), Authorization> {
        match command {
            Move::Timeout => Err(Authorization::NotPermitted),
            _ if seat != self.turn => Err(Authorization::WrongSeat),
            _ => Ok(()),
        }
    }

    fn apply(&mut self, command: Move) -> Result<Applied<u32>, &'static str> {
        let distance = match command {
            Move::Roll(0) => return Err("a roll must move"),
            Move::Roll(distance) => u32::from(distance),
            Move::Pass | Move::Timeout => 0,
        };
        self.position += distance;
        if self.position >= GOAL {
            self.winner = Some(self.turn);
        } else {
            self.turn = 1 - self.turn;
        }
        Ok(Applied {
            outcome: self.position,
            playback_ticks: distance,
        })
    }

    fn checksum(&self) -> u64 {
        u64::from(self.position) << 16
            | u64::from(self.turn) << 8
            | u64::from(self.winner.map_or(0xff, |seat| seat))
    }

    fn turn_duration_millis(&self) -> Option<u64> {
        self.timed.then_some(10_000)
    }

    fn playback_millis(&self, ticks: u32) -> u64 {
        u64::from(ticks) * 1_000 / u64::from(TICKS_PER_SECOND)
    }

    fn timeout_command(&self) -> Move {
        Move::Timeout
    }

    fn predict_immediately(&self, command: &Move) -> bool {
        !matches!(command, Move::Roll(_))
    }

    fn start_playback(&self, command: &Move, ticks: Option<u32>) -> Option<Rolling> {
        let Move::Roll(distance) = command else {
            return None;
        };
        Some(Rolling {
            remaining: ticks.unwrap_or_else(|| u32::from(*distance)),
            accumulator: 0.0,
        })
    }
}

const fn key(byte: u8) -> CommandKey {
    CommandKey([byte; 16])
}

fn accept(
    authority: &mut Authority<Marble>,
    key: CommandKey,
    seat: u8,
    command: Move,
    now: u64,
) -> Remote<u8, Move> {
    let Submission::New(proposal) = authority
        .submit(key, seat, authority.revision(), command, now)
        .unwrap()
    else {
        panic!("expected a new command");
    };
    let remote = Remote {
        key,
        seat,
        command,
        playback_ticks: proposal.accepted.playback_ticks,
    };
    authority.commit(*proposal);
    remote
}

fn play_out(replica: &mut Replica<Marble>, from_ms: f64) -> f64 {
    let mut now = from_ms;
    replica.advance(now);
    while replica.playback().is_some() {
        now += 16.0;
        replica.advance(now);
    }
    now
}

#[test]
fn authority_schedules_deadlines_after_motion_and_only_when_timed() {
    let timed = Authority::open(Marble::new(true), 1_000);
    assert_eq!(
        timed.deadline(),
        Some(Deadline {
            revision: 0,
            seat: 0,
            due_at_ms: 11_000
        })
    );
    assert_eq!(Authority::open(Marble::new(false), 1_000).deadline(), None);
    let mut authority = timed;
    accept(&mut authority, key(1), 0, Move::Roll(24), 2_000);
    // 24 ticks of motion (100 ms) precede the next turn's ten seconds.
    assert_eq!(
        authority.deadline().unwrap().due_at_ms,
        2_000 + 100 + 10_000
    );
    assert_eq!(authority.deadline().unwrap().seat, 1);
}

#[test]
fn submit_changes_nothing_until_commit() {
    let authority = Authority::open(Marble::new(false), 0);
    let before = authority.state().clone();
    let submission = authority.submit(key(1), 0, 0, Move::Roll(5), 0).unwrap();
    assert!(matches!(submission, Submission::New(_)));
    assert_eq!(authority.state(), &before);
    assert_eq!(authority.revision(), 0);
}

#[test]
fn authority_refuses_stale_wrong_seat_illegal_and_conflicting_commands() {
    let mut authority = Authority::open(Marble::new(false), 0);
    assert!(matches!(
        authority.submit(key(1), 0, 3, Move::Roll(5), 0),
        Err(Refusal::StaleRevision {
            expected: 3,
            actual: 0
        })
    ));
    assert!(matches!(
        authority.submit(key(1), 1, 0, Move::Roll(5), 0),
        Err(Refusal::Unauthorized(Authorization::WrongSeat))
    ));
    assert!(matches!(
        authority.submit(key(1), 0, 0, Move::Timeout, 0),
        Err(Refusal::Unauthorized(Authorization::NotPermitted))
    ));
    assert!(matches!(
        authority.submit(key(1), 0, 0, Move::Roll(0), 0),
        Err(Refusal::Rejected("a roll must move"))
    ));
    accept(&mut authority, key(1), 0, Move::Roll(5), 0);
    // An identical retransmission is acknowledged again; a reused key is not.
    let Submission::Duplicate(again) = authority.submit(key(1), 0, 0, Move::Roll(5), 0).unwrap()
    else {
        panic!("expected a duplicate");
    };
    assert_eq!(again.revision, 1);
    assert!(matches!(
        authority.submit(key(1), 0, 1, Move::Roll(6), 0),
        Err(Refusal::KeyConflict)
    ));
    assert!(matches!(
        authority.submit(key(1), 0, 1, Move::Roll(5), 0),
        Err(Refusal::KeyConflict)
    ));
    accept(&mut authority, key(2), 1, Move::Roll(30), 0);
    assert!(matches!(
        authority.submit(key(3), 0, 2, Move::Pass, 0),
        Err(Refusal::Ended)
    ));
}

#[test]
fn timeouts_are_due_only_for_the_current_deadline() {
    let mut authority = Authority::open(Marble::new(true), 0);
    assert_eq!(authority.due_timeout(9_999), None);
    assert_eq!(authority.due_timeout(10_000), Some((0, Move::Timeout)));
    // The authority itself may apply the timeout for the seat that lapsed.
    let (seat, command) = authority.due_timeout(10_000).unwrap();
    let mut state = authority.state().clone();
    state.apply(command).unwrap();
    accept(&mut authority, key(9), seat, Move::Pass, 10_000);
    assert_eq!(authority.due_timeout(10_000), None);
}

#[test]
fn restore_validates_deadlines_and_keeps_idempotency() {
    let mut original = Authority::open(Marble::new(true), 0);
    accept(&mut original, key(1), 0, Move::Roll(3), 0);
    let accepted = original.accepted(&key(1)).unwrap().clone();
    let restored = Authority::restore(
        original.state().clone(),
        1,
        original.deadline(),
        [(key(1), accepted)],
    )
    .unwrap();
    assert!(matches!(
        restored.submit(key(1), 0, 0, Move::Roll(3), 0).unwrap(),
        Submission::Duplicate(_)
    ));
    let mut wrong = original.deadline().unwrap();
    wrong.seat = 0;
    assert_eq!(
        Authority::restore(original.state().clone(), 1, Some(wrong), []).unwrap_err(),
        RestoreError::InvalidDeadline
    );
}

#[test]
fn shooter_plays_at_once_defers_resolution_and_keeps_playback_on_confirmation() {
    let mut authority = Authority::open(Marble::new(false), 0);
    let mut shooter = Replica::new(0, authority.state().clone());
    assert_eq!(shooter.submit(key(1), Move::Roll(12)).unwrap(), 0);
    assert!(
        shooter.has_unresolved(),
        "rolls are resolved after playback"
    );
    assert!(shooter.playback().is_some(), "motion starts at submit");
    let remote = accept(&mut authority, key(1), 0, Move::Roll(12), 0);
    let received = shooter.receive(1, authority.state().clone(), Some(remote));
    assert_eq!(
        received,
        Received {
            revision_changed: true,
            superseded: false,
            resend: None
        }
    );
    assert!(shooter.pending().is_none());
    assert!(
        shooter.playback().is_some(),
        "confirmation keeps the running playback"
    );
    play_out(&mut shooter, 0.0);
    let transition = shooter.take_settled().unwrap();
    assert_eq!(
        (transition.revision, transition.command),
        (1, Move::Roll(12))
    );
    assert_eq!(transition.before, Marble::new(false));
    assert_eq!(shooter.predicted(), authority.state());
}

#[test]
fn unanswered_commands_resolve_locally_once_playback_ends() {
    let mut shooter = Replica::new(0, Marble::new(false));
    shooter.submit(key(1), Move::Roll(6)).unwrap();
    play_out(&mut shooter, 0.0);
    assert!(!shooter.has_unresolved());
    assert_eq!(shooter.predicted().position, 6);
    assert_eq!(shooter.authoritative().position, 0);
}

#[test]
fn observers_replay_only_from_the_immediately_preceding_revision() {
    let mut authority = Authority::open(Marble::new(false), 0);
    let mut observer = Replica::new(0, authority.state().clone());
    let first = accept(&mut authority, key(1), 0, Move::Roll(8), 0);
    observer.receive(1, authority.state().clone(), Some(first));
    assert!(observer.playback().is_some());
    assert!(
        observer.take_settled().is_none(),
        "reported once motion ends"
    );
    play_out(&mut observer, 0.0);
    assert_eq!(observer.take_settled().unwrap().revision, 1);
    // Skipping a revision snaps to authority without replaying stale motion.
    accept(&mut authority, key(2), 1, Move::Roll(4), 0);
    let third = accept(&mut authority, key(3), 0, Move::Roll(4), 0);
    observer.receive(3, authority.state().clone(), Some(third));
    assert!(observer.playback().is_none());
    assert!(observer.take_settled().is_none());
    assert_eq!(observer.authoritative(), authority.state());
}

#[test]
fn a_pending_command_is_superseded_when_authority_moves_on_without_it() {
    let mut authority = Authority::open(Marble::new(true), 0);
    let mut shooter = Replica::new(0, authority.state().clone());
    shooter.submit(key(1), Move::Roll(5)).unwrap();
    // The turn timed out before the roll arrived.
    let (seat, _) = authority.due_timeout(10_000).unwrap();
    let Submission::New(proposal) = authority
        .submit(key(9), seat, 0, Move::Pass, 10_000)
        .unwrap()
    else {
        panic!("expected a new command");
    };
    authority.commit(*proposal);
    let received = shooter.receive(
        1,
        authority.state().clone(),
        Some(Remote {
            key: key(9),
            seat,
            command: Move::Pass,
            playback_ticks: 0,
        }),
    );
    assert!(received.superseded);
    assert!(shooter.pending().is_none() && shooter.playback().is_none());
    assert!(shooter.advance(0.0), "presentation is told to snap");
}

#[test]
fn reconnect_resends_the_same_command_once_the_revision_proves_it_unapplied() {
    let mut shooter = Replica::new(4, Marble::new(false));
    shooter.submit(key(7), Move::Pass).unwrap();
    shooter.request_resend();
    let received = shooter.receive(4, Marble::new(false), None);
    let resend = received.resend.unwrap();
    assert_eq!(
        (resend.key, resend.command, resend.expected_revision),
        (key(7), Move::Pass, 4)
    );
    // Only once.
    assert!(
        shooter
            .receive(4, Marble::new(false), None)
            .resend
            .is_none()
    );
}

#[test]
fn a_slow_frame_delays_motion_instead_of_skipping_it() {
    let mut shooter = Replica::new(0, Marble::new(false));
    shooter.submit(key(1), Move::Roll(200)).unwrap();
    shooter.advance(0.0);
    shooter.advance(5_000.0);
    let remaining = shooter.playback().unwrap().remaining;
    // At most one capped step (50 ms = 12 ticks) elapsed.
    assert!(remaining >= 200 - 12);
}

#[test]
fn authority_and_replicas_converge_over_a_full_game() {
    let mut authority = Authority::open(Marble::new(false), 0);
    let mut players = [
        Replica::new(0, authority.state().clone()),
        Replica::new(0, authority.state().clone()),
    ];
    let mut now = 0.0;
    for (turn, distance) in [7_u8, 9, 4, 11].into_iter().enumerate() {
        let seat = u8::try_from(turn % 2).unwrap();
        let command = Move::Roll(distance);
        let id = key(u8::try_from(turn).unwrap() + 1);
        players[usize::from(seat)].submit(id, command).unwrap();
        let remote = accept(&mut authority, id, seat, command, 0);
        for player in &mut players {
            player.receive(
                authority.revision(),
                authority.state().clone(),
                Some(remote),
            );
        }
        for player in &mut players {
            now = play_out(player, now);
            assert_eq!(player.predicted().checksum(), authority.state().checksum());
        }
    }
    assert_eq!(authority.state().winner, Some(1));
}
