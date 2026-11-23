//! The Unit session host.
//!
//! One process per Unit. It owns the shared session state and opens every
//! surface as its own top-level Wayland window; Hyprland tiles them. There is
//! no "main window": with no surfaces open the session keeps running headless
//! (the future audio engine will live outside even this process).

use crate::curtain::Curtain;
use crate::{hypr, ipc};
use patch_core::instance;
use patch_core::ipc::{Request, Status};
use patch_core::keys::{self, Command};
use patch_core::layout::Step;
use patch_core::{AppId, AppSpec, LayoutRecipe};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

pub const CONTROLLER: &str = "controller";

/// The compact Mixer can still show its faders below its full-detail height,
/// but its tone and send banks collapse. Use the shared size warning at the
/// full-console threshold so the user can choose to make room or keep it small.
fn minimum_size(surface: &str, declared: (u16, u16)) -> (u16, u16) {
    if instance::base(surface) == "mixer" {
        (declared.0, declared.1.max(480))
    } else {
        declared
    }
}

/// Implemented by each Unit crate for its generated `PatchWindow` via
/// [`surface_window!`](crate::surface_window).
pub trait SurfaceWindow: Sized + 'static {
    fn window(&self) -> &slint::Window;
    fn show(&self) -> Result<(), slint::PlatformError>;
    fn hide(&self) -> Result<(), slint::PlatformError>;
    fn apply_theme(&self, palette: &patch_ui::Palette);
    /// The window's style: its look (`patch_style`) and its layouts (`pro`).
    fn set_style(&self, style: patch_ui::style::Style);
    fn apply_host(&self, host: &HostState);
    fn bind_host(&self, host: Weak<dyn HostOps>, surface: &str);
    /// The "too small to show everything" note ("" = it fits).
    fn set_size_note(&self, note: &str);
}

/// Commands a window can issue to its session host. `close` and `key` get
/// the window's own id (`meters#2` for a second Meters window); `toggle` and
/// `open` get a surface id from the controller.
pub trait HostOps {
    fn toggle(&self, surface: &str);
    fn open(&self, surface: &str) -> bool;
    fn open_layout(&self, id: &str) -> bool;
    fn controller(&self);
    fn close(&self, surface: &str);
    fn quit(&self);
    fn key(&self, surface: &str, text: &str, ctrl: bool, shift: bool) -> bool;
    /// Give the window the room its surface needs (float it that big).
    fn make_room(&self, surface: &str);
    /// Room of a given size (the surface knows it needs more than its minimum).
    fn make_room_for(&self, surface: &str, w: u32, h: u32);
    /// Accept this surface small from now on (remembered).
    fn keep_small(&self, surface: &str);
    /// Flip one of the app's settings (the controller's switches).
    fn toggle_setting(&self, id: &str);
    fn choose_profile(&self, id: &str);
}

impl<U: HostedApp> HostOps for Session<U> {
    fn toggle(&self, surface: &str) {
        Session::toggle(self, surface)
    }
    fn open(&self, surface: &str) -> bool {
        Session::open(self, surface)
    }
    fn open_layout(&self, id: &str) -> bool {
        Session::open_layout(self, id)
    }
    fn controller(&self) {
        Session::controller(self)
    }
    fn close(&self, surface: &str) {
        Session::close_window(self, surface)
    }
    fn quit(&self) {
        Session::quit(self)
    }
    fn key(&self, surface: &str, text: &str, ctrl: bool, shift: bool) -> bool {
        Session::key(self, surface, text, ctrl, shift)
    }
    fn make_room(&self, surface: &str) {
        Session::make_room(self, surface)
    }
    fn make_room_for(&self, surface: &str, w: u32, h: u32) {
        Session::make_room_for(self, surface, w, h)
    }
    fn keep_small(&self, surface: &str) {
        Session::keep_small(self, surface)
    }
    fn toggle_setting(&self, id: &str) {
        self.app.toggle_setting(id);
        self.refresh();
    }
    fn choose_profile(&self, id: &str) {
        self.app.choose_profile(id);
        self.refresh();
    }
}

/// What a Unit crate provides to be hosted.
pub trait HostedApp: Sized + 'static {
    type Window: SurfaceWindow;
    fn id() -> AppId;
    fn spec() -> AppSpec;
    /// Footer label for data provenance, e.g. "SIMULATED".
    fn provenance(&self) -> String;
    /// Surface-local key hints, shown in the footer and help overlay.
    fn hints(surface: &str) -> Vec<(&'static str, &'static str)>;
    /// Header stamp for a surface: "MOCK" unless its data is real.
    fn badge(&self, _surface: &str) -> String {
        "MOCK".into()
    }
    /// A validated Patch command from the control socket, CLI or AI.
    fn command(
        &self,
        _cmd: patch_core::command::Command,
        _origin: patch_core::command::Origin,
    ) -> Result<(), String> {
        Err("this app takes no commands".into())
    }
    /// Read-only state as one JSON line (e.g. `ai`), for other frontends.
    fn query(&self, _what: &str) -> Option<String> {
        None
    }
    /// A window closed. Its Unit may keep running (and should say so).
    /// `open` lists the windows still open.
    fn window_closed(&self, _surface: &str, _open: &[String]) {}
    /// Switches the controller offers.
    fn settings(&self) -> Vec<SettingRow> {
        vec![]
    }
    fn toggle_setting(&self, _id: &str) {}
    /// Ready-made setups the controller offers, and choosing one.
    fn profiles(&self) -> Vec<ProfileRow> {
        vec![]
    }
    fn choose_profile(&self, _id: &str) {}
    /// A window's own name, for a window that shows one of several things
    /// (one rack plugin per `plugin#N`): its header and Wayland title.
    fn instance_title(&self, _window: &str) -> Option<String> {
        None
    }
    /// A surface the current setup leaves out (its module switched off).
    fn surface_hidden(&self, _surface: &str) -> bool {
        false
    }
    /// A layout the menus leave out (its modules switched off).
    fn layout_hidden(&self, _layout: &str) -> bool {
        false
    }
    /// A layout as this app opens it now (`screen`: the focused monitor's
    /// usable size, when known): the recipe as built in, by default.
    fn arrange(&self, recipe: LayoutRecipe, _screen: Option<(f32, f32)>) -> LayoutRecipe {
        recipe
    }
    fn new() -> Rc<Self>;
    /// Create a window for `surface`, bound to the shared domain state.
    fn create(self: &Rc<Self>, surface: &str) -> Self::Window;
}

#[derive(Clone)]
pub struct SurfaceRow {
    pub id: String,
    pub title: String,
    pub unit: String,
    pub key: String,
    pub open: bool,
    /// Audio, DJ Booth, Tools, or Planned (not built yet).
    pub group: String,
    /// The first row of its group (a heading goes above it), and how many
    /// groups start at or before it.
    pub first: bool,
    pub section: i32,
}

/// A switch in the controller.
#[derive(Clone)]
pub struct SettingRow {
    pub id: String,
    pub label: String,
    /// What it does, in one line.
    pub detail: String,
    /// Where it's listed ("Setup", or a Unit's name).
    pub group: String,
    pub on: bool,
    /// A button that opens something (a page, a terminal), not a switch.
    pub action: bool,
}

/// A ready-made setup in the controller.
#[derive(Clone)]
pub struct ProfileRow {
    pub id: String,
    pub name: String,
    pub about: String,
    pub active: bool,
}

/// One Unit as the controller lists it.
#[derive(Clone)]
pub struct UnitRow {
    pub name: String,
    pub scope: String,
    /// "live", "mock" or "planned".
    pub status: String,
    pub open: bool,
}

#[derive(Clone)]
pub struct TileRow {
    pub surface: String,
    pub title: String,
    pub rect: [f32; 4],
    pub planned: bool,
}

#[derive(Clone)]
pub struct LayoutRow {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub available: bool,
    pub note: String,
    pub tiles: Vec<TileRow>,
}

