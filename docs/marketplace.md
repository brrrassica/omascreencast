# Marketplace listing metadata

Copy for the Omarchy plugin marketplace and the project website. Keep the
manifest and this file in sync when the listing changes.

| Field | Value |
|---|---|
| **Name** | Omascreencast |
| **Plugin id** | `tech.alatcerdas.omascreencast` |
| **Version** | 0.1.1 |
| **Tagline** | Use a Chromecast TV as an arrangeable virtual second monitor. |
| **Category** | System |
| **Aliases** | `omascreencast`, `cast-monitor`, `virtual-monitor`, `network-monitor`, `display-arranger` |
| **Tags** | `chromecast`, `cast`, `monitor`, `display`, `hyprland`, `headless`, `virtual-output`, `screen-casting`, `arranger`, `hls`, `wayland` |
| **Author** | Sam Ralial |
| **License** | MIT |
| **Kinds** | `bar-widget`, `service` |
| **Default bar section** | right |
| **Permissions** | `system.display` |
| **Requirements** | Omarchy Quattro (Hyprland + Quickshell), `ffmpeg`, `wf-recorder` or `wl-screenrec`, Python 3 with `pychromecast` + `zeroconf` |
| **Screenshot** | [`preview.png`](../preview.png) (1200×675) |
| **Homepage** | <https://omascreencast.alatcerdas.tech> |
| **Mirror** | <https://brrrassica.github.io/omascreencast/> |
| **Source** | <https://github.com/brrrassica/omascreencast> |
| **Issues** | <https://github.com/brrrassica/omascreencast/issues> |
| **Install** | `omarchy plugin add https://github.com/brrrassica/omascreencast --enable` |

## Short description (≤ 160 chars)

> Cast a Hyprland headless output to a Chromecast TV as a real second monitor — plus an arranger for every display. Userspace only.

## Long description

Omascreencast extends your desktop onto a **real Hyprland headless output** and
streams that output to a Chromecast TV over the LAN. The TV becomes a second
display you can position, resize, scale, rotate, mirror, or disable alongside
your physical monitors — not a mirror of monitor #1.

It includes a monitor arranger for all displays, a bar widget with clear
casting state, trial/keep/revert layout changes, crash reconciliation, and a
fully rootless dependency path. Latency is measured at roughly two seconds
glass-to-glass; it is a reference/overflow display, not a gaming display.

## Security baseline checklist

- [x] No `sudo`, setuid helpers, or systemd units at runtime.
- [x] No install hooks (works with a clone-only `omarchy plugin add`).
- [x] No automatic firewall changes; the optional rule is documented.
- [x] No secrets, telemetry, or outbound connections beyond the local LAN.
- [x] User configuration is parsed literally and never evaluated.
- [x] Manifest validates and all entry points exist.
- [x] Symlink-free plugin folder.
- [x] MIT license + third-party notices.
- [x] Security policy with a private reporting path.

## Suggested marketplace copy

**What it does**
- Turns a Chromecast TV into a genuine second display via a `HEADLESS-*` output.
- Arranges every display: drag, snap, mode, scale, rotation, disable, mirror.
- Keeps state honest: trials with auto-revert, crash reconciliation, idempotent teardown.

**Honest limitations**
- One-way and ~2 s latency; no input path, no audio in v0.1.0.
- No EDID or hotplug: pick mode and scale yourself.
- One inbound stream port must be reachable from the TV.
