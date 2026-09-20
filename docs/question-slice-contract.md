# Question/view slice contract (implementation handoff)

No network calls. Live inference budget remains closed. Do not pretend local preview or
synthetic fixtures are model outputs. Native Graph remains authoritative.

Planning module public types (camelCase, strict request fields):
- QuestionRequest { seed:String, question:String, expected_revision:u64,
  evidence_depth:usize(default2,max3), max_visible:usize(default5,max12),
  allow_deeper_display:bool(defaultfalse), focus_terms:Vec<String>(defaultempty,max12) }
- QuestionPacket { packet_id:String, revision:u64, request:QuestionRequest,
  context:ViewResult, source_files:Vec<SourceFile>, warnings:Vec<String> }
  SHA256 of versioned request+revision+context+files; immutable, never trust client-supplied context.
- Relevance enum essential/supporting/incidental/uncertain.
- CallDecision { candidate_id:String, relevance:Relevance }
- SelectionEnvelope { packet_id:String, decisions:Vec<CallDecision> }.
- FocusedView { packet_id:String, revision:u64, question:String, selection_source:String,
  nodes:Vec<Symbol>, calls:Vec<CallSite>, regions:Vec<ControlRegion>,
  supporting_count:usize, omitted_count:usize, uncertain_count:usize,
  policy_hidden_count:usize, warnings:Vec<String> }
- QuestionPreview { packet:QuestionPacket, selection:SelectionEnvelope, view:FocusedView }

src/planning.rs functions:
prepare(store:&Store, request:QuestionRequest)->anyhow::Result<QuestionPacket>
  use query_view bounded80nodes300calls, evidence_depth; validate revision equals requested;
  fetch complete source files of context nodes/calls via source_at(expectedrevision) -> conflict on drift;
  cap complete packet at1MiB (error rather than silentlytruncate sources), include truncation diagnostics;
  all context call sites are candidates, no synthetic call edges.
preview(packet:&QuestionPacket)->Result<SelectionEnvelope>
  explicitly LOCAL deterministic, literal focus/question term scoring only, not ACP/LLM;
  direct seed calls first; deeper evidence never defaults visible, helpers notautoexpanded.
assemble(packet:&QuestionPacket, selection:&SelectionEnvelope, source:&str)->Result<FocusedView>
  exact complete one decision percall, no unknown/duplicate/missing IDs; packetId match;
  select essential ONLY; maxVisible bound; unless allowDeeperDisplay onlycaller==seed canappear;
  don't promote lower relevance merely to fillbudget; retain seed node; for showncalls include
  measuredcaller/knowninternal target; class/external/unresolved/callback boundaries unchanged;
  include necessary actualregions+parents only; neverinferbridges; warnings on policy filtering.

src/jev.rs pure functions:
request_for(packet:&QuestionPacket)->Result<serde_json::Value>
parse_response(packet:&QuestionPacket,response:&serde_json::Value)->Result<SelectionEnvelope>
  Use observed Jev API shape from tools/selection/jev.mjs, modeljev-1.13.0,
  choice labels essential/supporting/incidental/uncertain for each callsite, complete evidence once,
  source treated asdata, no request above176000bytes; exactanswer coverage/model,
  probability keys exactlylabels, finite0..1 sums within.002 confidencefinite0..1.
  No HTTPclient/envreading/spend. Import is unverifieduser-suppliedresponse, not livecompletedcall.

HTTP (parent owns src/http.rs integration):
POST /api/questions/preview QuestionRequest -> QuestionPreview; server remembers packet,
cachelast8packets, total<=8MiB. Failedpreparedoesnot evictgoodpacket. expectedRevision required.
GET /api/questions/{packetId}/jev-request -> providerrequestJSON, no network.
POST /api/questions/{packetId}/jev-response rawJevJSON -> {selection,view}; assemblyselectionSource=importedJev.
POST /api/questions/{packetId}/selection SelectionEnvelope -> {selection,view}; source=manual.
Any cachedpacket action checks Store.status revision same ->409 ifreindexed; missingpacket404;
client cannot submit files/graph facts; no paid endpoint and no providerkey config.

UI (separate worker) inserts dedicated questionform next to interactions, preserves raw depth explorer.
- Questiontextarea, optional focus terms comma separated, maxvisibledefault5, evidenceDepthdefault2,
  checkbox Allow deeper display defaultOFF. Separate controls notchangingrawquery depth1.
- Button Preview focus (offline), clearly label Local preview—not Jev/ACP. Show actual focusedcalls
  minimalmainlist+collapsed evidence using source links at exactviewrevision. Show hidden/uncertain
  counts and limitations. Keep raw explorer accessible via explicitreturnbutton.
- Export providerrequest (may fail176KB; clearerror), import JSON file offline, no networkprovider calls;
  imported labelunverified; no token in download, request includescode warnuserbeforeexport.
- Focused mode mustnot be saved as ordinary rawquery silently; disablesavecurrent orlabelseparate.
- Keep epoch/query/sourceguards so stale repliesneverreplace newer selectedseed orrefresh.


## Subsequent live-slice refinements

`CallDecision` now has optional `displayScore` (finite 0..1; absent by default). Jev parsing
supplies P(essential), not confidence. Assembly prefers direct essential calls, then uses
this relative score to choose budget membership, then renders the selected calls in source
order. It never promotes supporting/incidental/uncertain labels. Scores are not calibrated
confidence. Unscored local/manual selections keep source-order fallback.

Provider exports use lossless `state.encoding = "baleyg-evidence-tables-v1"`: explicit
column/row tables, identity dictionary and range columns. Native packets are unchanged;
independent test decoding proves exact reconstruction. Each question also names the
literal user question, human call identity and display eligibility. Full sources remain
included once. [Live transport and budget contract](live-jev-slice-contract.md).