/// Everything the `Host` global of one window displays.
pub struct HostState {
    pub app_name: String,
    pub app_kind: String,
    /// The Unit this window belongs to ("" for the controller).
    pub unit_name: String,
    /// What the Unit gathers (for a planned Unit's placeholder window).
    pub unit_scope: String,
    pub surface_id: String,
    /// The window's own id: the surface id, or an instance id (`plugin#3`).
    pub window_id: String,
    pub surface_title: String,
    /// Header heading: "Mixer", or "DJ Booth · Deck A".
    pub heading: String,
    /// Wayland title; Hyprland rules and focus dispatch match on it.
    pub window_title: String,
    pub units: Vec<UnitRow>,
    pub surfaces: Vec<SurfaceRow>,
    pub layouts: Vec<LayoutRow>,
    pub active_layout: String,
    pub hints: Vec<(String, String)>,
    pub common_hints: Vec<(String, String)>,
    /// The shared grammar, for the help's "Everywhere" group (keys::GLOBAL).
    pub global_hints: Vec<(String, String)>,
    pub surface_keys: Vec<(String, String)>,
    pub provenance: String,
    pub badge: String,
    pub version: String,
    pub hyprland: bool,
    /// The app's switches, and its ready-made setups.
    pub settings: Vec<SettingRow>,
    pub profiles: Vec<ProfileRow>,
}

/// What every window's [`HostState`] holds alike: built once per refresh,
/// not once per window. It asks the engine three questions (the settings)
/// and reads the setup for every Unit and layout; DJ Studio refreshed all
/// open windows after each of its 12 opened, 78 builds and 234 engine
/// calls behind the curtain, seconds when the engine was busy.
#[derive(Clone)]
pub struct HostShared {
    units: Vec<UnitRow>,
    surfaces: Vec<SurfaceRow>,
    layouts: Vec<LayoutRow>,
    active_layout: String,
    common_hints: Vec<(String, String)>,
    surface_keys: Vec<(String, String)>,
    provenance: String,
    version: String,
    hyprland: bool,
    settings: Vec<SettingRow>,
    profiles: Vec<ProfileRow>,
}

/// The controller's switches with each group together, groups in the
/// order they first appear: the Setup tab shows one heading per group (an
/// app that adds a group's rows in several places, as the DJ Booth's are,
/// repeated its heading).
pub fn grouped(rows: Vec<SettingRow>) -> Vec<SettingRow> {
    let mut order: Vec<String> = Vec::new();
    for r in &rows {
        if !order.contains(&r.group) {
            order.push(r.group.clone());
        }
    }
    let mut rows = rows;
    // Stable: rows keep their order within a group.
    rows.sort_by_key(|r| order.iter().position(|g| *g == r.group));
    rows
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(k, l)| (k.to_string(), l.to_string())).collect()
}

impl HostState {
    /// `surface` is a window id: a surface id, `controller`, or an instance
    /// id like `meters#2` (hints and badge follow the surface; the title
    /// carries the instance number).
    pub fn build<U: HostedApp>(
        app: &U,
        spec: &AppSpec,
        surface: &str,
        open: &[String],
        active_layout: Option<&str>,
    ) -> Self {
        Self::with_shared(app, spec, surface, HostShared::build(app, spec, open, active_layout))
    }

    /// One window's state around the part all windows share.
    pub fn with_shared<U: HostedApp>(
        app: &U,
        spec: &AppSpec,
        surface: &str,
        shared: HostShared,
    ) -> Self {
        let inst = spec.instance(surface);
        let this = inst.map(|(s, _)| s);
        let named = app.instance_title(surface);
        let title = if surface == CONTROLLER {
            "Controller".to_string()
        } else if let Some(named) = &named {
            named.clone()
        } else {
            inst.map(|(s, n)| s.instance_title(n))
                .unwrap_or_else(|| surface.to_string())
        };
        let window_title = match (inst, &named) {
            (Some((s, _)), Some(named)) => s.named_window_title(spec.id, named),
            (Some((s, n)), None) => s.instance_window_title(spec.id, n),
            (None, _) => format!("{} · {}", spec.id.name(), title),
        };
        let heading = window_title
            .strip_prefix(&format!("{} · ", spec.id.name()))
            .unwrap_or(&title)
            .to_string();
        let window_id = surface;
        let surface = instance::base(surface);
        let HostShared {
            units,
            surfaces,
            layouts,
            active_layout,
            common_hints,
            surface_keys,
            provenance,
            version,
            hyprland,
            settings,
            profiles,
        } = shared;
        HostState {
            app_name: spec.id.name().to_string(),
            app_kind: spec.id.kind().to_string(),
            unit_name: this.map(|s| s.unit.name().to_string()).unwrap_or_default(),
            unit_scope: this.map(|s| s.unit.scope().to_string()).unwrap_or_default(),
            surface_id: surface.to_string(),
            window_id: window_id.to_string(),
            surface_title: title,
            heading,
            window_title,
            units,
            surfaces,
            layouts,
            active_layout,
            hints: pairs(&U::hints(surface)),
            common_hints,
            global_hints: pairs(&keys::pairs(keys::GLOBAL)),
            surface_keys,
            provenance,
            badge: app.badge(surface),
            version,
            hyprland,
            settings,
            profiles,
        }
    }
}

