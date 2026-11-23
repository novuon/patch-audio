//! The show's own animations, in character cells: backdrops that move with
//! the beat and the spectrum, and acts that play the song's title (and
//! artist), their motion along ttfx's easing curves. All of them are pure
//! functions of the moment ([`Ctx`]): no state, the same beat draws the
//! same frame. (ttfx's stock effects, Omarchy's screensaver, don't play.)

use crate::grid::{Cell, Grid};
use crate::pack::{Palette, mix};

mod backdrops;
use backdrops::*;
mod acts;
mod modern;
use acts::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backdrop {
    Ambient(&'static str),
    SunsetDrive,
    Jellyfish,
    EventHorizon,
    Rings,
    Tunnel,
    Warp,
    Skyline,
    Plasma,
    Spectrum,
    Horizon,
    Lasers,
    Checker,
    Spiral,
    Glyphfall,
    Ripple,
    Chase,
    Fire,
    Burst,
    Scope,
    Dots,
    Stars,
    Lines,
    Grid,
    Orbits,
    Kaleido,
    Radial,
    Rainfall,
    Wavefront,
    Hexes,
    Aurora,
    Vortex,
    Prism,
}

impl Backdrop {
    pub const AMBIENT: &'static [Backdrop] = &[Self::SunsetDrive, Self::Jellyfish, Self::EventHorizon, Self::Ambient("neon-city"), Self::Ambient("vector-highway"), Self::Ambient("circuit-run"), Self::Ambient("eye-reactive"), Self::Ambient("matrix-reactive"), Self::Ambient("tunnel-reactive"), Self::Ambient("live-terminal"), Self::Ambient("data-globe"), Self::Ambient("aquarium"), Self::Ambient("cockpit"), Self::Ambient("sand-clock"), Self::Ambient("sand-clock-chladni"), Self::Ambient("sand-clock-dunes"), Self::Ambient("sand-clock-galaxy"), Self::Ambient("sand-clock-ferrofluid"), Self::Ambient("sand-clock-excavation"), Self::Ambient("sand-clock-zen"), Self::Ambient("sand-clock-foundry"), Self::Ambient("sand-clock-swarm"), Self::Ambient("sand-clock-shore"), Self::Ambient("sand-clock-glass"), Self::Ambient("sand-clock-dust-devil"), Self::Ambient("sand-clock-spectrum"), Self::Ambient("sand-clock-mandala"), Self::Ambient("supernova"), Self::Ambient("solar-system"), Self::Ambient("abyss"), Self::Ambient("load-canyon"), Self::Ambient("neon-avenue"), Self::Ambient("packet-grid"), Self::Ambient("silicon")];

    pub fn ambient(self) -> bool { Self::AMBIENT.contains(&self) }

    pub const ALL: &'static [Backdrop] = &[
        Backdrop::Rings,
        Backdrop::Tunnel,
        Backdrop::Warp,
        Backdrop::Skyline,
        Backdrop::Plasma,
        Backdrop::Spectrum,
        Backdrop::Horizon,
        Backdrop::Lasers,
        Backdrop::Checker,
        Backdrop::Spiral,
        Backdrop::Glyphfall,
        Backdrop::Ripple,
        Backdrop::Chase,
        Backdrop::Fire,
        Backdrop::Burst,
        Backdrop::Scope,
        Backdrop::Dots,
        Backdrop::Stars,
        Backdrop::Lines,
        Backdrop::Grid,
        Backdrop::Orbits,
        Backdrop::Kaleido,
        Backdrop::Radial,
        Backdrop::Rainfall,
        Backdrop::Wavefront,
        Backdrop::Hexes,
        Backdrop::Aurora,
        Backdrop::Vortex,
        Backdrop::Prism,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Backdrop::Ambient(name) => name,
            Backdrop::SunsetDrive => "sunset-drive",
            Backdrop::Jellyfish => "jellyfish",
            Backdrop::EventHorizon => "event-horizon",
            Backdrop::Rings => "rings",
            Backdrop::Tunnel => "tunnel",
            Backdrop::Warp => "warp",
            Backdrop::Skyline => "skyline",
            Backdrop::Plasma => "plasma",
            Backdrop::Spectrum => "spectrum",
            Backdrop::Horizon => "horizon",
            Backdrop::Lasers => "lasers",
            Backdrop::Checker => "checker",
            Backdrop::Spiral => "spiral",
            Backdrop::Glyphfall => "glyphfall",
            Backdrop::Ripple => "ripple",
            Backdrop::Chase => "chase",
            Backdrop::Fire => "fire",
            Backdrop::Burst => "burst",
            Backdrop::Scope => "scope",
            Backdrop::Dots => "dots",
            Backdrop::Stars => "stars",
            Backdrop::Lines => "lines",
            Backdrop::Grid => "grid",
            Backdrop::Orbits => "orbits",
            Backdrop::Kaleido => "kaleido",
            Backdrop::Radial => "radial",
            Backdrop::Rainfall => "rainfall",
            Backdrop::Wavefront => "wavefront",
            Backdrop::Hexes => "hexes",
            Backdrop::Aurora => "aurora",
            Backdrop::Vortex => "vortex",
            Backdrop::Prism => "prism",
        }
    }

    /// Calm enough for a breakdown.
    pub fn calm(self) -> bool {
        if let Self::Ambient(name)=self {return !matches!(name,"matrix-reactive"|"tunnel-reactive"|"sand-clock-foundry"|"sand-clock-dust-devil"|"supernova"|"neon-avenue"|"silicon");}
        matches!(
            self,
            Backdrop::SunsetDrive | Backdrop::Jellyfish | Backdrop::Aurora
                | Backdrop::Plasma
                | Backdrop::Stars
                | Backdrop::Lines
                | Backdrop::Ripple
                | Backdrop::Glyphfall
                | Backdrop::Scope
                | Backdrop::Dots
                | Backdrop::Kaleido
                | Backdrop::Rainfall
                | Backdrop::Wavefront
                | Backdrop::Orbits
        )
    }
}

