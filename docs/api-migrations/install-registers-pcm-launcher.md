### `copperpour init` registers `bin/copperpour` in the all-platforms package (patch release)

In the all-platforms PCM package, `bin/copperpour` is a launcher that runs
`bin-linux-x86_64/copperpour` or `bin-macos/copperpour`. `init`, `uninstall`,
`status` and the first-time setup now name the launcher, the documented path,
instead of the server it runs. Claude hooks naming that server are this
install's own: `init` replaces them, `uninstall` removes them, and `status`,
`get_installation_info` and `initialize` report them as `different` with reason
`alias_handler` instead of `other_executable`
([#400](https://github.com/Copperpour/Copperpour/issues/400)).
