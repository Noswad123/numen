//! Integer logical layer margins. Submitted client state is not compositor XY feedback.
use super::software::clamp_origin;
use crate::{
    corner::{Area, Corner},
    motion::{Facing, MotionClock, NativeMotion, Navigator, Point, PursuitStep},
};
use std::time::Instant;

pub(super) struct CornerTravel {
    output: (i32, i32),
    sprite: (u32, u32),
    target: Point,
    destination: (i32, i32),
    navigator: Navigator,
    clock: MotionClock,
    residual: NativeMotion,
    held: bool,
}

/// Revoke only travel-owned intent; manual drag requests have no travel owner.
pub(super) fn discard_pending(
    travel: &mut Option<CornerTravel>,
    origin: &mut (i32, i32),
    committed: (i32, i32),
    now: Instant,
) -> bool {
    let Some(travel) = travel else {
        return false;
    };
    travel.hold(now);
    let changed = *origin != committed;
    *origin = committed;
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::motion::FRAME_INTERVAL;
    use std::time::Duration;

    #[test]
    fn cancellation_rolls_back_travel_before_geometry_clamp_but_keeps_manual_intent() {
        let now = Instant::now();
        let mut travel = Some(
            CornerTravel::new(Corner::BottomRight, (800, 600), (100, 80), 360.0, now).unwrap(),
        );
        let committed = (600, 400);
        let mut pending = (620, 420);
        assert!(discard_pending(&mut travel, &mut pending, committed, now));
        travel = None;
        assert_eq!(pending, committed);
        assert_eq!(
            clamp_origin(pending, Some((710, 490)), (100, 80)),
            (600, 400)
        );
        assert_eq!(
            clamp_origin(pending, Some((650, 450)), (100, 80)),
            (550, 370)
        );
        pending = (620, 420);
        assert!(!discard_pending(&mut travel, &mut pending, committed, now));
        assert_eq!(pending, (620, 420));
    }

    #[test]
    fn all_four_frozen_targets_low_speed_and_density_independent_integer_steps() {
        for density_hint in [1, 2, 3] {
            for (corner, expected) in [
                (Corner::TopLeft, (12, 12)),
                (Corner::TopRight, (688, 12)),
                (Corner::BottomLeft, (12, 508)),
                (Corner::BottomRight, (688, 508)),
            ] {
                let now = Instant::now();
                let mut travel =
                    CornerTravel::new(corner, (800, 600), (100, 80), 40.0, now).unwrap();
                assert_eq!(
                    travel.destination, expected,
                    "density {density_hint} must not change logical target"
                );
                let frozen = travel.target;
                let mut origin = (350, 260);
                let mut completed = false;
                for frame in 0..2400 {
                    match travel.step(
                        now + FRAME_INTERVAL * frame,
                        Some((800, 600)),
                        (100, 80),
                        origin,
                        true,
                    ) {
                        Step::Move(next, _) => {
                            assert!((next.0 - origin.0).abs() <= 2);
                            assert!((next.1 - origin.1).abs() <= 2);
                            origin = next;
                        }
                        Step::Completed => {
                            completed = true;
                            break;
                        }
                        Step::Hold => {}
                        Step::Cancelled => panic!("low speed stalled"),
                    }
                    assert_eq!(travel.target, frozen);
                }
                assert!(completed);
                assert_eq!(origin, expected);
            }
        }
    }

    #[test]
    fn early_events_do_not_integrate_and_holds_rebase_without_debt() {
        let now = Instant::now();
        let mut travel =
            CornerTravel::new(Corner::BottomRight, (800, 600), (100, 80), 360.0, now).unwrap();
        let output = Some((800, 600));
        let origin = (100, 100);
        travel.step(now, output, (100, 80), origin, true);
        for event in 1..100 {
            assert_eq!(
                travel.step(
                    now + FRAME_INTERVAL * event / 101,
                    output,
                    (100, 80),
                    origin,
                    true
                ),
                Step::Hold
            );
        }
        let pending = travel.step(now + FRAME_INTERVAL, output, (100, 80), origin, true);
        assert!(matches!(pending, Step::Move(..)));
        // Explicit suspension discards elapsed debt; transient waits do not.
        for reason in 0..5 {
            let held = now + Duration::from_secs(10 + reason * 10);
            travel.hold(held);
            assert_eq!(
                travel.step(held, output, (100, 80), origin, false),
                Step::Hold
            );
            assert_eq!(
                travel.step(held, output, (100, 80), origin, true),
                Step::Move(origin, Facing::Right)
            );
        }
        let late = now + Duration::from_secs(1000);
        travel.hold(late);
        travel.step(late, output, (100, 80), origin, true);
        if let Step::Move(next, _) = travel.step(
            late + Duration::from_millis(100),
            output,
            (100, 80),
            origin,
            true,
        ) {
            assert!((next.0 - origin.0).abs() <= 18);
            assert!((next.1 - origin.1).abs() <= 18);
        } else {
            panic!("late tick should move");
        }
    }

    #[test]
    fn three_hundred_frame_opportunities_with_intervening_waits_still_arrive() {
        let now = Instant::now();
        let output = Some((800, 600));
        let sprite = (100, 80);
        let mut travel =
            CornerTravel::new(Corner::BottomRight, output.unwrap(), sprite, 360.0, now).unwrap();
        let mut committed = (12, 12);
        let mut completed = false;
        for frame in 0..300 {
            let at = now + FRAME_INTERVAL * frame;
            assert_eq!(
                travel.step(at, output, sprite, committed, false),
                Step::Hold
            );
            match travel.step(at, output, sprite, committed, true) {
                Step::Move(origin, _) => committed = origin,
                Step::Completed => {
                    completed = true;
                    break;
                }
                Step::Hold => {}
                Step::Cancelled => panic!("transient waits must not starve travel"),
            }
        }
        assert!(completed);
        assert_eq!(committed, (688, 508));
    }

    #[test]
    fn transient_wait_does_not_clear_stall_progress_or_catch_up() {
        let now = Instant::now();
        let output = Some((800, 600));
        let sprite = (100, 80);
        let origin = (12, 12);
        let mut travel =
            CornerTravel::new(Corner::BottomRight, output.unwrap(), sprite, 360.0, now).unwrap();
        travel.step(now, output, sprite, origin, true);
        assert_eq!(
            travel.step(now + Duration::from_secs(3), output, sprite, origin, false),
            Step::Hold
        );
        assert_eq!(
            travel.step(now + Duration::from_secs(3), output, sprite, origin, true),
            Step::Cancelled
        );

        let mut travel =
            CornerTravel::new(Corner::BottomRight, output.unwrap(), sprite, 360.0, now).unwrap();
        travel.step(now, output, sprite, origin, true);
        assert_eq!(
            travel.step(now + Duration::from_secs(1), output, sprite, origin, false),
            Step::Hold
        );
        let Step::Move(next, _) =
            travel.step(now + Duration::from_secs(1), output, sprite, origin, true)
        else {
            panic!("ready turn must move");
        };
        assert!((next.0 - origin.0).abs() <= 18 && (next.1 - origin.1).abs() <= 18);
    }

    #[test]
    fn explicit_hold_discards_fractional_intent_and_resumes_with_zero_debt() {
        let now = Instant::now();
        let output = Some((800, 600));
        let sprite = (100, 80);
        let origin = (12, 12);
        let mut travel =
            CornerTravel::new(Corner::BottomRight, output.unwrap(), sprite, 40.0, now).unwrap();
        travel.step(now, output, sprite, origin, true);
        travel.step(now + FRAME_INTERVAL, output, sprite, origin, true);
        let resumed = now + Duration::from_secs(10);
        travel.hold(resumed);
        let mut fresh =
            CornerTravel::new(Corner::BottomRight, output.unwrap(), sprite, 40.0, resumed).unwrap();
        for frame in 0..4 {
            let at = resumed + FRAME_INTERVAL * frame;
            assert_eq!(
                travel.step(at, output, sprite, origin, true),
                fresh.step(at, output, sprite, origin, true)
            );
        }
    }

    #[test]
    fn oversized_bounds_are_reachable_and_geometry_changes_cancel_even_held() {
        let now = Instant::now();
        for corner in [
            Corner::TopLeft,
            Corner::TopRight,
            Corner::BottomLeft,
            Corner::BottomRight,
        ] {
            let mut travel = CornerTravel::new(corner, (80, 60), (100, 80), 40.0, now).unwrap();
            assert_eq!(travel.destination, (0, 0));
            assert_eq!(
                travel.step(now, Some((80, 60)), (100, 80), (0, 0), true),
                Step::Completed
            );
            assert_eq!(
                travel.step(now, Some((81, 60)), (100, 80), (0, 0), false),
                Step::Cancelled
            );
            assert_eq!(
                travel.step(now, Some((80, 60)), (101, 80), (0, 0), false),
                Step::Cancelled
            );
            assert_eq!(
                travel.step(now, None, (100, 80), (0, 0), false),
                Step::Cancelled
            );
        }
    }
}

