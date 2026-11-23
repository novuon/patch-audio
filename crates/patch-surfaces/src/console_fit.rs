//! How the console spends the window's height, and where it pins its returns.
//!
//! Pan, solo, mute, the dB readout, the fader and the name stay on screen.
//! That is the mixer. Above them, a bank (trim and EQ, then four sends — 168 px, the
//! same rows as `mixer.slint`) opens row by row: the viewport takes a whole
//! row of cells or nothing, so a send is never sliced in half. A taller
//! window never loses a row and never raises the fader's bottom edge, and the
//! desk always fills the window exactly. The fader gives ground only when a
//! new bank row opens.
//!
//! Widely, the desk keeps 104 px columns and pins what a console pins: from
//! `pin_main` up, MAIN sits at the right edge; from `pin_buses` up, the
//! returns sit with it. Narrower than that, everything scrolls as one desk.

/// Pan, solo, mute and the dB. Must match the cap in `mixer.slint`.
pub const HEADER: f32 = 68.0;
/// The channel name and its colour bar.
pub const FOOT: f32 = 26.0;
#[cfg(test)]
const CHROME: f32 = HEADER + FOOT;

/// Trim, EQ, and four sends, in px. Must match the bank in `mixer.slint`.
pub const UPPER_FULL: f32 = 168.0;
/// The throw the fader reaches before the bank is allowed to open.
pub const FADER_PREFERRED: f32 = 200.0;
/// The first whole row: Trim. Below this the bank stays closed.
const UPPER_FIRST: f32 = 30.0;

/// The bands of one Mixer style. Classic (Omarchy) is the constants above;
/// the Patch style draws a larger trim knob (its first row 58 px) and a
/// taller cap (84 px: larger mute, solo and gain). Each must match its `.slint`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub header: f32,
    pub foot: f32,
    pub upper_full: f32,
    upper_first: f32,
    bank_rows: [f32; 7],
}

pub const CLASSIC: Geometry = Geometry {
    header: HEADER,
    foot: FOOT,
    upper_full: UPPER_FULL,
    upper_first: UPPER_FIRST,
    bank_rows: BANK_ROWS,
};

pub const PATCH_STYLE: Geometry = Geometry {
    header: 84.0,
    foot: FOOT,
    upper_full: 196.0,
    upper_first: 58.0,
    bank_rows: [58.0, 108.0, 124.0, 142.0, 160.0, 178.0, 196.0],
};

/// A column, the rail and the rule between groups. The view's numbers
/// (`Console.col`, `TrackList`'s width, the desk's separators).
pub const COL: f32 = 104.0;
pub const RAIL: f32 = 176.0;
const SEP: f32 = 13.0;
/// The outer hairline drawn immediately before the pinned group.
const PIN_RULE: f32 = 1.0;
/// The pinned group's own rules, padding and right inset, over its columns:
/// the desk pads 4 px each side, MAIN carries two 8 px spacers and a rule,
/// the group has a 1 px left hairline, and an 8 px gutter keeps MAIN clear of
/// the window edge.
const PIN_CHROME: f32 = 33.0;
/// How many whole channel columns must stay reachable beside MAIN pinned alone.
const KEEP_COLS: f32 = 3.0;
/// The same with the returns pinned beside MAIN: one fewer, so the returns
/// stay with MAIN in a half-width window (the user, 2026-11-14: they
/// unpinned and left MAIN on its own).
const KEEP_COLS_BUSES: f32 = 2.0;

/// The bank's rows: trim dial (30), EQ dials (50), SENDS head (16), then four
/// send rows (18 each). The viewport takes one of these or nothing.
const BANK_ROWS: [f32; 7] = [30.0, 80.0, 96.0, 114.0, 132.0, 150.0, 168.0];

/// MAIN's destination list is its own row set: its head (the output picker,
/// MIX and the section rule), then one row per destination and the two
/// metering rows. Past the channel bank's 168 px the viewport grows to this,
/// so every destination keeps a readable row; the rows themselves tighten to
/// `MAIN_PITCH_MIN` before the bank would have to slice one.
const MAIN_HEAD: f32 = 82.0;
const MAIN_PITCH_MIN: f32 = 12.0;

