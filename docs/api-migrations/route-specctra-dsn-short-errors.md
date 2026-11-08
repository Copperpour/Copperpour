### `route_specctra_dsn` and `check_freerouting` errors give the cause first, then the end of Freerouting's stderr (patch release)

When Freerouting had written to stderr, an error carried all of it in front of
the cause: about ten thousand characters of banner and warnings, with the
cause at the very end. The error now starts with the cause, followed by the
last 12 lines of stderr (at most 2000 characters, cut from the front with
`...`). When the run wrote a `.freerouting.log` next to the SES, the error
names it: it still holds all of stderr. Callers that parse the error text for
the cause no longer have to look past the banner.
