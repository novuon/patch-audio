/// Older layout names, kept so commands and launchers that use them still
/// work: (old, current).
pub const ALIASES: [(&str, &str); 10] = [
    ("minimal", "simple"),
    ("compact", "simple"),
    ("studio", "simple"),
    ("recording", "podcast"),
    ("live-dj", "performance-dj"),
    ("dj", "performance-dj"),
    ("dj-only", "performance-dj"),
    ("dev", "all-units"),
    ("performance", "pro"),
    ("all", "all-units"),
];

/// The current id of a layout name (an old name maps to its successor).
pub fn canonical(id: &str) -> &str {
    ALIASES
        .iter()
        .find(|(old, _)| *old == id)
        .map(|(_, new)| *new)
        .unwrap_or(id)
}

/// A layout recipe describes the *initial* arrangement of one workspace, or
/// of several (tiles carry a workspace number, from 0).
///
/// It is a starting point handed to the compositor, never a cage: once the
/// surfaces are open, Hyprland owns them and the user can retile freely.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutRecipe {
    pub id: &'static str,
    pub name: &'static str,
    pub summary: &'static str,
    pub status: LayoutStatus,
    /// Tiles in opening order. Coordinates are fractions of the workspace.
    pub tiles: Vec<Tile>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LayoutStatus {
    Available,
    /// The recipe depends on a plugin that does not exist yet.
    PluginMock {
        plugin: &'static str,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tile {
    /// Workspace within the recipe (0 = the first empty one it opens on).
    pub ws: u8,
    pub surface: &'static str,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// A floating window: its rectangle is a fraction of the monitor's
    /// usable area, and it takes no part in the tiling.
    pub float: bool,
}

pub const fn tile(surface: &'static str, x: f32, y: f32, w: f32, h: f32) -> Tile {
    Tile {
        ws: 0,
        surface,
        x,
        y,
        w,
        h,
        float: false,
    }
}

/// A tile on workspace `ws` of a multi-workspace recipe.
pub const fn tile_on(ws: u8, surface: &'static str, x: f32, y: f32, w: f32, h: f32) -> Tile {
    Tile {
        ws,
        surface,
        x,
        y,
        w,
        h,
        float: false,
    }
}

/// A floating window on workspace `ws`.
pub const fn float_on(ws: u8, surface: &'static str, x: f32, y: f32, w: f32, h: f32) -> Tile {
    Tile {
        ws,
        surface,
        x,
        y,
        w,
        h,
        float: true,
    }
}

/// A `&'static str` for a string read at run time (a saved layout's ids).
/// Each distinct string is kept once, however often layouts are reloaded.
pub fn intern(s: &str) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static POOL: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut pool = POOL
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(&kept) = pool.get(s) {
        return kept;
    }
    let kept: &'static str = Box::leak(s.to_owned().into_boxed_str());
    pool.insert(kept);
    kept
}

/// Edges closer than this (a fraction of the workspace) are one edge: the
/// gaps and borders between tiled windows.
const SNAP: f32 = 0.02;

