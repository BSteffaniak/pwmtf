#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Canonical, renderer-independent billiards state and simulation.

use thiserror::Error;

mod corpus;
mod rack;
mod rules;

pub use corpus::{ShotFixture, qualification_corpus, qualification_profile};
pub use rack::{RackError, RackSeed, standard_rack};
pub use rules::{
    AppliedCommand, CompletionReason, Foul, Group, MATCH_COMMAND_RESULT_VERSION,
    MATCH_COMMAND_VERSION, MATCH_CONFIGURATION_VERSION, MATCH_STATE_VERSION, MatchCommand,
    MatchCommandResult, MatchConfiguration, MatchDecodeError, MatchError, MatchOutcome,
    MatchResultDecodeError, MatchState, MatchStatus, Objective, Player, RULES_PROFILE_VERSION,
    RematchMetadata, RulesProfile, RulesProfileError, ShotResolution, VersionedMatchCommand,
};

/// Canonical table schema, including complete rolling and vertical spin state.
pub const TABLE_STATE_VERSION: u16 = 2;
mod motion;

/// Current built-in physics profile version.
pub const PHYSICS_PROFILE_VERSION: u16 = 4;
const TRIG_SCALE: i64 = 1_000_000;
const CORDIC_GAIN_INVERSE: i64 = 607_253;
const CORDIC_ANGLES: [i64; 29] = [
    125_000_000,
    73_791_809,
    38_989_565,
    19_791_712,
    9_934_262,
    4_971_974,
    2_486_594,
    1_243_373,
    621_696,
    310_849,
    155_425,
    77_712,
    38_856,
    19_428,
    9_714,
    4_857,
    2_429,
    1_214,
    607,
    304,
    152,
    76,
    38,
    19,
    9,
    5,
    2,
    1,
    1,
];
const MAX_BALLS: usize = 16;
const MAX_EVENTS: usize = 16_384;

/// Stable identifier for one of the sixteen pool balls.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct BallId(u8);

impl BallId {
    /// Cue-ball identifier.
    pub const CUE: Self = Self(0);

    /// Creates an identifier for a ball numbered from zero through fifteen.
    ///
    /// # Errors
    ///
    /// Returns [`BallIdError::OutOfRange`] when `number` exceeds fifteen.
    pub const fn new(number: u8) -> Result<Self, BallIdError> {
        if number <= 15 {
            Ok(Self(number))
        } else {
            Err(BallIdError::OutOfRange(number))
        }
    }

    /// Returns the canonical ball number.
    #[must_use]
    pub const fn number(self) -> u8 {
        self.0
    }
}

/// Failure to construct a canonical ball identifier.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum BallIdError {
    /// The supplied number is not in the inclusive range zero through fifteen.
    #[error("ball number {0} is outside 0..=15")]
    OutOfRange(u8),
}

/// Stable identifier for a table pocket.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PocketId {
    /// Upper-left corner pocket.
    TopLeft,
    /// Upper side pocket.
    TopCenter,
    /// Upper-right corner pocket.
    TopRight,
    /// Lower-left corner pocket.
    BottomLeft,
    /// Lower side pocket.
    BottomCenter,
    /// Lower-right corner pocket.
    BottomRight,
}

impl PocketId {
    const ALL: [Self; 6] = [
        Self::TopLeft,
        Self::TopCenter,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomCenter,
        Self::BottomRight,
    ];
}

/// Signed canonical distance or velocity component in one-millionth table units.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Scalar(i64);

impl Scalar {
    /// Zero scalar.
    pub const ZERO: Self = Self(0);

    /// Creates a scalar from its exact canonical micro-unit representation.
    #[must_use]
    pub const fn from_micros(micros: i64) -> Self {
        Self(micros)
    }

    /// Returns the exact canonical micro-unit representation.
    #[must_use]
    pub const fn micros(self) -> i64 {
        self.0
    }
}

/// Canonical two-dimensional vector.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Vector {
    /// Horizontal component.
    pub x: Scalar,
    /// Vertical component.
    pub y: Scalar,
}

impl Vector {
    /// Zero vector.
    pub const ZERO: Self = Self {
        x: Scalar::ZERO,
        y: Scalar::ZERO,
    };

    /// Creates a vector from exact canonical micro-unit components.
    #[must_use]
    pub const fn from_micros(x: i64, y: i64) -> Self {
        Self {
            x: Scalar::from_micros(x),
            y: Scalar::from_micros(y),
        }
    }
}

/// Versioned immutable table geometry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TableGeometry {
    version: u16,
    half_width: Scalar,
    half_height: Scalar,
    ball_radius: Scalar,
    pocket_radius: Scalar,
}

impl TableGeometry {
    /// Creates validated rectangular table geometry.
    ///
    /// # Errors
    ///
    /// Returns [`GeometryError`] when the version is unsupported, a dimension is
    /// non-positive, a pocket cannot contain a ball center, or the playable
    /// bounds cannot contain a ball.
    pub const fn new(
        version: u16,
        half_width: Scalar,
        half_height: Scalar,
        ball_radius: Scalar,
        pocket_radius: Scalar,
    ) -> Result<Self, GeometryError> {
        if version != 1 {
            return Err(GeometryError::UnsupportedVersion(version));
        }
        if half_width.0 <= 0 || half_height.0 <= 0 {
            return Err(GeometryError::NonPositiveTable);
        }
        if ball_radius.0 <= 0 {
            return Err(GeometryError::NonPositiveBallRadius);
        }
        if pocket_radius.0 < ball_radius.0 {
            return Err(GeometryError::PocketTooSmall);
        }
        if half_width.0 <= ball_radius.0 || half_height.0 <= ball_radius.0 {
            return Err(GeometryError::BallDoesNotFit);
        }
        Ok(Self {
            version,
            half_width,
            half_height,
            ball_radius,
            pocket_radius,
        })
    }

    /// Returns the geometry version.
    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    /// Returns the table half-width.
    #[must_use]
    pub const fn half_width(self) -> Scalar {
        self.half_width
    }

    /// Returns the table half-height.
    #[must_use]
    pub const fn half_height(self) -> Scalar {
        self.half_height
    }

    /// Returns the ball radius.
    #[must_use]
    pub const fn ball_radius(self) -> Scalar {
        self.ball_radius
    }

    /// Returns the pocket capture radius.
    #[must_use]
    pub const fn pocket_radius(self) -> Scalar {
        self.pocket_radius
    }

