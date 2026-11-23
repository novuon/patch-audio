use crate::plugins;
use patch_core::layout::{tile, tile_on};
use patch_core::{AppId, AppSpec, LayoutRecipe, LayoutStatus, SurfaceSpec, UnitKind};

pub fn spec() -> AppSpec {
    // Minimum sizes: the smallest window (logical px) in which every part of
    // the surface is shown (from each surface's own breakpoints). Presets
    // give at least this on a 2400×1350 workspace (test below); a smaller
    // window offers to make room.
    let mut surfaces = vec![
        SurfaceSpec::core(UnitKind::Sequencer, "sequencer", "Sequencer").min(720, 580),
        SurfaceSpec::core(UnitKind::Router, "graph", "Router").min(640, 420),
        // The console can collapse its tone/send bank to make room, so its
        // full-detail threshold is applied dynamically by the window host.
        SurfaceSpec::core(UnitKind::Mixer, "mixer", "Mixer").min(640, 380),
        SurfaceSpec::core(UnitKind::Device, "devices", "Device").min(420, 300),
        SurfaceSpec::core(UnitKind::Mixer, "inspector", "Inspector").min(420, 300),
        SurfaceSpec::core(UnitKind::Meter, "meters", "Meter")
            .multi()
            .min(560, 460),
        SurfaceSpec::core(UnitKind::Session, "presets", "Session").min(420, 300),
        SurfaceSpec::core(UnitKind::Player, "player", "Player").min(560, 400),
        SurfaceSpec::core(UnitKind::Recorder, "recorder", "Recorder").min(560, 380),
        SurfaceSpec::core(UnitKind::Analyzer, "analyzer", "Analyzer").min(720, 400),
        SurfaceSpec::core(UnitKind::Mic, "mic", "Mic").min(560, 400),
        SurfaceSpec::core(UnitKind::Tune, "tune", "Tune").min(480, 360),
        SurfaceSpec::core(UnitKind::Eq, "eq", "EQ").min(860, 460),
        // Also SUPER+ALT+P anywhere (omarchy-patch toggle perf).
        SurfaceSpec::core(UnitKind::Performance, "perf", "Performance").min(900, 560),
        // Engine timing, clock, latency and workers: core, beside what
        // Patch costs (developer mode puts one on every workspace). No
        // letter (Y is the Player's): the controller or the bar opens it.
        SurfaceSpec::core(UnitKind::Performance, "diagnostics", "Diagnostics")
            .multi()
            .min(480, 360),
        // "Patch · Controller" is the session controller's window title
        // (Hyprland floats it): the Controller Unit's window is named apart.
        SurfaceSpec::core(UnitKind::Controller, "controllers", "MIDI & HID").min(480, 320),
        // Any size: from a corner of the booth to a projector.
        SurfaceSpec::core(UnitKind::Visualiser, "visualiser", "Visualiser").min(320, 200),
        // Its control panel, a window of the same Unit (L in the Visualiser).
        SurfaceSpec::core(UnitKind::Visualiser, "visualiser-controls", "Controls").min(420, 480),
        // Sends Patch's audio out: Icecast, RTMP or a file.
        SurfaceSpec::core(UnitKind::Broadcast, "broadcast", "Broadcast").min(560, 420),
        SurfaceSpec::core(UnitKind::Broadcast, "performer", "Performer").min(640, 360),
        // Live-coded patterns for any Unit (Strudel or Patch code).
        SurfaceSpec::core(UnitKind::Pattern, "pattern", "Pattern").min(560, 400),
        // TidalCycles, native: live-coded music on Patch's own instrument.
        SurfaceSpec::core(UnitKind::Tidal, "tidal", "Tidal").min(640, 420),
    ];
    // Planned Units: a placeholder window each, clearly marked, so every
    // Unit has its place in the workspaces before it is built.
    surfaces.push(SurfaceSpec::core(UnitKind::Plugins, "plugins", "Plugins").min(560, 400));
    // One rack plugin per window: `plugin` is slot 1, `plugin#N` slot N.
    // Opened from the rack (no key, not in the menus).
    surfaces.push(
        SurfaceSpec::core(UnitKind::Plugins, "plugin", "Plugin")
            .multi()
            .min(420, 320),
    );
    // Patch's audio to and from other machines on the local network.
    surfaces.push(SurfaceSpec::core(UnitKind::Network, "network", "Network").min(520, 420));
    let dj = plugins::dj::manifest();
    surfaces.extend(dj.surfaces);
    let mut layouts = dj.layouts;

    // The presets, from everyday to the full performance. Each opens on
    // the next empty workspace(s); windows you already have elsewhere
    // stay. Every window gets its minimum size on a 1920×1080 screen
    // (tests below); a layout needs as many workspaces as that takes.
    layouts.push(LayoutRecipe {
        id: "simple",
        name: "Simple",
        summary: "Patch as your audio system on one workspace: the Mixer across the top, the EQ below.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile("mixer", 0.0, 0.0, 1.0, 0.5),
            tile("eq", 0.0, 0.5, 1.0, 0.5),
        ],
    });
    layouts.push(LayoutRecipe {
        id: "basic-dj",
        name: "Basic DJ",
        summary: "Two decks and the crossfader, the library below. One workspace.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile("deck-a", 0.0, 0.0, 0.36, 0.55),
            tile("dj-mixer", 0.36, 0.0, 0.28, 0.55),
            tile("deck-b", 0.64, 0.0, 0.36, 0.55),
            tile("library", 0.0, 0.55, 1.0, 0.45),
        ],
    });
    layouts.push(LayoutRecipe {
        id: "ai-dj",
        name: "Auto Mix",
        summary: "The DJ Booth with Auto Mix (its plan, queue and auto-mix), the library and the meter. One workspace.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile("dj", 0.0, 0.0, 0.65, 1.0),
            tile("library", 0.65, 0.0, 0.35, 0.5),
            tile("meters", 0.65, 0.5, 0.35, 0.5),
        ],
    });
    layouts.push(LayoutRecipe {
        id: "performance-dj",
        name: "Performance DJ",
        summary: "Three workspaces: 1 the booth (decks, crossfader, library, the DJ Booth with Auto Mix) · 2 control (EQ and the Mixer's Inspector, the Mixer and the Meter) · 3 the Visualiser, for a screen or a projector.",
        status: LayoutStatus::Available,
        tiles: vec![
            // 1 · The booth: decks either side of the crossfader, what you
            // play from and the DJ Booth (set and AI DJ) below.
            tile_on(0, "deck-a", 0.0, 0.0, 0.36, 0.55),
            tile_on(0, "dj-mixer", 0.36, 0.0, 0.28, 0.55),
            tile_on(0, "deck-b", 0.64, 0.0, 0.36, 0.55),
            tile_on(0, "library", 0.0, 0.55, 0.5, 0.45),
            tile_on(0, "dj", 0.5, 0.55, 0.5, 0.45),
            // 2 · Control: the EQ and the selected strip's Inspector, the Mixer
            // and the Meter.
            tile_on(1, "eq", 0.0, 0.0, 0.6, 0.5),
            tile_on(1, "inspector", 0.6, 0.0, 0.4, 0.5),
            tile_on(1, "mixer", 0.0, 0.5, 0.68, 0.5),
            tile_on(1, "meters", 0.68, 0.5, 0.32, 0.5),
            // 3 · The Visualiser, the whole workspace.
            tile_on(2, "visualiser", 0.0, 0.0, 1.0, 1.0),
        ],
    });
    // DJ Studio: the user's own "Basic DJ 1", with the meter and the EQ
    // widened (from the DJ Booth and the Router) to their full size on the
    // user's 2400 px screen. On 1920 px they show their compact forms: the
    // Router, the Inspector and the EQ need 1986 px side by side.
    layouts.push(LayoutRecipe {
        id: "dj-studio",
        name: "DJ Studio",
        summary: "Three workspaces: 1 the booth (decks either side of the crossfader; the Visualiser, the DJ Booth and the meter below) · 2 the mixing desk (Router, Inspector and EQ over the Mixer) · 3 the library, with Diagnostics.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile_on(0, "deck-a", 0.0, 0.0, 0.3629, 0.5498),
            tile_on(0, "dj-mixer", 0.3629, 0.0, 0.2771, 0.5498),
            tile_on(0, "visualiser", 0.0, 0.5498, 0.3629, 0.4502),
            tile_on(0, "dj", 0.3629, 0.5498, 0.3921, 0.4502),
            tile_on(0, "deck-b", 0.64, 0.0, 0.36, 0.5498),
            tile_on(0, "meters", 0.755, 0.5498, 0.245, 0.4502),
            tile_on(1, "graph", 0.0, 0.0, 0.3756, 0.5506),
            tile_on(1, "inspector", 0.3756, 0.0, 0.2494, 0.5506),
            tile_on(1, "mixer", 0.0, 0.5506, 1.0, 0.4494),
            tile_on(1, "eq", 0.625, 0.0, 0.375, 0.5506),
            tile_on(2, "diagnostics", 0.5, 0.0, 0.5, 1.0),
            tile_on(2, "library", 0.0, 0.0, 0.5, 1.0),
        ],
    });
    layouts.push(LayoutRecipe {
        id: "podcast",
        name: "Podcast & Broadcast",
        summary: "One workspace: the mic, the Mixer and the meter above; the Player (jingles), the Recorder and Broadcast below.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile("mic", 0.0, 0.0, 0.32, 0.5),
            tile("mixer", 0.32, 0.0, 0.36, 0.5),
            tile("meters", 0.68, 0.0, 0.32, 0.5),
            tile("player", 0.0, 0.5, 0.32, 0.5),
            tile("recorder", 0.32, 0.5, 0.36, 0.5),
            tile("broadcast", 0.68, 0.5, 0.32, 0.5),
        ],
    });
    layouts.push(LayoutRecipe {
        id: "live-code",
        name: "Live Code",
        summary: "Two workspaces: 1 Tidal (code, channels, the reference) with the meter and the Mixer beside it · 2 the Visualiser, for a screen or a projector.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile_on(0, "tidal", 0.0, 0.0, 0.65, 1.0),
            tile_on(0, "meters", 0.65, 0.0, 0.35, 0.5),
            tile_on(0, "mixer", 0.65, 0.5, 0.35, 0.5),
            tile_on(1, "visualiser", 0.0, 0.0, 1.0, 1.0),
        ],
    });
    // Pro: the engineer's desk, from the core Units (every install has
    // them). 1 · Mix: the EQ and the meter over the Mixer and its
    // Inspector. 2 · Route and record: the Router and the Analyzer over the
    // Recorder and the plugin rack (without the Plugins module the
    // Recorder takes the row: `arrange`).
    layouts.push(LayoutRecipe {
        id: "pro",
        name: "Pro",
        summary: "Two workspaces for mixing and engineering: 1 the EQ and the meter over the Mixer and its Inspector · 2 the Router and the Analyzer over the Recorder and the plugin rack.",
        status: LayoutStatus::Available,
        tiles: vec![
            tile_on(0, "eq", 0.0, 0.0, 0.55, 0.465),
            tile_on(0, "meters", 0.55, 0.0, 0.45, 0.465),
            tile_on(0, "mixer", 0.0, 0.465, 0.75, 0.535),
            tile_on(0, "inspector", 0.75, 0.465, 0.25, 0.535),
            tile_on(1, "graph", 0.0, 0.0, 0.6, 0.55),
            tile_on(1, "analyzer", 0.6, 0.0, 0.4, 0.55),
            tile_on(1, "recorder", 0.0, 0.55, 0.4, 0.45),
            tile_on(1, "plugins", 0.4, 0.55, 0.6, 0.45),
        ],
    });
    // Group surfaces by Unit (stable, so a Unit keeps its own order).
    surfaces.sort_by_key(|s| s.unit);
    // All Units, for development: every Unit, packed onto as many
    // workspaces as their minimum sizes need. Opening it packs again from
    // the Units the setup has (`arrange`). Dozens of windows: slow to open.
    let every: Vec<_> = pro_units(&surfaces, |_| true);
    layouts.push(LayoutRecipe {
        id: ALL_UNITS,
        name: "All Units (developer)",
        summary: "For development: every Unit you have installed, on as many workspaces as they need. Dozens of windows, slow to open.",
        status: LayoutStatus::Available,
        tiles: patch_core::layout::pack(&every),
    });
    AppSpec {
        id: AppId::Patch,
        surfaces,
        layouts,
    }
}

