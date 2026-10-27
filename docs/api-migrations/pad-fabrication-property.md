### `edit_footprint_pad` and `create_footprint` set a pad's fabrication property (minor release)

`edit_footprint_pad` and each `create_footprint` pad accept an optional
`fabrication_property`, one of KiCad's pad properties: `none`, `bga`,
`fiducial_glob`, `fiducial_loc`, `testpoint`, `heatsink`, `castellated`,
`mechanical` or `pressfit`. It is written as KiCad 10 writes it, and `none`
removes the property. Without the field, `edit_footprint_pad` leaves the pad
as it is and `create_footprint` writes no property. An unknown value is
refused as `invalid_argument`, naming the field, and nothing is written.

When the field is used, the response reports `fabrication_property` as read
back from the written pads. A property that doesn't suit the pad type, such
as `castellated` on an SMD pad, is still written, as KiCad allows, and the
response carries KiCad's padstack warning in `warnings`.
