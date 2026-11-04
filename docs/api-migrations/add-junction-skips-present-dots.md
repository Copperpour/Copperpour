### `add_junction` and `batch_add_junction` skip points that already have a dot (minor release)

Both tools used to write a junction at every requested point, so a point that
already had one, or that a `batch_add_junction` call listed twice, got a second
dot stacked on the first. KiCad keeps such dots through a resave and its ERC
does not report them. Now a point with a dot is left alone
([#281](https://github.com/Copperpour/Copperpour/issues/281)).

`batch_add_junction`'s `added` is now the number of dots written, not the
number of positions given; the new `already_present` counts the rest.
`add_junction` keeps `added_junction` and adds `already_present: true` when the
point had a dot. A call that adds nothing no longer rewrites the file. Callers
that read `added` as the request size should use `added + already_present`.