impl HostShared {
    pub fn build<U: HostedApp>(
        app: &U,
        spec: &AppSpec,
        open: &[String],
        active_layout: Option<&str>,
    ) -> Self {
        HostShared {
            units: spec
                .units()
                .into_iter()
                .map(|u| UnitRow {
                    name: u.kind.name().into(),
                    scope: u.kind.scope().into(),
                    status: match u.surfaces.first() {
                        Some(first) if u.implemented => {
                            if app.badge(first) == "LIVE" {
                                "live"
                            } else {
                                "mock"
                            }
                        }
                        _ => "planned",
                    }
                    .into(),
                    open: u.surfaces.iter().any(|s| open.iter().any(|o| o == s)),
                })
                .collect(),
            surfaces: {
                // By group, in the spec's order within one; planned last.
                let order = ["Audio", "DJ Booth", "Tools", "Planned"];
                let mut rows: Vec<SurfaceRow> = spec
                    .openable()
                    .filter(|s| !app.surface_hidden(s.id))
                    .map(|s| SurfaceRow {
                        id: s.id.into(),
                        title: s.title.into(),
                        unit: s.unit.name().into(),
                        key: s.key.map(String::from).unwrap_or_default(),
                        open: open.iter().any(|o| instance::is_of(o, s.id)),
                        group: if s.origin.is_implemented() {
                            s.unit.group()
                        } else {
                            "Planned"
                        }
                        .into(),
                        first: false,
                        section: 0,
                    })
                    .collect();
                rows.sort_by_key(|r| {
                    order
                        .iter()
                        .position(|g| *g == r.group)
                        .unwrap_or(order.len())
                });
                let mut section = 0;
                for i in 0..rows.len() {
                    rows[i].first = i == 0 || rows[i - 1].group != rows[i].group;
                    section += rows[i].first as i32;
                    rows[i].section = section;
                }
                rows
            },
            layouts: spec
                .layouts
                .iter()
                .filter(|l| !app.layout_hidden(l.id))
                .map(|l| LayoutRow {
                    id: l.id.into(),
                    name: l.name.into(),
                    summary: l.summary.into(),
                    available: l.is_available(),
                    note: match l.status {
                        patch_core::LayoutStatus::Available => String::new(),
                        patch_core::LayoutStatus::PluginMock { plugin } => {
                            format!("{plugin} plugin · not implemented")
                        }
                    },
                    tiles: l
                        .tiles
                        .iter()
                        .map(|t| {
                            let s = spec.instance(t.surface).map(|(s, _)| s);
                            // Several workspaces: side by side, with a gap.
                            let n = l.workspaces() as f32;
                            let gap = if n > 1.0 { 0.04 } else { 0.0 };
                            let span = (1.0 - gap * (n - 1.0)) / n;
                            let x = t.ws as f32 * (span + gap) + t.x * span;
                            TileRow {
                                surface: t.surface.into(),
                                title: s
                                    .map(|s| {
                                        if s.origin.is_implemented() {
                                            s.title
                                        } else {
                                            s.unit.name()
                                        }
                                    })
                                    .unwrap_or(t.surface)
                                    .into(),
                                rect: [x, t.y, t.w * span, t.h],
                                planned: s.is_none_or(|s| !s.origin.is_implemented()),
                            }
                        })
                        .collect(),
                })
                .collect(),
            active_layout: active_layout.unwrap_or("").to_string(),
            common_hints: pairs(keys::COMMON_HINTS),
            surface_keys: spec
                .openable()
                .filter_map(|s| Some((s.key?.to_string(), s.title.to_lowercase())))
                .collect(),
            provenance: app.provenance(),
            // The commit too, so every footer says exactly which build runs.
            version: match patch_core::git() {
                "" => format!("v{}", patch_core::VERSION),
                git => format!("v{} · {git}", patch_core::VERSION),
            },
            hyprland: hypr::available(),
            settings: grouped(app.settings()),
            profiles: app.profiles(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Launch {
    Controller,
    Open(Vec<String>),
    Layout(String),
    Headless,
}

pub struct Session<U: HostedApp> {
    app: Rc<U>,
    /// The built-in layouts, then the ones you saved (they change at run time).
    spec: RefCell<AppSpec>,
    /// Ids of the built-in layouts: never overwritten or deleted.
    builtin_layouts: Vec<&'static str>,
    palette: RefCell<patch_ui::Palette>,
    theme_watch: slint::Timer,
    /// Checks every window against its surface's minimum size.
    size_watch: slint::Timer,
    /// Each window's size at the last look, and the redraws it still owes.
    drawn_sizes: RefCell<std::collections::HashMap<String, (slint::PhysicalSize, u8)>>,
    /// Surfaces the user accepted smaller than they need (remembered).
    small_ok: RefCell<std::collections::HashSet<String>>,
    /// Each Unit's zoom (Ctrl+= / Ctrl+- / Ctrl+0), remembered; 100 % is absent.
    zoom: RefCell<std::collections::HashMap<String, f32>>,
    /// Per window: the monitor's own scale and the scale Patch last set.
    scales: RefCell<std::collections::HashMap<String, (f32, f32)>>,
    windows: RefCell<Vec<(String, U::Window)>>,
    active_layout: RefCell<Option<String>>,
    /// The active layout's windows are what is on screen (not after it was
    /// closed, or with windows opened one by one).
    layout_shown: Cell<bool>,
    /// The workspace a running layout opens its windows on.
    target_ws: Cell<Option<i64>>,
    job: RefCell<Option<slint::Timer>>,
    /// While a layout replays: a window that opens gets its own state (the
    /// part all windows share built once for the whole replay), and every
    /// window is refreshed once when the replay ends, not after each opens.
    replaying: Cell<bool>,
    replay_shared: RefCell<Option<HostShared>>,
    /// Covers the monitor while a layout builds its windows.
    curtain: RefCell<Option<Curtain>>,
    me: Weak<Self>,
}

impl<U: HostedApp> Session<U> {
    pub fn new() -> Rc<Self> {
        let mut spec = U::spec();
        let builtin_layouts = spec.layouts.iter().map(|l| l.id).collect();
        spec.layouts.extend(patch_core::user_layouts::load());
        Rc::new_cyclic(|me| Session {
            app: U::new(),
            active_layout: RefCell::new(spec.layouts.first().map(|l| l.id.to_string())),
            target_ws: Cell::new(None),
            spec: RefCell::new(spec),
            builtin_layouts,
            layout_shown: Cell::new(false),
            palette: RefCell::new(patch_ui::load_palette()),
            theme_watch: slint::Timer::default(),
            size_watch: slint::Timer::default(),
            drawn_sizes: RefCell::default(),
            small_ok: RefCell::new(load_small_ok()),
            zoom: RefCell::new(load_zoom()),
            scales: RefCell::default(),
            windows: RefCell::default(),
            job: RefCell::default(),
            replaying: Cell::new(false),
            replay_shared: RefCell::default(),
            curtain: RefCell::default(),
            me: me.clone(),
        })
    }

    /// Follow `omarchy-theme-set` while the session runs.
    fn watch_theme(&self) {
        let me = self.me.clone();
        let last = std::cell::Cell::new((patch_ui::theme_stamp(), patch_ui::style::stamp()));
        self.theme_watch.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(1500),
            move || {
                // The Omarchy theme and the unit styles (Settings · Style).
                let stamp = (patch_ui::theme_stamp(), patch_ui::style::stamp());
                if stamp == last.get() {
                    return;
                }
                last.set(stamp);
                if let Some(s) = me.upgrade() {
                    *s.palette.borrow_mut() = patch_ui::load_palette();
                    let styles = patch_ui::style::load();
                    for (id, w) in s.windows.borrow().iter() {
                        w.apply_theme(&s.palette.borrow());
                        w.set_style(styles.style(id));
                    }
                }
            },
        );
    }

    /// Every 700 ms: is each window big enough to show all of its surface?
    /// If not (and the user hasn't accepted it small), its frame says so and
    /// offers to make room. Layouts already give every surface its minimum.
    ///
    /// A resized window also draws again once it is on screen: a window a
    /// layout tiles on a workspace nobody is looking at keeps a frame of its
    /// old size, squeezed or cut off, and with nothing animating it stays so
    /// until the pointer passes. Twice, as the resize can arrive just before
    /// this look; nothing draws while no window's size changes.
    fn watch_sizes(&self) {
        if crate::shot::active() {
            return;
        }
        let me = self.me.clone();
        self.size_watch.start(slint::TimerMode::Repeated, Duration::from_millis(700), move || {
            let Some(s) = me.upgrade() else { return };
            s.apply_zoom();
            s.drawn_sizes.borrow_mut().retain(|id, _| s.windows.borrow().iter().any(|(w, _)| w == id));
            let sight = hypr::visible_titles();
            for (id, w) in s.windows.borrow().iter() {
                let base = instance::base(id);
                let now = w.window().size();
                let mut sizes = s.drawn_sizes.borrow_mut();
                // A new window owes too: it can reach its size before this first look.
                let (seen, owed) = sizes.entry(id.clone()).or_insert((now, 2));
                if *seen != now {
                    (*seen, *owed) = (now, 2);
                }
                if *owed > 0
                    && sight.as_ref().is_none_or(|v| v.read().is_ok_and(|v| v.contains(&s.title(id))))
                {
                    *owed -= 1;
                    w.window().request_redraw();
                }
                drop(sizes);
                let note = match s.spec().instance(id) {
                    Some((spec, _)) if !s.small_ok.borrow().contains(base) => {
                        let (mw, mh) = minimum_size(id, spec.min_size);
                        let win = w.window();
                        let size = win.size().to_logical(win.scale_factor());
                        // A little slack: borders and rounding never nag.
                        if size.width > 1.0 && (size.width + 8.0 < mw as f32 || size.height + 8.0 < mh as f32) {
                            format!("{} shows everything from {mw}×{mh} · this window is {:.0}×{:.0}", spec.title, size.width, size.height)
                        } else {
                            String::new()
                        }
                    }
                    _ => String::new(),
                };
                w.set_size_note(&note);
            }
        });
    }

    /// Zoom a Unit's windows a step in or out, or back to 100 % (`step` 0).
    fn zoom(&self, surface: &str, step: i8) {
        let unit = instance::base(surface).to_string();
        let now = self.zoom.borrow().get(&unit).copied().unwrap_or(1.0);
        let next = zoom_step(now, step);
        {
            let mut zoom = self.zoom.borrow_mut();
            if (next - 1.0).abs() < 1e-3 {
                zoom.remove(&unit);
            } else {
                zoom.insert(unit, next);
            }
            save_zoom(&zoom);
        }
        self.apply_zoom();
    }

    /// Draw each window at its monitor's scale times its Unit's zoom. When
    /// the scale is not the one Patch set, the compositor rescaled the
    /// window (a display scale change, a move to another monitor): that is
    /// the new monitor scale, and the zoom goes on top of it again.
    fn apply_zoom(&self) {
        use slint::platform::WindowEvent;
        let zoom = self.zoom.borrow();
        let windows = self.windows.borrow();
        let mut scales = self.scales.borrow_mut();
        scales.retain(|id, _| windows.iter().any(|(w, _)| w == id));
        for (id, w) in windows.iter() {
            let win = w.window();
            let now = win.scale_factor();
            let (monitor, set) = scales.entry(id.clone()).or_insert((now, now));
            if (now - *set).abs() > 1e-3 {
                (*monitor, *set) = (now, now);
            }
            let want = *monitor * zoom.get(instance::base(id)).copied().unwrap_or(1.0);
            if (now - want).abs() > 1e-3 {
                win.dispatch_event(WindowEvent::ScaleFactorChanged { scale_factor: want });
                *set = want;
            }
        }
    }

    fn make_room_for(&self, surface: &str, w: u32, h: u32) {
        hypr::make_room(&self.title(surface), w + 40, h + 40);
    }

    fn make_room(&self, surface: &str) {
        let Some((w, h)) = self
            .spec()
            .instance(surface)
            .map(|(s, _)| minimum_size(surface, s.min_size))
        else {
            return;
        };
        // Its minimum plus a little air, floating in the middle.
        hypr::make_room(&self.title(surface), w as u32 + 40, h as u32 + 40);
    }

    fn keep_small(&self, surface: &str) {
        let base = instance::base(surface).to_string();
        self.small_ok.borrow_mut().insert(base);
        save_small_ok(&self.small_ok.borrow());
        for (id, w) in self.windows.borrow().iter() {
            if instance::base(id) == instance::base(surface) {
                w.set_size_note("");
            }
        }
    }

    fn spec(&self) -> std::cell::Ref<'_, AppSpec> {
        self.spec.borrow()
    }

    /// A layout as it opens here and now (see [`HostedApp::arrange`]): the
    /// one recipe opening, "moved" checks and reset all go by.
    fn recipe(&self, id: &str) -> Option<LayoutRecipe> {
        let recipe = self.spec().layout(id).cloned()?;
        let screen = hypr::available().then(hypr::focused_area).flatten();
        Some(self.app.arrange(recipe, screen))
    }

    fn open_ids(&self) -> Vec<String> {
        self.windows
            .borrow()
            .iter()
            .map(|(s, _)| s.clone())
            .filter(|s| s != CONTROLLER)
            .collect()
    }

    fn title(&self, surface: &str) -> String {
        let named = self.app.instance_title(surface);
        match self.spec().instance(surface) {
            Some((s, _)) if let Some(named) = named => s.named_window_title(self.spec().id, &named),
            Some((s, n)) => s.instance_window_title(self.spec().id, n),
            None => format!("{} · Controller", self.spec().id.name()),
        }
    }

    pub fn refresh(&self) {
        let shared = self.shared();
        let spec = self.spec();
        for (surface, w) in self.windows.borrow().iter() {
            w.apply_host(&HostState::with_shared(&*self.app, &spec, surface, shared.clone()));
        }
    }

    fn shared(&self) -> HostShared {
        let layout = self.active_layout.borrow().clone();
        HostShared::build(&*self.app, &self.spec(), &self.open_ids(), layout.as_deref())
    }

    /// A window a layout replay just created: only its own state, on the
    /// shared part built once for the replay. False when no replay runs.
    fn refresh_opening(&self, surface: &str) -> bool {
        if !self.replaying.get() || self.job.borrow().is_none() {
            return false;
        }
        let shared = self
            .replay_shared
            .borrow_mut()
            .get_or_insert_with(|| self.shared())
            .clone();
        if let Some((_, w)) = self.windows.borrow().iter().find(|(s, _)| s == surface) {
            w.apply_host(&HostState::with_shared(&*self.app, &self.spec(), surface, shared));
        }
        true
    }

    /// The replay is over (done or replaced): every window up to date.
    fn end_replay(&self) {
        let was = self.replaying.replace(false);
        self.replay_shared.borrow_mut().take();
        if was {
            self.refresh();
        }
    }

    fn is_open(&self, surface: &str) -> bool {
        self.windows.borrow().iter().any(|(s, _)| s == surface)
    }

    fn window_ids(&self) -> Vec<String> {
        self.windows
            .borrow()
            .iter()
            .map(|(s, _)| s.clone())
            .collect()
    }

    /// The open window `surface` names: that window for an exact id, else
    /// the first open instance of the surface.
    fn resolve(&self, surface: &str) -> Option<String> {
        let open = self.window_ids();
        if open.iter().any(|o| o == surface) {
            return Some(surface.to_string());
        }
        if instance::number(surface) != Some(1) {
            return None;
        }
        instance::first(surface, &open).map(String::from)
    }

    /// Open a surface window, or focus it if it is already open (for a
    /// multi surface: its first open instance).
    pub fn open(&self, surface: &str) -> bool {
        match self.resolve(surface) {
            Some(id) => {
                self.focus(&id);
                true
            }
            None => self.open_window(surface),
        }
    }

    /// `open` from the command line: a multi surface that is already open
    /// gets another window; anything else is opened or focused.
    pub fn open_another(&self, surface: &str) -> bool {
        let multi = self.spec().surface(surface).is_some_and(|s| s.multi);
        if multi && self.resolve(surface).is_some() {
            return self.open_window(&instance::next(surface, &self.window_ids()));
        }
        self.open(surface)
    }

    /// Open the window with exactly this id, or focus it if it is open.
    fn open_window(&self, surface: &str) -> bool {
        let openable = surface == CONTROLLER
            || self
                .spec()
                .instance(surface)
                .is_some_and(|(s, _)| s.origin.can_open());
        if !openable {
            return false;
        }
        if self.is_open(surface) {
            self.focus(surface);
            return true;
        }
        let t0 = Instant::now();
        let log = std::env::var_os("PATCH_STARTUP_LOG").is_some();
        let w = self.app.create(instance::base(surface));
        let t_create = t0.elapsed();
        w.apply_theme(&self.palette.borrow());
        w.set_style(patch_ui::style::load().style(surface));
        let t_theme = t0.elapsed();
        let ops: Weak<dyn HostOps> = self.me.clone();
        w.bind_host(ops, surface);
        let me = self.me.clone();
        let id = surface.to_string();
        w.window().on_close_requested(move || {
            if let Some(s) = me.upgrade() {
                s.close_window(&id);
            }
            slint::CloseRequestResponse::HideWindow
        });
        self.windows.borrow_mut().push((surface.to_string(), w));
        let t_bind = t0.elapsed();
        if !self.refresh_opening(surface) {
            self.refresh();
        }
        let t_refresh = t0.elapsed();
        if let Some((_, w)) = self.windows.borrow().iter().find(|(s, _)| s == surface) {
            let _ = w.show();
            if instance::base(surface) == "dj-club" && !crate::shot::active() {
                // A maximize request before show can be lost when an existing
                // host maps another window. Apply it after mapping was requested.
                w.window().set_maximized(true);
            }
        }
        if log {
            eprintln!(
                "open {surface}: heap {:.1} MB · create {} · theme {} · bind {} · refresh {} · show {} ms",
                crate::heap::in_use_mb(),
                t_create.as_millis(),
                (t_theme - t_create).as_millis(),
                (t_bind - t_theme).as_millis(),
                (t_refresh - t_bind).as_millis(),
                (t0.elapsed() - t_refresh).as_millis()
            );
        }
        if hypr::available() {
            self.when_mapped(surface, hypr::make_opaque);
        }
        true
    }

    /// Run `f(title)` once the compositor has mapped the surface's window.
    fn when_mapped(&self, surface: &str, f: fn(&str) -> bool) {
        let title = self.title(surface);
        let tries = std::cell::Cell::new(0);
        let timer = Rc::new(slint::Timer::default());
        let keep = timer.clone();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(60),
            move || {
                tries.set(tries.get() + 1);
                if hypr::has_window(&title) {
                    f(&title);
                    keep.stop();
                } else if tries.get() > 40 {
                    keep.stop();
                }
            },
        );
    }

    pub fn focus(&self, surface: &str) {
        if hypr::available() {
            hypr::focus_title(&self.title(surface));
        } else if let Some((_, w)) = self.windows.borrow().iter().find(|(s, _)| s == surface) {
            let _ = w.show();
        }
    }

    /// Close by name: the window with exactly this id, or for a plain
    /// surface id the newest of its instances (`close meters`). A window
    /// closing itself goes through `close_window` with its own id.
    pub fn close(&self, surface: &str) {
        let open = self.window_ids();
        let id = if open.iter().any(|o| o == surface) && instance::number(surface) != Some(1) {
            Some(surface.to_string())
        } else {
            instance::newest(surface, &open).map(String::from)
        };
        if let Some(id) = id {
            self.close_window(&id);
        }
    }

    /// Hide now, drop on the next turn of the event loop (the request may be
    /// coming from inside that very window's callback).
    pub fn close_window(&self, surface: &str) {
        let taken = {
            let mut ws = self.windows.borrow_mut();
            ws.iter()
                .position(|(s, _)| s == surface)
                .map(|i| ws.remove(i))
        };
        if let Some((_, w)) = taken {
            let _ = w.hide();
            slint::Timer::single_shot(Duration::ZERO, move || drop(w));
            if surface != CONTROLLER {
                self.app
                    .window_closed(instance::base(surface), &self.open_ids());
                if self.open_ids().is_empty() {
                    self.layout_shown.set(false);
                }
            }
        }
        self.refresh();
    }

    /// Close the surface (its newest instance) if it is open, else open it.
    pub fn toggle(&self, surface: &str) {
        if self.resolve(surface).is_some() {
            self.close(surface);
        } else {
            // A Hyprland reload drops rules set at run time: set them again
            // (keybinds toggle; the Performance window floats).
            hypr::install_rules();
            self.open(surface);
        }
    }

    pub fn controller(&self) {
        self.open(CONTROLLER);
    }

    fn cycle(&self, from: &str, forward: bool) {
        // Spec order; instances of one surface together, in number order.
        let mut order = self.open_ids();
        order.sort_by_key(|id| {
            let pos = self
                .spec()
                .openable()
                .position(|s| s.id == instance::base(id));
            (
                pos.unwrap_or(usize::MAX),
                instance::number(id).unwrap_or(u32::MAX),
            )
        });
        if order.is_empty() {
            return;
        }
        let i = order.iter().position(|s| s == from).unwrap_or(0);
        let n = order.len();
        let next = if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        };
        self.focus(&order[next]);
    }

