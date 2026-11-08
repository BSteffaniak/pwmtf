#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Versioned authoritative 8-ball match transitions on `pwmtf_billiards`
//! physics: groups, fouls, called 8, timeouts, concessions and rematches.

use pwmtf_billiards::{
    BallId, BallState, PhysicsProfile, PocketId, RackError, RackSeed, SimulationError,
    SimulationEventKind, TableGeometry, Vector, VersionedShotCommand, VersionedTableState,
    simulate_shot, standard_rack,
};
use thiserror::Error;

/// Current canonical match-configuration version.
pub const MATCH_CONFIGURATION_VERSION: u16 = 1;
/// Current canonical match-command version.
pub const MATCH_COMMAND_VERSION: u16 = 1;
/// Current canonical match-result version.
pub const MATCH_COMMAND_RESULT_VERSION: u16 = 1;
/// Current canonical match snapshot version.
pub const MATCH_STATE_VERSION: u16 = 1;
/// Current launch rules profile version.
pub const RULES_PROFILE_VERSION: u16 = 1;
/// Casual house-rules profile version (Cozy Lounge).
///
/// An 8-ball pocketed on the break is re-spotted, a break that pockets any
/// object ball without a foul keeps the turn, shooting at the 8 requires a
/// called pocket, a scratched cue ball is re-spotted with adjustable
/// ball-in-hand, and there is no turn timer.
pub const LOUNGE_RULES_PROFILE_VERSION: u16 = 2;
const TURN_DURATION_MILLIS_V1: u64 = 30_000;

/// Immutable versioned 8-ball rules configuration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RulesProfile {
    version: u16,
}

impl RulesProfile {
    /// Creates a supported rules profile.
    ///
    /// # Errors
    ///
    /// Returns [`RulesProfileError::UnsupportedVersion`] for unknown versions.
    pub const fn new(version: u16) -> Result<Self, RulesProfileError> {
        if version == RULES_PROFILE_VERSION || version == LOUNGE_RULES_PROFILE_VERSION {
            Ok(Self { version })
        } else {
            Err(RulesProfileError::UnsupportedVersion(version))
        }
    }

    /// Returns the casual house-rules profile.
    #[must_use]
    pub const fn lounge() -> Self {
        Self {
            version: LOUNGE_RULES_PROFILE_VERSION,
        }
    }

    const fn is_lounge(self) -> bool {
        self.version == LOUNGE_RULES_PROFILE_VERSION
    }

    /// Whether a shot at the 8-ball is refused without a called pocket,
    /// rather than resolving as a loss.
    #[must_use]
    pub const fn requires_eight_call(self) -> bool {
        self.is_lounge()
    }

    /// Whether ball-in-hand stays active after a placement, so the shooter
    /// can adjust the cue ball until shooting.
    #[must_use]
    pub const fn ball_in_hand_adjustable(self) -> bool {
        self.is_lounge()
    }

    /// Returns the launch rules profile.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            version: RULES_PROFILE_VERSION,
        }
    }

    /// Returns the pinned profile version.
    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    /// Returns this profile's authoritative turn duration in milliseconds, or
    /// `None` when turns are untimed.
    #[must_use]
    pub const fn turn_duration_millis(self) -> Option<u64> {
        match self.version {
            RULES_PROFILE_VERSION => Some(TURN_DURATION_MILLIS_V1),
            _ => None,
        }
    }
}

/// Unsupported rules profile.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RulesProfileError {
    /// The supplied profile version is unknown.
    #[error("unsupported rules profile version {0}")]
    UnsupportedVersion(u16),
}

/// Stable participant seat in a two-player match.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Player {
    /// First participant.
    One,
    /// Second participant.
    Two,
}

impl Player {
    /// Returns the other participant.
    #[must_use]
    pub const fn opponent(self) -> Self {
        match self {
            Self::One => Self::Two,
            Self::Two => Self::One,
        }
    }
}

/// Assigned object-ball group.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Group {
    /// Balls one through seven.
    Solids,
    /// Balls nine through fifteen.
    Stripes,
}

impl Group {
    /// Returns whether `ball` belongs to this group.
    #[must_use]
    pub const fn contains(self, ball: BallId) -> bool {
        match self {
            Self::Solids => ball.number() >= 1 && ball.number() <= 7,
            Self::Stripes => ball.number() >= 9 && ball.number() <= 15,
        }
    }

    const fn opponent(self) -> Self {
        match self {
            Self::Solids => Self::Stripes,
            Self::Stripes => Self::Solids,
        }
    }
}

/// What a player must pocket next.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Objective {
    /// Groups are not assigned yet; any object ball except the 8 counts.
    OpenTable,
    /// The player's assigned group still has balls on the table.
    Group(Group),
    /// The player's group is cleared: the 8-ball in a called pocket wins.
    EightBall,
}

/// Terminal match reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionReason {
    /// The winner legally pocketed the 8-ball.
    LegalEightBall,
    /// The loser pocketed the 8-ball illegally.
    IllegalEightBall,
    /// The loser explicitly conceded.
    Concession,
}

/// Terminal match result.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatchOutcome {
    /// Winning participant.
    pub winner: Player,
    /// Canonical completion reason.
    pub reason: CompletionReason,
}

/// Current match lifecycle.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchStatus {
    /// Match accepts turns.
    InProgress,
    /// Match has a terminal result.
    Completed(MatchOutcome),
}

/// Canonical foul determined from a settled shot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Foul {
    /// Cue ball was pocketed.
    Scratch,
    /// Cue ball contacted no object ball.
    NoObjectContact,
    /// First object contact did not satisfy the shooter's target.
    WrongFirstContact,
    /// No ball was pocketed and no ball reached a cushion after object contact.
    NoRailAfterContact,
}

/// Result of one accepted canonical shot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShotResolution {
    /// Shooter whose command was resolved.
    pub shooter: Player,
    /// Fouls, in canonical priority order.
    pub fouls: Vec<Foul>,
    /// Whether this shot assigned groups.
    pub assigned_group: Option<Group>,
    /// Whether an 8-ball break caused a new rack.
    pub reracked: bool,
    /// Match status after resolution.
    pub status: MatchStatus,
    /// Active player after resolution when still in progress.
    pub next_player: Option<Player>,
    /// Whether the active player has ball-in-hand.
    pub ball_in_hand: bool,
}

/// Rematch linkage and breaker-selection metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RematchMetadata {
    /// Stable identifier of the completed match being rematched.
    pub previous_match_id: u128,
    /// Breaker selected by alternating the previous match's breaker.
    pub breaker: Player,
}

impl RematchMetadata {
    /// Creates metadata for a new match linked to a completed match.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::NotCompleted`] while the previous match is active.
    pub const fn from_completed(
        previous_match_id: u128,
        previous: &MatchState,
    ) -> Result<Self, MatchError> {
        if matches!(previous.status, MatchStatus::Completed(_)) {
            Ok(Self {
                previous_match_id,
                breaker: previous.breaker.opponent(),
            })
        } else {
            Err(MatchError::NotCompleted)
        }
    }
}

/// Versioned canonical command applied to a match aggregate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchCommand {
    /// Simulate and resolve the active player's shot.
    PlayShot {
        /// Bounded canonical physics command.
        shot: VersionedShotCommand,
        /// Called 8-ball pocket, if any.
        called_pocket: Option<PocketId>,
    },
    /// Apply an authoritative turn timeout.
    Timeout,
    /// Explicitly concede the match.
    Concede { player: Player },
    /// Place the cue ball during ball-in-hand.
    PlaceCueBall { position: Vector },
}

/// Explicitly versioned match command envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionedMatchCommand {
    /// Command schema version.
    pub version: u16,
    /// Canonical command payload.
    pub command: MatchCommand,
}

impl VersionedMatchCommand {
    /// Creates a current-version command envelope.
    #[must_use]
    pub const fn new(command: MatchCommand) -> Self {
        Self {
            version: MATCH_COMMAND_VERSION,
            command,
        }
    }
}

/// Authoritative command result plus its deterministic shot playback length.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppliedCommand {
    /// Canonical command result, unchanged from [`MatchState::apply_command`].
    pub result: MatchCommandResult,
    /// Fixed physics ticks the shot simulation ran; zero for non-shot commands
    /// and zero-power shots.
    pub playback_ticks: u32,
}

/// Result emitted by replaying a match command.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MatchCommandResult {
    /// A shot settled and rules were resolved.
    Shot(ShotResolution),
    /// A timeout selected the next player.
    Timeout { active_player: Player },
    /// A concession completed the match.
    Conceded(MatchOutcome),
    /// Cue-ball placement completed.
    CueBallPlaced,
}

