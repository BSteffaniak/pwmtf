//! Exact integer collision detection between rounded convex shapes.
//!
//! Every part and wall is placed in the world as a core (a point, a segment
//! or a quadrilateral) thickened by a radius: a disc is a rounded point, a
//! capsule a rounded segment, a box an unrounded quadrilateral and a wall an
//! unrounded segment. Separated cores are compared by their closest features;
//! overlapping cores by the separating axis with least penetration.

use crate::{Vector, integer_sqrt};

/// Fixed-point scale of unit normals; equal to [`crate::TRIG_UNIT`].
pub const UNIT: i64 = 1 << 30;

/// A shape placed in world coordinates.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// Core vertices; counter-clockwise for quadrilaterals.
    pub verts: [Vector; 4],
    /// 1 (point), 2 (segment) or 4 (quadrilateral).
    pub count: u8,
    /// Thickening around the core.
    pub radius: i64,
    /// The point the shape turns about, for surface velocities.
    pub pivot: Vector,
    /// Index of the part within its body.
    pub part: u8,
    /// Bumper kick speed.
    pub kick: i64,
    /// Half the narrowest width.
    pub thickness: i64,
}

impl Placed {
    /// A rounded point.
    pub const fn point(center: Vector, radius: i64) -> Self {
        Self {
            verts: [center; 4],
            count: 1,
            radius,
            pivot: center,
            part: 0,
            kick: 0,
            thickness: radius,
        }
    }

    /// An unrounded segment, such as a wall.
    pub const fn segment(start: Vector, end: Vector) -> Self {
        Self {
            verts: [start, end, end, end],
            count: 2,
            radius: 0,
            pivot: start,
            part: 0,
            kick: 0,
            thickness: 0,
        }
    }

    fn verts(&self) -> &[Vector] {
        &self.verts[..usize::from(self.count)]
    }

    const fn edge(&self, index: usize) -> (Vector, Vector) {
        match self.count {
            1 => (self.verts[0], self.verts[0]),
            2 => (self.verts[0], self.verts[1]),
            _ => (self.verts[index], self.verts[(index + 1) % 4]),
        }
    }

    const fn edge_count(&self) -> usize {
        match self.count {
            1 | 2 => 1,
            _ => 4,
        }
    }

    /// Moves the shape by `delta`.
    pub fn translate(&mut self, delta: Vector) {
        for vert in &mut self.verts {
            *vert = *vert + delta;
        }
        self.pivot = self.pivot + delta;
    }

    /// Unit axes normal to the core's edges (none for a point).
    fn axes(&self) -> ([Vector; 2], usize) {
        match self.count {
            1 => ([Vector::ZERO; 2], 0),
            2 => {
                let along = self.verts[1] - self.verts[0];
                if along == Vector::ZERO {
                    return ([Vector::ZERO; 2], 0);
                }
                (
                    [unit_normal(Vector::new(-along.y, along.x)), Vector::ZERO],
                    1,
                )
            }
            _ => {
                let first = self.verts[1] - self.verts[0];
                let second = self.verts[2] - self.verts[1];
                if first == Vector::ZERO || second == Vector::ZERO {
                    return ([Vector::ZERO; 2], 0);
                }
                (
                    [
                        unit_normal(Vector::new(-first.y, first.x)),
                        unit_normal(Vector::new(-second.y, second.x)),
                    ],
                    2,
                )
            }
        }
    }
}

/// Where and how deeply two shapes touch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Contact {
    /// Unit normal (length [`UNIT`]) from the first shape toward the second.
    pub normal: Vector,
    /// Penetration depth; negative within the query margin when apart.
    pub depth: i64,
    /// The contact point.
    pub point: Vector,
}

