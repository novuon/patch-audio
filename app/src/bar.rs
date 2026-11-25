//! `omarchy-patch bar`: Patch in the Omarchy bar's audio dropdown.
//!
//! `install` makes the plugin `patch.audio` from the audio widget the bar
//! shows now (`omarchy.audio` or a clone of it, with the user's changes),
//! adds the Patch section (integration/omarchy-shell/PatchSection.qml) and
//! puts it in that widget's place. `uninstall` puts the old widget back.
//! The plugin stays a clone of `omarchy.audio`, so it keeps the built-in
//! widget's media and OSD hooks. `status` is what the section polls.
//!
//! The plugin also takes the volume keys (VolumeKeys.qml): Omarchy's shell
//! steps the volume in place only for a hardware output, so with Patch's
//! sink as the default each press ran a script of ~15 processes. The
//! widget steps it in the shell instead, through Hyprland binds it sets
//! for the session; `install` reads the user's steps from hypr/*.lua.
//!
//! `health on` adds a second, optional widget, `patch.health`
//! (integration/omarchy-shell/health): a dot for Patch's audio buffers,
//! left of the audio widget. `install` ships it without placing it, so it
//! is also in the shell's own widget list.

use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const ID: &str = "patch.audio";
const SOURCE: &str = "omarchy.audio";
const SECTION: &str = include_str!("../../integration/omarchy-shell/PatchSection.qml");
const DRAG_SLIDER: &str = include_str!("../../integration/omarchy-shell/DragPanelSlider.qml");
const MARK: &str = include_str!("../../integration/omarchy-shell/PatchMark.qml");
const CLOSE_HELPER: &str = include_str!("../../integration/omarchy-shell/close-patch-windows.py");
const HEALTH_ID: &str = "patch.health";
const HEALTH_MANIFEST: &str = include_str!("../../integration/omarchy-shell/health/manifest.json");
const HEALTH_QML: &str = include_str!("../../integration/omarchy-shell/health/Health.qml");
const VOLUME_KEYS_QML: &str = include_str!("../../integration/omarchy-shell/VolumeKeys.qml");
// The Patch section goes above the output list.
const ANCHOR: &str = "// ---- Output devices ----";

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
}

fn plugins() -> PathBuf {
    config().join("omarchy/plugins")
}

fn shell_json() -> PathBuf {
    config().join("omarchy/shell.json")
}

/// Whether this desktop has the Omarchy shell's bar (its settings file):
/// the generic kit's tray stands in for it elsewhere.
#[cfg(feature = "generic")]
pub fn has_shell() -> bool {
    shell_json().exists()
}

fn state_file() -> PathBuf {
    config().join("omarchy-patch/bar.json")
}

