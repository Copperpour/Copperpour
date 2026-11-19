"""Tests for packaging/build-mcpb.py and packaging/validate-mcpb.py.

Run: python -m unittest discover -s packaging/tests -v  (needs jsonschema)
"""

import copy
import importlib.util
import json
import os
import sys
import tempfile
import unittest
import zipfile
from pathlib import Path

PACKAGING = Path(__file__).resolve().parent.parent


def _load(name: str, filename: str):
    spec = importlib.util.spec_from_file_location(name, PACKAGING / filename)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


build_mcpb = _load("build_mcpb", "build-mcpb.py")
validate_mcpb = _load("validate_mcpb", "validate-mcpb.py")

STUBS = {
    "windows": b"MZ\x90\x00" + b"\x00" * 16,
    "macos": b"\xca\xfe\xba\xbe" + b"\x00" * 16,  # universal Mach-O
    "linux": b"\x7fELF\x02\x01\x01\x00" + b"\x00" * 12,
}


def template() -> dict:
    return json.loads((PACKAGING / "mcpb-manifest.json").read_text(encoding="utf-8"))


class ManifestTemplateTests(unittest.TestCase):
    def test_repository_template_passes(self):
        self.assertEqual(
            validate_mcpb.validate_manifest(template(), "mcpb-manifest.json"), []
        )

    def test_template_targets_v0_3_schema(self):
        manifest = template()
        self.assertEqual(manifest["manifest_version"], "0.3")
        self.assertEqual(
            validate_mcpb.load_schema()["properties"]["manifest_version"]["const"],
            "0.3",
        )

    def test_license_matches_crate_license(self):
        self.assertEqual(template()["license"], "AGPL-3.0-only")

    def test_descriptions_quote_no_tool_or_toolset_count(self):
        manifest = template()
        for field in ("description", "long_description"):
            self.assertNotRegex(
                manifest[field], r"\d+\+?(\s+[\w-]+)?\s+tool", field
            )

    def test_schema_violation_is_reported_not_raised(self):
        manifest = template()
        manifest["author"] = "Copperpour"
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertEqual(len(found), 1, found)
        self.assertIn("schema violation at $.author", found[0])

    def test_rejects_non_binary_server_type(self):
        manifest = template()
        manifest["server"]["type"] = "node"
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("server.type must be 'binary'" in e for e in found), found)

    def test_rejects_missing_tools_generated(self):
        manifest = template()
        manifest["tools_generated"] = False
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("tools_generated must be true" in e for e in found), found)

    def test_rejects_multi_platform_declaration(self):
        manifest = template()
        manifest["compatibility"]["platforms"] = ["darwin", "linux"]
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("exactly one platform" in e for e in found), found)

    def test_rejects_mismatched_command_and_entry_point(self):
        manifest = template()
        manifest["server"]["mcp_config"]["command"] = "server/copperpour"
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("server.mcp_config.command" in e for e in found), found)

    def test_rejects_invalid_semver_version(self):
        manifest = template()
        manifest["version"] = "not-a-version"
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("not valid semver" in e for e in found), found)

    def test_rejects_non_empty_mcp_config_args(self):
        manifest = template()
        manifest["server"]["mcp_config"]["args"] = ["--config", "/tmp/x.toml"]
        found = validate_mcpb.validate_manifest(manifest, "mcpb-manifest.json")
        self.assertTrue(any("server.mcp_config.args must be empty" in e for e in found), found)


