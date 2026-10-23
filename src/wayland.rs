//! Wayland overlay on wlr-layer-shell (wlroots compositors, KDE, COSMIC, ...), the Wayland
//! counterpart of the winit path. Each card is a layer surface on the overlay layer, anchored to a
//! corner with margins for its stack slot, with no keyboard interactivity, so it never takes focus.
//! Wayland offers no per-surface opacity or global pointer position, so fades are baked into the
//! pixels and the compositor picks the output (normally the one you are working on).
//!
//! GNOME does not implement layer-shell; `run` then returns an error before touching stdin and
//! the caller falls back to a desktop notification.

use crate::anim::{Timing, IDLE_EXIT};
use crate::config::{Config, Position};
use crate::core::Card;
use crate::overlay::read_cards;
use crate::stack::{Offer, Stack};
use crate::{geom, log, protocol, render};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::registry_queue_init;
use smithay_client_toolkit::reexports::client::protocol::{
    wl_output, wl_pointer, wl_seat, wl_shm, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::slot::SlotPool;
use smithay_client_toolkit::shm::{Shm, ShmHandler};
use smithay_client_toolkit::{
    delegate_compositor, delegate_layer, delegate_output, delegate_pointer, delegate_registry,
    delegate_seat, delegate_shm, registry_handlers,
};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BTN_LEFT: u32 = 0x110;

enum Ev {
    Card(Card),
    Eof,
}

struct WCard {
    layer: LayerSurface,
    card: Card,
    timing: Timing,
    frame: usize,
    sprite: usize,
    scale: i32,
    /// The compositor has acked the surface, so buffers may be attached.
    configured: bool,
    /// Opacity baked into the last buffer, to know when a fade needs a redraw.
    drawn_alpha: f64,
}

struct Wl {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    compositor: CompositorState,
    layer_shell: LayerShell,
    pool: SlotPool,
    pointer: Option<wl_pointer::WlPointer>,

    stack: Stack,
    duration: Duration,
    position: Position,
    cards: Vec<WCard>,
    assets: render::Assets,
    rx: mpsc::Receiver<Ev>,
    eof: bool,
    idle_since: Option<Instant>,
}

/// Runs the layer-shell overlay until the daemon closes stdin or the cards have been idle for a
/// while. Errs, without having read stdin, if the compositor cannot host it.
pub fn run() -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| format!("connect: {e}"))?;
    let (globals, mut queue) =
        registry_queue_init::<Wl>(&conn).map_err(|e| format!("registry: {e}"))?;
    let qh = queue.handle();
    let compositor =
        CompositorState::bind(&globals, &qh).map_err(|e| format!("wl_compositor: {e:?}"))?;
    let layer_shell = LayerShell::bind(&globals, &qh)
        .map_err(|_| "compositor has no wlr-layer-shell".to_string())?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| format!("wl_shm: {e:?}"))?;
    let pool = SlotPool::new(
        4 * 4 * render::CARD_W as usize * render::CARD_H as usize,
        &shm,
    )
    .map_err(|e| format!("shm pool: {e}"))?;

    let (tx, rx) = mpsc::channel();
    let (wake_rd, wake_wr) = wake_pipe().map_err(|e| format!("pipe: {e}"))?;
    std::thread::spawn(move || {
        let mut wake = wake_wr;
        read_cards(|card| {
            let ok = tx.send(Ev::Card(card)).is_ok();
            let _ = wake.write_all(&[1]);
            ok
        });
        let _ = tx.send(Ev::Eof);
        let _ = wake.write_all(&[1]);
    });

    let cfg = Config::load();
    let mut wl = Wl {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        compositor,
        layer_shell,
        pool,
        pointer: None,
        stack: Stack::new(cfg.max_stack),
        duration: Duration::from_secs_f64(cfg.duration_secs),
        position: cfg.position,
        cards: Vec::new(),
        assets: render::Assets::load(),
        rx,
        eof: false,
        idle_since: None,
    };

    loop {
        while let Ok(ev) = wl.rx.try_recv() {
            wl.on_event(&qh, ev);
        }
        let wake_at = match wl.tick(&qh) {
            Tick::Exit => return Ok(()),
            Tick::Next(t) => t,
        };
        if let Err(e) = queue.flush() {
            log::line(&format!("wayland: flush: {e}"));
            return Ok(());
        }
        if let Err(e) = queue.dispatch_pending(&mut wl) {
            log::line(&format!("wayland: dispatch: {e}"));
            return Ok(());
        }
        // Wait for the compositor, a new card, or the next animation tick.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let mut fds = [
            libc::pollfd {
                fd: guard.connection_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_rd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let ms = wake_at
            .saturating_duration_since(Instant::now())
            .as_millis()
            .min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms + 1) };
        if n > 0 && fds[0].revents & libc::POLLIN != 0 {
            if let Err(e) = guard.read() {
                log::line(&format!("wayland: read: {e}"));
                return Ok(());
            }
        } else {
            drop(guard);
        }
        if n > 0 && fds[1].revents & libc::POLLIN != 0 {
            let mut buf = [0u8; 64];
            while unsafe { libc::read(wake_rd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) } > 0
            {
            }
        }
        if let Err(e) = queue.dispatch_pending(&mut wl) {
            log::line(&format!("wayland: dispatch: {e}"));
            return Ok(());
        }
    }
}