impl MatchCommandResult {
    /// Encodes the result into the stable version-one binary format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MATCH_COMMAND_RESULT_VERSION.to_be_bytes());
        match self {
            Self::Shot(resolution) => {
                bytes.push(1);
                bytes.push(encode_player(resolution.shooter));
                bytes.push(u8::try_from(resolution.fouls.len()).unwrap_or(u8::MAX));
                for foul in &resolution.fouls {
                    bytes.push(match foul {
                        Foul::Scratch => 1,
                        Foul::NoObjectContact => 2,
                        Foul::WrongFirstContact => 3,
                        Foul::NoRailAfterContact => 4,
                    });
                }
                bytes.push(encode_group(resolution.assigned_group));
                bytes.push(u8::from(resolution.reracked));
                encode_status(resolution.status, &mut bytes);
                bytes.push(resolution.next_player.map_or(0, encode_player));
                bytes.push(u8::from(resolution.ball_in_hand));
            }
            Self::Timeout { active_player } => {
                bytes.push(2);
                bytes.push(encode_player(*active_player));
            }
            Self::Conceded(outcome) => {
                bytes.push(3);
                encode_status(MatchStatus::Completed(*outcome), &mut bytes);
            }
            Self::CueBallPlaced => bytes.push(4),
        }
        bytes
    }

    /// Decodes and validates a stable canonical command result.
    ///
    /// # Errors
    ///
    /// Returns [`MatchResultDecodeError`] for unsupported, truncated, trailing,
    /// invalid, or internally inconsistent payloads.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MatchResultDecodeError> {
        let mut reader = ResultReader::new(bytes);
        let version = reader.u16()?;
        if version != MATCH_COMMAND_RESULT_VERSION {
            return Err(MatchResultDecodeError::UnsupportedVersion(version));
        }
        let result = match reader.u8()? {
            1 => {
                let shooter = decode_result_player(reader.u8()?)?;
                let foul_count = usize::from(reader.u8()?);
                if foul_count > 4 {
                    return Err(MatchResultDecodeError::Inconsistent);
                }
                let mut fouls = Vec::with_capacity(foul_count);
                for _ in 0..foul_count {
                    let foul = match reader.u8()? {
                        1 => Foul::Scratch,
                        2 => Foul::NoObjectContact,
                        3 => Foul::WrongFirstContact,
                        4 => Foul::NoRailAfterContact,
                        value => return Err(MatchResultDecodeError::InvalidDiscriminant(value)),
                    };
                    if fouls.contains(&foul) {
                        return Err(MatchResultDecodeError::Inconsistent);
                    }
                    fouls.push(foul);
                }
                let assigned_group = decode_result_group(reader.u8()?)?;
                let reracked = decode_result_bool(reader.u8()?)?;
                let status = decode_result_status(&mut reader)?;
                let next_player = match reader.u8()? {
                    0 => None,
                    value => Some(decode_result_player(value)?),
                };
                let ball_in_hand = decode_result_bool(reader.u8()?)?;
                if matches!(status, MatchStatus::Completed(_)) != next_player.is_none()
                    || matches!(status, MatchStatus::Completed(_)) && ball_in_hand
                {
                    return Err(MatchResultDecodeError::Inconsistent);
                }
                Self::Shot(ShotResolution {
                    shooter,
                    fouls,
                    assigned_group,
                    reracked,
                    status,
                    next_player,
                    ball_in_hand,
                })
            }
            2 => Self::Timeout {
                active_player: decode_result_player(reader.u8()?)?,
            },
            3 => match decode_result_status(&mut reader)? {
                MatchStatus::Completed(outcome) => Self::Conceded(outcome),
                MatchStatus::InProgress => return Err(MatchResultDecodeError::Inconsistent),
            },
            4 => Self::CueBallPlaced,
            value => return Err(MatchResultDecodeError::InvalidDiscriminant(value)),
        };
        if !reader.finished() {
            return Err(MatchResultDecodeError::TrailingBytes(reader.remaining()));
        }
        Ok(result)
    }
}

/// Failure to decode a versioned canonical command result.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MatchResultDecodeError {
    /// Result payload version is unknown.
    #[error("unsupported match result version {0}")]
    UnsupportedVersion(u16),
    /// Result ended before a complete field was available.
    #[error("match result payload is truncated")]
    Truncated,
    /// Result contains trailing bytes.
    #[error("match result payload has {0} trailing bytes")]
    TrailingBytes(usize),
    /// Result contains an unknown discriminant.
    #[error("match result payload has invalid discriminant {0}")]
    InvalidDiscriminant(u8),
    /// Decoded fields form an impossible result.
    #[error("match result payload is internally inconsistent")]
    Inconsistent,
}

struct ResultReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ResultReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn u8(&mut self) -> Result<u8, MatchResultDecodeError> {
        let value = *self
            .bytes
            .get(self.offset)
            .ok_or(MatchResultDecodeError::Truncated)?;
        self.offset += 1;
        Ok(value)
    }

    fn u16(&mut self) -> Result<u16, MatchResultDecodeError> {
        let end = self
            .offset
            .checked_add(2)
            .ok_or(MatchResultDecodeError::Truncated)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(MatchResultDecodeError::Truncated)?;
        self.offset = end;
        Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
    }

    const fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }

    const fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.offset)
    }
}

const fn decode_result_player(value: u8) -> Result<Player, MatchResultDecodeError> {
    match value {
        1 => Ok(Player::One),
        2 => Ok(Player::Two),
        _ => Err(MatchResultDecodeError::InvalidDiscriminant(value)),
    }
}

const fn decode_result_group(value: u8) -> Result<Option<Group>, MatchResultDecodeError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(Group::Solids)),
        2 => Ok(Some(Group::Stripes)),
        _ => Err(MatchResultDecodeError::InvalidDiscriminant(value)),
    }
}

const fn decode_result_bool(value: u8) -> Result<bool, MatchResultDecodeError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(MatchResultDecodeError::InvalidDiscriminant(value)),
    }
}

fn decode_result_status(
    reader: &mut ResultReader<'_>,
) -> Result<MatchStatus, MatchResultDecodeError> {
    match reader.u8()? {
        0 => Ok(MatchStatus::InProgress),
        1 => {
            let winner = decode_result_player(reader.u8()?)?;
            let reason = match reader.u8()? {
                1 => CompletionReason::LegalEightBall,
                2 => CompletionReason::IllegalEightBall,
                3 => CompletionReason::Concession,
                value => return Err(MatchResultDecodeError::InvalidDiscriminant(value)),
            };
            Ok(MatchStatus::Completed(MatchOutcome { winner, reason }))
        }
        value => Err(MatchResultDecodeError::InvalidDiscriminant(value)),
    }
}

/// Immutable canonical configuration pinned for one match.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatchConfiguration {
    rules: RulesProfile,
    physics: PhysicsProfile,
    geometry: TableGeometry,
    rack_seed: RackSeed,
    breaker: Player,
}

impl MatchConfiguration {
    /// Encodes the pinned configuration into its stable version-one format.
    #[must_use]
    pub fn to_bytes(self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(80);
        bytes.extend_from_slice(&MATCH_CONFIGURATION_VERSION.to_be_bytes());
        bytes.extend_from_slice(&self.rules.version().to_be_bytes());
        encode_physics(self.physics, &mut bytes);
        encode_geometry(self.geometry, &mut bytes);
        bytes.extend_from_slice(&self.rack_seed.value().to_be_bytes());
        bytes.push(encode_player(self.breaker));
        bytes
    }

    /// Decodes and validates one immutable match configuration.
    ///
    /// # Errors
    ///
    /// Returns [`MatchDecodeError`] for malformed, unsupported, or trailing data.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MatchDecodeError> {
        let mut reader = MatchReader::new(bytes);
        let version = reader.u16()?;
        if version != MATCH_CONFIGURATION_VERSION {
            return Err(MatchDecodeError::UnsupportedConfigurationVersion(version));
        }
        let configuration = Self {
            rules: RulesProfile::new(reader.u16()?)?,
            physics: decode_physics(&mut reader)?,
            geometry: decode_geometry(&mut reader)?,
            rack_seed: RackSeed::new(reader.u64()?),
            breaker: decode_player(reader.u8()?)?,
        };
        if !reader.finished() {
            return Err(MatchDecodeError::TrailingBytes(reader.remaining()));
        }
        Ok(configuration)
    }

    /// Returns the pinned rules profile.
    #[must_use]
    pub const fn rules(self) -> RulesProfile {
        self.rules
    }

    /// Returns the pinned physics profile.
    #[must_use]
    pub const fn physics(self) -> PhysicsProfile {
        self.physics
    }

    /// Returns the pinned table geometry.
    #[must_use]
    pub const fn geometry(self) -> TableGeometry {
        self.geometry
    }

    /// Returns the pinned rack seed.
    #[must_use]
    pub const fn rack_seed(self) -> RackSeed {
        self.rack_seed
    }

    /// Returns the canonically selected initial breaker.
    #[must_use]
    pub const fn breaker(self) -> Player {
        self.breaker
    }
}

