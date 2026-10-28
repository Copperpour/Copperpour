"""Tests for the narrowly-scoped KiCad 10 native Specctra bridge."""

import importlib.util
import json
import ntpath
import os
import tempfile
import types
import unittest
import urllib.error
import urllib.request
from unittest import mock


_PLUGIN_DIR = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
_SPEC = importlib.util.spec_from_file_location(
    "konnect_native_bridge_under_test",
    os.path.join(_PLUGIN_DIR, "native_bridge.py"),
)
native_bridge = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(native_bridge)


class _Board:
    def __init__(self, path):
        self.path = path

    def GetFileName(self):
        return self.path


class _Pcbnew:
    def __init__(self, board_path):
        self.board = _Board(board_path)

    def GetBoard(self):
        return self.board

    @staticmethod
    def ExportSpecctraDSN(output):
        with open(output, "w", encoding="utf-8") as stream:
            stream.write("(pcb native-test)\n")
        return True


class _Wx:
    @staticmethod
    def CallAfter(callback):
        callback()


def _request(registration, path, body=None, token=None):
    headers = {"Authorization": f"Bearer {token or registration['token']}"}
    data = None
    method = "GET"
    if body is not None:
        data = json.dumps(body).encode("utf-8")
        headers["Content-Type"] = "application/json"
        method = "POST"
    request = urllib.request.Request(
        registration["address"] + path,
        data=data,
        headers=headers,
        method=method,
    )
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.status, json.load(response)


# ntpath as KiCad 10's Python 3.11 has it: a rooted path without a drive
# (`\b`, `/b`) counts as absolute; 3.13 changed that.
_NTPATH_311 = types.SimpleNamespace(
    isabs=lambda path: ntpath.isabs(path) or path[:1] in ("\\", "/"),
    splitdrive=ntpath.splitdrive,
    join=ntpath.join,
)


def _bridge_root(env, platform="linux", os_name="posix"):
    # Only native_bridge sees the other platform's `os`, not the shared module.
    fake_os = types.SimpleNamespace(
        environ=env,
        name=os_name,
        path=_NTPATH_311 if os_name == "nt" else os.path,
        getuid=getattr(os, "getuid", None),
    )
    with mock.patch.object(native_bridge, "os", fake_os), mock.patch.object(
        native_bridge, "sys_platform", return_value=platform
    ):
        return native_bridge.bridge_root()


