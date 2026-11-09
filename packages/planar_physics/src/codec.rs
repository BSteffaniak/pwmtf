//! The versioned binary encoding of a complete world.
//!
//! Little-endian. A world using only plain discs, walls without friction,
//! no acceleration and no fields encodes as version 1 (unchanged, so
//! existing checksums still hold); anything else encodes as version 2.
//! Decoding accepts both and rejects every other version.
//!
//! Version 1, in order: version `u16`; configuration (tick rate `u16`,
//! substeps `u8`, speed limit `i64`); tick `u64`; walls, bodies and sensors
//! each as a `u16` count followed by their fields; then the sensed
//! `(sensor, body)` pairs as a `u32` count of `u16` pairs.
//!
//! Version 2 adds, in order: the world acceleration after the tick; each
//! wall's friction `u32` after its layers; after each body a shape tag (`0`
//! plain disc, `1` shaped, followed by its rotation state, parameters and
//! parts); after the sensed pairs the fields (a `u16` count of region,
//! acceleration and mask) and the kicking contacts in progress (a `u32`
//! count of bumper `u16`, part `u8` and body `u16`).
//!
//! Decoding re-validates everything through the same checks as
//! construction.

use thiserror::Error;

use crate::world::{Body, World, check_region};
use crate::{
    Aabb, Attachment, BodyDef, BodyId, BodyKind, Drive, FieldDef, Part, Region, SensorDef,
    SensorId, Shape, ShapedBodyDef, Vector, WallDef, WorldConfig, WorldError, check_vector,
};

/// The newest world encoding version, used for worlds with shaped bodies,
/// acceleration, fields or wall friction.
pub const WORLD_STATE_VERSION: u16 = 2;

/// The original encoding version, still written for worlds of only plain
/// discs, frictionless walls and no acceleration, and still decoded.
pub const DISC_WORLD_STATE_VERSION: u16 = 1;

/// Why bytes could not be decoded into a world.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DecodeError {
    /// The encoding version is neither [`DISC_WORLD_STATE_VERSION`] nor
    /// [`WORLD_STATE_VERSION`].
    #[error("unsupported world state version {0}")]
    UnsupportedVersion(u16),
    /// The bytes ended early.
    #[error("world state is truncated")]
    Truncated,
    /// Bytes remain after a complete world.
    #[error("world state has trailing bytes")]
    TrailingBytes,
    /// A tag byte has no meaning.
    #[error("world state contains an unknown tag")]
    UnknownTag,
    /// A decoded value fails validation.
    #[error("world state is invalid: {0}")]
    Invalid(#[from] WorldError),
}

impl World {
    /// Whether the world needs nothing beyond version 1.
    fn is_disc_only(&self) -> bool {
        self.gravity == Vector::ZERO
            && self.fields.is_empty()
            && self.touching.is_empty()
            && self.wall_friction.iter().all(|friction| *friction == 0)
            && self.bodies.iter().all(|body| body.spin.is_none())
    }

