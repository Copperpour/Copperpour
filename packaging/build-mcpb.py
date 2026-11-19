#!/usr/bin/env python3
"""Assemble a Claude Desktop MCP Bundle (.mcpb) for Copperpour.

Produces a reproducible, self-contained .mcpb (ZIP archive) containing:

  icon.png                  Copperpour 64x64 icon
  manifest.json             MCPB v0.3 manifest (version and platform stamped)
  server/copperpour[.exe]   Native Copperpour MCP server binary

Usage:
  python packaging/build-mcpb.py [--version X.Y.Z] [--binary PATH]
                                 [--platform windows|macos|linux] [--out DIR]

Defaults: version from Cargo.toml, platform from the host OS, binary from
target/release/copperpour[.exe], output to dist/.
Prints the .mcpb path, byte size, and SHA256.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import sys
import tempfile
import zipfile
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parent.parent
TEMPLATE_PATH = REPO_ROOT / "packaging" / "mcpb-manifest.json"
ICON_PATH = REPO_ROOT / "packaging" / "resources" / "icon.png"

# Fixed ZIP timestamp so repeated builds of the same binary and version are
# byte-for-byte reproducible.
REPRODUCIBLE_ZIP_TIME = (2020, 1, 1, 0, 0, 0)
ZIP_UNIX = 3

PLATFORMS = {
    "windows": {
        "mcpb_platform": "win32",
        "artifact_suffix": "windows-x86_64",
        "binary_name": "copperpour.exe",
        "entry_point": "server/copperpour.exe",
        "exec_mode": 0o100755,
    },
    "macos": {
        "mcpb_platform": "darwin",
        "artifact_suffix": "macos-universal",
        "binary_name": "copperpour",
        "entry_point": "server/copperpour",
        "exec_mode": 0o100755,
    },
    "linux": {
        "mcpb_platform": "linux",
        "artifact_suffix": "linux-x86_64",
        "binary_name": "copperpour",
        "entry_point": "server/copperpour",
        "exec_mode": 0o100755,
    },
}

DATA_MODE = 0o100644


def default_version() -> str:
    cargo_toml = (REPO_ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'^version\s*=\s*"([^"]+)"', cargo_toml, re.MULTILINE)
    if not match:
        raise RuntimeError("Could not determine workspace version from Cargo.toml")
    return match.group(1)


def default_platform() -> str:
    if sys.platform.startswith("win"):
        return "windows"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform.startswith("linux"):
        return "linux"
    raise RuntimeError(f"Unsupported host platform {sys.platform!r}; pass --platform")


def render_manifest(version: str, platform: str) -> bytes:
    spec = PLATFORMS[platform]
    manifest = json.loads(TEMPLATE_PATH.read_text(encoding="utf-8"))
    manifest["version"] = version
    manifest["server"]["entry_point"] = spec["entry_point"]
    manifest["server"]["mcp_config"] = {
        "command": f"${{__dirname}}/{spec['entry_point']}",
        "args": [],
    }
    manifest["compatibility"]["platforms"] = [spec["mcpb_platform"]]
    return (json.dumps(manifest, indent=2) + "\n").encode("utf-8")


def write_zip_entry(z: zipfile.ZipFile, arcname: str, data: bytes, mode: int) -> None:
    info = zipfile.ZipInfo(filename=arcname, date_time=REPRODUCIBLE_ZIP_TIME)
    info.compress_type = zipfile.ZIP_DEFLATED
    info.create_system = ZIP_UNIX
    info.external_attr = mode << 16
    z.writestr(info, data)


def build_bundle(version: str, binary: Path, platform: str, out_dir: Path) -> Path:
    if platform not in PLATFORMS:
        raise ValueError(f"Unsupported platform {platform!r}; expected one of {sorted(PLATFORMS)}")
    if not binary.is_file():
        raise FileNotFoundError(
            f"Binary not found: {binary}. Build it first: cargo build --release -p copperpour"
        )
    if not ICON_PATH.is_file():
        raise FileNotFoundError(f"Icon not found: {ICON_PATH}")

    spec = PLATFORMS[platform]
    out_dir.mkdir(parents=True, exist_ok=True)
    out_path = out_dir / f"copperpour-v{version}-{spec['artifact_suffix']}.mcpb"

    manifest_bytes = render_manifest(version, platform)
    icon_bytes = ICON_PATH.read_bytes()
    binary_bytes = binary.read_bytes()

    fd, tmp_name = tempfile.mkstemp(prefix=".copperpour-mcpb-", suffix=".tmp", dir=out_dir)
    os.close(fd)
    tmp_path = Path(tmp_name)
    try:
        with zipfile.ZipFile(tmp_path, "w", compression=zipfile.ZIP_DEFLATED) as z:
            write_zip_entry(z, "icon.png", icon_bytes, DATA_MODE)
            write_zip_entry(z, "manifest.json", manifest_bytes, DATA_MODE)
            write_zip_entry(z, spec["entry_point"], binary_bytes, spec["exec_mode"])
        os.replace(tmp_path, out_path)
    except BaseException:
        tmp_path.unlink(missing_ok=True)
        raise

    return out_path


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Build a Copperpour Claude Desktop .mcpb bundle.")
    ap.add_argument("--version", help="Package version (defaults to workspace Cargo.toml version)")
    ap.add_argument("--binary", type=Path, help="Path to the compiled copperpour server binary")
    ap.add_argument(
        "--platform",
        choices=sorted(PLATFORMS),
        help="Target OS for the bundled binary (defaults to host OS)",
    )
    ap.add_argument("--out", type=Path, default=REPO_ROOT / "dist", help="Output directory")
    args = ap.parse_args(argv)

    version = (args.version or default_version()).lstrip("v")
    platform = args.platform or default_platform()
    binary = args.binary or (REPO_ROOT / "target" / "release" / PLATFORMS[platform]["binary_name"])

    bundle_path = build_bundle(version, binary, platform, args.out)
    digest = hashlib.sha256(bundle_path.read_bytes()).hexdigest()
    size = bundle_path.stat().st_size
    print(f"Built {bundle_path}")
    print(f"  size:   {size} bytes")
    print(f"  sha256: {digest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
