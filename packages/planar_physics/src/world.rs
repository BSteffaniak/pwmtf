//! The simulated world and its fixed-tick step.

use std::collections::BTreeSet;

use crate::contact::{Contact, Placed, closest_on_segment, collide, unit_normal};
use crate::trig::{rotate, sin_cos, wrap_angle};
use crate::{
    Attachment, BodyDef, BodyId, BodyKind, Drive, Event, FieldDef, FieldId, MAX_ANGULAR_SPEED,
    MAX_EXTENT, MAX_MAGNITUDE, MAX_PARTS, MAX_SURFACE_FRICTION, MILLIONTHS, Part, PartPose, Region,
    SensorDef, SensorId, Shape, ShapedBodyDef, StepReport, Vector, WallDef, WallId, WorldConfig,
    WorldError, check_vector, integer_sqrt,
};

/// Fixed-point scale of unit normals.
const UNIT: i64 = crate::contact::UNIT;
/// Fixed-point scale of inverse effective masses in the shaped solver.
const INVERSE_SCALE: i128 = 1 << 56;
/// Microradians per radian.
const MICRORADIANS: i128 = 1_000_000;
/// Largest mass, keeping impulse products in range.
const MAX_MASS: i64 = 1_000_000_000;
/// Collision speeds can grow up to this multiple of the fastest body within
/// a tick (a light disc struck by a heavy or kinematic one); substeps are
/// sized for it.
const SPEED_GROWTH: i64 = 3;

/// A body's parameters and current motion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Body {
    pub(crate) def: BodyDef,
    pub(crate) target: Vector,
    pub(crate) active: bool,
    /// Rotation state and parameters of a shaped body; `None` for a plain
    /// disc, which never rotates.
    pub(crate) spin: Option<Spin>,
}

/// Rotation state, parameters and derived geometry of a shaped body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Spin {
    pub(crate) angle: i64,
    pub(crate) angular_velocity: i64,
    pub(crate) angle_target: i64,
    /// Effective cap, lowered so the farthest point never outruns the
    /// world's speed limit through rotation alone.
    pub(crate) max_angular_speed: i64,
    pub(crate) angular_damping: u32,
    pub(crate) surface_friction: u32,
    pub(crate) angle_range: Option<(i64, i64)>,
    /// The inertia per mass as requested (zero derives it).
    pub(crate) inertia_setting: i64,
    /// The effective inertia per mass, square micro-units.
    pub(crate) inertia: i64,
    pub(crate) drive: Option<Drive>,
    pub(crate) throttle: i32,
    pub(crate) steer: i32,
    pub(crate) first_part: usize,
    pub(crate) part_count: usize,
    /// The thinnest part's half width.
    pub(crate) thickness: i64,
    /// The farthest any part point lies from what it turns about.
    pub(crate) reach: i64,
}

impl Body {
    /// How the body moves.
    #[must_use]
    pub const fn kind(&self) -> BodyKind {
        self.def.kind
    }

    /// The current center in micro-units.
    #[must_use]
    pub const fn position(&self) -> Vector {
        self.def.position
    }

    /// The current velocity in micro-units per second. For a kinematic body
    /// this is its motion during the last tick.
    #[must_use]
    pub const fn velocity(&self) -> Vector {
        self.def.velocity
    }

    /// The kinematic target (the initial position for dynamic bodies).
    #[must_use]
    pub const fn target(&self) -> Vector {
        self.target
    }

    /// The disc radius in micro-units; for a shaped body, the distance from
    /// its origin to its farthest point.
    #[must_use]
    pub const fn radius(&self) -> i64 {
        self.def.radius
    }

    /// Whether the body takes part in simulation.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// The body's parameters, with its current position and velocity. A
    /// shaped body's radius is the distance from its origin to its farthest
    /// point.
    #[must_use]
    pub const fn def(&self) -> &BodyDef {
        &self.def
    }

    /// Whether the body was added by [`World::add_shaped_body`].
    #[must_use]
    pub const fn is_shaped(&self) -> bool {
        self.spin.is_some()
    }

    /// The angle in microradians (zero for a plain disc). Dynamic bodies
    /// and kinematic bodies without an angle range wrap into half a turn
    /// either way.
    #[must_use]
    pub const fn angle(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.angle,
            None => 0,
        }
    }

    /// The angular velocity in microradians per second (counter-clockwise
    /// positive). For a kinematic body this is its turn during the last
    /// tick.
    #[must_use]
    pub const fn angular_velocity(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.angular_velocity,
            None => 0,
        }
    }

    /// The kinematic angle target in microradians.
    #[must_use]
    pub const fn angle_target(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.angle_target,
            None => 0,
        }
    }

    /// The effective angular speed cap in microradians per second.
    #[must_use]
    pub const fn max_angular_speed(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.max_angular_speed,
            None => 0,
        }
    }

    /// The moment of inertia per unit mass about the origin, in square
    /// micro-units (zero for a plain disc, which never rotates).
    #[must_use]
    pub const fn inertia_per_mass(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.inertia,
            None => 0,
        }
    }

    /// The current drive input `(throttle, steer)` in millionths.
    #[must_use]
    pub const fn drive_input(&self) -> (i32, i32) {
        match self.spin {
            Some(spin) => (spin.throttle, spin.steer),
            None => (0, 0),
        }
    }

    /// Half the narrowest width of the body's thinnest part (the radius of a
    /// plain disc): what the tunnelling bound protects.
    #[must_use]
    pub const fn thickness(&self) -> i64 {
        match self.spin {
            Some(spin) => spin.thickness,
            None => self.def.radius,
        }
    }

    /// The speed of the body's fastest surface point, micro-units per
    /// second.
    fn surface_speed(&self) -> i64 {
        let linear = self.def.velocity.length();
        self.spin.map_or(linear, |spin| {
            let tip = i128::from(spin.angular_velocity.unsigned_abs()) * i128::from(spin.reach)
                / MICRORADIANS;
            linear.saturating_add(i64::try_from(tip).unwrap_or(i64::MAX))
        })
    }
}

/// A kinematic body's motion this tick.
#[derive(Clone, Copy, Debug)]
struct Plan {
    start: Vector,
    end: Vector,
    angle_start: i64,
    angle_step: i64,
}

/// A body's (or wall's, as `None`) motion as the shaped solver sees it.
#[derive(Clone, Copy, Debug)]
struct Mover {
    dynamic: bool,
    mass: i64,
    velocity: Vector,
    omega: i64,
    /// Inertia per mass; zero when the body cannot rotate.
    inertia: i64,
}

impl Mover {
    const STATIC: Self = Self {
        dynamic: false,
        mass: 1,
        velocity: Vector::ZERO,
        omega: 0,
        inertia: 0,
    };

    /// The inverse effective mass along a direction whose lever arm about
    /// the pivot is `arm`, scaled by [`INVERSE_SCALE`].
    fn weight(self, arm: i64) -> i128 {
        if !self.dynamic {
            return 0;
        }
        let mass = i128::from(self.mass);
        if self.inertia > 0 {
            let inertia = i128::from(self.inertia);
            INVERSE_SCALE.saturating_mul(inertia + i128::from(arm) * i128::from(arm))
                / (mass * inertia)
        } else {
            INVERSE_SCALE / mass
        }
    }
}

/// What a shaped contact did.
#[derive(Clone, Copy, Debug)]
struct Outcome {
    /// Approach speed at the contact point, when approaching.
    speed: Option<i64>,
    /// How far the first body was pushed.
    delta_a: Vector,
    /// How far the second body was pushed.
    delta_b: Vector,
}

/// A deterministic planar physics world.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct World {
    pub(crate) config: WorldConfig,
    pub(crate) tick: u64,
    pub(crate) bodies: Vec<Body>,
    pub(crate) walls: Vec<WallDef>,
    pub(crate) sensors: Vec<SensorDef>,
    /// `(sensor, body)` pairs currently sensed.
    pub(crate) occupancy: BTreeSet<(SensorId, BodyId)>,
    /// Parts of every shaped body, in body order.
    pub(crate) parts: Vec<Part>,
    /// Each wall's surface friction in millionths.
    pub(crate) wall_friction: Vec<u32>,
    /// World-wide acceleration in micro-units per second squared.
    pub(crate) gravity: Vector,
    /// Regional accelerations.
    pub(crate) fields: Vec<FieldDef>,
    /// `(bumper, part, body)` kicking contacts in progress.
    pub(crate) touching: BTreeSet<(BodyId, u8, BodyId)>,
}

