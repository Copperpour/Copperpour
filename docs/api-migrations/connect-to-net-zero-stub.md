### `connect_to_net` with `stub_length: 0` writes no wire, and refuses a negative length (patch release)

`stub_length: 0` used to write a wire whose two ends are the same point. It
now writes only the label, on the pin or point itself, as
`batch_connect_to_net` does, and the response's `wire` is `null` instead of an
object with equal ends. A negative `stub_length` used to flip the stub to the
opposite side; it is now refused with `invalid_argument` naming
`stub_length`, before anything is written. Pass `direction` to choose the
side (#143).