#[derive(Debug, PartialEq)]
pub(super) enum Step {
    Hold,
    Move((i32, i32), Facing),
    Completed,
    Cancelled,
}

impl CornerTravel {
    pub fn new(
        corner: Corner,
        output: (i32, i32),
        sprite: (u32, u32),
        speed: f64,
        now: Instant,
    ) -> Option<Self> {
        let target = corner.target(
            Area {
                x: 0.0,
                y: 0.0,
                width: output.0.into(),
                height: output.1.into(),
            },
            sprite.0.into(),
            sprite.1.into(),
        )?;
        // Clamp the origin *before* deriving the foot: oversized centered fits
        // otherwise produce negative, unreachable layer margins.
        let destination = clamp_origin(
            (
                (target.x - f64::from(sprite.0) / 2.0).round() as i32,
                (target.y - f64::from(sprite.1)).round() as i32,
            ),
            Some(output),
            sprite,
        );
        Some(Self {
            output,
            sprite,
            target: Self::foot(destination, sprite),
            destination,
            navigator: Navigator::new(speed, now),
            clock: MotionClock::default(),
            residual: NativeMotion::default(),
            held: false,
        })
    }

    fn foot(origin: (i32, i32), sprite: (u32, u32)) -> Point {
        Point {
            x: f64::from(origin.0) + f64::from(sprite.0) / 2.0,
            y: f64::from(origin.1) + f64::from(sprite.1),
        }
    }

