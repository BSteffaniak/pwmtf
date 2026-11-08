//! The simulated world and its fixed-tick step.

use std::collections::BTreeSet;

use crate::{
    BodyDef, BodyId, BodyKind, Event, MAX_MAGNITUDE, MILLIONTHS, Region, SensorDef, SensorId,
    StepReport, Vector, WallDef, WallId, WorldConfig, WorldError, check_vector, integer_sqrt,
};

/// Fixed-point scale of unit normals.
const UNIT: i64 = 1 << 30;
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

    /// The disc radius in micro-units.
    #[must_use]
    pub const fn radius(&self) -> i64 {
        self.def.radius
    }

    /// Whether the body takes part in simulation.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.active
    }

    /// The body's parameters, with its current position and velocity.
    #[must_use]
    pub const fn def(&self) -> &BodyDef {
        &self.def
    }
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
        if def.radius < minimum {
            return Err(WorldError::BodyTooSmall {
                radius: def.radius,
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
        self.bodies.push(Body {
            target: def.position,
            def,
            active: true,
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

    /// Advances exactly one tick.
    pub fn step(&mut self) -> StepReport {
        let tps = i128::from(self.config.ticks_per_second);
        let plans = self.plan_kinematics(tps);
        let substeps = self.substeps(tps);
        let bound = self.speed_bound();
        let mut report = StepReport {
            tick: self.tick,
            substeps,
            events: Vec::new(),
        };
        let divisor = tps * i128::from(substeps);
        for step in 1..=i128::from(substeps) {
            let starts: Vec<Vector> = self.bodies.iter().map(Body::position).collect();
            for (body, plan) in self.bodies.iter_mut().zip(&plans) {
                if !body.active {
                    continue;
                }
                match plan {
                    Some((start, end)) => {
                        body.def.position =
                            *start + (*end - *start).scale(step, i128::from(substeps));
                    }
                    None => {
                        body.def.position = body.def.position + body.def.velocity.scale(1, divisor);
                    }
                }
            }
            self.resolve_bodies(&mut report.events);
            self.resolve_walls(&starts, &mut report.events);
            self.confine_dynamic(bound);
            self.sense(&mut report.events);
        }
        self.decelerate(tps);
        self.tick += 1;
        report
    }

    /// Each active kinematic body's start and end position for this tick;
    /// also records its velocity for contacts.
    fn plan_kinematics(&mut self, tps: i128) -> Vec<Option<(Vector, Vector)>> {
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
                Some((start, end))
            })
            .collect()
    }

    /// The fastest any body may move this tick: the world's limit, or less
    /// when everything is slow.
    fn speed_bound(&self) -> i64 {
        let fastest = self
            .bodies
            .iter()
            .filter(|body| body.active)
            .map(|body| body.def.velocity.length())
            .max()
            .unwrap_or(0);
        fastest
            .saturating_mul(SPEED_GROWTH)
            .min(self.config.max_speed)
    }

    /// Enough substeps that no body moves more than half the smallest
    /// radius per substep at [`World::speed_bound`].
    fn substeps(&self, tps: i128) -> u8 {
        let smallest = self
            .bodies
            .iter()
            .filter(|body| body.active)
            .map(|body| body.def.radius)
            .min()
            .unwrap_or(1)
            .max(1);
        let needed = crate::ceil_div(
            i128::from(self.speed_bound()) * 2,
            tps * i128::from(smallest),
        );
        u8::try_from(needed.clamp(1, i128::from(self.config.max_substeps))).unwrap_or(1)
    }

    fn resolve_bodies(&mut self, events: &mut Vec<Event>) {
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
                let speed = match (a.def.kind, b.def.kind) {
                    (BodyKind::Dynamic, BodyKind::Dynamic) => self.dynamic_pair(first, second),
                    (BodyKind::Dynamic, BodyKind::Kinematic) => {
                        self.kinematic_contact(first, second)
                    }
                    (BodyKind::Kinematic, BodyKind::Dynamic) => {
                        self.kinematic_contact(second, first)
                    }
                    (BodyKind::Kinematic, BodyKind::Kinematic) => None,
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
            }
        }
    }

    /// Separates and bounces two overlapping dynamic bodies, weighted by
    /// mass. Returns the approach speed when they were approaching.
    fn dynamic_pair(&mut self, first: usize, second: usize) -> Option<i64> {
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
        let restitution = i128::from(a.restitution.min(b.restitution));
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
    fn kinematic_contact(&mut self, dynamic: usize, kinematic: usize) -> Option<i64> {
        let (d, k) = (self.bodies[dynamic].def, self.bodies[kinematic].def);
        let (normal, penetration) = overlap(k.position, d.position, d.radius + k.radius)?;
        let unit = i128::from(UNIT);
        self.bodies[dynamic].def.position =
            d.position + normal.scale(i128::from(penetration), unit);
        let approach = (d.velocity - k.velocity).dot(normal) / unit;
        if approach >= 0 {
            return None;
        }
        let restitution = i128::from(d.restitution.min(k.restitution));
        let impulse = approach * (i128::from(MILLIONTHS) + restitution);
        self.bodies[dynamic].def.velocity =
            d.velocity - normal.scale(impulse, unit * i128::from(MILLIONTHS));
        Some(i64::try_from(-approach).unwrap_or(i64::MAX))
    }

    /// Pushes dynamic bodies out of walls and bounces them. A body whose
    /// center crossed a wall's line within the segment during this substep
    /// (pushed by a kinematic body, say) returns to the side it started on.
    fn resolve_walls(&mut self, starts: &[Vector], events: &mut Vec<Event>) {
        let unit = i128::from(UNIT);
        for (index, body) in self.bodies.iter_mut().enumerate() {
            if !body.active || body.def.kind != BodyKind::Dynamic {
                continue;
            }
            for (wall_index, wall) in self.walls.iter().enumerate() {
                if body.def.mask & wall.layers == 0 {
                    continue;
                }
                let along = wall.end - wall.start;
                let side_before = side_of(wall.start, along, starts[index]);
                let crossed = crossed_within(wall.start, along, starts[index], body.def.position);
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
                let restitution = i128::from(body.def.restitution.min(wall.restitution));
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
    }

    /// Holds dynamic bodies inside their confinement (stopping outward
    /// motion) and within this tick's speed bound.
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
        }
    }
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

/// `offset` scaled to length [`UNIT`]. The length is computed with extra
/// fractional bits so the normal's error stays far below one part in a
/// million even for short offsets. `offset` must not be zero.
fn unit_normal(offset: Vector) -> Vector {
    let squared = offset.length_squared();
    let mut shift = 0_u32;
    while shift < 30 && squared << (2 * (shift + 1)) < 1_i128 << 124 {
        shift += 1;
    }
    let precise = integer_sqrt(squared << (2 * shift)).max(1);
    offset.scale(i128::from(UNIT) << shift, i128::from(precise))
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

fn closest_on_segment(start: Vector, end: Vector, point: Vector) -> Vector {
    let along = end - start;
    let length_squared = along.length_squared();
    let projection = (point - start).dot(along).clamp(0, length_squared);
    start + along.scale(projection, length_squared)
}

/// Records a contact, merging it with an earlier one of the same pair this
/// tick by keeping the faster approach.
fn merge_contact(events: &mut Vec<Event>, event: Event) {
    let same_pair = |existing: &Event| match (existing, &event) {
        (
            Event::BodyContact { first, second, .. },
            Event::BodyContact {
                first: f,
                second: s,
                ..
            },
        ) => first == f && second == s,
        (
            Event::WallContact { body, wall, .. },
            Event::WallContact {
                body: b, wall: w, ..
            },
        ) => body == b && wall == w,
        _ => false,
    };
    if let Some(existing) = events.iter_mut().find(|existing| same_pair(existing)) {
        match (existing, event) {
            (Event::BodyContact { speed, .. }, Event::BodyContact { speed: new, .. })
            | (Event::WallContact { speed, .. }, Event::WallContact { speed: new, .. }) => {
                *speed = (*speed).max(new);
            }
            _ => {}
        }
    } else {
        events.push(event);
    }
}
