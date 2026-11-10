# Third-party notices

Omascreencast itself is licensed under the MIT License (see [LICENSE](LICENSE)).
It interoperates with, and at runtime invokes, the following independent
projects. They are **not** bundled or redistributed with this repository; they
are installed by the user's distribution or provisioned on demand. Their
licenses apply to those components, not to this project.

## Clean-room provenance (D1)

The GameStream host added for 0.2.0 is a **clean-room implementation** written
from published protocol behaviour and from wire traces we capture ourselves.
It is **not** a port, translation, or derivative of Sunshine or any other GPL
GameStream implementation.

Concretely, for everything under `omascreencastd/` and `tools/m0-gamestream/`:

- Reference implementations such as [Sunshine](https://github.com/LizardByte/Sunshine)
  (GPL-3.0) are treated as **black boxes** — observed over the network, never
  read for transcription. Any `upstream-sunshine/` checkout used to build the
  M0 oracle stays gitignored and is never copied into the workspace.
- No Sunshine identifiers, comments, comment layout, struct definitions, or
  constants are imported. Protocol constants come from the wire (on-the-wire
  field values, captured traces) and from public documentation, and are
  recorded in the trace corpus with their provenance.
- If a cipher, packing layout, or state machine cannot be written without
  copying GPL text, the change stops and D1 in
  [`plans/towards-0.2.0.md`](plans/towards-0.2.0.md) is re-opened rather than
  shipping a quiet transcription.

This position is a release gate: a pull request that imports Sunshine source,
comments, or comment structure is rejected. See [`CONTRIBUTING.md`](CONTRIBUTING.md).

## Protocol compatibility references

| Component | Role | License | Project |
|---|---|---|---|
| Moonlight for Android TV | The client this host must satisfy (not bundled) | GPL-3.0-or-later | <https://github.com/moonlight-stream/moonlight-android> |

Moonlight and Sunshine are named only to describe interoperability. Omascreencast
is an independent project and is not affiliated with or endorsed by either.

| Component | Role | License | Project |
|---|---|---|---|
| `hyprctl` / Hyprland | Wayland compositor and output management | BSD-3-Clause | <https://hypr.land> |
| Quickshell | Shell runtime that hosts this plugin (`qs.*` modules) | LGPL-3.0-only | <https://quickshell.org> |
| FFmpeg | VAAPI encoding and fragmented MP4 packaging | GPL-3.0-only (Arch build; FFmpeg itself is LGPL/GPL depending on build flags) | <https://ffmpeg.org> |
| `wf-recorder` | Default capture backend | MIT | <https://github.com/ammen99/wf-recorder> |
| `wl-screenrec` | Optional zero-copy capture backend | MIT | <https://github.com/russelltg/wl-screenrec> |
| `pychromecast` | Chromecast discovery and playback | MIT | <https://github.com/home-assistant-libs/pychromecast> |
| `zeroconf` | mDNS/Zeroconf discovery | LGPL-2.1-or-later | <https://github.com/python-zeroconf/python-zeroconf> |
| Python | Helper script runtime | PSF-2.0 | <https://python.org> |
| Qt 6 / `qmllint` | QML tooling (development only) | LGPL-3.0-only / GPL-3.0-only | <https://qt.io> |

## Names and trademarks

Chromecast and Google TV are trademarks of Google LLC. Sony, Bravia, and
Panasonic are trademarks of their respective owners. Omascreencast is an
independent project and is not affiliated with or endorsed by any of them.

## Asset notice

`preview.png` is an illustrative mockup created for this project. It contains
no third-party screenshots, fonts, or icons. UI icons rendered at runtime come
from the user's installed Omarchy/Quickshell theme.
