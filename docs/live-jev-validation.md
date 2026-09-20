# Live Jev integration validation

## Outcome

The native daemon now supports explicit source-bearing Jev calls with durable reservation
accounting, exact packet-bound response validation, and bounded measured display assembly.
The user's inspector on **8877** is enabled, still at revision 2, with the same token and
unchanged saved views and notes. Validation used separate index state on 8878 and the same
shared budget ledger; that temporary server has been stopped.

**83 Rust tests and 19 UI tests passed.** Formatting, clippy with `-D warnings`, and build
passed. Tests cover concurrent/process-safe reservations, restart/DB-loss handling,
cancellation, bounded responses, disabled/default routes, pre/post-call revision conflicts,
credential-error redaction, explicit UI confirmation and stale-response guards.
Remote CI was not run.

## Seven retained live attempts

| Attempt | Outcome | Latency | Important qualification |
|---|---|---:|---|
| Atomic, original encoding | HTTP 400 `max_tokens_exceeded` | 692 ms | Retained failure; no automatic retry |
| Transition, 11 candidates | Valid response | 618 ms | Selected `coordinateRunJsonTransition` |
| Atomic, lossless tables, 49 candidates | Valid response | 904 ms | 42 essential labels: too much detail |
| Lock, lossless tables, 89 candidates | Invalid response | 660 ms | Two probability sums were 0.99; rejected, not displayed |
| Atomic, explicit question/call prompts | Valid response | 882 ms | 8 essential labels; old source-order cap hid commit |
| Lock, explicit question/call prompts | Valid response | 771 ms | Further display-membership check needed |
| Atomic, actual browser run with ranked display | Valid response | 809 ms | Both commit operations visible within five calls |

These few timings are not an SLO or a general quality comparison. Transport/schema success
is not synonymous with a useful answer. The malformed lock response was not normalized or
recovered into a claimed clean success. All request/response bodies remain in the private
budget ledger; only metadata is published here.

## Fixes observed and checked

- **Lossless representation:** graph tables and a shared identity dictionary avoid repeated
  fields and long identities. Independent decoding reconstructs the exact native packet.
  Sources, calls, regions, callbacks and boundary metadata remain complete. The atomic
  payload fell from 100,447 to about 50,391 bytes before richer per-call prompting.
- **Question-specific prompting:** each choice identifies the literal question, callee,
  human caller, location and display eligibility. Final atomic/lock requests were 68,592 /
  113,286 bytes; no source truncation was used.
- **Budget membership versus order:** direct essential calls are ranked by the returned
  essential score before applying the cap, then displayed in measured source order. Scores
  are uncalibrated ranking hints. Unscored local/manual choices retain source-order fallback.
- **No-spend replay:** retained real responses were reassembled with the new policy. Atomic
  retained `link` and `rename`; lock retained `canStealRunJsonLock` and
  `ownerlessLockIsReclaimable`. These were replays, not additional inference runs.
- **Actual browser path:** the explicit confirmation triggered a real Jev call. The final
  atomic view showed `resolveProtectedPath`, `link`, `assertSafeTarget`, `rename`, and
  `syncDirectory`. Unresolved targets stayed unresolved. No execution order was invented.

## Budget

New authorization: **$5.00**. Seven attempts retain **$0.70** in reservations; **$4.30 remains**.
Usage-bearing responses imply approximately **$0.00747251** at the previously
observed published input-only rate, including the rejected selection response. This is
not an invoice and excludes unknown charges for the HTTP400. Reservations are not refunded.
The previous experiment budget remains closed and byte-for-byte unchanged.

## Remaining gates

ACP intent planning/selective review, broader selection-quality evaluation, incoming caller
navigation, durable focused views and true sequence semantics remain unimplemented.
Prompt and ranking improvements on these examples do not establish general model quality.

[Full metadata](live-jev-validation.json) · [Actual live browser view](images/live-jev-focus.png)
