//! Software card renderer: rounded card, sprite, status dot, two lines of text.

use crate::core::Card;
use crate::protocol::Kind;
use fontdue::{Font, FontSettings};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Stroke, Transform};

pub const CARD_W: f64 = 320.0;
pub const CARD_H: f64 = 96.0;

const SPRITE_PNG: &[u8] = include_bytes!("../assets/clawd.png");

// System fonts for M0 (embedding an open-licensed subset is an M1 item). Helvetica.ttc is
// skipped: fontdue maps U+00B7 to the wrong glyph in it.
const FONT_CANDIDATES: &[(&str, usize)] = &[
    ("/System/Library/Fonts/Supplemental/Arial.ttf", 0),
    ("/Library/Fonts/Arial.ttf", 0),
    // Common Linux locations (Alpine/Arch, Debian/Ubuntu, Fedora).
    ("/usr/share/fonts/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSans-Regular.ttf", 0),
    ("/usr/share/fonts/dejavu/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/TTF/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans.ttf", 0),
    ("/usr/share/fonts/liberation/LiberationSans-Regular.ttf", 0),
    ("/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf", 0),
];
const BOLD_CANDIDATES: &[(&str, usize)] = &[
    ("/System/Library/Fonts/Supplemental/Arial Bold.ttf", 0),
    ("/usr/share/fonts/noto/NotoSans-Bold.ttf", 0),
    ("/usr/share/fonts/truetype/noto/NotoSans-Bold.ttf", 0),
    ("/usr/share/fonts/dejavu/DejaVuSans-Bold.ttf", 0),
    ("/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf", 0),
    ("/usr/share/fonts/TTF/DejaVuSans-Bold.ttf", 0),
    ("/usr/share/fonts/dejavu-sans-fonts/DejaVuSans-Bold.ttf", 0),
    ("/usr/share/fonts/liberation/LiberationSans-Bold.ttf", 0),
    ("/usr/share/fonts/truetype/liberation/LiberationSans-Bold.ttf", 0),
];

/// A sheet of square frames laid out left to right; frame size is the image height.
pub struct Sprite {
    w: usize,
    h: usize,
    rgba: Vec<u8>,
}

pub struct Assets {
    sprite: Option<Sprite>,
    regular: Option<Font>,
    bold: Option<Font>,
}

fn load_font(candidates: &[(&str, usize)]) -> Option<Font> {
    candidates.iter().find_map(|(path, idx)| {
        let data = std::fs::read(path).ok()?;
        let settings = FontSettings { collection_index: *idx as u32, ..Default::default() };
        Font::from_bytes(data, settings).ok()
    })
}

fn load_sprite() -> Option<Sprite> {
    let mut reader = png::Decoder::new(SPRITE_PNG).read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    (info.color_type == png::ColorType::Rgba && info.bit_depth == png::BitDepth::Eight).then(|| Sprite {
        w: info.width as usize,
        h: info.height as usize,
        rgba: buf[..info.buffer_size()].to_vec(),
    })
}

impl Sprite {
    pub fn frames(&self) -> usize {
        (self.w / self.h).max(1)
    }
}

impl Assets {
    pub fn frames(&self) -> usize {
        self.sprite.as_ref().map_or(1, Sprite::frames)
    }

    pub fn load() -> Assets {
        let regular = load_font(FONT_CANDIDATES);
        let bold = load_font(BOLD_CANDIDATES).or_else(|| load_font(FONT_CANDIDATES));
        Assets { sprite: load_sprite(), regular, bold }
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
fn draw_text(pm: &mut Pixmap, font: &Font, s: &str, px: f32, x: f32, y: f32, rgb: (u8, u8, u8)) -> f32 {
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

/// Renders a card at `scale`, showing sprite sheet frame `frame` (wraps). into premultiplied 0xAARRGGBB pixels (`w * h` of them).
pub fn render(card: &Card, assets: &Assets, frame: usize, w: u32, h: u32, scale: f64) -> Vec<u32> {
    let mut pm = Pixmap::new(w.max(1), h.max(1)).expect("pixmap");
    let s = scale as f32;
    let (fw, fh) = (w as f32, h as f32);

    if let Some(path) = rounded_rect(0.5 * s, 0.5 * s, fw - s, fh - s, 14.0 * s) {
        let mut paint = Paint::default();
        paint.set_color(Color::from_rgba8(0x26, 0x24, 0x22, 0xF5));
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        paint.set_color(Color::from_rgba8(0xFF, 0xFF, 0xFF, 0x26));
        let stroke = Stroke { width: s, ..Default::default() };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }

    if let Some(sp) = &assets.sprite {
        // Whole-number upscale keeps the pixel art crisp.
        let k = ((2.0 * scale).round() as usize).max(1);
        let (ox, oy) = ((16.0 * s) as usize, ((CARD_H as f32 * s) as usize).saturating_sub(sp.h * k) / 2);
        let frame_off = (frame % sp.frames()) * sp.h;
        let (pw, ph) = (pm.width() as usize, pm.height() as usize);
        let data = pm.data_mut();
        for y in 0..sp.h * k {
            for x in 0..sp.h * k {
                let si = ((y / k) * sp.w + frame_off + x / k) * 4;
                let a = sp.rgba[si + 3] as u32;
                let (dx, dy) = (ox + x, oy + y);
                if a == 0 || dx >= pw || dy >= ph {
                    continue;
                }
                let di = (dy * pw + dx) * 4;
                for c in 0..3 {
                    data[di + c] = (sp.rgba[si + c] as u32 * a / 255) as u8;
                }
                data[di + 3] = a as u8;
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
        pm.fill_path(&circle, &paint, FillRule::Winding, Transform::identity(), None);
    }

    if let (Some(bold), Some(reg)) = (&assets.bold, &assets.regular) {
        let tx = text_x + dot_r * 2.0 + 7.0 * s;
        let mut title = card.title();
        let tw = max_w - (tx - text_x);
        while text_width(bold, &title, 14.0 * s) > tw && title.pop().is_some() {}
        draw_text(&mut pm, bold, &title, 14.0 * s, tx, title_base, (0xF5, 0xF1, 0xEC));
        for (i, line) in wrap(reg, &card.body, 12.5 * s, max_w, 2).iter().enumerate() {
            let y = (54.0 + 17.0 * i as f32) * s;
            draw_text(&mut pm, reg, line, 12.5 * s, text_x, y, (0xC9, 0xC3, 0xBC));
        }
    }

    pm.data()
        .chunks_exact(4)
        .map(|p| (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect()
}

/// Pixels as bytes for a little-endian ARGB8888 buffer, faded to `q`/255 opacity. The pixels are
/// premultiplied, so fading is scaling every channel.
#[cfg(target_os = "linux")]
pub fn fade_bytes(pixels: &[u32], q: u8) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(pixels.len() * 4);
    for &p in pixels {
        let c = |sh: u32| (((p >> sh) & 0xff) * q as u32 / 255) << sh;
        bytes.extend_from_slice(&(c(24) | c(16) | c(8) | c(0)).to_le_bytes());
    }
    bytes
}