    /// Shared keyboard grammar for keys a surface did not consume.
    pub fn key(&self, surface: &str, text: &str, ctrl: bool, shift: bool) -> bool {
        let from_controller = surface == CONTROLLER;
        if std::env::var_os("UNIT_DEBUG_KEYS").is_some() {
            eprintln!("key {surface}: {:?} ctrl={ctrl} shift={shift}", text);
        }
        match keys::resolve(text, ctrl, shift) {
            // In the controller Tab moves between its own tabs.
            Some(Command::NextSurface | Command::PrevSurface) if from_controller => return false,
            Some(Command::NextSurface) => self.cycle(surface, true),
            Some(Command::PrevSurface) => self.cycle(surface, false),
            Some(Command::Controller) if from_controller => self.close_window(CONTROLLER),
            Some(Command::Controller) => self.controller(),
            // Closes the windows only: the engine (and the music) runs on.
            Some(Command::Quit) if from_controller => self.quit(),
            Some(Command::Quit) => return false,
            Some(Command::Zoom(step)) => self.zoom(surface, step),
            Some(Command::Surface(c)) => {
                let Some(id) = self.spec().surface_for_key(c).map(|s| s.id) else {
                    return false;
                };
                self.open(id);
                if from_controller {
                    self.close_window(CONTROLLER);
                }
            }
            Some(Command::NewSurface(c)) => {
                let Some(id) = self.spec().surface_for_key(c).map(|s| s.id) else {
                    return false;
                };
                self.open_another(id);
                if from_controller {
                    self.close_window(CONTROLLER);
                }
            }
            None => return false,
        }
        true
    }

