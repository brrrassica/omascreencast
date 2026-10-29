"""Smoke tests for the omascreencast helper scripts.

Run with:  python3 -m unittest discover -s tests -v
"""

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = ROOT / "scripts"


def _load(name: str):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


class SessionStateTest(unittest.TestCase):
    def test_load_default_shape(self):
        session = _load("session")
        state = session.load()
        self.assertIn("active", state)


class DisplayConfigTest(unittest.TestCase):
    def test_read_missing_file_is_empty(self):
        display = _load("display_config")
        self.assertEqual(display.read_config(Path("/nonexistent/monitors.lua")), "")


class BootstrapTest(unittest.TestCase):
    def test_check_returns_keys(self):
        bootstrap = _load("bootstrap")
        status = bootstrap.check()
        for key in ("ok", "missingBinaries", "missingPython", "venv", "venvExists"):
            self.assertIn(key, status)


if __name__ == "__main__":
    unittest.main()