class BridgeRootTests(unittest.TestCase):
    # The server's half of this table is a_relative_bridge_dir_gives_no_root.

    LEAF = os.path.join("copperpour", "native-bridge")

    @unittest.skipIf(os.name == "nt", "needs the POSIX account database")
    def test_linux_follows_the_dirs_rule(self):
        import pwd

        account_home = pwd.getpwuid(os.getuid()).pw_dir
        cases = [
            ({"HOME": "/h"}, "/h/.local/share"),
            ({"HOME": "/h", "XDG_DATA_HOME": "/x"}, "/x"),
            ({"HOME": "/h", "XDG_DATA_HOME": ""}, "/h/.local/share"),
            ({"HOME": "/h", "XDG_DATA_HOME": "data"}, "/h/.local/share"),
            ({"HOME": "home", "XDG_DATA_HOME": "/x"}, "/x"),
            ({"HOME": "", "XDG_DATA_HOME": ""}, account_home + "/.local/share"),
            ({}, account_home + "/.local/share"),
        ]
        for env, base in cases:
            with self.subTest(env=env):
                self.assertEqual(_bridge_root(env), os.path.join(base, self.LEAF))

    def test_no_absolute_directory_gives_none(self):
        cases = [
            ({"HOME": "home"}, "linux", "posix"),
            ({"HOME": "home", "XDG_DATA_HOME": ""}, "linux", "posix"),
            ({"HOME": "home", "XDG_DATA_HOME": "data"}, "linux", "posix"),
            ({"HOME": "home", "XDG_DATA_HOME": "/x"}, "darwin", "posix"),
            ({}, "win32", "nt"),
            ({"LOCALAPPDATA": ""}, "win32", "nt"),
            ({"LOCALAPPDATA": "relative"}, "win32", "nt"),
            ({"LOCALAPPDATA": "\\Users\\x"}, "win32", "nt"),
            ({"LOCALAPPDATA": "C:Users"}, "win32", "nt"),
            ({"COPPERPOUR_BRIDGE_DIR": "/b"}, "win32", "nt"),
            ({"COPPERPOUR_BRIDGE_DIR": "rel", "HOME": "/h"}, "linux", "posix"),
            ({"KONNECT_BRIDGE_DIR": "rel", "HOME": "/h"}, "linux", "posix"),
            ({"COPPERPOUR_BRIDGE_DIR": "rel", "KONNECT_BRIDGE_DIR": "/k"}, "linux", "posix"),
            ({"COPPERPOUR_BRIDGE_DIR": "~/b", "HOME": "/h"}, "linux", "posix"),
        ]
        for env, platform, os_name in cases:
            with self.subTest(env=env, platform=platform):
                self.assertIsNone(_bridge_root(env, platform, os_name))

    def test_macos_and_windows_bases(self):
        self.assertEqual(
            _bridge_root({"HOME": "/h", "XDG_DATA_HOME": "/x"}, "darwin"),
            os.path.join("/h/Library/Application Support", self.LEAF),
        )
        self.assertEqual(
            _bridge_root({"LOCALAPPDATA": "C:\\L"}, "win32", "nt"),
            "C:\\L\\copperpour\\native-bridge",
        )
        self.assertEqual(
            _bridge_root({"COPPERPOUR_BRIDGE_DIR": "\\\\srv\\share\\b"}, "win32", "nt"),
            "\\\\srv\\share\\b",
        )

    def test_override_is_used_as_is(self):
        self.assertEqual(_bridge_root({"COPPERPOUR_BRIDGE_DIR": "/b", "HOME": "home"}), "/b")
        self.assertEqual(
            _bridge_root({"COPPERPOUR_BRIDGE_DIR": "", "KONNECT_BRIDGE_DIR": "/k"}), "/k"
        )

    def test_without_a_directory_the_bridge_is_unavailable_and_writes_nothing(self):
        with tempfile.TemporaryDirectory() as cwd:
            previous = os.getcwd()
            os.chdir(cwd)
            self.addCleanup(os.chdir, previous)
            with mock.patch.dict(os.environ, {"HOME": "home", "XDG_DATA_HOME": ""}, clear=True):
                bridge = native_bridge.NativeSpecctraBridge(_Pcbnew("board.kicad_pcb"), _Wx())
                self.assertFalse(bridge.available())
                self.assertFalse(bridge.start())
                bridge.stop()
            self.assertFalse(bridge.running())
            self.assertEqual(os.listdir(cwd), [])


class NativeBridgeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.board = os.path.join(self.temp.name, "board.kicad_pcb")
        with open(self.board, "w", encoding="utf-8") as stream:
            stream.write("(kicad_pcb)\n")
        self.bridge = native_bridge.NativeSpecctraBridge(
            _Pcbnew(self.board),
            _Wx(),
            root=os.path.join(self.temp.name, "bridge"),
        )
        self.assertTrue(self.bridge.start())
        self.addCleanup(self.bridge.stop)
        with open(self.bridge.registration_path, encoding="utf-8") as stream:
            self.registration = json.load(stream)

    def test_status_and_export_require_the_registration_token(self):
        status, payload = _request(self.registration, "/v1/status")
        self.assertEqual(status, 200)
        self.assertTrue(payload["native_specctra_export"])

        with self.assertRaises(urllib.error.HTTPError) as caught:
            _request(self.registration, "/v1/status", token="wrong")
        self.assertEqual(caught.exception.code, 401)

    def test_export_uses_a_bridge_owned_path_and_reports_the_active_board(self):
        status, payload = _request(
            self.registration,
            "/v1/export-specctra-dsn",
            {"expected_board": self.board},
        )
        self.assertEqual(status, 200)
        self.assertTrue(payload["success"])
        self.assertEqual(os.path.realpath(payload["board_path"]), os.path.realpath(self.board))
        self.assertTrue(os.path.isfile(payload["dsn_path"]))
        self.assertEqual(
            os.path.commonpath([payload["dsn_path"], self.bridge.session_dir]),
            self.bridge.session_dir,
        )
        self.assertGreater(payload["dsn_bytes"], 0)

    def test_export_refuses_a_different_board(self):
        with self.assertRaises(urllib.error.HTTPError) as caught:
            _request(
                self.registration,
                "/v1/export-specctra-dsn",
                {"expected_board": os.path.join(self.temp.name, "other.kicad_pcb")},
            )
        self.assertEqual(caught.exception.code, 409)

    def test_stop_removes_registration_and_session_artifacts(self):
        registration = self.bridge.registration_path
        session = self.bridge.session_dir
        self.bridge.stop()
        self.assertFalse(os.path.exists(registration))
        self.assertFalse(os.path.exists(session))


if __name__ == "__main__":
    unittest.main()
