### JLCPCB CPL rotations: rules backed by observations, unverified families warned (minor release)

`export_manufacturing_package` with `fab_house: "jlcpcb"` changes the CPL
rotation of some parts:

- The built-in `USB_C_Receptacle_HRO_TYPE-C-31-M-12` rule (+180) is withdrawn:
  it turned a correctly rotated connector upside down. The footprint is now
  unverified: it keeps KiCad's rotation and is listed in `warnings`.
- The `SOIC-8_3.9x4.9mm_P1.27mm` rule (+270) widens to every `SOIC-` footprint.
- New built-in rules, each with its observation: +270 for `SSOP-`, `MSOP-`,
  `TI_SO-PowerPAD-`, `TQFN-`, `VQFN-`, `SOT-23-6`, `SW_DIP_SPSTx`,
  `ESP32-WROOM-32E` and `IDC-Header_2x05_P2.54mm_Vertical`; +180 for `SOT-23`
  and `SOT-23-5` (exact names only); 0 for the SOD-123/SMA/SMB/SMC diodes,
  0603/0805 LEDs, `CP_Elec_` capacitors, and a few switch, relay, terminal
  block and RJ45 footprints. `TSSOP-`, `QFN-`, `DFN-` and other `IDC-Header_`
  footprints are unverified.

`warnings` now lists unverified matches and the polarized or multi-pin
footprints no rule covers (resistors, capacitors, inductors, fuses, fiducials,
test points and mounting holes excepted); the package is not marked incomplete
for them. The `jlcpcb_cpl_corrections` policy file gains optional
`footprint_exact`, `status: "unverified"`, `evidence`,
`rotation_insensitive_prefixes` and `note`; a rule whose `rotation_degrees`
disagrees with its own `evidence` is refused. Existing v1 files stay valid.
