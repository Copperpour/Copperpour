### `plan_specctra_ses_import` and `apply_specctra_ses` accept negative footprint angles (patch release)

KiCad writes a footprint's angle in (-180, 180], and Freerouting returns it in
[0, 360): a footprint at -90 degrees comes back as 270. The import compared the
two numbers as written and refused every board with a footprint at a negative
angle with "SES changes placement of component '…'". It now compares placement
angles modulo 360, with the same 1e-9 degree tolerance. A session that really
turns a footprint (90 to 180, for example) is still refused with the same
error; position, side and image checks are unchanged.
