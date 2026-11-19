### New toolset `library_maintenance` with `upgrade_library` (minor release)

The new `library_maintenance` toolset holds `upgrade_library(library_path,
dry_run?, limit?)`. It upgrades a `.kicad_sym` symbol library or a `.pretty`
footprint library to the running KiCad's file format with `kicad-cli sym
upgrade` or `fp upgrade`. The upgrade runs into a temporary copy. Each file
older than the format kicad-cli writes is compared with its upgraded copy as
KiCad reads both, including the conversions KiCad's loader makes on older
files: overbars, `%R`/`%V`, footprint attributes before 20200826, margins and
clearances of 0 up to 20240201, net ties from keywords up to 20220815, De
Morgan body styles before 20250827, pad layer wildcards, and hidden footprint
texts as fields.

- **Not compared:** graphics, zones and groups are compared by count, not
  geometry, and text positions not at all.
- **Answer:** `kind` (`symbol` or `footprint`), `status` (`current`, `ready`,
  `blocked`, or `applied` after an apply), `format_version`, `counts` per file
  status (`current`, `upgraded`, `differs`), and `files`: up to `limit`
  (default 50) entries, those that differ first, then those with `notes`, then
  upgraded and current ones, each with `version`, `upgraded_version`, `status`,
  `differences` and, where there are any, `notes`. `shown` and `truncated`
  describe the list. `warning` says that older KiCad versions cannot open an
  upgraded file.
- **Dry run** (the default): writes nothing. An unfinished transaction in the
  library's directory is a `conflict`, since it could still change the files.
- **Apply** (`dry_run: false`): first completes an unfinished transaction in
  the library's directory and answers how many in `settled_transactions`; one
  it cannot complete is a `conflict` with nothing written. It then replaces the
  older files in one journalled transaction and answers `replaced`, the number
  of files. A file already in kicad-cli's format is left as it is. It is
  refused, writing nothing:
  - as `plan_blocked` when any file differs; `reasons` names the first
    differences;
  - as a `conflict` when a file changed after the read;
  - as a handler error when a file to replace is read-only, or when the
    library has more older files than the process may open at once.

  A failure after the transaction's journal is written is
  `mutation_outcome_uncertain`: the next apply completes the transaction, and
  `copperpour transaction status <directory>` shows what was applied.
- **Notes:** a footprint whose name inside the file is not its file name is a
  note, not a difference: KiCad uses the file name.