/// A non-blocking pipe the stdin thread writes to so the poll loop wakes for a new card.
fn wake_pipe() -> std::io::Result<(File, File)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) })
}

enum Tick {
    Exit,
    Next(Instant),
}

fn anchor(pos: Position) -> Anchor {
    match pos {
        Position::TopLeft => Anchor::TOP | Anchor::LEFT,
        Position::TopRight => Anchor::TOP | Anchor::RIGHT,
        Position::BottomLeft => Anchor::BOTTOM | Anchor::LEFT,
        Position::BottomRight => Anchor::BOTTOM | Anchor::RIGHT,
    }
}

/// Renders `c` at its scale with `alpha` baked in and presents it.
fn paint(pool: &mut SlotPool, assets: &render::Assets, c: &mut WCard, alpha: f64) {
    if !c.configured {
        return;
    }
    let scale = c.scale.max(1);
    let (w, h) = (render::CARD_W as i32 * scale, render::CARD_H as i32 * scale);
    let mut pixels = render::render(
        &c.card,
        assets,
        c.sprite,
        c.frame,
        w as u32,
        h as u32,
        scale as f64,
    );
    if alpha < 1.0 {
        render::fade(&mut pixels, alpha);
    }
    let Ok((buffer, canvas)) = pool.create_buffer(w, h, w * 4, wl_shm::Format::Argb8888) else {
        return log::line("wayland: cannot allocate a buffer");
    };
    for (dst, px) in canvas.chunks_mut(4).zip(pixels) {
        dst.copy_from_slice(&px.to_le_bytes());
    }
    let surface = c.layer.wl_surface();
    surface.set_buffer_scale(scale);
    surface.damage_buffer(0, 0, w, h);
    if buffer.attach_to(surface).is_err() {
        return;
    }
    c.layer.commit();
    c.drawn_alpha = alpha;
}

impl Wl {
    fn on_event(&mut self, qh: &QueueHandle<Wl>, ev: Ev) {
        let card = match ev {
            Ev::Eof => {
                self.eof = true;
                self.cards.clear();
                self.stack = Stack::new(1);
                return;
            }
            Ev::Card(c) => c,
        };
        self.idle_since = None;
        if card.expired(protocol::now_ms()) && !card.dismiss {
            return;
        }
        match self.stack.offer(&card) {
            Offer::Show => self.open(qh, card),
            Offer::Replace(i) => {
                let c = &mut self.cards[i];
                c.card = card;
                let alpha = if c.card.silent {
                    c.timing
                        .alpha(Instant::now(), c.card.hold(self.duration))
                        .unwrap_or(0.0)
                } else {
                    c.timing.restart();
                    c.sprite = self.assets.choose_sprite();
                    0.0
                };
                c.frame = self
                    .assets
                    .frame_at(&c.card, c.sprite, &c.timing, Instant::now());
                paint(&mut self.pool, &self.assets, c, alpha);
            }
            Offer::Queued => {}
            Offer::Dismiss(i) => self.remove(qh, i, "resolved or session closed"),
            Offer::Ignore => {}
        }
    }

    fn open(&mut self, qh: &QueueHandle<Wl>, card: Card) {
        let slot = self.cards.len();
        let surface = self.compositor.create_surface(qh);
        let layer = self.layer_shell.create_layer_surface(
            qh,
            surface,
            Layer::Overlay,
            Some("clawdtoast"),
            None,
        );
        layer.set_anchor(anchor(self.position));
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(render::CARD_W as u32, render::CARD_H as u32);
        let (t, r, b, l) = geom::layer_margins(render::CARD_H, slot, self.position);
        layer.set_margin(t, r, b, l);
        // Initial commit without a buffer; the compositor answers with `configure`, then we draw.
        layer.commit();
        if card.ts > 0 {
            log::line(&format!(
                "latency_ms={}",
                protocol::now_ms().saturating_sub(card.ts)
            ));
        }
        self.cards.push(WCard {
            layer,
            card,
            timing: Timing::new(),
            frame: 0,
            sprite: self.assets.choose_sprite(),
            scale: 1,
            configured: false,
            drawn_alpha: -1.0,
        });
    }