/// Ways to play the title.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    /// A word per beat, huge; then the artist; then the title.
    Words,
    /// The letters burst apart on each kick and snap back.
    Shatter,
    /// A wave runs through the letters, as tall as the bass.
    Wave,
    /// Slices jump sideways on the 16ths, glyphs flicker with the highs.
    Glitch,
    /// Each beat the next word grows from nothing to full size.
    Zoom,
    /// Typed out a letter per 16th, with a cursor.
    Type,
    /// Outlines only, flickering like a neon tube.
    Neon,
    /// Scrolling past, a letter per beat.
    Marquee,
    /// Each letter jumps in turn, a wave of kicks.
    Bounce,
    /// The title and its reflection, rippling.
    Mirror,
    /// Pixels fall into place over two bars.
    Assemble,
    /// A bright line scans down each bar, lighting what it passes.
    Scan,
    /// The rows slide in from alternate sides, one a 16th.
    Stack,
    /// Coarse blocks that sharpen into the letters over the scene.
    Pixelate,
    /// The letters drip, drops running down from them.
    Drip,
    /// Ghost copies trail the title, one per beat, fading.
    Echo,
    /// The top and bottom halves pull apart on the kick and rejoin.
    Split,
    /// Title and artist roll up like credits.
    Credits,
    /// The letters flip edge-on and back on every beat.
    Flip,
    /// Dark letters, lit where a moving spotlight passes.
    Spotlight,
    /// The title repeated small over the whole screen, rows scrolling
    /// opposite ways.
    Tile,
    /// The title's letters circling the centre.
    Orbit,
    /// The title in columns, top to bottom, falling into place.
    Vertical,
    /// Words popping up at random places and sizes, one a beat.
    Scatter,
    /// Lines of small text running at different speeds.
    Ticker,
    /// One letter of the title per beat, filling the screen.
    Letter,
    /// Glyph noise in red resolving letter by letter into clean type.
    Decode,
    /// Letters rising in turn from behind a baseline over a red rule.
    Rise,
    /// A grey title with a band of light sweeping across it each bar.
    Sheen,
    /// Three bands wiped in from alternating sides behind a red edge.
    Slice,
    /// No text: the backdrop is the show, at full brightness.
    Visual,
}

impl Act {
    pub const ALL: &'static [Act] = &[
        Act::Words,
        Act::Shatter,
        Act::Wave,
        Act::Glitch,
        Act::Zoom,
        Act::Type,
        Act::Neon,
        Act::Marquee,
        Act::Bounce,
        Act::Mirror,
        Act::Assemble,
        Act::Scan,
        Act::Stack,
        Act::Pixelate,
        Act::Drip,
        Act::Echo,
        Act::Split,
        Act::Credits,
        Act::Flip,
        Act::Spotlight,
        Act::Tile,
        Act::Orbit,
        Act::Vertical,
        Act::Scatter,
        Act::Ticker,
        Act::Letter,
        Act::Decode,
        Act::Rise,
        Act::Sheen,
        Act::Slice,
        Act::Visual,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Act::Words => "words",
            Act::Shatter => "shatter",
            Act::Wave => "wave",
            Act::Glitch => "glitch",
            Act::Zoom => "zoom",
            Act::Type => "type",
            Act::Neon => "neon",
            Act::Marquee => "marquee",
            Act::Bounce => "bounce",
            Act::Mirror => "mirror",
            Act::Assemble => "assemble",
            Act::Scan => "scan",
            Act::Stack => "stack",
            Act::Pixelate => "pixelate",
            Act::Drip => "drip",
            Act::Echo => "echo",
            Act::Split => "split",
            Act::Credits => "credits",
            Act::Flip => "flip",
            Act::Spotlight => "spotlight",
            Act::Tile => "tile",
            Act::Orbit => "orbit",
            Act::Vertical => "vertical",
            Act::Scatter => "scatter",
            Act::Ticker => "ticker",
            Act::Letter => "letter",
            Act::Decode => "decode",
            Act::Rise => "rise",
            Act::Sheen => "sheen",
            Act::Slice => "slice",
            Act::Visual => "visual",
        }
    }

    /// Hits hard: for lifts and drops.
    pub fn big(self) -> bool {
        matches!(
            self,
            Act::Words
                | Act::Shatter
                | Act::Zoom
                | Act::Glitch
                | Act::Bounce
                | Act::Split
                | Act::Flip
                | Act::Stack
                | Act::Scatter
                | Act::Letter
        )
    }
}

