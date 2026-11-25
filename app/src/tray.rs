//! `omarchy-patch tray`: the bar's Patch menu on desktops other than Omarchy.
//!
//! On Omarchy the bar's audio dropdown has a Patch section
//! (integration/omarchy-shell/PatchSection.qml). Every other desktop gets
//! the same things as a tray icon (a StatusNotifierItem: KDE Plasma,
//! GNOME with the AppIndicator extension, Cinnamon, Waybar, COSMIC, …):
//! what runs with its pause and stop, the windows on screen, the four
//! basic layouts and four recent ones, the recent Units, "Open a Unit or
//! layout…" (the controller, with its search) and the master EQ.
//!
//! The menu reads `bar status` only when it opens (dbusmenu's AboutToShow),
//! as the Omarchy section polls only while its dropdown is open. Between
//! openings the tray only looks at the icon's state every two seconds:
//! the buffer health the engine writes (`health.json`, the bar's dot) and
//! whether a take or a broadcast runs (their sockets). Nothing is spawned
//! and the engine is never asked.
//!
//! It starts with the session (an XDG autostart entry written by the `bar`
//! setup switch, which means this menu here), once: a second one exits.
//! Built only into the generic kit (the `generic` feature).

use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as ZValue};

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";
/// One tray per session (the autostart entry and a manual start alike).
const ONE: &str = "app.patchaudio.Tray";
const AUTOSTART: &str = "omarchy-patch-tray.desktop";

/// What a click does.
#[derive(Clone, Debug, PartialEq)]
enum Action {
    /// `omarchy-patch <args…>`, in the background.
    Run(Vec<String>),
    /// Close Patch's windows, then open this layout.
    Layout(String),
}

#[derive(Default)]
struct Menu {
    revision: u32,
    /// Item id → its layout node; 0 is the root.
    items: Vec<Node>,
}

struct Node {
    props: HashMap<String, OwnedValue>,
    children: Vec<i32>,
    action: Option<Action>,
}

fn s(v: &str) -> OwnedValue {
    OwnedValue::try_from(ZValue::from(v)).expect("a string is a value")
}

fn b(v: bool) -> OwnedValue {
    OwnedValue::from(v)
}

impl Menu {
    fn add(&mut self, parent: i32, props: Vec<(&str, OwnedValue)>, action: Option<Action>) -> i32 {
        let id = self.items.len() as i32;
        self.items.push(Node {
            props: props.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            children: Vec::new(),
            action,
        });
        if let Some(p) = self.items.get_mut(parent as usize) {
            p.children.push(id);
        }
        id
    }

    fn item(&mut self, parent: i32, label: &str, action: Option<Action>) -> i32 {
        let mut props = vec![("label", s(label))];
        if action.is_none() {
            props.push(("enabled", b(false)));
        }
        self.add(parent, props, action)
    }

    fn check(&mut self, parent: i32, label: &str, on: bool, radio: bool, action: Action) -> i32 {
        self.add(
            parent,
            vec![
                ("label", s(label)),
                ("toggle-type", s(if radio { "radio" } else { "checkmark" })),
                ("toggle-state", OwnedValue::from(i32::from(on))),
            ],
            Some(action),
        )
    }

    fn submenu(&mut self, parent: i32, label: &str) -> i32 {
        self.add(parent, vec![("label", s(label)), ("children-display", s("submenu"))], None)
    }

    fn separator(&mut self, parent: i32) {
        self.add(parent, vec![("type", s("separator"))], None);
    }

    /// The node and its children, `depth` levels down (-1: all), as
    /// dbusmenu's `(ia{sv}av)`.
    fn layout(&self, id: i32, depth: i32) -> (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>) {
        let node = &self.items[id as usize];
        let children = if depth == 0 {
            Vec::new()
        } else {
            node.children
                .iter()
                .filter_map(|c| OwnedValue::try_from(ZValue::from(self.layout(*c, depth - 1))).ok())
                .collect()
        };
        (id, node.props.clone(), children)
    }
}

