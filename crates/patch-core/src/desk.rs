//! The desk: a layout's Units inside one window, where no tiling window
//! manager places them.
//!
//! On Omarchy, Hyprland tiles each Unit's own window (`layout.rs` plans the
//! splits). A desktop that doesn't tile (GNOME, KDE Plasma, Cinnamon: the
//! generic kit's mini compositor) and macOS (`dev/macos`'s DJ desk) hold
//! the Units in one desk window instead. This is the geometry both share,
//! free of any window system: the same split tree Hyprland gets
//! (`layout::build`), turned into rectangles inside the desk, the
//! dividers between them, a divider dragged, and the desk's own minimum.
//!
//! Every Unit keeps at least its minimum size (`SurfaceSpec` min): a
//! divider stops where a neighbour would go below its minimum, and when the
//! desk is smaller than the sum of the minima the space is shared in their
//! proportion (the Unit shows its usual "shows everything from W×H" note).
//! A layout of several workspaces is several pages; floating tiles float
//! over their page at their fraction of it.

use crate::layout::{LayoutRecipe, Split, Tile, build};

/// A rectangle in the desk's own units (logical pixels on either system).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Rect { x, y, w, h }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// One node of a page's tiling: a Unit, or two parts side by side
/// (`across`: a and b left and right) or one over the other.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    Unit(&'static str),
    Split {
        across: bool,
        /// The share of the space (less the gap) `a` has.
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn from_split(s: &Split) -> Node {
        match s {
            Split::Leaf(t) => Node::Unit(t.surface),
            Split::Node { dir, ratio, a, b } => Node::Split {
                across: *dir == 'r',
                ratio: *ratio,
                a: Box::new(Node::from_split(a)),
                b: Box::new(Node::from_split(b)),
            },
        }
    }

    /// The Units in this node, in order.
    pub fn units(&self) -> Vec<&'static str> {
        match self {
            Node::Unit(s) => vec![*s],
            Node::Split { a, b, .. } => {
                let mut v = a.units();
                v.extend(b.units());
                v
            }
        }
    }

    fn at(&mut self, path: &[bool]) -> Option<&mut Node> {
        match (path.split_first(), self) {
            (None, n) => Some(n),
            (Some((first, rest)), Node::Split { a, b, .. }) => {
                if *first { b.at(rest) } else { a.at(rest) }
            }
            _ => None,
        }
    }
}

/// One page of the desk: a workspace of the layout.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub tiles: Option<Node>,
    /// Floating Units: their rectangle as a fraction of the page.
    pub floating: Vec<Tile>,
}

/// A divider between two parts of a page, to drag.
#[derive(Debug, Clone, PartialEq)]
pub struct Divider {
    /// Which split it is: from the page's top node, `false` into `a`,
    /// `true` into `b`.
    pub path: Vec<bool>,
    /// True for a divider between parts side by side (it moves left and right).
    pub across: bool,
    /// Where it is: the gap between the two parts.
    pub rect: Rect,
}

/// The minimum of a Unit, in the desk's units (its surface's min size).
pub type MinSize<'a> = &'a dyn Fn(&str) -> (f32, f32);

#[derive(Debug, Clone, PartialEq)]
pub struct Desk {
    pub pages: Vec<Page>,
    /// Space between Units (a divider sits in it).
    pub gap: f32,
}

impl Desk {
    /// A layout on the desk: its workspaces as pages, the tiles as Hyprland
    /// would split them. `None` when a page's tiles don't split cleanly
    /// (`LayoutRecipe::plan` refuses those too).
    pub fn from_recipe(recipe: &LayoutRecipe, openable: impl Fn(&str) -> bool, gap: f32) -> Option<Desk> {
        let arranged = recipe.arranged(openable);
        let pages = arranged.iter().map(|t| t.ws as usize + 1).max().unwrap_or(0);
        let mut out = Vec::with_capacity(pages);
        for ws in 0..pages as u8 {
            let tiled: Vec<Tile> = arranged.iter().filter(|t| t.ws == ws && !t.float).copied().collect();
            let tiles = if tiled.is_empty() {
                None
            } else {
                Some(Node::from_split(&build(&tiled, (0.0, 0.0, 1.0, 1.0))?))
            };
            let floating = arranged.iter().filter(|t| t.ws == ws && t.float).copied().collect();
            out.push(Page { tiles, floating });
        }
        Some(Desk { pages: out, gap })
    }

    /// The smallest a page can be with every Unit at its minimum.
    pub fn minimum(&self, page: usize, min: MinSize) -> (f32, f32) {
        self.pages
            .get(page)
            .and_then(|p| p.tiles.as_ref())
            .map(|n| minimum(n, self.gap, min))
            .unwrap_or((0.0, 0.0))
    }

