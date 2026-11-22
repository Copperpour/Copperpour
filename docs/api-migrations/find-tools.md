### `find_tools` searches the registered tool catalogue (minor release)

Call the always-visible `find_tools` with a non-blank `query` of up to 200
characters and an optional `limit` from 1 to 50 (default 20). It searches
registered KiCad tools by name and description, including tools from unloaded
toolsets. Each match includes `tool`, `toolset`, `description`, and `loaded`.
Call `load_toolset` with the reported toolset to expose its tools in `tools/list`.

Search is case-insensitive. It ranks exact names first, then names containing
all query words, then name substrings, then descriptions containing all query
words, then partial description matches by number of matched words. Ties follow
toolset registration order and then tool name. An empty `matches` array is a
successful search. `truncated` reports whether additional matches exist.
Searching leaves toolset load state and `tools/list` unchanged.