    pub fn hold(&mut self, now: Instant) {
        self.held = true;
        self.navigator.resume(now);
        self.clock.reset(now);
        self.residual = NativeMotion::default();
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.clock.deadline()
    }

    pub fn step(
        &mut self,
        now: Instant,
        output: Option<(i32, i32)>,
        sprite: (u32, u32),
        committed: (i32, i32),
        eligible: bool,
    ) -> Step {
        if output != Some(self.output) || sprite != self.sprite {
            return Step::Cancelled;
        }
        if !eligible {
            return Step::Hold;
        }
        if self.held {
            self.navigator.resume(now);
            self.clock.reset(now);
            self.held = false;
        }
        let Some(_) = self.clock.take(now) else {
            return Step::Hold;
        };
        match self
            .navigator
            .step_fixed(now, Self::foot(committed, sprite), self.target, 1.0)
        {
            PursuitStep::Arrived if committed == self.destination => Step::Completed,
            PursuitStep::Arrived => Step::Move(
                self.destination,
                if self.destination.0 < committed.0 {
                    Facing::Left
                } else {
                    Facing::Right
                },
            ),
            PursuitStep::Blocked(_) => Step::Cancelled,
            PursuitStep::Move { delta, facing } => {
                let point = self.residual.request(
                    Point {
                        x: committed.0.into(),
                        y: committed.1.into(),
                    },
                    delta,
                    1.0,
                );
                Step::Move(
                    clamp_origin((point.x as i32, point.y as i32), output, sprite),
                    facing,
                )
            }
        }
    }
}