impl World {
    /// Creates an empty world at tick zero.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidConfig`] when the tick rate or substep
    /// limit is zero or the speed limit is not in `1..=MAX_MAGNITUDE`.
    pub const fn new(config: WorldConfig) -> Result<Self, WorldError> {
        if config.ticks_per_second == 0
            || config.max_substeps == 0
            || config.max_speed <= 0
            || config.max_speed > MAX_MAGNITUDE
        {
            return Err(WorldError::InvalidConfig);
        }
        Ok(Self {
            config,
            tick: 0,
            bodies: Vec::new(),
            walls: Vec::new(),
            sensors: Vec::new(),
            occupancy: BTreeSet::new(),
            parts: Vec::new(),
            wall_friction: Vec::new(),
            gravity: Vector::ZERO,
            fields: Vec::new(),
            touching: BTreeSet::new(),
        })
    }

    /// The immutable configuration.
    #[must_use]
    pub const fn config(&self) -> WorldConfig {
        self.config
    }

    /// Ticks simulated so far.
    #[must_use]
    pub const fn tick(&self) -> u64 {
        self.tick
    }

    /// Adds a body; kinematic bodies start targeting their position. The
    /// body's speed cap is lowered to the world's, its velocity to its cap
    /// and its position into its confinement.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidParameter`] for a non-positive radius or
    /// speed cap, a dynamic mass outside `1..=1_000_000_000`, restitution or
    /// damping above one million or negative friction;
    /// [`WorldError::BodyTooSmall`] below [`WorldConfig::min_radius`];
    /// [`WorldError::OutOfRange`] for coordinates beyond [`MAX_MAGNITUDE`];
    /// and [`WorldError::Capacity`] beyond 65 535 bodies.
    pub fn add_body(&mut self, def: BodyDef) -> Result<BodyId, WorldError> {
        let mut def = def;
        let id = self.prepare(&mut def, None)?;
        self.bodies.push(Body {
            target: def.position,
            def,
            active: true,
            spin: None,
        });
        Ok(id)
    }

    /// Validates and normalizes a body's common parameters, returning its
    /// id. The tunnelling check uses `thickness`, or the radius when absent.
    fn prepare(&self, def: &mut BodyDef, thickness: Option<i64>) -> Result<BodyId, WorldError> {
        let dynamic = def.kind == BodyKind::Dynamic;
        if def.radius <= 0
            || def.radius > MAX_MAGNITUDE
            || def.max_speed <= 0
            || (dynamic && !(1..=MAX_MASS).contains(&def.mass))
            || def.restitution > MILLIONTHS
            || def.damping > MILLIONTHS
            || !(0..=MAX_MAGNITUDE).contains(&def.friction)
        {
            return Err(WorldError::InvalidParameter);
        }
        let minimum = self.config.min_radius();
        let thickness = thickness.unwrap_or(def.radius);
        if thickness < minimum {
            return Err(WorldError::BodyTooSmall {
                radius: thickness,
                minimum,
            });
        }
        check_vector(def.position)?;
        check_vector(def.velocity)?;
        let id = BodyId(u16::try_from(self.bodies.len()).map_err(|_| WorldError::Capacity)?);
        if id.0 == u16::MAX {
            return Err(WorldError::Capacity);
        }
        if !dynamic {
            def.mass = 1;
            def.velocity = Vector::ZERO;
        }
        def.max_speed = def.max_speed.min(self.config.max_speed);
        def.velocity = def.velocity.clamp_length(def.max_speed);
        if let Some(confine) = def.confine {
            def.position = confine.clamp(def.position);
        }
        Ok(id)
    }

    /// Adds a shaped body: one or more discs, oriented rectangles and
    /// capsules with rotation. Kinematic bodies start targeting their pose.
    /// Besides [`World::add_body`]'s normalization, the angular speed cap is
    /// lowered so rotation alone never moves a point faster than the world's
    /// speed limit, the angle is clamped into its range (or wrapped) and a
    /// dynamic body's angular velocity is clamped to its cap.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidParameter`] for no parts or more than
    /// [`MAX_PARTS`]; a part dimension that is not positive (a capsule's
    /// half length may be zero) or exceeds [`MAX_EXTENT`]; an offset beyond
    /// [`MAX_EXTENT`]; a kick that is negative, beyond [`MAX_MAGNITUDE`] or
    /// on a dynamic body; a spindle part on a dynamic body; an angle range on
    /// a dynamic body or with its minimum above its maximum; a drive on a
    /// kinematic body or with a negative or out-of-range rate; an angular
    /// speed, cap or inertia out of range; angular damping above one
    /// million or surface friction above [`MAX_SURFACE_FRICTION`]; and every
    /// error of [`World::add_body`] for the common parameters, where the
    /// tunnelling check ([`WorldError::BodyTooSmall`]) applies to the
    /// thinnest part.
    pub fn add_shaped_body(&mut self, def: ShapedBodyDef) -> Result<BodyId, WorldError> {
        let ShapedBodyDef {
            body: mut base,
            parts,
            angle,
            angular_velocity,
            max_angular_speed,
            angular_damping,
            surface_friction,
            angle_range,
            inertia_per_mass,
            drive,
        } = def;
        let dynamic = base.kind == BodyKind::Dynamic;
        if parts.is_empty()
            || parts.len() > MAX_PARTS
            || parts.iter().any(|part| !valid_part(part, dynamic))
            || !(0..=MAX_ANGULAR_SPEED).contains(&max_angular_speed)
            || angular_velocity.unsigned_abs() > MAX_ANGULAR_SPEED.unsigned_abs()
            || angular_damping > MILLIONTHS
            || surface_friction > MAX_SURFACE_FRICTION
            || !(0..=MAX_EXTENT * MAX_EXTENT).contains(&inertia_per_mass)
            || angle.unsigned_abs() > MAX_MAGNITUDE.unsigned_abs()
            || (dynamic && angle_range.is_some())
            || (!dynamic && drive.is_some())
            || angle_range.is_some_and(|(min, max)| {
                min > max
                    || min.unsigned_abs().max(max.unsigned_abs()) > MAX_MAGNITUDE.unsigned_abs()
            })
            || drive.is_some_and(|drive| !valid_drive(drive))
        {
            return Err(WorldError::InvalidParameter);
        }
        let thickness = parts
            .iter()
            .map(|part| part.shape.thickness())
            .min()
            .unwrap_or(0);
        let bounding = parts
            .iter()
            .map(|part| ceil_length(part.offset) + part.shape.extent())
            .max()
            .unwrap_or(0);
        let reach = parts
            .iter()
            .map(|part| match part.attachment {
                Attachment::Rigid => ceil_length(part.offset) + part.shape.extent(),
                Attachment::Spindle => part.shape.extent(),
            })
            .max()
            .unwrap_or(0);
        base.radius = bounding;
        let id = self.prepare(&mut base, Some(thickness))?;
        let tip_limit = i64::try_from(
            i128::from(self.config.max_speed) * MICRORADIANS / i128::from(reach.max(1)),
        )
        .unwrap_or(i64::MAX);
        let max_angular_speed = max_angular_speed.min(tip_limit);
        let inertia = if inertia_per_mass > 0 {
            inertia_per_mass
        } else {
            derived_inertia(&parts)
        };
        let angle = match angle_range {
            Some((min, max)) => angle.clamp(min, max),
            None => wrap_angle(angle),
        };
        let angular_velocity = if dynamic {
            angular_velocity.clamp(-max_angular_speed, max_angular_speed)
        } else {
            0
        };
        let first_part = self.parts.len();
        let part_count = parts.len();
        self.parts.extend(parts);
        self.bodies.push(Body {
            target: base.position,
            def: base,
            active: true,
            spin: Some(Spin {
                angle,
                angular_velocity,
                angle_target: angle,
                max_angular_speed,
                angular_damping,
                surface_friction,
                angle_range,
                inertia_setting: inertia_per_mass,
                inertia,
                drive,
                throttle: 0,
                steer: 0,
                first_part,
                part_count,
                thickness,
                reach,
            }),
        });
        Ok(id)
    }

    /// Adds a wall segment.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::DegenerateWall`] for a zero-length segment,
    /// [`WorldError::InvalidParameter`] for restitution above one million,
    /// [`WorldError::OutOfRange`] for coordinates beyond [`MAX_MAGNITUDE`]
    /// and [`WorldError::Capacity`] beyond 65 535 walls.
    pub fn add_wall(&mut self, wall: WallDef) -> Result<WallId, WorldError> {
        check_vector(wall.start)?;
        check_vector(wall.end)?;
        if wall.start == wall.end {
            return Err(WorldError::DegenerateWall);
        }
        if wall.restitution > MILLIONTHS {
            return Err(WorldError::InvalidParameter);
        }
        let id = WallId(u16::try_from(self.walls.len()).map_err(|_| WorldError::Capacity)?);
        if id.0 == u16::MAX {
            return Err(WorldError::Capacity);
        }
        self.walls.push(wall);
        self.wall_friction.push(0);
        Ok(id)
    }

