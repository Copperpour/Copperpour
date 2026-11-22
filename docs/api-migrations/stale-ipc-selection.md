### A stale IPC selection no longer implies a board is closed (patch release)

When discovery sees several possible editors, a Ping response does not prove which
one holds the requested board. If the selected listener disappears, Copperpour
checks the other candidates for that board. If a candidate's state remains unknown,
Copperpour refuses the operation rather than using the saved file or an unverified
editor. Close extra editors or restore IPC before retrying.

When several candidate paths stay the same, discovery can reuse a route for 250 ms
even if that editor stops listening. It does not cache the boards open in each
editor. An explicit `ipc_address` or `KICAD_API_SOCKET` still takes precedence.

Without an `open_board` selection, each board call checks multiple candidates in
parallel, even if discovery found a route. This avoids treating one editor's reply
as evidence about the others. A slow or inaccessible sibling can delay the answer
or cause an `ambiguous_open_board` refusal. A selected editor that cannot answer
the board query may also produce that refusal instead of an endpoint-only error.

During multi-candidate recovery, if Copperpour cannot access the selected endpoint,
it reports that transport failure and does not fall back to the saved board. A
Project Manager reply does not override a board lock or an unresolved live-board
observation.
