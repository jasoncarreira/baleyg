# View-selection benchmark — provisional human rubric

**Status: DRAFT. User review required before this is a benchmark.**
No inference calls were made to draft these questions. These are expected
source-supported facts, not measured model performance or approved gold labels.

## Scope and evidence

- Source root: `tests/fixtures/extraction/inputs/feature-factory/`.
- Graph: `tests/fixtures/extraction/feature-factory.graph.json`.
- Every JSON path is relative to the source root. Lines are 1-based and inclusive.
- `questions.json` is an array of 10 records. `q01-smoke` is the easy smoke test.
- Each seed is a named graph node in its specified file. Candidate paths are a
  deliberately bounded search surface, not an assertion that every file is relevant.
- The graph describes static lexical callsites, not execution order. Callback bodies
  are separate functions. It has no points-to analysis.

## Prevent label leakage

`questions.json` is a **human-review fixture**, not a provider prompt. Provider input
must use an explicit allowlist: `id`, `question`, `seed`, and `candidate_paths`, plus
whatever source/graph context the experiment gives every compared method. Never pass
`must_show`, `distractors`, `limitations`, this document, or prior human ratings to a
selection provider. Do not serialize whole question records into prompts. Keep the
rubric in the evaluator path and review selected diagrams only after generation.
Use the same seed and candidate scope for every compared selector.

## Human review procedure

1. Confirm each question and cited source fact. Edit or reject the draft before use.
2. Inspect the selected view with its source-backed annotations and callsite links.
3. For each must-show item, record **shown**, **partial**, **missing**, or
   **contradicted**, and point to the displayed evidence. Do not award a fact merely
   because its symbol name appears. A present source link with no visible fact is
   partial until a reviewer can verify what the view actually exposes.
4. Classify evidence correctly: a callsite supports a lexical call; a condition or
   branch supports a guard; passing a function as data is not invoking it. An async
   callback's calls belong to that callback. Unresolved dispatch stays unresolved.
5. Judge distractors by their role in this question, not by name similarity. A
   distractor can be useful context; penalize only clutter or a false relationship.
6. Record unsupported claims separately, especially invented direct calls, omitted
   conditional alternatives, runtime order asserted from graph edges, and success
   inferred from a guard's presence.
7. Record an overall judgment: **sufficient**, **needs source/branch detail**, or
   **misleading**. No numeric pass threshold is approved yet. Report guard-heavy
   questions separately if a view supports call graphs only; this can be a view
   capability limit rather than selection failure.

The `symbol_names` field is a review aid, not a complete node allowlist or a demand
that every named symbol have a node. Some facts require source regions, labels,
repeated callsites, external calls, or explicitly unresolved injected callbacks.
Do not invent resolved edges to satisfy the rubric. Do not infer missing callers
from a function definition or validation code.

## Questions and provisional must-show facts

### q01-smoke: Which function does writeProtectedJsonAtomic directly delegate to, and how does it prepare the data?
Seed: `core/atomic-write.js::writeProtectedJsonAtomic`.
Candidate paths: `core/atomic-write.js`, `core/write-core.js`, `core/run-lock.js`.

Must show:
- Direct lexical call: writeProtectedJsonAtomic returns writeProtectedFileAtomic with rootDir, relativePath, JSON.stringify(value, null, 2) plus a newline, and options. (`core/atomic-write.js:18-20`)

Potential distractors:
- coordinateRunJsonTransition: higher-level state coordination, not the wrapper callee.
- withRunJsonLock: lock acquisition is not performed by this wrapper.

Limits:
- Showing the direct edge is necessary; serialization details need a source-backed label or excerpt.
- This wrapper does not prove publication success or runtime execution.

### q02-atomic-branches: How does protected file publication differ between create-only and replacement writes, including the beforeCommit hook?
Seed: `core/atomic-write.js::writeProtectedFileAtomic`.
Candidate paths: `core/atomic-write.js`, `core/write-core.js`, `core/run-lock.js`.

