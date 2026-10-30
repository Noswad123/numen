//! Pure pursuit in logical desktop points. The caller owns window movement,
//! running art, and the one-shot success effect, and drops pursuit on arrival.

use std::time::{Duration, Instant};

pub const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
const ALPHA_THRESHOLD: u8 = 32;
const MAX_SAMPLES: usize = 4096;
const MAX_ELAPSED: Duration = Duration::from_millis(50);
const STALL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// `origin` is the sprite's top-left, not the bubble or outer window origin.
/// `pixel_scale` is logical desktop points per mask pixel, uniformly in X/Y.
#[derive(Clone, Copy, Debug)]
pub struct SpriteGeometry {
    pub origin: Point,
    pub pixel_scale: f64,
}

#[derive(Clone, Copy, Debug)]
#[cfg(test)]
pub struct SpriteMask<'a> {
    pub rgba: &'a [u8],
    pub width: usize,
    pub height: usize,
}

/// Cached physical sprite alpha and opaque anchor, rebuilt only for a new pose/grid.
pub struct RasterMask {
    alpha: Vec<u8>,
    width: usize,
    height: usize,
    anchor: Option<Point>,
}

impl RasterMask {
    pub fn new(width: usize, height: usize, alpha: Vec<u8>) -> Self {
        let mut mask = Self {
            alpha,
            width,
            height,
            anchor: None,
        };
        if width.checked_mul(height) == Some(mask.alpha.len()) && width > 0 && height > 0 {
            mask.anchor = opaque_anchor(Mask::Raster(&mask));
        }
        mask
    }

    pub fn contact(&self, cursor: Point, geometry: SpriteGeometry) -> bool {
        self.anchor.is_some()
            && cursor.x.is_finite()
            && cursor.y.is_finite()
            && geometry.origin.x.is_finite()
            && geometry.origin.y.is_finite()
            && geometry.pixel_scale.is_finite()
            && geometry.pixel_scale > 0.0
            && hit(
                cursor,
                geometry.origin,
                geometry.pixel_scale,
                Mask::Raster(self),
            )
    }
}

#[derive(Clone, Copy)]
enum Mask<'a> {
    #[cfg(test)]
    Source(SpriteMask<'a>),
    Raster(&'a RasterMask),
}

impl Mask<'_> {
    fn dimensions(self) -> (usize, usize) {
        match self {
            #[cfg(test)]
            Self::Source(m) => (m.width, m.height),
            Self::Raster(m) => (m.width, m.height),
        }
    }

    fn valid(self) -> bool {
        let (w, h) = self.dimensions();
        w > 0
            && h > 0
            && match self {
                #[cfg(test)]
                Self::Source(m) => {
                    w.checked_mul(h).and_then(|n| n.checked_mul(4)) == Some(m.rgba.len())
                }
                Self::Raster(m) => w.checked_mul(h) == Some(m.alpha.len()),
            }
    }

    fn alpha(self, x: usize, y: usize) -> u8 {
        match self {
            #[cfg(test)]
            Self::Source(m) => m.rgba[(y * m.width + x) * 4 + 3],
            Self::Raster(m) => m.alpha[y * m.width + x],
        }
    }
}

fn opaque_anchor(mask: Mask<'_>) -> Option<Point> {
    let (width, height) = mask.dimensions();
    let mut anchor = None;
    let mut nearest = f64::INFINITY;
    for y in 0..height {
        for x in 0..width {
            if mask.alpha(x, y) >= ALPHA_THRESHOLD {
                let point = Point {
                    x: x as f64 + 0.5,
                    y: y as f64 + 0.5,
                };
                let distance = (point.x - width as f64 / 2.0).hypot(point.y - height as f64 / 2.0);
                if distance < nearest {
                    nearest = distance;
                    anchor = Some(point);
                }
            }
        }
    }
    anchor
}

/// A deadline gate, not merely a requested wakeup. Late ticks never catch up.
#[derive(Default)]
pub struct MotionClock {
    next: Option<Instant>,
    last: Option<Instant>,
}

impl MotionClock {
    pub fn take(&mut self, now: Instant) -> Option<Duration> {
        if self.next.is_some_and(|next| now < next) {
            return None;
        }
        let elapsed = self.last.map_or(Duration::ZERO, |last| {
            now.saturating_duration_since(last).min(MAX_ELAPSED)
        });
        self.last = Some(now);
        self.next = Some(now + FRAME_INTERVAL);
        Some(elapsed)
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.next
    }

