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

  // --- shared state (read by Panel.qml) ---
  property var monitors: []
  property var devices: []
  property bool discovering: false
  property var doctor: ({})
  property string lastError: ""
  // Interpreter the engine should use: system python, or the private venv when
  // the system lacks pychromecast/zeroconf. Resolved by `doctor`.
  property string pythonPath: "python3"
  property bool ready: false

  readonly property int deviceCount: devices.length
  readonly property int monitorCount: monitors.length
  readonly property bool depsOk: doctor.ok === true

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

  // --- display enumeration ----------------------------------------------
  Process {
    id: monitorsProc
    command: ["hyprctl", "monitors", "all", "-j"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: root.monitors = Model.parseMonitors(text)
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
}
