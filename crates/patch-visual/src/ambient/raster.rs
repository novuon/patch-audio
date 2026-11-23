// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See LICENSE and README.md in this directory.
#![allow(dead_code)] // Shared upstream raster helpers, including its fingerprint oracle.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct DirectCell {
    pub x: usize,
    pub y: usize,
    pub ch: char,
    pub color: (u8, u8, u8),
    pub background: Option<(u8, u8, u8)>,
    pub attrs: u64,
}
fn clip_span(a: (f32, f32), d: (f32, f32), right: f32, bottom: f32) -> (f32, f32) {
    let side = |x: f32, y: f32| {
        u8::from(x < 0.)
            | (u8::from(x > right) << 1)
            | (u8::from(y < 0.) << 2)
            | (u8::from(y > bottom) << 3)
    };
    let end = (a.0 + d.0, a.1 + d.1);
    let inside = |x: f32, y: f32| x >= 0. && y >= 0. && x <= right && y <= bottom;
    // Preserve the original hot path before testing outside half-planes.
    if inside(a.0, a.1) && inside(end.0, end.1) {
        return (0., 1.);
    }
    // The caller rejects non-finite endpoints, but subtracting finite extreme
    // endpoints may overflow. Keep the previous arithmetic for that case too.
    if ![a.0, a.1, end.0, end.1].iter().all(|v| v.is_finite()) {
        return clip_span_general(a, d, right, bottom);
    }
    let first = side(a.0, a.1);
    let last = side(end.0, end.1);
    // A convex viewport cannot meet a segment wholly beyond one of its sides.
    if first & last != 0 {
        return (1., 0.);
    }
    clip_span_general(a, d, right, bottom)
}
fn clip_span_general(a: (f32, f32), d: (f32, f32), right: f32, bottom: f32) -> (f32, f32) {
    let (mut lo, mut hi) = (0f32, 1f32);
    for (p, q) in [
        (-d.0, a.0),
        (d.0, right - a.0),
        (-d.1, a.1),
        (d.1, bottom - a.1),
    ] {
        if p == 0. {
            if q < 0. {
                return (1., 0.);
            }
        } else {
            let t = q / p;
            if p < 0. {
                lo = lo.max(t);
            } else {
                hi = hi.min(t);
            }
        }
    }
    (lo, hi)
}

