//! Hyprland 0.56+ integration (Lua dispatch API).
//!
//! Nothing here edits the user's config. Window rules are registered for the
//! running compositor session only (`hyprctl eval`) and vanish on reload;
//! `integration/hypr/units.lua` is the persistent equivalent.

use std::process::{Command, Stdio};

/// Hyprland is there to talk to. Never while rendering headlessly: shots and
/// tests must not move focus or windows on the user's desktop.
pub fn available() -> bool {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        && std::env::var_os("UNIT_NO_HYPR").is_none()
        && !crate::shot::active()
}

/// A request to Hyprland: straight over its request socket (what `hyprctl`
/// itself does, ~0.1 ms), else through the `hyprctl` process (~3.5 ms of
/// fork and exec, on the UI thread: a layout makes hundreds of these).
fn hyprctl(args: &[&str]) -> Option<String> {
    if let Some(reply) = request(args) {
        return reply;
    }
    let out = Command::new("hyprctl")
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn dispatch(lua: &str) -> bool {
    hyprctl(&["dispatch", lua]).is_some_and(|s| s.trim() == "ok")
}

fn lua_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn title_regex(title: &str) -> String {
    let escaped: String = title
        .chars()
        .map(|c| {
            if "\\^$.|?*+()[]{}".contains(c) {
                format!("\\{c}")
            } else {
                c.to_string()
            }
        })
        .collect();
    format!("title:^({escaped})$")
}

pub fn focus_title(title: &str) -> bool {
    dispatch(&format!(
        "hl.dsp.focus({{ window = {} }})",
        lua_str(&title_regex(title))
    ))
}

/// Dense data surfaces stay fully opaque. Omarchy 4.5 leaves applications
/// opaque unless they opt in (`o.transparent_window`, Super+Backspace); a
/// rule that did match is still ordered ahead of anything set at run time,
/// so the window props are set once the window has mapped.
/// `UNIT_OPACITY=omarchy` keeps Omarchy's translucency.
pub fn make_opaque(title: &str) -> bool {
    if std::env::var("UNIT_OPACITY").is_ok_and(|v| v == "omarchy") {
        return true;
    }
    let w = lua_str(&title_regex(title));
    ["opacity", "opacity_inactive"].iter().all(|prop| {
        dispatch(&format!(
            "hl.dsp.window.set_prop({{ window = {w}, prop = \"{prop}\", value = \"1.0\" }})"
        ))
    })
}

pub fn preselect(dir: char) -> bool {
    dispatch(&format!("hl.dsp.layout(\"preselect {dir}\")"))
}

/// Dwindle split ratio for the focused window's node; `share` of the first
/// child. Hyprland 0.56 only takes deltas; a fresh split starts at 1.0 (even).
pub fn split_ratio(share: f32) -> bool {
    let delta = (share * 2.0).clamp(0.1, 1.9) - 1.0;
    delta.abs() < 0.005 || dispatch(&format!("hl.dsp.layout(\"splitratio {delta:.3}\")"))
}

/// Omarchy 4.5 gives each monitor a block of ten Hyprland workspace ids
/// (`default/hypr/monitor-workspaces.lua`). Outside that (Omarchy 4.4, or
/// Hyprland alone) workspaces stay a single global sequence.
const MONITOR_SLOTS: i64 = 10;

fn omarchy_state_dir() -> Option<std::path::PathBuf> {
    let base = match std::env::var_os("XDG_STATE_HOME") {
        Some(v) if !v.is_empty() => std::path::PathBuf::from(v),
        _ => {
            let home = std::env::var_os("HOME")?;
            if home.is_empty() {
                return None;
            }
            std::path::PathBuf::from(home).join(".local/state")
        }
    };
    Some(base.join("omarchy"))
}

/// `block<TAB>connector<TAB>identity`, as Omarchy writes `monitor-workspaces`.
pub(crate) fn block_for_connector(state: &str, connector: &str) -> Option<i64> {
    if connector.is_empty() {
        return None;
    }
    for line in state.lines() {
        let mut parts = line.splitn(3, '\t');
        let Some(block) = parts.next().and_then(|s| s.parse::<i64>().ok()) else {
            continue;
        };
        let Some(conn) = parts.next() else {
            continue;
        };
        if conn == connector && block >= 0 {
            return Some(block);
        }
    }
    None
}

fn focused_monitor_name() -> Option<String> {
    let json = hyprctl(&["monitors", "-j"])?;
    let all: serde_json::Value = serde_json::from_str(&json).ok()?;
    all.as_array()?
        .iter()
        .find(|m| m["focused"] == true)?["name"]
        .as_str()
        .map(str::to_string)
}

/// The focused monitor's block, when Omarchy is numbering workspaces per
/// monitor. `None` means the global sequence.
fn focused_monitor_block() -> Option<i64> {
    let text = std::fs::read_to_string(omarchy_state_dir()?.join("monitor-workspaces")).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    if let Some(name) = focused_monitor_name()
        && let Some(block) = block_for_connector(&text, &name)
    {
        return Some(block);
    }
    active_workspace_id().filter(|id| *id >= 1).map(|id| (id - 1) / MONITOR_SLOTS)
}

/// Block to search. A layout that already has a workspace stays in that
/// workspace's block, even if focus has drifted to another monitor.
fn search_block(after: i64) -> Option<i64> {
    let focused = focused_monitor_block()?;
    if after >= 1 {
        Some((after - 1) / MONITOR_SLOTS)
    } else {
        Some(focused)
    }
}

/// The next empty workspace id. With a monitor block, only that block of ten.
pub(crate) fn next_workspace_id(after: i64, busy: &[i64], block: Option<i64>) -> Option<i64> {
    let free = |id: i64| id > 0 && !busy.contains(&id);
    match block {
        Some(block) if block >= 0 => {
            let start = block.saturating_mul(MONITOR_SLOTS) + 1;
            let end = start + MONITOR_SLOTS - 1;
            let from = if after < start { start } else { after + 1 };
            (from..=end).find(|id| free(*id))
        }
        _ => (after.max(0) + 1..).find(|id| free(*id)),
    }
}

pub(crate) fn quoted_assignment<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("{key} = \"");
    let rest = text.get(text.find(&needle)? + needle.len()..)?;
    let end = rest.find('"')?;
    let value = &rest[..end];
    (!value.is_empty() && value.chars().all(|c| c.is_ascii_alphabetic())).then_some(value)
}