/// Complete canonical 8-ball aggregate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchState {
    rules: RulesProfile,
    physics: PhysicsProfile,
    geometry: TableGeometry,
    rack_seed: RackSeed,
    rerack_count: u32,
    table: VersionedTableState,
    active_player: Player,
    breaker: Player,
    break_pending: bool,
    player_one_group: Option<Group>,
    ball_in_hand: bool,
    status: MatchStatus,
}

impl MatchState {
    /// Creates a match with a deterministic standard rack.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::Rack`] when the pinned geometry cannot produce a
    /// valid standard rack.
    pub fn new(
        rules: RulesProfile,
        physics: PhysicsProfile,
        geometry: TableGeometry,
        rack_seed: RackSeed,
        breaker: Player,
    ) -> Result<Self, MatchError> {
        let table = standard_rack(geometry, rack_seed)?;
        Ok(Self {
            rules,
            physics,
            geometry,
            rack_seed,
            rerack_count: 0,
            table,
            active_player: breaker,
            breaker,
            break_pending: true,
            player_one_group: None,
            ball_in_hand: false,
            status: MatchStatus::InProgress,
        })
    }

    /// Returns the complete immutable configuration pinned for this match.
    #[must_use]
    pub const fn configuration(&self) -> MatchConfiguration {
        MatchConfiguration {
            rules: self.rules,
            physics: self.physics,
            geometry: self.geometry,
            rack_seed: self.rack_seed,
            breaker: self.breaker,
        }
    }

    /// Returns the pinned rules profile.
    #[must_use]
    pub const fn rules(&self) -> RulesProfile {
        self.rules
    }

    /// Returns the pinned physics profile.
    #[must_use]
    pub const fn physics(&self) -> PhysicsProfile {
        self.physics
    }

    /// Returns the pinned table geometry.
    #[must_use]
    pub const fn geometry(&self) -> TableGeometry {
        self.geometry
    }

    /// Returns the pinned rack seed.
    #[must_use]
    pub const fn rack_seed(&self) -> RackSeed {
        self.rack_seed
    }

    /// Returns the canonically selected breaker.
    #[must_use]
    pub const fn breaker(&self) -> Player {
        self.breaker
    }

    /// Returns the canonical table state.
    #[must_use]
    pub const fn table(&self) -> &VersionedTableState {
        &self.table
    }

    /// Returns the active participant.
    #[must_use]
    pub const fn active_player(&self) -> Player {
        self.active_player
    }

    /// Returns the current lifecycle status.
    #[must_use]
    pub const fn status(&self) -> MatchStatus {
        self.status
    }

    /// Returns whether the active player has ball-in-hand.
    #[must_use]
    pub const fn ball_in_hand(&self) -> bool {
        self.ball_in_hand
    }

    /// Places the cue ball during an authoritative ball-in-hand phase.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::Completed`] after completion,
    /// [`MatchError::BallInHandUnavailable`] outside a ball-in-hand phase, or
    /// [`MatchError::InvalidCuePlacement`] when the cue ball would be outside
    /// playable bounds, inside a pocket, or overlap an in-play object ball.
    pub fn place_cue_ball(&mut self, position: Vector) -> Result<(), MatchError> {
        if !matches!(self.status, MatchStatus::InProgress) {
            return Err(MatchError::Completed);
        }
        if !self.ball_in_hand {
            return Err(MatchError::BallInHandUnavailable);
        }
        let mut balls = self.table.balls().to_vec();
        let cue_index = balls
            .binary_search_by_key(&BallId::CUE, |ball| ball.id)
            .map_err(|_| MatchError::InvalidCuePlacement)?;
        balls[cue_index] = BallState::stationary(BallId::CUE, position);
        self.table = VersionedTableState::new(
            self.table.version(),
            self.geometry,
            self.table.tick(),
            balls,
        )
        .map_err(|_| MatchError::InvalidCuePlacement)?;
        self.ball_in_hand = self.rules.ball_in_hand_adjustable();
        Ok(())
    }

    /// Returns a participant's assigned group, if groups have been assigned.
    #[must_use]
    pub const fn group(&self, player: Player) -> Option<Group> {
        match (self.player_one_group, player) {
            (Some(group), Player::One) => Some(group),
            (Some(group), Player::Two) => Some(group.opponent()),
            (None, _) => None,
        }
    }

    /// Returns what `player` must pocket next.
    #[must_use]
    pub fn objective(&self, player: Player) -> Objective {
        match self.group(player) {
            None => Objective::OpenTable,
            Some(_) if self.player_cleared_group(player, &self.table) => Objective::EightBall,
            Some(group) => Objective::Group(group),
        }
    }

    /// Returns whether the next shot is a break.
    #[must_use]
    pub const fn break_pending(&self) -> bool {
        self.break_pending
    }

    /// Canonically simulates and resolves a shot from the active player.
    ///
    /// `called_pocket` is considered only when the 8-ball is pocketed.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::Completed`] after completion, or
    /// [`MatchError::Simulation`] when canonical simulation rejects the shot.
    pub fn play_shot(
        &mut self,
        command: VersionedShotCommand,
        called_pocket: Option<PocketId>,
    ) -> Result<ShotResolution, MatchError> {
        self.play_shot_timed(command, called_pocket)
            .map(|(resolution, _)| resolution)
    }