    /// Replace the open surfaces with a layout recipe.
    pub fn open_layout(&self, id: &str) -> bool {
        let Some(recipe) = self.recipe(id) else {
            return false;
        };
        if !recipe.is_available() {
            return false;
        }
        // Reuse this recipe's existing first workspace. Its windows are
        // hidden asynchronously below, so asking Hyprland for an empty
        // workspace immediately afterward would skip over them and drift
        // the layout to 6/7/8 on every replay.
        let existing_first_workspace = if hypr::available() {
            recipe
                .tiles
                .iter()
                .filter(|tile| tile.ws == 0)
                .find_map(|tile| {
                    self.resolve(tile.surface)
                        .and_then(|surface| hypr::workspace_for_title(&self.title(&surface)))
                })
        } else {
            None
        };
        // A previous layout may still be waiting for compositor window maps.
        // Drop that timer before it can resume opening its own windows.
        self.job.borrow_mut().take();
        self.replaying.set(false);
        self.replay_shared.borrow_mut().take();
        self.target_ws.set(None);
        *self.active_layout.borrow_mut() = Some(recipe.id.to_string());
        // A layout is a complete starting arrangement. Clear every open
        // surface first so windows from the previous layout (for example
        // AI DJ or Studio units) cannot remain visible beside it.
        for s in self.open_ids() {
            self.close_window(&s);
        }
        self.close_window(CONTROLLER);
        self.layout_shown.set(true);

        let spec = self.spec();
        // Recipe tiles may name an extra instance of a multi surface (`meters#2`).
        let openable = |id: &str| spec.instance(id).is_some_and(|(s, _)| s.origin.can_open());
        let steps: VecDeque<Step> = match (hypr::available(), recipe.plan_open(openable)) {
            (true, Some(plan)) => {
                // An explicit empty workspace: every window is checked
                // against it, so none lands on the user's workspace.
                let Some(ws) = existing_first_workspace.or_else(|| hypr::first_empty_workspace(0))
                else {
                    return false;
                };
                self.target_ws.set(Some(ws));
                // A floating or scrolling workspace (Omarchy 4.5, Super+L)
                // ignores preselect and splitratio. Tile it before the
                // curtain, so the cover itself is not restyled as a cascade.
                if patch_core::layout::opens_tiled(&plan) {
                    hypr::ensure_dwindle(ws);
                }
                // The first Open step switches to `ws`, once the curtain
                // is up to hide the switching and splitting that follow.
                hypr::install_rules();
                let title = format!("{} · Opening layout", self.spec().id.name());
                let heading = format!("Opening {}", recipe.name);
                self.curtain.borrow_mut().take();
                *self.curtain.borrow_mut() =
                    Curtain::show(&title, &heading, &self.palette.borrow());
                plan.into()
            }
            _ => recipe
                .surfaces()
                .filter(|id| openable(id))
                .map(Step::Open)
                .collect(),
        };
        self.replaying.set(true);
        self.run_steps(steps);
        true
    }