/// Tidy windows as they are on screen into recipe tiles: per workspace,
/// the tiled windows' edges that meet (across a gap) become one edge, and
/// their bounding box becomes the unit square. Floating windows are kept
/// as they are. `ws` numbers are expected to be 0, 1, … already.
pub fn capture(windows: &[Tile]) -> Vec<Tile> {
    let round = |v: f32| (v * 10_000.0).round() / 10_000.0;
    let mut out = vec![];
    let workspaces = windows.iter().map(|t| t.ws as usize + 1).max().unwrap_or(0);
    for ws in 0..workspaces as u8 {
        let tiled: Vec<&Tile> = windows.iter().filter(|t| t.ws == ws && !t.float).collect();
        let snap_axis = |edges: Vec<f32>| -> Vec<(f32, f32)> {
            // (raw, snapped) pairs: clusters of nearby edges share their mean.
            let mut sorted = edges;
            sorted.sort_by(f32::total_cmp);
            let mut out = vec![];
            let mut cluster: Vec<f32> = vec![];
            for e in sorted {
                if cluster.last().is_some_and(|&l| e - l >= SNAP) {
                    let mean = cluster.iter().sum::<f32>() / cluster.len() as f32;
                    out.extend(cluster.drain(..).map(|c| (c, mean)));
                }
                cluster.push(e);
            }
            if !cluster.is_empty() {
                let mean = cluster.iter().sum::<f32>() / cluster.len() as f32;
                out.extend(cluster.drain(..).map(|c| (c, mean)));
            }
            out
        };
        let xs = snap_axis(tiled.iter().flat_map(|t| [t.x, t.x + t.w]).collect());
        let ys = snap_axis(tiled.iter().flat_map(|t| [t.y, t.y + t.h]).collect());
        let at = |axis: &[(f32, f32)], v: f32| {
            axis.iter()
                .find(|(raw, _)| *raw == v)
                .map_or(v, |(_, s)| *s)
        };
        let (x0, x1) = (
            xs.first().map_or(0.0, |e| e.1),
            xs.last().map_or(1.0, |e| e.1),
        );
        let (y0, y1) = (
            ys.first().map_or(0.0, |e| e.1),
            ys.last().map_or(1.0, |e| e.1),
        );
        for t in &tiled {
            let (l, r) = (at(&xs, t.x), at(&xs, t.x + t.w));
            let (top, bottom) = (at(&ys, t.y), at(&ys, t.y + t.h));
            let nx = |v: f32| round((v - x0) / (x1 - x0));
            let ny = |v: f32| round((v - y0) / (y1 - y0));
            out.push(Tile {
                x: nx(l),
                y: ny(top),
                w: round(nx(r) - nx(l)),
                h: round(ny(bottom) - ny(top)),
                ..**t
            });
        }
        out.extend(
            windows
                .iter()
                .filter(|t| t.ws == ws && t.float)
                .map(|t| Tile {
                    x: round(t.x),
                    y: round(t.y),
                    w: round(t.w),
                    h: round(t.h),
                    ..*t
                }),
        );
    }
    out
}

/// How far a window may sit from its tile and still count as in place.
const SAME: f32 = 0.03;

/// The same windows on the same workspaces, each where the other has it.
pub fn same_arrangement(a: &[Tile], b: &[Tile]) -> bool {
    let close = |p: &Tile, q: &Tile| {
        p.ws == q.ws
            && p.surface == q.surface
            && p.float == q.float
            && [(p.x, q.x), (p.y, q.y), (p.w, q.w), (p.h, q.h)]
                .iter()
                .all(|(u, v)| (u - v).abs() < SAME)
    };
    a.len() == b.len() && a.iter().all(|p| b.iter().any(|q| close(p, q)))
}

impl LayoutRecipe {
    pub fn is_available(&self) -> bool {
        matches!(self.status, LayoutStatus::Available)
    }

    pub fn surfaces(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.tiles.iter().map(|t| t.surface)
    }

    /// Number of workspaces the recipe spans.
    pub fn workspaces(&self) -> usize {
        self.tiles
            .iter()
            .map(|t| t.ws as usize + 1)
            .max()
            .unwrap_or(1)
    }
}

