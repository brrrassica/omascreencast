# Omascreencast

An [Omarchy](https://omarchy.org) Quattro plugin that turns a Chromecast-capable TV into an
**arrangeable virtual second monitor**, plus a **monitor arranger** for every connected display.

Unlike screen *mirroring*, omascreencast creates a Hyprland **headless virtual output**, extends your
desktop onto it, and streams *that output* to the TV. The TV becomes a real second display you can place,
resize, scale, rotate, mirror, or disable — alongside your physical monitors.

> **Status:** pre-alpha skeleton (v0.1.0 in progress). Discovery, casting, and the arranger are not wired
> up yet. See `plans/milestones.md` for the roadmap.

## What it does

- **Virtual network monitor** — a `HEADLESS-*` output enumerated by Hyprland and streamed to your TV.
- **Monitor arranger** — position, resolution, scale, orientation, enable/disable, and mirror for all
  displays, virtual ones included.
- **Rootless** — everything runs as your user. No `sudo`, no systemd unit. Casting needs one inbound
  firewall rule for the local stream port (or an already-open port); see Honest limitations.
- **Standard install** — a plain Omarchy plugin: clone, validate, enable.

## Honest limitations

- **One-way and laggy.** Chromecast is a sink; there is no input path. Measured on a Panasonic
  receiver: **~2 s** glass-to-glass with an LL-HLS-tagged fMP4 playlist (plain HLS was ~4 s; the
  receiver's buffer dominates). Great for dashboards, terminals, video, and presentations — not for
  gaming.
- **No EDID / hotplug.** The TV cannot report modes or wake the compositor; you pick mode and scale.
- **Cursor appears on the TV** when the virtual output is placed adjacent to a physical one
  (measured); otherwise the pointer is clamped at the edge and cannot reach it.
- **One inbound firewall rule is required** for the local stream port (or reuse an already-open
  port), so the receiver can fetch the stream from your machine.
- **Video only in v0.1.0.** Audio arrives in v0.1.x.
- **Chromecast only in v0.1.0.** Miracast/WFD and DLNA are out of scope.

## Requirements

- Omarchy Quattro (Hyprland + Quickshell) on Arch. Verified on Omarchy 4.0.6 / Hyprland 0.56.2 /
  Quickshell 0.3.2.
- `hyprctl`, `wf-recorder`, `ffmpeg` (all in the official Arch repos).
- Python 3 with `pychromecast` and `zeroconf`.

Dependencies are resolved **without root**: the plugin prefers system-installed
`python-pychromecast` / `python-zeroconf`, and otherwise provisions a private venv under
`~/.local/share/omascreencast/venv` on first use.

Optional, faster path (one-time, requires root):

```sh
sudo pacman -S python-pychromecast python-zeroconf wf-recorder ffmpeg
```

## Install

Via the standard Omarchy plugin flow:

```sh
omarchy plugin add https://github.com/<you>/omascreencast --enable
```

For local development against this checkout:

```sh
omarchy plugin add file://$HOME/projects/omascreencast --enable --yes
```

## Develop

```sh
# validate the plugin folder + QML
omarchy plugin validate .
./scripts/lint.sh   # qmllint with the Quickshell `qs.*` module resolvable

# hot-reload after edits
omarchy-shell shell rescanPlugins

# python unit tests
python3 -m unittest discover -s tests -v
```

Logs and opt-in debug dumps are written to `~/.config/omascreencast/`
(`cast_engine.log`, plus `debug/` when `stream --debug` is used). The HLS
segments themselves are buffered **in RAM** and never written to disk.

## Repository layout

```
manifest.json          # Omarchy plugin manifest (id: tech.alatcerdas.omascreencast)
Panel.qml              # bar-widget entry point + popup host (arranger + cast UI)
Service.qml            # headless service singleton: session state, discovery, recovery
Model.js               # pure JSON -> view-model helpers
scripts/
  bootstrap.py         # rootless dependency resolution (system pkgs -> private venv)
  cast_engine.py       # discovery + capture -> encode -> serve -> Chromecast (in-memory LL-HLS)
  display_config.py    # literal-only editor for ~/.config/hypr/monitors.lua
  session.py           # session-state tracking + orphan reconciliation
tests/                 # unittest suite + captured fixtures
plans/                 # local-only roadmap (gitignored)
```

## Uninstall

```sh
omarchy plugin remove tech.alatcerdas.omascreencast
```

Python packages and the private venv (if created) are left in place; remove `~/.local/share/omascreencast`
manually if desired.

## License

MIT — see [LICENSE](LICENSE).
