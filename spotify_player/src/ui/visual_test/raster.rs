//! Turns a rendered ratatui `Buffer` into something comparable: a PNG for buffers that hold the
//! whole picture (text + halfblock images) and a structural text dump for Kitty placeholders.

use font8x8::UnicodeFonts;
use image::{Rgba, RgbaImage};
use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier},
};

pub const CELL_W: u32 = 8;
pub const CELL_H: u32 = 16;

const DEFAULT_FG: [u8; 3] = [0xf8, 0xf8, 0xf2];
const DEFAULT_BG: [u8; 3] = [0x28, 0x2a, 0x36];

/// A pixel counts as changed when any channel moves by more than this.
const CHANNEL_TOLERANCE: u8 = 24;

const KITTY_PLACEHOLDER: char = '\u{10EEEE}';

/// Leading entries of kitty's `rowcolumn-diacritics.txt`, enough to name the rows used in tests.
const KITTY_DIACRITICS: [char; 40] = [
    '\u{305}', '\u{30D}', '\u{30E}', '\u{310}', '\u{312}', '\u{33D}', '\u{33E}', '\u{33F}',
    '\u{346}', '\u{34A}', '\u{34B}', '\u{34C}', '\u{350}', '\u{351}', '\u{352}', '\u{357}',
    '\u{35B}', '\u{363}', '\u{364}', '\u{365}', '\u{366}', '\u{367}', '\u{368}', '\u{369}',
    '\u{36A}', '\u{36B}', '\u{36C}', '\u{36D}', '\u{36E}', '\u{36F}', '\u{483}', '\u{484}',
    '\u{485}', '\u{486}', '\u{487}', '\u{592}', '\u{593}', '\u{594}', '\u{595}', '\u{597}',
];

pub fn rasterize(buf: &Buffer) -> RgbaImage {
    let area = buf.area;
    let mut img = RgbaImage::new(
        u32::from(area.width) * CELL_W,
        u32::from(area.height) * CELL_H,
    );
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(area.x + x, area.y + y)];
            let mut fg = resolve(cell.fg, DEFAULT_FG);
            let mut bg = resolve(cell.bg, DEFAULT_BG);
            if cell.modifier.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let (ox, oy) = (u32::from(x) * CELL_W, u32::from(y) * CELL_H);
            fill(&mut img, ox, oy, CELL_W, CELL_H, bg);
            draw_symbol(&mut img, ox, oy, cell.symbol(), fg);
        }
    }
    img
}

fn resolve(color: Color, default: [u8; 3]) -> [u8; 3] {
    match color {
        Color::Reset => default,
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(i) => xterm_256(i),
        Color::Black => xterm_256(0),
        Color::Red => xterm_256(1),
        Color::Green => xterm_256(2),
        Color::Yellow => xterm_256(3),
        Color::Blue => xterm_256(4),
        Color::Magenta => xterm_256(5),
        Color::Cyan => xterm_256(6),
        Color::Gray => xterm_256(7),
        Color::DarkGray => xterm_256(8),
        Color::LightRed => xterm_256(9),
        Color::LightGreen => xterm_256(10),
        Color::LightYellow => xterm_256(11),
        Color::LightBlue => xterm_256(12),
        Color::LightMagenta => xterm_256(13),
        Color::LightCyan => xterm_256(14),
        Color::White => xterm_256(15),
    }
}

