# Pre-edit class-catalog goldens

These 24 compact UTF-8 JSON files have no trailing newline. They are byte-exact
`serde_json::to_vec(&Catalog::build(...))` results on the unchanged implementation
at `ecdd7b8bfb9a79ae14c137e8696cf45c85a332e2` (src/classes.rs Git blob
`d0000d1f72808234a88162575b579663de76da5d`). Command:

`BALEYG_CAPTURE_CLASS_GOLDENS=<run-local-artifact-directory> cargo test --locked --test classes -- --test-threads=1`

23/23 tests passed. The capture artifact manifest SHA-256 was
`ff53242f59f5c75932cc73373455a2f1733dc0013f8180d25315cc674227d0e1`.
Fixtures include intentional per-file truncation and warnings; none are excluded.
The #72 sample uses the checked-in generated small Java/Python inputs under
`source/`, indexed at `small/java/Csmall0000.java` and
`small/python/Csmall0000.py`. Their source SHA-256 hashes are respectively
`0ef14f96943160d87be2f6e10031c9fe1dacfb2ec984c7d826adc1ed81bc47f2`
and `1ef1666cfd0f416d2bd406527437a0b860fc9edac7b33fb1d96aaf5f96020ab9`.
Capture mode was removed after freezing: tests read goldens and fail if absent.
