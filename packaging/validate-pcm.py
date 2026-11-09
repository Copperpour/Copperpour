#!/usr/bin/env python3
"""Validate Copperpour's PCM packaging against KiCAD's addon schema.

Two modes, combinable:

  python packaging/validate-pcm.py --metadata packaging/metadata.json
      Validate a metadata.json file against the vendored packages.v2 schema
      and KiCad's addon guidelines.

  python packaging/validate-pcm.py --zip dist/copperpour-pcm-v0.1.2.zip
      Assert the PCM zip structure (metadata at root, plugin launcher,
      per-OS entrypoint binary, icons, install_size, download size) and
      validate the embedded metadata. A zip is either one platform's package
      (build-pcm.sh, build-pcm.ps1) or the multi-platform one (merge-pcm.py).

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


# The multi-platform package's binary directories, with pcm-launcher.sh at
# plugins/bin/copperpour (#398; layout in docs/KICAD_INTEGRATION.md).
MULTI_PLATFORM_DIRS = {
    "windows": "plugins/bin/",
    "linux": "plugins/bin-linux-x86_64/",
    "macos": "plugins/bin-macos/",
}
SERVER_NAMES = {"windows": "copperpour.exe", "linux": "copperpour", "macos": "copperpour"}
LAUNCHER = "plugins/bin/copperpour"

# KiCad's addon repository stops downloading a package at 100 MB and asks for a
# manual review. Fail with room to spare.
MAX_DOWNLOAD_SIZE = 90_000_000

# zipfile's create_system for an entry made on Unix, whose mode KiCad applies.
ZIP_UNIX = 3


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
    size = zip_path.stat().st_size
    if size > MAX_DOWNLOAD_SIZE:
        errors.append(
            f"{zip_path.name}: is {size} bytes; KiCad's addon repository stops "
            f"downloading at 100 MB, and this gate at {MAX_DOWNLOAD_SIZE}"
        )
    z = zipfile.ZipFile(zip_path)
    names = set(z.namelist())

    for entry in REQUIRED_ZIP_ENTRIES:
        if entry not in names:
            errors.append(f"{zip_path.name}: missing required entry {entry}")

    # plugin.json's entrypoint must name a file the zip carries (PR #7's
    # per-OS stamping contract); validate_platforms checks the binaries.
    if not any(n.startswith("plugins/bin/copperpour") for n in names):
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

        errors += validate_platforms(z, meta, zip_path.name)
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


def validate_platforms(z, meta: dict, label: str) -> list[str]:
    """Every version must declare exactly the platforms its binaries can run on.

    Omitting `platforms` means "all platforms" to KiCAD's PCM, which is never
    true of a package that bundles native binaries — that is how a
    Windows-only package came to be offered to macOS and Linux users.
    """
    errors = []
    for v in meta.get("versions", []):
        declared = v.get("platforms")
        if not declared:
            errors.append(
                f"{label}: version {v.get('version')} declares no 'platforms' — "
                f"PCM would offer this native package to every OS. Declare the "
                f"platforms its binaries are built for."
            )
            continue
        if len(declared) == 1:
            dirs = {"plugins/bin/": declared[0]}
        else:
            dirs = {prefix: p for p, prefix in MULTI_PLATFORM_DIRS.items()}
        errors += validate_binaries(z, dirs, set(declared), v.get("version"), label)
    return errors


def validate_binaries(z, dirs: dict, declared: set, version, label: str) -> list[str]:
    """Each declared platform has its server, and every binary runs where it sits.

    `dirs` maps each binary directory to the platform whose files it holds.
    POSIX files must be stored with their exec bit, which KiCad's PCM applies
    on install. A multi-platform package's plugins/bin/copperpour is the
    launcher, which must be the POSIX script.
    """
    errors = []
    infos = {i.filename: i for i in z.infolist() if not i.is_dir()}
    multi = len(declared) > 1
    for name in sorted(infos):
        in_bin = name.startswith(("plugins/bin/", "plugins/bin-"))
        if in_bin and not any(
            name.startswith(prefix) and "/" not in name[len(prefix):] for prefix in dirs
        ):
            errors.append(f"{label}: {name} is not in a binary directory version {version} uses")
    for prefix, platform in dirs.items():
        files = sorted(
            n for n in infos
            if n.startswith(prefix) and "/" not in n[len(prefix):]
            and not (multi and n == LAUNCHER)
        )
        if platform not in declared:
            for name in files:
                errors.append(
                    f"{label}: {name} is a {platform} file, but version {version} "
                    f"declares platforms {sorted(declared)}"
                )
            continue
        server = prefix + SERVER_NAMES[platform]
        if server not in infos:
            errors.append(f"{label}: version {version} declares {platform} but has no {server}")
        for name in files:
            with z.open(name) as f:
                actual = identify_executable(f.read(4))
            if actual != platform:
                found = "not a recognized executable" if actual is None else f"a {actual} executable"
                errors.append(f"{label}: {name} is {found}, but version {version} "
                              f"puts it with the {platform} binaries")
            elif platform != "windows" and not stored_executable(infos[name]):
                errors.append(f"{label}: {name} is not stored executable")

    if multi and declared & {"linux", "macos"}:
        # KiCad runs the entrypoint on every platform, so it must be the launcher.
        plugin = json.loads(z.read("plugins/plugin.json")) if "plugins/plugin.json" in infos else {}
        for action in plugin.get("actions", []):
            if action.get("entrypoint") != LAUNCHER.removeprefix("plugins/"):
                errors.append(
                    f"{label}: plugin.json entrypoint {action.get('entrypoint')!r} is not "
                    f"the launcher {LAUNCHER.removeprefix('plugins/')!r}"
                )
        launcher = infos.get(LAUNCHER)
        if launcher is None:
            errors.append(f"{label}: version {version} declares POSIX platforms but has no {LAUNCHER}")
        elif not z.read(LAUNCHER).startswith(b"#!/bin/sh\n"):
            errors.append(f"{label}: {LAUNCHER} is not the POSIX launcher")
        elif not stored_executable(launcher):
            errors.append(f"{label}: {LAUNCHER} is not stored executable")
    return errors


def stored_executable(info: zipfile.ZipInfo) -> bool:
    """Whether KiCad's PCM installs this entry with an exec bit."""
    return info.create_system == ZIP_UNIX and bool((info.external_attr >> 16) & 0o111)


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