    /// Adds the four walls of a rectangular arena, in the order bottom,
    /// right, top, left.
    ///
    /// # Errors
    ///
    /// Returns the errors of [`World::add_wall`]; the arena must have
    /// positive width and height. On error no wall is added.
    pub fn add_bounds(
        &mut self,
        arena: crate::Aabb,
        restitution: u32,
        layers: u32,
    ) -> Result<[WallId; 4], WorldError> {
        let (min, max) = (arena.min(), arena.max());
        if min.x == max.x || min.y == max.y {
            return Err(WorldError::DegenerateWall);
        }
        if restitution > MILLIONTHS {
            return Err(WorldError::InvalidParameter);
        }
        if self.walls.len() + 4 >= usize::from(u16::MAX) {
            return Err(WorldError::Capacity);
        }
        let corners = [
            min,
            Vector::new(max.x, min.y),
            max,
            Vector::new(min.x, max.y),
        ];
        let mut ids = [WallId(0); 4];
        for (index, id) in ids.iter_mut().enumerate() {
            *id = self.add_wall(WallDef {
                start: corners[index],
                end: corners[(index + 1) % 4],
                restitution,
                layers,
            })?;
        }
        Ok(ids)
    }

    /// Adds a sensor region.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidParameter`] for a negative circle radius,
    /// [`WorldError::OutOfRange`] for coordinates beyond [`MAX_MAGNITUDE`]
    /// and [`WorldError::Capacity`] beyond 65 535 sensors.
    pub fn add_sensor(&mut self, sensor: SensorDef) -> Result<SensorId, WorldError> {
        if let Region::Circle { center, radius } = sensor.region {
            check_vector(center)?;
            if !(0..=MAX_MAGNITUDE).contains(&radius) {
                return Err(WorldError::InvalidParameter);
            }
        }
        let id = SensorId(u16::try_from(self.sensors.len()).map_err(|_| WorldError::Capacity)?);
        if id.0 == u16::MAX {
            return Err(WorldError::Capacity);
        }
        self.sensors.push(sensor);
        Ok(id)
    }

    /// A body by id.
    #[must_use]
    pub fn body(&self, id: BodyId) -> Option<&Body> {
        self.bodies.get(usize::from(id.0))
    }

    /// Every body in insertion order.
    pub fn bodies(&self) -> impl Iterator<Item = (BodyId, &Body)> {
        self.bodies
            .iter()
            .enumerate()
            .map(|(index, body)| (BodyId(index_u16(index)), body))
    }

    /// Every wall in insertion order.
    pub fn walls(&self) -> impl Iterator<Item = (WallId, &WallDef)> {
        self.walls
            .iter()
            .enumerate()
            .map(|(index, wall)| (WallId(index_u16(index)), wall))
    }

    /// Every sensor in insertion order.
    pub fn sensors(&self) -> impl Iterator<Item = (SensorId, &SensorDef)> {
        self.sensors
            .iter()
            .enumerate()
            .map(|(index, sensor)| (SensorId(index_u16(index)), sensor))
    }

    /// Whether `sensor` currently senses `body`.
    #[must_use]
    pub fn is_sensing(&self, sensor: SensorId, body: BodyId) -> bool {
        self.occupancy.contains(&(sensor, body))
    }

    fn body_mut(&mut self, id: BodyId) -> Result<&mut Body, WorldError> {
        self.bodies
            .get_mut(usize::from(id.0))
            .ok_or(WorldError::UnknownBody)
    }

    /// Sets where a kinematic body heads. It moves there at up to its speed
    /// cap, staying inside its confinement.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::NotKinematic`] or
    /// [`WorldError::OutOfRange`].
    pub fn set_target(&mut self, id: BodyId, target: Vector) -> Result<(), WorldError> {
        check_vector(target)?;
        let body = self.body_mut(id)?;
        if body.def.kind != BodyKind::Kinematic {
            return Err(WorldError::NotKinematic);
        }
        body.target = target;
        Ok(())
    }

    /// Teleports a body (clamped into its confinement). A kinematic body's
    /// target moves with it.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`] or [`WorldError::OutOfRange`].
    pub fn set_position(&mut self, id: BodyId, position: Vector) -> Result<(), WorldError> {
        check_vector(position)?;
        let body = self.body_mut(id)?;
        let position = body
            .def
            .confine
            .map_or(position, |confine| confine.clamp(position));
        body.def.position = position;
        if body.def.kind == BodyKind::Kinematic {
            body.target = position;
            body.def.velocity = Vector::ZERO;
        }
        Ok(())
    }

    /// Sets a dynamic body's velocity, clamped to its speed cap.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::OutOfRange`] or
    /// [`WorldError::InvalidParameter`] for a kinematic body.
    pub fn set_velocity(&mut self, id: BodyId, velocity: Vector) -> Result<(), WorldError> {
        check_vector(velocity)?;
        let body = self.body_mut(id)?;
        if body.def.kind != BodyKind::Dynamic {
            return Err(WorldError::InvalidParameter);
        }
        body.def.velocity = velocity.clamp_length(body.def.max_speed);
        Ok(())
    }

    /// Includes or excludes a body from simulation. Inactive bodies neither
    /// move nor collide, and sensors report them leaving.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`].
    pub fn set_active(&mut self, id: BodyId, active: bool) -> Result<(), WorldError> {
        self.body_mut(id)?.active = active;
        Ok(())
    }

    fn spin_mut(&mut self, id: BodyId) -> Result<(BodyKind, &mut Spin), WorldError> {
        let body = self.body_mut(id)?;
        let kind = body.def.kind;
        body.spin
            .as_mut()
            .map(|spin| (kind, spin))
            .ok_or(WorldError::NotShaped)
    }

    /// Sets the angle a kinematic shaped body turns toward, in
    /// microradians, at up to its angular speed cap. With an angle range the
    /// target is clamped into it; without one the body turns the short way
    /// round.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::NotShaped`],
    /// [`WorldError::NotKinematic`], or [`WorldError::OutOfRange`] beyond
    /// [`MAX_MAGNITUDE`].
    pub fn set_angle_target(&mut self, id: BodyId, angle: i64) -> Result<(), WorldError> {
        check_angle(angle)?;
        let (kind, spin) = self.spin_mut(id)?;
        if kind != BodyKind::Kinematic {
            return Err(WorldError::NotKinematic);
        }
        spin.angle_target = spin
            .angle_range
            .map_or(angle, |(min, max)| angle.clamp(min, max));
        Ok(())
    }

    /// Teleports a shaped body's angle (clamped into its range, or
    /// wrapped). A kinematic body's angle target moves with it and its
    /// angular velocity resets.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::NotShaped`] or
    /// [`WorldError::OutOfRange`] beyond [`MAX_MAGNITUDE`].
    pub fn set_angle(&mut self, id: BodyId, angle: i64) -> Result<(), WorldError> {
        check_angle(angle)?;
        let (kind, spin) = self.spin_mut(id)?;
        let angle = spin
            .angle_range
            .map_or_else(|| wrap_angle(angle), |(min, max)| angle.clamp(min, max));
        spin.angle = angle;
        if kind == BodyKind::Kinematic {
            spin.angle_target = angle;
            spin.angular_velocity = 0;
        }
        Ok(())
    }

    /// Sets a dynamic shaped body's angular velocity in microradians per
    /// second, clamped to its cap.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::NotShaped`],
    /// [`WorldError::InvalidParameter`] for a kinematic body, or
    /// [`WorldError::OutOfRange`] beyond [`MAX_ANGULAR_SPEED`].
    pub fn set_angular_velocity(&mut self, id: BodyId, omega: i64) -> Result<(), WorldError> {
        if omega.unsigned_abs() > MAX_ANGULAR_SPEED.unsigned_abs() {
            return Err(WorldError::OutOfRange);
        }
        let (kind, spin) = self.spin_mut(id)?;
        if kind != BodyKind::Dynamic {
            return Err(WorldError::InvalidParameter);
        }
        spin.angular_velocity = omega.clamp(-spin.max_angular_speed, spin.max_angular_speed);
        Ok(())
    }

    /// Sets a driven body's throttle and steer in millionths, each clamped
    /// to `-1_000_000..=1_000_000`. They persist until changed and act at
    /// the start of every tick.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownBody`], [`WorldError::NotShaped`], or
    /// [`WorldError::InvalidParameter`] for a body without a drive.
    pub fn set_drive(&mut self, id: BodyId, throttle: i32, steer: i32) -> Result<(), WorldError> {
        let (_, spin) = self.spin_mut(id)?;
        if spin.drive.is_none() {
            return Err(WorldError::InvalidParameter);
        }
        let limit = i32::try_from(MILLIONTHS).unwrap_or(i32::MAX);
        spin.throttle = throttle.clamp(-limit, limit);
        spin.steer = steer.clamp(-limit, limit);
        Ok(())
    }

