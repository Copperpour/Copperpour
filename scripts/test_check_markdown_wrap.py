"""Integration tests for the advisory Markdown workflow (requires rumdl)."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
LONG_PROSE = (
    "A long sentence with plenty of separate words that should be wrapped by "
    "the advisory Markdown workflow."
)
LONG_BARE_URL = "https://example.test/" + "x" * 100
EXEMPT_MARKDOWN = "\n\n".join([
    f"# {LONG_PROSE}",
    f"| Heading |\n| --- |\n| {LONG_PROSE} |",
    f"```text\n{LONG_PROSE}\n```",
    LONG_BARE_URL,
    f"- <{LONG_BARE_URL}>",
    "[Reference](https://example.com/" + "a" * 100 + ")",
    "See `" + "command --with-a-long-option " * 5 + "` for details.",
])


class TestMarkdownWrap(unittest.TestCase):
    def setUp(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        self.repo = Path(folder.name)
        self.env = {
            **os.environ,
            "GIT_AUTHOR_NAME": "Test", "GIT_AUTHOR_EMAIL": "test@example.com",
            "GIT_COMMITTER_NAME": "Test", "GIT_COMMITTER_EMAIL": "test@example.com",
        }
        shutil.copyfile(ROOT / ".rumdl.toml", self.repo / ".rumdl.toml")

    def run_command(self, *args):
        return subprocess.check_output(args, cwd=self.repo, env=self.env, text=True)

    def write(self, path, content):
        (self.repo / path).write_text(content + "\n", encoding="utf-8")

    def commit(self, message):
        self.run_command("git", "add", ".")
        self.run_command("git", "commit", "-qm", message)

    def test_ci_annotates_only_new_or_edited_prose(self):
        self.run_command("git", "init", "-q")
        self.write("guide.md", LONG_PROSE + "\n\nShort original text.")
        self.write("rename.md", LONG_PROSE + "\n\n" + "Retained words. " * 40 + "\n\nShort text.")
        self.write("removed.md", "This draft is being deleted.")
        self.write("deletion.md", f"Short introduction.\n{LONG_PROSE}\nDelete this line.")
        self.write("directory.md", LONG_PROSE)
        self.commit("base")
        base = self.run_command("git", "rev-parse", "HEAD").strip()

        self.write("guide.md", LONG_PROSE + "\n\n" + LONG_PROSE)
        (self.repo / "rename.md").rename(self.repo / "renamed.md")
        self.write("renamed.md", LONG_PROSE + "\n\n" + "Retained words. " * 40 + "\n\n" + LONG_PROSE)
        (self.repo / "removed.md").unlink()
        self.write("new, notes: 你好.md", LONG_PROSE + "\n\n" + EXEMPT_MARKDOWN)
        self.write("notes.txt", f"Plain text. {LONG_PROSE}")
        (self.repo / "file-link.md").symlink_to("notes.txt")
        self.write("deletion.md", f"Short introduction.\n{LONG_PROSE}")
        (self.repo / "directory.md").rename(self.repo / "reference.md")
        (self.repo / "directory.md").mkdir()
        self.write("directory.md/new.md", "Different short words.")
        (self.repo / "docs").mkdir()
        self.write("docs/API_MIGRATIONS.md", f"Released history. {LONG_PROSE}")
        self.commit("head")

        output = self.run_command(
            sys.executable, ROOT / "scripts/check_markdown_wrap.py", f"{base}...HEAD"
        )
        self.assertCountEqual(
            [line.split(",col=", 1)[0] for line in output.splitlines()],
            [
                "::warning file=guide.md,line=3",
                "::warning file=renamed.md,line=5",
                "::warning file=new%2C notes%3A 你好.md,line=1",
            ],
        )

    def test_ci_uses_physical_lines_in_list_items(self):
        self.run_command("git", "init", "-q")
        self.write("guide.md", f"- Original opener.\n  {LONG_PROSE}\n\n- Another item.\n  Short.")
        self.commit("base")
        self.write("guide.md", f"- Edited opener.\n  {LONG_PROSE}\n\n- Another item.\n  {LONG_PROSE}")
        self.commit("head")

        output = self.run_command(
            sys.executable, ROOT / "scripts/check_markdown_wrap.py", "HEAD^...HEAD"
        )
        annotations = [line.split(",col=", 1)[0] for line in output.splitlines()]
        self.assertEqual(annotations, ["::warning file=guide.md,line=5"])

    def test_ci_and_local_check_agree_at_the_wrap_boundary(self):
        self.run_command("git", "init", "-q")
        self.commit("base")
        prefix = (
            "This sentence uses ordinary words so the final word can move onto "
            "a new line without "
        )
        self.write("guide.md", "\n\n".join([
            prefix + "fail.",
            prefix + "error.",
            prefix + "difficulty.",
            LONG_BARE_URL,
            f"See {LONG_BARE_URL}",
        ]))
        self.commit("head")

        local = json.loads(self.run_command(
            "rumdl", "check", "--fail-on", "never", "--output-format", "json", "guide.md"
        ))
        self.assertEqual([finding["line"] for finding in local], [3, 5, 9])

        output = self.run_command(
            sys.executable, ROOT / "scripts/check_markdown_wrap.py", "HEAD^...HEAD"
        )
        annotations = [line.split(",col=", 1)[0] for line in output.splitlines()]
        self.assertEqual(annotations, [
            "::warning file=guide.md,line=3",
            "::warning file=guide.md,line=5",
            "::warning file=guide.md,line=9",
        ])
        self.run_command("rumdl", "check", "--config", "MD013.reflow = true", "--fix", "guide.md")
        self.run_command("rumdl", "check", "guide.md")

    def test_local_fix_wraps_prose_and_preserves_exempt_content(self):
        self.write("guide.md", LONG_PROSE + "\n\n" + EXEMPT_MARKDOWN)
        self.run_command("rumdl", "check", "--config", "MD013.reflow = true", "--fix", "guide.md")
        self.run_command("rumdl", "check", "guide.md")
        wrapped, exempt = (self.repo / "guide.md").read_text(encoding="utf-8").split("\n\n", 1)
        self.assertEqual(" ".join(wrapped.splitlines()), LONG_PROSE)
        self.assertTrue(all(len(line) <= 90 for line in wrapped.splitlines()))
        self.assertEqual(exempt, EXEMPT_MARKDOWN + "\n")
