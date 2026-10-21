//! `clawdtoast overlay` on Linux (Wayland). Same contract as the macOS overlay: the daemon feeds it
//! one JSON card per line on stdin and it exits shortly after the last card is gone. Each card is a
//! `wlr-layer-shell` surface on the overlay layer, anchored to a corner, with no keyboard focus, so
//! it floats above everything (full-screen windows included) without ever taking focus.

use crate::config::{Config, Position};
use crate::core::Card;
use crate::feed::Feed;
use crate::stack::{Offer, Stack};
use crate::timeline::{Timeline, IDLE_EXIT};
use crate::{log, protocol, render};
use std::io::Write;
use std::os::fd::{AsFd, AsRawFd, FromRawFd};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer, wl_compositor, wl_output, wl_pointer, wl_registry, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_client::{delegate_noop, Connection, Dispatch, QueueHandle, WEnum};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{Layer, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1},
};

const MARGIN: i32 = 12;
const GAP: i32 = 8;
const BTN_LEFT: u32 = 0x110;

struct Shown {
    surface: wl_surface::WlSurface,
    layer: ZwlrLayerSurfaceV1,
    tl: Timeline,
    configured: bool,
    /// Present when the compositor does fractional scaling: the buffer is `scale` times the card
    /// size and the viewport maps it back onto the card's logical size.
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    scale: f64,
    /// (frame, opacity in 1/255ths, scale in 1/120ths) of what is attached now, to skip identical redraws.
    drawn: Option<(usize, u8, u32)>,
}

struct State {
    qh: QueueHandle<State>,
    compositor: wl_compositor::WlCompositor,
    shm: wl_shm::WlShm,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    fractional_manager: Option<WpFractionalScaleManagerV1>,
    /// Integer scale of the sharpest output, used when the compositor has no fractional scaling.
    output_scale: i32,
    pointer: Option<wl_pointer::WlPointer>,
    stack: Stack,
    duration: Duration,
    position: Position,
    shown: Vec<Shown>,
    assets: Option<render::Assets>,
    /// Card that has the pointer over it.
    hovered: Option<wl_surface::WlSurface>,
}

pub fn run() {
    let Ok(conn) = Connection::connect_to_env() else { return crate::overlay_x11::run() };
    let (globals, mut queue) = match registry_queue_init::<State>(&conn) {
        Ok(x) => x,
        Err(e) => return log::line(&format!("overlay: registry: {e}")),
    };
    let qh = queue.handle();
    let (Ok(compositor), Ok(shm)) = (globals.bind(&qh, 4..=6, ()), globals.bind(&qh, 1..=1, ())) else {
        return log::line("overlay: compositor lacks wl_compositor/wl_shm");
    };
    // GNOME's Mutter has no layer-shell; its cards go through XWayland instead.
    let Ok(layer_shell) = globals.bind(&qh, 1..=4, ()) else { return crate::overlay_x11::run() };
    let _: Result<wl_seat::WlSeat, _> = globals.bind(&qh, 1..=5, ());
    let viewporter = globals.bind(&qh, 1..=1, ()).ok();
    let fractional_manager = globals.bind(&qh, 1..=1, ()).ok();
    let outputs: Vec<u32> = globals.contents().with_list(|l| {
        l.iter().filter(|g| g.interface == "wl_output").map(|g| g.name).collect()
    });
    for name in outputs {
        let _ = globals.registry().bind::<wl_output::WlOutput, _, _>(name, 2, &qh, ());
    }

    let Some(feed) = Feed::spawn() else { return };

    let cfg = Config::load();
    let mut st = State {
        qh,
        compositor,
        shm,
        layer_shell,
        viewporter,
        fractional_manager,
        output_scale: 1,
        pointer: None,
        stack: Stack::new(cfg.max_stack),
        duration: Duration::from_secs_f64(cfg.duration_secs),
        position: cfg.position,
        shown: Vec::new(),
        assets: None,
        hovered: None,
    };
    let mut eof = false;
    let mut idle_since: Option<Instant> = None;

    loop {
        // Cards from the daemon.
        while let Ok(msg) = feed.rx.try_recv() {
            match msg {
                None => eof = true,
                Some(card) => {
                    idle_since = None;
                    st.offer(card);
                }
            }
        }

        let now = Instant::now();
        let hold = st.duration;
        let expired: Vec<usize> =
            (0..st.shown.len()).filter(|&i| st.shown[i].tl.alpha(now, hold).is_none()).collect();
        for i in expired.into_iter().rev() {
            st.remove(i, "timeout");
        }

        let mut next = now + Duration::from_secs(3600);
        let frames = st.assets.get_or_insert_with(render::Assets::load).frames();
        for i in 0..st.shown.len() {
            let Some((alpha, wake)) = st.shown[i].tl.tick(now, hold, frames) else { continue };
            st.draw(i, alpha);
            if let Some(w) = wake {
                next = next.min(w);
            }
        }

        if st.shown.is_empty() && st.stack.is_empty() {
            let since = *idle_since.get_or_insert(now);
            if eof || now.duration_since(since) >= IDLE_EXIT {
                return;
            }
            next = next.min(since + IDLE_EXIT);
        }

        // Sleep until a wayland event, a card from stdin, or the next animation/timeout tick.
        if queue.dispatch_pending(&mut st).is_err() || queue.flush().is_err() {
            return log::line("overlay: wayland connection lost");
        }
        let Some(guard) = queue.prepare_read() else { continue };
        let mut pfds = [
            libc::pollfd { fd: guard.connection_fd().as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: feed.wake_fd, events: libc::POLLIN, revents: 0 },
        ];
        let timeout = next.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
        unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout) };
        if pfds[1].revents != 0 {
            feed.drain();
        }
        if pfds[0].revents != 0 {
            if guard.read().is_err() {
                return log::line("overlay: wayland read failed");
            }
        } else {
            drop(guard);
        }
        if queue.dispatch_pending(&mut st).is_err() {
            return log::line("overlay: wayland dispatch failed");
        }
    }
}