fn read_json(p: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// Where a plugin's files are: the user's plugins, then the built-in ones.
fn plugin_dir(id: &str) -> Option<PathBuf> {
    let user = plugins().join(id);
    if user.join("manifest.json").is_file() {
        return Some(user);
    }
    let base = std::env::var_os("OMARCHY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/share/omarchy".into());
    for kind in std::fs::read_dir(base.join("shell/plugins"))
        .ok()?
        .flatten()
    {
        for dir in std::fs::read_dir(kind.path())
            .into_iter()
            .flatten()
            .flatten()
        {
            if read_json(&dir.path().join("manifest.json")).is_some_and(|m| m["id"] == id) {
                return Some(dir.path());
            }
        }
    }
    None
}

/// The widget ids in the bar, in order.
fn bar_ids(shell: &Value) -> Vec<String> {
    let layout = &shell["bar"]["layout"];
    ["left", "center", "right"]
        .iter()
        .flat_map(|s| layout[*s].as_array().into_iter().flatten())
        .filter_map(|w| w["id"].as_str().map(str::to_string))
        .collect()
}

fn is_audio(id: &str) -> bool {
    id == SOURCE
        || plugin_dir(id)
            .and_then(|d| read_json(&d.join("manifest.json")))
            .is_some_and(|m| m["omarchy"]["clonedFrom"] == SOURCE)
}

/// Replace the widget `from` by `to` in shell.json, as text, so the file
/// keeps its layout and everything else in it stays byte for byte.
fn swap_widget(text: &str, from: &str, to: &str) -> anyhow::Result<String> {
    let needle = format!("\"{from}\"");
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut n = 0;
    while let Some(i) = rest.find(&needle) {
        let before = rest[..i].trim_end();
        let is_id = before
            .strip_suffix(':')
            .is_some_and(|b| b.trim_end().ends_with("\"id\""));
        out.push_str(&rest[..i]);
        let id = if is_id {
            n += 1;
            to
        } else {
            from
        };
        out.push_str(&format!("\"{id}\""));
        rest = &rest[i + needle.len()..];
    }
    out.push_str(rest);
    if n == 0 {
        bail!("'{from}' is not in the bar");
    }
    Ok(out)
}

/// A directory's files by name, with their contents.
fn dir_files(dir: &Path) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((e.file_name(), std::fs::read(e.path()).ok()?)))
        .collect()
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if src.is_dir() {
            copy_dir(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn shell(args: &[&str]) {
    let _ = std::process::Command::new("omarchy-shell")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// How omarchy-patch was started, symlinks kept (~/.local/bin survives a
/// rebuild in another place; the shell's PATH may not have it).
pub fn invoked() -> Option<PathBuf> {
    let arg0 = PathBuf::from(std::env::args_os().next()?);
    if arg0.components().count() > 1 {
        return std::path::absolute(arg0).ok();
    }
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|d| d.join(&arg0))
        .find(|p| p.is_file())
}

struct StagedPlugin(PathBuf);

impl Drop for StagedPlugin {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Longest needle that occurs exactly once. A 4.4 needle that is only a
/// piece of a 4.5 line must not win over the 4.5 line.
fn apply_panel_edit(panel: &str, options: &[(&str, &str)]) -> Option<String> {
    let mut best: Option<(&str, &str)> = None;
    for &(old, new) in options {
        if !old.is_empty()
            && panel.matches(old).count() == 1
            && best.is_none_or(|(prev, _)| old.len() > prev.len())
        {
            best = Some((old, new));
        }
    }
    best.map(|(old, new)| panel.replace(old, new))
}

/// Patch the audio panel. Omarchy 4.4 and 4.5 (the panel from #7783, #16470
/// and the platform-hidden nodes) both have to match: 4.5 keeps
/// `platformHides` and measures height from `panel.loadedContentHeight`.
fn patch_audio_panel(panel: &str, from: &str) -> anyhow::Result<String> {
    let output_switch_old = "Pipewire.preferredDefaultAudioSink = node\n    if (node.id !== undefined && node.name) {\n      Quickshell.execDetached([\n        \"omarchy-audio-output-set-default\",\n        String(node.id),\n        String(node.name)\n      ])\n    }";
    let output_switch_new = "if (!node || !node.name) return\n    var name = String(node.name)\n    chosenOutput = name\n    Quickshell.execDetached([\"@PATCH_BIN@\", \"output\", \"set-default\", name])";
    let input_switch_old = "Pipewire.preferredDefaultAudioSource = node\n    if (node.id !== undefined && node.name) {\n      Quickshell.execDetached([\n        \"omarchy-audio-input-set-default\",\n        String(node.id),\n        String(node.name)\n      ])\n    }";
    let input_switch_new = "if (!node || !node.name) return\n    var name = String(node.name)\n    chosenInput = name\n    Quickshell.execDetached([\"@PATCH_BIN@\", \"input\", \"set-default\", name])";
    // 4.5 guards the assignment: `if (node.audio) Pipewire.preferred…`.
    // The longer needle has to win, or the guard is left in front of the
    // replacement and an untyped input can no longer be selected.
    let input_switch_rc2 = format!("if (node.audio) {input_switch_old}");
    let edits: Vec<(&str, bool, Vec<(&str, &str)>)> = vec![
        (
            "selected output",
            true,
            vec![(
                "readonly property var sink: Pipewire.defaultAudioSink",
                "// What the engine plays to and records from. These live on the panel root:\n  // the Patch section sits in the menu content, a separate component, so its\n  // id does not exist out here (a ReferenceError on every binding, and a row\n  // click that did nothing). A click writes the name at once; the runtime\n  // files are what the engine applied.\n  property string chosenOutput: \"\"\n  property string chosenInput: \"\"\n\n  FileView {\n    path: (Quickshell.env(\"XDG_RUNTIME_DIR\") || \"\") + \"/omarchy-patch/output-selection\"\n    watchChanges: true\n    printErrors: false\n    onLoaded: root.chosenOutput = String(text() || \"\").trim()\n    onFileChanged: reload()\n  }\n  FileView {\n    path: (Quickshell.env(\"XDG_RUNTIME_DIR\") || \"\") + \"/omarchy-patch/input-selection\"\n    watchChanges: true\n    printErrors: false\n    onLoaded: root.chosenInput = String(text() || \"\").trim()\n    onFileChanged: reload()\n  }\n\n  function nodeNamed(name, wantSink) {\n    if (!name) return null\n    var all = root.nodes || []\n    for (var i = 0; i < all.length; i++) {\n      var n = all[i]\n      if (!n || n.isStream || String(n.name || \"\") !== name) continue\n      if (wantSink && !n.isSink) continue\n      return n\n    }\n    return null\n  }\n\n  // While Patch owns the output the default sink is its bus: the row and the\n  // icon follow the device the engine is driving.\n  readonly property var sink: nodeNamed(chosenOutput, true) || Pipewire.defaultAudioSink",
            )],
        ),
        (
            "outputs",
            true,
            vec![
                (
                    "if (n && n.isSink && !n.isStream) list.push(n)",
                    "if (n && n.isSink && !n.isStream && (String(n.name || \"\") === \"patch\" || String(n.name || \"\").indexOf(\"patch\") !== 0)) list.push(n)",
                ),
                (
                    "      if (!n || !n.isSink || n.isStream || platformHides(n)) continue\n      list.push(n)",
                    "      if (!n || !n.isSink || n.isStream || platformHides(n)) continue\n      var patchName = String(n.name || \"\")\n      if (patchName.indexOf(\"patch\") === 0 && patchName !== \"patch\") continue\n      list.push(n)",
                ),
            ],
        ),
        (
            "inputs",
            true,
            vec![
                (
                    "if (name === \"quickshell\") continue",
                    "if (name === \"quickshell\" || (name.indexOf(\"patch\") === 0 && name !== \"patch.input\") || name.indexOf(\"loopback-\") !== -1) continue",
                ),
                (
                    "if (name === \"quickshell\" || platformHides(n)) continue",
                    "if (name === \"quickshell\" || platformHides(n) || (name.indexOf(\"patch\") === 0 && name !== \"patch.input\") || name.indexOf(\"loopback-\") !== -1) continue",
                ),
            ],
        ),
        (
            "streams",
            false,
            vec![(
                "if (String(n.name || \"\").indexOf(\"omarchy_speaker_tuning\") === 0) continue",
                "if (String(n.name || \"\").indexOf(\"omarchy_speaker_tuning\") === 0) continue\n      // Patch's plumbing is not an application: the loopback's capture side\n      // is named `input.loopback-<pid>-<n>` by PipeWire, so match the marker\n      // anywhere in the node's identifying strings, not just at the front.\n      var _id = (String(n.name || \"\") + \" \" + String(n.description || \"\") + \" \"\n                 + String(n.properties ? (n.properties[\"media.name\"] || \"\") : \"\")).toLowerCase()\n      if (_id.indexOf(\"patch\") === 0 || _id.indexOf(\"loopback-\") !== -1) continue",
            )],
        ),
        (
            "output switch",
            true,
            vec![(output_switch_old, output_switch_new)],
        ),
        (
            "listening volume",
            true,
            vec![(
                "readonly property var volumeSink: {",
                "readonly property var volumeSink: {\n    var bus = Pipewire.defaultAudioSink\n    if (bus && String(bus.name || \"\") === \"patch\") return bus",
            )],
        ),
        (
            "selected input",
            true,
            vec![(
                "readonly property var source: Pipewire.defaultAudioSource",
                "readonly property var captureSource: Pipewire.defaultAudioSource\n  readonly property var source: {\n    var want = chosenInput\n    if (want && want !== \"patch.input\") {\n      var found = nodeNamed(want, false)\n      if (found) return found\n    }\n    return captureSource\n  }",
            )],
        ),
        (
            "input switch",
            true,
            vec![
                (input_switch_old, input_switch_new),
                (input_switch_rc2.as_str(), input_switch_new),
            ],
        ),
        (
            "output row",
            false,
            vec![(
                "readonly property bool isActive: root.sink && node && root.sink.id === node.id",
                "readonly property bool isActive: !!(node && node.name && String(node.name) === (root.chosenOutput || (root.sink ? String(root.sink.name || \"\") : \"\")))",
            )],
        ),
        (
            "input row",
            false,
            vec![(
                "readonly property bool isActive: root.source && node && root.source.id === node.id",
                "readonly property bool isActive: !!(node && node.name && String(node.name) === (root.chosenInput || (root.source ? String(root.source.name || \"\") : \"\")))",
            )],
        ),
        (
            // A dot on the icon while Patch records or broadcasts.
            "recording mark",
            false,
            vec![(
                "  BarIconButton {\n    id: button\n",
                "  BarIconButton {\n    id: button\n    PatchMark { anchors.fill: parent }\n",
            )],
        ),
        (
            "height",
            false,
            vec![
                (
                    "fittedContentHeight(panelColumn.implicitHeight, Style.space(560))",
                    "fittedContentHeight(panelColumn.implicitHeight, Style.space(820))",
                ),
                (
                    "fittedContentHeight(panel.loadedContentHeight, Style.space(560))",
                    "fittedContentHeight(panel.loadedContentHeight, Style.space(820))",
                ),
            ],
        ),
    ];
    let mut panel = panel.to_string();
    for (what, required, options) in &edits {
        if let Some(next) = apply_panel_edit(&panel, options) {
            panel = next;
        } else if *required {
            bail!("'{from}' has changed: cannot safely install Patch's {what} integration");
        } else {
            eprintln!("note: '{from}' has changed; Patch's {what} tweak is skipped");
        }
    }
    // Volume and mute belong to the capture mix, while the label names its device.
    Ok(patch_menu_scrolling(&panel)
        .replace("source.audio", "captureSource.audio")
        // `source` can be the chosen device while the capture mix is not there
        // yet, as when the shell starts: test the object that is read.
        .replace("source && captureSource.audio", "captureSource && captureSource.audio")
        .replace(" PanelSlider {", " DragPanelSlider {"))
}

/// Mouse hover follows the pointer; only keyboard navigation scrolls a row
/// into view. Patch adds sections above Output, so its slider is not at y=0.
fn patch_menu_scrolling(panel: &str) -> String {
    if !panel.contains("function ensureCursorVisible(item) {")
        || panel.contains("property bool patchKeyboardNavigation:")
    {
        return panel.to_string();
    }
    let edits = [
        ("function ensureCursorVisible(item) {", "readonly property real patchMenuHeightCap: {\n    var available = panel.screen ? panel.screen.height - Style.space(80) : Style.space(820)\n    if (typeof panel.availableCardHeight === \"number\" && panel.availableCardHeight > 0)\n      available = panel.availableCardHeight\n    return Math.max(1, Math.min(Style.space(820), available))\n  }\n\n  property bool patchKeyboardNavigation: false\n\n  function ensureCursorVisible(item) {\n    if (!root.patchKeyboardNavigation) return"),
        ("onMoveRequested: function(dx, dy) {", "onMoveRequested: function(dx, dy) {\n        root.patchKeyboardNavigation = true"),
        ("function moveSection(delta) {", "function moveSection(delta) {\n    root.patchKeyboardNavigation = true"),
        ("function setHeaderCursor() {", "function setHeaderCursor() {\n    root.patchKeyboardNavigation = false"),
        ("onOpenedChanged: {", "onOpenedChanged: {\n    root.patchKeyboardNavigation = false"),
        ("onHoveredChanged: if (hovered) {", "onHoveredChanged: if (hovered) {\n                  root.patchKeyboardNavigation = false"),
        ("onContainsMouseChanged: if (containsMouse) {", "onContainsMouseChanged: if (containsMouse) {\n        root.patchKeyboardNavigation = false"),
        ("if (maxY <= Style.space(24) || (root.focusSection === \"output\" && root.selectedIndex === -1)) {", "if (maxY <= 0) {"),
        ("contentWidth: panel.fittedContentWidth(Style.space(380))", "contentWidth: panel.fittedContentWidth(Style.space(560))"),
        ("fittedContentHeight(panelColumn.implicitHeight, Style.space(820))", "fittedContentHeight(panelColumn.implicitHeight, root.patchMenuHeightCap)"),
        ("fittedContentHeight(panel.loadedContentHeight, Style.space(820))", "fittedContentHeight(panel.loadedContentHeight, root.patchMenuHeightCap)"),
    ];
    edits.iter().fold(panel.to_string(), |text, (old, new)| text.replace(old, new))
}

pub fn install() -> anyhow::Result<()> {
    install_as(false)
}

/// After a release update (`engine upgrade`): the widget again from this
/// build, when the bar shows it and it changed. A reinstall over the old
/// one ran `install`, but an update by `engine upgrade` kept the widget of
/// the build that installed it (the volume keys never came). An unchanged
/// widget is left alone: nothing rewritten, no shell reload.
pub fn refresh() -> anyhow::Result<()> {
    if !plugins().join(ID).exists() || !state_file().exists() {
        return Ok(());
    }
    install_as(true)
}

/// The omarchy-patch an installed widget runs (`~/.local/bin`'s link, not
/// the release the update runs from: that one is pruned later).
fn installed_bin(dir: &Path) -> Option<PathBuf> {
    let qml = std::fs::read_to_string(dir.join("PatchSection.qml")).ok()?;
    let rest = qml.split_once("readonly property string bin: \"")?.1;
    Some(PathBuf::from(rest.split_once('"')?.0))
}

fn install_as(refresh: bool) -> anyhow::Result<()> {
    let path = shell_json();
    let text =
        std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let cfg: Value =
        serde_json::from_str(&text).with_context(|| format!("{} is not JSON", path.display()))?;
    let ids = bar_ids(&cfg);
    // Taken out of the bar since: it stays out.
    if refresh && !ids.iter().any(|i| i == ID) {
        return Ok(());
    }
    let saved = read_json(&state_file());
    // The widget Patch takes the place of (again, on a reinstall).
    let from = if ids.iter().any(|i| i == ID) {
        saved
            .as_ref()
            .and_then(|s| s["replaced"].as_str().map(str::to_string))
            .unwrap_or_else(|| SOURCE.into())
    } else {
        match ids.iter().find(|i| is_audio(i)) {
            Some(i) => i.clone(),
            None => bail!("the bar has no audio widget to replace: add Audio to the bar first"),
        }
    };
    let src = plugin_dir(&from).with_context(|| format!("can't find the plugin '{from}'"))?;

    // The plugin: the old widget's files, a new manifest, the Patch section.
    let installed_dir = plugins().join(ID);
    let dir = plugins().join(format!(".patch-audio-stage-{}", std::process::id()));
    if dir.exists() {
        bail!("a Patch plugin installation is already staged at {}", dir.display());
    }
    let _staged = StagedPlugin(dir.clone());
    copy_dir(&src, &dir)?;
    let mut manifest =
        read_json(&src.join("manifest.json")).context("the source has no manifest")?;
    manifest["id"] = json!(ID);
    manifest["name"] = json!("Patch Audio");
    manifest["author"] = json!("Patch");
    manifest["description"] =
        json!("Volume, outputs and apps, with Patch: its service, the EQ and the layouts");
    manifest["barWidget"]["displayName"] = json!("Patch Audio");
    manifest["barWidget"]["description"] = manifest["description"].clone();
    manifest["omarchy"] = json!({"clonedFrom": SOURCE});
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )?;
    let exe = match refresh.then(|| installed_bin(&installed_dir)).flatten() {
        Some(bin) => bin,
        None => invoked().unwrap_or(std::env::current_exe()?),
    };
    std::fs::write(
        dir.join("PatchSection.qml"),
        SECTION
            .replace("@PATCH_BIN@", &exe.to_string_lossy())
            .replace(
                "@PATCH_CLOSE@",
                &installed_dir.join("close-patch-windows.py").to_string_lossy(),
            ),
    )?;
    std::fs::write(dir.join("DragPanelSlider.qml"), DRAG_SLIDER)?;
    std::fs::write(dir.join("PatchMark.qml"), MARK)?;
    let close_helper = dir.join("close-patch-windows.py");
    std::fs::write(&close_helper, CLOSE_HELPER)?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&close_helper, std::fs::Permissions::from_mode(0o755))?;
    let panel_path = dir.join(
        manifest["entryPoints"]["barWidget"]
            .as_str()
            .unwrap_or("Panel.qml"),
    );
    let panel = std::fs::read_to_string(&panel_path)?;
    let Some(at) = panel.find(ANCHOR) else {
        std::fs::remove_dir_all(&dir)?;
        bail!("'{from}' has changed: its panel has no '{ANCHOR}' to put Patch above");
    };
    let line = panel[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent = &panel[line..at];
    let section = [
        "PatchSection {",
        "  id: patchSection",
        "  width: parent.width",
        "  bar: root.bar",
        "  active: root.opened",
        "  keys: keyCatcher",
        "  onLaunched: root.close()",
        "}",
        "",
        "",
    ]
    .iter()
    .map(|l| {
        if l.is_empty() {
            String::new()
        } else {
            format!("{indent}{l}")
        }
    })
    .collect::<Vec<_>>()
    .join("\n");
    let panel = format!("{}{section}{}", &panel[..line], &panel[line..]);
    // Expose the public Patch bus and capture source, keeping internal deck,
    // app and system-sounds plumbing out of the device lists. The selected
    // destination and the bus's listening-volume control are separate.
    let mut panel = patch_audio_panel(&panel, &from)?;
    // The volume keys, stepped in the shell: at the widget's top level, so
    // they work with the dropdown closed. Named by its content: the shell
    // keeps a QML type it has loaded through a plugin reload, so a changed
    // file under the same name would only count after a shell restart.
    let keys = VOLUME_KEYS_QML.replace("@BIND_LUA@", &serde_json::to_string(&volume_keys_lua(&hypr_config()))?);
    let keys_type = format!("VolumeKeys_{:08x}", fnv1a(keys.as_bytes()) as u32);
    std::fs::write(dir.join(format!("{keys_type}.qml")), keys)?;
    let Some(end) = panel.trim_end().rfind('}') else {
        bail!("'{from}' has changed: its panel doesn't end in '}}'");
    };
    panel.insert_str(end, &format!("\n  {keys_type} {{ panel: root }}\n"));
    std::fs::write(&panel_path, panel.replace("@PATCH_BIN@", &exe.to_string_lossy()))?;
    // Validate the complete integration before replacing the user's plugin.
    let previous = plugins().join(format!(".patch-audio-previous-{}", std::process::id()));
    if previous.exists() {
        bail!("previous Patch plugin backup already exists at {}", previous.display());
    }
    let had_previous = installed_dir.exists();
    // The shell keeps a plugin's QML from before a reload: a changed widget
    // shows only after a restart of the shell.
    let changed = had_previous && dir_files(&dir) != dir_files(&installed_dir);
    let health = plugins().join(HEALTH_ID);
    let health_changed = health.exists()
        && (std::fs::read(health.join("Health.qml")).ok().as_deref() != Some(HEALTH_QML.as_bytes())
            || std::fs::read(health.join("manifest.json")).ok().as_deref() != Some(HEALTH_MANIFEST.as_bytes()));
    if refresh && !changed && !health_changed {
        return Ok(());
    }
    if had_previous {
        std::fs::rename(&installed_dir, &previous)?;
    }
    if let Err(error) = std::fs::rename(&dir, &installed_dir) {
        if had_previous {
            std::fs::rename(&previous, &installed_dir)?;
        }
        return Err(error.into());
    }
    if had_previous {
        std::fs::remove_dir_all(&previous)?;
    }

    // The bar: Patch in the old widget's place, the old file kept once.
    if from != ID && !ids.iter().any(|i| i == ID) {
        let backup = path.with_extension("json.before-patch");
        if !backup.exists() {
            std::fs::copy(&path, &backup)?;
        }
        std::fs::write(&path, swap_widget(&text, &from, ID)?)?;
    }
    std::fs::create_dir_all(state_file().parent().unwrap())?;
    std::fs::write(
        state_file(),
        serde_json::to_string_pretty(&json!({"replaced": from, "installed": installed_dir}))? + "\n",
    )?;
    write_health()?;
    shell(&["shell", "rescanPlugins"]);
    shell(&["shell", "reloadConfig"]);
    if changed || health_changed {
        let _ = std::process::Command::new("omarchy-restart-shell")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    if refresh {
        println!("the bar's Patch widget is this build's now");
    } else {
        println!(
            "Patch is in the bar's audio dropdown (in place of {from}): omarchy-patch bar uninstall puts it back"
        );
    }
    Ok(())
}

pub fn uninstall() -> anyhow::Result<()> {
    let back = read_json(&state_file())
        .and_then(|s| s["replaced"].as_str().map(str::to_string))
        .unwrap_or_else(|| SOURCE.into());
    let path = shell_json();
    let text = std::fs::read_to_string(&path)?;
    if bar_ids(&serde_json::from_str(&text)?)
        .iter()
        .any(|i| i == ID)
    {
        std::fs::write(&path, swap_widget(&text, ID, &back)?)?;
    }
    if bar_ids(&serde_json::from_str(&std::fs::read_to_string(&path)?)?)
        .iter()
        .any(|i| i == HEALTH_ID)
    {
        shell(&["shell", "setPluginEnabled", HEALTH_ID, "false"]);
    }
    for id in [ID, HEALTH_ID] {
        let dir = plugins().join(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
    }
    let _ = std::fs::remove_file(state_file());
    shell(&["shell", "rescanPlugins"]);
    shell(&["shell", "reloadConfig"]);
    println!("the bar shows {back} again");
    Ok(())
}

/// The buffer-health plugin's files (written over each time: an update
/// brings its new version).
fn write_health() -> anyhow::Result<()> {
    let dir = plugins().join(HEALTH_ID);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("manifest.json"), HEALTH_MANIFEST)?;
    std::fs::write(dir.join("Health.qml"), HEALTH_QML)?;
    Ok(())
}

/// `bar health on|off`: the buffer-health dot in the bar, left of Patch's
/// audio widget (or where the shell puts new widgets, without it), or out.
pub fn health(on: bool) -> anyhow::Result<()> {
    if !on {
        let out = std::process::Command::new("omarchy-shell")
            .args(["shell", "setPluginEnabled", HEALTH_ID, "false"])
            .output()
            .context("running omarchy-shell")?;
        if String::from_utf8_lossy(&out.stdout).trim() != "ok" {
            bail!("the bar has no buffer-health dot");
        }
        println!("the buffer-health dot is out of the bar");
        return Ok(());
    }
    write_health()?;
    shell(&["shell", "rescanPlugins"]);
    let text = std::fs::read_to_string(shell_json()).unwrap_or_default();
    let ids = serde_json::from_str(&text).map(|v| bar_ids(&v)).unwrap_or_default();
    let place = if ids.iter().any(|i| i == ID) {
        json!({"before": ID}).to_string()
    } else {
        "{}".into()
    };
    let out = std::process::Command::new("omarchy-shell")
        .args(["shell", "enablePlugin", HEALTH_ID, &place])
        .output()
        .context("running omarchy-shell")?;
    let result = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if result != "ok" {
        bail!("the shell didn't add it: {result}");
    }
    println!("the buffer-health dot is in the bar: hover it for why; omarchy-patch bar health off takes it out");
    Ok(())
}

/// `omarchy-patch health`: what the dot shows, for a report.
pub fn health_status() -> anyhow::Result<()> {
    let path = patch_core::ipc::runtime_dir().join("health.json");
    let v: Value = read_json(&path).context("the engine isn't running (no health.json)")?;
    let age = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs()
        .saturating_sub(v["at"].as_u64().unwrap_or(0));
    if age > 3 {
        bail!("the engine isn't running (health.json is {age} s old)");
    }
    println!("{}: {}", v["state"].as_str().unwrap_or("?"), v["reason"].as_str().unwrap_or(""));
    if let Some(d) = v["detail"].as_str() {
        println!("{d}");
    }
    let k = &v["skips"];
    println!(
        "since the engine started: device ran dry {}, processing late {}, deck underruns {}",
        k["dry"], k["late"], k["decks"]
    );
    Ok(())
}

/// The built-in layouts the dropdown offers: the ones this setup has
/// (as in the controller), with Pro last.
fn bar_layouts() -> Vec<(&'static str, &'static str)> {
    let setup = patch_core::setup::load();
    patch_surfaces::spec()
        .layouts
        .iter()
        .filter(|l| patch_surfaces::spec::layout_offered(&setup, l.id))
        .map(|l| (l.id, l.name))
        .collect()
}

/// The layout buttons, then the layout on screen: its id and name, whether
/// its windows moved since it was opened or saved, and whether it is one
/// you saved (Save writes over it; a built-in one only takes Save as…).
fn layout_status(ui: Option<&str>) -> Value {
    use patch_host::HostedApp;
    let status = ui
        .and_then(patch_core::ipc::Status::parse)
        .unwrap_or_default();
    let saved = patch_core::user_layouts::load();
    let mut layouts: Vec<Value> = bar_layouts()
        .iter()
        .map(|(id, label)| json!({"id": id, "label": label, "custom": false}))
        .collect();
    layouts.extend(
        saved
            .iter()
            .map(|l| json!({"id": l.id, "label": l.name, "custom": true})),
    );
    let (standard, slots) = layout_picks(&layouts, &recent());
    let id = status.layout.unwrap_or_default();
    let custom = saved.iter().any(|l| l.id == id);
    let name = saved
        .iter()
        .find(|l| l.id == id)
        .map(|l| l.name)
        .or_else(|| patch_surfaces::AudioApp::spec().layout(&id).map(|l| l.name))
        .unwrap_or("");
    json!({"layouts": layouts, "standard": standard, "slots": slots, "layout": id, "layoutName": name, "layoutEdited": status.edited, "layoutCustom": custom})
}

/// The basic layouts the menu always shows.
const STANDARD_LAYOUTS: [&str; 4] = ["simple", "basic-dj", "ai-dj", "dj-studio"];

/// The menu's layout buttons: four basic ones, then four slots with the
/// layouts opened most lately (yours or built-in), topped up with the other
/// built-ins, then yours. Every other layout is one search away. The user, 2026-11-16:
/// every layout as a button grows too long with many saved layouts.
fn layout_picks(all: &[Value], recent: &[(String, String)]) -> (Vec<Value>, Vec<Value>) {
    let find = |id: &str| all.iter().find(|l| l["id"] == id).cloned();
    let builtin = all.iter().filter(|l| l["custom"] == false);
    let mut standard: Vec<Value> = STANDARD_LAYOUTS.iter().filter_map(|id| find(id)).collect();
    for l in builtin.clone() {
        if standard.len() >= 4 {
            break;
        }
        if !standard.contains(l) {
            standard.push(l.clone());
        }
    }
    let mut slots: Vec<Value> = Vec::new();
    let lately = recent
        .iter()
        .filter(|(k, _)| k == "layout")
        .filter_map(|(_, id)| find(id));
    for l in lately.chain(builtin.cloned()).chain(all.iter().cloned()) {
        if slots.len() == 4 {
            break;
        }
        if !standard.contains(&l) && !slots.contains(&l) {
            slots.push(l);
        }
    }
    (standard, slots)
}

/// One line of JSON for the dropdown: the service, what runs (`now`), the
/// windows on screen, the master EQ, the layouts and what to open.
pub fn status(ui: Option<String>) -> Value {
    let call = |v: Value| patch_engine::client::call(&v).ok();
    let setup = patch_core::setup::load();
    let spec = patch_surfaces::spec();
    let mut out = layout_status(ui.as_deref());
    let open = ui
        .as_deref()
        .and_then(patch_core::ipc::Status::parse)
        .unwrap_or_default()
        .open;
    out["open"] = json!(open_windows(&spec, &open));
    let units = units(&spec, &setup);
    out["suggest"] = json!(suggestions(&recent(), &units, &open));
    out["units"] = json!(units);
    out["output_selection"] = json!(std::fs::read_to_string(
        patch_core::ipc::runtime_dir().join("output-selection")
    ).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    out["input_selection"] = json!(std::fs::read_to_string(
        patch_core::ipc::runtime_dir().join("input-selection")
    ).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    // The device the engine drives. The bar's audio menu resolves its bus
    // default to this, so the OUTPUT row names (and marks current) the
    // device Patch really plays through; the panel's own read of the runtime
    // file can come back empty when the shell it runs in has no
    // XDG_RUNTIME_DIR, and then no row looked selected.
    out["output"] = json!(std::fs::read_to_string(
        patch_core::ipc::runtime_dir().join("output-device")
    )
    .ok()
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty()));
    // What runs. The recorder, the stream and the player are processes of
    // their own: asked only when their socket is there (nothing probed).
    let paused = Paused::load();
    let shows = |id: &str| setup.shows(id);
    let mut now = Vec::new();
    let runtime = patch_core::ipc::runtime_dir();
    if runtime.join("broadcast.sock").exists()
        && let Some(b) = patch_broadcast::daemon::status()
    {
        now.push(broadcast_row(&b));
    }
    if runtime.join("recorder.sock").exists()
        && let Some(r) = patch_record::daemon::status()
    {
        now.push(record_row(&r));
    }
    // Network links: their own services, read from their status files.
    if let Some(detail) = patch_network::ops::summary() {
        now.push(network_row(&detail));
    }
    let engine = call(json!({"op": "ping"})).is_some();
    let dj = (engine && shows("deck-a"))
        .then(|| call(json!({"op": "dj"})))
        .flatten();
    if let Some(dj) = &dj {
        now.extend(dj_rows(dj, &paused, &shows));
    }
    if runtime.join("player.sock").exists() {
        now.extend(player_row(patch_surfaces::player::snapshot().as_ref(), &paused));
    }
    // The Player's carts: voices of their own, asked only when one runs.
    if patch_surfaces::player::carts::any() {
        now.extend(carts_row(&patch_surfaces::player::carts::status()));
    }
    if engine && shows("tidal") {
        let tidal = call(json!({"op": "tidal", "action": "status"}));
        now.extend(tidal_row(tidal.as_ref(), &paused));
    }
    if engine && shows("sequencer") {
        let seq = call(json!({"op": "sequencer"}));
        now.extend(sequencer_row(seq.as_ref(), &paused));
    }
    if engine && shows("pattern") {
        let pattern = call(json!({"op": "pattern"}));
        now.extend(pattern_row(pattern.as_ref(), &paused));
    }
    out["now"] = json!(now);
    // In the mix: what processes the sound without a window (the rack's
    // plugins that are on, a mic you hear yourself on).
    let rack = if shows("plugins") {
        patch_plugin::load_or_default().map(|r| r.slots).unwrap_or_default()
    } else {
        vec![]
    };
    let running = (engine && rack.iter().any(|s| s.active))
        .then(|| call(json!({"op": "plugins"})))
        .flatten();
    let mut chain = plugin_chips(&rack, running.as_ref());
    if engine && shows("mic") {
        chain.extend(mic_chips(call(json!({"op": "mics"})).as_ref()));
    }
    out["chain"] = json!(chain);
    // Every plugin in the rack is found by the search, on or off: its window
    // is where it is changed.
    if let Some(units) = out["units"].as_array_mut() {
        units.extend(rack.iter().map(|s| {
            json!({"id": plugin_window(s.id), "label": s.name, "hint": "plugin",
                   "words": format!("{} {} plugin vst {}", s.name, s.vendor, s.place.label()).to_lowercase()})
        }));
    }
    if !engine {
        out["engine"] = json!(false);
        return out;
    }
    let playing = dj
        .as_ref()
        .map(|dj| playing_decks(dj).iter().map(|(d, t)| format!("{} · {t}", d.to_uppercase())).collect::<Vec<_>>().join("  "))
        .unwrap_or_default();
    let eq = call(json!({"op": "master_eq", "target": "master"})).unwrap_or(Value::Null);
    let flat = eq["gain_db"].as_f64().unwrap_or(0.0).abs() < 0.01 && !eq["bands"].as_array().into_iter().flatten().any(|b| {
        b["on"] == true
            && (b["gain_db"].as_f64().unwrap_or(0.0).abs() >= 0.01
                || !matches!(b["kind"].as_str(), Some("bell" | "lowshelf" | "highshelf")))
    });
    let presets: Vec<Value> = call(json!({"op": "eq_presets"}))
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .iter()
        .map(|p| p["name"].clone())
        .collect();
    out["engine"] = json!(true);
    out["capture_main"] = call(json!({"op":"input_mix"})).unwrap_or(Value::Null)["main"].clone();
    out["playing"] = json!(playing);
    out["eq"] = json!({"preset": eq["preset"], "edited": eq["edited"], "bypass": eq["bypass"], "flat": flat});
    out["presets"] = json!(presets);
    out
}

// ---- What runs: one row per activity ---------------------------------------
//
// A row is what runs with every window closed: its name, one line of
// state, the windows that belong to it (`go`, the first is a click on the
// row) and at most two actions (`do`: the words after `bar do`). A
// consequential action (`confirm`) asks once in the menu.

fn go(id: &str, label: &str) -> Value {
    json!({"id": id, "label": label})
}

fn act(what: &str, label: &str, confirm: bool) -> Value {
    json!({"do": what, "label": label, "confirm": confirm})
}

fn clock(seconds: f64) -> String {
    let s = seconds.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

const DECKS: [&str; 4] = ["a", "b", "c", "d"];

/// The decks that play, with their titles, from the engine's `dj` read.
fn playing_decks(dj: &Value) -> Vec<(&'static str, String)> {
    deck_titles(dj, |d| d["playing"] == true)
}

fn deck_titles(dj: &Value, keep: impl Fn(&Value) -> bool) -> Vec<(&'static str, String)> {
    DECKS
        .iter()
        .zip(dj["decks"].as_array().into_iter().flatten())
        .filter(|(_, d)| keep(d))
        .map(|(n, d)| {
            let title = d["title"].as_str().filter(|t| !t.is_empty()).unwrap_or("untitled");
            (*n, title.to_string())
        })
        .collect()
}

fn deck_go(letters: &[&str], shows: &dyn Fn(&str) -> bool) -> Vec<Value> {
    letters
        .iter()
        .map(|d| (format!("deck-{d}"), format!("Deck {}", d.to_uppercase())))
        .filter(|(id, _)| shows(id))
        .map(|(id, label)| go(&id, &label))
        .collect()
}

/// The Auto Mix set (its decks belong to it), or the decks played by hand.
fn dj_rows(dj: &Value, paused: &Paused, shows: &dyn Fn(&str) -> bool) -> Vec<Value> {
    let set = &dj["set"];
    let playing = playing_decks(dj);
    let titles = playing.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" → ");
    let letters: Vec<&str> = playing.iter().map(|(d, _)| *d).collect();
    if set["on"] == true && set["ended"] != true {
        let held = set["paused"] == true;
        let minutes = set["elapsed_s"].as_f64().unwrap_or(0.0) / 60.0;
        // On but silent (the engine restarted under it): nothing to pause.
        let silent = !held && titles.is_empty();
        let state = if held {
            "Paused".to_string()
        } else if silent {
            "Not playing".to_string()
        } else {
            titles
        };
        let mut windows = Vec::new();
        if shows("dj") {
            windows.push(go("dj", "Auto Mix"));
        }
        // The decks it plays (paused: the ones it will resume).
        let decks: Vec<&str> = if held {
            deck_titles(dj, |d| d["loaded"] == true).iter().map(|(d, _)| *d).filter(|d| ["a", "b"].contains(d)).collect()
        } else {
            letters
        };
        windows.extend(deck_go(&decks, shows));
        return vec![json!({
            "id": "set",
            "label": "Auto Mix",
            "detail": format!("{state} · {} min", minutes.floor() as u64),
            "paused": held,
            "go": windows,
            "actions": if silent {
                vec![act("set end", "End set", true)]
            } else {
                vec![
                    if held { act("set resume", "Resume", false) } else { act("set pause", "Pause", false) },
                    act("set end", "End set", true),
                ]
            },
        })];
    }
    if !playing.is_empty() {
        let mut windows = deck_go(&letters, shows);
        if shows("dj-mixer") {
            windows.push(go("dj-mixer", "Crossfader"));
        }
        return vec![json!({
            "id": "decks", "label": "Decks", "detail": titles, "paused": false,
            "go": windows, "actions": [act("decks pause", "Pause", false)],
        })];
    }
    // Paused from the menu: offered back while those decks still hold a track.
    let held = deck_titles(dj, |d| d["loaded"] == true);
    let held: Vec<_> = held.iter().filter(|(d, _)| paused.decks.iter().any(|p| p == d)).collect();
    if held.is_empty() {
        return vec![];
    }
    let letters: Vec<&str> = held.iter().map(|(d, _)| *d).collect();
    vec![json!({
        "id": "decks", "label": "Decks",
        "detail": format!("Paused · {}", held.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" · ")),
        "paused": true, "go": deck_go(&letters, shows), "actions": [act("decks play", "Play", false)],
    })]
}

fn record_row(r: &Value) -> Value {
    let held = r["paused"] == true;
    let time = clock(r["seconds"].as_f64().unwrap_or(0.0));
    json!({
        "id": "record", "label": "Recording", "live": true, "paused": held,
        "detail": if held { format!("Paused · {time}") } else { time },
        "go": [go("recorder", "Recorder")],
        "actions": [act("record stop", "Stop", true)],
    })
}

fn broadcast_row(b: &Value) -> Value {
    let state = match b["state"].as_str().unwrap_or("") {
        "live" => "Live",
        "connecting" => "Connecting",
        "reconnecting" => "Reconnecting",
        "error" => "Failed",
        other => other,
    };
    let mut detail = format!("{state} {}", clock(b["seconds"].as_f64().unwrap_or(0.0)));
    if let Some(to) = b["target"].as_str().filter(|t| !t.is_empty()) {
        detail.push_str(&format!(" · {to}"));
    }
    json!({
        "id": "broadcast", "label": "Broadcast", "live": true, "paused": false, "detail": detail,
        "go": [go("broadcast", "Broadcast")],
        "actions": [act("broadcast stop", "Stop", true)],
    })
}

/// Network sends and receives. Not "live": the red dot is for recording
/// and broadcasting only.
fn network_row(detail: &str) -> Value {
    json!({
        "id": "network", "label": "Network", "live": false, "paused": false, "detail": detail,
        "go": [go("network", "Network")],
        "actions": [act("network stop", "Stop", true)],
    })
}

/// The carts that play (a bed left running with the windows closed):
/// their names, and one Stop for all of them.
fn carts_row(playing: &[patch_surfaces::player::carts::Playing]) -> Option<Value> {
    let first = playing.first()?;
    let detail = match playing.len() {
        1 => first.label.clone(),
        n => format!("{} + {} more", first.label, n - 1),
    };
    Some(json!({
        "id": "carts", "label": "Carts", "detail": detail, "paused": false,
        "go": [go("player", "Player")], "actions": [act("player carts stop", "Stop", false)],
    }))
}

fn player_row(p: Option<&patch_surfaces::player::Snapshot>, paused: &Paused) -> Option<Value> {
    let p = p.filter(|p| !p.path.is_empty())?;
    let title = if p.title.is_empty() { "untitled" } else { p.title.as_str() };
    if p.playing {
        return Some(json!({
            "id": "player", "label": "Player", "detail": title, "paused": false,
            "go": [go("player", "Player")], "actions": [act("player pause", "Pause", false)],
        }));
    }
    paused.player.then(|| json!({
        "id": "player", "label": "Player", "detail": format!("Paused · {title}"), "paused": true,
        "go": [go("player", "Player")], "actions": [act("player play", "Play", false)],
    }))
}

fn tidal_row(t: Option<&Value>, paused: &Paused) -> Option<Value> {
    let t = t?;
    let bpm = t["status"]["bpm"].as_f64().or(t["tempo"]["bpm"].as_f64()).unwrap_or(0.0);
    if t["status"]["playing"] == true {
        return Some(json!({
            "id": "tidal", "label": "Tidal", "detail": format!("{} BPM", bpm.round()), "paused": false,
            "go": [go("tidal", "Tidal")], "actions": [act("tidal stop", "Stop", false)],
        }));
    }
    paused.tidal.then(|| json!({
        "id": "tidal", "label": "Tidal", "detail": "Stopped", "paused": true,
        "go": [go("tidal", "Tidal")], "actions": [act("tidal play", "Play", false)],
    }))
}

fn sequencer_row(q: Option<&Value>, paused: &Paused) -> Option<Value> {
    let q = q?;
    let bpm = q["pattern"]["bpm"].as_f64().unwrap_or(0.0);
    if q["playing"] == true {
        return Some(json!({
            "id": "sequencer", "label": "Sequencer", "detail": format!("{} BPM", bpm.round()), "paused": false,
            "go": [go("sequencer", "Sequencer")], "actions": [act("sequencer stop", "Stop", false)],
        }));
    }
    paused.sequencer.then(|| json!({
        "id": "sequencer", "label": "Sequencer", "detail": "Stopped", "paused": true,
        "go": [go("sequencer", "Sequencer")], "actions": [act("sequencer play", "Play", false)],
    }))
}

fn pattern_row(t: Option<&Value>, paused: &Paused) -> Option<Value> {
    let t = t?;
    if t["status"]["playing"] == true {
        let layers = t["layers"].as_array().map_or(0, Vec::len);
        let detail = match layers {
            0 => "Playing".to_string(),
            1 => "1 layer".to_string(),
            n => format!("{n} layers"),
        };
        return Some(json!({
            "id": "pattern", "label": "Pattern", "detail": detail, "paused": false,
            "go": [go("pattern", "Pattern")], "actions": [act("pattern stop", "Stop", false)],
        }));
    }
    paused.pattern.then(|| json!({
        "id": "pattern", "label": "Pattern", "detail": "Stopped", "paused": true,
        "go": [go("pattern", "Pattern")], "actions": [act("pattern play", "Play", false)],
    }))
}

/// A rack slot's window: `plugin` for slot 1, `plugin#N` after.
fn plugin_window(slot: u32) -> String {
    patch_core::instance::id("plugin", slot)
}

/// The rack's plugins that are on, each opening its own window; one the
/// host held out (it crashed or failed to load) says so. Past five, the
/// last opens the rack.
fn plugin_chips(rack: &[patch_plugin::config::Slot], running: Option<&Value>) -> Vec<Value> {
    let failed = |id: u32| {
        running
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|s| s["id"].as_u64() == Some(u64::from(id)))
            .is_some_and(|s| s["state"].get("failed").is_some())
    };
    let on: Vec<_> = rack.iter().filter(|s| s.active).collect();
    let shown = if on.len() > 5 { 4 } else { on.len() };
    let mut out: Vec<Value> = on[..shown]
        .iter()
        .map(|s| {
            let held = failed(s.id);
            json!({
                "id": plugin_window(s.id),
                "label": if held { format!("{} · held out", s.name) } else { s.name.clone() },
                "detail": format!("{} on {}", s.name, s.place.label()),
                "warn": held,
            })
        })
        .collect();
    if on.len() > shown {
        out.push(json!({"id": "plugins", "label": format!("+{} plugins", on.len() - shown),
                        "detail": "The rack", "warn": false}));
    }
    out
}

/// A mic that plays into your ears (its monitor is on): easy to forget.
/// Only mics that are there; an absent one is never asked about.
fn mic_chips(mics: Option<&Value>) -> Vec<Value> {
    mics.map(|m| &m["mics"])
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m["present"] == true && m["monitor"] == true)
        .map(|m| {
            let n = m["channel"].as_str().unwrap_or("").trim_start_matches("mic-").to_string();
            json!({"id": "mic", "label": format!("Mic {n} monitor"),
                   "detail": format!("You hear {} in your headphones", m["description"].as_str().unwrap_or("this mic")),
                   "warn": false})
        })
        .collect()
}

/// What the menu paused, so its row offers Play until that plays again
/// (an hour at most): a paused deck or player is otherwise just loaded.
#[derive(Default)]
struct Paused {
    decks: Vec<String>,
    player: bool,
    tidal: bool,
    sequencer: bool,
    pattern: bool,
}

const PAUSED_FOR: u64 = 3600;

fn paused_file() -> PathBuf {
    patch_core::ipc::runtime_dir().join("bar-paused.json")
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl Paused {
    fn load() -> Paused {
        let v = read_json(&paused_file()).unwrap_or(Value::Null);
        let fresh = |k: &str| v[k]["at"].as_u64().is_some_and(|at| unix_now().saturating_sub(at) < PAUSED_FOR);
        Paused {
            decks: if fresh("decks") {
                v["decks"]["which"].as_array().into_iter().flatten().filter_map(|d| d.as_str().map(str::to_string)).collect()
            } else {
                vec![]
            },
            player: fresh("player"),
            tidal: fresh("tidal"),
            sequencer: fresh("sequencer"),
            pattern: fresh("pattern"),
        }
    }

    /// Remember (`Some`) or forget (`None`) one thing the menu paused.
    fn set(what: &str, value: Option<Value>) {
        let mut v = read_json(&paused_file()).filter(Value::is_object).unwrap_or_else(|| json!({}));
        match value {
            Some(mut x) => {
                x["at"] = json!(unix_now());
                v[what] = x;
            }
            None => {
                v.as_object_mut().map(|o| o.remove(what));
            }
        }
        let _ = std::fs::write(paused_file(), v.to_string());
    }
}

/// `bar do <what> <verb>`: the actions on the menu's rows.
pub fn act_on(words: &[&str]) -> anyhow::Result<()> {
    let engine = |v: Value| patch_engine::client::call(&v).map_err(anyhow::Error::msg);
    let session = |action: &str| {
        engine(json!({"op": "command", "origin": "user",
            "command": {"op": "dj.session", "request": {"action": action}}}))
    };
    match words {
        ["set", "pause"] => drop(session("pause")?),
        ["set", "resume"] => drop(session("resume")?),
        ["set", "end"] => drop(session("end")?),
        ["decks", "pause"] => {
            let dj = engine(json!({"op": "dj"}))?;
            let which: Vec<&str> = playing_decks(&dj).iter().map(|(d, _)| *d).collect();
            for d in &which {
                engine(json!({"op": "deck", "deck": d, "do": "pause"}))?;
            }
            if !which.is_empty() {
                Paused::set("decks", Some(json!({"which": which})));
            }
        }
        ["decks", "play"] => {
            for d in Paused::load().decks {
                engine(json!({"op": "deck", "deck": d, "do": "play"}))?;
            }
            Paused::set("decks", None);
        }
        ["player", verb @ ("pause" | "play")] => {
            crate::player_cli::run(&[*verb])?;
            Paused::set("player", (*verb == "pause").then(|| json!({})));
        }
        ["player", "carts", "stop"] => crate::player_cli::run(&["cart", "stop"])?,
        ["tidal", verb @ ("stop" | "play")] => {
            engine(json!({"op": "tidal", "action": verb}))?;
            Paused::set("tidal", (*verb == "stop").then(|| json!({})));
        }
        ["sequencer", verb @ ("stop" | "play")] => {
            engine(json!({"op": "sequencer", "playing": *verb == "play"}))?;
            Paused::set("sequencer", (*verb == "stop").then(|| json!({})));
        }
        ["pattern", verb @ ("stop" | "play")] => {
            engine(json!({"op": "pattern", "action": verb}))?;
            Paused::set("pattern", (*verb == "stop").then(|| json!({})));
        }
        ["record", "stop"] => drop(patch_record::daemon::stop_and_wait().map_err(anyhow::Error::msg)?),
        ["broadcast", "stop"] => drop(patch_broadcast::daemon::stop_and_wait().map_err(anyhow::Error::msg)?),
        ["network", "stop"] => drop(patch_network::ops::stop_all().map_err(anyhow::Error::msg)?),
        _ => bail!("bar do set pause|resume|end · decks pause|play · player pause|play · player carts stop · tidal|sequencer|pattern stop|play · record stop · broadcast stop · network stop"),
    }
    Ok(())
}

// ---- Opening things: the windows on screen, the Units, recent picks --------

/// The windows open now, by name (a second Meter is "Meter 2").
fn open_windows(spec: &patch_core::AppSpec, open: &[String]) -> Vec<Value> {
    open.iter()
        .filter_map(|id| {
            let s = spec.surface(patch_core::instance::base(id))?;
            let label = match patch_core::instance::number(id) {
                Some(n) if n > 1 => format!("{} {n}", s.title),
                _ => s.title.to_string(),
            };
            Some(go(id, &label))
        })
        .collect()
}

/// Every window the menu can open under this setup, with the words a
/// search matches: its name, its Unit and what the Unit does.
fn units(spec: &patch_core::AppSpec, setup: &patch_core::setup::Setup) -> Vec<Value> {
    spec.surfaces
        .iter()
        .filter(|s| s.origin.is_implemented() && setup.shows(s.id))
        // One per rack slot and the Visualiser's panel open from their Unit.
        .filter(|s| !matches!(s.id, "plugin" | "visualiser-controls"))
        .map(|s| {
            let extra = match s.id {
                "dj" => "ai dj set automix",
                "dj-mixer" => "dj mixer xfader",
                "dj-mini" => "mini small compact controller decks ai dj set",
                "graph" => "routing",
                "meters" => "levels loudness",
                "recorder" => "record recording",
                "broadcast" => "stream streaming live",
                "network" => "lan rtp aes67 vban send receive multicast",
                _ => "",
            };
            let words = format!("{} {} {} {extra}", s.title, s.unit.name(), s.unit.scope()).to_lowercase();
            json!({"id": s.id, "label": s.title, "words": words})
        })
        .collect()
}

fn recent_file() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("omarchy-patch/recent")
}

/// What was opened lately (`unit mixer`, `layout ai-dj`), newest first.
fn recent() -> Vec<(String, String)> {
    std::fs::read_to_string(recent_file())
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(k, id)| (k.to_string(), id.to_string()))
        .collect()
}