/// The moment everything is drawn for.
#[derive(Clone, Copy, Debug)]
pub struct Ctx<'a> {
    pub cols: usize,
    pub rows: usize,
    /// Beats (with no grid: seconds × 2).
    pub beat: f64,
    /// Real musical beat, independent of the animation clock.
    pub music_beat: f64,
    /// Seconds.
    pub t: f64,
    /// Beats since the scene began, and how many it lasts.
    pub age: f64,
    pub span: f64,
    /// Loudness and the three bands, 0…1.
    pub energy: f32,
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
    /// The spectrum, low to high, 0…1 each.
    pub bands: &'a [f32],
    /// Recent loudness, oldest first.
    pub history: &'a [f32],
    pub pal: &'a Palette,
    pub title: &'a str,
    pub artist: &'a str,
    pub seed: u64,
    /// The song's motion curve (ttfx's easing functions).
    pub ease: ttfx::utils::easing::Easing,
    /// How big the title plays (0 small, 1 normal, 2 as big as it goes)
    /// and where (0 centre, 1 upper, 2 lower third).
    pub size: u8,
    pub place: u8,
    /// Backdrop brightness: 1 behind a title, more when it is the show.
    pub bright: f32,
    /// The type: 0 our block letters, n a system face (n − 1 in `fonts`),
    /// and how its pixels are drawn (0 solid, 1 outline, 2 dots, 3 slanted).
    pub fonts: Option<&'a crate::fonts::Fonts>,
    pub font: u8,
    pub style: u8,
    /// How much the beat hits (0 silent or no kick … 1): the kick's
    /// pulses scale with it; the motion goes on.
    pub gate: f32,
}

impl Ctx<'_> {
    fn into(&self) -> f32 {
        (if self.gate > 0.5 {
            self.music_beat
        } else {
            self.beat
        })
        .rem_euclid(1.0) as f32
    }

    /// 1 on the beat, falling fast (nothing without a kick heard).
    fn kick(&self) -> f32 {
        (1.0 - self.music_beat.rem_euclid(1.0) as f32).powi(3) * self.gate
    }

    /// `p` (0…1) along the song's easing curve (may overshoot: back,
    /// elastic).
    fn eased(&self, p: f32) -> f32 {
        self.ease.ease(p.clamp(0.0, 1.0) as f64) as f32
    }

    /// Which 16th of the beat.
    fn sixteenth(&self) -> i64 {
        ((if self.gate > 0.5 {
            self.music_beat
        } else {
            self.beat
        }) * 4.0)
            .floor() as i64
    }
}

const SHADES: [char; 5] = [' ', '·', '░', '▒', '▓'];

fn shade(i: f32) -> char {
    SHADES[(i.clamp(0.0, 0.999) * SHADES.len() as f32) as usize]
}

fn scale(c: [u8; 3], k: f32) -> [u8; 3] {
    c.map(|v| (v as f32 * k.clamp(0.0, 1.0)) as u8)
}

fn hash(x: u64) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h & 0xFFFF) as f32 / 65535.0
}

/// Cells about twice as tall as wide: x and y from the centre, in cell
/// widths, and the distance normalised to the corner.
fn polar(c: &Ctx, x: usize, y: usize) -> (f32, f32, f32) {
    let (cx, cy) = (c.cols as f32 / 2.0, c.rows as f32 / 2.0);
    let (dx, dy) = (x as f32 + 0.5 - cx, (y as f32 + 0.5 - cy) * 2.0);
    let maxr = (cx * cx + (cy * 2.0) * (cy * 2.0)).sqrt();
    (dx, dy, (dx * dx + dy * dy).sqrt() / maxr)
}