    /// The world-wide acceleration in micro-units per second squared.
    #[must_use]
    pub const fn gravity(&self) -> Vector {
        self.gravity
    }

    /// Sets the world-wide acceleration applied to every dynamic body, such
    /// as a pinball playfield's slope or a tilted marble lane. It may change
    /// every tick.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::OutOfRange`] beyond [`MAX_MAGNITUDE`].
    pub fn set_gravity(&mut self, acceleration: Vector) -> Result<(), WorldError> {
        self.gravity = check_vector(acceleration)?;
        Ok(())
    }

    /// Adds a regional acceleration, applied on top of the world's to
    /// dynamic bodies whose center lies in its region and whose layers
    /// intersect its mask.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::InvalidParameter`] for a negative circle radius,
    /// [`WorldError::OutOfRange`] beyond [`MAX_MAGNITUDE`] and
    /// [`WorldError::Capacity`] beyond 65 535 fields.
    pub fn add_field(&mut self, field: FieldDef) -> Result<FieldId, WorldError> {
        check_region(field.region)?;
        check_vector(field.acceleration)?;
        let id = FieldId(u16::try_from(self.fields.len()).map_err(|_| WorldError::Capacity)?);
        if id.0 == u16::MAX {
            return Err(WorldError::Capacity);
        }
        self.fields.push(field);
        Ok(id)
    }

    /// Changes a field's acceleration.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownField`] or [`WorldError::OutOfRange`].
    pub fn set_field_acceleration(
        &mut self,
        id: FieldId,
        acceleration: Vector,
    ) -> Result<(), WorldError> {
        check_vector(acceleration)?;
        self.fields
            .get_mut(usize::from(id.0))
            .ok_or(WorldError::UnknownField)?
            .acceleration = acceleration;
        Ok(())
    }

    /// Every field in insertion order.
    pub fn fields(&self) -> impl Iterator<Item = (FieldId, &FieldDef)> {
        self.fields
            .iter()
            .enumerate()
            .map(|(index, field)| (FieldId(index_u16(index)), field))
    }

    /// Sets a wall's surface friction coefficient in millionths (zero by
    /// default). A contact with a shaped body uses the geometric mean of the
    /// wall's and the body's.
    ///
    /// # Errors
    ///
    /// Returns [`WorldError::UnknownWall`], or
    /// [`WorldError::InvalidParameter`] above [`MAX_SURFACE_FRICTION`].
    pub fn set_wall_friction(&mut self, id: WallId, friction: u32) -> Result<(), WorldError> {
        if friction > MAX_SURFACE_FRICTION {
            return Err(WorldError::InvalidParameter);
        }
        *self
            .wall_friction
            .get_mut(usize::from(id.0))
            .ok_or(WorldError::UnknownWall)? = friction;
        Ok(())
    }

    /// A wall's surface friction coefficient in millionths.
    #[must_use]
    pub fn wall_friction(&self, id: WallId) -> Option<u32> {
        self.wall_friction.get(usize::from(id.0)).copied()
    }

    /// A shaped body's parts (empty for a plain disc or an unknown body).
    #[must_use]
    pub fn parts(&self, id: BodyId) -> &[Part] {
        self.body(id)
            .and_then(|body| body.spin)
            .map_or(&[], |spin| {
                &self.parts[spin.first_part..spin.first_part + spin.part_count]
            })
    }

    /// Where each of a body's parts is now: one disc for a plain body, empty
    /// for an unknown one.
    #[must_use]
    pub fn part_poses(&self, id: BodyId) -> Vec<PartPose> {
        let Some(body) = self.body(id) else {
            return Vec::new();
        };
        let Some(spin) = body.spin else {
            return vec![PartPose {
                shape: Shape::Disc {
                    radius: body.def.radius,
                },
                center: body.def.position,
                angle: 0,
            }];
        };
        let turn = sin_cos(spin.angle);
        self.parts(id)
            .iter()
            .map(|part| PartPose {
                shape: part.shape,
                center: match part.attachment {
                    Attachment::Rigid => body.def.position + rotate(part.offset, turn),
                    Attachment::Spindle => body.def.position + part.offset,
                },
                angle: spin.angle + part.angle,
            })
            .collect()
    }

    /// Advances exactly one tick.
    pub fn step(&mut self) -> StepReport {
        let tps = i128::from(self.config.ticks_per_second);
        self.apply_drives(tps);
        let plans = self.plan_kinematics(tps);
        let (field_speed, rest) = self.field_speeds(tps);
        let bound = self.speed_bound(field_speed.saturating_add(self.max_kick()));
        let substeps = self.substeps(tps, bound);
        let mut report = StepReport {
            tick: self.tick,
            substeps,
            events: Vec::new(),
        };
        let divisor = tps * i128::from(substeps);
        for step in 1..=i128::from(substeps) {
            let starts: Vec<Vector> = self.bodies.iter().map(Body::position).collect();
            self.accelerate(step, divisor);
            for (body, plan) in self.bodies.iter_mut().zip(&plans) {
                if !body.active {
                    continue;
                }
                if let Some(plan) = plan {
                    body.def.position =
                        plan.start + (plan.end - plan.start).scale(step, i128::from(substeps));
                    if let Some(spin) = &mut body.spin {
                        spin.angle = plan.angle_start
                            + crate::div_round(
                                i128::from(plan.angle_step) * step,
                                i128::from(substeps),
                            );
                    }
                } else {
                    body.def.position = body.def.position + body.def.velocity.scale(1, divisor);
                    if let Some(spin) = &mut body.spin {
                        spin.angle = wrap_angle(
                            spin.angle
                                + crate::div_round(i128::from(spin.angular_velocity), divisor),
                        );
                    }
                }
            }
            self.resolve_bodies(&mut report.events, rest);
            self.resolve_walls(&starts, &mut report.events, rest);
            self.confine_dynamic(bound);
            self.sense(&mut report.events);
        }
        for body in &mut self.bodies {
            if let Some(spin) = &mut body.spin
                && spin.angle_range.is_none()
            {
                spin.angle = wrap_angle(spin.angle);
            }
        }
        self.decelerate(tps);
        self.tick += 1;
        report
    }

    /// Applies each driven body's throttle, steering and lateral grip.
    fn apply_drives(&mut self, tps: i128) {
        let unit = i128::from(UNIT);
        let millionths = i128::from(MILLIONTHS);
        for body in &mut self.bodies {
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            let Some(spin) = &mut body.spin else {
                continue;
            };
            let Some(drive) = spin.drive else {
                continue;
            };
            let (sine, cosine) = sin_cos(spin.angle);
            let forward = Vector::new(cosine, sine);
            let lateral = Vector::new(-sine, cosine);
            let push = crate::div_round(
                i128::from(spin.throttle) * i128::from(drive.acceleration),
                millionths * tps,
            );
            let mut velocity = body.def.velocity + forward.scale(i128::from(push), unit);
            let sideways = crate::div_round(velocity.dot(lateral), unit);
            let grip = crate::div_round(i128::from(drive.grip), tps);
            velocity = velocity - lateral.scale(i128::from(sideways.clamp(-grip, grip)), unit);
            body.def.velocity = velocity.clamp_length(body.def.max_speed);
            let wanted = crate::div_round(
                i128::from(spin.steer) * i128::from(drive.turn_rate),
                millionths,
            );
            let turn = crate::div_round(i128::from(drive.turn_acceleration), tps);
            let omega = spin.angular_velocity + (wanted - spin.angular_velocity).clamp(-turn, turn);
            spin.angular_velocity = omega.clamp(-spin.max_angular_speed, spin.max_angular_speed);
        }
    }