/// What the window is wide enough to keep in place.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct ConsolePins {
    /// MAIN (the output strip) pinned to the right edge.
    pub pin_main: bool,
    /// The returns pinned with it.
    pub pin_buses: bool,
    /// The width the pinned group occupies, including its rule.
    pub pinned_w: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ConsoleFit {
    /// True: this desk never shows the links toolbar (it would shorten the
    /// fader's throw as the window grew). Pan and the name stay with the fader.
    pub compact: bool,
    /// Pan, solo, mute, dB. Scales only when the window is shorter than the chrome.
    pub header: f32,
    /// 0 while the bank is closed, otherwise 1. Rows do not scale.
    pub knob_scale: f32,
    /// The whole bank rows that fit, 0 … 168.
    pub knob_viewport: f32,
    pub knob_content: f32,
    pub fader: f32,
    pub foot: f32,
    /// Where the returns and MAIN sit.
    pub pins: ConsolePins,
}

/// The bank's viewport for the room left above the fader's preferred throw:
/// whole rows only, and nothing at all until half the tone band fits.
fn bank_viewport(g: &Geometry, available: f32) -> f32 {
    if available < g.upper_first {
        return 0.0;
    }
    g.bank_rows
        .iter()
        .rev()
        .find(|row| **row <= available + 0.05)
        .copied()
        .unwrap_or(0.0)
}

/// The bank's viewport and the fader's throw for `room` above the chrome.
/// `main_need` is what MAIN's destination list wants; when it is more than
/// the channel bank holds, and the room above the fader's preferred throw
/// covers it, the viewport grows past 168 px. Otherwise the whole channel
/// rows rule stands.
fn bands(g: &Geometry, room: f32, main_need: f32) -> (f32, f32) {
    let room = room.max(0.0);
    let spare = room - FADER_PREFERRED;
    let view = if main_need > g.upper_full && main_need <= spare + 0.05 {
        main_need
    } else {
        bank_viewport(g, spare)
    };
    (view, (room - view).max(0.0))
}

/// The width the pinned group occupies: the rule before it and the desk's
/// own padding (PIN_CHROME), one 104 px column per return while they are
/// pinned (with their rules between the groups), then MAIN. The view draws
/// exactly this: `Bands`' `Desk` pads 4 px each side and rules 1 px.
pub fn pinned_w(buses: usize, with_buses: bool) -> f32 {
    if with_buses {
        (buses + 1) as f32 * COL + PIN_CHROME + SEP + PIN_RULE
    } else {
        COL + PIN_CHROME + PIN_RULE
    }
}

/// What the console pins at this window width: MAIN whenever three whole
/// channel columns still fit beside it, the returns too while two do.
pub fn console_pins(width: f32, buses: usize) -> ConsolePins {
    let width = if width.is_finite() { width.max(0.0) } else { 0.0 };
    // The whole pinned group — its own rules and padding included — has to
    // leave KEEP_COLS (with the returns, KEEP_COLS_BUSES) whole channel
    // columns beside it.
    let keep = |cols: f32| RAIL + SEP + COL * cols;
    let pin_main = width >= keep(KEEP_COLS) + pinned_w(buses, false);
    let pin_buses = pin_main && buses > 0 && width >= keep(KEEP_COLS_BUSES) + pinned_w(buses, true);
    ConsolePins {
        pin_main,
        pin_buses,
        pinned_w: if pin_main {
            pinned_w(buses, pin_buses)
        } else {
            0.0
        },
    }
}

/// `inner` is the mixer surface height inside the window frame, in px; `width`
/// is the surface's own width, `buses` how many returns the console has and
/// `dests` how many destinations MAIN's PLAYS TO list carries.
#[cfg(test)]
pub fn console_fit(inner: f32, width: f32, buses: usize, dests: usize) -> ConsoleFit {
    console_fit_with(&CLASSIC, inner, width, buses, dests)
}

/// [`console_fit`] for a Mixer style's bands.
pub fn console_fit_with(g: &Geometry, inner: f32, width: f32, buses: usize, dests: usize) -> ConsoleFit {
    let chrome = g.header + g.foot;
    let pins = console_pins(width, buses);
    let inner = if inner.is_finite() { inner.max(0.0) } else { 0.0 };
    if inner < chrome {
        let t = inner / chrome;
        return ConsoleFit {
            compact: true,
            header: g.header * t,
            knob_scale: 0.0,
            knob_viewport: 0.0,
            knob_content: 0.0,
            fader: 0.0,
            foot: g.foot * t,
            pins,
        };
    }
    // The fader takes whatever is left after the bank. A toolbar here would
    // shorten that throw as the window grew, so the links stay off this desk.
    let main_need = MAIN_HEAD + (dests as f32 + 2.0) * MAIN_PITCH_MIN;
    let (view, fader) = bands(g, inner - chrome, main_need);
    ConsoleFit {
        compact: true,
        header: g.header,
        knob_scale: if view > 0.0 { 1.0 } else { 0.0 },
        knob_viewport: view,
        knob_content: g.upper_full,
        fader,
        foot: g.foot,
        pins,
    }
}

