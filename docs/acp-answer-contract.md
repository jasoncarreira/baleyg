# ACP answer slice contract (implementation)

Offline tests first. No live model calls without separate ACP authorization; Jev's $5 ledger
and closed experiment ledger are not ACP authorization. Source workspaces remain read-only.

Answer schema, strict camelCase:
AnswerEnvelope {packetId:String, summary:Vec<AnswerClaim> (1..4), branches:Vec<AnswerClaim> (0..6), limitations:Vec<String> (0..6, each nonempty and at most 1200 UTF-8 bytes)}
AnswerClaim {text:String (1..1200 bytes), citations:Vec<AnswerCitation> (1..4)}
AnswerCitation {path:String, startLine:u32, endLine:u32, quote:String}
Citations must reference a complete packet source file, valid 1-based inclusive lines, at most
12 lines, and quote must be nonblank and exactly match those lines joined by LF (strip CR in CRLF line endings).
Do not accept arbitrary URL links, invented paths, mismatched quotes, cross-packet responses,
unknown fields, empty claims, overlarge answers (>48KiB), or unsupported coordinate ranges.
All claim text rendered as text, not HTML/Markdown. Limitations are explicitly unverified model
caveats. Valid citation anchoring is not proof that the assertion logically follows.

src/answer.rs: pub parse_response(packet:&QuestionPacket,response:&serde_json::Value)->anyhow::Result<AnswerEnvelope>;
pub build_prompt(packet:&QuestionPacket)->anyhow::Result<String> (bounded <=2MiB, full facts and complete
sources, ideally source lines numbered ONCE not duplicated). No graph/selection modifications.
Model receives full evidence, not just displayed five calls. Graph is static, source is untrusted
DATA, unresolved/callback boundaries explicit; no inferred execution timeline. Ask for concise
direct answer with branch cases/caveats and exact quoted source citations. No ACP/Jev orchestration
inside this pure module. Jev not a prerequisite: evidence preview alone suffices.

src/acp.rs process transport: AcpConfig {runner:PathBuf, state_dir:PathBuf,max_attempts:u64,workspace:PathBuf};
pub Acp::open(config:AcpConfig)->Result<Self>; pub status(&self)->Result<AcpStatus>;
pub async fn run(&self,packet:&QuestionPacket)->Result<AcpAnswer>.
AcpStatus {maxAttempts,attempts,remainingAttempts,model:"sonnet",maxEstimatedUsdPerAttempt:1.0}.
AcpAnswer {attemptId:String, answer:AnswerEnvelope, latencyMs:u64, estimatedUsd:Option<f64>}.
Transport depends on answer.rs; do not register module yourself (root owns lib.rs).
Separate private durable attempt allowance, immutable workspace/cap; retain failed/incomplete
attempts. Reserve before spawn. Never touch Jev/experiment ledgers. One in-flight per daemon;
no automatic retry, max 120s, bounded stdout 64KiB/stderr discarded, process cleanup on failure/
cancellation. No source repo cwd, no inherited JEV_KEY/ANTHROPIC_API_KEY/provider credentials.
Existing Claude subscription auth only; no API fallback. Native runner stdin JSON {packetId,prompt}
(EOF-delimited) -> stdout JSON {answer:<AnswerEnvelope>,estimatedUsd:number|null}; exact one success
JSON only, no stream prose. Native validates answer again. Sanitized failures; no raw stderr in API.
Node runtime under runtime/acp; pinned ACP sdk1.4.0/Claude adapter0.79.0. Use existing experiment
as prior art but no imports that charge its closed budget. tools:[], allowedTools:[], settingSources:[],
no MCP servers, persistSession:false,maxTurns:2,maxBudgetUsd:1,output ceiling4096. Empty private scratch
cwd, fs/terminal capabilities false, deny permission requests, require subscription account status,
select sonnet explicitly, accept only end_turn terminal success and complete JSON text; errors/partial
emissions never successes. Estimate is NOT a bill or hard spending guarantee.

HTTP: preserve new/new_with_jev; add new_with_providers(...,jev:Option<Arc<LiveJev>>,acp:Option<Arc<Acp>>).
GET /api/acp/status -> {enabled:bool,status:AcpStatus|null}.
POST /api/questions/{id}/acp-answer body{} -> {packetId,revision,source:"liveAcp",attemptId,answer,latencyMs,estimatedUsd}.
Only server-cached immutable evidence packet, pre/post snapshot revision checks, no client prompt,
source/runner/budget overrides. Disabled503, missing404, stale409, invalid answer422, exhausted429,
provider error502. No answer cache/persistence needed this slice; HTTP request generation guards UI.
CLI paired --acp-runner PATH --acp-state-dir DIR --acp-max-attempts N (1..20); disabled by default.

UI explicit Explain with ACP action and confirmation of sending FULL source evidence to Claude,
subscription allowance separate from Jev. Answer appears above calls, plain text claim+clickable
source citations (current snapshot /source links, highlight cited range). Branches/limitations.
No inference on typing/preview/status/import/refresh. Preserve navigation and 5-call display default.
Invalidate answer on packet/question/session/revision change, guard stale success AND failures.
Do not display local/mock answers as model generated. Minimal diagnostic metadata, truthful citations
validated label, no implication that claim truth has been machine verified.
