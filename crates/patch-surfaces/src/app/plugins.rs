//! The Plugins Unit's windows: the rack (add, switch on, order, open) and
//! one window per rack plugin, `plugin#N` for slot N: Patch's face for it,
//! and, beside it, the plugin's own editor ("Original") in a process of its
//! own ([`patch_plugin::editor`]).
//!
//! The rack file is the truth; this keeps a copy, saves it shortly after a
//! change and tells the engine. Turning a control goes to the engine at
//! once (a background sender keeps the newest value per control), so the
//! plugin on the master follows the hand. A plugin window holds a mirror
//! instance of its plugin for the names and readouts of its controls; what
//! the hand does in the plugin's own editor reaches the engine, the face
//! and the rack the same way.
//!
//! Models are updated in place: a rack row rebuilt between a button's press
//! and its release loses the click.

use super::*;
use crate::perf_view::in_place;
use patch_plugin::editor::{Editor, Request};
use patch_plugin::{Mirror, Parameter, Place, RackConfig, View, ZOOMS};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::SystemTime;

/// Engine status (levels, load) while a plugin window is open.
const POLL: Duration = Duration::from_millis(100);
/// The rack file, for changes made elsewhere (the CLI).
const RECHECK: Duration = Duration::from_millis(700);
/// A change is saved this long after the last one (a knob's drag).
const SAVE_AFTER: Duration = Duration::from_millis(400);

enum Send {
    Set(u32, u32, f64),
    Sync,
}

/// Engine requests off the UI thread, in order; a burst of values for one
/// control collapses to its newest.
#[derive(Default)]
struct Sender {
    tx: RefCell<Option<mpsc::Sender<Send>>>,
}

impl Sender {
    fn send(&self, message: Send) {
        let mut tx = self.tx.borrow_mut();
        if tx.is_none() {
            let (sender, rx) = mpsc::channel::<Send>();
            let started = std::thread::Builder::new()
                .name("patch-ui-plugins".into())
                .spawn(move || {
                    while let Ok(first) = rx.recv() {
                        let mut batch = vec![first];
                        batch.extend(rx.try_iter());
                        let mut sets: Vec<(u32, u32, f64)> = Vec::new();
                        let flush = |sets: &mut Vec<(u32, u32, f64)>| {
                            for (slot, id, value) in sets.drain(..) {
                                let _ = patch_engine::client::call(&json!({
                                    "op": "plugins", "action": "set",
                                    "slot": slot, "id": id, "value": value
                                }));
                            }
                        };
                        for message in batch {
                            match message {
                                Send::Set(slot, id, value) => {
                                    match sets.iter_mut().find(|s| s.0 == slot && s.1 == id) {
                                        Some(s) => s.2 = value,
                                        None => sets.push((slot, id, value)),
                                    }
                                }
                                Send::Sync => {
                                    flush(&mut sets);
                                    let _ = patch_engine::client::call(
                                        &json!({"op": "plugins", "action": "sync"}),
                                    );
                                }
                            }
                        }
                        flush(&mut sets);
                    }
                });
            if started.is_err() {
                return;
            }
            *tx = Some(sender);
        }
        if let Some(tx) = tx.as_ref() {
            let _ = tx.send(message);
        }
    }
}

/// A slot's mirror and the controls it shows.
struct Face {
    mirror: Mirror,
    parameters: Vec<Parameter>,
}

impl Face {
    /// A control the plugin calls continuous but writes as yes/no.
    fn reads_as_switch(&self, id: u32) -> bool {
        const WORDS: [&str; 8] = ["yes", "no", "on", "off", "true", "false", "enabled", "disabled"];
        let word = |v: f64| self.mirror.text(id, v).trim().to_lowercase();
        let (low, high) = (word(0.0), word(1.0));
        low != high && WORDS.contains(&low.as_str()) && WORDS.contains(&high.as_str())
    }

    fn text(&self, id: u32, value: f64) -> String {
        let unit = self.parameters.iter().find(|p| p.id == id).map_or("", |p| p.unit.as_str());
        with_unit(&self.mirror.text(id, value), unit)
    }
}

#[derive(Default)]
pub(super) struct PluginUi {
    rack: RefCell<RackConfig>,
    loaded: Cell<bool>,
    stamp: Cell<Option<SystemTime>>,
    checked: Cell<Option<Instant>>,
    save_due: Cell<Option<Instant>>,
    installed: RefCell<Vec<patch_plugin::BundleInfo>>,
    faces: RefCell<HashMap<u32, Face>>,
    /// The plugins' own editors that are open, by slot.
    editors: RefCell<HashMap<u32, Editor>>,
    /// The engine's chain: slot id → its status row.
    status: RefCell<HashMap<u32, Value>>,
    engine: Cell<bool>,
    polls: crate::poll::Poller,
    polled: Cell<Option<Instant>>,
    message: RefCell<String>,
    sender: Sender,
    /// A plugin being probed before it joins the rack.
    adding: RefCell<Option<mpsc::Receiver<Result<(PathBuf, patch_plugin::PluginInfo), String>>>>,
    /// A slot whose defaults are being read (a probe, off this thread):
    /// each control's id and default.
    resetting: RefCell<Option<(u32, mpsc::Receiver<Result<Vec<(u32, f64)>, String>>)>>,
    /// A download being installed (Install…): what to say when it is done.
    installing: RefCell<Option<mpsc::Receiver<String>>>,
    install_note: RefCell<String>,
    /// The windows' bookkeeping runs at this pace, not every frame.
    ticked: Cell<Option<Instant>>,
    /// The Mixer's strips a plugin can go on, with their engine channels
    /// (asked of the audio provider once a second, not on every fill).
    strips: RefCell<Vec<StripChannel>>,
    strips_at: Cell<Option<Instant>>,
}

/// A Mixer strip as a place for a plugin.
#[derive(Clone, PartialEq)]
struct StripChannel {
    id: String,
    name: String,
    channel: Option<String>,
    application: bool,
}

/// Put `rows` in a window's model: changed rows only, when it has as many.
fn update<T: Clone + PartialEq + 'static>(current: ModelRc<T>, rows: Vec<T>, set: impl FnOnce(ModelRc<T>)) {
    match current.as_any().downcast_ref::<VecModel<T>>() {
        Some(model) => in_place(model, rows),
        None => set(ModelRc::new(VecModel::from(rows))),
    }
}

fn stamp() -> Option<SystemTime> {
    std::fs::metadata(patch_plugin::config_path())
        .and_then(|m| m.modified())
        .ok()
}

/// The slot a plugin window shows: `plugin` is 1, `plugin#N` is N.
fn slot_of(window: &str) -> Option<u32> {
    (patch_core::instance::base(window) == "plugin")
        .then(|| patch_core::instance::number(window))
        .flatten()
}

fn window_of(slot: u32) -> String {
    patch_core::instance::id("plugin", slot)
}

fn db(linear: f32) -> f32 {
    20.0 * linear.max(1e-9).log10()
}

fn level_text(linear: f32) -> String {
    let d = db(linear);
    if d < -60.0 { "−∞".into() } else { crate::num::fixed(d, 0).replace('-', "−") }
}

/// Which section each control goes in, by name: a strip's leading caps or
/// numbers ("CLEAN 15\" Gain", "FX 57 Drive", "ROOM FAR Pan"), or a first
/// word several share ("Allow C", "Bend C#"). Sections need three
/// controls; the rest stay loose ("").
fn group_keys(names: &[&str]) -> Vec<String> {
    let lead = |name: &str| -> String {
        let words: Vec<&str> = name.split_whitespace().collect();
        if words.len() < 2 {
            return String::new();
        }
        let strip = |w: &&str| {
            (w.chars().any(|c| c.is_alphabetic()) && !w.chars().any(|c| c.is_lowercase()))
                || w.chars().any(|c| c.is_ascii_digit() || c == '"')
        };
        let run = words[..words.len() - 1].iter().take_while(|w| strip(w)).count();
        if run > 0 && words[0].chars().any(|c| c.is_alphabetic()) {
            words[..run].join(" ")
        } else {
            words[0].to_string()
        }
    };
    let keys: Vec<String> = names.iter().map(|n| lead(n)).collect();
    keys.iter()
        .map(|k| {
            if !k.is_empty() && keys.iter().filter(|o| *o == k).count() >= 3 {
                k.clone()
            } else {
                String::new()
            }
        })
        .collect()
}