/// Light a backdrop cell: intensity i (0…1) and a place along the palette.
fn lit(g: &mut Grid, c: &Ctx, x: usize, y: usize, i: f32, along: f32) {
    let i = (i * (0.55 + 0.45 * c.energy) * c.bright).min(1.0);
    if i > 0.04 && x < c.cols && y < c.rows {
        let col = scale(c.pal.at(along), (0.3 + 0.55 * i) * c.bright.min(1.25));
        g.cells[y * c.cols + x] = Cell {
            ch: shade(i),
            fg: Some(col),
            bg: None,
        };
    }
}

fn band(c: &Ctx, f: f32) -> f32 {
    if c.bands.is_empty() {
        return c.energy;
    }
    let i = (f.clamp(0.0, 0.999) * c.bands.len() as f32) as usize;
    c.bands[i]
}

/// Paint `kind`, kept dim so the title stands out.
pub fn backdrop(kind: Backdrop, c: &Ctx) -> Grid {
    if kind.ambient() { return crate::ambient::Scenes::default().draw(kind, c, None); }
    let (cols, rows) = (c.cols, c.rows);
    let mut g = Grid::blank(cols, rows);
    match kind {
        Backdrop::Ambient(_) | Backdrop::SunsetDrive | Backdrop::Jellyfish | Backdrop::EventHorizon => unreachable!("Ambient is stateful"),
        Backdrop::Aurora => backdrop_aurora(&mut g, c),
        Backdrop::Vortex => backdrop_vortex(&mut g, c),
        Backdrop::Prism => backdrop_prism(&mut g, c),
        Backdrop::Rings => backdrop_rings(&mut g, c),
        Backdrop::Tunnel => backdrop_tunnel(&mut g, c),
        Backdrop::Warp => backdrop_warp(&mut g, c),
        Backdrop::Skyline => backdrop_skyline(&mut g, c),
        Backdrop::Plasma => backdrop_plasma(&mut g, c),
        Backdrop::Spectrum => backdrop_spectrum(&mut g, c),
        Backdrop::Horizon => backdrop_horizon(&mut g, c),
        Backdrop::Lasers => backdrop_lasers(&mut g, c),
        Backdrop::Checker => backdrop_checker(&mut g, c),
        Backdrop::Spiral => backdrop_spiral(&mut g, c),
        Backdrop::Glyphfall => backdrop_glyphfall(&mut g, c),
        Backdrop::Ripple => backdrop_ripple(&mut g, c),
        Backdrop::Chase => backdrop_chase(&mut g, c),
        Backdrop::Fire => backdrop_fire(&mut g, c),
        Backdrop::Burst => backdrop_burst(&mut g, c),
        Backdrop::Scope => backdrop_scope(&mut g, c),
        Backdrop::Dots => backdrop_dots(&mut g, c),
        Backdrop::Stars => backdrop_stars(&mut g, c),
        Backdrop::Lines => backdrop_lines(&mut g, c),
        Backdrop::Grid => backdrop_grid(&mut g, c),
        Backdrop::Orbits => backdrop_orbits(&mut g, c),
        Backdrop::Kaleido => backdrop_kaleido(&mut g, c),
        Backdrop::Radial => backdrop_radial(&mut g, c),
        Backdrop::Rainfall => backdrop_rainfall(&mut g, c),
        Backdrop::Wavefront => backdrop_wavefront(&mut g, c),
        Backdrop::Hexes => backdrop_hexes(&mut g, c),
    }
    g
}

/// The lit pixels of block-letter art ('█'), and its size.
struct Bits {
    w: usize,
    h: usize,
    on: Vec<bool>,
    /// Each cell as drawn: '█' for block letters, a 2×4 shape for type.
    shape: Vec<char>,
}

fn bits(art: &str) -> Bits {
    let lines: Vec<Vec<char>> = art.lines().map(|l| l.chars().collect()).collect();
    let (h, w) = (
        lines.len(),
        lines.iter().map(|l| l.len()).max().unwrap_or(0),
    );
    let mut on = vec![false; w * h];
    for (y, l) in lines.iter().enumerate() {
        for (x, ch) in l.iter().enumerate() {
            on[y * w + x] = *ch == '█';
        }
    }
    let shape = vec!['█'; w * h];
    Bits { w, h, on, shape }
}

