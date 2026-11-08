//! Bounded, incremental previews using the canonical solver, never authority.
use bevy::prelude::*;
use pwmtf_billiards::{
    PhysicsProfile, TableGeometry, VersionedShotCommand, VersionedTableState, advance_tick,
    start_shot,
};

#[derive(Resource, Default)]
pub struct ExactPreview {
    key: Option<(
        VersionedTableState,
        PhysicsProfile,
        TableGeometry,
        VersionedShotCommand,
    )>,
    moving: Option<VersionedTableState>,
    ticks: u32,
    lines: Vec<(u8, Vec2, Vec2)>,
    short_lines: Vec<(u8, Vec2, Vec2)>,
    contact: Option<Vec2>,
    object: Option<u8>,
    lengths: [f32; 16],
    stopped: [bool; 16],
    published: Vec<(u8, Vec2, Vec2)>,
    published_contact: Option<Vec2>,
    contact_tick: Option<u32>,
    mode: bool,
    unlimited: bool,
}

fn clip_segment(start: Vec2, end: Vec2, used: &mut f32, budget: f32) -> Option<Vec2> {
    let length = start.distance(end);
    let remaining = (budget - *used).max(0.0);
    if length <= f32::EPSILON || remaining <= 0.0 {
        return None;
    }
    let take = length.min(remaining);
    *used += take;
    Some(start.lerp(end, take / length))
}

// Net displacement over equal-time solver samples is the average velocity
// direction. Publish one straight segment, never the sampled curved polyline.
fn average_paths(samples: &[(u8, Vec2, Vec2)]) -> Vec<(u8, Vec2, Vec2)> {
    let mut paths = std::collections::BTreeMap::new();
    for &(id, start, end) in samples {
        paths
            .entry(id)
            .and_modify(|path: &mut (Vec2, Vec2)| path.1 = end)
            .or_insert((start, end));
    }
    paths
        .into_iter()
        .filter_map(|(id, (start, end))| {
            let direction = end - start;
            let length = direction.length();
            (length > 0.5).then(|| {
                (
                    id,
                    start,
                    start + direction.normalize() * if id == 0 { 80.0 } else { 100.0 },
                )
            })
        })
        .collect()
}

// Tick endpoints include collision response and overlap correction. Exclude the
// whole tick on secondary contact, rather than averaging its post-impact motion.
fn stop_colliding_paths(stopped: &mut [bool; 16], events: &[pwmtf_billiards::SimulationEvent]) {
    use pwmtf_billiards::SimulationEventKind;
    for event in events {
        match event.kind {
            SimulationEventKind::BallContact { first, second } => {
                stopped[usize::from(first.number())] = true;
                stopped[usize::from(second.number())] = true;
            }
            SimulationEventKind::CushionContact { ball, .. }
            | SimulationEventKind::BallPocketed { ball, .. } => {
                stopped[usize::from(ball.number())] = true;
            }
            _ => {}
        }
    }
}

