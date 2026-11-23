//! A look per song: a palette, a few backdrops, a few text animations and
//! how hard the kick hits. Chosen when a song comes in (from its title, so
//! the same song keeps its look within a set) and never the last song's:
//! every track looks different.

use crate::stage::{Act, Backdrop};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub name: &'static str,
    /// The ground, almost black, tinted.
    pub bg: [u8; 3],
    /// Main, second and highlight colours.
    pub a: [u8; 3],
    pub b: [u8; 3],
    pub c: [u8; 3],
}

impl Palette {
    /// Along a → b → c for t in 0…1.
    pub fn at(&self, t: f32) -> [u8; 3] {
        let t = t.clamp(0.0, 1.0);
        if t < 0.5 {
            mix(self.a, self.b, t * 2.0)
        } else {
            mix(self.b, self.c, t * 2.0 - 1.0)
        }
    }

    pub fn hex(c: [u8; 3]) -> String {
        format!("{:02x}{:02x}{:02x}", c[0], c[1], c[2])
    }
}

pub fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|k| (a[k] as f32 * (1.0 - t) + b[k] as f32 * t) as u8)
}

/// Curated schemes.
pub const PALETTES: &[Palette] = &[
    Palette {
        name: "neon",
        bg: [6, 4, 14],
        a: [255, 40, 200],
        b: [40, 220, 255],
        c: [255, 255, 255],
    },
    Palette {
        name: "sunset",
        bg: [14, 5, 8],
        a: [255, 90, 40],
        b: [255, 40, 120],
        c: [255, 210, 120],
    },
    Palette {
        name: "ice",
        bg: [3, 8, 14],
        a: [60, 160, 255],
        b: [170, 230, 255],
        c: [255, 255, 255],
    },
    Palette {
        name: "acid",
        bg: [5, 10, 3],
        a: [140, 255, 30],
        b: [255, 250, 60],
        c: [255, 255, 255],
    },
    Palette {
        name: "fire",
        bg: [12, 3, 2],
        a: [255, 50, 20],
        b: [255, 150, 20],
        c: [255, 240, 150],
    },
    Palette {
        name: "ocean",
        bg: [2, 9, 12],
        a: [0, 180, 170],
        b: [30, 110, 255],
        c: [200, 255, 250],
    },
    Palette {
        name: "vapor",
        bg: [10, 5, 16],
        a: [255, 110, 200],
        b: [120, 90, 255],
        c: [110, 255, 240],
    },
    Palette {
        name: "gold",
        bg: [10, 8, 3],
        a: [255, 190, 40],
        b: [255, 230, 150],
        c: [255, 255, 255],
    },
    Palette {
        name: "blood",
        bg: [8, 1, 2],
        a: [220, 10, 30],
        b: [255, 60, 60],
        c: [255, 220, 220],
    },
    Palette {
        name: "ultraviolet",
        bg: [6, 2, 14],
        a: [150, 40, 255],
        b: [255, 50, 255],
        c: [230, 200, 255],
    },
    Palette {
        name: "matrix",
        bg: [1, 8, 3],
        a: [0, 200, 60],
        b: [120, 255, 140],
        c: [230, 255, 230],
    },
    Palette {
        name: "mono",
        bg: [6, 6, 6],
        a: [150, 150, 150],
        b: [220, 220, 220],
        c: [255, 255, 255],
    },
];

/// The Omarchy theme's accent as a scheme: its hue, a neighbour and white.
pub fn from_theme(accent: [u8; 3], bright: [u8; 3], bg: [u8; 3], turn: f32) -> Palette {
    let (h, s, v) = hsv(accent);
    Palette {
        name: "theme",
        bg,
        a: accent,
        b: rgb((h + turn).rem_euclid(1.0), s, v.max(0.8)),
        c: bright,
    }
}

fn hsv(c: [u8; 3]) -> (f32, f32, f32) {
    let [r, g, b] = c.map(|v| v as f32 / 255.0);
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let d = mx - mn;
    let h = if d == 0.0 {
        0.0
    } else if mx == r {
        ((g - b) / d).rem_euclid(6.0) / 6.0
    } else if mx == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, if mx == 0.0 { 0.0 } else { d / mx }, mx)
}

