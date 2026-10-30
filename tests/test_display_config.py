"""Unit tests for the M4 literal-only monitor-config editor.

No Hyprland or real monitors.lua is touched: every test uses a temporary file.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"

SOURCE = '''\
-- See https://hypr.land
local omarchy_monitor_scale = 1

hl.env("GDK_SCALE", tostring(1))

-- primary
hl.monitor({ output = "HDMI-A-1", mode = "preferred", position = "0x0",
             scale = omarchy_monitor_scale })

-- internal
hl.monitor({ output = "desc:Lenovo Group Limited 0x4169",
             mode = "1920x1080@60.00000", position = "13x1080", scale = 1,
             transform = 0, disabled = false, mirror = "" })
'''


def _load():
    spec = importlib.util.spec_from_file_location("display_config", SCRIPTS / "display_config.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules["display_config"] = module
    spec.loader.exec_module(module)
    return module


class ParseRulesTest(unittest.TestCase):
    def setUp(self):
        self.dc = _load()

    def test_parses_multiline_rules(self):
        rules = self.dc.parse_rules(SOURCE)
        self.assertEqual(len(rules), 2)
        self.assertEqual(rules[0]["output"], "HDMI-A-1")
        self.assertEqual(rules[0]["fields"]["mode"], "preferred")
        self.assertIn("scale", rules[0]["nonLiteral"])
        self.assertEqual(rules[1]["output"], "desc:Lenovo Group Limited 0x4169")
        self.assertEqual(rules[1]["fields"]["position"], "13x1080")
        self.assertFalse(rules[1]["fields"]["disabled"])

    def test_comments_and_strings_do_not_confuse_the_scanner(self):
        source = '''
-- hl.monitor({ output = "FAKE", mode = "preferred" })
local note = "hl.monitor({ output = 'NOPE' })"
hl.monitor({ output = "eDP-1", mode = "1920x1080@60", position = "-100x0" })
'''
        rules = self.dc.parse_rules(source)
        self.assertEqual([r["output"] for r in rules], ["eDP-1"])

    def test_unbalanced_source_is_rejected(self):
        with self.assertRaises(self.dc.ConfigError):
            self.dc.parse_rules('hl.monitor({ output = "eDP-1"')

    def test_long_bracket_strings_and_comments_are_not_rules(self):
        source = '''\
local doc = [[
hl.monitor({ output = "DOC-1", mode = "preferred" })
]]
--[==[ hl.monitor({ output = "DOC-2" }) ]==]
hl.monitor({ output = "eDP-1", position = "0x0" })
'''
        self.assertEqual([r["output"] for r in self.dc.parse_rules(source)], ["eDP-1"])

    def test_non_table_call_is_skipped_without_hiding_other_rules(self):
        source = '''\
hl.monitor("eDP-1,preferred,0x0,1")
hl.monitor({ output = "DP-1", position = "1920x0" })
'''
        self.assertEqual([r["output"] for r in self.dc.parse_rules(source)], ["DP-1"])


class RewriteTest(unittest.TestCase):
    def setUp(self):
        self.dc = _load()

    def test_existing_rule_values_are_updated_in_place(self):
        selector = "desc:Lenovo Group Limited 0x4169"
        updated, changed = self.dc.update_rule(SOURCE, selector, {
            "output": selector,
            "mode": "1920x1080@60",
            "position": "1920x0",
            "scale": 1.25,
            "transform": 1,
            "disabled": True,
            "mirror": "",
        })
        self.assertTrue(changed)
        self.assertIn('position = "1920x0"', updated)
        self.assertIn("scale = 1.25", updated)
        self.assertIn("transform = 1", updated)
        self.assertIn("disabled = true", updated)
        # The unrelated HDMI rule (including its variable-valued scale) and the
        # comments must survive untouched.
        self.assertIn('hl.env("GDK_SCALE", tostring(1))', updated)
        self.assertIn("scale = omarchy_monitor_scale", updated)
        self.assertIn("-- internal", updated)

    def test_missing_field_is_appended(self):
        selector = "HDMI-A-1"
        updated, _ = self.dc.update_rule(SOURCE, selector, {
            "output": selector, "transform": 2, "mirror": "eDP-1",
        })
        self.assertIn("transform = 2", updated)
        self.assertIn('mirror = "eDP-1"', updated)
        # Still a single rule for that output.
        rules = [r for r in self.dc.parse_rules(updated) if r["output"] == selector]
        self.assertEqual(len(rules), 1)

    def test_unknown_output_appends_a_new_rule(self):
        selector = "HEADLESS-1"
        updated, changed = self.dc.update_rule(SOURCE, selector, {
            "output": selector, "mode": "1920x1080@60", "position": "1920x0", "scale": 1,
        })
        self.assertTrue(changed)
        rules = self.dc.parse_rules(updated)
        self.assertEqual(len(rules), 3)
        self.assertEqual(rules[-1]["output"], "HEADLESS-1")

    def test_render_rule_is_literal(self):
        rule = self.dc.render_rule({
            "output": "HEADLESS-1", "mode": "1920x1080@60",
            "position": "1920x0", "scale": 1, "disabled": False,
        })
        self.assertEqual(
            rule,
            'hl.monitor({ output = "HEADLESS-1", mode = "1920x1080@60", '
            'position = "1920x0", scale = 1, disabled = false })',
        )


class ValidationTest(unittest.TestCase):
    def setUp(self):
        self.dc = _load()

    def test_rejects_injection(self):
        for rule in (
            {"output": 'X", scale=1 }); os.exit()', "parameter": "oops"},
            {"output": "eDP-1", "mode": "not-a-mode"},
            {"output": "eDP-1", "position": "0,0"},
            {"output": "eDP-1", "scale": 0},
            {"output": "eDP-1", "transform": 9},
            {"output": "eDP-1", "disabled": "yes"},
            {"output": "eDP-1", "bogus": 1},
        ):
            with self.assertRaises(self.dc.ConfigError):
                self.dc.normalize_rule(rule)

    def test_desc_selector_is_allowed(self):
        self.assertEqual(
            self.dc.selector_for_monitor({
                "name": "eDP-1", "description": "Lenovo Group Limited 0x4169",
            }),
            "desc:Lenovo Group Limited 0x4169",
        )

    def test_name_selector_when_no_description(self):
        self.assertEqual(self.dc.selector_for_monitor({"name": "HEADLESS-1"}), "HEADLESS-1")


class SaveRuleTest(unittest.TestCase):
    def setUp(self):
        self.dc = _load()
        self._tmp = tempfile.TemporaryDirectory()
        self.path = Path(self._tmp.name) / "monitors.lua"
        self.addCleanup(self._tmp.cleanup)

    def test_new_file_is_created_with_a_rule(self):
        result = self.dc.save_rule(
            self.path, {"name": "HEADLESS-1"},
            {"output": "HEADLESS-1", "mode": "1920x1080@60", "position": "1920x0"},
        )
        self.assertTrue(result["ok"])
        self.assertEqual(result["backup"], "")
        self.assertIn('output = "HEADLESS-1"', self.path.read_text())

    def test_existing_file_is_backed_up_before_write(self):
        self.path.write_text(SOURCE, encoding="utf-8")
        result = self.dc.save_rule(
            self.path, {"name": "HEADLESS-1"},
            {"output": "HEADLESS-1", "mode": "1920x1080@60", "position": "1920x0"},
        )
        self.assertTrue(Path(result["backup"]).exists())
        self.assertIn(SOURCE, Path(result["backup"]).read_text())
        self.assertIn('output = "HEADLESS-1"', self.path.read_text())

    def test_save_uses_desc_selector(self):
        self.path.write_text(SOURCE, encoding="utf-8")
        result = self.dc.save_rule(
            self.path,
            {"name": "eDP-1", "description": "Lenovo Group Limited 0x4169"},
            {"output": "eDP-1", "mode": "1920x1080@60", "position": "13x1080"},
        )
        self.assertEqual(result["selector"], "desc:Lenovo Group Limited 0x4169")
        self.assertEqual(
            len([r for r in self.dc.parse_rules(self.path.read_text())
                 if r["output"] == result["selector"]]),
            1,
        )

    def test_name_rule_is_migrated_to_desc_selector(self):
        self.path.write_text(
            'hl.monitor({ output = "eDP-1", mode = "preferred", position = "0x0" })\n',
            encoding="utf-8",
        )
        result = self.dc.save_rule(
            self.path,
            {"name": "eDP-1", "description": "BOE 0x0A1F"},
            {"output": "eDP-1", "mode": "1920x1080@60", "position": "0x0"},
        )
        self.assertEqual(result["selector"], "desc:BOE 0x0A1F")
        rules = self.dc.parse_rules(self.path.read_text())
        self.assertEqual(len(rules), 1)
        self.assertEqual(rules[0]["output"], "desc:BOE 0x0A1F")
        self.assertNotIn('output = "eDP-1"', self.path.read_text())

    def test_long_bracket_documentation_survives_save(self):
        source = (
            'local doc = [[\nhl.monitor({ output = "DOC-1" })\n]]\n'
            'hl.monitor({ output = "HEADLESS-1", position = "0x0" })\n'
        )
        self.path.write_text(source, encoding="utf-8")
        self.dc.save_rule(
            self.path, {"name": "HEADLESS-1"},
            {"output": "HEADLESS-1", "position": "1920x0"},
        )
        text = self.path.read_text()
        self.assertIn('hl.monitor({ output = "DOC-1" })', text)
        self.assertIn('position = "1920x0"', text)

    def test_atomic_write_preserves_mode(self):
        self.path.write_text("-- empty\n", encoding="utf-8")
        self.path.chmod(0o640)
        self.dc._atomic_write(self.path, "-- updated\n")
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o640)

    def test_backups_are_pruned(self):
        self.path.write_text(SOURCE, encoding="utf-8")
        for index in range(5):
            stale = self.path.with_suffix(f".lua.omascreencast-stale{index}.bak")
            stale.write_text("old", encoding="utf-8")
        self.dc.backup_config(self.path)
        backups = list(self.path.parent.glob(self.path.name + ".omascreencast-*.bak"))
        self.assertLessEqual(len(backups), 3)

    def test_invalid_generated_source_never_replaces_the_file(self):
        # A rule whose selector collides with an existing broken call would
        # fail re-parsing; save_rule must leave the file untouched.
        broken = 'hl.monitor({ output = "HEADLESS-1"'
        self.path.write_text(broken, encoding="utf-8")
        with self.assertRaises(self.dc.ConfigError):
            self.dc.save_rule(
                self.path, {"name": "HEADLESS-1"},
                {"output": "HEADLESS-1", "mode": "1920x1080@60", "position": "1920x0"},
            )
        self.assertEqual(self.path.read_text(), broken)


if __name__ == "__main__":
    unittest.main()
