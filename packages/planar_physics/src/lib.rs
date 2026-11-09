#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]
//! Deterministic, integer fixed-point planar physics for live table games.
//!
//! A [`World`] holds dynamic discs (pucks, balls, carts), kinematic discs
//! driven toward a target at a bounded speed (pointer-driven mallets, rods),
//! static wall segments and sensor regions. Shaped bodies
//! ([`World::add_shaped_body`]) add oriented boxes, capsules and compounds of
//! several parts with rotation, kinematic angle control (foosball rods,
//! pinball flippers), steering drives (bumper carts) and bumper kicks; a
//! world-wide or regional acceleration field tilts the table (pinball,
//! marble lanes). [`World::step`] advances exactly one fixed tick, splitting
//! it into enough substeps that no body moves more than half its thickness
//! per substep, and reports contact and sensor events in a canonical order.
//!
//! All state is integers: distances in micro-units (one table unit, such as a
//! metre, is [`MICROS_PER_UNIT`]), velocities in micro-units per second,
//! ratios in millionths. Bodies, walls and sensors are processed in insertion
//! order, so a native server and WASM clients compute identical bits. The
//! crate depends on no renderer, transport, persistence or identity.

mod codec;
mod contact;
mod shape;
mod trig;
mod world;

#[cfg(test)]
mod shape_tests;
#[cfg(test)]
mod tests;

use std::ops::{Add, Neg, Sub};

use thiserror::Error;

pub use codec::{DISC_WORLD_STATE_VERSION, DecodeError, WORLD_STATE_VERSION};
pub use shape::{
    Attachment, Drive, FieldDef, MAX_ANGULAR_SPEED, MAX_EXTENT, MAX_PARTS, MAX_SURFACE_FRICTION,
    Part, PartPose, Shape, ShapedBodyDef,
};
pub use trig::{PI_MICRORADIANS, TRIG_UNIT, TURN_MICRORADIANS, rotate, sin_cos, wrap_angle};
pub use world::{Body, World};

/// Micro-units in one table unit (for example, micrometres per metre).
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// The denominator of every ratio expressed in millionths.
pub const MILLIONTHS: u32 = 1_000_000;

/// Largest supported coordinate or velocity magnitude, in micro-units
/// (one million table units), keeping all intermediate products in range.
pub const MAX_MAGNITUDE: i64 = 1_000_000_000_000;

/// A two-dimensional integer vector: a position in micro-units or a velocity
/// in micro-units per second.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Vector {
    /// Horizontal component.
    pub x: i64,
    /// Vertical component.
    pub y: i64,
}

impl Vector {
    /// The zero vector.
    pub const ZERO: Self = Self { x: 0, y: 0 };

    /// Creates a vector from micro-unit components.
    #[must_use]
    pub const fn new(x: i64, y: i64) -> Self {
        Self { x, y }
    }

    /// The exact dot product.
    #[must_use]
    pub const fn dot(self, other: Self) -> i128 {
        self.x as i128 * other.x as i128 + self.y as i128 * other.y as i128
    }

    /// The exact squared length.
    #[must_use]
    pub const fn length_squared(self) -> i128 {
        self.dot(self)
    }

    /// The length, rounded down.
    #[must_use]
    pub fn length(self) -> i64 {
        integer_sqrt(self.length_squared())
    }

    /// The largest component magnitude.
    #[must_use]
    pub const fn max_abs(self) -> i64 {
        let x = self.x.saturating_abs();
        let y = self.y.saturating_abs();
        if x > y { x } else { y }
    }

    /// Scales by `numerator / denominator`, rounding half away from zero.
    /// `denominator` must not be zero.
    #[must_use]
    pub fn scale(self, numerator: i128, denominator: i128) -> Self {
        Self {
            x: div_round(i128::from(self.x) * numerator, denominator),
            y: div_round(i128::from(self.y) * numerator, denominator),
        }
    }

    /// The same direction with at most `limit` length.
    #[must_use]
    pub fn clamp_length(self, limit: i64) -> Self {
        let limit = limit.max(0);
        let length_squared = self.length_squared();
        if length_squared <= i128::from(limit) * i128::from(limit) {
            return self;
        }
        let length = integer_sqrt(length_squared).max(1);
        self.scale(i128::from(limit), i128::from(length))
    }
}

impl Add for Vector {
    type Output = Self;

    fn add(self, other: Self) -> Self {
        Self::new(
            self.x.saturating_add(other.x),
            self.y.saturating_add(other.y),
        )
    }
}

