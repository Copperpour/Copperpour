#!/usr/bin/env python3
"""Validate Copperpour's PCM packaging against KiCAD's addon schema.

Two modes, combinable:

  python packaging/validate-pcm.py --metadata packaging/metadata.json
      Validate a metadata.json file against the vendored packages.v2 schema
      and KiCad's addon guidelines.

  python packaging/validate-pcm.py --zip dist/copperpour-pcm-v0.1.2.zip
      Assert the PCM zip structure (metadata at root, plugin launcher,
      per-OS entrypoint binary, icons, install_size) and validate the embedded
      metadata.

Exits non-zero on any failure. This is the gate that would have caught the
v0.1.1 install failures (#4/#8: missing author.contact) and the schema-invalid
license/sha fields found while building it.

Requires: pip install jsonschema
"""

import argparse
import json
import sys
import zipfile
from pathlib import Path

import jsonschema

SCHEMA_PATH = Path(__file__).parent / "schema" / "packages.v2.schema.json"

# https://dev-docs.kicad.org/en/addons/ caps `description` at 150 characters;
# the schema allows 500, so only this check enforces it.
MAX_DESCRIPTION = 150

REQUIRED_ZIP_ENTRIES = [
    "metadata.json",
    "plugins/__init__.py",
    "plugins/plugin.json",
    "plugins/settings_dialog.py",
    "plugins/native_bridge.py",
    "plugins/resources/icon.png",
    "resources/icon.png",
]

# Executable formats, by the magic bytes each platform's loader requires. A
# package bundles a native binary, so its declared platform is checkable rather
# than a promise: a Windows PE in a package declaring "macos" is unrunnable.
EXECUTABLE_MAGIC = {
    "windows": [b"MZ"],
    "linux": [b"\x7fELF"],
    "macos": [
        b"\xcf\xfa\xed\xfe",  # Mach-O 64-bit, little-endian
        b"\xce\xfa\xed\xfe",  # Mach-O 32-bit, little-endian
        b"\xca\xfe\xba\xbe",  # universal ("fat") binary
        b"\xbe\xba\xfe\xca",  # universal, byte-swapped
    ],
}


def identify_executable(blob: bytes) -> str | None:
    """Name the platform whose loader can run `blob`, or None if unrecognized."""
    for platform, magics in EXECUTABLE_MAGIC.items():
        if any(blob.startswith(m) for m in magics):
            return platform
    return None


def load_schema():
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


def validate_metadata(meta: dict, label: str) -> list[str]:
    errors = []
    try:
        jsonschema.validate(meta, load_schema())
    except jsonschema.ValidationError as e:
        # The checks below assume the schema's shapes.
        return [f"{label}: schema violation at {e.json_path}: {e.message}"]
    description = meta["description"]
    if len(description) > MAX_DESCRIPTION:
        errors.append(
            f"{label}: description is {len(description)} characters; KiCad's "
            f"addon guidelines allow {MAX_DESCRIPTION}"
        )
    # Without `runtime`, PCM assumes SWIG, and KiCad 11 marks SWIG plugins
    # incompatible. The IPC action needs the API server, which `ipc` prompts for.
    for v in meta.get("versions", []):
        runtime = v.get("runtime")
        if runtime != "ipc":
            declared = "no runtime" if runtime is None else f"runtime {runtime!r}"
            errors.append(
                f"{label}: version {v['version']} declares {declared}, not 'ipc'"
            )
    return errors