impl ExactPreview {
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &mut self,
        gizmos: &mut Gizmos,
        table: VersionedTableState,
        physics: PhysicsProfile,
        geometry: TableGeometry,
        shot: VersionedShotCommand,
        compact: bool,
    ) {
        let key = (table, physics, geometry, shot);
        if self.key.as_ref() != Some(&key) || self.mode != compact {
            // Never retain a guide across a changed table or physics profile.
            if self
                .key
                .as_ref()
                .is_none_or(|old| old.0 != key.0 || old.1 != key.1 || old.2 != key.2)
                || self.mode != compact
            {
                self.published.clear();
                self.published_contact = None;
            }
            self.mode = compact;
            self.moving = start_shot(physics, key.0.clone(), shot).ok();
            self.key = Some(key);
            self.ticks = 0;
            self.lines.clear();
            self.short_lines.clear();
            self.contact = None;
            self.object = None;
            self.lengths = [0.0; 16];
            self.stopped = [false; 16];
            self.contact_tick = None;
        }
        // A short trajectory, generated in bounded work slices while aiming.
        // Only complete ticks are drawn; never substitute a geometric estimate.
        let unlimited = crate::aim_preview::unlimited_range();
        let range_changed = unlimited != self.unlimited;
        self.unlimited = unlimited;
        let computing = self.moving.is_some();
        self.compute(physics, geometry, compact);
        if (computing || range_changed) && self.moving.is_none() {
            self.publish(compact);
        }
        for &(number, a, b) in &self.published {
            let color = if number == 0 {
                Color::srgba(0.96, 0.96, 0.86, 0.75)
            } else {
                Color::srgb(0.95, 0.72, 0.25)
            };
            gizmos.line_2d(a, b, color);
        }
        if let Some(contact) = self.published_contact {
            gizmos.circle_2d(contact, crate::BALL_RADIUS, Color::WHITE);
        }
    }

    fn compute(&mut self, physics: PhysicsProfile, geometry: TableGeometry, compact: bool) {
        // Compact guides must finish for the current input in this frame: an
        // old heading is not a valid aiming aid. Full trajectories remain sliced.
        let started = bevy::platform::time::Instant::now();
        let tick_limit = if compact {
            u32::from(physics.ticks_per_second()) * 3
        } else {
            240
        };
        for _ in 0..tick_limit {
            if !compact && started.elapsed().as_secs_f64() >= 0.002 {
                break;
            }
            let Some(before) = self.moving.take() else {
                break;
            };
            let Ok(result) = advance_tick(geometry, physics, before.clone()) else {
                break;
            };
            self.ticks += 1;
            let had_contact = self.contact.is_some();
            if had_contact {
                stop_colliding_paths(&mut self.stopped, &result.events);
            }
            if self.contact.is_none() {
                for event in &result.events {
                    if let pwmtf_billiards::SimulationEventKind::BallContact { first, second } =
                        event.kind
                        && (first.number() == 0 || second.number() == 0)
                    {
                        self.object = Some(if first.number() == 0 {
                            second.number()
                        } else {
                            first.number()
                        });
                        self.contact = result
                            .state
                            .balls()
                            .iter()
                            .find(|ball| ball.id.number() == 0)
                            .map(|ball| crate::canonical_to_world(ball.position));
                        break;
                    }
                    if matches!(event.kind, pwmtf_billiards::SimulationEventKind::CushionContact { ball, .. } if ball.number() == 0)
                    {
                        self.contact = result
                            .state
                            .balls()
                            .iter()
                            .find(|ball| ball.id.number() == 0)
                            .map(|ball| crate::canonical_to_world(ball.position));
                        break;
                    }
                }
            }
            if self.contact.is_some() && self.contact_tick.is_none() {
                self.contact_tick = Some(self.ticks);
                self.stop_same_tick_contacts(&result.events);
            }
            for (a, b) in before.balls().iter().zip(result.state.balls()) {
                if !a.pocketed && !b.pocketed && a.position != b.position {
                    if had_contact
                        && !self.stopped[usize::from(a.id.number())]
                        && (a.id.number() == 0 || Some(a.id.number()) == self.object)
                    {
                        let number = a.id.number();
                        let start = crate::canonical_to_world(a.position);
                        let end = crate::canonical_to_world(b.position);
                        let budget = if number == 0 { 80.0 } else { 100.0 };
                        if let Some(end) =
                            clip_segment(start, end, &mut self.lengths[usize::from(number)], budget)
                        {
                            self.short_lines.push((number, start, end));
                        }
                    }
                    self.lines.push((
                        a.id.number(),
                        crate::canonical_to_world(a.position),
                        crate::canonical_to_world(b.position),
                    ));
                }
            }
            let sampled_contact = compact
                && self.contact_tick.is_some_and(|tick| {
                    self.ticks - tick >= u32::from(physics.ticks_per_second()) / 3
                });
            if !result.settled
                && !sampled_contact
                && self.ticks < u32::from(physics.ticks_per_second()) * 3
            {
                self.moving = Some(result.state);
            }
        }
    }

    fn stop_same_tick_contacts(&mut self, events: &[pwmtf_billiards::SimulationEvent]) {
        // Events within a tick are canonicalized, so when another
        // contact shares the initiating tick there is no safe free
        // flight to average for its participants.
        let mut skipped_initial = false;
        for event in events {
            if !skipped_initial
                && matches!(event.kind,
                        pwmtf_billiards::SimulationEventKind::BallContact { first, second }
                        if first.number() == 0 || second.number() == 0)
            {
                skipped_initial = true;
                continue;
            }
            stop_colliding_paths(&mut self.stopped, std::slice::from_ref(event));
        }
    }

    fn publish(&mut self, compact: bool) {
        if !compact {
            self.published.clone_from(&self.lines);
            self.published_contact = None;
            return;
        }
        let in_range = self.unlimited
            || self
                .key
                .as_ref()
                .and_then(|(table, _, _, _)| {
                    table.balls().iter().find(|ball| ball.id.number() == 0)
                })
                .zip(self.contact)
                .is_some_and(|(cue, contact)| {
                    crate::canonical_to_world(cue.position).distance(contact)
                        <= crate::aim_preview::GUIDE_LENGTH
                });
        let mut lines = if in_range {
            average_paths(&self.short_lines)
        } else {
            Vec::new()
        };
        if let Some((table, _, _, _)) = &self.key
            && let Some(cue) = table.balls().iter().find(|ball| ball.id.number() == 0)
        {
            let origin = crate::canonical_to_world(cue.position);
            if let Some(end) = self.contact.or_else(|| {
                self.lines
                    .iter()
                    .rev()
                    .find(|line| line.0 == 0)
                    .map(|line| line.2)
            }) {
                let direction = (end - origin).normalize_or_zero();
                let length = origin.distance(end).min(crate::aim_preview::GUIDE_LENGTH);
                if length > crate::BALL_RADIUS {
                    lines.push((
                        0,
                        origin + direction * crate::BALL_RADIUS,
                        origin + direction * length,
                    ));
                }
            }
        }
        self.published = lines;
        self.published_contact = self.contact.filter(|_| self.object.is_some() && in_range);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_range_gates_outgoing_lines_and_marker_but_not_incoming() {
        let geometry = TableGeometry::standard();
        let table =
            pwmtf_billiards::standard_rack(geometry, pwmtf_billiards::RackSeed::new(42)).unwrap();
        let origin = crate::canonical_to_world(table.balls()[0].position);
        let mut preview = ExactPreview {
            key: Some((
                table,
                PhysicsProfile::standard(),
                geometry,
                crate::practice_shot(&crate::PrototypeInput::default()),
            )),
            object: Some(1),
            ..Default::default()
        };
        for (distance, expected) in [(499.0, true), (500.0, true), (501.0, false)] {
            let contact = origin + Vec2::X * distance;
            preview.contact = Some(contact);
            preview.short_lines = vec![(1, contact, contact + Vec2::X * 10.0)];
            preview.publish(true);
            assert_eq!(preview.published_contact.is_some(), expected);
            assert_eq!(preview.published.iter().any(|line| line.0 == 1), expected);
            assert!(preview.published.iter().any(|line| line.0 == 0));
        }
        preview.unlimited = true;
        preview.publish(true);
        assert!(preview.published_contact.is_some());
        assert!(preview.published.iter().any(|line| line.0 == 1));
    }

    #[test]
    fn secondary_contacts_stop_only_involved_paths_and_stay_stopped() {
        use pwmtf_billiards::{BallId, CushionAxis, SimulationEvent, SimulationEventKind};
        let mut stopped = [false; 16];
        stop_colliding_paths(
            &mut stopped,
            &[SimulationEvent {
                tick: 2,
                sequence: 0,
                kind: SimulationEventKind::CushionContact {
                    ball: BallId::CUE,
                    axis: CushionAxis::Horizontal,
                },
            }],
        );
        assert!(stopped[0]);
        assert!(!stopped[1]);
        stop_colliding_paths(
            &mut stopped,
            &[SimulationEvent {
                tick: 3,
                sequence: 0,
                kind: SimulationEventKind::BallContact {
                    first: BallId::new(1).unwrap(),
                    second: BallId::new(2).unwrap(),
                },
            }],
        );
        stop_colliding_paths(&mut stopped, &[]);
        assert!(stopped[0] && stopped[1] && stopped[2]);
        assert!(!stopped[3]);
    }

    #[test]
    fn curved_samples_publish_one_straight_average_per_ball() {
        let samples = [
            (0, Vec2::ZERO, Vec2::new(10.0, 0.0)),
            (0, Vec2::new(10.0, 0.0), Vec2::new(10.0, 10.0)),
            (1, Vec2::X, Vec2::new(20.0, 0.0)),
        ];
        let lines = average_paths(&samples);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].1, Vec2::ZERO);
        assert!((lines[0].2.normalize() - Vec2::new(10.0, 10.0).normalize()).length() < 0.001);
        assert!((lines[0].1.distance(lines[0].2) - 80.0).abs() < 0.001);
    }

    #[test]
    fn compact_finishes_current_input_in_one_compute_call() {
        let physics = PhysicsProfile::standard();
        let geometry = TableGeometry::standard();
        let table =
            pwmtf_billiards::standard_rack(geometry, pwmtf_billiards::RackSeed::new(42)).unwrap();
        let command = crate::practice_shot(&crate::PrototypeInput::default());
        let mut preview = ExactPreview {
            moving: start_shot(physics, table, command).ok(),
            ..Default::default()
        };
        preview.compute(physics, geometry, true);
        assert!(
            preview.moving.is_none(),
            "compact guide must not trail across frames"
        );
    }

    #[test]
    fn calculation_does_not_modify_published_lines() {
        let physics = PhysicsProfile::standard();
        let geometry = TableGeometry::standard();
        let table =
            pwmtf_billiards::standard_rack(geometry, pwmtf_billiards::RackSeed::new(42)).unwrap();
        let command = crate::practice_shot(&crate::PrototypeInput::default());
        let published = vec![(0, Vec2::ZERO, Vec2::X)];
        let mut preview = ExactPreview {
            moving: start_shot(physics, table, command).ok(),
            published: published.clone(),
            ..Default::default()
        };
        preview.compute(physics, geometry, true);
        assert_eq!(preview.published, published);
    }

    #[test]
    fn short_paths_clip_at_the_display_budget() {
        let mut used = 0.0;
        assert_eq!(
            clip_segment(Vec2::ZERO, Vec2::new(60.0, 0.0), &mut used, 80.0),
            Some(Vec2::new(60.0, 0.0))
        );
        assert_eq!(
            clip_segment(Vec2::new(60.0, 0.0), Vec2::new(120.0, 0.0), &mut used, 80.0),
            Some(Vec2::new(80.0, 0.0))
        );
        assert!(clip_segment(Vec2::ZERO, Vec2::X, &mut used, 80.0).is_none());
    }
}
