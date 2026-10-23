//! `clawdtoast overlay`: short-lived UI process. The daemon spawns it when a card is due and feeds
//! it one JSON card per line on stdin. It exits shortly after the last card is gone, so the
//! always-running daemon never carries AppKit's (or Xlib's) memory.
//!
//! macOS and X11 draw through winit; Wayland uses wlr-layer-shell directly (`wayland.rs`), and
//! compositors without it get a desktop notification (`notify.rs`).

use crate::anim::{Timing, IDLE_EXIT};
use crate::config::{Config, Position};
use crate::core::Card;
use crate::stack::{Offer, Stack};
use crate::{log, protocol, render};
use std::io::BufRead;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::window::{Window, WindowId, WindowLevel};

#[cfg(target_os = "macos")]
use crate::macos as platform;
#[cfg(windows)]
use crate::windows as platform;
#[cfg(target_os = "linux")]
use crate::x11 as platform;
#[cfg(windows)]
use winit::platform::windows::WindowAttributesExtWindows;

enum Ev {
    Card(Card),
    Eof,
}

/// Calls `f` with each card the daemon writes to stdin until EOF, or until `f` returns false.
pub fn read_cards(mut f: impl FnMut(Card) -> bool) {
    for line in std::io::stdin().lock().lines().map_while(Result::ok) {
        match serde_json::from_str::<Card>(&line) {
            Ok(card) => {
                if !f(card) {
                    return;
                }
            }
            Err(e) => log::line(&format!("overlay: bad card: {e}")),
        }
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
/// For overlays with no window of their own: calls `f` for each card, and returns once no card
/// has arrived for `IDLE_EXIT` or the daemon closed stdin. The daemon waits for its overlay to
/// exit before it does, so an overlay that only stopped at EOF would deadlock with it.
pub fn serve_cards(mut f: impl FnMut(Card)) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || read_cards(|card| tx.send(card).is_ok()));
    while let Ok(card) = rx.recv_timeout(IDLE_EXIT) {
        f(card);
    }
}

#[cfg(not(target_os = "linux"))]
pub fn run() {
    run_winit();
}

#[cfg(target_os = "linux")]
pub fn run() {
    use crate::backend::{self, Backend};
    for b in backend::order(|k| std::env::var(k).ok()) {
        match b {
            Backend::Wayland => match crate::wayland::run() {
                Ok(()) => return,
                Err(e) => log::line(&format!("overlay: wayland: {e}")),
            },
            Backend::X11 => {
                if run_winit() {
                    return;
                }
            }
            Backend::Notify => {
                crate::notify::run();
                return;
            }
        }
    }
    // No display at all (SSH, a bare VT): nothing to draw on. Keep reading so the daemon's writes
    // succeed instead of respawning us for every card.
    log::line("overlay: no display");
    serve_cards(|_| {});
}

fn event_loop() -> Result<EventLoop<Ev>, winit::error::EventLoopError> {
    #[allow(unused_mut)]
    let mut builder = EventLoop::<Ev>::with_user_event();
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder
            .with_activation_policy(ActivationPolicy::Accessory)
            .with_default_menu(false)
            // winit otherwise activates the app at launch, which steals focus from whatever you're using.
            .with_activate_ignoring_other_apps(false);
    }
    builder.build()
}

/// Runs the winit overlay; false if the event loop could not start (no usable display).
fn run_winit() -> bool {
    #[cfg(target_os = "linux")]
    {
        let missing = platform::missing_libs();
        if !missing.is_empty() {
            log::line(&format!(
                "overlay: X11 libraries not installed: {}",
                missing.join(", ")
            ));
            return false;
        }
    }
    let event_loop = match event_loop() {
        Ok(l) => l,
        Err(e) => {
            log::line(&format!("overlay: event loop: {e}"));
            return false;
        }
    };
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        read_cards(|card| proxy.send_event(Ev::Card(card)).is_ok());
        let _ = proxy.send_event(Ev::Eof);
    });
    let cfg = Config::load();
    let mut app = App {
        stack: Stack::new(cfg.max_stack),
        duration: Duration::from_secs_f64(cfg.duration_secs),
        position: cfg.position,
        shown: Vec::new(),
        assets: None,
        eof: false,
        idle_since: None,
        opaque: opaque_cards(),
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        log::line(&format!("overlay: run_app: {e}"));
    }
    true
}

