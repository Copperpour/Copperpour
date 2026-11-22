### The Specctra custom-rules refusal states its reason (patch release)

`export_specctra_dsn` still refuses a board with a sibling `.kicad_dru`, and
`apply_specctra_ses` and `plan_specctra_ses_import` still refuse a reverse
manifest that records `custom_rules`. Their messages now say why: custom DRC rules
are not carried to the router, so Copperpour cannot vouch for routes against them.
Freerouting routes only to what the DSN carries, whichever exporter wrote it.

The import refusal no longer reads "manifest does not describe the supported
first routing profile"; that message remains for the manifest's other profile
fields. Error kinds, arguments and response keys are unchanged. Callers that
matched the old message text should match the new one (#75). The v0.17.0 note on
native Specctra export order said the export refusal named an SES-import reason;
that reason is superseded by this one.

Import still refuses on the manifest's record, not on the rules file. When a
`.kicad_dru` appears only after export, `apply_specctra_ses` applies the session,
as before; when `drc.project_rules_applied` is `true`, its candidate DRC reports
violations of the project's current rules.
