use super::*;

#[test]
fn scene_transitions_cover_the_whole_stage_and_finish_exactly() {
    let mut incoming = Grid::blank(13, 7);
    for c in &mut incoming.cells {
        c.ch = 'N';
        c.fg = Some([200, 100, 50]);
    }
    let mut old = Grid::blank(5, 3);
    for c in &mut old.cells {
        c.ch = 'O';
        c.fg = Some([50, 100, 200]);
    }
    for seed in 0..3 {
        let mut start = incoming.clone();
        transition(&mut start, &old, 0.0, seed);
        assert!(
            start
                .cells
                .iter()
                .all(|c| c.ch == 'O' && c.fg == Some([50, 100, 200]))
        );
        let mut end = incoming.clone();
        transition(&mut end, &old, 1.0, seed);
        assert!(
            end.cells
                .iter()
                .all(|c| c.ch == 'N' && c.fg == Some([200, 100, 50]))
        );
        let mut midway = incoming.clone();
        transition(&mut midway, &old, 0.5, seed);
        assert!(midway.cells.iter().any(|c| c.ch == 'O'));
        assert!(midway.cells.iter().any(|c| c.ch == 'N'));
    }
}

#[test]
fn rhythmic_phase_follows_music_after_slow_motion() {
    let pal = crate::pack::PALETTES[0];
    let mut c = ctx(10.75, &pal, &[], &[]);
    c.music_beat = 32.125;
    assert_eq!(Ctx::into(&c), 0.125);
    assert_eq!(c.sixteenth(), 128);
    c.gate = 0.0;
    assert_eq!(Ctx::into(&c), 0.75);
    assert_eq!(c.sixteenth(), 43);
}

pub(super) fn ctx<'a>(beat: f64, pal: &'a Palette, hist: &'a [f32], bands: &'a [f32]) -> Ctx<'a> {
    Ctx {
        cols: 120,
        rows: 40,
        beat,
        music_beat: beat,
        t: beat / 2.0,
        age: beat.rem_euclid(8.0) + 2.0,
        span: 8.0,
        ease: ttfx::utils::easing::Easing::OutBack,
        size: 1,
        place: 0,
        bright: 1.0,
        fonts: None,
        font: 0,
        style: 0,
        gate: 1.0,
        energy: 0.8,
        bass: 0.7,
        mid: 0.5,
        high: 0.6,
        bands,
        history: hist,
        pal,
        title: "Tell Me Why",
        artist: "Supermode",
        seed: 3,
    }
}

#[test]
fn every_backdrop_paints_and_moves() {
    let pal = crate::pack::PALETTES[0];
    let hist = vec![0.7f32; 60];
    let bands: Vec<f32> = (0..32)
        .map(|i| 0.3 + (i as f32 * 0.7).sin().abs() * 0.6)
        .collect();
    for &kind in Backdrop::ALL {
        let (a, b) = (
            backdrop(kind, &ctx(8.05, &pal, &hist, &bands)),
            backdrop(kind, &ctx(8.55, &pal, &hist, &bands)),
        );
        let lit = a.cells.iter().filter(|c| c.ch != ' ').count();
        assert!(lit > 20, "{}: {lit} cells", kind.name());
        if !matches!(kind, Backdrop::Skyline | Backdrop::Spectrum) {
            assert!(a.cells != b.cells, "{} moves", kind.name());
        }
    }
}

#[test]
fn scattered_titles_use_their_quadrant_at_different_stage_densities() {
    let pal = crate::pack::PALETTES[1];
    for (cols, rows) in [(240, 68), (480, 136)] {
        let c = Ctx {
            cols,
            rows,
            title: "BODY SHAKE",
            artist: "",
            beat: 8.0,
            font: 0,
            style: 0,
            ..ctx(8.0, &pal, &[], &[])
        };
        let g = act(Act::Scatter, &c);
        // The current word is in the top-left quadrant at beat 8.
        let used: Vec<usize> = (0..rows / 2)
            .filter(|&y| (0..cols / 2).any(|x| g.at(x, y).ch != ' '))
            .collect();
        let height = used.last().unwrap() - used.first().unwrap() + 1;
        assert!(
            height >= rows / 5,
            "{cols}×{rows}: only {height} rows of title"
        );
    }
}

#[test]
fn every_act_shows_the_title_and_moves() {
    let pal = crate::pack::PALETTES[1];
    for &kind in Act::ALL.iter().filter(|a| **a != Act::Visual) {
        let at = |beat: f64| act(kind, &ctx(beat, &pal, &[], &[]));
        let (a, b, on_one) = (at(9.1), at(10.6), at(8.05));
        let lit = a.cells.iter().filter(|c| c.ch != ' ').count();
        // (Orbit and ticker are small text: a cell a letter.)
        assert!(lit > 20, "{}: {lit} cells", kind.name());
        assert!(
            a.cells != b.cells || a.cells != on_one.cells,
            "{} moves",
            kind.name()
        );
    }
}