/// The menu from a `bar status` (the same JSON the Omarchy section reads).
fn build(status: &Value) -> Menu {
    let mut m = Menu::default();
    m.add(-1, vec![("children-display", s("submenu"))], None);
    let run = |args: &[&str]| Action::Run(args.iter().map(|a| a.to_string()).collect());
    let label = |v: &Value| v["label"].as_str().unwrap_or("").to_string();

    // What runs, with every window closed: a row each.
    let now = status["now"].as_array().cloned().unwrap_or_default();
    if now.is_empty() {
        m.item(0, "Nothing plays", None);
    }
    for row in &now {
        let detail = row["detail"].as_str().unwrap_or("");
        let title = if detail.is_empty() { label(row) } else { format!("{} · {detail}", label(row)) };
        let sub = m.submenu(0, &title);
        for a in row["actions"].as_array().into_iter().flatten() {
            let words: Vec<String> = ["bar", "do"]
                .into_iter()
                .map(str::to_string)
                .chain(a["do"].as_str().unwrap_or("").split(' ').map(str::to_string))
                .collect();
            if a["confirm"].as_bool() == Some(true) {
                // Asked once, as in the bar: a submenu to confirm in.
                let ask = m.submenu(sub, &label(a));
                m.item(ask, &format!("{} now", label(a)), Some(Action::Run(words)));
            } else {
                m.item(sub, &label(a), Some(Action::Run(words)));
            }
        }
        let go = row["go"].as_array().cloned().unwrap_or_default();
        if !go.is_empty() {
            m.separator(sub);
        }
        for g in &go {
            if let Some(id) = g["id"].as_str() {
                m.item(sub, &label(g), Some(run(&["open", id])));
            }
        }
    }
    m.separator(0);

    // The windows on screen, and the layout's own commands.
    let open = status["open"].as_array().cloned().unwrap_or_default();
    if !open.is_empty() {
        let sub = m.submenu(0, &format!("Windows on screen ({})", open.len()));
        for w in &open {
            if let Some(id) = w["id"].as_str() {
                m.item(sub, &label(w), Some(run(&["open", id])));
            }
        }
        if !status["layout"].as_str().unwrap_or("").is_empty() {
            m.separator(sub);
            m.item(sub, "Show the layout", Some(run(&["layout", "show"])));
            m.item(sub, "Reset the layout", Some(run(&["layout", "reset"])));
        }
        m.item(sub, "Close Patch's windows", Some(run(&["quit"])));
    }

    // Four basic layouts and four recent ones (docs/decisions.md: the bar
    // keeps quick picks, never every layout; the controller finds the rest).
    let layouts = m.submenu(0, "Layouts");
    let current = status["layout"].as_str().unwrap_or("");
    let mut shown = 0;
    for key in ["standard", "slots"] {
        if shown > 0 {
            m.separator(layouts);
        }
        for l in status[key].as_array().into_iter().flatten() {
            if let Some(id) = l["id"].as_str() {
                m.check(layouts, &label(l), id == current, true, Action::Layout(id.to_string()));
                shown += 1;
            }
        }
    }
    for u in status["suggest"].as_array().into_iter().flatten() {
        if u["kind"].as_str() == Some("unit")
            && let Some(id) = u["id"].as_str()
        {
            m.item(0, &format!("Open {}", label(u)), Some(run(&["open", id])));
        }
    }
    m.item(0, "Open a Unit or layout…", Some(run(&[])));
    m.separator(0);

    // The master EQ: its presets and bypass.
    let eq = m.submenu(0, "EQ");
    let preset = status["eq"]["preset"].as_str().unwrap_or("");
    for p in status["presets"].as_array().into_iter().flatten().filter_map(|p| p.as_str()) {
        m.check(eq, p, p == preset, true, run(&["eq", "master", "preset", p]));
    }
    m.separator(eq);
    let bypass = status["eq"]["bypass"].as_bool() == Some(true);
    m.check(eq, "Bypass", bypass, false, run(&["eq", "master", "bypass", if bypass { "off" } else { "on" }]));
    m.item(eq, "Open the EQ", Some(run(&["open", "eq"])));
    m
}

/// The icon's state: the bar's dot (buffer health, docs/decisions.md: only
/// when something is wrong) and a take or a broadcast in progress.
#[derive(Clone, PartialEq, Default)]
struct Look {
    /// "", "amber" or "red".
    alert: &'static str,
    tip: String,
}

