import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons

// A dot on the bar's audio icon while Patch records or broadcasts, so a
// take or a stream left running with every window closed is never out of
// sight. The recorder and the broadcast process each rewrite a runtime file
// once a second while they run (patch_core::ipc::heartbeat); this reads
// them every two seconds, nothing spawned. A file older than 4 s is a
// process that died: no dot.
Item {
  id: mark

  property real now: Date.now() / 1000
  property real recordingAt: 0
  property real broadcastingAt: 0
  readonly property bool recording: now - recordingAt < 4
  readonly property bool broadcasting: now - broadcastingAt < 4
  readonly property string dir: (Quickshell.env("XDG_RUNTIME_DIR") || ("/run/user/" + Quickshell.env("UID"))) + "/omarchy-patch/"

  function at(text) {
    try { return Number(JSON.parse(text).at) || 0 } catch (e) { return 0 }
  }

  FileView {
    id: recordingFile
    path: mark.dir + "recording.json"
    printErrors: false
    onLoaded: mark.recordingAt = mark.at(text())
    onLoadFailed: mark.recordingAt = 0
  }

  FileView {
    id: broadcastingFile
    path: mark.dir + "broadcasting.json"
    printErrors: false
    onLoaded: mark.broadcastingAt = mark.at(text())
    onLoadFailed: mark.broadcastingAt = 0
  }

  Timer {
    interval: 2000
    running: true
    repeat: true
    triggeredOnStart: true
    onTriggered: {
      mark.now = Date.now() / 1000
      recordingFile.reload()
      broadcastingFile.reload()
    }
  }

  Rectangle {
    visible: mark.recording || mark.broadcasting
    width: Style.space(6)
    height: width
    radius: width / 2
    color: Color.urgent
    anchors.top: parent.top
    anchors.right: parent.right
    anchors.topMargin: Style.space(4)
    anchors.rightMargin: Style.space(2)
  }
}
