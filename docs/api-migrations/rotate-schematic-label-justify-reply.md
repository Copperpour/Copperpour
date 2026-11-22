### `rotate_schematic_label` reports the justify it writes (patch release)

`rotate_schematic_label`'s reply carries `justify` as the direction it
writes, `"left"` or `"right"`, as it did before
[#516](https://github.com/Copperpour/Copperpour/pull/516). Since then it was
the byte range of the label's old justify within the label's block, such as
`[88, 109]`, or `null` for a label with no justify. What the tool writes to
the file is unchanged.

Issue: [#746](https://github.com/Copperpour/Copperpour/issues/746).
