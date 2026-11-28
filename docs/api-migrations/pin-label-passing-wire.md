### Labels on pins no longer join lone passing wires (patch release)

A label on a pin or a no-connect flag does not name a lone passing wire. A label over
multiple wires can still join them. `add_schematic_connection` refuses a route into
the unrelated net before writing if joining it would short the nets.
