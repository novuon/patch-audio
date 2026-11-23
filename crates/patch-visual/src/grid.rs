//! One frame as a grid of character cells, each with its colours.

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Cell {
    pub ch: char,
    /// None: the terminal's default foreground (the theme's text).
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
}

#[derive(Debug, Default)]
pub struct Grid {
    pub cols: usize,
    pub rows: usize,
    /// Row-major, top row first.
    pub cells: Vec<Cell>,
}

// A frame builds several grids (backdrop, overlay, words, a copy for a
// pixel style) and drops them: 120 MB a second through the allocator at
// 480 × 200 cells. A dropped grid's cells go back to a small pool on its
// thread; the next blank grid takes them, already the right size.
thread_local! {
    static POOL: std::cell::RefCell<Vec<Vec<Cell>>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn cells_for(n: usize) -> Vec<Cell> {
    let mut v = POOL
        .with(|p| {
            let mut p = p.borrow_mut();
            let i = p
                .iter()
                .position(|v| v.capacity() >= n)
                .or(if p.is_empty() {
                    None
                } else {
                    Some(p.len() - 1)
                });
            i.map(|i| p.swap_remove(i))
        })
        .unwrap_or_default();
    v.clear();
    v
}

impl Drop for Grid {
    fn drop(&mut self) {
        let v = std::mem::take(&mut self.cells);
        if v.capacity() > 0 {
            POOL.with(|p| {
                let mut p = p.borrow_mut();
                if p.len() < 12 {
                    p.push(v);
                }
            });
        }
    }
}

impl Clone for Grid {
    fn clone(&self) -> Grid {
        let mut cells = cells_for(self.cells.len());
        cells.extend_from_slice(&self.cells);
        Grid {
            cols: self.cols,
            rows: self.rows,
            cells,
        }
    }
}

impl Grid {
    pub fn blank(cols: usize, rows: usize) -> Grid {
        let mut cells = cells_for(cols * rows);
        cells.resize(
            cols * rows,
            Cell {
                ch: ' ',
                fg: None,
                bg: None,
            },
        );
        Grid { cols, rows, cells }
    }

    pub fn at(&self, col: usize, row: usize) -> Cell {
        self.cells[row * self.cols + col]
    }
}

/// A cell's own 2×4 pattern of sub-pixels (bit `row * 2 + col`, top left
/// first): letters in real type at four times a cell's detail. Drawn by
/// the renderer itself (a private-use code point per pattern), not a font.
pub fn shape(mask: u8) -> char {
    char::from_u32(SHAPES + mask as u32).unwrap_or('█')
}

/// A cell's 2×4 sub-pixels with how much ink each has (0…3, two bits
/// each, sub-pixel `row * 2 + col` at bit 2 × that): the edge of a letter
/// falls between sub-pixels, so a curve is drawn smooth, not in steps.
pub fn shape_inked(levels: u16) -> char {
    char::from_u32(INKED + levels as u32).unwrap_or('█')
}

/// A shape's ink per sub-pixel (an on/off shape: 3 where on).
pub fn shape_levels(c: char) -> Option<u16> {
    let v = c as u32;
    if (SHAPES..SHAPES + 256).contains(&v) {
        let m = v - SHAPES;
        Some(
            (0..8)
                .filter(|b| m & (1 << b) != 0)
                .fold(0u16, |l, b| l | (3 << (b * 2))),
        )
    } else if (INKED..=INKED + 0xffff).contains(&v) {
        Some((v - INKED) as u16)
    } else {
        None
    }
}

/// The pattern of a shape cell: the sub-pixels mostly inked.
pub fn shape_mask(c: char) -> Option<u8> {
    shape_levels(c).map(|l| {
        (0..8)
            .filter(|b| (l >> (b * 2)) & 3 >= 2)
            .fold(0u8, |m, b| m | (1 << b))
    })
}

/// Ink of any kind: a full block or a shape (letter art's cells).
pub fn is_ink(c: char) -> bool {
    c == '█' || shape_levels(c).is_some_and(|l| l != 0)
}

const SHAPES: u32 = 0xF0000;
const INKED: u32 = 0x100000;