Must show:
- writeProtectedFileAtomic resolves the path and checks the target before opening an exclusive temporary file, writing it, and syncing it. (`core/atomic-write.js:22-43`)
- Branch/source evidence: create-only rechecks absence, invokes an optional beforeCommit callback, then publishes by link; replacement invokes that callback, rechecks target safety, then calls rename. (`core/atomic-write.js:45-66`)
- Target safety rejects an existing create-only target or a non-file replacement target; ENOENT is permitted. (`core/atomic-write.js:106-117`)
- After publication, the function calls syncDirectory; selected directory-sync errors are tolerated. (`core/atomic-write.js:85-86`)
- syncDirectory tolerates EINVAL, EPERM, EISDIR, EACCES and ENOTSUP, rather than promising universal directory fsync. (`core/atomic-write.js:119-129`)

Potential distractors:
- releaseOwnedRunJsonLock: lock cleanup is not temporary publication cleanup.

Limits:
- link and rename can be injected via options.fsOps; do not always label them native filesystem calls.
- beforeCommit is an optional awaited callback, not a resolved direct edge to a named project function.
- A call graph alone cannot prove race freedom or power-loss durability.

### q03-lock-reclaim: Which checks allow withRunJsonLock to attempt reclaiming an existing lock, and where is the protected callback invoked?
Seed: `core/run-lock.js::withRunJsonLock`.
Candidate paths: `core/run-lock.js`, `core/write-core.js`, `state/session-lock.js`.

Must show:
- When mkdir reports EEXIST, the acquisition loop reads directory identity and owner evidence; it attempts stealByRename for a stealable owner or a reclaimable ownerless lock. (`core/run-lock.js:36-56`)
- canStealRunJsonLock requires a durable owner whose inspected liveness is dead. (`core/run-lock.js:79-81`)
- Liveness is a same-host acquired_at TTL comparison, not an OS process-liveness probe; foreign hosts and invalid owners are indeterminate. (`core/run-lock.js:174-185`)
- Ownerless reclaim requires no owner entry and a directory age exceeding the grace period. (`core/run-lock.js:251-260`)
- The owner is published and checked before awaiting fn; the finally block releases a published owner or conditionally cleans up an unpublished lock. (`core/run-lock.js:61-76`)

Potential distractors:
- state/session-lock.js: a separate lock domain; do not substitute its policy for run-json.lock.

Limits:
- fn is an injected async-capable callback; lexical graph edges do not resolve its runtime identity.
- TTL policy is source guard evidence, not proof that a process is actually dead.

### q04-transition-cas: How does transition reach the protected write, and where are the two unchanged-state checks relative to reobservation and final rename?
Seed: `state/transition.js::transition`.
Candidate paths: `state/transition.js`, `core/write-core.js`, `core/atomic-write.js`, `core/run-lock.js`, `core/contracts.js`.

Must show:
- transition builds a frozen descriptor and directly calls coordinateRunJsonTransition with contracts, validateRun, reobservers, hooks and finalGuard. (`state/transition.js:13-25`)
- The coordinator calls withRunJsonLock with an async callback. Reading, applying, validating and writeProtectedJsonAtomic occur inside that callback, not as direct lexical coordinator calls. (`core/write-core.js:23-47`)
- The injected fsOps.rename async function reads and checks unchanged state before contract.reobserve, then reads and checks again after reobservation. (`core/write-core.js:47-72`)
- The optional finalGuard runs after the second comparison, rejects a thenable return, and is followed by the imported filesystem rename call. (`core/write-core.js:71-77`)
- assertUnchanged rejects states that are not deeply strictly equal. (`core/write-core.js:85-89`)
- writeProtectedFileAtomic takes options.fsOps.rename and invokes that chosen function in the replacement branch. (`core/atomic-write.js:25-28`)
- Replacement publication calls the selected rename after the optional hook and target check. (`core/atomic-write.js:61-65`)