    /// Removes the card at `idx`, promotes a queued card if any, and closes the gap.
    fn remove(&mut self, qh: &QueueHandle<Wl>, idx: usize, why: &str) {
        log::line(&format!("dismiss: {why}"));
        self.cards.remove(idx); // dropping the layer surface destroys it
        if let Some(next) = self.stack.remove(idx) {
            self.open(qh, next);
        }
        for (i, c) in self.cards.iter().enumerate().skip(idx) {
            let (t, r, b, l) = geom::layer_margins(render::CARD_H, i, self.position);
            c.layer.set_margin(t, r, b, l);
            c.layer.commit();
        }
    }

    fn tick(&mut self, qh: &QueueHandle<Wl>) -> Tick {
        let now = Instant::now();
        let hold = self.duration;
        let expired: Vec<usize> = (0..self.cards.len())
            .filter(|&i| {
                self.cards[i].card.expired(protocol::now_ms())
                    || self.cards[i]
                        .timing
                        .alpha(now, self.cards[i].card.hold(hold))
                        .is_none()
            })
            .collect();
        for i in expired.into_iter().rev() {
            self.remove(qh, i, "timeout");
        }

        let mut next = now + Duration::from_secs(3600);
        for c in self.cards.iter_mut() {
            let hold = c.card.hold(hold);
            let Some(alpha) = c.timing.alpha(now, hold) else {
                continue;
            };
            let frame = self.assets.frame_at(&c.card, c.sprite, &c.timing, now);
            if c.configured && (frame != c.frame || (alpha - c.drawn_alpha).abs() > 0.004) {
                c.frame = frame;
                paint(&mut self.pool, &self.assets, c, alpha);
            }
            if let Some(wake) = self
                .assets
                .next_wake(&c.card, c.sprite, &c.timing, now, hold)
            {
                next = next.min(wake);
            }
        }

        if self.cards.is_empty() && self.stack.is_empty() {
            let since = *self.idle_since.get_or_insert(now);
            if self.eof || now.duration_since(since) >= IDLE_EXIT {
                return Tick::Exit;
            }
            next = next.min(since + IDLE_EXIT);
        }
        Tick::Next(next)
    }

    fn index_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.cards
            .iter()
            .position(|c| c.layer.wl_surface() == surface)
    }
}

impl CompositorHandler for Wl {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if let Some(i) = self.index_of(surface) {
            let c = &mut self.cards[i];
            c.scale = new_factor.max(1);
            let alpha = c.timing.alpha(Instant::now(), self.duration).unwrap_or(1.0);
            paint(&mut self.pool, &self.assets, c, alpha);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Wl {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for Wl {
    fn closed(&mut self, _: &Connection, qh: &QueueHandle<Self>, layer: &LayerSurface) {
        // The compositor took the surface away (output unplugged): treat it as dismissed.
        if let Some(i) = self
            .cards
            .iter()
            .position(|c| c.layer.wl_surface() == layer.wl_surface())
        {
            self.remove(qh, i, "closed by compositor");
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self
            .cards
            .iter()
            .position(|c| c.layer.wl_surface() == layer.wl_surface())
        else {
            return;
        };
        let c = &mut self.cards[i];
        c.configured = true;
        // Start invisible; the next tick fades it in. Painting here keeps the first frame blank
        // instead of showing a full-opacity card for one frame.
        paint(&mut self.pool, &self.assets, c, 0.0);
    }
}

impl SeatHandler for Wl {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Pointer && self.pointer.is_none() {
            match self.seat_state.get_pointer(qh, &seat) {
                Ok(p) => self.pointer = Some(p),
                Err(e) => log::line(&format!("wayland: pointer: {e}")),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if cap == Capability::Pointer {
            if let Some(p) = self.pointer.take() {
                p.release();
            }
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl PointerHandler for Wl {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let Some(i) = self.index_of(&event.surface) else {
                continue;
            };
            match event.kind {
                PointerEventKind::Enter { .. } => self.cards[i].timing.hover(true),
                PointerEventKind::Leave { .. } => self.cards[i].timing.hover(false),
                // A click dismisses the card. Wayland has no way to raise another client's window,
                // so unlike macOS and X11 it cannot also focus the terminal.
                PointerEventKind::Press {
                    button: BTN_LEFT, ..
                } => {
                    self.remove(qh, i, "click");
                    return; // indices shifted
                }
                _ => {}
            }
        }
    }
}

impl ShmHandler for Wl {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Wl);
delegate_output!(Wl);
delegate_shm!(Wl);
delegate_seat!(Wl);
delegate_pointer!(Wl);
delegate_layer!(Wl);
delegate_registry!(Wl);

impl ProvidesRegistryState for Wl {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}
