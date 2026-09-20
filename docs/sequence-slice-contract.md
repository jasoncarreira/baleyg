# File / method / sequence vertical slice

Primary interaction: indexed file tree -> expand file INLINE to methods -> click method ->
sequence diagram alongside tree. This uses no provider calls. Keep existing inspector/question
features accessible but secondary. Preserve selected root and explicit expansion.

Shared language-neutral behavior DTO (src/behavior.rs; no tree-sitter here):
SequenceView {revision:u64,seed:Symbol,participants:Vec<Participant>,steps:Vec<SequenceStep>,warnings:Vec<String>,hiddenSteps:usize,truncated:bool}
Participant {id:String,label:String,kind:String} kind method/internal/boundary; seed id is symbol.id.
SequenceStep {id:String,kind:String,label:String,path:String,range:SourceRange,callId:Option<String>,target:Option<String>,resolution:Option<Resolution>,children:Vec<SequenceStep>,alternate:Vec<SequenceStep>,hidden:bool}
Kinds: call,branch,loop,try,return,throw,await,boundary,note,effect (strings wire; use enuminternallyifdesired).
Target is participant id for call arrows. Branch children true body, alternate false/else; try children try body,
alternate catch/finally grouped labeled nodes. No fabricated caller/return edges. Bare return/throw are notes.
All real call steps keep original callId/resolution/source range. Unresolved targets boundaryparticipants;
do not present same receiver text as provenobjectidentity. Calls on selectedmethod receiver mayuse self.
Do NOT use lexical start-byte order as evaluation order for nestedcalls: arguments/receiver evaluatedbeforeouter.
Represent &&/||/??/conditional evaluation as guarded branches, not unconditionalcalls. Function/callback/class
bodies not executed by declaration: explicit boundary, no flattening. Async marks await boundaries, nottimeline.
Loops are fragments not unrolled. Return/throw terminate sameblock; remainingcode markedunreachable/omitted
withwarning, never shownexecutingafterreturn. Exceptionpaths notassertedwhenunknown.
Unsupportedcomplexcontrol => source-linked boundary/warning; never quietlyfabricatebehavior. Evidence is
static possible paths, NOT observed runtime order. Bounds max200steps/20participants/depth24 failsofttruncated
warning; sourcequotes/labels bounded. Default showAll=false collapses only confidentlyincidental logging;
retain validations,writes,external/unresolvedeffects,branchtests,returns/throws. Hidden counts+Showall control.
This is deterministic heuristic selectivity, NOT claimed semantic consequentialness. Source-links mandatory.

JS adapter src/behavior_js.rs builds the neutral DTO from cached SourceFile, selectedSymbol, measuredcalls.
Root exports modules. Keepgraphschema/storagebackwardscompatible: derivebehaviorfromcachedsourceatrequesttime
inthisfirstslice; graphmodeldownstream/renderingneverimportsJSgrammar. Signature agreed:
behavior::build_sequence(revision:u64, seed:&Symbol, file:&SourceFile, calls:&[CallSite], show_all:bool)->Result<SequenceView>
This dispatcheslanguage to behavior_js::build(...sameargs)->Result<SequenceView>. Unsupportedlanguage error.

Catalog/API workerowns src/store.rs src/http.rs tests/browse_http.rs (notexistingtests unlessneeded).
GET /api/files?revision=N&offset=0&limit=200 -> {revision,items:[{path,language,methodCount}],nextOffset:number|null}.
GET /api/methods?revision=N&path=... -> {revision,items:[{symbol:Symbol,consequential:bool,reason:String}],truncated:bool}.
Methods = indexed Function/Method includingnestedmethods labeled bynames/path ranges; no class/moduleasmethod.
Conservativeheuristic only hide accessor/trivialreturnliteral/identifier methods ifdemonstrablytrivial; otherwise
include. If trivialanalysisnotavailable keepall exceptaccessors labeledheuristic. Neverhidepurechecksjust0calls.
POST /api/sequence {seed:String,expectedRevision:u64,showAll:bool(defaultfalse)} -> SequenceView.
Useonlycurrentcachedsnapshot, revisionchecks, rejectunknownfields/invalidshape, sourcepathmustindexed,
no sourceworkspacefilesystemreads orcommands, auth/host/origin/bodyguardsunchanged. missing404 stale409 bad422.
Storeworkerchooseshelpers butmustsnapshotconsistency. BuildsequenceCPUonspawn_blocking/dbwrapper.
No autoindex/modelcalls. Puremethodclick never callsquestions/ACP/Jev.

UIworker owns web/app.js web/index.html web/style.css tests/browse-ui.test.cjs (existingquestionUITestsreadonly).
Implement actual SVG sequence diagram (noCDN/externalfonts/network renderer). Lifelines, directionalcallarrows,
selfcalls, branch/loop/tryfragments visible; preservehierarchy, don'tflattenalternativesintostraighttimeline.
Renderer module web/sequence.js plainbrowserJS window.BaleygSequence={render(container,view,onSource)};
root/backendworker serves /sequence.js CSPself. UIworker ownsrenderer too.
Safe DOMtextContent/createElementNS no userHTML/mermaid stringinjection. Every step keyboard-clickable source
location through existing showSource helper; horizontaloverflowcontaineddiagramnotpage. Sourceaccessiblelist
orbuttons equivalent; smallscreensusable. Explicit showAllsteps toggles serverrequest. Fileexpandloadsinline
methods and preservesotherbranches; filterfilepaths localexistingcatalog, showallmethodstoggle. 200filepages
Loadmore control. Concurrencygenerationsperfile+diagram+session+revision prevents stale successANDfailure,
logout/indexrevision clearcatalog/diagram/state. Showclearloading/empty/unsupported states; labelstaticpossible
paths and uncalibrated heuristics. Preserveexisting rawquery/question features andtests. No inferencecalls.