    pub fn reset(&mut self, now: Instant) {
        self.next = Some(now);
        self.last = Some(now);
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Integrate fractional intent; never turn every small event into a native step.
#[derive(Default)]
pub struct NativeMotion {
    residual: Point,
    backing: f64,
}

impl NativeMotion {
    /// AppKit snaps window origins to whole logical points even on Retina.
    /// Keep this separate from backing scale used for raster/desktop conversion.
    pub fn position_scale(backing: f64) -> f64 {
        if cfg!(target_os = "macos") {
            1.0
        } else {
            backing
        }
    }

    pub fn request(&mut self, origin: Point, delta: Point, backing: f64) -> Point {
        if backing != self.backing {
            self.residual = Point::default();
            self.backing = backing;
        }
        let x = self.residual.x + delta.x * backing;
        let y = self.residual.y + delta.y * backing;
        let dx = x.trunc();
        let dy = y.trunc();
        self.residual = Point {
            x: x - dx,
            y: y - dy,
        };
        Point {
            x: origin.x + dx / backing,
            y: origin.y + dy / backing,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Facing {
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PursuitStep {
    Move { delta: Point, facing: Facing },
    Arrived,
    Blocked(&'static str),
}

pub struct Navigator {
    speed: f64,
    last_tick: Instant,
    progress: Option<(Point, Instant)>,
    facing: Facing,
    approach: Option<(Point, f64, Instant)>,
}

impl Navigator {
    /// Fixed foot-center pursuit. Arrival is based only on actual native feedback;
    /// one native position step per axis tolerates fractional quantization.
    pub fn step_fixed(
        &mut self,
        now: Instant,
        current: Point,
        target: Point,
        backing: f64,
    ) -> PursuitStep {
        let elapsed = now
            .saturating_duration_since(self.last_tick)
            .min(MAX_ELAPSED);
        self.last_tick = now;
        if ![current.x, current.y, target.x, target.y, backing]
            .iter()
            .all(|v| v.is_finite())
            || backing <= 0.0
        {
            return PursuitStep::Blocked("invalid fixed target geometry");
        }
        let aim = Point {
            x: target.x - current.x,
            y: target.y - current.y,
        };
        let distance = aim.x.hypot(aim.y);
        if !distance.is_finite() {
            return PursuitStep::Blocked("fixed target vector is not representable");
        }
        if aim.x.abs() <= 1.0 / backing && aim.y.abs() <= 1.0 / backing {
            return PursuitStep::Arrived;
        }
        match self.approach {
            Some((previous, best, since)) if previous == target && distance >= best - 1e-6 => {
                if now.saturating_duration_since(since) >= STALL_TIMEOUT {
                    return PursuitStep::Blocked("fixed target has not converged for two seconds");
                }
            }
            _ => self.approach = Some((target, distance, now)),
        }
        if aim.x < 0.0 {
            self.facing = Facing::Left;
        } else if aim.x > 0.0 {
            self.facing = Facing::Right;
        }
        let travel = (self.speed * elapsed.as_secs_f64()).min(distance);
        PursuitStep::Move {
            delta: Point {
                x: aim.x / distance * travel,
                y: aim.y / distance * travel,
            },
            facing: self.facing,
        }
    }

    /// Nonfinite speeds use the minimum; finite speeds clamp to 40..=2000.
    pub fn new(speed: f64, now: Instant) -> Self {
        Self {
            speed: if speed.is_finite() {
                speed.clamp(40.0, 2000.0)
            } else {
                40.0
            },
            last_tick: now,
            progress: None,
            facing: Facing::Right,
            approach: None,
        }
    }

    /// Call after an explicit pause or restoration of zero-sized native geometry.
    pub fn resume(&mut self, now: Instant) {
        self.last_tick = now;
        self.progress = None;
        self.approach = None;
    }

    #[cfg(test)]
    pub fn step(
        &mut self,
        now: Instant,
        cursor: Point,
        geometry: SpriteGeometry,
        mask: SpriteMask<'_>,
    ) -> PursuitStep {
        self.step_mask(now, cursor, geometry, Mask::Source(mask))
    }

    pub fn step_raster(
        &mut self,
        now: Instant,
        cursor: Point,
        geometry: SpriteGeometry,
        mask: &RasterMask,
    ) -> PursuitStep {
        self.step_mask(now, cursor, geometry, Mask::Raster(mask))
    }

    fn step_mask(
        &mut self,
        now: Instant,
        cursor: Point,
        geometry: SpriteGeometry,
        mask: Mask<'_>,
    ) -> PursuitStep {
        let elapsed = now
            .saturating_duration_since(self.last_tick)
            .min(MAX_ELAPSED);
        self.last_tick = now;
        let origin = geometry.origin;
        let scale = geometry.pixel_scale;
        let (width, height) = mask.dimensions();
        if !mask.valid() {
            return PursuitStep::Blocked("malformed sprite mask");
        }
        if !cursor.x.is_finite()
            || !cursor.y.is_finite()
            || !origin.x.is_finite()
            || !origin.y.is_finite()
            || !scale.is_finite()
            || scale <= 0.0
            || !(origin.x + width as f64 * scale).is_finite()
            || !(origin.y + height as f64 * scale).is_finite()
        {
            return PursuitStep::Blocked("invalid logical sprite geometry or pointer");
        }

        // Select a pixel center, not the bounding-box center (which may be a hole).
        let anchor = match mask {
            #[cfg(test)]
            Mask::Source(_) => opaque_anchor(mask),
            Mask::Raster(m) => m.anchor,
        };
        let Some(anchor) = anchor else {
            return PursuitStep::Blocked("sprite mask is fully transparent");
        };
        if hit(cursor, origin, scale, mask) {
            return PursuitStep::Arrived;
        }

        // Only actual origin changes count as progress, never target changes or
        // requested deltas: the window manager may refuse/clamp every request.
        match self.progress {
            Some((previous, since)) if previous == origin => {
                if now.saturating_duration_since(since) >= STALL_TIMEOUT {
                    return PursuitStep::Blocked("sprite has not moved for two seconds");
                }
            }
            _ => self.progress = Some((origin, now)),
        }

        let aim = Point {
            x: (cursor.x - origin.x) - anchor.x * scale,
            y: (cursor.y - origin.y) - anchor.y * scale,
        };
        let distance = aim.x.hypot(aim.y);
        if !distance.is_finite() || distance == 0.0 {
            return PursuitStep::Blocked("pursuit vector is not representable");
        }
        // Native rounding can alternate endpoints forever while each endpoint
        // appears to be positional progress. Require improvement toward a stable
        // live target as well, and always confirm actual contact before blocking.
        match self.approach {
            Some((target, best, since)) if target == cursor && distance >= best - 1e-6 => {
                if now.saturating_duration_since(since) >= STALL_TIMEOUT {
                    return PursuitStep::Blocked(
                        "native pursuit is not converging on pointer contact",
                    );
                }
            }
            _ => self.approach = Some((cursor, distance, now)),
        }
        if aim.x < 0.0 {
            self.facing = Facing::Left;
        } else if aim.x > 0.0 {
            self.facing = Facing::Right;
        }

        // Shorten the path rather than undersample tiny sprites. Computing the
        // limit before dividing also avoids infinite sample counts at tiny scales.
        let travel = (self.speed * elapsed.as_secs_f64())
            .min(distance)
            .min(scale * (MAX_SAMPLES as f64 / 2.0));
        let delta = Point {
            x: aim.x / distance * travel,
            y: aim.y / distance * travel,
        };
        let samples = ((travel / scale * 2.0).ceil() as usize).clamp(1, MAX_SAMPLES);
        for sample in 1..=samples {
            let fraction = sample as f64 / samples as f64;
            let candidate = Point {
                x: delta.x * fraction,
                y: delta.y * fraction,
            };
            let moved_origin = Point {
                x: origin.x + candidate.x,
                y: origin.y + candidate.y,
            };
            if !moved_origin.x.is_finite() || !moved_origin.y.is_finite() {
                return PursuitStep::Blocked("requested sprite position is not representable");
            }
            if hit(cursor, moved_origin, scale, mask) {
                // Even a predicted hit is only a request. Confirm next frame
                // against the actual position supplied by the GUI.
                return PursuitStep::Move {
                    delta: candidate,
                    facing: self.facing,
                };
            }
        }
        PursuitStep::Move {
            delta,
            facing: self.facing,
        }
    }
}

fn hit(cursor: Point, origin: Point, scale: f64, mask: Mask<'_>) -> bool {
    let x = (cursor.x - origin.x) / scale;
    let y = (cursor.y - origin.y) / scale;
    let (width, height) = mask.dimensions();
    if x < 0.0 || y < 0.0 || x >= width as f64 || y >= height as f64 {
        return false;
    }
    mask.alpha(x as usize, y as usize) >= ALPHA_THRESHOLD
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(target_os = "macos")]
    fn native_logical_steps_preserve_fractional_intent_across_density_changes() {
        for direction in [-1.0, 1.0] {
            let mut native = NativeMotion::default();
            let mut actual = Point {
                x: -800.0,
                y: 900.0,
            };
            for frame in 0..120 {
                let backing = if frame < 60 { 2.0 } else { 1.0 };
                let requested = native.request(
                    actual,
                    Point {
                        x: direction * 0.5,
                        y: direction * 0.5,
                    },
                    NativeMotion::position_scale(backing),
                );
                // Model native whole-point snapping, not ideal setter feedback.
                actual = Point {
                    x: requested.x.floor(),
                    y: requested.y.ceil(),
                };
            }
            assert_eq!(
                actual,
                Point {
                    x: -800.0 + direction * 60.0,
                    y: 900.0 + direction * 60.0
                }
            );
        }
    }

    #[test]
    fn fixed_targets_use_native_feedback_at_fractional_densities() {
        for backing in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let start = Instant::now();
            let mut nav = Navigator::new(2000.0, start);
            let mut native = NativeMotion::default();
            let mut current = Point {
                x: -500.0,
                y: -600.0,
            };
            let target = Point {
                x: -201.37,
                y: -105.23,
            };
            let mut arrived = false;
            for frame in 0..180 {
                match nav.step_fixed(start + FRAME_INTERVAL * frame, current, target, backing) {
                    PursuitStep::Move { delta, .. } => {
                        assert!(delta.x.hypot(delta.y) <= 100.0);
                        current = native.request(current, delta, backing);
                    }
                    PursuitStep::Arrived => {
                        assert!((current.x - target.x).abs() <= 1.0 / backing);
                        assert!((current.y - target.y).abs() <= 1.0 / backing);
                        arrived = true;
                        break;
                    }
                    other => panic!("unexpected {other:?}"),
                }
            }
            assert!(arrived);
        }
    }

    #[test]
    fn fixed_stall_oscillation_resume_and_invalid_geometry() {
        let start = Instant::now();
        let mut nav = Navigator::new(2000.0, start);
        let current = Point::default();
        let target = Point { x: 10.25, y: 0.0 };
        assert!(matches!(
            nav.step_fixed(start, current, target, 2.0),
            PursuitStep::Move { .. }
        ));
        assert!(matches!(
            nav.step_fixed(start + STALL_TIMEOUT, current, target, 2.0),
            PursuitStep::Blocked(_)
        ));
        nav.resume(start + Duration::from_secs(100));
        assert_eq!(
            movement(nav.step_fixed(start + Duration::from_secs(100), current, target, 2.0)).0,
            Point::default()
        );
        for frame in 1..=130 {
            let step = nav.step_fixed(
                start + Duration::from_secs(100) + FRAME_INTERVAL * frame,
                Point {
                    x: if frame % 2 == 0 { -1.0 } else { 0.0 },
                    y: 0.0,
                },
                target,
                2.0,
            );
            if frame == 130 {
                assert!(matches!(step, PursuitStep::Blocked(_)));
            }
        }
        for backing in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                nav.step_fixed(start, current, target, backing),
                PursuitStep::Blocked(_)
            ));
        }
        assert!(matches!(
            nav.step_fixed(
                start,
                current,
                Point {
                    x: f64::NAN,
                    y: 0.0
                },
                1.0
            ),
            PursuitStep::Blocked(_)
        ));
    }

    #[test]
    fn hard_clocks_bound_pointer_and_physics_despite_one_hundred_early_events() {
        fn simulate(busy: bool) -> (usize, usize, Point) {
            let start = Instant::now();
            let mut shared = MotionClock::default();
            let mut local = MotionClock::default();
            let mut nav = Navigator::new(40.0, start);
            let mut native = NativeMotion::default();
            let mut origin = Point::default();
            let mut polls = 0;
            let mut steps = 0;
            let mut event = |now| {
                if shared.take(now).is_some() {
                    polls += 1;
                    if local.take(now).is_some() {
                        steps += 1;
                        let (delta, _) = movement(nav.step(
                            now,
                            Point { x: 10000.5, y: 0.5 },
                            geometry(origin.x, origin.y, 1.0),
                            solid(),
                        ));
                        origin = native.request(origin, delta, 1.0);
                    }
                }
            };
            for frame in 0..=120 {
                let now = start + FRAME_INTERVAL * frame;
                event(now);
                if busy && frame < 120 {
                    for early in 1..=100 {
                        event(now + FRAME_INTERVAL * early / 101);
                    }
                }
            }
            (polls, steps, origin)
        }
        let normal = simulate(false);
        assert_eq!(simulate(true), normal);
        assert_eq!((normal.0, normal.1), (121, 121));
        assert!(normal.2.x <= 40.0 * (FRAME_INTERVAL * 120).as_secs_f64());
        assert!(normal.2.x >= 79.0);
    }

    #[test]
    fn native_tiny_contact_feedback_confirms_arrival_or_blocks_in_bounded_time() {
        for backing in [1.0, 1.25, 1.5, 2.0, 3.0] {
            let start = Instant::now();
            let mut nav = Navigator::new(40.0, start);
            let mut native = NativeMotion::default();
            let mut origin = Point::default();
            let cursor = Point { x: 0.5, y: 0.125 };
            let mut ended = false;
            for frame in 1..=180 {
                let g = geometry(origin.x, origin.y, 0.25);
                match nav.step(start + FRAME_INTERVAL * frame, cursor, g, solid()) {
                    PursuitStep::Move { delta, .. } => {
                        origin = native.request(origin, delta, backing);
                        assert!((origin.x * backing - (origin.x * backing).round()).abs() < 1e-10);
                    }
                    PursuitStep::Arrived => {
                        assert!(hit(cursor, origin, 0.25, Mask::Source(solid())));
                        ended = true;
                        break;
                    }
                    PursuitStep::Blocked(_) => {
                        ended = true;
                        break;
                    }
                }
            }
            assert!(
                ended,
                "unreachable native contact must not oscillate forever at {backing}x"
            );
        }
    }

    #[test]
    fn alternating_native_endpoints_are_not_convergent_progress_and_resume_clears_it() {
        let start = Instant::now();
        let cursor = Point { x: 10.5, y: 0.5 };
        let mut nav = Navigator::new(40.0, start);
        for frame in 0..120 {
            assert!(matches!(
                nav.step(
                    start + FRAME_INTERVAL * frame,
                    cursor,
                    geometry((frame % 2) as f64, 0.0, 1.0),
                    solid()
                ),
                PursuitStep::Move { .. }
            ));
        }
        assert!(matches!(
            nav.step(
                start + Duration::from_secs(3),
                cursor,
                geometry(1.0, 0.0, 1.0),
                solid()
            ),
            PursuitStep::Blocked(_)
        ));
        let resumed = start + Duration::from_secs(100);
        nav.resume(resumed);
        assert!(matches!(
            nav.step(resumed, cursor, geometry(1.0, 0.0, 1.0), solid()),
            PursuitStep::Move {
                delta: Point { x: 0.0, y: 0.0 },
                ..
            }
        ));
    }

    #[test]
    fn motion_clock_never_catches_up_and_clears_or_resumes_without_elapsed_debt() {
        let start = Instant::now();
        let mut clock = MotionClock::default();
        assert_eq!(clock.take(start), Some(Duration::ZERO));
        assert_eq!(clock.take(start), None);
        let late = start + Duration::from_secs(100);
        assert_eq!(clock.take(late), Some(MAX_ELAPSED));
        assert_eq!(clock.take(late), None);
        assert_eq!(clock.deadline(), Some(late + FRAME_INTERVAL));
        clock.clear();
        assert!(clock.deadline().is_none());
        clock.reset(late);
        assert_eq!(clock.take(late), Some(Duration::ZERO));
    }

    const SOLID: [u8; 4] = [0, 0, 0, 255];

    fn geometry(x: f64, y: f64, pixel_scale: f64) -> SpriteGeometry {
        SpriteGeometry {
            origin: Point { x, y },
            pixel_scale,
        }
    }

    fn solid() -> SpriteMask<'static> {
        SpriteMask {
            rgba: &SOLID,
            width: 1,
            height: 1,
        }
    }

    fn movement(step: PursuitStep) -> (Point, Facing) {
        match step {
            PursuitStep::Move { delta, facing } => (delta, facing),
            other => panic!("expected Move, got {other:?}"),
        }
    }

    #[test]
    fn zero_elapsed_then_live_cursor_and_vertical_heading() {
        let now = Instant::now();
        let mut nav = Navigator::new(100.0, now);
        let g = geometry(0.0, 0.0, 1.0);
        assert_eq!(
            movement(nav.step(now, Point { x: 100.5, y: 0.5 }, g, solid())).0,
            Point::default()
        );
        let (delta, facing) =
            movement(nav.step(now + MAX_ELAPSED, Point { x: -100.5, y: 0.5 }, g, solid()));
        assert_eq!(delta, Point { x: -5.0, y: 0.0 });
        assert_eq!(facing, Facing::Left);
        let (delta, facing) = movement(nav.step(
            now + MAX_ELAPSED * 2,
            Point { x: 0.5, y: 100.5 },
            g,
            solid(),
        ));
        assert_eq!(delta, Point { x: 0.0, y: 5.0 });
        assert_eq!(facing, Facing::Left);
        let (delta, facing) = movement(nav.step(
            now + MAX_ELAPSED * 3,
            Point { x: 100.5, y: 0.5 },
            g,
            solid(),
        ));
        assert_eq!(delta.x, 5.0);
        assert_eq!(facing, Facing::Right);
    }

    #[test]
    fn diagonal_negative_desktop_coordinates() {
        let now = Instant::now();
        let mut nav = Navigator::new(100.0, now);
        let (delta, facing) = movement(nav.step(
            now + MAX_ELAPSED,
            Point { x: -69.5, y: -59.5 },
            geometry(-100.0, -100.0, 1.0),
            solid(),
        ));
        assert!((delta.x - 3.0).abs() < 1e-12);
        assert!((delta.y - 4.0).abs() < 1e-12);
        assert_eq!(facing, Facing::Right);
    }

    #[test]
    fn already_under_pointer_and_alpha_threshold() {
        let now = Instant::now();
        let rgba = [0, 0, 0, 31, 0, 0, 0, 32];
        let mask = SpriteMask {
            rgba: &rgba,
            width: 2,
            height: 1,
        };
        let mut nav = Navigator::new(100.0, now);
        let g = geometry(-20.0, -10.0, 2.0);
        assert_eq!(
            nav.step(now, Point { x: -17.0, y: -9.0 }, g, mask),
            PursuitStep::Arrived
        );
        assert!(matches!(
            nav.step(now, Point { x: -19.0, y: -9.0 }, g, mask),
            PursuitStep::Move { .. }
        ));
    }

    #[test]
    fn transparent_center_aims_at_opaque_pixel() {
        let now = Instant::now();
        let rgba = [0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 255];
        let mask = SpriteMask {
            rgba: &rgba,
            width: 3,
            height: 1,
        };
        let mut nav = Navigator::new(100.0, now);
        let cursor = Point { x: 1.5, y: 0.5 };
        let (delta, _) =
            movement(nav.step(now + MAX_ELAPSED, cursor, geometry(0.0, 0.0, 1.0), mask));
        assert!(delta.x > 0.0 && delta.x <= 1.0);
        assert_eq!(
            nav.step(
                now + MAX_ELAPSED * 2,
                cursor,
                geometry(delta.x, delta.y, 1.0),
                mask
            ),
            PursuitStep::Arrived
        );
    }

    #[test]
    fn sweep_stops_at_first_hit_not_anchor_or_later_island() {
        let now = Instant::now();
        let mut rgba = [0; 9 * 4];
        rgba[4 * 4 + 3] = 255;
        rgba[8 * 4 + 3] = 255;
        let mask = SpriteMask {
            rgba: &rgba,
            width: 9,
            height: 1,
        };
        let mut nav = Navigator::new(2000.0, now);
        let cursor = Point { x: 20.5, y: 0.5 };
        let (delta, _) =
            movement(nav.step(now + MAX_ELAPSED, cursor, geometry(0.0, 0.0, 1.0), mask));
        assert_eq!(delta.x, 12.0);
        assert_eq!(
            nav.step(
                now + MAX_ELAPSED * 2,
                cursor,
                geometry(delta.x, 0.0, 1.0),
                mask
            ),
            PursuitStep::Arrived
        );
    }

    #[test]
    fn ignored_window_moves_never_complete_and_eventually_stall() {
        let now = Instant::now();
        let mut nav = Navigator::new(2000.0, now);
        let g = geometry(0.0, 0.0, 1.0);
        for millis in [0, 50, 500, 1999] {
            assert!(matches!(
                nav.step(
                    now + Duration::from_millis(millis),
                    Point { x: 2.5, y: 0.5 },
                    g,
                    solid()
                ),
                PursuitStep::Move { .. }
            ));
        }
        assert!(matches!(
            nav.step(now + STALL_TIMEOUT, Point { x: 900.0, y: 0.5 }, g, solid()),
            PursuitStep::Blocked(_)
        ));
    }

    #[test]
    fn actual_progress_and_resume_reset_stall_clock() {
        let now = Instant::now();
        let mut nav = Navigator::new(100.0, now);
        let cursor = Point { x: 100.0, y: 100.0 };
        for second in 0..5 {
            assert!(matches!(
                nav.step(
                    now + Duration::from_secs(second),
                    cursor,
                    geometry(second as f64, 0.0, 1.0),
                    solid()
                ),
                PursuitStep::Move { .. }
            ));
        }
        let resumed = now + Duration::from_secs(100);
        nav.resume(resumed);
        assert_eq!(
            movement(nav.step(resumed, cursor, geometry(4.0, 0.0, 1.0), solid())).0,
            Point::default()
        );
        assert!(matches!(
            nav.step(
                resumed + Duration::from_millis(1999),
                cursor,
                geometry(4.0, 0.0, 1.0),
                solid()
            ),
            PursuitStep::Move { .. }
        ));
    }

    #[test]
    fn lag_speed_and_tiny_scales_are_bounded() {
        let now = Instant::now();
        for scale in [1.0, 1e-100, 1e-300, f64::MIN_POSITIVE, f64::from_bits(1)] {
            let mut nav = Navigator::new(1e100, now);
            let (delta, _) = movement(nav.step(
                now + Duration::from_secs(100),
                Point { x: 1000.0, y: 0.0 },
                geometry(0.0, 0.0, scale),
                solid(),
            ));
            assert!(delta.x.is_finite() && delta.y.is_finite());
            assert!(delta.x.hypot(delta.y) <= 100.0);
            assert!(delta.x.hypot(delta.y) <= scale * 2048.0);
        }
        for speed in [f64::NAN, f64::INFINITY, -1.0, 0.0] {
            let mut nav = Navigator::new(speed, now);
            let (delta, _) = movement(nav.step(
                now + MAX_ELAPSED,
                Point { x: 1000.5, y: 0.5 },
                geometry(0.0, 0.0, 1.0),
                solid(),
            ));
            assert_eq!(delta.x, 2.0);
        }
    }

    #[test]
    fn rejects_bad_masks_and_nonfinite_geometry_without_panicking() {
        let now = Instant::now();
        let mut nav = Navigator::new(100.0, now);
        let cursor = Point { x: 10.0, y: 10.0 };
        for mask in [
            SpriteMask {
                rgba: &[],
                width: 0,
                height: 0,
            },
            SpriteMask {
                rgba: &SOLID,
                width: 2,
                height: 1,
            },
            SpriteMask {
                rgba: &SOLID,
                width: usize::MAX,
                height: 2,
            },
            SpriteMask {
                rgba: &[0; 4],
                width: 1,
                height: 1,
            },
        ] {
            assert!(matches!(
                nav.step(now, cursor, geometry(0.0, 0.0, 1.0), mask),
                PursuitStep::Blocked(_)
            ));
        }
        for invalid in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            for (point, g) in [
                (Point { x: invalid, y: 0.0 }, geometry(0.0, 0.0, 1.0)),
                (Point { x: 0.0, y: invalid }, geometry(0.0, 0.0, 1.0)),
                (cursor, geometry(invalid, 0.0, 1.0)),
                (cursor, geometry(0.0, invalid, 1.0)),
                (cursor, geometry(0.0, 0.0, invalid)),
            ] {
                assert!(matches!(
                    nav.step(now, point, g, solid()),
                    PursuitStep::Blocked(_)
                ));
            }
        }
        for scale in [0.0, -1.0] {
            assert!(matches!(
                nav.step(now, cursor, geometry(0.0, 0.0, scale), solid()),
                PursuitStep::Blocked(_)
            ));
        }
        assert!(matches!(
            nav.step(
                now,
                Point {
                    x: -f64::MAX,
                    y: 0.0
                },
                geometry(f64::MAX, 0.0, 1.0),
                solid()
            ),
            PursuitStep::Blocked(_)
        ));
    }
}
