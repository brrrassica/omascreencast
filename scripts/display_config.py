#!/usr/bin/env python3
"""omascreencast display configuration (M4).

Reads and updates Omarchy Quattro's Hyprland monitor rules in
``~/.config/hypr/monitors.lua``, which use the Lua form:

    hl.monitor({ output = "eDP-1", mode = "1920x1080@60", position = "0x0",
                 scale = 1, transform = 0, disabled = false, mirror = "" })

    hl.env("GDK_SCALE", tostring(omarchy_gdk_scale))

Design rules
------------
- **Literal-only editing.** The file is never evaluated. A small scanner finds
  ``hl.monitor({...})`` calls while skipping strings and comments; only literal
  field *values* are rewritten, and new rules are rendered from validated
  literals. ``hl.env(...)`` and unknown / variable-valued fields are preserved.
- **Runtime changes are ephemeral.** ``apply`` uses ``hyprctl eval`` with a
  rendered ``hl.monitor({...})`` rule (``hyprctl keyword monitor`` is rejected
  on this Hyprland build — Spike I).
- **Persistence is explicit and safe.** ``save`` applies the rule at runtime,
  backs up ``monitors.lua``, writes atomically, and validates the generated
  source before replacing the file. Persistence uses a stable selector
  (``desc:<description>`` when known, otherwise the output name).
- Never change the firewall, never invoke sudo.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import tempfile
from datetime import datetime, timezone
from pathlib import Path

DEFAULT_CONFIG = Path.home() / ".config" / "hypr" / "monitors.lua"

MONITOR_KEYS = ("output", "mode", "position", "scale", "transform", "disabled", "mirror")
_ORDER = {key: index for index, key in enumerate(MONITOR_KEYS)}

_NAME_RE = re.compile(r"^[A-Za-z0-9._:-]+$")
_MODE_RE = re.compile(r"^(\d{2,5}x\d{2,5}@\d{2,3}(?:\.\d+)?|preferred|highres|highrr|auto)$")
_POSITION_RE = re.compile(r"^-?\d+x-?\d+$")
_NUMBER_RE = re.compile(r"^-?\d+(?:\.\d+)?$")
_KEY_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
_LONG_OPEN_RE = re.compile(r"\[(=*)\[")


class ConfigError(ValueError):
    """A rule or the config source could not be parsed/validated safely."""


# ---------------------------------------------------------------------------
# Literal Lua scanning (never evaluates user code)
# ---------------------------------------------------------------------------
def _skip_string(text: str, index: int) -> int:
    """Return the index just past the quoted string starting at ``index``."""
    quote = text[index]
    i = index + 1
    while i < len(text):
        char = text[i]
        if char == "\\":
            i += 2
            continue
        if char == quote:
            return i + 1
        i += 1
    return len(text)


def _long_bracket_end(text: str, index: int) -> int | None:
    """Return the index past a Lua long-bracket literal, or None.

    Handles ``[[...]]`` and the levelled ``[=[...]=]`` forms used by long
    strings and long comments. Without this, text inside documentation could be
    mistaken for a real ``hl.monitor`` call.
    """
    match = _LONG_OPEN_RE.match(text, index)
    if match is None:
        return None
    close = "]" + match.group(1) + "]"
    end = text.find(close, match.end())
    return len(text) if end < 0 else end + len(close)


def _skip_comment(text: str, index: int) -> int:
    """Return the index just past a Lua comment starting at ``index``."""
    if text.startswith("--", index):
        long_end = _long_bracket_end(text, index + 2)
        if long_end is not None:
            return long_end
    end = text.find("\n", index)
    return len(text) if end < 0 else end + 1


def _find_matching(text: str, open_index: int, open_ch: str, close_ch: str) -> int:
    """Find the matching close delimiter, skipping strings and comments."""
    depth = 0
    i = open_index
    while i < len(text):
        if text.startswith("--", i):
            i = _skip_comment(text, i)
            continue
        char = text[i]
        if char in ("'", '"'):
            i = _skip_string(text, i)
            continue
        if char == "[":
            long_end = _long_bracket_end(text, i)
            if long_end is not None:
                i = long_end
                continue
        if char == open_ch:
            depth += 1
        elif char == close_ch:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    raise ConfigError(f"unbalanced {open_ch!r} in monitor config")


def _is_ident_char(char: str) -> bool:
    return char.isalnum() or char == "_"


def find_monitor_calls(text: str) -> list[tuple[int, int]]:
    """Return ``(start, end)`` spans of every ``hl.monitor(...)`` call."""
    calls: list[tuple[int, int]] = []
    i = 0
    marker = "hl.monitor"
    while i < len(text):
        if text.startswith("--", i):
            i = _skip_comment(text, i)
            continue
        if text[i] in ("'", '"'):
            i = _skip_string(text, i)
            continue
        if text[i] == "[":
            long_end = _long_bracket_end(text, i)
            if long_end is not None:
                i = long_end
                continue
        if text.startswith(marker, i):
            before_ok = i == 0 or not _is_ident_char(text[i - 1])
            after = i + len(marker)
            after_ok = after >= len(text) or not _is_ident_char(text[after])
            j = after
            while j < len(text) and text[j].isspace():
                j += 1
            if before_ok and after_ok and j < len(text) and text[j] == "(":
                close = _find_matching(text, j, "(", ")")
                calls.append((i, close + 1))
                i = close + 1
                continue
        i += 1
    return calls


def _split_top_level(text: str, separator: str = ",") -> list[tuple[int, int]]:
    """Split on ``separator`` outside strings/braces; return (start, end) spans."""
    spans: list[tuple[int, int]] = []
    depth = 0
    start = 0
    i = 0
    while i < len(text):
        if text.startswith("--", i):
            i = _skip_comment(text, i)
            continue
        char = text[i]
        if char in ("'", '"'):
            i = _skip_string(text, i)
            continue
        if char == "[":
            long_end = _long_bracket_end(text, i)
            if long_end is not None:
                i = long_end
                continue
        if char in "{([":
            depth += 1
        elif char in "})]":
            depth -= 1
        elif char == separator and depth == 0:
            spans.append((start, i))
            start = i + 1
        i += 1
    spans.append((start, len(text)))
    return [(a, b) for a, b in spans if text[a:b].strip()]


def _parse_lua_string(raw: str) -> tuple[str, int] | None:
    """Parse a quoted Lua string prefix; return (value, consumed) or None."""
    if not raw or raw[0] not in ("'", '"'):
        return None
    quote = raw[0]
    out: list[str] = []
    i = 1
    while i < len(raw):
        char = raw[i]
        if char == "\\" and i + 1 < len(raw):
            nxt = raw[i + 1]
            out.append({"n": "\n", "r": "\r", "t": "\t"}.get(nxt, nxt))
            i += 2
            continue
        if char == quote:
            return "".join(out), i + 1
        out.append(char)
        i += 1
    return None


def _parse_literal(raw: str):
    """Return the literal value of ``raw``, or ``None`` when it is not literal."""
    text = raw.strip()
    if not text:
        return None
    string = _parse_lua_string(text)
    if string is not None:
        value, consumed = string
        return value if text[consumed:].strip() == "" else None
    if text == "true":
        return True
    if text == "false":
        return False
    if _NUMBER_RE.match(text):
        return float(text) if "." in text else int(text)
    return None


def _table_entries(call: str) -> tuple[list[dict], int, int]:
    """Parse ``hl.monitor({ key = value, ... })``.

    Returns ``(entries, brace_open, brace_close)`` where each entry has
    ``key``, ``value`` (literal or None), ``start``/``end`` value spans, and
    ``raw``. Spans are relative to ``call``.
    """
    paren = call.find("(")
    if paren < 0:
        raise ConfigError("monitor call has no argument list")
    brace = call.find("{", paren)
    if brace < 0:
        raise ConfigError("monitor call has no rule table")
    brace_close = _find_matching(call, brace, "{", "}")
    body = call[brace + 1:brace_close]

    entries: list[dict] = []
    for rel_start, rel_end in _split_top_level(body):
        chunk = body[rel_start:rel_end]
        eq = _find_assignment(chunk)
        if eq < 0:
            continue
        key = chunk[:eq].strip()
        if not _KEY_RE.match(key):
            continue
        value_raw = chunk[eq + 1:]
        # Offset of the trimmed value within the original call.
        lead = len(value_raw) - len(value_raw.lstrip())
        value_start = brace + 1 + rel_start + eq + 1 + lead
        value_text = value_raw.strip()
        value_end = value_start + len(value_text)
        entries.append({
            "key": key,
            "raw": value_text,
            "value": _parse_literal(value_text),
            "start": value_start,
            "end": value_end,
        })
    return entries, brace, brace_close


def _find_assignment(chunk: str) -> int:
    depth = 0
    i = 0
    while i < len(chunk):
        char = chunk[i]
        if char in ("'", '"'):
            i = _skip_string(chunk, i)
            continue
        if char == "[":
            long_end = _long_bracket_end(chunk, i)
            if long_end is not None:
                i = long_end
                continue
        if char in "{([":
            depth += 1
        elif char in "})]":
            depth -= 1
        elif char == "=" and depth == 0:
            return i
        i += 1
    return -1


def parse_rules(text: str) -> list[dict]:
    """Parse every ``hl.monitor`` call into a plain dict rule."""
    rules: list[dict] = []
    for start, end in find_monitor_calls(text):
        call = text[start:end]
        try:
            entries, _brace, _close = _table_entries(call)
        except ConfigError:
            # A non-table call (legacy string form, etc.) must not make every
            # other rule unreadable.
            continue
        fields = {e["key"]: e["value"] for e in entries}
        raw = {e["key"]: e["raw"] for e in entries}
        output = fields.get("output")
        rules.append({
            "start": start,
            "end": end,
            "output": output if isinstance(output, str) else "",
            "fields": {k: v for k, v in fields.items() if v is not None},
            "nonLiteral": sorted(k for k, v in fields.items() if v is None),
            "raw": raw,
        })
    return rules


# ---------------------------------------------------------------------------
# Validation + rendering
# ---------------------------------------------------------------------------
def _validate_output(value) -> str:
    text = str(value)
    if not text or len(text) > 128:
        raise ConfigError(f"invalid output selector: {text!r}")
    if any(ch in text for ch in "\"'\\\n\r\t"):
        raise ConfigError(f"invalid output selector: {text!r}")
    # `desc:<free text>` is allowed; a plain output name is stricter.
    if text.startswith("desc:"):
        return text
    if not _NAME_RE.match(text):
        raise ConfigError(f"invalid output name: {text!r}")
    return text


def validate_field(key: str, value):
    """Validate one literal field value; return its normalized Python value."""
    if key not in MONITOR_KEYS:
        raise ConfigError(f"unknown monitor field: {key!r}")
    if key == "output":
        return _validate_output(value)
    if key == "mode":
        mode = str(value)
        if not _MODE_RE.match(mode):
            raise ConfigError(f"invalid mode: {mode!r} (expected WxH@Hz or preferred)")
        return mode
    if key == "position":
        position = str(value)
        if not _POSITION_RE.match(position):
            raise ConfigError(f"invalid position: {position!r} (expected XxY)")
        return position
    if key == "scale":
        try:
            scale = float(value)
        except (TypeError, ValueError) as exc:
            raise ConfigError(f"invalid scale: {value!r}") from exc
        if not 0 < scale <= 4:
            raise ConfigError(f"invalid scale: {value!r} (expected 0 < scale <= 4)")
        return scale
    if key == "transform":
        if isinstance(value, bool):
            raise ConfigError(f"invalid transform: {value!r}")
        try:
            transform = int(value)
        except (TypeError, ValueError) as exc:
            raise ConfigError(f"invalid transform: {value!r}") from exc
        if transform not in range(0, 8):
            raise ConfigError(f"invalid transform: {value!r} (expected 0-7)")
        return transform
    if key == "disabled":
        if not isinstance(value, bool):
            raise ConfigError(f"invalid disabled: {value!r} (expected true/false)")
        return value
    if key == "mirror":
        mirror = str(value)
        if any(ch in mirror for ch in "\"'\\\n\r\t"):
            raise ConfigError(f"invalid mirror target: {mirror!r}")
        return mirror
    raise ConfigError(f"unknown monitor field: {key!r}")


def normalize_rule(rule: dict) -> dict:
    """Validate a rule mapping and return only literal monitor fields."""
    if not isinstance(rule, dict):
        raise ConfigError("rule must be a JSON object")
    fields: dict = {}
    for key, value in rule.items():
        if key in ("name", "description"):
            continue
        if key not in MONITOR_KEYS:
            raise ConfigError(f"unknown monitor field: {key!r}")
        fields[key] = validate_field(key, value)
    if not fields.get("output"):
        name = rule.get("name")
        if name:
            fields["output"] = validate_field("output", name)
    if not fields.get("output"):
        raise ConfigError("rule requires an 'output' or 'name'")
    return fields


def _fmt_number(value) -> str:
    number = float(value)
    return str(int(number)) if number.is_integer() else str(number)


def render_value(key: str, value) -> str:
    if key in ("output", "mode", "position", "mirror"):
        return json.dumps(str(value), ensure_ascii=False)
    if key == "scale":
        return _fmt_number(value)
    if key == "transform":
        return str(int(value))
    if key == "disabled":
        return "true" if value else "false"
    raise ConfigError(f"cannot render field: {key!r}")


def render_rule(fields: dict) -> str:
    """Render a fully literal ``hl.monitor({...})`` call."""
    ordered = sorted(
        ((k, v) for k, v in fields.items() if k in MONITOR_KEYS),
        key=lambda item: _ORDER.get(item[0], 99),
    )
    body = ", ".join(f"{key} = {render_value(key, value)}" for key, value in ordered)
    return f"hl.monitor({{ {body} }})"


def _rewrite_call(call: str, updates: dict) -> str:
    """Rewrite only the literal values of the given fields in one call."""
    entries, brace, brace_close = _table_entries(call)
    by_key = {entry["key"]: entry for entry in entries}
    edits: list[tuple[int, int, str]] = []

    for key, value in updates.items():
        rendered = render_value(key, value)
        if key in by_key:
            entry = by_key[key]
            edits.append((entry["start"], entry["end"], rendered))

    missing = [k for k in updates if k not in by_key]
    if missing:
        if not entries:
            prefix = ""
        elif call[:brace_close].rstrip().endswith(","):
            prefix = " "
        else:
            prefix = ", "
        addition = prefix + ", ".join(
            f"{key} = {render_value(key, updates[key])}"
            for key in sorted(missing, key=lambda k: _ORDER.get(k, 99))
        )
        edits.append((brace_close, brace_close, addition))

    edits.sort(key=lambda edit: (edit[0], edit[1]))
    out: list[str] = []
    cursor = 0
    for start, end, replacement in edits:
        if start < cursor:
            continue
        out.append(call[cursor:start])
        out.append(replacement)
        cursor = end
    out.append(call[cursor:])
    return "".join(out)


def update_rule(text: str, selector: str, fields: dict,
                aliases: tuple[str, ...] = ()) -> tuple[str, bool]:
    """Update the rule for ``selector`` or append one; return (source, changed).

    ``aliases`` lets an existing rule keyed by output *name* be migrated to the
    stable ``desc:`` selector instead of appending a duplicate rule.
    """
    rules = parse_rules(text)
    matches = {selector, *aliases}
    for rule in rules:
        if rule["output"] in matches:
            merged = dict(fields)
            merged["output"] = selector
            call = text[rule["start"]:rule["end"]]
            rewritten = _rewrite_call(call, merged)
            return text[:rule["start"]] + rewritten + text[rule["end"]:], True

    # No matching rule: append a standalone, fully literal rule.
    merged = dict(fields)
    merged["output"] = selector
    addition = render_rule(merged)
    if text and not text.endswith("\n"):
        text += "\n"
    return text + addition + "\n", True


def selector_for_monitor(monitor: dict) -> str:
    """Stable persistence selector: ``desc:...`` when known, else the name."""
    description = str(monitor.get("description") or "").strip()
    if description:
        return _validate_output("desc:" + description)
    return _validate_output(str(monitor.get("name") or ""))


def rule_for_monitor(rules: list[dict], monitor: dict) -> dict | None:
    """Find the rule that governs a monitor (by name or stable selector)."""
    name = str(monitor.get("name") or "")
    selector = ""
    try:
        selector = selector_for_monitor(monitor)
    except ConfigError:
        selector = ""
    for rule in rules:
        if rule["output"] and rule["output"] in (name, selector):
            return rule
    return None


# ---------------------------------------------------------------------------
# File handling
# ---------------------------------------------------------------------------
def read_config(path: Path) -> str:
    return path.read_text(encoding="utf-8") if path.exists() else ""


def backup_config(path: Path) -> str:
    """Copy the current config aside with a UTC timestamp; return the backup path."""
    stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    backup = path.with_suffix(path.suffix + f".omascreencast-{stamp}.bak")
    shutil.copy2(path, backup)
    _prune_backups(path)
    return str(backup)


def _prune_backups(path: Path, keep: int = 3) -> None:
    """Keep only the most recent backups so the config dir does not grow forever."""
    try:
        backups = sorted(path.parent.glob(path.name + ".omascreencast-*.bak"))
    except OSError:
        return
    for old in backups[:-keep] if keep > 0 else backups:
        try:
            old.unlink()
        except OSError:
            pass


def _atomic_write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    try:
        mode = path.stat().st_mode & 0o777
    except OSError:
        mode = None
    handle, tmp_name = tempfile.mkstemp(
        prefix=path.name + ".", suffix=".tmp", dir=str(path.parent)
    )
    tmp = Path(tmp_name)
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        if mode is not None:
            os.chmod(tmp, mode)
        os.replace(tmp, path)
        try:
            dir_fd = os.open(path.parent, os.O_RDONLY)
            try:
                os.fsync(dir_fd)
            finally:
                os.close(dir_fd)
        except OSError:
            pass
    finally:
        if tmp.exists():
            tmp.unlink()


def save_rule(path: Path, monitor: dict, fields: dict) -> dict:
    """Persist a validated rule for ``monitor``; backup + atomic + validated."""
    selector = selector_for_monitor(monitor)
    merged = dict(fields)
    merged["output"] = selector
    source = read_config(path)
    name = str(monitor.get("name") or "")
    aliases = (name,) if name and name != selector else ()
    updated, changed = update_rule(source, selector, merged, aliases=aliases)
    if not changed:
        return {"ok": True, "changed": False, "selector": selector, "backup": ""}

    # Re-parse what we are about to write; a parse failure must never replace
    # the user's file.
    try:
        written = [r for r in parse_rules(updated) if r["output"] == selector]
    except ConfigError:
        written = []
    if not written:
        raise ConfigError("generated monitor config did not re-parse; refusing to write")

    backup = backup_config(path) if path.exists() else ""
    _atomic_write(path, updated)
    return {"ok": True, "changed": True, "selector": selector, "backup": backup}


# ---------------------------------------------------------------------------
# Runtime application (hyprctl eval; Spike I)
# ---------------------------------------------------------------------------
def apply_rule(fields: dict) -> dict:
    """Apply a literal rule at runtime (ephemeral) via ``hyprctl eval``."""
    normalized = normalize_rule(fields)
    rule = render_rule(normalized)
    try:
        result = subprocess.run(
            ["hyprctl", "eval", rule], capture_output=True, text=True, timeout=5,
        )
    except FileNotFoundError as exc:
        raise ConfigError("`hyprctl` not found — is this a Hyprland session?") from exc
    except subprocess.SubprocessError as exc:
        raise ConfigError(f"hyprctl eval failed: {exc}") from exc

    message = (result.stdout or result.stderr or "").strip()
    lowered = message.lower()
    if result.returncode != 0 or "error" in lowered or "invalid" in lowered:
        raise ConfigError(f"could not apply monitor rule: {message or 'unknown error'}")
    return {"ok": True, "rule": rule, "result": message}


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def _emit(payload: dict) -> None:
    print(json.dumps(payload, separators=(",", ":")))


def _load_rule(raw: str) -> dict:
    try:
        data = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise ConfigError(f"--rule is not valid JSON: {exc}") from exc
    if not isinstance(data, dict):
        raise ConfigError("--rule must be a JSON object")
    return data


def _rule_name(rule: dict) -> str:
    name = rule.get("name") or rule.get("output") or ""
    return validate_field("output", name) if name else ""


def cmd_show(args: argparse.Namespace) -> int:
    print(read_config(Path(args.config)), end="")
    return 0


def cmd_rules(args: argparse.Namespace) -> int:
    path = Path(args.config)
    try:
        source = read_config(path)
        rules = parse_rules(source)
    except (OSError, ConfigError) as exc:
        _emit({"ok": False, "error": str(exc), "rules": []})
        return 1
    _emit({
        "ok": True,
        "config": str(path),
        "rules": [
            {
                "output": rule["output"],
                "fields": rule["fields"],
                "nonLiteral": rule["nonLiteral"],
            }
            for rule in rules
        ],
    })
    return 0


def cmd_backup(args: argparse.Namespace) -> int:
    path = Path(args.config)
    if not path.exists():
        _emit({"ok": False, "error": f"not found: {path}"})
        return 1
    _emit({"ok": True, "backup": backup_config(path)})
    return 0


def cmd_apply(args: argparse.Namespace) -> int:
    try:
        rule = _load_rule(args.rule)
        name = _rule_name(rule)
        fields = normalize_rule(rule)
        if not fields.get("output") and name:
            fields["output"] = name
        # Runtime application always uses the output *name* (persistence uses
        # the stable desc: selector). A rule carrying a desc: selector alone
        # cannot be applied at runtime.
        if str(fields.get("output", "")).startswith("desc:"):
            raise ConfigError("apply needs the output name, not a desc: selector")
        result = apply_rule(fields)
        result["output"] = fields["output"]
        _emit(result)
        return 0
    except (ConfigError, OSError) as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1


def cmd_save(args: argparse.Namespace) -> int:
    path = Path(args.config)
    try:
        rule = _load_rule(args.rule)
        name = _rule_name(rule)
        fields = normalize_rule(rule)
        if not fields.get("output") and name:
            fields["output"] = name
        if not name:
            raise ConfigError("save requires a 'name' (or 'output')")

        # Apply at runtime first (by name), then persist under the stable
        # selector (desc:... when a description is supplied).
        runtime = dict(fields)
        runtime["output"] = name
        applied = apply_rule(runtime)
        monitor = {"name": name, "description": str(rule.get("description") or "")}
        persisted = save_rule(path, monitor, fields)
        _emit({
            "ok": True,
            "output": name,
            "rule": applied["rule"],
            **persisted,
        })
        return 0
    except (ConfigError, OSError) as exc:
        _emit({"ok": False, "error": str(exc)})
        return 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", default=str(DEFAULT_CONFIG), help="path to monitors.lua")
    sub = parser.add_subparsers(dest="command", required=True)
    sub.add_parser("show", help="print the current monitor config")
    sub.add_parser("rules", help="print parsed hl.monitor rules as JSON")
    sub.add_parser("backup", help="back up the monitor config")
    apply = sub.add_parser("apply", help="apply an ephemeral arrangement rule")
    apply.add_argument("--rule", required=True,
                       help='JSON rule, e.g. {"name":"HEADLESS-1","position":"1920x0"}')
    save = sub.add_parser("save", help="apply and persist an arrangement rule")
    save.add_argument("--rule", required=True, help="JSON rule with name/description/fields")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handlers = {
        "show": cmd_show,
        "rules": cmd_rules,
        "backup": cmd_backup,
        "apply": cmd_apply,
        "save": cmd_save,
    }
    return handlers[args.command](args)


if __name__ == "__main__":
    raise SystemExit(main())