    /// Continuous cushion noses and pocket facings. Each pair is a segment;
    /// endpoints join exactly, with ball-radius contact rounding handled by
    /// the solver. Rendering consumes this same boundary.
    #[must_use]
    pub fn cushion_segments(self) -> Vec<(Vector, Vector)> {
        let w = self.half_width.0;
        let h = self.half_height.0;
        let side = self.pocket_radius.0 * 3 / 2;
        let corner = self.pocket_radius.0 * 9 / 5;
        let throat = self.pocket_radius.0;
        let depth = self.ball_radius.0;
        let mut segments = Vec::with_capacity(18);
        for y in [-1, 1] {
            for x in [-1, 1] {
                segments.push((
                    Vector::from_micros(x * side, y * h),
                    Vector::from_micros(x * (w - corner), y * h),
                ));
                segments.push((
                    Vector::from_micros(x * side, y * h),
                    Vector::from_micros(x * throat, y * (h + depth)),
                ));
                segments.push((
                    Vector::from_micros(x * (w - corner), y * h),
                    Vector::from_micros(x * (w - throat), y * (h + depth)),
                ));
                segments.push((
                    Vector::from_micros(x * w, y * (h - corner)),
                    Vector::from_micros(x * (w + depth), y * (h - throat)),
                ));
            }
        }
        for x in [-1, 1] {
            segments.push((
                Vector::from_micros(x * w, -h + corner),
                Vector::from_micros(x * w, h - corner),
            ));
        }
        segments
    }

    /// Whether a coordinate along a horizontal/vertical rail is in a pocket mouth.
    #[must_use]
    pub const fn rail_opening(self, horizontal: bool, along: Scalar) -> bool {
        let corner = self.pocket_radius.0 * 9 / 5;
        if horizontal {
            along.0.abs() < self.pocket_radius.0 * 3 / 2
                || along.0.abs() > self.half_width.0 - corner
        } else {
            along.0.abs() > self.half_height.0 - corner
        }
    }

    /// Returns the initial production-candidate table geometry.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            version: 1,
            half_width: Scalar(1_270_000),
            half_height: Scalar(635_000),
            ball_radius: Scalar(28_575),
            pocket_radius: Scalar(50_000),
        }
    }
}

/// Invalid table geometry.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum GeometryError {
    /// The geometry payload version is not supported.
    #[error("unsupported table geometry version {0}")]
    UnsupportedVersion(u16),
    /// A table half-dimension is not positive.
    #[error("table half-dimensions must be positive")]
    NonPositiveTable,
    /// Ball radius is not positive.
    #[error("ball radius must be positive")]
    NonPositiveBallRadius,
    /// Pocket radius is smaller than the ball radius.
    #[error("pocket radius must be at least the ball radius")]
    PocketTooSmall,
    /// The table cannot contain a complete ball.
    #[error("table dimensions must exceed the ball radius")]
    BallDoesNotFit,
}

/// Immutable fixed-step simulation parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PhysicsProfile {
    version: u16,
    ticks_per_second: u16,
    maximum_ticks: u32,
    maximum_speed_per_second: Scalar,
    rolling_deceleration_per_second_squared: Scalar,
    cushion_restitution_millionths: u32,
    collision_restitution_millionths: u32,
    settling_speed_per_second: Scalar,
}

impl PhysicsProfile {
    /// Creates a validated immutable physics profile.
    ///
    /// # Errors
    ///
    /// Returns [`PhysicsProfileError`] for an unsupported version, zero timing
    /// or speed bounds, invalid restitution, or a settling threshold above the
    /// maximum speed.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        version: u16,
        ticks_per_second: u16,
        maximum_ticks: u32,
        maximum_speed_per_second: Scalar,
        rolling_deceleration_per_second_squared: Scalar,
        cushion_restitution_millionths: u32,
        collision_restitution_millionths: u32,
        settling_speed_per_second: Scalar,
    ) -> Result<Self, PhysicsProfileError> {
        if version != PHYSICS_PROFILE_VERSION {
            return Err(PhysicsProfileError::UnsupportedVersion(version));
        }
        if ticks_per_second == 0 || maximum_ticks == 0 {
            return Err(PhysicsProfileError::InvalidTiming);
        }
        if maximum_speed_per_second.0 <= 0 || rolling_deceleration_per_second_squared.0 <= 0 {
            return Err(PhysicsProfileError::InvalidMotionBound);
        }
        if cushion_restitution_millionths > 1_000_000
            || collision_restitution_millionths > 1_000_000
        {
            return Err(PhysicsProfileError::InvalidRestitution);
        }
        if settling_speed_per_second.0 < 0
            || settling_speed_per_second.0 > maximum_speed_per_second.0
        {
            return Err(PhysicsProfileError::InvalidSettlingSpeed);
        }
        Ok(Self {
            version,
            ticks_per_second,
            maximum_ticks,
            maximum_speed_per_second,
            rolling_deceleration_per_second_squared,
            cushion_restitution_millionths,
            collision_restitution_millionths,
            settling_speed_per_second,
        })
    }

    /// Returns the initial production-candidate profile.
    #[must_use]
    pub const fn standard() -> Self {
        Self {
            version: PHYSICS_PROFILE_VERSION,
            ticks_per_second: 240,
            maximum_ticks: 14_400,
            maximum_speed_per_second: Scalar(7_500_000),
            rolling_deceleration_per_second_squared: Scalar(100_000),
            cushion_restitution_millionths: 820_000,
            collision_restitution_millionths: 960_000,
            settling_speed_per_second: Scalar(8_000),
        }
    }

    /// Returns the profile version.
    #[must_use]
    pub const fn version(self) -> u16 {
        self.version
    }

    /// Returns the fixed simulation frequency.
    #[must_use]
    pub const fn ticks_per_second(self) -> u16 {
        self.ticks_per_second
    }

    /// Returns the wall-clock duration of `ticks` fixed simulation ticks in
    /// whole milliseconds, rounded up so playback is never under-budgeted.
    #[must_use]
    pub const fn playback_millis(self, ticks: u32) -> u64 {
        playback_millis(self, ticks)
    }

    /// Returns the maximum simulated ticks for one shot.
    #[must_use]
    pub const fn maximum_ticks(self) -> u32 {
        self.maximum_ticks
    }

    /// Returns the maximum initial linear speed per second.
    #[must_use]
    pub const fn maximum_speed_per_second(self) -> Scalar {
        self.maximum_speed_per_second
    }

    /// Returns rolling deceleration per second squared.
    #[must_use]
    pub const fn rolling_deceleration_per_second_squared(self) -> Scalar {
        self.rolling_deceleration_per_second_squared
    }

    /// Returns cushion restitution in millionths.
    #[must_use]
    pub const fn cushion_restitution_millionths(self) -> u32 {
        self.cushion_restitution_millionths
    }

    /// Returns equal-mass ball collision restitution in millionths.
    #[must_use]
    pub const fn collision_restitution_millionths(self) -> u32 {
        self.collision_restitution_millionths
    }

    /// Returns the speed at or below which a ball settles.
    #[must_use]
    pub const fn settling_speed_per_second(self) -> Scalar {
        self.settling_speed_per_second
    }
}

