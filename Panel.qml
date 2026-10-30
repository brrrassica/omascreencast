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
  readonly property int fps: setting("fps", 30)
  readonly property int bitrateMbps: setting("bitrateMbps", 6)
  readonly property string audio: setting("audio", "Off")

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
      service.refreshDevices()
    }
    cursorActive = false
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
        else root.service.connectDevice(device)
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
              implicitHeight: Math.max(doctorHeader.implicitHeight, doctorRerun.implicitHeight)

              PanelSectionHeader {
                id: doctorHeader
                text: "DOCTOR"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
              }

              Button {
                id: doctorRerun
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: "Re-run"
                iconText: "󰑐"
                foreground: root.bar.foreground
                fontFamily: root.bar.fontFamily
                horizontalPadding: Style.spacing.sm
                verticalPadding: Style.spacing.controlPaddingY
                onClicked: if (root.service) root.service.runDoctor()
              }
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: root.depsOk ? "All dependencies present." : "Some dependencies are missing."
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

  component MonitorRow: CursorSurface {
    id: monitorRow
    required property var display
    required property int rowIndex

    readonly property bool isFocused: display && display.focused
    readonly property bool isVirtual: display && display.virtual
    readonly property string castSuffix: Model.monitorSessionLabel(display, root.activeSession, root.devices)
    readonly property bool castTarget: castSuffix !== ""

    hasCursor: root.cursorActive && root.focusSection === "displays" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.ensureCursorVisible(monitorRow)
    current: isFocused
    foreground: root.bar.foreground
    fill: Style.hoverFillFor(root.bar.foreground, Commons.Color.accent)
    currentFill: Style.selectedFillFor(root.bar.foreground, Commons.Color.accent)
    implicitHeight: monitorInner.implicitHeight + Style.spacing.xl

    Row {
      id: monitorInner
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(6)
      anchors.rightMargin: Style.space(6)
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
        width: parent.width - Style.space(22) - Style.space(14) - Style.space(16)
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
    }

    MouseArea {
      anchors.fill: parent
      hoverEnabled: true
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
          else root.service.connectDevice(deviceRow.device)
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
