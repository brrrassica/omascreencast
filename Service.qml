import QtQuick
import Quickshell
import Quickshell.Io
import "Model.js" as Model

// omascreencast — headless service singleton (`kind: service`).
//
// Owns the shared state the bar widget reads: the physical/virtual display
// list, the discovered Chromecast devices, and the dependency `doctor` report.
// It survives a closed panel, so discovery keeps running while the popup is
// hidden. The panel receives this object through the shell's injected
// `service` property (see shell.qml: `if ("service" in item) item.service = ...`).
//
// M1 scope: discovery + doctor + display enumeration. Cast/session lifecycle
// lands in M2/M3.
Item {
  id: root

  property var shell: null
  property string omarchyPath: Quickshell.env("OMARCHY_PATH")

  readonly property string pluginId: "tech.alatcerdas.omascreencast"
  readonly property string pluginVersion: "0.1.1"

  // Absolute path to this plugin's bundled helper scripts, resolved relative
  // to this QML file (not OMARCHY_PATH string math) so helpers are found
  // wherever the plugin was installed — cloned by `omarchy plugin add` or
  // dropped in by hand. decodeURIComponent undoes Qt.resolvedUrl's
  // percent-encoding, which would otherwise break a path containing a space.
  readonly property string scriptsDir: decodeURIComponent(
    Qt.resolvedUrl("scripts").toString().replace(/^file:\/\//, ""))

  readonly property string castEngine: scriptsDir + "/cast_engine.py"
  readonly property string bootstrap: scriptsDir + "/bootstrap.py"
  readonly property string sessionScript: scriptsDir + "/session.py"
  readonly property string displayScript: scriptsDir + "/display_config.py"

  // --- shared state (read by Panel.qml) ---
  property var monitors: []
  property var devices: []
  property bool discovering: false
  property var doctor: ({})
  property string lastError: ""
  // Active virtual-monitor session (single source of truth: session.py).
  property var activeSession: null
  property var sessionOrphans: []
  property bool sessionBusy: false
  // Persisted hl.monitor rules (display_config.py) and arranger state (M4).
  property var displayRules: []
  property string displayError: ""
  property bool displayBusy: false
  // M6 trial/keep/revert: an Apply is provisional until the user keeps it.
  property var trialLayout: null
  property int trialRemaining: 0
  property var pendingRevertPayload: null
  property int trialSeconds: 15
  readonly property bool trialActive: trialLayout !== null
  readonly property int trialRemainingSeconds: trialRemaining
  // M6 receiver-drop preference: "keep" the virtual output or remove it.
  property string dropPolicy: "keep"
  property var pendingReconnectDevice: null
  property bool handlingDrop: false
  readonly property bool disconnected: activeSession !== null
    && String(activePhase) === "disconnected"
  // Manually entered receivers survive every watcher snapshot; the engine
  // cannot discover them over mDNS, so they live beside the discovery list.
  property var manualDevices: ({})
  property real lastDiscoveryAt: 0
  property int discoveryFailures: 0
  property bool provisioning: false
  // Interpreter the engine should use: system python, or the private venv when
  // the system lacks pychromecast/zeroconf. Resolved by `doctor`.
  property string pythonPath: "python3"
  property bool ready: false

  // --- M5 settings (mirrored from the bar entry by Panel.qml) ------------
  property var settings: ({})
  property string language: "auto"
  property bool autoReconnect: false
  property string deviceNickname: ""
  property string resolution: "1920x1080@60"
  property int fps: 30
  property int bitrateMbps: 6
  property string codec: "auto"

  // --- runtime phase tracking (driven by session.py events) --------------
  property string eventPhase: ""
  property string eventDetail: ""
  property int eventsRetries: 0
  property bool userInitiatedStop: false
  property int reconnectAttempts: 0
  property int elapsedTick: 0
  property var monitorNames: []
  // Notification queue so a burst of state changes is delivered in order.
  property var notifyQueue: []

  readonly property int deviceCount: devices.length
  readonly property int monitorCount: monitors.length
  readonly property bool depsOk: doctor.ok === true
  readonly property bool pythonOk: doctor.pythonOk === true
  readonly property bool pipelineOk: doctor.pipelineOk === true
  readonly property string uiLanguage: language === "auto" ? "en" : language
  readonly property string activePhase: eventPhase !== ""
    ? eventPhase : (activeSession ? String(activeSession.phase || "") : "")
  readonly property bool casting: activeSession !== null
    && ["", "starting", "casting"].indexOf(String(activePhase)) !== -1
  readonly property int castElapsedSeconds: {
    var tick = elapsedTick
    if (activePhase !== "casting") return 0
    return Model.sessionElapsedSeconds(activeSession, Date.now())
  }
  readonly property string barState: Model.barState({
    lastError: lastError,
    activeSession: activeSession,
    sessionBusy: sessionBusy,
    discovering: discovering
  })
  readonly property string activeTarget: {
    if (!activeSession) return ""
    if (deviceNickname !== "") return deviceNickname
    return Model.sessionTargetLabel(activeSession, devices)
  }

  // --- settings / notifications -----------------------------------------
  function applySettings(values) {
    var v = values || {}
    settings = v
    language = String(v.language || "auto")
    autoReconnect = v.autoReconnect === true
    deviceNickname = String(v.deviceNickname || "")
    resolution = String(v.resolution || "1920x1080@60")
    fps = Number(v.fps || 30)
    bitrateMbps = Number(v.bitrateMbps || 6)
    codec = String(v.codec || "auto")
    dropPolicy = String(v.dropPolicy || "keep")
  }

  function connectOptions() {
    return {
      res: resolution,
      fps: fps,
      bitrate: String(bitrateMbps) + "M",
      codec: codec
    }
  }

  function notify(title, body, urgency) {
    var t = String(title || "").trim()
    if (t === "") return
    var entry = {
      title: t,
      body: String(body || ""),
      urgency: String(urgency || "normal")
    }
    var queue = notifyQueue.slice()
    queue.push(entry)
    notifyQueue = queue
    if (!notifyProc.running) nextNotify()
  }

  function nextNotify() {
    if (notifyQueue.length === 0) return
    var entry = notifyQueue[0]
    notifyQueue = notifyQueue.slice(1)
    var args = ["notify-send", "-a", "Omascreencast", "-u", entry.urgency]
    if (entry.body !== "") args.push(entry.title, entry.body)
    else args.push(entry.title)
    notifyProc.command = args
    notifyProc.running = true
  }

  // --- actions -----------------------------------------------------------

  function refreshMonitors() {
    if (!monitorsProc.running) monitorsProc.running = true
  }

  // Merge manually entered receivers into a watcher snapshot so they do not
  // vanish when discovery publishes.
  function mergeManualHosts(list) {
    var out = list instanceof Array ? list.slice() : []
    for (var host in manualDevices) {
      if (!Object.prototype.hasOwnProperty.call(manualDevices, host)) continue
      var present = false
      for (var i = 0; i < out.length; i++) {
        if (String(out[i].host || "") === String(host)) { present = true; break }
      }
      if (!present) out.push(manualDevices[host])
    }
    return out
  }

  // Restart the long-lived discovery browser to force a fresh sweep. A direct
  // false->true flip races the async process exit, so route the restart
  // through a one-shot timer once the old browser has actually stopped. Avoid
  // restarting when the panel merely reopens: that would blank the list.
  function refreshDevices() {
    if (discoveryProc.running) {
      if (Date.now() - lastDiscoveryAt < 15000) return
      restartDiscovery.pending = true
      discoveryProc.running = false
    } else {
      startDiscovery()
    }
  }

  function addManualHost(host) {
    var h = String(host || "").trim()
    if (h === "") return
    var placeholder = {
      uuid: "", name: h, host: h, port: 8009, model: "",
      online: true, codecs: ["h264"]
    }
    var copy = {}
    for (var key in manualDevices) {
      if (Object.prototype.hasOwnProperty.call(manualDevices, key)) copy[key] = manualDevices[key]
    }
    copy[h] = placeholder
    manualDevices = copy
    devices = mergeManualHosts(devices)
    manualProc.command = [pythonPath, castEngine, "discovery", "--host", h]
    if (!manualProc.running) manualProc.running = true
  }

  // Rootless first-run dependency setup (bootstrap.py provision).
  function provisionDependencies() {
    if (provisionProc.running) return
    provisioning = true
    lastError = ""
    provisionProc.running = true
  }

  function runDoctor() {
    if (!doctorProc.running) doctorProc.running = true
  }

  // --- session lifecycle (M2) -------------------------------------------

  function refreshSession() {
    if (sessionBusy || statusProc.running) return
    statusProc.command = [pythonPath, sessionScript, "status"]
    statusProc.running = true
  }

  // --- monitor arranger (M4) --------------------------------------------
  function refreshDisplayRules() {
    if (displayRulesProc.running) return
    displayRulesProc.command = [pythonPath, displayScript, "rules"]
    displayRulesProc.running = true
  }

  // Ephemeral live apply (hyprctl eval; reverts on logout/restart).
  function applyDisplayRule(payload, force) {
    if (!payload) return
    if (displayApplyProc.running) {
      if (force) pendingRevertPayload = payload
      return
    }
    if (displayBusy && force !== true) return
    displayError = ""
    displayBusy = true
    displayApplyProc.command = [
      pythonPath, displayScript, "apply", "--rule", JSON.stringify(payload)
    ]
    displayApplyProc.running = true
  }

  // M6 trial: apply provisionally, then auto-revert unless kept.
  function beginDisplayTrial(payload) {
    if (displayBusy || !payload) return
    var monitor = null
    for (var i = 0; i < monitors.length; i++) {
      if (String(monitors[i].name || "") === String(payload.name || "")) {
        monitor = monitors[i]
        break
      }
    }
    var previous = monitor
      ? Model.displayFieldsFor(monitor, displayRules)
      : {}
    trialLayout = {
      name: String(payload.name || ""),
      description: String(payload.description || ""),
      previous: previous
    }
    trialRemaining = Math.max(3, trialSeconds)
    displayTrialTimer.restart()
    applyDisplayRule(payload)
  }

  function keepDisplayTrial() {
    if (!trialLayout) return
    trialLayout = null
    trialRemaining = 0
    pendingRevertPayload = null
    displayTrialTimer.stop()
    notify(Model.tr(uiLanguage, "layout_kept"), "", "low")
  }

  function revertDisplayTrial() {
    if (!trialLayout) return
    var trial = trialLayout
    trialLayout = null
    trialRemaining = 0
    displayTrialTimer.stop()
    var fields = trial.previous || {}
    if (String(fields.mode || "") === "") {
      notify(Model.tr(uiLanguage, "layout_reverted"), "", "low")
      return
    }
    var payload = Model.displayRulePayload(
      { name: trial.name, description: trial.description }, fields)
    applyDisplayRule(payload, true)
    notify(Model.tr(uiLanguage, "layout_reverted"), "", "low")
  }

  // Apply and persist into monitors.lua (backed up + atomic).
  function saveDisplayRule(payload) {
    if (displayBusy || !payload) return
    if (trialLayout) keepDisplayTrial()
    displayError = ""
    displayBusy = true
    displaySaveProc.command = [
      pythonPath, displayScript, "save", "--rule", JSON.stringify(payload)
    ]
    displaySaveProc.running = true
  }

  // Compare recorded state with Hyprland reality: cleans stale sessions and
  // reports untracked HEADLESS-* outputs (never removes them automatically).
  function reconcileSession() {
    if (sessionBusy || reconcileProc.running) return
    reconcileProc.command = [pythonPath, sessionScript, "reconcile"]
    reconcileProc.running = true
  }

  function connectDevice(device, options) {
    if (!device || sessionBusy) return
    if (casting) {
      lastError = "Disconnect the active session first"
      return
    }
    reconnectTimer.stop()
    var opts = options || {}
    var cmd = [pythonPath, sessionScript, "create"]
    if (device.uuid) cmd.push("--device", String(device.uuid))
    if (device.host) cmd.push("--host", String(device.host))
    if (device.name) cmd.push("--device-name", String(device.name))
    if (opts.res) cmd.push("--res", String(opts.res))
    if (opts.fps) cmd.push("--fps", String(opts.fps))
    if (opts.bitrate) cmd.push("--bitrate", String(opts.bitrate))
    if (opts.codec) cmd.push("--codec", String(opts.codec))
    lastError = ""
    eventDetail = ""
    eventPhase = "starting"
    userInitiatedStop = false
    reconnectAttempts = 0
    sessionBusy = true
    createProc.command = cmd
    createProc.running = true
  }

  function disconnect() {
    if (sessionBusy || activeSession === null) return
    reconnectTimer.stop()
    pendingReconnectDevice = null
    handlingDrop = false
    reconnectAttempts = 0
    userInitiatedStop = true
    lastError = ""
    sessionBusy = true
    removeProc.command = [pythonPath, sessionScript, "remove"]
    removeProc.running = true
  }

  // M6: an unexpected receiver/network drop. Keep or remove the virtual output
  // per the user's preference, and reconnect when opted in.
  function handleUnexpectedDrop(detail) {
    var previous = activeSession || {}
    var device = Model.reconnectDeviceFromSession(previous)
    var target = Model.sessionTargetLabel(previous, devices)
    var shouldReconnect = autoReconnect && reconnectAttempts < 3 && device !== null
    var removeOutput = shouldReconnect || dropPolicy === "remove"
    var updated = {}
    for (var key in previous) {
      if (Object.prototype.hasOwnProperty.call(previous, key)) updated[key] = previous[key]
    }
    updated.phase = "disconnected"
    updated.pid = 0
    updated.detail = String(detail || "")
    activeSession = updated
    eventPhase = "disconnected"
    stopEvents()
    handlingDrop = true
    lastError = ""
    if (shouldReconnect) {
      reconnectAttempts = reconnectAttempts + 1
      pendingReconnectDevice = device
      notify(Model.tr(uiLanguage, "tv_disconnected"),
        Model.tr(uiLanguage, "reconnecting_to") + " " + target, "critical")
      removeProc.command = [pythonPath, sessionScript, "remove"]
      sessionBusy = true
      removeProc.running = true
    } else if (removeOutput) {
      notify(Model.tr(uiLanguage, "tv_disconnected"),
        Model.tr(uiLanguage, "monitor_removed"), "critical")
      removeProc.command = [pythonPath, sessionScript, "remove"]
      sessionBusy = true
      removeProc.running = true
    } else {
      notify(Model.tr(uiLanguage, "tv_disconnected"),
        Model.tr(uiLanguage, "monitor_kept"), "normal")
      keepProc.command = [pythonPath, sessionScript, "keep"]
      keepProc.running = true
    }
  }

  // M6: idempotent teardown for plugin disable/remove or a manual IPC call.
  function teardownSession() {
    if (sessionBusy || teardownProc.running) return
    sessionBusy = true
    handlingDrop = false
    pendingReconnectDevice = null
    teardownProc.command = [pythonPath, sessionScript, "teardown"]
    teardownProc.running = true
  }

  function cleanupOrphan(name) {
    if (sessionBusy || !name) return
    lastError = ""
    sessionBusy = true
    cleanupProc.command = [pythonPath, sessionScript, "cleanup", "--name", String(name)]
    cleanupProc.running = true
  }

  function startDiscovery() {
    if (discoveryProc.running || !ready) return
    // Discovery only needs the Python modules; capture binaries are required
    // later, at cast time.
    if (!pythonOk) {
      lastError = "Missing Python dependencies — open Doctor"
      return
    }
    discovering = true
    discoveryProc.command = [pythonPath, castEngine, "discovery", "--watch"]
    discoveryProc.running = true
  }

  // --- M5 phase event stream --------------------------------------------
  // The engine writes JSONL phase events to its log; session.py follows only
  // this run's slice and re-emits them on stdout. The panel/service state is
  // therefore driven by events, not by polling session.json.
  function startEvents() {
    if (eventsProc.running || activeSession === null) return
    if (String(activeSession.phase || "") === "disconnected") return
    eventPhase = String(activeSession.phase || "")
    eventsProc.command = [pythonPath, sessionScript, "events"]
    eventsProc.running = true
  }

  function stopEvents() {
    eventsRetries = 0
    if (eventsProc.running) eventsProc.running = false
  }

  function handlePhaseEvent(ev) {
    if (!ev || ev.type === "") return
    if (ev.phase === "" && ev.detail === "") return
    var previous = activeSession || {}
    var prevPhase = String(previous.phase || "")
    if (ev.phase !== "") {
      eventPhase = ev.phase
      if (ev.detail !== "") eventDetail = ev.detail
      var updated = {}
      for (var key in previous) {
        if (Object.prototype.hasOwnProperty.call(previous, key)) updated[key] = previous[key]
      }
      updated.phase = ev.phase
      if (ev.url !== "") updated.url = ev.url
      if (ev.detail !== "") updated.detail = ev.detail
      activeSession = updated
    }

    if (ev.phase === "casting" && prevPhase !== "casting") {
      reconnectAttempts = 0
      reconnectTimer.stop()
      notify(Model.tr(uiLanguage, "connected"), activeTarget, "normal")
      refreshMonitors()
    } else if (ev.phase === "error") {
      lastError = ev.detail !== "" ? ev.detail : Model.tr(uiLanguage, "cast_failed")
      notify(Model.tr(uiLanguage, "cast_failed"), lastError, "critical")
      Qt.callLater(root.reconcileSession)
    } else if (ev.phase === "disconnected") {
      if (userInitiatedStop) {
        eventPhase = ""
        activeSession = null
        refreshMonitors()
        reconcileSession()
      } else {
        handleUnexpectedDrop(ev.detail)
      }
    } else if (ev.phase === "idle" || ev.phase === "stopped") {
      var wasCasting = prevPhase === "casting" || prevPhase === "starting"
      if (wasCasting && !userInitiatedStop) {
        handleUnexpectedDrop(ev.detail)
      } else {
        eventPhase = ""
        activeSession = null
        refreshMonitors()
        reconcileSession()
      }
    }
  }

  function scheduleReconnect() {
    if (!autoReconnect || reconnectAttempts >= 3) return
    var device = Model.reconnectDeviceFromSession(activeSession)
    if (!device) return
    reconnectAttempts = reconnectAttempts + 1
    reconnectTimer.device = device
    reconnectTimer.interval = 3000 * reconnectAttempts
    reconnectTimer.restart()
    notify(Model.tr(uiLanguage, "auto_reconnect"), device.name, "low")
  }

  // --- M5 hotplug handling ----------------------------------------------
  function applyMonitors(list) {
    var previous = monitorNames
    monitors = list
    var names = []
    var physical = []
    for (var i = 0; i < list.length; i++) {
      var name = String(list[i].name || "")
      names.push(name)
      if (list[i].virtual !== true) physical.push(name)
    }
    monitorNames = names
    if (activeSession === null) return
    if (names.indexOf(String(activeSession.name || "")) < 0) {
      reconcileSession()
      return
    }
    // A physical hotplug must never break the virtual output: re-place it
    // adjacent to whatever display remains, without touching the stream.
    if (previous.length > 0) {
      var prevPhysical = []
      for (var j = 0; j < previous.length; j++) {
        if (String(previous[j]).indexOf("HEADLESS") !== 0) prevPhysical.push(String(previous[j]))
      }
      if (prevPhysical.sort().join(",") !== physical.sort().join(",")) scheduleRearrange()
    }
  }

  function scheduleRearrange() {
    rearrangeTimer.restart()
  }

  function runRearrange() {
    if (rearrangeProc.running) return
    rearrangeProc.command = [pythonPath, sessionScript, "rearrange"]
    rearrangeProc.running = true
  }

  Component.onCompleted: {
    refreshMonitors()
    refreshDisplayRules()
    runDoctor()
    reconcileSession()
  }

  // --- dependency doctor: resolve deps, then start discovery -------------
  Process {
    id: doctorProc
    command: ["python3", root.bootstrap, "doctor"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var d = Model.parseDoctor(text)
        root.doctor = d
        if (d.python) root.pythonPath = d.python
        root.ready = true
        if (d.pythonOk) {
          root.lastError = ""
          root.startDiscovery()
        } else {
          root.lastError = "Missing Python dependencies — open Doctor"
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  // --- rootless dependency provisioning --------------------------------
  Process {
    id: provisionProc
    command: ["python3", root.bootstrap, "provision"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        root.provisioning = false
        root.runDoctor()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        root.provisioning = false
        if (text.trim() !== "") root.lastError = text.trim()
      }
    }
    onExited: root.provisioning = false
  }

  // --- long-lived discovery browser (incremental JSONL snapshots) --------
  Process {
    id: discoveryProc
    stdout: SplitParser {
      onRead: function(line) {
        var ev = Model.parseDiscoveryEvent(line)
        if (ev.type === "discovery") {
          root.lastDiscoveryAt = Date.now()
          if (!ev.browsing) root.discoveryFailures = 0
          root.devices = root.mergeManualHosts(ev.devices)
          root.discovering = ev.browsing
        } else if (ev.type === "phase" && ev.phase === "error") {
          root.lastError = ev.detail
          root.discoveryFailures = root.discoveryFailures + 1
          root.discovering = false
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: {
      root.discovering = false
      if (restartDiscovery.pending) {
        restartDiscovery.pending = false
        restartDiscovery.restart()
      } else if (root.ready && root.pythonOk && root.discoveryFailures < 5
                 && root.discoveryFailures > 0) {
        root.discoveryRetry.interval = Math.min(
          60000, 5000 * Math.pow(2, root.discoveryFailures - 1))
        root.discoveryRetry.restart()
      }
    }
  }

  Timer {
    id: discoveryRetry
    repeat: false
    onTriggered: root.startDiscovery()
  }

  Timer {
    id: restartDiscovery
    property bool pending: false
    interval: 150
    repeat: false
    onTriggered: root.startDiscovery()
  }

  // --- manual IP entry (mDNS-blocked / cross-VLAN receivers, R6) ---------
  Process {
    id: manualProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var ev = Model.parseDiscoveryEvent(text)
        if (ev.devices.length === 0) return
        var copy = {}
        for (var key in root.manualDevices) {
          if (Object.prototype.hasOwnProperty.call(root.manualDevices, key))
            copy[key] = root.manualDevices[key]
        }
        for (var i = 0; i < ev.devices.length; i++) {
          var host = String(ev.devices[i].host || "").trim()
          if (host !== "") copy[host] = ev.devices[i]
        }
        root.manualDevices = copy
        root.devices = root.mergeManualHosts(root.devices)
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  // --- session processes (M2) -------------------------------------------
  Process {
    id: statusProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        if (root.sessionBusy) return
        var r = Model.parseSession(text)
        if (!r.ok) return
        root.activeSession = r.active
        if (!r.active) {
          root.eventPhase = ""
          return
        }
        var phase = String(r.active.phase || "")
        // A finished cast leaves phase "stopped"; a crashed engine leaves a
        // dead pid or a vanished output. Both converge automatically.
        if (phase === "disconnected") {
          root.eventPhase = "disconnected"
          root.stopEvents()
          if (r.outputPresent === false) root.reconcileSession()
          return
        }
        if (phase === "stopped"
            || ((phase === "starting" || phase === "casting")
                && (r.alive === false || r.outputPresent === false))) {
          root.reconcileSession()
        } else {
          root.startEvents()
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  Process {
    id: reconcileProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.activeSession = r.active
        root.sessionOrphans = r.orphans
        if (!r.ok && r.error !== "") root.lastError = r.error
        if (r.cleaned) root.refreshMonitors()
        if (r.active && String(r.active.phase || "") === "disconnected") {
          root.eventPhase = "disconnected"
          root.stopEvents()
        } else if (r.active) {
          root.startEvents()
        } else {
          root.eventPhase = ""
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  Process {
    id: createProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.sessionBusy = false
        if (r.ok) {
          root.activeSession = r.active
          root.eventPhase = "starting"
          root.lastError = ""
          root.startEvents()
        } else {
          root.lastError = r.error !== "" ? r.error : "Could not create the virtual monitor"
          root.eventPhase = ""
          root.notify(Model.tr(root.uiLanguage, "cast_failed"), root.lastError, "critical")
        }
        root.refreshMonitors()
        root.refreshSession()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: if (root.sessionBusy) root.sessionBusy = false
  }

  Process {
    id: removeProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.sessionBusy = false
        var reconnectDevice = root.pendingReconnectDevice
        var wasDrop = root.handlingDrop
        root.pendingReconnectDevice = null
        root.handlingDrop = false
        if (r.ok) {
          root.activeSession = null
          root.eventPhase = ""
          root.stopEvents()
          root.userInitiatedStop = false
          if (reconnectDevice === null) root.reconnectAttempts = 0
          if (!wasDrop) {
            root.notify(Model.tr(root.uiLanguage, "disconnected"),
              root.deviceNickname !== "" ? root.deviceNickname : "Omascreencast", "normal")
          }
          if (r.warnings.length > 0) root.lastError = r.warnings.join("; ")
          if (reconnectDevice !== null) {
            root.reconnectTimer.device = reconnectDevice
            root.reconnectTimer.interval = 1000
            root.reconnectTimer.restart()
          }
        } else {
          root.lastError = r.error !== "" ? r.error : "Could not stop the session"
          root.notify(Model.tr(root.uiLanguage, "cast_failed"), root.lastError, "critical")
        }
        root.refreshMonitors()
        root.reconcileSession()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: if (root.sessionBusy) root.sessionBusy = false
  }

  Process {
    id: cleanupProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.sessionBusy = false
        if (!r.ok) root.lastError = r.error !== "" ? r.error : "Could not remove the output"
        root.refreshMonitors()
        root.reconcileSession()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: if (root.sessionBusy) root.sessionBusy = false
  }

  // M6: keep the virtual output after a receiver/network drop.
  Process {
    id: keepProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.handlingDrop = false
        if (r.ok && r.active) {
          root.activeSession = r.active
          root.eventPhase = "disconnected"
        } else if (r.error !== "") {
          root.lastError = r.error
        }
        root.refreshMonitors()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  // M6: idempotent teardown for plugin disable/remove or the IPC surface.
  Process {
    id: teardownProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        root.sessionBusy = false
        if (!r.ok) root.lastError = r.error !== "" ? r.error : "Could not tear down the session"
        root.activeSession = null
        root.eventPhase = ""
        root.stopEvents()
        root.pendingReconnectDevice = null
        root.handlingDrop = false
        root.refreshMonitors()
        root.reconcileSession()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: if (root.sessionBusy) root.sessionBusy = false
  }

  // --- monitor arranger (M4) --------------------------------------------
  Process {
    id: displayRulesProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseDisplayRules(text)
        if (r.ok) {
          root.displayRules = r.rules
          if (root.displayError !== "" && r.error === "") root.displayError = ""
        } else if (r.error !== "") {
          root.displayError = r.error
        }
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.displayError = text.trim()
    }
  }

  Process {
    id: displayApplyProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseDisplayRules(text)
        root.displayBusy = false
        if (!r.ok) {
          root.displayError = r.error !== "" ? r.error : "Could not apply the display layout"
          // A refused/failed trial never took effect, so do not "revert" it.
          root.trialLayout = null
          root.trialRemaining = 0
          displayTrialTimer.stop()
        }
        root.refreshMonitors()
        root.refreshDisplayRules()
        var pending = root.pendingRevertPayload
        root.pendingRevertPayload = null
        if (pending !== null) root.applyDisplayRule(pending, true)
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.displayError = text.trim()
    }
    onExited: if (root.displayBusy) root.displayBusy = false
  }

  // M6 layout trial countdown (auto-revert when it reaches zero).
  Timer {
    id: displayTrialTimer
    interval: 1000
    repeat: true
    onTriggered: {
      if (!root.trialLayout) return
      root.trialRemaining = root.trialRemaining - 1
      if (root.trialRemaining <= 0) root.revertDisplayTrial()
    }
  }

  Process {
    id: displaySaveProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseDisplayRules(text)
        root.displayBusy = false
        if (!r.ok) root.displayError = r.error !== "" ? r.error : "Could not save the display layout"
        root.refreshMonitors()
        root.refreshDisplayRules()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.displayError = text.trim()
    }
    onExited: if (root.displayBusy) root.displayBusy = false
  }

  // --- live phase events (M5) -------------------------------------------
  Process {
    id: eventsProc
    stdout: SplitParser {
      onRead: function(line) {
        root.handlePhaseEvent(Model.parseSessionEvent(line))
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
    onExited: {
      // The follower ends on a terminal phase; if the engine is still alive
      // (e.g. the follower was restarted) retry a bounded number of times.
      if (root.activeSession !== null && !root.userInitiatedStop
          && root.casting && root.eventsRetries < 3) {
        root.eventsRetries = root.eventsRetries + 1
        eventsRetry.restart()
      }
    }
  }

  Timer {
    id: eventsRetry
    interval: 1000
    repeat: false
    onTriggered: root.startEvents()
  }

  // Re-place the virtual output after a physical hotplug (M5).
  Process {
    id: rearrangeProc
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var r = Model.parseSession(text)
        if (r.ok && r.active) root.activeSession = r.active
        else if (r.error !== "") root.lastError = r.error
        root.refreshMonitors()
      }
    }
    stderr: StdioCollector {
      waitForEnd: true
      onStreamFinished: if (text.trim() !== "") root.lastError = text.trim()
    }
  }

  Timer {
    id: rearrangeTimer
    interval: 400
    repeat: false
    onTriggered: root.runRearrange()
  }

  // Elapsed-time ticker for the bar label (state only, no file polling).
  Timer {
    interval: 1000
    repeat: true
    running: root.activePhase === "casting"
    onTriggered: root.elapsedTick = root.elapsedTick + 1
  }

  // Auto-reconnect after a network drop (opt-in).
  Timer {
    id: reconnectTimer
    property var device: null
    repeat: false
    onTriggered: {
      if (device && !root.sessionBusy && !root.casting)
        root.connectDevice(device, root.connectOptions())
    }
  }

  // Desktop notifications reuse the shell's notification server.
  Process {
    id: notifyProc
    onExited: root.nextNotify()
  }

  // --- display enumeration ----------------------------------------------
  Process {
    id: monitorsProc
    command: ["hyprctl", "monitors", "all", "-j"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.applyMonitors(Model.parseMonitors(text))
    }
  }

  // Keep the display list fresh while the shell runs; hyprctl is cheap and
  // hotplug is otherwise invisible to a closed panel.
  Timer {
    interval: 10000
    running: true
    repeat: true
    onTriggered: root.refreshMonitors()
  }

  // --- IPC surface for keybinds and scripts (M5) ------------------------
  IpcHandler {
    target: "omascreencast"

    function start(payloadJson: string): string {
      try {
        var payload = payloadJson ? JSON.parse(payloadJson) : ({})
        var device = null
        if (payload && (payload.uuid || payload.host)) device = payload
        else {
          for (var i = 0; i < root.devices.length; i++) {
            if (root.devices[i].online !== false) { device = root.devices[i]; break }
          }
        }
        if (!device) {
          root.refreshDevices()
          return JSON.stringify({ ok: false, error: "no Chromecast device available" })
        }
        var opts = root.connectOptions()
        if (payload.res) opts.res = payload.res
        if (payload.fps) opts.fps = payload.fps
        if (payload.bitrate) opts.bitrate = payload.bitrate
        if (payload.codec) opts.codec = payload.codec
        root.connectDevice(device, opts)
        return JSON.stringify({ ok: true, state: "connecting" })
      } catch (e) {
        return JSON.stringify({ ok: false, error: String(e) })
      }
    }

    function stop(): string {
      root.userInitiatedStop = true
      root.disconnect()
      return JSON.stringify({ ok: true, state: "stopping" })
    }

    function teardown(): string {
      root.teardownSession()
      return JSON.stringify({ ok: true, state: "tearing-down" })
    }

    function status(): string {
      return JSON.stringify({
        ok: true,
        state: root.barState,
        language: root.uiLanguage,
        active: root.activeSession,
        elapsed: root.castElapsedSeconds,
        monitors: root.monitorCount,
        devices: root.deviceCount,
        trial: root.trialActive,
        dropPolicy: root.dropPolicy
      })
    }

    function refresh(): string {
      root.refreshMonitors()
      root.refreshDevices()
      root.refreshDisplayRules()
      root.refreshSession()
      return "ok"
    }
  }
}