/// A readout with its unit when the plugin left it out: "−3.5 dB".
fn with_unit(text: &str, unit: &str) -> String {
    let text = text.trim().replace('-', "−");
    if unit.is_empty() || text.ends_with(unit) || text.len() > 6 {
        text
    } else {
        format!("{text} {unit}")
    }
}

/// A slot's zoom as the pickers show it (0: 1×, 1: 1.5×, 2: 2×).
fn zoom_index(zoom: f32) -> i32 {
    ZOOMS
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - zoom).abs().total_cmp(&(b.1 - zoom).abs()))
        .map_or(0, |(i, _)| i as i32)
}

fn step(id: u32, name: &str, sub: &str, kind: &str, live: bool) -> RouteStep {
    RouteStep { id: id as i32, name: name.into(), sub: sub.into(), kind: kind.into(), live }
}

fn kind_of(p: &Parameter) -> &'static str {
    if p.read_only {
        "readout"
    } else if p.steps == 1 {
        "switch"
    } else if p.steps >= 2 && (p.list || p.steps <= 12) {
        "choice"
    } else {
        "knob"
    }
}

impl AudioApp {
    fn plugin_windows(&self) -> Vec<PatchWindow> {
        self.windows
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| matches!(w.get_surface().as_str(), "plugins" | "plugin"))
            .collect()
    }

    fn face_slot(w: &PatchWindow) -> u32 {
        let id = w.global::<Host>().get_window_id();
        slot_of(&id).unwrap_or(1)
    }

    fn rack_loaded(&self) {
        let ui = &self.plugins;
        if !ui.loaded.get() {
            ui.loaded.set(true);
            ui.stamp.set(stamp());
            match patch_plugin::load_or_default() {
                Ok(rack) => *ui.rack.borrow_mut() = rack,
                Err(e) => *ui.message.borrow_mut() = format!("{e:#}"),
            }
            *ui.installed.borrow_mut() = patch_plugin::scan::installed();
        }
    }

    /// The name a plugin window carries in its header and title.
    pub(super) fn plugin_window_title(&self, window: &str) -> Option<String> {
        let slot = slot_of(window)?;
        self.rack_loaded();
        self.plugins.rack.borrow().slot(slot).map(|s| s.name.clone())
    }

    /// Save soon and tell the engine; refresh the windows now.
    fn rack_changed(&self, sync: bool) {
        self.plugins.save_due.set(Some(Instant::now()));
        if sync {
            self.save_rack_now();
            self.plugins.sender.send(Send::Sync);
        }
        self.refresh_plugin_windows();
    }

    fn save_rack_now(&self) {
        let ui = &self.plugins;
        ui.save_due.set(None);
        if let Err(e) = patch_plugin::save_config(&ui.rack.borrow()) {
            *ui.message.borrow_mut() = format!("Could not save the rack: {e:#}");
        }
        ui.stamp.set(stamp());
    }

    pub(super) fn bind_plugins(&self, w: &PatchWindow) {
        self.rack_loaded();
        let s = w.global::<PluginState>();
        let me = self.me.clone();
        s.on_add(move |path| {
            if let Some(app) = me.upgrade() {
                app.add_plugin(PathBuf::from(path.as_str()));
            }
        });
        let me = self.me.clone();
        s.on_defaults(move |id| {
            if let Some(app) = me.upgrade() {
                app.reset_plugin(id as u32);
            }
        });
        let me = self.me.clone();
        s.on_remove(move |id| {
            if let Some(app) = me.upgrade() {
                let id = id as u32;
                app.close_plugin(id);
                app.plugins.rack.borrow_mut().slots.retain(|s| s.id != id);
                app.rack_changed(true);
            }
        });
        let me = self.me.clone();
        s.on_power(move |id, on| {
            if let Some(app) = me.upgrade() {
                app.set_plugin_power(id as u32, on);
            }
        });
        let me = self.me.clone();
        s.on_shift(move |id, by| {
            if let Some(app) = me.upgrade() {
                let moved = app.plugins.rack.borrow_mut().shift(id as u32, by);
                if moved {
                    app.rack_changed(true);
                }
            }
        });
        let me = self.me.clone();
        let window = w.as_weak();
        s.on_open(move |id, original| {
            if let (Some(app), Some(w)) = (me.upgrade(), window.upgrade()) {
                app.open_plugin(&w, id as u32, original);
            }
        });
        self.bind_editor_controls(w);
        let me = self.me.clone();
        s.on_install(move || {
            if let Some(app) = me.upgrade() {
                app.install_plugin();
            }
        });
        let me = self.me.clone();
        s.on_uninstall(move |path| {
            if let Some(app) = me.upgrade() {
                app.uninstall_plugin(std::path::Path::new(path.as_str()));
            }
        });
        let me = self.me.clone();
        s.on_rescan(move || {
            if let Some(app) = me.upgrade() {
                *app.plugins.installed.borrow_mut() = patch_plugin::scan::installed();
                app.refresh_plugin_windows();
            }
        });
        self.fill_rack(w);
    }

    pub(super) fn bind_plugin_face(&self, w: &PatchWindow) {
        self.rack_loaded();
        let s = w.global::<PluginState>();
        s.set_slot(0);
        let me = self.me.clone();
        let window = w.as_weak();
        s.on_set(move |id, value| {
            if let (Some(app), Some(w)) = (me.upgrade(), window.upgrade()) {
                app.set_plugin_control(Self::face_slot(&w), id as u32, f64::from(value));
            }
        });
        let me = self.me.clone();
        let window = w.as_weak();
        s.on_reset(move |id| {
            if let (Some(app), Some(w)) = (me.upgrade(), window.upgrade()) {
                let slot = Self::face_slot(&w);
                let default = app.plugins.faces.borrow().get(&slot).and_then(|f| {
                    f.parameters.iter().find(|p| p.id == id as u32).map(|p| p.default)
                });
                if let Some(default) = default {
                    app.set_plugin_control(slot, id as u32, default);
                }
            }
        });
        let me = self.me.clone();
        let window = w.as_weak();
        s.on_face_power(move |on| {
            if let (Some(app), Some(w)) = (me.upgrade(), window.upgrade()) {
                app.set_plugin_power(Self::face_slot(&w), on);
            }
        });
        self.bind_editor_controls(w);
        let me = self.me.clone();
        let window = w.as_weak();
        s.on_original(move |on| {
            if let (Some(app), Some(w)) = (me.upgrade(), window.upgrade()) {
                app.show_original(Self::face_slot(&w), on);
            }
        });
        self.fill_face(w, Self::face_slot(w));
    }

    /// The plugin's own editor from the rack or a face: open or close it,
    /// and its zoom.
    fn bind_editor_controls(&self, w: &PatchWindow) {
        let s = w.global::<PluginState>();
        let me = self.me.clone();
        s.on_show_original(move |slot, on| {
            if let Some(app) = me.upgrade() {
                app.show_original(slot as u32, on);
            }
        });
        let me = self.me.clone();
        s.on_set_zoom(move |slot, index| {
            if let Some(app) = me.upgrade() {
                app.set_plugin_zoom(slot as u32, index);
            }
        });
        let me = self.me.clone();
        s.on_set_place(move |slot, key| {
            if let Some(app) = me.upgrade() {
                app.set_plugin_place(slot as u32, &key);
            }
        });
    }

    /// The editor's size: saved with the slot, and the open editor follows
    /// in place.
    fn set_plugin_zoom(&self, slot: u32, index: i32) {
        let zoom = ZOOMS[index.clamp(0, ZOOMS.len() as i32 - 1) as usize];
        match self.plugins.rack.borrow_mut().slot_mut(slot) {
            Some(s) => s.zoom = zoom,
            None => return,
        }
        self.plugins.save_due.set(Some(Instant::now()));
        if let Some(editor) = self.plugins.editors.borrow_mut().get_mut(&slot) {
            editor.zoom(zoom);
        }
        self.refresh_plugin_windows();
    }

    fn set_plugin_power(&self, id: u32, on: bool) {
        if let Some(slot) = self.plugins.rack.borrow_mut().slot_mut(id) {
            slot.active = on;
        }
        self.rack_changed(true);
    }

    fn set_plugin_control(&self, slot: u32, id: u32, value: f64) {
        let value = value.clamp(0.0, 1.0);
        let text = self.plugins.faces.borrow().get(&slot).map(|face| {
            face.mirror.set(id, value);
            face.text(id, value)
        });
        if let Some(editor) = self.plugins.editors.borrow_mut().get_mut(&slot) {
            editor.set(id, value);
        }
        self.plugins.rack.borrow_mut().set_parameter(slot, id, value);
        self.plugins.sender.send(Send::Set(slot, id, value));
        self.plugins.save_due.set(Some(Instant::now()));
        self.show_control(slot, id, value, &text.unwrap_or_default());
    }

    /// A control's new value in every window showing its plugin.
    fn show_control(&self, slot: u32, id: u32, value: f64, text: &str) {
        for w in self.plugin_windows() {
            if w.get_surface() != "plugin" || Self::face_slot(&w) != slot {
                continue;
            }
            let groups = w.global::<PluginState>().get_groups();
            for g in 0..groups.row_count() {
                let Some(group) = groups.row_data(g) else { continue };
                for row in 0..group.controls.row_count() {
                    if let Some(mut c) = group.controls.row_data(row)
                        && c.id as u32 == id
                    {
                        c.value = value as f32;
                        c.text = text.into();
                        group.controls.set_row_data(row, c);
                        return;
                    }
                }
            }
        }
    }

    /// Probe in a child process, then add: a plugin that crashes on load
    /// never reaches Patch.
    fn add_plugin(&self, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        *self.plugins.adding.borrow_mut() = Some(rx);
        let name = patch_plugin::scan::bundle_info(&path).name;
        *self.plugins.message.borrow_mut() = format!("checking {name}…");
        std::thread::spawn(move || {
            let result = patch_plugin::probe_isolated(&path)
                .map(|info| (path, info))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        self.refresh_plugin_windows();
        self.ensure_timer();
    }

    /// Every control of a slot back to the plugin's default (its on/off
    /// switch aside). The defaults come from the slot's open window, or a
    /// probe in a child process: a plugin never loads on this thread for it.
    fn reset_plugin(&self, slot: u32) {
        let known = self.plugins.faces.borrow().get(&slot).map(|f| {
            f.parameters.iter().filter(|p| !p.bypass).map(|p| (p.id, p.default)).collect::<Vec<_>>()
        });
        if let Some(defaults) = known {
            return self.apply_defaults(slot, &defaults);
        }
        let Some(path) = self.plugins.rack.borrow().slot(slot).map(|s| s.plugin.clone()) else { return };
        if self.plugins.resetting.borrow().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *self.plugins.resetting.borrow_mut() = Some((slot, rx));
        std::thread::spawn(move || {
            let result = patch_plugin::probe_isolated(&path)
                .map(|info| info.parameters.iter().filter(|p| !p.bypass).map(|p| (p.id, p.default)).collect())
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
        self.ensure_timer();
    }

    fn finish_reset(&self) {
        let (slot, result) = {
            let resetting = self.plugins.resetting.borrow();
            let Some((slot, rx)) = resetting.as_ref() else { return };
            match rx.try_recv() {
                Ok(result) => (*slot, result),
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => (*slot, Err("the check stopped".into())),
            }
        };
        self.plugins.resetting.borrow_mut().take();
        match result {
            Ok(defaults) => self.apply_defaults(slot, &defaults),
            Err(e) => {
                *self.plugins.message.borrow_mut() = format!("Not reset: {e}");
                self.refresh_plugin_windows();
            }
        }
    }

    fn apply_defaults(&self, slot: u32, defaults: &[(u32, f64)]) {
        for &(id, value) in defaults {
            self.set_plugin_control(slot, id, value);
        }
        let name = self.plugins.rack.borrow().slot(slot).map(|s| s.name.clone()).unwrap_or_default();
        *self.plugins.message.borrow_mut() = format!("{name}: every control at its default");
        self.refresh_plugin_windows();
    }

    /// Install…: the desktop's file chooser, then the download unpacked
    /// into ~/.vst3 (checked, never run), off the UI thread.
    fn install_plugin(&self) {
        if self.plugins.installing.borrow().is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        *self.plugins.installing.borrow_mut() = Some(rx);
        *self.plugins.install_note.borrow_mut() = String::new();
        std::thread::spawn(move || {
            let downloads = std::process::Command::new("xdg-user-dir")
                .arg("DOWNLOAD")
                .output()
                .ok()
                .map(|o| PathBuf::from(String::from_utf8_lossy(&o.stdout).trim()))
                .filter(|p| p.is_dir())
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join("Downloads")))
                .unwrap_or_default();
            let picked = crate::portal::pick_file_in(
                "Install a VST3 plugin: its download, or a file inside its .vst3 folder",
                downloads,
                // All files first: a download unpacked already is a folder,
                // and an archives-only start shows nothing inside it.
                &[
                    ("All files (a download, or a file inside a .vst3 folder)", &["*"]),
                    ("Downloads (.zip, .tar.gz)", &["*.zip", "*.tar.gz", "*.tgz", "*.tar"]),
                ],
            );
            let note = match picked {
                Ok(None) => String::new(),
                Ok(Some(file)) => match patch_plugin::install::plugins_dir() {
                    Some(into) => match patch_plugin::install::install(&file, &into) {
                        Ok(done) => patch_plugin::install::summary(&done),
                        Err(e) => format!("Not installed: {e:#}"),
                    },
                    None => "Not installed: no home folder".into(),
                },
                Err(e) => format!("{e}. From a terminal: omarchy-patch plugins install <file>"),
            };
            let _ = tx.send(note);
        });
        self.refresh_plugin_windows();
    }

    /// Uninstall… confirmed: the bundle goes to the Trash (a rename, on
    /// this thread), unless the rack still has it.
    fn uninstall_plugin(&self, bundle: &std::path::Path) {
        let note = match patch_plugin::install::uninstall_unused(bundle, &self.plugins.rack.borrow()) {
            Ok(name) => format!("Uninstalled {name}: it is in the Trash"),
            Err(e) => format!("Not uninstalled: {e:#}"),
        };
        *self.plugins.install_note.borrow_mut() = note;
        *self.plugins.installed.borrow_mut() = patch_plugin::scan::installed();
        self.refresh_plugin_windows();
    }

    fn finish_install(&self) {
        let note = match self.plugins.installing.borrow().as_ref().map(mpsc::Receiver::try_recv) {
            Some(Ok(note)) => note,
            Some(Err(mpsc::TryRecvError::Disconnected)) => "Not installed: the install stopped".into(),
            _ => return,
        };
        self.plugins.installing.borrow_mut().take();
        *self.plugins.install_note.borrow_mut() = note;
        *self.plugins.installed.borrow_mut() = patch_plugin::scan::installed();
        self.refresh_plugin_windows();
    }

    fn finish_add(&self) {
        let result = {
            let adding = self.plugins.adding.borrow();
            match adding.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(result)) => result,
                Some(Err(mpsc::TryRecvError::Disconnected)) => Err("the check stopped".into()),
                _ => return,
            }
        };
        self.plugins.adding.borrow_mut().take();
        let message = match result {
            Ok((path, info)) => {
                let mut bundle = patch_plugin::scan::bundle_info(&path);
                bundle.name = info.name.clone();
                if !info.vendor.is_empty() {
                    bundle.vendor = info.vendor.clone();
                }
                if !info.version.is_empty() {
                    bundle.version = info.version.clone();
                }
                match self.plugins.rack.borrow_mut().add(&path, &bundle) {
                    Ok(_) => String::new(),
                    Err(e) => format!("{e:#}"),
                }
            }
            Err(e) => format!("Not added: {e}"),
        };
        *self.plugins.message.borrow_mut() = message;
        self.rack_changed(true);
    }

    /// From the rack: the plugin's window, and its own editor beside it if
    /// that is how it opens.
    fn open_plugin(&self, from: &PatchWindow, slot: u32, original: bool) {
        from.global::<Host>().invoke_focus_surface(window_of(slot).into());
        if original {
            self.show_original(slot, true);
        }
    }

    /// The plugin's own editor open or closed; it is what the slot opens as.
    fn show_original(&self, slot: u32, on: bool) {
        let request = {
            let mut rack = self.plugins.rack.borrow_mut();
            let Some(s) = rack.slot_mut(slot) else { return };
            s.view = if on { View::Original } else { View::Patch };
            Request {
                plugin: s.plugin.clone(),
                parameters: s.parameters.clone(),
                scale: 1.0,
                zoom: s.zoom,
                title: format!("{} · Original", s.name),
            }
        };
        self.plugins.save_due.set(Some(Instant::now()));
        let open = self.plugins.editors.borrow().get(&slot).is_some_and(Editor::is_open);
        if on && open {
            // Already up (perhaps behind): forward.
            patch_host::hypr::focus_title(&request.title);
        } else if on {
            let scale = self
                .plugin_windows()
                .first()
                .map_or(1.0, |w| w.window().scale_factor());
            // A plugin that plays is shown by the plugin host, on the
            // instance that hears the audio (its meters move); one that is
            // off, or that the host refuses, in an editor process.
            let running = self.plugins.status.borrow().get(&slot).is_some_and(|r| r["state"] == "running");
            let request = Request { scale, ..request };
            let editor = if running {
                Editor::connect(slot, &request).or_else(|_| Editor::spawn(&request))
            } else {
                Editor::spawn(&request)
            };
            match editor {
                Ok(editor) => {
                    self.plugins.editors.borrow_mut().insert(slot, editor);
                }
                Err(e) => *self.plugins.message.borrow_mut() = format!("{e:#}"),
            }
        } else if !on {
            // Dropped: it closes its window and leaves.
            self.plugins.editors.borrow_mut().remove(&slot);
        }
        self.refresh_plugin_windows();
        self.ensure_timer();
    }

    /// The plugin's own editor in an editor process (its own instance).
    fn show_original_in_process(&self, slot: u32) {
        let request = {
            let rack = self.plugins.rack.borrow();
            let Some(s) = rack.slot(slot) else { return };
            Request {
                plugin: s.plugin.clone(),
                parameters: s.parameters.clone(),
                scale: self.plugin_windows().first().map_or(1.0, |w| w.window().scale_factor()),
                zoom: s.zoom,
                title: format!("{} · Original", s.name),
            }
        };
        match Editor::spawn(&request) {
            Ok(editor) => {
                self.plugins.editors.borrow_mut().insert(slot, editor);
            }
            Err(e) => *self.plugins.message.borrow_mut() = format!("{e:#}"),
        }
    }

    fn close_plugin(&self, slot: u32) {
        self.plugins.editors.borrow_mut().remove(&slot);
        self.plugins.faces.borrow_mut().remove(&slot);
        if let Some(w) = self.plugin_windows().into_iter().find(|w| {
            w.get_surface() == "plugin" && Self::face_slot(w) == slot
        }) {
            w.global::<Host>().invoke_close_window();
        }
    }

    fn ensure_face(&self, slot: u32) -> Result<(), String> {
        if self.plugins.faces.borrow().contains_key(&slot) {
            return Ok(());
        }
        let (path, saved) = {
            let rack = self.plugins.rack.borrow();
            let s = rack.slot(slot).ok_or("This slot is no longer in the rack.")?;
            (s.plugin.clone(), s.parameters.clone())
        };
        let mirror = Mirror::load(&path, &saved).map_err(|e| format!("{e:#}"))?;
        let parameters = mirror.parameters();
        self.plugins
            .faces
            .borrow_mut()
            .insert(slot, Face { mirror, parameters });
        Ok(())
    }

    /// A plugin window's header: what can change without its controls
    /// changing (on, the view, its name).
    fn fill_face_header(&self, w: &PatchWindow, slot: u32) {
        let s = w.global::<PluginState>();
        let rack = self.plugins.rack.borrow();
        let Some(entry) = rack.slot(slot) else { return };
        s.set_name(entry.name.as_str().into());
        s.set_active(entry.active);
        s.set_prefers_original(entry.view == View::Original);
        s.set_zoom(zoom_index(entry.zoom));
        s.set_original_open(self.plugins.editors.borrow().get(&slot).is_some_and(Editor::is_open));
        drop(rack);
        self.fill_levels(w, slot);
    }

    /// A plugin window whole: its header and every control.
    fn fill_face(&self, w: &PatchWindow, slot: u32) {
        let s = w.global::<PluginState>();
        s.set_slot(slot as i32);
        let rack = self.plugins.rack.borrow().clone();
        let Some(entry) = rack.slot(slot) else {
            s.set_name("Plugin".into());
            s.set_face_state("This slot is empty: add a plugin in the Plugins window.".into());
            s.set_control_count(0);
            s.set_groups(ModelRc::default());
            return;
        };
        s.set_name(entry.name.as_str().into());
        let about: Vec<&str> = [entry.vendor.as_str(), entry.version.as_str()]
            .into_iter()
            .filter(|t| !t.is_empty())
            .collect();
        s.set_about(about.join(" · ").into());
        let category = self
            .plugins
            .installed
            .borrow()
            .iter()
            .find(|b| b.path == entry.plugin)
            .map(|b| b.category.replace('|', " · "))
            .unwrap_or_default();
        s.set_category(category.into());
        s.set_active(entry.active);
        s.set_prefers_original(entry.view == View::Original);
        s.set_zoom(zoom_index(entry.zoom));
        match self.ensure_face(slot) {
            Ok(()) => {
                let faces = self.plugins.faces.borrow();
                let face = &faces[&slot];
                let shown: Vec<&Parameter> = face.parameters.iter().filter(|p| !p.bypass).collect();
                let names: Vec<&str> = shown.iter().map(|p| p.name.as_str()).collect();
                let keys = group_keys(&names);
                let mut groups: Vec<(String, Vec<PluginControl>)> = Vec::new();
                for (p, key) in shown.iter().zip(&keys) {
                    let value = entry
                        .parameters
                        .iter()
                        .find(|saved| saved.id == p.id)
                        .map_or(p.value, |saved| saved.value);
                    let origin = if p.steps == 0 && (0.45..=0.55).contains(&p.default) {
                        p.default
                    } else {
                        0.0
                    };
                    let name = p.name.strip_prefix(key.as_str()).unwrap_or(&p.name).trim();
                    let name = if name.is_empty() { p.name.as_str() } else { name };
                    let control = PluginControl {
                        id: p.id as i32,
                        name: name.into(),
                        value: value as f32,
                        text: with_unit(&face.mirror.text(p.id, value), &p.unit).into(),
                        kind: if p.steps == 0 && !p.read_only && face.reads_as_switch(p.id) {
                            "switch"
                        } else {
                            kind_of(p)
                        }
                        .into(),
                        origin: origin as f32,
                        default: p.default as f32,
                        steps: p.steps.max(0),
                    };
                    match groups.iter_mut().find(|(k, _)| k == key) {
                        Some((_, controls)) => controls.push(control),
                        None => groups.push((key.clone(), vec![control])),
                    }
                }
                // Loose controls first, then the sections as the plugin lists them.
                groups.sort_by_key(|(k, _)| !k.is_empty());
                s.set_control_count(shown.len() as i32);
                s.set_groups(ModelRc::new(VecModel::from(
                    groups
                        .into_iter()
                        .map(|(name, controls)| PluginGroup {
                            name: name.to_uppercase().into(),
                            controls: ModelRc::new(VecModel::from(controls)),
                        })
                        .collect::<Vec<_>>(),
                )));
                s.set_category(face.mirror.info().category.replace('|', " · ").into());
                s.set_has_editor(face.mirror.has_editor());
                s.set_original_open(self.plugins.editors.borrow().get(&slot).is_some_and(Editor::is_open));
                s.set_face_state("".into());
            }
            Err(e) => {
                s.set_has_editor(false);
                s.set_original_open(false);
                s.set_face_state(format!("Could not load {}: {e}", entry.name).into());
                s.set_control_count(0);
                s.set_groups(ModelRc::default());
            }
        }
        self.fill_levels(w, slot);
    }

    fn fill_levels(&self, w: &PatchWindow, slot: u32) {
        let s = w.global::<PluginState>();
        let status = self.plugins.status.borrow();
        let row = status.get(&slot);
        let peak = |k: &str| row.and_then(|r| r[k].as_f64()).unwrap_or(0.0) as f32;
        let pos = |linear: f32| patch_ui::meter::MeterKind::System.position(db(linear));
        s.set_in_level(pos(peak("input_peak")));
        s.set_out_level(pos(peak("output_peak")));
        s.set_in_text(level_text(peak("input_peak")).into());
        s.set_out_text(level_text(peak("output_peak")).into());
        drop(status);
        let place = self.plugins.rack.borrow().slot(slot).map(|e| e.place.clone()).unwrap_or_default();
        update(s.get_face_route(), self.route(&place, Some(slot)), |m| s.set_face_route(m));
        let status = self.plugins.status.borrow();
        let row = status.get(&slot);
        let active = self.plugins.rack.borrow().slot(slot).is_some_and(|s| s.active);
        s.set_load(
            match (active, row, self.plugins.engine.get()) {
                (false, _, _) => "off · not in the signal path".to_string(),
                (true, _, false) => "on · the engine is not running".to_string(),
                (true, None, true) => "on · loading".to_string(),
                (true, Some(r), true) => match &r["state"] {
                    Value::String(state) if state == "running" => format!(
                        "live on the master · {:.0}% of each block · one block of latency",
                        r["load"].as_f64().unwrap_or(0.0) * 100.0
                    ),
                    Value::String(state) => state.clone(),
                    other => format!("failed: {}", other["failed"].as_str().unwrap_or("?")),
                },
            }
            .into(),
        );
    }

    /// The rack window whole: its rows, the path, where plugins can go,
    /// the installed plugins.
    fn fill_rack(&self, w: &PatchWindow) {
        if self.plugins.strips_at.get().is_none() {
            self.refresh_strip_channels();
        }
        self.fill_rack_rows(w, true);
    }

    /// What moves while plugins run (their state, level and load, the
    /// path's live dots); `all`: the rest too.
    fn fill_rack_rows(&self, w: &PatchWindow, all: bool) {
        let s = w.global::<PluginState>();
        let rack = self.plugins.rack.borrow();
        let status = self.plugins.status.borrow();
        let engine = self.plugins.engine.get();
        let n = rack.slots.len();
        let slots: Vec<RackSlot> = rack
            .slots
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let row = status.get(&slot.id);
                let failed = row.and_then(|r| r["state"]["failed"].as_str());
                let state = match (slot.active, row, failed) {
                    (false, _, _) => "off",
                    (true, _, Some(_)) => "failed",
                    (true, Some(r), None) if r["state"] == "running" => "on",
                    (true, _, None) if engine => "loading",
                    _ => "on",
                };
                let mut detail: Vec<String> = Vec::new();
                if !slot.vendor.is_empty() {
                    detail.push(slot.vendor.clone());
                }
                detail.push(match state {
                    "failed" => failed.unwrap_or("failed").to_string(),
                    "loading" => "loading…".into(),
                    "off" => "off".into(),
                    _ if engine => "on the master".into(),
                    _ => "on · engine not running".into(),
                });
                let out = row.and_then(|r| r["output_peak"].as_f64()).unwrap_or(0.0) as f32;
                RackSlot {
                    id: slot.id as i32,
                    name: slot.name.as_str().into(),
                    vendor: slot.vendor.as_str().into(),
                    active: slot.active,
                    state: state.into(),
                    detail: detail.join(" · ").into(),
                    level: patch_ui::meter::MeterKind::System.position(db(out)),
                    load: row
                        .and_then(|r| r["load"].as_f64())
                        .map(|l| format!("{:.0}% cpu", l * 100.0))
                        .unwrap_or_default()
                        .into(),
                    original: slot.view == View::Original,
                    original_open: self.plugins.editors.borrow().get(&slot.id).is_some_and(Editor::is_open),
                    zoom: zoom_index(slot.zoom),
                    place: self.place_label(&slot.place).into(),
                    place_key: self.place_key(&slot.place).into(),
                    first: i == 0,
                    last: i + 1 == n,
                }
            })
            .collect();
        let own = patch_plugin::install::plugins_dir().and_then(|d| d.canonicalize().ok());
        let installed: Vec<InstalledPlugin> = if !all {
            Vec::new()
        } else {
            self.plugins
                .installed
                .borrow()
                .iter()
                .map(|b| {
                    let detail: Vec<&str> = [b.vendor.as_str(), b.version.as_str(), b.category.as_str()]
                        .into_iter()
                        .filter(|t| !t.is_empty())
                        .collect();
                    InstalledPlugin {
                        name: b.name.as_str().into(),
                        vendor: b.vendor.as_str().into(),
                        detail: if b.native {
                            detail.join(" · ")
                        } else {
                            "no build for this machine".into()
                        }
                        .into(),
                        path: b.path.to_string_lossy().as_ref().into(),
                        in_rack: rack.slots.iter().any(|s| s.plugin == b.path),
                        native: b.native,
                        removable: own.is_some()
                            && b.path.parent().and_then(|p| p.canonicalize().ok()) == own,
                    }
                })
                .collect()
        };
        drop(status);
        drop(rack);
        // The lines' steps are models of their own: put only what changed,
        // so a chip under the hand is not rebuilt.
        let lines = self.route_lines();
        let current = s.get_route();
        if current.row_count() == lines.len() {
            for (i, (title, steps)) in lines.into_iter().enumerate() {
                let Some(mut row) = current.row_data(i) else { continue };
                if row.title.as_str() != title {
                    row.title = title.into();
                    current.set_row_data(i, row.clone());
                }
                update(row.steps.clone(), steps, |m| {
                    row.steps = m;
                    current.set_row_data(i, row.clone());
                });
            }
        } else {
            s.set_route(ModelRc::new(VecModel::from(
                lines
                    .into_iter()
                    .map(|(title, steps)| RouteLine { title: title.into(), steps: ModelRc::new(VecModel::from(steps)) })
                    .collect::<Vec<_>>(),
            )));
        }
        update(s.get_slots(), slots, |m| s.set_slots(m));
        if all {
            let places = self.place_options();
            update(s.get_places(), places, |m| s.set_places(m));
            update(s.get_installed(), installed, |m| s.set_installed(m));
        }
        s.set_message(self.plugins.message.borrow().as_str().into());
        s.set_engine(engine);
        s.set_busy(self.plugins.adding.borrow().is_some());
        s.set_installing(self.plugins.installing.borrow().is_some());
        let note = self.plugins.install_note.borrow();
        s.set_install_note(note.as_str().into());
        s.set_install_failed(note.starts_with("Not "));
    }

    /// The Mixer channel behind an engine channel: its strip's name and
    /// whether it is playing.
    fn channel_strip(&self, channel: &str) -> Option<(String, bool)> {
        let cache = self.plugins.strips.borrow();
        let strip = cache.iter().find(|s| s.channel.as_deref() == Some(channel))?;
        let live = self.strips.iter().any(|s| s.id.as_str() == strip.id && s.level > 0.05 && !s.mute);
        Some((strip.name.clone(), live))
    }

    /// The Mixer's strips with their engine channels, again from the
    /// provider; true when they changed.
    fn refresh_strip_channels(&self) -> bool {
        let fresh: Vec<StripChannel> = {
            let provider = self.provider.borrow();
            self.strips
                .iter()
                .filter(|s| !s.master)
                .map(|s| StripChannel {
                    channel: provider.strip_channel(s.id.as_str()),
                    id: s.id.to_string(),
                    name: s.name.to_string(),
                    application: s.application,
                })
                .collect()
        };
        self.plugins.strips_at.set(Some(Instant::now()));
        let changed = *self.plugins.strips.borrow() != fresh;
        *self.plugins.strips.borrow_mut() = fresh;
        changed
    }

    /// A place as people read it: a channel by its strip's name.
    fn place_label(&self, place: &Place) -> String {
        match place {
            Place::Channel(ch) => self.channel_strip(ch).map_or_else(|| place.label(), |(name, _)| name),
            _ => place.label(),
        }
    }

    /// Where a plugin can go: the master, the buses, and every channel
    /// strip with an engine channel or an app that can be given one.
    fn place_options(&self) -> Vec<PlaceOption> {
        let option = |key: &str, name: &str, detail: &str, head: bool| PlaceOption {
            key: key.into(),
            name: name.into(),
            detail: detail.into(),
            head,
        };
        let mut options = vec![
            option("", "The whole mix", "", true),
            option("master", "Master", "after the master EQ", false),
            option("", "Buses", "", true),
            option("aux-1", "AUX 1", "its sends", false),
            option("aux-2", "AUX 2", "its sends", false),
            option("fx-1", "FX 1", "after the reverb", false),
            option("fx-2", "FX 2", "after the delay", false),
        ];
        let channels: Vec<PlaceOption> = self
            .plugins
            .strips
            .borrow()
            .iter()
            .filter(|s| s.channel.is_some() || s.application)
            .map(|s| {
                let detail = if s.channel.is_some() { "" } else { "gets a channel" };
                option(&format!("node:{}", s.id), &s.name, detail, false)
            })
            .collect();
        if !channels.is_empty() {
            options.push(option("", "One channel", "", true));
            options.extend(channels);
        }
        options
    }

    /// The key a slot's place has in [`Self::place_options`].
    fn place_key(&self, place: &Place) -> String {
        match place {
            Place::Channel(ch) => self
                .plugins
                .strips
                .borrow()
                .iter()
                .find(|s| s.channel.as_deref() == Some(ch.as_str()))
                .map_or_else(|| ch.clone(), |s| format!("node:{}", s.id)),
            _ => place.key(),
        }
    }

    /// Move a plugin elsewhere in the mix; an app strip is given an
    /// engine channel of its own first (as its knobs would).
    fn set_plugin_place(&self, slot: u32, key: &str) {
        let place = match key.strip_prefix("node:") {
            Some(node) => match self.provider.borrow_mut().assign_channel(node) {
                Ok(ch) => Place::Channel(ch),
                Err(e) => {
                    *self.toast.borrow_mut() = format!("Not moved: {e}");
                    self.push_toast();
                    return;
                }
            },
            None => Place::parse(key),
        };
        match self.plugins.rack.borrow_mut().slot_mut(slot) {
            Some(s) => s.place = place,
            None => return,
        }
        self.refresh_strip_channels();
        self.rack_changed(true);
    }

    /// The signal path through one place as it runs now, in the engine's
    /// order, with that place's plugins: the rack shows them all (those off
    /// as passed by); a plugin's window (`this`) shows the ones on and
    /// itself, by name only.
    ///
    /// * the master: the channels playing into MAIN, the master EQ, the
    ///   plugins, Tune, the main fader, the limiter, the output device;
    /// * a channel: its input, the plugins, its EQ and fader, then the mix
    ///   (its sends go out after the plugins too);
    /// * AUX: its sends, its level, the plugins, out of Patch;
    /// * FX: its sends, the effect, the plugins, the return, the mix.
    fn route(&self, place: &Place, this: Option<u32>) -> Vec<RouteStep> {
        let compact = this.is_some();
        // Levels are read only where a plugin is on: with none on, the
        // path stands still and costs nothing.
        let on_here = self.plugins.rack.borrow().slots.iter().any(|s| s.active && &s.place == place);
        let master_live =
            on_here && self.strips.iter().any(|s| !s.master && s.available && !s.mute && s.level > 0.05);
        let end = |name: &str, sub: &str, kind: &str, live: bool| {
            if compact { step(0, if sub.is_empty() { name } else { sub }, "", kind, live) } else { step(0, name, sub, kind, live) }
        };
        let (mut steps, live) = match place {
            Place::Master => {
                let mut sources: Vec<(String, bool)> = self
                    .strips
                    .iter()
                    .filter(|s| !s.master && s.available && !s.mute)
                    .map(|s| (s.name.to_string(), on_here && s.level > 0.05))
                    .collect();
                sources.sort_by_key(|(_, live)| !live);
                let names = match sources.len() {
                    0 => "nothing routed to MAIN".to_string(),
                    1..=3 => sources.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "),
                    n => format!(
                        "{} +{}",
                        sources.iter().take(2).map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", "),
                        n - 2
                    ),
                };
                (vec![end("IN", &names, "in", master_live), step(0, "EQ", "master EQ", "stage", master_live)], master_live)
            }
            Place::Channel(ch) => {
                let (name, live) = self.channel_strip(ch).unwrap_or_else(|| (place.label(), false));
                let live = live && on_here;
                (vec![end("IN", &name, "in", live)], live)
            }
            Place::Aux(n) => (
                vec![end("IN", &format!("AUX {n} sends"), "in", false), step(0, "Level", &format!("AUX {n} master"), "stage", false)],
                false,
            ),
            Place::Fx(n) => (
                vec![
                    end("IN", &format!("FX {n} sends"), "in", false),
                    step(0, if *n == 1 { "Reverb" } else { "Delay" }, "built-in effect", "stage", false),
                ],
                false,
            ),
        };
        let rack = self.plugins.rack.borrow();
        let status = self.plugins.status.borrow();
        for slot in rack.slots.iter().filter(|s| &s.place == place) {
            let mine = this == Some(slot.id);
            if compact && !slot.active && !mine {
                continue;
            }
            let failed = status.get(&slot.id).and_then(|r| r["state"]["failed"].as_str());
            let (kind, sub) = match (slot.active, failed) {
                (true, Some(why)) => ("failed", why.to_string()),
                (false, _) => ("off", "off · passed by".to_string()),
                (true, None) => ("plugin", "on".to_string()),
            };
            let kind = if mine { "this" } else { kind };
            let name = if mine && !slot.active { format!("{} (off)", slot.name) } else { slot.name.clone() };
            steps.push(step(slot.id, &name, &sub, kind, live && slot.active));
        }
        match place {
            Place::Master => {
                let device = self
                    .outputs
                    .iter()
                    .find(|o| o.current)
                    .map(|o| o.name.to_string())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| "the output".into());
                steps.push(step(0, "Tune", "room correction", "stage", live));
                steps.push(step(0, "Fader", "MAIN level", "stage", live));
                steps.push(step(0, "Limiter", "ceiling", "stage", live));
                steps.push(end("OUT", &device, "out", live));
            }
            Place::Channel(_) => {
                steps.push(step(0, "Strip", "EQ · fader · sends", "stage", live));
                steps.push(end("OUT", "the mix", "out", live));
            }
            Place::Aux(n) => steps.push(end("OUT", &format!("AUX {n} out"), "out", false)),
            Place::Fx(n) => {
                steps.push(step(0, "Return", &format!("FX {n} return"), "stage", false));
                steps.push(end("OUT", "the mix", "out", false));
            }
        }
        steps
    }

    /// The rack's path: the master, then each other place a plugin is set
    /// to, in rack order.
    fn route_lines(&self) -> Vec<(String, Vec<RouteStep>)> {
        let mut places = vec![Place::Master];
        for slot in &self.plugins.rack.borrow().slots {
            if !places.contains(&slot.place) {
                places.push(slot.place.clone());
            }
        }
        places
            .iter()
            .map(|place| (self.place_label(place).to_uppercase(), self.route(place, None)))
            .collect()
    }

    /// The rack and every plugin window's header (their controls stay:
    /// rebuilding them under the hand drops a drag).
    fn refresh_plugin_windows(&self) {
        for w in self.plugin_windows() {
            if w.get_surface() == "plugins" {
                self.fill_rack(&w);
            } else {
                self.fill_face_header(&w, Self::face_slot(&w));
            }
        }
        self.plugin_insert_summary();
    }

    /// Everything again, controls too (the rack changed elsewhere).
    fn rebuild_plugin_windows(&self) {
        for w in self.plugin_windows() {
            if w.get_surface() == "plugins" {
                self.fill_rack(&w);
            } else {
                self.fill_face(&w, Self::face_slot(&w));
            }
        }
        self.plugin_insert_summary();
    }

    /// MAIN's INSERT row and the Router's node.
    fn plugin_insert_summary(&self) {
        let rack = self.plugins.rack.borrow();
        let on: Vec<&str> = rack.active().map(|s| s.name.as_str()).collect();
        let text = match on.as_slice() {
            [] if rack.slots.is_empty() => "—".to_string(),
            [] => "OFF".to_string(),
            [one] => one.to_uppercase(),
            [first, rest @ ..] => format!("{} +{}", first.to_uppercase(), rest.len()),
        };
        let any = !on.is_empty();
        self.broadcast(|state| {
            state.set_plugins_enabled(any);
            state.set_plugins_insert(text.as_str().into());
        });
    }

    /// The rack's state for the Router: (shown, on, name, detail).
    pub(super) fn plugin_node(&self) -> Option<(bool, String, String)> {
        self.rack_loaded();
        let rack = self.plugins.rack.borrow();
        if rack.slots.is_empty() {
            return None;
        }
        let on: Vec<&str> = rack.active().map(|s| s.name.as_str()).collect();
        let name = match on.as_slice() {
            [] => "Plugins".to_string(),
            [one] => one.to_string(),
            [first, rest @ ..] => format!("{first} +{}", rest.len()),
        };
        let detail = if on.is_empty() {
            "VST3 rack · off".to_string()
        } else {
            format!("VST3 rack · {} on the master", on.len())
        };
        Some((!on.is_empty(), name, detail))
    }

    /// Every frame: the plugins' own editors, edits made there, the
    /// engine's levels, and saving.
    pub(super) fn plugins_tick(&self) {
        let ui = &self.plugins;
        if !ui.loaded.get() {
            return;
        }
        let now = Instant::now();
        self.finish_add();
        self.finish_reset();
        self.finish_install();
        // Edits in the plugins' own editors follow to the engine and the
        // face; a closed editor leaves the plugin on Patch's face.
        let mut edited: Vec<(u32, Vec<patch_plugin::SavedParameter>)> = Vec::new();
        let mut closed: Vec<u32> = Vec::new();
        let mut zoomed: Vec<(u32, f32, bool)> = Vec::new();
        let mut refused: Vec<u32> = Vec::new();
        for (&slot, editor) in ui.editors.borrow_mut().iter_mut() {
            let news = editor.poll();
            if let Some((zoom, scales)) = news.zoom {
                zoomed.push((slot, zoom, scales));
            }
            if !news.edits.is_empty() {
                edited.push((slot, news.edits));
            }
            if news.refused.is_some() {
                refused.push(slot);
            } else if news.closed {
                closed.push(slot);
            }
        }
        // The plugin host could not show it: an editor process does.
        for slot in refused {
            ui.editors.borrow_mut().remove(&slot);
            self.show_original_in_process(slot);
        }
        // A zoom the plugin cannot show says so.
        for (slot, zoom, scales) in zoomed {
            let (asked, name) = match ui.rack.borrow().slot(slot) {
                Some(s) => (s.zoom, s.name.clone()),
                None => continue,
            };
            let note = if !scales && asked > 1.01 {
                format!("{name} draws its editor at one size: it does not zoom")
            } else if scales && zoom + 0.05 < asked {
                format!("{name} at {asked}× would not fit the screen: shown at {zoom:.1}×")
            } else {
                continue;
            };
            *self.toast.borrow_mut() = note;
            self.push_toast();
        }
        for (slot, edits) in edited {
            for edit in edits {
                ui.rack.borrow_mut().set_parameter(slot, edit.id, edit.value);
                ui.sender.send(Send::Set(slot, edit.id, edit.value));
                let text = ui
                    .faces
                    .borrow()
                    .get(&slot)
                    .map(|f| {
                        f.mirror.set(edit.id, edit.value);
                        f.text(edit.id, edit.value)
                    })
                    .unwrap_or_default();
                self.show_control(slot, edit.id, edit.value, &text);
            }
            ui.save_due.set(Some(now));
        }
        if !closed.is_empty() {
            for slot in closed {
                ui.editors.borrow_mut().remove(&slot);
                if let Some(s) = ui.rack.borrow_mut().slot_mut(slot) {
                    s.view = View::Patch;
                }
            }
            ui.save_due.set(Some(now));
            self.refresh_plugin_windows();
        }
        // The rest ten times a second, not every frame.
        if ui.ticked.get().is_some_and(|t| now - t < POLL) {
            return;
        }
        ui.ticked.set(Some(now));
        let windows = self.plugin_windows();
        // A face window learns its slot once the host names it.
        for w in &windows {
            if w.get_surface() == "plugin" {
                let slot = Self::face_slot(w);
                if w.global::<PluginState>().get_slot() != slot as i32 {
                    self.fill_face(w, slot);
                }
            }
        }
        // Mirrors nothing shows any more go.
        let shown: Vec<u32> = windows
            .iter()
            .filter(|w| w.get_surface() == "plugin")
            .map(Self::face_slot)
            .collect();
        ui.faces.borrow_mut().retain(|slot, _| shown.contains(slot));
        // Changes made elsewhere (the CLI).
        if ui.checked.get().is_none_or(|t| now - t > RECHECK) {
            ui.checked.set(Some(now));
            let current = stamp();
            if current != ui.stamp.get() && ui.save_due.get().is_none() {
                ui.stamp.set(current);
                if let Ok(rack) = patch_plugin::load_or_default() {
                    *ui.rack.borrow_mut() = rack;
                    self.rebuild_plugin_windows();
                }
            }
        }
        if ui.save_due.get().is_some_and(|t| now - t > SAVE_AFTER) {
            self.save_rack_now();
        }
        if windows.is_empty() {
            return;
        }
        // The Mixer's strips (names, apps coming and going), once a second
        // while the rack is shown.
        let rack_shown = windows.iter().any(|w| w.get_surface() == "plugins");
        if rack_shown
            && ui.strips_at.get().is_none_or(|t| now - t > Duration::from_secs(1))
            && self.refresh_strip_channels()
        {
            self.refresh_plugin_windows();
        }
        // The engine's levels, only while a plugin is on: with every plugin
        // off, nothing is asked and nothing redrawn.
        let any_on = ui.rack.borrow().active().next().is_some();
        if !any_on {
            ui.polls.take("plugins");
            if !ui.status.borrow().is_empty() {
                ui.status.borrow_mut().clear();
                self.refresh_plugin_windows();
            }
            ui.engine.set(self.is_live());
            return;
        }
        if let Some(reply) = ui.polls.take("plugins") {
            ui.engine.set(reply.result.is_ok());
            let rows = reply.result.ok().and_then(|v| v.as_array().cloned()).unwrap_or_default();
            *ui.status.borrow_mut() = rows
                .into_iter()
                .filter_map(|r| Some((r["id"].as_u64()? as u32, r)))
                .collect();
            for w in &windows {
                if w.get_surface() == "plugins" {
                    self.fill_rack_rows(w, false);
                } else {
                    self.fill_levels(w, Self::face_slot(w));
                }
            }
        }
        if ui.polled.get().is_none_or(|t| now - t > POLL) {
            ui.polled.set(Some(now));
            ui.polls
                .request("plugins", json!({"op": "plugins", "action": "status"}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use patch_host::session::HostedApp;
    use slint::platform::{PointerEventButton, WindowEvent};

    fn click(w: &PatchWindow, x: f32, y: f32, between: impl FnOnce()) {
        let position = slint::LogicalPosition::new(x, y);
        let button = PointerEventButton::Left;
        w.window().dispatch_event(WindowEvent::PointerMoved { position });
        w.window().dispatch_event(WindowEvent::PointerPressed { position, button });
        between();
        w.window().dispatch_event(WindowEvent::PointerReleased { position, button });
        w.window().dispatch_event(WindowEvent::PointerExited);
    }

    /// The rack's buttons take a click while the engine's status keeps
    /// arriving (every 100 ms): a status that lands between the press and
    /// the release must not lose it.
    #[test]
    fn a_rack_click_survives_a_status_update() {
        let scratch = std::env::temp_dir().join(format!("patch-rack-test-{}.json", std::process::id()));
        // SAFETY: set before anything in this binary reads it; only the
        // plugin rack's path reads it.
        unsafe { std::env::set_var("PATCH_PLUGINS_CONFIG", &scratch) };
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        {
            let ui = &app.plugins;
            ui.loaded.set(true);
            ui.stamp.set(stamp());
            let mut rack = RackConfig::default();
            rack.slots.push(patch_plugin::Slot {
                id: 1,
                plugin: "/nonexistent/Test.vst3".into(),
                name: "Test".into(),
                vendor: "Patch".into(),
                version: String::new(),
                active: false,
                view: View::Patch,
                zoom: 1.0,
                place: patch_plugin::Place::Master,
                parameters: Vec::new(),
            });
            *ui.rack.borrow_mut() = rack;
        }
        let w = app.create("plugins");
        {
            use patch_host::session::SurfaceWindow;
            let spec = AudioApp::spec();
            w.apply_host(&patch_host::session::HostState::build(&*app, &spec, "plugins", &[], None));
        }
        let shooter = patch_host::shot::Shooter { scale: 1.0 };
        let image = shooter.render(&w, 1200, 700);
        if let Some(dir) = std::env::var_os("PATCH_TEST_SHOT") {
            image.save(std::path::Path::new(&dir).join("rack-test.png")).unwrap();
        }
        w.show().unwrap();
        let opened = Rc::new(RefCell::new(Vec::<String>::new()));
        {
            let opened = opened.clone();
            w.global::<Host>().on_focus_surface(move |s| opened.borrow_mut().push(s.to_string()));
        }
        // Find "Open" on the row: plain clicks, left to right from past the
        // power switch (so before ↑ ↓ ×), down the rack.
        let mut at = None;
        // Below the signal path (its chips open plugins too).
        'scan: for y in (110..300).step_by(6) {
            for x in (300..1200).step_by(8) {
                click(&w, x as f32, y as f32, || {});
                if !opened.borrow().is_empty() {
                    at = Some((x as f32, y as f32));
                    break 'scan;
                }
            }
        }
        let (x, y) = at.expect("no Open button on the rack row");
        assert_eq!(opened.borrow().as_slice(), ["plugin"]);
        // Again, with the engine's status landing mid-click.
        opened.borrow_mut().clear();
        click(&w, x, y, || {
            app.plugins.status.borrow_mut().insert(1, json!({"id": 1, "state": "running", "output_peak": 0.3, "load": 0.02}));
            app.fill_rack(&w);
        });
        assert_eq!(opened.borrow().as_slice(), ["plugin"], "the click was lost");
        let rack = app.plugins.rack.borrow();
        assert_eq!((rack.slots.len(), rack.slots[0].active), (1, false), "the scan changed the rack");
        drop(rack);
        let _ = std::fs::remove_file(&scratch);
    }

    /// The path reads in the engine's order: the sources, the master EQ,
    /// the rack (all of it; a plugin's window: those on and itself), Tune,
    /// the fader, the limiter, the output.
    #[test]
    fn the_signal_path_runs_in_the_engines_order() {
        let scratch = std::env::temp_dir().join(format!("patch-rack-test-{}.json", std::process::id()));
        // SAFETY: as in the test above (same value).
        unsafe { std::env::set_var("PATCH_PLUGINS_CONFIG", &scratch) };
        let _ = patch_host::shot::install();
        let app = AudioApp::new();
        let ui = &app.plugins;
        ui.loaded.set(true);
        let slot = |id: u32, name: &str, active: bool| patch_plugin::Slot {
            id,
            plugin: format!("/nonexistent/{name}.vst3").into(),
            name: name.into(),
            vendor: String::new(),
            version: String::new(),
            active,
            view: View::Patch,
            zoom: 1.0,
            place: patch_plugin::Place::Master,
            parameters: Vec::new(),
        };
        ui.rack.borrow_mut().slots = vec![slot(1, "First", true), slot(2, "Second", false), slot(3, "Third", true)];
        let kinds = |steps: Vec<RouteStep>| -> Vec<String> {
            steps.iter().map(|s| format!("{}:{}", s.kind, if s.kind == "in" || s.kind == "out" { "" } else { s.name.as_str() })).collect()
        };
        let master = Place::Master;
        assert_eq!(
            kinds(app.route(&master, None)),
            ["in:", "stage:EQ", "plugin:First", "off:Second", "plugin:Third", "stage:Tune", "stage:Fader", "stage:Limiter", "out:"]
        );
        assert_eq!(
            kinds(app.route(&master, Some(3))),
            ["in:", "stage:EQ", "plugin:First", "this:Third", "stage:Tune", "stage:Fader", "stage:Limiter", "out:"]
        );
        assert_eq!(kinds(app.route(&master, Some(2)))[3], "this:Second (off)");
        assert_eq!(zoom_index(1.5), 1);
        // Elsewhere in the mix: a line of its own, and gone from the master's.
        ui.rack.borrow_mut().slot_mut(3).unwrap().place = Place::parse("mic-1");
        ui.rack.borrow_mut().slot_mut(2).unwrap().place = Place::Fx(1);
        let lines = app.route_lines();
        // In rack order: Second (FX 1) before Third (Mic 1).
        assert_eq!(lines.len(), 3, "master, FX 1, mic-1");
        assert_eq!(kinds(lines[0].1.clone()), ["in:", "stage:EQ", "plugin:First", "stage:Tune", "stage:Fader", "stage:Limiter", "out:"]);
        assert_eq!(kinds(lines[1].1.clone()), ["in:", "stage:Reverb", "off:Second", "stage:Return", "out:"]);
        assert_eq!(kinds(lines[2].1.clone()), ["in:", "plugin:Third", "stage:Strip", "out:"]);
        assert_eq!((lines[1].0.as_str(), lines[2].0.as_str()), ("FX 1", "MIC 1"));
    }

    #[test]
    fn strips_and_shared_words_make_sections() {
        let names = [
            "CLEAN 15\" Gain", "CLEAN 15\" Mute", "CLEAN 15\" Pan",
            "CLEAN 12\" Gain", "CLEAN 12\" Mute", "CLEAN 12\" Pan",
            "FX 57 Drive", "FX 57 Gain", "FX 57 Pan",
            "Allow C", "Allow D", "Allow E",
            "Wet Mix", "2nd Sub Oct Mix", "2nd Sub 5th Mix", "Output Gain",
        ];
        let keys = group_keys(&names);
        assert_eq!(keys[0], "CLEAN 15\"");
        assert_eq!(keys[3], "CLEAN 12\"");
        assert_eq!(keys[6], "FX 57");
        assert_eq!(keys[9], "Allow");
        assert!(keys[12..].iter().all(|k| k.is_empty()), "{keys:?}");
    }
}