/// `v.round()` for what a clipped line walks: nothing below zero. Generic
/// x86-64 has no rounding instruction, so `round` is a call per cell.
fn nearest(v: f32) -> usize {
    let whole = v as usize;
    whole + usize::from(v - whole as f32 >= 0.5)
}
#[derive(Clone, Copy, Default)]
struct Mark {
    ch: char,
    color: (u8, u8, u8),
}
/// One cell of the scene fingerprint, shared with the complete-frame oracle.
pub(crate) fn frame_print_cell(print: u64, c: &DirectCell) -> u64 {
    let (r, g, b) = c.color;
    let cell = (c.x as u64) << 44
        ^ (c.y as u64) << 32
        ^ (c.ch as u64) << 8
        ^ (r as u64) << 16
        ^ (g as u64) << 24
        ^ b as u64;
    (print ^ cell)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .rotate_left(29)
}
pub(crate) struct Raster {
    cols: usize,
    rows: usize,
    depth: Vec<f32>,
    /// Only what a cell adds to its position: at a few frames a second this
    /// buffer is cold, and every byte per cell shows up as a cache miss.
    marks: Vec<Mark>,
    /// One bit per cell drawn this frame. A frame touches a fraction of the
    /// grid, and only that fraction is read back and cleared.
    drawn: Vec<u64>,
}
impl Raster {
    pub(crate) fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            depth: vec![f32::INFINITY; cols * rows],
            marks: vec![Mark::default(); cols * rows],
            drawn: vec![0; (cols * rows).div_ceil(64)],
        }
    }
    pub(crate) fn fits(&self, cols: usize, rows: usize) -> bool {
        self.cols == cols && self.rows == rows
    }
    /// Whether something nearer than `z` already holds the cell: lets a
    /// costly shader skip what would be thrown away. The cell must be in range.
    pub(crate) fn hidden(&self, x: usize, y: usize, z: f32) -> bool {
        self.depth[y * self.cols + x] <= z
    }
    /// Hand over the frame and leave the buffers ready for the next one.
    pub(crate) fn recycle(&mut self, out: &mut Vec<DirectCell>) {
        self.drain::<false>(out, false);
    }
    /// As `recycle`, less blank black cells: those only hid what was behind
    /// them and are the same pixels as no cell at all.
    pub(crate) fn recycle_visible(&mut self, out: &mut Vec<DirectCell>) {
        self.drain::<false>(out, true);
    }
    /// Hand over cells and fingerprint them while they are already in hand.
    /// The ordinary preview draw does not pay for this producer-only pass.
    pub(crate) fn recycle_with_print(&mut self, out: &mut Vec<DirectCell>) -> u64 {
        self.drain::<true>(out, false)
    }
    fn drain<const PRINT: bool>(&mut self, out: &mut Vec<DirectCell>, skip_blank: bool) -> u64 {
        out.clear();
        let mut print = if PRINT {
            self.drawn
                .iter()
                .map(|bits| bits.count_ones() as u64)
                .sum::<u64>()
                + 1
        } else {
            0
        };
        // Cells come out in order, so the row only ever moves forward: no
        // division per cell.
        let cols = self.cols.max(1);
        let (mut y, mut row) = (0usize, 0usize);
        for (word, bits) in self.drawn.iter_mut().enumerate() {
            let mut left = std::mem::take(bits);
            while left != 0 {
                let i = word * 64 + left.trailing_zeros() as usize;
                left &= left - 1;
                self.depth[i] = f32::INFINITY;
                let Mark { ch, color } = self.marks[i];
                if skip_blank && ch == ' ' && color == (0, 0, 0) {
                    continue;
                }
                if i - row >= cols {
                    y = i / cols;
                    row = y * cols;
                }
                let cell = DirectCell {
                    x: i - row,
                    y,
                    ch,
                    color,
                    background: None,
                    attrs: 1,
                };
                if PRINT {
                    print = frame_print_cell(print, &cell);
                }
                out.push(cell);
            }
        }
        print
    }
    #[cfg(test)]
    pub(crate) fn finish(mut self, out: &mut Vec<DirectCell>) {
        self.recycle(out);
    }
    /// Light thrown on what is already drawn in `x0..x1` by `y0..y1`: each
    /// cell brightens by `k(x, y)` (0 leaves it, 1 makes it glow), keeping
    /// its hue. Empty and blank cells stay dark.
    pub(crate) fn lift(
        &mut self,
        (x0, x1): (usize, usize),
        (y0, y1): (usize, usize),
        mut k: impl FnMut(usize, usize) -> f32,
    ) {
        for y in y0..y1.min(self.rows) {
            for x in x0..x1.min(self.cols) {
                let i = y * self.cols + x;
                if (self.drawn[i / 64] >> (i % 64)) & 1 == 0 {
                    continue;
                }
                let m = &mut self.marks[i];
                if m.ch == ' ' {
                    continue;
                }
                let a = k(x, y).min(1.);
                if a <= 0.01 {
                    continue;
                }
                // Dim cells gain most, so a faint fill shows its colour too.
                let peak = m.color.0.max(m.color.1).max(m.color.2).max(1) as f32;
                let gain = (1. + a * 4.).max(a * 170. / peak);
                let up = |v: u8| {
                    let v = (v as f32 * gain).min(255.);
                    (v + (255. - v) * a * 0.45) as u8
                };
                m.color = (up(m.color.0), up(m.color.1), up(m.color.2));
            }
        }
    }
    pub(crate) fn dot(&mut self, x: i32, y: i32, z: f32, ch: char, color: (u8, u8, u8)) {
        if x < 0 || y < 0 || x as usize >= self.cols || y as usize >= self.rows {
            return;
        }
        let i = y as usize * self.cols + x as usize;
        if z < self.depth[i] {
            self.depth[i] = z;
            self.drawn[i / 64] |= 1 << (i % 64);
            self.marks[i] = Mark { ch, color };
        }
    }

    pub(crate) fn line(
        &mut self,
        a: (f32, f32),
        b: (f32, f32),
        z: f32,
        color: (u8, u8, u8),
        ring: bool,
    ) {
        self.line_with_glyph(a, b, z, color, ring, None);
    }
    pub(crate) fn line_with_glyph(
        &mut self,
        a: (f32, f32),
        b: (f32, f32),
        z: f32,
        color: (u8, u8, u8),
        ring: bool,
        glyph: Option<char>,
    ) {
        if ![a.0, a.1, b.0, b.1].iter().all(|v| v.is_finite()) {
            return;
        }
        // Clip before walking: near geometry can project far beyond the viewport.
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let (lo, hi) = clip_span(a, (dx, dy), self.cols as f32 - 1., self.rows as f32 - 1.);
        if lo > hi {
            return;
        }
        let start = (a.0 + dx * lo, a.1 + dy * lo);
        let end = (a.0 + dx * hi, a.1 + dy * hi);
        let steps = ((end.0 - start.0).abs().max((end.1 - start.1).abs()).ceil() as usize).max(1);
        let ch = if ring {
            if dy.abs() < dx.abs() * 0.35 {
                '='
            } else if dx.abs() < dy.abs() * 0.35 {
                '|'
            } else if dx * dy > 0. {
                '\\'
            } else {
                '/'
            }
        } else {
            '.'
        };
        let ch = glyph.unwrap_or(ch);
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let x = nearest(start.0 + (end.0 - start.0) * t).min(self.cols - 1);
            let y = nearest(start.1 + (end.1 - start.1) * t).min(self.rows - 1);
            let index = y * self.cols + x;
            if z < self.depth[index] {
                self.depth[index] = z;
                self.drawn[index / 64] |= 1 << (index % 64);
                self.marks[index] = Mark { ch, color };
            }
        }
    }
}
