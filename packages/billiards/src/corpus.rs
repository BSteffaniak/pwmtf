//! Representative canonical shot corpus.

use crate::{
    Aim, BallId, BallState, PhysicsProfile, ShotPower, Spin, TableGeometry, Vector,
    VersionedShotCommand, VersionedTableState,
};

/// Named representative shot fixture used to qualify canonical simulation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShotFixture {
    /// Stable fixture name.
    pub name: &'static str,
    /// Canonical starting state.
    pub state: VersionedTableState,
    /// Canonical shot command.
    pub command: VersionedShotCommand,
}

/// Returns the deterministic launch qualification corpus.
///
/// # Panics
///
/// Panics only if package-owned fixture constants violate canonical validation;
/// this indicates a programming error and is covered by the corpus test.
#[must_use]
pub fn qualification_corpus() -> Vec<ShotFixture> {
    let geometry = TableGeometry::standard();
    vec![
        fixture(
            "straight_contact",
            vec![ball(0, -400_000, 0), ball(1, 0, 0)],
            0,
            2_000,
            geometry,
        ),
        fixture(
            "glancing_contact",
            vec![ball(0, -500_000, -25_000), ball(2, 0, 0)],
            50,
            3_000,
            geometry,
        ),
        fixture(
            "side_cushion",
            vec![ball(0, 900_000, 200_000)],
            0,
            3_000,
            geometry,
        ),
        fixture(
            "corner_cushions",
            vec![ball(0, 900_000, 300_000)],
            800,
            4_000,
            geometry,
        ),
        fixture(
            "side_pocket",
            vec![ball(0, 0, 500_000)],
            2_500,
            2_000,
            geometry,
        ),
        fixture(
            "object_ball_pocket",
            vec![ball(0, -300_000, 500_000), ball(3, 0, 500_000)],
            0,
            3_000,
            geometry,
        ),
        fixture(
            "low_speed_settle",
            vec![ball(0, -200_000, 0)],
            0,
            50,
            geometry,
        ),
        fixture(
            "three_ball_chain",
            vec![
                ball(0, -500_000, 0),
                ball(4, -100_000, 0),
                ball(5, 200_000, 0),
            ],
            0,
            3_000,
            geometry,
        ),
        fixture(
            "simultaneous_split",
            vec![
                ball(0, -500_000, 0),
                ball(6, 0, -28_575),
                ball(7, 0, 28_575),
            ],
            0,
            4_000,
            geometry,
        ),
        fixture(
            "corner_jaw_approach",
            vec![ball(0, 1_100_000, 450_000)],
            700,
            3_000,
            geometry,
        ),
        fixture(
            "scratch_side_pocket",
            vec![ball(0, 0, 450_000)],
            2_500,
            2_000,
            geometry,
        ),
        ShotFixture {
            name: "maximum_power_break",
            state: crate::standard_rack(geometry, crate::RackSeed::new(42))
                .expect("package-owned rack fixture must be valid"),
            command: VersionedShotCommand::new(
                Aim::new(0).expect("package-owned fixture aim must be valid"),
                ShotPower::new(ShotPower::MAX).expect("package-owned fixture power must be valid"),
                Spin::CENTER,
            ),
        },
    ]
}

/// Returns the standard physics profile used by the launch corpus.
#[must_use]
pub const fn qualification_profile() -> PhysicsProfile {
    PhysicsProfile::standard()
}

fn fixture(
    name: &'static str,
    balls: Vec<BallState>,
    aim: u16,
    power: u16,
    geometry: TableGeometry,
) -> ShotFixture {
    ShotFixture {
        name,
        state: VersionedTableState::new(crate::TABLE_STATE_VERSION, geometry, 0, balls)
            .expect("package-owned shot fixture must be valid"),
        command: VersionedShotCommand::new(
            Aim::new(aim).expect("package-owned fixture aim must be valid"),
            ShotPower::new(power).expect("package-owned fixture power must be valid"),
            Spin::CENTER,
        ),
    }
}