    /// Each active kinematic body's motion for this tick; also records its
    /// linear and angular velocity for contacts.
    fn plan_kinematics(&mut self, tps: i128) -> Vec<Option<Plan>> {
        self.bodies
            .iter_mut()
            .map(|body| {
                if !body.active || body.def.kind != BodyKind::Kinematic {
                    return None;
                }
                let start = body.def.position;
                let reach = i64::try_from(i128::from(body.def.max_speed) / tps).unwrap_or(0);
                let mut end = start + (body.target - start).clamp_length(reach);
                if let Some(confine) = body.def.confine {
                    end = confine.clamp(end);
                }
                let moved = end - start;
                body.def.velocity = Vector::new(
                    i64::try_from(i128::from(moved.x) * tps).unwrap_or(0),
                    i64::try_from(i128::from(moved.y) * tps).unwrap_or(0),
                );
                let (angle_start, angle_step) = body.spin.as_mut().map_or((0, 0), |spin| {
                    let turn = i64::try_from(i128::from(spin.max_angular_speed) / tps).unwrap_or(0);
                    let wanted = match spin.angle_range {
                        Some(_) => spin.angle_target - spin.angle,
                        None => wrap_angle(spin.angle_target - spin.angle),
                    };
                    let mut step = wanted.clamp(-turn, turn);
                    if let Some((min, max)) = spin.angle_range {
                        step = (spin.angle + step).clamp(min, max) - spin.angle;
                    }
                    spin.angular_velocity = i64::try_from(i128::from(step) * tps).unwrap_or(0);
                    (spin.angle, step)
                });
                Some(Plan {
                    start,
                    end,
                    angle_start,
                    angle_step,
                })
            })
            .collect()
    }

    /// The speed the acceleration fields can add in one tick (rounded up)
    /// and the approach speed at or below which contacts do not bounce, so
    /// bodies resting under a field settle instead of chattering.
    fn field_speeds(&self, tps: i128) -> (i64, i64) {
        if self.gravity == Vector::ZERO && self.fields.is_empty() {
            return (0, 0);
        }
        let strongest = self
            .fields
            .iter()
            .map(|field| field.acceleration.length())
            .max()
            .unwrap_or(0);
        let total = i128::from(self.gravity.length()) + i128::from(strongest);
        let per_tick = i64::try_from(crate::ceil_div(total, tps)).unwrap_or(i64::MAX);
        (per_tick, per_tick)
    }

    /// The largest bumper kick of any active body.
    fn max_kick(&self) -> i64 {
        self.bodies
            .iter()
            .filter(|body| body.active)
            .filter_map(|body| body.spin)
            .flat_map(|spin| &self.parts[spin.first_part..spin.first_part + spin.part_count])
            .map(|part| part.kick)
            .max()
            .unwrap_or(0)
    }

    /// Adds this substep's share of the acceleration fields to every active
    /// dynamic body, so a tick adds exactly the per-tick amount.
    fn accelerate(&mut self, step: i128, divisor: i128) {
        if self.gravity == Vector::ZERO && self.fields.is_empty() {
            return;
        }
        for body in &mut self.bodies {
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            let mut acceleration = self.gravity;
            for field in &self.fields {
                if body.def.layers & field.mask != 0 && field.region.contains(body.def.position) {
                    acceleration = acceleration + field.acceleration;
                }
            }
            let share = acceleration.scale(step, divisor) - acceleration.scale(step - 1, divisor);
            body.def.velocity = body.def.velocity + share;
        }
    }

    /// The fastest any surface point may move this tick: the world's limit,
    /// or less when everything is slow. `extra` covers speed the fields and
    /// kicks may add.
    fn speed_bound(&self, extra: i64) -> i64 {
        let fastest = self
            .bodies
            .iter()
            .filter(|body| body.active)
            .map(Body::surface_speed)
            .max()
            .unwrap_or(0);
        fastest
            .saturating_mul(SPEED_GROWTH)
            .saturating_add(extra)
            .min(self.config.max_speed)
    }

    /// Enough substeps that no body moves more than half the smallest
    /// thickness per substep at `bound`.
    fn substeps(&self, tps: i128, bound: i64) -> u8 {
        let smallest = self
            .bodies
            .iter()
            .filter(|body| body.active)
            .map(Body::thickness)
            .min()
            .unwrap_or(1)
            .max(1);
        let needed = crate::ceil_div(i128::from(bound) * 2, tps * i128::from(smallest));
        u8::try_from(needed.clamp(1, i128::from(self.config.max_substeps))).unwrap_or(1)
    }

    fn resolve_bodies(&mut self, events: &mut Vec<Event>, rest: i64) {
        let shaped = self.bodies.iter().any(|body| body.spin.is_some());
        let mut placed: Vec<Vec<Placed>> = if shaped {
            let bodies = &self.bodies;
            self.touching.retain(|(bumper, _, body)| {
                bodies[usize::from(bumper.0)].active && bodies[usize::from(body.0)].active
            });
            (0..self.bodies.len())
                .map(|index| self.place(index))
                .collect()
        } else {
            Vec::new()
        };
        for first in 0..self.bodies.len() {
            for second in first + 1..self.bodies.len() {
                let (a, b) = (self.bodies[first], self.bodies[second]);
                if !a.active
                    || !b.active
                    || a.def.layers & b.def.mask == 0
                    || b.def.layers & a.def.mask == 0
                {
                    continue;
                }
                let mut details = Vec::new();
                let speed = match (a.def.kind, b.def.kind) {
                    (BodyKind::Kinematic, BodyKind::Kinematic) => None,
                    _ if a.spin.is_some() || b.spin.is_some() => {
                        self.shaped_pair(first, second, &mut placed, rest, &mut details)
                    }
                    (BodyKind::Dynamic, BodyKind::Dynamic) => {
                        self.dynamic_pair(first, second, rest)
                    }
                    (BodyKind::Dynamic, BodyKind::Kinematic) => {
                        self.kinematic_contact(first, second, rest)
                    }
                    (BodyKind::Kinematic, BodyKind::Dynamic) => {
                        self.kinematic_contact(second, first, rest)
                    }
                };
                if let Some(speed) = speed {
                    merge_contact(
                        events,
                        Event::BodyContact {
                            first: BodyId(index_u16(first)),
                            second: BodyId(index_u16(second)),
                            speed,
                        },
                    );
                }
                for detail in details {
                    merge_contact(events, detail);
                }
            }
        }
    }

    /// Where a body's parts are now (a plain disc is one rounded point).
    fn place(&self, index: usize) -> Vec<Placed> {
        let body = &self.bodies[index];
        let position = body.def.position;
        let Some(spin) = body.spin else {
            return vec![Placed::point(position, body.def.radius)];
        };
        let turn = sin_cos(spin.angle);
        self.parts[spin.first_part..spin.first_part + spin.part_count]
            .iter()
            .enumerate()
            .map(|(number, part)| place_part(position, spin.angle, turn, part, number))
            .collect()
    }

    /// Resolves every part pair of two bodies of which at least one is
    /// shaped and at most one kinematic. Returns the fastest approach and
    /// pushes part details and kicks to `details`.
    fn shaped_pair(
        &mut self,
        first: usize,
        second: usize,
        placed: &mut [Vec<Placed>],
        rest: i64,
        details: &mut Vec<Event>,
    ) -> Option<i64> {
        let restitution = self.bodies[first]
            .def
            .restitution
            .min(self.bodies[second].def.restitution);
        let friction = combined_friction(
            self.bodies[first]
                .spin
                .map_or(0, |spin| spin.surface_friction),
            self.bodies[second]
                .spin
                .map_or(0, |spin| spin.surface_friction),
        );
        let mut fastest: Option<i64> = None;
        for part_a in 0..placed[first].len() {
            for part_b in 0..placed[second].len() {
                let (a, b) = (placed[first][part_a], placed[second][part_b]);
                // Only kinematic parts kick, and at most one body is
                // kinematic, so the other is dynamic.
                let kicker = if a.kick > 0 {
                    Some((first, a.part, second, a.kick, false))
                } else if b.kick > 0 {
                    Some((second, b.part, first, b.kick, true))
                } else {
                    None
                };
                let margin = kicker.map_or(0, |_| (a.thickness.min(b.thickness) / 4).max(1));
                let Some(contact) = collide(&a, &b, margin) else {
                    if let Some((bumper, part, body, _, _)) = kicker {
                        self.touching.remove(&(
                            BodyId(index_u16(bumper)),
                            part,
                            BodyId(index_u16(body)),
                        ));
                    }
                    continue;
                };
                if contact.depth <= 0 {
                    continue;
                }
                let outcome = self.respond(
                    Some(first),
                    Some(second),
                    (a.pivot, b.pivot),
                    contact,
                    (restitution, friction, rest),
                );
                for shape in &mut placed[first] {
                    shape.translate(outcome.delta_a);
                }
                for shape in &mut placed[second] {
                    shape.translate(outcome.delta_b);
                }
                if let Some(speed) = outcome.speed {
                    fastest = Some(fastest.map_or(speed, |known| known.max(speed)));
                    details.push(Event::PartContact {
                        first: BodyId(index_u16(first)),
                        first_part: a.part,
                        second: BodyId(index_u16(second)),
                        second_part: b.part,
                        point: contact.point,
                        normal: contact.normal,
                        speed,
                    });
                }
                if let Some((bumper, part, body, kick, flipped)) = kicker {
                    let key = (BodyId(index_u16(bumper)), part, BodyId(index_u16(body)));
                    if self.touching.insert(key) {
                        let normal = if flipped {
                            -contact.normal
                        } else {
                            contact.normal
                        };
                        let target = &mut self.bodies[body].def;
                        target.velocity =
                            target.velocity + normal.scale(i128::from(kick), i128::from(UNIT));
                        details.push(Event::Bumped {
                            bumper: key.0,
                            part,
                            body: key.2,
                            normal,
                            speed: kick,
                        });
                    }
                }
            }
        }
        fastest
    }

