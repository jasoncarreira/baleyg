# Selection smoke results

## Outcome

**The three-way experiment is set up and its first smoke check is complete.**
This is not an approved benchmark, calibration study, or evidence that one selector
is better. The ten-question draft still needs human review and input-coverage work.

Question: **Which function does writeProtectedJsonAtomic directly delegate to, and how does it prepare the data?**

All three selectors received the same 35-candidate packet. They all showed
`writeProtectedJsonAtomic` and its direct callee `writeProtectedFileAtomic`. The diagram
contains the one measured direct call; serialization details are available in the
source disclosure, not inferred from a symbol name.

| Approach | Essential | Supporting | Incidental | End-to-end latency | Reported/estimated USD |
| --- | ---: | ---: | ---: | ---: | ---: |
| Deterministic one-hop baseline | 2 | 4 | 29 | Not timed | 0 |
| Jev 1.13.0 | 2 | 0 | 33 | 0.88 s | 0.00137029 |
| Opus via Claude ACP | 2 | 4 | 29 | 28.95 s | 1.532102 |

Baseline and Opus classifications were identical on this easy question. Jev marked
the four lower-level helpers incidental rather than supporting. The main diagram is
identical in all three panels. A human has not yet scored fact completeness or utility.

Jev latency is one HTTPS request. ACP latency includes process/session startup,
model configuration and multiple agent turns. This is useful integration data, not a
controlled model-compute benchmark. There were no repetitions, so no p95/stability
claim can be made.

## Review artifacts

- [Provisional questions and rubric](QUESTIONS.md)
- [Reproduction and safety controls](../../../tools/selection/README.md)
- [Compact measured evidence](SMOKE-RESULTS.json)
- Local blinded review: `tests/fixtures/selection/outputs/smoke-review/review.html`
- Private panel key: `tests/fixtures/selection/outputs/smoke-review/blind-key.json` (do not open before rating)
- Raw provider and baseline results: `tests/fixtures/selection/outputs/q01-smoke.*.json`

Open the review HTML directly, or run `npm run review` from this directory and visit
http://127.0.0.1:8875/review.html. The server binds to loopback and serves only the
review HTML, not the blind key or `.env`. It was browser-tested at 1440px and 390px;
source disclosures work and provider metadata is absent from the page.

## ACP integration repair — included, not hidden

The first ACP attempt authenticated the existing team subscription, but did not emit
a valid view. Its session config reported Opus despite the Sonnet option, it denied one
permission, and its cumulative token-equivalent estimate was $0.523703.
The original result remains `tests/fixtures/selection/outputs/q01-smoke.claude.json`; no failed output is scored.

Before a second paid attempt:

- The user clarified that Opus use above the initial $2 reservation is fine on their
  subscription. The corrected run explicitly selected and verified `opus[1m]` through
  ACP configuration, rather than assuming SDK metadata chose the model.
- The SDK usage threshold became $3, with four turns and a 4,096-output-token ceiling.
- The local ledger reserved $5, while retaining the failed attempt's $2 reservation.
- The duplicate full packet in MCP text/structured content was removed. We cannot
  attribute all observed token overhead to that duplication from available telemetry.
- Permission handling can approve only exact experiment MCP tool names with validated
  arguments and an `allow_once` option; all other requests fail closed. Built-in tools,
  client filesystem and terminal capabilities remain disabled. The corrected live run
  had no permission requests or denials and ended normally with a validated emission.

Corrected output: `tests/fixtures/selection/outputs/q01-smoke.claude-v2.json`. No API-key fallback, login,
account switch, automatic retry or unrestricted source tools were used.

## Costs and token footprint

Observed estimates including the failed attempt total **$2.057175**.
The conservative ledger currently accounts for **$3.632102** against the
original $10 experiment cap because uncertain costs retain reserved allowance.
**These are not confirmed invoices.** Claude's reported amount is token-equivalent
usage on the user's subscription, not proof of a cash charge. Jev's estimate uses its
published input-token rate.

Jev reported 32,626 input tokens and
1,854 output tokens (output is free on the published rate card).
ACP reported 394,252 aggregate tokens, including
257,280 cache reads and
134,791 cache writes; its last context report was roughly
135k tokens. That is unexpectedly large relative to the compact ~64KB packet. Investigate
ACP/MCP serialization, tool responses and multi-turn accounting before scaling to all
questions. This smoke has no tool-call trace sufficient to explain that overhead fully.

## Validation and security

- `npm test`: **20 offline tests passed**.
- Tests cover decision validation, budget fail-closed behavior, MCP stdio, process-group
  cleanup, explicit model selection, narrow permissions, safe rendering and blind keys.
- Generated experiment files were checked for the known Jev key: no matches.
- Root `.env` is ignored. Jev's key is not inherited by the ACP subprocess.
- The comparison server returned 404 for the blind-key and `.env` routes.
- The source graph/snapshot was reused; no feature-factory source changes were made.

## Next gate

1. Review the ten questions and their human-only facts in `QUESTIONS.md`.
2. Resolve input coverage before judging harder cases. `review-readiness.json` flags
   truncated bodies and omitted candidates; q04 currently omits an `assertUnchanged`
   candidate. Missing evidence must not be scored as a model-selection failure.
3. Investigate ACP's context overhead and add tool-call/phase timing audits before
   spending on the wider run.
4. Predeclare repetitions, correctness/usefulness criteria and cost accounting, then
   run the reviewed benchmark. Keep this easy smoke separate from its results.
