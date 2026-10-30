# Changelog

All notable changes to omascreencast are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-10-30

First public release. A Chromecast-capable TV becomes an arrangeable Hyprland
virtual output, streamed over the LAN, plus a monitor arranger for every
connected display. Userspace only.

### Added

- **Virtual network monitor.** `session.py create` asks Hyprland for a
  `HEADLESS-*` output, places it to the right of the primary display, and casts
  it. `remove` is idempotent; reconciliation on shell start cleans stale
  sessions without ever deleting a `HEADLESS-*` output the plugin did not
  create.
- **Capture → encode → serve → cast pipeline.** `wf-recorder`/`wl-screenrec`
  capture, VAAPI H.264/HEVC encoding (H.264 when the receiver is unknown, VP9
  software fallback, AV1 experimental/off), an **in-memory** LL-HLS fMP4
  packager with a bounded ring buffer, and a LAN-bound HTTP server with the
  CORS headers receivers require.
- **Low-latency tuning.** Disabled B-frames, a short keyframe-aligned GOP,
  0.5 s parts, LL-HLS tags, blocking playlist reload, and fast first-frame
  startup. Measured floor: roughly **2 s** glass-to-glass.
- **Monitor arranger for all displays.** Drag-to-position with edge snapping,
  mode/scale/orientation/disable/mirror controls, live `hyprctl eval` apply,
  and safe persistence with a backup + literal-only parser + atomic write.
- **Trial / Keep / Revert.** Arrangement changes auto-revert on timeout; a
  layout that would leave no visible output is refused.
- **Usability.** Bar widget states (idle / discovering / connecting / casting /
  error), panel with displays and devices, manual IP entry, desktop
  notifications, an IPC surface (`start`, `stop`, `status`, `refresh`,
  `teardown`), settings, and hotplug handling while casting.
- **Rootless dependency bootstrap.** System packages first; otherwise a private
  venv under `~/.local/share/omascreencast/venv`, with a Doctor UI and a
  one-click **Set up** action. No `sudo`, no install hooks, no automated
  firewall changes.
- **Release engineering.** `scripts/smoke.sh` (manifest + unit tests + headless
  pipeline smoke + `omarchy plugin validate` + `qmllint`), a headless
  synthetic-source pipeline test, GitHub Actions CI, marketplace listing copy,
  a static website, and third-party notices.

### Security

- Literal-only parsing of `~/.config/hypr/monitors.lua`; user Lua is never
  evaluated. Every write is backed up and validated before an atomic replace.
- The stream server serves only in-memory segments and is stopped with the
  session. It is unauthenticated by design (Chromecast requires no auth); see
  [SECURITY.md](SECURITY.md).
- No telemetry, credentials, or non-LAN connections.

### Known gaps

Tracked openly for v0.1.x and later:

- **Hardware verification on the author's receivers** (Sony Bravia X75J,
  Panasonic) is still being completed for codec negotiation (HEVC vs H.264) and
  end-to-end playback. The safe H.264 default covers unknown receivers.
- **Capture-backend benchmark** (`wf-recorder` vs zero-copy `wl-screenrec`) is
  not finalised; `wf-recorder` is the default because it pins the GOP the
  packager assumes, and `wl-screenrec` remains the fallback.
- **Second-GPU render-node validation** is untested; the engine discovers
  `/dev/dri/renderD*` rather than hardcoding a node.
- **Audio is not included.** It is the first item of v0.1.x (M8).
- **Chromecast only.** Miracast/WFD and DLNA are out of scope for v0.1.0.

### Install

```sh
omarchy plugin add https://github.com/brrrassica/omascreencast --enable
```

See the [README](README.md) for prerequisites, the quick start, and
troubleshooting.
