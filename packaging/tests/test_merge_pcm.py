"""Tests for the multi-platform PCM package (#398): merge-pcm.py, the checks
validate-pcm.py applies to it, its submission metadata, and pcm-launcher.sh.

Run: python -m unittest discover -s packaging/tests -v  (needs jsonschema; the
launcher tests need a POSIX sh)
"""

import importlib.util
import io
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import unittest
import unittest.mock
import zipfile
from pathlib import Path

PACKAGING = Path(__file__).resolve().parent.parent
REPO = PACKAGING.parent


def load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, PACKAGING / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


validate_pcm = load("validate_pcm", "validate-pcm.py")
merge_pcm = load("merge_pcm", "merge-pcm.py")
submission = load("pcm_submission_metadata", "pcm-submission-metadata.py")

MAGIC = {
    "linux": b"\x7fELF\x02\x01\x01\x00",
    "macos": b"\xcf\xfa\xed\xfe",
    "windows": b"MZ\x90\x00",
}
EXE = {"linux": "", "macos": "", "windows": ".exe"}

# The layout #398 settled on, restated rather than read from merge-pcm.py.
MERGED_BINARIES = {
    "plugins/bin/copperpour.exe": "windows",
    "plugins/bin/schematic-viewer.exe": "windows",
    "plugins/bin-linux-x86_64/copperpour": "linux",
    "plugins/bin-linux-x86_64/schematic-viewer": "linux",
    "plugins/bin-macos/copperpour": "macos",
    "plugins/bin-macos/schematic-viewer": "macos",
}


def settle(meta: dict, others: int) -> bytes:
    total = others
    for _ in range(8):
        meta["versions"][0]["install_size"] = total
        blob = (json.dumps(meta, indent=2) + "\n").encode()
        if others + len(blob) == total:
            return blob
        total = others + len(blob)
    raise AssertionError("install_size did not settle")


def platform_zip(directory: Path, platform: str, version: str = "0.0.0",
                 change=None) -> Path:
    """A legacy zip including the standalone viewer; old releases remain supported."""
    files = {
        f"plugins/{name}": (REPO / "plugin" / name).read_bytes()
        for name in ("__init__.py", "settings_dialog.py", "native_bridge.py")
    }
    icon = (PACKAGING / "resources" / "icon.png").read_bytes()
    files["plugins/resources/icon.png"] = icon
    files["resources/icon.png"] = icon
    plugin = json.loads((REPO / "plugin" / "plugin.json").read_text(encoding="utf-8"))
    for action in plugin["actions"]:
        action["entrypoint"] = f"bin/copperpour{EXE[platform]}"
    files["plugins/plugin.json"] = json.dumps(plugin, indent=2).encode()
    for name in ("copperpour", "schematic-viewer"):
        files[f"plugins/bin/{name}{EXE[platform]}"] = MAGIC[platform] + name.encode()

    meta = json.loads((PACKAGING / "metadata.json").read_text(encoding="utf-8"))
    meta["versions"] = [meta["versions"][0]]
    meta["versions"][0]["version"] = version
    meta["versions"][0]["platforms"] = [platform]
    if change:
        change(meta, files)
    blob = settle(meta, sum(len(b) for b in files.values()))

    path = directory / f"copperpour-pcm-v{version}-{platform}.zip"
    with zipfile.ZipFile(path, "w") as z:
        z.writestr("metadata.json", blob)
        for name, data in files.items():
            info = zipfile.ZipInfo(name)
            if platform == "windows":
                info.create_system = 0  # Compress-Archive records no Unix mode
            else:
                info.create_system = 3  # zip on Linux and macOS keeps it
                info.external_attr = (0o100755 if "/bin/" in name else 0o100644) << 16
            z.writestr(info, data)
    return path


def run_merge(directory: Path, zips: list[Path], version: str = "0.0.0"):
    out = directory / "out"
    result = subprocess.run(
        [sys.executable, str(PACKAGING / "merge-pcm.py"), "--version", version,
         "--out", str(out), *map(str, zips)],
        capture_output=True, text=True,
    )
    return result, out / f"copperpour-pcm-v{version}.zip"


def rewrite(src: Path, dst: Path, edit) -> Path:
    """Copy a zip, letting `edit(name, info, data)` change or drop (None) entries."""
    with zipfile.ZipFile(src) as zin, zipfile.ZipFile(dst, "w") as zout:
        for info in zin.infolist():
            changed = edit(info.filename, info, zin.read(info))
            if changed is not None:
                zout.writestr(*changed)
    return dst