/// Remember an opened Unit window or layout for the menu's suggestions.
pub fn remember(kind: &str, id: &str) {
    let line = format!("{kind} {id}");
    let mut lines = vec![line.clone()];
    lines.extend(
        std::fs::read_to_string(recent_file())
            .unwrap_or_default()
            .lines()
            .filter(|l| *l != line)
            .take(15)
            .map(str::to_string),
    );
    let path = recent_file();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

/// Four Units to open with one click: recent picks that aren't on screen,
/// then the everyday Units. Layouts are not picks: the menu shows every
/// layout as its own button (the user, 2026-11-16).
fn suggestions(recent: &[(String, String)], units: &[Value], open: &[String]) -> Vec<Value> {
    let label_of = |id: &str| -> Option<String> {
        units.iter().find(|v| v["id"] == id).and_then(|v| v["label"].as_str()).map(str::to_string)
    };
    let defaults = ["mixer", "library", "eq", "recorder"];
    let mut out: Vec<Value> = Vec::new();
    let candidates = recent
        .iter()
        .filter(|(k, _)| k == "unit")
        .map(|(_, id)| id.as_str())
        .chain(defaults);
    for id in candidates {
        if out.len() == 4 {
            break;
        }
        let shown = open.iter().any(|o| patch_core::instance::base(o) == id);
        if shown || out.iter().any(|v| v["id"] == id) {
            continue;
        }
        if let Some(label) = label_of(id) {
            out.push(json!({"kind": "unit", "id": id, "label": label}));
        }
    }
    out
}

/// A volume key Patch's widget takes over, and Omarchy's binding of it:
/// its step and the shell shortcut it dispatches (none: the script).
struct VolumeKey {
    keys: &'static str,
    description: &'static str,
    // +1 up, -1 down, 0 mute.
    sign: i32,
    step: u32,
    omarchy: Option<&'static str>,
}

const VOLUME_KEYS: [VolumeKey; 5] = [
    VolumeKey { keys: "XF86AudioRaiseVolume", description: "Volume up", sign: 1, step: 5, omarchy: Some("audio.raise") },
    VolumeKey { keys: "XF86AudioLowerVolume", description: "Volume down", sign: -1, step: 5, omarchy: Some("audio.lower") },
    VolumeKey { keys: "ALT + XF86AudioRaiseVolume", description: "Volume up precise", sign: 1, step: 1, omarchy: None },
    VolumeKey { keys: "ALT + XF86AudioLowerVolume", description: "Volume down precise", sign: -1, step: 1, omarchy: None },
    VolumeKey { keys: "XF86AudioMute", description: "Mute", sign: 0, step: 0, omarchy: Some("audio.mute-toggle") },
];

/// FNV-1a: a stable hash (the same in every build) for a file name.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3))
}

