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
function parseDevices(raw) {
  var list = []
  try {
    var devices = JSON.parse(raw)
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