    /// Plays a shot and also returns the fixed ticks its simulation ran.
    #[allow(clippy::too_many_lines)]
    fn play_shot_timed(
        &mut self,
        command: VersionedShotCommand,
        called_pocket: Option<PocketId>,
    ) -> Result<(ShotResolution, u32), MatchError> {
        if !matches!(self.status, MatchStatus::InProgress) {
            return Err(MatchError::Completed);
        }
        if self.rules.requires_eight_call()
            && called_pocket.is_none()
            && self.objective(self.active_player) == Objective::EightBall
        {
            return Err(MatchError::EightBallCallRequired);
        }
        let before = self.table.clone();
        let result = simulate_shot(self.geometry, self.physics, before.clone(), command)?;
        // Measured before any rerack replaces the table with a fresh rack.
        let playback_ticks = result.state.tick().saturating_sub(before.tick());
        let shooter = self.active_player;
        let on_break = self.break_pending;
        let first_contact = result.events.iter().find_map(|event| match event.kind {
            SimulationEventKind::BallContact { first, second } if first == BallId::CUE => {
                Some(second)
            }
            SimulationEventKind::BallContact { first, second } if second == BallId::CUE => {
                Some(first)
            }
            _ => None,
        });
        let pocketed = result
            .events
            .iter()
            .filter_map(|event| match event.kind {
                SimulationEventKind::BallPocketed { ball, pocket } => Some((ball, pocket)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let first_contact_tick = result.events.iter().find_map(|event| match event.kind {
            SimulationEventKind::BallContact { first, second }
                if first == BallId::CUE || second == BallId::CUE =>
            {
                Some(event.tick)
            }
            _ => None,
        });
        let rail_after_contact = first_contact_tick.is_some_and(|tick| {
            result.events.iter().any(|event| {
                event.tick >= tick
                    && matches!(event.kind, SimulationEventKind::CushionContact { .. })
            })
        });

        let mut fouls = Vec::new();
        if pocketed.iter().any(|(ball, _)| *ball == BallId::CUE) {
            fouls.push(Foul::Scratch);
        }
        match first_contact {
            None => fouls.push(Foul::NoObjectContact),
            Some(ball) if !self.legal_first_contact(shooter, ball, &before, on_break) => {
                fouls.push(Foul::WrongFirstContact);
            }
            Some(_) => {}
        }
        if first_contact.is_some() && pocketed.is_empty() && !rail_after_contact {
            fouls.push(Foul::NoRailAfterContact);
        }

        let eight_pocket = pocketed
            .iter()
            .find(|(ball, _)| ball.number() == 8)
            .copied();
        let lounge = self.rules.is_lounge();
        if on_break && eight_pocket.is_some() && !lounge {
            self.rerack_count = self.rerack_count.saturating_add(1);
            self.table = standard_rack(
                self.geometry,
                RackSeed::new(derive_rerack_seed(
                    self.rack_seed.value(),
                    self.rerack_count,
                )),
            )?;
            self.break_pending = true;
            self.ball_in_hand = false;
            return Ok((
                ShotResolution {
                    shooter,
                    fouls,
                    assigned_group: None,
                    reracked: true,
                    status: self.status,
                    next_player: Some(self.active_player),
                    ball_in_hand: false,
                },
                playback_ticks,
            ));
        }

        // House rules re-spot a break 8 instead of reracking (the only way
        // to reach here with a break 8 pocketed).
        let respot_eight = on_break && eight_pocket.is_some();
        self.table = if respot_eight {
            respot(&result.state, self.geometry, 8, foot_spot(self.geometry))?
        } else {
            result.state
        };
        self.break_pending = false;
        if let Some((_, actual_pocket)) = eight_pocket.filter(|_| !respot_eight) {
            let legal = fouls.is_empty()
                && self.player_cleared_group(shooter, &before)
                && called_pocket == Some(actual_pocket);
            let outcome = MatchOutcome {
                winner: if legal { shooter } else { shooter.opponent() },
                reason: if legal {
                    CompletionReason::LegalEightBall
                } else {
                    CompletionReason::IllegalEightBall
                },
            };
            self.status = MatchStatus::Completed(outcome);
            self.ball_in_hand = false;
            return Ok((
                ShotResolution {
                    shooter,
                    fouls,
                    assigned_group: None,
                    reracked: false,
                    status: self.status,
                    next_player: None,
                    ball_in_hand: false,
                },
                playback_ticks,
            ));
        }

        let assigned_group = if !on_break && fouls.is_empty() && self.player_one_group.is_none() {
            pocketed.iter().find_map(|(ball, _)| group_for_ball(*ball))
        } else {
            None
        };
        if let Some(group) = assigned_group {
            self.player_one_group = Some(if shooter == Player::One {
                group
            } else {
                group.opponent()
            });
        }

        let continue_turn = fouls.is_empty()
            && if on_break {
                // House rules: a clean break that pockets any object ball keeps the turn.
                lounge && pocketed.iter().any(|(ball, _)| *ball != BallId::CUE)
            } else {
                self.group(shooter)
                    .is_some_and(|group| pocketed.iter().any(|(ball, _)| group.contains(*ball)))
            };
        if !continue_turn {
            self.active_player = shooter.opponent();
        }
        self.ball_in_hand = !fouls.is_empty();
        if lounge && fouls.contains(&Foul::Scratch) {
            // House rules: the cue returns to the head spot; ball-in-hand lets
            // the next shooter move it before shooting.
            self.table = respot(&self.table, self.geometry, 0, head_spot(self.geometry))?;
        }
        Ok((
            ShotResolution {
                shooter,
                fouls,
                assigned_group,
                reracked: false,
                status: self.status,
                next_player: Some(self.active_player),
                ball_in_hand: self.ball_in_hand,
            },
            playback_ticks,
        ))
    }

    /// Applies the authoritative turn-timeout transition exactly once at the
    /// aggregate boundary selected by the server scheduler.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::Completed`] after match completion.
    pub const fn timeout_turn(&mut self) -> Result<Player, MatchError> {
        if !matches!(self.status, MatchStatus::InProgress) {
            return Err(MatchError::Completed);
        }
        self.active_player = self.active_player.opponent();
        self.ball_in_hand = true;
        Ok(self.active_player)
    }

    /// Completes the match by explicit concession.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::Completed`] if the match already has a result.
    pub const fn concede(&mut self, conceding_player: Player) -> Result<MatchOutcome, MatchError> {
        if !matches!(self.status, MatchStatus::InProgress) {
            return Err(MatchError::Completed);
        }
        let outcome = MatchOutcome {
            winner: conceding_player.opponent(),
            reason: CompletionReason::Concession,
        };
        self.status = MatchStatus::Completed(outcome);
        self.ball_in_hand = false;
        Ok(outcome)
    }

    fn legal_first_contact(
        &self,
        shooter: Player,
        ball: BallId,
        before: &VersionedTableState,
        on_break: bool,
    ) -> bool {
        if ball.number() == 0 {
            return false;
        }
        if on_break || self.group(shooter).is_none() {
            return ball.number() != 8;
        }
        if self.player_cleared_group(shooter, before) {
            ball.number() == 8
        } else if let Some(group) = self.group(shooter) {
            group.contains(ball)
        } else {
            false
        }
    }

    fn player_cleared_group(&self, player: Player, table: &VersionedTableState) -> bool {
        self.group(player).is_some_and(|group| {
            table
                .balls()
                .iter()
                .filter(|ball| group.contains(ball.id))
                .all(|ball| ball.pocketed)
        })
    }

    /// Applies one explicitly versioned canonical match command.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::UnsupportedCommandVersion`] for an unknown
    /// envelope, or the relevant aggregate transition error.
    pub fn apply_command(
        &mut self,
        command: VersionedMatchCommand,
    ) -> Result<MatchCommandResult, MatchError> {
        self.apply_command_timed(command)
            .map(|applied| applied.result)
    }

    /// Applies one explicitly versioned canonical match command and reports
    /// how many fixed physics ticks of shot playback the transition spans.
    ///
    /// The resulting state and [`MatchCommandResult`] are identical to
    /// [`Self::apply_command`]. `playback_ticks` is zero for non-shot
    /// commands and zero-power shots; for an 8-ball-on-break rerack it is the
    /// simulated shot's duration.
    ///
    /// # Errors
    ///
    /// Returns [`MatchError::UnsupportedCommandVersion`] for an unknown
    /// envelope, or the relevant aggregate transition error.
    pub fn apply_command_timed(
        &mut self,
        command: VersionedMatchCommand,
    ) -> Result<AppliedCommand, MatchError> {
        if command.version != MATCH_COMMAND_VERSION {
            return Err(MatchError::UnsupportedCommandVersion(command.version));
        }
        let untimed = |result| AppliedCommand {
            result,
            playback_ticks: 0,
        };
        match command.command {
            MatchCommand::PlayShot {
                shot,
                called_pocket,
            } => self
                .play_shot_timed(shot, called_pocket)
                .map(|(resolution, playback_ticks)| AppliedCommand {
                    result: MatchCommandResult::Shot(resolution),
                    playback_ticks,
                }),
            MatchCommand::Timeout => self
                .timeout_turn()
                .map(|active_player| untimed(MatchCommandResult::Timeout { active_player })),
            MatchCommand::Concede { player } => self
                .concede(player)
                .map(|outcome| untimed(MatchCommandResult::Conceded(outcome))),
            MatchCommand::PlaceCueBall { position } => {
                self.place_cue_ball(position)?;
                Ok(untimed(MatchCommandResult::CueBallPlaced))
            }
        }
    }

    /// Replays an ordered command history from a canonical initial aggregate.
    ///
    /// # Errors
    ///
    /// Returns the first [`MatchError`] produced by the ordered command stream.
    pub fn replay(
        mut initial: Self,
        commands: &[VersionedMatchCommand],
    ) -> Result<Self, MatchError> {
        for command in commands {
            initial.apply_command(*command)?;
        }
        Ok(initial)
    }

    /// Encodes the complete aggregate into the stable version-one binary format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let table = self.table.to_bytes();
        let mut bytes = Vec::with_capacity(53 + table.len());
        bytes.extend_from_slice(&MATCH_STATE_VERSION.to_be_bytes());
        bytes.extend_from_slice(&self.rules.version().to_be_bytes());
        encode_physics(self.physics, &mut bytes);
        encode_geometry(self.geometry, &mut bytes);
        bytes.extend_from_slice(&self.rack_seed.value().to_be_bytes());
        bytes.extend_from_slice(&self.rerack_count.to_be_bytes());
        bytes.push(encode_player(self.active_player));
        bytes.push(encode_player(self.breaker));
        bytes.push(u8::from(self.break_pending));
        bytes.push(encode_group(self.player_one_group));
        bytes.push(u8::from(self.ball_in_hand));
        encode_status(self.status, &mut bytes);
        bytes.extend_from_slice(&u32::try_from(table.len()).unwrap_or(u32::MAX).to_be_bytes());
        bytes.extend_from_slice(&table);
        bytes
    }

    /// Decodes and validates a complete canonical aggregate snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`MatchDecodeError`] for malformed, truncated, unsupported, or
    /// internally inconsistent payloads.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, MatchDecodeError> {
        let mut reader = MatchReader::new(bytes);
        let version = reader.u16()?;
        if version != MATCH_STATE_VERSION {
            return Err(MatchDecodeError::UnsupportedVersion(version));
        }
        let rules = RulesProfile::new(reader.u16()?)?;
        let physics = decode_physics(&mut reader)?;
        let geometry = decode_geometry(&mut reader)?;
        let rack_seed = RackSeed::new(reader.u64()?);
        let rerack_count = reader.u32()?;
        let active_player = decode_player(reader.u8()?)?;
        let breaker = decode_player(reader.u8()?)?;
        let break_pending = decode_bool(reader.u8()?)?;
        let player_one_group = decode_group(reader.u8()?)?;
        let ball_in_hand = decode_bool(reader.u8()?)?;
        let status = decode_status(&mut reader)?;
        let table_length = usize::try_from(reader.u32()?).unwrap_or(usize::MAX);
        let table_bytes = reader.bytes(table_length)?;
        if !reader.finished() {
            return Err(MatchDecodeError::TrailingBytes(reader.remaining()));
        }
        let table = VersionedTableState::from_bytes(geometry, table_bytes)?;
        if matches!(status, MatchStatus::Completed(_)) && ball_in_hand {
            return Err(MatchDecodeError::InconsistentState);
        }
        Ok(Self {
            rules,
            physics,
            geometry,
            rack_seed,
            rerack_count,
            table,
            active_player,
            breaker,
            break_pending,
            player_one_group,
            ball_in_hand,
            status,
        })
    }

    /// Returns a stable checksum over the canonical aggregate encoding.
    #[must_use]
    pub fn checksum(&self) -> u64 {
        self.to_bytes()
            .into_iter()
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }
}

/// Authoritative match transition failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MatchError {
    /// Match already has a terminal result.
    #[error("match is already complete")]
    Completed,
    /// Match command envelope version is unknown.
    #[error("unsupported match command version {0}")]
    UnsupportedCommandVersion(u16),
    /// Match has not completed and cannot be rematched.
    #[error("match is not complete")]
    NotCompleted,
    /// Ball-in-hand placement was attempted outside a ball-in-hand phase.
    #[error("ball-in-hand placement is unavailable")]
    BallInHandUnavailable,
    /// Cue-ball placement violates canonical table-state constraints.
    #[error("cue-ball placement is invalid")]
    InvalidCuePlacement,
    /// Opening or replacement rack construction failed.
    #[error(transparent)]
    Rack(#[from] RackError),
    /// Canonical shot simulation failed.
    #[error(transparent)]
    Simulation(#[from] SimulationError),
    /// This rules profile requires a called pocket when shooting at the 8.
    #[error("a called pocket is required for the 8-ball")]
    EightBallCallRequired,
    /// No free position remained to re-spot a ball.
    #[error("no free position to re-spot ball {0}")]
    RespotUnavailable(u8),
}

/// Failure to decode a canonical match snapshot.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MatchDecodeError {
    /// Match configuration version is unknown.
    #[error("unsupported match configuration version {0}")]
    UnsupportedConfigurationVersion(u16),
    /// Match snapshot version is unknown.
    #[error("unsupported match state version {0}")]
    UnsupportedVersion(u16),
    /// Snapshot ended before a complete field was available.
    #[error("match state payload is truncated")]
    Truncated,
    /// Snapshot contains trailing bytes.
    #[error("match state payload has {0} trailing bytes")]
    TrailingBytes(usize),
    /// An enum or boolean discriminant is unknown.
    #[error("match state payload has invalid discriminant {0}")]
    InvalidDiscriminant(u8),
    /// Decoded fields form an impossible aggregate state.
    #[error("match state payload is internally inconsistent")]
    InconsistentState,
    /// Rules profile version is unsupported.
    #[error(transparent)]
    Rules(#[from] RulesProfileError),
    /// Physics profile is invalid or unsupported.
    #[error(transparent)]
    Physics(#[from] pwmtf_billiards::PhysicsProfileError),
    /// Table geometry is invalid or unsupported.
    #[error(transparent)]
    Geometry(#[from] pwmtf_billiards::GeometryError),
    /// Canonical table state is malformed.
    #[error(transparent)]
    Table(#[from] pwmtf_billiards::TableStateDecodeError),
}

struct MatchReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> MatchReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn bytes(&mut self, size: usize) -> Result<&'a [u8], MatchDecodeError> {
        let end = self
            .offset
            .checked_add(size)
            .ok_or(MatchDecodeError::Truncated)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(MatchDecodeError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    fn array<const SIZE: usize>(&mut self) -> Result<[u8; SIZE], MatchDecodeError> {
        let mut value = [0; SIZE];
        value.copy_from_slice(self.bytes(SIZE)?);
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, MatchDecodeError> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16, MatchDecodeError> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32, MatchDecodeError> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, MatchDecodeError> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64, MatchDecodeError> {
        Ok(i64::from_be_bytes(self.array()?))
    }
    const fn finished(&self) -> bool {
        self.offset == self.bytes.len()
    }
    const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

fn encode_geometry(value: TableGeometry, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&value.version().to_be_bytes());
    for scalar in [
        value.half_width(),
        value.half_height(),
        value.ball_radius(),
        value.pocket_radius(),
    ] {
        bytes.extend_from_slice(&scalar.micros().to_be_bytes());
    }
}

fn decode_geometry(reader: &mut MatchReader<'_>) -> Result<TableGeometry, MatchDecodeError> {
    Ok(TableGeometry::new(
        reader.u16()?,
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
    )?)
}

fn encode_physics(value: PhysicsProfile, bytes: &mut Vec<u8>) {
    bytes.extend_from_slice(&value.version().to_be_bytes());
    bytes.extend_from_slice(&value.ticks_per_second().to_be_bytes());
    bytes.extend_from_slice(&value.maximum_ticks().to_be_bytes());
    bytes.extend_from_slice(&value.maximum_speed_per_second().micros().to_be_bytes());
    bytes.extend_from_slice(
        &value
            .rolling_deceleration_per_second_squared()
            .micros()
            .to_be_bytes(),
    );
    bytes.extend_from_slice(&value.cushion_restitution_millionths().to_be_bytes());
    bytes.extend_from_slice(&value.collision_restitution_millionths().to_be_bytes());
    bytes.extend_from_slice(&value.settling_speed_per_second().micros().to_be_bytes());
}

fn decode_physics(reader: &mut MatchReader<'_>) -> Result<PhysicsProfile, MatchDecodeError> {
    Ok(PhysicsProfile::new(
        reader.u16()?,
        reader.u16()?,
        reader.u32()?,
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
        reader.u32()?,
        reader.u32()?,
        pwmtf_billiards::Scalar::from_micros(reader.i64()?),
    )?)
}

const fn encode_player(value: Player) -> u8 {
    match value {
        Player::One => 1,
        Player::Two => 2,
    }
}
const fn decode_player(value: u8) -> Result<Player, MatchDecodeError> {
    match value {
        1 => Ok(Player::One),
        2 => Ok(Player::Two),
        _ => Err(MatchDecodeError::InvalidDiscriminant(value)),
    }
}
const fn encode_group(value: Option<Group>) -> u8 {
    match value {
        None => 0,
        Some(Group::Solids) => 1,
        Some(Group::Stripes) => 2,
    }
}
const fn decode_group(value: u8) -> Result<Option<Group>, MatchDecodeError> {
    match value {
        0 => Ok(None),
        1 => Ok(Some(Group::Solids)),
        2 => Ok(Some(Group::Stripes)),
        _ => Err(MatchDecodeError::InvalidDiscriminant(value)),
    }
}
const fn decode_bool(value: u8) -> Result<bool, MatchDecodeError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(MatchDecodeError::InvalidDiscriminant(value)),
    }
}

fn encode_status(status: MatchStatus, bytes: &mut Vec<u8>) {
    match status {
        MatchStatus::InProgress => bytes.push(0),
        MatchStatus::Completed(outcome) => {
            bytes.push(1);
            bytes.push(encode_player(outcome.winner));
            bytes.push(match outcome.reason {
                CompletionReason::LegalEightBall => 1,
                CompletionReason::IllegalEightBall => 2,
                CompletionReason::Concession => 3,
            });
        }
    }
}

fn decode_status(reader: &mut MatchReader<'_>) -> Result<MatchStatus, MatchDecodeError> {
    match reader.u8()? {
        0 => Ok(MatchStatus::InProgress),
        1 => {
            let winner = decode_player(reader.u8()?)?;
            let reason = match reader.u8()? {
                1 => CompletionReason::LegalEightBall,
                2 => CompletionReason::IllegalEightBall,
                3 => CompletionReason::Concession,
                value => return Err(MatchDecodeError::InvalidDiscriminant(value)),
            };
            Ok(MatchStatus::Completed(MatchOutcome { winner, reason }))
        }
        value => Err(MatchDecodeError::InvalidDiscriminant(value)),
    }
}

const fn group_for_ball(ball: BallId) -> Option<Group> {
    match ball.number() {
        1..=7 => Some(Group::Solids),
        9..=15 => Some(Group::Stripes),
        _ => None,
    }
}

/// Where the rack's apex sits; a re-spotted 8 prefers it.
const fn foot_spot(geometry: TableGeometry) -> Vector {
    Vector::from_micros(geometry.half_width().micros() / 4, 0)
}

/// Where the cue ball starts a rack; a re-spotted cue ball prefers it.
const fn head_spot(geometry: TableGeometry) -> Vector {
    Vector::from_micros(-geometry.half_width().micros() / 2, 0)
}

/// Returns `table` with ball `number` stationary at `preferred`, or at the
/// nearest free position on a ball-spaced grid: in bounds, clear of every
/// in-play ball and of every pocket's capture area.
fn respot(
    table: &VersionedTableState,
    geometry: TableGeometry,
    number: u8,
    preferred: Vector,
) -> Result<VersionedTableState, MatchError> {
    let id = BallId::new(number).map_err(|_| MatchError::RespotUnavailable(number))?;
    let clear_of_pockets = |position: Vector| {
        let reach = i128::from(geometry.pocket_radius().micros() + geometry.ball_radius().micros());
        PocketId::ALL.into_iter().all(|pocket| {
            let center = geometry.pocket_center(pocket);
            let dx = i128::from(position.x.micros() - center.x.micros());
            let dy = i128::from(position.y.micros() - center.y.micros());
            dx * dx + dy * dy > reach * reach
        })
    };
    let place = |position: Vector| {
        if !clear_of_pockets(position) {
            return None;
        }
        let mut balls = table.balls().to_vec();
        let slot = balls.iter().position(|ball| ball.id == id)?;
        balls[slot] = BallState::stationary(id, position);
        VersionedTableState::new(table.version(), geometry, table.tick(), balls).ok()
    };
    if let Some(placed) = place(preferred) {
        return Ok(placed);
    }
    let radius = geometry.ball_radius().micros();
    let step = radius * 2 + radius / 4;
    let (reach_x, reach_y) = (
        geometry.half_width().micros() - radius,
        geometry.half_height().micros() - radius,
    );
    let mut candidates = Vec::new();
    let mut x = -reach_x;
    while x <= reach_x {
        let mut y = -reach_y;
        while y <= reach_y {
            candidates.push(Vector::from_micros(x, y));
            y += step;
        }
        x += step;
    }
    let distance = |position: Vector| {
        let dx = i128::from(position.x.micros() - preferred.x.micros());
        let dy = i128::from(position.y.micros() - preferred.y.micros());
        dx * dx + dy * dy
    };
    candidates.sort_by_key(|position| {
        (
            distance(*position),
            position.x.micros(),
            position.y.micros(),
        )
    });
    candidates
        .into_iter()
        .find_map(place)
        .ok_or(MatchError::RespotUnavailable(number))
}

fn derive_rerack_seed(seed: u64, rerack_count: u32) -> u64 {
    seed ^ u64::from(rerack_count).wrapping_mul(0x9e37_79b9_7f4a_7c15)
}

#[cfg(test)]
mod tests {
    use pwmtf_billiards::{Aim, ShotPower, Spin};

    use super::*;

    fn match_state() -> MatchState {
        MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .expect("match starts")
    }

    #[test]
    fn objective_moves_from_open_table_to_group_to_eight_ball() {
        let mut state = match_state();
        assert!(state.break_pending());
        assert_eq!(state.objective(Player::One), Objective::OpenTable);
        state.player_one_group = Some(Group::Stripes);
        state.break_pending = false;
        assert_eq!(
            state.objective(Player::One),
            Objective::Group(Group::Stripes)
        );
        assert_eq!(
            state.objective(Player::Two),
            Objective::Group(Group::Solids)
        );
        // Pocket every stripe: player one is now on the 8, player two is not.
        let balls = state
            .table()
            .balls()
            .iter()
            .map(|ball| {
                let mut ball = *ball;
                if Group::Stripes.contains(ball.id) {
                    ball.pocketed = true;
                }
                ball
            })
            .collect();
        state.table = VersionedTableState::new(
            state.table.version(),
            state.geometry,
            state.table.tick(),
            balls,
        )
        .unwrap();
        assert_eq!(state.objective(Player::One), Objective::EightBall);
        assert_eq!(
            state.objective(Player::Two),
            Objective::Group(Group::Solids)
        );
        assert!(Group::Solids.contains(BallId::new(7).unwrap()));
        assert!(!Group::Solids.contains(BallId::new(8).unwrap()));
    }

    fn command(aim: u16, power: u16) -> VersionedShotCommand {
        VersionedShotCommand::new(
            Aim::new(aim).unwrap(),
            ShotPower::new(power).unwrap(),
            Spin::CENTER,
        )
    }

    fn side_pocket_state(object_ball: u8) -> VersionedTableState {
        VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            TableGeometry::standard(),
            0,
            vec![
                pwmtf_billiards::BallState::stationary(
                    BallId::CUE,
                    pwmtf_billiards::Vector::from_micros(0, 200_000),
                ),
                pwmtf_billiards::BallState::stationary(
                    BallId::new(object_ball).unwrap(),
                    pwmtf_billiards::Vector::from_micros(0, 450_000),
                ),
            ],
        )
        .unwrap()
    }

    fn cleared_solids_eight_ball_state() -> VersionedTableState {
        let mut balls = vec![
            pwmtf_billiards::BallState::stationary(
                BallId::CUE,
                pwmtf_billiards::Vector::from_micros(0, 200_000),
            ),
            pwmtf_billiards::BallState::stationary(
                BallId::new(8).unwrap(),
                pwmtf_billiards::Vector::from_micros(0, 450_000),
            ),
        ];
        for number in 1..=7 {
            let mut ball = pwmtf_billiards::BallState::stationary(
                BallId::new(number).unwrap(),
                pwmtf_billiards::Vector::ZERO,
            );
            ball.pocketed = true;
            balls.push(ball);
        }
        VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            TableGeometry::standard(),
            0,
            balls,
        )
        .unwrap()
    }

    #[test]
    fn full_command_replay_matches_incremental_application() {
        let initial = match_state();
        let commands = [
            VersionedMatchCommand::new(MatchCommand::Timeout),
            VersionedMatchCommand::new(MatchCommand::PlaceCueBall {
                position: pwmtf_billiards::Vector::from_micros(-600_000, 100_000),
            }),
            VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: command(0, 0),
                called_pocket: None,
            }),
        ];
        let replayed = MatchState::replay(initial.clone(), &commands).unwrap();
        let mut incremental = initial;
        for command in commands {
            incremental.apply_command(command).unwrap();
        }
        assert_eq!(replayed, incremental);
        assert_eq!(replayed.checksum(), incremental.checksum());
    }