impl Sub for Vector {
    type Output = Self;

    fn sub(self, other: Self) -> Self {
        Self::new(
            self.x.saturating_sub(other.x),
            self.y.saturating_sub(other.y),
        )
    }
}

impl Neg for Vector {
    type Output = Self;

    fn neg(self) -> Self {
        Self::new(self.x.saturating_neg(), self.y.saturating_neg())
    }
}

/// An axis-aligned rectangle in micro-units, inclusive of its edges.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Aabb {
    min: Vector,
    max: Vector,
}

impl Aabb {
    /// Creates a rectangle from two opposite corners.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidRegion`] when `min` exceeds `max` on an
    /// axis, and [`WorldError::OutOfRange`] when a coordinate exceeds
    /// [`MAX_MAGNITUDE`].
    pub fn new(min: Vector, max: Vector) -> Result<Self, WorldError> {
        check_vector(min)?;
        check_vector(max)?;
        if min.x > max.x || min.y > max.y {
            return Err(WorldError::InvalidRegion);
        }
        Ok(Self { min, max })
    }

    /// The minimum corner.
    #[must_use]
    pub const fn min(self) -> Vector {
        self.min
    }

    /// The maximum corner.
    #[must_use]
    pub const fn max(self) -> Vector {
        self.max
    }

    /// Whether `point` lies inside or on the edge.
    #[must_use]
    pub const fn contains(self, point: Vector) -> bool {
        point.x >= self.min.x
            && point.x <= self.max.x
            && point.y >= self.min.y
            && point.y <= self.max.y
    }

    /// The nearest point inside the rectangle.
    #[must_use]
    pub const fn clamp(self, point: Vector) -> Vector {
        Vector::new(
            clamp_i64(point.x, self.min.x, self.max.x),
            clamp_i64(point.y, self.min.y, self.max.y),
        )
    }
}

/// Identifies a body by insertion order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BodyId(pub(crate) u16);

/// Identifies a wall by insertion order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WallId(pub(crate) u16);

/// Identifies a sensor by insertion order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SensorId(pub(crate) u16);

/// Identifies an acceleration field region by insertion order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FieldId(pub(crate) u16);

macro_rules! id_accessors {
    ($($id:ident),*) => {$(
        impl $id {
            /// The insertion index.
            #[must_use]
            pub const fn index(self) -> u16 {
                self.0
            }
        }
    )*};
}

id_accessors!(BodyId, WallId, SensorId, FieldId);

/// Immutable world parameters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WorldConfig {
    /// Fixed ticks per second; at least one.
    pub ticks_per_second: u16,
    /// Most substeps per tick; at least one. Together with the tick rate it
    /// bounds the smallest body radius for [`WorldConfig::max_speed`].
    pub max_substeps: u8,
    /// Speed no body exceeds, in micro-units per second.
    pub max_speed: i64,
}

impl WorldConfig {
    /// The smallest body radius that cannot tunnel at
    /// [`WorldConfig::max_speed`]: at most half a radius per substep.
    #[must_use]
    pub fn min_radius(self) -> i64 {
        let per_tick =
            i128::from(self.ticks_per_second.max(1)) * i128::from(self.max_substeps.max(1));
        i64::try_from(ceil_div(i128::from(self.max_speed) * 2, per_tick)).unwrap_or(i64::MAX)
    }
}

/// How a body moves.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum BodyKind {
    /// Moved by its velocity and collisions.
    Dynamic,
    /// Moved toward its target at up to its maximum speed; pushes dynamic
    /// bodies as if infinitely heavy and ignores walls.
    Kinematic,
}

/// Everything needed to add a body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BodyDef {
    /// How the body moves.
    pub kind: BodyKind,
    /// Disc radius in micro-units.
    pub radius: i64,
    /// Mass in arbitrary positive units (for example grams); ignored for
    /// kinematic bodies.
    pub mass: i64,
    /// Initial center.
    pub position: Vector,
    /// Initial velocity (dynamic bodies).
    pub velocity: Vector,
    /// Bounciness in millionths; a contact uses the smaller of the two.
    pub restitution: u32,
    /// Fraction of speed lost per second, in millionths (air drag).
    pub damping: u32,
    /// Constant deceleration in micro-units per second squared (table
    /// friction).
    pub friction: i64,
    /// Speed cap in micro-units per second: a kinematic body's chase speed,
    /// a dynamic body's terminal speed. Never above the world's.
    pub max_speed: i64,
    /// Region the center is held inside, such as a player's half.
    pub confine: Option<Aabb>,
    /// Layers this body occupies.
    pub layers: u32,
    /// Layers this body collides with or is sensed by.
    pub mask: u32,
}