    /// Encodes the complete world, including parameters and sensor state,
    /// in the oldest version that can represent it.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let extended = !self.is_disc_only();
        let mut out = Writer(Vec::with_capacity(64 + self.bodies.len() * 128));
        out.u16(if extended {
            WORLD_STATE_VERSION
        } else {
            DISC_WORLD_STATE_VERSION
        });
        out.u16(self.config.ticks_per_second);
        out.u8(self.config.max_substeps);
        out.i64(self.config.max_speed);
        out.u64(self.tick);
        if extended {
            out.vector(self.gravity);
        }
        out.u16(len_u16(self.walls.len()));
        for (wall, friction) in self.walls.iter().zip(&self.wall_friction) {
            out.vector(wall.start);
            out.vector(wall.end);
            out.u32(wall.restitution);
            out.u32(wall.layers);
            if extended {
                out.u32(*friction);
            }
        }
        out.u16(len_u16(self.bodies.len()));
        for body in &self.bodies {
            let def = &body.def;
            out.u8(match def.kind {
                BodyKind::Dynamic => 0,
                BodyKind::Kinematic => 1,
            });
            out.i64(def.radius);
            out.i64(def.mass);
            out.vector(def.position);
            out.vector(def.velocity);
            out.u32(def.restitution);
            out.u32(def.damping);
            out.i64(def.friction);
            out.i64(def.max_speed);
            match def.confine {
                None => out.u8(0),
                Some(confine) => {
                    out.u8(1);
                    out.vector(confine.min());
                    out.vector(confine.max());
                }
            }
            out.u32(def.layers);
            out.u32(def.mask);
            out.vector(body.target);
            out.u8(u8::from(body.active));
            if extended {
                self.write_spin(&mut out, body);
            }
        }
        out.u16(len_u16(self.sensors.len()));
        for sensor in &self.sensors {
            out.region(sensor.region);
            out.u32(sensor.mask);
        }
        out.u32(u32::try_from(self.occupancy.len()).unwrap_or(u32::MAX));
        for (sensor, body) in &self.occupancy {
            out.u16(sensor.0);
            out.u16(body.0);
        }
        if extended {
            out.u16(len_u16(self.fields.len()));
            for field in &self.fields {
                out.region(field.region);
                out.vector(field.acceleration);
                out.u32(field.mask);
            }
            out.u32(u32::try_from(self.touching.len()).unwrap_or(u32::MAX));
            for (bumper, part, body) in &self.touching {
                out.u16(bumper.0);
                out.u8(*part);
                out.u16(body.0);
            }
        }
        out.0
    }

    fn write_spin(&self, out: &mut Writer, body: &Body) {
        let Some(spin) = body.spin else {
            out.u8(0);
            return;
        };
        out.u8(1);
        out.i64(spin.angle);
        out.i64(spin.angular_velocity);
        out.i64(spin.angle_target);
        out.i64(spin.max_angular_speed);
        out.u32(spin.angular_damping);
        out.u32(spin.surface_friction);
        match spin.angle_range {
            None => out.u8(0),
            Some((min, max)) => {
                out.u8(1);
                out.i64(min);
                out.i64(max);
            }
        }
        out.i64(spin.inertia_setting);
        match spin.drive {
            None => out.u8(0),
            Some(drive) => {
                out.u8(1);
                out.i64(drive.acceleration);
                out.i64(drive.turn_rate);
                out.i64(drive.turn_acceleration);
                out.i64(drive.grip);
            }
        }
        out.i32(spin.throttle);
        out.i32(spin.steer);
        let parts = &self.parts[spin.first_part..spin.first_part + spin.part_count];
        out.u8(u8::try_from(parts.len()).unwrap_or(u8::MAX));
        for part in parts {
            match part.shape {
                Shape::Disc { radius } => {
                    out.u8(0);
                    out.i64(radius);
                }
                Shape::Rect {
                    half_width,
                    half_height,
                } => {
                    out.u8(1);
                    out.i64(half_width);
                    out.i64(half_height);
                }
                Shape::Capsule {
                    half_length,
                    radius,
                } => {
                    out.u8(2);
                    out.i64(half_length);
                    out.i64(radius);
                }
            }
            out.vector(part.offset);
            out.i64(part.angle);
            out.u8(match part.attachment {
                Attachment::Rigid => 0,
                Attachment::Spindle => 1,
            });
            out.i64(part.kick);
        }
    }

    /// Decodes a world produced by [`World::to_bytes`], of either version.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] for an unknown version, truncated or trailing
    /// bytes, unknown tags, or values that fail validation (including
    /// sensed pairs or kicking contacts naming missing sensors, bodies or
    /// parts).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut input = Reader(bytes);
        let version = input.u16()?;
        let extended = match version {
            DISC_WORLD_STATE_VERSION => false,
            WORLD_STATE_VERSION => true,
            _ => return Err(DecodeError::UnsupportedVersion(version)),
        };
        let config = WorldConfig {
            ticks_per_second: input.u16()?,
            max_substeps: input.u8()?,
            max_speed: input.i64()?,
        };
        let mut world = Self::new(config)?;
        world.tick = input.u64()?;
        if extended {
            world.set_gravity(input.vector()?)?;
        }
        for _ in 0..input.u16()? {
            let id = world.add_wall(WallDef {
                start: input.vector()?,
                end: input.vector()?,
                restitution: input.u32()?,
                layers: input.u32()?,
            })?;
            if extended {
                world.set_wall_friction(id, input.u32()?)?;
            }
        }
        for _ in 0..input.u16()? {
            read_body(&mut input, &mut world, extended)?;
        }
        for _ in 0..input.u16()? {
            let region = input.region()?;
            world.add_sensor(SensorDef {
                region,
                mask: input.u32()?,
            })?;
        }
        for _ in 0..input.u32()? {
            let sensor = SensorId(input.u16()?);
            let body = BodyId(input.u16()?);
            if usize::from(sensor.0) >= world.sensors.len()
                || usize::from(body.0) >= world.bodies.len()
            {
                return Err(DecodeError::Invalid(WorldError::UnknownBody));
            }
            world.occupancy.insert((sensor, body));
        }
        if extended {
            for _ in 0..input.u16()? {
                let region = check_region(input.region()?)?;
                world.add_field(FieldDef {
                    region,
                    acceleration: input.vector()?,
                    mask: input.u32()?,
                })?;
            }
            for _ in 0..input.u32()? {
                let bumper = BodyId(input.u16()?);
                let part = input.u8()?;
                let body = BodyId(input.u16()?);
                let kicks = world
                    .parts(bumper)
                    .get(usize::from(part))
                    .is_some_and(|part| part.kick > 0);
                if !kicks || usize::from(body.0) >= world.bodies.len() {
                    return Err(DecodeError::Invalid(WorldError::UnknownBody));
                }
                world.touching.insert((bumper, part, body));
            }
        }
        if !input.0.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(world)
    }

    /// A stable FNV-1a checksum of [`World::to_bytes`].
    #[must_use]
    pub fn checksum(&self) -> u64 {
        fnv1a(&self.to_bytes())
    }
}

