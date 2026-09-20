# Jev hundredth-rounding compatibility

Observed two user-visible failures: all 49 / 89 decisions were returned with HTTP 200,
but one / two probability distributions summed to 0.99. Their values were on a 0.01 grid.
The original strict 0.002 sum tolerance rejected the whole response.

The bounded compatibility rule retains the original tolerance. It additionally accepts a
sum deviation of at most 0.01 (with floating-point epsilon) only if **every** probability
is finite, within [0,1], and on the hundredth grid within numerical epsilon. Sums 0.98 and
1.02, and arbitrary-precision distributions outside the original tolerance, stay invalid.
Candidate coverage, labels, schema, model, packet binding and confidence validation are
unchanged. Values and labels are not normalized or rewritten. A visible warning identifies
when this exception was used; scores remain uncalibrated ranking hints.

Historical failed attempts and their reservations are not rewritten or refunded. Saved
responses can be replayed through offline import to validate compatibility without any
new provider request. This is a parser compatibility change, not evidence of model quality.

## Validation and deployment

Both retained user responses (49 and 89 decisions) replayed successfully through offline
HTTP import, with one and two rounding warnings respectively. Labels remained identical;
scores were not normalized (cross-runtime floating-point comparison used 1e-15 tolerance).
No new provider request was made. Original failure records and reservations remain intact.

91 Rust tests and 19 UI tests passed, as did formatting and clippy.
A broader test exposed a pre-existing SQLite journal race: a writer can unlink a sidecar
between opening and checking its metadata. Only such already-open sidecars now allow zero
links; main database/lock checks still require one link. Owner, mode, type and no-follow
checks remain. Deterministic boundary tests and repeated concurrency tests pass.

The inspector on 8877 was restarted with the same token, views and notes. Ten existing
attempts retain $1.00, leaving $4.00 available at deployment. [Validation data](jev-rounding-validation.json).