/// No title leaves the canvas: every act, size, place, font and
/// style, for real titles on wide, square and tall canvases.
#[test]
fn titles_stay_on_the_canvas() {
    let fonts = crate::fonts::Fonts::load();
    let pal = crate::pack::PALETTES[0];
    let titles = [
        ("Ride", "Karen Harding"),
        ("Midnight Transit", "Kasper Lind"),
        ("Load. Ready. System.", "Novuon"),
        ("Where She Come From", "DJ Dan, Taurus"),
        ("LADIES & GENTLEMEN", "Chris Lake"),
    ];
    let canvases = [(240, 60), (160, 90), (120, 120), (80, 110), (300, 70)];
    let mut bad = vec![];
    for &(title, artist) in &titles {
        for &(cols, rows) in &canvases {
            for &kind in Act::ALL.iter().filter(|a| {
                !matches!(
                    a,
                    Act::Visual | Act::Marquee | Act::Ticker | Act::Tile | Act::Credits
                )
            }) {
                for size in 0..3u8 {
                    for place in 0..3u8 {
                        for font in 0..=fonts.len().min(10) as u8 {
                            let c = Ctx {
                                cols,
                                rows,
                                title,
                                artist,
                                size,
                                place,
                                font,
                                fonts: Some(&fonts),
                                bright: 0.7,
                                beat: 12.4,
                                age: 6.0,
                                ..ctx(12.4, &pal, &[], &[])
                            };
                            // Where the title art itself would be.
                            let (b, x0, y0) = title_art(&c);
                            let off = x0 < 0
                                || y0 < 0
                                || x0 + b.w as isize > cols as isize
                                || y0 + b.h as isize > rows as isize;
                            let empty = b.on.iter().all(|x| !x);
                            if (off || empty)
                                && !matches!(
                                    kind,
                                    Act::Words
                                        | Act::Zoom
                                        | Act::Letter
                                        | Act::Scatter
                                        | Act::Vertical
                                        | Act::Orbit
                                )
                            {
                                bad.push(format!("{title} {cols}×{rows} size {size} place {place} font {} {}: art {}×{} at {x0},{y0}", font, if empty { "EMPTY" } else { "" }, b.w, b.h));
                            }
                            // What the act draws: nothing outside the canvas.
                            CLIPPED.with(|c| c.set(0));
                            let _ = act(kind, &c);
                            let cut = CLIPPED.with(|c| c.get());
                            if cut > 0 && !matches!(kind, Act::Drip) {
                                bad.push(format!("{title} {cols}×{rows} size {size} place {place} font {} EDGE {} {cut}", font, kind.name()));
                            }
                        }
                    }
                }
            }
        }
    }
    bad.sort();
    bad.dedup();
    let kinds: std::collections::BTreeSet<String> = bad
        .iter()
        .map(|b| {
            let w: Vec<&str> = b.split_whitespace().collect();
            let i = w.iter().position(|x| *x == "size").unwrap_or(0);
            format!(
                "size {} {} canvas {} {}",
                w[i + 1],
                if b.contains("EMPTY") {
                    "EMPTY"
                } else if b.contains("EDGE") {
                    "CUT"
                } else {
                    "OFF"
                },
                w[i - 1],
                if b.contains("EDGE") {
                    w[w.len() - 2]
                } else {
                    ""
                }
            )
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} off the canvas, kinds:\n{}\ne.g.\n{}",
        bad.len(),
        kinds.into_iter().collect::<Vec<_>>().join("\n"),
        bad.iter()
            .filter(|b| !b.contains("EMPTY"))
            .take(10)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

#[test]
fn words_are_never_plain_characters() {
    // A letter a cell reads as ~10 px on a big screen: every word an
    // act draws is letter art, in every act, size, face and canvas.
    let pal = crate::pack::PALETTES[0];
    let fonts = crate::fonts::Fonts::load();
    let mut bad = std::collections::BTreeSet::new();
    for &(cols, rows) in &[(80usize, 24usize), (160, 45), (240, 68), (480, 135)] {
        for &kind in Act::ALL {
            for size in 0..3u8 {
                for font in 0..=fonts.len().min(3) as u8 {
                    for beat in [4.2, 12.4, 30.9] {
                        let c = Ctx {
                            cols,
                            rows,
                            size,
                            font,
                            fonts: Some(&fonts),
                            beat,
                            age: beat / 2.0,
                            artist: "Alaia & Gallo",
                            ..ctx(beat, &pal, &[], &[])
                        };
                        if act(kind, &c).cells.iter().any(|x| x.ch.is_alphabetic()) {
                            bad.insert(format!(
                                "{} {cols}×{rows} size {size} font {font}",
                                kind.name()
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "plain letters in:\n{}",
        bad.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn the_artist_is_readable_and_smaller_than_the_title() {
    let pal = crate::pack::PALETTES[0];
    let fonts = crate::fonts::Fonts::load();
    let titles = [
        ("Tell Me Why", "Supermode"),
        ("Dejected (Sidney Charles Remix)", "Astin"),
        ("Lipstick", "Alaia & Gallo"),
        ("Losing It", "FISHER (OZ)"),
        ("Load. Ready. System", "Novuon"),
    ];
    let mut bad = vec![];
    for &(title, artist) in &titles {
        for &(cols, rows) in &[(80usize, 24usize), (160, 45), (240, 68), (480, 135)] {
            for size in 0..3u8 {
                for font in 0..=fonts.len().min(4) as u8 {
                    let c = Ctx {
                        cols,
                        rows,
                        size,
                        font,
                        title,
                        artist,
                        fonts: Some(&fonts),
                        ..ctx(12.4, &pal, &[], &[])
                    };
                    let (b, _, y0) = title_art(&c);
                    let title_h = (0..b.h)
                        .filter(|y| (0..b.w).any(|x| b.on[y * b.w + x]))
                        .count();
                    let mut g = Grid::blank(cols, rows);
                    byline(&mut g, &c, y0, y0 + b.h as isize);
                    let lit: Vec<usize> = (0..rows)
                        .filter(|y| (0..cols).any(|x| g.cells[y * cols + x].ch != ' '))
                        .collect();
                    if let (Some(a), Some(z)) = (lit.first(), lit.last()) {
                        let h = z - a + 1;
                        if h < small_rows(&c) || h >= title_h {
                            bad.push(format!("{title} {cols}×{rows} size {size} font {font}: artist {h} rows, title {title_h}, floor {}", small_rows(&c)));
                        }
                    }
                    // The artist's own beat in Words is smaller than the title's.
                    let beats = title.split_whitespace().count() as f64;
                    let words = |beat: f64| {
                        let g = act(Act::Words, &Ctx { beat, ..c });
                        (0..rows)
                            .filter(|y| (0..cols).any(|x| g.cells[y * cols + x].ch != ' '))
                            .count()
                    };
                    let (who, whole) = (words(beats + 0.1), words(beats + 1.1));
                    if who > 0 && who >= whole {
                        bad.push(format!("{title} {cols}×{rows} size {size} font {font}: words artist {who} rows, title {whole}"));
                    }
                }
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn all_of_it_is_fast() {
    let pal = crate::pack::PALETTES[2];
    let hist = vec![0.7f32; 60];
    let bands = vec![0.5f32; 32];
    let t = std::time::Instant::now();
    for &kind in Backdrop::ALL {
        std::hint::black_box(backdrop(
            kind,
            &Ctx {
                cols: 300,
                rows: 100,
                ..ctx(8.3, &pal, &hist, &bands)
            },
        ));
    }
    for &kind in Act::ALL {
        std::hint::black_box(act(
            kind,
            &Ctx {
                cols: 300,
                rows: 100,
                ..ctx(8.3, &pal, &hist, &bands)
            },
        ));
    }
    let ms = t.elapsed().as_secs_f64() * 1e3 / (Backdrop::ALL.len() + Act::ALL.len()) as f64;
    if crate::check_budgets() {
        assert!(ms < 4.0, "{ms:.2} ms");
    }
}

/// Digests of every backdrop and act over beats, sizes, places, styles,
/// gates and bands: run before and after a change to the drawing and
/// `cmp` the files to show it draws exactly as before.
/// `PATCH_STAGE_DUMP=<file> cargo test --release -p patch-visual --lib
/// stage_dump -- --ignored`
#[test]
#[ignore = "stage digest dump: PATCH_STAGE_DUMP"]
fn stage_dump() {
    use std::io::Write;
    let mut out = std::fs::File::create(std::env::var("PATCH_STAGE_DUMP").unwrap()).unwrap();
    let pal = crate::pack::PALETTES[1];
    let hist: Vec<f32> = (0..64).map(|i| (i as f32 * 0.37).sin().abs()).collect();
    let bands: Vec<f32> = (0..32).map(|i| (i as f32 * 0.61).cos().abs()).collect();
    let digest = |g: &Grid| {
        format!("{:?}", g.cells).bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x0100_0000_01b3)
        })
    };
    for beat in [0.0, 0.37, 1.5, 7.9, 31.2] {
        for (size, place, style, gate, bass) in [(1, 0, 0, 1.0, 0.7), (0, 1, 1, 0.0, 0.2), (2, 2, 2, 0.6, 1.0), (1, 2, 3, 1.0, 0.0)] {
            let mut c = ctx(beat, &pal, &hist, &bands);
            (c.size, c.place, c.style, c.gate, c.bass) = (size, place, style, gate, bass);
            for &b in Backdrop::ALL {
                writeln!(out, "backdrop {b:?} {beat} {size}{place}{style} {gate} {bass} {:016x}", digest(&backdrop(b, &c))).unwrap();
            }
            for &a in Act::ALL {
                writeln!(out, "act {a:?} {beat} {size}{place}{style} {gate} {bass} {:016x}", digest(&act(a, &c))).unwrap();
            }
        }
    }
}
