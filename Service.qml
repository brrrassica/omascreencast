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
  readonly property string pluginVersion: "0.1.0"

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

  readonly property int deviceCount: devices.length
  readonly property int monitorCount: monitors.length
  readonly property bool depsOk: doctor.ok === true
  readonly property bool pythonOk: doctor.pythonOk === true
  readonly property bool pipelineOk: doctor.pipelineOk === true
  readonly property bool casting: activeSession !== null
    && String(activeSession.phase || "") !== "stopped"

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
  function applyDisplayRule(payload) {
    if (displayBusy || !payload) return
    displayError = ""
    displayBusy = true
    displayApplyProc.command = [
      pythonPath, displayScript, "apply", "--rule", JSON.stringify(payload)
    ]
    displayApplyProc.running = true
  }

  // Apply and persist into monitors.lua (backed up + atomic).
  function saveDisplayRule(payload) {
    if (displayBusy || !payload) return
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
    sessionBusy = true
    createProc.command = cmd
    createProc.running = true
  }

  function disconnect() {
    if (sessionBusy || activeSession === null) return
    lastError = ""
    sessionBusy = true
    removeProc.command = [pythonPath, sessionScript, "remove"]
    removeProc.running = true
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
        if (!r.active) return
        var phase = String(r.active.phase || "")
        // A finished cast leaves phase "stopped"; a crashed engine leaves a
        // dead pid or a vanished output. Both converge automatically.
        if (phase === "stopped"
            || ((phase === "starting" || phase === "casting")
                && (r.alive === false || r.outputPresent === false)))
          root.reconcileSession()
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
          root.lastError = ""
        } else {
          root.lastError = r.error !== "" ? r.error : "Could not create the virtual monitor"
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
        if (r.ok) {
          root.activeSession = null
          if (r.warnings.length > 0) root.lastError = r.warnings.join("; ")
        } else {
          root.lastError = r.error !== "" ? r.error : "Could not stop the session"
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
        if (!r.ok) root.displayError = r.error !== "" ? r.error : "Could not apply the display layout"
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

  // --- display enumeration ----------------------------------------------
  Process {
    id: monitorsProc
    command: ["hyprctl", "monitors", "all", "-j"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.monitors = Model.parseMonitors(text)
    }
  }

  // Poll the unified session state while a cast is active so the panel tracks
  // phase changes (starting -> casting -> stopped) written by the engine.
  Timer {
    interval: 2000
    repeat: true
    running: root.casting
    onTriggered: root.refreshSession()
  }

  // Keep the display list fresh while the shell runs; hyprctl is cheap and
  // hotplug is otherwise invisible to a closed panel.
  Timer {
    interval: 10000
    running: true
    repeat: true
    onTriggered: root.refreshMonitors()
  }
}
