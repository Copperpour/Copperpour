# Selected no-connect pin function at a contact

Built with Copperpour's served `create_project` and `add_schematic_component`
tools using stock `Interface_USB:CP2105-F01-GM` and `Device:R`. Opened in
Eeschema 10.0.8, selected `NC` for `U1` pin 16 in Symbol Properties > Pin
Functions, and saved in Eeschema.

`U1.16` touches `R1.1` at `(114.3, 71.12)`. The underlying pin is an input; the
selected `NC` function sets its electrical type to `no_connect`.
