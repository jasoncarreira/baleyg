# Live Jev slice contract

New user authorization: “You can set the Jev budget at $5. Keep going.”
The old tests/fixtures/selection/outputs/budget.json remains closed and untouched.
New shared ledger directory: .baleyg/jev-question-budget (private), cap 500 cents.
Reserve 10 cents durably before EVERY outbound attempt; retain reservation on success,
error, invalid JSON/labels, timeout, cancellation or crash. No automatic retries.
This is conservative reservation accounting, not verified provider billing.

src/live_jev.rs exports:
- LiveJev::open(dir:&Path, key:String, cap_cents:u64, workspace:&Path)->anyhow::Result<Self>
- LiveJev::budget(&self)->anyhow::Result<BudgetStatus> (Serialize camelCase:
  capCents, reservedCents, remainingCents, attempts; no credentials)
- LiveJev::run(&self, packet:&QuestionPacket)->async Result<LiveSelection>
- LiveSelection (Serialize camelCase): attemptId:String, selection:SelectionEnvelope,
  latencyMs:u64, estimatedUsd:Option<f64>, usage:Option<Value>.
LiveJev is Send+Sync (Arc supported), no Debug exposing key. Ledger persists workspace
binding and immutable cap across reopens. Use SQLite transaction for atomic reservation.
Private attempt artifacts include request and bounded response, audit failure status.
45s timeout, fixed https://api.typesafe.ai/v1/systemone, no redirects/no retries,
request_for byte guard and parse_response validation; no key/body in error messages.
Use existing published input-only estimate $0.042/M tokens, explicitly not invoice.
Validate packet/request BEFORE charging reservation. Empty candidates rejected without call.

HTTP constructor new(...) stays offline unchanged; add new_with_jev(...same args,
provider:Option<Arc<LiveJev>>) -> Result<Arc<DaemonState>>.
GET /api/jev/status -> {enabled:bool,budget:BudgetStatus|null}.
POST /api/questions/{packetId}/jev-run -> {selection,view,attemptId,latencyMs,estimatedUsd,usage}
view source liveJev. Existing auth/Host/Origin/bodylimits apply. Refuse stale packet before
call; check again after response, return409 if graph changed (attempt remains accounted).
Provider disabled ->503. Budget exhausted ->429 (recognize "Jev budget exhausted" prefix).
No provider key/API URL/budget increase endpoint. No provider call from preview/import.

CLI opt-in: serve --jev-budget-dir DIR --jev-budget-cents 500, both required together.
Key read only from JEV_KEY at startup when enabled; never log. Bind ledger to canonical
workspace. Main adds reqwest dependency/module declaration. UI asks for source-sharing
confirmation per explicit Run Jev click, status/budget visible; local preview remains
separate and accurately labeled; no automatic calls on typing, preview, refresh, import.
All same request-generation/race guards remain. Imported response remains unverified,
live response is provider selection, not proof of correctness or sequence semantics.
