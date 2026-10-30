.pragma library

// omascreencast — pure view-model helpers shared by Panel.qml and the service.
//
// Keeps JSON -> UI mapping testable without a live shell. Grows through M1–M4
// as hyprctl / discovery payloads are parsed.

// Parse the JSON array emitted by `hyprctl monitors all -j`.
function parseMonitors(raw) {
  var list = []
  try {
    var monitors = JSON.parse(raw)
    for (var i = 0; i < monitors.length; i++) {
      var m = monitors[i]
      list.push({
        name: String(m.name || ""),
        description: String(m.description || ""),
        width: Number(m.width || 0),
        height: Number(m.height || 0),
        refreshRate: Number(m.refreshRate || 0),
        x: Number(m.x || 0),
        y: Number(m.y || 0),
        scale: Number(m.scale || 1),
        transform: Number(m.transform || 0),
        disabled: m.disabled === true,
        focused: m.focused === true,
        virtual: String(m.name || "").indexOf("HEADLESS") === 0
      })
    }
  } catch (e) {
    console.warn("omascreencast: parseMonitors failed: " + e)
  }
  return list
}

// Parse the discovery payload emitted by scripts/cast_engine.py discovery.
// Accepts either a JSON array or a JSON string of one.
function parseDevices(raw) {
  var list = []
  try {
    var devices = typeof raw === "string" ? JSON.parse(raw) : raw
    if (!(devices instanceof Array)) return list
    for (var i = 0; i < devices.length; i++) {
      var d = devices[i]
      list.push({
        uuid: String(d.uuid || ""),
        name: String(d.name || ""),
        host: String(d.host || ""),
        port: Number(d.port || 8009),
        model: String(d.model || ""),
        online: d.online === true,
        codecs: d.codecs instanceof Array ? d.codecs : []
      })
    }
  } catch (e) {
    console.warn("omascreencast: parseDevices failed: " + e)
  }
  return list
}

// Parse one JSONL line from `cast_engine.py discovery` (one-shot or --watch).
// Returns { type, phase, detail, devices } with safe defaults so a malformed
// line never throws in the panel.
function parseDiscoveryEvent(raw) {
  var result = { type: "", phase: "", detail: "", devices: [] }
  try {
    var msg = JSON.parse(String(raw || ""))
    if (!msg || typeof msg !== "object") return result
    result.type = String(msg.type || "")
    result.phase = String(msg.phase || "")
    result.detail = String(msg.detail || "")
    if (msg.devices instanceof Array) result.devices = parseDevices(msg.devices)
  } catch (e) {
    console.warn("omascreencast: parseDiscoveryEvent failed: " + e)
  }
  return result
}

// Parse the JSON emitted by scripts/session.py (status / create / remove /
// reconcile). Returns a safe shape so a malformed line never throws in QML.
function parseSession(raw) {
  var result = {
    ok: false,
    active: null,
    error: "",
    cleaned: false,
    reasons: [],
    orphans: [],
    warnings: []
  }
  try {
    var d = JSON.parse(String(raw || ""))
    if (!d || typeof d !== "object") return result
    result.ok = d.ok !== false
    result.error = String(d.error || "")
    result.cleaned = d.cleaned === true
    result.reasons = d.reasons instanceof Array ? d.reasons.map(String) : []
    result.orphans = d.orphans instanceof Array ? d.orphans.map(String) : []
    result.warnings = d.warnings instanceof Array ? d.warnings.map(String) : []
    if (d.active && typeof d.active === "object") {
      var a = d.active
      result.active = {
        name: String(a.name || a.output || ""),
        deviceUuid: String(a.deviceUuid || a.device || ""),
        deviceName: String(a.deviceName || ""),
        deviceHost: String(a.deviceHost || ""),
        url: String(a.url || ""),
        port: Number(a.port || 0),
        codec: String(a.codec || ""),
        res: String(a.res || ""),
        position: String(a.position || ""),
        scale: Number(a.scale || 1),
        phase: String(a.phase || ""),
        pid: Number(a.pid || 0)
      }
    }
  } catch (e) {
    console.warn("omascreencast: parseSession failed: " + e)
  }
  return result
}

