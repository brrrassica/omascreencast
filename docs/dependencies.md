# Dependencies and privilege model

Omascreencast runs **entirely in userspace at runtime**. It never runs `sudo`,
never installs a systemd unit, never changes your firewall, and never touches
system packages. `omarchy plugin add` only clones and enables the plugin; it
does not run install hooks. Everything the plugin needs is either already on an
Omarchy/Arch system or provisioned lazily under your home directory.

## Required system tools

| Tool | Arch package | Used for |
|---|---|---|
| `hyprctl` | `hyprland` | Enumerate/create/remove headless outputs, apply monitor rules |
| `ffmpeg` | `ffmpeg` | VAAPI H.264/HEVC encode and fMP4 fragmentation |
| `wf-recorder` **or** `wl-screenrec` | `wf-recorder` / `wl-screenrec` | Capture the virtual output |
| `python3` | `python` | Helper scripts (`scripts/*.py`) |
| `qmllint` (development only) | `qt6-declarative` | `./scripts/lint.sh` |

`wf-recorder` is the default capture backend because it can pin a short,
keyframe-aligned GOP (one part per fragment), which the LL-HLS packager
assumes. `wl-screenrec` is the zero-copy fallback and can be selected
explicitly. `doctor` reports which one is present.

## Python packages

| Package | Arch package | License | Used for |
|---|---|---|---|
| `pychromecast` | `python-pychromecast` | MIT | Cast discovery and playback |
| `zeroconf` | `python-zeroconf` | LGPL-2.1-or-later | Long-lived mDNS discovery |

Resolution order (`scripts/bootstrap.py`):

1. **System packages first.** If both modules import from the system Python,
   the engine uses `python3` directly.
2. **Private venv fallback.** Otherwise the plugin creates
   `~/.local/share/omascreencast/venv` and installs `pychromecast` + `zeroconf`
   into it with its own `pip`. No `--user` flag is needed inside a venv, and no
   root is involved. This is what the panel's **Set up** action calls
   (`bootstrap.py provision`).
3. **Never** invoke `sudo`, `pacman`, or modify system packages from the plugin.

If the machine is offline and the system packages are missing, provisioning
fails with a clear `doctor` result. The optional fast path (one-time, requires
root, entirely your choice) is:

```sh
sudo pacman -S python-pychromecast python-zeroconf wf-recorder ffmpeg
# optional zero-copy capture backend:
# sudo pacman -S wl-screenrec
```

## Firewall

The receiver fetches the HLS stream from your machine over the LAN, so one
inbound TCP port must be reachable. The default port is **7236**. `doctor`
prints the exact rule; nothing is applied automatically:

```sh
sudo ufw allow 7236/tcp
```

If that port is already open, or you prefer another one, keep the firewall
untouched and point the engine at an allowed port (the panel's Doctor section
shows the resolved port and rule). Outbound connections to the TV are ordinary
LAN traffic.

## Runtime state

| Path | Contents |
|---|---|
| `~/.local/state/omascreencast/session.json` | Single source of truth for the active cast session |
| `~/.local/state/omascreencast/cast_engine.log` | Engine JSONL/phase log |
| `~/.local/state/omascreencast/debug/` | Opt-in dumps (`stream --debug`) |
| `~/.local/share/omascreencast/venv/` | Optional self-provisioned Python venv |
| `~/.config/hypr/monitors.lua` | Arrangement persistence (backed up before writes) |

The HLS ring buffer lives **in RAM**; segments are never written to disk.

## Checking your setup

```sh
python3 scripts/bootstrap.py doctor
```

The same information is shown in the panel's Doctor section, including a
one-click **Set up** action for the private venv.
