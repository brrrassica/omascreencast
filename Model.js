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
        make: String(m.make || ""),
        model: String(m.model || ""),
        width: Number(m.width || 0),
        height: Number(m.height || 0),
        refreshRate: Number(m.refreshRate || 0),
        x: Number(m.x || 0),
        y: Number(m.y || 0),
        scale: Number(m.scale || 1),
        transform: Number(m.transform || 0),
        disabled: m.disabled === true,
        focused: m.focused === true,
        mirrorOf: String(m.mirrorOf || "none"),
        availableModes: m.availableModes instanceof Array ? m.availableModes.map(String) : [],
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
  var result = { type: "", phase: "", detail: "", devices: [], browsing: false }
  try {
    var msg = JSON.parse(String(raw || ""))
    if (!msg || typeof msg !== "object") return result
    result.type = String(msg.type || "")
    result.phase = String(msg.phase || "")
    result.detail = String(msg.detail || "")
    result.browsing = msg.browsing === true
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
    warnings: [],
    alive: false,
    outputPresent: true
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
    result.alive = d.alive === true
    // Only an explicit false is a missing output; null means "could not check".
    result.outputPresent = d.outputPresent !== false
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
    pythonOk: false,
    pipelineOk: false,
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
    result.pythonOk = d.pythonOk === true
    result.pipelineOk = d.pipelineOk === true
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

// ---------------------------------------------------------------------------
// M4 — monitor arranger helpers
// ---------------------------------------------------------------------------

// `hyprctl` reports available modes as "1920x1080@60.00Hz"; monitors.lua wants
// "1920x1080@60". Unknown/edge forms are returned unchanged so the UI never
// silently invents a mode.
function normalizeMode(raw) {
  var mode = String(raw || "").trim()
  if (mode === "" || mode === "preferred" || mode === "highres" || mode === "highrr") return mode
  var m = mode.match(/^(\d+x\d+@\d+(?:\.\d+)?)Hz$/)
  if (!m) return mode
  var parts = m[1].split("@")
  var hz = Number(parts[1])
  if (!isFinite(hz)) return m[1]
  return parts[0] + "@" + String(Math.round(hz * 1000) / 1000)
}

// Modes suitable for the per-display dropdown.
function monitorModes(monitor) {
  var out = []
  var seen = {}
  var raw = monitor && monitor.availableModes instanceof Array ? monitor.availableModes : []
  for (var i = 0; i < raw.length; i++) {
    var mode = normalizeMode(raw[i])
    if (mode !== "" && seen[mode] === undefined) { seen[mode] = true; out.push(mode) }
  }
  return out
}

// Parse `display_config.py rules` JSON into a safe shape.
function parseDisplayRules(raw) {
  var result = { ok: false, config: "", rules: [], error: "" }
  try {
    var d = JSON.parse(String(raw || ""))
    if (!d || typeof d !== "object") return result
    result.ok = d.ok === true
    result.config = String(d.config || "")
    result.error = String(d.error || "")
    var rules = d.rules instanceof Array ? d.rules : []
    for (var i = 0; i < rules.length; i++) {
      var r = rules[i] || {}
      var fields = {}
      var f = r.fields && typeof r.fields === "object" ? r.fields : {}
      for (var key in f) {
        if (Object.prototype.hasOwnProperty.call(f, key)) fields[key] = f[key]
      }
      result.rules.push({
        output: String(r.output || fields.output || ""),
        fields: fields,
        nonLiteral: r.nonLiteral instanceof Array ? r.nonLiteral.map(String) : []
      })
    }
  } catch (e) {
    console.warn("omascreencast: parseDisplayRules failed: " + e)
  }
  return result
}

// Stable persistence selector, mirroring display_config.py.
function monitorSelector(monitor) {
  if (!monitor) return ""
  var description = String(monitor.description || "").trim()
  if (description !== "") return "desc:" + description
  return String(monitor.name || "")
}

// Find the persisted rule for a monitor (by name or desc: selector).
function displayRuleFor(monitor, rules) {
  if (!monitor) return null
  var name = String(monitor.name || "")
  var selector = monitorSelector(monitor)
  var list = rules instanceof Array ? rules : []
  for (var i = 0; i < list.length; i++) {
    var output = String(list[i].output || "")
    if (output !== "" && (output === name || output === selector)) return list[i]
  }
  return null
}

// The mode Hyprland is currently using, mapped onto an advertised mode string
// when possible ("1920x1080@60.00Hz" -> "1920x1080@60").
function currentMode(monitor) {
  var modes = monitorModes(monitor)
  var width = Number(monitor && monitor.width || 0)
  var height = Number(monitor && monitor.height || 0)
  var hz = Number(monitor && monitor.refreshRate || 60)
  for (var i = 0; i < modes.length; i++) {
    var m = modes[i].match(/^(\d+)x(\d+)@(\d+(?:\.\d+)?)$/)
    if (!m) continue
    if (Number(m[1]) === width && Number(m[2]) === height
        && Math.abs(Number(m[3]) - hz) < 0.5) return modes[i]
  }
  if (width > 0 && height > 0)
    return String(width) + "x" + String(height) + "@"
      + String(Math.round(hz * 1000) / 1000)
  return ""
}

// Effective literal fields for a monitor. Live `hyprctl` state is authoritative
// (so an ephemeral Apply is reflected when the user then drags the display);
// the persisted rule only supplies a fallback when hyprctl omits a value.
function displayFieldsFor(monitor, rules) {
  if (!monitor) return {}
  var rule = displayRuleFor(monitor, rules)
  var fields = rule && rule.fields ? rule.fields : {}
  var mirror = String(monitor.mirrorOf || "none")
  return {
    mode: currentMode(monitor) || String(fields.mode || "preferred"),
    position: String(monitor.x || 0) + "x" + String(monitor.y || 0),
    scale: Number(monitor.scale || fields.scale || 1),
    transform: Number(monitor.transform || 0),
    disabled: monitor.disabled === true,
    mirror: mirror === "none" ? "" : mirror
  }
}

function transformLabel(transform) {
  var t = Number(transform || 0)
  if (t === 1) return "90°"
  if (t === 2) return "180°"
  if (t === 3) return "270°"
  return "Normal"
}

// Logical width/height (layout pixels) for canvas geometry.
function monitorLogicalSize(monitor) {
  var scale = Number(monitor && monitor.scale || 1)
  if (!isFinite(scale) || scale <= 0) scale = 1
  var width = Math.round(Number(monitor && monitor.width || 0) / scale)
  var height = Math.round(Number(monitor && monitor.height || 0) / scale)
  // 90°/270° transforms swap the logical axes; canvas bounds, overlap and
  // snapping must use the rotated rectangle.
  var transform = Number(monitor && monitor.transform || 0)
  if (transform % 2 === 1) {
    var swapped = width
    width = height
    height = swapped
  }
  return { width: width, height: height }
}

// Bounding box of the current (or previewed) layout.
function arrangementBounds(monitors) {
  var list = monitors instanceof Array ? monitors : []
  if (list.length === 0) return { minX: 0, minY: 0, maxX: 1, maxY: 1, width: 1, height: 1 }
  var minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity
  for (var i = 0; i < list.length; i++) {
    var size = monitorLogicalSize(list[i])
    var x = Number(list[i].x || 0)
    var y = Number(list[i].y || 0)
    minX = Math.min(minX, x)
    minY = Math.min(minY, y)
    maxX = Math.max(maxX, x + size.width)
    maxY = Math.max(maxY, y + size.height)
  }
  return {
    minX: minX, minY: minY, maxX: maxX, maxY: maxY,
    width: Math.max(1, maxX - minX), height: Math.max(1, maxY - minY)
  }
}

// Names of displays whose logical rectangles overlap (arrangement warning).
function overlappingDisplays(monitors) {
  var list = monitors instanceof Array ? monitors : []
  var pairs = []
  for (var i = 0; i < list.length; i++) {
    if (list[i].disabled === true) continue
    var a = monitorLogicalSize(list[i])
    for (var j = i + 1; j < list.length; j++) {
      if (list[j].disabled === true) continue
      var b = monitorLogicalSize(list[j])
      var ax = Number(list[i].x || 0), ay = Number(list[i].y || 0)
      var bx = Number(list[j].x || 0), by = Number(list[j].y || 0)
      if (ax < bx + b.width && bx < ax + a.width && ay < by + b.height && by < ay + a.height)
        pairs.push([String(list[i].name || ""), String(list[j].name || "")])
    }
  }
  return pairs
}

// Snap a dragged display's top-left to another display's edges.
function snapPosition(monitor, x, y, monitors, threshold) {
  var list = monitors instanceof Array ? monitors : []
  var limit = Number(threshold || 16)
  var size = monitorLogicalSize(monitor)
  var bestX = Number(x), bestY = Number(y)
  var bestDx = limit + 1, bestDy = limit + 1
  for (var i = 0; i < list.length; i++) {
    if (list[i] === monitor || String(list[i].name) === String(monitor && monitor.name)) continue
    var other = monitorLogicalSize(list[i])
    var ox = Number(list[i].x || 0), oy = Number(list[i].y || 0)
    var xCandidates = [ox, ox + other.width, ox - size.width]
    var yCandidates = [oy, oy + other.height, oy - size.height]
    for (var a = 0; a < xCandidates.length; a++) {
      var dx = Math.abs(Number(x) - xCandidates[a])
      if (dx <= limit && dx < bestDx) { bestDx = dx; bestX = xCandidates[a] }
    }
    for (var b = 0; b < yCandidates.length; b++) {
      var dy = Math.abs(Number(y) - yCandidates[b])
      if (dy <= limit && dy < bestDy) { bestDy = dy; bestY = yCandidates[b] }
    }
  }
  return { x: Math.round(bestX), y: Math.round(bestY) }
}

// JSON payload handed to display_config.py apply/save.
function displayRulePayload(monitor, fields) {
  var f = fields || {}
  return {
    name: String(monitor && monitor.name || ""),
    description: String(monitor && monitor.description || ""),
    mode: String(f.mode || ""),
    position: String(f.position || ""),
    scale: Number(f.scale !== undefined ? f.scale : 1),
    transform: Number(f.transform || 0),
    disabled: f.disabled === true,
    mirror: String(f.mirror || "")
  }
}