/// Invalid physics profile.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PhysicsProfileError {
    /// The profile version is unsupported.
    #[error("unsupported physics profile version {0}")]
    UnsupportedVersion(u16),
    /// Tick frequency or shot tick bound is zero.
    #[error("simulation timing bounds must be non-zero")]
    InvalidTiming,
    /// Maximum speed or rolling deceleration is non-positive.
    #[error("motion bounds must be positive")]
    InvalidMotionBound,
    /// A restitution coefficient exceeds one million millionths.
    #[error("restitution must be in 0..=1,000,000 millionths")]
    InvalidRestitution,
    /// Settling speed is negative or exceeds maximum speed.
    #[error("settling speed must be between zero and maximum speed")]
    InvalidSettlingSpeed,
}

/// Complete canonical state of one ball.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BallState {
    /// Stable ball identifier.
    pub id: BallId,
    /// Ball-center position.
    pub position: Vector,
    /// Linear velocity per second.
    pub velocity: Vector,
    /// Horizontal
    /// rolling surface velocity `(R*omega_y, -R*omega_x)`, in micro-metres/s.
    pub angular_velocity: Vector,
    /// Vertical-axis spin expressed as `R*omega_z`, in micro-metres/s.
    pub side_spin: Scalar,
    /// Whether the ball has entered a pocket and left play.
    pub pocketed: bool,
}

impl BallState {
    /// Creates a stationary in-play ball.
    #[must_use]
    pub const fn stationary(id: BallId, position: Vector) -> Self {
        Self {
            id,
            position,
            velocity: Vector::ZERO,
            angular_velocity: Vector::ZERO,
            side_spin: Scalar::from_micros(0),
            pocketed: false,
        }
    }
}

/// Complete versioned canonical table snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionedTableState {
    version: u16,
    tick: u32,
    balls: Vec<BallState>,
}

impl VersionedTableState {
    /// Creates a validated canonical table state.
    ///
    /// Balls are sorted by identifier so input collection ordering cannot
    /// affect simulation ordering.
    ///
    /// # Errors
    ///
    /// Returns [`TableStateError`] for an unsupported version, empty or
    /// oversized state, duplicate identifiers, out-of-bounds balls, overlapping
    /// in-play balls or moving pocketed balls.
    pub fn new(
        version: u16,
        geometry: TableGeometry,
        tick: u32,
        mut balls: Vec<BallState>,
    ) -> Result<Self, TableStateError> {
        if version != TABLE_STATE_VERSION {
            return Err(TableStateError::UnsupportedVersion(version));
        }
        if balls.is_empty() || balls.len() > MAX_BALLS {
            return Err(TableStateError::InvalidBallCount(balls.len()));
        }
        balls.sort_unstable_by_key(|ball| ball.id);
        for pair in balls.windows(2) {
            if pair[0].id == pair[1].id {
                return Err(TableStateError::DuplicateBall(pair[0].id));
            }
        }
        for (index, ball) in balls.iter().enumerate() {
            if ball.pocketed {
                if ball.velocity != Vector::ZERO {
                    return Err(TableStateError::MovingPocketedBall(ball.id));
                }
                continue;
            }
            if !inside_playable_bounds(ball.position, geometry) {
                return Err(TableStateError::BallOutOfBounds(ball.id));
            }
            for other in balls.iter().skip(index + 1).filter(|other| !other.pocketed) {
                if squared_distance(ball.position, other.position)
                    < squared_i128(geometry.ball_radius.0 * 2)
                {
                    return Err(TableStateError::OverlappingBalls(ball.id, other.id));
                }
            }
        }
        Ok(Self {
            version,
            tick,
            balls,
        })
    }

    /// Returns the snapshot version.
    #[must_use]
    pub const fn version(&self) -> u16 {
        self.version
    }

    /// Returns the elapsed simulation tick.
    #[must_use]
    pub const fn tick(&self) -> u32 {
        self.tick
    }

    /// Returns balls in stable identifier order.
    #[must_use]
    pub fn balls(&self) -> &[BallState] {
        &self.balls
    }

    /// Returns one ball by stable identifier.
    #[must_use]
    pub fn ball(&self, id: BallId) -> Option<&BallState> {
        self.balls
            .binary_search_by_key(&id, |ball| ball.id)
            .ok()
            .map(|index| &self.balls[index])
    }