// The steps VolumeKeys.qml has shortcuts for.
const VOLUME_STEPS: [u32; 6] = [1, 2, 3, 4, 5, 10];

/// The Hyprland config the volume keys come from: Omarchy's media binds
/// (a release's run the script, newer ones a shell shortcut), then every
/// hypr/*.lua of the user's, hyprland.lua last (it loads the others first,
/// then its own lines).
fn hypr_config() -> Vec<String> {
    let omarchy = std::env::var_os("OMARCHY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/share/omarchy".into());
    let dir = config().join("hypr");
    let mut files = vec![omarchy.join("default/hypr/bindings/media.lua")];
    let mut user: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "lua") && !p.ends_with("hyprland.lua"))
        .collect();
    user.sort();
    files.extend(user);
    files.push(dir.join("hyprland.lua"));
    files.iter().filter_map(|p| std::fs::read_to_string(p).ok()).collect()
}

/// How the config binds `keys`: Omarchy's default (`{ audio = … }`), the
/// volume script with its argument, or something else (the last binding
/// wins, `None` when the config doesn't bind it).
fn volume_binding(config: &[String], keys: &str) -> Option<Result<Option<String>, ()>> {
    let norm = |s: &str| s.split_whitespace().collect::<String>().to_uppercase();
    let want = norm(keys);
    let mut found = None;
    for line in config.iter().flat_map(|t| t.lines()) {
        let line = line.trim();
        if line.starts_with("--") || !line.contains("bind(") {
            continue;
        }
        let Some(first) = line.split('"').nth(1) else { continue };
        if norm(first) != want {
            continue;
        }
        found = Some(if line.contains("audio = \"") {
            Ok(None)
        } else if let Some((_, rest)) = line.split_once("omarchy-audio-output-volume ") {
            Ok(Some(rest.split(['"', '\'', ' ']).next().unwrap_or_default().to_string()))
        } else {
            Err(())
        });
    }
    found
}

