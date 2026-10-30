# Security policy

## Reporting a vulnerability

Please report security issues **privately** rather than opening a public issue:
use GitHub's *Report a vulnerability* (Security → Advisories) on the repository.
Include the version, your distribution, and a minimal reproduction. You should
receive an acknowledgement within a few days.

## Privilege model

Omascreencast is designed to run **entirely in userspace**:

- No `sudo`, no `pkexec`, no setuid helpers, no systemd units.
- No `pacman` calls from the plugin; dependencies are either already installed
  or provisioned into a private venv under `~/.local/share/omascreencast/venv`.
- No automatic firewall changes. `doctor` prints the exact optional `ufw` rule;
  applying it is your decision.
- `omarchy plugin add` only clones and enables the plugin; there are no install
  hooks.

## Data handling and attack surface

- **Local stream server.** The HLS server listens on the LAN IPv4 address on
  TCP **7236** (configurable) and serves only the init segment, playlist, and
  in-memory media segments. It sends `Access-Control-Allow-Origin: *` because
  Chromecast receivers require it. The stream is **not authenticated** — anyone
  who can reach the port on your LAN can view the cast. Use it only on trusted
  networks; the server is stopped when the session ends.
- **In-memory buffering.** Media segments are never written to disk. Only logs
  and opt-in debug dumps go to `~/.local/state/omascreencast/`.
- **No user-code evaluation.** `monitors.lua` is parsed with a literal-only
  scanner. It is never executed or `eval`'d, so a hostile or malformed config
  cannot run code through the arranger. Every write is backed up first and
  validated before an atomic replace.
- **No secrets.** The plugin stores no credentials or tokens. Chromecast
  discovery and playback use the normal unauthenticated Cast protocol on the
  LAN.

## Supported versions

Security fixes target the latest `v0.1.x` release. Pre-release builds from
`main` are supported on a best-effort basis.

## Scope

Reports about the underlying projects (Hyprland, Quickshell, FFmpeg,
`wf-recorder`, `pychromecast`, `zeroconf`, Qt) should go to those projects.
Issues in how omascreencast invokes or exposes them are in scope.