    #[test]
    fn match_command_results_round_trip_and_reject_unknown_versions() {
        let results = [
            MatchCommandResult::Timeout {
                active_player: Player::Two,
            },
            MatchCommandResult::Conceded(MatchOutcome {
                winner: Player::One,
                reason: CompletionReason::Concession,
            }),
            MatchCommandResult::CueBallPlaced,
            MatchCommandResult::Shot(ShotResolution {
                shooter: Player::One,
                fouls: vec![Foul::Scratch, Foul::NoRailAfterContact],
                assigned_group: Some(Group::Solids),
                reracked: false,
                status: MatchStatus::InProgress,
                next_player: Some(Player::Two),
                ball_in_hand: true,
            }),
        ];
        for result in results {
            assert_eq!(
                MatchCommandResult::from_bytes(&result.to_bytes()),
                Ok(result)
            );
        }
        let mut unknown = MatchCommandResult::CueBallPlaced.to_bytes();
        unknown[1] = 2;
        assert_eq!(
            MatchCommandResult::from_bytes(&unknown),
            Err(MatchResultDecodeError::UnsupportedVersion(2))
        );
    }

    #[test]
    fn unknown_match_command_versions_fail_closed() {
        let mut game = match_state();
        assert_eq!(
            game.apply_command(VersionedMatchCommand {
                version: 2,
                command: MatchCommand::Timeout,
            }),
            Err(MatchError::UnsupportedCommandVersion(2))
        );
        assert_eq!(game, match_state());
    }