/// True when the screen has no compositor, so a transparent window would show garbage corners.
#[cfg(target_os = "linux")]
fn opaque_cards() -> bool {
    !platform::compositor_running()
}

#[cfg(not(target_os = "linux"))]
fn opaque_cards() -> bool {
    false
}

struct Shown {
    window: Arc<Window>,
    surface: softbuffer::Surface<Arc<Window>, Arc<Window>>,
    card: Card,
    timing: Timing,
    frame: usize,
    sprite: usize,
}

struct App {
    stack: Stack,
    duration: Duration,
    position: Position,
    shown: Vec<Shown>,
    assets: Option<render::Assets>,
    eof: bool,
    idle_since: Option<Instant>,
    opaque: bool,
}

fn draw(assets: &render::Assets, s: &mut Shown, opaque: bool) {
    let size = s.window.inner_size();
    let (Some(w), Some(h)) = (
        std::num::NonZeroU32::new(size.width),
        std::num::NonZeroU32::new(size.height),
    ) else {
        return;
    };
    let mut pixels = render::render(
        &s.card,
        assets,
        s.sprite,
        s.frame,
        size.width,
        size.height,
        s.window.scale_factor(),
    );
    if opaque {
        render::flatten(&mut pixels);
    }
    if s.surface.resize(w, h).is_err() {
        return;
    }
    if let Ok(mut buf) = s.surface.buffer_mut() {
        let n = buf.len().min(pixels.len());
        buf[..n].copy_from_slice(&pixels[..n]);
        let _ = buf.present();
    }
}

impl App {
    fn open(&mut self, el: &ActiveEventLoop, card: Card) {
        let slot = self.shown.len();
        let attrs = Window::default_attributes();
        #[cfg(windows)]
        let attrs = attrs.with_skip_taskbar(true);
        let attrs = attrs
            .with_title("clawdtoast")
            .with_inner_size(LogicalSize::new(render::CARD_W, render::CARD_H))
            .with_decorations(false)
            .with_transparent(!self.opaque)
            .with_resizable(false)
            .with_active(false)
            .with_window_level(WindowLevel::AlwaysOnTop);
        #[cfg(target_os = "linux")]
        let attrs = {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            // Override-redirect: the window manager never sees it, so it cannot take focus, get a
            // frame, or be tiled. It is mapped by `configure` once placed.
            attrs
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::Notification])
                .with_visible(false)
        };
        let window = match el.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => return log::line(&format!("create_window: {e}")),
        };
        let surface = match softbuffer::Context::new(window.clone())
            .and_then(|ctx| softbuffer::Surface::new(&ctx, window.clone()))
        {
            Ok(s) => s,
            Err(e) => return log::line(&format!("softbuffer: {e}")),
        };
        platform::set_alpha(&window, 0.0);
        let mut s = Shown {
            window,
            surface,
            card,
            timing: Timing::new(),
            frame: 0,
            sprite: self
                .assets
                .get_or_insert_with(render::Assets::load)
                .choose_sprite(),
        };
        #[cfg(not(target_os = "linux"))]
        platform::configure(
            &s.window,
            render::CARD_W,
            render::CARD_H,
            slot,
            self.position,
        );
        draw(
            self.assets.get_or_insert_with(render::Assets::load),
            &mut s,
            self.opaque,
        );
        // X11 maps the window in `configure`, so draw first to keep the first visible frame from being blank.
        #[cfg(target_os = "linux")]
        platform::configure(
            &s.window,
            render::CARD_W,
            render::CARD_H,
            slot,
            self.position,
        );
        if s.card.ts > 0 {
            log::line(&format!(
                "latency_ms={}",
                protocol::now_ms().saturating_sub(s.card.ts)
            ));
        }
        self.shown.push(s);
    }

    /// Removes the card at `idx`, promotes a queued card if any, and closes the gap.
    fn remove(&mut self, el: &ActiveEventLoop, idx: usize, why: &str) {
        log::line(&format!("dismiss: {why}"));
        self.shown.remove(idx); // dropping the window destroys it
        if let Some(next) = self.stack.remove(idx) {
            self.open(el, next);
        }
        for (i, s) in self.shown.iter().enumerate().skip(idx) {
            platform::place(&s.window, render::CARD_W, render::CARD_H, i, self.position);
        }
    }

    fn index_of(&self, id: WindowId) -> Option<usize> {
        self.shown.iter().position(|s| s.window.id() == id)
    }
}

