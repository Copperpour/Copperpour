"""Tests for the Copperpour settings dialog and window parenting behavior."""

import json
import os
from pathlib import Path
import sys
import tempfile
import types
import unittest
from unittest import mock

PLUGIN_DIR = Path(__file__).resolve().parents[1]


def _ensure_stubs():
    if "pcbnew" not in sys.modules:
        pcbnew = types.ModuleType("pcbnew")
        class _ActionPlugin:
            def register(self):
                pass
        pcbnew.ActionPlugin = _ActionPlugin
        pcbnew.GetBoard = lambda: None
        sys.modules["pcbnew"] = pcbnew
    else:
        pcbnew = sys.modules["pcbnew"]
        if not hasattr(pcbnew, "ActionPlugin"):
            class _ActionPlugin:
                def register(self):
                    pass
            pcbnew.ActionPlugin = _ActionPlugin
        if not hasattr(pcbnew, "GetBoard"):
            pcbnew.GetBoard = lambda: None

    if "wx" not in sys.modules:
        wx = types.ModuleType("wx")
        sys.modules["wx"] = wx
    else:
        wx = sys.modules["wx"]

    wx.DEFAULT_DIALOG_STYLE = 1
    wx.RESIZE_BORDER = 2
    wx.ID_ANY = -1
    wx.ID_OK = 5100
    wx.ID_CLOSE = 5105
    wx.ID_SAVE = 5106
    wx.OK = 4
    wx.ICON_INFORMATION = 800
    wx.ALIGN_CENTER_VERTICAL = 1
    wx.ALIGN_RIGHT = 2
    wx.EXPAND = 4
    wx.ALL = 8
    wx.LEFT = 16
    wx.RIGHT = 32
    wx.BOTTOM = 64
    wx.VERTICAL = 1
    wx.HORIZONTAL = 2
    wx.EVT_BUTTON = object()

    class _Window:
        def __init__(self, title=""):
            self._title = title
            self._shown = True
            self._active = False

        def GetTitle(self):
            return self._title

        def IsShown(self):
            return self._shown

        def IsActive(self):
            return self._active

        def Raise(self):
            pass

    class _Dialog(_Window):
        def __init__(self, parent=None, title="", style=0):
            super().__init__(title=title)
            self.parent = parent
            self._end_modal_code = None

        def EndModal(self, code):
            self._end_modal_code = code

        def FromDIP(self, size):
            return size

        def SetMinClientSize(self, size):
            pass

        def Fit(self):
            pass

        def CenterOnParent(self):
            pass

        def SetSizer(self, sizer):
            pass

    class _DummyCtrl:
        def __init__(self, *a, **k):
            self._val = ""
            self._sel = 0

        def SetValue(self, val):
            self._val = val

        def GetValue(self):
            return self._val

        def SetSelection(self, sel):
            self._sel = sel

        def GetSelection(self):
            return self._sel

        def Enable(self, en):
            pass

        def Bind(self, *a, **k):
            pass

        def SetSizer(self, sizer):
            pass

    wx.Window = _Window
    wx.Dialog = _Dialog
    wx.Panel = lambda *a, **k: _DummyCtrl()
    wx.BoxSizer = lambda *a, **k: mock.MagicMock()
    wx.StaticBoxSizer = lambda *a, **k: mock.MagicMock()
    wx.FlexGridSizer = lambda *a, **k: mock.MagicMock()
    wx.StdDialogButtonSizer = lambda *a, **k: mock.MagicMock()
    wx.StaticText = lambda *a, **k: _DummyCtrl()
    wx.TextCtrl = lambda *a, **k: _DummyCtrl()
    wx.Button = lambda *a, **k: _DummyCtrl()
    wx.Choice = lambda *a, **k: _DummyCtrl()
    wx.CheckBox = lambda *a, **k: _DummyCtrl()
    wx.Size = lambda *a, **k: None
    wx.MessageBox = mock.MagicMock()
    wx.GetTopLevelWindows = lambda: []
    wx.GetActiveWindow = lambda: None
    wx.GetTopLevelParent = lambda win: win
    wx.Window.FindFocus = lambda: None

    if str(PLUGIN_DIR) not in sys.path:
        sys.path.insert(0, str(PLUGIN_DIR))

    import importlib
    spec_diag = importlib.util.spec_from_file_location(
        "settings_dialog", PLUGIN_DIR / "settings_dialog.py"
    )
    settings_dialog = importlib.util.module_from_spec(spec_diag)
    sys.modules["settings_dialog"] = settings_dialog
    spec_diag.loader.exec_module(settings_dialog)

    spec_init = importlib.util.spec_from_file_location(
        "konnect_init", PLUGIN_DIR / "__init__.py"
    )
    konnect_init = importlib.util.module_from_spec(spec_init)
    spec_init.loader.exec_module(konnect_init)

    return konnect_init, settings_dialog, wx, pcbnew


class SettingsDialogFocusTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.plugin, cls.dialog_mod, cls.wx, cls.pcbnew = _ensure_stubs()

    def test_find_pcb_editor_window_prefers_pcb_editor_over_project_manager(self):
        proj_mgr = self.wx.Window(title="MyProject — KiCad 10.0")
        pcb_editor = self.wx.Window(title="MyProject — PCB Editor")

        with mock.patch.object(self.plugin.wx, "GetTopLevelWindows", return_value=[proj_mgr, pcb_editor]), \
                mock.patch.object(self.plugin.wx.Window, "FindFocus", return_value=None), \
                mock.patch.object(self.plugin.wx, "GetActiveWindow", return_value=None):
            found = self.plugin.find_pcb_editor_window()
            self.assertIs(found, pcb_editor, "PCB Editor frame must be chosen over the KiCad Project Manager")

    def test_find_pcb_editor_window_uses_focus_when_available(self):
        proj_mgr = self.wx.Window(title="MyProject — KiCad 10.0")
        pcb_editor = self.wx.Window(title="MyProject — PCB Editor")
        focused_child = self.wx.Window(title="canvas")

        with mock.patch.object(self.plugin.wx, "GetTopLevelWindows", return_value=[proj_mgr, pcb_editor]), \
                mock.patch.object(self.plugin.wx.Window, "FindFocus", return_value=focused_child), \
                mock.patch.object(self.plugin.wx, "GetTopLevelParent", return_value=pcb_editor):
            found = self.plugin.find_pcb_editor_window()
            self.assertIs(found, pcb_editor)

    def test_find_pcb_editor_window_matches_board_filename(self):
        proj_mgr = self.wx.Window(title="Lichtwecker — KiCad 10.0")
        pcb_editor = self.wx.Window(title="*Lichtwecker.kicad_pcb")

        fake_board = mock.MagicMock()
        fake_board.GetFileName.return_value = "/path/to/Lichtwecker.kicad_pcb"

        with mock.patch.object(self.plugin.wx, "GetTopLevelWindows", return_value=[proj_mgr, pcb_editor]), \
                mock.patch.object(self.plugin.wx.Window, "FindFocus", return_value=None), \
                mock.patch.object(self.plugin.wx, "GetActiveWindow", return_value=None), \
                mock.patch.object(self.plugin.pcbnew, "GetBoard", return_value=fake_board):
            found = self.plugin.find_pcb_editor_window()
            self.assertIs(found, pcb_editor)

    def test_the_action_plugin_class_keeps_its_name(self):
        # KiCad stores the toolbar button setting under `<file>/<class name>`,
        # so renaming the class resets it for every install (#250).
        base = self.pcbnew.ActionPlugin
        names = [
            value.__name__
            for value in vars(self.plugin).values()
            if isinstance(value, type) and issubclass(value, base) and value is not base
        ]
        self.assertEqual(names, ["CopperpourPlugin"])

    def test_save_preserves_settings_without_controls(self):
        saved_settings = {
            "eager_toolsets": True,
            "future_setting": {"enabled": False, "items": ["example"]},
            "kicad_cli": "/fake/kicad-cli",
            "log_level": "info",
        }

        with tempfile.TemporaryDirectory() as plugin_dir:
            settings_path = Path(plugin_dir) / "settings.json"
            settings_path.write_text(json.dumps(saved_settings), encoding="utf-8")
            dlg = self.dialog_mod.CopperpourSettingsDialog(parent=None, plugin_dir=plugin_dir)
            dlg.log_level_ctrl.SetSelection(3)  # debug

            dlg._on_save(None)

            persisted = json.loads(settings_path.read_text(encoding="utf-8"))
            self.assertIs(persisted.get("eager_toolsets"), True)
            self.assertEqual(persisted.get("future_setting"), saved_settings["future_setting"])
            self.assertEqual(persisted["log_level"], "debug")

    def test_on_save_passes_parent_self_to_messagebox(self):
        dlg = self.dialog_mod.CopperpourSettingsDialog(
            parent=None,
            plugin_dir=str(PLUGIN_DIR),
        )
        self.wx.MessageBox.reset_mock()
        with mock.patch("settings_dialog.save_settings"):
            dlg._on_save(None)

        self.wx.MessageBox.assert_called_once()
        _args, kwargs = self.wx.MessageBox.call_args
        self.assertIs(kwargs.get("parent"), dlg, "wx.MessageBox must be parented to the dialog")

    def test_plugin_applies_saved_bridge_setting_after_dialog_closes(self):
        bridge = mock.Mock()
        bridge.available.return_value = True
        with tempfile.TemporaryDirectory() as plugin_dir, \
                mock.patch.object(self.plugin, "PLUGIN_DIR", plugin_dir), \
                mock.patch.object(self.plugin, "SETTINGS_PATH", os.path.join(plugin_dir, "settings.json")), \
                mock.patch.object(self.plugin, "_native_bridge", bridge), \
                mock.patch.object(self.plugin, "CopperpourSettingsDialog") as dialog:
            settings_path = Path(plugin_dir) / "settings.json"
            for enabled in (True, False):
                with self.subTest(enabled=enabled):
                    bridge.reset_mock()
                    bridge.running.return_value = not enabled

                    def save_and_close():
                        settings_path.write_text(
                            json.dumps({"native_specctra_bridge": enabled}), encoding="utf-8"
                        )
                        return self.wx.ID_CLOSE

                    dialog.return_value.ShowModal.side_effect = save_and_close
                    self.plugin.CopperpourPlugin().Run()

                    dialog.assert_called_with(
                        parent=None,
                        plugin_dir=plugin_dir,
                        native_bridge_available=True,
                        native_bridge_running=not enabled,
                    )
                    self.assertEqual(
                        bridge.method_calls,
                        [mock.call.available(), mock.call.running(),
                         mock.call.start() if enabled else mock.call.stop()],
                    )
                    dialog.return_value.Destroy.assert_called()


if __name__ == "__main__":
    unittest.main()