    #[test]
    fn match_configuration_round_trip_remains_pinned() {
        let game = match_state();
        let configuration =
            MatchConfiguration::from_bytes(&game.configuration().to_bytes()).unwrap();
        assert_eq!(configuration, game.configuration());
        let mut unknown = configuration.to_bytes();
        unknown[1] = 2;
        assert_eq!(
            MatchConfiguration::from_bytes(&unknown),
            Err(MatchDecodeError::UnsupportedConfigurationVersion(2))
        );
        let restored = MatchState::from_bytes(&game.to_bytes()).unwrap();
        assert_eq!(restored.configuration(), configuration);
        assert_eq!(configuration.rules(), game.rules());
        assert_eq!(configuration.physics(), game.physics());
        assert_eq!(configuration.geometry(), game.geometry());
        assert_eq!(configuration.rack_seed(), game.rack_seed());
        assert_eq!(configuration.breaker(), game.breaker());
    }

    #[test]
    fn match_snapshot_round_trip_preserves_checksum_and_continuation() {
        let mut game = match_state();
        game.timeout_turn().unwrap();
        game.place_cue_ball(pwmtf_billiards::Vector::from_micros(-600_000, 100_000))
            .unwrap();
        let bytes = game.to_bytes();
        let restored = MatchState::from_bytes(&bytes).unwrap();
        assert_eq!(restored, game);
        assert_eq!(restored.checksum(), game.checksum());

        let mut first = game;
        let mut second = restored;
        assert_eq!(
            first.play_shot(command(0, 0), None),
            second.play_shot(command(0, 0), None)
        );
        assert_eq!(first, second);
    }

    #[test]
    fn match_snapshot_rejects_unknown_and_trailing_data() {
        let game = match_state();
        let mut bytes = game.to_bytes();
        bytes[1] = 2;
        assert_eq!(
            MatchState::from_bytes(&bytes),
            Err(MatchDecodeError::UnsupportedVersion(2))
        );
        let mut trailing = game.to_bytes();
        trailing.push(0);
        assert_eq!(
            MatchState::from_bytes(&trailing),
            Err(MatchDecodeError::TrailingBytes(1))
        );
    }

