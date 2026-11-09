"""Tests for packaging/validate-pcm.py's metadata checks.

Run: python -m unittest discover -s packaging/tests -v  (needs jsonschema)
"""

import copy
import importlib.util
import json
import tempfile
import unittest
import zipfile
from pathlib import Path

PACKAGING = Path(__file__).resolve().parent.parent

_spec = importlib.util.spec_from_file_location(
    "validate_pcm", PACKAGING / "validate-pcm.py"
)
validate_pcm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(validate_pcm)

# KiCad's addon guidelines (https://dev-docs.kicad.org/en/addons/) cap the
# short description at 150 characters. A literal, not the validator's constant.
GUIDELINE_DESCRIPTION_CAP = 150


def template() -> dict:
    return json.loads((PACKAGING / "metadata.json").read_text(encoding="utf-8"))


def plugin_manifest() -> dict:
    return json.loads(
        (PACKAGING.parent / "plugin" / "plugin.json").read_text(encoding="utf-8")
    )


def errors(meta: dict) -> list[str]:
    return validate_pcm.validate_metadata(meta, "metadata.json")


class MetadataTemplate(unittest.TestCase):
    def test_repository_template_passes(self):
        self.assertEqual(errors(template()), [])

    def test_template_targets_the_vendored_v2_schema(self):
        meta = template()
        self.assertEqual(meta["$schema"], "https://go.kicad.org/pcm/schemas/v2")
        self.assertEqual(validate_pcm.load_schema()["$id"], meta["$schema"])

    def test_license_is_the_crate_license(self):
        # v1's license enum has no AGPL, so this also fails under the v1 schema.
        self.assertEqual(template()["license"], "AGPL-3.0-only")

    def test_descriptions_quote_no_tool_or_toolset_count(self):
        meta = template()
        plugin = plugin_manifest()
        for label, text in (
            ("description", meta["description"]),
            ("description_full", meta["description_full"]),
            ("plugin.json description", plugin["description"]),
        ):
            # "245 tools", "245+ tools", "245 MCP tools", "22 toolsets"
            self.assertNotRegex(text, r"\d+\+?(\s+[\w-]+)?\s+tool", label)


class SchemaViolation(unittest.TestCase):
    def test_wrong_shapes_are_reported_not_raised(self):
        for key, value in (("description", None), ("versions", ["oops"])):
            meta = template()
            meta[key] = value
            found = errors(meta)
            self.assertEqual(len(found), 1, found)
            self.assertIn(f"schema violation at $.{key}", found[0])


class DescriptionCap(unittest.TestCase):
    def with_description(self, n: int) -> dict:
        meta = template()
        meta["description"] = "x" * n
        return meta

    def test_description_at_the_cap_passes(self):
        self.assertEqual(errors(self.with_description(GUIDELINE_DESCRIPTION_CAP)), [])

    def test_description_over_the_cap_fails(self):
        found = errors(self.with_description(GUIDELINE_DESCRIPTION_CAP + 1))
        self.assertEqual(len(found), 1, found)
        self.assertIn("151 characters", found[0])


class Runtime(unittest.TestCase):
    def with_runtime(self, runtime) -> dict:
        meta = template()
        second = copy.deepcopy(meta["versions"][0])
        second["version"] = "0.0.1"
        second.pop("runtime")
        if runtime is not None:
            second["runtime"] = runtime
        meta["versions"].append(second)
        return meta

    def test_missing_runtime_fails(self):
        # PCM assumes SWIG when `runtime` is absent.
        found = errors(self.with_runtime(None))
        self.assertEqual(len(found), 1, found)
        self.assertIn("version 0.0.1 declares no runtime", found[0])

    def test_swig_runtime_fails(self):
        found = errors(self.with_runtime("swig"))
        self.assertEqual(len(found), 1, found)
        self.assertIn("'swig'", found[0])


class IpcAction(unittest.TestCase):
    def test_actions_hide_their_toolbar_button(self):
        # The IPC action has no icon and runs a stdio server that exits on EOF,
        # so its button does nothing (Copperpour/Copperpour#277). Show it once
        # Copperpour/Copperpour#24 fixes both.
        plugin = plugin_manifest()
        self.assertTrue(plugin["actions"])
        for action in plugin["actions"]:
            self.assertIs(action.get("show-button"), False, action["identifier"])


def package_metadata(install_size) -> dict:
    meta = template()
    v = meta["versions"][0]
    meta["versions"] = [v]
    v["version"] = "0.0.0"
    v["platforms"] = ["linux"]
    for field in ("download_sha256", "download_url", "download_size"):
        v.pop(field, None)
    if install_size is None:
        v.pop("install_size", None)
    else:
        v["install_size"] = install_size
    return meta


class InstallSize(unittest.TestCase):
    """install_size must count every file in the zip, metadata.json included (#398)."""

    FILES = {
        "plugins/__init__.py": b"# launcher\n",
        "plugins/plugin.json": json.dumps({"actions": []}).encode(),
        "plugins/settings_dialog.py": b"# dialog\n",
        "plugins/native_bridge.py": b"# bridge\n",
        "plugins/resources/icon.png": b"\x89PNG" + b"\0" * 64,
        "resources/icon.png": b"\x89PNG" + b"\0" * 64,
        "plugins/bin/copperpour": b"\x7fELF\x02\x01\x01\x00",
    }

    def errors_for(self, size_of) -> list[str]:
        """Validate a zip whose install_size is `size_of(others, metadata_len)`."""
        others = sum(len(b) for b in self.FILES.values())
        # Settle the metadata's own length the way the build scripts do.
        size = others
        for _ in range(8):
            blob = json.dumps(package_metadata(size), indent=2).encode()
            if others + len(blob) == size:
                break
            size = others + len(blob)
        declared = size_of(others, len(blob))
        blob = json.dumps(package_metadata(declared), indent=2).encode()
        with tempfile.TemporaryDirectory() as d:
            path = Path(d) / "copperpour-pcm-v0.0.0-linux.zip"
            with zipfile.ZipFile(path, "w") as z:
                z.writestr("metadata.json", blob)
                for name, data in self.FILES.items():
                    info = zipfile.ZipInfo(name)
                    info.create_system = 3  # zip -X on Linux keeps the mode
                    info.external_attr = (0o100755 if "/bin/" in name else 0o100644) << 16
                    z.writestr(info, data)
            return validate_pcm.validate_zip(path)

    def test_total_with_metadata_passes(self):
        self.assertEqual(self.errors_for(lambda others, meta: others + meta), [])

    def test_total_without_metadata_fails(self):
        # What build-pcm.sh and build-pcm.ps1 wrote for v0.16.0.
        errs = self.errors_for(lambda others, meta: others)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("metadata.json included", errs[0])

    def test_missing_install_size_fails(self):
        errs = self.errors_for(lambda others, meta: None)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("declares no install_size", errs[0])


if __name__ == "__main__":
    unittest.main()