def validate_zip(zip_path: Path) -> list[str]:
    errors = []
    z = zipfile.ZipFile(zip_path)
    names = set(z.namelist())

    for entry in REQUIRED_ZIP_ENTRIES:
        if entry not in names:
            errors.append(f"{zip_path.name}: missing required entry {entry}")

    # Exactly one server binary must be present, and plugin.json's entrypoint
    # must point at it (PR #7's per-OS stamping contract).
    binaries = [n for n in names if n.startswith("plugins/bin/copperpour")]
    if not binaries:
        errors.append(f"{zip_path.name}: no plugins/bin/copperpour* binary found")

    if "plugins/plugin.json" in names:
        plugin = json.loads(z.read("plugins/plugin.json"))
        for action in plugin.get("actions", []):
            ep = action.get("entrypoint", "")
            if ep.startswith("bin/") and f"plugins/{ep}" not in names:
                errors.append(
                    f"{zip_path.name}: plugin.json entrypoint '{ep}' "
                    f"not present in the zip"
                )

    if "metadata.json" in names:
        meta = json.loads(z.read("metadata.json"))
        errors += validate_metadata(meta, f"{zip_path.name}:metadata.json")
        # Empty-string download fields pass nothing; they must be real or absent.
        for v in meta.get("versions", []):
            for field in ("download_sha256", "download_url"):
                if v.get(field) == "":
                    errors.append(
                        f"{zip_path.name}: {field} is an empty string — "
                        f"omit the field or provide a real value"
                    )

        errors += validate_platforms(z, meta, binaries, zip_path.name)
        errors += validate_install_size(z, meta, zip_path.name)

    return errors


def validate_install_size(z, meta: dict, label: str) -> list[str]:
    """Every version must state the unzipped size of every file, its own included.

    KiCad's addon repository sums the files in the zip and rejects a package
    whose install_size is more than 1024 bytes off. v0.16.0 was rejected that
    way: the build scripts summed the files before writing metadata.json (#398).
    The build writes the exact total, so anything else is a build fault.
    """
    actual = sum(info.file_size for info in z.infolist() if not info.is_dir())
    errors = []
    for v in meta.get("versions", []):
        declared = v.get("install_size")
        if declared is None:
            errors.append(
                f"{label}: version {v.get('version')} declares no install_size; "
                f"the files in the zip total {actual} bytes"
            )
        elif declared != actual:
            errors.append(
                f"{label}: version {v.get('version')} declares install_size "
                f"{declared}, but the files in the zip, metadata.json included, "
                f"total {actual} bytes"
            )
    return errors


def validate_platforms(z, meta: dict, binaries: list[str], label: str) -> list[str]:
    """Every version must declare exactly the platform its binaries can run on.

    Omitting `platforms` means "all platforms" to KiCAD's PCM, which is never
    true of a package that bundles one native binary — that is how a
    Windows-only package came to be offered to macOS and Linux users.
    """
    errors = []
    for v in meta.get("versions", []):
        declared = v.get("platforms")
        if not declared:
            errors.append(
                f"{label}: version {v.get('version')} declares no 'platforms' — "
                f"PCM would offer this native package to every OS. Declare the "
                f"one platform its binaries are built for."
            )
            continue
        if len(declared) != 1:
            errors.append(
                f"{label}: version {v.get('version')} declares platforms "
                f"{declared}; a package bundles one platform's binaries"
            )
            continue

        # The declaration must match what is actually in the zip.
        for name in binaries:
            actual = identify_executable(z.read(name)[:4])
            if actual is None:
                errors.append(f"{label}: {name} is not a recognized executable")
            elif actual != declared[0]:
                errors.append(
                    f"{label}: declares platforms {declared} but {name} is a "
                    f"{actual} executable"
                )
    return errors


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--metadata", type=Path, help="metadata.json to validate")
    ap.add_argument("--zip", type=Path, help="PCM zip to validate")
    args = ap.parse_args()

    if not args.metadata and not args.zip:
        ap.error("provide --metadata and/or --zip")

    errors: list[str] = []
    if args.metadata:
        meta = json.loads(args.metadata.read_text(encoding="utf-8"))
        errors += validate_metadata(meta, str(args.metadata))
    if args.zip:
        errors += validate_zip(args.zip)

    if errors:
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1

    checked = " and ".join(
        str(p) for p in (args.metadata, args.zip) if p is not None
    )
    print(f"OK: {checked} passed PCM validation")
    return 0


if __name__ == "__main__":
    sys.exit(main())