/// How Omarchy 4.5 names a workspace's layout: the workspace's own file, else
/// the saved default, else the tiled layout Hyprland reports. Floating is not
/// a Hyprland layout, so it only shows up in the files.
pub(crate) fn layout_mode_from(
    per_workspace: Option<&str>,
    default_file: Option<&str>,
    tiled: Option<&str>,
) -> Option<String> {
    if let Some(text) = per_workspace
        && let Some(mode) = quoted_assignment(text, "mode")
    {
        return Some(mode.to_string());
    }
    if let Some(mode) = default_file.and_then(|text| quoted_assignment(text, "default")) {
        return Some(mode.to_string());
    }
    // No saved mode: Hyprland's own tiled layout, as the Omarchy helper does.
    tiled.filter(|s| !s.is_empty()).map(str::to_string)
}

fn tiled_layout_name(id: i64) -> Option<String> {
    let json = hyprctl(&["workspaces", "-j"])?;
    let all: serde_json::Value = serde_json::from_str(&json).ok()?;
    all.as_array()?
        .iter()
        .find(|w| w["id"].as_i64() == Some(id))?["tiledLayout"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn workspace_layout_mode(id: i64) -> Option<String> {
    let dir = omarchy_state_dir()?.join("workspace-layouts");
    let per = std::fs::read_to_string(dir.join(format!("{id}.lua"))).ok();
    let default = std::fs::read_to_string(dir.join("default.lua")).ok();
    layout_mode_from(per.as_deref(), default.as_deref(), tiled_layout_name(id).as_deref())
}

/// Put a workspace on dwindle so a tiled layout's preselect and split ratio
/// land. Omarchy 4.5's Super+L floating (and scrolling) mode floats or
/// scrolls every window, and a reload reapplies the mode from
/// `workspace-layouts/<id>.lua`, so the file is updated with the live mode.
/// A workspace Omarchy is not managing is left as it is.
pub fn ensure_dwindle(id: i64) -> bool {
    if id < 1 {
        return false;
    }
    let Some(mode) = workspace_layout_mode(id) else {
        return true;
    };
    if mode == "dwindle" {
        return true;
    }
    // A Lua call, not a dispatcher: `dispatch` wraps its argument in
    // `hl.dispatch(…)`, which refuses it. Omarchy's Super+L uses `eval` too.
    let lua = format!("o.workspace_mode({{ workspace = \"{id}\", mode = \"dwindle\" }})");
    if !hyprctl(&["eval", &lua]).is_some_and(|s| s.trim() == "ok") {
        return false;
    }
    if let Some(dir) = omarchy_state_dir() {
        let dir = dir.join("workspace-layouts");
        let file = dir.join(format!("{id}.lua"));
        // As Omarchy's toggle does: on the default, the workspace follows the
        // default again rather than keeping a file of its own.
        let default = std::fs::read_to_string(dir.join("default.lua")).ok();
        let general = hyprctl(&["getoption", "general.layout", "-j"])
            .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok())
            .and_then(|v| v["str"].as_str().map(str::to_string));
        if layout_mode_from(None, default.as_deref(), general.as_deref()).as_deref() == Some("dwindle") {
            let _ = std::fs::remove_file(file);
        } else if std::fs::create_dir_all(&dir).is_ok() {
            let _ = std::fs::write(file, format!("{lua}\n"));
        }
    }
    true
}

pub fn focus_empty_workspace() -> bool {
    // `emptym` mints the lowest free global id, which on Omarchy 4.5 is
    // another monitor's block. The caller already searched this block.
    if focused_monitor_block().is_some() {
        return false;
    }
    dispatch("hl.dsp.focus({ workspace = \"emptym\" })")
}

/// The lowest workspace id above `after` without windows (Patch opens on
/// clean workspaces, never beside the user's work). On Omarchy 4.5 that
/// search stays inside one monitor's block of ten.
pub fn first_empty_workspace(after: i64) -> Option<i64> {
    let json = hyprctl(&["workspaces", "-j"])?;
    let v: serde_json::Value = serde_json::from_str(&json).ok()?;
    let busy: Vec<i64> = v
        .as_array()?
        .iter()
        .filter(|w| w["windows"].as_u64().unwrap_or(0) > 0)
        .filter_map(|w| w["id"].as_i64())
        .collect();
    next_workspace_id(after, &busy, search_block(after))
}

pub fn active_workspace_id() -> Option<i64> {
    let json = hyprctl(&["activeworkspace", "-j"])?;
    serde_json::from_str::<serde_json::Value>(&json).ok()?["id"].as_i64()
}

pub fn focus_workspace(id: i64) -> bool {
    dispatch(&format!("hl.dsp.focus({{ workspace = \"{id}\" }})"))
}

/// Number of windows on the active workspace.
pub fn active_workspace_windows() -> usize {
    hyprctl(&["activeworkspace", "-j"])
        .and_then(|s| {
            let i = s.find("\"windows\":")?;
            s[i + 10..]
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

/// Is a window with this exact title mapped?
pub fn has_window(title: &str) -> bool {
    hyprctl(&["clients", "-j"])
        .is_some_and(|s| s.contains(&format!("\"title\": {}", lua_str(title))))
}

/// Workspace containing a mapped window with this exact title.
pub fn workspace_for_title(title: &str) -> Option<i64> {
    let json = hyprctl(&["clients", "-j"])?;
    let clients: serde_json::Value = serde_json::from_str(&json).ok()?;
    clients
        .as_array()?
        .iter()
        .find(|client| client["title"].as_str() == Some(title))?["workspace"]["id"]
        .as_i64()
}

/// Put a newly mapped layout window on its assigned workspace. Focusing a
/// workspace before mapping is not sufficient when another client changes
/// focus during the Wayland map handshake.
pub fn move_title_to_workspace(title: &str, workspace: i64) -> bool {
    dispatch(&format!(
        "hl.dsp.window.move({{ workspace = \"{workspace}\", window = {} }})",
        lua_str(&title_regex(title))
    ))
}

/// A mapped window of `class`, in logical px.
#[derive(Debug, Clone)]
pub struct Client {
    pub title: String,
    pub workspace: i64,
    pub monitor: i64,
    pub floating: bool,
    pub rect: [f32; 4],
}

/// Mapped, visible windows, of one class or (`None`) all.
pub fn clients(class: Option<&str>) -> Vec<Client> {
    let Some(json) = hyprctl(&["clients", "-j"]) else {
        return vec![];
    };
    let Ok(serde_json::Value::Array(all)) = serde_json::from_str(&json) else {
        return vec![];
    };
    let f = |v: &serde_json::Value, i: usize| v[i].as_f64().unwrap_or(0.0) as f32;
    all.iter()
        .filter(|c| class.is_none_or(|class| c["class"].as_str() == Some(class)))
        .filter(|c| c["mapped"].as_bool() != Some(false) && c["hidden"].as_bool() != Some(true))
        .map(|c| Client {
            title: c["title"].as_str().unwrap_or("").to_string(),
            workspace: c["workspace"]["id"].as_i64().unwrap_or(-1),
            monitor: c["monitor"].as_i64().unwrap_or(0),
            floating: c["floating"].as_bool().unwrap_or(false),
            rect: [
                f(&c["at"], 0),
                f(&c["at"], 1),
                f(&c["size"], 0),
                f(&c["size"], 1),
            ],
        })
        .collect()
}

/// Each monitor's usable area (without the bar), in logical px: (id, [x, y, w, h]).
pub fn monitor_areas() -> Vec<(i64, [f32; 4])> {
    let Some(json) = hyprctl(&["monitors", "-j"]) else {
        return vec![];
    };
    let Ok(serde_json::Value::Array(all)) = serde_json::from_str(&json) else {
        return vec![];
    };
    all.iter()
        .map(|m| {
            let n = |k: &str| m[k].as_f64().unwrap_or(0.0) as f32;
            let scale = n("scale").max(0.1);
            let r = |i: usize| m["reserved"][i].as_f64().unwrap_or(0.0) as f32;
            let (w, h) = if m["transform"].as_i64().unwrap_or(0) % 2 == 1 {
                (n("height"), n("width"))
            } else {
                (n("width"), n("height"))
            };
            (
                m["id"].as_i64().unwrap_or(0),
                [
                    n("x") + r(0),
                    n("y") + r(1),
                    w / scale - r(0) - r(2),
                    h / scale - r(1) - r(3),
                ],
            )
        })
        .collect()
}

/// The focused monitor's usable size (without the bar), in logical px:
/// where a layout opens.
pub fn focused_area() -> Option<(f32, f32)> {
    let json = hyprctl(&["monitors", "-j"])?;
    let all: Vec<serde_json::Value> = serde_json::from_str(&json).ok()?;
    let id = all.iter().find(|m| m["focused"] == true)?["id"].as_i64()?;
    monitor_areas()
        .into_iter()
        .find(|(m, _)| *m == id)
        .map(|(_, [_, _, w, h])| (w, h))
}

/// Float a window at a fraction of its monitor's usable area.
pub fn place(title: &str, [x, y, w, h]: [f32; 4]) -> bool {
    let Some(client) = clients(None).into_iter().find(|c| c.title == title) else {
        return false;
    };
    let Some((_, [ax, ay, aw, ah])) = monitor_areas()
        .into_iter()
        .find(|(id, _)| *id == client.monitor)
    else {
        return false;
    };
    let win = lua_str(&title_regex(title));
    (client.floating
        || dispatch(&format!(
            "hl.dsp.window.float({{ action = \"enable\", window = {win} }})"
        )))
        && dispatch(&format!(
            "hl.dsp.window.resize({{ x = {}, y = {}, window = {win} }})",
            (w * aw).round(),
            (h * ah).round()
        ))
        && dispatch(&format!(
            "hl.dsp.window.move({{ x = {}, y = {}, window = {win} }})",
            (ax + x * aw).round(),
            (ay + y * ah).round()
        ))
}

/// Rules for the transient surfaces. Patterns must match the whole string.
/// Re-registering identical rules on each
/// launch is harmless (a Lua guard would survive `hyprctl reload`, which
/// drops the rules themselves).
pub const RULES_LUA: &str = r#"
  local function patch_rule(match, rules)
    local class = match.class
    local seen = false
    if class and o ~= nil and o.floating_apps and o.floating_apps.class then
      for _, pattern in ipairs(o.floating_apps.class) do
        if pattern == class then seen = true break end
      end
    end
    -- o.window records the class, so a floating workspace's generic open
    -- rule (size and cascade) leaves these windows to the rules below.
    -- The class is recorded once; later calls only reinstall the rule,
    -- which a reload drops.
    if not seen and o ~= nil and type(o.window) == "function" then
      o.window(match, rules)
    else
      rules.match = match
      hl.window_rule(rules)
    end
  end
  patch_rule({ class = "omarchy\\.patch", title = "Patch · Controller" }, {
    float = true, center = true, pin = true, stay_focused = true, size = { 700, 480 },
  })
  patch_rule({ class = "omarchy\\.patch", title = "Patch · Performance" }, {
    float = true, center = true, size = { 1280, 820 },
  })
  patch_rule({ class = "omarchy\\.patch", title = "Patch · DJ Booth · Mini Booth" }, {
    float = true, center = true, size = { 1130, 240 },
  })
  patch_rule({ class = "omarchy\\.patch\\.plugin" }, {
    float = true, center = true,
  })
  patch_rule({ class = "omarchy\\.patch", title = "Patch · Opening layout" }, {
    float = true, pin = true, no_anim = true, no_focus = true, no_initial_focus = true,
    no_border = true, no_shadow = true, no_dim = true, no_blur = true,
    size = { "(monitor_w)", "(monitor_h)" }, move = { 0, 0 },
  })
"#;

/// Give a window the room it needs: float it at `w` × `h` (logical px),
/// centred on its monitor.
pub fn make_room(title: &str, w: u32, h: u32) -> bool {
    let win = lua_str(&title_regex(title));
    dispatch(&format!(
        "hl.dsp.window.float({{ action = \"enable\", window = {win} }})"
    )) && dispatch(&format!(
        "hl.dsp.window.resize({{ x = {w}, y = {h}, window = {win} }})"
    )) && dispatch(&format!("hl.dsp.window.center({{ window = {win} }})"))
}

/// A config option's value as Lua, e.g. `("animations", "enabled")` → `true`.
pub fn option(section: &str, key: &str) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_str(&hyprctl(&["getoption", &format!("{section}:{key}"), "-j"])?).ok()?;
    v.get("bool")
        .and_then(|b| b.as_bool())
        .map(|b| b.to_string())
        .or_else(|| v.get("int").and_then(|i| i.as_i64()).map(|i| i.to_string()))
}

/// Set a config option (a Lua value) for the running session only.
pub fn set_option(section: &str, key: &str, value: &str) -> bool {
    hyprctl(&[
        "eval",
        &format!("hl.config({{ {section} = {{ {key} = {value} }} }})"),
    ])
    .is_some_and(|s| s.trim() == "ok")
}

/// Move a window out of sight without asking its client (which may not be
/// answering): see-through, and off every monitor.
pub fn banish(title: &str) -> bool {
    let win = lua_str(&title_regex(title));
    dispatch(&format!(
        "hl.dsp.window.set_prop({{ window = {win}, prop = \"opacity\", value = \"0\" }})"
    )) | dispatch(&format!(
        "hl.dsp.window.move({{ x = -20000, y = -20000, window = {win} }})"
    ))
}

/// Make windows invisible and unanimated together, in one compositor call,
/// before they close. Closed one by one, each unmap re-tiled the rest: the
/// windows still open stretched over the freed room, again and again, until
/// the last one went.
pub fn vanish(titles: &[String]) -> bool {
    if titles.is_empty() {
        return true;
    }
    let lua: String = titles
        .iter()
        .map(|t| {
            let win = lua_str(&title_regex(t));
            format!(
                "hl.dispatch(hl.dsp.window.set_prop({{ window = {win}, prop = \"no_anim\", value = \"1\" }})) \
                 hl.dispatch(hl.dsp.window.set_prop({{ window = {win}, prop = \"opacity\", value = \"0\" }})) "
            )
        })
        .collect();
    hyprctl(&["eval", &lua]).is_some_and(|s| s.trim() == "ok")
}

pub fn install_rules() {
    if available() {
        hyprctl(&["eval", RULES_LUA]);
        hyprctl(&["eval", &border_rule(quiet_border(patch_core::setup::load().focus_border))]);
        // 0412e15 stopped focus following the pointer by mistake (only the
        // border was meant): that rule, where a session still holds it, off.
        hyprctl(&["eval", OLD_HOVER_RULE_OFF]);
    }
}

/// Keep Patch's rules through Hyprland reloads. A reload (an Omarchy theme
/// switch — 4.5 still runs `hyprctl reload`, only sooner — or a config save)
/// drops every rule set at run time, and rebuilds `o.floating_apps` from the
/// config: the open windows lost the quiet focused border until Patch opened
/// another window. A background thread waits for Hyprland's `configreloaded`
/// event and sets the rules, and the open windows' border, again. Once per
/// process. Animations may be off session-wide; the curtain's `no_anim` rule
/// is part of what is put back.
pub fn keep_rules_through_reloads() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        let Some(socket) = event_socket().filter(|_| available()) else {
            return;
        };
        let _ = std::thread::Builder::new()
            .name("patch-hypr-reload".into())
            .spawn(move || {
                use std::io::BufRead;
                loop {
                    if let Ok(s) = std::os::unix::net::UnixStream::connect(&socket) {
                        for line in std::io::BufReader::new(s).lines().map_while(Result::ok) {
                            if line.starts_with("configreloaded>>") {
                                install_rules();
                                set_focus_border(patch_core::setup::load().focus_border);
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            });
    });
}

/// `hyprctl <args>` as its socket request: `j/` for `-j` (JSON), the
/// words joined by spaces after it (`j/clients`, `/eval <lua>`).
fn request_line(args: &[&str]) -> String {
    let json = args.contains(&"-j");
    let words: Vec<&str> = args.iter().copied().filter(|a| *a != "-j").collect();
    format!("{}/{}", if json { "j" } else { "" }, words.join(" "))
}

/// One request over Hyprland's request socket: `None` when the socket
/// can't be reached (the caller falls back to the `hyprctl` process), else
/// the reply, `Some(None)` if it failed. Once connected it is never sent
/// again: a dispatch whose reply timed out may already have run.
fn request(args: &[&str]) -> Option<Option<String>> {
    use std::io::{Read, Write};
    let path = event_socket()?.with_file_name(".socket.sock");
    let mut socket = std::os::unix::net::UnixStream::connect(path).ok()?;
    let limit = Some(std::time::Duration::from_secs(2));
    let reply = (|| {
        socket.set_read_timeout(limit).ok()?;
        socket.set_write_timeout(limit).ok()?;
        socket.write_all(request_line(args).as_bytes()).ok()?;
        let mut reply = String::new();
        socket.read_to_string(&mut reply).ok()?;
        Some(reply)
    })();
    Some(reply)
}

/// Hyprland's event socket (one line per event).
fn event_socket() -> Option<std::path::PathBuf> {
    let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let runtime = std::env::var("XDG_RUNTIME_DIR").ok()?;
    Some(std::path::PathBuf::from(runtime).join("hypr").join(sig).join(".socket2.sock"))
}

const OLD_HOVER_RULE_OFF: &str = r#"hl.window_rule({ name = "patch-no-hover-focus", enabled = false, match = { class = "omarchy\\.patch" }, no_follow_mouse = true })"#;

/// Hyprland's `col.*_border` option (`gradient data: aarrggbb … 45deg`) as a
/// rule value: its colours (`rgba(rrggbbaa) …`).
fn border_option(key: &str) -> Option<String> {
    let out = hyprctl(&["getoption", &format!("general:col.{key}")])?;
    gradient_colours(out.lines().find_map(|l| l.trim().strip_prefix("gradient data:"))?)
}

/// The colours of a gradient as Hyprland prints it. Its angle stays out:
/// a window's border with one (`rgba(…) 0deg`) was read as no colour at
/// all, so a focused Patch window lost its line while the others kept
/// theirs.
fn gradient_colours(data: &str) -> Option<String> {
    let parts: Vec<String> = data
        .split_whitespace()
        .filter(|p| p.len() == 8 && p.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|p| format!("rgba({}{})", &p[2..], &p[..2]))
        .collect();
    (!parts.is_empty()).then(|| parts.join(" "))
}

/// One of Patch's own Omarchy themes is on (Patch Dark, Patch Light).
pub fn patch_theme() -> bool {
    let Some(home) = std::env::var_os("HOME") else { return false };
    std::fs::read_to_string(std::path::Path::new(&home).join(".local/state/omarchy/current/theme.name"))
        .is_ok_and(|name| matches!(name.trim(), "patch-dark" | "patch-light"))
}

/// Focused Patch windows keep the inactive border colour: only in Patch's
/// own themes, and only while the "Highlight focused window" setting is
/// off. Every other theme draws them as Omarchy draws any window.
fn quiet_border(focus_border: bool) -> bool {
    !focus_border && patch_theme()
}

/// The rule for Patch windows to come (`quiet`: see [`quiet_border`]). A
/// named rule: switching the setting or the theme redefines it.
fn border_rule(quiet: bool) -> String {
    let colour = border_option("inactive_border").unwrap_or_else(|| "rgba(595959aa)".into());
    format!(
        r#"hl.window_rule({{ name = "patch-quiet-border", enabled = {}, match = {{ class = "omarchy\\.patch" }}, border_color = {} }})"#,
        quiet,
        lua_str(&colour)
    )
}

/// Apply the "Highlight focused window" setting (and the theme) now: the
/// rule for windows to come, and the active border colour of every Patch
/// window open.
pub fn set_focus_border(on: bool) {
    if !available() {
        return;
    }
    let quiet = quiet_border(on);
    hyprctl(&["eval", &border_rule(quiet)]);
    hyprctl(&["eval", OLD_HOVER_RULE_OFF]);
    let key = if quiet { "inactive_border" } else { "active_border" };
    let colour = border_option(key).unwrap_or_else(|| "rgba(595959aa)".into());
    let Some(list) = hyprctl(&["clients", "-j"]) else {
        return;
    };
    let clients: Vec<serde_json::Value> = serde_json::from_str(&list).unwrap_or_default();
    for c in clients.iter().filter(|c| c["class"] == "omarchy.patch") {
        if let Some(addr) = c["address"].as_str() {
            let win = lua_str(&format!("address:{addr}"));
            dispatch(&format!(
                "hl.dsp.window.set_prop({{ window = {win}, prop = \"active_border_color\", value = {} }})",
                lua_str(&colour)
            ));
            // Focus follows the pointer again where 0412e15 stopped it.
            dispatch(&format!(
                "hl.dsp.window.set_prop({{ window = {win}, prop = \"no_follow_mouse\", value = \"0\" }})"
            ));
        }
    }
}

/// Titles of the windows on a visible workspace (each monitor's active one
/// and any special workspace shown on it), kept current from Hyprland's
/// event socket by a background thread (and every 5 s, in case an event
/// was missed). `None` without Hyprland: callers then use their own guess.
pub fn visible_titles()
-> Option<std::sync::Arc<std::sync::RwLock<std::collections::HashSet<String>>>> {
    use std::io::BufRead;
    use std::sync::{Arc, RwLock};
    static SEEN: std::sync::OnceLock<Option<Arc<RwLock<std::collections::HashSet<String>>>>> =
        std::sync::OnceLock::new();
    SEEN.get_or_init(|| {
        if !available() {
            return None;
        }
        let socket = event_socket()?;
        let set = Arc::new(RwLock::new(visible_now()));
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        // Events: a line each; only those that change what's on screen.
        std::thread::Builder::new()
            .name("patch-sight-events".into())
            .spawn(move || {
                loop {
                    if let Ok(s) = std::os::unix::net::UnixStream::connect(&socket) {
                        for line in std::io::BufReader::new(s).lines().map_while(Result::ok) {
                            let ev = line.split(">>").next().unwrap_or("");
                            if matches!(
                                ev,
                                "workspace"
                                    | "workspacev2"
                                    | "focusedmon"
                                    | "focusedmonv2"
                                    | "activespecial"
                                    | "activespecialv2"
                                    | "movewindow"
                                    | "movewindowv2"
                                    | "openwindow"
                                    | "closewindow"
                                    | "windowtitle"
                                    | "windowtitlev2"
                                    | "moveworkspace"
                                    | "moveworkspacev2"
                                    | "monitoradded"
                                    | "monitorremoved"
                            ) && tx.send(()).is_err()
                            {
                                return;
                            }
                        }
                    }
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
            })
            .ok()?;
        let shared = set.clone();
        std::thread::Builder::new()
            .name("patch-sight".into())
            .spawn(move || {
                loop {
                    // A burst of events is one query, a moment after it.
                    if rx.recv_timeout(std::time::Duration::from_secs(5)).is_ok() {
                        std::thread::sleep(std::time::Duration::from_millis(60));
                        while rx.try_recv().is_ok() {}
                    }
                    let now = visible_now();
                    if let Ok(mut s) = shared.write() {
                        *s = now;
                    }
                }
            })
            .ok()?;
        Some(set)
    })
    .clone()
}

fn visible_now() -> std::collections::HashSet<String> {
    let mut shown = std::collections::HashSet::new();
    if let Some(json) = hyprctl(&["monitors", "-j"])
        && let Ok(serde_json::Value::Array(ms)) = serde_json::from_str(&json)
    {
        for m in &ms {
            for key in ["activeWorkspace", "specialWorkspace"] {
                if let Some(id) = m[key]["id"].as_i64().filter(|id| *id != 0) {
                    shown.insert(id);
                }
            }
        }
    }
    clients(None)
        .into_iter()
        .filter(|c| shown.contains(&c.workspace))
        .map(|c| c.title)
        .collect()
}

#[cfg(test)]
mod border_tests {
    #[test]
    fn the_border_rules_are_valid_lua() {
        // Lua has no `\.` escape: the regex dot must reach it as `\\.`.
        for r in [super::border_rule(false), super::OLD_HOVER_RULE_OFF.to_string()] {
            assert!(r.contains(r#"class = "omarchy\\.patch""#), "{r}");
        }
        assert!(super::border_rule(true).contains("enabled = true"));
        assert!(super::border_rule(false).contains("enabled = false"));
        assert!(super::OLD_HOVER_RULE_OFF.contains("enabled = false"));
    }

    #[test]
    fn border_colours_leave_the_angle_out() {
        assert_eq!(super::gradient_colours("ff89b4fa 0deg").as_deref(), Some("rgba(89b4faff)"));
        assert_eq!(
            super::gradient_colours("fff0243b ff9d7cf0 45deg").as_deref(),
            Some("rgba(f0243bff) rgba(9d7cf0ff)")
        );
        assert_eq!(super::gradient_colours("0deg"), None);
    }
}

#[cfg(test)]
mod tests {
    use super::request_line;

    #[test]
    fn a_monitor_block_stays_on_that_monitor() {
        let state = "not a row\n0\tDP-1\tDell U2720Q\n1\teDP-1\tLaptop\n";
        assert_eq!(super::block_for_connector(state, "eDP-1"), Some(1));
        assert_eq!(super::block_for_connector(state, "DP-1"), Some(0));
        assert_eq!(super::block_for_connector(state, "HDMI-A-1"), None);
        assert_eq!(super::block_for_connector(state, ""), None);
        // Block 1 is workspaces 11–20. A layout that started at 12 continues
        // there, and does not wrap back onto a busy earlier slot or the
        // other monitor's 1–10.
        let busy = [11, 12, 14];
        assert_eq!(super::next_workspace_id(0, &busy, Some(1)), Some(13));
        assert_eq!(super::next_workspace_id(12, &busy, Some(1)), Some(13));
        assert_eq!(super::next_workspace_id(13, &busy, Some(1)), Some(15));
        assert_eq!(super::next_workspace_id(20, &[], Some(1)), None);
        assert_eq!(super::next_workspace_id(0, &[1, 2], Some(0)), Some(3));
        assert_eq!(super::next_workspace_id(5, &[6], None), Some(7));
    }

    #[test]
    fn workspace_layout_mode_follows_omarchy_files() {
        let floating = "o.workspace_mode({ workspace = \"4\", mode = \"floating\" })\n";
        let scrolling = "o.workspace_mode({ default = \"scrolling\" })\n";
        assert_eq!(
            super::layout_mode_from(Some(floating), Some(scrolling), Some("dwindle")).as_deref(),
            Some("floating")
        );
        assert_eq!(
            super::layout_mode_from(Some("-- old file\n"), Some(scrolling), Some("dwindle")).as_deref(),
            Some("scrolling")
        );
        assert_eq!(
            super::layout_mode_from(None, None, Some("scrolling")).as_deref(),
            Some("scrolling")
        );
        assert_eq!(
            super::layout_mode_from(Some("-- old file\n"), None, Some("scrolling")).as_deref(),
            Some("scrolling")
        );
        assert_eq!(super::layout_mode_from(None, None, None), None);
        assert_eq!(super::quoted_assignment(floating, "mode"), Some("floating"));
    }

    #[test]
    fn transient_rules_opt_out_of_a_floating_workspace() {
        assert!(super::RULES_LUA.contains("o.window"));
        assert!(super::RULES_LUA.contains("no_anim = true"));
        assert!(super::RULES_LUA.contains(r#"class = "omarchy\\.patch""#));
        assert!(super::RULES_LUA.contains(r#"class = "omarchy\\.patch\\.plugin""#));
    }

    /// The socket takes what `hyprctl` sends: flags before the slash.
    #[test]
    fn requests_read_like_hyprctl_sends_them() {
        assert_eq!(request_line(&["clients", "-j"]), "j/clients");
        assert_eq!(request_line(&["getoption", "cursor:no_warps", "-j"]), "j/getoption cursor:no_warps");
        assert_eq!(request_line(&["dispatch", "hl.dsp.focus({ workspace = 2 })"]), "/dispatch hl.dsp.focus({ workspace = 2 })");
        assert_eq!(request_line(&["eval", "a\nb"]), "/eval a\nb");
    }
}
