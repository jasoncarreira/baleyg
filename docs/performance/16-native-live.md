# #16 native live refresh — run-level Stage 3 measurement

## Scope and status

The owner approved Stage 3 as **run-level evidence**, not a merge gate for `leader-root-fallback`. This note defines the measurement before the exclusive reference-host slot is scheduled. No timed watcher samples or measured p95 values have been collected in this slice. `cargo test --locked --all-targets` verifies correctness, not this latency gate. The #67 single-CLI performance runner is not watcher evidence. #73 remains parked and must not share the timed host.

## Watcher-driven local sample protocol

- Freeze one medium corpus and one large corpus, including file lists, language mix, options and native fact floors. Record checkout hashes, host, executable hash, machine load, and daemon configuration.
- Start the actual daemon with its leader watcher; wait for selected status. For each size, perform 20 counted mixed edits **only within #67's proven-local same-path body-only class**. Record the edited path and old revision before each write. Observe a different revision via `status` and check that revision's own native fact floors and selected source. Never substitute an explicit `index` command for the watcher.
- Measure wall time from completion of the file write to the first status observation of the new revision. This includes the actual small debounce and status polling interval; record both. Record each latency and use nearest-rank p95: sorted sample number 19 of 20. Medium must be ≤2 seconds; large ≤5 seconds. A rejected sample or failed fact floor cannot be discarded or replaced without an explicit logged reason and rerun.
- Run the timed job only after the orchestrator grants the exclusive reference-host slot. Report raw samples, nearest-rank p95, edit shapes, fact floors and pass/fail at run level. Do not turn an unmeasured gate green.

## Full-native fallback (separate series)

Measure a one-file declaration-surface edit that the #67 classifier does not prove local. Record file-write-to-new-status times, mode (`full`), full-native fact floors for each selected revision, corpus size and method. Report sample count and individual times separately from the proven-local p95. Do **not** claim the #68 declaration-surface p95 gate or expand #67 local eligibility here.

| Series | Corpus | Count | Raw latencies | Nearest-rank p95 | Native facts per selected revision | Result |
| --- | --- | ---: | --- | --- | --- | --- |
| Watcher proven-local | Medium | pending (target 20) | pending | pending; ≤2 s gate | pending | not run |
| Watcher proven-local | Large | pending (target 20) | pending | pending; ≤5 s gate | pending | not run |
| Declaration-surface full fallback | Medium/large, record separately | pending | pending | report only | pending | not run |
