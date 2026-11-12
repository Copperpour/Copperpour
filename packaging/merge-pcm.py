#!/usr/bin/env python3
"""Merge the three platform PCM zips into the one KiCad's addon repository lists.

  python packaging/merge-pcm.py --version X.Y.Z --out dist \\
      dist/copperpour-pcm-vX.Y.Z-{linux,macos,windows}.zip

KiCad's addon repository requires every version of a package to have its own
version number, so a release cannot list one zip per platform (#398). Each
platform's zip is still built on its own machine; this script combines them
into copperpour-pcm-v<version>.zip, one version entry for all three platforms:

  plugins/bin/copperpour.exe              Windows server (browser viewer embedded)
  plugins/bin/copperpour                  pcm-launcher.sh, runs the one below
  plugins/bin-linux-x86_64/copperpour     Linux server (browser viewer embedded)
  plugins/bin-macos/copperpour            macOS universal server (browser viewer embedded)

Everything else must be the same in all three zips, line endings aside (a
Windows checkout may carry CRLF); plugin.json may differ only in the
entrypoint. Both come from the Linux zip, whose entrypoint is
bin/copperpour, the launcher. docs/KICAD_INTEGRATION.md explains the layout.

Requires: pip install jsonschema
"""

import argparse
import copy
import hashlib
import importlib.util
import json
import sys
import zipfile
from pathlib import Path

PACKAGING = Path(__file__).resolve().parent

_spec = importlib.util.spec_from_file_location(
    "validate_pcm", PACKAGING / "validate-pcm.py"
)
validate_pcm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(validate_pcm)

PLATFORMS = tuple(sorted(validate_pcm.MULTI_PLATFORM_DIRS))
BIN = "plugins/bin/"
# Fixed, so a merge of the same inputs by the same Python and zlib gives the
# same zip. Another zlib may compress differently: hash the zip you publish.
DATE_TIME = (1980, 1, 1, 0, 0, 0)


class MergeError(Exception):
    pass


def read_zip(path: Path) -> tuple[str, dict, dict[str, bytes]]:
    """(platform, metadata, every other file) of one platform's zip."""
    with zipfile.ZipFile(path) as z:
        files = {i.filename: z.read(i) for i in z.infolist() if not i.is_dir()}
    meta = json.loads(files.pop("metadata.json"))
    platforms = meta["versions"][0].get("platforms", [])
    if len(meta["versions"]) != 1 or len(platforms) != 1:
        raise MergeError(f"{path.name}: is not one platform's package")
    return platforms[0], meta, files


def comparable(meta: dict) -> dict:
    """The metadata with what differs between platform zips taken out."""
    meta = copy.deepcopy(meta)
    for field in ("platforms", "install_size"):
        meta["versions"][0].pop(field, None)
    return meta


def shared_files(files: dict[str, bytes]) -> dict:
    """What every platform zip must agree on: all but the binaries, with
    plugin.json parsed and its stamped entrypoints left out."""
    shared = {
        n: b.replace(b"\r\n", b"\n") for n, b in files.items() if not n.startswith(BIN)
    }
    plugin = json.loads(shared.pop("plugins/plugin.json"))
    for action in plugin.get("actions", []):
        action.pop("entrypoint", None)
    shared["plugins/plugin.json"] = plugin
    return shared