impl Bits {
    /// The letters as they are drawn: runs of lit columns between empty
    /// ones, as (first column, one past the last). (Dividing the width
    /// evenly cut letters apart: reported, a title torn to pieces.)
    fn letters(&self) -> Vec<(usize, usize)> {
        let lit = |x: usize| (0..self.h).any(|y| self.on[y * self.w + x]);
        let mut out = vec![];
        let mut start = None;
        for x in 0..self.w {
            match (lit(x), start) {
                (true, None) => start = Some(x),
                (false, Some(a)) => {
                    out.push((a, x));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(a) = start {
            out.push((a, self.w));
        }
        out
    }

    fn at(&self, x: isize, y: isize) -> bool {
        x >= 0
            && y >= 0
            && (x as usize) < self.w
            && (y as usize) < self.h
            && self.on[y as usize * self.w + x as usize]
    }
}

thread_local! {
    /// Letters drawn outside the canvas (cut off), for the tests.
    static CLIPPED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn put(g: &mut Grid, x: isize, y: isize, ch: char, col: [u8; 3]) {
    if x >= 0 && y >= 0 && (x as usize) < g.cols && (y as usize) < g.rows {
        let i = y as usize * g.cols + x as usize;
        g.cells[i] = Cell {
            ch,
            fg: Some(col),
            bg: None,
        };
    } else if ch != ' ' {
        CLIPPED.with(|c| c.set(c.get() + 1));
    }
}

/// The fewest rows a line of words takes: words are letter art at least
/// a sixteenth of the canvas high. (Plain characters, one a cell, read as
/// about 10 px on a big screen: reported, an artist nobody could read.)
fn line_rows(c: &Ctx) -> usize {
    let r = (c.rows / 16).max(3);
    if c.font > 0 {
        r
    } else {
        r.max(crate::banner::H)
    }
}

/// The artist's floor: smaller than a line (it always stays under the
/// title's size), still well above a letter a cell.
fn small_rows(c: &Ctx) -> usize {
    let r = (c.rows / 28).max(3);
    if c.font > 0 {
        r
    } else {
        r.max(crate::banner::H)
    }
}

/// The artist next to a title `title_h` rows high: about half of it,
/// never under the floor; None when it can't be smaller than the title.
fn artist_art(c: &Ctx, artist: &str, cols: usize, title_h: usize) -> Option<Bits> {
    let min = small_rows(c);
    let h = (title_h / 2).clamp(min, (c.rows / 5).max(min));
    let b = art(c, artist, cols, h);
    (b.on.iter().any(|x| *x) && b.h >= min && b.h < title_h).then_some(b)
}

/// Whether letter art came out at a readable size (a line that didn't fit
/// shrinks, or falls back to plain text, which lights nothing).
fn readable(b: &Bits, c: &Ctx) -> bool {
    b.on.iter().any(|x| *x) && b.h >= line_rows(c)
}

/// `s` in letter art centred across the canvas with its top at `y`.
fn put_art(g: &mut Grid, b: &Bits, y: isize, col: impl Fn(usize) -> [u8; 3]) -> isize {
    let x0 = (g.cols as isize - b.w as isize) / 2;
    for yy in 0..b.h {
        for x in 0..b.w {
            if b.on[yy * b.w + x] {
                put(
                    g,
                    x0 + x as isize,
                    y + yy as isize,
                    b.shape[yy * b.w + x],
                    col(x),
                );
            }
        }
    }
    x0
}

/// A line of words scrolling sideways at row `y` (its top), `h` rows
/// high, moved `off` columns; colour by position in the line.
fn scroll_line(
    g: &mut Grid,
    c: &Ctx,
    s: &str,
    y: isize,
    h: usize,
    off: isize,
    col: impl Fn(f32) -> [u8; 3],
) {
    let b = art(c, s, usize::MAX / 4, h);
    if !b.on.iter().any(|x| *x) {
        return;
    }
    let w = b.w as isize;
    for yy in 0..b.h {
        for x in 0..g.cols as isize {
            let sx = (x + off).rem_euclid(w) as usize;
            if b.on[yy * b.w + sx] {
                put(
                    g,
                    x,
                    y + yy as isize,
                    b.shape[yy * b.w + sx],
                    col(sx as f32 / b.w as f32),
                );
            }
        }
    }
}

/// The artist under the title (`top`…`below` is the title's rows), in
/// letter art about half the title's height, always smaller than it;
/// above the title when there is no room below, left out when there is
/// none at all or it can't be smaller than the title.
fn byline(g: &mut Grid, c: &Ctx, top: isize, below: isize) {
    let a = c.artist.trim().to_uppercase();
    if a.is_empty() {
        return;
    }
    let Some(b) = artist_art(
        c,
        &a,
        (c.cols as f32 * 0.85) as usize,
        (below - top).max(0) as usize,
    ) else {
        return;
    };
    let gap = (b.h as isize / 3).max(1);
    let y = if below + gap + b.h as isize <= c.rows as isize {
        below + gap
    } else if top - gap - b.h as isize >= 0 {
        top - gap - b.h as isize
    } else {
        return;
    };
    put_art(g, &b, y, |_| c.pal.b);
}

/// `text` as large as fits `cols` × `rows`, in the scene's type (a system
/// face, or our block letters when it has none or it doesn't fit).
fn art(c: &Ctx, text: &str, cols: usize, rows: usize) -> Bits {
    if c.font > 0
        && let Some(f) = c.fonts
        && let Some(a) = f.art(c.font as usize - 1, text, cols, rows)
    {
        return Bits {
            w: a.w,
            h: a.h,
            on: a.on,
            shape: a.shape,
        };
    }
    bits(&crate::banner::huge(text, cols, rows))
}

/// The title as art centred in the canvas: (art, x0, y0).
fn title_art(c: &Ctx) -> (Bits, isize, isize) {
    // Small (a third of the canvas), normal, or as big as one line goes.
    let b = if c.font > 0 {
        let (w, h) = match c.size {
            0 => (
                (c.cols as f32 * 0.45) as usize,
                (c.rows as f32 * 0.3) as usize,
            ),
            2 => (c.cols.saturating_sub(2), c.rows.saturating_sub(6)),
            _ => (c.cols.saturating_sub(6), (c.rows as f32 * 0.45) as usize),
        };
        art(c, c.title, w, h)
    } else {
        bits(&match c.size {
            0 => crate::banner::banner(
                c.title,
                "",
                (c.cols as f32 * 0.42) as usize,
                (c.rows as f32 * 0.35) as usize,
            ),
            2 => crate::banner::huge(c.title, c.cols.saturating_sub(2), c.rows.saturating_sub(6)),
            _ => crate::banner::banner(
                c.title,
                "",
                c.cols.saturating_sub(4),
                c.rows.saturating_sub(6),
            ),
        })
    };
    // Too small to draw (a narrow canvas at the small size): the normal
    // size, then block letters as big as fit (it vanished: plain text).
    let b = if b.on.iter().any(|x| *x) {
        b
    } else if c.size == 0 {
        return title_art(&Ctx { size: 1, ..*c });
    } else {
        bits(&crate::banner::huge(
            c.title,
            c.cols.saturating_sub(2),
            c.rows.saturating_sub(4),
        ))
    };
    let x0 = (c.cols as isize - b.w as isize) / 2;
    let free = c.rows as isize - b.h as isize;
    let y0 = match c.place {
        1 => free / 5,
        2 => free * 4 / 5 - 1,
        _ => free / 2 - 1,
    };
    (b, x0, y0)
}

/// Whether `kind` can show this title readably on this canvas (letters
/// on a ring that don't overlap, words in columns that fit).
fn fits(kind: Act, c: &Ctx) -> bool {
    match kind {
        Act::Orbit => {
            let n = c.title.chars().filter(|ch| !ch.is_whitespace()).count() as f32;
            // Ring circumference in cells against a block letter's width.
            let around = std::f32::consts::TAU * 0.6 * c.cols as f32 / 2.0;
            around >= n * 14.0 && c.rows >= 28
        }
        Act::Vertical => {
            c.title
                .split_whitespace()
                .all(|w| w.chars().count() * 7 <= c.rows)
                && c.title.split_whitespace().count() * 12 <= c.cols
        }
        _ => true,
    }
}

/// Play `act` (one that can't show this title readably here plays as
/// a wave).
pub fn act(kind: Act, c: &Ctx) -> Grid {
    let kind = if fits(kind, c) { kind } else { Act::Wave };
    let mut g = act_plain(kind, c);
    style(&mut g, c.style);
    g
}

/// The scene's pixel style on the letters: outline (hollow), dots, or
/// slanted rows.
fn style(g: &mut Grid, style: u8) {
    let (w, h) = (g.cols, g.rows);
    let solid = |g: &Grid, x: isize, y: isize| {
        x >= 0
            && y >= 0
            && (x as usize) < w
            && (y as usize) < h
            && crate::grid::shape_mask(g.cells[y as usize * w + x as usize].ch)
                .map_or(g.cells[y as usize * w + x as usize].ch == '█', |m| {
                    m == 0xff
                })
    };
    match style {
        // Hollow letters with shapes: the outline at the sub-pixel (lit,
        // touching an unlit one), so a curve's contour stays smooth (done
        // per cell it went in steps a cell high).
        1 if g
            .cells
            .iter()
            .any(|c| crate::grid::shape_mask(c.ch).is_some()) =>
        {
            let (sw, sh) = (w * 2, h * 4);
            let mut on = vec![false; sw * sh];
            for (i, c) in g.cells.iter().enumerate() {
                let m = if c.ch == '█' {
                    0xff
                } else {
                    crate::grid::shape_mask(c.ch).unwrap_or(0)
                };
                for bit in 0..8 {
                    if m & (1 << bit) != 0 {
                        on[((i / w) * 4 + bit / 2) * sw + (i % w) * 2 + bit % 2] = true;
                    }
                }
            }
            let lit = |x: isize, y: isize| {
                x >= 0
                    && y >= 0
                    && (x as usize) < sw
                    && (y as usize) < sh
                    && on[y as usize * sw + x as usize]
            };
            for (i, c) in g.cells.iter_mut().enumerate() {
                if !crate::grid::is_ink(c.ch) {
                    continue;
                }
                let mut m = 0u8;
                for bit in 0..8 {
                    let (x, y) = (
                        ((i % w) * 2 + bit % 2) as isize,
                        ((i / w) * 4 + bit / 2) as isize,
                    );
                    if lit(x, y)
                        && !(lit(x - 1, y) && lit(x + 1, y) && lit(x, y - 1) && lit(x, y + 1))
                    {
                        m |= 1 << bit;
                    }
                }
                c.ch = if m == 0 { ' ' } else { crate::grid::shape(m) };
            }
        }
        1 => {
            let before = g.clone();
            for y in 0..h as isize {
                for x in 0..w as isize {
                    if solid(&before, x, y)
                        && solid(&before, x - 1, y)
                        && solid(&before, x + 1, y)
                        && solid(&before, x, y - 1)
                        && solid(&before, x, y + 1)
                    {
                        g.cells[y as usize * w + x as usize].ch = ' ';
                    }
                }
            }
        }
        2 => {
            for c in g.cells.iter_mut().filter(|c| crate::grid::is_ink(c.ch)) {
                c.ch = '●';
            }
        }
        3 => {
            let before = g.clone();
            for c in g.cells.iter_mut() {
                *c = Cell {
                    ch: ' ',
                    fg: None,
                    bg: None,
                };
            }
            for y in 0..h {
                let shift = ((h / 2) as isize - y as isize) * 2 / 5;
                for x in 0..w {
                    let nx = x as isize + shift;
                    if nx >= 0 && (nx as usize) < w {
                        g.cells[y * w + nx as usize] = before.cells[y * w + x];
                    }
                }
            }
        }
        _ => {}
    }
}

fn act_plain(kind: Act, c: &Ctx) -> Grid {
    let mut g = Grid::blank(c.cols, c.rows);
    match kind {
        Act::Words => return words_grid(c, false),
        Act::Zoom => return words_grid(c, true),
        Act::Shatter => act_shatter(&mut g, c),
        Act::Wave => act_wave(&mut g, c),
        Act::Glitch => act_glitch(&mut g, c),
        Act::Type => act_type(&mut g, c),
        Act::Neon => act_neon(&mut g, c),
        Act::Marquee => act_marquee(&mut g, c),
        Act::Bounce => act_bounce(&mut g, c),
        Act::Mirror => act_mirror(&mut g, c),
        Act::Assemble => act_assemble(&mut g, c),
        Act::Scan => act_scan(&mut g, c),
        Act::Stack => act_stack(&mut g, c),
        Act::Pixelate => act_pixelate(&mut g, c),
        Act::Drip => act_drip(&mut g, c),
        Act::Echo => act_echo(&mut g, c),
        Act::Split => act_split(&mut g, c),
        Act::Credits => act_credits(&mut g, c),
        Act::Flip => act_flip(&mut g, c),
        Act::Spotlight => act_spotlight(&mut g, c),
        Act::Tile => act_tile(&mut g, c),
        Act::Orbit => act_orbit(&mut g, c),
        Act::Vertical => act_vertical(&mut g, c),
        Act::Scatter => act_scatter(&mut g, c),
        Act::Ticker => act_ticker(&mut g, c),
        Act::Letter => act_letter(&mut g, c),
        Act::Decode => modern::act_decode(&mut g, c),
        Act::Rise => modern::act_rise(&mut g, c),
        Act::Sheen => modern::act_sheen(&mut g, c),
        Act::Slice => modern::act_slice(&mut g, c),
        Act::Visual => {}
    }
    g
}

/// Words a beat: the title's words, the artist, the title. Zoom: each
/// grows from small to its full size over its beat.
fn words_grid(c: &Ctx, zoom: bool) -> Grid {
    let mut g = Grid::blank(c.cols, c.rows);
    let mut seq: Vec<String> = c.title.split_whitespace().map(String::from).collect();
    if seq.is_empty() {
        return g;
    }
    if !c.artist.trim().is_empty() {
        seq.push(c.artist.trim().to_string());
    }
    seq.push(c.title.to_string());
    let k = c.beat.max(0.0).floor() as usize % seq.len();
    let grow = if zoom {
        0.25 + 0.75 * c.eased(c.into()).max(0.0)
    } else {
        1.0
    };
    let rows = ((c.rows.saturating_sub(2)) as f32 * grow) as usize;
    let is_artist = !c.artist.trim().is_empty() && k == seq.len() - 2;
    let b = if is_artist {
        // Smaller than the whole title's beat (reported: an artist bigger
        // than its title); left out when it can't be.
        let title_h = art(c, c.title, c.cols.saturating_sub(2), rows.max(7)).h;
        match artist_art(c, &seq[k], c.cols.saturating_sub(2), title_h) {
            Some(b) => b,
            None => return g,
        }
    } else {
        art(c, &seq[k], c.cols.saturating_sub(2), rows.max(7))
    };
    let (x0, y0) = (
        (c.cols as isize - b.w as isize) / 2,
        (c.rows as isize - b.h as isize) / 2,
    );
    let col = [c.pal.a, c.pal.b, c.pal.c][k % 3];
    for y in 0..b.h {
        for x in 0..b.w {
            if b.on[y * b.w + x] {
                put(
                    &mut g,
                    x0 + x as isize,
                    y0 + y as isize,
                    b.shape[y * b.w + x],
                    col,
                );
            }
        }
    }
    g
}

/// `text` over `base`, readable: around every cell of it the backdrop is
/// cleared (two cells across, one up and down), so letters always stand
/// on dark ground.
pub fn over_readable(base: &mut Grid, text: &Grid) {
    if (base.cols, base.rows) != (text.cols, text.rows) {
        return;
    }
    let (w, h) = (base.cols as isize, base.rows as isize);
    let lit: Vec<bool> = text.cells.iter().map(|t| t.ch != ' ').collect();
    for y in 0..h {
        for x in 0..w {
            if !lit[(y * w + x) as usize] {
                continue;
            }
            for dy in -1..=1 {
                for dx in -2..=2 {
                    let (xx, yy) = (x + dx, y + dy);
                    if xx >= 0 && yy >= 0 && xx < w && yy < h && !lit[(yy * w + xx) as usize] {
                        base.cells[(yy * w + xx) as usize] = Cell {
                            ch: ' ',
                            fg: None,
                            bg: None,
                        };
                    }
                }
            }
        }
    }
    over(base, text);
}

/// Backdrops dense enough to fill a screen, and sparse ones to lay over
/// them (a visual scene is one of each).
pub fn dense(b: Backdrop) -> bool {
    matches!(
        b,
        Backdrop::Aurora
            | Backdrop::Vortex
            | Backdrop::Prism
            | Backdrop::Tunnel
            | Backdrop::Plasma
            | Backdrop::Kaleido
            | Backdrop::Fire
            | Backdrop::Hexes
            | Backdrop::Checker
            | Backdrop::Spiral
            | Backdrop::Ripple
            | Backdrop::Grid
            | Backdrop::Horizon
            | Backdrop::Rings
            | Backdrop::Spectrum
    )
}

/// A phrase change unfolds diagonally, radially or through vertical slats.
/// Sample the prior stage proportionally so font changes and resizing keep
/// the transition filling the viewport. Only two grids are needed briefly.
pub fn transition(next: &mut Grid, previous: &Grid, progress: f32, seed: u64) {
    transition_style(next,previous,progress,seed%3,seed);
}

pub fn transition_style(next: &mut Grid, previous: &Grid, progress: f32, style:u64, seed: u64) {
    if progress >= 1.0 || previous.cols == 0 || previous.rows == 0 {
        return;
    }
    let p = progress.clamp(0.0, 1.0);
    let p = p * p * (3.0 - 2.0 * p);
    for y in 0..next.rows {
        for x in 0..next.cols {
            let u = (x as f32 + 0.5) / next.cols as f32;
            let v = (y as f32 + 0.5) / next.rows as f32;
            let at = match style {
                3 => {let h=(x as u64).wrapping_mul(0x9e3779b9)^(y as u64).wrapping_mul(0x85ebca6b)^seed;((h^(h>>16)).wrapping_mul(0xc2b2ae35) as u32) as f32/u32::MAX as f32},
                0 => u * 0.65 + v * 0.35,
                1 => (((u - 0.5).powi(2) + (v - 0.5).powi(2)) * 2.0).sqrt(),
                _ => (u * 8.0).fract() * 0.7 + v * 0.3,
            };
            let alpha = ((p * 1.12 - at) / 0.12).clamp(0.0, 1.0);
            let old = previous.cells
                [(y * previous.rows / next.rows) * previous.cols + x * previous.cols / next.cols];
            let cell = &mut next.cells[y * next.cols + x];
            let incoming = *cell;
            if alpha < 0.5 {
                *cell = old;
            }
            cell.fg = match (old.fg, incoming.fg) {
                (None, None) => None,
                (a, b) => Some(mix(a.unwrap_or([0; 3]), b.unwrap_or([0; 3]), alpha)),
            };
        }
    }
}

/// `top` over `base`: every cell of `top` that shows something.
pub fn over(base: &mut Grid, top: &Grid) {
    if (base.cols, base.rows) != (top.cols, top.rows) {
        return;
    }
    for (b, t) in base.cells.iter_mut().zip(&top.cells) {
        if t.ch != ' ' || t.bg.is_some() {
            *b = *t;
        }
    }
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;