    /// The motion of body `index`, or of a static wall for `None`.
    fn mover(&self, index: Option<usize>) -> Mover {
        let Some(index) = index else {
            return Mover::STATIC;
        };
        let body = &self.bodies[index];
        let dynamic = body.def.kind == BodyKind::Dynamic;
        let (omega, inertia) = body.spin.map_or((0, 0), |spin| {
            let rotates = dynamic && spin.max_angular_speed > 0;
            (
                spin.angular_velocity,
                if rotates { spin.inertia } else { 0 },
            )
        });
        Mover {
            dynamic,
            mass: body.def.mass.max(1),
            velocity: body.def.velocity,
            omega,
            inertia,
        }
    }

    /// Applies impulse `impulse` (mass times micro-units per second) along
    /// unit `direction` with lever arm `arm` to body `index`, if dynamic.
    fn apply_impulse(&mut self, index: Option<usize>, impulse: i128, direction: Vector, arm: i64) {
        let mover = self.mover(index);
        let Some(index) = index else {
            return;
        };
        if !mover.dynamic || impulse == 0 {
            return;
        }
        let body = &mut self.bodies[index];
        let mass = i128::from(mover.mass);
        body.def.velocity = body.def.velocity + direction.scale(impulse, mass * i128::from(UNIT));
        if mover.inertia > 0
            && let Some(spin) = &mut body.spin
        {
            let turn = crate::div_round(
                MICRORADIANS
                    .saturating_mul(i128::from(arm))
                    .saturating_mul(impulse),
                mass * i128::from(mover.inertia),
            );
            spin.angular_velocity = spin.angular_velocity.saturating_add(turn);
        }
    }

    /// Separates and bounces two shapes in contact (`None` is a static
    /// wall) with rotation and Coulomb friction. `contact.normal` points
    /// from `a` toward `b`; `pivots` are what each shape turns about and
    /// `material` is `(restitution, friction, rest speed)`.
    fn respond(
        &mut self,
        a: Option<usize>,
        b: Option<usize>,
        pivots: (Vector, Vector),
        contact: Contact,
        material: (u32, u32, i64),
    ) -> Outcome {
        let unit = i128::from(UNIT);
        let (restitution, friction, rest) = material;
        let (mover_a, mover_b) = (self.mover(a), self.mover(b));
        let normal = contact.normal;
        let depth = i128::from(contact.depth);
        let (delta_a, delta_b) = match (mover_a.dynamic, mover_b.dynamic) {
            (true, true) => {
                let total = i128::from(mover_a.mass) + i128::from(mover_b.mass);
                (
                    -normal.scale(depth * i128::from(mover_b.mass), unit * total),
                    normal.scale(depth * i128::from(mover_a.mass), unit * total),
                )
            }
            (true, false) => (-normal.scale(depth, unit), Vector::ZERO),
            (false, true) => (Vector::ZERO, normal.scale(depth, unit)),
            (false, false) => (Vector::ZERO, Vector::ZERO),
        };
        for (index, delta) in [(a, delta_a), (b, delta_b)] {
            if let Some(index) = index {
                let body = &mut self.bodies[index];
                body.def.position = body.def.position + delta;
            }
        }
        let mut outcome = Outcome {
            speed: None,
            delta_a,
            delta_b,
        };
        // After separation both surfaces meet here; arms are measured from
        // the moved pivots so a rolling disc's lever arm is its full radius.
        let meeting = contact.point
            + Vector::new(
                crate::div_round(i128::from(delta_a.x) + i128::from(delta_b.x), 2),
                crate::div_round(i128::from(delta_a.y) + i128::from(delta_b.y), 2),
            );
        let arm_a = meeting - (pivots.0 + delta_a);
        let arm_b = meeting - (pivots.1 + delta_b);
        let relative = point_velocity(mover_b, arm_b) - point_velocity(mover_a, arm_a);
        let approach = relative.dot(normal) / unit;
        if approach >= 0 {
            return outcome;
        }
        let (lever_a, lever_b) = (cross(arm_a, normal), cross(arm_b, normal));
        let resistance = mover_a.weight(lever_a) + mover_b.weight(lever_b);
        if resistance == 0 {
            return outcome;
        }
        let restitution = if -approach <= i128::from(rest) {
            0
        } else {
            i128::from(restitution)
        };
        let millionths = i128::from(MILLIONTHS);
        let impulse = (-approach)
            .saturating_mul(millionths + restitution)
            .saturating_mul(INVERSE_SCALE)
            / (millionths * resistance);
        self.apply_impulse(a, -impulse, normal, lever_a);
        self.apply_impulse(b, impulse, normal, lever_b);
        outcome.speed = Some(i64::try_from(-approach).unwrap_or(i64::MAX));
        if friction == 0 {
            return outcome;
        }
        let (mover_a, mover_b) = (self.mover(a), self.mover(b));
        let tangent = Vector::new(-normal.y, normal.x);
        let relative = point_velocity(mover_b, arm_b) - point_velocity(mover_a, arm_a);
        let slip = relative.dot(tangent) / unit;
        if slip == 0 {
            return outcome;
        }
        let (lever_a, lever_b) = (cross(arm_a, tangent), cross(arm_b, tangent));
        let resistance = mover_a.weight(lever_a) + mover_b.weight(lever_b);
        if resistance == 0 {
            return outcome;
        }
        let limit = impulse.saturating_mul(i128::from(friction)) / millionths;
        let grip = ((-slip).saturating_mul(INVERSE_SCALE) / resistance).clamp(-limit, limit);
        self.apply_impulse(a, -grip, tangent, lever_a);
        self.apply_impulse(b, grip, tangent, lever_b);
        outcome
    }

    /// Separates and bounces two overlapping dynamic bodies, weighted by
    /// mass. Returns the approach speed when they were approaching.
    fn dynamic_pair(&mut self, first: usize, second: usize, rest: i64) -> Option<i64> {
        let (a, b) = (self.bodies[first].def, self.bodies[second].def);
        let (normal, penetration) = overlap(a.position, b.position, a.radius + b.radius)?;
        let total = i128::from(a.mass) + i128::from(b.mass);
        let unit = i128::from(UNIT);
        let push_a = normal.scale(i128::from(penetration) * i128::from(b.mass), unit * total);
        let push_b = normal.scale(i128::from(penetration) * i128::from(a.mass), unit * total);
        self.bodies[first].def.position = a.position - push_a;
        self.bodies[second].def.position = b.position + push_b;
        let approach = (b.velocity - a.velocity).dot(normal) / unit;
        if approach >= 0 {
            return None;
        }
        let restitution = resting(approach, rest, a.restitution.min(b.restitution));
        let impulse = approach * (i128::from(MILLIONTHS) + restitution);
        let scale = unit * i128::from(MILLIONTHS) * total;
        self.bodies[first].def.velocity =
            a.velocity + normal.scale(impulse * i128::from(b.mass), scale);
        self.bodies[second].def.velocity =
            b.velocity - normal.scale(impulse * i128::from(a.mass), scale);
        Some(i64::try_from(-approach).unwrap_or(i64::MAX))
    }

    /// Pushes a dynamic body out of a kinematic one, which acts infinitely
    /// heavy. Returns the approach speed when they were approaching.
    fn kinematic_contact(&mut self, dynamic: usize, kinematic: usize, rest: i64) -> Option<i64> {
        let (d, k) = (self.bodies[dynamic].def, self.bodies[kinematic].def);
        let (normal, penetration) = overlap(k.position, d.position, d.radius + k.radius)?;
        let unit = i128::from(UNIT);
        self.bodies[dynamic].def.position =
            d.position + normal.scale(i128::from(penetration), unit);
        let approach = (d.velocity - k.velocity).dot(normal) / unit;
        if approach >= 0 {
            return None;
        }
        let restitution = resting(approach, rest, d.restitution.min(k.restitution));
        let impulse = approach * (i128::from(MILLIONTHS) + restitution);
        self.bodies[dynamic].def.velocity =
            d.velocity - normal.scale(impulse, unit * i128::from(MILLIONTHS));
        Some(i64::try_from(-approach).unwrap_or(i64::MAX))
    }