fn rgb(h: f32, s: f32, v: f32) -> [u8; 3] {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    [r, g, b].map(|x| (x * 255.0) as u8)
}

/// One song's look.
#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    pub palette: Palette,
    pub backdrops: Vec<Backdrop>,
    pub acts: Vec<Act>,
    /// The motion's character: a ttfx easing (springy for a banger,
    /// smooth for a slow one).
    pub ease: ttfx::utils::easing::Easing,
    /// The song's type: three picks of 0 (our block letters) or 1…=n (a
    /// system face), and how often a scene draws it hollow, dotted or
    /// slanted.
    pub fonts: [u8; 3],
    pub fancy: f32,
    /// How hard the kick hits the picture (0.5…1.5).
    pub punch: f32,
    pub glow: f32,
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

struct Pick(u64);

impl Pick {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }

    fn unit(&mut self) -> f32 {
        (self.next() % 10_000) as f32 / 10_000.0
    }

    /// `n` of `from`, avoiding `avoid` while there are others.
    fn some<T: Copy + PartialEq>(&mut self, from: &[T], n: usize, avoid: &[T]) -> Vec<T> {
        let mut pool: Vec<T> = from
            .iter()
            .copied()
            .filter(|x| !avoid.contains(x))
            .collect();
        if pool.len() < n {
            pool = from.to_vec();
        }
        let mut out = vec![];
        while out.len() < n && !pool.is_empty() {
            let i = self.next() as usize % pool.len();
            out.push(pool.remove(i));
        }
        out
    }
}

/// How laid back a song's show is, from its tempo: 1 at 88 BPM and
/// slower, 0 from 118 up (unknown tempo: 0).
pub fn calmness(bpm: f64) -> f32 {
    if bpm <= 0.0 {
        0.0
    } else {
        ((118.0 - bpm) / 30.0).clamp(0.0, 1.0) as f32
    }
}

/// How laid back a track's show is, 0…1: slow ([`calmness`]), or a song
/// rather than a club track — a voice most of the way (`voice`, the share
/// of the track with one) over no steady four-on-the-floor kick (`kick`,
/// the grid's kick coverage and jitter in ms). Unmeasured: tempo only.
/// Measured on the library: 88 % of Omarchy Radio and 77 % of a pop chart
/// come out laid back, 0–5 % of the house, tech house and mainstage
/// collections (1 of 20 vocal house tracks).
pub fn laid_back(bpm: f64, voice: Option<f32>, kick: Option<(f32, f32)>) -> f32 {
    let steady = kick.map_or(0.0, |(coverage, jitter_ms)| {
        ((coverage - 0.45) / 0.3).clamp(0.0, 1.0) * ((2.5 - jitter_ms) / 1.5).clamp(0.0, 1.0)
    });
    let song = voice.map_or(0.0, |v| ((v - 0.5) / 0.25).clamp(0.0, 1.0)) * (1.0 - steady);
    calmness(bpm).max(song)
}