class BuildAndValidateBundleTests(unittest.TestCase):
    def _build_stub(self, td: Path, platform: str, version: str = "0.16.1", blob: bytes | None = None) -> Path:
        stub = td / f"stub-{platform}"
        stub.write_bytes(STUBS[platform] if blob is None else blob)
        return build_mcpb.build_bundle(version, stub, platform, td / "dist")

    def test_all_three_platforms_build_and_validate(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            expected_names = {
                "windows": "copperpour-v0.16.1-windows-x86_64.mcpb",
                "macos": "copperpour-v0.16.1-macos-universal.mcpb",
                "linux": "copperpour-v0.16.1-linux-x86_64.mcpb",
            }
            for platform, expected_name in expected_names.items():
                with self.subTest(platform=platform):
                    bundle = self._build_stub(root, platform)
                    self.assertEqual(bundle.name, expected_name)
                    errors, manifest = validate_mcpb.validate_bundle(bundle, "0.16.1")
                    self.assertEqual(errors, [])
                    self.assertIsNotNone(manifest)
                    self.assertEqual(manifest["version"], "0.16.1")

    def test_build_is_byte_for_byte_reproducible(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            stub = root / "stub-linux"
            stub.write_bytes(STUBS["linux"])
            first = build_mcpb.build_bundle("1.2.3", stub, "linux", root / "out1")
            second = build_mcpb.build_bundle("1.2.3", stub, "linux", root / "out2")
            self.assertEqual(first.read_bytes(), second.read_bytes())

    def test_rejects_mismatched_binary_magic_for_platform(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "linux", blob=STUBS["windows"])
            errors, _ = validate_mcpb.validate_bundle(bundle, "0.16.1")
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("a win32 executable", errors[0])
            self.assertIn("contradicting declared platform 'linux'", errors[0])

    def test_rejects_version_mismatch(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "linux", version="0.16.1")
            errors, _ = validate_mcpb.validate_bundle(bundle, expected_version="0.17.0")
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("expected '0.17.0'", errors[0])

    def test_rejects_posix_binary_without_exec_bit(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "macos")
            repacked = root / "no-exec.mcpb"
            with zipfile.ZipFile(bundle) as src, zipfile.ZipFile(repacked, "w") as dst:
                for info in src.infolist():
                    new_info = copy.copy(info)
                    if info.filename == "server/copperpour":
                        new_info.external_attr = 0o100644 << 16
                    dst.writestr(new_info, src.read(info.filename))
            errors, _ = validate_mcpb.validate_bundle(repacked)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("not stored with Unix executable bits", errors[0])

    def test_rejects_invalid_png_icon(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "linux")
            repacked = root / "bad-icon.mcpb"
            with zipfile.ZipFile(bundle) as src, zipfile.ZipFile(repacked, "w") as dst:
                for info in src.infolist():
                    data = b"not a png" if info.filename == "icon.png" else src.read(info.filename)
                    dst.writestr(info, data)
            errors, _ = validate_mcpb.validate_bundle(repacked)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("icon.png is not a valid PNG image", errors[0])

    def test_rejects_unexpected_archive_entries_and_path_traversal(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "windows")
            for bad_name in ("../escape.txt", "..\\escape.txt", "C:/Windows/win.ini"):
                with self.subTest(bad_name=bad_name):
                    repacked = root / f"extra-{abs(hash(bad_name))}.mcpb"
                    with zipfile.ZipFile(bundle) as src, zipfile.ZipFile(repacked, "w") as dst:
                        for info in src.infolist():
                            dst.writestr(info, src.read(info.filename))
                        dst.writestr(bad_name, b"boom")
                    errors, _ = validate_mcpb.validate_bundle(repacked)
                    self.assertTrue(any("unsafe archive entry path" in e for e in errors), errors)
                    self.assertTrue(any("unexpected archive entries" in e for e in errors), errors)

    def test_rejects_duplicate_archive_entries(self):
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            bundle = self._build_stub(root, "linux")
            repacked = root / "dup.mcpb"
            with zipfile.ZipFile(bundle) as src, zipfile.ZipFile(repacked, "w") as dst:
                for info in src.infolist():
                    dst.writestr(info, src.read(info.filename))
                dst.writestr("icon.png", src.read("icon.png"))
            errors, _ = validate_mcpb.validate_bundle(repacked)
            self.assertTrue(any("duplicate entries" in e for e in errors), errors)


FAKE_MCP_SERVER = f"""#!{sys.executable}
import json, sys
from pathlib import Path

if len(sys.argv) > 1 and sys.argv[1] == "--version":
    print("copperpour __VERSION__")
    sys.exit(0)

loaded = False
for raw in sys.stdin:
    msg = json.loads(raw)
    method = msg.get("method")
    req_id = msg.get("id")
    if method == "initialize":
        resp = {{"jsonrpc": "2.0", "id": req_id, "result": {{
            "instructions": "Operating rules",
            "serverInfo": {{"name": "copperpour", "version": "__VERSION__"}},
        }}}}
    elif method == "tools/list":
        tools = [
            {{"name": "get_guidance"}},
            {{"name": "get_installation_info"}},
            {{"name": "list_toolboxes"}},
            {{"name": "load_toolset"}},
        ]
        if loaded:
            tools.append({{"name": "add_component"}})
        resp = {{"jsonrpc": "2.0", "id": req_id, "result": {{"tools": tools}}}}
    elif method == "tools/call":
        name = msg["params"]["name"]
        if name == "get_installation_info":
            body = json.dumps({{"runtime": {{"executable_path": str(Path(__file__).resolve())}}}})
            resp = {{"jsonrpc": "2.0", "id": req_id, "result": {{"content": [{{"type": "text", "text": body}}], "isError": False}}}}
        elif name == "load_toolset":
            loaded = True
            sys.stdout.write(json.dumps({{"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}}) + "\\n")
            sys.stdout.flush()
            resp = {{"jsonrpc": "2.0", "id": req_id, "result": {{"content": [{{"type": "text", "text": "ok"}}], "isError": False}}}}
        else:
            resp = {{"jsonrpc": "2.0", "id": req_id, "result": {{"isError": True}}}}
    else:
        continue
    sys.stdout.write(json.dumps(resp) + "\\n")
    sys.stdout.flush()
"""


@unittest.skipIf(os.name == "nt", "POSIX shebang fake server test")
class SmokeTestRunnerTests(unittest.TestCase):
    def _build_fake_bundle(self, root: Path, manifest_version: str, binary_version: str) -> tuple[Path, dict]:
        script = root / "fake-copperpour"
        script.write_text(FAKE_MCP_SERVER.replace("__VERSION__", binary_version), encoding="utf-8")
        platform = "macos" if sys.platform == "darwin" else "linux"
        bundle = build_mcpb.build_bundle(manifest_version, script, platform, root / "dist")
        with zipfile.ZipFile(bundle) as z:
            manifest = json.loads(z.read("manifest.json").decode("utf-8"))
        return bundle, manifest

    def test_smoke_test_passes_and_checks_binary_version(self):
        with tempfile.TemporaryDirectory() as td:
            bundle, manifest = self._build_fake_bundle(Path(td), "0.16.1", "0.16.1")
            errors = validate_mcpb.smoke_test_bundle(bundle, manifest, check_binary_version=True)
            self.assertEqual(errors, [])

    def test_smoke_test_rejects_binary_version_mismatch(self):
        with tempfile.TemporaryDirectory() as td:
            bundle, manifest = self._build_fake_bundle(Path(td), "0.16.1", "0.16.0")
            errors = validate_mcpb.smoke_test_bundle(bundle, manifest, check_binary_version=True)
            self.assertEqual(len(errors), 1, errors)
            self.assertIn("bundled binary reports version '0.16.0'", errors[0])
            # Without --check-binary-version (dry run), version mismatch is allowed.
            errors_dry = validate_mcpb.smoke_test_bundle(bundle, manifest, check_binary_version=False)
            self.assertEqual(errors_dry, [])


if __name__ == "__main__":
    unittest.main()
