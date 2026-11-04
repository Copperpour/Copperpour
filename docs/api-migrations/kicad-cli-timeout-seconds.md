### `kicad_cli_timeout_seconds` sets the long kicad-cli timeout; a timed-out kicad-cli is killed (minor release)

The timeout of long kicad-cli operations (DRC, ERC, exports, upgrades) was a
fixed 600 s. The new config key `kicad_cli_timeout_seconds` sets it (default
600, so nothing changes without it; 0 is read as 1). A call that times out now
kills kicad-cli instead of leaving it running, then as a zombie.
