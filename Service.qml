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
  // Interpreter the engine should use: system python, or the private venv when
  // the system lacks pychromecast/zeroconf. Resolved by `doctor`.
  property string pythonPath: "python3"
  property bool ready: false

  readonly property int deviceCount: devices.length
  readonly property int monitorCount: monitors.length
  readonly property bool depsOk: doctor.ok === true
  readonly property bool casting: activeSession !== null
    && String(activeSession.phase || "") !== "stopped"

  // --- actions -----------------------------------------------------------

  function refreshMonitors() {
    if (!monitorsProc.running) monitorsProc.running = true
  }

  // Restart the long-lived discovery browser to force a fresh sweep. A direct
  // false->true flip races the async process exit, so route the restart
  // through a one-shot timer once the old browser has actually stopped.
  function refreshDevices() {
    if (discoveryProc.running) {
      restartDiscovery.pending = true
      discoveryProc.running = false
    } else {
      startDiscovery()
    }
  }

  function addManualHost(host) {
    var h = String(host || "").trim()
    if (h === "") return
    manualProc.command = [pythonPath, castEngine, "discovery", "--host", h]
    if (!manualProc.running) manualProc.running = true
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

  // Compare recorded state with Hyprland reality: cleans stale sessions and
  // reports untracked HEADLESS-* outputs (never removes them automatically).
  function reconcileSession() {
    if (sessionBusy || reconcileProc.running) return
    reconcileProc.command = [pythonPath, sessionScript, "reconcile"]
    reconcileProc.running = true
  }

  function connectDevice(device) {
    if (!device || sessionBusy) return
    if (casting) {
      lastError = "Disconnect the active session first"
      return
    }
    var cmd = [pythonPath, sessionScript, "create"]
    if (device.uuid) cmd.push("--device", String(device.uuid))
    if (device.host) cmd.push("--host", String(device.host))
    if (device.name) cmd.push("--device-name", String(device.name))
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
    if (!depsOk) {
      lastError = "Missing dependencies — open Doctor"
      return
    }
    discovering = true
    discoveryProc.command = [pythonPath, castEngine, "discovery", "--watch"]
    discoveryProc.running = true
  }

  Component.onCompleted: {
    refreshMonitors()
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
        if (d.ok) {
          root.lastError = ""
          root.startDiscovery()
        } else {
          root.lastError = "Missing dependencies — open Doctor"
        }
      }
    }
  }

  // --- long-lived discovery browser (incremental JSONL snapshots) --------
  Process {
    id: discoveryProc
    stdout: SplitParser {
      onRead: function(line) {
        var ev = Model.parseDiscoveryEvent(line)
        if (ev.type === "discovery") {
          root.devices = ev.devices
          root.discovering = false
        } else if (ev.type === "phase" && ev.phase === "error") {
          root.lastError = ev.detail
          root.discovering = false
        }
      }
    }
    onExited: {
      root.discovering = false
      if (restartDiscovery.pending) {
        restartDiscovery.pending = false
        restartDiscovery.restart()
      }
    }
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
        var merged = root.devices.slice()
        for (var i = 0; i < ev.devices.length; i++) {
          var d = ev.devices[i]
          var exists = false
          for (var j = 0; j < merged.length; j++) {
            if (merged[j].host === d.host) { exists = true; break }
          }
          if (!exists) merged.push(d)
        }
        root.devices = merged
      }
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
        // A stopped session is stale state: converge automatically.
        if (r.active && String(r.active.phase || "") === "stopped")
          root.reconcileSession()
      }
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
