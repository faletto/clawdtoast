//! Card timing shared by the winit and Wayland overlays: fade in, hold (paused while hovered),
//! fade out, plus an independent animation clock.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

use std::time::{Duration, Instant};

pub const FADE_IN: Duration = Duration::from_millis(150);
pub const FADE_OUT: Duration = Duration::from_millis(300);
pub const FRAME: Duration = Duration::from_millis(16);
pub const WAVE_FRAME: Duration = Duration::from_millis(125); // Clawd: 8 fps
pub const IDLE_EXIT: Duration = Duration::from_secs(3);

pub struct Timing {
    pub shown_at: Instant,
    pub hover_since: Option<Instant>,
    animation_since: Instant,
}

impl Timing {
    pub fn new() -> Timing {
        let now = Instant::now();
        Timing {
            shown_at: now,
            hover_since: None,
            animation_since: now,
        }
    }

    pub fn restart(&mut self) {
        *self = Timing::new();
    }

    pub fn hover(&mut self, on: bool) {
        if on {
            self.hover_since.get_or_insert_with(Instant::now);
        } else if let Some(h) = self.hover_since.take() {
            self.shown_at += h.elapsed(); // time spent hovering does not count
        }
    }

    /// Time on screen so far, frozen while the pointer hovers.
    pub fn age(&self, now: Instant) -> Duration {
        self.hover_since
            .unwrap_or(now)
            .saturating_duration_since(self.shown_at)
    }

    /// Opacity now, or None once the card has faded out.
    pub fn alpha(&self, now: Instant, hold: Duration) -> Option<f64> {
        let t = self.age(now);
        if t < FADE_IN {
            Some(t.as_secs_f64() / FADE_IN.as_secs_f64())
        } else if t < hold {
            Some(1.0)
        } else if t < hold + FADE_OUT {
            Some(1.0 - (t - hold).as_secs_f64() / FADE_OUT.as_secs_f64())
        } else {
            None
        }
    }

    /// Animation keeps playing while hovering pauses the card dismissal timer.
    pub fn wave_frame(&self, now: Instant, frames: usize, frame_interval: Duration) -> usize {
        (now.saturating_duration_since(self.animation_since)
            .as_nanos()
            / frame_interval.as_nanos().max(1)) as usize
            % frames.max(1)
    }

    /// Earliest instant this card needs an animation tick, fade redraw, or removal.
    pub fn next_wake(
        &self,
        now: Instant,
        hold: Duration,
        frame_interval: Duration,
    ) -> Option<Instant> {
        let tick = frame_interval.as_nanos().max(1);
        let age = now
            .saturating_duration_since(self.animation_since)
            .as_nanos();
        let delay = Duration::from_nanos((tick - age % tick) as u64);
        self.next_wake_after(now, hold, delay)
    }

    /// Source presentation times can vary inside a loop, even in nominally 8.33 fps clips.
    fn frame_phase(&self, now: Instant, durations: &[Duration]) -> (usize, Duration) {
        let cycle: Duration = durations.iter().copied().sum();
        let mut phase = now
            .saturating_duration_since(self.animation_since)
            .as_nanos()
            % cycle.as_nanos().max(1);
        for (i, duration) in durations.iter().enumerate() {
            if phase < duration.as_nanos() {
                return (
                    i,
                    Duration::from_nanos((duration.as_nanos() - phase) as u64),
                );
            }
            phase -= duration.as_nanos();
        }
        (0, WAVE_FRAME)
    }

    pub fn wave_frame_durations(&self, now: Instant, durations: &[Duration]) -> usize {
        self.frame_phase(now, durations).0
    }

    pub fn next_wake_durations(
        &self,
        now: Instant,
        hold: Duration,
        durations: &[Duration],
    ) -> Option<Instant> {
        self.next_wake_after(now, hold, self.frame_phase(now, durations).1)
    }

    fn next_wake_after(&self, now: Instant, hold: Duration, to_frame: Duration) -> Option<Instant> {
        let t = self.age(now);
        if t < FADE_IN || t >= hold {
            Some(now + FRAME)
        } else if self.hover_since.is_some() {
            Some(now + to_frame)
        } else {
            Some((now + to_frame).min(self.shown_at + hold))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_durations_control_boundaries_looping_and_hover() {
        let mut t = Timing::new();
        let durations = [120, 100, 150].map(Duration::from_millis);
        let at = |ms| t.shown_at + Duration::from_millis(ms);
        for (ms, frame) in [
            (119, 0),
            (120, 1),
            (219, 1),
            (220, 2),
            (369, 2),
            (370, 0),
            (490, 1),
        ] {
            assert_eq!(t.wave_frame_durations(at(ms), &durations), frame);
        }
        let now = at(225);
        assert_eq!(
            t.next_wake_durations(now, Duration::from_secs(5), &durations),
            Some(at(370))
        );
        t.hover_since = Some(t.shown_at + Duration::from_millis(200));
        assert_eq!(t.wave_frame_durations(now, &durations), 2);
        assert_eq!(
            t.next_wake_durations(now, Duration::from_secs(5), &durations),
            Some(t.shown_at + Duration::from_millis(370))
        );
    }

    #[test]
    fn fades_in_holds_fades_out() {
        let t = Timing::new();
        let hold = Duration::from_secs(5);
        let at = |ms| t.shown_at + Duration::from_millis(ms);
        assert!(t.alpha(at(75), hold).unwrap() > 0.4 && t.alpha(at(75), hold).unwrap() < 0.6);
        assert_eq!(t.alpha(at(1000), hold), Some(1.0));
        assert!(t.alpha(at(5150), hold).unwrap() < 0.6);
        assert_eq!(t.alpha(at(5300), hold), None);
    }

    #[test]
    fn hover_freezes_age() {
        let mut t = Timing::new();
        t.hover(true);
        let frozen = t.age(t.shown_at + Duration::from_secs(100));
        assert_eq!(frozen, t.hover_since.unwrap() - t.shown_at);
        let tick = Duration::from_millis(80);
        let later = t.shown_at + Duration::from_millis(410);
        assert_eq!(t.wave_frame(later, 11, tick), 5);
        // Hover began during fade-in, so the fade still needs a 16ms tick.
        assert_eq!(
            t.next_wake(later, Duration::from_secs(5), tick),
            Some(later + FRAME)
        );
    }

    #[test]
    fn custom_frame_rate_controls_frames_and_wake() {
        let t = Timing::new();
        let tick = Duration::from_millis(80);
        let now = t.shown_at + Duration::from_millis(170);
        assert_eq!(t.wave_frame(now, 11, tick), 2);
        assert_eq!(
            t.next_wake(now, Duration::from_secs(5), tick),
            Some(now + Duration::from_millis(70))
        );
    }

    #[test]
    fn hover_does_not_reset_animation_phase_after_exit() {
        let mut t = Timing::new();
        let tick = Duration::from_millis(80);
        t.hover_since = Some(t.shown_at + Duration::from_millis(200));
        let later = t.shown_at + Duration::from_millis(410);
        // Simulate a completed hover without relying on wall-clock sleeps.
        t.shown_at += Duration::from_millis(210);
        t.hover_since = None;
        assert_eq!(t.age(later), Duration::from_millis(200));
        assert_eq!(t.wave_frame(later, 11, tick), 5);
        assert_eq!(
            t.next_wake(later, Duration::from_secs(5), tick),
            Some(later + Duration::from_millis(70))
        );
    }
}