class Merge(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.zips = [platform_zip(self.tmp, p) for p in ("linux", "macos", "windows")]

    def merged(self) -> Path:
        result, path = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 0, result.stderr)
        return path

    def test_merged_zip_lays_out_every_platform(self):
        path = self.merged()
        self.assertEqual(validate_pcm.validate_zip(path), [])
        with zipfile.ZipFile(path) as z:
            infos = {i.filename: i for i in z.infolist()}
            meta = json.loads(z.read("metadata.json"))
            for name, platform in MERGED_BINARIES.items():
                self.assertTrue(z.read(name).startswith(MAGIC[platform]), name)
            self.assertEqual(
                z.read("plugins/bin/copperpour"),
                (PACKAGING / "pcm-launcher.sh").read_bytes(),
            )
            entry = json.loads(z.read("plugins/plugin.json"))["actions"][0]["entrypoint"]
            unzipped = sum(i.file_size for i in infos.values())
        self.assertEqual(entry, "bin/copperpour")
        self.assertEqual(sorted(meta["versions"][0]["platforms"]), ["linux", "macos", "windows"])
        self.assertEqual(len(meta["versions"]), 1)
        self.assertEqual(meta["versions"][0]["install_size"], unzipped)
        # KiCad's PCM applies the stored mode's exec bits on Linux and macOS.
        for name in ("plugins/bin/copperpour", "plugins/bin-linux-x86_64/copperpour",
                     "plugins/bin-macos/copperpour"):
            self.assertEqual(infos[name].create_system, 3, name)
            self.assertEqual((infos[name].external_attr >> 16) & 0o777, 0o755, name)
        self.assertNotIn("plugins/bin/schematic-viewer", infos)

    def test_merge_is_reproducible(self):
        first = self.merged().read_bytes()
        self.assertEqual(self.merged().read_bytes(), first)

    def test_missing_platform_is_refused(self):
        result, path = run_merge(self.tmp, self.zips[:2])
        self.assertEqual(result.returncode, 1)
        self.assertIn("one each for", result.stderr)
        self.assertFalse(path.exists())

    def test_second_package_for_a_platform_is_refused(self):
        other = self.tmp / "again"
        other.mkdir()
        result, _ = run_merge(self.tmp, [*self.zips, platform_zip(other, "linux")])
        self.assertEqual(result.returncode, 1)
        self.assertIn("a second linux package", result.stderr)

    def test_other_version_is_refused(self):
        result, _ = run_merge(self.tmp, self.zips, version="0.0.1")
        self.assertEqual(result.returncode, 1)
        self.assertIn("is not '0.0.1'", result.stderr)

    def test_differing_plugin_file_is_refused(self):
        def change(meta, files):
            files["plugins/__init__.py"] += b"# built from another commit\n"
        self.zips[2] = platform_zip(self.tmp, "windows", change=change)
        result, _ = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 1)
        self.assertIn("plugins/__init__.py", result.stderr)

    def test_windows_checkout_line_endings_merge(self):
        # A Windows checkout under core.autocrlf gives the .py files CRLF.
        def change(meta, files):
            for name in ("plugins/__init__.py", "plugins/settings_dialog.py"):
                files[name] = files[name].replace(b"\n", b"\r\n")
        self.zips[2] = platform_zip(self.tmp, "windows", change=change)
        path = self.merged()
        with zipfile.ZipFile(path) as z:
            self.assertNotIn(b"\r\n", z.read("plugins/__init__.py"))

    def test_differing_metadata_is_refused(self):
        def change(meta, files):
            meta["versions"][0]["kicad_version_max"] = "10.0"
        self.zips[1] = platform_zip(self.tmp, "macos", change=change)
        result, _ = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 1)
        self.assertIn("metadata differs", result.stderr)

    def test_input_with_wrong_install_size_is_refused(self):
        # The merge recomputes install_size, so only the input gate sees this.
        path = self.zips[1]
        def edit(name, info, data):
            if name == "metadata.json":
                meta = json.loads(data)
                meta["versions"][0]["install_size"] -= 1475  # v0.16.0's shortfall
                return info, json.dumps(meta, indent=2).encode()
            return info, data
        self.zips[1] = rewrite(path, self.tmp / "short-macos.zip", edit)
        result, merged = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 1)
        self.assertIn("short-macos.zip: version 0.0.0 declares install_size", result.stderr)
        self.assertFalse(merged.exists())

    def test_merged_zip_over_the_cap_is_not_written(self):
        # Each input is under the cap, the merged zip is not: only the output
        # gate sees this. Incompressible servers make the merged zip the largest.
        def change(meta, files):
            name = next(n for n in files if n.startswith("plugins/bin/copperpour"))
            files[name] += random.Random(name + meta["versions"][0]["platforms"][0]).randbytes(65536)
        self.zips = [platform_zip(self.tmp, p, change=change) for p in ("linux", "macos", "windows")]
        cap = max(z.stat().st_size for z in self.zips) + 1
        saved = merge_pcm.validate_pcm.MAX_DOWNLOAD_SIZE
        merge_pcm.validate_pcm.MAX_DOWNLOAD_SIZE = cap
        self.addCleanup(setattr, merge_pcm.validate_pcm, "MAX_DOWNLOAD_SIZE", saved)
        out = self.tmp / "out"
        argv = ["merge-pcm.py", "--version", "0.0.0", "--out", str(out), *map(str, self.zips)]
        with unittest.mock.patch.object(sys, "argv", argv), \
                unittest.mock.patch("sys.stderr", new_callable=io.StringIO) as err:
            self.assertEqual(merge_pcm.main(), 1)
        self.assertIn("bytes; KiCad's addon repository stops downloading", err.getvalue())
        self.assertEqual(list(out.iterdir()), [])

    def test_invalid_input_zip_is_refused(self):
        # A Windows binary in the Linux package: the input gate runs first.
        def change(meta, files):
            files["plugins/bin/copperpour"] = MAGIC["windows"]
        self.zips[0] = platform_zip(self.tmp, "linux", change=change)
        result, _ = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 1)
        self.assertIn("is a windows executable", result.stderr)