/// How a tiling compositor with binary splits (Hyprland dwindle) can replay a
/// recipe: open the first surface, then repeatedly focus a window, preselect
/// a split direction, open the next surface and set the split ratio.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Open(&'static str),
    Focus(&'static str),
    /// 'r' (new window to the right) or 'd' (below).
    Preselect(char),
    /// Share of the focused window's split node given to its first child.
    Ratio(f32),
    /// Continue on the next empty workspace.
    NextWorkspace,
    /// Float an open window at a fraction of the monitor's usable area.
    Place {
        surface: &'static str,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    },
}

/// The guillotine split tree of a workspace's tiles (Hyprland's dwindle
/// splits, and the desk's: `desk.rs`).
#[derive(Debug)]
pub(crate) enum Split {
    Leaf(Tile),
    Node {
        dir: char,
        ratio: f32,
        a: Box<Split>,
        b: Box<Split>,
    },
}

const EPS: f32 = 0.002;

pub(crate) fn build(tiles: &[Tile], (rx, ry, rw, rh): (f32, f32, f32, f32)) -> Option<Split> {
    if tiles.len() == 1 {
        return Some(Split::Leaf(tiles[0]));
    }
    // Rows first (a cut across the whole width), then columns: each row
    // then resizes on its own. Columns first put a window and the one
    // under it in one column wherever their edges lined up (DJ Studio:
    // Deck A over the Visualiser), and widening one widened the other.
    for vertical in [false, true] {
        for t in tiles {
            let cut = if vertical { t.x + t.w } else { t.y + t.h };
            let (start, extent) = if vertical { (rx, rw) } else { (ry, rh) };
            if cut >= start + extent - EPS {
                continue;
            }
            let (a, b): (Vec<Tile>, Vec<Tile>) = tiles.iter().partition(|u| {
                let end = if vertical { u.x + u.w } else { u.y + u.h };
                end <= cut + EPS
            });
            let clean = b
                .iter()
                .all(|u| (if vertical { u.x } else { u.y }) >= cut - EPS);
            if a.is_empty() || b.is_empty() || !clean {
                continue;
            }
            let ratio = (cut - start) / extent;
            let (ra, rb) = if vertical {
                ((rx, ry, cut - rx, rh), (cut, ry, rx + rw - cut, rh))
            } else {
                ((rx, ry, rw, cut - ry), (rx, cut, rw, ry + rh - cut))
            };
            return Some(Split::Node {
                dir: if vertical { 'r' } else { 'd' },
                ratio,
                a: Box::new(build(&a, ra)?),
                b: Box::new(build(&b, rb)?),
            });
        }
    }
    None
}

fn first(s: &Split) -> &'static str {
    match s {
        Split::Leaf(t) => t.surface,
        Split::Node { a, .. } => first(a),
    }
}

fn walk(s: &Split, out: &mut Vec<Step>) {
    if let Split::Node { dir, ratio, a, b } = s {
        out.push(Step::Focus(first(a)));
        out.push(Step::Preselect(*dir));
        out.push(Step::Open(first(b)));
        out.push(Step::Focus(first(a)));
        out.push(Step::Ratio(*ratio));
        walk(a, out);
        walk(b, out);
    }
}

impl LayoutRecipe {
    /// Compositor steps for this recipe, or `None` if the tiles are not a
    /// guillotine partition of the workspace.
    pub fn plan(&self) -> Option<Vec<Step>> {
        self.plan_open(|_| true)
    }

    /// The tiles as they open: only those whose surface can be opened
    /// (planned Units have no window yet: the real ones share their
    /// workspace between them), a workspace left empty skipped, and the
    /// tiled ones scaled to the bounding box of what actually opens.
    pub fn arranged(&self, openable: impl Fn(&str) -> bool) -> Vec<Tile> {
        let mut out = vec![];
        let mut next = 0u8;
        for ws in 0..self.workspaces() as u8 {
            let open: Vec<Tile> = self
                .tiles
                .iter()
                .filter(|t| t.ws == ws && openable(t.surface))
                .copied()
                .collect();
            if open.is_empty() {
                continue;
            }
            let tiled = || open.iter().filter(|t| !t.float);
            let x0 = tiled().map(|t| t.x).fold(1.0, f32::min);
            let y0 = tiled().map(|t| t.y).fold(1.0, f32::min);
            let x1 = tiled().map(|t| t.x + t.w).fold(0.0, f32::max);
            let y1 = tiled().map(|t| t.y + t.h).fold(0.0, f32::max);
            out.extend(open.iter().map(|t| match t.float {
                true => Tile { ws: next, ..*t },
                false => Tile {
                    ws: next,
                    x: (t.x - x0) / (x1 - x0),
                    y: (t.y - y0) / (y1 - y0),
                    w: t.w / (x1 - x0),
                    h: t.h / (y1 - y0),
                    ..*t
                },
            }));
            next += 1;
        }
        out
    }