impl BodyDef {
    /// A dynamic disc with full restitution, no damping or friction, the
    /// world's speed limit and every layer.
    #[must_use]
    pub const fn dynamic(radius: i64, mass: i64, position: Vector) -> Self {
        Self {
            kind: BodyKind::Dynamic,
            radius,
            mass,
            position,
            velocity: Vector::ZERO,
            restitution: MILLIONTHS,
            damping: 0,
            friction: 0,
            max_speed: i64::MAX,
            confine: None,
            layers: u32::MAX,
            mask: u32::MAX,
        }
    }

    /// A kinematic disc chasing its target at up to `max_speed`.
    #[must_use]
    pub const fn kinematic(radius: i64, position: Vector, max_speed: i64) -> Self {
        Self {
            kind: BodyKind::Kinematic,
            mass: 1,
            max_speed,
            ..Self::dynamic(radius, 1, position)
        }
    }
}

/// A static wall segment.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WallDef {
    /// One end, in micro-units.
    pub start: Vector,
    /// The other end, in micro-units.
    pub end: Vector,
    /// Bounciness in millionths.
    pub restitution: u32,
    /// Layers the wall occupies; it stops dynamic bodies whose mask matches.
    pub layers: u32,
}

/// A sensor's area.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Region {
    /// A rectangle.
    Rect(Aabb),
    /// A circle.
    Circle {
        /// Center in micro-units.
        center: Vector,
        /// Radius in micro-units.
        radius: i64,
    },
}

impl Region {
    /// Whether `point` lies inside or on the edge.
    #[must_use]
    pub const fn contains(self, point: Vector) -> bool {
        match self {
            Self::Rect(rect) => rect.contains(point),
            Self::Circle { center, radius } => {
                let dx = point.x as i128 - center.x as i128;
                let dy = point.y as i128 - center.y as i128;
                dx * dx + dy * dy <= radius as i128 * radius as i128
            }
        }
    }
}

/// A region that reports bodies whose center enters or leaves it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SensorDef {
    /// The area.
    pub region: Region,
    /// Layers sensed: a body is sensed when its layers intersect this mask.
    pub mask: u32,
}

/// Something that happened during a tick.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Event {
    /// Two bodies collided; `first` has the lower index. Repeated contacts
    /// of the same pair within a tick are merged, keeping the fastest.
    BodyContact {
        /// The lower-indexed body.
        first: BodyId,
        /// The higher-indexed body.
        second: BodyId,
        /// Approach speed along the contact normal, micro-units per second.
        speed: i64,
    },
    /// A body hit a wall. Repeats within a tick are merged.
    WallContact {
        /// The body.
        body: BodyId,
        /// The wall.
        wall: WallId,
        /// Approach speed along the contact normal, micro-units per second.
        speed: i64,
    },
    /// A body's center entered a sensor.
    SensorEnter {
        /// The sensor.
        sensor: SensorId,
        /// The body.
        body: BodyId,
    },
    /// A body's center left a sensor (or the body was deactivated).
    SensorExit {
        /// The sensor.
        sensor: SensorId,
        /// The body.
        body: BodyId,
    },
    /// Detail of a [`Event::BodyContact`] involving a shaped body: which
    /// parts touched, where and along which normal. Merged per part pair
    /// within a tick, keeping the fastest.
    PartContact {
        /// The lower-indexed body.
        first: BodyId,
        /// The part of `first` (zero for a plain disc).
        first_part: u8,
        /// The higher-indexed body.
        second: BodyId,
        /// The part of `second` (zero for a plain disc).
        second_part: u8,
        /// The contact point in micro-units.
        point: Vector,
        /// The unit contact normal from `first` toward `second`, scaled by
        /// [`TRIG_UNIT`].
        normal: Vector,
        /// Approach speed along the normal at the contact point, micro-units
        /// per second.
        speed: i64,
    },
    /// Detail of a [`Event::WallContact`] of a shaped body. Merged per part
    /// and wall within a tick, keeping the fastest.
    PartWallContact {
        /// The body.
        body: BodyId,
        /// The part that touched.
        part: u8,
        /// The wall.
        wall: WallId,
        /// The contact point in micro-units.
        point: Vector,
        /// The unit normal from the wall toward the body, scaled by
        /// [`TRIG_UNIT`].
        normal: Vector,
        /// Approach speed along the normal, micro-units per second.
        speed: i64,
    },
    /// A kicking part fired: a dynamic body started touching it and was
    /// kicked away. Fires once per contact; the body must separate before it
    /// can fire again.
    Bumped {
        /// The kinematic body owning the kicking part.
        bumper: BodyId,
        /// The kicking part.
        part: u8,
        /// The dynamic body kicked.
        body: BodyId,
        /// The unit kick direction from the bumper toward the body, scaled
        /// by [`TRIG_UNIT`].
        normal: Vector,
        /// The speed added, micro-units per second.
        speed: i64,
    },
}