    /// Encodes the complete canonical state into the stable version-one binary format.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(7 + self.balls.len() * 50);
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.tick.to_be_bytes());
        let count = u8::try_from(self.balls.len()).unwrap_or(u8::MAX);
        bytes.push(count);
        for ball in &self.balls {
            bytes.push(ball.id.number());
            bytes.push(u8::from(ball.pocketed));
            for value in [
                ball.position.x.0,
                ball.position.y.0,
                ball.velocity.x.0,
                ball.velocity.y.0,
                ball.angular_velocity.x.0,
                ball.angular_velocity.y.0,
            ] {
                bytes.extend_from_slice(&value.to_be_bytes());
            }
            bytes.extend_from_slice(&ball.side_spin.0.to_be_bytes());
        }
        bytes
    }

    /// Decodes and validates a complete canonical state.
    ///
    /// # Errors
    ///
    /// Returns [`TableStateDecodeError`] when bytes are truncated, contain
    /// trailing data or an invalid boolean/identifier, or decode to an invalid
    /// canonical state.
    pub fn from_bytes(
        geometry: TableGeometry,
        bytes: &[u8],
    ) -> Result<Self, TableStateDecodeError> {
        let mut reader = ByteReader::new(bytes);
        let version = reader.read_u16()?;
        if version != TABLE_STATE_VERSION {
            return Err(TableStateDecodeError::State(
                TableStateError::UnsupportedVersion(version),
            ));
        }
        let tick = reader.read_u32()?;
        let count = usize::from(reader.read_u8()?);
        if count == 0 || count > MAX_BALLS {
            return Err(TableStateDecodeError::State(
                TableStateError::InvalidBallCount(count),
            ));
        }
        let mut balls = Vec::with_capacity(count);
        for _ in 0..count {
            let id = BallId::new(reader.read_u8()?).map_err(TableStateDecodeError::BallId)?;
            let pocketed = match reader.read_u8()? {
                0 => false,
                1 => true,
                value => return Err(TableStateDecodeError::InvalidBoolean(value)),
            };
            balls.push(BallState {
                id,
                position: Vector::from_micros(reader.read_i64()?, reader.read_i64()?),
                velocity: Vector::from_micros(reader.read_i64()?, reader.read_i64()?),
                angular_velocity: Vector::from_micros(reader.read_i64()?, reader.read_i64()?),
                side_spin: Scalar::from_micros(reader.read_i64()?),
                pocketed,
            });
        }
        if !reader.is_finished() {
            return Err(TableStateDecodeError::TrailingBytes(reader.remaining()));
        }
        Self::new(version, geometry, tick, balls).map_err(TableStateDecodeError::State)
    }
}

/// Invalid canonical table state.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TableStateError {
    /// Snapshot version is unsupported.
    #[error("unsupported table state version {0}")]
    UnsupportedVersion(u16),
    /// Ball count is outside one through sixteen.
    #[error("table state contains invalid ball count {0}")]
    InvalidBallCount(usize),
    /// Two balls share an identifier.
    #[error("duplicate ball identifier {0:?}")]
    DuplicateBall(BallId),
    /// An in-play ball center is beyond the playable bounds.
    #[error("ball {0:?} is outside playable bounds")]
    BallOutOfBounds(BallId),
    /// Two in-play balls overlap.
    #[error("balls {0:?} and {1:?} overlap")]
    OverlappingBalls(BallId, BallId),
    /// A pocketed ball retains velocity.
    #[error("pocketed ball {0:?} must be stationary")]
    MovingPocketedBall(BallId),
}

/// Failure to decode a canonical table-state payload.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TableStateDecodeError {
    /// The payload ended before a complete value was available.
    #[error("table state payload is truncated")]
    Truncated,
    /// A ball number is outside the canonical range.
    #[error(transparent)]
    BallId(#[from] BallIdError),
    /// A boolean byte was not zero or one.
    #[error("invalid canonical boolean byte {0}")]
    InvalidBoolean(u8),
    /// The decoded state violates canonical state invariants.
    #[error(transparent)]
    State(#[from] TableStateError),
    /// Bytes remained after the declared ball records.
    #[error("table state payload has {0} trailing bytes")]
    TrailingBytes(usize),
}

struct ByteReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> ByteReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn read<const SIZE: usize>(&mut self) -> Result<[u8; SIZE], TableStateDecodeError> {
        let end = self
            .offset
            .checked_add(SIZE)
            .ok_or(TableStateDecodeError::Truncated)?;
        let source = self
            .bytes
            .get(self.offset..end)
            .ok_or(TableStateDecodeError::Truncated)?;
        let mut value = [0; SIZE];
        value.copy_from_slice(source);
        self.offset = end;
        Ok(value)
    }

    fn read_u8(&mut self) -> Result<u8, TableStateDecodeError> {
        Ok(self.read::<1>()?[0])
    }

    fn read_u16(&mut self) -> Result<u16, TableStateDecodeError> {
        Ok(u16::from_be_bytes(self.read()?))
    }

    fn read_u32(&mut self) -> Result<u32, TableStateDecodeError> {
        Ok(u32::from_be_bytes(self.read()?))
    }

    fn read_i64(&mut self) -> Result<i64, TableStateDecodeError> {
        Ok(i64::from_be_bytes(self.read()?))
    }

    const fn is_finished(&self) -> bool {
        self.offset == self.bytes.len()
    }

    const fn remaining(&self) -> usize {
        self.bytes.len() - self.offset
    }
}

/// Quantized aim angle in one-ten-thousandth of a turn.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Aim(u16);

impl Aim {
    /// Number of quantized aim steps in a full turn.
    pub const STEPS_PER_TURN: u16 = 10_000;

    /// Creates a canonical aim.
    ///
    /// # Errors
    ///
    /// Returns [`ShotCommandError::BadAim`] unless `steps` is below
    /// [`Self::STEPS_PER_TURN`].
    pub const fn new(steps: u16) -> Result<Self, ShotCommandError> {
        if steps < Self::STEPS_PER_TURN {
            Ok(Self(steps))
        } else {
            Err(ShotCommandError::BadAim(steps))
        }
    }

    /// Returns the quantized turn steps.
    #[must_use]
    pub const fn steps(self) -> u16 {
        self.0
    }
}

/// Quantized shot power from zero through ten thousand.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShotPower(u16);

impl ShotPower {
    /// Maximum canonical power value.
    pub const MAX: u16 = 10_000;

    /// Creates bounded canonical power.
    ///
    /// # Errors
    ///
    /// Returns [`ShotCommandError::ExcessivePower`] when `units` exceeds ten
    /// thousand.
    pub const fn new(units: u16) -> Result<Self, ShotCommandError> {
        if units <= Self::MAX {
            Ok(Self(units))
        } else {
            Err(ShotCommandError::ExcessivePower(units))
        }
    }

    /// Returns the canonical power units.
    #[must_use]
    pub const fn units(self) -> u16 {
        self.0
    }
}

/// Quantized cue-ball contact offset.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Spin {
    /// Left/right contact offset in ten-thousandths of the cue-ball radius.
    pub side: i16,
    /// Back/top contact offset in ten-thousandths of the cue-ball radius.
    pub vertical: i16,
}

impl Spin {
    /// No English.
    pub const CENTER: Self = Self {
        side: 0,
        vertical: 0,
    };