/// The look for `title`, unlike `previous`. A laid-back song (`calm`, see
/// [`laid_back`]) gets calm backdrops, gentle acts and a soft kick.
pub fn for_song(title: &str, previous: Option<&Pack>, theme: Palette, calm: f32) -> Pack {
    let calm = calm > 0.5;
    let mut p = Pick(hash(title) ^ 0x5eed);
    let mut palettes: Vec<Palette> = PALETTES.to_vec();
    palettes.push(theme);
    let last = previous.map(|x| x.palette.name);
    let options: Vec<Palette> = palettes
        .iter()
        .copied()
        .filter(|x| Some(x.name) != last)
        .collect();
    let palette = options[p.next() as usize % options.len()];
    let calm_backdrops: Vec<Backdrop> =
        Backdrop::ALL.iter().copied().filter(|b| b.calm()).collect();
    // (Visual, no text, comes by chance in any song: not in the pack.)
    let gentle_acts: Vec<Act> = Act::ALL
        .iter()
        .copied()
        .filter(|a| !a.big() && *a != Act::Visual)
        .collect();
    let all_acts: Vec<Act> = Act::ALL
        .iter()
        .copied()
        .filter(|a| *a != Act::Visual)
        .collect();
    let (bpool, apool): (&[Backdrop], &[Act]) = if calm {
        (&calm_backdrops, &gentle_acts)
    } else {
        (Backdrop::ALL, &all_acts)
    };
    let backdrops = p.some(bpool, 6, previous.map_or(&[][..], |x| &x.backdrops[..]));
    let acts = p.some(apool, 6, previous.map_or(&[][..], |x| &x.acts[..]));
    use ttfx::utils::easing::Easing as E;
    let (eases, punch, glow): (&[E], f32, f32) = if calm {
        (
            &[
                E::InOutSine,
                E::OutSine,
                E::InOutQuad,
                E::OutCubic,
                E::InOutCirc,
            ],
            0.2 + 0.3 * p.unit(),
            0.35 + 0.3 * p.unit(),
        )
    } else {
        (
            &[
                E::OutBack,
                E::OutElastic,
                E::OutBounce,
                E::OutExpo,
                E::OutQuint,
                E::InOutBack,
                E::OutCirc,
            ],
            0.6 + 0.8 * p.unit(),
            0.45 + 0.5 * p.unit(),
        )
    };
    let ease = eases[p.next() as usize % eases.len()];
    // Our block letters or one of up to ten system faces.
    let fonts = [0, 0, 0].map(|_| (p.next() % 11) as u8);
    let fancy = 0.15 + 0.35 * p.unit();
    Pack {
        palette,
        backdrops,
        acts,
        ease,
        fonts,
        fancy,
        punch,
        glow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_song_its_own_look_never_the_last_ones() {
        let theme = from_theme([230, 70, 80], [255, 255, 255], [10, 10, 12], 0.08);
        let a = for_song("One", None, theme, 0.0);
        assert_eq!(
            a,
            for_song("One", None, theme, 0.0),
            "the same song, the same look"
        );
        let b = for_song("Two", Some(&a), theme, 0.0);
        assert_ne!(a.palette.name, b.palette.name);
        assert!(
            b.backdrops.iter().all(|x| !a.backdrops.contains(x)),
            "{:?} vs {:?}",
            a.backdrops,
            b.backdrops
        );
        assert!(b.acts.iter().all(|x| !a.acts.contains(x)));
        assert_eq!((b.backdrops.len(), b.acts.len()), (6, 6));
    }

    #[test]
    fn a_slow_song_is_laid_back() {
        let theme = from_theme([230, 70, 80], [255, 255, 255], [10, 10, 12], 0.08);
        for title in ["Slow One", "Ballad", "Late Night", "Sunday"] {
            let p = for_song(title, None, theme, calmness(84.0));
            assert!(
                p.backdrops.iter().all(|b| b.calm()),
                "{title}: {:?}",
                p.backdrops
            );
            assert!(p.acts.iter().all(|a| !a.big()), "{title}: {:?}", p.acts);
            assert!(p.punch <= 0.5);
        }
        assert_eq!(calmness(128.0), 0.0);
        assert_eq!(calmness(80.0), 1.0);
    }

    #[test]
    fn a_song_is_laid_back_a_club_track_is_not() {
        // Omarchy Radio's "You Never Ask Me Why": 132 BPM, a voice in 78 %.
        assert!(laid_back(132.0, Some(0.78), Some((0.38, 3.6))) > 0.9);
        // The same voice over a steady four-on-the-floor kick: vocal house.
        assert_eq!(laid_back(126.0, Some(0.78), Some((0.8, 0.5))), 0.0);
        // Tech house: little voice.
        assert_eq!(laid_back(126.0, Some(0.27), Some((0.52, 0.7))), 0.0);
        // Not measured: the tempo decides.
        assert_eq!(laid_back(128.0, None, None), 0.0);
        assert_eq!(laid_back(80.0, None, None), 1.0);
    }
}
