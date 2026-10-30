#!/usr/bin/env bash
# omascreencast release smoke test.
#
# Runs every check that does not need a Chromecast or a graphical session:
#   1. manifest sanity (JSON + required fields)
#   2. Python unit tests
#   3. headless capture/encode/packager/HTTP smoke (real ffmpeg, synthetic source)
#   4. `omarchy plugin validate`           (skipped when omarchy is absent)
#   5. `qmllint`                            (skipped when qmllint is absent)
#
# Usage: ./scripts/smoke.sh [--strict]
#   --strict  treat skipped checks (missing omarchy/qmllint) as failures
set -euo pipefail

HERE="$(cd "$(dirname "$0")/.." && pwd)"
STRICT=0
[[ ${1:-} == --strict ]] && STRICT=1
FAIL=0

step() { printf '\n== %s ==\n' "$*"; }
skip() {
  if ((STRICT)); then
    printf 'smoke: FAIL: %s (required by --strict)\n' "$*" >&2
    FAIL=1
  else
    printf 'smoke: SKIP: %s\n' "$*"
  fi
}

cd "$HERE"

step "manifest"
python3 - <<'PY'
import json, pathlib, sys
path = pathlib.Path("manifest.json")
data = json.loads(path.read_text())
missing = [k for k in ("schemaVersion", "id", "name", "version", "kinds", "entryPoints") if k not in data]
if missing:
    sys.exit(f"manifest.json missing required fields: {', '.join(missing)}")
if data["schemaVersion"] != 1:
    sys.exit(f"unsupported schemaVersion: {data['schemaVersion']!r}")
if data["id"].startswith("omarchy."):
    sys.exit("plugin id uses the reserved omarchy.* namespace")
for name, rel in data["entryPoints"].items():
    if not (pathlib.Path(rel)).is_file():
        sys.exit(f"entry point {name!r} does not exist: {rel}")
print(f"manifest OK: {data['id']} v{data['version']}")
PY

step "python unit tests"
python3 -m unittest discover -s tests -v

step "headless pipeline smoke"
python3 tests/headless_smoke.py

step "omarchy plugin validate"
if command -v omarchy >/dev/null 2>&1; then
  omarchy plugin validate "$HERE"
else
  skip "omarchy not installed"
fi

step "qmllint"
QMLLINT="${QMLLINT:-}"
if [[ -z $QMLLINT ]]; then
  for candidate in "$(command -v qmllint 2>/dev/null)" /usr/lib/qt6/bin/qmllint /usr/lib/qt/bin/qmllint; do
    if [[ -n $candidate && -x $candidate ]]; then QMLLINT="$candidate"; break; fi
  done
fi
if [[ -n $QMLLINT && -d ${OMARCHY_PATH:-/usr/share/omarchy}/shell ]]; then
  ./scripts/lint.sh
else
  skip "qmllint or the Omarchy shell sources not available"
fi

echo
if ((FAIL)); then
  echo "smoke: FAILED"
  exit 1
fi
echo "smoke: PASSED"