    /// Creates a bounded spin contact point inside the cue-ball contact disc.
    ///
    /// # Errors
    ///
    /// Returns [`ShotCommandError::ContactOutsideBall`] when either component is
    /// outside -10,000 through 10,000 or the point lies outside the contact disc.
    pub const fn new(side: i16, vertical: i16) -> Result<Self, ShotCommandError> {
        let side_squared = side as i32 * side as i32;
        let vertical_squared = vertical as i32 * vertical as i32;
        if side < -10_000
            || side > 10_000
            || vertical < -10_000
            || vertical > 10_000
            || side_squared + vertical_squared > 100_000_000
        {
            Err(ShotCommandError::ContactOutsideBall { side, vertical })
        } else {
            Ok(Self { side, vertical })
        }
    }
}

/// Versioned, bounded canonical shot input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VersionedShotCommand {
    /// Command schema version.
    pub version: u16,
    /// Quantized aim.
    pub aim: Aim,
    /// Quantized power.
    pub power: ShotPower,
    /// Quantized cue-ball contact point.
    pub spin: Spin,
}

impl VersionedShotCommand {
    /// Current command schema version.
    pub const VERSION: u16 = 1;

    /// Creates a current-version shot command.
    #[must_use]
    pub const fn new(aim: Aim, power: ShotPower, spin: Spin) -> Self {
        Self {
            version: Self::VERSION,
            aim,
            power,
            spin,
        }
    }
}

/// Invalid external shot input.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ShotCommandError {
    /// Aim is not below one full quantized turn.
    #[error("aim {0} is outside 0..10,000")]
    BadAim(u16),
    /// Power exceeds the canonical maximum.
    #[error("power {0} exceeds 10,000")]
    ExcessivePower(u16),
    /// Spin contact lies outside the cue-ball contact disc.
    #[error("spin contact ({side}, {vertical}) is outside the cue-ball disc")]
    ContactOutsideBall {
        /// Horizontal contact offset.
        side: i16,
        /// Vertical contact offset.
        vertical: i16,
    },
}

/// Canonical simulation event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SimulationEvent {
    /// Tick on which the event occurred.
    pub tick: u32,
    /// Stable event ordering within the tick.
    pub sequence: u16,
    /// Event payload.
    pub kind: SimulationEventKind,
}

/// Canonical billiards event payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimulationEventKind {
    /// A shot was applied to the cue ball.
    ShotStarted,
    /// Two balls contacted.
    BallContact { first: BallId, second: BallId },
    /// A ball contacted a cushion axis.
    CushionContact { ball: BallId, axis: CushionAxis },
    /// A ball entered a pocket.
    BallPocketed { ball: BallId, pocket: PocketId },
    /// Every remaining in-play ball settled.
    Settled,
}

/// Cushion axis used by a canonical contact event.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum CushionAxis {
    /// Left or right cushion.
    Horizontal,
    /// Top or bottom cushion.
    Vertical,
}

/// Complete result of one simulated shot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SimulationResult {
    /// Settled canonical state.
    pub state: VersionedTableState,
    /// Canonically ordered shot events.
    pub events: Vec<SimulationEvent>,
}

/// Result of advancing an in-flight canonical simulation by one fixed tick.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TickResult {
    /// Canonical state after the tick.
    pub state: VersionedTableState,
    /// Canonically ordered events emitted during the tick.
    pub events: Vec<SimulationEvent>,
    /// Whether every in-play ball has settled.
    pub settled: bool,
}

/// Advances an in-flight canonical table state by exactly one fixed tick.
///
/// This operation is the replay and rollback continuation boundary: callers can
/// serialize a moving [`VersionedTableState`], restore it, and continue from the
/// same tick without retaining hidden solver state.
///
/// # Errors
///
/// Returns [`SimulationError::EventLimitExceeded`] if one tick exceeds the
/// fixed canonical event bound.
pub fn advance_tick(
    geometry: TableGeometry,
    profile: PhysicsProfile,
    mut state: VersionedTableState,
) -> Result<TickResult, SimulationError> {
    state.tick = state.tick.saturating_add(1);
    let mut kinds = Vec::new();
    motion::advance(geometry, profile, &mut state, &mut kinds);
    kinds.sort_unstable_by_key(|event| event_sort_key(*event));
    let mut events = Vec::with_capacity(kinds.len() + 1);
    for (sequence, kind) in kinds.into_iter().enumerate() {
        push_event(
            &mut events,
            SimulationEvent {
                tick: state.tick,
                sequence: u16::try_from(sequence).unwrap_or(u16::MAX),
                kind,
            },
        )?;
    }
    let settled = is_settled(&state.balls, profile);
    if settled {
        for ball in &mut state.balls {
            ball.velocity = Vector::ZERO;
            ball.angular_velocity = Vector::ZERO;
            ball.side_spin = Scalar::from_micros(0);
        }
        let sequence = u16::try_from(events.len()).unwrap_or(u16::MAX);
        push_event(
            &mut events,
            SimulationEvent {
                tick: state.tick,
                sequence,
                kind: SimulationEventKind::Settled,
            },
        )?;
    }
    Ok(TickResult {
        state,
        events,
        settled,
    })
}

/// Converts fixed simulation ticks under `physics` into whole milliseconds,
/// computed as `ceil(ticks * 1000 / ticks_per_second)`.
#[must_use]
pub const fn playback_millis(physics: PhysicsProfile, ticks: u32) -> u64 {
    // `ticks_per_second` is validated non-zero by every constructor.
    (ticks as u64 * 1_000).div_ceil(physics.ticks_per_second as u64)
}

/// Canonical shot simulation failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SimulationError {
    /// The command version is unsupported.
    #[error("unsupported shot command version {0}")]
    UnsupportedCommandVersion(u16),
    /// The cue ball is absent from the state.
    #[error("canonical state has no cue ball")]
    MissingCueBall,
    /// The cue ball is already pocketed.
    #[error("cannot shoot a pocketed cue ball")]
    CueBallPocketed,
    /// One or more balls are already moving.
    #[error("shot requires a settled table")]
    TableMoving,
    /// The bounded simulation did not settle.
    #[error("shot did not settle within {0} ticks")]
    DidNotSettle(u32),
    /// Canonical event storage exceeded its fixed safety bound.
    #[error("shot exceeded the canonical event limit")]
    EventLimitExceeded,
}