def merge(zips: list[Path], version: str) -> tuple[dict, dict[str, tuple[bytes, bool]]]:
    """The merged metadata and files, each file with whether it is executable."""
    packages = {}
    for path in zips:
        platform, meta, files = read_zip(path)
        if platform in packages:
            raise MergeError(f"{path.name}: a second {platform} package")
        if meta["versions"][0].get("version") != version:
            raise MergeError(
                f"{path.name}: version {meta['versions'][0].get('version')!r} is not {version!r}"
            )
        packages[platform] = (path, meta, files)
    if sorted(packages) != sorted(PLATFORMS):
        raise MergeError(
            f"got packages for {sorted(packages)}; the merge needs one each for "
            f"{list(PLATFORMS)}"
        )

    linux_path, meta, linux_files = packages["linux"]
    ours = shared_files(linux_files)
    for platform in PLATFORMS:
        if platform == "linux":
            continue
        path, other_meta, files = packages[platform]
        if comparable(other_meta) != comparable(meta):
            raise MergeError(f"{path.name}: metadata differs from {linux_path.name}")
        theirs = shared_files(files)
        if theirs != ours:
            differ = sorted(n for n in ours.keys() | theirs.keys() if ours.get(n) != theirs.get(n))
            raise MergeError(f"{path.name}: differs from {linux_path.name} in {differ}")

    out = {n: (b, False) for n, b in linux_files.items() if not n.startswith(BIN)}
    for platform in PLATFORMS:
        prefix = validate_pcm.MULTI_PLATFORM_DIRS[platform]
        for name, data in packages[platform][2].items():
            if name.startswith(BIN):
                out[prefix + name[len(BIN):]] = (data, platform != "windows")
    out[validate_pcm.LAUNCHER] = ((PACKAGING / "pcm-launcher.sh").read_bytes(), True)

    meta = copy.deepcopy(meta)
    meta["versions"][0]["platforms"] = list(PLATFORMS)
    return meta, out


def metadata_blob(meta: dict, files: dict[str, tuple[bytes, bool]]) -> bytes:
    """metadata.json, its install_size the unzipped total, its own size included."""
    others = sum(len(data) for data, _ in files.values())
    total = others
    for _ in range(8):
        meta["versions"][0]["install_size"] = total
        blob = (json.dumps(meta, indent=2) + "\n").encode()
        if others + len(blob) == total:
            return blob
        total = others + len(blob)
    raise MergeError("install_size did not settle")


def write_zip(path: Path, blob: bytes, files: dict[str, tuple[bytes, bool]]) -> None:
    def entry(name: str, executable: bool) -> zipfile.ZipInfo:
        info = zipfile.ZipInfo(name, DATE_TIME)
        info.create_system = validate_pcm.ZIP_UNIX
        info.external_attr = (0o100755 if executable else 0o100644) << 16
        info.compress_type = zipfile.ZIP_DEFLATED
        return info

    with zipfile.ZipFile(path, "w") as z:
        z.writestr(entry("metadata.json", False), blob, compresslevel=9)
        for name in sorted(files):
            data, executable = files[name]
            z.writestr(entry(name, executable), data, compresslevel=9)


def fail(errors: list[str]) -> int:
    for e in errors:
        print(f"FAIL: {e}", file=sys.stderr)
    return 1


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--version", required=True, help="package version, without a v")
    ap.add_argument("--out", type=Path, required=True, help="directory for the merged zip")
    ap.add_argument("zips", type=Path, nargs="+")
    args = ap.parse_args()

    errors: list[str] = []
    for z in args.zips:
        errors += validate_pcm.validate_zip(z) if z.is_file() else [f"{z}: no such file"]
    if errors:
        return fail(errors)
    try:
        meta, files = merge(args.zips, args.version)
        blob = metadata_blob(meta, files)
    except MergeError as e:
        return fail([str(e)])

    args.out.mkdir(parents=True, exist_ok=True)
    path = args.out / f"copperpour-pcm-v{args.version}.zip"
    partial = path.with_name(path.name + ".partial")
    write_zip(partial, blob, files)
    errors = validate_pcm.validate_zip(partial)
    if errors:
        partial.unlink()
        return fail(errors)
    partial.replace(path)

    with path.open("rb") as f:
        sha = hashlib.file_digest(f, "sha256").hexdigest()
    print(f"PCM package: {path}")
    print(f"  download_size:   {path.stat().st_size}")
    print(f"  install_size:    {meta['versions'][0]['install_size']}")
    print(f"  download_sha256: {sha}")
    print(f"  platforms:       {list(PLATFORMS)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
