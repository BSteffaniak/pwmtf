//! Deterministic canonical 8-ball rack generation.

use crate::{BallId, BallState, TableGeometry, TableStateError, Vector, VersionedTableState};
use thiserror::Error;

/// Immutable seed used to select a canonical rack ordering.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RackSeed(u64);

impl RackSeed {
    /// Creates a rack seed from an authoritative random value.
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// Returns the exact seed value.
    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Failure to construct a canonical rack.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum RackError {
    /// The selected table geometry cannot contain the standard rack and cue ball.
    #[error("table geometry is too small for a standard 8-ball rack")]
    GeometryTooSmall,
    /// Generated state failed canonical validation.
    #[error(transparent)]
    State(#[from] TableStateError),
    /// Internal rack generation produced an invalid ball number.
    #[error("rack generation produced invalid ball number {0}")]
    InvalidGeneratedBall(u8),
}

/// Generates a deterministic standard 8-ball opening state.
///
/// The 8-ball occupies the center, and the rear corners contain one solid and
/// one stripe. Remaining object-ball positions are deterministically shuffled
/// from the authoritative rack seed.
///
/// # Errors
///
/// Returns [`RackError::GeometryTooSmall`] when the geometry cannot contain the
/// cue ball and five-row rack, or [`RackError::State`] if generated state does
/// not satisfy canonical state invariants.
pub fn standard_rack(
    geometry: TableGeometry,
    seed: RackSeed,
) -> Result<VersionedTableState, RackError> {
    let radius = geometry.ball_radius().micros();
    let diameter = radius * 2;
    let row_spacing = mul_div_ceil(radius, 1_732_051, 1_000_000);
    let apex_x = geometry.half_width().micros() / 4;
    let cue_x = -geometry.half_width().micros() / 2;
    let rear_x = apex_x + row_spacing * 4;
    let maximum_y = diameter * 2;
    let playable_x = geometry.half_width().micros() - radius;
    let playable_y = geometry.half_height().micros() - radius;
    if cue_x.abs() > playable_x || rear_x > playable_x || maximum_y > playable_y {
        return Err(RackError::GeometryTooSmall);
    }

    let mut solids = [1_u8, 2, 3, 4, 5, 6, 7];
    let mut stripes = [9_u8, 10, 11, 12, 13, 14, 15];
    let mut random = SplitMix64::new(seed.value());
    shuffle(&mut solids, &mut random);
    shuffle(&mut stripes, &mut random);

    let swap_corners = random.next() & 1 == 1;
    let rear_low = if swap_corners { stripes[0] } else { solids[0] };
    let rear_high = if swap_corners { solids[0] } else { stripes[0] };
    let mut remaining = [0_u8; 12];
    remaining[..6].copy_from_slice(&solids[1..]);
    remaining[6..].copy_from_slice(&stripes[1..]);
    shuffle(&mut remaining, &mut random);

    let mut balls = Vec::with_capacity(16);
    balls.push(BallState::stationary(
        BallId::CUE,
        Vector::from_micros(cue_x, 0),
    ));
    let mut remaining_index = 0;
    for row in 0_i64..5 {
        for column in 0_i64..=row {
            let number = match (row, column) {
                (2, 1) => 8,
                (4, 0) => rear_low,
                (4, 4) => rear_high,
                _ => {
                    let value = remaining[remaining_index];
                    remaining_index += 1;
                    value
                }
            };
            let id = BallId::new(number).map_err(|_| RackError::InvalidGeneratedBall(number))?;
            balls.push(BallState::stationary(
                id,
                Vector::from_micros(apex_x + row * row_spacing, (column * 2 - row) * radius),
            ));
        }
    }
    VersionedTableState::new(crate::TABLE_STATE_VERSION, geometry, 0, balls)
        .map_err(RackError::State)
}

fn shuffle<const SIZE: usize>(values: &mut [u8; SIZE], random: &mut SplitMix64) {
    for index in (1..SIZE).rev() {
        let bound = u64::try_from(index + 1).unwrap_or(u64::MAX);
        let selected = usize::try_from(random.next() % bound).unwrap_or(0);
        values.swap(index, selected);
    }
}

struct SplitMix64(u64);

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self(seed)
    }

    const fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut value = self.0;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        value ^ (value >> 31)
    }
}

fn mul_div_ceil(value: i64, multiplier: i64, divisor: i64) -> i64 {
    let numerator = i128::from(value) * i128::from(multiplier);
    i64::try_from((numerator + i128::from(divisor) - 1) / i128::from(divisor)).unwrap_or(i64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rack_is_repeatable_and_contains_every_ball() {
        let geometry = TableGeometry::standard();
        let first = standard_rack(geometry, RackSeed::new(42)).expect("rack is valid");
        let second = standard_rack(geometry, RackSeed::new(42)).expect("rack is valid");
        assert_eq!(first, second);
        assert_eq!(first.balls().len(), 16);
        for number in 0..=15 {
            assert!(first.ball(BallId::new(number).unwrap()).is_some());
        }
    }

    #[test]
    fn rack_constraints_hold_for_many_seeds() {
        let geometry = TableGeometry::standard();
        for seed in 0..256 {
            let rack = standard_rack(geometry, RackSeed::new(seed)).expect("rack is valid");
            let eight = rack.ball(BallId::new(8).unwrap()).unwrap();
            let apex_x = geometry.half_width().micros() / 4;
            let row_spacing = mul_div_ceil(geometry.ball_radius().micros(), 1_732_051, 1_000_000);
            assert_eq!(eight.position.x.micros(), apex_x + row_spacing * 2);
            assert_eq!(eight.position.y.micros(), 0);

            let rear_x = apex_x + row_spacing * 4;
            let mut rear = rack
                .balls()
                .iter()
                .filter(|ball| ball.position.x.micros() == rear_x)
                .collect::<Vec<_>>();
            rear.sort_unstable_by_key(|ball| ball.position.y.micros());
            assert!(matches!(rear[0].id.number(), 1..=7 | 9..=15));
            assert!(matches!(rear[4].id.number(), 1..=7 | 9..=15));
            assert_ne!(rear[0].id.number() <= 7, rear[4].id.number() <= 7);
        }
    }
}
