# Offline question focus

This page describes the offline path, which separates evidence depth from visible detail.
It does not run ACP or Jev. [Live Jev](live-jev.md) and [ACP answers](acp-answers.md) are separate explicit opt-ins.
Select a symbol, enter a question, and choose **Preview focus (offline)**.

The local preview only matches literal callee names against the question or optional focus
terms. It does not understand source semantics. No match can correctly produce an empty
view with uncertain candidates. This is an integration preview, not an AI answer.

## Display policy

- Evidence depth defaults to 2, with at most 80 nodes and 300 calls. Callback definitions
  are not expanded. Complete indexed source files accompany the bounded graph.
- Visible calls default to immediate seed calls only, with a limit of 5.
- Only essential labels are displayed. Supporting labels do not fill an unused budget.
- Deeper display requires explicit opt-in. Hidden essentials and uncertainty are counted.
- All calls, nodes, regions and boundary metadata come from the measured snapshot.
  Selection cannot invent connections or execution order.
- Raw outgoing navigation remains separate. Incoming caller queries are not implemented.
  Individual branch expansion retains the root; focused results cannot be silently saved
  as an ordinary raw query.

## Provider boundary

**Export Jev request JSON** downloads source-bearing JSON; it sends nothing. Do not share
it without permission to send that repository's source to a provider. Full sources are
included once. Requests exceeding 176,000 UTF-8 bytes are rejected rather than truncated.
Narrow evidence depth or choose a narrower root if needed.

**Import Jev response JSON** accepts user-supplied, unverified JSON for that exact packet.
Question keys bind each candidate to the complete packet hash. Responses require the
expected model, exact candidate coverage, known labels, valid probability distributions,
and finite confidence. A valid import does not prove that inference ran or that labels
are correct. The UI labels it accordingly.

Packets are immutable and revision-scoped. The daemon keeps at most eight packets / 8 MiB
in private memory. Cached actions return 404 after eviction/restart, and 409 after an index
revision change. Prepare a new preview in either case. Packet data is capped at 1 MiB.

Endpoints and Rust interfaces: [question slice contract](question-slice-contract.md).
Navigation rationale: [IntelliJ reference](navigation-reference.md).

## Deferred

ACP intent planning and selective review, durable focused-view persistence, incoming
callers, and true sequence semantics. Live provider transport and spend reservations are
now implemented separately; see [live validation](live-jev-validation.md).
The previous selection experiment ledger remains closed. Synthetic protocol tests are
not evidence of model quality or live adapter operation.