/// The contact between `a` and `b`, if they overlap or lie within `margin`
/// of each other.
pub fn collide(a: &Placed, b: &Placed, margin: i64) -> Option<Contact> {
    let reach = a.radius + b.radius;
    if cores_intersect(a, b) {
        return Some(separating_axis(a, b));
    }
    // Closest features: every vertex of one against every edge of the other.
    let mut best: Option<(i128, Vector, Vector)> = None;
    let mut candidates: [(i128, Vector, Vector); 16] = [(0, Vector::ZERO, Vector::ZERO); 16];
    let mut count = 0;
    for (from, to, flip) in [(a, b, false), (b, a, true)] {
        for &vert in from.verts() {
            for edge in 0..to.edge_count() {
                let (start, end) = to.edge(edge);
                let closest = closest_on_segment(start, end, vert);
                let distance = (vert - closest).length_squared();
                let pair = if flip {
                    (distance, closest, vert)
                } else {
                    (distance, vert, closest)
                };
                if best.is_none_or(|(least, _, _)| distance < least) {
                    best = Some(pair);
                }
                if count < candidates.len() {
                    candidates[count] = pair;
                    count += 1;
                }
            }
        }
    }
    let (least, on_a, on_b) = best?;
    let limit = i128::from(reach + margin);
    if least >= limit * limit {
        return None;
    }
    let distance = integer_sqrt(least);
    if distance == 0 {
        return Some(separating_axis(a, b));
    }
    let normal = unit_normal(on_b - on_a);
    // Average the features within a small tolerance of the closest, so flat
    // faces resting against each other touch at their middle, not a corner.
    let tolerance = i128::from(distance + (reach / 8).max(1));
    let tolerance = tolerance * tolerance;
    let (mut sum_a, mut sum_b, mut used) = ((0_i128, 0_i128), (0_i128, 0_i128), 0_i128);
    for &(squared, point_a, point_b) in &candidates[..count] {
        if squared <= tolerance {
            sum_a = (
                sum_a.0 + i128::from(point_a.x),
                sum_a.1 + i128::from(point_a.y),
            );
            sum_b = (
                sum_b.0 + i128::from(point_b.x),
                sum_b.1 + i128::from(point_b.y),
            );
            used += 1;
        }
    }
    let (on_a, on_b) = if used > 1 {
        (
            Vector::new(
                crate::div_round(sum_a.0, used),
                crate::div_round(sum_a.1, used),
            ),
            Vector::new(
                crate::div_round(sum_b.0, used),
                crate::div_round(sum_b.1, used),
            ),
        )
    } else {
        (on_a, on_b)
    };
    let unit = i128::from(UNIT);
    let surface_a = on_a + normal.scale(i128::from(a.radius), unit);
    let surface_b = on_b - normal.scale(i128::from(b.radius), unit);
    Some(Contact {
        normal,
        depth: reach - distance,
        point: midpoint(surface_a, surface_b),
    })
}

/// Whether the cores overlap or touch.
fn cores_intersect(a: &Placed, b: &Placed) -> bool {
    if a.count >= 2 && b.count >= 2 {
        for first in 0..a.edge_count() {
            let (start_a, end_a) = a.edge(first);
            for second in 0..b.edge_count() {
                let (start_b, end_b) = b.edge(second);
                if segments_intersect(start_a, end_a, start_b, end_b) {
                    return true;
                }
            }
        }
    }
    (a.count == 4 && b.verts().iter().any(|vert| inside_quad(a, *vert)))
        || (b.count == 4 && a.verts().iter().any(|vert| inside_quad(b, *vert)))
}

/// The axis of least penetration for overlapping cores, with the contact
/// point on the incident shape.
fn separating_axis(a: &Placed, b: &Placed) -> Contact {
    let unit = i128::from(UNIT);
    let (axes_a, count_a) = a.axes();
    let (axes_b, count_b) = b.axes();
    let mut best: Option<(i64, Vector, bool)> = None;
    for (axis, from_a) in axes_a[..count_a]
        .iter()
        .map(|axis| (*axis, true))
        .chain(axes_b[..count_b].iter().map(|axis| (*axis, false)))
    {
        let (min_a, max_a) = project(a, axis);
        let (min_b, max_b) = project(b, axis);
        let forward = max_a + a.radius - (min_b - b.radius);
        let backward = max_b + b.radius - (min_a - a.radius);
        let (depth, normal) = if forward <= backward {
            (forward, axis)
        } else {
            (backward, -axis)
        };
        if best.is_none_or(|(least, _, _)| depth < least) {
            best = Some((depth, normal, from_a));
        }
    }
    let Some((depth, normal, from_a)) = best else {
        // Two coincident points: separate along +x.
        return Contact {
            normal: Vector::new(UNIT, 0),
            depth: a.radius + b.radius,
            point: a.verts[0],
        };
    };
    // Halfway between the incident shape's deepest surface point and the
    // reference surface it sank below.
    let half = i128::from(depth);
    let point = if from_a {
        support(b, -normal) - normal.scale(i128::from(b.radius) * 2 - half, unit * 2)
    } else {
        support(a, normal) + normal.scale(i128::from(a.radius) * 2 - half, unit * 2)
    };
    Contact {
        normal,
        depth,
        point,
    }
}

