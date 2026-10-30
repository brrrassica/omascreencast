# Omascreencast

An [Omarchy](https://omarchy.org) Quattro plugin that turns a Chromecast-capable TV into an
**arrangeable virtual second monitor**, plus a **monitor arranger** for every connected display.

Unlike screen *mirroring*, omascreencast creates a Hyprland **headless virtual output**, extends your
desktop onto it, and streams *that output* to the TV. The TV becomes a real second display you can place,
resize, scale, rotate, mirror, or disable — alongside your physical monitors.

> **Status:** pre-alpha (v0.1.0 in progress). Discovery, the virtual-monitor session (create /
> remove / reconcile), the capture → encode → LL-HLS → Chromecast pipeline, and the monitor
> arranger (drag, snap, mode/scale/rotation/disable/mirror, persist) are wired, including a
> rootless “Set up” dependency action, crash-out reconciliation, guarded layouts, trial/keep/
> revert arrangement changes, receiver-drop handling, and idempotent teardown. Remaining work is
> hardware verification and packaging. See `plans/milestones.md`.

## What it does

- **Virtual network monitor** — a `HEADLESS-*` output enumerated by Hyprland and streamed to your TV.
- **Monitor arranger** — position, resolution, scale, orientation, enable/disable, and mirror for all
  displays, virtual ones included. Live changes are **trials**: press **Keep** before the timeout or
  they revert automatically, and a layout that would hide every display is refused.
- **Rootless** — everything runs as your user. No `sudo`, no systemd unit. Casting needs one inbound
  firewall rule for the local stream port (or an already-open port); see Honest limitations.
- **Recoverable** — state is reconciled against Hyprland on every shell start; a crash or a bad layout
  cannot lock you out, and uninstall never leaves an orphaned headless output.
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
- `hyprctl`, `wf-recorder` (or `wl-screenrec`), `ffmpeg` (all in the official Arch repos).
- Python 3 with `pychromecast` and `zeroconf`.

Dependencies are resolved **without root**: the plugin prefers system-installed
`python-pychromecast` / `python-zeroconf`, and otherwise provisions a private venv under
`~/.local/share/omascreencast/venv` on first use (the panel’s Doctor section exposes a
**Set up** action for this).

Optional, faster path (one-time, requires root):

```sh
sudo pacman -S python-pychromecast python-zeroconf wf-recorder ffmpeg
# optional zero-copy capture backend (preferred when installed):
# sudo pacman -S wl-screenrec
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

Logs and opt-in debug dumps are written to `~/.local/state/omascreencast/`
(`cast_engine.log`, plus `debug/` when `stream --debug` is used). The HLS
segments themselves are buffered **in RAM** and never written to disk. For
LL-HLS the capture backend prefers `wf-recorder` because it can pin a short GOP
(one keyframe-aligned part per fragment); `wl-screenrec` remains the fallback
and can be selected explicitly. The VAAPI node is discovered from
`/dev/dri/renderD*`.

The active virtual-monitor session is tracked in the single source of truth
`~/.local/state/omascreencast/session.json` (atomic writes). The service
reconciles it against `hyprctl monitors` on startup: stale sessions, outputs we
recorded as ours, and outputs still targeted by one of our own `cast_engine.py`
processes are cleaned automatically. A genuinely foreign `HEADLESS-*` output is
only reported in the panel, so a display you created by hand is never removed
behind your back.

### Recovery

- **Bad layout / monitors.lua edit.** Every save backs up `~/.config/hypr/monitors.lua`
  (`*.omascreencast-<timestamp>.bak`) and writes atomically. If a generated file fails to
  re-parse it is never written. Restore the newest backup with:

  ```sh
  python3 scripts/display_config.py restore
  ```

- **Orphaned output or recorder after a crash.** Reconcile (also run automatically on shell start):

  ```sh
  python3 scripts/session.py reconcile
  ```

- **Full teardown** (idempotent; stops every owned stream and removes every owned output):

  ```sh
  python3 scripts/session.py teardown
  # or through the IPC surface:
  # omarchy-shell ipc call omascreencast teardown
  ```

  A `HEADLESS-*` output the plugin did not create is never touched; remove it manually
  with `hyprctl output remove HEADLESS-N` if you no longer need it.

### TV disappears

The `dropPolicy` setting chooses what happens when the receiver drops off the network while
casting: **keep** the virtual monitor (default; the panel shows a Reconnect action) or
**remove** it. With auto-reconnect enabled, the old output is torn down and a new session is
retried automatically.

## Repository layout

```
manifest.json          # Omarchy plugin manifest (id: tech.alatcerdas.omascreencast)
Panel.qml              # bar-widget entry point + popup host (arranger + cast UI)
Service.qml            # headless service singleton: session state, discovery, recovery
Model.js               # pure JSON -> view-model helpers
scripts/
  bootstrap.py         # rootless dependency resolution (system pkgs -> private venv)
  cast_engine.py       # discovery + capture -> encode -> serve -> Chromecast (in-memory LL-HLS)
  display_config.py    # literal-only reader/writer for ~/.config/hypr/monitors.lua (arranger)
  session.py           # session-state tracking + orphan reconciliation
tests/                 # unittest suite + captured fixtures
plans/                 # local-only roadmap (gitignored)
```

## Uninstall

Omarchy has no plugin-disable hook, so tear down any live session **before** removing the plugin:

```sh
python3 ~/.config/omarchy/plugins/tech.alatcerdas.omascreencast/scripts/session.py teardown
omarchy plugin remove tech.alatcerdas.omascreencast
```

`teardown` is idempotent and safe to run with no active session. If the plugin folder is already
gone, clean up manually:

```sh
pkill -f 'cast_engine.py stream' || true
hyprctl monitors all -j | jq -r '.[].name' | grep '^HEADLESS-' | xargs -r -n1 hyprctl output remove
```

Python packages and the private venv (if created) are left in place; remove `~/.local/share/omascreencast`
manually if desired.

## License

MIT — see [LICENSE](LICENSE).
