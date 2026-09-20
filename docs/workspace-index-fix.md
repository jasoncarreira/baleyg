# Workspace indexing correction

The previous cwd-tree deployment exposed Baleyg while the index button still targeted the
nested feature-factory sample. That mismatch was a product bug, not a failed indexing job.

The running inspector now uses `--workspace . --state-dir .baleyg/cwd-native`, with the same
existing token via `--token-file .baleyg/native-smoke/daemon.token`. Browsing defaults to the
indexed workspace, and the workspace defaults to cwd. Explicit different browse roots remain
possible, but their indexing scope is displayed prominently and the Index button names its target.
Changing workspace invalidates browser evidence even when revision numbers happen to match.

Observed browser Index action: Baleyg revision2,35 JavaScript files,1031 symbols,4736 calls.
`web/app.js` expands to methods and `describe` renders a sequence. `src/main.rs` explicitly
says Rust indexing is not supported yet. No SCIP artifact exists for this workspace; lexical
calls remain unresolved rather than borrowing the sample's semantic evidence.

148 Rust and104 UI tests pass; build, formatting and clippy pass. No remote CI was run.
The previous `.baleyg/native-smoke` index and saved data remain intact. Its Jev and ACP
allowances are not reset or rebound. Providers are disabled for this new workspace until
separately configured and authorized. No provider calls were made.