fn xterm_256(i: u8) -> [u8; 3] {
    const BASE: [[u8; 3]; 16] = [
        [0, 0, 0],
        [205, 0, 0],
        [0, 205, 0],
        [205, 205, 0],
        [0, 0, 238],
        [205, 0, 205],
        [0, 205, 205],
        [229, 229, 229],
        [127, 127, 127],
        [255, 0, 0],
        [0, 255, 0],
        [255, 255, 0],
        [92, 92, 255],
        [255, 0, 255],
        [0, 255, 255],
        [255, 255, 255],
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match i {
        0..=15 => BASE[i as usize],
        16..=231 => {
            let i = i - 16;
            [
                LEVELS[(i / 36) as usize],
                LEVELS[(i / 6 % 6) as usize],
                LEVELS[(i % 6) as usize],
            ]
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            [v, v, v]
        }
    }
}

fn fill(img: &mut RgbaImage, x: u32, y: u32, w: u32, h: u32, c: [u8; 3]) {
    for py in y..(y + h).min(img.height()) {
        for px in x..(x + w).min(img.width()) {
            img.put_pixel(px, py, Rgba([c[0], c[1], c[2], 255]));
        }
    }
}

fn draw_symbol(img: &mut RgbaImage, ox: u32, oy: u32, symbol: &str, fg: [u8; 3]) {
    let mut chars = symbol.chars();
    let Some(ch) = chars.next() else { return };
    let (w, h) = (CELL_W, CELL_H);
    let (mx, my) = (w / 2 - 1, h / 2 - 1);
    let hline = |img: &mut RgbaImage, x0: u32, x1: u32| fill(img, ox + x0, oy + my, x1 - x0, 2, fg);
    let vline = |img: &mut RgbaImage, y0: u32, y1: u32| fill(img, ox + mx, oy + y0, 2, y1 - y0, fg);
    match ch {
        ' ' => {}
        '█' => fill(img, ox, oy, w, h, fg),
        '▀' => fill(img, ox, oy, w, h / 2, fg),
        '▄' => fill(img, ox, oy + h / 2, w, h / 2, fg),
        '▂' => fill(img, ox, oy + h - h / 4, w, h / 4, fg),
        '🮂' => fill(img, ox, oy, w, h / 4, fg),
        '▎' => fill(img, ox, oy, w / 4, h, fg),
        '🮇' => fill(img, ox + w - w / 4, oy, w / 4, h, fg),
        '─' | '━' | '═' => hline(img, 0, w),
        '│' | '┃' | '║' => vline(img, 0, h),
        '┌' | '╭' | '╔' | '┏' => {
            hline(img, mx, w);
            vline(img, my, h);
        }
        '┐' | '╮' | '╗' | '┓' => {
            hline(img, 0, mx + 2);
            vline(img, my, h);
        }
        '└' | '╰' | '╚' | '┗' => {
            hline(img, mx, w);
            vline(img, 0, my + 2);
        }
        '┘' | '╯' | '╝' | '┛' => {
            hline(img, 0, mx + 2);
            vline(img, 0, my + 2);
        }
        _ => {
            if let Some(glyph) = font8x8::BASIC_FONTS.get(ch) {
                for (row, bits) in glyph.iter().enumerate() {
                    for col in 0..8 {
                        if bits & (1 << col) != 0 {
                            fill(img, ox + col, oy + row as u32 * 2, 1, 2, fg);
                        }
                    }
                }
            } else {
                // unknown glyph: outline the cell so it's still visible in the picture
                fill(img, ox + 1, oy + 1, w - 2, 1, fg);
                fill(img, ox + 1, oy + h - 2, w - 2, 1, fg);
                fill(img, ox + 1, oy + 1, 1, h - 2, fg);
                fill(img, ox + w - 2, oy + 1, 1, h - 2, fg);
            }
        }
    }
}

/// Fraction of differing pixels plus a diff image (actual, dimmed, with changes in red).
pub fn compare(golden: &RgbaImage, actual: &RgbaImage) -> (f64, RgbaImage) {
    if golden.dimensions() != actual.dimensions() {
        return (1.0, actual.clone());
    }
    let mut diff = RgbaImage::new(actual.width(), actual.height());
    let mut changed = 0u64;
    for (x, y, a) in actual.enumerate_pixels() {
        let g = golden.get_pixel(x, y);
        let differs = (0..3).any(|c| a[c].abs_diff(g[c]) > CHANNEL_TOLERANCE);
        let px = if differs {
            changed += 1;
            Rgba([255, 0, 0, 255])
        } else {
            let l = ((u32::from(a[0]) + u32::from(a[1]) + u32::from(a[2])) / 9) as u8;
            Rgba([l, l, l, 255])
        };
        diff.put_pixel(x, y, px);
    }
    (
        changed as f64 / f64::from(actual.width() * actual.height()),
        diff,
    )
}

/// Lay images out left to right with a gap, for side-by-side review.
pub fn side_by_side(images: &[&RgbaImage]) -> RgbaImage {
    const GAP: u32 = 16;
    let width = images.iter().map(|i| i.width()).sum::<u32>() + GAP * (images.len() as u32 - 1);
    let height = images.iter().map(|i| i.height()).max().unwrap_or(0);
    let mut out = RgbaImage::from_pixel(width, height, Rgba([255, 0, 255, 255]));
    let mut x = 0;
    for img in images {
        image::imageops::replace(&mut out, *img, i64::from(x), 0);
        x += img.width() + GAP;
    }
    out
}

/// Remove kitty graphics commands (`ESC _ G ... ESC \`) from a cell symbol.
fn strip_apc(symbol: &str) -> String {
    let mut out = String::new();
    let mut rest = symbol;
    while let Some(start) = rest.find("\x1b_G") {
        out.push_str(&rest[..start]);
        match rest[start..].find("\x1b\\") {
            Some(end) => rest = &rest[start + end + 2..],
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

/// Number of kitty transmit commands (`a=T`) and total graphics-command bytes in a buffer.
pub fn kitty_transmits(buf: &Buffer) -> (usize, usize) {
    buf.content.iter().fold((0, 0), |(n, bytes), cell| {
        let s = cell.symbol();
        let apc_bytes = s.len() - strip_apc(s).len();
        (n + s.matches("a=T,").count(), bytes + apc_bytes)
    })
}

/// Structural dump of a buffer rendered with the Kitty protocol.
///
/// Placeholder runs become `[#<cover> r<row> w<width>]`, where `<cover>` numbers image ids in
/// order of first appearance (ids are random) and `<row>` is the image row shown on that line.
pub fn kitty_snapshot(buf: &Buffer) -> String {
    let mut ids: Vec<(u32, Option<char>)> = Vec::new();
    let area = buf.area;
    let mut out = String::new();
    for y in 0..area.height {
        let mut line = String::new();
        let mut x = 0;
        while x < area.width {
            let cell = &buf[(area.x + x, area.y + y)];
            let symbol = strip_apc(cell.symbol());
            if let Some((id, row, width)) = parse_placeholder_run(&symbol, cell.fg) {
                let slot = ids.iter().position(|i| *i == id).unwrap_or_else(|| {
                    ids.push(id);
                    ids.len() - 1
                });
                let row = KITTY_DIACRITICS
                    .iter()
                    .position(|d| *d == row)
                    .map_or_else(|| format!("?{:X}", row as u32), |r| r.to_string());
                line.push_str(&format!("[#{slot} r{row} w{width}]"));
                x += width.max(1);
            } else {
                line.push_str(&symbol);
                x += 1;
            }
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// Parse a cell holding a run of kitty unicode placeholders.
///
/// Returns `((id, id_extra_diacritic), row_diacritic, run_width)`.
fn parse_placeholder_run(symbol: &str, fg: Color) -> Option<((u32, Option<char>), char, u16)> {
    let first = symbol.find(KITTY_PLACEHOLDER)?;
    let id = parse_sgr_rgb(&symbol[..first])
        .or(match fg {
            Color::Rgb(r, g, b) => Some((r, g, b)),
            _ => None,
        })
        .map(|(r, g, b)| (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b))?;
    let mut after = symbol[first..].chars().skip(1);
    let row = after.next()?;
    let _col = after.next();
    let extra = after.next().filter(|c| *c != KITTY_PLACEHOLDER);
    let width = symbol.matches(KITTY_PLACEHOLDER).count() as u16;
    Some(((id, extra), row, width))
}

fn parse_sgr_rgb(s: &str) -> Option<(u8, u8, u8)> {
    let start = s.rfind("\x1b[38;2;")? + "\x1b[38;2;".len();
    let end = start + s[start..].find('m')?;
    let mut parts = s[start..end].split(';').map(str::parse::<u8>);
    Some((
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    ))
}