    /// Pushes dynamic bodies out of walls and bounces them.
    fn resolve_walls(&mut self, starts: &[Vector], events: &mut Vec<Event>, rest: i64) {
        for (index, start) in starts.iter().enumerate() {
            let body = &self.bodies[index];
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            if body.spin.is_none() {
                self.plain_walls(index, *start, events, rest);
            } else {
                self.shaped_walls(index, events, rest);
            }
        }
    }

    /// Pushes a plain disc out of walls and bounces it. A disc whose center
    /// crossed a wall's line within the segment during this substep (pushed
    /// by a kinematic body, say) returns to the side it started on.
    fn plain_walls(&mut self, index: usize, start: Vector, events: &mut Vec<Event>, rest: i64) {
        let unit = i128::from(UNIT);
        let body = &mut self.bodies[index];
        for (wall_index, wall) in self.walls.iter().enumerate() {
            if body.def.mask & wall.layers == 0 {
                continue;
            }
            let along = wall.end - wall.start;
            let side_before = side_of(wall.start, along, start);
            let crossed = crossed_within(wall.start, along, start, body.def.position);
            let closest = closest_on_segment(wall.start, wall.end, body.def.position);
            let offset = body.def.position - closest;
            let radius = body.def.radius;
            let distance_squared = offset.length_squared();
            if !crossed && distance_squared >= i128::from(radius) * i128::from(radius) {
                continue;
            }
            let distance = if crossed {
                0
            } else {
                integer_sqrt(distance_squared)
            };
            let normal = if distance == 0 {
                let left = unit_normal(Vector::new(-along.y, along.x));
                let toward_left = match side_before {
                    0 => body.def.velocity.dot(left) <= 0,
                    side => side > 0,
                };
                if toward_left { left } else { -left }
            } else {
                unit_normal(offset)
            };
            if crossed {
                body.def.position = closest;
            }
            body.def.position =
                body.def.position + normal.scale(i128::from(radius - distance), unit);
            let approach = body.def.velocity.dot(normal) / unit;
            if approach >= 0 {
                continue;
            }
            let restitution = resting(approach, rest, body.def.restitution.min(wall.restitution));
            let impulse = approach * (i128::from(MILLIONTHS) + restitution);
            body.def.velocity =
                body.def.velocity - normal.scale(impulse, unit * i128::from(MILLIONTHS));
            merge_contact(
                events,
                Event::WallContact {
                    body: BodyId(index_u16(index)),
                    wall: WallId(index_u16(wall_index)),
                    speed: i64::try_from(-approach).unwrap_or(i64::MAX),
                },
            );
        }
    }

    /// Pushes a shaped body's parts out of walls and bounces them.
    fn shaped_walls(&mut self, index: usize, events: &mut Vec<Event>, rest: i64) {
        let mut parts = self.place(index);
        let (restitution, friction, mask) = {
            let body = &self.bodies[index];
            (
                body.def.restitution,
                body.spin.map_or(0, |spin| spin.surface_friction),
                body.def.mask,
            )
        };
        for wall_index in 0..self.walls.len() {
            let wall = self.walls[wall_index];
            if mask & wall.layers == 0 {
                continue;
            }
            let segment = Placed::segment(wall.start, wall.end);
            let material = (
                restitution.min(wall.restitution),
                combined_friction(friction, self.wall_friction[wall_index]),
                rest,
            );
            let mut fastest: Option<i64> = None;
            let mut details = Vec::new();
            for number in 0..parts.len() {
                let part = parts[number];
                let Some(contact) = collide(&segment, &part, 0) else {
                    continue;
                };
                if contact.depth <= 0 {
                    continue;
                }
                let outcome = self.respond(
                    None,
                    Some(index),
                    (segment.pivot, part.pivot),
                    contact,
                    material,
                );
                for shape in &mut parts {
                    shape.translate(outcome.delta_b);
                }
                if let Some(speed) = outcome.speed {
                    fastest = Some(fastest.map_or(speed, |known| known.max(speed)));
                    details.push(Event::PartWallContact {
                        body: BodyId(index_u16(index)),
                        part: part.part,
                        wall: WallId(index_u16(wall_index)),
                        point: contact.point,
                        normal: contact.normal,
                        speed,
                    });
                }
            }
            if let Some(speed) = fastest {
                merge_contact(
                    events,
                    Event::WallContact {
                        body: BodyId(index_u16(index)),
                        wall: WallId(index_u16(wall_index)),
                        speed,
                    },
                );
            }
            for detail in details {
                merge_contact(events, detail);
            }
        }
    }

    /// Holds dynamic bodies inside their confinement (stopping outward
    /// motion) and within this tick's speed bound and angular cap.
    fn confine_dynamic(&mut self, bound: i64) {
        for body in &mut self.bodies {
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            if let Some(confine) = body.def.confine {
                let held = confine.clamp(body.def.position);
                if held.x != body.def.position.x {
                    body.def.velocity.x = 0;
                }
                if held.y != body.def.position.y {
                    body.def.velocity.y = 0;
                }
                body.def.position = held;
            }
            body.def.velocity = body
                .def
                .velocity
                .clamp_length(bound.min(body.def.max_speed));
            if let Some(spin) = &mut body.spin {
                spin.angular_velocity = spin
                    .angular_velocity
                    .clamp(-spin.max_angular_speed, spin.max_angular_speed);
            }
        }
    }

    fn sense(&mut self, events: &mut Vec<Event>) {
        for (sensor_index, sensor) in self.sensors.iter().enumerate() {
            let sensor_id = SensorId(index_u16(sensor_index));
            for (body_index, body) in self.bodies.iter().enumerate() {
                let body_id = BodyId(index_u16(body_index));
                let sensed = body.active
                    && body.def.layers & sensor.mask != 0
                    && sensor.region.contains(body.def.position);
                let key = (sensor_id, body_id);
                if sensed && self.occupancy.insert(key) {
                    events.push(Event::SensorEnter {
                        sensor: sensor_id,
                        body: body_id,
                    });
                } else if !sensed && self.occupancy.remove(&key) {
                    events.push(Event::SensorExit {
                        sensor: sensor_id,
                        body: body_id,
                    });
                }
            }
        }
    }

    /// Applies damping, friction and speed caps once per tick.
    fn decelerate(&mut self, tps: i128) {
        let per_tick = i128::from(MILLIONTHS) * tps;
        for body in &mut self.bodies {
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            let mut velocity = body.def.velocity;
            if body.def.damping > 0 {
                velocity = velocity.scale(per_tick - i128::from(body.def.damping), per_tick);
            }
            if body.def.friction > 0 {
                let speed = velocity.length();
                let slowdown = crate::div_round(i128::from(body.def.friction), tps);
                velocity = if speed <= slowdown {
                    Vector::ZERO
                } else {
                    velocity.scale(i128::from(speed - slowdown), i128::from(speed))
                };
            }
            body.def.velocity = velocity.clamp_length(body.def.max_speed);
            if let Some(spin) = &mut body.spin {
                if spin.angular_damping > 0 {
                    // Truncation toward zero lets slow spins come fully to
                    // rest.
                    let kept = i128::from(spin.angular_velocity)
                        * (per_tick - i128::from(spin.angular_damping))
                        / per_tick;
                    spin.angular_velocity = i64::try_from(kept).unwrap_or(0);
                }
                if body.def.friction > 0 {
                    // Table friction also brakes spin: the radius of
                    // gyration decelerates at the friction rate.
                    let gyration = i128::from(integer_sqrt(i128::from(spin.inertia)).max(1));
                    let slowdown = i64::try_from(
                        i128::from(body.def.friction) * MICRORADIANS / (gyration * tps),
                    )
                    .unwrap_or(i64::MAX);
                    let omega = spin.angular_velocity;
                    spin.angular_velocity = omega - omega.clamp(-slowdown, slowdown);
                }
            }
        }
    }
}

/// The restitution to use for an approach (negative) speed: none at or
/// below the rest speed, so resting contacts settle.
fn resting(approach: i128, rest: i64, restitution: u32) -> i128 {
    if -approach <= i128::from(rest) {
        0
    } else {
        i128::from(restitution)
    }
}

/// The geometric mean of two friction coefficients in millionths.
fn combined_friction(first: u32, second: u32) -> u32 {
    u32::try_from(integer_sqrt(i128::from(first) * i128::from(second))).unwrap_or(u32::MAX)
}

