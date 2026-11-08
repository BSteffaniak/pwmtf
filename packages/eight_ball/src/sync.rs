//! Synchronizes 8-ball matches with [`pwmtf_sync`].

use pwmtf_billiards::playback::ShotPlayback;
use pwmtf_sync::{Applied, Authorization, ReplayGame};

use crate::{
    MatchCommand, MatchCommandResult, MatchError, MatchState, MatchStatus, Player,
    VersionedMatchCommand,
};

impl ReplayGame for MatchState {
    type Seat = Player;
    type Command = VersionedMatchCommand;
    type Outcome = MatchCommandResult;
    type Error = MatchError;
    type Playback = ShotPlayback;

    fn in_progress(&self) -> bool {
        self.status() == MatchStatus::InProgress
    }

    fn active_seat(&self) -> Option<Player> {
        self.in_progress().then(|| self.active_player())
    }

    /// Shots and placements belong to the active player and a concession to
    /// the conceding player. Timeouts are accepted from either seat here;
    /// hosts must never forward a timeout from a client.
    fn authorize(
        &self,
        seat: Player,
        command: &VersionedMatchCommand,
    ) -> Result<(), Authorization> {
        match command.command {
            MatchCommand::PlayShot { .. } | MatchCommand::PlaceCueBall { .. }
                if seat != self.active_player() =>
            {
                Err(Authorization::WrongSeat)
            }
            MatchCommand::Concede { player } if player != seat => Err(Authorization::WrongSeat),
            _ => Ok(()),
        }
    }

    fn apply(
        &mut self,
        command: VersionedMatchCommand,
    ) -> Result<Applied<MatchCommandResult>, MatchError> {
        let applied = self.apply_command_timed(command)?;
        Ok(Applied {
            outcome: applied.result,
            playback_ticks: applied.playback_ticks,
        })
    }

    fn checksum(&self) -> u64 {
        Self::checksum(self)
    }

    fn turn_duration_millis(&self) -> Option<u64> {
        self.rules().turn_duration_millis()
    }

    fn playback_millis(&self, ticks: u32) -> u64 {
        self.physics().playback_millis(ticks)
    }

    fn timeout_command(&self) -> VersionedMatchCommand {
        VersionedMatchCommand::new(MatchCommand::Timeout)
    }

    /// Shots are simulated in full only after their playback, unless the
    /// authority answers first; everything else resolves at once.
    fn predict_immediately(&self, command: &VersionedMatchCommand) -> bool {
        !matches!(command.command, MatchCommand::PlayShot { .. })
    }

    fn start_playback(
        &self,
        command: &VersionedMatchCommand,
        ticks: Option<u32>,
    ) -> Option<ShotPlayback> {
        let MatchCommand::PlayShot { shot, .. } = command.command else {
            return None;
        };
        ShotPlayback::start(
            self.physics(),
            self.geometry(),
            self.table().clone(),
            shot,
            ticks,
        )
        .ok()
    }
}

#[cfg(test)]
mod tests {
    use pwmtf_billiards::{
        Aim, PhysicsProfile, RackSeed, ShotPower, Spin, TableGeometry, VersionedShotCommand,
    };
    use pwmtf_sync::{Authority, CommandKey, Remote, Replica, Submission};

    use super::*;
    use crate::RulesProfile;

    fn game(rules: RulesProfile) -> MatchState {
        MatchState::new(
            rules,
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap()
    }

    fn accept(
        authority: &mut Authority<MatchState>,
        key: CommandKey,
        seat: Player,
        command: VersionedMatchCommand,
        now: u64,
    ) -> Remote<Player, VersionedMatchCommand> {
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

    fn break_shot() -> VersionedMatchCommand {
        VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(9_000).unwrap(),
                Spin::CENTER,
            ),
            called_pocket: None,
        })
    }

    #[test]
    fn shooter_and_observer_replay_a_break_onto_the_authoritative_table() {
        let mut authority = Authority::open(game(RulesProfile::standard()), 0);
        let mut shooter = Replica::new(0, authority.state().clone());
        let mut observer = Replica::new(0, authority.state().clone());
        let key = CommandKey([1; 16]);
        shooter.submit(key, break_shot()).unwrap();
        assert!(
            shooter.has_unresolved(),
            "the break is simulated after playback"
        );
        let remote = accept(&mut authority, key, Player::One, break_shot(), 1_000);
        assert!(remote.playback_ticks > 0);
        for replica in [&mut shooter, &mut observer] {
            replica.receive(1, authority.state().clone(), Some(remote));
            let mut now = 0.0;
            replica.advance(now);
            while replica.playback().is_some() {
                now += 16.0;
                replica.advance(now);
            }
            assert_eq!(replica.predicted(), authority.state());
            assert_eq!(
                replica.take_settled().unwrap().before,
                game(RulesProfile::standard())
            );
        }
        // The next turn's deadline starts once the break has played back.
        let deadline = authority.deadline().unwrap();
        let physics = PhysicsProfile::standard();
        assert_eq!(
            deadline.due_at_ms,
            1_000 + physics.playback_millis(remote.playback_ticks) + 30_000
        );
    }

    #[test]
    fn timed_turns_expire_and_house_rules_are_untimed() {
        let mut authority = Authority::open(game(RulesProfile::standard()), 0);
        let (seat, command) = authority.due_timeout(30_000).unwrap();
        assert_eq!(seat, Player::One);
        accept(&mut authority, CommandKey([9; 16]), seat, command, 30_000);
        assert_eq!(authority.state().active_player(), Player::Two);
        let lounge = Authority::open(game(RulesProfile::lounge()), 0);
        assert_eq!(lounge.deadline(), None);
        assert_eq!(lounge.due_timeout(u64::MAX), None);
    }

    #[test]
    fn seats_may_only_act_on_their_own_turn_and_concede_for_themselves() {
        let state = game(RulesProfile::standard());
        assert_eq!(
            state.authorize(Player::Two, &break_shot()),
            Err(Authorization::WrongSeat)
        );
        let concede = VersionedMatchCommand::new(MatchCommand::Concede {
            player: Player::One,
        });
        assert_eq!(
            state.authorize(Player::Two, &concede),
            Err(Authorization::WrongSeat)
        );
        assert_eq!(state.authorize(Player::One, &concede), Ok(()));
    }
}
