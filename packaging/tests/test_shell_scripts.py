"""Shell scripts in packaging/ must parse under macOS's /bin/bash 3.2.

Run: python -m unittest discover -s packaging/tests -v

CI has no bash 3.2, so this checks the source for the construct that broke it
(#408): bash 3.2 scans a command substitution for quotes, heredoc text
included, so an apostrophe in a heredoc inside $( ... ) breaks the parse.
"""

import re
import unittest
from pathlib import Path

PACKAGING = Path(__file__).resolve().parent.parent
HEREDOC = re.compile(r"<<-?\s*['\"]?\w+")


def heredocs_in_substitutions(text: str) -> list[int]:
    """Line numbers where a heredoc starts inside an unclosed $( on that line."""
    hits = []
    for number, line in enumerate(text.splitlines(), 1):
        match = HEREDOC.search(line)
        if not match:
            continue
        before = line[: match.start()]
        if "$(" in before and before.count("(") > before.count(")"):
            hits.append(number)
    return hits


class HeredocPlacementTests(unittest.TestCase):
    def test_scripts_keep_heredocs_out_of_command_substitutions(self):
        scripts = sorted(PACKAGING.glob("*.sh"))
        self.assertIn(PACKAGING / "build-pcm.sh", scripts)
        for script in scripts:
            with self.subTest(script=script.name):
                self.assertEqual(
                    heredocs_in_substitutions(script.read_text(encoding="utf-8")),
                    [],
                    f"{script.name}: a heredoc inside $( ... ) fails to parse "
                    "under macOS's bash 3.2 when its text has an apostrophe",
                )

    def test_detects_the_line_that_broke_macos(self):
        # The line build-pcm.sh carried when macos.yml failed (#408).
        line = (
            'install_size="$(python3 - "$repo_root/packaging/metadata.json" '
            "\"$staging/metadata.json\" \"$version\" \"$staging\" \"$platform\" <<'PY'"
        )
        self.assertEqual(heredocs_in_substitutions(line), [1])

    def test_closed_substitution_before_a_heredoc_passes(self):
        self.assertEqual(heredocs_in_substitutions('x="$(pwd)"; cat <<EOF'), [])
        self.assertEqual(heredocs_in_substitutions("python3 - <<'PY'"), [])


if __name__ == "__main__":
    unittest.main()
