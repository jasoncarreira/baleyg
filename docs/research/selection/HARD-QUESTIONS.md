# HARD question rubrics

**Provisional human-grounded assessment. Not objective correctness truth.**

> None of the extra stuff is useful unless I ask about it specifically.

These rubrics narrow the existing `questions.json` requirements without changing the questions. Extra true context is not a requirement. Default views should keep optional context collapsed. Required candidates are explanation anchors, not a transitive call-graph checklist. Compact groups and source annotations may preserve anchors without separate cards.

Evidence comes from the full source texts in `tests/fixtures/extraction/feature-factory.graph.json` and the three `tests/fixtures/selection/inputs/hard-v1/` packets (the shared provider payload includes full source once in `sourceFiles`). No model outputs or `.env` were read. No inference was run. Snapshot file hashes and structured evidence are in `hard-rubric.json`.

All three full-source snapshots and all three new hard-v1 input packets support the minimal facts below. The packets contain all 35, 40 and 73 eligible candidates respectively, with no truncation or omitted candidates. Every cited source file matches the extraction snapshot; all required candidate anchors are present, including q04 `rename` and `assertUnchanged`. These findings were made independently of model answers. Symbol selection alone cannot express the requested guards and order.

## q02-atomic-branches

How does protected file publication differ between create-only and replacement writes, including the beforeCommit hook?

### Minimal necessary facts

1. createOnly is enabled only by options.createOnly === true. Create-only rechecks target absence, awaits beforeCommit if it is a function, then publishes the temporary file with the selected link operation; EEXIST is rejected rather than replacing a target. The temporary link is then removed. Evidence: `core/atomic-write.js:25-28`; `core/atomic-write.js:45-59`.
2. Replacement awaits beforeCommit if it is a function, then rechecks target safety, then calls the selected rename operation. Safety permits an absent target, but an existing target must be a regular file; create-only rejects any existing target. Evidence: `core/atomic-write.js:61-66`; `core/atomic-write.js:106-117`.
3. beforeCommit is an optional dynamic callback. link and rename come from options.fsOps when provided, otherwise the imported filesystem operations; the graph does not resolve these selected calls to fixed callees. Evidence: `core/atomic-write.js:7-7`; `core/atomic-write.js:26-28`; `core/atomic-write.js:45-64`.

### Essential explanation anchors

- `core/atomic-write.js::writeProtectedFileAtomic` — Owns the requested branch and hook ordering.

### Optional context — omit unless needed or asked

- Both branches stage and sync an exclusive temporary file before publication. Evidence: `core/atomic-write.js:31-43`.
- Directory sync and cleanup error handling are shared or failure-path details, not needed to explain the requested publication contrast. Evidence: `core/atomic-write.js:53-59`; `core/atomic-write.js:67-85`; `core/atomic-write.js:119-134`.

### Acceptable collapsed details

- `assertSafeTarget` — Keep absence-versus-regular-file semantics in a branch annotation; a separate helper node is unnecessary.
- `resolveProtectedPath` — Path validation internals are outside the requested contrast.
- `syncDirectory` — Directory-sync implementation and tolerated error codes are optional, not required.
- `constructor` — Error class implementation is not needed.

### Unrelated examples

- Run-lock acquisition, stale-lock reclaim and release helpers.
- Transition contracts, projections and CAS comparisons.

### Source and representation limits

- Selecting the writer or its safety helper alone cannot express the opposite hook/check ordering in the two branches. Show a source-backed branch annotation.
- Do not claim observed publication, race freedom, or guaranteed power-loss durability.

**Input packet (hard-v1): complete for the minimal requested facts.** Full `sourceFiles` matches the extraction snapshot and all required anchors are present. No source-evidence gaps found. Symbol selection alone still cannot convey the required guards and ordering.

## q03-lock-reclaim

Which checks allow withRunJsonLock to attempt reclaiming an existing lock, and where is the protected callback invoked?

### Minimal necessary facts