impl ConsoleFit {
    /// Cap, bank, fader and foot. Never more than the inner height.
    #[cfg(test)]
    pub fn spent(&self) -> f32 {
        self.header + self.knob_viewport + self.fader + self.foot
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fader's shortest useful throw: below this the window is too small
    /// for a strip and every band scales down instead (the view's `compact`).
    const FLOOR: f32 = 96.0;

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.05
    }

    #[test]
    fn the_reviewed_heights() {
        // The short shot, inside the window frame: pan, solo, mute, a real fader, the name.
        let short = console_fit(190.0, 800.0, 4, 2);
        assert!(short.compact);
        assert!(near(short.header, HEADER));
        assert!(near(short.fader, FLOOR));
        assert_eq!(short.knob_viewport, 0.0);
        assert!(near(short.foot, FOOT));

        // The mixer's own window: one whole trim row, and the fader keeps its throw.
        let strip = console_fit(334.0, 800.0, 4, 2);
        assert!(near(strip.knob_viewport, 30.0));
        assert!(near(strip.fader, 210.0));

        // Quarter window: the bank opens whole rows only, and the fader gives the ground back.
        let quarter = console_fit(400.0, 800.0, 4, 2);
        assert!(near(quarter.knob_scale, 1.0));
        assert!(near(quarter.knob_viewport, 96.0), "{}", quarter.knob_viewport);
        assert!(quarter.knob_viewport + 1.0 < quarter.knob_content);
        assert!(near(quarter.fader, 210.0));

        // Wide window: the bank is open and the fader has grown into the rest.
        let open = console_fit(660.0, 1600.0, 4, 2);
        assert!(near(open.knob_viewport, UPPER_FULL));
        assert!(near(open.fader, 398.0));

        // Half window: the fader fills down to the name. Nothing is left over.
        let half = console_fit(1240.0, 1600.0, 4, 2);
        assert!(near(half.knob_viewport, UPPER_FULL));
        assert!(near(half.fader, 978.0));
        assert!(near(half.spent(), 1240.0));
    }

    /// The bank's content starts at the viewport's top: Slint centres a child
    /// with no `y`, which sliced the trim row and hid another below it.
    #[test]
    fn the_bank_opens_from_its_top_row() {
        for source in [include_str!("../ui/mixer.slint"), include_str!("../ui/mixer-classic.slint")] {
            let band = source.find("band: 0;").expect("the bank's desk");
            let desk = source[..band].rfind("Desk {").expect("the bank's desk");
            assert!(source[desk..band].contains("y: 0px;"), "the bank's desk has no y");
        }
    }

    #[test]
    fn the_patch_style_heights() {
        // Patch style: a 58 px trim row and an 84 px cap; the same rules.
        let g = &PATCH_STYLE;
        let strip = console_fit_with(g, 378.0, 800.0, 4, 2);
        assert!(near(strip.header, 84.0));
        assert!(near(strip.knob_viewport, 58.0));
        assert!(near(strip.fader, 210.0));
        let quarter = console_fit_with(g, 444.0, 800.0, 4, 2);
        assert!(near(quarter.knob_viewport, 124.0), "{}", quarter.knob_viewport);
        let open = console_fit_with(g, 660.0, 1600.0, 4, 2);
        assert!(near(open.knob_viewport, 196.0));
        assert!(near(open.fader, 354.0));
        assert!(near(open.spent(), 660.0));
    }

    #[test]
    fn the_bank_never_slices_a_row() {
        for px in 0..1400 {
            for width in [640.0, 800.0, 1600.0] {
                let fit = console_fit(px as f32, width, 4, 12);
                // Above the channel bank the viewport holds MAIN's rows at
                // their own pitch; the whole-rows rule is the channels'.
                if fit.knob_viewport == 0.0 || fit.knob_viewport >= UPPER_FULL - 0.05 {
                    continue;
                }
                let whole = BANK_ROWS
                    .iter()
                    .any(|row| (row - fit.knob_viewport).abs() < 0.05);
                assert!(whole, "{px}: viewport {} slices a row", fit.knob_viewport);
            }
        }
    }

