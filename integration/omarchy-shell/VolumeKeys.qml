import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Io
import Quickshell.Services.Pipewire
import qs.Commons

// The volume keys, in the shell. Omarchy's shell steps the volume itself
// only for a hardware output; with Patch's sink as the default its keys ran
// omarchy-audio-output-volume, ~15 processes a press (8 of them pactl),
// about a second on a slow laptop. Here: Patch's sink or a hardware output
// stepped in place, nothing spawned, Omarchy's OSD. Anything else (a DSP
// sink such as EasyEffects) still goes through the script, which knows
// where its loudness lives.
//
// The keys reach this through Hyprland binds set for the running session
// (`hyprctl eval`, again after each config reload; nothing in the user's
// config). Each press checks the marker file this writes while loaded;
// without it a key does what it did before. `omarchy-patch bar install`
// writes the binds from the user's own steps (bar.rs, volume_keys_lua).
Item {
  id: keys

  // The audio widget (its volumeSink: the `patch` bus while Patch plays).
  required property var panel

  readonly property var sink: Pipewire.defaultAudioSink
  readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") || ("/run/user/" + Quickshell.env("UID"))
  readonly property string marker: runtimeDir + "/omarchy-patch-volume-keys"
  // Written by the installer.
  readonly property string bindLua: @BIND_LUA@
  property double lastMute: 0

  PwObjectTracker { objects: keys.sink ? [keys.sink] : [] }

  // The node to step in place, or null for the script.
  function target() {
    var name = sink ? String(sink.name || "") : ""
    if (name === "patch") return panel.volumeSink
    if (name.indexOf("alsa_output.") === 0) return sink
    return null
  }

  function step(delta) {
    var node = target()
    if (!node || !node.audio) {
      Quickshell.execDetached(["omarchy-audio-output-volume", (delta > 0 ? "+" : "-") + Math.abs(delta)])
      return
    }
    var percent = Math.max(0, Math.min(100, Math.round(node.audio.volume * 100) + delta))
    node.audio.muted = false
    node.audio.volume = percent / 100
    osd(percent, false)
  }

  function toggleMute() {
    // Some keyboards bounce the mute key (Omarchy ignores 250 ms too).
    var now = Date.now()
    if (now - lastMute < 250) return
    lastMute = now
    var node = target()
    if (!node || !node.audio) {
      Quickshell.execDetached(["omarchy-audio-output-volume", "mute-toggle"])
      return
    }
    var muted = !node.audio.muted
    node.audio.muted = muted
    osd(Math.round(node.audio.volume * 100), muted)
  }

  // Omarchy's own volume OSD, from the values just set: summoned in the
  // shell (a clone of omarchy.audio may), else through its IPC as
  // omarchy-osd does, one process after the volume has changed.
  function osd(percent, muted) {
    var payload = JSON.stringify({
      icon: muted || percent === 0 ? "volume-muted" : "volume-high",
      message: "",
      value: String(percent),
      // The language's digits where the shell has them (not Omarchy 4.0).
      progressText: (typeof I18n !== "undefined" ? I18n.formatNumber(percent) : String(percent)) + "%",
      max: "100",
      duration: ""
    })
    var shown = panel.bar && panel.bar.shell && panel.bar.shell.summon("omarchy.osd", payload)
    if (shown !== true)
      Quickshell.execDetached(["omarchy-shell", "-q", "osd", "show", payload])
  }

  // patch:volume.up-N / patch:volume.down-N, N the binding's step.
  Variants {
    model: [1, 2, 3, 4, 5, 10]

    Item {
      required property int modelData

      GlobalShortcut {
        appid: "patch"
        name: "volume.up-" + modelData
        description: "Volume up " + modelData + "%"
        onPressed: keys.step(modelData)
      }

      GlobalShortcut {
        appid: "patch"
        name: "volume.down-" + modelData
        description: "Volume down " + modelData + "%"
        onPressed: keys.step(-modelData)
      }
    }
  }

  GlobalShortcut {
    appid: "patch"
    name: "volume.mute"
    description: "Mute"
    onPressed: keys.toggleMute()
  }

  FileView {
    id: ready
    path: keys.marker
    printErrors: false
  }

  // Hyprland's request socket, what hyprctl writes to: one request per
  // connection.
  Socket {
    id: hypr
    path: keys.runtimeDir + "/hypr/" + Quickshell.env("HYPRLAND_INSTANCE_SIGNATURE") + "/.socket.sock"
    onConnectedChanged: {
      if (connected) {
        write("/eval " + keys.bindLua)
        flush()
      }
    }
  }

  function bind() {
    if (!bindLua) return
    hypr.connected = false
    hypr.connected = true
  }

  // A reload puts the config's own binds back.
  Connections {
    target: Hyprland
    function onRawEvent(event) {
      if (event.name === "configreloaded") keys.bind()
    }
  }

  // This instance's mark: a reload (an update of the widget) makes the new
  // instance before the old one goes, and the old one must not take the
  // new one's marker with it.
  readonly property string token: Date.now() + "-" + Math.floor(Math.random() * 1e9)

  Component.onCompleted: {
    ready.setText(token + "\n")
    bind()
  }
  Component.onDestruction: Quickshell.execDetached(["sh", "-c", '[ "$(cat "$1" 2>/dev/null)" = "$2" ] && rm -f "$1"', "sh", keys.marker, token])
}
