#!/usr/bin/env python3
"""Write the metadata.json for a KiCAD addon repository submission.

  python packaging/pcm-submission-metadata.py --version X.Y.Z \
      --base-url https://github.com/Copperpour/Copperpour/releases/download/vX.Y.Z/ \
      --out dist/copperpour-pcm-vX.Y.Z-metadata.json dist/copperpour-pcm-vX.Y.Z.zip

KiCAD's official repository (gitlab.com/kicad/addons/metadata) lists a package
from a metadata.json whose versions carry download_sha256 and download_url. Those
values exist only once the release zips are built, and they must not appear in
the metadata inside a zip, so packaging/metadata.json stays a template and this
script writes the version entry from the built zip. The repository rejects two
entries with one version number, so a release submits the one zip merge-pcm.py
writes, whose entry covers every platform (#398).

The release workflow attaches the result to the GitHub release. A submission
copies its `versions` entries into packages/<identifier>/metadata.json there.

Requires: pip install jsonschema
"""

import argparse
import hashlib
import importlib.util
import json
import sys
import zipfile
from pathlib import Path


_spec = importlib.util.spec_from_file_location(
    "validate_pcm", Path(__file__).parent / "validate-pcm.py"
)
validate_pcm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(validate_pcm)

PLATFORMS = sorted(validate_pcm.MULTI_PLATFORM_DIRS)


def version_entry(zip_path: Path, template: dict, version: str, base_url: str) -> dict:
    """One version entry for a built zip, from the metadata the zip carries."""
    with zipfile.ZipFile(zip_path) as z:
        inner = json.loads(z.read("metadata.json"))
    if inner.get("identifier") != template["identifier"]:
        raise ValueError(
            f"{zip_path.name}: identifier {inner.get('identifier')!r} is not "
            f"{template['identifier']!r}"
        )
    entry = dict(inner["versions"][0])
    if entry.get("version") != version:
        raise ValueError(f"{zip_path.name}: version {entry.get('version')!r} is not {version!r}")

    data = zip_path.read_bytes()
    entry["download_sha256"] = hashlib.sha256(data).hexdigest()
    entry["download_size"] = len(data)
    entry["download_url"] = base_url + zip_path.name
    return entry


def build(template: dict, zips: list[Path], version: str, base_url: str) -> dict:
    if len(zips) != 1:
        raise ValueError(
            f"got {len(zips)} zips; KiCad's addon repository requires unique "
            f"version numbers, so submit the one zip merge-pcm.py writes"
        )
    entry = version_entry(zips[0], template, version, base_url)
    if sorted(entry.get("platforms", [])) != PLATFORMS:
        raise ValueError(
            f"{zips[0].name} declares platforms {entry.get('platforms')}; a "
            f"submission needs {PLATFORMS}, the zip merge-pcm.py writes"
        )
    meta = dict(template)
    # The repository also checks against the v1 schema, whose license list has
    # no AGPL. "open-source" is its documented fallback for an OSI-approved
    # license; the zip's own metadata keeps the real one.
    meta["license"] = "open-source"
    meta["versions"] = [entry]
    return meta


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--template", type=Path,
                    default=Path(__file__).parent / "metadata.json")
    ap.add_argument("--version", required=True, help="package version, without a v")
    ap.add_argument("--base-url", required=True,
                    help="release download URL the zip names are appended to")
    ap.add_argument("--out", type=Path, required=True)
    ap.add_argument("zips", type=Path, nargs="+")
    args = ap.parse_args()

    base_url = args.base_url if args.base_url.endswith("/") else args.base_url + "/"
    errors: list[str] = []
    for z in args.zips:
        if z.is_file():
            errors += validate_pcm.validate_zip(z)
        else:
            errors.append(f"{z}: no such file")
    if not errors:
        try:
            meta = build(json.loads(args.template.read_text(encoding="utf-8")),
                         args.zips, args.version, base_url)
        except ValueError as e:
            errors.append(str(e))
        else:
            errors += validate_pcm.validate_metadata(meta, str(args.out))
    if errors:
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(meta, indent=2) + "\n", encoding="utf-8")
    print(f"OK: wrote {args.out} for {args.zips[0].name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