/// The core's projection interval onto a unit axis, in micro-units.
fn project(shape: &Placed, axis: Vector) -> (i64, i64) {
    let unit = i128::from(UNIT);
    let mut range = (i64::MAX, i64::MIN);
    for vert in shape.verts() {
        let projection = crate::div_round(vert.dot(axis), unit);
        range = (range.0.min(projection), range.1.max(projection));
    }
    range
}

/// The average of the core's vertices furthest along `direction`.
fn support(shape: &Placed, direction: Vector) -> Vector {
    let unit = i128::from(UNIT);
    let projections: Vec<i64> = shape
        .verts()
        .iter()
        .map(|vert| crate::div_round(vert.dot(direction), unit))
        .collect();
    let most = projections.iter().copied().max().unwrap_or(0);
    let tolerance = (shape.thickness / 8).max(1);
    let (mut x, mut y, mut used) = (0_i128, 0_i128, 0_i128);
    for (vert, projection) in shape.verts().iter().zip(&projections) {
        if *projection >= most - tolerance {
            x += i128::from(vert.x);
            y += i128::from(vert.y);
            used += 1;
        }
    }
    Vector::new(
        crate::div_round(x, used.max(1)),
        crate::div_round(y, used.max(1)),
    )
}

fn midpoint(a: Vector, b: Vector) -> Vector {
    Vector::new(
        crate::div_round(i128::from(a.x) + i128::from(b.x), 2),
        crate::div_round(i128::from(a.y) + i128::from(b.y), 2),
    )
}

/// The sign of the turn from `a` to `b` to `c`.
fn orientation(a: Vector, b: Vector, c: Vector) -> i128 {
    let (ab, ac) = (b - a, c - a);
    (i128::from(ab.x) * i128::from(ac.y) - i128::from(ab.y) * i128::from(ac.x)).signum()
}

/// Whether `point` lies within the bounding box of `a` and `b`.
fn within_box(a: Vector, b: Vector, point: Vector) -> bool {
    point.x >= a.x.min(b.x)
        && point.x <= a.x.max(b.x)
        && point.y >= a.y.min(b.y)
        && point.y <= a.y.max(b.y)
}

/// Whether segments `p`–`q` and `r`–`s` share a point.
fn segments_intersect(p: Vector, q: Vector, r: Vector, s: Vector) -> bool {
    let (o1, o2) = (orientation(p, q, r), orientation(p, q, s));
    let (o3, o4) = (orientation(r, s, p), orientation(r, s, q));
    if o1 * o2 < 0 && o3 * o4 < 0 {
        return true;
    }
    (o1 == 0 && within_box(p, q, r))
        || (o2 == 0 && within_box(p, q, s))
        || (o3 == 0 && within_box(r, s, p))
        || (o4 == 0 && within_box(r, s, q))
}

/// Whether `point` lies inside or on a counter-clockwise quadrilateral.
fn inside_quad(quad: &Placed, point: Vector) -> bool {
    (0..4).all(|index| {
        let (start, end) = quad.edge(index);
        orientation(start, end, point) >= 0
    })
}

/// The point of segment `start`–`end` nearest `point`.
pub fn closest_on_segment(start: Vector, end: Vector, point: Vector) -> Vector {
    let along = end - start;
    let length_squared = along.length_squared();
    if length_squared == 0 {
        return start;
    }
    let projection = (point - start).dot(along).clamp(0, length_squared);
    start + along.scale(projection, length_squared)
}