    /// Where each Unit of a page goes inside `area` (tiled first, then the
    /// floating ones over them).
    pub fn place(&self, page: usize, area: Rect, min: MinSize) -> Vec<(&'static str, Rect)> {
        let mut out = Vec::new();
        let Some(p) = self.pages.get(page) else { return out };
        if let Some(n) = &p.tiles {
            place(n, area, self.gap, min, &mut out, &mut Vec::new(), &mut Vec::new());
        }
        for t in &p.floating {
            out.push((
                t.surface,
                Rect::new(area.x + t.x * area.w, area.y + t.y * area.h, t.w * area.w, t.h * area.h),
            ));
        }
        out
    }

    /// The dividers of a page inside `area`.
    pub fn dividers(&self, page: usize, area: Rect, min: MinSize) -> Vec<Divider> {
        let mut out = Vec::new();
        if let Some(Some(n)) = self.pages.get(page).map(|p| &p.tiles) {
            place(n, area, self.gap, min, &mut Vec::new(), &mut out, &mut Vec::new());
        }
        out
    }

    /// A divider dragged to `at` (x for one between parts side by side, y
    /// for one between parts one over the other): its split's share
    /// follows, stopping where either side would go below its minimum.
    pub fn drag(&mut self, page: usize, path: &[bool], at: f32, area: Rect, min: MinSize) {
        if !self.dividers(page, area, min).iter().any(|d| d.path == path) {
            return;
        }
        // The split's own rectangle: the parent's part the divider cuts.
        let gap = self.gap;
        let Some(Some(root)) = self.pages.get_mut(page).map(|p| p.tiles.as_mut()) else { return };
        let Some(region) = region_of(root, path, area, gap, min) else { return };
        let Some(Node::Split { across, ratio, a, b }) = root.at(path) else { return };
        let (ma, mb) = (minimum(a, gap, min), minimum(b, gap, min));
        let (start, extent, lo, hi) = if *across {
            (region.x, region.w - gap, ma.0, mb.0)
        } else {
            (region.y, region.h - gap, ma.1, mb.1)
        };
        if extent <= 0.0 {
            return;
        }
        let first = (at - gap / 2.0 - start).clamp(lo.min(extent), (extent - hi).max(lo.min(extent)));
        *ratio = (first / extent).clamp(0.0, 1.0);
    }
}

fn minimum(n: &Node, gap: f32, min: MinSize) -> (f32, f32) {
    match n {
        Node::Unit(s) => min(s),
        Node::Split { across, a, b, .. } => {
            let (ma, mb) = (minimum(a, gap, min), minimum(b, gap, min));
            if *across {
                (ma.0 + gap + mb.0, ma.1.max(mb.1))
            } else {
                (ma.0.max(mb.0), ma.1 + gap + mb.1)
            }
        }
    }
}

/// How much of `extent` (the space less the gap) the first part gets: its
/// share, kept between the two minima; when both minima don't fit, the
/// space in their proportion.
fn first_part(ratio: f32, extent: f32, min_a: f32, min_b: f32) -> f32 {
    if extent <= 0.0 {
        return 0.0;
    }
    if min_a + min_b > extent {
        let total = (min_a + min_b).max(f32::EPSILON);
        return extent * min_a / total;
    }
    (ratio * extent).clamp(min_a, extent - min_b)
}

fn split_rects(n: &Node, r: Rect, gap: f32, min: MinSize) -> Option<(Rect, Rect, Rect)> {
    let Node::Split { across, ratio, a, b } = n else { return None };
    let (ma, mb) = (minimum(a, gap, min), minimum(b, gap, min));
    Some(if *across {
        let wa = first_part(*ratio, r.w - gap, ma.0, mb.0);
        (
            Rect::new(r.x, r.y, wa, r.h),
            Rect::new(r.x + wa, r.y, gap, r.h),
            Rect::new(r.x + wa + gap, r.y, (r.w - gap - wa).max(0.0), r.h),
        )
    } else {
        let ha = first_part(*ratio, r.h - gap, ma.1, mb.1);
        (
            Rect::new(r.x, r.y, r.w, ha),
            Rect::new(r.x, r.y + ha, r.w, gap),
            Rect::new(r.x, r.y + ha + gap, r.w, (r.h - gap - ha).max(0.0)),
        )
    })
}

