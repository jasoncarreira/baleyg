# Pre-edit class-catalog goldens

These 24 compact UTF-8 JSON files have no trailing newline. They are raw,
byte-exact `serde_json::to_vec(&Catalog::build(...))` results from the pre-edit
`src/classes.rs` Git blob `d0000d1f72808234a88162575b579663de76da5d`.
The isolated baseline commit was `e4a45b2056377b094c12b348bcfafe2b3976475d`,
parent `ecdd7b8bfb9a79ae14c137e8696cf45c85a332e2`. Its `tests/classes.rs`
fixture pinned the valid v4 workspace marker
`123e4567-e89b-42d3-a456-426614174000` before indexing, so source-set IDs
never depend on the random temporary directory.

Two separate captures used
`TRELLIS_CAPTURE_CLASS_GOLDENS=<run-local-artifact-directory> cargo test --locked --test classes -- --test-threads=1`.
Both passed 23/23 tests and had identical bytes for all 24 files, with manifest
SHA-256 `e878dc22a7c3c65fe894e26a556d8a309f24961118f4f1fa1ff8a64a636671f7`.
Four outputs have intentional per-file truncation and warnings. The original
unstable-ID outputs remain archived in Git commit `63f7c04ae1c04842db257fdef8ee7c78bf562955`;
they are **not** the current goldens. Capture mode was removed: tests now read
every expected file and fail if one is absent or differs.

The #72 sample uses the checked-in generated small Java/Python inputs under
`source/`, indexed at `small/java/Csmall0000.java` and
`small/python/Csmall0000.py`. Their source SHA-256 hashes are respectively
`0ef14f96943160d87be2f6e10031c9fe1dacfb2ec984c7d826adc1ed81bc47f2`
and `1ef1666cfd0f416d2bd406527437a0b860fc9edac7b33fb1d96aaf5f96020ab9`.
