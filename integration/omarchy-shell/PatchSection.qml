import QtQuick
import Quickshell
import Quickshell.Io
import qs.Ui
import qs.Commons

// Patch in the bar's audio dropdown (`omarchy-patch bar install`). It grows
// with what runs, not with options:
//
// * NOW: one row per activity that runs with every window closed (a
//   stream, a take, the AI DJ's set or the decks, the Player, Tidal). The
//   row's state in one line, its pause/stop, and the windows that belong
//   to it (click the row for the first). Stopping a stream or a take, or
//   ending a set, asks once.
// * WINDOWS: the Patch windows on screen, collapsed to one line; open it for
//   each window and the layout's Show/Reset/Save.
// * One search, "Open a Unit or layout…": typing finds Units (by name or
//   what they do) and every layout; empty, recent Units, four basic
//   layouts and four slots with the layouts opened lately, one click each.
//   The user, 2026-11-16: quick-select layouts stay in the menu, but not
//   every saved one (that list grows too long).
// * The master EQ.
//
// The rest of the dropdown (volume, outputs, apps) is the audio widget it
// was made from, and keeps working as it did: with Patch as the system's
// audio the volume slider and media keys set the `patch` sink's volume.
//
// Everything goes through the `omarchy-patch` CLI: the widget holds no
// state of its own and never talks to the engine directly.
Column {
  id: patch

  property QtObject bar: null
  // The bar's ink and type, with the theme's when there is no bar: Omarchy
  // 4.x can build the panel before (or without) one — 2026-11-13 a shell
  // update left `bar` null and every binding threw ("Cannot read property
  // 'foreground' of null"), so the section drew nothing. Read these, never
  // `bar.` directly.
  readonly property color ink: bar ? bar.foreground : Color.foreground
  readonly property string face: bar ? bar.fontFamily : Style.font.family
  // The panel's key handler: Down from an empty search hands the keyboard
  // back to the dropdown's own navigation.
  property Item keys: null
  // Poll only while the dropdown is open.
  property bool active: false
  // Written by the installer: the omarchy-patch it was installed from.
  readonly property string bin: "@PATCH_BIN@"
  readonly property string closeHelper: "@PATCH_CLOSE@"
  property string pendingLayout: ""

  property bool engine: false
  property string preset: ""
  property bool edited: false
  property bool bypass: false
  property bool flat: true
  property var presets: []
  property string outputDevice: ""
  property string outputSelection: ""
  property string inputSelection: ""
  property bool captureMain: false
  // What runs, the windows on screen, what can be opened.
  property var now: []
  // In the mix: plugins that are on, a mic you monitor.
  property var chain: []
  property var open: []
  property var units: []
  property var suggest: []
  // The layout buttons (built-in, then yours) and the layout on screen.
  property var layouts: []
  property var standard: []
  property var slots: []
  property string layout: ""
  property string layoutName: ""
  property bool layoutEdited: false
  property bool layoutCustom: false
  // The Windows line opened up.
  property bool windowsOpen: false
  // Save as…: the name field is open.
  property bool naming: false
  // An action that asks once ("Stop?"): the row's id and its words.
  property string armed: ""
  // What the last action said.
  property string message: ""
  // The search: its matches and the highlighted one.
  readonly property var results: matches(searchField.text)
  property int selected: 0
  // Typing goes to the search, not to the dropdown's keys.
  readonly property bool typing: searchField.activeFocus || nameField.activeFocus

  signal launched()

  spacing: Style.space(10)

  // Anything that opens windows outlives the bar: a text-size or display
  // scale change rebuilds this section, and a child Process dies with it,
  // closing every Patch window (the engine played on). Short commands that
  // report errors use run().
  function launch(args) {
    Quickshell.execDetached([patch.bin].concat(args))
  }

  function run(args) {
    var p = actionProc.createObject(patch, { command: [patch.bin].concat(args) })
    p.running = true
  }

  function refresh() {
    if (!statusProc.running) statusProc.running = true
    if (!outputProc.running) outputProc.running = true
  }

  function closeAllThen(layoutId) {
    if (closeAllProc.running) return
    patch.pendingLayout = layoutId || ""
    patch.message = ""
    closeAllProc.running = true
  }

  function loadLayout(id) {
    patch.closeAllThen(id)
  }

  function closeAll() {
    patch.closeAllThen("")
  }

  // A window: opened, or brought forward when it is open already.
  function openUnit(id) {
    patch.launch(["open", id])
    patch.launched()
  }

  function choose(item) {
    if (!item) return
    if (item.kind === "layout") patch.loadLayout(item.id)
    else patch.openUnit(item.id)
  }

  // A row's action: a consequential one asks once, for three seconds.
  function act(rowId, action) {
    var key = rowId + " " + action.do
    if (action.confirm && patch.armed !== key) {
      patch.armed = key
      disarm.restart()
      return
    }
    patch.armed = ""
    patch.message = ""
    patch.run(["bar", "do"].concat(action.do.split(" ")))
  }

  // Units by name first, then by what they do; layouts by name.
  function matches(text) {
    var q = String(text || "").trim().toLowerCase()
    if (q === "") return []
    function score(label, words) {
      var l = String(label).toLowerCase()
      if (l === q) return 100
      if (l.indexOf(q) === 0) return 80
      if ((" " + l).indexOf(" " + q) >= 0) return 60
      if (l.indexOf(q) >= 0) return 40
      if (words && (" " + words).indexOf(" " + q) >= 0) return 20
      return 0
    }
    var out = []
    for (var i = 0; i < patch.units.length; i++) {
      var u = patch.units[i]
      var s = score(u.label, u.words)
      if (s > 0) out.push({ kind: "unit", id: u.id, label: u.label, hint: u.hint || "", score: s + 1 })
    }
    for (var j = 0; j < patch.layouts.length; j++) {
      var l = patch.layouts[j]
      var t = score(l.label, "layout")
      if (t > 0) out.push({ kind: "layout", id: l.id, label: l.label, score: t })
    }
    out.sort(function(a, b) { return b.score - a.score || a.label.length - b.label.length })
    return out.slice(0, 6)
  }

  function nextPreset(step) {
    if (patch.presets.length === 0) return
    var i = patch.presets.indexOf(patch.preset)
    i = i < 0 ? (step > 0 ? 0 : patch.presets.length - 1)
      : (i + step + patch.presets.length) % patch.presets.length
    patch.preset = patch.presets[i]
    patch.edited = false
    patch.run(["eq", "master", "preset", patch.presets[i]])
  }

  function eqText() {
    if (!patch.engine) return "—"
    if (patch.bypass) return "Bypassed"
    if (patch.preset) return patch.preset + (patch.edited ? " *" : "")
    return patch.flat ? "Flat" : "Custom"
  }

  function windowNames() {
    var names = []
    for (var i = 0; i < patch.open.length; i++) names.push(patch.open[i].label)
    return names.join(", ")
  }

  onActiveChanged: {
    if (active) {
      searchField.text = ""
      patch.armed = ""
      patch.message = ""
      patch.windowsOpen = false
      refresh()
      // Open the dropdown and type: the search has the keyboard.
      focusLater.restart()
    }
  }
  onResultsChanged: selected = 0
  // While a field here has the keyboard, the panel's keys (j/k/h/l/x and
  // Space move its cursor) leave typing alone.
  onTypingChanged: if (keys) keys.blocked = typing

  // The panel gives its own key handler the focus as it opens: take it
  // after that, and once more in case the panel was still showing.
  Timer {
    id: focusLater
    interval: 150
    repeat: true
    property int tries: 0
    onTriggered: {
      searchField.forceActiveFocus()
      if (++tries >= 3 || !patch.active) { stop(); tries = 0 }
    }
  }

  Timer {
    id: disarm
    interval: 3000
    onTriggered: patch.armed = ""
  }

  Process {
    id: statusProc
    command: [patch.bin, "bar", "status"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        try {
          var s = JSON.parse(String(text || "{}"))
          patch.engine = s.engine === true
          patch.preset = s.eq && s.eq.preset || ""
          patch.edited = !!(s.eq && s.eq.edited)
          patch.bypass = !!(s.eq && s.eq.bypass)
          patch.flat = !s.eq || s.eq.flat !== false
          patch.presets = s.presets || []
          patch.outputDevice = s.output || s.output_chosen || patch.outputDevice
          patch.outputSelection = s.output_selection || patch.outputDevice
          patch.inputSelection = s.input_selection || ""
          patch.captureMain = s.capture_main === true
          patch.now = s.now || []
          patch.chain = s.chain || []
          patch.open = s.open || []
          patch.units = s.units || []
          patch.suggest = s.suggest || []
          patch.layouts = s.layouts || []
          patch.standard = s.standard || []
          patch.slots = s.slots || []
          patch.layout = s.layout || ""
          patch.layoutName = s.layoutName || ""
          patch.layoutEdited = s.layoutEdited === true
          patch.layoutCustom = s.layoutCustom === true
          if (!patch.layout) patch.naming = false
        } catch (_error) {
          patch.engine = false
        }
      }
    }
  }

  // Which device the engine drives. The bar's status does not carry it (it
  // carries the bar's own fields), and the bus must never be what the user
  // reads as "the output": the engine remembers its choice in a runtime file
  // for exactly this question, so read that.
  Process {
    id: outputProc
    command: ["sh", "-c", "cat \"$XDG_RUNTIME_DIR/omarchy-patch/output-device\""]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: patch.outputDevice = String(text || "").trim()
    }
  }

  // Clear old sessions before a layout load. The helper also handles a Patch
  // UI process that stopped answering, without touching the separate audio engine.
  Process {
    id: closeAllProc
    command: [patch.closeHelper]
    stdout: StdioCollector { id: closeAllOut; waitForEnd: true }
    stderr: StdioCollector { id: closeAllErr; waitForEnd: true }
    onExited: function(code) {
      var next = patch.pendingLayout
      patch.pendingLayout = ""
      if (code !== 0) {
        patch.message = String(closeAllErr.text || "Could not close Patch windows").trim()
      } else if (next !== "") {
        patch.launch(["layout", next])
        patch.launched()
      } else {
        patch.message = String(closeAllOut.text || "Patch windows closed").trim()
        patch.refresh()
      }
    }
  }

  // Save and delete say what went wrong (a taken name, …): keep stderr.
  Process {
    id: layoutProc
    stdout: StdioCollector { id: layoutOut; waitForEnd: true }
    stderr: StdioCollector { id: layoutErr; waitForEnd: true }
    onExited: function(code) {
      var err = String(layoutErr.text || "").trim().replace(/^Error: /, "")
      patch.message = code === 0 ? String(layoutOut.text || "").trim() : err
      if (code === 0) patch.naming = false
      patch.refresh()
    }
  }

  function layoutCommand(args) {
    if (layoutProc.running) return
    patch.message = ""
    layoutProc.command = [patch.bin, "layout"].concat(args)
    layoutProc.running = true
  }

  function startNaming() {
    nameField.text = patch.layoutCustom ? "" : patch.layoutName + " (mine)"
    patch.message = ""
    patch.naming = true
    nameField.forceActiveFocus()
    nameField.selectAll()
  }

  Component {
    id: actionProc
    Process {
      stderr: StdioCollector { id: actionError; waitForEnd: true }
      onExited: function(code) {
        if (code !== 0) patch.message = String(actionError.text || "Action failed").trim().replace(/^Error: /, "")
        patch.refresh()
        destroy()
      }
    }
  }

  Timer {
    interval: 2000
    running: patch.active
    repeat: true
    onTriggered: patch.refresh()
  }

  PanelSeparator { foreground: patch.ink }

  Item {
    width: parent.width
    implicitHeight: Math.max(patchHeader.implicitHeight, patchState.implicitHeight)

    PanelSectionHeader {
      id: patchHeader
      text: "PATCH"
      foreground: patch.ink
      fontFamily: patch.face
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
    }

    Text {
      id: patchState
      textFormat: Text.PlainText
      text: !patch.engine ? "SERVICE OFF" : patch.now.length === 0 && patch.chain.length === 0 && patch.open.length === 0 ? "READY" : ""
      color: Qt.darker(patch.ink, 1.4)
      font.family: patch.face
      font.pixelSize: Style.font.caption
      font.bold: true
      font.letterSpacing: 1.2
      elide: Text.ElideRight
      horizontalAlignment: Text.AlignRight
      width: parent.width - patchHeader.implicitWidth - Style.space(12)
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
    }
  }

  // The service is off: one button starts it (audio keeps flowing without
  // it, straight to the output).
  Button {
    visible: !patch.engine
    width: parent.width
    text: "Start the Patch service"
    fontSize: Style.font.bodySmall
    foreground: patch.ink
    fontFamily: patch.face
    bordered: true
    onClicked: patch.launch(["engine", "start"])
  }

  // ---- Now: what runs ----
  Repeater {
    model: patch.now

    Column {
      id: nowRow
      required property var modelData
      width: patch.width
      spacing: Style.space(6)

      readonly property var row: modelData
      readonly property var go: row.go || []
      readonly property var actions: row.actions || []

      Item {
        width: parent.width
        implicitHeight: Math.max(Style.space(30), rowActions.implicitHeight)

        // The row itself: its first window.
        MouseArea {
          anchors.fill: parent
          anchors.rightMargin: rowActions.width + Style.space(6)
          enabled: nowRow.go.length > 0
          cursorShape: enabled ? Qt.PointingHandCursor : Qt.ArrowCursor
          hoverEnabled: true
          onClicked: patch.openUnit(nowRow.go[0].id)

          Rectangle {
            anchors.fill: parent
            radius: Style.space(4)
            color: patch.ink
            opacity: parent.containsMouse ? 0.06 : 0
          }
        }

        Rectangle {
          id: rowDot
          width: Style.space(8)
          height: width
          radius: width / 2
          anchors.left: parent.left
          anchors.leftMargin: Style.space(4)
          anchors.verticalCenter: parent.verticalCenter
          color: nowRow.row.paused ? "transparent"
            : nowRow.row.live ? Color.urgent
            : patch.ink
          border.width: nowRow.row.paused ? 1 : 0
          border.color: Qt.darker(patch.ink, 1.4)
        }

        Text {
          id: rowLabel
          textFormat: Text.PlainText
          text: nowRow.row.label
          color: patch.ink
          font.family: patch.face
          font.pixelSize: Style.font.bodySmall
          font.bold: true
          anchors.left: rowDot.right
          anchors.leftMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
        }

        Text {
          textFormat: Text.PlainText
          text: nowRow.row.detail || ""
          color: Qt.darker(patch.ink, 1.3)
          font.family: patch.face
          font.pixelSize: Style.font.bodySmall
          elide: Text.ElideRight
          anchors.left: rowLabel.right
          anchors.leftMargin: Style.space(8)
          anchors.right: rowActions.left
          anchors.rightMargin: Style.space(8)
          anchors.verticalCenter: parent.verticalCenter
        }

        Row {
          id: rowActions
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.space(6)

          Repeater {
            model: nowRow.actions
            Button {
              required property var modelData
              readonly property bool asking: patch.armed === nowRow.row.id + " " + modelData.do
              text: asking ? modelData.label + "?" : modelData.label
              active: asking
              fontSize: Style.font.bodySmall
              foreground: patch.ink
              fontFamily: patch.face
              bordered: true
              tooltipText: asking ? "Click again to " + modelData.label.toLowerCase() : ""
              onClicked: patch.act(nowRow.row.id, modelData)
            }
          }
        }
      }

      // Its windows: the AI DJ, Deck A, …
      Flow {
        visible: nowRow.go.length > 0
        width: parent.width
        leftPadding: Style.space(20)
        spacing: Style.space(6)

        Repeater {
          model: nowRow.go
          Button {
            required property var modelData
            text: modelData.label
            fontSize: Style.font.bodySmall
            foreground: patch.ink
            fontFamily: patch.face
            bordered: true
            tooltipText: "Open " + modelData.label
            onClicked: patch.openUnit(modelData.id)
          }
        }
      }
    }
  }

  // ---- In the mix: what processes the sound with no window open ----
  // One line, only while something is there; a click opens that plugin's
  // (or the mic's) window, the rest stays out of sight.
  Item {
    visible: patch.chain.length > 0
    width: parent.width
    implicitHeight: chainFlow.implicitHeight

    Text {
      id: chainLabel
      textFormat: Text.PlainText
      text: "In the mix"
      color: Qt.darker(patch.ink, 1.3)
      font.family: patch.face
      font.pixelSize: Style.font.bodySmall
      anchors.left: parent.left
      anchors.leftMargin: Style.space(4)
      y: Math.max(0, (Style.space(30) - implicitHeight) / 2)
    }

    Flow {
      id: chainFlow
      anchors.left: chainLabel.right
      anchors.leftMargin: Style.space(10)
      anchors.right: parent.right
      spacing: Style.space(6)

      Repeater {
        model: patch.chain
        Button {
          required property var modelData
          text: modelData.label
          active: modelData.warn === true
          fontSize: Style.font.bodySmall
          foreground: patch.ink
          fontFamily: patch.face
          bordered: true
          tooltipText: modelData.detail || ""
          onClicked: patch.openUnit(modelData.id)
        }
      }
    }
  }

  // ---- Windows on screen: one line, opened on demand ----
  Column {
    visible: patch.open.length > 0
    width: parent.width
    spacing: Style.space(6)

    Item {
      width: parent.width
      implicitHeight: Math.max(Style.space(30), closeAllButton.implicitHeight)

      MouseArea {
        id: windowsLine
        anchors.fill: parent
        anchors.rightMargin: closeAllButton.width + Style.space(6)
        cursorShape: Qt.PointingHandCursor
        hoverEnabled: true
        onClicked: patch.windowsOpen = !patch.windowsOpen

        Rectangle {
          anchors.fill: parent
          radius: Style.space(4)
          color: patch.ink
          opacity: parent.containsMouse ? 0.06 : 0
        }
      }

      Text {
        id: windowsLabel
        textFormat: Text.PlainText
        text: (patch.windowsOpen ? "⌄ " : "› ") + "Windows"
        color: patch.ink
        font.family: patch.face
        font.pixelSize: Style.font.bodySmall
        font.bold: true
        anchors.left: parent.left
        anchors.leftMargin: Style.space(4)
        anchors.verticalCenter: parent.verticalCenter
      }

      Text {
        textFormat: Text.PlainText
        text: (patch.layoutName ? patch.layoutName + (patch.layoutEdited ? " (changed)" : "") + " · " : "") + patch.windowNames()
        color: Qt.darker(patch.ink, 1.3)
        font.family: patch.face
        font.pixelSize: Style.font.bodySmall
        elide: Text.ElideRight
        anchors.left: windowsLabel.right
        anchors.leftMargin: Style.space(8)
        anchors.right: closeAllButton.left
        anchors.rightMargin: Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
      }

      Button {
        id: closeAllButton
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: "Close all"
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: "Close Patch windows on every workspace · the audio keeps playing"
        onClicked: patch.closeAll()
      }
    }

    // Each window (a click brings it forward), then the layout's own actions.
    Flow {
      visible: patch.windowsOpen
      width: parent.width
      leftPadding: Style.space(20)
      spacing: Style.space(6)

      Repeater {
        model: patch.windowsOpen ? patch.open : []
        Button {
          required property var modelData
          text: modelData.label
          fontSize: Style.font.bodySmall
          foreground: patch.ink
          fontFamily: patch.face
          bordered: true
          tooltipText: "Go to " + modelData.label
          onClicked: patch.openUnit(modelData.id)
        }
      }
    }

    Row {
      id: onScreenRow
      visible: patch.windowsOpen && patch.layout !== "" && !patch.naming
      width: parent.width
      spacing: Style.space(6)

      readonly property int count: patch.layoutCustom ? 5 : 3
      readonly property real cellWidth: (width - spacing * (count - 1)) / count

      Button {
        width: onScreenRow.cellWidth
        text: "Show"
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: "Go to the layout's first workspace"
        onClicked: { patch.launch(["layout", "show"]); patch.launched() }
      }

      Button {
        width: onScreenRow.cellWidth
        text: "Reset"
        enabled: patch.layoutEdited
        opacity: enabled ? 1 : 0.45
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: "Put the windows back as the layout has them"
        onClicked: { patch.launch(["layout", "reset"]); patch.launched() }
      }

      Button {
        visible: patch.layoutCustom
        width: onScreenRow.cellWidth
        text: "Save"
        enabled: patch.layoutEdited
        opacity: enabled ? 1 : 0.45
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: "Save the windows as they are now over " + patch.layoutName
        onClicked: patch.layoutCommand(["save"])
      }

      Button {
        width: onScreenRow.cellWidth
        text: "Save as…"
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: patch.layoutCustom ? "Save the windows as they are now as a new layout" : patch.layoutName + " is built in: save your arrangement under its own name"
        onClicked: patch.startNaming()
      }

      Button {
        visible: patch.layoutCustom
        width: onScreenRow.cellWidth
        text: "Delete"
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: "Delete " + patch.layoutName + " · its windows stay open"
        onClicked: patch.layoutCommand(["delete", patch.layout])
      }
    }

    // Save as…: the name, then Save (or Enter) · Cancel (or Escape).
    Row {
      id: nameRow
      visible: patch.naming
      width: parent.width
      spacing: Style.space(6)

      TextField {
        id: nameField
        width: nameRow.width - nameSave.width - nameCancel.width - nameRow.spacing * 2
        placeholderText: "Name"
        foreground: patch.ink
        font.family: patch.face
        font.pixelSize: Style.font.bodySmall
        Keys.onReturnPressed: nameSave.save()
        Keys.onEnterPressed: nameSave.save()
        Keys.onEscapePressed: patch.naming = false
      }

      Button {
        id: nameSave
        text: "Save"
        enabled: nameField.text.trim() !== ""
        opacity: enabled ? 1 : 0.45
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        function save() { if (enabled) patch.layoutCommand(["save", nameField.text.trim()]) }
        onClicked: save()
      }

      Button {
        id: nameCancel
        text: "Cancel"
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        onClicked: patch.naming = false
      }
    }
  }

  // ---- Open a Unit or layout ----
  TextField {
    id: searchField
    width: parent.width
    placeholderText: "Open a Unit or layout…"
    foreground: patch.ink
    font.family: patch.face
    font.pixelSize: Style.font.bodySmall
    Keys.onReturnPressed: patch.choose(patch.results[patch.selected])
    Keys.onEnterPressed: patch.choose(patch.results[patch.selected])
    Keys.onUpPressed: patch.selected = Math.max(0, patch.selected - 1)
    Keys.onDownPressed: {
      if (patch.results.length > 0) patch.selected = Math.min(patch.results.length - 1, patch.selected + 1)
      else if (patch.keys) patch.keys.forceActiveFocus()
    }
    Keys.onTabPressed: if (patch.keys) patch.keys.forceActiveFocus()
    Keys.onEscapePressed: {
      if (text !== "") text = ""
      else patch.launched()
    }
  }

  // Empty: recent Units, then every layout, one click each. Typing: the
  // matches, Enter opens the highlighted one.
  readonly property var unitPicks: patch.suggest.filter(function (s) { return s.kind !== "layout" })

  Flow {
    visible: searchField.text.trim() === "" && patch.unitPicks.length > 0
    width: parent.width
    spacing: Style.space(6)

    Repeater {
      model: searchField.text.trim() === "" ? patch.unitPicks : []
      Button {
        required property var modelData
        text: modelData.label
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        tooltipText: modelData.kind === "layout" ? "Open the layout " + modelData.label + " (closes the Patch windows on screen)" : "Open " + modelData.label
        onClicked: patch.choose(modelData)
      }
    }
  }

  // The layouts: each opens Patch's windows on the next empty workspace.
  // Four basic ones, then four slots with the layouts opened lately (yours
  // or built-in); the search finds every other one. The user, 2026-11-16:
  // every layout as a button grows too long with many saved layouts.
  Grid {
    id: standardGrid
    visible: searchField.text.trim() === "" && patch.standard.length > 0
    width: parent.width
    columns: width >= Style.space(320) ? 2 : 1
    spacing: Style.space(6)

    readonly property real cellWidth: (width - spacing * (columns - 1)) / columns

    Repeater {
      model: searchField.text.trim() === "" ? patch.standard : []
      Button {
        required property var modelData
        width: standardGrid.cellWidth
        // Long names stay readable at bar-panel widths: the stock Button
        // elides its own label, so give it a wrapping one.
        text: ""
        Accessible.name: label.text
        implicitHeight: Math.max(Style.space(32), label.implicitHeight + Style.space(12))
        Text {
          id: label
          anchors.centerIn: parent
          width: parent.width - Style.space(16)
          text: modelData.label + (patch.layout === modelData.id && patch.layoutEdited ? " •" : "")
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          horizontalAlignment: Text.AlignHCenter
          color: patch.ink
          font.family: patch.face
          font.pixelSize: Style.font.bodySmall
        }
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        active: patch.layout === modelData.id
        tooltipText: (modelData.custom ? "Your layout · " : "") + "Open " + modelData.label + " (closes the Patch windows on screen)"
        onClicked: patch.loadLayout(modelData.id)
      }
    }
  }

  Text {
    visible: searchField.text.trim() === "" && patch.slots.length > 0
    text: "RECENT LAYOUTS"
    color: Qt.darker(patch.ink, 1.4)
    font.family: patch.face
    font.pixelSize: Style.font.caption
    font.bold: true
    font.letterSpacing: 1.2
  }

  Grid {
    id: slotGrid
    visible: searchField.text.trim() === "" && patch.slots.length > 0
    width: parent.width
    columns: width >= Style.space(320) ? 2 : 1
    spacing: Style.space(6)

    readonly property real cellWidth: (width - spacing * (columns - 1)) / columns

    Repeater {
      model: searchField.text.trim() === "" ? patch.slots : []
      Button {
        required property var modelData
        width: slotGrid.cellWidth
        // Long names stay readable at bar-panel widths: the stock Button
        // elides its own label, so give it a wrapping one.
        text: ""
        Accessible.name: label.text
        implicitHeight: Math.max(Style.space(32), label.implicitHeight + Style.space(12))
        Text {
          id: label
          anchors.centerIn: parent
          width: parent.width - Style.space(16)
          text: modelData.label + (patch.layout === modelData.id && patch.layoutEdited ? " •" : "")
          textFormat: Text.PlainText
          wrapMode: Text.Wrap
          horizontalAlignment: Text.AlignHCenter
          color: patch.ink
          font.family: patch.face
          font.pixelSize: Style.font.bodySmall
        }
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        bordered: true
        active: patch.layout === modelData.id
        tooltipText: (modelData.custom ? "Your layout · " : "") + "Open " + modelData.label + " (closes the Patch windows on screen)"
        onClicked: patch.loadLayout(modelData.id)
      }
    }
  }

  Column {
    visible: searchField.text.trim() !== ""
    width: parent.width
    spacing: Style.space(4)

    Repeater {
      model: patch.results
      Button {
        required property var modelData
        required property int index
        width: parent.width
        text: modelData.label + (modelData.kind === "layout" ? "  · layout" : modelData.hint ? "  · " + modelData.hint : "")
        leftAlign: true
        selected: index === patch.selected
        fontSize: Style.font.bodySmall
        foreground: patch.ink
        fontFamily: patch.face
        onClicked: patch.choose(modelData)
      }
    }

    Text {
      visible: patch.results.length === 0
      textFormat: Text.PlainText
      text: "Nothing called that"
      color: Qt.darker(patch.ink, 1.4)
      font.family: patch.face
      font.pixelSize: Style.font.caption
    }
  }

  // ---- Master EQ: step through the presets, or bypass ----
  Row {
    id: eqRow
    visible: patch.engine
    width: parent.width
    spacing: Style.space(6)

    Button {
      id: eqPrev
      text: "‹"
      fontSize: Style.font.bodySmall
      foreground: patch.ink
      fontFamily: patch.face
      bordered: true
      tooltipText: "Previous EQ preset"
      onClicked: patch.nextPreset(-1)
    }

    Button {
      width: eqRow.width - eqPrev.width - eqNext.width - eqBypass.width - eqRow.spacing * 3
      text: "EQ  " + patch.eqText()
      fontSize: Style.font.bodySmall
      foreground: patch.ink
      fontFamily: patch.face
      bordered: true
      leftAlign: true
      tooltipText: "Open the EQ"
      onClicked: patch.openUnit("eq")
    }

    Button {
      id: eqNext
      text: "›"
      fontSize: Style.font.bodySmall
      foreground: patch.ink
      fontFamily: patch.face
      bordered: true
      tooltipText: "Next EQ preset"
      onClicked: patch.nextPreset(1)
    }

    Button {
      id: eqBypass
      text: "Bypass"
      fontSize: Style.font.bodySmall
      foreground: patch.ink
      fontFamily: patch.face
      bordered: true
      active: patch.bypass
      tooltipText: patch.bypass ? "The EQ is bypassed" : "Hear the program without the EQ"
      onClicked: {
        patch.bypass = !patch.bypass
        patch.run(["eq", "master", "bypass", patch.bypass ? "on" : "off"])
      }
    }
  }

  Button {
    visible: patch.engine && patch.inputSelection === "patch.input"
    width: parent.width
    text: patch.captureMain ? "Patch input: Main mix" : "Patch input: Inputs"
    fontSize: Style.font.bodySmall
    foreground: patch.ink
    fontFamily: patch.face
    bordered: true
    onClicked: patch.run(["input", "mix", patch.captureMain ? "inputs" : "main"])
  }

  Text {
    visible: patch.message !== ""
    textFormat: Text.PlainText
    text: patch.message
    color: Qt.darker(patch.ink, 1.2)
    font.family: patch.face
    font.pixelSize: Style.font.caption
    wrapMode: Text.Wrap
    width: parent.width
  }
}
