### `load_toolset` no longer repeats the tools of a toolset that is already loaded (minor release)

Loading a toolset that is already loaded used to return every one of its tools
with its description again. Now the single-name form adds
`"already_loaded": true` and returns `tools` as an empty array, and the array
form lists such toolsets in a new `already_loaded` array and leaves their tools
out of `tools`. `loaded` and `tools_added` keep their meaning: `tools_added`
still counts the tools of every toolset named in `loaded`.

A caller that read tool descriptions from a repeated `load_toolset` result
should take them from `tools/list`, where a loaded toolset's tools already are.