1. After mkdir reports EEXIST, reclaim eligibility is considered only while stealAttempted is false. The loop reads directory identity and owner evidence; a qualifying owned or ownerless case sets stealAttempted, and a non-null observed directory identity is required to call stealByRename. This is an attempt, not successful acquisition. Evidence: `core/run-lock.js:36-56`; `core/run-lock.js:218-226`.
2. The owned-lock case requires a durable owner and liveness classified as dead: valid owner record, same host, finite acquired_at age strictly greater than the stale TTL. This is a timestamp policy, not an OS process-death probe; foreign-host or invalid owners are indeterminate. Evidence: `core/run-lock.js:79-94`; `core/run-lock.js:174-185`; `core/run-lock.js:197-205`.
3. The ownerless case requires no usable owner evidence AND no owner entry, plus finite directory mtime age strictly greater than the grace period. An invalid but existing owner entry is not reclaimable through this branch; inspection errors fail closed. Evidence: `core/run-lock.js:49-51`; `core/run-lock.js:83-94`; `core/run-lock.js:187-195`; `core/run-lock.js:251-260`.
4. After acquiring the directory, withRunJsonLock verifies its identity, exclusively publishes owner.json and verifies the owner. It then invokes and awaits the supplied fn({ lock_dir: lockDir, owner }) at line 69, inside try before finally cleanup. fn is dynamic, not a statically resolved project function. Evidence: `core/run-lock.js:59-76`.

### Essential explanation anchors

- `core/run-lock.js::withRunJsonLock` — Owns EEXIST gating, the two eligibility branches, and callback placement.
- `core/run-lock.js::canStealRunJsonLock` — Defines the owned-lock eligibility predicate.
- `core/run-lock.js::inspectLockOwnerLiveness` — Defines the same-host TTL policy needed to explain dead.
- `core/run-lock.js::ownerlessLockIsReclaimable` — Defines the distinct ownerless grace policy.

### Optional context — omit unless needed or asked

- After an attempt is requested, stealByRename optionally awaits onBeforeSteal, rechecks owned evidence, and uses identity-checked quarantine rename. These are attempt-execution safeguards, not additional eligibility branches in withRunJsonLock. Evidence: `core/run-lock.js:111-135`; `core/run-lock.js:157-168`.
- Exact owner validation fields are positive integer pid, nonblank hostname, parseable acquired_at and UUID-shaped nonce; numeric TTL/grace defaults and timeout/retry policy are implementation context. Evidence: `core/run-lock.js:11-14`; `core/run-lock.js:54-55`; `core/run-lock.js:197-205`.

### Acceptable collapsed details

- `readLockOwnerEvidence` — Summarize usable owner evidence versus an existing invalid entry.
- `isDurableLockOwner` — Label valid durable owner; do not expand every field unless asked.
- `lockOwnerEntryExists` — Label no owner entry and fail-closed inspection.
- `lockDirectoryIdentity` — Label non-null directory identity without expanding lstat mechanics.
- `stealByRename` — Show the attempted operation by name; its quarantine implementation is optional.
- `sameLockOwner` — Summarize publication verification at callback placement.
- `sameLockDirectoryIdentity` — Summarize identity verification at callback placement.
- `releaseOwnedRunJsonLock` — Finally cleanup can stay collapsed.
- `quarantineAndRemoveOwnedLock` — Unpublished-owner cleanup is not requested.
- `renameOwnedLockToQuarantine` — Quarantine mechanics are not needed to identify eligibility.
- `normalizePositiveInteger` — Timing validation internals are not required.
- `delay` — Retry scheduling internals are not required.

### Unrelated examples

- state/session-lock.js: separate lock domain and policy.
- coordinateRunJsonTransition, projections and protected-write CAS logic.

### Source and representation limits

- A list of reclaim helper symbols cannot express EEXIST, the attempt gate, the AND/OR conditions, strict age comparisons, or callback placement. Keep source-backed guard labels.
- Do not equate TTL dead with a proven dead process or a reclaim attempt with successful lock ownership.

**Input packet (hard-v1): complete for the minimal requested facts.** Full `sourceFiles` matches the extraction snapshot and all required anchors are present. No source-evidence gaps found. Symbol selection alone still cannot convey the required guards and ordering.

## q04-transition-cas

How does transition reach the protected write, and where are the two unchanged-state checks relative to reobservation and final rename?

### Minimal necessary facts