fn look() -> Look {
    let runtime = patch_core::ipc::runtime_dir();
    let mut tip = Vec::new();
    let mut alert = "";
    if runtime.join("recorder.sock").exists() {
        tip.push("Recording".to_string());
        alert = "red";
    }
    if runtime.join("broadcast.sock").exists() {
        tip.push("Broadcasting".to_string());
        alert = "red";
    }
    if let Some(h) = std::fs::read_to_string(runtime.join("health.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    {
        match h["state"].as_str() {
            Some("red") => {
                alert = "red";
                tip.push(h["reason"].as_str().unwrap_or("Audio skipped").to_string());
            }
            Some("amber") => {
                if alert.is_empty() {
                    alert = "amber";
                }
                tip.push(h["reason"].as_str().unwrap_or("Audio near its limit").to_string());
            }
            _ => {}
        }
    }
    Look { alert, tip: tip.join(" · ") }
}

struct State {
    menu: Menu,
    look: Look,
}

struct Item(Arc<Mutex<State>>);

#[zbus::interface(name = "org.kde.StatusNotifierItem")]
impl Item {
    #[zbus(property)]
    fn category(&self) -> &str {
        "ApplicationStatus"
    }
    #[zbus(property)]
    fn id(&self) -> &str {
        "omarchy-patch"
    }
    #[zbus(property)]
    fn title(&self) -> &str {
        "Patch"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        if self.0.lock().unwrap().look.alert == "red" { "NeedsAttention" } else { "Active" }
    }
    #[zbus(property)]
    fn window_id(&self) -> i32 {
        0
    }
    #[zbus(property)]
    fn icon_name(&self) -> String {
        match self.0.lock().unwrap().look.alert {
            "" => "omarchy-patch".into(),
            a => format!("omarchy-patch-{a}"),
        }
    }
    #[zbus(property)]
    fn attention_icon_name(&self) -> &str {
        "omarchy-patch-red"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> String {
        icon_dir().to_string_lossy().into_owned()
    }
    #[zbus(property)]
    fn tool_tip(&self) -> (String, Vec<(i32, i32, Vec<u8>)>, String, String) {
        let tip = self.0.lock().unwrap().look.tip.clone();
        ("omarchy-patch".into(), Vec::new(), "Patch".into(), tip)
    }
    /// Left click opens the controller; the menu is on the other button.
    #[zbus(property)]
    fn item_is_menu(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn menu(&self) -> OwnedObjectPath {
        OwnedObjectPath::try_from(MENU_PATH).expect("a valid path")
    }
    fn activate(&self, _x: i32, _y: i32) {
        perform(Action::Run(Vec::new()));
    }
    fn secondary_activate(&self, _x: i32, _y: i32) {}
    fn context_menu(&self, _x: i32, _y: i32) {}
    fn scroll(&self, _delta: i32, _orientation: &str) {}

    #[zbus(signal)]
    async fn new_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_attention_icon(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_tool_tip(emitter: &SignalEmitter<'_>) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn new_status(emitter: &SignalEmitter<'_>, status: &str) -> zbus::Result<()>;
}

struct DbusMenu(Arc<Mutex<State>>);

type Layout = (i32, HashMap<String, OwnedValue>, Vec<OwnedValue>);

#[zbus::interface(name = "com.canonical.dbusmenu")]
impl DbusMenu {
    #[zbus(property)]
    fn version(&self) -> u32 {
        3
    }
    #[zbus(property)]
    fn text_direction(&self) -> &str {
        "ltr"
    }
    #[zbus(property)]
    fn status(&self) -> &str {
        "normal"
    }
    #[zbus(property)]
    fn icon_theme_path(&self) -> Vec<String> {
        vec![icon_dir().to_string_lossy().into_owned()]
    }

    fn get_layout(&self, parent: i32, depth: i32, _names: Vec<String>) -> zbus::fdo::Result<(u32, Layout)> {
        let st = self.0.lock().unwrap();
        if parent < 0 || parent as usize >= st.menu.items.len() {
            return Err(zbus::fdo::Error::InvalidArgs(format!("no item {parent}")));
        }
        Ok((st.menu.revision, st.menu.layout(parent, depth)))
    }

    fn get_group_properties(&self, ids: Vec<i32>, _names: Vec<String>) -> Vec<(i32, HashMap<String, OwnedValue>)> {
        let st = self.0.lock().unwrap();
        let all: Vec<i32> = if ids.is_empty() { (0..st.menu.items.len() as i32).collect() } else { ids };
        all.into_iter()
            .filter_map(|id| st.menu.items.get(id as usize).map(|n| (id, n.props.clone())))
            .collect()
    }

    fn get_property(&self, id: i32, name: String) -> zbus::fdo::Result<OwnedValue> {
        let st = self.0.lock().unwrap();
        st.menu
            .items
            .get(id as usize)
            .and_then(|n| n.props.get(&name))
            .and_then(|v| v.try_clone().ok())
            .ok_or_else(|| zbus::fdo::Error::InvalidArgs(format!("no {name} on {id}")))
    }

    fn event(&self, id: i32, event: String, _data: OwnedValue, _time: u32) {
        if event != "clicked" {
            return;
        }
        let action = self.0.lock().unwrap().menu.items.get(id as usize).and_then(|n| n.action.clone());
        if let Some(a) = action {
            perform(a);
        }
    }

    fn event_group(&self, events: Vec<(i32, String, OwnedValue, u32)>) -> Vec<i32> {
        for (id, event, data, time) in events {
            self.event(id, event, data, time);
        }
        Vec::new()
    }

    /// The menu is about to open: read what runs now.
    async fn about_to_show(
        &self,
        id: i32,
        #[zbus(signal_emitter)] emitter: SignalEmitter<'_>,
    ) -> bool {
        if id != 0 {
            return false;
        }
        let status = crate::bar::status(crate::send(patch_core::ipc::Request::Status));
        let revision = {
            let mut st = self.0.lock().unwrap();
            let mut menu = build(&status);
            menu.revision = st.menu.revision.wrapping_add(1);
            st.menu = menu;
            st.menu.revision
        };
        let _ = Self::layout_updated(&emitter, revision, 0).await;
        true
    }

    fn about_to_show_group(&self, ids: Vec<i32>) -> (Vec<i32>, Vec<i32>) {
        (ids.into_iter().filter(|i| *i == 0).collect(), Vec::new())
    }

    #[zbus(signal)]
    async fn layout_updated(emitter: &SignalEmitter<'_>, revision: u32, parent: i32) -> zbus::Result<()>;
    #[zbus(signal)]
    async fn items_properties_updated(
        emitter: &SignalEmitter<'_>,
        updated: Vec<(i32, HashMap<String, OwnedValue>)>,
        removed: Vec<(i32, Vec<String>)>,
    ) -> zbus::Result<()>;
}

fn icon_dir() -> PathBuf {
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .unwrap_or_default();
    data.join("icons")
}

/// A click: Patch itself, in the background, outside this process's group.
fn perform(action: Action) {
    std::thread::spawn(move || {
        let Ok(exe) = crate::bar::invoked().map(Ok).unwrap_or_else(std::env::current_exe) else {
            return;
        };
        let spawn = |args: &[String]| {
            use std::os::unix::process::CommandExt as _;
            Command::new(&exe)
                .args(args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .and_then(|mut c| c.wait())
        };
        match action {
            Action::Run(args) => {
                let _ = spawn(&args);
            }
            Action::Layout(id) => {
                // As the bar does: the windows on screen close, then the layout
                // opens. A desk's own session takes the layout in place.
                if !crate::desk::running() {
                    let _ = spawn(&["quit".to_string()]);
                }
                for _ in 0..40 * usize::from(!crate::desk::running()) {
                    let closed = Command::new(&exe)
                        .arg("status")
                        .output()
                        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "closed");
                    if closed {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
                let _ = spawn(&["layout".to_string(), id]);
            }
        }
    });
}

/// Run the tray (until the session ends).
pub fn run() -> anyhow::Result<()> {
    let state = Arc::new(Mutex::new(State {
        menu: build(&json!({})),
        look: look(),
    }));
    let name = format!("org.kde.StatusNotifierItem-{}-1", std::process::id());
    let conn = zbus::blocking::connection::Builder::session()?
        .serve_at(ITEM_PATH, Item(state.clone()))?
        .serve_at(MENU_PATH, DbusMenu(state.clone()))?
        .build()?;
    // One tray per session: a second start (the autostart entry and a
    // manual one) leaves.
    use zbus::fdo::{RequestNameFlags, RequestNameReply};
    match conn.request_name_with_flags(ONE, RequestNameFlags::DoNotQueue.into()) {
        Ok(RequestNameReply::PrimaryOwner) | Ok(RequestNameReply::AlreadyOwner) => {}
        _ => return Ok(()),
    }
    conn.request_name(name.as_str())?;
    // The Player in the desktop's media controls (mpris.rs), on this
    // connection; it follows the Player whenever its socket is there.
    let player_now = crate::mpris::serve(&conn)?;
    let following = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Register with the tray host now, and again whenever one (re)starts
    // (a panel restart, GNOME's AppIndicator extension switched on).
    let register = {
        let conn = conn.clone();
        let name = name.clone();
        move || {
            let _ = conn.call_method(
                Some(WATCHER),
                "/StatusNotifierWatcher",
                Some(WATCHER),
                "RegisterStatusNotifierItem",
                &(name.as_str(),),
            );
        }
    };
    register();
    {
        let conn = conn.clone();
        let register = register.clone();
        std::thread::Builder::new().name("patch-tray-watcher".into()).spawn(move || {
            let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return };
            let Ok(changes) = dbus.receive_name_owner_changed_with_args(&[(0, WATCHER)]) else { return };
            for c in changes {
                if c.args().is_ok_and(|a| a.new_owner().is_some()) {
                    register();
                }
            }
        })?;
    }

    // The icon follows the bar's dot and a take or broadcast; a new
    // Patch installed over this one takes over the tray.
    let item = conn.object_server().interface::<_, Item>(ITEM_PATH)?;
    let started = std::fs::read_link("/proc/self/exe").ok();
    loop {
        use std::sync::atomic::Ordering;
        if !following.load(Ordering::Relaxed) && patch_core::ipc::runtime_dir().join("player.sock").exists() {
            following.store(true, Ordering::Relaxed);
            let (conn, player_now, following) = (conn.clone(), player_now.clone(), following.clone());
            let _ = std::thread::Builder::new().name("patch-mpris".into()).spawn(move || {
                crate::mpris::follow(&conn, &player_now);
                following.store(false, Ordering::Relaxed);
            });
        }
        std::thread::sleep(Duration::from_secs(2));
        let now = look();
        let changed = {
            let mut st = state.lock().unwrap();
            let changed = st.look != now;
            st.look = now.clone();
            changed
        };
        if changed {
            let e = item.signal_emitter();
            let status = if now.alert == "red" { "NeedsAttention" } else { "Active" };
            let _ = zbus::block_on(async {
                Item::new_icon(e).await?;
                Item::new_tool_tip(e).await?;
                Item::new_status(e, status).await
            });
        }
        if let Some(path) = &started
            && std::fs::read_link("/proc/self/exe").is_ok_and(|p| p.to_string_lossy().ends_with(" (deleted)"))
        {
            use std::os::unix::process::CommandExt as _;
            drop(conn);
            let exe = crate::bar::invoked().unwrap_or_else(|| {
                PathBuf::from(path.to_string_lossy().trim_end_matches(" (deleted)"))
            });
            let err = Command::new(exe).arg("tray").exec();
            anyhow::bail!("restarting the tray: {err}");
        }
    }
}

fn autostart_file() -> PathBuf {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_default();
    config.join("autostart").join(AUTOSTART)
}

/// The `bar` setup switch outside Omarchy: the tray starts with the
/// session (XDG autostart: GNOME, KDE, Cinnamon, Xfce, MATE, …) and now.
pub fn install() -> anyhow::Result<()> {
    let file = autostart_file();
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(
        &file,
        "[Desktop Entry]\nType=Application\nName=Patch tray\nComment=What Patch plays, its windows and layouts, in the panel\nExec=omarchy-patch tray\nIcon=omarchy-patch\nTerminal=false\nNoDisplay=true\nX-GNOME-Autostart-enabled=true\n",
    )?;
    // Now too, when this runs inside the desktop session.
    if std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
        && (std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some())
    {
        let exe = crate::bar::invoked().unwrap_or(std::env::current_exe()?);
        // Its own scope, as the layouts are started: it outlives the
        // installer's terminal and process group.
        let scoped = Command::new("systemd-run")
            .args(["--user", "--scope", "--collect", "--quiet"])
            .arg(&exe)
            .arg("tray")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if scoped.is_err() {
            use std::os::unix::process::CommandExt as _;
            let _ = Command::new(&exe).arg("tray").process_group(0).spawn();
        }
    }
    Ok(())
}

/// The switch off: no tray from the next session, and none now.
pub fn uninstall() -> anyhow::Result<()> {
    let _ = std::fs::remove_file(autostart_file());
    if let Ok(conn) = zbus::blocking::Connection::session()
        && let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn)
        && let Ok(owner) = dbus.get_connection_unix_process_id(zbus::names::BusName::try_from(ONE)?)
    {
        // SAFETY: a plain signal to the tray's own pid.
        unsafe { libc::kill(owner as i32, libc::SIGTERM) };
    }
    Ok(())
}

/// Whether this desktop has the Omarchy bar (then the bar has the menu).
pub fn omarchy_bar() -> bool {
    crate::bar::has_shell()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> Value {
        json!({
            "now": [{"actions": [{"confirm": false, "do": "set resume", "label": "Resume"},
                                 {"confirm": true, "do": "set end", "label": "End set"}],
                     "detail": "Paused · 28 min", "go": [{"id": "deck-a", "label": "Deck A"}],
                     "id": "set", "label": "Auto Mix"}],
            "open": [{"id": "mixer", "label": "Mixer"}],
            "layout": "simple",
            "standard": [{"id": "simple", "label": "Simple"}, {"id": "basic-dj", "label": "Basic DJ"},
                         {"id": "ai-dj", "label": "Auto Mix"}, {"id": "dj-studio", "label": "DJ Studio"}],
            "slots": [{"id": "pro", "label": "Pro"}, {"id": "mine", "label": "Mine"},
                      {"id": "podcast", "label": "Podcast"}, {"id": "live-code", "label": "Live Code"}],
            "layouts": [{"id": "a"}, {"id": "b"}, {"id": "c"}, {"id": "d"}, {"id": "e"}, {"id": "f"},
                        {"id": "g"}, {"id": "h"}, {"id": "i"}, {"id": "j"}],
            "suggest": [{"id": "eq", "kind": "unit", "label": "EQ"}, {"id": "pro", "kind": "layout", "label": "Pro"}],
            "eq": {"preset": "Warm", "bypass": false},
            "presets": ["Flat", "Warm"],
        })
    }

    fn labels(m: &Menu, id: i32) -> Vec<String> {
        m.items[id as usize]
            .children
            .iter()
            .map(|c| {
                m.items[*c as usize]
                    .props
                    .get("label")
                    .and_then(|v| <&str>::try_from(&**v).ok().map(str::to_string))
                    .unwrap_or_default()
            })
            .collect()
    }

    fn find(m: &Menu, label: &str) -> i32 {
        (0..m.items.len() as i32)
            .find(|i| {
                m.items[*i as usize]
                    .props
                    .get("label")
                    .and_then(|v| <&str>::try_from(&**v).ok())
                    == Some(label)
            })
            .unwrap_or_else(|| panic!("no {label}"))
    }

    #[test]
    fn a_running_set_has_its_actions_and_ending_it_asks_once() {
        let m = build(&status());
        let set = find(&m, "Auto Mix · Paused · 28 min");
        assert_eq!(labels(&m, set), ["Resume", "End set", "", "Deck A"]);
        let end = find(&m, "End set");
        let now = m.items[end as usize].children[0];
        assert_eq!(m.items[now as usize].action, Some(Action::Run(vec!["bar".into(), "do".into(), "set".into(), "end".into()])));
        assert_eq!(m.items[end as usize].action, None, "the first click only opens the question");
    }

    #[test]
    fn the_menu_keeps_four_basic_layouts_and_four_recent_ones() {
        let m = build(&status());
        let layouts = find(&m, "Layouts");
        assert_eq!(
            labels(&m, layouts),
            ["Simple", "Basic DJ", "Auto Mix", "DJ Studio", "", "Pro", "Mine", "Podcast", "Live Code"],
            "docs/decisions.md: quick picks, never every layout"
        );
    }

    #[test]
    fn units_open_and_the_eq_shows_its_preset() {
        let m = build(&status());
        assert!(labels(&m, 0).contains(&"Open EQ".to_string()));
        assert!(!labels(&m, 0).contains(&"Open Pro".to_string()), "layouts are under Layouts");
        let warm = find(&m, "Warm");
        assert_eq!(m.items[warm as usize].props.get("toggle-state").and_then(|v| i32::try_from(&**v).ok()), Some(1));
    }

    #[test]
    fn nothing_running_says_so() {
        let m = build(&json!({}));
        assert_eq!(labels(&m, 0)[0], "Nothing plays");
    }

    #[test]
    fn the_layout_is_dbusmenus_shape() {
        let m = build(&status());
        let v = OwnedValue::try_from(ZValue::from(m.layout(0, -1))).unwrap();
        assert_eq!(v.value_signature().to_string(), "(ia{sv}av)");
    }
}