/// The velocity of the point at `arm` from a mover's pivot.
fn point_velocity(mover: Mover, arm: Vector) -> Vector {
    let omega = i128::from(mover.omega);
    mover.velocity
        + Vector::new(
            crate::div_round(-omega * i128::from(arm.y), MICRORADIANS),
            crate::div_round(omega * i128::from(arm.x), MICRORADIANS),
        )
}

/// The two-dimensional cross product of `arm` with a unit direction, in
/// micro-units.
fn cross(arm: Vector, direction: Vector) -> i64 {
    crate::div_round(
        i128::from(arm.x) * i128::from(direction.y) - i128::from(arm.y) * i128::from(direction.x),
        i128::from(UNIT),
    )
}

/// Places one part of a body at `position` turned by `angle` (whose sine
/// and cosine are `turn`).
fn place_part(
    position: Vector,
    angle: i64,
    turn: (i64, i64),
    part: &Part,
    number: usize,
) -> Placed {
    let (center, pivot) = match part.attachment {
        Attachment::Rigid => (position + rotate(part.offset, turn), position),
        Attachment::Spindle => {
            let center = position + part.offset;
            (center, center)
        }
    };
    let rotation = if part.angle == 0 {
        turn
    } else {
        sin_cos(angle + part.angle)
    };
    let mut placed = match part.shape {
        Shape::Disc { radius } => Placed::point(center, radius),
        Shape::Capsule {
            half_length,
            radius,
        } => {
            if half_length == 0 {
                Placed::point(center, radius)
            } else {
                let half = rotate(Vector::new(half_length, 0), rotation);
                let mut segment = Placed::segment(center - half, center + half);
                segment.radius = radius;
                segment
            }
        }
        Shape::Rect {
            half_width,
            half_height,
        } => {
            let corner = |x: i64, y: i64| center + rotate(Vector::new(x, y), rotation);
            Placed {
                verts: [
                    corner(-half_width, -half_height),
                    corner(half_width, -half_height),
                    corner(half_width, half_height),
                    corner(-half_width, half_height),
                ],
                count: 4,
                radius: 0,
                pivot,
                part: 0,
                kick: 0,
                thickness: 0,
            }
        }
    };
    placed.pivot = pivot;
    placed.part = u8::try_from(number).unwrap_or(u8::MAX);
    placed.kick = part.kick;
    placed.thickness = part.shape.thickness();
    placed
}

/// Whether a part's geometry and kick are valid for a body.
fn valid_part(part: &Part, dynamic: bool) -> bool {
    let size = 1..=MAX_EXTENT;
    let shape = match part.shape {
        Shape::Disc { radius } => size.contains(&radius),
        Shape::Rect {
            half_width,
            half_height,
        } => size.contains(&half_width) && size.contains(&half_height),
        Shape::Capsule {
            half_length,
            radius,
        } => (0..=MAX_EXTENT).contains(&half_length) && size.contains(&radius),
    };
    shape
        && part.offset.max_abs() <= MAX_EXTENT
        && part.offset.x != i64::MIN
        && part.offset.y != i64::MIN
        && part.angle.unsigned_abs() <= MAX_MAGNITUDE.unsigned_abs()
        && (0..=MAX_MAGNITUDE).contains(&part.kick)
        && !(dynamic && (part.kick > 0 || part.attachment == Attachment::Spindle))
}

/// Whether a drive's rates are in range.
fn valid_drive(drive: Drive) -> bool {
    (0..=MAX_MAGNITUDE).contains(&drive.acceleration)
        && (0..=MAX_ANGULAR_SPEED).contains(&drive.turn_rate)
        && (0..=MAX_MAGNITUDE).contains(&drive.turn_acceleration)
        && (0..=MAX_MAGNITUDE).contains(&drive.grip)
}

/// The length of `vector`, rounded up.
fn ceil_length(vector: Vector) -> i64 {
    let squared = vector.length_squared();
    let root = integer_sqrt(squared);
    if i128::from(root) * i128::from(root) < squared {
        root + 1
    } else {
        root
    }
}

/// The moment of inertia per unit mass about the origin of uniform parts,
/// weighted by area (π as 355/113), in square micro-units; at least one.
fn derived_inertia(parts: &[Part]) -> i64 {
    let disc_area = |radius: i128| 355 * radius * radius / 113;
    let (mut weighted, mut total) = (0_i128, 0_i128);
    for part in parts {
        let (area, own) = match part.shape {
            Shape::Disc { radius } => {
                let radius = i128::from(radius);
                (disc_area(radius), radius * radius / 2)
            }
            Shape::Rect {
                half_width,
                half_height,
            } => {
                let (w, h) = (i128::from(half_width), i128::from(half_height));
                (4 * w * h, (w * w + h * h) / 3)
            }
            Shape::Capsule {
                half_length,
                radius,
            } => {
                let (l, r) = (i128::from(half_length), i128::from(radius));
                let (core, caps) = (4 * l * r, disc_area(r));
                let area = core + caps;
                let own = (core * ((l * l + r * r) / 3) + caps * (r * r / 2 + l * l)) / area.max(1);
                (area, own)
            }
        };
        weighted += area * (own + part.offset.length_squared());
        total += area;
    }
    i64::try_from(weighted / total.max(1))
        .unwrap_or(i64::MAX)
        .max(1)
}

const fn check_angle(angle: i64) -> Result<i64, WorldError> {
    if angle.unsigned_abs() > MAX_MAGNITUDE.unsigned_abs() {
        Err(WorldError::OutOfRange)
    } else {
        Ok(angle)
    }
}

pub fn check_region(region: Region) -> Result<Region, WorldError> {
    if let Region::Circle { center, radius } = region {
        check_vector(center)?;
        if !(0..=MAX_MAGNITUDE).contains(&radius) {
            return Err(WorldError::InvalidParameter);
        }
    }
    Ok(region)
}

fn index_u16(index: usize) -> u16 {
    u16::try_from(index).unwrap_or(u16::MAX)
}

/// The unit normal from `from` toward `to` and their penetration depth when
/// discs centered there with radii summing to `reach` overlap. Coincident
/// centers separate along +x.
fn overlap(from: Vector, to: Vector, reach: i64) -> Option<(Vector, i64)> {
    let offset = to - from;
    let distance_squared = offset.length_squared();
    if distance_squared >= i128::from(reach) * i128::from(reach) {
        return None;
    }
    let distance = integer_sqrt(distance_squared);
    let normal = if distance == 0 {
        Vector::new(UNIT, 0)
    } else {
        unit_normal(offset)
    };
    Some((normal, reach - distance))
}

/// Which side of the line through `start` along `along` a point lies on:
/// positive to the left, negative to the right, zero on it.
fn side_of(start: Vector, along: Vector, point: Vector) -> i128 {
    let offset = point - start;
    (i128::from(along.x) * i128::from(offset.y) - i128::from(along.y) * i128::from(offset.x))
        .signum()
}

/// Whether the path from `from` to `to` crosses the segment from `start`
/// along `along` strictly from one side to the other.
fn crossed_within(start: Vector, along: Vector, from: Vector, to: Vector) -> bool {
    let cross = |point: Vector| {
        let offset = point - start;
        i128::from(along.x) * i128::from(offset.y) - i128::from(along.y) * i128::from(offset.x)
    };
    let (before, after) = (cross(from), cross(to));
    if before == 0 || before.signum() == after.signum() {
        return false;
    }
    let crossing = from + (to - from).scale(before, before - after);
    let projection = (crossing - start).dot(along);
    (0..=along.length_squared()).contains(&projection)
}

/// Records a contact, merging it with an earlier one of the same pair (or
/// part pair) this tick by keeping the faster approach.
fn merge_contact(events: &mut Vec<Event>, event: Event) {
    let key = |event: &Event| match *event {
        Event::BodyContact { first, second, .. } => Some((0, first.0, 0, second.0, 0)),
        Event::WallContact { body, wall, .. } => Some((1, body.0, 0, wall.0, 0)),
        Event::PartContact {
            first,
            first_part,
            second,
            second_part,
            ..
        } => Some((2, first.0, first_part, second.0, second_part)),
        Event::PartWallContact {
            body, part, wall, ..
        } => Some((3, body.0, part, wall.0, 0)),
        _ => None,
    };
    let Some(wanted) = key(&event) else {
        events.push(event);
        return;
    };
    let speed_of = |event: &Event| match *event {
        Event::BodyContact { speed, .. }
        | Event::WallContact { speed, .. }
        | Event::PartContact { speed, .. }
        | Event::PartWallContact { speed, .. } => speed,
        _ => 0,
    };
    if let Some(existing) = events
        .iter_mut()
        .find(|existing| key(existing) == Some(wanted))
    {
        if speed_of(&event) > speed_of(existing) {
            *existing = event;
        }
    } else {
        events.push(event);
    }
}
