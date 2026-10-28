### Per-user paths are never relative to the working directory (patch release)

Per-user paths used to resolve against the server's working directory when
`HOME` (`APPDATA` on Windows) was unset, empty or relative. Now an unset `HOME`
on Unix falls back to the account's home directory, and an unset `APPDATA`
to the roaming profile folder. When there is still no absolute home directory
(a relative `HOME`, say), per-user paths resolve to nothing:

- `load_user_config`, `save_user_config`, `get_effective_config`,
  `list_design_rules` and `add_design_rule` with `scope: "user"` refuse.
- `list_symbol_libraries` and `list_footprint_libraries` with the global table
  in scope, and `register_symbol_library` and `register_footprint_library` with
  `scope: "global"`, refuse.
- The JLCPCB tools refuse unless the server config sets `jlcpcb_db_path`
  (or `download_jlcpcb_database` is given `output_path`); component placement
  and part lookups that use the database only to enrich a result skip it.
- `copperpour install`, `uninstall` and `status`, and the startup guidance
  check, fail or are skipped as they already were for an unset `HOME`.
- User templates, the EasyEDA cache, the global library tables used for
  symbol and footprint lookup, the per-user `config.toml`, the call log and
  the run registry are skipped.

Set `HOME` (or `APPDATA`) to an absolute path to restore the old locations
([#171](https://github.com/Copperpour/Copperpour/issues/171)).
