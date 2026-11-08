### `generate_jlcpcb_order_link` (minor release)

A new `manufacturing` tool uploads a board to JLCPCB and returns the link to
its quote page ([#208](https://github.com/Copperpour/Copperpour/issues/208)):
`generate_jlcpcb_order_link(board, schematic?, include_assembly?, skip_drc?,
quantity?, thickness?, color?, surface_finish?, copper_weight?,
jlcpcb_cpl_corrections_path?)`. It writes the JLCPCB package as
`export_manufacturing_package` does and runs DRC on the board it was exported
from. Then it posts the Gerbers and drill files, and with assembly the BOM and
CPL, to EasyEDA's order endpoint. This sends the design to a third party; it
places no order. The tool comes with the `jlcpcb` build feature, which release
builds have.

`schematic` is required when `include_assembly` is true, the default. The BOM
sends the schematic's `LCSC`, `MPN` and `Manufacturer` fields. An incomplete
package, a DRC error, or a board without an Edge.Cuts outline refuses the
upload before anything is sent. `skip_drc` skips DRC.

#### Response

`upload_outcome` is `not_sent`, `sent` or `uncertain`, and `uploaded` is
`false`, `true` or `null` with it:

- `sent` with `order_url`: JLCPCB's link to the uploaded files. It asks for a
  JLCPCB login, then opens the quote page with the files loaded. `upload_id`
  is EasyEDA's id for the upload.
- `not_sent`: a refusal before the request, or a failure to build the request
  or to connect. Nothing left the machine.
- `sent` with `order_url: null`: the endpoint received the files, but its
  answer was not a success (redirects are not followed), could not be read,
  or had no link. The first two give `http_status`. A sent result does not
  prove the upload was accepted or that a quote exists.
- `uncertain`: another transport failure, which may come after the files left
  the machine. Check the earlier outcome before uploading again; don't retry
  automatically.

A failure once the request is under way keeps `order_id` and the reason, and
a failed request adds `recovery`, which says whether retrying is safe.
