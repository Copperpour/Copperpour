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


def git_diff(revision_range, *options, paths=()):
    return subprocess.check_output(
        ["git", "--literal-pathspecs", "diff", "--find-renames", "--no-color",
         "--no-ext-diff", "--no-textconv", *options, revision_range, "--", *paths]
    )


def changed_markdown_paths(revision_range):
    changes = git_diff(revision_range, "--name-status", "-z", "--diff-filter=AMRT")
    entries = iter(os.fsdecode(changes).split("\0")[:-1])
    for status in entries:
        paths = [next(entries)]
        if status.startswith("R"):
            paths.append(next(entries))
        path = Path(paths[-1])
        if path.suffix == ".md" and not path.is_symlink() and path.is_file():
            yield paths


def added_lines(revision_range, paths):
    patch = git_diff(revision_range, "--unified=0", "--inter-hunk-context=0", paths=paths)
    for first, count in HUNK.findall(patch):
        start = int(first)
        lines = range(start, start + int(count or b"1"))
        if lines:
            yield lines


def annotate(revision_range):
    for paths in changed_markdown_paths(revision_range):
        ranges = list(added_lines(revision_range, paths))
        if not ranges:
            continue

        path = paths[-1]
        # Reflow can anchor a continuation warning to the first line of a list item.
        findings = json.loads(subprocess.check_output(
            ["rumdl", "check", "--config", ".rumdl.toml", "--config", "MD013.reflow = false",
             "--no-cache",
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