    #[test]
    fn the_destination_list_expands_the_bank() {
        // MAIN's list wants more than the channel bank holds: the viewport
        // grows to it at the minimum pitch, and the fader keeps its preferred
        // throw. Derived from the same constants the rows use.
        let need = MAIN_HEAD + 14.0 * MAIN_PITCH_MIN;
        assert!(need > UPPER_FULL);
        let fit = console_fit(CHROME + FADER_PREFERRED + need + 40.0, 1600.0, 4, 12);
        assert!(near(fit.knob_viewport, need), "{}", fit.knob_viewport);
        assert!(fit.fader + 0.05 >= FADER_PREFERRED);
        // Too short to grow: the whole channel rows rule stands unchanged.
        let short = console_fit(CHROME + FADER_PREFERRED + UPPER_FULL, 1600.0, 4, 12);
        assert!(near(short.knob_viewport, UPPER_FULL));
    }

    #[test]
    fn a_taller_window_never_gives_ground() {
        // A taller window never loses a bank row, never raises the fader's
        // bottom edge, and always spends exactly the room it has: the fader
        // gives ground only when a new row opens, and the desk fills.
        let mut prev = console_fit(0.0, 800.0, 4, 12);
        for px in 1..1400 {
            let fit = console_fit(px as f32, 800.0, 4, 12);
            assert!(
                fit.knob_scale + 1e-4 >= prev.knob_scale,
                "{px}: scale {} < {}",
                fit.knob_scale,
                prev.knob_scale
            );
            assert!(
                fit.knob_viewport + 0.05 >= prev.knob_viewport,
                "{px}: view {} < {}",
                fit.knob_viewport,
                prev.knob_viewport
            );
            assert!(
                fit.fader + fit.knob_viewport + 0.05 >= prev.fader + prev.knob_viewport,
                "{px}: the fader's bottom edge rose"
            );
            assert!(fit.spent() <= px as f32 + 0.05, "{px}: spent {}", fit.spent());
            if (px as f32) > CHROME {
                assert!(
                    near(fit.spent(), px as f32),
                    "{px}: the desk left {} px unspent",
                    px as f32 - fit.spent()
                );
            }
            if (px as f32) >= CHROME + FLOOR {
                assert!(fit.fader + 0.05 >= FLOOR, "{px}");
            }
            prev = fit;
        }
    }

    /// The user's rule (docs/decisions.md): MAIN, AUX and FX sit at the
    /// right edge, never cut off. Ask the user before changing this test.
    #[test]
    fn pins_follow_the_width() {
        // Narrow: everything scrolls as one desk.
        assert_eq!(console_pins(600.0, 4), ConsolePins::default());
        // Wide enough for MAIN beside three whole columns.
        let main = console_pins(900.0, 4);
        assert!(main.pin_main && !main.pin_buses);
        assert!(near(main.pinned_w, pinned_w(4, false)));
        // The returns stay with MAIN while two whole columns fit (the user,
        // 2026-11-14): a half-width window on a 27" screen keeps them.
        let half = console_pins(1000.0, 4);
        assert!(half.pin_main && half.pin_buses);
        assert!(console_pins(RAIL + SEP + 2.0 * COL + pinned_w(4, true), 4).pin_buses);
        assert!(!console_pins(RAIL + SEP + 2.0 * COL + pinned_w(4, true) - 1.0, 4).pin_buses);
        // Wide enough for the returns too.
        let both = console_pins(1600.0, 4);
        assert!(both.pin_main && both.pin_buses);
        assert!(near(both.pinned_w, pinned_w(4, true)));
        assert!(near(both.pinned_w, 5.0 * COL + PIN_CHROME + SEP + PIN_RULE));
        assert!(near(both.pinned_w, 567.0));
        // No returns: only MAIN pins.
        assert!(!console_pins(1600.0, 0).pin_buses);
        assert!(console_pins(1600.0, 0).pin_main);
        assert!(near(pinned_w(0, false), 138.0));
    }

    #[test]
    fn the_pinned_columns_stay_reachable() {
        // Wherever MAIN is pinned alone, three whole channel columns still
        // fit; with the returns beside it, two.
        for px in 0..3000 {
            let w = px as f32;
            let fit = console_fit(900.0, w, 4, 2);
            if !fit.pins.pin_main {
                continue;
            }
            let room = w - RAIL - SEP - fit.pins.pinned_w;
            let keep = if fit.pins.pin_buses { KEEP_COLS_BUSES } else { KEEP_COLS };
            assert!(room >= COL * keep - 0.05, "{w}: {room} px of channels");
        }
    }
}
