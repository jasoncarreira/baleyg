# Three harder questions: exploratory selection results

## Bottom line

**Jev and Opus agreed on the main selections for two questions. On the hardest
question, Opus kept more of the requested write path; Jev focused on the state checks
and collapsed the actual writer functions.** The extra writer nodes answer the question,
so they are not merely adjacent detail. This is a useful tradeoff to review, not a
statistically supported model ranking.

The user clarified that these arrows are not a sequence diagram and that extra detail
is not useful unless requested. The shared instructions and human rubric were narrowed
accordingly. The review is now labelled **call-selection sketch, not sequence diagram**.
Models classify relevance; no model creates or rewrites call edges.

## Review

Run `npm run review:hard` from `tools/selection`, then open
http://127.0.0.1:8876/. Alternatively open `tests/fixtures/selection/outputs/hard-v1/review/index.html` directly.
Start with the third question, where the final model selections differ.

Panels are independently shuffled. Private keys remain beside each HTML file and are
not served. Run caveats are visible on partial-result panels. Source disclosures show
complete candidate bodies; omitted helpers are not automatically expanded.

| Question | Candidates | Baseline shown | Jev shown | Opus shown | Observation |
| --- | ---: | ---: | ---: | ---: | --- |
| Create-only versus replacement publication, including hook | 35 | 5 | 2 | 2 | Same main selections and all relevance labels |
| Lock reclaim checks and protected callback placement | 40 | 13 | 4 | 4 | Same main selections; less default clutter than baseline |
| Transition to protected write; checks around reobservation/rename | 73 | 3 | 5* | 8 | Opus retains more of the requested write path |

*Jev classified four functions essential; the seed `transition` is forced visible by
assembly, so its main diagram has five nodes. It labelled the seed supporting.

### Publication branches

Both select `writeProtectedFileAtomic` and `assertSafeTarget`. Both collapse four
helpers rather than showing the error constructor, path validation and directory sync
by default. They have identical classifications on this case. The branch/hook ordering
still requires source-backed branch annotations; the two-node sketch alone does not
answer it.

### Lock reclaim

Both select `withRunJsonLock`, `canStealRunJsonLock`,
`ownerlessLockIsReclaimable`, and `inspectLockOwnerLiveness`. These expose the distinct
owned/ownerless policies without the baseline's 13-node tangle. Jev marks 15 other
candidates supporting versus Opus's 7; do not reward extra collapsed context by default.
Neither selection alone states the same-host TTL conditions or distinguishes that
policy from actual process-death detection. Those need the source/branch layer.

### Protected-write path and two checks

Jev's essential set is the coordinator, its lock callback, the injected rename
function, and `assertUnchanged`. It relegates `transition`, `readRunState`,
`writeProtectedJsonAtomic` and `writeProtectedFileAtomic` to supporting context.

With the full input delivered inline, Opus selects those same four plus `transition`,
`readRunState`, `writeProtectedJsonAtomic` and `writeProtectedFileAtomic`. That better
covers the question's explicit “how does transition reach the protected write” part.
Jev's pruning risks losing that requested path, even though it finds the interesting
checks. Both omit `withRunJsonLock` from the essential set; the provisional rubric
identifies it as an explanation anchor, so neither is an unquestioned perfect answer.

The graph has unresolved injected calls and callbacks. The sketches intentionally do
not invent direct edges to make selected nodes look connected. A real sequence view
needs ownership, branch frames, evaluation semantics and explicit dynamic boundaries.

## Inputs and fairness

- Complete source from three, three and five manually scoped files, respectively.
- All 35/40/73 eligible candidates included; zero candidate omissions or truncated
  bodies. The old q04 omission of `rename` and `assertUnchanged` is fixed.
- Source appears once in the provider packet, not repeatedly under nested functions.
  Local review packets retain full per-function source for human inspection.
- Compact packet bytes: 46,911 / 46,051 / 90,896. Omitting null/empty call fields is
  lossless, tested by decoding back to original measured calls.
- Jev and the selected Opus outputs have equal provider-packet and instruction hashes
  for each question. Human rubrics are not sent to either provider.
- Retrieval scope is manually supplied, so this is not an agent-discovery comparison.
- Identical supplied packets do not guarantee identical model-visible context after
  SDK processing. We found an important exception described below. Early q02/q03
  tool responses were not size-audited, so their delivery fidelity is not fully proven.

