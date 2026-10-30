pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// omascreencast — bar widget entry point and popup host.
//
// M1: lists every connected display (physical + virtual) and the discovered
// Chromecast devices, with a manual-IP fallback and a dependency `doctor`
// report. Cast controls land in M2/M3 and the arranger canvas in M4.
//
// The shared state lives in the `service` singleton, injected by the shell
// (shell.qml: `if ("service" in item) item.service = ...`), so discovery keeps
// running while the popup is closed.
Panel {
  id: root
  moduleName: "tech.alatcerdas.omascreencast"
  ipcTarget: "omascreencast"
  manageIpc: false

  // Injected by the shell; null only if the service entry failed to load.
  property var service: null

  // Read from this widget's inline shell.json entry (see Ui/BarWidget.setting).
  readonly property string codec: setting("codec", "auto")
  readonly property string resolution: setting("resolution", "1920x1080@60")
  readonly property int fps: setting("fps", 30)
  readonly property int bitrateMbps: setting("bitrateMbps", 6)
  readonly property string audio: setting("audio", "Off")

  // Tunables handed to session.py create (M3).
  function connectOptions() {
    return {
      res: root.resolution,
      fps: root.fps,
      bitrate: String(root.bitrateMbps) + "M",
      codec: root.codec
    }
  }

  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground

  // --- state mirrored from the service (guarded for a missing service) ---
  readonly property var monitors: service ? service.monitors : []
  readonly property var devices: service ? service.devices : []
  readonly property bool discovering: service ? service.discovering : false
  readonly property var doctor: service ? service.doctor : ({})
  readonly property string lastError: service ? service.lastError : ""
  readonly property bool depsOk: doctor.ok === true
  readonly property var activeSession: service ? service.activeSession : null
  readonly property var sessionOrphans: service ? service.sessionOrphans : []
  readonly property bool sessionBusy: service ? service.sessionBusy : false
  readonly property bool sessionActive: activeSession !== null
    && String(activeSession.phase || "") !== "stopped"
  readonly property var displayRules: service ? service.displayRules : []
  readonly property string displayError: service ? service.displayError : ""
  readonly property bool displayBusy: service ? service.displayBusy : false
  readonly property bool provisioning: service ? service.provisioning : false
  readonly property var overlappingPairs: Model.overlappingDisplays(monitors)

  readonly property string stateIcon: {
    if (lastError !== "") return "󰅚"
    if (activeSession !== null) return "󰒍"
    if (discovering) return "󰑐"
    if (devices.length > 0) return "󰍺"
    return "󰍹"
  }

  readonly property string statusText: {
    if (lastError !== "") return lastError.toUpperCase()
    if (activeSession !== null) {
      const target = Model.sessionTargetLabel(activeSession, devices)
      let line = Model.sessionPhaseLabel(activeSession) + " " + activeSession.name
      if (target !== "") line += " → " + target
      return line.toUpperCase()
    }
    if (discovering) return "SEARCHING…"
    var parts = []
    parts.push(monitors.length + (monitors.length === 1 ? " display" : " displays"))
    parts.push(devices.length + (devices.length === 1 ? " device" : " devices"))
    return parts.join(" · ").toUpperCase()
  }

  // True when this discovered device is the one the active session casts to.
  function isActiveDevice(device) {
    return Model.sessionMatchesDevice(activeSession, device)
  }

  // --- cursor model (keyboard + mouse share one highlight) ---------------
  // Sections: "displays" (only when a display exists) and "devices".
  readonly property var visibleSections: {
    var list = []
    if (monitors.length > 0) list.push("displays")
    list.push("devices")
    return list
  }

  property string focusSection: "devices"
  property int selectedIndex: 0
  property bool cursorActive: false

  function sectionCount(section) {
    if (section === "displays") return monitors.length
    if (section === "devices") return devices.length
    return 0
  }

  function moveCursor(delta) {
    var sections = visibleSections
    if (!sections || sections.length === 0) return
    var sIdx = sections.indexOf(focusSection)
    if (sIdx < 0) {
      focusSection = sections[0]
      selectedIndex = 0
      return
    }
    var max = sectionCount(focusSection) - 1
    if (delta > 0) {
      if (selectedIndex < max) { selectedIndex = selectedIndex + 1; return }
      if (sIdx < sections.length - 1) { focusSection = sections[sIdx + 1]; selectedIndex = 0 }
    } else {
      if (selectedIndex > 0) { selectedIndex = selectedIndex - 1; return }
      if (sIdx > 0) {
        let prev = sections[sIdx - 1]
        focusSection = prev
        selectedIndex = Math.max(0, sectionCount(prev) - 1)
      }
    }
  }

  function clampCursor() {
    var sections = visibleSections
    if (!sections || !sections.length) return
    if (sections.indexOf(focusSection) < 0) {
      focusSection = sections[0]
      selectedIndex = 0
      return
    }
    var count = sectionCount(focusSection)
    if (count === 0) {
      let sIdx = sections.indexOf(focusSection)
      focusSection = sIdx > 0 ? sections[sIdx - 1] : sections[0]
      selectedIndex = 0
      return
    }
    if (selectedIndex > count - 1) selectedIndex = count - 1
    if (selectedIndex < 0) selectedIndex = 0
  }

  function ensureCursorVisible(item) {
    if (!item || !scrollArea) return
    var flick = scrollArea.contentItem
    if (!flick || flick.contentY === undefined) return
    var pt = item.mapToItem(flick.contentItem || flick, 0, 0)
    var top = pt.y
    var bottom = top + (item.height || 0)
    var viewTop = flick.contentY
    var viewBottom = viewTop + flick.height
    var margin = 6
    if (top < viewTop + margin) flick.contentY = Math.max(0, top - margin)
    else if (bottom > viewBottom - margin) flick.contentY = bottom + margin - flick.height
  }

  onMonitorsChanged: clampCursor()
  onDevicesChanged: clampCursor()
  onVisibleSectionsChanged: clampCursor()

  // Refresh the display list and re-sweep discovery whenever the panel opens.
  onOpenedChanged: {
    if (!opened) return
    if (service) {
      service.refreshMonitors()
      service.refreshDisplayRules()
      service.refreshDevices()
    }
    cursorActive = false
  }

  // --- arranger actions (M4) --------------------------------------------
  function displayFields(display) {
    return Model.displayFieldsFor(display, root.displayRules)
  }

  function withField(fields, key, value) {
    var out = {}
    for (var k in fields) out[k] = fields[k]
    out[key] = value
    return out
  }

  function applyDisplay(display, fields) {
    if (!service || !display) return
    service.applyDisplayRule(Model.displayRulePayload(display, fields))
  }

  function saveDisplay(display, fields) {
    if (!service || !display) return
    service.saveDisplayRule(Model.displayRulePayload(display, fields))
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.stateIcon
    tooltipText: "Omascreencast — " + root.statusText
    onPressed: function(b) { root.toggle() }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(panelColumn.implicitHeight, Style.space(560))

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent

      // While typing an IP the keys belong to the field, not the cursor.
      blocked: manualField.activeFocus

      onMoveRequested: function(dx, dy) {
        if (!root.cursorActive) { root.cursorActive = true; return }
        if (dy !== 0) root.moveCursor(dy)
      }
      onActivateRequested: {
        if (!root.cursorActive) return
        if (root.focusSection !== "devices" || !root.service) return
        var device = root.devices[root.selectedIndex]
        if (!device) return
        if (root.isActiveDevice(device)) root.service.disconnect()
        else root.service.connectDevice(device, root.connectOptions())
      }
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      ScrollView {
        id: scrollArea
        anchors.fill: parent
        clip: true
        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff
        ScrollBar.vertical.policy: panelColumn.implicitHeight > height ? ScrollBar.AsNeeded : ScrollBar.AlwaysOff
        Binding {
          target: scrollArea.contentItem
          property: "interactive"
          value: panelColumn.implicitHeight > scrollArea.height
        }

        Column {
          id: panelColumn
          width: scrollArea.availableWidth
          spacing: Style.space(14)

          // ---------- Hero ----------
          Item {
            width: parent.width
            implicitHeight: Math.max(heroIcon.implicitHeight, heroLabels.implicitHeight)

            Text {
              id: heroIcon
              textFormat: Text.PlainText
              text: root.stateIcon
              color: root.lastError !== "" ? Color.urgent : root.bar.foreground
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.display
              anchors.left: parent.left
              anchors.verticalCenter: parent.verticalCenter
            }

            Column {
              id: heroLabels
              anchors.left: heroIcon.right
              anchors.leftMargin: Style.space(14)
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              spacing: Style.space(2)

              Text {
                textFormat: Text.PlainText
                text: "Omascreencast"
                color: root.bar.foreground
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.title
                font.bold: true
                elide: Text.ElideRight
                width: parent.width
              }

              Text {
                textFormat: Text.PlainText
                text: root.statusText
                color: root.lastError !== "" ? Color.urgent : Qt.darker(root.bar.foreground, 1.4)
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                font.letterSpacing: 1.2
                elide: Text.ElideRight
                width: parent.width
              }
            }
          }

          // ---------- Displays ----------
          PanelSeparator { foreground: root.bar.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            PanelSectionHeader {
              text: "DISPLAYS"
              foreground: root.bar.foreground
              fontFamily: root.bar.fontFamily
            }

            Text {
              textFormat: Text.PlainText
              visible: root.monitors.length === 0
              width: parent.width
              text: "No displays reported by Hyprland."
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            // Drag displays to set their layout position; edges snap to
            // neighbouring displays (M4).
            ArrangementCanvas {
              id: arrangement
              visible: root.monitors.length > 0
              width: parent.width
            }

            Text {
              textFormat: Text.PlainText
              visible: root.overlappingPairs.length > 0
              width: parent.width
              text: "Overlapping displays: " + root.overlappingPairs.map(function(pair) {
                return pair.join(" / ")
              }).join(", ")
              color: Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.displayError !== ""
              width: parent.width
              text: root.displayError
              color: Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Row {
              width: parent.width
              spacing: Style.space(8)
              visible: root.sessionOrphans.length > 0

              Text {
                textFormat: Text.PlainText
                width: parent.width - orphanButton.width - parent.spacing
                text: "Untracked virtual output"
                  + (root.sessionOrphans.length === 1 ? ": " : "s: ")
                  + root.sessionOrphans.join(", ")
                color: Color.urgent
                font.family: root.bar.fontFamily
                font.pixelSize: Style.font.bodySmall
                wrapMode: Text.Wrap
              }

              Button {
                id: orphanButton
                text: "Clean up"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                horizontalPadding: Style.spacing.sm
                verticalPadding: Style.spacing.controlPaddingY
                enabled: !root.sessionBusy
                onClicked: if (root.service) root.service.cleanupOrphan(root.sessionOrphans[0])
              }
            }

            Repeater {
              model: root.monitors

              MonitorRow {
                required property var modelData
                required property int index

                width: panelColumn.width
                display: modelData
                rowIndex: index
              }
            }
          }

          // ---------- Chromecast devices ----------
          PanelSeparator { foreground: root.bar.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            Item {
              width: parent.width
              implicitHeight: Math.max(devicesHeader.implicitHeight, refreshButton.implicitHeight)

              PanelSectionHeader {
                id: devicesHeader
                text: "CHROMECAST"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
              }

              Button {
                id: refreshButton
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                iconText: "󰑐"
                iconSpinning: root.discovering
                tooltipText: "Search again"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                horizontalPadding: Style.spacing.sm
                verticalPadding: Style.spacing.controlPaddingY
                onClicked: if (root.service) root.service.refreshDevices()
              }
            }

            Text {
              textFormat: Text.PlainText
              visible: root.devices.length === 0
              width: parent.width
              text: root.discovering
                ? "Searching the local network…"
                : "No Chromecast devices found. Make sure the TV is on and on the same network, or add its IP below."
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Repeater {
              model: root.devices

              DeviceRow {
                required property var modelData
                required property int index

                width: panelColumn.width
                device: modelData
                rowIndex: index
              }
            }
          }

          // ---------- Manual IP ----------
          PanelSeparator { foreground: root.bar.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)

            PanelSectionHeader {
              text: "MANUAL IP"
              foreground: root.bar.foreground
              fontFamily: root.bar.fontFamily
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: "For mDNS-blocked or cross-VLAN receivers."
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Row {
              width: parent.width
              spacing: Style.space(8)

              TextField {
                id: manualField
                width: parent.width - addButton.width - parent.spacing
                placeholderText: "192.168.1.50"
                foreground: root.bar.foreground
                font.family: root.bar.fontFamily
                onAccepted: root.submitManual()
              }

              Button {
                id: addButton
                text: "Add"
                iconText: "󰐕"
                bordered: true
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                onClicked: root.submitManual()
              }
            }
          }

          // ---------- Doctor ----------
          PanelSeparator { foreground: root.bar.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)

            Item {
              width: parent.width
              implicitHeight: Math.max(doctorHeader.implicitHeight, doctorActions.implicitHeight)

              PanelSectionHeader {
                id: doctorHeader
                text: "DOCTOR"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
              }

              Row {
                id: doctorActions
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.space(6)

                Button {
                  id: doctorSetup
                  text: root.provisioning ? "Setting up…" : "Set up"
                  iconText: "󰏗"
                  bordered: true
                  enabled: !root.provisioning
                  opacity: enabled ? 1 : 0.45
                  foreground: root.bar.foreground
                  fontFamily: root.bar.fontFamily
                  horizontalPadding: Style.spacing.sm
                  verticalPadding: Style.spacing.controlPaddingY
                  onClicked: if (root.service) root.service.provisionDependencies()
                }

                Button {
                  id: doctorRerun
                  text: "Re-run"
                  iconText: "󰑐"
                  foreground: root.bar.foreground
                  fontFamily: root.bar.fontFamily
                  horizontalPadding: Style.spacing.sm
                  verticalPadding: Style.spacing.controlPaddingY
                  onClicked: if (root.service) root.service.runDoctor()
                }
              }
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: root.depsOk
                ? "All dependencies present."
                : (root.doctor.pythonOk === true
                   ? "Capture tools are missing — see below."
                   : "Python dependencies are missing — press Set up.")
              color: root.depsOk ? Qt.darker(root.bar.foreground, 1.3) : Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              font.bold: true
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.missingBinaries && root.doctor.missingBinaries.length > 0
              width: parent.width
              text: "Missing tools: " + (root.doctor.missingBinaries || []).join(", ")
              color: Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.missingPython && root.doctor.missingPython.length > 0
              width: parent.width
              text: "Missing Python modules: " + (root.doctor.missingPython || []).join(", ")
              color: Color.urgent
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.venv !== ""
              width: parent.width
              text: "Private venv: " + (root.doctor.venvReady ? "ready" : (root.doctor.venvExists ? "present (incomplete)" : "not created"))
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.fastPath !== ""
              width: parent.width
              text: "Fast path (optional, root): " + root.doctor.fastPath
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.firewallRule !== ""
              width: parent.width
              text: "Stream port " + root.doctor.streamPort + " must be reachable. If blocked: " + root.doctor.firewallRule
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }
          }

          Item {
            width: parent.width
            height: Style.space(4)
          }
        }
      }
    }
  }

  function submitManual() {
    if (!service) return
    var host = String(manualField.text || "").trim()
    if (host === "") return
    service.addManualHost(host)
    manualField.text = ""
  }

  // Scrollable-free spatial preview of the layout: drag a display to move it.
  component ArrangementCanvas: Rectangle {
    id: canvas
    readonly property var bounds: Model.arrangementBounds(root.monitors)
    readonly property real pad: Style.space(10)
    readonly property real factor: Math.min(
      Math.max(1, width - pad * 2) / bounds.width,
      Math.max(1, implicitHeight - pad * 2) / bounds.height)
    readonly property real offX: pad + (Math.max(1, width - pad * 2) - bounds.width * factor) / 2
    readonly property real offY: pad + (Math.max(1, implicitHeight - pad * 2) - bounds.height * factor) / 2
    implicitHeight: Style.space(150)
    color: "transparent"
    border.width: 1
    border.color: Qt.darker(root.bar.foreground, 2.4)
    radius: Style.space(6)
    clip: true

    function pxX(logicalX) { return offX + (Number(logicalX) - bounds.minX) * factor }
    function pxY(logicalY) { return offY + (Number(logicalY) - bounds.minY) * factor }

    Repeater {
      model: root.monitors

      Rectangle {
        id: card
        required property var modelData
        required property int index

        property bool dragging: false
        property real dragX: Number(modelData.x || 0)
        property real dragY: Number(modelData.y || 0)
        readonly property var size: Model.monitorLogicalSize(modelData)
        readonly property bool isVirtual: modelData.virtual === true
        readonly property bool isDisabled: modelData.disabled === true

        x: Math.round(canvas.pxX(dragging ? dragX : modelData.x))
        y: Math.round(canvas.pxY(dragging ? dragY : modelData.y))
        width: Math.max(Style.space(24), size.width * canvas.factor)
        height: Math.max(Style.space(16), size.height * canvas.factor)
        radius: Style.space(3)
        opacity: isDisabled ? 0.35 : 1
        color: isVirtual ? Qt.darker(Commons.Color.accent, 2.6) : "transparent"
        border.width: 1
        border.color: isVirtual ? Commons.Color.accent : root.bar.foreground

        Text {
          anchors.fill: parent
          anchors.margins: Style.space(2)
          textFormat: Text.PlainText
          text: String(modelData.name || "")
          color: root.bar.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
          horizontalAlignment: Text.AlignHCenter
          verticalAlignment: Text.AlignVCenter
        }

        MouseArea {
          anchors.fill: parent
          enabled: !card.isDisabled
          cursorShape: Qt.SizeAllCursor
          property real pressX: 0
          property real pressY: 0
          property real startLogicalX: 0
          property real startLogicalY: 0

          onPressed: function(mouse) {
            var point = mapToItem(canvas, mouse.x, mouse.y)
            pressX = point.x
            pressY = point.y
            startLogicalX = Number(modelData.x || 0)
            startLogicalY = Number(modelData.y || 0)
            card.dragging = true
            card.dragX = startLogicalX
            card.dragY = startLogicalY
          }

          onPositionChanged: function(mouse) {
            if (!pressed) return
            var point = mapToItem(canvas, mouse.x, mouse.y)
            var logicalX = startLogicalX + (point.x - pressX) / canvas.factor
            var logicalY = startLogicalY + (point.y - pressY) / canvas.factor
            var snapped = Model.snapPosition(modelData, logicalX, logicalY, root.monitors, 16)
            card.dragX = snapped.x
            card.dragY = snapped.y
          }

          onReleased: {
            if (!card.dragging) return
            card.dragging = false
            var fields = root.displayFields(modelData)
            fields = root.withField(fields, "position",
              Math.round(card.dragX) + "x" + Math.round(card.dragY))
            root.applyDisplay(modelData, fields)
          }
        }
      }
    }
  }

  component MonitorRow: CursorSurface {
    id: monitorRow
    required property var display
    required property int rowIndex

    property bool expanded: false
    property var edits: Model.displayFieldsFor(display, root.displayRules)

    readonly property bool isFocused: display && display.focused
    readonly property bool isVirtual: display && display.virtual
    readonly property string castSuffix: Model.monitorSessionLabel(display, root.activeSession, root.devices)
    readonly property bool castTarget: castSuffix !== ""
    readonly property var modeChoices: {
      var list = Model.monitorModes(display)
      var mode = String(edits.mode || "")
      if (mode !== "" && list.indexOf(mode) < 0) list.unshift(mode)
      return list
    }
    readonly property var mirrorChoices: {
      var list = ["None"]
      for (var i = 0; i < root.monitors.length; i++) {
        var other = root.monitors[i]
        if (String(other.name) !== String(display.name)) list.push(String(other.name))
      }
      return list
    }
    readonly property var scaleChoices: {
      var list = ["0.75", "1", "1.25", "1.5", "1.75", "2", "2.5", "3"]
      var current = String(edits.scale)
      if (current !== "" && list.indexOf(current) < 0) list.unshift(current)
      return list
    }
    readonly property var orientationChoices: ["Normal", "90°", "180°", "270°"]

    function resetEdits() { edits = Model.displayFieldsFor(display, root.displayRules) }
    function update(key, value) { edits = root.withField(edits, key, value) }

    onDisplayChanged: resetEdits()
    onExpandedChanged: if (expanded) resetEdits()

    hasCursor: root.cursorActive && root.focusSection === "displays" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.ensureCursorVisible(monitorRow)
    current: isFocused
    foreground: root.bar.foreground
    fill: Style.hoverFillFor(root.bar.foreground, Commons.Color.accent)
    currentFill: Style.selectedFillFor(root.bar.foreground, Commons.Color.accent)
    implicitHeight: monitorColumn.implicitHeight + Style.spacing.xl

    Column {
      id: monitorColumn
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.top: parent.top
      anchors.topMargin: Style.space(6)
      anchors.leftMargin: Style.space(6)
      anchors.rightMargin: Style.space(6)
      spacing: Style.space(8)

      Row {
        id: monitorInner
        width: parent.width
        spacing: Style.space(8)

        Text {
          textFormat: Text.PlainText
          text: monitorRow.isVirtual ? "󰒍" : "󰍹"
          color: root.bar.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.title
          width: Style.space(22)
          horizontalAlignment: Text.AlignHCenter
          anchors.verticalCenter: parent.verticalCenter
        }

        Text {
          textFormat: Text.PlainText
          text: Model.monitorLabel(monitorRow.display)
            + monitorRow.castSuffix
            + (monitorRow.isFocused ? " · focused" : "")
          color: monitorRow.castTarget ? Commons.Color.accent : root.bar.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.body
          elide: Text.ElideRight
          width: parent.width - Style.space(22) - Style.space(14) - Style.space(34)
            - arrangeButton.width - Style.space(24)
          anchors.verticalCenter: parent.verticalCenter
        }

        Text {
          textFormat: Text.PlainText
          text: monitorRow.castTarget ? "CAST" : (monitorRow.isVirtual ? "NET" : "")
          visible: monitorRow.castTarget || monitorRow.isVirtual
          color: Commons.Color.accent
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          width: Style.space(30)
          horizontalAlignment: Text.AlignRight
          anchors.verticalCenter: parent.verticalCenter
        }

        Button {
          id: arrangeButton
          text: monitorRow.expanded ? "Close" : "Arrange"
          bordered: true
          foreground: root.bar.foreground
          fontFamily: root.bar.fontFamily
          horizontalPadding: Style.spacing.sm
          verticalPadding: Style.spacing.controlPaddingY
          anchors.verticalCenter: parent.verticalCenter
          onClicked: monitorRow.expanded = !monitorRow.expanded
        }
      }

      Column {
        id: controls
        width: parent.width
        visible: monitorRow.expanded
        spacing: Style.space(6)

        Row {
          width: parent.width
          spacing: Style.space(8)

          Column {
            spacing: Style.space(2)
            Text {
              textFormat: Text.PlainText
              text: "Mode"
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
            }
            ComboBox {
              width: Style.space(160)
              model: monitorRow.modeChoices
              currentIndex: Math.max(0, monitorRow.modeChoices.indexOf(String(monitorRow.edits.mode)))
              font.family: root.bar.fontFamily
              onActivated: monitorRow.update("mode", currentText)
            }
          }

          Column {
            spacing: Style.space(2)
            Text {
              textFormat: Text.PlainText
              text: "Scale"
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
            }
            ComboBox {
              width: Style.space(78)
              model: monitorRow.scaleChoices
              currentIndex: Math.max(0, monitorRow.scaleChoices.indexOf(String(monitorRow.edits.scale)))
              font.family: root.bar.fontFamily
              onActivated: monitorRow.update("scale", Number(currentText))
            }
          }

          Column {
            spacing: Style.space(2)
            Text {
              textFormat: Text.PlainText
              text: "Rotate"
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
            }
            ComboBox {
              width: Style.space(96)
              model: monitorRow.orientationChoices
              currentIndex: Math.max(0, Math.min(3, Number(monitorRow.edits.transform || 0)))
              font.family: root.bar.fontFamily
              onActivated: monitorRow.update("transform", currentIndex)
            }
          }
        }

        Row {
          width: parent.width
          spacing: Style.space(8)

          Column {
            spacing: Style.space(2)
            Text {
              textFormat: Text.PlainText
              text: "Mirror"
              color: Qt.darker(root.bar.foreground, 1.4)
              font.family: root.bar.fontFamily
              font.pixelSize: Style.font.caption
            }
            ComboBox {
              width: Style.space(160)
              model: monitorRow.mirrorChoices
              currentIndex: {
                var mirror = String(monitorRow.edits.mirror || "")
                var idx = monitorRow.mirrorChoices.indexOf(mirror === "" ? "None" : mirror)
                return idx < 0 ? 0 : idx
              }
              font.family: root.bar.fontFamily
              onActivated: monitorRow.update("mirror", currentIndex === 0 ? "" : currentText)
            }
          }

          Button {
            text: monitorRow.edits.disabled ? "Disabled" : "Enabled"
            bordered: true
            foreground: root.bar.foreground
            fontFamily: root.bar.fontFamily
            horizontalPadding: Style.spacing.sm
            verticalPadding: Style.spacing.controlPaddingY
            anchors.bottom: parent.bottom
            onClicked: monitorRow.update("disabled", monitorRow.edits.disabled !== true)
          }
        }

        Row {
          width: parent.width
          spacing: Style.space(8)

          Text {
            textFormat: Text.PlainText
            width: parent.width - applyButton.width - saveButton.width - Style.space(16)
            text: "Position " + String(monitorRow.edits.position || "")
            color: Qt.darker(root.bar.foreground, 1.4)
            font.family: root.bar.fontFamily
            font.pixelSize: Style.font.caption
            elide: Text.ElideRight
            anchors.verticalCenter: parent.verticalCenter
          }

          Button {
            id: applyButton
            text: "Apply"
            iconText: "󰄬"
            bordered: true
            enabled: !root.displayBusy
            opacity: enabled ? 1 : 0.45
            foreground: root.bar.foreground
            fontFamily: root.bar.fontFamily
            horizontalPadding: Style.spacing.sm
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.applyDisplay(monitorRow.display, monitorRow.edits)
          }

          Button {
            id: saveButton
            text: "Save"
            iconText: "󰆓"
            bordered: true
            enabled: !root.displayBusy
            opacity: enabled ? 1 : 0.45
            foreground: root.bar.foreground
            fontFamily: root.bar.fontFamily
            horizontalPadding: Style.spacing.sm
            verticalPadding: Style.spacing.controlPaddingY
            onClicked: root.saveDisplay(monitorRow.display, monitorRow.edits)
          }
        }

        Text {
          textFormat: Text.PlainText
          width: parent.width
          visible: Model.displayRuleFor(monitorRow.display, root.displayRules) === null
          text: monitorRow.isVirtual
            ? "Not saved yet — use Save to persist this virtual output's layout."
            : "No saved rule — Apply is temporary; Save writes monitors.lua (backed up)."
          color: Qt.darker(root.bar.foreground, 1.4)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          wrapMode: Text.Wrap
        }
      }
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      acceptedButtons: Qt.NoButton
      cursorShape: Qt.ArrowCursor
      onContainsMouseChanged: if (containsMouse) {
        root.cursorActive = true
        root.focusSection = "displays"
        root.selectedIndex = monitorRow.rowIndex
      }
    }
  }

  component DeviceRow: CursorSurface {
    id: deviceRow
    required property var device
    required property int rowIndex

    readonly property bool isActive: root.isActiveDevice(device)

    hasCursor: root.cursorActive && root.focusSection === "devices" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.ensureCursorVisible(deviceRow)
    foreground: root.bar.foreground
    fill: Style.hoverFillFor(root.bar.foreground, Commons.Color.accent)
    currentFill: Style.selectedFillFor(root.bar.foreground, Commons.Color.accent)
    implicitHeight: deviceInner.implicitHeight + Style.spacing.xl

    Row {
      id: deviceInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(6)
      anchors.rightMargin: Style.space(6)
      spacing: Style.space(8)

      Text {
        textFormat: Text.PlainText
        text: "󰐊"
        color: deviceRow.device.online ? root.bar.foreground : Qt.darker(root.bar.foreground, 1.6)
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.title
        width: Style.space(22)
        horizontalAlignment: Text.AlignHCenter
        anchors.verticalCenter: parent.verticalCenter
      }

      Column {
        width: parent.width - Style.space(22) - Style.space(8) - codecText.width
          - Style.space(8) - connectButton.width - Style.space(8)
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.space(1)

        Text {
          textFormat: Text.PlainText
          width: parent.width
          text: Model.deviceLabel(deviceRow.device)
          color: root.bar.foreground
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.body
          elide: Text.ElideRight
        }

        Text {
          textFormat: Text.PlainText
          width: parent.width
          visible: deviceRow.device.model !== "" && deviceRow.device.model !== deviceRow.device.name
          text: deviceRow.device.model
          color: Qt.darker(root.bar.foreground, 1.4)
          font.family: root.bar.fontFamily
          font.pixelSize: Style.font.caption
          elide: Text.ElideRight
        }
      }

      Text {
        id: codecText
        textFormat: Text.PlainText
        text: Model.deviceCodecLabel(deviceRow.device)
        color: Qt.darker(root.bar.foreground, 1.3)
        font.family: root.bar.fontFamily
        font.pixelSize: Style.font.caption
        anchors.verticalCenter: parent.verticalCenter
      }

      Button {
        id: connectButton
        text: deviceRow.isActive ? "Stop" : "Cast"
        iconText: deviceRow.isActive ? "󰓛" : "󰐊"
        bordered: true
        foreground: root.bar.foreground
        fontFamily: root.bar.fontFamily
        horizontalPadding: Style.spacing.sm
        verticalPadding: Style.spacing.controlPaddingY
        enabled: !root.sessionBusy && (!root.sessionActive || deviceRow.isActive)
        opacity: enabled ? 1 : 0.45
        anchors.verticalCenter: parent.verticalCenter
        onClicked: {
          if (!root.service) return
          if (deviceRow.isActive) root.service.disconnect()
          else root.service.connectDevice(deviceRow.device, root.connectOptions())
        }
      }
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
      // Hover drives the shared cursor; clicks belong to the Cast/Stop button.
      acceptedButtons: Qt.NoButton
      onContainsMouseChanged: if (containsMouse) {
        root.cursorActive = true
        root.focusSection = "devices"
        root.selectedIndex = deviceRow.rowIndex
      }
    }
  }
}