Potential distractors:
- configure in core/effective-push.js: Git configuration is unrelated to run.json CAS.

Limits:
- The graph has no points-to analysis: injected rename and contract methods may remain unresolved. Show callback ownership instead of inventing direct coordinator edges.
- Source order in the rename callback is not global execution-order proof or a formal atomic CAS guarantee.

### q05-push-target: How do bootstrap and check differ when enforcing the effective push target, and why are targets compared as bytes?
Seed: `core/effective-push.js::enforceEffectivePushTarget`.
Candidate paths: `core/effective-push.js`, `bin/factory.js`, `core/executable.js`.

Must show:
- Both operations capture the operator target. Only bootstrap checks argvSafe, configures the sandbox, and recaptures the operator target; both capture the sandbox and require byte equality. (`core/effective-push.js:59-87`)
- capture uses remote get-url --push origin, rejects failed results, and removes exactly one required LF before returning nonempty bytes. (`core/effective-push.js:23-41`)
- argvSafe checks a UTF-8 round trip, while configure calls execute with config --replace-all remote.origin.pushurl. (`core/effective-push.js:47-56`)
- execute invokes the injected run function with git, shell:false, C locale and piped output; no encoding is supplied. (`core/effective-push.js:11-20`)

Potential distractors:
- resolveSpawnExecutable: executable lookup is not effective remote comparison.

Limits:
- run defaults to spawnSync but is injectable; do not treat every indirect run call as a resolved edge.
- No git push is executed here; equality checking is not evidence that a push occurred.

### q06-review-binding: How do assertReviewBinding and readValidatorReview differ in checking verdicts while binding reviews to an observed commit?
Seed: `observe/review.js::assertReviewBinding`.
Candidate paths: `observe/review.js`, `state/schema.js`, `observe/index.js`.

Must show:
- assertReviewBinding optionally checks subject and attempt, calls isApproving, requires a SHA-shaped observed head and equality with reviewed_commit. (`observe/review.js:68-87`)
- isApproving checks the uppercased string against APPROVING_VERDICTS. (`observe/review.js:61-63`)
- readValidatorReview calls reviewRef and readReview, checks the implementation-validator subject and VALIDATOR_VERDICTS membership, then observed head shape and equality. It does not call assertReviewBinding. (`observe/review.js:100-115`)

Potential distractors:
- observeMergeProof: merge-content verification is separate from review-record binding.

Limits:
- Accepted verdict constants are data references, not function-call edges.
- These guards do not independently observe Git HEAD; they check the supplied observedHead.

### q07-merge-proof: Which observations and comparisons does observeMergeProof use to accept a two-parent merge without requiring whole-tree equality?
Seed: `observe/review.js::observeMergeProof`.
Candidate paths: `observe/review.js`, `observe/index.js`, `state/transition.js`.

Must show:
- observeMergeProof calls observeAncestry and revList, rejecting non-ancestor observations, unavailable parents, or a parent count other than two. (`observe/review.js:143-161`)
- It calls pathsChanged for base-to-reviewed and first-parent-to-merge and rejects unavailable, extra or missing changed paths. (`observe/review.js:163-176`)
- It compares reviewed-to-merge drift and rejects drift only where it intersects reviewedPaths before returning proven:true. (`observe/review.js:206-213`)
- revList and pathsChanged call git; pathsChanged uses --literal-pathspecs diff --name-only -z and splits NUL-delimited output without trimming filenames. (`observe/review.js:216-230`)

Potential distractors:
- assertReviewBinding: it validates a record binding, not merge path/content comparisons.

Limits:
- A generic pathsChanged edge cannot alone explain its three distinct argument pairs; show callsites or source labels.
- This is not proof that every change anywhere on the integration branch was reviewed.

### q08-publication: What evidence dependencies and conditional alternatives must a view show for assertPublicationReady, including the repair-tested-head shortcut?
Seed: `observe/review.js::assertPublicationReady`.
Candidate paths: `observe/review.js`, `observe/repair-record.js`, `observe/index.js`, `state/schema.js`.