// Human label for the receiver a session is casting to, preferring the name
// recorded at create time and falling back to the live discovery list.
function sessionTargetLabel(session, devices) {
  if (!session) return ""
  var name = String(session.deviceName || "")
  if (name !== "") return name
  var uuid = String(session.deviceUuid || "")
  var host = String(session.deviceHost || "")
  var list = devices instanceof Array ? devices : []
  for (var i = 0; i < list.length; i++) {
    var d = list[i]
    if ((uuid !== "" && String(d.uuid || "") === uuid) ||
        (host !== "" && String(d.host || "") === host)) {
      return String(d.name || d.host || "")
    }
  }
  return host !== "" ? host : uuid
}

// True when the active session targets the given discovered device. A manual
// host (no uuid) is matched by its IP.
function sessionMatchesDevice(session, device) {
  if (!session || !device) return false
  var uuid = String(session.deviceUuid || "")
  var devUuid = String(device.uuid || "")
  if (uuid !== "" && devUuid !== "") return uuid === devUuid
  var host = String(session.deviceHost || "")
  var devHost = String(device.host || "")
  return host !== "" && host === devHost
}

// Suffix for a virtual output row: " → Sony Bravia" when it is the cast target.
function monitorSessionLabel(monitor, session, devices) {
  if (!monitor || !session) return ""
  if (String(session.name || "") !== String(monitor.name || "")) return ""
  var target = sessionTargetLabel(session, devices)
  return target !== "" ? " → " + target : ""
}

// Short uppercase phase label for the bar icon tooltip / hero.
function sessionPhaseLabel(session) {
  if (!session || String(session.name || "") === "") return ""
  var phase = String(session.phase || "")
  if (phase === "casting") return "CASTING"
  if (phase === "starting") return "CONNECTING"
  if (phase === "stopped") return "STOPPED"
  return phase !== "" ? phase.toUpperCase() : "ACTIVE"
}

// Parse the JSON emitted by scripts/bootstrap.py doctor.
function parseDoctor(raw) {
  var result = {
    ok: false,
    missingBinaries: [],
    missingPython: [],
    venv: "",
    venvExists: false,
    venvReady: false,
    python: "python3",
    streamPort: 0,
    fastPath: "",
    firewallRule: ""
  }
  try {
    var d = JSON.parse(String(raw || ""))
    if (!d || typeof d !== "object") return result
    result.ok = d.ok === true
    result.missingBinaries = d.missingBinaries instanceof Array ? d.missingBinaries : []
    result.missingPython = d.missingPython instanceof Array ? d.missingPython : []
    result.venv = String(d.venv || "")
    result.venvExists = d.venvExists === true
    result.venvReady = d.venvReady === true
    result.python = String(d.python || "python3")
    result.streamPort = Number(d.streamPort || 0)
    result.fastPath = String(d.fastPath || "")
    result.firewallRule = String(d.firewallRule || "")
  } catch (e) {
    console.warn("omascreencast: parseDoctor failed: " + e)
  }
  return result
}

// Human label for a discovered device: name plus host, since mDNS names are
// often generic and the IP is what the user recognises.
function deviceLabel(device) {
  if (!device) return ""
  var name = String(device.name || "")
  var host = String(device.host || "")
  if (name === "") return host
  if (host === "" || name === host) return name
  return name + " · " + host
}

// Short codec summary for a device, e.g. "HEVC · H.264".
function deviceCodecLabel(device) {
  if (!device || !(device.codecs instanceof Array) || device.codecs.length === 0) return "H.264"
  var names = { hevc: "HEVC", h264: "H.264", vp9: "VP9", av1: "AV1" }
  var out = []
  for (var i = 0; i < device.codecs.length; i++) {
    var key = String(device.codecs[i])
    out.push(names[key] !== undefined ? names[key] : key.toUpperCase())
  }
  return out.join(" · ")
}

// Human label for a monitor: name plus resolution, with a network badge for
// virtual (HEADLESS-*) outputs.
function monitorLabel(monitor) {
  if (!monitor) return ""
  var name = String(monitor.name || "")
  var res = Number(monitor.width) > 0 && Number(monitor.height) > 0
    ? monitor.width + "×" + monitor.height : ""
  if (res === "") return name
  return name + " · " + res
}