/// Starts a bounded shot without advancing time, for fixed-tick playback.
///
/// # Errors
///
/// Returns [`SimulationError`] for an unsupported command version, a moving
/// table, or an absent or pocketed cue ball.
pub fn start_shot(
    profile: PhysicsProfile,
    mut state: VersionedTableState,
    command: VersionedShotCommand,
) -> Result<VersionedTableState, SimulationError> {
    if command.version != VersionedShotCommand::VERSION {
        return Err(SimulationError::UnsupportedCommandVersion(command.version));
    }
    if state.balls.iter().any(|ball| ball.velocity != Vector::ZERO) {
        return Err(SimulationError::TableMoving);
    }
    let cue_index = state
        .balls
        .binary_search_by_key(&BallId::CUE, |ball| ball.id)
        .map_err(|_| SimulationError::MissingCueBall)?;
    if state.balls[cue_index].pocketed {
        return Err(SimulationError::CueBallPocketed);
    }

    state.balls[cue_index].velocity = shot_velocity(profile, command);
    motion::strike(&mut state.balls[cue_index], command);
    Ok(state)
}

/// Simulates a complete bounded shot using canonical fixed-tick physics.
///
/// # Errors
///
/// Returns [`SimulationError`] if the command or table cannot start a shot,
/// the event limit is exceeded, or the shot fails to settle in time.
pub fn simulate_shot(
    geometry: TableGeometry,
    profile: PhysicsProfile,
    state: VersionedTableState,
    command: VersionedShotCommand,
) -> Result<SimulationResult, SimulationError> {
    let mut state = start_shot(profile, state, command)?;
    let mut events = Vec::new();
    push_event(
        &mut events,
        SimulationEvent {
            tick: state.tick,
            sequence: 0,
            kind: SimulationEventKind::ShotStarted,
        },
    )?;

    if command.power.units() == 0 {
        push_event(
            &mut events,
            SimulationEvent {
                tick: state.tick,
                sequence: 1,
                kind: SimulationEventKind::Settled,
            },
        )?;
        return Ok(SimulationResult { state, events });
    }

    for _ in 0..profile.maximum_ticks {
        let tick = advance_tick(geometry, profile, state)?;
        state = tick.state;
        for event in tick.events {
            push_event(&mut events, event)?;
        }
        if tick.settled {
            return Ok(SimulationResult { state, events });
        }
    }

    Err(SimulationError::DidNotSettle(profile.maximum_ticks))
}

fn shot_velocity(profile: PhysicsProfile, command: VersionedShotCommand) -> Vector {
    let speed = mul_div(
        profile.maximum_speed_per_second.0,
        i64::from(command.power.units()),
        i64::from(ShotPower::MAX),
    );
    let (cosine, sine) = direction(command.aim);
    Vector::from_micros(
        mul_div(speed, cosine, TRIG_SCALE),
        mul_div(speed, sine, TRIG_SCALE),
    )
}

fn direction(aim: Aim) -> (i64, i64) {
    let turn = i64::from(aim.steps()) * 1_000_000_000 / i64::from(Aim::STEPS_PER_TURN);
    let quadrant = turn / 250_000_000;
    let reduced = turn % 250_000_000;
    let (cosine, sine) = cordic_quarter_turn(reduced);
    match quadrant {
        0 => (cosine, sine),
        1 => (-sine, cosine),
        2 => (-cosine, -sine),
        _ => (sine, -cosine),
    }
}

fn cordic_quarter_turn(mut angle: i64) -> (i64, i64) {
    let mut x = CORDIC_GAIN_INVERSE;
    let mut y = 0_i64;
    for (index, step) in CORDIC_ANGLES.into_iter().enumerate() {
        let previous_x = x;
        if angle >= 0 {
            x -= y >> index;
            y += previous_x >> index;
            angle -= step;
        } else {
            x += y >> index;
            y -= previous_x >> index;
            angle += step;
        }
    }
    (x, y)
}

fn resolve_pockets(
    balls: &mut [BallState],
    geometry: TableGeometry,
    events: &mut Vec<SimulationEventKind>,
) {
    for ball in balls.iter_mut().filter(|ball| !ball.pocketed) {
        if let Some(pocket) = PocketId::ALL.into_iter().find(|pocket| {
            let center = pocket_position(*pocket, geometry);
            let x = ball.position.x.0 * center.x.0.signum();
            let y = ball.position.y.0 * center.y.0.signum();
            if center.x.0 == 0 {
                ball.position.x.0.abs() <= geometry.pocket_radius.0 * 3 / 2
                    && y >= geometry.half_height.0
            } else {
                x >= geometry.half_width.0 - geometry.pocket_radius.0 * 9 / 5
                    && y >= geometry.half_height.0 - geometry.pocket_radius.0 * 9 / 5
                    && x + y
                        >= geometry.half_width.0 + geometry.half_height.0 - geometry.pocket_radius.0
            }
        }) {
            ball.pocketed = true;
            ball.velocity = Vector::ZERO;
            ball.angular_velocity = Vector::ZERO;
            events.push(SimulationEventKind::BallPocketed {
                ball: ball.id,
                pocket,
            });
        }
    }
}

fn separate_overlap(
    first: &mut BallState,
    second: &mut BallState,
    diameter: i64,
    dx: i64,
    dy: i64,
    distance_squared: i128,
) {
    if distance_squared == 0 {
        first.position.x.0 -= diameter / 2;
        second.position.x.0 += diameter - diameter / 2;
        return;
    }
    let distance = integer_sqrt(distance_squared);
    if distance >= diameter {
        return;
    }
    let overlap = diameter - distance;
    let correction_x = mul_div(overlap, dx, distance.max(1));
    let correction_y = mul_div(overlap, dy, distance.max(1));
    first.position.x.0 -= correction_x / 2;
    first.position.y.0 -= correction_y / 2;
    second.position.x.0 += correction_x - correction_x / 2;
    second.position.y.0 += correction_y - correction_y / 2;
}

fn is_settled(balls: &[BallState], profile: PhysicsProfile) -> bool {
    let threshold_squared = squared_i128(profile.settling_speed_per_second.0);
    balls.iter().all(|ball| {
        ball.pocketed
            || (squared_i128(ball.velocity.x.0) + squared_i128(ball.velocity.y.0)
                <= threshold_squared
                && squared_i128(ball.angular_velocity.x.0)
                    + squared_i128(ball.angular_velocity.y.0)
                    <= threshold_squared
                && squared_i128(ball.side_spin.0) <= threshold_squared)
    })
}