/// The development layout that opens every Unit.
pub const ALL_UNITS: &str = "all-units";

/// The Units All Units opens, in the controller's order, with their minimum
/// sizes: every implemented one `keep` lets through (the setup's).
fn pro_units(
    surfaces: &[SurfaceSpec],
    keep: impl Fn(&str) -> bool,
) -> Vec<(&'static str, (u16, u16))> {
    surfaces
        .iter()
        .filter(|s| s.origin.is_implemented() && s.origin.can_open() && keep(s.id))
        .map(|s| (s.id, s.min_size))
        .collect()
}

/// Diagnostics windows, one per workspace (developer mode).
const DIAGNOSTICS: [&str; 12] = [
    "diagnostics", "diagnostics#2", "diagnostics#3", "diagnostics#4",
    "diagnostics#5", "diagnostics#6", "diagnostics#7", "diagnostics#8",
    "diagnostics#9", "diagnostics#10", "diagnostics#11", "diagnostics#12",
];

/// A built-in layout as this setup opens it: All Units packed from the
/// Units it has, Pro without the plugin rack when the setup has no
/// plugins; in developer mode, a Diagnostics window on every workspace.
/// `screen` is the monitor it opens on (logical px, less the bar), when known.
pub fn arrange(
    setup: &patch_core::setup::Setup,
    mut recipe: LayoutRecipe,
    screen: Option<(f32, f32)>,
) -> LayoutRecipe {
    let s = spec();
    let min = |id: &str| s.instance(id).map_or((320, 200), |(x, _)| x.min_size);
    let diag = min("diagnostics");
    if recipe.id == "simple" && !setup.shows("eq") {
        // Standard without the EQ: the Mixer takes the workspace.
        recipe.tiles.retain(|t| t.surface != "eq");
        for t in recipe.tiles.iter_mut() {
            t.h = 1.0;
        }
    }
    if recipe.id == "pro" && !setup.shows("plugins") {
        // The Recorder takes the rack's place on its row.
        recipe.tiles.retain(|t| t.surface != "plugins");
        for t in recipe.tiles.iter_mut().filter(|t| t.surface == "recorder") {
            t.w = 1.0;
        }
    }
    if recipe.id == ALL_UNITS {
        // In developer mode Diagnostics gets its own column (below).
        let units = pro_units(&s.surfaces, |id| {
            setup.shows(id) && !(setup.dev_mode && id == "diagnostics")
        });
        let (w, h) = screen.unwrap_or(patch_core::layout::ROOM);
        if setup.dev_mode {
            // Diagnostics' column reserved on every workspace first: Pro
            // fills its screens, so no window would have room to give.
            let col = diag.0 as f32 + patch_core::layout::GAP;
            let f = 1.0 - col / w.max(patch_core::layout::ROOM.0);
            let mut tiles = patch_core::layout::pack_on(&units, (w - col, h));
            let n = tiles.iter().map(|t| t.ws).max().map_or(1, |m| m + 1);
            for t in &mut tiles {
                t.x *= f;
                t.w *= f;
            }
            for ws in 0..n {
                tiles.push(patch_core::layout::Tile {
                    ws,
                    surface: DIAGNOSTICS[ws as usize % DIAGNOSTICS.len()],
                    x: f,
                    y: 0.0,
                    w: 1.0 - f,
                    h: 1.0,
                    float: false,
                });
            }
            recipe.tiles = tiles;
            return recipe;
        }
        recipe.tiles = patch_core::layout::pack_on(&units, (w, h));
    }
    if setup.dev_mode {
        recipe.tiles = patch_core::layout::with_on_every_workspace(&recipe.tiles, &DIAGNOSTICS, diag, min);
    }
    recipe
}