Must show:
- Source guards reject parked or terminal state, unapproved gates, an empty slice plan and unmerged slices. (`observe/review.js:245-267`)
- The supplied observeHead callback is invoked; a multi-slice run requires a validator and any recorded validator must approve and bind to that head. (`observe/review.js:271-283`)
- Without a validator, a recorded pre_pr reviewed_head must match; an existing test-verifier step must be accepted. (`observe/review.js:288-300`)
- The direct call to assertRepairPublicationReady can satisfy tested-head evidence and return early when repair.tested equals head. (`observe/review.js:302-308`)
- Otherwise evidenceRef and readEvidence obtain test-verifier evidence; guards require matching subject, observed tests, zero exit, review_ready and commit equal to head. (`observe/review.js:318-334`)

Potential distractors:
- readValidatorReview: not called by assertPublicationReady; this function checks the already-recorded state.validator.
- observeMergeProof: not a direct publication-readiness callsite here.

Limits:
- Guard conditions require branch/source annotations; listing callees alone is insufficient.
- This question concerns the readiness function, not whether every CLI publication path invokes it.
- observeHead is injected; do not label it a direct Git observation without caller evidence.

### q09-repair-lifecycle: Where does reverifyRepair hold the run lock, and how are detached execution and evidence publication separated?
Seed: `observe/repair-reverification.js::reverifyRepair`.
Candidate paths: `observe/repair-reverification.js`, `observe/repair-record.js`, `core/run-lock.js`, `core/atomic-write.js`, `observe/index.js`.

Must show:
- reverifyRepair reads repair state, checks eligibility, creates a detached worktree and asserts its detached state, with cleanup on failed assertion. (`observe/repair-reverification.js:69-82`)
- The first withRunJsonLock receives an async callback that rereads state and publishes a create-only marker through writeProtectedJsonAtomic, then verifies read-back. (`observe/repair-reverification.js:84-111`)
- After the first lock call resolves, spawnSync runs the configured trigger in the temporary worktree; Git HEAD and cleanliness are observed and the worktree is removed before the second lock call. (`observe/repair-reverification.js:117-133`)
- The second lock callback checks unchanged run/journal bytes and reservation, publishes create-only result evidence, rereads it and returns whether that attempt effectively passed. (`observe/repair-reverification.js:133-162`)
- After the second callback result, ineffective completion throws; success reports physical_status needs-human and effective_status verified. (`observe/repair-reverification.js:163-168`)

Potential distractors:
- coordinateRunJsonTransition: this function publishes evidence through the atomic writer, not through a transition call.

Limits:
- Marker and result writer calls belong to different async callbacks, not direct reverifyRepair call edges.
- Lexical source supports lock-scope placement, not an observed execution trace or actual passing test result.
- Returned effective_status does not establish that run.json was transitioned to verified.

### q10-executable-search: How does resolveSpawnExecutable choose direct-path versus PATH candidates, and what checks can return success?
Seed: `core/executable.js::resolveSpawnExecutable`.
Candidate paths: `core/executable.js`, `observe/index.js`, `core/effective-push.js`.

Must show:
- The function refuses win32, accepts injectable stat/access functions and uses a direct-path branch when argv0 contains a slash; otherwise it searches PATH or a POSIX default path. (`core/executable.js:10-20`)
- Candidate checks require stat(candidate).isFile() and access(candidate, X_OK); the first passing candidate returns ok:true and exhausted candidates return not-executable. (`core/executable.js:21-28`)

Potential distractors:
- execute in core/effective-push.js: it runs Git commands rather than resolving an executable candidate.

Limits:
- The map callback constructing PATH candidates is separate lexical ownership, not an external executable call.
- Injected stat/access are not necessarily resolved native calls. No process is spawned by this function.
- A passing check is not proof the executable remains available when another function later spawns it.