fn place(
    n: &Node,
    r: Rect,
    gap: f32,
    min: MinSize,
    units: &mut Vec<(&'static str, Rect)>,
    dividers: &mut Vec<Divider>,
    path: &mut Vec<bool>,
) {
    match n {
        Node::Unit(s) => units.push((s, r)),
        Node::Split { across, a, b, .. } => {
            let (ra, rd, rb) = split_rects(n, r, gap, min).expect("a split");
            dividers.push(Divider { path: path.clone(), across: *across, rect: rd });
            path.push(false);
            place(a, ra, gap, min, units, dividers, path);
            path.pop();
            path.push(true);
            place(b, rb, gap, min, units, dividers, path);
            path.pop();
        }
    }
}

/// The rectangle the split at `path` divides.
fn region_of(root: &Node, path: &[bool], area: Rect, gap: f32, min: MinSize) -> Option<Rect> {
    let mut node = root;
    let mut r = area;
    for step in path {
        let (ra, _, rb) = split_rects(node, r, gap, min)?;
        let Node::Split { a, b, .. } = node else { return None };
        (node, r) = if *step { (&**b, rb) } else { (&**a, ra) };
    }
    Some(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{tile, tile_on};

    fn recipe(tiles: Vec<Tile>) -> LayoutRecipe {
        LayoutRecipe {
            id: "t",
            name: "T",
            summary: "",
            status: crate::layout::LayoutStatus::Available,
            tiles,
        }
    }

    fn min(_: &str) -> (f32, f32) {
        (200.0, 100.0)
    }

    /// Deck A | Crossfader | Deck B over the Library: the DJ desk's shape.
    fn dj() -> Desk {
        let r = recipe(vec![
            tile("deck-a", 0.0, 0.0, 0.4, 0.6),
            tile("crossfader", 0.4, 0.0, 0.2, 0.6),
            tile("deck-b", 0.6, 0.0, 0.4, 0.6),
            tile("library", 0.0, 0.6, 1.0, 0.4),
        ]);
        Desk::from_recipe(&r, |_| true, 10.0).unwrap()
    }

    #[test]
    fn a_layout_fills_the_desk_as_hyprland_would_split_it() {
        let d = dj();
        let placed = d.place(0, Rect::new(0.0, 0.0, 1010.0, 510.0), &min);
        let get = |s: &str| placed.iter().find(|(u, _)| *u == s).unwrap().1;
        // Rows first (as layout::build): the top row 60 % of 500, the library below.
        assert_eq!(get("library").y, 310.0);
        assert_eq!(get("library").h, 200.0);
        assert_eq!(get("deck-a"), Rect::new(0.0, 0.0, 400.0, 300.0));
        assert!((get("deck-b").x + get("deck-b").w - 1010.0).abs() < 0.01, "the row reaches the edge");
        // No Unit overlaps another.
        for (i, (_, a)) in placed.iter().enumerate() {
            for (_, b) in &placed[i + 1..] {
                let apart = a.x + a.w <= b.x + 0.01 || b.x + b.w <= a.x + 0.01 || a.y + a.h <= b.y + 0.01 || b.y + b.h <= a.y + 0.01;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_divider_stops_at_a_units_minimum() {
        let mut d = dj();
        let area = Rect::new(0.0, 0.0, 1010.0, 510.0);
        let divs = d.dividers(0, area, &min);
        assert_eq!(divs.len(), 3, "the row cut, and two between the top three");
        let row = divs.iter().find(|v| !v.across).unwrap().clone();
        d.drag(0, &row.path, 5.0, area, &min);
        let placed = d.place(0, area, &min);
        let top = placed.iter().find(|(u, _)| *u == "deck-a").unwrap().1;
        assert_eq!(top.h, 100.0, "dragged to the top: the decks keep their minimum height");
        d.drag(0, &row.path, 260.0, area, &min);
        let top = d.place(0, area, &min).iter().find(|(u, _)| *u == "deck-a").unwrap().1;
        assert_eq!(top.h, 255.0, "the divider's middle under the pointer");
    }

    #[test]
    fn a_desk_too_small_shares_the_space_by_the_minima() {
        let d = dj();
        assert_eq!(d.minimum(0, &min), (620.0, 210.0));
        let placed = d.place(0, Rect::new(0.0, 0.0, 310.0, 210.0), &min);
        let w: f32 = placed.iter().filter(|(u, _)| *u != "library").map(|(_, r)| r.w).sum();
        assert!((w - 290.0).abs() < 0.01, "three Units share 310 less two gaps");
    }

    #[test]
    fn workspaces_are_pages_and_floating_units_float() {
        let r = recipe(vec![
            tile_on(0, "mixer", 0.0, 0.0, 1.0, 1.0),
            tile_on(1, "eq", 0.0, 0.0, 0.5, 1.0),
            tile_on(1, "meters", 0.5, 0.0, 0.5, 1.0),
            crate::layout::float_on(1, "controller", 0.25, 0.25, 0.5, 0.5),
        ]);
        let d = Desk::from_recipe(&r, |_| true, 8.0).unwrap();
        assert_eq!(d.pages.len(), 2);
        let second = d.place(1, Rect::new(0.0, 0.0, 800.0, 600.0), &min);
        assert_eq!(second.last().unwrap(), &("controller", Rect::new(200.0, 150.0, 400.0, 300.0)));
        assert_eq!(d.pages[1].tiles.as_ref().unwrap().units(), ["eq", "meters"]);
    }
}