    #[test]
    fn ball_in_hand_placement_is_validated_and_consumed() {
        let mut game = match_state();
        assert_eq!(
            game.place_cue_ball(pwmtf_billiards::Vector::ZERO),
            Err(MatchError::BallInHandUnavailable)
        );
        game.timeout_turn().unwrap();
        assert_eq!(
            game.place_cue_ball(pwmtf_billiards::Vector::from_micros(2_000_000, 0)),
            Err(MatchError::InvalidCuePlacement)
        );
        assert!(game.ball_in_hand());
        game.place_cue_ball(pwmtf_billiards::Vector::from_micros(-600_000, 100_000))
            .unwrap();
        assert!(!game.ball_in_hand());
        assert_eq!(
            game.table().ball(BallId::CUE).unwrap().position,
            pwmtf_billiards::Vector::from_micros(-600_000, 100_000)
        );
    }

    #[test]
    fn rematch_links_completed_match_and_alternates_breaker() {
        let mut game = match_state();
        assert_eq!(
            RematchMetadata::from_completed(7, &game),
            Err(MatchError::NotCompleted)
        );
        game.concede(Player::Two).unwrap();
        assert_eq!(
            RematchMetadata::from_completed(7, &game),
            Ok(RematchMetadata {
                previous_match_id: 7,
                breaker: Player::Two,
            })
        );
    }

    #[test]
    fn break_uses_canonical_contact_and_rail_foul_precedence() {
        let mut game = match_state();
        game.table = VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            TableGeometry::standard(),
            0,
            vec![
                pwmtf_billiards::BallState::stationary(
                    BallId::CUE,
                    pwmtf_billiards::Vector::from_micros(-400_000, 0),
                ),
                pwmtf_billiards::BallState::stationary(
                    BallId::new(1).unwrap(),
                    pwmtf_billiards::Vector::ZERO,
                ),
            ],
        )
        .unwrap();
        let result = game.play_shot(command(0, 100), None).unwrap();
        assert_eq!(result.fouls, vec![Foul::NoObjectContact]);
        assert_eq!(result.status, MatchStatus::InProgress);
        assert!(!result.reracked);
    }

    #[test]
    fn timeout_never_completes_match_and_always_grants_ball_in_hand() {
        let mut game = match_state();
        for expected in [Player::Two, Player::One].into_iter().cycle().take(1_000) {
            assert_eq!(game.timeout_turn(), Ok(expected));
            assert_eq!(game.status(), MatchStatus::InProgress);
            assert!(game.ball_in_hand());
        }
    }

    #[test]
    fn concession_is_the_only_non_eight_ball_completion() {
        let mut game = match_state();
        let outcome = game.concede(Player::One).unwrap();
        assert_eq!(outcome.winner, Player::Two);
        assert_eq!(outcome.reason, CompletionReason::Concession);
        assert_eq!(game.status(), MatchStatus::Completed(outcome));
        assert_eq!(game.timeout_turn(), Err(MatchError::Completed));
    }

    #[test]
    fn post_break_object_ball_assigns_group_and_continues_turn() {
        let mut game = match_state();
        game.break_pending = false;
        game.table = side_pocket_state(1);
        let result = game.play_shot(command(2_500, 3_000), None).unwrap();
        assert!(result.fouls.is_empty());
        assert_eq!(result.assigned_group, Some(Group::Solids));
        assert_eq!(game.group(Player::One), Some(Group::Solids));
        assert_eq!(result.next_player, Some(Player::One));
    }

    #[test]
    fn scratch_grants_opponent_ball_in_hand() {
        let mut game = match_state();
        game.break_pending = false;
        game.table = VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            TableGeometry::standard(),
            0,
            vec![pwmtf_billiards::BallState::stationary(
                BallId::CUE,
                pwmtf_billiards::Vector::from_micros(0, 500_000),
            )],
        )
        .unwrap();
        let result = game.play_shot(command(2_500, 2_000), None).unwrap();
        assert!(result.fouls.contains(&Foul::Scratch));
        assert_eq!(result.next_player, Some(Player::Two));
        assert!(result.ball_in_hand);
    }

    #[test]
    fn legal_called_eight_ball_wins_after_group_is_cleared() {
        let mut game = match_state();
        game.break_pending = false;
        game.player_one_group = Some(Group::Solids);
        game.table = cleared_solids_eight_ball_state();
        let result = game
            .play_shot(command(2_500, 3_000), Some(PocketId::TopCenter))
            .unwrap();
        assert_eq!(
            result.status,
            MatchStatus::Completed(MatchOutcome {
                winner: Player::One,
                reason: CompletionReason::LegalEightBall,
            })
        );
    }

    #[test]
    fn wrong_called_pocket_loses_after_group_is_cleared() {
        let mut game = match_state();
        game.break_pending = false;
        game.player_one_group = Some(Group::Solids);
        game.table = cleared_solids_eight_ball_state();
        let result = game
            .play_shot(command(2_500, 3_000), Some(PocketId::TopLeft))
            .unwrap();
        assert_eq!(
            result.status,
            MatchStatus::Completed(MatchOutcome {
                winner: Player::Two,
                reason: CompletionReason::IllegalEightBall,
            })
        );
    }

    #[test]
    fn illegal_eight_ball_loses_match() {
        let mut game = match_state();
        game.break_pending = false;
        game.table = side_pocket_state(8);
        let result = game
            .play_shot(command(2_500, 3_000), Some(PocketId::TopCenter))
            .unwrap();
        assert_eq!(
            result.status,
            MatchStatus::Completed(MatchOutcome {
                winner: Player::Two,
                reason: CompletionReason::IllegalEightBall,
            })
        );
    }

    #[test]
    fn zero_power_break_is_a_foul_and_changes_turn() {
        let mut game = match_state();
        let result = game.play_shot(command(0, 0), None).unwrap();
        assert!(result.fouls.contains(&Foul::NoObjectContact));
        assert_eq!(result.next_player, Some(Player::Two));
        assert!(result.ball_in_hand);
        assert_eq!(game.status(), MatchStatus::InProgress);
    }

    #[test]
    fn timed_break_reports_playback_ticks_and_matches_untimed_application() {
        let shot = VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: command(0, 3_000),
            called_pocket: None,
        });
        let mut timed = match_state();
        let mut untimed = match_state();
        let start_tick = timed.table().tick();
        let applied = timed.apply_command_timed(shot).unwrap();
        let result = untimed.apply_command(shot).unwrap();
        assert!(applied.playback_ticks > 0);
        assert_eq!(
            applied.playback_ticks,
            timed.table().tick() - start_tick,
            "playback spans exactly the simulated ticks"
        );
        assert_eq!(applied.result, result);
        assert_eq!(timed, untimed);
        assert_eq!(timed.checksum(), untimed.checksum());
    }

    fn lounge_state() -> MatchState {
        MatchState::new(
            RulesProfile::lounge(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap()
    }

    #[test]
    fn house_rules_keep_the_turn_after_a_clean_break_that_pockets_a_ball() {
        let mut lounge = lounge_state();
        lounge.table = side_pocket_state(1);
        let result = lounge.play_shot(command(2_500, 3_000), None).unwrap();
        assert!(result.fouls.is_empty());
        assert_eq!(result.next_player, Some(Player::One));
        assert_eq!(
            result.assigned_group, None,
            "groups are never set on the break"
        );
        let mut standard = match_state();
        standard.table = side_pocket_state(1);
        let result = standard.play_shot(command(2_500, 3_000), None).unwrap();
        assert_eq!(result.next_player, Some(Player::Two));
    }

    #[test]
    fn house_rules_respot_a_break_eight_instead_of_reracking() {
        let mut game = lounge_state();
        game.table = side_pocket_state(8);
        let result = game.play_shot(command(2_500, 3_000), None).unwrap();
        assert!(!result.reracked);
        assert_eq!(result.status, MatchStatus::InProgress);
        let eight = game.table.ball(BallId::new(8).unwrap()).unwrap();
        assert!(!eight.pocketed);
        assert_eq!(eight.position, foot_spot(game.geometry));
        assert_eq!(game.rerack_count, 0);
    }

    #[test]
    fn house_rules_refuse_an_uncalled_eight_ball_shot_atomically() {
        let mut lounge = lounge_state();
        lounge.break_pending = false;
        lounge.player_one_group = Some(Group::Solids);
        lounge.table = cleared_solids_eight_ball_state();
        let before = lounge.clone();
        assert_eq!(
            lounge.play_shot(command(2_500, 3_000), None),
            Err(MatchError::EightBallCallRequired)
        );
        assert_eq!(lounge, before);
        // Standard rules resolve the same shot as a loss instead.
        let mut standard = match_state();
        standard.break_pending = false;
        standard.player_one_group = Some(Group::Solids);
        standard.table = cleared_solids_eight_ball_state();
        let result = standard.play_shot(command(2_500, 3_000), None).unwrap();
        assert!(
            matches!(result.status, MatchStatus::Completed(outcome) if outcome.winner == Player::Two)
        );
    }

    #[test]
    fn house_rules_respot_a_scratched_cue_with_adjustable_ball_in_hand() {
        let mut game = lounge_state();
        game.break_pending = false;
        game.table = VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            TableGeometry::standard(),
            0,
            vec![pwmtf_billiards::BallState::stationary(
                BallId::CUE,
                pwmtf_billiards::Vector::from_micros(0, 500_000),
            )],
        )
        .unwrap();
        let result = game.play_shot(command(2_500, 2_000), None).unwrap();
        assert!(result.fouls.contains(&Foul::Scratch));
        assert!(game.ball_in_hand());
        let cue = game.table.ball(BallId::CUE).unwrap();
        assert!(!cue.pocketed);
        assert_eq!(cue.position, head_spot(game.geometry));
        // Placements can be adjusted until the shot.
        for x in [-500_000, -400_000] {
            game.place_cue_ball(pwmtf_billiards::Vector::from_micros(x, 0))
                .unwrap();
            assert!(game.ball_in_hand());
        }
        // Standard rules end ball-in-hand on the first placement.
        let mut standard = match_state();
        standard.ball_in_hand = true;
        standard
            .place_cue_ball(pwmtf_billiards::Vector::from_micros(-500_000, 0))
            .unwrap();
        assert!(!standard.ball_in_hand());
    }

    #[test]
    fn respot_finds_the_nearest_free_spot_clear_of_balls_and_pockets() {
        let geometry = TableGeometry::standard();
        let spot = foot_spot(geometry);
        let blocked = VersionedTableState::new(
            pwmtf_billiards::TABLE_STATE_VERSION,
            geometry,
            0,
            vec![pwmtf_billiards::BallState::stationary(BallId::CUE, spot), {
                let mut eight = pwmtf_billiards::BallState::stationary(
                    BallId::new(8).unwrap(),
                    pwmtf_billiards::Vector::ZERO,
                );
                eight.pocketed = true;
                eight
            }],
        )
        .unwrap();
        let table = respot(&blocked, geometry, 8, spot).unwrap();
        let eight = table.ball(BallId::new(8).unwrap()).unwrap();
        assert!(!eight.pocketed);
        assert_ne!(eight.position, spot);
        let gap = eight.position.x.micros() - spot.x.micros();
        let rise = eight.position.y.micros() - spot.y.micros();
        let diameter = geometry.ball_radius().micros() * 2;
        assert!(gap * gap + rise * rise >= diameter * diameter);
        assert!(
            gap.abs() + rise.abs() < diameter * 4,
            "stays near the preferred spot"
        );
        // A spot inside a pocket's capture area is never chosen.
        let corner = geometry.pocket_center(PocketId::TopRight);
        let table = respot(&blocked, geometry, 8, corner).unwrap();
        let moved = table.ball(BallId::new(8).unwrap()).unwrap().position;
        let reach = geometry.pocket_radius().micros() + geometry.ball_radius().micros();
        let (dx, dy) = (
            moved.x.micros() - corner.x.micros(),
            moved.y.micros() - corner.y.micros(),
        );
        assert!(i128::from(dx).pow(2) + i128::from(dy).pow(2) > i128::from(reach).pow(2));
    }

    #[test]
    fn house_rules_round_trip_on_a_lounge_sized_table() {
        // Cozy Lounge's 2.24 x 1.12 m table, 28 mm balls, 65 mm pockets.
        let geometry = TableGeometry::new(
            1,
            pwmtf_billiards::Scalar::from_micros(1_120_000),
            pwmtf_billiards::Scalar::from_micros(560_000),
            pwmtf_billiards::Scalar::from_micros(28_000),
            pwmtf_billiards::Scalar::from_micros(65_000),
        )
        .unwrap();
        let mut game = MatchState::new(
            RulesProfile::lounge(),
            PhysicsProfile::standard(),
            geometry,
            RackSeed::new(7),
            Player::Two,
        )
        .unwrap();
        assert_eq!(game.rules().turn_duration_millis(), None);
        game.apply_command(VersionedMatchCommand::new(MatchCommand::PlayShot {
            shot: command(0, 9_000),
            called_pocket: None,
        }))
        .unwrap();
        assert_eq!(MatchState::from_bytes(&game.to_bytes()).unwrap(), game);
        assert_eq!(
            RulesProfile::new(LOUNGE_RULES_PROFILE_VERSION),
            Ok(RulesProfile::lounge())
        );
        assert_eq!(
            RulesProfile::new(3),
            Err(RulesProfileError::UnsupportedVersion(3))
        );
    }

    #[test]
    fn timed_rerack_reports_the_simulated_shot_ticks() {
        let mut game = match_state();
        game.table = side_pocket_state(8);
        let before_tick = game.table.tick();
        let shot = command(2_500, 3_000);
        let expected = simulate_shot(game.geometry, game.physics, game.table.clone(), shot)
            .unwrap()
            .state
            .tick()
            - before_tick;
        let applied = game
            .apply_command_timed(VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot,
                called_pocket: None,
            }))
            .unwrap();
        assert!(matches!(
            &applied.result,
            MatchCommandResult::Shot(resolution) if resolution.reracked
        ));
        assert!(expected > 0);
        assert_eq!(applied.playback_ticks, expected);
    }

    #[test]
    fn timed_non_shot_and_zero_power_commands_have_no_playback() {
        let mut timed = match_state();
        let mut untimed = match_state();
        let commands = [
            VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: command(0, 0),
                called_pocket: None,
            }),
            VersionedMatchCommand::new(MatchCommand::PlaceCueBall {
                position: pwmtf_billiards::Vector::from_micros(-600_000, 100_000),
            }),
            VersionedMatchCommand::new(MatchCommand::Timeout),
            VersionedMatchCommand::new(MatchCommand::Concede {
                player: Player::One,
            }),
        ];
        for command in commands {
            let applied = timed.apply_command_timed(command).unwrap();
            assert_eq!(applied.playback_ticks, 0);
            assert_eq!(Ok(applied.result), untimed.apply_command(command));
            assert_eq!(timed, untimed);
        }
        let unknown = VersionedMatchCommand {
            version: MATCH_COMMAND_VERSION + 1,
            command: MatchCommand::Timeout,
        };
        assert_eq!(
            timed.apply_command_timed(unknown),
            Err(MatchError::UnsupportedCommandVersion(
                MATCH_COMMAND_VERSION + 1
            ))
        );
    }

    #[test]
    fn playback_millis_rounds_up_whole_milliseconds() {
        let physics = PhysicsProfile::standard();
        assert_eq!(physics.ticks_per_second(), 240);
        assert_eq!(pwmtf_billiards::playback_millis(physics, 0), 0);
        assert_eq!(pwmtf_billiards::playback_millis(physics, 1), 5);
        assert_eq!(pwmtf_billiards::playback_millis(physics, 240), 1_000);
        assert_eq!(physics.playback_millis(241), 1_005);
        assert_eq!(
            physics.playback_millis(u32::MAX),
            (u64::from(u32::MAX) * 1_000).div_ceil(240)
        );
    }

    #[test]
    fn unknown_rules_versions_fail_closed() {
        assert_eq!(
            RulesProfile::standard().turn_duration_millis(),
            Some(30_000)
        );
        assert_eq!(RulesProfile::lounge().turn_duration_millis(), None);
        assert_eq!(
            RulesProfile::new(9),
            Err(RulesProfileError::UnsupportedVersion(9))
        );
    }
}

