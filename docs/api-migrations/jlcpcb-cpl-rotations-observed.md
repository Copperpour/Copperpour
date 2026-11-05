### JLCPCB CPL rotations: rules backed by observations, unverified families warned (minor release)

`export_manufacturing_package` with `fab_house: "jlcpcb"` changes the CPL
rotation of one footprint, and reports more:

- The built-in policy becomes `copperpour-jlcpcb-cpl-v2`. Its
  `USB_C_Receptacle_HRO_TYPE-C-31-M-12` rule (+180) is withdrawn: it turned a
  correctly rotated connector upside down. The footprint is now unverified: it
  keeps KiCad's rotation and is listed in `warnings`. The SOIC-8 rule is
  unchanged.
- `warnings` now lists unverified matches and the polarized or multi-pin
  footprints no rule covers (all but resistors, capacitors, inductors, fuses,
  fiducials, test points and mounting holes; resistor arrays are reviewed); the
  package is not marked incomplete for them, but the summary no longer says
  "No warnings".

The `jlcpcb_cpl_corrections` policy file gains optional `footprint_exact`,
`status: "unverified"`, `evidence` (with `lcsc` and `side`),
`rotation_insensitive_prefixes`, `rotation_sensitive_prefixes` and `note`. A
rule whose `rotation_degrees` disagrees with its own `evidence` is refused, and
so is a bottom-side observation. Existing v1 files stay valid.