    /// Steps for [`Self::arranged`], one workspace after another: the tiled
    /// windows, then the floating ones.
    pub fn plan_open(&self, openable: impl Fn(&str) -> bool) -> Option<Vec<Step>> {
        let arranged = self.arranged(openable);
        let mut out = vec![];
        for ws in 0..arranged
            .iter()
            .map(|t| t.ws as usize + 1)
            .max()
            .unwrap_or(0) as u8
        {
            if ws > 0 {
                out.push(Step::NextWorkspace);
            }
            let tiled: Vec<Tile> = arranged
                .iter()
                .filter(|t| t.ws == ws && !t.float)
                .copied()
                .collect();
            if !tiled.is_empty() {
                let tree = build(&tiled, (0.0, 0.0, 1.0, 1.0))?;
                out.push(Step::Open(first(&tree)));
                walk(&tree, &mut out);
            }
            for t in arranged.iter().filter(|t| t.ws == ws && t.float) {
                out.push(Step::Open(t.surface));
                out.push(Step::Place {
                    surface: t.surface,
                    x: t.x,
                    y: t.y,
                    w: t.w,
                    h: t.h,
                });
            }
        }
        (!out.is_empty()).then_some(out)
    }
}

/// Whether the steps up to the next workspace open a tiled window.
///
/// A floating tile is `Open` followed immediately by [`Step::Place`]. Anything
/// else opened here is a dwindle split. Omarchy 4.5 can float a whole
/// workspace (Super+L); those splits only land after that workspace is put
/// back on dwindle, and a workspace of only placed windows can stay floating.
pub fn opens_tiled(steps: &[Step]) -> bool {
    let mut i = 0;
    while i < steps.len() {
        match steps[i] {
            Step::NextWorkspace => return false,
            Step::Open(_) if !matches!(steps.get(i + 1), Some(Step::Place { .. })) => {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// The smallest screen layouts are sized for: every window gets at least
/// its minimum on it (logical px: 1920×1080 less Omarchy's 24 px bar),
/// after Hyprland's gaps and borders.
pub const ROOM: (f32, f32) = (1920.0, 1056.0);
/// Gaps and borders per tile, each way (logical px).
pub const GAP: f32 = 22.0;

fn fits(w: f32, h: f32, min: (u16, u16), screen: (f32, f32)) -> bool {
    w * screen.0 - GAP >= min.0 as f32 && h * screen.1 - GAP >= min.1 as f32
}

/// Pack `surfaces` (with their minimum sizes) onto as few workspaces as
/// [`ROOM`] allows, in order: each workspace a grid of rows, as many
/// windows as fit at their minimum; a short last row stretches its windows.
pub fn pack(surfaces: &[(&'static str, (u16, u16))]) -> Vec<Tile> {
    pack_on(surfaces, ROOM)
}

/// [`pack`] for a given screen (logical px, without the bar): a larger
/// screen takes more windows per workspace. Shelves: each workspace fills
/// rows left to right at each window's own minimum width, a row as tall
/// as its tallest window; the room left over is shared out in proportion.
pub fn pack_on(surfaces: &[(&'static str, (u16, u16))], screen: (f32, f32)) -> Vec<Tile> {
    let (sw, sh) = screen;
    let need = |m: (u16, u16)| (m.0 as f32 + GAP, m.1 as f32 + GAP);
    // Workspaces of rows of (id, minimum + gaps).
    let mut workspaces: Vec<Vec<Vec<(&'static str, (f32, f32))>>> = vec![];
    for &(id, m) in surfaces {
        let (w, h) = need(m);
        let ws = workspaces.last_mut().filter(|rows| {
            let row = rows.last().unwrap();
            let used: f32 = rows.iter().map(|r| r.iter().map(|x| x.1.1).fold(0.0, f32::max)).sum();
            let row_h = row.iter().map(|x| x.1.1).fold(0.0, f32::max);
            let row_w: f32 = row.iter().map(|x| x.1.0).sum();
            // Beside the last row's windows, or a new row below them.
            (row_w + w <= sw && used - row_h + row_h.max(h) <= sh) || used + h <= sh
        });
        match ws {
            Some(rows) => {
                let row = rows.last().unwrap();
                let used: f32 = rows.iter().map(|r| r.iter().map(|x| x.1.1).fold(0.0, f32::max)).sum();
                let row_h = row.iter().map(|x| x.1.1).fold(0.0, f32::max);
                let row_w: f32 = row.iter().map(|x| x.1.0).sum();
                if row_w + w <= sw && used - row_h + row_h.max(h) <= sh {
                    rows.last_mut().unwrap().push((id, (w, h)));
                } else {
                    rows.push(vec![(id, (w, h))]);
                }
            }
            None => workspaces.push(vec![vec![(id, (w, h))]]),
        }
    }
    let mut out = vec![];
    for (ws, rows) in workspaces.iter().enumerate() {
        let heights: Vec<f32> = rows.iter().map(|r| r.iter().map(|x| x.1.1).fold(0.0, f32::max)).collect();
        let total_h: f32 = heights.iter().sum();
        let mut y = 0.0;
        for (row, h) in rows.iter().zip(&heights) {
            let fh = h / total_h;
            let total_w: f32 = row.iter().map(|x| x.1.0).sum();
            let mut x = 0.0;
            for (id, (w, _)) in row {
                let fw = w / total_w;
                out.push(Tile { ws: ws as u8, surface: id, x, y, w: fw, h: fh, float: false });
                x += fw;
            }
            y += fh;
        }
    }
    out
}

/// `tiles` with one window of `extra` (minimum `extra_min`) on every
/// workspace that has none: it takes a slice of the tiled window with the
/// most room to spare (beside it or below it), so every window keeps its
/// minimum on [`ROOM`] where it can. `instances` are the ids to use, one
/// per workspace (`diagnostics`, `diagnostics#2`, …).
pub fn with_on_every_workspace(
    tiles: &[Tile],
    instances: &[&'static str],
    extra_min: (u16, u16),
    min: impl Fn(&str) -> (u16, u16),
) -> Vec<Tile> {
    let base = |id: &str| crate::instance::base(id).to_string();
    let extra_base = instances.first().map(|i| base(i)).unwrap_or_default();
    let mut out: Vec<Tile> = tiles.to_vec();
    let workspaces = tiles.iter().map(|t| t.ws).max().map_or(0, |m| m + 1);
    let mut next = instances
        .iter()
        .filter(|i| !tiles.iter().any(|t| t.surface == **i))
        .copied();
    for ws in 0..workspaces {
        let here: Vec<usize> = (0..out.len())
            .filter(|&i| out[i].ws == ws && !out[i].float)
            .collect();
        if here.is_empty() || out.iter().any(|t| t.ws == ws && base(t.surface) == extra_base) {
            continue;
        }
        let Some(id) = next.next() else { break };
        // (spare, index, beside): the split leaving the most spare room
        // of the window it shrinks, on the reference screen first.
        let mut best: Option<(f32, usize, bool)> = None;
        for screen in [ROOM, (2400.0, 1326.0)] {
            for &i in &here {
                let t = out[i];
                let m = min(t.surface);
                for beside in [true, false] {
                    let (len, px) = if beside { (t.w, screen.0) } else { (t.h, screen.1) };
                    let need = if beside { extra_min.0 } else { extra_min.1 } as f32 + GAP;
                    let cut = need / px;
                    let (w1, h1, w2, h2) = if beside {
                        (t.w - cut, t.h, cut, t.h)
                    } else {
                        (t.w, t.h - cut, t.w, cut)
                    };
                    if cut < len && fits(w1, h1, m, screen) && fits(w2, h2, extra_min, screen) {
                        let left = if beside { w1 * screen.0 - m.0 as f32 } else { h1 * screen.1 - m.1 as f32 };
                        if best.is_none_or(|b| left > b.0) {
                            best = Some((left, i, beside));
                        }
                    }
                }
            }
            if best.is_some() {
                break;
            }
        }
        // Nowhere it fits: the workspace narrows and the window takes a
        // full-height column on the right (never a sliver of one window).
        let Some((_, i, beside)) = best else {
            let col = ((extra_min.0 as f32 + GAP) / ROOM.0).min(0.5);
            for &j in &here {
                out[j].x *= 1.0 - col;
                out[j].w *= 1.0 - col;
            }
            out.push(Tile { ws, surface: id, x: 1.0 - col, y: 0.0, w: col, h: 1.0, float: false });
            continue;
        };
        let t = out[i];
        let need = if beside { extra_min.0 } else { extra_min.1 } as f32 + GAP;
        let px = if beside { ROOM.0 } else { ROOM.1 };
        let len = if beside { t.w } else { t.h };
        let frac = (need / px / len).min(0.5);
        let slice = if beside {
            out[i].w = t.w * (1.0 - frac);
            Tile { x: t.x + out[i].w, w: t.w * frac, surface: id, ..t }
        } else {
            out[i].h = t.h * (1.0 - frac);
            Tile { y: t.y + out[i].h, h: t.h * frac, surface: id, ..t }
        };
        out.push(slice);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_a_three_pane_recipe() {
        let r = LayoutRecipe {
            id: "t",
            name: "T",
            summary: "",
            status: LayoutStatus::Available,
            tiles: vec![
                tile("graph", 0.0, 0.0, 0.6, 1.0),
                tile("mixer", 0.6, 0.0, 0.4, 0.5),
                tile("meters", 0.6, 0.5, 0.4, 0.5),
            ],
        };
        let plan = r.plan().unwrap();
        assert_eq!(plan[0], Step::Open("graph"));
        assert!(plan.contains(&Step::Preselect('r')));
        assert!(plan.contains(&Step::Preselect('d')));
        assert_eq!(
            plan.iter().filter(|s| matches!(s, Step::Open(_))).count(),
            3
        );
    }

    /// Rows resize on their own: a window lined up with the one above it
    /// is not put in its column (the user, 2026-10-24: widening the
    /// Visualiser widened Deck A).
    #[test]
    fn rows_split_before_columns() {
        let r = LayoutRecipe {
            id: "t",
            name: "T",
            summary: "",
            status: LayoutStatus::Available,
            tiles: vec![
                tile("deck-a", 0.0, 0.0, 0.36, 0.55),
                tile("deck-b", 0.36, 0.0, 0.64, 0.55),
                tile("visualiser", 0.0, 0.55, 0.36, 0.45),
                tile("dj", 0.36, 0.55, 0.64, 0.45),
            ],
        };
        let plan = r.plan().unwrap();
        // The first split stacks the rows: the Visualiser opens below Deck A.
        let at = plan.iter().position(|s| matches!(s, Step::Preselect(_))).unwrap();
        assert_eq!(plan[at], Step::Preselect('d'));
        assert_eq!(plan[at + 1], Step::Open("visualiser"));
    }

    #[test]
    fn plans_across_workspaces_skipping_planned_tiles() {
        let r = LayoutRecipe {
            id: "all",
            name: "All",
            summary: "",
            status: LayoutStatus::Available,
            tiles: vec![
                tile_on(0, "a", 0.0, 0.0, 0.5, 1.0),
                tile_on(0, "b", 0.5, 0.0, 0.5, 1.0),
                tile_on(1, "c", 0.0, 0.0, 0.5, 1.0),
                tile_on(1, "planned", 0.5, 0.0, 0.5, 1.0),
                tile_on(2, "planned2", 0.0, 0.0, 1.0, 1.0),
            ],
        };
        assert_eq!(r.workspaces(), 3);
        let plan = r.plan_open(|s| !s.starts_with("planned")).unwrap();
        assert_eq!(
            plan.iter().filter(|s| **s == Step::NextWorkspace).count(),
            1,
            "workspace 3 has nothing to open"
        );
        let opens: Vec<_> = plan
            .iter()
            .filter_map(|s| {
                if let Step::Open(o) = s {
                    Some(*o)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(opens, ["a", "b", "c"]);
    }

    #[test]
    fn a_placed_window_is_not_a_tiled_open() {
        let tiled = vec![Step::Open("a"), Step::Preselect('r'), Step::Open("b")];
        assert!(opens_tiled(&tiled));
        assert!(opens_tiled(&[Step::Open("solo")]));
        let floating = vec![
            Step::Open("a"),
            Step::Place { surface: "a", x: 0.1, y: 0.1, w: 0.4, h: 0.4 },
            Step::NextWorkspace,
            Step::Open("b"),
            Step::Preselect('d'),
        ];
        assert!(!opens_tiled(&floating));
        assert!(opens_tiled(&floating[3..]));
    }

    #[test]
    fn a_captured_arrangement_replays_and_matches() {
        let r = LayoutRecipe {
            id: "t",
            name: "T",
            summary: "",
            status: LayoutStatus::Available,
            tiles: vec![
                tile_on(0, "a", 0.0, 0.0, 0.36, 0.55),
                tile_on(0, "b", 0.36, 0.0, 0.64, 0.55),
                tile_on(0, "c", 0.0, 0.55, 1.0, 0.45),
                tile_on(1, "d", 0.0, 0.0, 1.0, 1.0),
                float_on(1, "e", 0.3, 0.2, 0.4, 0.5),
            ],
        };
        let expected = r.arranged(|_| true);
        // On screen: gaps between the tiles, and a margin around them.
        let screen: Vec<Tile> = expected
            .iter()
            .map(|t| {
                if t.float {
                    *t
                } else {
                    Tile {
                        x: 0.01 + t.x * 0.98 + 0.003,
                        y: 0.01 + t.y * 0.98 + 0.003,
                        w: t.w * 0.98 - 0.006,
                        h: t.h * 0.98 - 0.006,
                        ..*t
                    }
                }
            })
            .collect();
        let captured = capture(&screen);
        assert!(same_arrangement(&expected, &captured), "{captured:?}");
        let saved = LayoutRecipe {
            tiles: captured,
            ..r.clone()
        };
        let plan = saved.plan().expect("a captured arrangement replays");
        assert!(plan.contains(&Step::Place {
            surface: "e",
            x: 0.3,
            y: 0.2,
            w: 0.4,
            h: 0.5
        }));
        // A window moved elsewhere is a change.
        let mut moved = screen.clone();
        moved[0].w *= 0.5;
        assert!(!same_arrangement(&expected, &capture(&moved)));
    }

    #[test]
    fn pack_gives_every_window_its_minimum() {
        let units: Vec<(&'static str, (u16, u16))> = vec![
            ("mixer", (640, 380)), ("graph", (640, 420)), ("meters", (560, 460)),
            ("eq", (860, 460)), ("player", (560, 400)), ("recorder", (560, 380)),
            ("devices", (420, 300)), ("analyzer", (720, 400)), ("perf", (900, 560)),
        ];
        let tiles = pack(&units);
        assert_eq!(tiles.len(), units.len(), "every Unit once");
        for t in &tiles {
            let m = units.iter().find(|u| u.0 == t.surface).unwrap().1;
            assert!(fits(t.w, t.h, m, ROOM), "{} gets {}×{}", t.surface, t.w * ROOM.0, t.h * ROOM.1);
        }
        let recipe = LayoutRecipe { id: "pro", name: "Pro", summary: "", status: LayoutStatus::Available, tiles };
        assert!(recipe.workspaces() >= 2, "nine Units don't fit one 1080p workspace");
        assert!(recipe.plan_open(|_| true).is_some(), "it tiles");
        let big = pack_on(&units, (2400.0, 1326.0));
        let ws = |t: &[Tile]| t.iter().map(|t| t.ws).max().unwrap() + 1;
        assert!(ws(&big) <= ws(&recipe.tiles), "a larger screen needs no more workspaces");
    }

    #[test]
    fn every_workspace_gets_the_extra_window_with_room() {
        let tiles = vec![
            tile_on(0, "mixer", 0.0, 0.0, 1.0, 0.55),
            tile_on(0, "graph", 0.0, 0.55, 0.6, 0.45),
            tile_on(0, "player", 0.6, 0.55, 0.4, 0.45),
            tile_on(1, "visualiser", 0.0, 0.0, 1.0, 1.0),
        ];
        let mins = |id: &str| match id {
            "mixer" => (640, 380),
            "graph" => (640, 420),
            "player" => (560, 400),
            _ => (320, 200),
        };
        let out = with_on_every_workspace(&tiles, &["diagnostics", "diagnostics#2"], (480, 360), mins);
        assert_eq!(out.len(), 6);
        for ws in 0..2 {
            assert_eq!(out.iter().filter(|t| t.ws == ws && t.surface.starts_with("diagnostics")).count(), 1);
        }
        for t in &out {
            let m = if t.surface.starts_with("diagnostics") { (480, 360) } else { mins(t.surface) };
            assert!(fits(t.w, t.h, m, ROOM), "{} gets {:.0}×{:.0}", t.surface, t.w * ROOM.0, t.h * ROOM.1);
        }
        // The mixer (most spare) gave the room; it still tiles.
        let recipe = LayoutRecipe { id: "x", name: "", summary: "", status: LayoutStatus::Available, tiles: out };
        assert!(recipe.plan_open(|_| true).is_some());
    }
}
