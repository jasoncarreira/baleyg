# Source-backed answers with ACP

Jev selects useful evidence; an answer explains it. The ACP answer path receives the full
revision-bound question packet, not only the five visible call sites. Jev selection is not
a prerequisite: prepare the offline packet, then explicitly choose **Explain with ACP**.

The result separates a short answer, branch differences and limitations. Each answer/branch
claim has one or more source citations with exact quotes. Baleyg validates the packet ID,
file, inclusive line range and quoted text before display. Citation validation proves that
an anchor exists in the snapshot, **not** that the model's assertion logically follows.
Graph edges remain measured facts; an explanation is not a runtime sequence trace.

## Explicit opt-in and authorization

ACP is disabled by default. It uses the pinned Claude ACP adapter and an existing Claude
subscription login, with API-key fallback disabled. It never receives `JEV_KEY` or other
inherited provider credentials. The source-sharing confirmation is separate from **Run Jev**.
There are no automatic model calls or retries.

The ACP attempt allowance is separate from the $5 Jev authorization and the closed selection
experiment budget. Do not enable it until a separate ACP allowance has been authorized.
Each attempt is retained, including failures, timeout, cancellation and crashes. The configured
allowance is immutable across restarts. Model-reported costs and the SDK's per-run $1 estimate
limit are **not** subscription invoices or a guaranteed cash ceiling.

Install the pinned runtime dependencies once:

```sh
npm ci --prefix runtime/acp
```

After authorization, add all three flags to your usual `baleyg serve` command:

```sh
--acp-runner /absolute/path/to/baleyg/runtime/acp/runner.mjs \
--acp-state-dir /private/path/to/new-acp-allowance \
--acp-max-attempts <authorized-attempt-count>
```

The maximum supported allowance is 20 attempts. Use a new ACP-only directory, outside the
source workspace. Existing Jev or experiment ledgers must not be reused or reopened. The
runner is trusted application code, not an executable from the repository being inspected.

## Runtime boundary

- Explicit `sonnet` selection, existing subscription authentication only.
- No built-in tools, MCP servers, repository instructions, filesystem or terminal capabilities.
- Permission requests denied; no source-repository command execution.
- Empty private scratch directory and allowlisted environment.
- Bounded input/output, 120-second timeout, two-turn limit, no persistent agent session.
- Only complete successful terminal output is accepted. Partial/error emissions are failures.
- Independent native answer validation, pre/post-run revision checks, and stale UI guards.

This reduces the tool surface; it is not an OS capability sandbox for a malicious adapter.
Complete source evidence can contain secrets: review the scope before confirming sharing.

## API

`GET /api/acp/status` reports enablement and the separate attempt allowance.
`POST /api/questions/{packetId}/acp-answer` takes `{}` and uses only a server-cached packet.
The response contains `source: "liveAcp"`, packet/revision/attempt IDs, the structured answer,
latency and an optional cost estimate. Source, runner and budget overrides are not accepted.

See [the contract](acp-answer-contract.md) for schema and limits. ACP intent planning and
selective critique of Jev decisions remain separate future work; this slice writes answers.


## Host policy caveat

The runner sets `strictMcpConfig: true` so ambient MCP definitions are not merged, and
requests `settings: {disableAllHooks: true}`. Host-managed policy can override non-managed
settings. This boundary trusts the installed adapter and host policy; it does not claim to
sandbox a malicious adapter, disable mandatory managed hooks, or contain arbitrary `setsid`
descendants after a daemon SIGKILL.


## Initial live validation

The first two authorized Claude attempts did not produce an accepted answer. One stopped
at authentication before answer generation. A second, after bounded auth-notification
handling was added, reached the prompt phase with account authentication but returned
HTTP 401 indicating revoked OAuth access. Local `auth status` still reported a logged-in
team subscription; that is not proof that the inference service accepts its credentials.
Both attempts remain retained. No third Claude attempt is made pending manual re-login.
The remaining allowance is one of the initial three attempts. No extra Jev call was made.


## Latest user attempt

A third attempt returned the same HTTP 401 revoked-OAuth diagnostic. The separate ACP
allowance is now exhausted (3/3), not reset. The diagnostic classifier missed the provider's
literal `Failed to authenticate. ` prefix; this is now covered by an offline regression.
The answer-status area now preserves the safe, actionable API error instead of replacing
it with generic failure text. No new inference was used to test these changes.
