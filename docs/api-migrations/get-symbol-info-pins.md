### `get_symbol_info` lists every pin and counts pins as KiCad does (minor release)

[#712](https://github.com/Copperpour/Copperpour/issues/712), as agreed there.
`get_symbol_info` dropped every pin whose number another pin of the symbol had
already used, so a duplicate was hidden and `pin_count` was the number of
distinct pin numbers.

- **Pins:** every pin is listed, each with `unit` and `body_style` (the values
  `edit_symbol_pin` takes; `null` for a unit whose name KiCad would not read). A
  De Morgan symbol lists each pin once per body style.
- **Derived symbols:** a symbol that `extends` another shows its root's pins,
  as KiCad does; pins written on the derived symbol itself are not shown.
- **`pin_count`:** KiCad's count (`LIB_SYMBOL::GetPinCount`): the pins of every
  unit in body style 1, a pin of body style 0 included, a stacked number such
  as `[2,3,6,7]` counting once per pin; a run like `[1-1000000000]` is counted,
  never expanded. Correct symbols with one drawing per
  pin keep their count. It changes for:
  - stacked numbers, which counted once (`TL431D` 3 → 6): a fix;
  - a pin drawn in two units under one number, such as a shared clock or
    clear, which KiCad counts twice (`74LS78` 14 → 16), as its chooser and
    footprint filter do;
  - real duplicates, which were hidden (two pins 8 in one unit: 17 → 18).

  Of KiCad 10.0.8's 22,911 stock symbols, 72 change.
- **`warnings`:** new, present only when there is one: each pair of pins KiCad's
  symbol checker reports as duplicates (same number, unless both body styles
  are set and differ), with both pins' names, positions, units and body styles.
  Where stacked runs overlap, a pair is reported once for the overlapping range
  (`duplicate pin numbers 500-1000000000: …`), not once per number.

Callers that compared `pin_count` with a footprint's pad count see the
difference for shared pins; the warning names them.
