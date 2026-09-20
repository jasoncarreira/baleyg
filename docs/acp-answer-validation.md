# ACP answer slice validation

## Implemented and tested

- Direct answer above the outgoing-call list, with branch cases and limitations.
- Full immutable question evidence, independent of Jev selection or the visible-call cap.
- Packet-bound exact-source quote/range validation and clickable indexed-source highlights.
- Explicit source-sharing confirmation, stale-result guards and bounded browser requests.
- Separate private durable attempt allowance, retained failures and bounded private audit.
- No built-in tools or ambient MCP; controlled hooks settings, scratch cwd, sanitized environment.
- Account-only Claude authentication, no API fallback, no automatic retries, subprocess cleanup.

116 Rust tests, 40 UI tests and 18 Node runner tests passed, along with formatting and
clippy. Remote CI was not run. A clearly labelled synthetic browser answer appeared above
the calls; clicking its citation highlighted the four matching source lines. That fixture
was not a model-generated answer.

## Live result: blocked, not a successful explanation

Two authorized Claude ACP attempts were retained. The first failed at subscription-auth
checking. Compatibility handling was improved for raw/prefixed plan names and delayed
auth notifications. The second reached the prompt phase with account auth but received
HTTP 401 indicating revoked OAuth access. Local CLI auth status reported a logged-in
team account, which did not establish inference authorization.

No accepted live answer or general answer quality has been demonstrated. Re-authenticate
Claude manually before the remaining test attempt. One of the separate three ACP attempts
remains. The model's cost estimate is not a billing guarantee; failures are not refunded.
No additional Jev calls were made by the agent during this slice. At deployment, the Jev
ledger reports eleven total attempts, $1.10 reserved and $3.90 available. The old experiment
budget remains closed and unchanged.

The inspector on 8877 now includes the answer path with the same token, saved views and
notes. Temporary validation on 8878 is stopped. Re-prepare the packet after restart.

[Full validation metadata](acp-answer-validation.json) · [Usage](acp-answers.md)

## Codex alternative

The user also authorized Codex. The installed CLI and official ACP adapter were checked,
but no Codex inference was run: the checked adapter cannot provide the same verified
no-filesystem-tools boundary. Local JSON validation is viable; residual file access is the
blocker. See [the compatibility check](codex-acp-check.md).


## Latest user attempt

A third attempt returned the same HTTP 401 revoked-OAuth diagnostic. The separate ACP
allowance is now exhausted (3/3), not reset. The diagnostic classifier missed the provider's
literal `Failed to authenticate. ` prefix; this is now covered by an offline regression.
The answer-status area now preserves the safe, actionable API error instead of replacing
it with generic failure text. No new inference was used to test these changes.
