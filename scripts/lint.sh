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

exec qmllint -I "$ROOT" "$HERE/Panel.qml" "$HERE/Service.qml"