impl ApplicationHandler<Ev> for App {
    fn resumed(&mut self, _: &ActiveEventLoop) {}

    fn user_event(&mut self, el: &ActiveEventLoop, ev: Ev) {
        let card = match ev {
            Ev::Eof => {
                self.eof = true;
                // The owning daemon exited or was replaced. Do not leave its
                // attention windows alive alongside a new daemon's overlay.
                self.shown.clear();
                el.exit();
                return;
            }
            Ev::Card(c) => c,
        };
        self.idle_since = None;
        if card.expired(protocol::now_ms()) && !card.dismiss {
            return;
        }
        match self.stack.offer(&card) {
            Offer::Show => self.open(el, card),
            Offer::Replace(i) => {
                let s = &mut self.shown[i];
                s.card = card;
                if !s.card.silent {
                    s.timing.restart();
                    s.sprite = self
                        .assets
                        .get_or_insert_with(render::Assets::load)
                        .choose_sprite();
                }
                s.frame = self
                    .assets
                    .get_or_insert_with(render::Assets::load)
                    .frame_at(&s.card, s.sprite, &s.timing, Instant::now());
                draw(
                    self.assets.get_or_insert_with(render::Assets::load),
                    s,
                    self.opaque,
                );
            }
            Offer::Queued => {}
            Offer::Dismiss(i) => self.remove(el, i, "resolved or session closed"),
            Offer::Ignore => {}
        }
    }

    fn window_event(&mut self, el: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(i) = self.index_of(id) else { return };
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                platform::focus_pid(self.shown[i].card.pid, &self.shown[i].card.app);
                self.remove(el, i, "click")
            }
            WindowEvent::CursorEntered { .. } => self.shown[i].timing.hover(true),
            WindowEvent::CursorLeft { .. } => self.shown[i].timing.hover(false),
            WindowEvent::RedrawRequested => {
                if let Some(a) = self.assets.as_ref() {
                    draw(a, &mut self.shown[i], self.opaque);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, el: &ActiveEventLoop) {
        let now = Instant::now();
        let hold = self.duration;

        let expired: Vec<usize> = (0..self.shown.len())
            .filter(|&i| {
                self.shown[i].card.expired(protocol::now_ms())
                    || self.shown[i]
                        .timing
                        .alpha(now, self.shown[i].card.hold(hold))
                        .is_none()
            })
            .collect();
        for i in expired.into_iter().rev() {
            self.remove(el, i, "timeout");
        }

        let mut next = now + Duration::from_secs(3600);
        let assets = self.assets.get_or_insert_with(render::Assets::load);
        for s in self.shown.iter_mut() {
            let hold = s.card.hold(hold);
            let Some(a) = s.timing.alpha(now, hold) else {
                continue;
            };
            platform::set_alpha(&s.window, a);
            let frame = assets.frame_at(&s.card, s.sprite, &s.timing, now);
            if frame != s.frame {
                s.frame = frame;
                draw(assets, s, self.opaque);
            }
            if let Some(wake) = assets.next_wake(&s.card, s.sprite, &s.timing, now, hold) {
                next = next.min(wake);
            }
        }

        if self.shown.is_empty() && self.stack.is_empty() {
            let since = *self.idle_since.get_or_insert(now);
            if self.eof || now.duration_since(since) >= IDLE_EXIT {
                el.exit();
                return;
            }
            next = next.min(since + IDLE_EXIT);
        }
        el.set_control_flow(ControlFlow::WaitUntil(next));
    }
}
