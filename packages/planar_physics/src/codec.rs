//! The versioned binary encoding of a complete world.
//!
//! Little-endian, in this order: version `u16`; configuration (tick rate
//! `u16`, substeps `u8`, speed limit `i64`); tick `u64`; walls, bodies and
//! sensors each as a `u16` count followed by their fields; then the sensed
//! `(sensor, body)` pairs as a `u32` count of `u16` pairs. Decoding
//! re-validates everything through the same checks as construction.

use thiserror::Error;

use crate::world::{Body, World};
use crate::{
    Aabb, BodyDef, BodyId, BodyKind, Region, SensorDef, SensorId, Vector, WallDef, WorldConfig,
    WorldError, check_vector,
};

/// The current world encoding version.
pub const WORLD_STATE_VERSION: u16 = 1;

/// Why bytes could not be decoded into a world.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum DecodeError {
    /// The encoding version is not [`WORLD_STATE_VERSION`].
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
    /// Encodes the complete world, including parameters and sensor state.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Writer(Vec::with_capacity(64 + self.bodies.len() * 128));
        out.u16(WORLD_STATE_VERSION);
        out.u16(self.config.ticks_per_second);
        out.u8(self.config.max_substeps);
        out.i64(self.config.max_speed);
        out.u64(self.tick);
        out.u16(len_u16(self.walls.len()));
        for wall in &self.walls {
            out.vector(wall.start);
            out.vector(wall.end);
            out.u32(wall.restitution);
            out.u32(wall.layers);
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
        }
        out.u16(len_u16(self.sensors.len()));
        for sensor in &self.sensors {
            match sensor.region {
                Region::Rect(rect) => {
                    out.u8(0);
                    out.vector(rect.min());
                    out.vector(rect.max());
                }
                Region::Circle { center, radius } => {
                    out.u8(1);
                    out.vector(center);
                    out.i64(radius);
                }
            }
            out.u32(sensor.mask);
        }
        out.u32(u32::try_from(self.occupancy.len()).unwrap_or(u32::MAX));
        for (sensor, body) in &self.occupancy {
            out.u16(sensor.0);
            out.u16(body.0);
        }
        out.0
    }

    /// Decodes a world produced by [`World::to_bytes`].
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] for an unknown version, truncated or trailing
    /// bytes, unknown tags, or values that fail validation (including
    /// sensed pairs naming missing sensors or bodies).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut input = Reader(bytes);
        let version = input.u16()?;
        if version != WORLD_STATE_VERSION {
            return Err(DecodeError::UnsupportedVersion(version));
        }
        let config = WorldConfig {
            ticks_per_second: input.u16()?,
            max_substeps: input.u8()?,
            max_speed: input.i64()?,
        };
        let mut world = Self::new(config)?;
        world.tick = input.u64()?;
        for _ in 0..input.u16()? {
            world.add_wall(WallDef {
                start: input.vector()?,
                end: input.vector()?,
                restitution: input.u32()?,
                layers: input.u32()?,
            })?;
        }
        for _ in 0..input.u16()? {
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
            let active = match input.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::UnknownTag),
            };
            let velocity = def.velocity;
            let id = world.add_body(def)?;
            let body: &mut Body = &mut world.bodies[usize::from(id.0)];
            // Kinematic velocity records last tick's motion; keep it exact.
            body.def.velocity = velocity;
            body.target = target;
            body.active = active;
        }
        for _ in 0..input.u16()? {
            let region = match input.u8()? {
                0 => Region::Rect(Aabb::new(input.vector()?, input.vector()?)?),
                1 => Region::Circle {
                    center: input.vector()?,
                    radius: input.i64()?,
                },
                _ => return Err(DecodeError::UnknownTag),
            };
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

    fn vector(&mut self, value: Vector) {
        self.i64(value.x);
        self.i64(value.y);
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

    fn vector(&mut self) -> Result<Vector, DecodeError> {
        Ok(Vector::new(self.i64()?, self.i64()?))
    }
}
