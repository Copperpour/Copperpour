#!/usr/bin/env python3
"""Validate Copperpour's Claude Desktop MCP Bundle (.mcpb) packaging.

Modes (combinable):

  python packaging/validate-mcpb.py --manifest packaging/mcpb-manifest.json
      Validate a manifest.json template against the vendored MCPB v0.3 schema
      and Copperpour's packaging invariants.

  python packaging/validate-mcpb.py --bundle dist/copperpour-v0.16.1-linux-x86_64.mcpb
      Validate a .mcpb archive: ZIP structure, embedded manifest.json,
      declared platform vs. bundled executable magic bytes, POSIX exec bit,
      PNG icon, and launch configuration.

  python packaging/validate-mcpb.py --bundle <path> --expected-version 0.16.1 \
      --smoke-test [--check-binary-version]
      Also unpack the bundle on the matching host OS, launch the bundled binary
      using manifest.json's mcp_config, and run a stdio MCP smoke test covering
      initialize, tools/list, get_installation_info, and load_toolset.

Exits non-zero on any failure.
Requires: pip install jsonschema
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import subprocess
import sys
import tempfile
import threading
import zipfile
from pathlib import Path

import jsonschema

SCHEMA_PATH = Path(__file__).parent / "schema" / "mcpb-manifest-v0.3.schema.json"

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"
MAX_BUNDLE_SIZE = 90_000_000
ZIP_UNIX = 3

# Map MCPB compatibility.platforms values to executable magic and expected path.
PLATFORM_SPECS = {
    "win32": {
        "entry_point": "server/copperpour.exe",
        "posix": False,
        "magic": [b"MZ"],
    },
    "darwin": {
        "entry_point": "server/copperpour",
        "posix": True,
        "magic": [
            b"\xcf\xfa\xed\xfe",  # Mach-O 64-bit, little-endian
            b"\xce\xfa\xed\xfe",  # Mach-O 32-bit, little-endian
            b"\xca\xfe\xba\xbe",  # universal ("fat") binary
            b"\xbe\xba\xfe\xca",  # universal, byte-swapped
        ],
    },
    "linux": {
        "entry_point": "server/copperpour",
        "posix": True,
        "magic": [b"\x7fELF"],
    },
}

SEMVER_RE = re.compile(r"^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$")


def load_schema() -> dict:
    return json.loads(SCHEMA_PATH.read_text(encoding="utf-8"))


def identify_executable(blob: bytes) -> str | None:
    """Return the MCPB platform ('win32', 'darwin', 'linux') matching blob magic."""
    for platform, spec in PLATFORM_SPECS.items():
        if any(blob.startswith(m) for m in spec["magic"]):
            return platform
    return None


def stored_executable(info: zipfile.ZipInfo) -> bool:
    """Whether unzipping this entry on POSIX preserves an executable bit."""
    return info.create_system == ZIP_UNIX and bool((info.external_attr >> 16) & 0o111)


def validate_manifest(manifest: dict, label: str) -> list[str]:
    errors: list[str] = []
    try:
        jsonschema.validate(manifest, load_schema())
    except jsonschema.ValidationError as e:
        return [f"{label}: schema violation at {e.json_path}: {e.message}"]

    if manifest.get("manifest_version") != "0.3":
        errors.append(
            f"{label}: manifest_version must be '0.3' (got {manifest.get('manifest_version')!r})"
        )
    if manifest.get("name") != "copperpour":
        errors.append(f"{label}: name must be 'copperpour' (got {manifest.get('name')!r})")
    if not SEMVER_RE.match(manifest.get("version", "")):
        errors.append(f"{label}: version {manifest.get('version')!r} is not valid semver")
    if manifest.get("license") != "AGPL-3.0-only":
        errors.append(
            f"{label}: license must be 'AGPL-3.0-only' (got {manifest.get('license')!r})"
        )
    if manifest.get("tools_generated") is not True:
        errors.append(f"{label}: tools_generated must be true (Copperpour loads toolsets dynamically)")

    server = manifest["server"]
    if server.get("type") != "binary":
        errors.append(f"{label}: server.type must be 'binary' (got {server.get('type')!r})")

    compat = manifest.get("compatibility") or {}
    platforms = compat.get("platforms")
    if not platforms:
        errors.append(
            f"{label}: compatibility.platforms must declare the bundled binary's platform"
        )
    elif len(platforms) != 1 or platforms[0] not in PLATFORM_SPECS:
        errors.append(
            f"{label}: per-platform bundle must declare exactly one platform in "
            f"{sorted(PLATFORM_SPECS)} (got {platforms!r})"
        )
    else:
        platform = platforms[0]
        expected_entry = PLATFORM_SPECS[platform]["entry_point"]
        if server.get("entry_point") != expected_entry:
            errors.append(
                f"{label}: server.entry_point is {server.get('entry_point')!r}, "
                f"expected {expected_entry!r} for platform {platform!r}"
            )
        mcp_config = server.get("mcp_config", {})
        expected_cmd = f"${{__dirname}}/{expected_entry}"
        if mcp_config.get("command") != expected_cmd:
            errors.append(
                f"{label}: server.mcp_config.command is {mcp_config.get('command')!r}, "
                f"expected {expected_cmd!r}"
            )
        args = mcp_config.get("args", [])
        if args != []:
            errors.append(f"{label}: server.mcp_config.args must be empty (got {args!r})")

    if manifest.get("icon") != "icon.png":
        errors.append(f"{label}: icon must be 'icon.png' (got {manifest.get('icon')!r})")

    return errors


def validate_bundle(
    bundle_path: Path, expected_version: str | None = None
) -> tuple[list[str], dict | None]:
    errors: list[str] = []
    if not bundle_path.is_file():
        return [f"{bundle_path}: file not found"], None

    size = bundle_path.stat().st_size
    if size > MAX_BUNDLE_SIZE:
        errors.append(
            f"{bundle_path.name}: is {size} bytes, exceeding the {MAX_BUNDLE_SIZE}-byte limit"
        )

    try:
        z = zipfile.ZipFile(bundle_path)
    except zipfile.BadZipFile as e:
        return [f"{bundle_path.name}: invalid ZIP archive: {e}"], None

    with z:
        infos = [i for i in z.infolist() if not i.is_dir()]
        names = [i.filename for i in infos]
        if len(names) != len(set(names)):
            errors.append(f"{bundle_path.name}: archive contains duplicate entries")

        for name in names:
            parts = re.split(r"[\\/]", name)
            if (
                name.startswith("/")
                or name.startswith("\\")
                or re.match(r"^[A-Za-z]:", name)
                or ".." in parts
            ):
                errors.append(f"{bundle_path.name}: unsafe archive entry path {name!r}")

        by_name = {i.filename: i for i in infos}

        if "manifest.json" not in by_name:
            errors.append(f"{bundle_path.name}: missing required entry manifest.json")
            return errors, None

        try:
            manifest = json.loads(z.read("manifest.json").decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as e:
            return [f"{bundle_path.name}: manifest.json is not valid UTF-8 JSON: {e}"], None

        manifest_errors = validate_manifest(manifest, f"{bundle_path.name}:manifest.json")
        errors.extend(manifest_errors)

        if expected_version is not None and manifest.get("version") != expected_version:
            errors.append(
                f"{bundle_path.name}: manifest version is {manifest.get('version')!r}, "
                f"expected {expected_version!r}"
            )

        if "icon.png" not in by_name:
            errors.append(f"{bundle_path.name}: missing required entry icon.png")
        elif not z.read("icon.png").startswith(PNG_MAGIC):
            errors.append(f"{bundle_path.name}: icon.png is not a valid PNG image")

        if manifest_errors:
            return errors, manifest

        platform = manifest["compatibility"]["platforms"][0]
        spec = PLATFORM_SPECS[platform]
        entry_point = spec["entry_point"]

        expected_entries = {"manifest.json", "icon.png", entry_point}
        unexpected = sorted(set(by_name) - expected_entries)
        if unexpected:
            errors.append(f"{bundle_path.name}: unexpected archive entries {unexpected}")

        if entry_point not in by_name:
            errors.append(f"{bundle_path.name}: missing server entry_point {entry_point}")
        else:
            with z.open(entry_point) as f:
                actual = identify_executable(f.read(4))
            if actual != platform:
                found = "not a recognized executable" if actual is None else f"a {actual} executable"
                errors.append(
                    f"{bundle_path.name}: {entry_point} is {found}, contradicting declared "
                    f"platform {platform!r}"
                )
            elif spec["posix"] and not stored_executable(by_name[entry_point]):
                errors.append(
                    f"{bundle_path.name}: {entry_point} is not stored with Unix executable bits"
                )

    return errors, manifest


def _extract_bundle(bundle_path: Path, dest: Path) -> None:
    """Extract bundle preserving Unix executable permissions from ZIP metadata."""
    with zipfile.ZipFile(bundle_path) as z:
        for info in z.infolist():
            if info.is_dir():
                continue
            target = dest / info.filename
            target.parent.mkdir(parents=True, exist_ok=True)
            with z.open(info) as src, open(target, "wb") as dst:
                dst.write(src.read())
            if info.create_system == ZIP_UNIX:
                mode = (info.external_attr >> 16) & 0o777
                if mode:
                    os.chmod(target, mode)


SMOKE_TIMEOUT_SECS = 15


def _start_stdout_reader(stream) -> queue.Queue:
    q: queue.Queue = queue.Queue()

    def _reader() -> None:
        try:
            for line in stream:
                q.put(("line", line))
        finally:
            q.put(("eof", None))

    t = threading.Thread(target=_reader, daemon=True)
    t.start()
    return q


def _rpc_roundtrip(
    proc: subprocess.Popen,
    stdout_q: queue.Queue,
    stderr_path: Path,
    payload: dict,
    timeout: float = SMOKE_TIMEOUT_SECS,
) -> dict:
    assert proc.stdin is not None
    expected_id = payload["id"]
    proc.stdin.write(json.dumps(payload) + "\n")
    proc.stdin.flush()
    while True:
        try:
            kind, line = stdout_q.get(timeout=timeout)
        except queue.Empty as e:
            stderr = stderr_path.read_text(encoding="utf-8", errors="replace") if stderr_path.is_file() else ""
            raise TimeoutError(
                f"MCP server timed out after {timeout}s waiting for id={expected_id}. stderr:\n{stderr}"
            ) from e
        if kind == "eof":
            stderr = stderr_path.read_text(encoding="utf-8", errors="replace") if stderr_path.is_file() else ""
            raise RuntimeError(f"MCP server closed stdout unexpectedly. stderr:\n{stderr}")
        msg = json.loads(line)
        if msg.get("id") == expected_id:
            return msg


def smoke_test_bundle(
    bundle_path: Path, manifest: dict, check_binary_version: bool = False
) -> list[str]:
    """Unpack the bundle, launch its server via manifest.json, and test MCP stdio."""
    errors: list[str] = []
    with tempfile.TemporaryDirectory(prefix="copperpour-mcpb-smoke-") as td:
        root = Path(td)
        ext_dir = root / "ext"
        home_dir = root / "home"
        config_dir = root / "config"
        data_dir = root / "data"
        state_dir = root / "state"
        stderr_path = root / "server.stderr.log"
        for d in (ext_dir, home_dir, config_dir, data_dir, state_dir):
            d.mkdir()

        _extract_bundle(bundle_path, ext_dir)

        mcp_config = manifest["server"]["mcp_config"]
        cmd = os.path.normpath(mcp_config["command"].replace("${__dirname}", str(ext_dir)))
        args = [a.replace("${__dirname}", str(ext_dir)) for a in mcp_config.get("args", [])]

        env = os.environ.copy()
        env["HOME"] = str(home_dir)
        env["USERPROFILE"] = str(home_dir)
        env["APPDATA"] = str(config_dir)
        env["LOCALAPPDATA"] = str(data_dir)
        env["XDG_CONFIG_HOME"] = str(config_dir)
        env["XDG_DATA_HOME"] = str(data_dir)
        env["XDG_STATE_HOME"] = str(state_dir)
        env["COPPERPOUR_STATE_DIR"] = str(state_dir)
        env.pop("COPPERPOUR_CONFIG", None)
        env.pop("KICAD_API_SOCKET", None)

        # 1. Verify --version works and matches manifest when requested.
        ver = subprocess.run(
            [cmd, "--version"],
            capture_output=True,
            text=True,
            env=env,
            timeout=SMOKE_TIMEOUT_SECS,
        )
        if ver.returncode != 0:
            return [f"smoke-test: {cmd} --version exited {ver.returncode}: {ver.stderr.strip()}"]
        ver_out = ver.stdout.strip()
        if not ver_out.startswith("copperpour "):
            errors.append(f"smoke-test: unexpected --version output {ver_out!r}")
        else:
            binary_version = ver_out.split(None, 1)[1].strip()
            if check_binary_version and binary_version != manifest["version"]:
                errors.append(
                    f"smoke-test: bundled binary reports version {binary_version!r}, "
                    f"but manifest.json declares {manifest['version']!r}"
                )

        # 2. Launch over stdio using manifest.json's mcp_config and test MCP protocol.
        with open(stderr_path, "w", encoding="utf-8") as stderr_file:
            proc = subprocess.Popen(
                [cmd, *args],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr_file,
                text=True,
                env=env,
            )
            assert proc.stdout is not None
            stdout_q = _start_stdout_reader(proc.stdout)
            try:
                init = _rpc_roundtrip(
                    proc,
                    stdout_q,
                    stderr_path,
                    {
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "initialize",
                        "params": {
                            "protocolVersion": "2024-11-05",
                            "capabilities": {},
                            "clientInfo": {"name": "mcpb-smoke-test", "version": "0.1.0"},
                        },
                    },
                )
                init_result = init.get("result") or {}
                if not init_result.get("instructions"):
                    errors.append("smoke-test: initialize returned empty instructions")
                server_info = init_result.get("serverInfo") or {}
                if server_info.get("name") != "copperpour":
                    errors.append(
                        f"smoke-test: unexpected serverInfo.name {server_info.get('name')!r}"
                    )

                assert proc.stdin is not None
                proc.stdin.write(
                    json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"
                )
                proc.stdin.flush()

                listed = _rpc_roundtrip(
                    proc, stdout_q, stderr_path, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"}
                )
                starter_tools = {t["name"] for t in listed.get("result", {}).get("tools", [])}
                for required in (
                    "get_guidance",
                    "get_installation_info",
                    "list_toolboxes",
                    "load_toolset",
                ):
                    if required not in starter_tools:
                        errors.append(f"smoke-test: starter tools/list missing {required!r}")

                info_resp = _rpc_roundtrip(
                    proc,
                    stdout_q,
                    stderr_path,
                    {
                        "jsonrpc": "2.0",
                        "id": 3,
                        "method": "tools/call",
                        "params": {"name": "get_installation_info", "arguments": {}},
                    },
                )
                info_result = info_resp.get("result") or {}
                if info_result.get("isError"):
                    errors.append(f"smoke-test: get_installation_info failed: {info_result}")
                else:
                    text = "".join(c.get("text", "") for c in info_result.get("content", []))
                    parsed = json.loads(text)
                    exe_path = parsed.get("runtime", {}).get("executable_path", "")
                    if not exe_path or not Path(exe_path).resolve().is_relative_to(ext_dir.resolve()):
                        errors.append(
                            f"smoke-test: get_installation_info runtime.executable_path {exe_path!r} "
                            f"is not inside unpacked extension dir {str(ext_dir.resolve())!r}"
                        )

                load_resp = _rpc_roundtrip(
                    proc,
                    stdout_q,
                    stderr_path,
                    {
                        "jsonrpc": "2.0",
                        "id": 4,
                        "method": "tools/call",
                        "params": {"name": "load_toolset", "arguments": {"name": "sch_components"}},
                    },
                )
                load_result = load_resp.get("result") or {}
                if load_result.get("isError"):
                    errors.append(f"smoke-test: load_toolset(sch_components) failed: {load_result}")

                listed_after = _rpc_roundtrip(
                    proc, stdout_q, stderr_path, {"jsonrpc": "2.0", "id": 5, "method": "tools/list"}
                )
                loaded_tools = {t["name"] for t in listed_after.get("result", {}).get("tools", [])}
                if len(loaded_tools) <= len(starter_tools):
                    errors.append(
                        "smoke-test: tools/list did not grow after load_toolset(sch_components)"
                    )
            except (RuntimeError, TimeoutError, json.JSONDecodeError) as e:
                errors.append(f"smoke-test: {e}")
            finally:
                if proc.stdin:
                    proc.stdin.close()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
                    proc.wait(timeout=5)
                if proc.stdout:
                    proc.stdout.close()

    return errors


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Validate Copperpour MCPB manifest or .mcpb bundle.")
    ap.add_argument("--manifest", type=Path, help="manifest.json file to validate")
    ap.add_argument("--bundle", type=Path, help=".mcpb archive to validate")
    ap.add_argument("--expected-version", help="Assert manifest.json version equals this string")
    ap.add_argument(
        "--smoke-test",
        action="store_true",
        help="Unpack --bundle on matching host OS and run a stdio MCP smoke test",
    )
    ap.add_argument(
        "--check-binary-version",
        action="store_true",
        help="With --smoke-test, also assert binary --version equals manifest.json version",
    )
    args = ap.parse_args(argv)

    if not args.manifest and not args.bundle:
        ap.error("provide --manifest and/or --bundle")
    if args.smoke_test and not args.bundle:
        ap.error("--smoke-test requires --bundle")
    if args.check_binary_version and not args.smoke_test:
        ap.error("--check-binary-version requires --smoke-test")

    errors: list[str] = []
    if args.manifest:
        manifest = json.loads(args.manifest.read_text(encoding="utf-8"))
        errors.extend(validate_manifest(manifest, str(args.manifest)))

    bundle_manifest = None
    if args.bundle:
        bundle_errors, bundle_manifest = validate_bundle(args.bundle, args.expected_version)
        errors.extend(bundle_errors)
        if not bundle_errors and args.smoke_test and bundle_manifest is not None:
            errors.extend(
                smoke_test_bundle(
                    args.bundle,
                    bundle_manifest,
                    check_binary_version=args.check_binary_version,
                )
            )

    if errors:
        for e in errors:
            print(f"FAIL: {e}", file=sys.stderr)
        return 1

    checked = " and ".join(str(p) for p in (args.manifest, args.bundle) if p is not None)
    suffix = " (with stdio MCP smoke test)" if args.smoke_test else ""
    print(f"OK: {checked} passed MCPB validation{suffix}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