    /// Replays compositor steps, waiting for each window to map before the
    /// next split is issued.
    fn run_steps(&self, mut steps: VecDeque<Step>) {
        let me = self.me.clone();
        let mut waiting: Option<(String, Instant)> = None;
        let mut curtain_wait = Some(Instant::now());
        let timer = slint::Timer::default();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(10),
            move || {
                let Some(s) = me.upgrade() else { return };
                if let Some(since) = curtain_wait {
                    let title = s.curtain.borrow().as_ref().map(|c| c.title().to_string());
                    if let Some(title) = title {
                        let mapped = hypr::has_window(&title);
                        if !mapped && since.elapsed() < Duration::from_millis(800) {
                            return;
                        }
                        if mapped {
                            hypr::make_opaque(&title);
                        }
                    }
                    curtain_wait = None;
                }
                if let Some((title, since)) = &waiting {
                    let mapped = !hypr::available() || hypr::has_window(title);
                    if !mapped && since.elapsed() < Duration::from_millis(1500) {
                        return;
                    }
                    if mapped
                        && hypr::available()
                        && let Some(ws) = s.target_ws.get()
                    {
                        if hypr::workspace_for_title(title) != Some(ws) {
                            hypr::move_title_to_workspace(title, ws);
                        }
                        hypr::focus_workspace(ws);
                        hypr::focus_title(title);
                    }
                    if std::env::var_os("PATCH_LAYOUT_LOG").is_some() {
                        eprintln!(
                            "layout: {title} {} after {} ms",
                            if mapped { "mapped" } else { "NOT mapped" },
                            since.elapsed().as_millis()
                        );
                    }
                    waiting = None;
                }
                // Progress: the curtain's watchdog counts from here.
                if let Some(c) = s.curtain.borrow().as_ref() {
                    c.alive();
                }
                // The steps between windows (focus, preselect, ratio, the next
                // workspace, placing) need no wait: all of them in this tick.
                loop {
                    match steps.pop_front() {
                        Some(Step::Open(surface)) => {
                            // Focus may have moved while windows mapped: back to
                            // the layout's workspace first.
                            if let Some(ws) = s.target_ws.get()
                                && hypr::active_workspace_id() != Some(ws)
                            {
                                hypr::focus_workspace(ws);
                            }
                            // Exactly this window: another open instance must not
                            // stand in for the recipe's tile.
                            s.open_window(surface);
                            waiting = Some((s.title(surface), Instant::now()));
                            return; // the next steps wait until it maps
                        }
                        Some(Step::Focus(surface)) => s.focus(surface),
                        Some(Step::Preselect(d)) => {
                            hypr::preselect(d);
                        }
                        Some(Step::NextWorkspace) => {
                            let next = hypr::first_empty_workspace(s.target_ws.get().unwrap_or(0));
                            s.target_ws.set(next);
                            match next {
                                Some(ws) => {
                                    if patch_core::layout::opens_tiled(steps.make_contiguous()) {
                                        hypr::ensure_dwindle(ws);
                                    }
                                    hypr::focus_workspace(ws);
                                }
                                // A full monitor block must not mint a workspace
                                // on the next monitor. The rest of this layout
                                // stays on the workspace just filled.
                                None => {
                                    hypr::focus_empty_workspace();
                                }
                            };
                        }
                        Some(Step::Ratio(r)) => {
                            hypr::split_ratio(r);
                        }
                        Some(Step::Place {
                            surface,
                            x,
                            y,
                            w,
                            h,
                        }) => {
                            hypr::place(&s.title(surface), [x, y, w, h]);
                        }
                        None => {
                            s.target_ws.set(None);
                            s.end_replay();
                            let first = s
                                .recipe(s.active_layout.borrow().as_deref().unwrap_or(""))
                                .and_then(|l| l.tiles.first().map(|t| t.surface));
                            if let Some(f) = first {
                                s.focus(f);
                            }
                            let s2 = s.me.clone();
                            slint::Timer::single_shot(Duration::ZERO, move || {
                                if let Some(s) = s2.upgrade() {
                                    s.job.borrow_mut().take();
                                    s.curtain.borrow_mut().take();
                                }
                            });
                            return;
                        }
                    }
                }
            },
        );
        *self.job.borrow_mut() = Some(timer);
    }

    pub fn quit(&self) {
        self.vanish_all();
        for (_, w) in self.windows.borrow_mut().drain(..) {
            let _ = w.hide();
        }
        let _ = slint::quit_event_loop();
    }

    pub fn status(&self) -> Status {
        let layout = self.shown_layout();
        let edited = layout.as_deref().is_some_and(|id| self.layout_edited(id));
        Status {
            open: self.open_ids(),
            layout,
            edited,
            heap: crate::heap::stats(),
        }
    }

    /// The layout whose windows are on screen.
    fn shown_layout(&self) -> Option<String> {
        let open = !self.open_ids().is_empty();
        self.active_layout
            .borrow()
            .clone()
            .filter(|_| open && self.layout_shown.get())
    }

    /// The open windows as a layout: workspaces in order from 0, each
    /// window's rectangle a fraction of its monitor's usable area.
    fn capture(&self) -> Vec<patch_core::Tile> {
        let windows: Vec<(String, String)> = self
            .open_ids()
            .into_iter()
            .map(|id| (self.title(&id), id))
            .collect();
        let clients: Vec<(hypr::Client, &str)> = hypr::clients(Some(&U::id().app_id()))
            .into_iter()
            .filter(|c| c.workspace > 0)
            .filter_map(|c| {
                let id = windows
                    .iter()
                    .find(|(title, _)| *title == c.title)?
                    .1
                    .as_str();
                Some((c, id))
            })
            .collect();
        let mut workspaces: Vec<i64> = clients.iter().map(|(c, _)| c.workspace).collect();
        workspaces.sort();
        workspaces.dedup();
        let areas = hypr::monitor_areas();
        let tiles: Vec<patch_core::Tile> = clients
            .iter()
            .filter_map(|(c, id)| {
                let [ax, ay, aw, ah] = areas.iter().find(|(m, _)| *m == c.monitor)?.1;
                let [x, y, w, h] = c.rect;
                Some(patch_core::Tile {
                    ws: workspaces.iter().position(|ws| *ws == c.workspace)? as u8,
                    surface: patch_core::layout::intern(id),
                    x: (x - ax) / aw,
                    y: (y - ay) / ah,
                    w: w / aw,
                    h: h / ah,
                    float: c.floating,
                })
            })
            .collect();
        patch_core::layout::capture(&tiles)
    }

    /// The layout's windows no longer sit as it has them. Never while it is
    /// still opening them.
    fn layout_edited(&self, id: &str) -> bool {
        if !hypr::available() || self.job.borrow().is_some() {
            return false;
        }
        let Some(recipe) = self.recipe(id) else {
            return false;
        };
        let spec = self.spec();
        let expected =
            recipe.arranged(|id| spec.instance(id).is_some_and(|(s, _)| s.origin.can_open()));
        let actual = self.capture();
        let same = patch_core::layout::same_arrangement(&expected, &actual);
        if !same && std::env::var_os("PATCH_LAYOUT_LOG").is_some() {
            eprintln!("layout {id} changed:\n  expected {expected:?}\n  on screen {actual:?}");
        }
        !same
    }

    /// Go to the first workspace of the layout on screen.
    pub fn show_layout(&self) -> Result<(), String> {
        self.shown_layout().ok_or("no Patch layout is open")?;
        let titles: Vec<String> = self.open_ids().iter().map(|id| self.title(id)).collect();
        let first = hypr::clients(Some(&U::id().app_id()))
            .into_iter()
            .filter(|c| c.workspace > 0 && titles.contains(&c.title))
            .min_by_key(|c| c.workspace)
            .ok_or("Patch's windows are not on a workspace")?;
        hypr::focus_workspace(first.workspace);
        Ok(())
    }

    /// Put the layout on screen back as it was saved.
    pub fn reset_layout(&self) -> Result<(), String> {
        let id = self.shown_layout().ok_or("no Patch layout is open")?;
        self.open_layout(&id)
            .then_some(())
            .ok_or_else(|| format!("'{id}' can't be opened"))
    }

    /// Every open window out of sight at once, before they close one by one.
    fn vanish_all(&self) {
        if hypr::available() {
            let titles: Vec<String> = self.window_ids().iter().map(|s| self.title(s)).collect();
            hypr::vanish(&titles);
        }
    }

    /// Close every Patch window. The Units keep running: audio never stops.
    pub fn close_layout(&self) {
        self.job.borrow_mut().take();
        self.replaying.set(false);
        self.replay_shared.borrow_mut().take();
        self.curtain.borrow_mut().take();
        self.vanish_all();
        for s in self.open_ids() {
            self.close_window(&s);
        }
        self.close_window(CONTROLLER);
        self.layout_shown.set(false);
    }

    /// Save the arrangement on screen: over the layout itself when it is
    /// one you saved, or under a new name. A built-in layout is never
    /// changed: saving it needs a name. Returns the saved layout's id.
    pub fn save_layout(&self, name: Option<&str>) -> Result<String, String> {
        let active = self.shown_layout().ok_or("no Patch layout is open")?;
        let (from_name, from_summary) = {
            let spec = self.spec();
            let from = spec.layout(&active).ok_or("the layout on screen is gone")?;
            (from.name, from.summary)
        };
        let builtin = self.builtin_layouts.contains(&active.as_str());
        let name = match name.map(str::trim).filter(|n| !n.is_empty()) {
            Some(n) => n.to_string(),
            None if builtin => {
                return Err(format!("{from_name} is built in: save it under a new name"));
            }
            None => from_name.to_string(),
        };
        let id = patch_core::user_layouts::slug(&name);
        if id.is_empty() {
            return Err("the name needs a letter or a digit".into());
        }
        let taken = self.builtin_layouts.contains(&id.as_str())
            || patch_core::layout::canonical(&id) != id
            || ["show", "reset", "close", "save", "delete"].contains(&id.as_str());
        if taken {
            return Err(format!("'{name}' is a built-in name: choose another"));
        }
        if !hypr::available() {
            return Err("saving a layout needs Hyprland".into());
        }
        let tiles = self.capture();
        if tiles.is_empty() {
            return Err("no Patch windows to save".into());
        }
        let summary = if id == active && !builtin {
            from_summary.to_string()
        } else {
            format!("Your layout, saved from {from_name}.")
        };
        let check = patch_core::LayoutRecipe {
            id: "",
            name: "",
            summary: "",
            status: patch_core::LayoutStatus::Available,
            tiles: tiles.clone(),
        };
        if check.plan().is_none() {
            return Err(
                "these windows can't be replayed as splits: tile them again and save".into(),
            );
        }
        let saved = patch_core::user_layouts::save(&name, &summary, &tiles)
            .map_err(|e| format!("can't save the layout: {e}"))?;
        {
            let mut spec = self.spec.borrow_mut();
            spec.layouts.retain(|l| l.id != saved.id);
            spec.layouts.push(saved);
        }
        *self.active_layout.borrow_mut() = Some(id.clone());
        self.refresh();
        Ok(id)
    }

    /// Delete a layout you saved. Its windows stay open.
    pub fn delete_layout(&self, id: &str) -> Result<(), String> {
        if self
            .builtin_layouts
            .contains(&patch_core::layout::canonical(id))
        {
            return Err(format!("'{id}' is built in and can't be deleted"));
        }
        match patch_core::user_layouts::delete(id) {
            Ok(true) => {}
            Ok(false) => return Err(format!("there is no saved layout '{id}'")),
            Err(e) => return Err(format!("can't delete '{id}': {e}")),
        }
        self.spec.borrow_mut().layouts.retain(|l| l.id != id);
        if self.active_layout.borrow().as_deref() == Some(id) {
            self.layout_shown.set(false);
        }
        self.refresh();
        Ok(())
    }

    pub fn handle(&self, req: Request) -> String {
        let ok = |b: bool| {
            if b {
                "ok".to_string()
            } else {
                "err".to_string()
            }
        };
        let reply = |r: Result<(), String>| match r {
            Ok(()) => "ok".to_string(),
            Err(e) => format!("err {e}"),
        };
        match req {
            Request::Ping => "pong".into(),
            Request::Status => self.status().encode(),
            Request::Controller => ok(self.open(CONTROLLER)),
            Request::Open(s) => ok(self.open_another(&s)),
            Request::Close(s) => {
                self.close(&s);
                "ok".into()
            }
            Request::Toggle(s) => {
                self.toggle(&s);
                "ok".into()
            }
            Request::Layout(l) => ok(self.open_layout(&l)),
            Request::LayoutShow => reply(self.show_layout()),
            Request::LayoutReset => reply(self.reset_layout()),
            Request::LayoutClose => {
                self.close_layout();
                "ok".into()
            }
            Request::LayoutSave(name) => match self.save_layout(name.as_deref()) {
                Ok(id) => format!("ok {id}"),
                Err(e) => format!("err {e}"),
            },
            Request::LayoutDelete(id) => reply(self.delete_layout(&id)),
            Request::Command { origin, json } => {
                match patch_core::command::Command::from_json(&json) {
                    Ok(cmd) => match self.app.command(cmd, origin) {
                        Ok(()) => "ok".into(),
                        Err(e) => format!("err {e}"),
                    },
                    Err(e) => format!("err bad command: {e}"),
                }
            }
            Request::Query(q) => match self.app.query(&q) {
                Some(v) => v,
                None => format!("err nothing to query as '{q}'"),
            },
            Request::Quit => {
                self.quit();
                "ok".into()
            }
        }
    }

    pub fn launch(&self, launch: &Launch) {
        match launch {
            Launch::Controller => self.controller(),
            Launch::Open(list) => {
                for s in list {
                    self.open_another(s);
                }
            }
            Launch::Layout(l) => {
                self.open_layout(l);
            }
            Launch::Headless => {}
        }
    }
}

