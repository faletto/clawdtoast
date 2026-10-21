//! `clawdtoast overlay` through X11, for Wayland compositors without layer-shell (GNOME's Mutter)
//! and plain X sessions. Each card is an override-redirect window with a 32-bit ARGB visual: the
//! window manager never sees it, so it is placed exactly where we say, floats above normal windows
//! and never takes focus. Under Wayland it runs on XWayland.

use crate::config::{Config, Position};
use crate::core::Card;
use crate::feed::Feed;
use crate::stack::{Offer, Stack};
use crate::timeline::{Timeline, IDLE_EXIT};
use crate::{log, protocol, render};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::{
    AtomEnum, ColormapAlloc, ConfigureWindowAux, ConnectionExt as _, CreateGCAux, CreateWindowAux, EventMask,
    Gcontext, ImageFormat, VisualClass, Window, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;

const MARGIN: i32 = 12;
const GAP: i32 = 8;

struct Shown {
    win: Window,
    gc: Gcontext,
    tl: Timeline,
    /// (frame, opacity in 1/255ths) of what is on the window now, to skip identical redraws.
    drawn: Option<(usize, u8)>,
}

/// A rectangle on screen: x, y, width, height.
type Area = (i32, i32, i32, i32);

struct X {
    conn: RustConnection,
    root: Window,
    visual: u32,
    cmap: u32,
    scale: f64,
    stack: Stack,
    duration: Duration,
    position: Position,
    shown: Vec<Shown>,
    assets: Option<render::Assets>,
    /// Where cards go: the monitor holding the pointer when the first card of a stack appeared.
    area: Area,
}

pub fn run() {
    let (conn, screen_num) = match x11rb::connect(None) {
        Ok(c) => c,
        Err(e) => return log::line(&format!("overlay: no layer-shell and no X server: {e}")),
    };
    let screen = &conn.setup().roots[screen_num];
    let Some(visual) = screen
        .allowed_depths
        .iter()
        .filter(|d| d.depth == 32)
        .flat_map(|d| &d.visuals)
        .find(|v| v.class == VisualClass::TRUE_COLOR)
        .map(|v| v.visual_id)
    else {
        return log::line("overlay: X server has no 32-bit ARGB visual");
    };
    let root = screen.root;
    let screen_size = (screen.width_in_pixels as i32, screen.height_in_pixels as i32);
    let cmap = conn.generate_id().unwrap_or(0);
    if conn.create_colormap(ColormapAlloc::NONE, cmap, root, visual).is_err() {
        return log::line("overlay: create_colormap failed");
    }
    let Some(feed) = Feed::spawn() else { return };

    let cfg = Config::load();
    let mut x = X {
        scale: xft_scale(&conn, root),
        conn,
        root,
        visual,
        cmap,
        stack: Stack::new(cfg.max_stack),
        duration: Duration::from_secs_f64(cfg.duration_secs),
        position: cfg.position,
        shown: Vec::new(),
        assets: None,
        area: (0, 0, screen_size.0, screen_size.1),
    };
    let fd = x.conn.stream().as_raw_fd();
    let mut eof = false;
    let mut idle_since: Option<Instant> = None;

    loop {
        while let Ok(msg) = feed.rx.try_recv() {
            match msg {
                None => eof = true,
                Some(card) => {
                    idle_since = None;
                    x.offer(card);
                }
            }
        }

        let now = Instant::now();
        let hold = x.duration;
        let expired: Vec<usize> = (0..x.shown.len()).filter(|&i| x.shown[i].tl.alpha(now, hold).is_none()).collect();
        for i in expired.into_iter().rev() {
            x.remove(i, "timeout");
        }

        let mut next = now + Duration::from_secs(3600);
        let frames = x.assets.get_or_insert_with(render::Assets::load).frames();
        for i in 0..x.shown.len() {
            let Some((alpha, wake)) = x.shown[i].tl.tick(now, hold, frames) else { continue };
            x.draw(i, alpha);
            if let Some(w) = wake {
                next = next.min(w);
            }
        }

        if x.shown.is_empty() && x.stack.is_empty() {
            let since = *idle_since.get_or_insert(now);
            if eof || now.duration_since(since) >= IDLE_EXIT {
                return;
            }
            next = next.min(since + IDLE_EXIT);
        }

        if x.conn.flush().is_err() {
            return log::line("overlay: X connection lost");
        }
        let mut pfds = [
            libc::pollfd { fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: feed.wake_fd, events: libc::POLLIN, revents: 0 },
        ];
        let timeout = next.saturating_duration_since(Instant::now()).as_millis().min(i32::MAX as u128) as i32;
        unsafe { libc::poll(pfds.as_mut_ptr(), 2, timeout) };
        if pfds[1].revents != 0 {
            feed.drain();
        }
        loop {
            match x.conn.poll_for_event() {
                Ok(Some(ev)) => x.event(ev),
                Ok(None) => break,
                Err(_) => return log::line("overlay: X connection lost"),
            }
        }
    }
}

/// `Xft.dpi` over 96, the desktop's text scale, which desktops also use as the UI scale.
fn xft_scale(conn: &RustConnection, root: Window) -> f64 {
    let Ok(reply) = conn
        .get_property(false, root, AtomEnum::RESOURCE_MANAGER, AtomEnum::STRING, 0, 1 << 16)
        .map_err(|_| ())
        .and_then(|c| c.reply().map_err(|_| ()))
    else {
        return 1.0;
    };
    String::from_utf8_lossy(&reply.value)
        .lines()
        .find_map(|l| l.strip_prefix("Xft.dpi:"))
        .and_then(|v| v.trim().parse::<f64>().ok())
        .map_or(1.0, |dpi| (dpi / 96.0).clamp(1.0, 4.0))
}

impl X {
    fn size(&self) -> (u16, u16) {
        ((render::CARD_W * self.scale).round() as u16, (render::CARD_H * self.scale).round() as u16)
    }

    /// The monitor holding the pointer (else the primary, else the whole screen), shrunk to the
    /// desktop's work area (below GNOME's top bar, say) when that lies on the monitor.
    fn pick_area(&self) -> Area {
        let monitors = self
            .conn
            .randr_get_monitors(self.root, true)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| r.monitors)
            .unwrap_or_default();
        let pointer = self.conn.query_pointer(self.root).ok().and_then(|c| c.reply().ok());
        let inside = |m: &x11rb::protocol::randr::MonitorInfo, px: i32, py: i32| {
            px >= m.x as i32 && px < m.x as i32 + m.width as i32 && py >= m.y as i32 && py < m.y as i32 + m.height as i32
        };
        let chosen = pointer
            .as_ref()
            .and_then(|p| monitors.iter().find(|m| inside(m, p.root_x as i32, p.root_y as i32)))
            .or_else(|| monitors.iter().find(|m| m.primary))
            .or_else(|| monitors.first());
        let Some(m) = chosen else { return self.area };
        let mut area = (m.x as i32, m.y as i32, m.width as i32, m.height as i32);
        if let Some(wa) = self.work_area() {
            let (x0, y0) = (area.0.max(wa.0), area.1.max(wa.1));
            let (x1, y1) = ((area.0 + area.2).min(wa.0 + wa.2), (area.1 + area.3).min(wa.1 + wa.3));
            if x1 > x0 && y1 > y0 {
                area = (x0, y0, x1 - x0, y1 - y0);
            }
        }
        area
    }

    fn work_area(&self) -> Option<Area> {
        let atom = self.conn.intern_atom(false, b"_NET_WORKAREA").ok()?.reply().ok()?.atom;
        let reply =
            self.conn.get_property(false, self.root, atom, AtomEnum::CARDINAL, 0, 4).ok()?.reply().ok()?;
        let v: Vec<i32> = reply.value32()?.map(|n| n as i32).collect();
        (v.len() == 4).then(|| (v[0], v[1], v[2], v[3]))
    }

    /// Top-left of stack slot `slot` in this corner. Top corners stack downward, bottom corners upward.
    fn slot_xy(&self, slot: usize) -> (i32, i32) {
        let (w, h) = self.size();
        let (w, h) = (w as i32, h as i32);
        let (ax, ay, aw, ah) = self.area;
        let m = (MARGIN as f64 * self.scale).round() as i32;
        let off = slot as i32 * (h + (GAP as f64 * self.scale).round() as i32);
        let x = if self.position.is_left() { ax + m + 4 } else { ax + aw - w - m - 4 };
        let y = if self.position.is_top() { ay + m + off } else { ay + ah - h - m - off };
        (x, y)
    }

    fn offer(&mut self, card: Card) {
        match self.stack.offer(&card) {
            Offer::Show => self.open(card),
            Offer::Replace(i) => {
                self.shown[i].tl.replace(card);
                self.shown[i].drawn = None;
            }
            Offer::Queued => {}
        }
    }

    fn open(&mut self, card: Card) {
        if self.shown.is_empty() {
            self.area = self.pick_area();
        }
        let (w, h) = self.size();
        let (x, y) = self.slot_xy(self.shown.len());
        let (Ok(win), Ok(gc)) = (self.conn.generate_id(), self.conn.generate_id()) else { return };
        let aux = CreateWindowAux::new()
            .background_pixel(0)
            .border_pixel(0)
            .override_redirect(1)
            .colormap(self.cmap)
            .event_mask(EventMask::BUTTON_PRESS | EventMask::ENTER_WINDOW | EventMask::LEAVE_WINDOW | EventMask::EXPOSURE);
        let made = self
            .conn
            .create_window(32, win, self.root, x as i16, y as i16, w, h, 0, WindowClass::INPUT_OUTPUT, self.visual, &aux)
            .and_then(|_| self.conn.create_gc(gc, win, &CreateGCAux::new()))
            .and_then(|_| self.conn.map_window(win));
        if let Err(e) = made {
            return log::line(&format!("overlay: create window: {e}"));
        }
        if card.ts > 0 {
            log::line(&format!("latency_ms={}", protocol::now_ms().saturating_sub(card.ts)));
        }
        self.shown.push(Shown { win, gc, tl: Timeline::new(card), drawn: None });
    }

    /// Removes the card at `idx`, promotes a queued card if any, and closes the gap.
    fn remove(&mut self, idx: usize, why: &str) {
        log::line(&format!("dismiss: {why}"));
        let s = self.shown.remove(idx);
        let _ = self.conn.destroy_window(s.win);
        let _ = self.conn.free_gc(s.gc);
        if let Some(next) = self.stack.remove(idx) {
            self.open(next);
        }
        for i in idx..self.shown.len() {
            let (x, y) = self.slot_xy(i);
            let _ = self.conn.configure_window(self.shown[i].win, &ConfigureWindowAux::new().x(x).y(y));
        }
    }

    fn draw(&mut self, i: usize, alpha: f64) {
        let s = &mut self.shown[i];
        let q = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if s.drawn == Some((s.tl.frame, q)) {
            return;
        }
        s.drawn = Some((s.tl.frame, q));
        let (w, h) = ((render::CARD_W * self.scale).round() as u32, (render::CARD_H * self.scale).round() as u32);
        let assets = self.assets.get_or_insert_with(render::Assets::load);
        let pixels = render::render(&s.tl.card, assets, s.tl.frame, w, h, self.scale);
        let bytes = render::fade_bytes(&pixels, q);
        // A request has a size limit, so send the image in bands of rows.
        let row = w as usize * 4;
        let rows_per = ((self.conn.maximum_request_bytes().saturating_sub(64)) / row).clamp(1, h as usize);
        for (n, band) in bytes.chunks(rows_per * row).enumerate() {
            let rows = (band.len() / row) as u16;
            let y = (n * rows_per) as i16;
            let _ = self.conn.put_image(ImageFormat::Z_PIXMAP, s.win, s.gc, w as u16, rows, 0, y, 0, 32, band);
        }
    }

    fn index_of(&self, win: Window) -> Option<usize> {
        self.shown.iter().position(|s| s.win == win)
    }

    fn event(&mut self, ev: Event) {
        match ev {
            Event::ButtonPress(e) if e.detail == 1 => {
                // X has no way to raise another client's window, so a click only dismisses.
                if let Some(i) = self.index_of(e.event) {
                    self.remove(i, "click");
                }
            }
            Event::EnterNotify(e) => {
                if let Some(i) = self.index_of(e.event) {
                    self.shown[i].tl.hover();
                }
            }
            Event::LeaveNotify(e) => {
                if let Some(i) = self.index_of(e.event) {
                    self.shown[i].tl.unhover();
                }
            }
            Event::Expose(e) => {
                if let Some(i) = self.index_of(e.window) {
                    self.shown[i].drawn = None;
                }
            }
            _ => {}
        }
    }
}