/// Decodes one body record and adds it to `world`.
fn read_body(input: &mut Reader<'_>, world: &mut World, extended: bool) -> Result<(), DecodeError> {
    let kind = match input.u8()? {
        0 => BodyKind::Dynamic,
        1 => BodyKind::Kinematic,
        _ => return Err(DecodeError::UnknownTag),
    };
    let mut def = BodyDef {
        kind,
        radius: input.i64()?,
        mass: input.i64()?,
        position: input.vector()?,
        velocity: input.vector()?,
        restitution: input.u32()?,
        damping: input.u32()?,
        friction: input.i64()?,
        max_speed: input.i64()?,
        confine: None,
        layers: 0,
        mask: 0,
    };
    def.confine = match input.u8()? {
        0 => None,
        1 => Some(Aabb::new(input.vector()?, input.vector()?)?),
        _ => return Err(DecodeError::UnknownTag),
    };
    def.layers = input.u32()?;
    def.mask = input.u32()?;
    let target = check_vector(input.vector()?)?;
    let active = input.flag()?;
    let velocity = def.velocity;
    let shaped = extended && input.flag()?;
    let id = if shaped {
        let (shaped, state) = read_spin(input, def)?;
        let id = world.add_shaped_body(shaped)?;
        let spin = world.bodies[usize::from(id.0)]
            .spin
            .as_mut()
            .ok_or(DecodeError::Invalid(WorldError::NotShaped))?;
        // The angular velocity must respect the effective cap.
        if state.angular_velocity.unsigned_abs() > spin.max_angular_speed.unsigned_abs() {
            return Err(DecodeError::Invalid(WorldError::InvalidParameter));
        }
        // Restore the exact state; construction may have clamped it.
        spin.angle = state.angle;
        spin.angular_velocity = state.angular_velocity;
        spin.angle_target = state.angle_target;
        spin.throttle = state.throttle;
        spin.steer = state.steer;
        id
    } else {
        world.add_body(def)?
    };
    let body: &mut Body = &mut world.bodies[usize::from(id.0)];
    // Kinematic velocity records last tick's motion; keep it exact.
    body.def.velocity = velocity;
    body.target = target;
    body.active = active;
    Ok(())
}

/// A shaped body's exact rotation state, restored after construction.
struct SpinState {
    angle: i64,
    angular_velocity: i64,
    angle_target: i64,
    throttle: i32,
    steer: i32,
}