/// Re-run this executable with arguments, detached.
pub fn spawn_self(args: &[&str]) {
    if let Ok(exe) = std::env::current_exe()
        && let Ok(mut child) = std::process::Command::new(exe)
            .args(args)
            .stdin(std::process::Stdio::null())
            .spawn()
    {
        // Collected when it ends, so no finished process is left behind.
        std::thread::spawn(move || child.wait());
    }
}

fn launch_requests(launch: &Launch) -> Vec<Request> {
    match launch {
        Launch::Controller => vec![Request::Controller],
        Launch::Open(list) => list.iter().map(|s| Request::Open(s.clone())).collect(),
        Launch::Layout(l) => vec![Request::Layout(l.clone())],
        Launch::Headless => vec![],
    }
}

type Handler = Box<dyn Fn(Request) -> String>;

thread_local! {
    static HANDLER: RefCell<Option<Handler>> = RefCell::new(None);
}

fn dispatch(req: Request) -> String {
    HANDLER.with(|h| {
        h.borrow()
            .as_ref()
            .map(|f| f(req))
            .unwrap_or_else(|| "err no session".into())
    })
}

/// Without a saved choice: femtovg, unless the GPU's driver is known to take
/// a context and then fail every draw, which leaves windows empty. Only
/// `asahi-g15` so far (M3; `EOPNOTSUPP` on each submit, 2026-11-05).
fn default_renderer() -> &'static str {
    let driver = std::fs::read_link("/sys/class/drm/renderD128/device/driver").ok();
    renderer_for(driver.as_deref().and_then(|d| d.file_name()).and_then(|d| d.to_str()))
}

fn renderer_for(driver: Option<&str>) -> &'static str {
    match driver {
        Some("asahi-g15") => "software",
        _ => "femtovg",
    }
}

/// Select the native (winit/Wayland) backend and set the Wayland app_id that
/// Hyprland sees as the window class.
pub fn init_platform(app_id: &str) -> anyhow::Result<()> {
    // PATCH_RENDERER=femtovg|software picks the renderer (A/B tests; Skia
    // measured no better, see docs/performance.md, and is not built in).
    // ~/.config/omarchy-patch/renderer holds a machine's own choice, for
    // windows the engine, launcher or bar start: software where the GPU
    // driver accepts a context but fails every draw (an M3 on Asahi's
    // experimental G15 driver: EOPNOTSUPP on each submit, empty windows).
    let renderer = std::env::var("PATCH_RENDERER")
        .ok()
        .or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")))?;
            let s = std::fs::read_to_string(base.join("omarchy-patch/renderer")).ok()?;
            Some(s.trim().to_string()).filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| default_renderer().into());
    let chosen = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name(renderer.clone())
        .select();
    if let Err(e) = chosen {
        eprintln!("patch: renderer {renderer} unavailable ({e}), using femtovg");
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("femtovg".into())
            .select()?;
    }
    slint::set_xdg_app_id(app_id)?;
    Ok(())
}

