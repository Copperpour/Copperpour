"""Annotate rumdl findings on added lines in a Git diff from the repository root."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys

HUNK = re.compile(rb"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@", re.MULTILINE)
ANNOTATION_DATA = {"%": "%25", "\r": "%0D", "\n": "%0A"}
ANNOTATION_PROPERTY = {**ANNOTATION_DATA, ",": "%2C", ":": "%3A"}


def git_diff(*args):
    return subprocess.check_output(
        ["git", "diff", "--no-color", "--no-ext-diff", "--no-textconv", *args]
    )


def changed_markdown_files(revision_range):
    changes = git_diff(
        "--raw", "-z", "--no-abbrev", "--find-renames", "--diff-filter=AMRT",
        revision_range, "--",
    )
    entries = iter(os.fsdecode(changes).split("\0")[:-1])
    for header in entries:
        old_mode, _, old_blob, new_blob, status = header[1:].split()
        path = next(entries)
        if status.startswith("R"):
            path = next(entries)
        file = Path(path)
        if file.suffix == ".md" and not file.is_symlink() and file.is_file():
            yield path, old_blob if old_mode.startswith("100") else None, new_blob


def added_lines(old_blob, new_blob):
    if old_blob is None:
        content = subprocess.check_output(["git", "cat-file", "blob", new_blob])
        count = content.count(b"\n") + bool(content and not content.endswith(b"\n"))
        if count:
            yield range(1, count + 1)
        return

    # Blob IDs keep a rename's diff independent of other changed paths.
    patch = git_diff("--unified=0", "--inter-hunk-context=0", old_blob, new_blob)
    for first, count in HUNK.findall(patch):
        start = int(first)
        lines = range(start, start + int(count or b"1"))
        if lines:
            yield lines


def annotate(revision_range):
    for path, old_blob, new_blob in changed_markdown_files(revision_range):
        ranges = list(added_lines(old_blob, new_blob))
        if not ranges:
            continue

        findings = json.loads(subprocess.check_output(
            ["rumdl", "check", "--config", ".rumdl.toml", "--no-cache",
             "--fail-on", "never", "--output-format", "json", "--", path]
        ))
        # GitHub workflow commands require extra escaping in annotation properties.
        file = path.translate(str.maketrans(ANNOTATION_PROPERTY))
        for finding in findings:
            line = finding["line"]
            if any(line in lines for lines in ranges):
                message = finding["message"].translate(str.maketrans(ANNOTATION_DATA))
                print(f"::warning file={file},line={line},col={finding['column']},"
                      f"title=Markdown prose wrap::{message}")


if __name__ == "__main__":
    annotate(sys.argv[1])
