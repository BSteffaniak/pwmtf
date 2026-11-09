//! Integer trigonometry on microradian angles.
//!
//! Sine and cosine are evaluated from Taylor series in 2^60 fixed point after
//! an exact octant reduction, so every platform computes identical bits.

/// Fixed-point scale of [`sin_cos`] results and of rotation matrices: one is
/// `1 << 30`.
pub const TRIG_UNIT: i64 = 1 << 30;

/// Microradians in a full turn, rounded to the nearest integer. Dynamic body
/// angles wrap by this amount, an error of a third of a microradian per turn.
pub const TURN_MICRORADIANS: i64 = 6_283_185;

/// Microradians in half a turn (π), rounded to the nearest integer.
pub const PI_MICRORADIANS: i64 = 3_141_593;

/// π in 2^60 fixed point.
const PI_FIX: i128 = 3_622_009_729_038_561_421;
const FRACTION_BITS: u32 = 60;
const ONE: i128 = 1 << FRACTION_BITS;

/// The sine and cosine of `angle` microradians, scaled by [`TRIG_UNIT`] and
/// rounded to nearest; exact at every multiple of a quarter turn of the
/// fixed-point π.
#[must_use]
pub fn sin_cos(angle: i64) -> (i64, i64) {
    let half_pi = PI_FIX / 2;
    let quarter_pi = PI_FIX / 4;
    let radians = (i128::from(angle) << FRACTION_BITS) / 1_000_000;
    let reduced = radians.rem_euclid(2 * PI_FIX);
    let quadrant = reduced / half_pi;
    let within = reduced - quadrant * half_pi;
    let (sin, cos) = if within > quarter_pi {
        let (s, c) = series(half_pi - within);
        (c, s)
    } else {
        series(within)
    };
    let (sin, cos) = match quadrant {
        0 => (sin, cos),
        1 => (cos, -sin),
        2 => (-sin, -cos),
        _ => (-cos, sin),
    };
    (to_unit(sin), to_unit(cos))
}

/// Wraps an angle into `[-TURN/2, TURN/2]` microradians.
#[must_use]
pub const fn wrap_angle(angle: i64) -> i64 {
    let half = TURN_MICRORADIANS / 2;
    (angle + half).rem_euclid(TURN_MICRORADIANS) - half
}

/// Sine and cosine of `x` in `[0, π/4]`, 2^60 fixed point.
const fn series(x: i128) -> (i128, i128) {
    let square = (x * x) >> FRACTION_BITS;
    let mut sin = ONE;
    let mut cos = ONE;
    // sin x = x (1 - x²/(2·3) (1 - x²/(4·5) (...)))
    // cos x = 1 - x²/(1·2) (1 - x²/(3·4) (...))
    let mut term = 14;
    while term >= 2 {
        let sin_div = term * (term + 1);
        let cos_div = (term - 1) * term;
        sin = ONE - ((square * sin) >> FRACTION_BITS) / sin_div;
        cos = ONE - ((square * cos) >> FRACTION_BITS) / cos_div;
        term -= 2;
    }
    ((x * sin) >> FRACTION_BITS, cos)
}

#[allow(clippy::cast_possible_truncation)]
const fn to_unit(value: i128) -> i64 {
    ((value + (1 << 29)) >> 30) as i64
}

/// Rotates `vector` by the rotation whose sine and cosine (scaled by
/// [`TRIG_UNIT`]) are given, rounding to nearest.
#[must_use]
pub fn rotate(vector: crate::Vector, (sin, cos): (i64, i64)) -> crate::Vector {
    let unit = i128::from(TRIG_UNIT);
    let (x, y) = (i128::from(vector.x), i128::from(vector.y));
    let (s, c) = (i128::from(sin), i128::from(cos));
    crate::Vector::new(
        crate::div_round(x * c - y * s, unit),
        crate::div_round(x * s + y * c, unit),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_angles_are_exact_and_identities_hold() {
        assert_eq!(sin_cos(0), (0, TRIG_UNIT));
        let (s, c) = sin_cos(PI_MICRORADIANS / 2);
        assert!((s - TRIG_UNIT).abs() <= 1 && c.abs() < 400, "{s} {c}");
        // 523_599 microradians is within a quarter microradian of π/6.
        let (s, c) = sin_cos(523_599);
        assert!((s - TRIG_UNIT / 2).abs() < 300, "{s}");
        assert!((c - 929_887_697).abs() < 300, "{c}");
        let (s, c) = sin_cos(-785_398);
        assert!((s + c).abs() < 300, "{s} {c}");
        for angle in (-20_000_000..20_000_000).step_by(77_777) {
            let (s, c) = sin_cos(angle);
            let norm = i128::from(s) * i128::from(s) + i128::from(c) * i128::from(c);
            let one = i128::from(TRIG_UNIT) * i128::from(TRIG_UNIT);
            assert!((norm - one).abs() < one / 1_000_000_000 * 4, "{angle}");
            assert_eq!(sin_cos(-angle), (-s, c));
        }
    }

    #[test]
    fn wrapping_keeps_angles_within_half_a_turn() {
        assert_eq!(wrap_angle(0), 0);
        assert_eq!(wrap_angle(TURN_MICRORADIANS + 5), 5);
        assert_eq!(wrap_angle(-TURN_MICRORADIANS - 5), -5);
        assert_eq!(
            wrap_angle(PI_MICRORADIANS),
            PI_MICRORADIANS - TURN_MICRORADIANS
        );
    }
}
