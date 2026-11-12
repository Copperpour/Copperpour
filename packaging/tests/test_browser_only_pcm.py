"""Browser-only packages retain server entrypoints and reject retired viewer flags."""
import json
import shutil
import subprocess
import tempfile
import unittest
import zipfile
from pathlib import Path

from test_merge_pcm import MAGIC, PACKAGING, platform_zip, run_merge, validate_pcm


class BrowserOnlyPackages(unittest.TestCase):
    def test_merge_accepts_server_only_packages_on_all_platforms(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            def without_viewer(meta, files):
                for name in list(files):
                    if "schematic-viewer" in name:
                        del files[name]
            zips = [platform_zip(directory, p, change=without_viewer)
                    for p in ("linux", "macos", "windows")]
            result, merged = run_merge(directory, zips)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(validate_pcm.validate_zip(merged), [])
            with zipfile.ZipFile(merged) as archive:
                self.assertFalse(any("schematic-viewer" in n for n in archive.namelist()))
                for name, platform in [("bin/copperpour.exe", "windows"),
                                       ("bin-linux-x86_64/copperpour", "linux"),
                                       ("bin-macos/copperpour", "macos")]:
                    self.assertTrue(archive.read("plugins/" + name).startswith(MAGIC[platform]))

    @unittest.skipUnless(shutil.which("bash") and shutil.which("zip"), "needs bash and zip")
    def test_bash_builder_stages_only_server_and_refuses_retired_flag_before_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            binary = directory / "copperpour"
            binary.write_bytes(MAGIC["linux"])
            output = directory / "output"
            command = ["bash", str(PACKAGING / "build-pcm.sh"), "--version", "0.0.0",
                       "--binary", str(binary), "--platform", "linux", "--out", str(output)]
            refused = subprocess.run(command + ["--viewer", str(binary)], capture_output=True, text=True)
            self.assertEqual(refused.returncode, 2)
            self.assertIn("--viewer was retired", refused.stderr)
            self.assertFalse(output.exists())
            built = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stderr)
            archive_path = output / "copperpour-pcm-v0.0.0-linux.zip"
            self.assertEqual(validate_pcm.validate_zip(archive_path), [])
            with zipfile.ZipFile(archive_path) as archive:
                binaries = [n for n in archive.namelist() if n.startswith("plugins/bin/") and not n.endswith("/")]
                self.assertEqual(binaries, ["plugins/bin/copperpour"])
                manifest = json.loads(archive.read("plugins/plugin.json"))
                self.assertEqual(manifest["actions"][0]["entrypoint"], "bin/copperpour")

    @unittest.skipUnless(shutil.which("pwsh"), "needs PowerShell")
    def test_powershell_builder_stages_only_server_and_refuses_retired_flag_before_output(self):
        with tempfile.TemporaryDirectory() as scratch:
            directory = Path(scratch)
            binary = directory / "copperpour.exe"
            binary.write_bytes(MAGIC["windows"])
            output = directory / "output"
            command = ["pwsh", "-NoProfile", "-File", str(PACKAGING / "build-pcm.ps1"),
                       "-Version", "0.0.0", "-BinaryPath", str(binary), "-OutDir", str(output)]
            refused = subprocess.run(command + ["-ViewerPath", str(binary)], capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertIn("-ViewerPath was retired", refused.stderr)
            self.assertFalse(output.exists())
            built = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(built.returncode, 0, built.stderr)
            archive_path = output / "copperpour-pcm-v0.0.0-windows.zip"
            self.assertEqual(validate_pcm.validate_zip(archive_path), [])
            with zipfile.ZipFile(archive_path) as archive:
                binaries = [n for n in archive.namelist() if n.startswith("plugins/bin/") and not n.endswith("/")]
                self.assertEqual(binaries, ["plugins/bin/copperpour.exe"])
