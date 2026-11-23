import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui
import qs.Commons

// Patch's buffer health: the engine writes health.json once a second
// (crates/patch-engine/src/daemon/health.rs); this reads it once a second,
// nothing spawned. A file older than 4 s: the engine isn't running.
BarWidget {
  id: root
  moduleName: "patch.health"

  property var health: ({})
  property real now: Date.now() / 1000
  readonly property bool live: health.at !== undefined && now - health.at < 4
  readonly property string level: live ? String(health.state || "") : ""
  readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || ("/run/user/" + Quickshell.env("UID"))
  readonly property string themeDir: (Quickshell.env("XDG_STATE_HOME") || Quickshell.env("HOME") + "/.local/state") + "/omarchy/current/theme"
  property var palette: ({})

  function themeColor(name, fallback) {
    var v = palette[name]
    return v ? v : fallback
  }

  // Only there when something is wrong: a skip, or a cycle close to its
  // limit. Healthy buffers and a stopped engine leave the bar alone.
  visible: level === "red" || level === "amber"
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FileView {
    id: file
    path: root.runtimeDir + "/omarchy-patch/health.json"
    printErrors: false
    onLoaded: {
      try { root.health = JSON.parse(text()) } catch (e) { root.health = ({}) }
    }
    onLoadFailed: root.health = ({})
  }

  FileView {
    path: root.themeDir + "/colors.toml"
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: {
      var next = {}
      var lines = String(text() || "").split("\n")
      for (var i = 0; i < lines.length; i++) {
        var m = lines[i].match(/^\s*(green|yellow|red)\s*=\s*"([^"]+)"/)
        if (m) next[m[1]] = m[2]
      }
      root.palette = next
    }
  }

  Timer {
    interval: 1000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: {
      root.now = Date.now() / 1000
      file.reload()
    }
  }

  WidgetButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    hasVisualContent: true
    fixedWidth: root.vertical ? -1 : Style.bar.iconSlot
    fixedHeight: root.vertical ? Style.bar.iconSlot : -1
    tooltipText: !root.live ? "Patch buffers: the engine isn't running"
      : "Patch buffers: " + (root.health.reason || "") + (root.health.detail ? "\n" + root.health.detail : "")
    onPressed: function(b) { root.bar.run("omarchy-shell shell toggle patch.audio") }

    Rectangle {
      anchors.centerIn: parent
      width: Style.space(8)
      height: width
      radius: width / 2
      color: root.level === "red" ? root.themeColor("red", Color.urgent)
        : root.level === "amber" ? root.themeColor("yellow", "#d9ad3c")
        : root.level === "green" ? root.themeColor("green", "#5fb070")
        : "transparent"
      border.width: root.level === "" ? 1 : 0
      border.color: Color.muted
    }
  }
}
