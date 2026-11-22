"""
Copperpour -- Action Plugin launcher.

This thin Python plugin registers a menu item in KiCAD's Tools menu.
Clicking it opens a dialog for Copperpour settings and the native Specctra bridge.

Installation (KiCAD PCM):
  The plugin appears in KiCAD's Plugin and Content Manager.
  After installation the binary is placed in the plugin directory.
  KiCAD loads this __init__.py automatically.
"""

import atexit
import os
import sys

import pcbnew  # Available inside KiCAD

# Import the settings dialog (same directory)
_plugin_dir = os.path.dirname(os.path.abspath(__file__))
if _plugin_dir not in sys.path:
    sys.path.insert(0, _plugin_dir)

from settings_dialog import CopperpourSettingsDialog, load_settings
from native_bridge import NativeSpecctraBridge
import wx

PLUGIN_DIR = _plugin_dir
SETTINGS_PATH = os.path.join(PLUGIN_DIR, "settings.json")
_native_bridge = NativeSpecctraBridge(pcbnew, wx)


def _sync_native_bridge():
    """Apply the persisted KiCad-10 bridge setting without exposing internals."""
    enabled = bool(load_settings(SETTINGS_PATH).get("native_specctra_bridge", False))
    if enabled:
        try:
            _native_bridge.start()
        except Exception as error:
            print(f"[Copperpour] Native Specctra bridge failed to start: {error}", file=sys.stderr)
    else:
        _native_bridge.stop()


atexit.register(_native_bridge.stop)


def find_pcb_editor_window():
    """Locate the active PCB Editor top-level window to parent dialogs.

    On macOS, parenting dialogs to wx.GetTopLevelWindows()[0] picks the KiCad
    project manager when KiCad was launched normally. Cocoa then raises the
    project manager to the front and covers the open board whenever child
    dialogs or message boxes open, focus, or close.
    """
    if not hasattr(wx, "GetTopLevelWindows"):
        return None

    # 1. Check window with current focus
    if hasattr(wx.Window, "FindFocus"):
        focus = wx.Window.FindFocus()
        if focus and hasattr(wx, "GetTopLevelParent"):
            top = wx.GetTopLevelParent(focus)
            if top and getattr(top, "IsShown", lambda: True)():
                return top

    # 2. Check active top-level window
    if hasattr(wx, "GetActiveWindow"):
        active = wx.GetActiveWindow()
        if active and hasattr(wx, "GetTopLevelParent"):
            top = wx.GetTopLevelParent(active)
            if top and getattr(top, "IsShown", lambda: True)():
                return top

    # 3. Match against open board name or PCB Editor in title
    board = None
    try:
        board = pcbnew.GetBoard()
    except Exception:
        pass

    board_name = ""
    if board and hasattr(board, "GetFileName") and board.GetFileName():
        board_name = os.path.splitext(os.path.basename(board.GetFileName()))[0].lower()

    top_windows = [w for w in wx.GetTopLevelWindows() if getattr(w, "IsShown", lambda: True)()]

    if board_name:
        for win in top_windows:
            title = getattr(win, "GetTitle", lambda: "")().lower()
            if board_name in title and ("pcb editor" in title or "pcb" in title or "— kicad" not in title):
                return win

    for win in top_windows:
        title = getattr(win, "GetTitle", lambda: "")().lower()
        if "pcb editor" in title or "pcbnew" in title or "leiterplatten" in title:
            return win

    for win in top_windows:
        if getattr(win, "IsActive", lambda: False)():
            return win

    # Fall back to a non-project-manager window if possible
    non_manager = [
        w for w in top_windows if "— kicad" not in getattr(w, "GetTitle", lambda: "")().lower()
    ]
    if non_manager:
        return non_manager[-1]

    return top_windows[0] if top_windows else None


# ─── KiCAD Action Plugin entry point ─────────────────────────────────────────

# KiCad stores the toolbar button setting under `<file>/<class name>`, so
# renaming this class resets it for every install.
class CopperpourPlugin(pcbnew.ActionPlugin):
    def defaults(self):
        self.name = "Copperpour"
        self.category = "AI Tools"
        self.description = (
            "Configure Copperpour -- enables AI assistants "
            "like Claude to design PCBs and schematics via the Model Context Protocol."
        )
        self.show_toolbar_button = True
        self.icon_file_name = os.path.join(PLUGIN_DIR, "resources", "icon.png")

    def Run(self):
        """Open the settings dialog and apply the saved bridge setting on close."""
        parent = find_pcb_editor_window()

        dlg = CopperpourSettingsDialog(
            parent=parent,
            plugin_dir=PLUGIN_DIR,
            native_bridge_available=_native_bridge.available(),
            native_bridge_running=_native_bridge.running(),
        )

        dlg.ShowModal()
        dlg.Destroy()
        _sync_native_bridge()

        if parent and hasattr(parent, "Raise"):
            try:
                parent.Raise()
            except Exception:
                pass


# Register the plugin with KiCAD
_sync_native_bridge()
CopperpourPlugin().register()
