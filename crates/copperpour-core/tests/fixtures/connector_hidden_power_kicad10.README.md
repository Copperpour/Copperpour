# Hidden power input on an ordinary symbol

`connector_hidden_power_kicad10.kicad_sch` is the #799 review fixture, resaved with
`kicad-cli` 10.0.8 (`sch upgrade --force`). SHA-256:
`9ad362af88bece155b62b350efc8762ef531e7a054552e9a5375a11e97de88b2`.

J1.1 and U1.1 share a wire. U1.14 is a hidden `power_in` pin named `VCC` at
that wire's endpoint. A different wire joins R1.1 to a `power:VCC` symbol.
`kicad-cli sch export netlist` on 10.0.6 and 10.0.8 puts exactly J1.1, R1.1,
U1.1, and U1.14 on `VCC`. The power symbol is evidence for J1.1, but it is not
on J1.1's own drawn wire run.