fn push_event(
    events: &mut Vec<SimulationEvent>,
    event: SimulationEvent,
) -> Result<(), SimulationError> {
    if events.len() >= MAX_EVENTS {
        return Err(SimulationError::EventLimitExceeded);
    }
    events.push(event);
    Ok(())
}

const fn event_sort_key(event: SimulationEventKind) -> (u8, u8, u8, u8) {
    match event {
        SimulationEventKind::BallPocketed { ball, pocket } => (0, ball.number(), pocket as u8, 0),
        SimulationEventKind::CushionContact { ball, axis } => (1, ball.number(), axis as u8, 0),
        SimulationEventKind::BallContact { first, second } => {
            (2, first.number(), second.number(), 0)
        }
        SimulationEventKind::ShotStarted => (3, 0, 0, 0),
        SimulationEventKind::Settled => (4, 0, 0, 0),
    }
}

const fn inside_playable_bounds(position: Vector, geometry: TableGeometry) -> bool {
    let max_x = geometry.half_width.0 - geometry.ball_radius.0;
    let max_y = geometry.half_height.0 - geometry.ball_radius.0;
    (position.x.0.abs() <= geometry.half_width.0
        && position.y.0.abs() <= geometry.half_height.0
        && ((position.x.0.abs() > max_x && geometry.rail_opening(false, position.y))
            || (position.y.0.abs() > max_y && geometry.rail_opening(true, position.x))))
        || (position.x.0 >= -max_x
            && position.x.0 <= max_x
            && position.y.0 >= -max_y
            && position.y.0 <= max_y)
}

const fn pocket_position(pocket: PocketId, geometry: TableGeometry) -> Vector {
    match pocket {
        PocketId::TopLeft => Vector::from_micros(-geometry.half_width.0, geometry.half_height.0),
        PocketId::TopCenter => Vector::from_micros(0, geometry.half_height.0),
        PocketId::TopRight => Vector::from_micros(geometry.half_width.0, geometry.half_height.0),
        PocketId::BottomLeft => {
            Vector::from_micros(-geometry.half_width.0, -geometry.half_height.0)
        }
        PocketId::BottomCenter => Vector::from_micros(0, -geometry.half_height.0),
        PocketId::BottomRight => {
            Vector::from_micros(geometry.half_width.0, -geometry.half_height.0)
        }
    }
}

const fn squared_i128(value: i64) -> i128 {
    value as i128 * value as i128
}

const fn squared_distance(first: Vector, second: Vector) -> i128 {
    squared_i128(second.x.0 - first.x.0) + squared_i128(second.y.0 - first.y.0)
}

fn integer_sqrt(value: i128) -> i64 {
    if value <= 0 {
        return 0;
    }
    let mut estimate = value;
    let mut next = i128::midpoint(estimate, value / estimate);
    while next < estimate {
        estimate = next;
        next = i128::midpoint(estimate, value / estimate);
    }
    i64::try_from(estimate).unwrap_or(i64::MAX)
}

