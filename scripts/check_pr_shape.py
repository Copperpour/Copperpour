"""Check the mechanical parts of a pull request's shape (CONTRIBUTING.md).

Run by .github/workflows/pr-shape.yml. Reads the PR body from PR_BODY and the
diff's `git diff --name-status --no-renames` lines from stdin, prints each
problem, and exits 1 if there is any.

1. The body references an issue on a line of its own (`Closes #N`, `Part of #N`),
   outside HTML comments and code blocks, or says `No issue: <reason>` for a mechanical change.
2. A change to tool, router or MCP code adds or edits a note in
   docs/api-migrations/, or the body says `API migration: none — <reason>`.
"""

import os
import re
import sys

# Declarations sit on one line, indented less than four spaces (more is a code
# block), so `[ \t]` and never `\s`: a newline must not let a later line supply
# the issue number or reason.
#
# A closing keyword or `Part of` at the start of a line, after any list marker,
# quote or bold, then `#N`, `owner/repo#N` or the issue's URL.
ISSUE_LINE = re.compile(
    r"^ {0,3}(?:[-*+>][ \t]*)*(?:\*\*|__)?"
    r"(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?|part of)[ \t]*:?[ \t]+"
    r"(?:(?:[\w.-]+/[\w.-]+)?#\d+|https://github\.com/[\w.-]+/[\w.-]+/issues/\d+)\b",
    re.IGNORECASE | re.MULTILINE,
)
# A stated reason must be filled in: the template's `<reason>` placeholder is not one.
NO_ISSUE = re.compile(r"^ {0,3}no issue:[ \t]*(?!<reason>)\S", re.IGNORECASE | re.MULTILINE)
NO_MIGRATION = re.compile(
    r"^ {0,3}api migration:[ \t]*none\b[ \t\u2014\u2013:-]*(?!<reason>)[^\s\u2014\u2013:-]",
    re.IGNORECASE | re.MULTILINE,
)
# GitHub ignores links inside HTML comments, and the template is full of them.
# Examples in fenced code blocks are not declarations either. An unclosed
# comment or fence runs to the end of the body.
HTML_COMMENT = re.compile(r"<!--.*?(?:-->|\Z)", re.DOTALL)
CODE_FENCE = re.compile(
    r"^ {0,3}(`{3,}|~{3,}).*?(?:^ {0,3}\1[`~]*[ \t]*$|\Z)",
    re.DOTALL | re.MULTILINE,
)

API_CODE = re.compile(r"^crates/copperpour-core/src/(?:tools|router|mcp)/.+\.rs$")
NOTES_DIR = "docs/api-migrations/"


def is_test_path(path: str) -> bool:
    name = path.rsplit("/", 1)[-1]
    return "/tests/" in path or name == "tests.rs" or name.endswith("_tests.rs")


def problems(body: str, name_status: list[str]) -> list[str]:
    found = []
    body = CODE_FENCE.sub("", HTML_COMMENT.sub("", body))
    if not ISSUE_LINE.search(body) and not NO_ISSUE.search(body):
        found.append(
            "The body has no issue reference. Put `Closes #N` (or `Part of #N` for a partial "
            "PR) on a line of its own, or `No issue: <reason>` for a mechanical change. "
            "A prose `Issue: #N` line does not link the issue."
        )

    changed = []
    for line in name_status:
        parts = line.rstrip("\n").split("\t")
        if len(parts) >= 2:
            changed.append((parts[0][:1], parts[-1]))

    api_files = [p for s, p in changed if API_CODE.match(p) and not is_test_path(p)]
    notes = [
        p
        for s, p in changed
        if s in ("A", "M")
        and p.startswith(NOTES_DIR)
        and p.endswith(".md")
        and p != NOTES_DIR + "README.md"
    ]
    if api_files and not notes and not NO_MIGRATION.search(body):
        shown = ", ".join(api_files[:5]) + (" …" if len(api_files) > 5 else "")
        found.append(
            f"Tool code changed ({shown}) but no note in {NOTES_DIR} was added or "
            "edited. Add one for a change a caller can see (docs/api-migrations/README.md), "
            "or say `API migration: none — <reason>` in the body."
        )
    return found


def main() -> int:
    found = problems(os.environ.get("PR_BODY", ""), sys.stdin.readlines())
    for problem in found:
        print(f"::error::{problem}")
    return 1 if found else 0


if __name__ == "__main__":
    sys.exit(main())
