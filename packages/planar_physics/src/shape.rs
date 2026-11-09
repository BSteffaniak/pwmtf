//! Shaped bodies: oriented boxes, capsules and compounds of several parts,
//! with rotation, kinematic angle control, steering drives, acceleration
//! fields and bumper kicks.

use crate::{BodyDef, Region, Vector};

/// Most parts in one shaped body.
pub const MAX_PARTS: usize = 16;

/// Largest part dimension or offset, in micro-units (one hundred table
/// units), keeping inertia and impulse products in range.
pub const MAX_EXTENT: i64 = 100_000_000;

/// Largest surface friction coefficient, in millionths (four).
pub const MAX_SURFACE_FRICTION: u32 = 4_000_000;

/// Largest angular speed, in microradians per second (one thousand radians
/// per second).
pub const MAX_ANGULAR_SPEED: i64 = 1_000_000_000;

/// The geometry of one part, in the part's own frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Shape {
    /// A circle.
    Disc {
        /// Radius in micro-units.
        radius: i64,
    },
    /// A rectangle centered on the part, before the part's rotation.
    Rect {
        /// Half the extent along the part's local x axis.
        half_width: i64,
        /// Half the extent along the part's local y axis.
        half_height: i64,
    },
    /// A segment along the part's local x axis from `-half_length` to
    /// `+half_length`, thickened by `radius` (a stadium).
    Capsule {
        /// Half the core segment's length; zero makes a disc.
        half_length: i64,
        /// Radius around the core segment.
        radius: i64,
    },
}

impl Shape {
    /// Half the narrowest width: how far the shape may move per substep
    /// (twice this) without tunnelling.
    #[must_use]
    pub const fn thickness(self) -> i64 {
        match self {
            Self::Disc { radius } | Self::Capsule { radius, .. } => radius,
            Self::Rect {
                half_width,
                half_height,
            } => {
                if half_width < half_height {
                    half_width
                } else {
                    half_height
                }
            }
        }
    }

    /// The distance from the part's center to its farthest point, rounded
    /// up.
    #[must_use]
    pub fn extent(self) -> i64 {
        match self {
            Self::Disc { radius } => radius,
            Self::Capsule {
                half_length,
                radius,
            } => half_length + radius,
            Self::Rect {
                half_width,
                half_height,
            } => {
                let squared = i128::from(half_width) * i128::from(half_width)
                    + i128::from(half_height) * i128::from(half_height);
                let root = crate::integer_sqrt(squared);
                if i128::from(root) * i128::from(root) < squared {
                    root + 1
                } else {
                    root
                }
            }
        }
    }
}

/// How a part turns with its body.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Attachment {
    /// Rigidly attached: the offset rotates with the body, so the part
    /// orbits the body's origin.
    Rigid,
    /// Mounted on a spindle at its offset: the part translates with the body
    /// but turns about its own center by the body's angle, like each figure
    /// on a foosball rod. Only kinematic bodies may have spindle parts.
    Spindle,
}

/// One shape of a body, placed in the body's frame.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Part {
    /// The geometry.
    pub shape: Shape,
    /// The part's center relative to the body's origin, in micro-units.
    pub offset: Vector,
    /// The part's rotation relative to the body, in microradians.
    pub angle: i64,
    /// How the part turns with the body.
    pub attachment: Attachment,
    /// Bumper kick: a speed in micro-units per second added along the
    /// contact normal to a dynamic body each time it starts touching this
    /// part (after the ordinary bounce), reported as [`crate::Event::Bumped`].
    /// Zero for none; only kinematic bodies' parts may kick.
    pub kick: i64,
}

impl Part {
    /// A rigid part centered on the body's origin.
    #[must_use]
    pub const fn new(shape: Shape) -> Self {
        Self {
            shape,
            offset: Vector::ZERO,
            angle: 0,
            attachment: Attachment::Rigid,
            kick: 0,
        }
    }

    /// A rigid part at `offset` turned by `angle` microradians.
    #[must_use]
    pub const fn at(shape: Shape, offset: Vector, angle: i64) -> Self {
        Self {
            offset,
            angle,
            ..Self::new(shape)
        }
    }
}

