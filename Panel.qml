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
  readonly property string language: setting("language", "auto")
  readonly property string deviceNickname: setting("deviceNickname", "")
  readonly property bool autoReconnect: setting("autoReconnect", false) === true
  readonly property string dropPolicy: setting("dropPolicy", "keep")
  readonly property string uiLanguage: language === "auto" ? "en" : language

  // Tunables handed to session.py create (M3).
  function connectOptions() {
    return {
      res: root.resolution,
      fps: root.fps,
      bitrate: String(root.bitrateMbps) + "M",
      codec: root.codec
    }
  }

  // --- settings (M5) -----------------------------------------------------
  function currentSettings() {
    return {
      codec: root.codec,
      resolution: root.resolution,
      fps: root.fps,
      bitrateMbps: root.bitrateMbps,
      audio: root.audio,
      language: root.language,
      deviceNickname: root.deviceNickname,
      autoReconnect: root.autoReconnect,
      dropPolicy: root.dropPolicy
    }
  }

  function applySettingsToService() {
    if (root.service) root.service.applySettings(root.currentSettings())
  }

  // Applied locally first so the panel redraws on the click itself; the
  // shell.json write comes back as the same value (same pattern as the
  // first-party clock panel).
  function persistSettings(values) {
    var entry = { id: root.moduleName }
    for (var existing in root.settings) {
      if (existing !== "id") entry[existing] = root.settings[existing]
    }
    for (var key in values) {
      if (Object.prototype.hasOwnProperty.call(values, key)) entry[key] = values[key]
    }
    root.settings = entry
    if (root.bar && root.bar.shell
        && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
    applySettingsToService()
  }

  Component.onCompleted: applySettingsToService()

  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground

  // Explicitly typed aliases keep qmllint static: qs.Commons declares
  // Style.spacing/font as QtObject, so member access on them is untyped.
  // fontToken()/fontPx() honour theme overrides exactly like Style.font.*.
  readonly property int captionSize: Style.fontToken("caption", Style.fontPx(0.833))
  readonly property int titleSize: Style.fontToken("title", Style.fontPx(1.167))
  readonly property int bodySize: Style.fontToken("body", Style.fontPx(1.0))

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
  readonly property bool disconnected: activeSession !== null
    && String(activeSession.phase || "") === "disconnected"
  readonly property bool trialActive: service ? service.trialActive : false
  readonly property int trialRemaining: service ? service.trialRemainingSeconds : 0
  readonly property var displayRules: service ? service.displayRules : []
  readonly property string displayError: service ? service.displayError : ""
  readonly property bool displayBusy: service ? service.displayBusy : false
  readonly property bool provisioning: service ? service.provisioning : false
  readonly property var overlappingPairs: Model.overlappingDisplays(monitors)
  readonly property string barState: service
    ? service.barState
    : Model.barState({ lastError: lastError, activeSession: activeSession,
                       sessionBusy: sessionBusy, discovering: discovering })
  readonly property int castElapsedSeconds: service ? service.castElapsedSeconds : 0
  readonly property string activeTarget: service && service.activeTarget !== ""
    ? service.activeTarget
    : Model.sessionTargetLabel(activeSession, devices)

  readonly property string stateIcon: {
    if (barState === "error") return "󰅚"
    if (barState === "casting") return "󰐊"
    if (barState === "connecting") return "󰒍"
    if (barState === "discovering") return "󰑐"
    if (devices.length > 0) return "󰍺"
    return "󰍹"
  }

  readonly property string statusText: {
    if (barState === "error") return lastError.toUpperCase()
    if (barState === "casting") {
      var target = activeTarget !== "" ? activeTarget : "TV"
      var line = Model.tr(uiLanguage, "casting_to") + " " + target
      if (castElapsedSeconds > 0) line += " · " + Model.formatElapsed(castElapsedSeconds)
      return line.toUpperCase()
    }
    if (barState === "connecting") return Model.tr(uiLanguage, "connecting").toUpperCase()
    if (barState === "discovering") return Model.tr(uiLanguage, "searching").toUpperCase()
    var parts = []
    parts.push(monitors.length + " " + Model.tr(uiLanguage, "displays"))
    parts.push(devices.length + " " + Model.tr(uiLanguage, "devices"))
    return parts.join(" · ").toUpperCase()
  }

  // Destructive steps ask for confirmation (M5).
  property bool confirmDisconnect: false

  function requestDisconnect(device) {
    if (!root.sessionActive) return
    if (device && !root.isActiveDevice(device)) return
    root.confirmDisconnect = true
  }

  function confirmDisconnectNow() {
    root.confirmDisconnect = false
    if (root.service) root.service.disconnect()
  }

  function cancelDisconnect() {
    root.confirmDisconnect = false
  }

  // True when this discovered device is the one the active session casts to.
  function isActiveDevice(device) {
    return Model.sessionMatchesDevice(activeSession, device)
  }

  // --- cursor model (keyboard + mouse share one highlight) ---------------
  // The arranger is pointer-driven; keyboard navigation covers receivers.
  readonly property var visibleSections: ["devices"]

  property string focusSection: "devices"
  property int selectedIndex: 0
  property bool cursorActive: false

  function sectionCount(section) {
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

  onMonitorsChanged: { clampCursor(); ensureDisplaySelection() }
  onDevicesChanged: clampCursor()
  onVisibleSectionsChanged: clampCursor()

  // Refresh the display list and re-sweep discovery whenever the panel opens.
  onOpenedChanged: {
    if (!opened) {
      confirmDisconnect = false
      return
    }
    applySettingsToService()
    if (service) {
      service.refreshMonitors()
      service.refreshDisplayRules()
      service.refreshDevices()
    }
    ensureDisplaySelection()
    cursorActive = false
  }

  // --- selected display (arranger detail card) --------------------------
  property string selectedDisplayName: ""
  property var detailEdits: ({})
  property bool identifyActive: false

  onTrialActiveChanged: if (!trialActive) refreshSelectedEdits()

  function monitorByName(name) {
    var target = String(name || "")
    if (target === "") return null
    for (var i = 0; i < monitors.length; i++) {
      if (String(monitors[i].name || "") === target) return monitors[i]
    }
    return null
  }

  function selectedMonitor() {
    return monitorByName(selectedDisplayName)
  }

  function selectDisplay(display) {
    if (!display) return
    selectedDisplayName = String(display.name || "")
    detailEdits = Model.displayFieldsFor(display, displayRules)
  }

  // Keep a display selected as the list grows/shrinks (hotplug, cast start).
  function ensureDisplaySelection() {
    if (monitors.length === 0) {
      selectedDisplayName = ""
      detailEdits = ({})
      return
    }
    if (selectedMonitor() !== null) return
    selectDisplay(monitors[0])
  }

  // Re-sync the detail card with Hyprland after an apply/revert/save.
  function refreshSelectedEdits() {
    var display = selectedMonitor()
    if (display) detailEdits = Model.displayFieldsFor(display, displayRules)
  }

  function updateSelected(key, value) {
    detailEdits = withField(detailEdits, key, value)
  }

  function applySelected() {
    var display = selectedMonitor()
    if (display) applyDisplay(display, detailEdits)
  }

  function saveSelected() {
    var display = selectedMonitor()
    if (display) saveDisplay(display, detailEdits)
  }

  // Power is a single-field action, so it applies immediately as a trial
  // (guard_visible_layout still refuses to hide the last visible display).
  function toggleSelectedPower() {
    var display = selectedMonitor()
    if (!display) return
    updateSelected("disabled", detailEdits.disabled !== true)
    applyDisplay(display, detailEdits)
  }

  // Flash the numbering inside the arranger (no layer-shell overlays).
  function identify() {
    identifyActive = true
    identifyTimer.restart()
  }

  Timer {
    id: identifyTimer
    interval: 1400
    repeat: false
    onTriggered: root.identifyActive = false
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

  // All live applies are trials: the service auto-reverts after a timeout
  // unless the user presses Keep (M6).
  function applyDisplay(display, fields) {
    if (!service || !display) return
    service.beginDisplayTrial(Model.displayRulePayload(display, fields))
  }

  function reconnectActive() {
    if (!root.service || !root.activeSession) return
    var device = Model.reconnectDeviceFromSession(root.activeSession)
    if (device) root.service.connectDevice(device, root.connectOptions())
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
        if (root.isActiveDevice(device)) root.requestDisconnect(device)
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
              color: root.lastError !== "" ? Color.urgent : root.foreground
              font.family: root.fontFamily
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
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: root.titleSize
                font.bold: true
                elide: Text.ElideRight
                width: parent.width
              }

              Text {
                textFormat: Text.PlainText
                text: root.statusText
                color: root.lastError !== "" ? Color.urgent : Qt.darker(root.foreground, 1.4)
                font.family: root.fontFamily
                font.pixelSize: root.captionSize
                font.bold: true
                font.letterSpacing: 1.2
                elide: Text.ElideRight
                width: parent.width
              }
            }
          }

          // ---------- Active session (M5) ----------
          Column {
            width: parent.width
            spacing: Style.space(8)
            visible: root.sessionActive

            PanelSeparator { foreground: root.foreground }

            Row {
              width: parent.width
              spacing: Style.space(8)

              Text {
                textFormat: Text.PlainText
                text: "󰐊"
                color: Commons.Color.accent
                font.family: root.fontFamily
                font.pixelSize: root.titleSize
                width: Style.space(22)
                horizontalAlignment: Text.AlignHCenter
                anchors.verticalCenter: parent.verticalCenter
              }

              Column {
                width: parent.width - Style.space(22) - disconnectButton.width - Style.space(16)
                  - (reconnectButton.visible ? reconnectButton.width + parent.spacing : 0)
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.space(1)

                Text {
                  textFormat: Text.PlainText
                  width: parent.width
                  text: root.activeTarget !== ""
                    ? root.activeTarget
                    : String(root.activeSession ? root.activeSession.name : "")
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: root.bodySize
                  elide: Text.ElideRight
                }

                Text {
                  textFormat: Text.PlainText
                  width: parent.width
                  text: root.disconnected
                    ? Model.tr(root.uiLanguage, "tv_disconnected")
                    : (root.barState === "connecting"
                       ? Model.tr(root.uiLanguage, "connecting")
                       : Model.tr(root.uiLanguage, "casting") + " · "
                         + Model.formatElapsed(root.castElapsedSeconds))
                  color: root.disconnected ? Color.urgent : Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                  elide: Text.ElideRight
                }
              }

              Button {
                id: reconnectButton
                visible: root.disconnected
                text: Model.tr(root.uiLanguage, "reconnect")
                iconText: "󰑐"
                bordered: true
                enabled: !root.sessionBusy
                opacity: enabled ? 1 : 0.45
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                anchors.verticalCenter: parent.verticalCenter
                onClicked: root.reconnectActive()
              }

              Button {
                id: disconnectButton
                text: Model.tr(root.uiLanguage, "disconnect")
                iconText: "󰓛"
                bordered: true
                enabled: !root.sessionBusy
                opacity: enabled ? 1 : 0.45
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                anchors.verticalCenter: parent.verticalCenter
                onClicked: root.requestDisconnect(null)
              }
            }

            Text {
              textFormat: Text.PlainText
              visible: root.confirmDisconnect
              width: parent.width
              text: Model.tr(root.uiLanguage, "confirm_disconnect")
              color: Color.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Row {
              visible: root.confirmDisconnect
              spacing: Style.space(8)

              Button {
                text: Model.tr(root.uiLanguage, "disconnect")
                iconText: "󰓛"
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                onClicked: root.confirmDisconnectNow()
              }

              Button {
                text: "Cancel"
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                onClicked: root.cancelDisconnect()
              }
            }
          }

          // ---------- Displays ----------
          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            Item {
              width: parent.width
              implicitHeight: Math.max(displaysHeader.implicitHeight,
                                      identifyButton.implicitHeight)

              PanelSectionHeader {
                id: displaysHeader
                text: "DISPLAYS"
                foreground: root.foreground
                fontFamily: root.fontFamily
                anchors.left: parent.left
                anchors.verticalCenter: parent.verticalCenter
              }

              Button {
                id: identifyButton
                visible: root.monitors.length > 0
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                text: root.identifyActive ? "Identify…" : "Identify"
                iconText: "󰍹"
                tooltipText: "Flash each display's number in this arrangement"
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                onClicked: root.identify()
              }
            }

            Text {
              textFormat: Text.PlainText
              visible: root.monitors.length === 0
              width: parent.width
              text: "No displays reported by Hyprland."
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            // Drag displays to set their layout position; edges snap to
            // neighbouring displays (M4).
            ArrangementCanvas {
              id: arrangement
              visible: root.monitors.length > 0
              width: parent.width
              selectedName: root.selectedDisplayName
              onSelectRequested: function(name) {
                root.selectDisplay(root.monitorByName(name))
              }
            }

            Text {
              textFormat: Text.PlainText
              visible: root.monitors.length > 0
              width: parent.width
              text: "Drag a display to arrange it — edges snap together. Click a display to change its settings."
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            // M6: an Apply is a trial until the user keeps it.
            Column {
              width: parent.width
              visible: root.trialActive
              spacing: Style.space(6)

              Text {
                textFormat: Text.PlainText
                width: parent.width
                text: Model.tr(root.uiLanguage, "layout_trial")
                  + " · " + root.trialRemaining + "s — "
                  + Model.tr(root.uiLanguage, "trial_hint")
                color: Commons.Color.accent
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                wrapMode: Text.Wrap
              }

              Row {
                spacing: Style.space(8)

                Button {
                  text: Model.tr(root.uiLanguage, "keep_layout")
                  iconText: "󰄬"
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  verticalPadding: Style.space(6)
                  onClicked: if (root.service) root.service.keepDisplayTrial()
                }

                Button {
                  text: Model.tr(root.uiLanguage, "revert_layout")
                  iconText: "󰁮"
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  verticalPadding: Style.space(6)
                  onClicked: if (root.service) root.service.revertDisplayTrial()
                }
              }
            }

            Text {
              textFormat: Text.PlainText
              visible: root.overlappingPairs.length > 0
              width: parent.width
              text: "Overlapping displays: " + root.overlappingPairs.map(function(pair) {
                return pair.join(" / ")
              }).join(", ")
              color: Color.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.displayError !== ""
              width: parent.width
              text: root.displayError
              color: Color.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            // ---------- Selected display detail card ----------
            Column {
              id: displayDetails
              width: parent.width
              visible: root.selectedMonitor() !== null
              spacing: Style.space(8)

              readonly property var monitor: root.selectedMonitor()
              readonly property var edits: root.detailEdits
              readonly property var resolutionList: Model.resolutionChoices(monitor, edits.mode)
              readonly property var rateList: Model.refreshChoices(monitor, edits.mode)
              readonly property var mirrorList: {
                var out = [""]
                for (var i = 0; i < root.monitors.length; i++) {
                  var other = root.monitors[i]
                  if (String(other.name || "") !== String(monitor ? monitor.name : ""))
                    out.push(String(other.name || ""))
                }
                return out
              }
              readonly property bool powered: edits.disabled !== true
              readonly property string castSuffix:
                Model.monitorSessionLabel(monitor, root.activeSession, root.devices)

              PanelSeparator { foreground: root.foreground }

              Row {
                width: parent.width
                spacing: Style.space(8)

                Text {
                  textFormat: Text.PlainText
                  text: displayDetails.monitor && displayDetails.monitor.virtual
                    ? "󰒍" : "󰍹"
                  color: displayDetails.powered
                    ? root.foreground : Qt.darker(root.foreground, 1.6)
                  font.family: root.fontFamily
                  font.pixelSize: root.titleSize
                  width: Style.space(22)
                  horizontalAlignment: Text.AlignHCenter
                  anchors.verticalCenter: parent.verticalCenter
                }

                Column {
                  width: parent.width - Style.space(22) - powerButton.width
                    - parent.spacing * 2
                  anchors.verticalCenter: parent.verticalCenter
                  spacing: Style.space(1)

                  Text {
                    textFormat: Text.PlainText
                    width: parent.width
                    text: String(displayDetails.monitor ? displayDetails.monitor.name : "")
                      + displayDetails.castSuffix
                    color: displayDetails.castSuffix !== ""
                      ? Commons.Color.accent : root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: root.bodySize
                    font.bold: true
                    elide: Text.ElideRight
                  }

                  Text {
                    textFormat: Text.PlainText
                    width: parent.width
                    text: (displayDetails.monitor && displayDetails.monitor.description
                           ? displayDetails.monitor.description + " · " : "")
                      + Model.resolutionLabel(Model.resolutionOf(displayDetails.edits.mode))
                      + (displayDetails.powered ? "" : " · off")
                    color: Qt.darker(root.foreground, 1.4)
                    font.family: root.fontFamily
                    font.pixelSize: root.captionSize
                    elide: Text.ElideRight
                  }
                }

                Button {
                  id: powerButton
                  text: displayDetails.powered ? "Turn off" : "Turn on"
                  iconText: displayDetails.powered ? "󰐥" : "󰐊"
                  tooltipText: displayDetails.powered
                    ? "Disable this output (trial)"
                    : "Enable this output (trial)"
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  verticalPadding: Style.space(6)
                  enabled: !root.displayBusy && !!displayDetails.monitor
                  opacity: enabled ? 1 : 0.45
                  anchors.verticalCenter: parent.verticalCenter
                  onClicked: root.toggleSelectedPower()
                }
              }

              Column {
                width: parent.width
                enabled: displayDetails.powered && !!displayDetails.monitor
                opacity: enabled ? 1 : 0.45
                spacing: Style.space(8)

                Row {
                  width: parent.width
                  spacing: Style.space(8)

                  Column {
                    spacing: Style.space(2)
                    Text {
                      textFormat: Text.PlainText
                      text: "Resolution"
                      color: Qt.darker(root.foreground, 1.4)
                      font.family: root.fontFamily
                      font.pixelSize: root.captionSize
                    }
                    ComboBox {
                      width: Style.space(150)
                      model: displayDetails.resolutionList.map(function(res) {
                        return Model.resolutionLabel(res)
                      })
                      currentIndex: Math.max(0,
                        displayDetails.resolutionList.indexOf(
                          Model.resolutionOf(displayDetails.edits.mode)))
                      font.family: root.fontFamily
                      onActivated: {
                        var res = Model.resolutionFromLabel(currentText)
                        var rate = Model.refreshOf(displayDetails.edits.mode)
                        var rates = Model.refreshChoices(displayDetails.monitor,
                          res + "@" + rate)
                        if (rates.indexOf(rate) < 0)
                          rate = rates.length > 0 ? rates[0] : rate
                        root.updateSelected("mode", Model.modeFor(res, rate))
                      }
                    }
                  }

                  Column {
                    spacing: Style.space(2)
                    Text {
                      textFormat: Text.PlainText
                      text: "Refresh rate"
                      color: Qt.darker(root.foreground, 1.4)
                      font.family: root.fontFamily
                      font.pixelSize: root.captionSize
                    }
                    ComboBox {
                      width: Style.space(120)
                      model: displayDetails.rateList.map(function(hz) {
                        return String(hz) + " Hz"
                      })
                      currentIndex: Math.max(0,
                        displayDetails.rateList.indexOf(
                          Model.refreshOf(displayDetails.edits.mode)))
                      font.family: root.fontFamily
                      onActivated: root.updateSelected("mode",
                        Model.modeFor(Model.resolutionOf(displayDetails.edits.mode),
                          Number(String(currentText).replace(" Hz", ""))))
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
                      text: "Scale"
                      color: Qt.darker(root.foreground, 1.4)
                      font.family: root.fontFamily
                      font.pixelSize: root.captionSize
                    }
                    ComboBox {
                      width: Style.space(110)
                      model: Model.scaleChoices(displayDetails.edits.scale).map(
                        function(scale) { return Model.scaleLabel(scale) })
                      currentIndex: {
                        var choices = Model.scaleChoices(displayDetails.edits.scale)
                        return Math.max(0,
                          choices.indexOf(Number(displayDetails.edits.scale)))
                      }
                      font.family: root.fontFamily
                      onActivated: root.updateSelected("scale",
                        Model.scaleFromLabel(currentText))
                    }
                  }

                  Column {
                    spacing: Style.space(2)
                    Text {
                      textFormat: Text.PlainText
                      text: "Orientation"
                      color: Qt.darker(root.foreground, 1.4)
                      font.family: root.fontFamily
                      font.pixelSize: root.captionSize
                    }
                    ComboBox {
                      width: Style.space(160)
                      model: [
                        Model.orientationLabel(0),
                        Model.orientationLabel(1),
                        Model.orientationLabel(2),
                        Model.orientationLabel(3)
                      ]
                      currentIndex: {
                        var transform = Number(displayDetails.edits.transform || 0)
                        return transform >= 0 && transform <= 3 ? transform : 0
                      }
                      font.family: root.fontFamily
                      onActivated: root.updateSelected("transform", currentIndex)
                    }
                  }
                }

                Column {
                  width: parent.width
                  spacing: Style.space(2)
                  Text {
                    textFormat: Text.PlainText
                    text: "Display mode"
                    color: Qt.darker(root.foreground, 1.4)
                    font.family: root.fontFamily
                    font.pixelSize: root.captionSize
                  }
                  ComboBox {
                    width: Math.min(parent.width, Style.space(260))
                    model: displayDetails.mirrorList.map(function(name) {
                      return name === "" ? "Extend desktop" : "Mirror " + name
                    })
                    currentIndex: {
                      var idx = displayDetails.mirrorList.indexOf(
                        String(displayDetails.edits.mirror || ""))
                      return idx < 0 ? 0 : idx
                    }
                    font.family: root.fontFamily
                    onActivated: root.updateSelected("mirror",
                      currentIndex === 0
                        ? ""
                        : displayDetails.mirrorList[currentIndex])
                  }
                }

                Row {
                  width: parent.width
                  spacing: Style.space(8)

                  Text {
                    textFormat: Text.PlainText
                    width: parent.width - applyButton.width - saveButton.width
                      - Style.space(16)
                    text: "Position " + String(displayDetails.edits.position || "")
                    color: Qt.darker(root.foreground, 1.4)
                    font.family: root.fontFamily
                    font.pixelSize: root.captionSize
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
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    horizontalPadding: Style.space(4)
                    verticalPadding: Style.space(6)
                    onClicked: root.applySelected()
                  }

                  Button {
                    id: saveButton
                    text: "Save"
                    iconText: "󰆓"
                    bordered: true
                    enabled: !root.displayBusy
                    opacity: enabled ? 1 : 0.45
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    horizontalPadding: Style.space(4)
                    verticalPadding: Style.space(6)
                    onClicked: root.saveSelected()
                  }
                }

                Text {
                  textFormat: Text.PlainText
                  width: parent.width
                  text: "Apply previews this layout as a trial; Save writes it to monitors.lua (backed up first)."
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                  wrapMode: Text.Wrap
                }
              }
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
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                wrapMode: Text.Wrap
              }

              Button {
                id: orphanButton
                text: "Clean up"
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                enabled: !root.sessionBusy
                onClicked: if (root.service) root.service.cleanupOrphan(root.sessionOrphans[0])
              }
            }
          }

          // ---------- Chromecast devices ----------
          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(10)

            Item {
              width: parent.width
              implicitHeight: Math.max(devicesHeader.implicitHeight, refreshButton.implicitHeight)

              PanelSectionHeader {
                id: devicesHeader
                text: "CHROMECAST"
                foreground: root.foreground
                fontFamily: root.fontFamily
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
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
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
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
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
          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)

            PanelSectionHeader {
              text: "MANUAL IP"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: "For mDNS-blocked or cross-VLAN receivers."
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
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
                foreground: root.foreground
                font.family: root.fontFamily
                onAccepted: root.submitManual()
              }

              Button {
                id: addButton
                text: "Add"
                iconText: "󰐕"
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.submitManual()
              }
            }
          }

          // ---------- Settings (M5) ----------
          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)

            PanelSectionHeader {
              text: "SETTINGS"
              foreground: root.foreground
              fontFamily: root.fontFamily
            }

            Text {
              textFormat: Text.PlainText
              width: parent.width
              text: "Applies to the next connection."
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Row {
              width: parent.width
              spacing: Style.space(8)

              Column {
                spacing: Style.space(2)

                Text {
                  textFormat: Text.PlainText
                  text: "Language"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(168)
                  model: ["Auto", "English", "Bahasa Indonesia"]
                  currentIndex: {
                    var idx = ["auto", "en", "id"].indexOf(root.language)
                    return idx < 0 ? 0 : idx
                  }
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({
                    language: ["auto", "en", "id"][currentIndex]
                  })
                }
              }

              Column {
                spacing: Style.space(2)

                Text {
                  textFormat: Text.PlainText
                  text: "Codec"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(120)
                  model: ["Auto", "HEVC", "H.264", "VP9"]
                  currentIndex: {
                    var idx = ["auto", "hevc", "h264", "vp9"].indexOf(root.codec)
                    return idx < 0 ? 0 : idx
                  }
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({
                    codec: ["auto", "hevc", "h264", "vp9"][currentIndex]
                  })
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
                  text: "Resolution"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(168)
                  model: ["1920x1080@60", "1600x900@60", "1280x720@60"]
                  currentIndex: {
                    var idx = ["1920x1080@60", "1600x900@60", "1280x720@60"]
                      .indexOf(root.resolution)
                    return idx < 0 ? 0 : idx
                  }
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({ resolution: currentText })
                }
              }

              Column {
                spacing: Style.space(2)

                Text {
                  textFormat: Text.PlainText
                  text: "FPS"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(82)
                  model: ["24", "30", "50", "60"]
                  currentIndex: {
                    var idx = ["24", "30", "50", "60"].indexOf(String(root.fps))
                    return idx < 0 ? 1 : idx
                  }
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({ fps: Number(currentText) })
                }
              }

              Column {
                spacing: Style.space(2)

                Text {
                  textFormat: Text.PlainText
                  text: "Mbps"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(82)
                  model: ["4", "6", "8", "10", "12", "16"]
                  currentIndex: {
                    var idx = ["4", "6", "8", "10", "12", "16"]
                      .indexOf(String(root.bitrateMbps))
                    return idx < 0 ? 1 : idx
                  }
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({
                    bitrateMbps: Number(currentText)
                  })
                }
              }
            }

            Row {
              width: parent.width
              spacing: Style.space(8)

              Column {
                spacing: Style.space(2)
                width: parent.width - reconnectToggle.width - parent.spacing

                Text {
                  textFormat: Text.PlainText
                  text: "Device nickname"
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                TextField {
                  id: nicknameField
                  width: parent.width
                  text: root.deviceNickname
                  placeholderText: "Living room TV"
                  foreground: root.foreground
                  font.family: root.fontFamily
                  onEditingFinished: root.persistSettings({
                    deviceNickname: String(text || "").trim()
                  })
                }
              }

              Button {
                id: reconnectToggle
                text: root.autoReconnect
                  ? "Auto-reconnect: On" : "Auto-reconnect: Off"
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                horizontalPadding: Style.space(4)
                verticalPadding: Style.space(6)
                anchors.bottom: parent.bottom
                onClicked: root.persistSettings({
                  autoReconnect: !root.autoReconnect
                })
              }
            }

            Row {
              width: parent.width
              spacing: Style.space(8)

              Column {
                spacing: Style.space(2)

                Text {
                  textFormat: Text.PlainText
                  text: Model.tr(root.uiLanguage, "drop_policy")
                  color: Qt.darker(root.foreground, 1.4)
                  font.family: root.fontFamily
                  font.pixelSize: root.captionSize
                }

                ComboBox {
                  width: Style.space(200)
                  model: [
                    Model.tr(root.uiLanguage, "drop_keep"),
                    Model.tr(root.uiLanguage, "drop_remove")
                  ]
                  currentIndex: root.dropPolicy === "remove" ? 1 : 0
                  font.family: root.fontFamily
                  onActivated: root.persistSettings({
                    dropPolicy: currentIndex === 1 ? "remove" : "keep"
                  })
                }
              }
            }
          }

          // ---------- Doctor ----------
          PanelSeparator { foreground: root.foreground }

          Column {
            width: parent.width
            spacing: Style.space(8)

            Item {
              width: parent.width
              implicitHeight: Math.max(doctorHeader.implicitHeight, doctorActions.implicitHeight)

              PanelSectionHeader {
                id: doctorHeader
                text: "DOCTOR"
                foreground: root.foreground
                fontFamily: root.fontFamily
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
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  verticalPadding: Style.space(6)
                  onClicked: if (root.service) root.service.provisionDependencies()
                }

                Button {
                  id: doctorRerun
                  text: "Re-run"
                  iconText: "󰑐"
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  horizontalPadding: Style.space(4)
                  verticalPadding: Style.space(6)
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
              color: root.depsOk ? Qt.darker(root.foreground, 1.3) : Color.urgent
              font.family: root.fontFamily
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
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.missingPython && root.doctor.missingPython.length > 0
              width: parent.width
              text: "Missing Python modules: " + (root.doctor.missingPython || []).join(", ")
              color: Color.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.venv !== ""
              width: parent.width
              text: "Private venv: " + (root.doctor.venvReady ? "ready" : (root.doctor.venvExists ? "present (incomplete)" : "not created"))
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.fastPath !== ""
              width: parent.width
              text: "Fast path (optional, root): " + root.doctor.fastPath
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
              font.pixelSize: Style.font.bodySmall
              wrapMode: Text.Wrap
            }

            Text {
              textFormat: Text.PlainText
              visible: root.doctor.firewallRule !== ""
              width: parent.width
              text: "Stream port " + root.doctor.streamPort + " must be reachable. If blocked: " + root.doctor.firewallRule
              color: Qt.darker(root.foreground, 1.4)
              font.family: root.fontFamily
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

  // Spatial arrangement preview: numbered display cards sharing one scale, with
  // edge snapping while dragging (M4).
  component ArrangementCanvas: Rectangle {
    id: canvas

    property string selectedName: ""
    signal selectRequested(string displayName)

    readonly property var bounds: Model.arrangementBounds(root.monitors)
    readonly property real pad: Style.space(16)
    readonly property real factor: Math.min(
      Math.max(1, width - pad * 2) / bounds.width,
      Math.max(1, implicitHeight - pad * 2) / bounds.height)
    readonly property real offX: pad + (Math.max(1, width - pad * 2) - bounds.width * factor) / 2
    readonly property real offY: pad + (Math.max(1, implicitHeight - pad * 2) - bounds.height * factor) / 2

    implicitHeight: Style.space(180)
    color: "transparent"
    border.width: 1
    border.color: Qt.darker(root.foreground, 2.6)
    radius: Style.space(8)
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
        readonly property bool isSelected:
          String(modelData.name || "") === canvas.selectedName
        readonly property bool highlighted: isSelected || root.identifyActive

        x: Math.round(canvas.pxX(dragging ? dragX : modelData.x))
        y: Math.round(canvas.pxY(dragging ? dragY : modelData.y))
        width: Math.max(Style.space(46), size.width * canvas.factor)
        height: Math.max(Style.space(34), size.height * canvas.factor)
        radius: Style.space(6)
        opacity: isDisabled ? 0.4 : 1
        color: isSelected
          ? Style.selectedFillFor(root.foreground, Commons.Color.accent)
          : (isVirtual ? Qt.darker(Commons.Color.accent, 2.8) : "transparent")
        border.width: highlighted ? 2 : 1
        border.color: highlighted || isVirtual
          ? Commons.Color.accent
          : Qt.darker(root.foreground, 1.5)

        Column {
          anchors.centerIn: parent
          width: Math.max(0, parent.width - Style.space(8))
          spacing: Style.space(1)

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: String(card.index + 1)
            color: card.highlighted ? Commons.Color.accent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: root.titleSize
            font.bold: true
            horizontalAlignment: Text.AlignHCenter
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            text: String(card.modelData.name || "")
            color: card.highlighted ? Commons.Color.accent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: root.captionSize
            elide: Text.ElideRight
            horizontalAlignment: Text.AlignHCenter
          }

          Text {
            textFormat: Text.PlainText
            width: parent.width
            visible: card.isDisabled
            text: "Off"
            color: Qt.darker(root.foreground, 1.4)
            font.family: root.fontFamily
            font.pixelSize: root.captionSize
            horizontalAlignment: Text.AlignHCenter
          }
        }

        MouseArea {
          anchors.fill: parent
          cursorShape: card.isDisabled ? Qt.ArrowCursor : Qt.SizeAllCursor
          property real pressX: 0
          property real pressY: 0
          property real startLogicalX: 0
          property real startLogicalY: 0

          onPressed: function(mouse) {
            canvas.selectRequested(String(card.modelData.name || ""))
            if (card.isDisabled) return
            var point = mapToItem(canvas, mouse.x, mouse.y)
            pressX = point.x
            pressY = point.y
            startLogicalX = Number(card.modelData.x || 0)
            startLogicalY = Number(card.modelData.y || 0)
            card.dragging = true
            card.dragX = startLogicalX
            card.dragY = startLogicalY
          }

          onPositionChanged: function(mouse) {
            if (!pressed || !card.dragging) return
            var point = mapToItem(canvas, mouse.x, mouse.y)
            var logicalX = startLogicalX + (point.x - pressX) / canvas.factor
            var logicalY = startLogicalY + (point.y - pressY) / canvas.factor
            var snapped = Model.snapPosition(card.modelData, logicalX, logicalY,
              root.monitors, 16)
            card.dragX = snapped.x
            card.dragY = snapped.y
          }

          onReleased: {
            if (!card.dragging) return
            card.dragging = false
            var fields = root.displayFields(card.modelData)
            fields = root.withField(fields, "position",
              Math.round(card.dragX) + "x" + Math.round(card.dragY))
            if (root.selectedDisplayName === String(card.modelData.name || ""))
              root.detailEdits = fields
            root.applyDisplay(card.modelData, fields)
          }
        }
      }
    }
  }

  component DeviceRow: CursorSurface {
    id: deviceRow
    required property var device
    required property int rowIndex

    readonly property bool isActive: root.isActiveDevice(device)
    readonly property bool isDropped: isActive && root.disconnected

    hasCursor: root.cursorActive && root.focusSection === "devices" && root.selectedIndex === rowIndex
    onHasCursorChanged: if (hasCursor) root.ensureCursorVisible(deviceRow)
    foreground: root.foreground
    fill: Style.hoverFillFor(root.foreground, Commons.Color.accent)
    currentFill: Style.selectedFillFor(root.foreground, Commons.Color.accent)
    implicitHeight: deviceInner.implicitHeight + Style.space(10)

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
        color: deviceRow.device.online ? root.foreground : Qt.darker(root.foreground, 1.6)
        font.family: root.fontFamily
        font.pixelSize: root.titleSize
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
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: root.bodySize
          elide: Text.ElideRight
        }

        Text {
          textFormat: Text.PlainText
          width: parent.width
          visible: deviceRow.device.model !== "" && deviceRow.device.model !== deviceRow.device.name
          text: deviceRow.device.model
          color: Qt.darker(root.foreground, 1.4)
          font.family: root.fontFamily
          font.pixelSize: root.captionSize
          elide: Text.ElideRight
        }
      }

      Text {
        id: codecText
        textFormat: Text.PlainText
        text: Model.deviceCodecLabel(deviceRow.device)
        color: Qt.darker(root.foreground, 1.3)
        font.family: root.fontFamily
        font.pixelSize: root.captionSize
        anchors.verticalCenter: parent.verticalCenter
      }

      Button {
        id: connectButton
        text: deviceRow.isDropped
          ? Model.tr(root.uiLanguage, "reconnect")
          : (deviceRow.isActive
             ? Model.tr(root.uiLanguage, "disconnect")
             : Model.tr(root.uiLanguage, "use_tv"))
        iconText: deviceRow.isDropped ? "󰑐" : (deviceRow.isActive ? "󰓛" : "󰐊")
        tooltipText: deviceRow.isDropped
          ? "Reconnect the TV to the kept virtual monitor"
          : (deviceRow.isActive
             ? "Stop the stream and remove the virtual monitor"
             : "Use TV as second monitor")
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        horizontalPadding: Style.space(4)
        verticalPadding: Style.space(6)
        enabled: !root.sessionBusy && (!root.sessionActive || deviceRow.isActive)
        opacity: enabled ? 1 : 0.45
        anchors.verticalCenter: parent.verticalCenter
        onClicked: {
          if (!root.service) return
          if (deviceRow.isDropped) root.reconnectActive()
          else if (deviceRow.isActive) root.requestDisconnect(deviceRow.device)
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