1. transition delegates to coordinateRunJsonTransition. The coordinator passes an async callback to withRunJsonLock; that callback reads initial state, applies/validates the candidate and calls writeProtectedJsonAtomic. The JSON wrapper delegates to writeProtectedFileAtomic with options preserved. The write call belongs to the lock callback, not directly to the coordinator. Evidence: `state/transition.js:13-25`; `core/write-core.js:23-47`; `core/atomic-write.js:18-20`.
2. The lock callback supplies fsOps.rename as a nested async function. The atomic writer selects options.fsOps.rename when present; this call uses replacement mode, whose optional beforeCommit hook and safety check precede the selected rename call. This source wiring identifies the intended callback without turning the unresolved graph call into a resolved edge. Evidence: `core/write-core.js:44-80`; `core/atomic-write.js:25-28`; `core/atomic-write.js:61-65`.
3. Inside the injected rename callback, reread state and compare it with initial before projecting and awaiting contract.reobserve. After reobservation, reread again and compare with the same initial state. assertUnchanged rejects anything not deeply strictly equal. Evidence: `core/write-core.js:47-64`; `core/write-core.js:71-72`; `core/write-core.js:85-94`.
4. After the second unchanged-state check, an optional synchronous finalGuard runs (thenable results are rejected); then the callback calls the imported filesystem rename. Distinguish this final filesystem call from the atomic writer invoking the injected rename callback. Evidence: `core/write-core.js:1-2`; `core/write-core.js:71-77`.

### Essential explanation anchors

- `state/transition.js::transition` — Entry point and delegation.
- `core/write-core.js::coordinateRunJsonTransition` — Owns the lock call and callback registration.
- `core/run-lock.js::withRunJsonLock` — Names the lock boundary on the requested write path; its internals may stay collapsed.
- `core/write-core.js::<callback@23:34>` — Owns candidate preparation, protected JSON write and injected rename registration.
- `core/atomic-write.js::writeProtectedJsonAtomic` — Connects the protected JSON write to the file writer.
- `core/atomic-write.js::writeProtectedFileAtomic` — Selects and invokes the injected rename in replacement mode.
- `core/write-core.js::rename` — Nested callback at lines 47-78, not the imported filesystem function; owns both comparisons and reobservation.
- `core/write-core.js::assertUnchanged` — Defines what the requested unchanged-state comparisons mean.

### Optional context — omit unless needed or asked

- Descriptor freezing, contract registries, projection validation and recursive freezing support transition validation but their implementations are not needed for this path/order question. Evidence: `state/transition.js:14-17`; `core/write-core.js:18-42`; `core/write-core.js:96-146`.
- JSON pretty-printing and trailing newline are wrapper details, not required to follow delegation. Evidence: `core/atomic-write.js:18-20`.

### Acceptable collapsed details

- `readRunState` — Label each reread in the rename callback; parsing/validation helper internals can stay collapsed.
- `projectAll` — Label projection before reobservation without expanding projection validation.
- `deepFreeze` — Recursive freezing is not needed to show either comparison.
- `contractRegistry` — Registry validation is not the requested path or check placement.
- `participantRegistry` — Participant validation is not the requested path or check placement.
- `assertSafeTarget` — Label the safety check before injected rename; no separate expansion required.
- `withRunJsonLock internals` — Keep the required lock boundary but collapse reclaim/release internals.

### Unrelated examples

- core/contracts.js individual family contract implementations; no need to resolve dynamic contract.reobserve methods.
- Stale-lock liveness, ownerless grace and quarantine cleanup.
- Create-only publication and directory-fsync error lists.
- Git push configuration and executable lookup.

### Source and representation limits

- Selection alone cannot encode callback ownership, two separate read/check callsites, or reobservation between them. Supply source-backed ownership and order annotations.
- contract.reobserve, finalGuard and the selected options.fsOps.rename remain dynamic/unresolved graph calls. Do not invent direct coordinator-to-writer or atomic-writer-to-injected-callback edges as resolved lexical edges.
- Source order is not an observed execution trace, global execution order, or a formal atomic CAS guarantee.

**Input packet (hard-v1): complete for the minimal requested facts.** Full `sourceFiles` matches the extraction snapshot and all required anchors are present. No source-evidence gaps found. Symbol selection alone still cannot convey the required guards and ordering.

## Changes from the broad requirements

- q02: path-validation internals, temporary-file staging and directory-fsync error lists are not mandatory. Keep the publication contrast and hook/check ordering.
- q03: release and quarantine mechanics are not mandatory. Keep eligibility guards and the callback invocation site. A malformed existing owner file is not an ownerless-reclaim shortcut.
- q04: descriptor construction, all projection helpers and family contracts are not mandatory. Keep the callback-owned write path, injected rename distinction, both comparisons, reobservation and final rename placement.

The previous head-tail packets are not the evidence baseline for this audit. Do not score a correct list of symbols as a complete explanation of branch guards or ordering. These are human review judgments, not automatic correctness labels.
