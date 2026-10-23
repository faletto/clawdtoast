//! Software card renderer: rounded card, sprite, status dot, two lines of text.

use crate::config::{Animation, Config, SpriteChoice};
use crate::core::Card;
use crate::protocol::Kind;
use fontdue::{Font, FontSettings};
use std::collections::hash_map::RandomState;
use std::hash::BuildHasher;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const CARD_W: f64 = 320.0;
pub const CARD_H: f64 = 96.0;

// Always bundle Clawd. Personal sprite sheets are loaded from the user's config at runtime.
const SPRITE_PNG: &[u8] = include_bytes!("../assets/clawd.png");
const CODEX_SPRITE_PNG: &[u8] = include_bytes!("../assets/openai-spin.png");
const BAE_SPRITE_PNG: &[u8] = include_bytes!("../assets/bae.png");

// macOS and Linux system fonts. Windows fonts are resolved from the Windows directory below.
// Helvetica.ttc is skipped: fontdue maps U+00B7 to the wrong glyph in it.
const FONT_CANDIDATES: &[(&str, usize)] = &[
    ("/System/Library/Fonts/Supplemental/Arial.ttf", 0),
    ("/Library/Fonts/Arial.ttf", 0),
    // Linux: Debian/Ubuntu, Fedora, Arch layouts.
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
    (
        "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
        0,
    ),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf", 0),
    (
        "/usr/share/fonts/liberation-sans/LiberationSans-Regular.ttf",
        0,
    ),
    ("/usr/share/fonts/google-noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/TTF/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
];
const BOLD_CANDIDATES: &[(&str, usize)] = &[
    ("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 0),
    (
        "/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf",
        0,
    ),
    ("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf", 0),
    ("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf", 0),
    (
        "/usr/share/fonts/liberation-sans/LiberationSans-Bold.ttf",
        0,
    ),
    ("/usr/share/fonts/google-noto/NotoSans-Bold.ttf", 0),
    ("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf", 0),
    ("/usr/share/fonts/noto/NotoSans-Bold.ttf", 0),
];

/// Asks fontconfig for a sans font when none of the well-known paths exist (Linux only).
fn fontconfig_font(style: &str) -> Option<Font> {
    if cfg!(not(target_os = "linux")) {
        return None;
    }
    let out = std::process::Command::new("fc-match")
        .args(["-f", "%{file}", &format!("sans:{style}")])
        .output()
        .ok()?;
    let path = String::from_utf8(out.stdout).ok()?;
    let data = std::fs::read(path.trim()).ok()?;
    Font::from_bytes(data, FontSettings::default()).ok()
}

/// A sheet of square frames laid out left to right; frame size is the image height.
pub struct Sprite {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

pub struct Assets {
    sprite: Option<Sprite>,
    codex_sprite: Option<Sprite>,
    override_both: bool,
    frame_interval: Duration,
    sprite_size: u32,
    hololive: Vec<TimedSprite>,
    regular: Option<Font>,
    bold: Option<Font>,
}

struct TimedSprite {
    sprite: Sprite,
    durations: Vec<Duration>,
}

fn load_font(candidates: &[(&str, usize)]) -> Option<Font> {
    candidates.iter().find_map(|(path, idx)| {
        let data = std::fs::read(path).ok()?;
        let settings = FontSettings {
            collection_index: *idx as u32,
            ..Default::default()
        };
        Font::from_bytes(data, settings).ok()
    })
}

#[cfg(target_os = "windows")]
fn windows_font(bold: bool) -> Option<Font> {
    let root = std::env::var_os("WINDIR").or_else(|| std::env::var_os("SystemRoot"))?;
    let dir = std::path::PathBuf::from(root).join("Fonts");
    let names: &[&str] = if bold {
        &[
            "segoeuib.ttf",
            "arialbd.ttf",
            "tahomabd.ttf",
            "verdanab.ttf",
        ]
    } else {
        &["segoeui.ttf", "arial.ttf", "tahoma.ttf", "verdana.ttf"]
    };
    names.iter().find_map(|name| {
        let data = std::fs::read(dir.join(name)).ok()?;
        Font::from_bytes(data, FontSettings::default()).ok()
    })
}

#[cfg(not(target_os = "windows"))]
fn windows_font(_bold: bool) -> Option<Font> {
    None
}

fn decode_sprite(bytes: &[u8]) -> Result<Sprite, String> {
    let mut reader = png::Decoder::new(bytes)
        .read_info()
        .map_err(|e| e.to_string())?;
    let info = reader.info();
    let (w, h) = (info.width as usize, info.height as usize);
    // Reject malformed or oversized sheets before allocating a decode buffer.
    if h == 0 || h > 512 || w < h || w % h != 0 || w / h > 128 || w * h > 4_194_304 {
        return Err("expected 1-128 square frames in a horizontal strip (max frame 512px)".into());
    }
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).map_err(|e| e.to_string())?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return Err("sprite must be an 8-bit RGBA PNG".into());
    }
    let mut sprite = Sprite {
        w: info.width as usize,
        h: info.height as usize,
        rgba: buf[..info.buffer_size()].to_vec(),
    };
    // Convert partially transparent source pixels to hard pixel-art edges.
    for px in sprite.rgba.chunks_mut(4) {
        px[3] = if px[3] < 128 { 0 } else { 255 };
    }
    Ok(sprite)
}

impl Sprite {
    pub fn frames(&self) -> usize {
        (self.w / self.h).max(1)
    }
}

impl Assets {
    #[cfg(test)]
    pub fn frames(&self) -> usize {
        self.sprite.as_ref().map_or(1, Sprite::frames)
    }

    #[cfg(test)]
    pub fn frame_interval(&self) -> Duration {
        self.frame_interval
    }

    /// Called once when a popup opens or receives a new non-silent alert.
    pub fn choose_sprite(&self) -> usize {
        if self.hololive.is_empty() {
            0
        } else {
            (RandomState::new().hash_one(()) % self.hololive.len() as u64) as usize
        }
    }

    fn sprite_for(&self, card: &Card, variant: usize) -> Option<&Sprite> {
        if let Some(selected) = self.hololive.get(variant) {
            return Some(&selected.sprite);
        }
        if !self.override_both && card.agent == "codex" {
            self.codex_sprite.as_ref().or(self.sprite.as_ref())
        } else {
            self.sprite.as_ref()
        }
    }

    pub fn frames_for(&self, card: &Card, variant: usize) -> usize {
        self.sprite_for(card, variant).map_or(1, Sprite::frames)
    }

    pub fn frame_interval_for(&self, card: &Card) -> Duration {
        if !self.override_both && card.agent == "codex" {
            crate::anim::WAVE_FRAME
        } else {
            self.frame_interval
        }
    }

    pub fn frame_at(
        &self,
        card: &Card,
        variant: usize,
        timing: &crate::anim::Timing,
        now: Instant,
    ) -> usize {
        if let Some(selected) = self.hololive.get(variant) {
            timing.wave_frame_durations(now, &selected.durations)
        } else {
            timing.wave_frame(
                now,
                self.frames_for(card, variant),
                self.frame_interval_for(card),
            )
        }
    }

    pub fn next_wake(
        &self,
        card: &Card,
        variant: usize,
        timing: &crate::anim::Timing,
        now: Instant,
        hold: Duration,
    ) -> Option<Instant> {
        if let Some(selected) = self.hololive.get(variant) {
            timing.next_wake_durations(now, hold, &selected.durations)
        } else {
            timing.next_wake(now, hold, self.frame_interval_for(card))
        }
    }

    fn size_for(&self, card: &Card) -> u32 {
        if !self.override_both && card.agent == "codex" {
            64
        } else {
            self.sprite_size
        }
    }

    pub fn load() -> Assets {
        Assets::load_with_animation(&Config::load().animation)
    }

    fn load_with_animation(animation: &Animation) -> Assets {
        let mut sprite = decode_sprite(SPRITE_PNG).ok();
        let codex_sprite = decode_sprite(CODEX_SPRITE_PNG).ok();
        let mut override_both = false;
        let mut frame_interval = crate::anim::WAVE_FRAME;
        let mut sprite_size = 64;
        let mut hololive = Vec::new();
        if animation.enabled {
            let choice = match animation.sprite {
                SpriteChoice::Auto if animation.file.is_some() => SpriteChoice::Custom,
                other => other,
            };
            let selected = match choice {
                SpriteChoice::Auto => None,
                SpriteChoice::Clawd => decode_sprite(SPRITE_PNG).ok().map(|s| (s, 64)),
                SpriteChoice::Codex => decode_sprite(CODEX_SPRITE_PNG).ok().map(|s| (s, 64)),
                SpriteChoice::Bae => decode_sprite(BAE_SPRITE_PNG)
                    .ok()
                    .map(|s| (s, animation.size)),
                SpriteChoice::Hololive => {
                    let metadata = crate::hololive::metadata();
                    for (name, bytes) in crate::hololive::SHEETS {
                        let sprite = decode_sprite(bytes).expect("bundled Hololive sprite");
                        let entry = metadata
                            .iter()
                            .find(|entry| entry.name == *name)
                            .expect("timing for bundled Hololive sprite");
                        assert_eq!(entry.frame_durations_ms.len(), sprite.frames());
                        assert!(entry.frame_durations_ms.iter().all(|ms| *ms > 0));
                        hololive.push(TimedSprite {
                            sprite,
                            durations: entry
                                .frame_durations_ms
                                .iter()
                                .copied()
                                .map(Duration::from_millis)
                                .collect(),
                        });
                    }
                    override_both = true;
                    sprite_size = animation.size.clamp(32, 88);
                    None
                }
                SpriteChoice::Custom => {
                    if let Some(file) = animation.file.as_deref().filter(|s| !s.trim().is_empty()) {
                        let path = crate::config::expand_home(file);
                        let custom = std::fs::metadata(&path)
                            .map_err(|e| e.to_string())
                            .and_then(|m| {
                                if m.len() > 16 * 1024 * 1024 {
                                    Err("PNG exceeds 16 MiB".to_string())
                                } else {
                                    std::fs::read(&path).map_err(|e| e.to_string())
                                }
                            })
                            .and_then(|bytes| decode_sprite(&bytes));
                        match custom {
                            Ok(custom) => Some((custom, animation.size)),
                            Err(err) => {
                                crate::log::line(&format!(
                                    "renderer: custom sprite {}: {err}; using bundled defaults",
                                    path.display()
                                ));
                                None
                            }
                        }
                    } else {
                        crate::log::line("renderer: custom animation selected without a PNG file");
                        None
                    }
                }
            };
            if let Some((selected, size)) = selected {
                sprite = Some(selected);
                override_both = true;
                sprite_size = size.clamp(32, 88);
                frame_interval = Duration::from_secs_f64(1.0 / animation.fps.clamp(1.0, 60.0));
            }
        }
        let regular = windows_font(false)
            .or_else(|| load_font(FONT_CANDIDATES))
            .or_else(|| fontconfig_font("regular"));
        let bold = windows_font(true)
            .or_else(|| load_font(BOLD_CANDIDATES))
            .or_else(|| fontconfig_font("bold"))
            .or_else(|| windows_font(false))
            .or_else(|| load_font(FONT_CANDIDATES));
        if regular.is_none() && bold.is_none() {
            crate::log::line("renderer: no system fonts found; notification text cannot render");
        }
        Assets {
            sprite,
            codex_sprite,
            override_both,
            frame_interval,
            sprite_size,
            hololive,
            regular,
            bold,
        }
    }
}

fn dot_color(kind: Kind) -> Color {
    match kind {
        Kind::Done => Color::from_rgba8(0x5B, 0xC2, 0x6B, 255),
        Kind::Permission => Color::from_rgba8(0xF2, 0xA9, 0x3B, 255),
        Kind::Idle => Color::from_rgba8(0x6C, 0xA8, 0xF0, 255),
        _ => Color::from_rgba8(0xB0, 0xB0, 0xB0, 255),
    }
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

/// Draws `s` with its baseline at `y`; returns the advance. Coverage is blended over the pixmap.
fn draw_text(
    pm: &mut Pixmap,
    font: &Font,
    s: &str,
    px: f32,
    x: f32,
    y: f32,
    rgb: (u8, u8, u8),
) -> f32 {
    let (pw, ph) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    let mut pen = x;
    for ch in s.chars() {
        let (m, bitmap) = font.rasterize(ch, px);
        let gx = pen.round() as i32 + m.xmin;
        let gy = y.round() as i32 - m.height as i32 - m.ymin;
        for row in 0..m.height {
            for col in 0..m.width {
                let cov = bitmap[row * m.width + col] as u32;
                let (tx, ty) = (gx + col as i32, gy + row as i32);
                if cov == 0 || tx < 0 || ty < 0 || tx >= pw || ty >= ph {
                    continue;
                }
                let i = ((ty * pw + tx) * 4) as usize;
                // Source is opaque text colour scaled by coverage (premultiplied).
                let inv = 255 - cov;
                data[i] = ((rgb.0 as u32 * cov + data[i] as u32 * inv) / 255) as u8;
                data[i + 1] = ((rgb.1 as u32 * cov + data[i + 1] as u32 * inv) / 255) as u8;
                data[i + 2] = ((rgb.2 as u32 * cov + data[i + 2] as u32 * inv) / 255) as u8;
                data[i + 3] = (cov + data[i + 3] as u32 * inv / 255).min(255) as u8;
            }
        }
        pen += m.advance_width;
    }
    pen - x
}

fn text_width(font: &Font, s: &str, px: f32) -> f32 {
    s.chars().map(|c| font.metrics(c, px).advance_width).sum()
}

/// Greedy word wrap into at most `max_lines` lines, ellipsizing the last if text is cut off.
fn wrap(font: &Font, s: &str, px: f32, max_w: f32, max_lines: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in s.split_whitespace() {
        match lines.last_mut() {
            Some(cur) if text_width(font, &format!("{cur} {word}"), px) <= max_w => {
                cur.push(' ');
                cur.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    let cut = lines.len() > max_lines;
    lines.truncate(max_lines);
    let last = lines.len().saturating_sub(1);
    for (i, l) in lines.iter_mut().enumerate() {
        // The last line gets an ellipsis if text was cut; any line wider than the card is hard-cut.
        let needs = (cut && i == last) || text_width(font, l, px) > max_w;
        if needs {
            while !l.is_empty() && text_width(font, &format!("{l}…"), px) > max_w {
                l.pop();
            }
            l.push('…');
        }
    }
    lines
}

/// Renders a card at `scale` using popup `variant` and sheet `frame` (wraps),
/// into `w * h` premultiplied 0xAARRGGBB pixels.
pub fn render(
    card: &Card,
    assets: &Assets,
    variant: usize,
    frame: usize,
    w: u32,
    h: u32,
    scale: f64,
) -> Vec<u32> {
    let mut pm = Pixmap::new(w.max(1), h.max(1)).expect("pixmap");
    let s = scale as f32;
    let (fw, fh) = (w as f32, h as f32);

    if let Some(path) = rounded_rect(0.5 * s, 0.5 * s, fw - s, fh - s, 14.0 * s) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(0x26, 0x24, 0x22, 0xF5));
        paint.anti_alias = true;
        pm.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        paint.set_color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0x26));
        let stroke = Stroke {
            width: s,
            ..Default::default()
        };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    if let Some(sp) = assets.sprite_for(card, variant) {
        // Nearest-neighbour scaling fits any square sprite into its configured
        // logical-pixel area, including fractional display scale factors.
        let side = (assets.size_for(card) as f64 * scale).round().max(1.0) as usize;
        // Keep the art centered in the 96px left column, leaving the text
        // anchored at x=96 even for a larger custom character.
        let ox = ((96.0 * scale).round() as usize).saturating_sub(side) / 2;
        let oy = ((CARD_H * scale).round() as usize).saturating_sub(side) / 2;
        let frame_off = (frame % sp.frames()) * sp.h;
        let (pw, ph) = (pm.width() as usize, pm.height() as usize);
        let data = pm.data_mut();
        for y in 0..side {
            for x in 0..side {
                let si = ((y * sp.h / side) * sp.w + frame_off + x * sp.h / side) * 4;
                let a = sp.rgba[si + 3] as u32;
                let (dx, dy) = (ox + x, oy + y);
                if a == 0 || dx >= pw || dy >= ph {
                    continue;
                }
                let di = (dy * pw + dx) * 4;
                // Blend into the existing card instead of replacing its
                // opaque background with transparent sprite edge pixels.
                let inv = 255 - a;
                for c in 0..3 {
                    data[di + c] =
                        ((sp.rgba[si + c] as u32 * a + data[di + c] as u32 * inv) / 255) as u8;
                }
                data[di + 3] = (a + data[di + 3] as u32 * inv / 255) as u8;
            }
        }
    }

    let text_x = (16.0 + 64.0 + 16.0) * s;
    let max_w = (CARD_W as f32 - 16.0 - 80.0 - 14.0) * s;
    let dot_r = 4.5 * s;
    let title_base = 34.0 * s;
    if let Some(circle) = {
        let mut pb = PathBuilder::new();
        pb.push_circle(text_x + dot_r, title_base - 5.0 * s, dot_r);
        pb.finish()
    } {
        let mut paint = Paint::default();
        paint.set_color(dot_color(card.kind));
        paint.anti_alias = true;
        pm.fill_path(
            &circle,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    let reg = assets.regular.as_ref().or(assets.bold.as_ref());
    let bold = assets.bold.as_ref().or(reg);
    if let (Some(bold), Some(reg)) = (bold, reg) {
        let tx = text_x + dot_r * 2.0 + 7.0 * s;
        // Put the observed count before the project so narrow cards cannot clip
        // it away. The body explicitly labels its uncertainty.
        let mut title = if card.pending_count > 0 {
            format!("{} ({})", card.kind.status(), card.pending_count)
        } else {
            card.title()
        };
        let tw = max_w - (tx - text_x);
        while text_width(bold, &title, 14.0 * s) > tw && title.pop().is_some() {}
        draw_text(
            &mut pm,
            bold,
            &title,
            14.0 * s,
            tx,
            title_base,
            (0xF5, 0xF1, 0xEC),
        );
        let body = if card.pending_count > 0 {
            format!("Unconfirmed · {}: {}", card.project, card.body)
        } else {
            card.body.clone()
        };
        for (i, line) in wrap(reg, &body, 12.5 * s, max_w, 2).iter().enumerate() {
            let y = (54.0 + 17.0 * i as f32) * s;
            draw_text(&mut pm, reg, line, 12.5 * s, text_x, y, (0xC9, 0xC3, 0xBC));
        }
    }

    pm.data()
        .chunks(4)
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect()
}

/// Composites premultiplied 0xAARRGGBB pixels onto the card's solid colour, for X11 screens with
/// no compositor, where a window cannot be transparent.
pub fn flatten(pixels: &mut [u32]) {
    const BG: [u32; 3] = [0x26, 0x24, 0x22];
    for p in pixels {
        let inv = 255 - (*p >> 24);
        let ch = |shift: u32, bg: u32| (((*p >> shift) & 0xFF) + bg * inv / 255).min(255);
        *p = 0xFF00_0000 | ch(16, BG[0]) << 16 | ch(8, BG[1]) << 8 | ch(0, BG[2]);
    }
}

/// Scales premultiplied 0xAARRGGBB pixels by `alpha` (0..=1), for compositors with no window opacity.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn fade(pixels: &mut [u32], alpha: f64) {
    let a = (alpha.clamp(0.0, 1.0) * 256.0) as u32;
    for p in pixels {
        let ch = |shift: u32| ((((*p >> shift) & 0xFF) * a) >> 8).min(255);
        *p = ch(24) << 24 | ch(16) << 16 | ch(8) << 8 | ch(0);
    }
}

#[cfg(test)]
mod tests {
    use super::{fade, flatten};

    #[test]
    fn hololive_plays_every_character_at_source_timing_with_crisp_pixels() {
        use super::{render, Assets, CARD_H, CARD_W};
        use crate::anim::Timing;
        use crate::config::{Animation, Config};
        use std::time::Duration;

        let mut animation = Config::parse(include_str!("../presets/hololive.toml"))
            .unwrap()
            .animation;
        animation.fps = 60.0; // The profile must preserve source timing, regardless of this setting.
        let assets = Assets::load_with_animation(&animation);
        let mut card = crate::core::Card {
            session_id: "hololive".into(),
            kind: crate::protocol::Kind::Done,
            project: "test".into(),
            body: "Done".into(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        // Independent frame counts and loop lengths read from the supplied clips.
        let expected = [
            (30, 3590),
            (30, 3600),
            (10, 1200),
            (30, 3300),
            (40, 4800),
            (30, 3600),
            (30, 3600),
            (30, 3620),
            (30, 3600),
            (32, 3520),
            (30, 3600),
            (30, 3600),
            (30, 3600),
            (30, 3600),
            (40, 4800),
        ];
        assert_eq!(assets.hololive.len(), expected.len());
        let timing = Timing::new();
        let at = |ms| timing.shown_at + Duration::from_millis(ms);
        for (variant, (frames, loop_ms)) in expected.into_iter().enumerate() {
            let sprite = &assets.hololive[variant].sprite;
            assert_eq!(assets.frames_for(&card, variant), frames);
            assert_eq!(
                assets.frame_at(&card, variant, &timing, at(loop_ms - 1)),
                frames - 1
            );
            assert_eq!(assets.frame_at(&card, variant, &timing, at(loop_ms)), 0);
            assert!(sprite.rgba.chunks(4).all(|px| matches!(px[3], 0 | 255)));
            assert!(sprite.rgba.chunks(4).any(|px| px[3] == 0));
            let first = render(
                &card,
                &assets,
                variant,
                0,
                CARD_W as u32,
                CARD_H as u32,
                1.0,
            );
            assert!((1..frames).any(|frame| render(
                &card,
                &assets,
                variant,
                frame,
                CARD_W as u32,
                CARD_H as u32,
                1.0
            ) != first));
            // Both agents must display the selected character, with exactly the same timing.
            card.agent = "codex".into();
            assert_eq!(
                render(
                    &card,
                    &assets,
                    variant,
                    0,
                    CARD_W as u32,
                    CARD_H as u32,
                    1.0
                ),
                first
            );
            card.agent.clear();
            for scale in [1.0, 1.25, 2.0] {
                let (w, h) = ((CARD_W * scale) as u32, (CARD_H * scale) as u32);
                let side = (88.0 * scale).round() as usize;
                let ox = ((96.0 * scale).round() as usize - side) / 2;
                let oy = (h as usize - side) / 2;
                let image = render(&card, &assets, variant, 0, w, h, scale);
                for y in 0..side {
                    for x in 0..side {
                        let si = ((y * sprite.h / side) * sprite.w + x * sprite.h / side) * 4;
                        if sprite.rgba[si + 3] == 255 {
                            let color = 0xff00_0000
                                | (sprite.rgba[si] as u32) << 16
                                | (sprite.rgba[si + 1] as u32) << 8
                                | sprite.rgba[si + 2] as u32;
                            assert_eq!(
                                image[(oy + y) * w as usize + ox + x],
                                color,
                                "Hololive variant {variant} blurred at scale {scale}"
                            );
                        }
                    }
                }
            }
        }
        assert_eq!(assets.frame_at(&card, 0, &timing, at(700)), 6); // Ame's 100 ms frame.
        assert_eq!(assets.frame_at(&card, 7, &timing, at(360)), 2); // Kiara's 150 ms frame.
        assert_eq!(assets.frame_at(&card, 3, &timing, at(110)), 1); // Fauna: 9.09 fps.
        assert_eq!(assets.frame_at(&card, 9, &timing, at(110)), 1); // Lamy: 9.09 fps.
        assert_eq!(
            assets.next_wake(&card, 0, &timing, at(610), Duration::from_secs(5)),
            Some(at(700))
        );

        let choices: std::collections::HashSet<_> =
            (0..256).map(|_| assets.choose_sprite()).collect();
        assert!(
            choices.len() > 1,
            "new popups should not always choose the same character"
        );
        assert!(choices.iter().all(|&i| i < 15));
        let disabled = Assets::load_with_animation(&Animation {
            enabled: false,
            ..animation
        });
        assert!(disabled.hololive.is_empty());
    }

    #[test]
    fn bundled_character_is_transparent_and_animates() {
        use super::{render, Assets, CARD_H, CARD_W};
        use crate::core::Card;
        use crate::protocol::Kind;

        let assets = Assets::load_with_animation(&crate::config::Animation::default());
        assert_eq!(assets.frames(), 4);
        assert_eq!(assets.frame_interval(), crate::anim::WAVE_FRAME);
        assert_eq!(assets.sprite_size, 64);
        let sprite = assets.sprite.as_ref().unwrap();
        assert_eq!(sprite.h, 32);
        assert_eq!(sprite.rgba[3], 0, "background must be transparent");
        assert!(
            sprite.rgba.chunks(4).all(|px| matches!(px[3], 0 | 255)),
            "pixel-art alpha must be binary, with no antialiased edges"
        );

        let card = Card {
            session_id: "test".into(),
            kind: Kind::Done,
            project: "project".into(),
            body: "Done".into(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        let frame_0 = render(&card, &assets, 0, 0, CARD_W as u32, CARD_H as u32, 1.0);
        let frame_1 = render(&card, &assets, 0, 1, CARD_W as u32, CARD_H as u32, 1.0);
        let changed = (16..80).any(|y| {
            (16..80).any(|x| {
                let pos = y * CARD_W as usize + x;
                frame_0[pos] != frame_1[pos]
            })
        });
        assert!(
            changed,
            "the notification must display distinct character frames"
        );

        // The sprite uses nearest-neighbour sampling at every DPI. Every
        // visible destination pixel must exactly match an original source
        // pixel, with no interpolated colours or semitransparent borders.
        for scale in [1.0_f64, 1.25, 2.0] {
            let w = (CARD_W * scale) as u32;
            let h = (CARD_H * scale) as u32;
            let drawn = render(&card, &assets, 0, 0, w, h, scale);
            let side = (64.0 * scale).round() as usize;
            let ox = (16.0 * scale as f32) as usize;
            let oy = (h as usize - side) / 2;
            for y in 0..side {
                for x in 0..side {
                    let si = ((y * sprite.h / side) * sprite.w + x * sprite.h / side) * 4;
                    if sprite.rgba[si + 3] != 255 {
                        continue;
                    }
                    let pixel = drawn[(oy + y) * w as usize + ox + x];
                    let expected = 0xff00_0000
                        | (sprite.rgba[si] as u32) << 16
                        | (sprite.rgba[si + 1] as u32) << 8
                        | sprite.rgba[si + 2] as u32;
                    assert_eq!(
                        pixel, expected,
                        "sprite changed at scale {scale}, ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn custom_sprite_loads_and_missing_file_falls_back_to_clawd() {
        use super::Assets;
        use crate::config::Animation;
        use std::time::Duration;

        let path = std::env::temp_dir().join(format!(
            "clawdtoast-sprite-{}-{}.png",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer
                .write_image_data(&[255, 0, 0, 255, 0, 255, 0, 100, 0, 0, 255, 190, 0, 0, 0, 0])
                .unwrap();
        }
        std::fs::write(&path, &bytes).unwrap();
        let custom = Animation {
            enabled: true,
            sprite: crate::config::SpriteChoice::Auto,
            file: Some(path.to_string_lossy().into()),
            fps: 12.5,
            size: 88,
        };
        let assets = Assets::load_with_animation(&custom);
        assert_eq!(assets.frames(), 1);
        assert_eq!(assets.frame_interval(), Duration::from_millis(80));
        assert_eq!(assets.sprite_size, 88);
        let card = crate::core::Card {
            session_id: "test".into(),
            kind: crate::protocol::Kind::Done,
            project: "test".into(),
            body: "Done".into(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        // A large custom sprite retains its original opaque pixel colors even
        // at fractional DPI, with no interpolated or semitransparent edges.
        for scale in [1.0_f64, 1.25, 2.0] {
            let w = (super::CARD_W * scale) as u32;
            let h = (super::CARD_H * scale) as u32;
            let image = super::render(&card, &assets, 0, 0, w, h, scale);
            let side = (88.0 * scale).round() as usize;
            let ox = ((96.0 * scale).round() as usize - side) / 2;
            let oy = (h as usize - side) / 2;
            // Top-left source pixel is pure red and fully opaque.
            let pos = (oy + side / 8) * w as usize + ox + side / 8;
            assert_eq!(
                image[pos], 0xffff_0000,
                "custom sprite blurred at scale {scale}"
            );
        }
        let pixels = &assets.sprite.unwrap().rgba;
        assert_eq!(
            (pixels[3], pixels[7], pixels[11], pixels[15]),
            (255, 0, 255, 0)
        );

        std::fs::remove_file(&path).unwrap();
        let fallback = Assets::load_with_animation(&custom);
        assert_eq!(fallback.frames(), 4);
        assert_eq!(fallback.frame_interval(), crate::anim::WAVE_FRAME);
        assert_eq!(fallback.sprite_size, 64);
        let disabled = Assets::load_with_animation(&Animation {
            enabled: false,
            ..custom
        });
        assert_eq!(disabled.frames(), 4);
        assert_eq!(disabled.sprite_size, 64);
    }

    #[test]
    fn rejects_non_square_sprite_sheets() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 3, 2);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder
                .write_header()
                .unwrap()
                .write_image_data(&[0; 24])
                .unwrap();
        }
        assert!(super::decode_sprite(&bytes).is_err());
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn windows_fonts_render_notification_body() {
        use super::{render, Assets, CARD_H, CARD_W};
        use crate::core::Card;
        use crate::protocol::Kind;

        let assets = Assets::load();
        assert!(assets.regular.is_some(), "Windows regular font should load");
        assert!(assets.bold.is_some(), "Windows bold font should load");

        let mut card = Card {
            session_id: "test".into(),
            kind: Kind::Done,
            project: "project".into(),
            body: String::new(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        let without_body = render(&card, &assets, 0, 0, CARD_W as u32, CARD_H as u32, 1.0);
        card.body = "Finished the Codex task".into();
        let with_body = render(&card, &assets, 0, 0, CARD_W as u32, CARD_H as u32, 1.0);

        // Restrict to the body area so sprite, title, and status dot cannot satisfy the test.
        let body_changed = (42..89).any(|y| {
            (96..310).any(|x| {
                let pos = y * CARD_W as usize + x;
                without_body[pos] != with_body[pos]
            })
        });
        assert!(
            body_changed,
            "Notification body must affect rendered pixels"
        );
    }

    #[test]
    fn codex_cards_use_the_openai_sprite_sheet() {
        use super::{render, Assets, CARD_H, CARD_W};
        use crate::core::Card;
        use crate::protocol::Kind;

        let assets = Assets::load_with_animation(&crate::config::Animation::default());
        let sheet = assets.codex_sprite.as_ref().expect("codex sprite embedded");
        assert_eq!((sheet.w, sheet.h, sheet.frames()), (128, 32, 4));

        let mut card = Card {
            session_id: "t".into(),
            kind: Kind::Done,
            project: "p".into(),
            body: String::new(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        let draw = |c: &Card, f| render(c, &assets, 0, f, CARD_W as u32, CARD_H as u32, 1.0);
        let claude = draw(&card, 0);
        card.agent = "codex".into();
        assert_eq!(assets.frames_for(&card, 0), 4);
        assert_eq!(assets.frame_interval_for(&card), crate::anim::WAVE_FRAME);
        let codex: Vec<Vec<u32>> = (0..4).map(|f| draw(&card, f)).collect();
        assert_ne!(claude, codex[0], "codex card must not use the Clawd sprite");
        for f in 1..4 {
            assert_ne!(
                codex[0], codex[f],
                "frame {f} must differ so the logo spins"
            );
        }
    }

    #[test]
    fn bae_and_named_sprites_override_both_agents_only_when_enabled() {
        use super::{render, Assets, CARD_H, CARD_W};
        use crate::config::{Animation, SpriteChoice};
        use crate::core::Card;
        use crate::protocol::Kind;
        use std::time::Duration;

        let mut card = Card {
            session_id: "test".into(),
            kind: Kind::Done,
            project: "project".into(),
            body: "Done".into(),
            badge: None,
            ts: 0,
            pid: 0,
            app: String::new(),
            agent: String::new(),
            pending_count: 0,
            expires_at: 0,
            silent: false,
            dismiss: false,
        };
        let defaults = Assets::load_with_animation(&Animation::default());
        let bae_config = Animation {
            enabled: true,
            sprite: SpriteChoice::Bae,
            fps: 11.0,
            ..Animation::default()
        };
        let bae = Assets::load_with_animation(&bae_config);
        assert_eq!(bae.frames_for(&card, 0), 11);
        assert_eq!(bae.sprite_size, 88);
        assert_eq!(
            bae.frame_interval_for(&card),
            Duration::from_secs_f64(1.0 / 11.0)
        );
        assert_eq!(
            (
                bae.sprite.as_ref().unwrap().w,
                bae.sprite.as_ref().unwrap().h
            ),
            (704, 64)
        );
        assert!(bae
            .sprite
            .as_ref()
            .unwrap()
            .rgba
            .chunks(4)
            .all(|px| px[3] == 0 || px[3] == 255));
        let draw = |assets: &Assets, card: &Card, frame| {
            render(card, assets, 0, frame, CARD_W as u32, CARD_H as u32, 1.0)
        };
        assert_ne!(draw(&defaults, &card, 0), draw(&bae, &card, 0));
        assert_ne!(draw(&bae, &card, 0), draw(&bae, &card, 1));

        card.agent = "codex".into();
        assert_eq!(bae.frames_for(&card, 0), 11);
        assert_eq!(
            bae.frame_interval_for(&card),
            Duration::from_secs_f64(1.0 / 11.0)
        );
        assert_ne!(draw(&defaults, &card, 0), draw(&bae, &card, 0));

        let codex_for_all = Assets::load_with_animation(&Animation {
            sprite: SpriteChoice::Codex,
            ..bae_config.clone()
        });
        assert_eq!(codex_for_all.frames_for(&card, 0), 4);
        assert_eq!(codex_for_all.size_for(&card), 64);
        assert_eq!(draw(&codex_for_all, &card, 0), draw(&defaults, &card, 0));
        card.agent.clear();
        assert_eq!(
            draw(&codex_for_all, &card, 0),
            draw(
                &defaults,
                &Card {
                    agent: "codex".into(),
                    pending_count: 0,
                    expires_at: 0,
                    silent: false,
                    dismiss: false,
                    ..card.clone()
                },
                0
            )
        );

        let disabled = Assets::load_with_animation(&Animation {
            enabled: false,
            ..bae_config
        });
        assert_eq!(disabled.frames_for(&card, 0), 4);
        assert_eq!(draw(&disabled, &card, 0), draw(&defaults, &card, 0));
    }

    #[test]
    fn fade_scales_every_premultiplied_channel() {
        let mut px = [0xFF80_4020, 0x1111_1111];
        fade(&mut px, 1.0);
        assert_eq!(px[0], 0xFF80_4020);
        fade(&mut px, 0.5);
        assert_eq!(px[0], 0x7F40_2010);
        fade(&mut px, 0.0);
        assert_eq!(px, [0, 0]);
    }

    #[test]
    fn flatten_fills_transparent_with_card_colour_and_keeps_opaque() {
        let mut px = [0x0000_0000, 0xFFAA_BBCC];
        flatten(&mut px);
        assert_eq!(px, [0xFF26_2422, 0xFFAA_BBCC]);
    }
}
