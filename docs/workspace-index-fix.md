# Workspace indexing correction

The previous cwd-tree deployment exposed Baleyg while the index button still targeted the
nested feature-factory sample. That mismatch was a product bug, not a failed indexing job.

At the time of this recorded snapshot, the inspector used `--workspace . --state-dir .baleyg/cwd-native`
and an existing token at `.baleyg/native-smoke/daemon.token`. Those commands and in-checkout
paths are historical, not current startup instructions. Today `--state-dir` is removed.
Run `serve --workspace . --token-file /absolute/private/path/outside/checkout/daemon.token`
with an existing private parent directory: the token path must be an external absolute file,
not inside the selected checkout. Fixed per-user locations hold indexes and saved records;
this command does not migrate or reset old data. Browsing defaults to the indexed workspace,
and the workspace defaults to cwd. Explicit different browse roots remain possible, but
their indexing scope is displayed prominently and the Index button names its target.
Changing workspace invalidates browser evidence even when revision numbers happen to match.

**Historical snapshot observations (not current runtime limits):**

Observed browser Index action: Baleyg revision2,35 JavaScript files,1031 symbols,4736 calls.
`web/app.js` expands to methods and `describe` renders a sequence. `src/main.rs` explicitly
says Rust indexing is not supported yet. No SCIP artifact exists for this workspace; lexical
calls remain unresolved rather than borrowing the sample's semantic evidence.

148 Rust and104 UI tests pass; build, formatting and clippy pass. No remote CI was run.
At the time of the snapshot, the previous `.baleyg/native-smoke` index and saved data
remained intact. Its Jev and ACP allowances were not reset or rebound. Providers were
disabled for that workspace pending separate configuration and authorization. No provider
calls were made.