fn mul_div(value: i64, multiplier: i64, divisor: i64) -> i64 {
    i64::try_from(i128::from(value) * i128::from(multiplier) / i128::from(divisor)).unwrap_or_else(
        |_| {
            let numerator_non_negative = (value >= 0) == (multiplier >= 0);
            if numerator_non_negative == (divisor >= 0) {
                i64::MAX
            } else {
                i64::MIN
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(balls: Vec<BallState>) -> VersionedTableState {
        VersionedTableState::new(TABLE_STATE_VERSION, TableGeometry::standard(), 0, balls)
            .expect("fixture is valid")
    }

    fn command(aim: u16, power: u16) -> VersionedShotCommand {
        VersionedShotCommand::new(
            Aim::new(aim).expect("aim is valid"),
            ShotPower::new(power).expect("power is valid"),
            Spin::CENTER,
        )
    }

    #[test]
    fn identifiers_and_inputs_reject_out_of_range_values() {
        assert_eq!(BallId::new(16), Err(BallIdError::OutOfRange(16)));
        assert_eq!(Aim::new(10_000), Err(ShotCommandError::BadAim(10_000)));
        assert_eq!(
            ShotPower::new(10_001),
            Err(ShotCommandError::ExcessivePower(10_001))
        );
        assert!(matches!(
            Spin::new(10_000, 1),
            Err(ShotCommandError::ContactOutsideBall { .. })
        ));
    }

    #[test]
    fn fixed_point_direction_covers_cardinal_axes() {
        assert_eq!(direction(Aim::new(0).unwrap()), (1_000_002, 3));
        assert_eq!(direction(Aim::new(2_500).unwrap()), (-3, 1_000_002));
        assert_eq!(direction(Aim::new(5_000).unwrap()), (-1_000_002, -3));
        assert_eq!(direction(Aim::new(7_500).unwrap()), (3, -1_000_002));
    }

    #[test]
    fn state_is_sorted_and_rejects_invalid_snapshots() {
        let cue = BallState::stationary(BallId::CUE, Vector::from_micros(-300_000, 0));
        let one = BallState::stationary(BallId::new(1).unwrap(), Vector::from_micros(300_000, 0));
        let canonical = state(vec![one, cue]);
        assert_eq!(canonical.balls()[0].id, BallId::CUE);
        assert_eq!(canonical.balls()[1].id, BallId::new(1).unwrap());
        assert_eq!(canonical.clone(), canonical);

        assert!(matches!(
            VersionedTableState::new(
                TABLE_STATE_VERSION,
                TableGeometry::standard(),
                0,
                vec![cue, cue]
            ),
            Err(TableStateError::DuplicateBall(BallId::CUE))
        ));
    }

    #[test]
    fn canonical_state_binary_round_trip_is_exact() {
        let original = state(vec![
            BallState::stationary(BallId::CUE, Vector::from_micros(-300_000, 10_000)),
            BallState::stationary(
                BallId::new(1).unwrap(),
                Vector::from_micros(300_000, -10_000),
            ),
        ]);
        let bytes = original.to_bytes();
        assert_eq!(
            VersionedTableState::from_bytes(TableGeometry::standard(), &bytes),
            Ok(original)
        );
        assert_eq!(
            VersionedTableState::from_bytes(TableGeometry::standard(), &bytes[..bytes.len() - 1]),
            Err(TableStateDecodeError::Truncated)
        );
        let mut trailing = bytes;
        trailing.push(0);
        assert_eq!(
            VersionedTableState::from_bytes(TableGeometry::standard(), &trailing),
            Err(TableStateDecodeError::TrailingBytes(1))
        );
    }

    #[test]
    fn moving_snapshot_round_trip_continues_identically() {
        let geometry = TableGeometry::standard();
        let profile = PhysicsProfile::standard();
        let moving = VersionedTableState::new(
            TABLE_STATE_VERSION,
            geometry,
            12,
            vec![BallState {
                id: BallId::CUE,
                position: Vector::from_micros(-300_000, 20_000),
                velocity: Vector::from_micros(1_000_000, 200_000),
                angular_velocity: Vector::from_micros(100_000, -100_000),
                side_spin: Scalar::from_micros(0),
                pocketed: false,
            }],
        )
        .expect("moving state is valid");
        let restored = VersionedTableState::from_bytes(geometry, &moving.to_bytes())
            .expect("moving state round trips");
        assert_eq!(
            advance_tick(geometry, profile, moving).unwrap(),
            advance_tick(geometry, profile, restored).unwrap()
        );
    }

    #[test]
    fn stationary_shot_settles_without_moving() {
        let initial = state(vec![BallState::stationary(BallId::CUE, Vector::ZERO)]);
        let result = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial.clone(),
            command(0, 0),
        )
        .expect("zero-power shot settles");
        assert_eq!(result.state, initial);
        assert_eq!(result.events.len(), 2);
        assert!(matches!(
            result.events[0].kind,
            SimulationEventKind::ShotStarted
        ));
        assert!(matches!(
            result.events[1].kind,
            SimulationEventKind::Settled
        ));
    }

    #[test]
    fn single_ball_motion_is_repeatable() {
        let initial = state(vec![BallState::stationary(
            BallId::CUE,
            Vector::from_micros(-500_000, 0),
        )]);
        let first = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial.clone(),
            command(0, 2_000),
        )
        .expect("shot settles");
        let second = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial,
            command(0, 2_000),
        )
        .expect("shot settles");
        assert_eq!(first, second);
        assert!(first.state.ball(BallId::CUE).unwrap().position.x.micros() > -500_000);
    }

    #[test]
    fn head_on_contact_transfers_motion() {
        let initial = state(vec![
            BallState::stationary(BallId::CUE, Vector::from_micros(-400_000, 0)),
            BallState::stationary(BallId::new(1).unwrap(), Vector::from_micros(0, 0)),
        ]);
        let result = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial,
            command(0, 2_000),
        )
        .expect("shot settles");
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            SimulationEventKind::BallContact { first, second }
                if first == BallId::CUE && second == BallId::new(1).unwrap()
        )));
        assert!(
            result
                .state
                .ball(BallId::new(1).unwrap())
                .unwrap()
                .position
                .x
                .micros()
                > 0
        );
    }

    #[test]
    fn cushion_contact_is_bounded_and_recorded() {
        let initial = state(vec![BallState::stationary(
            BallId::CUE,
            Vector::from_micros(1_000_000, 200_000),
        )]);
        let result = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial,
            command(0, 3_000),
        )
        .expect("shot settles");
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            SimulationEventKind::CushionContact {
                ball: BallId::CUE,
                axis: CushionAxis::Horizontal
            }
        )));
        assert!(inside_playable_bounds(
            result.state.ball(BallId::CUE).unwrap().position,
            TableGeometry::standard()
        ));
    }

    #[test]
    fn ball_can_enter_a_pocket() {
        let geometry = TableGeometry::standard();
        let initial = state(vec![BallState::stationary(
            BallId::CUE,
            Vector::from_micros(0, 500_000),
        )]);
        let result = simulate_shot(
            geometry,
            PhysicsProfile::standard(),
            initial,
            command(2_500, 2_000),
        )
        .expect("shot settles");
        assert!(result.state.ball(BallId::CUE).unwrap().pocketed);
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            SimulationEventKind::BallPocketed {
                ball: BallId::CUE,
                pocket: PocketId::TopCenter
            }
        )));
    }

    #[test]
    fn side_spin_changes_cushion_rebound() {
        let initial = state(vec![BallState::stationary(
            BallId::CUE,
            Vector::from_micros(1_000_000, 0),
        )]);
        let center = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial.clone(),
            command(0, 3_000),
        )
        .unwrap();
        let side = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial,
            VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(3_000).unwrap(),
                Spin::new(8_000, 0).unwrap(),
            ),
        )
        .unwrap();
        assert_ne!(
            center.state.ball(BallId::CUE).unwrap().position.y,
            side.state.ball(BallId::CUE).unwrap().position.y
        );
    }

    #[test]
    fn top_spin_changes_object_ball_transfer() {
        let initial = state(vec![
            BallState::stationary(BallId::CUE, Vector::from_micros(-400_000, 0)),
            BallState::stationary(BallId::new(1).unwrap(), Vector::ZERO),
        ]);
        let center = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial.clone(),
            command(0, 2_000),
        )
        .unwrap();
        let top = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            initial,
            VersionedShotCommand::new(
                Aim::new(0).unwrap(),
                ShotPower::new(2_000).unwrap(),
                Spin::new(0, 8_000).unwrap(),
            ),
        )
        .unwrap();
        assert_ne!(
            center
                .state
                .ball(BallId::new(1).unwrap())
                .unwrap()
                .position
                .x,
            top.state.ball(BallId::new(1).unwrap()).unwrap().position.x
        );
    }

    #[test]
    fn maximum_power_break_does_not_tunnel_through_rack_front() {
        let mut balls = vec![BallState::stationary(
            BallId::CUE,
            Vector::from_micros(-800_000, 0),
        )];
        for row in 0_i64..5 {
            for column in 0_i64..=row {
                let number = u8::try_from(row * (row + 1) / 2 + column + 1).unwrap();
                balls.push(BallState::stationary(
                    BallId::new(number).unwrap(),
                    Vector::from_micros(200_000 + row * 50_000, (column * 2 - row) * 30_000),
                ));
            }
        }
        let result = simulate_shot(
            TableGeometry::standard(),
            PhysicsProfile::standard(),
            state(balls),
            command(0, ShotPower::MAX),
        )
        .expect("maximum-power break settles");
        assert!(result.events.iter().any(|event| matches!(
            event.kind,
            SimulationEventKind::BallContact {
                first: BallId::CUE,
                second
            } if second == BallId::new(1).unwrap()
        )));
    }
}