/// What one [`World::step`] did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StepReport {
    /// The tick that was advanced from.
    pub tick: u64,
    /// Substeps used.
    pub substeps: u8,
    /// Events in canonical order: by the substep of first occurrence, then
    /// within a substep body contacts (by index pair, each followed by its
    /// part details and kicks in part order), wall contacts (by body, then
    /// wall, each followed by its part details) and sensor changes (by
    /// sensor, then body).
    pub events: Vec<Event>,
}

/// Why a world operation was refused. The world is unchanged.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum WorldError {
    /// The tick rate or substep limit is zero, or the speed limit is not
    /// positive or exceeds [`MAX_MAGNITUDE`].
    #[error("invalid world configuration")]
    InvalidConfig,
    /// A radius, mass, speed or ratio is out of its valid range.
    #[error("invalid body, wall or sensor parameter")]
    InvalidParameter,
    /// A body's radius is below [`WorldConfig::min_radius`].
    #[error("body radius {radius} is below the tunnelling-safe minimum {minimum}")]
    BodyTooSmall {
        /// The requested radius.
        radius: i64,
        /// The smallest safe radius.
        minimum: i64,
    },
    /// A rectangle's minimum corner exceeds its maximum.
    #[error("region minimum exceeds its maximum")]
    InvalidRegion,
    /// A coordinate or velocity exceeds [`MAX_MAGNITUDE`].
    #[error("coordinate out of range")]
    OutOfRange,
    /// A wall has zero length.
    #[error("wall has zero length")]
    DegenerateWall,
    /// The world already holds the most bodies, walls, sensors or fields
    /// (65 535).
    #[error("too many bodies, walls, sensors or fields")]
    Capacity,
    /// No such body.
    #[error("unknown body")]
    UnknownBody,
    /// The operation needs a kinematic body.
    #[error("body is not kinematic")]
    NotKinematic,
    /// The operation needs a shaped body (added by
    /// [`World::add_shaped_body`]).
    #[error("body is not shaped")]
    NotShaped,
    /// No such wall.
    #[error("unknown wall")]
    UnknownWall,
    /// No such acceleration field.
    #[error("unknown field")]
    UnknownField,
}

pub(crate) const fn check_vector(vector: Vector) -> Result<Vector, WorldError> {
    if vector.x.unsigned_abs() > MAX_MAGNITUDE.unsigned_abs()
        || vector.y.unsigned_abs() > MAX_MAGNITUDE.unsigned_abs()
    {
        Err(WorldError::OutOfRange)
    } else {
        Ok(vector)
    }
}

const fn clamp_i64(value: i64, min: i64, max: i64) -> i64 {
    if value < min {
        min
    } else if value > max {
        max
    } else {
        value
    }
}

/// Integer square root, rounded down; zero for non-positive input.
pub(crate) fn integer_sqrt(value: i128) -> i64 {
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

/// Divides, rounding half away from zero, saturating to `i64`. The
/// denominator must not be zero.
pub(crate) fn div_round(numerator: i128, denominator: i128) -> i64 {
    let (numerator, denominator) = if denominator < 0 {
        (numerator.saturating_neg(), denominator.saturating_neg())
    } else {
        (numerator, denominator)
    };
    let half = denominator / 2;
    let quotient = if numerator >= 0 {
        numerator.saturating_add(half) / denominator
    } else {
        numerator.saturating_sub(half) / denominator
    };
    i64::try_from(quotient).unwrap_or(if quotient > 0 { i64::MAX } else { i64::MIN })
}

/// Divides non-negative `numerator` by positive `denominator`, rounding up.
pub(crate) const fn ceil_div(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator - 1) / denominator
}