/// `offset` scaled to length [`UNIT`]. The length is computed with extra
/// fractional bits so the normal's error stays far below one part in a
/// million even for short offsets. `offset` must not be zero.
pub fn unit_normal(offset: Vector) -> Vector {
    let squared = offset.length_squared();
    let mut shift = 0_u32;
    while shift < 30 && squared << (2 * (shift + 1)) < 1_i128 << 124 {
        shift += 1;
    }
    let precise = integer_sqrt(squared << (2 * shift)).max(1);
    offset.scale(i128::from(UNIT) << shift, i128::from(precise))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quad(center: Vector, half: i64) -> Placed {
        Placed {
            verts: [
                center + Vector::new(-half, -half),
                center + Vector::new(half, -half),
                center + Vector::new(half, half),
                center + Vector::new(-half, half),
            ],
            count: 4,
            radius: 0,
            pivot: center,
            part: 0,
            kick: 0,
            thickness: half,
        }
    }

    #[test]
    fn point_against_box_face_corner_and_inside() {
        let square = quad(Vector::ZERO, 100);
        // Face: a disc of radius 30 centered 120 right of center.
        let contact = collide(&square, &Placed::point(Vector::new(120, 10), 30), 0).unwrap();
        assert_eq!(contact.normal, Vector::new(UNIT, 0));
        assert_eq!(contact.depth, 10);
        // Corner: diagonal normal.
        let contact = collide(&square, &Placed::point(Vector::new(110, 110), 20), 0).unwrap();
        assert!((contact.normal.x - contact.normal.y).abs() <= 1);
        assert!(contact.normal.x > 0);
        // Apart.
        assert!(collide(&square, &Placed::point(Vector::new(140, 0), 30), 0).is_none());
        // Within the margin.
        let near = collide(&square, &Placed::point(Vector::new(140, 0), 30), 20).unwrap();
        assert_eq!(near.depth, -10);
        // Center inside, nearer the top face: pushed out upward.
        let contact = collide(&square, &Placed::point(Vector::new(10, 90), 5), 0).unwrap();
        assert_eq!(contact.normal, Vector::new(0, UNIT));
        assert_eq!(contact.depth, 15);
    }

    #[test]
    fn segments_and_boxes_resolve_along_least_penetration() {
        let floor = Placed::segment(Vector::new(-1_000, 0), Vector::new(1_000, 0));
        // A box sunk 20 into a floor from above is pushed up.
        let block = quad(Vector::new(0, 80), 100);
        let contact = collide(&floor, &block, 0).unwrap();
        assert_eq!(contact.normal, Vector::new(0, UNIT));
        assert_eq!(contact.depth, 20);
        assert_eq!(contact.point.y, -10);
        assert_eq!(contact.point.x, 0, "flat faces touch at their middle");
        // Crossing capsule cores.
        let mut capsule = Placed::segment(Vector::new(0, -50), Vector::new(0, 200));
        capsule.radius = 10;
        let contact = collide(&floor, &capsule, 0).unwrap();
        assert_eq!(contact.normal, Vector::new(0, UNIT));
        assert_eq!(contact.depth, 60);
        // Parallel capsule resting on the floor touches at its middle.
        let mut lying = Placed::segment(Vector::new(-100, 8), Vector::new(100, 8));
        lying.radius = 10;
        let contact = collide(&floor, &lying, 0).unwrap();
        assert_eq!(contact.depth, 2);
        assert_eq!(contact.point.x, 0);
        assert_eq!(contact.normal, Vector::new(0, UNIT));
    }

    #[test]
    fn box_against_box() {
        let a = quad(Vector::ZERO, 100);
        let b = quad(Vector::new(190, 30), 100);
        let contact = collide(&a, &b, 0).unwrap();
        assert_eq!(contact.normal, Vector::new(UNIT, 0));
        assert_eq!(contact.depth, 10);
        let contact = collide(&b, &a, 0).unwrap();
        assert_eq!(contact.normal, Vector::new(-UNIT, 0));
    }
}