class SinglePlatformValidation(unittest.TestCase):
    """validate-pcm.py on a legacy single-platform zip, including its standalone viewer."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)

    def test_each_platform_passes(self):
        for platform in ("linux", "macos", "windows"):
            path = platform_zip(self.tmp, platform)
            self.assertEqual(validate_pcm.validate_zip(path), [], platform)

    def test_wrong_platform_viewer_fails(self):
        def change(meta, files):
            files["plugins/bin/schematic-viewer"] = MAGIC["windows"]
        errs = validate_pcm.validate_zip(platform_zip(self.tmp, "linux", change=change))
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("plugins/bin/schematic-viewer is a windows executable", errs[0])

    def test_server_without_exec_bit_fails(self):
        path = platform_zip(self.tmp, "macos")
        def edit(name, info, data):
            if name == "plugins/bin/copperpour":
                info.external_attr = 0o100644 << 16
            return info, data
        errs = validate_pcm.validate_zip(rewrite(path, self.tmp / "edited.zip", edit))
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("plugins/bin/copperpour is not stored executable", errs[0])

    def test_other_platforms_directory_fails(self):
        def change(meta, files):
            files["plugins/bin-macos/copperpour"] = MAGIC["macos"]
        errs = validate_pcm.validate_zip(platform_zip(self.tmp, "linux", change=change))
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("plugins/bin-macos/copperpour is not in a binary directory", errs[0])

    def test_missing_server_fails(self):
        def change(meta, files):
            del files["plugins/bin/copperpour.exe"]
            plugin = json.loads(files["plugins/plugin.json"])
            plugin["actions"][0]["entrypoint"] = "bin/schematic-viewer.exe"
            files["plugins/plugin.json"] = json.dumps(plugin).encode()
        errs = validate_pcm.validate_zip(platform_zip(self.tmp, "windows", change=change))
        self.assertTrue(
            any("declares windows but has no plugins/bin/copperpour.exe" in e for e in errs), errs)


class MultiPlatformValidation(unittest.TestCase):
    """validate-pcm.py on a merged zip that something changed afterwards."""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        zips = [platform_zip(self.tmp, p) for p in ("linux", "macos", "windows")]
        result, self.path = run_merge(self.tmp, zips)
        self.assertEqual(result.returncode, 0, result.stderr)

    def errors_after(self, edit) -> list[str]:
        """Errors for the merged zip with `edit` applied and install_size re-settled."""
        edited = rewrite(self.path, self.tmp / "edited.zip", edit)
        with zipfile.ZipFile(edited) as z:
            meta = json.loads(z.read("metadata.json"))
            others = sum(i.file_size for i in z.infolist() if i.filename != "metadata.json")
        blob = settle(meta, others)
        final = rewrite(edited, self.tmp / "final.zip",
                        lambda n, i, d: (i, blob if n == "metadata.json" else d))
        return validate_pcm.validate_zip(final)

    def test_unchanged_merge_passes(self):
        self.assertEqual(self.errors_after(lambda n, i, d: (i, d)), [])

    def test_binary_in_another_platforms_directory_fails(self):
        def edit(name, info, data):
            if name == "plugins/bin-linux-x86_64/schematic-viewer":
                return info, MAGIC["macos"]
            return info, data
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("bin-linux-x86_64/schematic-viewer is a macos executable", errs[0])

    def test_missing_server_fails(self):
        errs = self.errors_after(
            lambda n, i, d: None if n == "plugins/bin-macos/copperpour" else (i, d))
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("declares macos but has no plugins/bin-macos/copperpour", errs[0])

    def test_posix_binary_without_exec_bit_fails(self):
        def edit(name, info, data):
            if name == "plugins/bin-linux-x86_64/copperpour":
                info.external_attr = 0o100644 << 16
            return info, data
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("bin-linux-x86_64/copperpour is not stored executable", errs[0])

    def test_launcher_without_exec_bit_fails(self):
        def edit(name, info, data):
            if name == "plugins/bin/copperpour":
                info.create_system = 0  # what a zip made on Windows records
            return info, data
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("plugins/bin/copperpour is not stored executable", errs[0])

    def test_launcher_that_is_not_a_script_fails(self):
        def edit(name, info, data):
            return (info, MAGIC["linux"]) if name == "plugins/bin/copperpour" else (info, data)
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("is not the POSIX launcher", errs[0])

    def test_missing_launcher_fails(self):
        errs = self.errors_after(
            lambda n, i, d: None if n == "plugins/bin/copperpour" else (i, d))
        self.assertTrue(any("has no plugins/bin/copperpour" in e for e in errs), errs)

    def test_undeclared_platform_files_fail(self):
        def edit(name, info, data):
            if name == "metadata.json":
                meta = json.loads(data)
                meta["versions"][0]["platforms"] = ["linux", "windows"]
                return info, json.dumps(meta).encode()
            return info, data
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 2, errs)
        self.assertTrue(all("is a macos file" in e for e in errs), errs)

    def test_nested_file_in_a_binary_directory_fails(self):
        def edit(name, info, data):
            if name == "plugins/bin-macos/copperpour":
                info.filename = "plugins/bin-macos/lib/copperpour"
            return info, data
        errs = self.errors_after(edit)
        self.assertIn("plugins/bin-macos/lib/copperpour is not in a binary directory", " ".join(errs))

    def test_windows_entrypoint_fails(self):
        def edit(name, info, data):
            if name == "plugins/plugin.json":
                plugin = json.loads(data)
                plugin["actions"][0]["entrypoint"] = "bin/copperpour.exe"
                return info, json.dumps(plugin).encode()
            return info, data
        errs = self.errors_after(edit)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn("entrypoint 'bin/copperpour.exe' is not the launcher 'bin/copperpour'", errs[0])

    def test_download_size_cap_is_under_kicads_limit(self):
        # KiCad's addon repository stops downloading at 100 MB; this is that
        # promise, restated, not a mirror of the validator's constant.
        self.assertLess(validate_pcm.MAX_DOWNLOAD_SIZE, 100 * 1000 * 1000)

    def test_zip_over_the_cap_fails(self):
        size = self.path.stat().st_size
        saved = validate_pcm.MAX_DOWNLOAD_SIZE
        validate_pcm.MAX_DOWNLOAD_SIZE = size - 1
        self.addCleanup(setattr, validate_pcm, "MAX_DOWNLOAD_SIZE", saved)
        errs = validate_pcm.validate_zip(self.path)
        self.assertEqual(len(errs), 1, errs)
        self.assertIn(f"is {size} bytes", errs[0])


class Submission(unittest.TestCase):
    URL = "https://example.invalid/v0.0.0/"

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.zips = [platform_zip(self.tmp, p) for p in ("linux", "macos", "windows")]

    def template(self) -> dict:
        return json.loads((PACKAGING / "metadata.json").read_text(encoding="utf-8"))

    def test_merged_zip_is_one_version_entry_for_every_platform(self):
        result, merged = run_merge(self.tmp, self.zips)
        self.assertEqual(result.returncode, 0, result.stderr)
        meta = submission.build(self.template(), [merged], "0.0.0", self.URL)
        self.assertEqual(len(meta["versions"]), 1)
        entry = meta["versions"][0]
        self.assertEqual(sorted(entry["platforms"]), ["linux", "macos", "windows"])
        self.assertEqual(entry["download_url"], self.URL + "copperpour-pcm-v0.0.0.zip")
        self.assertEqual(entry["download_size"], merged.stat().st_size)
        # KiCad's v1 schema has no AGPL entry; this is its documented fallback.
        self.assertEqual(meta["license"], "open-source")
        self.assertEqual(validate_pcm.validate_metadata(meta, "submission"), [])

    def test_one_zip_per_platform_is_refused(self):
        # What v0.16.0 submitted: "package versions must have unique version numbers".
        with self.assertRaisesRegex(ValueError, "unique version numbers"):
            submission.build(self.template(), self.zips, "0.0.0", self.URL)

    def test_single_platform_zip_is_refused(self):
        with self.assertRaisesRegex(ValueError, "a submission needs"):
            submission.build(self.template(), self.zips[:1], "0.0.0", self.URL)


@unittest.skipIf(os.name != "posix", "pcm-launcher.sh runs under a POSIX sh")
class Launcher(unittest.TestCase):
    """pcm-launcher.sh, run as KiCad installs it, under a uname the test picks."""

    SERVER = """#!/bin/sh
