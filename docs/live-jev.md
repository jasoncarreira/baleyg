# Explicit live Jev selection

Live selection is optional. Offline preview/export/import still work without credentials.
A local preview is literal-name matching; **Run Jev** sends the prepared source-bearing
packet to the hosted provider and validates its labels before displaying measured calls.
Neither output proves runtime ordering or constitutes a sequence diagram.

## Authorization and startup

The user authorized a new $5 budget for the feature-factory snapshot. Its independent
ledger is `.baleyg/jev-question-budget`; the previous experiment ledger stays closed.
Supply `JEV_KEY` through the process environment from a local secret store. The daemon
never reads `.env` itself. Do not put the key in command arguments, source or logs.

```sh
cargo run --locked -- serve \
  --workspace tests/fixtures/extraction/inputs/feature-factory \
  --state-dir .baleyg/native-smoke \
  --scip tests/fixtures/extraction/feature-factory.scip \
  --manifest tests/fixtures/extraction/feature-factory.hashes.json \
  --jev-budget-dir .baleyg/jev-question-budget \
  --jev-budget-cents 500
```

Without both budget flags, live inference is disabled. Existing ledger caps and workspace
bindings cannot be changed on reopen. Keep this durable budget directory separate from
rebuildable index caches. Never delete or replace it to restore available budget.

## Accounting and privacy

- Each outbound attempt reserves 10 cents in a SQLite transaction before sending.
- Reservations remain charged on success, failure, timeout, cancellation or crash.
- A $5 cap therefore permits at most 50 attempts. There are no automatic retries.
- This is conservative reservation accounting, not verified provider billing.
- Usage-derived estimates use the previously observed published input-only rate,
  $0.042 per million input tokens. They are not invoices and do not refund reservations.
- Request and bounded raw response audit records stay in the private SQLite ledger.
  Treat it as source-bearing private data. It does not store the configured key.
- Requests use the fixed HTTPS endpoint, reject redirects and use a 45-second timeout.
- Complete source evidence is sent, not just the few calls displayed. The UI asks for
  confirmation for each explicit run. Typing, previewing, refreshing and importing do
  not trigger inference.

## API

- `GET /api/jev/status`: enabled state and aggregate reservation balance; no credentials.
- `POST /api/questions/{packetId}/jev-run`: no body or `{}`. Returns validated selection,
  measured focused view, attempt ID, latency, usage and estimated USD.
- Disabled provider: 503. Exhausted reservations: 429. Stale packet: 409.
  If the index changes during a paid attempt, its reservation remains accounted and the
  obsolete result is not displayed. Prepare a new packet; do not retry automatically.

All routes retain local bearer authentication, Host/Origin checks and body limits.
There is no API for changing provider URL, credentials or budget authorization.

## Evidence encoding and current quality limits

The provider-only `baleyg-evidence-tables-v1` encoding interns repeated identities and
stores graph objects as column/row tables with explicit source-range columns. Independent
test decoding reconstructs the exact native packet. Full source text is never truncated.
Candidate aliases still bind to the full native packet hash.

The local byte guard is not a guarantee that the provider token limit will fit. Context
rejections are retained and require an explicit narrower request; there is no automatic
fallback that silently discards evidence. Malformed probability distributions fail closed, except for the documented
[narrow hundredth-rounding allowance](jev-rounding-fix.md), which preserves original
values and adds a visible warning.

Successful schema validation does not establish useful selection. An early live atomic
question over-selected calls; source-order display capping then hid its commit operation.
Question-specific prompt tuning and display-policy evaluation remain separate quality work.

For scored selections, the display budget prefers direct calls and then higher essential
scores; the chosen calls are rendered in measured source order. Lower relevance labels
never fill the budget. Local previews and manual selections without scores keep the
source-order fallback. Scores are optional finite values in [0,1], treated only as relative
ranking hints—not calibrated confidence. Hidden essential labels remain counted rather
than being silently relabeled as supporting.
