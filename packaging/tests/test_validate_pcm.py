"""Tests for packaging/validate-pcm.py's metadata checks.

Run: python -m unittest discover -s packaging/tests -v  (needs jsonschema)
"""

import copy
import importlib.util
import json
import unittest
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
        plugin = json.loads(
            (PACKAGING.parent / "plugin" / "plugin.json").read_text(encoding="utf-8")
        )
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


if __name__ == "__main__":
    unittest.main()