impl State {
    fn offer(&mut self, card: Card) {
        match self.stack.offer(&card) {
            Offer::Show => self.open(card),
            Offer::Replace(i) => {
                let s = &mut self.shown[i];
                s.tl.replace(card);
                s.drawn = None;
            }
            Offer::Queued => {}
        }
    }

    /// Anchor and margins that put stack slot `slot` in this corner. Top corners stack downward,
    /// bottom corners upward.
    fn place(&self, layer: &ZwlrLayerSurfaceV1, slot: usize) {
        let off = slot as i32 * (render::CARD_H as i32 + GAP);
        let (top, side) = (self.position.is_top(), self.position.is_left());
        layer.set_anchor(
            if top { Anchor::Top } else { Anchor::Bottom } | if side { Anchor::Left } else { Anchor::Right },
        );
        let (v, h) = (MARGIN + off, MARGIN + 4);
        layer.set_margin(
            if top { v } else { 0 },
            if side { 0 } else { h },
            if top { 0 } else { v },
            if side { h } else { 0 },
        );
    }

    fn open(&mut self, card: Card) {
        let surface = self.compositor.create_surface(&self.qh, ());
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            None, // the compositor picks the output, normally the one you are working on
            Layer::Overlay,
            "clawdtoast".into(),
            &self.qh,
            (),
        );
        let fractional = match (&self.fractional_manager, &self.viewporter) {
            (Some(m), Some(_)) => Some(m.get_fractional_scale(&surface, &self.qh, ())),
            _ => None,
        };
        let viewport = fractional.as_ref().and(self.viewporter.as_ref()).map(|v| {
            let vp = v.get_viewport(&surface, &self.qh, ());
            vp.set_destination(render::CARD_W as i32, render::CARD_H as i32);
            vp
        });
        layer.set_size(render::CARD_W as u32, render::CARD_H as u32);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        self.place(&layer, self.shown.len());
        surface.commit(); // the first commit has no buffer; we attach one on `configure`
        if card.ts > 0 {
            log::line(&format!("latency_ms={}", protocol::now_ms().saturating_sub(card.ts)));
        }
        self.shown.push(Shown {
            surface,
            layer,
            tl: Timeline::new(card),
            configured: false,
            viewport,
            fractional,
            scale: 1.0,
            drawn: None,
        });
    }

    /// Removes the card at `idx`, promotes a queued card if any, and closes the gap.
    fn remove(&mut self, idx: usize, why: &str) {
        log::line(&format!("dismiss: {why}"));
        let s = self.shown.remove(idx);
        if self.hovered.as_ref() == Some(&s.surface) {
            self.hovered = None;
        }
        if let Some(f) = &s.fractional {
            f.destroy();
        }
        if let Some(v) = &s.viewport {
            v.destroy();
        }
        s.layer.destroy();
        s.surface.destroy();
        if let Some(next) = self.stack.remove(idx) {
            self.open(next);
        }
        for i in idx..self.shown.len() {
            self.place(&self.shown[i].layer, i);
            self.shown[i].surface.commit();
        }
    }

    fn draw(&mut self, i: usize, alpha: f64) {
        let s = &mut self.shown[i];
        let q = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        // Without fractional scaling the buffer scale must be a whole number.
        let scale = if s.fractional.is_some() { s.scale } else { self.output_scale as f64 };
        let key = (s.tl.frame, q, (scale * 120.0).round() as u32);
        if !s.configured || s.drawn == Some(key) {
            return;
        }
        s.drawn = Some(key);
        let (w, h) = ((render::CARD_W * scale).round() as u32, (render::CARD_H * scale).round() as u32);
        let assets = self.assets.get_or_insert_with(render::Assets::load);
        let pixels = render::render(&s.tl.card, assets, s.tl.frame, w, h, scale);
        let bytes = render::fade_bytes(&pixels, q);
        let fd = unsafe { libc::memfd_create(c"clawdtoast".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return log::line("overlay: memfd_create failed");
        }
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        if file.write_all(&bytes).is_err() {
            return log::line("overlay: shm write failed");
        }
        let pool = self.shm.create_pool(file.as_fd(), bytes.len() as i32, &self.qh, ());
        let buf = pool.create_buffer(0, w as i32, h as i32, w as i32 * 4, wl_shm::Format::Argb8888, &self.qh, ());
        pool.destroy(); // the buffer keeps the memory alive until the compositor releases it
        if s.viewport.is_none() {
            s.surface.set_buffer_scale(scale as i32);
        }
        s.surface.attach(Some(&buf), 0, 0);
        s.surface.damage_buffer(0, 0, w as i32, h as i32);
        s.surface.commit();
    }

    fn index_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
        self.shown.iter().position(|s| s.surface == *surface)
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

delegate_noop!(State: ignore wl_compositor::WlCompositor);
delegate_noop!(State: ignore wl_shm::WlShm);
delegate_noop!(State: ignore wl_shm_pool::WlShmPool);
delegate_noop!(State: ignore wl_surface::WlSurface);
delegate_noop!(State: ignore ZwlrLayerShellV1);
delegate_noop!(State: ignore WpViewporter);
delegate_noop!(State: ignore WpViewport);
delegate_noop!(State: ignore WpFractionalScaleManagerV1);

impl Dispatch<WpFractionalScaleV1, ()> for State {
    fn event(
        st: &mut Self,
        f: &WpFractionalScaleV1,
        ev: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = ev {
            if let Some(s) = st.shown.iter_mut().find(|s| s.fractional.as_ref() == Some(f)) {
                s.scale = scale as f64 / 120.0;
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for State {
    fn event(
        st: &mut Self,
        _: &wl_output::WlOutput,
        ev: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Scale { factor } = ev {
            st.output_scale = st.output_scale.max(factor);
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, ()> for State {
    fn event(_: &mut Self, buf: &wl_buffer::WlBuffer, ev: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = ev {
            buf.destroy();
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, ()> for State {
    fn event(
        st: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        ev: zwlr_layer_surface_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(i) = st.shown.iter().position(|s| s.layer == *layer) else { return };
        match ev {
            zwlr_layer_surface_v1::Event::Configure { serial, .. } => {
                layer.ack_configure(serial);
                st.shown[i].configured = true;
                st.shown[i].drawn = None; // the main loop draws it with the right opacity
            }
            zwlr_layer_surface_v1::Event::Closed => st.remove(i, "closed by compositor"),
            _ => {}
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        st: &mut Self,
        seat: &wl_seat::WlSeat,
        ev: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = ev {
            if caps.contains(wl_seat::Capability::Pointer) && st.pointer.is_none() {
                st.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for State {
    fn event(
        st: &mut Self,
        _: &wl_pointer::WlPointer,
        ev: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match ev {
            wl_pointer::Event::Enter { surface, .. } => {
                if let Some(i) = st.index_of(&surface) {
                    st.shown[i].tl.hover();
                }
                st.hovered = Some(surface);
            }
            wl_pointer::Event::Leave { surface, .. } => {
                if let Some(i) = st.index_of(&surface) {
                    st.shown[i].tl.unhover();
                }
                if st.hovered.as_ref() == Some(&surface) {
                    st.hovered = None;
                }
            }
            wl_pointer::Event::Button { state: WEnum::Value(wl_pointer::ButtonState::Pressed), button, .. }
                if button == BTN_LEFT =>
            {
                if let Some(i) = st.hovered.as_ref().and_then(|h| st.index_of(h)) {
                    // Wayland has no way to raise another client's window, so a click only dismisses.
                    st.remove(i, "click");
                }
            }
            _ => {}
        }
    }
}