echo "pid $$"
for arg in "$@"; do echo "arg [$arg]"; done
echo "stdin [$(cat)]"
echo "stderr line" >&2
exit 7
"""

    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.tmp)
        self.plugins = self.tmp / "plugins"
        (self.plugins / "bin").mkdir(parents=True)
        self.launcher = self.plugins / "bin" / "copperpour"
        shutil.copyfile(PACKAGING / "pcm-launcher.sh", self.launcher)
        self.launcher.chmod(0o755)
        for platform in ("linux-x86_64", "macos"):
            server = self.plugins / f"bin-{platform}" / "copperpour"
            server.parent.mkdir()
            server.write_text(self.SERVER.replace("pid", f"{platform} pid"))
            server.chmod(0o755)
        self.fake = self.tmp / "fake"
        self.fake.mkdir()

    def run_as(self, system: str, machine: str, command=None):
        uname = self.fake / "uname"
        uname.write_text(
            f'#!/bin/sh\ncase "$1" in -s) echo {system};; -m) echo {machine};; esac\n')
        uname.chmod(0o755)
        env = dict(os.environ, PATH=f"{self.fake}{os.pathsep}{os.environ['PATH']}")
        process = subprocess.Popen(
            [str(command or self.launcher), "two words", "--config", ""],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            env=env, cwd=self.tmp,
        )
        out, err = process.communicate(b"request")
        return process, out.decode(), err.decode()

    def test_linux_runs_its_server_with_stdio_args_and_status(self):
        process, out, err = self.run_as("Linux", "x86_64")
        self.assertEqual(process.returncode, 7)
        self.assertEqual(err, "stderr line\n")
        self.assertEqual(out.splitlines(), [
            f"linux-x86_64 pid {process.pid}",
            "arg [two words]", "arg [--config]", "arg []",
            "stdin [request]",
        ])

    def test_macos_runs_the_universal_server(self):
        process, out, _ = self.run_as("Darwin", "arm64")
        self.assertEqual(process.returncode, 7)
        self.assertEqual(out.splitlines()[0], f"macos pid {process.pid}")

    def test_unsupported_machine_fails_on_stderr_only(self):
        process, out, err = self.run_as("Linux", "aarch64")
        self.assertEqual(process.returncode, 126)
        self.assertEqual(out, "")
        self.assertIn("no server for Linux aarch64", err)

    def test_symlink_loop_fails_instead_of_spinning(self):
        loop = self.tmp / "loop"
        loop.symlink_to("loop2")
        (self.tmp / "loop2").symlink_to("loop")
        # A command can't be run through a looping link, so run the launcher's
        # text with $0 naming the loop, as if it had been.
        script = (PACKAGING / "pcm-launcher.sh").read_text()
        result = subprocess.run(
            ["sh", "-c", script, str(loop)], capture_output=True, text=True, timeout=10,
        )
        self.assertEqual(result.returncode, 126)
        self.assertEqual(result.stdout, "")
        self.assertIn("too many levels of symbolic links", result.stderr)

    def test_symlink_to_launcher_finds_the_package(self):
        link_dir = self.tmp / "path-bin"
        link_dir.mkdir()
        link = link_dir / "copperpour"
        link.symlink_to(os.path.relpath(self.launcher, link_dir))
        process, out, _ = self.run_as("Linux", "x86_64", command=link)
        self.assertEqual(process.returncode, 7)
        self.assertEqual(out.splitlines()[0], f"linux-x86_64 pid {process.pid}")


if __name__ == "__main__":
    unittest.main()
