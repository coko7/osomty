//! Braille-dot raster canvas: each terminal cell holds 2x4 "pixels".
//! Area fills are rendered as cell background colours, lines as coloured braille dots,
//! and labels are placed on top with simple collision detection.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use unicode_width::UnicodeWidthStr;

const BRAILLE_BITS: [[u8; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// Pixel-space clip rectangle (x0/y0 inclusive, x1/y1 exclusive).
#[derive(Clone, Copy, Debug)]
pub struct Clip {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Clip {
    pub fn intersect(self, o: Clip) -> Option<Clip> {
        let c = Clip {
            x0: self.x0.max(o.x0),
            y0: self.y0.max(o.y0),
            x1: self.x1.min(o.x1),
            y1: self.y1.min(o.y1),
        };
        (c.x0 < c.x1 && c.y0 < c.y1).then_some(c)
    }

    pub fn contains(&self, x: f64, y: f64) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

struct Label {
    col: u16,
    row: u16,
    text: String,
    style: Style,
}

pub struct Canvas {
    pub cols: u16,
    pub rows: u16,
    dots: Vec<u8>,
    fg: Vec<Color>,
    fill: Vec<u8>,
    bg: Vec<Color>,
    occupied: Vec<bool>,
    labels: Vec<Label>,
}

impl Canvas {
    pub fn new(cols: u16, rows: u16) -> Self {
        let n = cols as usize * rows as usize;
        Canvas {
            cols,
            rows,
            dots: vec![0; n],
            fg: vec![Color::Reset; n],
            fill: vec![0; n],
            bg: vec![Color::Reset; n],
            occupied: vec![false; n],
            labels: Vec::new(),
        }
    }

    pub fn width_px(&self) -> f64 {
        self.cols as f64 * 2.0
    }

    pub fn height_px(&self) -> f64 {
        self.rows as f64 * 4.0
    }

    pub fn bounds(&self) -> Clip {
        Clip {
            x0: 0.0,
            y0: 0.0,
            x1: self.width_px(),
            y1: self.height_px(),
        }
    }

    fn cell(&self, x: i64, y: i64) -> Option<(usize, u8)> {
        if x < 0 || y < 0 {
            return None;
        }
        let (col, row) = (x / 2, y / 4);
        if col >= self.cols as i64 || row >= self.rows as i64 {
            return None;
        }
        let idx = row as usize * self.cols as usize + col as usize;
        Some((idx, BRAILLE_BITS[(y % 4) as usize][(x % 2) as usize]))
    }

    fn dot(&mut self, x: i64, y: i64, color: Color) {
        if let Some((idx, bit)) = self.cell(x, y) {
            self.dots[idx] |= bit;
            self.fg[idx] = color;
        }
    }

    pub fn line(&mut self, a: (f64, f64), b: (f64, f64), clip: &Clip, color: Color) {
        let Some(((x0, y0), (x1, y1))) = clip_segment(a, b, clip) else {
            return;
        };
        let steps = (x1 - x0).abs().max((y1 - y0).abs()).ceil().max(1.0);
        let (dx, dy) = ((x1 - x0) / steps, (y1 - y0) / steps);
        for i in 0..=steps as i64 {
            let (x, y) = (x0 + dx * i as f64, y0 + dy * i as f64);
            if clip.contains(x, y) {
                self.dot(x.floor() as i64, y.floor() as i64, color);
            }
        }
    }

    pub fn polyline(&mut self, pts: &[(f64, f64)], clip: &Clip, color: Color) {
        for w in pts.windows(2) {
            self.line(w[0], w[1], clip, color);
        }
    }

    /// Even-odd scanline fill; the result becomes the background colour of covered cells.
    pub fn fill_polygon(&mut self, rings: &[Vec<(f64, f64)>], clip: &Clip, color: Color) {
        let (mut ymin, mut ymax) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut xmin, mut xmax) = (f64::INFINITY, f64::NEG_INFINITY);
        let mut edges = Vec::new();
        for ring in rings {
            for w in ring.windows(2) {
                let ((ax, ay), (bx, by)) = (w[0], w[1]);
                ymin = ymin.min(ay);
                ymax = ymax.max(ay);
                xmin = xmin.min(ax);
                xmax = xmax.max(ax);
                if ay != by {
                    edges.push((ax, ay, bx, by));
                }
            }
        }
        if edges.is_empty() || xmax < clip.x0 || xmin >= clip.x1 {
            return;
        }
        let row_start = ymin.max(clip.y0).floor() as i64;
        let row_end = ymax.min(clip.y1).ceil() as i64;
        let col_lo = clip.x0.ceil() as i64;
        let col_hi = clip.x1.ceil() as i64; // exclusive
        let mut xs = Vec::new();
        for y in row_start..row_end {
            let sy = y as f64 + 0.5;
            if sy < clip.y0 || sy >= clip.y1 {
                continue;
            }
            xs.clear();
            for &(ax, ay, bx, by) in &edges {
                if (sy >= ay && sy < by) || (sy >= by && sy < ay) {
                    xs.push(ax + (sy - ay) * (bx - ax) / (by - ay));
                }
            }
            xs.sort_unstable_by(f64::total_cmp);
            for pair in xs.as_chunks::<2>().0 {
                let from = ((pair[0] - 0.5).ceil() as i64).max(col_lo);
                let to = ((pair[1] - 0.5).ceil() as i64).min(col_hi);
                for x in from..to {
                    if let Some((idx, bit)) = self.cell(x, y) {
                        self.fill[idx] |= bit;
                        self.bg[idx] = color;
                    }
                }
            }
        }
    }

    /// Place a label at a cell position unless it collides with an existing one.
    pub fn label(&mut self, col: i64, row: i64, text: &str, style: Style) -> bool {
        let width = text.width() as i64;
        if width == 0 || row < 0 || row >= self.rows as i64 {
            return false;
        }
        if col < 0 || col + width > self.cols as i64 {
            return false;
        }
        let row_base = row as usize * self.cols as usize;
        let lo = (col - 1).max(0) as usize;
        let hi = ((col + width + 1) as usize).min(self.cols as usize);
        if self.occupied[row_base + lo..row_base + hi]
            .iter()
            .any(|&o| o)
        {
            return false;
        }
        self.occupied[row_base + lo..row_base + hi].fill(true);
        self.labels.push(Label {
            col: col as u16,
            row: row as u16,
            text: text.to_owned(),
            style,
        });
        true
    }

    /// Label centred horizontally on a pixel position.
    pub fn label_centered(&mut self, x: f64, y: f64, text: &str, style: Style) -> bool {
        let col = (x / 2.0).floor() as i64 - text.width() as i64 / 2;
        self.label(col, (y / 4.0).floor() as i64, text, style)
    }

    /// Marker symbol + name to the right of a pixel position (left if no room).
    pub fn label_marker(&mut self, x: f64, y: f64, symbol: char, name: &str, style: Style) -> bool {
        let (col, row) = ((x / 2.0).floor() as i64, (y / 4.0).floor() as i64);
        let right = format!("{symbol} {name}");
        if self.label(col, row, &right, style) {
            return true;
        }
        let left = format!("{name} {symbol}");
        self.label(col - left.width() as i64 + 1, row, &left, style)
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer) {
        for row in 0..self.rows.min(area.height) {
            for col in 0..self.cols.min(area.width) {
                let idx = row as usize * self.cols as usize + col as usize;
                let dots = self.dots[idx];
                let ch = if dots == 0 {
                    ' '
                } else {
                    char::from_u32(0x2800 + dots as u32).unwrap_or(' ')
                };
                let bg = if self.fill[idx].count_ones() >= 4 {
                    self.bg[idx]
                } else {
                    Color::Reset
                };
                if let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) {
                    cell.set_char(ch).set_fg(self.fg[idx]).set_bg(bg);
                }
            }
        }
        for l in &self.labels {
            if l.row < area.height && l.col < area.width {
                buf.set_stringn(
                    area.x + l.col,
                    area.y + l.row,
                    &l.text,
                    (area.width - l.col) as usize,
                    l.style,
                );
            }
        }
    }
}

/// Liang–Barsky segment clipping.
fn clip_segment(a: (f64, f64), b: (f64, f64), c: &Clip) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - c.x0),
        (dx, c.x1 - a.0),
        (-dy, a.1 - c.y0),
        (dy, c.y1 - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((
        (a.0 + t0 * dx, a.1 + t0 * dy),
        (a.0 + t1 * dx, a.1 + t1 * dy),
    ))
}