/// A steering drive for a dynamic body, such as a bumper cart: forward is
/// the body's local +x axis.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Drive {
    /// Acceleration along the body's forward axis at full throttle, in
    /// micro-units per second squared (negative throttle reverses or
    /// brakes).
    pub acceleration: i64,
    /// Angular speed at full steer, in microradians per second (positive is
    /// counter-clockwise).
    pub turn_rate: i64,
    /// How quickly angular velocity approaches the steered rate, in
    /// microradians per second squared; also how quickly bump spin fades.
    pub turn_acceleration: i64,
    /// Largest sideways deceleration the wheels provide, in micro-units per
    /// second squared: sideways velocity loses up to this much per second so
    /// the body tracks its heading instead of sliding like a puck, while a
    /// hard bump still slides it briefly.
    pub grip: i64,
}

/// Everything needed to add a shaped body.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShapedBodyDef {
    /// Kind, mass, pose, motion, material, confinement and layers. Its
    /// `radius` is ignored: the body's radius becomes the distance from its
    /// origin to its farthest point.
    pub body: BodyDef,
    /// One to [`MAX_PARTS`] parts. A dynamic body's origin is treated as its
    /// center of mass, so arrange parts around it.
    pub parts: Vec<Part>,
    /// Initial angle in microradians.
    pub angle: i64,
    /// Initial angular velocity in microradians per second (dynamic).
    pub angular_velocity: i64,
    /// Angular speed cap in microradians per second: a kinematic body's
    /// chase speed toward its angle target, a dynamic body's terminal spin.
    /// Zero locks a dynamic body's rotation.
    pub max_angular_speed: i64,
    /// Fraction of angular speed lost per second, in millionths.
    pub angular_damping: u32,
    /// Coulomb friction coefficient in millionths. A contact uses the
    /// geometric mean of both surfaces (walls default to zero, see
    /// [`crate::World::set_wall_friction`]), so friction needs both.
    pub surface_friction: u32,
    /// Kinematic only: the angle target is clamped into `(min, max)`
    /// microradians and the angle moves directly toward it without wrapping,
    /// as a flipper between rest and raised. Without a range a kinematic
    /// body turns the short way round and its angle wraps.
    pub angle_range: Option<(i64, i64)>,
    /// Moment of inertia per unit mass (squared radius of gyration about the
    /// origin) in square micro-units; zero derives it from the parts'
    /// areas.
    pub inertia_per_mass: i64,
    /// Dynamic only: a steering drive controlled by [`crate::World::set_drive`].
    pub drive: Option<Drive>,
}

impl ShapedBodyDef {
    /// A shaped body from `body` and `parts` with no rotation, no angular
    /// damping or friction, an angular speed cap of one hundred radians per
    /// second and derived inertia.
    #[must_use]
    pub const fn new(body: BodyDef, parts: Vec<Part>) -> Self {
        Self {
            body,
            parts,
            angle: 0,
            angular_velocity: 0,
            max_angular_speed: 100_000_000,
            angular_damping: 0,
            surface_friction: 0,
            angle_range: None,
            inertia_per_mass: 0,
            drive: None,
        }
    }

    /// A dynamic shaped body (see [`BodyDef::dynamic`] for defaults).
    #[must_use]
    pub const fn dynamic(mass: i64, position: Vector, parts: Vec<Part>) -> Self {
        Self::new(BodyDef::dynamic(1, mass, position), parts)
    }

    /// A kinematic shaped body chasing its position target at up to
    /// `max_speed` and its angle target at up to `max_angular_speed`.
    #[must_use]
    pub const fn kinematic(
        position: Vector,
        max_speed: i64,
        max_angular_speed: i64,
        parts: Vec<Part>,
    ) -> Self {
        let mut def = Self::new(BodyDef::kinematic(1, position, max_speed), parts);
        def.max_angular_speed = max_angular_speed;
        def
    }
}

/// A part's placement in the world.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PartPose {
    /// The part's geometry.
    pub shape: Shape,
    /// The part's center in world micro-units.
    pub center: Vector,
    /// The part's world angle in microradians.
    pub angle: i64,
}

/// A region with its own acceleration, added to the world's.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FieldDef {
    /// Where the acceleration applies (to bodies whose center is inside).
    pub region: Region,
    /// Acceleration in micro-units per second squared.
    pub acceleration: Vector,
    /// Layers affected.
    pub mask: u32,
}
