### CLI-backed tools report signal termination (patch release)

On Unix, when a signal kills `kicad-cli`, the failure message reports the signal instead of `exited with -1`. The error kind, diagnostic output and messages for ordinary exit codes are unchanged. Use `isError` and `error.kind` where present, and keep checking tool-specific warnings for incomplete results.