/// The windows a built-in layout must have: all its tiles but those
/// `arrange` leaves out (Simple's EQ, Pro's plugin rack).
fn required(l: &LayoutRecipe, t: &patch_core::layout::Tile) -> bool {
    !(l.id == "simple" && t.surface == "eq") && !(l.id == "pro" && t.surface == "plugins")
}

/// Whether menus offer this layout under `setup`: a saved layout and All
/// Units always; a built-in one when every window it opens is in the
/// setup.
pub fn layout_offered(setup: &patch_core::setup::Setup, id: &str) -> bool {
    if id == ALL_UNITS {
        return true;
    }
    spec()
        .layout(id)
        .filter(|l| l.id == id)
        .is_none_or(|l| l.tiles.iter().all(|t| !required(l, t) || setup.shows(t.surface)))
}

/// The modules a built-in layout needs and the setup leaves out, by name.
/// All Units needs none: it opens the Units the setup has.
pub fn layout_missing(setup: &patch_core::setup::Setup, id: &str) -> Vec<&'static str> {
    let spec = spec();
    let Some(l) = spec.layout(id).filter(|l| l.id != ALL_UNITS) else {
        return vec![];
    };
    let mut out: Vec<&'static str> = l
        .tiles
        .iter()
        .filter(|t| required(l, t) && !setup.shows(t.surface))
        .filter_map(|t| patch_core::setup::surface_module(t.surface))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Windows that show what a Unit will do but cannot do it yet: Tune (no
/// device profiles or controls) and Session (presets are not saved or
/// applied yet). Their header and their own empty state say PREVIEW.
pub const PREVIEW: &[&str] = &["tune", "presets"];

/// A window's own keys, from the keymap (patch_core::keys).
pub fn hints(surface: &str) -> Vec<(&'static str, &'static str)> {
    patch_core::keys::pairs(patch_core::keys::surface(surface))
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_core::UnitKind;

    #[test]
    fn every_surface_belongs_to_a_unit() {
        let s = spec();
        let units = s.units();
        assert_eq!(units.len(), UnitKind::ALL.len());
        let find = |k| units.iter().find(|u| u.kind == k).unwrap();
        assert!(find(UnitKind::Mixer).implemented);
        assert_eq!(find(UnitKind::Mixer).surfaces, ["mixer", "inspector"]);
        assert!(find(UnitKind::DjBooth).implemented);
        assert!(find(UnitKind::Broadcast).implemented);
        assert_eq!(find(UnitKind::Broadcast).surfaces, ["broadcast", "performer"]);
        assert!(find(UnitKind::Analyzer).implemented);
        assert!(find(UnitKind::Plugins).implemented);
        assert!(find(UnitKind::Network).implemented);
        assert_eq!(
            find(UnitKind::Plugins).surfaces,
            ["plugins", "plugin"],
            "the Plugins Unit"
        );
    }

    #[test]
    fn window_titles_name_the_unit() {
        let s = spec();
        assert_eq!(
            s.surface("mixer").unwrap().window_title(s.id),
            "Patch · Mixer"
        );
        assert_eq!(
            s.surface("inspector").unwrap().window_title(s.id),
            "Patch · Mixer · Inspector"
        );
        assert_eq!(
            s.surface("visualiser-controls").unwrap().window_title(s.id),
            "Patch · Visualiser · Controls"
        );
        assert_eq!(
            s.surface("deck-a").unwrap().window_title(s.id),
            "Patch · DJ Booth · Deck A"
        );
        // Never the session controller's own title (Hyprland floats that one).
        for x in &s.surfaces {
            assert_ne!(x.window_title(s.id), "Patch · Controller", "{}", x.id);
        }
    }
}

#[cfg(test)]
mod all_units {
    use super::*;

    #[test]
    fn legacy_dj_surfaces_and_ai_stage_are_gone() {
        let s = spec();
        assert!(
            s.surface("light-dj").is_none(),
            "legacy Light DJ is disabled"
        );
        assert!(
            s.surface("ai-dj").is_none(),
            "legacy standalone Auto Mix is disabled"
        );
        assert!(
            s.surface("ai-stage").is_none(),
            "AI Stage is gone: the DJ Booth shows the AI DJ (the user, 2026-11-22)"
        );
        assert!(
            !UnitKind::ALL.iter().any(|u| matches!(u.name(), "AI DJ" | "Auto Mix")),
            "Auto Mix is part of the DJ Booth, not its own unit"
        );
        let layout = s.layout("ai-dj").expect("layout id stays ai-dj");
        assert_eq!(layout.name, "Auto Mix");
        assert!(!layout.summary.contains("AI DJ"));
    }

    #[test]
    fn performance_dj_shows_one_dj_booth_on_the_booth_workspace() {
        let s = spec();
        let live = s.layout("live-dj").expect("the old name still works");
        assert_eq!(live.id, "performance-dj");
        assert_eq!(live.workspaces(), 3);
        let dj: Vec<_> = live.tiles.iter().filter(|t| t.surface == "dj").collect();
        assert_eq!(dj.len(), 1, "one DJ Booth window");
        assert_eq!(dj[0].ws, 0, "on the booth workspace");
        assert!(
            !live.tiles.iter().any(|t| t.surface == "ai-stage"),
            "the Auto Mix lives in the DJ Booth window"
        );
    }

    #[test]
    fn each_default_layout_is_a_preset_its_modules_can_open() {
        use patch_core::setup::{DefaultLayout, Setup};
        let s = spec();
        for l in DefaultLayout::ALL {
            // Only the modules the layout says it needs: every window shows.
            let mut only = patch_core::setup::Profile::Minimal.setup();
            for m in l.needs() {
                only.set(m, true);
            }
            let recipe = arrange(&only, s.layout(l.id()).unwrap_or_else(|| panic!("{}", l.id())).clone(), None);
            for t in &recipe.tiles {
                assert!(only.shows(t.surface), "{}: {} needs a module it doesn't name", l.id(), t.surface);
            }
            assert!(only.offers(l));
            let _ = Setup::default();
        }
    }

    #[test]
    fn all_units_opens_every_installed_unit() {
        use patch_core::setup::Profile;
        let s = spec();
        let pro = s.layout("dev").expect("the old name still works");
        assert_eq!(pro.id, ALL_UNITS);
        let full = arrange(&Profile::Full.setup(), pro.clone(), None);
        for u in UnitKind::ALL {
            let implemented = s.surfaces.iter().any(|x| x.unit == u && x.origin.is_implemented());
            assert_eq!(
                full.tiles.iter().any(|t| s.instance(t.surface).is_some_and(|(x, _)| x.unit == u)),
                implemented,
                "{}",
                u.name()
            );
        }
        let min = arrange(&Profile::Minimal.setup(), pro.clone(), None);
        assert!(min.tiles.len() < full.tiles.len(), "a smaller install, fewer Units");
        assert!(min.tiles.iter().all(|t| Profile::Minimal.setup().shows(t.surface)));
        assert!(min.workspaces() < full.workspaces());
        let big = arrange(&Profile::Full.setup(), pro.clone(), Some((2400.0, 1326.0)));
        assert!(big.workspaces() <= full.workspaces(), "the user's screen packs tighter");
        for l in [&full, &min, &big] {
            assert!(l.plan_open(|_| true).is_some(), "All Units tiles cleanly");
        }
    }

    /// Pro is a desk for mixing and engineering, not every Unit: two
    /// workspaces with the EQ and Studio tools, the plugin rack only with
    /// the Plugins module.
    #[test]
    fn pro_is_the_engineers_desk() {
        use patch_core::setup::Profile;
        let s = spec();
        let pro = s.layout("pro").unwrap();
        let full = arrange(&Profile::Full.setup(), pro.clone(), None);
        let mut desk = Profile::Standard.setup();
        desk.set("studio", true);
        let min = arrange(&desk, pro.clone(), None);
        assert_eq!(full.workspaces(), 2);
        assert_eq!(full.tiles.len(), 8, "a desk, not every Unit");
        assert!(full.tiles.iter().any(|t| t.surface == "plugins"));
        assert!(!min.tiles.iter().any(|t| t.surface == "plugins"), "no rack without plugins");
        assert!(min.tiles.iter().all(|t| desk.shows(t.surface)));
        let recorder = min.tiles.iter().find(|t| t.surface == "recorder").unwrap();
        assert_eq!((recorder.x, recorder.w), (0.0, 1.0), "the Recorder takes the rack's row");
        for l in [&full, &min] {
            assert!(l.plan_open(|_| true).is_some(), "Pro tiles cleanly");
        }
        // Pro is the engineer's desk: it comes with the EQ and Studio tools.
        assert!(!layout_offered(&Profile::Standard.setup(), "pro"));
        assert_eq!(layout_missing(&Profile::Standard.setup(), "pro"), ["studio"]);
        let mut studio = Profile::Standard.setup();
        studio.set("studio", true);
        assert!(layout_offered(&studio, "pro") && layout_missing(&studio, "pro").is_empty());
        // Simple opens in every setup: without the EQ the Mixer fills it.
        let min = Profile::Minimal.setup();
        assert!(layout_offered(&min, "simple"));
        let simple = arrange(&min, s.layout("simple").unwrap().clone(), None);
        assert_eq!(simple.tiles.len(), 1);
        assert_eq!((simple.tiles[0].surface, simple.tiles[0].h), ("mixer", 1.0));
    }

    #[test]
    fn developer_mode_puts_diagnostics_on_every_workspace() {
        use patch_core::setup::Profile;
        let s = spec();
        let mut dev = Profile::Full.setup();
        dev.dev_mode = true;
        for (l, screen) in s.layouts.iter().flat_map(|l| [(l, None), (l, Some((2918.0, 1617.0))), (l, Some((2400.0, 1326.0)))]) {
            let a = arrange(&dev, l.clone(), screen);
            let room = screen.unwrap_or(patch_core::layout::ROOM);
            for t in &a.tiles {
                let (mw, mh) = s.instance(t.surface).unwrap().0.min_size;
                assert!(
                    t.w * room.0 - 22.0 >= mw as f32 * 0.99 || l.id != ALL_UNITS,
                    "{} at {room:?}: {} gets {:.0} px of {mw}",
                    l.id,
                    t.surface,
                    t.w * room.0 - 22.0
                );
                assert!(t.w * room.0 >= 120.0, "{}: {} is a sliver", l.id, t.surface);
                let _ = mh;
            }
            if l.id != ALL_UNITS {
                assert_eq!(a.workspaces(), l.workspaces(), "{}: no workspace more", l.id);
            }
            for ws in 0..a.workspaces() as u8 {
                let n = a
                    .tiles
                    .iter()
                    .filter(|t| t.ws == ws && patch_core::instance::base(t.surface) == "diagnostics")
                    .count();
                assert_eq!(n, 1, "{}: workspace {ws}", l.id);
            }
            let ids: Vec<_> = a.tiles.iter().map(|t| t.surface).collect();
            let mut unique = ids.clone();
            unique.sort();
            unique.dedup();
            assert_eq!(unique.len(), ids.len(), "{}: one window per instance", l.id);
            assert!(a.tiles.iter().all(|t| s.instance(t.surface).is_some()), "{}", l.id);
            assert!(a.plan_open(|_| true).is_some(), "{} tiles cleanly", l.id);
        }
        // Off: every layout as it was.
        let off = Profile::Full.setup();
        let simple = s.layout("simple").unwrap();
        assert_eq!(arrange(&off, simple.clone(), None).tiles, simple.tiles);
    }

    #[test]
    fn presets_give_every_window_its_room() {
        // On a 2400×1350 screen (4K at 1.6) less the bar, after Hyprland's gaps and
        // borders (~22 px per tile each way), every tile is at least its
        // surface's minimum size: nothing opens cramped.
        let s = spec();
        // And on a 1920×1080 screen (scale 1). Both less Omarchy's bar.
        for (ws_w, ws_h, gaps) in [(2400.0f32, 1326.0f32, 22.0f32), (1920.0, 1056.0, 22.0)] {
        for l in &s.layouts {
            for t in &l.tiles {
                let (surface, _) = s
                    .instance(t.surface)
                    .unwrap_or_else(|| panic!("{}: no surface {}", l.id, t.surface));
                let (w, h) = (t.w * ws_w - gaps, t.h * ws_h - gaps);
                let (mw, mh) = surface.min_size;
                // The one exception: DJ Studio fits the user's 2400 px
                // screen; on 1920 px its EQ (beside the Router and the
                // Inspector) and its meter show their compact forms.
                if l.id == "dj-studio" && ws_w < 2400.0 && matches!(t.surface, "eq" | "meters") {
                    continue;
                }
                assert!(
                    w >= mw as f32 && h >= mh as f32,
                    "{} at {ws_w}×{ws_h}: {} gets {w:.0}×{h:.0}, needs {mw}×{mh}",
                    l.id,
                    t.surface
                );
            }
            let openable = |id: &str| s.instance(id).is_some_and(|(x, _)| x.origin.can_open());
            l.plan_open(openable)
                .unwrap_or_else(|| panic!("{} tiles cleanly", l.id));
        }
        }
    }

    #[test]
    fn the_presets_and_their_old_names() {
        let s = spec();
        let ids: Vec<&str> = s.layouts.iter().map(|l| l.id).collect();
        assert_eq!(ids, ["simple", "basic-dj", "ai-dj", "performance-dj", "dj-studio", "podcast", "live-code", "pro", ALL_UNITS]);
        for (old, new) in patch_core::layout::ALIASES {
            assert_eq!(s.layout(old).map(|l| l.id), Some(new), "{old}");
        }
    }
}