/// Lua for `hyprctl eval`: the volume keys to Patch's shell shortcuts
/// (VolumeKeys.qml) while its marker file is there, else what they did
/// before. A key bound to anything but the volume (or with a step there is
/// no shortcut for) is left as it is.
fn volume_keys_lua(config: &[String]) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut binds = String::new();
    for k in &VOLUME_KEYS {
        let (arg, before) = match volume_binding(config, k.keys) {
            Some(Err(())) => continue,
            Some(Ok(Some(arg))) => (arg.clone(), format!("hl.dsp.exec_cmd({})", q(&format!("omarchy-audio-output-volume {arg}")))),
            None | Some(Ok(None)) => {
                let arg = match k.sign {
                    0 => "mute-toggle".to_string(),
                    1 if k.omarchy.is_some() => "raise".into(),
                    -1 if k.omarchy.is_some() => "lower".into(),
                    s => format!("{}{}", if s > 0 { '+' } else { '-' }, k.step),
                };
                let before = match k.omarchy {
                    Some(name) => format!("hl.dsp.global({})", q(&format!("omarchy:{name}"))),
                    None => format!("hl.dsp.exec_cmd({})", q(&format!("omarchy-audio-output-volume {arg}"))),
                };
                (arg, before)
            }
        };
        let shortcut = if k.sign == 0 {
            if arg != "mute-toggle" {
                continue;
            }
            "volume.mute".to_string()
        } else {
            let step = match arg.as_str() {
                "raise" | "lower" => 5,
                a => match a.strip_prefix(if k.sign > 0 { '+' } else { '-' }).and_then(|n| n.parse().ok()) {
                    Some(n) => n,
                    None => continue,
                },
            };
            if !VOLUME_STEPS.contains(&step) {
                continue;
            }
            format!("volume.{}-{step}", if k.sign > 0 { "up" } else { "down" })
        };
        binds += &format!(
            "key({}, {}, {}, {}, {}, {}) ",
            q(k.keys),
            q(k.description),
            q(&shortcut),
            before,
            q(&format!("omarchy-audio-output-volume {arg}")),
            k.sign != 0
        );
    }
    if binds.is_empty() {
        return String::new();
    }
    // Hyprland's own bind calls: Omarchy 4.0 releases have no o.rebind (nor
    // o.bind_commands, which names a function bind in its keybindings menu).
    format!(
        "if hl and hl.bind and hl.unbind then \
         local marker = (os.getenv(\"XDG_RUNTIME_DIR\") or \"\") .. \"/omarchy-patch-volume-keys\" \
         local function here() local f = io.open(marker, \"r\") if f then f:close() return true end return false end \
         local function key(keys, description, shortcut, before, command, repeating) \
         local function press() if here() then hl.dispatch(hl.dsp.global(\"patch:\" .. shortcut)) else hl.dispatch(before) end end \
         if o and o.bind_commands then o.bind_commands[press] = command end \
         hl.unbind(keys) \
         hl.bind(keys, press, {{ locked = true, repeating = repeating, description = description }}) end \
         {binds}end"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audio panel of the Omarchy installed here still takes Patch's
    /// edits (run after an Omarchy update: `cargo test -p patch --
    /// --ignored the_installed_omarchy_panel`).
    #[test]
    #[ignore]
    fn the_installed_omarchy_panel_takes_patchs_edits() {
        let path = "/usr/share/omarchy/shell/plugins/panels/audio/Panel.qml";
        let panel = std::fs::read_to_string(path).expect("Omarchy's audio panel");
        let out = patch_audio_panel(&panel, "omarchy.audio").expect("every required edit applies");
        assert!(out.contains("chosenOutput"));
    }

    /// 2026-11-13: an Omarchy shell update left the panel's `bar` null and
    /// every `patch.bar.foreground` binding threw, so the section drew
    /// nothing. It reads the bar's ink and type through guarded properties.
    #[test]
    fn the_section_draws_without_a_bar() {
        assert!(!SECTION.contains("patch.bar."), "read patch.ink / patch.face, not patch.bar.*");
        assert!(SECTION.contains("readonly property color ink: bar ? bar.foreground : Color.foreground"));
        assert!(SECTION.contains("readonly property string face: bar ? bar.fontFamily : Style.font.family"));
    }

    #[test]
    fn volume_keys_keep_the_users_steps() {
        let omarchy = volume_keys_lua(&[]);
        assert!(omarchy.contains(r#"key("XF86AudioRaiseVolume", "Volume up", "volume.up-5", hl.dsp.global("omarchy:audio.raise"), "omarchy-audio-output-volume raise", true)"#), "{omarchy}");
        assert!(omarchy.contains(r#"key("ALT + XF86AudioLowerVolume", "Volume down precise", "volume.down-1", hl.dsp.exec_cmd("omarchy-audio-output-volume -1")"#));
        assert!(omarchy.contains(r#"key("XF86AudioMute", "Mute", "volume.mute", hl.dsp.global("omarchy:audio.mute-toggle"), "omarchy-audio-output-volume mute-toggle", false)"#));

        let mine = [
            "-- o.rebind(\"XF86AudioMute\", \"Mute\", \"pamixer -t\")\n".to_string(),
            "o.rebind(\"XF86AudioRaiseVolume\", \"Volume up\", \"omarchy-audio-output-volume +2\", { locked = true })\n\
             o.rebind(\"XF86AudioLowerVolume\", \"Volume down\", \"omarchy-audio-output-volume -2\", { locked = true })\n\
             o.rebind(\"ALT+XF86AudioRaiseVolume\", \"Up\", \"omarchy-audio-output-volume +7\")\n\
             o.rebind(\"ALT + XF86AudioLowerVolume\", \"Down\", \"wpctl set-volume @DEFAULT_SINK@ 1%-\")\n"
                .to_string(),
        ];
        let lua = volume_keys_lua(&mine);
        assert!(lua.contains(r#""volume.up-2", hl.dsp.exec_cmd("omarchy-audio-output-volume +2")"#), "{lua}");
        assert!(lua.contains(r#""volume.down-2", hl.dsp.exec_cmd("omarchy-audio-output-volume -2")"#));
        // No shortcut for 7, a key bound elsewhere: both left alone; the
        // commented-out mute binding doesn't count.
        assert!(!lua.contains("ALT + XF86AudioRaiseVolume") && !lua.contains("ALT + XF86AudioLowerVolume"));
        assert!(lua.contains(r#""volume.mute", hl.dsp.global("omarchy:audio.mute-toggle")"#));
        assert!(!lua.contains('\n'), "one line for the request socket");

        // Omarchy 4.0's own binds run the script: no shell shortcut to fall
        // back to, the script is.
        let release = ["o.bind(\"XF86AudioRaiseVolume\", \"Volume up\", \"omarchy-audio-output-volume raise\", { locked = true, repeating = true })\n\
                        o.bind(\"XF86AudioMute\", \"Mute\", \"omarchy-audio-output-volume mute-toggle\", { locked = true })\n"
            .to_string()];
        let lua = volume_keys_lua(&release);
        assert!(lua.contains(r#""volume.up-5", hl.dsp.exec_cmd("omarchy-audio-output-volume raise")"#), "{lua}");
        assert!(lua.contains(r#""volume.mute", hl.dsp.exec_cmd("omarchy-audio-output-volume mute-toggle")"#));
    }

    #[test]
    fn a_widget_swaps_only_where_it_is_an_id() {
        let text = "{\n  \"bar\": {\"layout\": {\"right\": [\n    {\n      \"id\": \"example.audio\"\n    }, {\"id\":\"x\"}]}},\n  \"note\": \"example.audio\"\n}\n";
        let out = swap_widget(text, "example.audio", ID).unwrap();
        assert!(out.contains("\"id\": \"patch.audio\""));
        assert!(out.contains("\"note\": \"example.audio\""));
        assert_eq!(swap_widget(&out, ID, "example.audio").unwrap(), text);
        assert!(swap_widget(text, "nope", ID).is_err());
    }
    fn scrolling_fixture() -> &'static str {
        r#"
  function ensureCursorVisible(item) {
    if (!item || !scrollArea) return
    var flick = scrollArea.contentItem
    if (!flick || flick.contentY === undefined) return
    var margin = 6
    var maxY = Math.max(0, (flick.contentHeight || 0) - flick.height)
    if (maxY <= Style.space(24) || (root.focusSection === "output" && root.selectedIndex === -1)) {
      flick.contentY = 0
      return
    }
    var pt = item.mapToItem(flick.contentItem || flick, 0, 0)
    var top = pt.y
    var bottom = top + (item.height || 0)
    var viewTop = flick.contentY
    var viewBottom = viewTop + flick.height
    if (top < viewTop + margin) flick.contentY = Math.max(0, Math.min(maxY, top - margin))
    else if (bottom > viewBottom - margin)
      flick.contentY = Math.max(0, Math.min(maxY, bottom + margin - flick.height))
  }

  onMoveRequested: function(dx, dy) { root.moveCursor(dy) }
  onOpenedChanged: { root.cursorActive = false }
  onHoveredChanged: if (hovered) { root.selectedIndex = -1 }
  onContainsMouseChanged: if (containsMouse) { root.selectedIndex = 0 }
"#
    }

    fn panel_44() -> String {
        r#"
  readonly property var sink: Pipewire.defaultAudioSink
  readonly property var source: Pipewire.defaultAudioSource
  readonly property var candidateSinks: {
    if (n && n.isSink && !n.isStream) list.push(n)
  }
  readonly property var candidateSources: {
    if (name === "quickshell") continue
  }
  if (String(n.name || "").indexOf("omarchy_speaker_tuning") === 0) continue
  function setDefaultSink(node) {
    Pipewire.preferredDefaultAudioSink = node
    if (node.id !== undefined && node.name) {
      Quickshell.execDetached([
        "omarchy-audio-output-set-default",
        String(node.id),
        String(node.name)
      ])
    }
  }
  readonly property var volumeSink: {
    if (volumeSinkName === "") return sink
  }
  readonly property real inputVolume: source && source.audio ? source.audio.volume : 0
  function setDefaultSource(node) {
    Pipewire.preferredDefaultAudioSource = node
    if (node.id !== undefined && node.name) {
      Quickshell.execDetached([
        "omarchy-audio-input-set-default",
        String(node.id),
        String(node.name)
      ])
    }
  }
  contentWidth: panel.fittedContentWidth(Style.space(380))
  contentHeight: panel.fittedContentHeight(panelColumn.implicitHeight, Style.space(560))
"#
        .to_string() + scrolling_fixture()
    }

    fn panel_45() -> String {
        r#"
  readonly property var sink: Pipewire.defaultAudioSink
  readonly property var source: Pipewire.defaultAudioSource
  function platformHides(node) { return false }
  readonly property var candidateSinks: {
      if (!n || !n.isSink || n.isStream || platformHides(n)) continue
      list.push(n)
  }
  readonly property var candidateSources: {
      if (name === "quickshell" || platformHides(n)) continue
      list.push(n)
  }
  if (String(n.name || "").indexOf("omarchy_speaker_tuning") === 0) continue
      if (platformHides(n)) continue
  function setDefaultSink(node) {
    Pipewire.preferredDefaultAudioSink = node
    if (node.id !== undefined && node.name) {
      Quickshell.execDetached([
        "omarchy-audio-output-set-default",
        String(node.id),
        String(node.name)
      ])
    }
  }
  readonly property var volumeSink: {
    if (volumeSinkName === "") return sink
  }
  readonly property real inputVolume: source && source.audio ? source.audio.volume : 0
  function setDefaultSource(node) {
    if (node.audio) Pipewire.preferredDefaultAudioSource = node
    if (node.id !== undefined && node.name) {
      Quickshell.execDetached([
        "omarchy-audio-input-set-default",
        String(node.id),
        String(node.name)
      ])
    }
  }
  contentWidth: panel.fittedContentWidth(Style.space(380))
  contentHeight: panel.fittedContentHeight(panel.loadedContentHeight, Style.space(560))
"#
        .to_string() + scrolling_fixture()
    }

    #[test]
    fn the_44_panel_still_installs() {
        let out = patch_audio_panel(&panel_44(), "omarchy.audio").unwrap();
        assert!(out.contains("indexOf(\"patch\") !== 0"));
        assert!(out.contains("name !== \"patch.input\""));
        assert!(out.contains("loopback-"));
        assert!(out.contains("Style.space(820)"));
        assert!(out.contains("panelColumn.implicitHeight"));
        assert!(out.contains("captureSource.audio"));
        assert!(!out.contains("source.audio"));
        assert!(out.contains("chosenOutput"));
    }

    /// The Patch section is inside the menu content on the current panel, so
    /// its id does not exist in the root's bindings and functions: each use
    /// was a ReferenceError and a bar row that selected nothing.
    #[test]
    fn nothing_outside_the_section_reaches_into_it() {
        for panel in [panel_44(), panel_45()] {
            let out = patch_audio_panel(&panel, "omarchy.audio").unwrap();
            assert!(!out.contains("patchSection."));
            assert!(!out.contains("source && captureSource.audio"));
            assert!(out.contains("\"@PATCH_BIN@\", \"output\", \"set-default\", name"));
            assert!(out.contains("\"@PATCH_BIN@\", \"input\", \"set-default\", name"));
            assert!(out.contains("omarchy-patch/output-selection"));
        }
    }

    /// The panel Omarchy really ships, on a machine that has it (CI does not).
    #[test]
    fn the_installed_panel_gets_every_edit() {
        let path = std::path::Path::new("/usr/share/omarchy/shell/plugins/panels/audio/Panel.qml");
        let Ok(panel) = std::fs::read_to_string(path) else {
            return;
        };
        let out = patch_audio_panel(&panel, "omarchy.audio").unwrap();
        assert!(!out.contains("patchSection."));
        assert!(!out.contains("omarchy-audio-output-set-default"));
        assert!(!out.contains("omarchy-audio-input-set-default"));
    }

    #[test]
    fn the_45_panel_keeps_platform_hides_and_grows() {
        let out = patch_audio_panel(&panel_45(), "omarchy.audio").unwrap();
        assert!(out.contains("platformHides(n)"));
        assert!(out.contains("patchName.indexOf(\"patch\") === 0 && patchName !== \"patch\""));
        assert!(out.contains("platformHides(n) || (name.indexOf(\"patch\") === 0"));
        assert!(out.contains("if (platformHides(n)) continue"));
        assert!(out.contains("panel.loadedContentHeight, root.patchMenuHeightCap"));
        assert!(!out.contains("fittedContentHeight(panel.loadedContentHeight, Style.space(560))"));
        assert!(out.contains("if (!node || !node.name) return"));
        assert!(!out.contains("if (node.audio)"));
        assert!(!out.contains("omarchy-audio-input-set-default"));
        assert!(out.contains("captureSource.audio"));
        assert!(out.contains("var bus = Pipewire.defaultAudioSink"));
    }

    /// The real Omarchy 4.5 panel, when this checkout has it beside the
    /// tree. CI does not, so the fixtures above are what always runs.
    #[test]
    fn the_rc2_panel_source_installs_when_it_is_checked_out() {
        let path = std::path::Path::new("/tmp/omarchy-rc2/shell/plugins/panels/audio/Panel.qml");
        if !path.is_file() {
            return;
        }
        let panel = std::fs::read_to_string(path).unwrap();
        let out = patch_audio_panel(&panel, "omarchy.audio").unwrap();
        assert!(out.contains("platformHides(n)"));
        assert!(out.contains("patchName.indexOf(\"patch\") === 0 && patchName !== \"patch\""));
        assert!(out.contains("panel.loadedContentHeight, root.patchMenuHeightCap"));
        assert!(!out.contains("omarchy-audio-output-set-default"));
        assert!(!out.contains("omarchy-audio-input-set-default"));
        assert!(!out.contains("if (node.audio)"));
        assert!(out.contains("if (platformHides(n)) continue"));
        assert!(out.contains("captureSource.audio"));
    }

    #[test]
    fn menu_scroll_patch_is_idempotent_and_uses_the_screen() {
        let out = patch_audio_panel(&panel_44(), "omarchy.audio").unwrap();
        assert_eq!(patch_menu_scrolling(&out), out);
        assert_eq!(out.matches("property bool patchKeyboardNavigation:").count(), 1);
        assert!(out.contains("if (!root.patchKeyboardNavigation) return"));
        assert!(out.contains("panel.availableCardHeight"));
        assert!(!out.contains("maxY <= Style.space(24)"));
        assert!(!out.contains("root.focusSection === \"output\" && root.selectedIndex === -1"));
        assert!(out.contains("contentWidth: panel.fittedContentWidth(Style.space(560))"));
        assert!(out.contains("panelColumn.implicitHeight, root.patchMenuHeightCap"));
    }

    #[test]
    fn a_panel_without_the_output_list_is_refused() {
        let err = patch_audio_panel(
            "readonly property var sink: Pipewire.defaultAudioSink",
            "omarchy.audio",
        );
        assert!(err.is_err());
    }

}

#[cfg(test)]
mod now_tests {
    use super::*;

    fn every(_: &str) -> bool {
        true
    }

    fn deck(playing: bool, title: &str) -> Value {
        json!({"playing": playing, "loaded": !title.is_empty(), "title": title})
    }

    #[test]
    fn a_set_is_one_row_with_pause_end_and_its_windows() {
        let dj = json!({
            "set": {"on": true, "ended": false, "paused": false, "elapsed_s": 2155},
            "decks": [deck(true, "Radiance"), deck(true, "Timeline"), deck(false, ""), deck(false, "")],
        });
        let rows = dj_rows(&dj, &Paused::default(), &every);
        assert_eq!(rows.len(), 1, "the decks belong to the set");
        let row = &rows[0];
        assert_eq!(row["label"], "Auto Mix");
        assert_eq!(row["detail"], "Radiance → Timeline · 35 min");
        let go: Vec<&str> = row["go"].as_array().unwrap().iter().map(|g| g["id"].as_str().unwrap()).collect();
        assert_eq!(go, ["dj", "deck-a", "deck-b"]);
        assert_eq!(row["actions"][0]["do"], "set pause");
        assert_eq!(row["actions"][1]["do"], "set end");
        assert_eq!(row["actions"][1]["confirm"], true, "ending a set asks once");
    }

    #[test]
    fn a_paused_set_offers_resume_and_its_decks() {
        let dj = json!({
            "set": {"on": true, "ended": false, "paused": true, "elapsed_s": 60},
            "decks": [deck(false, "Radiance"), deck(false, ""), deck(false, ""), deck(false, "")],
        });
        let row = &dj_rows(&dj, &Paused::default(), &every)[0];
        assert_eq!(row["paused"], true);
        assert_eq!(row["actions"][0]["do"], "set resume");
        assert_eq!(row["go"][1]["id"], "deck-a");
    }

    #[test]
    fn decks_by_hand_pause_and_come_back_from_the_menu() {
        let playing = json!({"set": {"on": false}, "decks": [deck(false, "One"), deck(true, "Two")]});
        let row = &dj_rows(&playing, &Paused::default(), &every)[0];
        assert_eq!(row["label"], "Decks");
        assert_eq!(row["actions"][0]["do"], "decks pause");
        let stopped = json!({"set": {"on": false}, "decks": [deck(false, "One"), deck(false, "Two")]});
        assert!(dj_rows(&stopped, &Paused::default(), &every).is_empty(), "a loaded deck is not a row");
        let paused = Paused { decks: vec!["b".into()], ..Paused::default() };
        let row = &dj_rows(&stopped, &paused, &every)[0];
        assert_eq!(row["actions"][0]["do"], "decks play");
        assert_eq!(row["detail"], "Paused · Two");
    }

    #[test]
    fn a_silent_set_says_so_and_only_ends() {
        let dj = json!({
            "set": {"on": true, "ended": false, "paused": false, "elapsed_s": 6180},
            "decks": [deck(false, "Radiance"), deck(false, ""), deck(false, ""), deck(false, "")],
        });
        let row = &dj_rows(&dj, &Paused::default(), &every)[0];
        assert_eq!(row["detail"], "Not playing · 103 min");
        assert_eq!(row["actions"].as_array().unwrap().len(), 1);
        assert_eq!(row["actions"][0]["do"], "set end");
        assert_eq!(row["go"][0]["id"], "dj", "the Auto Mix window is still offered");
    }

    fn slot(id: u32, name: &str, active: bool) -> patch_plugin::config::Slot {
        serde_json::from_value(json!({"id": id, "plugin": format!("/x/{name}.vst3"), "name": name,
            "active": active, "place": "master"})).unwrap()
    }

    #[test]
    fn plugins_that_are_on_each_open_their_window() {
        let rack = vec![slot(1, "Box Of Bass", true), slot(2, "Graillon 3", false), slot(3, "Gluon", true)];
        let running = json!([{"id": 1, "state": "running"}, {"id": 3, "state": {"failed": "crashed"}}]);
        let chips = plugin_chips(&rack, Some(&running));
        let ids: Vec<&str> = chips.iter().map(|c| c["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["plugin", "plugin#3"], "slot 1 is `plugin`, an off plugin is not in the mix");
        assert_eq!(chips[0]["detail"], "Box Of Bass on Master");
        assert_eq!(chips[1]["label"], "Gluon · held out");
        assert_eq!(chips[1]["warn"], true);
        let many: Vec<_> = (1..=7).map(|n| slot(n, &format!("P{n}"), true)).collect();
        let chips = plugin_chips(&many, None);
        assert_eq!(chips.len(), 5);
        assert_eq!(chips[4]["id"], "plugins", "past five, the rack");
        assert_eq!(chips[4]["label"], "+3 plugins");
    }

    #[test]
    fn only_a_present_monitored_mic_is_in_the_mix() {
        let mics = json!({"mics": [
            {"channel": "mic-1", "present": true, "monitor": true, "description": "BLUE"},
            {"channel": "mic-2", "present": true, "monitor": false},
            {"channel": "mic-5", "present": false, "monitor": true},
        ]});
        let chips = mic_chips(Some(&mics));
        assert_eq!(chips.len(), 1);
        assert_eq!(chips[0]["label"], "Mic 1 monitor");
    }

    #[test]
    fn the_sequencer_stops_and_comes_back() {
        let playing = json!({"playing": true, "pattern": {"bpm": 122}});
        let row = sequencer_row(Some(&playing), &Paused::default()).unwrap();
        assert_eq!(row["detail"], "122 BPM");
        assert_eq!(row["actions"][0]["do"], "sequencer stop");
        let stopped = json!({"playing": false, "pattern": {"bpm": 122}});
        assert!(sequencer_row(Some(&stopped), &Paused::default()).is_none());
        let held = Paused { sequencer: true, ..Paused::default() };
        assert_eq!(sequencer_row(Some(&stopped), &held).unwrap()["actions"][0]["do"], "sequencer play");
    }

    #[test]
    fn an_ended_set_is_gone() {
        let dj = json!({"set": {"on": true, "ended": true}, "decks": [deck(false, "A")]});
        assert!(dj_rows(&dj, &Paused::default(), &every).is_empty());
    }

    #[test]
    fn a_take_and_a_stream_stop_only_after_asking() {
        let rec = record_row(&json!({"seconds": 724.0, "paused": false}));
        assert_eq!(rec["detail"], "12:04");
        assert_eq!(rec["live"], true);
        assert_eq!(rec["actions"][0]["confirm"], true);
        let live = broadcast_row(&json!({"state": "live", "seconds": 3731.0, "target": "icecast.example"}));
        assert_eq!(live["detail"], "Live 1:02:11 · icecast.example");
        assert_eq!(live["actions"][0]["confirm"], true);
        // Network links stop after asking too, and never light the red dot.
        let net = network_row("Sending Program");
        assert_eq!(net["actions"][0]["confirm"], true);
        assert_eq!(net["live"], false);
    }

    #[test]
    fn the_menu_shows_four_basic_layouts_and_four_recent_slots() {
        let l = |id: &str, custom: bool| json!({"id": id, "label": id, "custom": custom});
        let all = vec![
            l("simple", false), l("basic-dj", false), l("ai-dj", false), l("performance-dj", false),
            l("dj-studio", false), l("podcast", false), l("pro", false),
            l("live-1", true), l("studio", true), l("show", true), l("old", true), l("older", true),
        ];
        let r = |k: &str, id: &str| (k.to_string(), id.to_string());
        let recent = vec![
            r("layout", "studio"), r("unit", "mixer"), r("layout", "ai-dj"), r("layout", "--help"),
            r("layout", "live-1"), r("layout", "studio"), r("layout", "pro"),
        ];
        let ids = |v: &[Value]| v.iter().map(|x| x["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let (standard, slots) = layout_picks(&all, &recent);
        assert_eq!(ids(&standard), ["simple", "basic-dj", "ai-dj", "dj-studio"]);
        // Lately opened first (a basic one or an unknown id takes no slot),
        // then the other built-ins.
        assert_eq!(ids(&slots), ["studio", "live-1", "pro", "performance-dj"]);
        // A fresh install: the slots hold the other built-ins.
        let (_, slots) = layout_picks(&all, &[]);
        assert_eq!(ids(&slots), ["performance-dj", "podcast", "pro", "live-1"]);
    }

    #[test]
    fn suggestions_skip_what_is_on_screen_and_fill_with_everyday_units() {
        let units = vec![
            json!({"id": "mixer", "label": "Mixer"}),
            json!({"id": "library", "label": "Library"}),
            json!({"id": "eq", "label": "EQ"}),
            json!({"id": "recorder", "label": "Recorder"}),
            json!({"id": "deck-a", "label": "Deck A"}),
        ];
        let recent = vec![
            ("unit".to_string(), "deck-a".to_string()),
            ("layout".to_string(), "ai-dj".to_string()),
            ("unit".to_string(), "gone".to_string()),
        ];
        let open = vec!["mixer".to_string()];
        let out = suggestions(&recent, &units, &open);
        let ids: Vec<&str> = out.iter().map(|v| v["id"].as_str().unwrap()).collect();
        assert_eq!(ids, ["deck-a", "library", "eq", "recorder"], "Units only: every layout has its own button");
    }

    /// The panel Omarchy really ships, on a machine that has it (CI does not).
    #[test]
    fn the_installed_panel_gets_the_icon_mark() {
        let path = std::path::Path::new("/usr/share/omarchy/shell/plugins/panels/audio/Panel.qml");
        let Ok(panel) = std::fs::read_to_string(path) else {
            return;
        };
        let out = patch_audio_panel(&panel, "omarchy.audio").unwrap();
        assert!(out.contains("    PatchMark { anchors.fill: parent }\n"));
    }
}
