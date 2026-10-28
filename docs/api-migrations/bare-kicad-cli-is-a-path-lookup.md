### A bare `kicad_cli` or `kicad_binary` is looked up on `PATH` only (patch release)

A bare name, such as the default `kicad-cli`, no longer resolves to a file of
that name in the server's working directory, and empty or relative `PATH`
entries are skipped. A stray `kicad-cli` there used to stop discovery:
`get_installation_info` reported `cli_path: "kicad-cli"`, and on Windows the
installed KiCad was never found. The lookup now continues to `PATH` and the
known install locations, and `cli_path` names the binary found. To use a
binary in the working directory, configure it as `./kicad-cli`
([#182](https://github.com/Copperpour/Copperpour/issues/182)).
