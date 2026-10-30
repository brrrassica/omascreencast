#!/usr/bin/env bash
# Lint omascreencast QML with the Quickshell `qs.*` pseudo-module resolvable.
#
# Quickshell reserves the `qs` import prefix for the shell config directory at
# runtime, but qmllint needs a real path. Emulate it with a temporary `qs`
# symlink pointing at $OMARCHY_PATH/shell.
set -euo pipefail

: "${OMARCHY_PATH:=/usr/share/omarchy}"
HERE="$(cd "$(dirname "$0")/.." && pwd)"

ROOT="$(mktemp -d)"
trap 'rm -rf "$ROOT"' EXIT
ln -s "$OMARCHY_PATH/shell" "$ROOT/qs"

# qmllint ships with Qt 6 but is not always on PATH (it lives in either the
# distro Qt bin dir or the Arch Qt6 package's private bin dir).
QMLLINT="${QMLLINT:-}"
if [[ -z $QMLLINT ]]; then
  for candidate in "$(command -v qmllint 2>/dev/null)" \
                   /usr/lib/qt6/bin/qmllint \
                   /usr/lib/qt/bin/qmllint; do
    if [[ -n $candidate && -x $candidate ]]; then
      QMLLINT="$candidate"
      break
    fi
  done
fi
[[ -n $QMLLINT ]] || { echo "lint.sh: qmllint not found (install qt6-declarative)" >&2; exit 1; }

exec "$QMLLINT" -I "$ROOT" "$HERE/Panel.qml" "$HERE/Service.qml"