/// Stored PWMTF matches depend on these exact encodings; a change here must
/// be a deliberate, versioned format change, never a side effect.
#[cfg(test)]
mod golden_encoding {
    use super::*;
    use pwmtf_billiards::{Aim, ShotPower, Spin, TableGeometry, VersionedShotCommand};

    fn fnv(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }

    #[test]
    fn standard_profile_encodings_are_byte_stable() {
        let mut state = MatchState::new(
            RulesProfile::standard(),
            PhysicsProfile::standard(),
            TableGeometry::standard(),
            RackSeed::new(42),
            Player::One,
        )
        .unwrap();
        let initial = state.to_bytes();
        assert_eq!(
            (initial.len(), fnv(&initial)),
            (1_035, 0x4cf2_40e5_361f_9e4d)
        );
        let result = state
            .apply_command(VersionedMatchCommand::new(MatchCommand::PlayShot {
                shot: VersionedShotCommand::new(
                    Aim::new(0).unwrap(),
                    ShotPower::new(9_000).unwrap(),
                    Spin::CENTER,
                ),
                called_pocket: None,
            }))
            .unwrap();
        let after = state.to_bytes();
        assert_eq!((after.len(), fnv(&after)), (1_035, 0x3aa8_a580_b61a_ad23));
        assert_eq!(fnv(&result.to_bytes()), 0x3017_c9ed_457d_3712);
        assert_eq!(state.checksum(), 0x3aa8_a580_b61a_ad23);
    }
}