The source-grounded, question-specific human criteria are in
[HARD-QUESTIONS.md](HARD-QUESTIONS.md) and `hard-rubric.json`. They were prepared before
model-output review. They are provisional, and symbol presence is not fact correctness.

## Latency and completion status

| Selected output | End-to-end time | Completion |
| --- | ---: | --- |
| Jev publication branches | 0.78 s | Valid response, recorded diagnostic retry |
| Opus publication branches | 21.98 s | Validated emission, then terminal error |
| Jev lock reclaim | 0.81 s | Valid response |
| Opus lock reclaim | 12.45 s | Normal end_turn |
| Jev transition checks | 1.10 s | Valid response |
| Opus transition checks, full inline context | 27.33 s | Validated emission, then budget terminal error |

Opus values include startup, protocol handling and post-emission work. These are not
isolated inference latencies or repeat-run percentiles. Two Opus views are explicitly
**partial runs**, recovered from schema-validated MCP emissions; terminal failure is
preserved. We did not regenerate those classifications to obtain a more favorable answer.

## Transport findings and every repair

1. The first Jev publication-branch response failed our probability-sum check. Its
   body was not retained, which was an instrumentation mistake; the cause cannot be
   diagnosed retrospectively. The recorded diagnostic retry passed. All subsequent
   Jev responses are saved before validation. This is not a calibration result.
2. The first Opus publication run emitted a valid selection, then ended with a terminal error. Its reported usage exceeded the configured
   SDK threshold, but the exact SDK error category was not captured for that attempt. `recover-emission.mjs` validates packet identity, audited
   emission and every candidate label before producing a derived, explicitly partial
   review result. The original failed terminal record is untouched.
3. The first Opus transition run made 41 calls (one `get_candidates`, then forty
   `get_candidate`) and emitted no selection. It is not scored as an empty selection.
4. Claude documents an MCP output limit, with large results persisted to files. This
   restricted session has no filesystem-read tool. Raising `MAX_MCP_OUTPUT_TOKENS` to
   64,000 did not solve delivery: instrumentation observed a saved-output marker and
   only 2,312 text bytes in the returned preview. That run marked 66/73 candidates
   uncertain. It is excluded from the model-quality comparison; missing evidence is
   not a model-reasoning failure. Why persistence remained active is unresolved.
5. One final corrective run supplied the exact same full provider packet directly in
   the ACP prompt. MCP remained the validated output surface. It made one `emit_view`
   call, supplied all 73 labels, then hit the SDK budget threshold. The accepted
   emission is the explicitly partial Opus view used for the third question.

The final inline run still grew from approximately 42–44k context usage to 157k after
an acknowledgement of only 34 bytes. Thus the earlier ~113k growth is **not explained
solely by the large MCP response**. Its cause remains unresolved. No further paid
attempts were made. Future work should audit SDK context assembly before expanding
this ACP classification experiment; do not enable arbitrary file tools as a shortcut.

## Budget and validation

All attempts, including errors, remain in `tests/fixtures/selection/outputs/budget.json` and their artifacts.
Final provider-reported token estimates and conservative unknown-cost reservations
account for **$9.9519955 of the authorized $10** across the smoke and harder tests.
These are not confirmed cash charges on the user's subscription. Known failed-turn
estimates were reconciled with explicit artifact references and before-ledger backups;
no attempt was deleted. The final SDK report exceeded its local reservation by
$0.0598305, and all further inference is disabled. The SDK's dollar threshold is not a
hard invoice cap.

`npm test`: **39 offline tests passed**. These cover full-input audits, lossless
compaction, validators, budget controls, MCP tools, model selection, inline delivery,
process cleanup, partial emissions, failure panels and safe HTML. The review was
checked in a browser at desktop and mobile widths. The HTML-only server rejects blind
keys and `.env`. No source repository was edited, no login/account switch was
performed, and no secret value was intentionally written to an experiment artifact.

## What to do with this evidence

Jev is promising for fast, bounded view filtering. It matched Opus's main selections
on two richer cases and found the deep callback/check nodes on the third. But aggressive
pruning can remove the write path that the user explicitly asked to understand.

Before choosing a production selector, review the third case for that tradeoff. Then
add question-aware path-preservation constraints and the missing sequence semantics.
Keep classification quality, input delivery, and diagram usefulness as separate tests.
This three-question exploratory comparison is not a replacement for the full benchmark.