fn ball(number: u8, x: i64, y: i64) -> BallState {
    BallState::stationary(
        BallId::new(number).expect("package-owned fixture ball number must be valid"),
        Vector::from_micros(x, y),
    )
}

#[cfg(test)]
mod tests {
    use crate::{SimulationEventKind, simulate_shot};

    use super::*;

    #[test]
    fn simultaneous_contacts_have_stable_identifier_order() {
        let fixture = qualification_corpus()
            .into_iter()
            .find(|fixture| fixture.name == "simultaneous_split")
            .unwrap();
        let result = simulate_shot(
            TableGeometry::standard(),
            qualification_profile(),
            fixture.state,
            fixture.command,
        )
        .unwrap();
        let contacts = result
            .events
            .iter()
            .filter_map(|event| match event.kind {
                SimulationEventKind::BallContact { first, second } => Some((first, second)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(contacts.len() >= 2);
        assert_eq!(contacts[0], (BallId::CUE, BallId::new(6).unwrap()));
        assert_eq!(contacts[1], (BallId::CUE, BallId::new(7).unwrap()));
    }

    #[test]
    fn break_and_scratch_corpus_paths_emit_expected_events() {
        for (name, expected) in [
            ("maximum_power_break", "contact"),
            ("scratch_side_pocket", "scratch"),
            ("corner_jaw_approach", "jaw"),
        ] {
            let fixture = qualification_corpus()
                .into_iter()
                .find(|fixture| fixture.name == name)
                .unwrap();
            let result = simulate_shot(
                TableGeometry::standard(),
                qualification_profile(),
                fixture.state,
                fixture.command,
            )
            .unwrap();
            match expected {
                "contact" => assert!(result.events.iter().any(|event| matches!(
                    event.kind,
                    SimulationEventKind::BallContact {
                        first: BallId::CUE,
                        ..
                    }
                ))),
                "scratch" => assert!(result.events.iter().any(|event| matches!(
                    event.kind,
                    SimulationEventKind::BallPocketed {
                        ball: BallId::CUE,
                        ..
                    }
                ))),
                "jaw" => assert!(result.events.iter().any(|event| matches!(
                    event.kind,
                    SimulationEventKind::CushionContact {
                        ball: BallId::CUE,
                        ..
                    } | SimulationEventKind::BallPocketed {
                        ball: BallId::CUE,
                        ..
                    }
                ))),
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn corpus_native_parallel_execution_matches_serial_results() {
        let geometry = TableGeometry::standard();
        let profile = qualification_profile();
        let fixtures = qualification_corpus();
        let serial = fixtures
            .iter()
            .map(|fixture| {
                simulate_shot(geometry, profile, fixture.state.clone(), fixture.command).unwrap()
            })
            .collect::<Vec<_>>();
        let parallel = fixtures
            .into_iter()
            .map(|fixture| {
                std::thread::spawn(move || {
                    simulate_shot(geometry, profile, fixture.state, fixture.command).unwrap()
                })
            })
            .map(|worker| worker.join().expect("qualification worker completes"))
            .collect::<Vec<_>>();
        assert_eq!(parallel, serial);
    }

    #[test]
    fn every_corpus_shot_is_repeatable_and_settles() {
        let geometry = TableGeometry::standard();
        let profile = qualification_profile();
        for fixture in qualification_corpus() {
            let first = simulate_shot(geometry, profile, fixture.state.clone(), fixture.command)
                .unwrap_or_else(|error| panic!("{} failed: {error}", fixture.name));
            let second = simulate_shot(geometry, profile, fixture.state, fixture.command)
                .unwrap_or_else(|error| panic!("{} replay failed: {error}", fixture.name));
            assert_eq!(first, second, "{} replay diverged", fixture.name);
            assert!(matches!(
                first.events.last().map(|event| event.kind),
                Some(SimulationEventKind::Settled)
            ));
        }
    }
}