/// Run a Unit session (or forward to the one already running).
/// Surfaces accepted smaller than they need, one id per line.
fn small_ok_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
        })?;
    Some(base.join("omarchy-patch/small-windows"))
}

fn load_small_ok() -> std::collections::HashSet<String> {
    small_ok_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

/// The zoom steps, as in a browser.
const ZOOMS: [f32; 11] = [0.5, 0.67, 0.75, 0.8, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0];

fn zoom_step(now: f32, step: i8) -> f32 {
    match step {
        0 => 1.0,
        s if s > 0 => ZOOMS.iter().copied().find(|z| *z > now + 1e-3).unwrap_or(ZOOMS[ZOOMS.len() - 1]),
        _ => ZOOMS.iter().rev().copied().find(|z| *z < now - 1e-3).unwrap_or(ZOOMS[0]),
    }
}

fn zoom_path() -> Option<std::path::PathBuf> {
    small_ok_path().map(|p| p.with_file_name("zoom"))
}

/// `unit zoom` per line.
fn load_zoom() -> std::collections::HashMap<String, f32> {
    zoom_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_zoom(&t))
        .unwrap_or_default()
}

fn parse_zoom(text: &str) -> std::collections::HashMap<String, f32> {
    text.lines()
        .filter_map(|l| {
            let (unit, z) = l.trim().split_once(' ')?;
            let z: f32 = z.trim().parse().ok()?;
            (ZOOMS[0]..=ZOOMS[ZOOMS.len() - 1]).contains(&z).then(|| (unit.to_string(), z))
        })
        .collect()
}

fn save_zoom(zoom: &std::collections::HashMap<String, f32>) {
    if let Some(p) = zoom_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")));
        let mut v: Vec<_> = zoom.iter().collect();
        v.sort_by(|a, b| a.0.cmp(b.0));
        let _ = std::fs::write(p, v.into_iter().map(|(u, z)| format!("{u} {z}\n")).collect::<String>());
    }
}

#[test]
fn zoom_steps_and_file() {
    assert_eq!(zoom_step(1.0, 1), 1.1);
    assert_eq!(zoom_step(1.0, -1), 0.9);
    assert_eq!(zoom_step(2.0, 1), 2.0, "stops at 200 %");
    assert_eq!(zoom_step(0.5, -1), 0.5, "stops at 50 %");
    assert_eq!(zoom_step(1.3, 1), 1.5, "an odd value goes to the next step");
    assert_eq!(zoom_step(1.75, 0), 1.0);
    let z = parse_zoom("dj-club 1.25\nmixer 0.8\nbad x\nhuge 9\nnan NaN\n");
    assert_eq!(z.len(), 2);
    assert_eq!(z["dj-club"], 1.25);
}

fn save_small_ok(set: &std::collections::HashSet<String>) {
    if let Some(p) = small_ok_path() {
        let _ = std::fs::create_dir_all(p.parent().unwrap_or(std::path::Path::new(".")));
        let mut v: Vec<&String> = set.iter().collect();
        v.sort();
        let _ = std::fs::write(
            p,
            v.into_iter().map(|s| format!("{s}\n")).collect::<String>(),
        );
    }
}

pub fn run<U: HostedApp>(launch: Launch) -> anyhow::Result<()> {
    let unit = U::id();
    if ipc::is_running(unit) {
        for req in launch_requests(&launch) {
            match ipc::send(unit, &req).as_deref() {
                Some("ok") => {}
                Some("err") => match req {
                    Request::Layout(id) => eprintln!(
                        "Patch does not have a layout named '{id}'. Restart Patch to load layouts from the current build."
                    ),
                    _ => eprintln!("Patch could not apply the request."),
                },
                Some(reply) => eprintln!("Patch could not apply the request: {reply}"),
                None => eprintln!("Patch stopped responding while handling the request."),
            }
        }
        return Ok(());
    }
    let t0 = Instant::now();
    let mark = |what: &str| {
        if std::env::var_os("PATCH_STARTUP_LOG").is_some() {
            eprintln!(
                "host: {what} at {} ms · heap {:.1} MB",
                t0.elapsed().as_millis(),
                crate::heap::in_use_mb()
            );
        }
    };
    init_platform(&unit.app_id())?;
    mark("platform");
    hypr::install_rules();
    hypr::keep_rules_through_reloads();
    mark("window rules");
    let session = Session::<U>::new();
    mark("session");
    {
        let s = session.clone();
        HANDLER.with(|h| *h.borrow_mut() = Some(Box::new(move |req| s.handle(req))));
    }
    ipc::serve(unit, dispatch)?;
    mark("ipc");
    session.watch_theme();
    session.watch_sizes();
    mark("theme");
    session.launch(&launch);
    mark("launched");
    let result = slint::run_event_loop_until_quit();
    ipc::cleanup(unit);
    HANDLER.with(|h| h.borrow_mut().take());
    result?;
    Ok(())
}

#[cfg(test)]
mod renderer_tests {
    #[test]
    fn only_a_driver_that_cannot_draw_gives_up_the_gpu() {
        assert_eq!(super::renderer_for(Some("asahi-g15")), "software");
        for driver in [None, Some("nvidia"), Some("amdgpu"), Some("i915"), Some("asahi")] {
            assert_eq!(super::renderer_for(driver), "femtovg");
        }
    }
}

#[cfg(test)]
mod setup_tab_tests {
    use super::{SettingRow, grouped};

    fn row(id: &str, group: &str) -> SettingRow {
        SettingRow {
            id: id.into(),
            label: id.into(),
            detail: String::new(),
            group: group.into(),
            on: false,
            action: false,
        }
    }

    /// The headings the Setup tab draws: one where the group changes.
    fn headings(rows: &[SettingRow]) -> Vec<&str> {
        rows.iter()
            .enumerate()
            .filter(|(i, r)| *i == 0 || rows[i - 1].group != r.group)
            .map(|(_, r)| r.group.as_str())
            .collect()
    }

    #[test]
    fn each_setup_heading_shows_once() {
        // The order Patch adds its switches in (switches, then the rows
        // the engine and the styles add, then the stem separator).
        let rows = vec![
            row("dj", "Modules"),
            row("plugins", "Modules"),
            row("system_audio", "Setup"),
            row("bar", "Setup"),
            row("hq_decks", "DJ Booth"),
            row("focus_border", "Windows"),
            row("show_track_path", "DJ Booth"),
            row("track_map", "DJ Booth"),
            row("dev_mode", "Developer"),
            row("notify_tracks", "DJ Booth"),
            row("levels:balanced", "DJ Booth"),
            row("meters:classic", "Meters"),
            row("style:default:classic", "Style"),
            row("stems_separator", "Setup"),
        ];
        let before = headings(&rows);
        assert_eq!(before.iter().filter(|g| **g == "DJ Booth").count(), 3);
        let after = grouped(rows);
        assert_eq!(
            headings(&after),
            ["Modules", "Setup", "DJ Booth", "Windows", "Developer", "Meters", "Style"]
        );
        let ids: Vec<&str> = after.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "dj",
                "plugins",
                "system_audio",
                "bar",
                "stems_separator",
                "hq_decks",
                "show_track_path",
                "track_map",
                "notify_tracks",
                "levels:balanced",
                "focus_border",
                "dev_mode",
                "meters:classic",
                "style:default:classic"
            ],
            "rows keep their order within a group"
        );
    }
}