fn read_spin(
    input: &mut Reader<'_>,
    body: BodyDef,
) -> Result<(ShapedBodyDef, SpinState), DecodeError> {
    let angle = check_angle(input.i64()?)?;
    let angular_velocity = check_angle(input.i64()?)?;
    let angle_target = check_angle(input.i64()?)?;
    let mut def = ShapedBodyDef::new(body, Vec::new());
    def.angle = angle;
    def.angular_velocity =
        angular_velocity.clamp(-crate::MAX_ANGULAR_SPEED, crate::MAX_ANGULAR_SPEED);
    def.max_angular_speed = input.i64()?;
    def.angular_damping = input.u32()?;
    def.surface_friction = input.u32()?;
    def.angle_range = match input.u8()? {
        0 => None,
        1 => Some((input.i64()?, input.i64()?)),
        _ => return Err(DecodeError::UnknownTag),
    };
    def.inertia_per_mass = input.i64()?;
    def.drive = match input.u8()? {
        0 => None,
        1 => Some(Drive {
            acceleration: input.i64()?,
            turn_rate: input.i64()?,
            turn_acceleration: input.i64()?,
            grip: input.i64()?,
        }),
        _ => return Err(DecodeError::UnknownTag),
    };
    let throttle = input.i32()?;
    let steer = input.i32()?;
    let limit = i32::try_from(crate::MILLIONTHS).unwrap_or(i32::MAX);
    if throttle.unsigned_abs() > limit.unsigned_abs() || steer.unsigned_abs() > limit.unsigned_abs()
    {
        return Err(DecodeError::Invalid(WorldError::InvalidParameter));
    }
    for _ in 0..input.u8()? {
        let shape = match input.u8()? {
            0 => Shape::Disc {
                radius: input.i64()?,
            },
            1 => Shape::Rect {
                half_width: input.i64()?,
                half_height: input.i64()?,
            },
            2 => Shape::Capsule {
                half_length: input.i64()?,
                radius: input.i64()?,
            },
            _ => return Err(DecodeError::UnknownTag),
        };
        let offset = input.vector()?;
        let angle = input.i64()?;
        let attachment = match input.u8()? {
            0 => Attachment::Rigid,
            1 => Attachment::Spindle,
            _ => return Err(DecodeError::UnknownTag),
        };
        def.parts.push(Part {
            shape,
            offset,
            angle,
            attachment,
            kick: input.i64()?,
        });
    }
    Ok((
        def,
        SpinState {
            angle,
            angular_velocity,
            angle_target,
            throttle,
            steer,
        },
    ))
}

const fn check_angle(angle: i64) -> Result<i64, DecodeError> {
    if angle.unsigned_abs() > crate::MAX_MAGNITUDE.unsigned_abs() {
        Err(DecodeError::Invalid(WorldError::OutOfRange))
    } else {
        Ok(angle)
    }
}

/// FNV-1a over `bytes`, the checksum PWMTF uses for canonical encodings.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn len_u16(len: usize) -> u16 {
    u16::try_from(len).unwrap_or(u16::MAX)
}

struct Writer(Vec<u8>);

impl Writer {
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.0.extend(value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.0.extend(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.0.extend(value.to_le_bytes());
    }

    fn i64(&mut self, value: i64) {
        self.0.extend(value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.0.extend(value.to_le_bytes());
    }

    fn vector(&mut self, value: Vector) {
        self.i64(value.x);
        self.i64(value.y);
    }

    fn region(&mut self, region: Region) {
        match region {
            Region::Rect(rect) => {
                self.u8(0);
                self.vector(rect.min());
                self.vector(rect.max());
            }
            Region::Circle { center, radius } => {
                self.u8(1);
                self.vector(center);
                self.i64(radius);
            }
        }
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let (head, tail) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(DecodeError::Truncated)?;
        self.0 = tail;
        Ok(*head)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        self.take().map(u16::from_le_bytes)
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.take().map(u32::from_le_bytes)
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        self.take().map(u64::from_le_bytes)
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        self.take().map(i64::from_le_bytes)
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        self.take().map(i32::from_le_bytes)
    }

    fn vector(&mut self) -> Result<Vector, DecodeError> {
        Ok(Vector::new(self.i64()?, self.i64()?))
    }

    fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::UnknownTag),
        }
    }

    fn region(&mut self) -> Result<Region, DecodeError> {
        Ok(match self.u8()? {
            0 => Region::Rect(Aabb::new(self.vector()?, self.vector()?)?),
            1 => Region::Circle {
                center: self.vector()?,
                radius: self.i64()?,
            },
            _ => return Err(DecodeError::UnknownTag),
        })
    }
}
