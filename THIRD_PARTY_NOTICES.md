# Third-party notices

Omascreencast itself is licensed under the MIT License (see [LICENSE](LICENSE)).
It interoperates with, and at runtime invokes, the following independent
projects. They are **not** bundled or redistributed with this repository; they
are installed by the user's distribution or provisioned on demand. Their
licenses apply to those components, not to this project.

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
