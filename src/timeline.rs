//! When a card fades in, waves, holds and fades out. Shared by the Linux overlay backends; the
//! backends only differ in how they put pixels on screen.

use crate::core::Card;
use std::time::{Duration, Instant};

pub const IDLE_EXIT: Duration = Duration::from_secs(3);
const FADE_IN: Duration = Duration::from_millis(150);
const FADE_OUT: Duration = Duration::from_millis(300);
const FRAME: Duration = Duration::from_millis(16);
const WAVE_FRAME: Duration = Duration::from_millis(125); // 8 fps

pub struct Timeline {
    pub card: Card,
    pub frame: usize,
    shown_at: Instant,
    hover_since: Option<Instant>,
}

impl Timeline {
    pub fn new(card: Card) -> Timeline {
        Timeline { card, frame: 0, shown_at: Instant::now(), hover_since: None }
    }

    /// Shows `card` in place of the current one, starting the timer over.
    pub fn replace(&mut self, card: Card) {
        *self = Timeline::new(card);
    }

    pub fn hover(&mut self) {
        self.hover_since = Some(Instant::now());
    }

    pub fn unhover(&mut self) {
        if let Some(h) = self.hover_since.take() {
            self.shown_at += h.elapsed(); // time spent hovering does not count
        }
    }

    /// Time on screen so far, frozen while the pointer hovers.
    fn age(&self, now: Instant) -> Duration {
        self.hover_since.unwrap_or(now).saturating_duration_since(self.shown_at)
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

    /// Advances the wave frame. Returns the opacity now and when to wake next (None while hovering,
    /// when only pointer events matter), or None once the card has faded out.
    pub fn tick(&mut self, now: Instant, hold: Duration, frames: usize) -> Option<(f64, Option<Instant>)> {
        let alpha = self.alpha(now, hold)?;
        let t = self.age(now);
        self.frame = (t.as_millis() / WAVE_FRAME.as_millis()) as usize % frames.max(1);
        let wake = if self.hover_since.is_some() && t >= FADE_IN && t < hold {
            None
        } else if t < FADE_IN || t >= hold {
            Some(now + FRAME)
        } else {
            let to_frame = WAVE_FRAME - Duration::from_millis((t.as_millis() % WAVE_FRAME.as_millis()) as u64);
            Some((now + to_frame).min(self.shown_at + hold))
        };
        Some((alpha, wake))
    }
}
