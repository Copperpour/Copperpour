"""Regression tests for the plugin's server-PID bookkeeping (#103).

The plugin runs inside KiCAD, so `plugin/__init__.py` imports `pcbnew`,
`wx` and its own `settings_dialog` at module level. None of those exist in
CI, so they are stubbed in `sys.modules` before the import — the PID logic
under test touches none of them.

Run with:  python -m unittest discover -s plugin/tests
"""

import os
import sys
import tempfile
import types
import unittest
from unittest import mock

_PLUGIN_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _load_plugin():
    """Import plugin/__init__.py with its KiCAD-only dependencies stubbed."""
    pcbnew = types.ModuleType("pcbnew")

    class _ActionPlugin:
        def register(self):
            pass

    pcbnew.ActionPlugin = _ActionPlugin
    sys.modules.setdefault("pcbnew", pcbnew)

    wx = types.ModuleType("wx")
    wx.Dialog = object
    sys.modules.setdefault("wx", wx)

    settings_dialog = types.ModuleType("settings_dialog")
    settings_dialog.KonnectSettingsDialog = object
    settings_dialog.load_settings = lambda *a, **k: {}
    sys.modules.setdefault("settings_dialog", settings_dialog)

    if _PLUGIN_DIR not in sys.path:
        sys.path.insert(0, _PLUGIN_DIR)
    import importlib

    spec = importlib.util.spec_from_file_location(
        "konnect_plugin_under_test", os.path.join(_PLUGIN_DIR, "__init__.py")
    )
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PidFileLifecycle(unittest.TestCase):
    def setUp(self):
        self.plugin = _load_plugin()
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # Point the module's cache paths at a throwaway directory.
        self._orig_cache = self.plugin._CACHE_DIR
        self._orig_pid = self.plugin._PID_FILE
        self.plugin._CACHE_DIR = self.tmp.name
        self.plugin._PID_FILE = os.path.join(self.tmp.name, "server.pid")

    def tearDown(self):
        self.plugin._CACHE_DIR = self._orig_cache
        self.plugin._PID_FILE = self._orig_pid

    def test_clearing_removes_the_record_this_session_wrote(self):
        self.plugin._write_pid_file(4242)
        self.plugin._clear_pid_file(4242)
        self.assertFalse(
            os.path.exists(self.plugin._PID_FILE),
            "a session must clean up the record it wrote",
        )

    def test_clearing_leaves_another_sessions_record_alone(self):
        """The bug in #103: every KiCAD session writes the same file.

        Session A's server exiting used to `os.remove` unconditionally,
        deleting session B's record while B's server was still running. B
        then became untracked, so no later preflight could ever reap it.
        """
        self.plugin._write_pid_file(1111)  # session B, still running
        self.plugin._clear_pid_file(2222)  # session A's server exits
        self.assertTrue(
            os.path.exists(self.plugin._PID_FILE),
            "a foreign PID record must survive another session's exit",
        )
        with open(self.plugin._PID_FILE) as f:
            self.assertEqual(f.read().strip(), "1111")

    def test_clearing_a_missing_or_corrupt_record_is_not_an_error(self):
        # No file at all.
        self.plugin._clear_pid_file(1)
        # Unparseable contents — a truncated write, or a stray edit.
        with open(self.plugin._PID_FILE, "w") as f:
            f.write("not-a-pid")
        self.plugin._clear_pid_file(1)
        self.assertTrue(
            os.path.exists(self.plugin._PID_FILE),
            "an unreadable record is left for a human rather than guessed at",
        )

    def test_writing_creates_the_cache_directory(self):
        nested = os.path.join(self.tmp.name, "deep", "cache")
        self.plugin._CACHE_DIR = nested
        self.plugin._PID_FILE = os.path.join(nested, "server.pid")
        self.plugin._write_pid_file(7)
        with open(self.plugin._PID_FILE) as f:
            self.assertEqual(f.read().strip(), "7")


class _Process:
    pid = 31337

    def __init__(self):
        self.killed = False

    def poll(self):
        return 0 if self.killed else None

    def kill(self):
        self.killed = True


class WithoutCacheDirectory(unittest.TestCase):
    """#190: no absolute cache directory means no file below KiCad's cwd."""

    def setUp(self):
        self.cwd = tempfile.TemporaryDirectory()
        self.addCleanup(self.cwd.cleanup)
        previous = os.getcwd()
        os.chdir(self.cwd.name)
        self.addCleanup(os.chdir, previous)
        env = {"HOME": "home", "LOCALAPPDATA": "data"}
        with mock.patch.dict(os.environ, env, clear=True):
            self.plugin = _load_plugin()
        # Its atexit handler would otherwise end a fake server at exit.
        self.addCleanup(setattr, self.plugin, "_server_process", None)

    def tearDown(self):
        self.assertEqual(os.listdir(self.cwd.name), [], "nothing is written below the cwd")

    def test_a_relative_home_gives_no_cache_directory(self):
        self.assertIsNone(self.plugin._CACHE_DIR)
        self.assertIsNone(self.plugin._PID_FILE)

    def test_pid_bookkeeping_is_skipped(self):
        self.plugin._write_pid_file(4242)
        self.plugin._clear_pid_file(4242)
        self.plugin._kill_tracked()
        self.assertFalse(self.plugin.is_server_running())

    def test_the_start_preflight_leaves_this_sessions_server_running(self):
        process = _Process()
        self.plugin._server_process = process
        self.plugin._kill_tracked()
        self.assertFalse(process.killed)
        self.assertTrue(self.plugin.is_server_running())

    def test_the_binary_runs_from_the_plugin_unstaged(self):
        with mock.patch.object(self.plugin, "sys", types.SimpleNamespace(platform="win32")):
            self.assertEqual(self.plugin._stage("plugin/copperpour.exe"), "plugin/copperpour.exe")

    def test_stop_still_ends_the_server_this_session_started(self):
        process = _Process()
        self.plugin._server_process = process
        self.assertTrue(self.plugin.is_server_running())
        self.plugin.stop_server()
        self.assertTrue(process.killed)
        self.assertFalse(self.plugin.is_server_running())


if __name__ == "__main__":
    unittest.main()
