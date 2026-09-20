# Source and member navigation

User wants source view -> classes/methods (context menu) and class members -> declared types/methods.
Use cached revision-bound evidence, not bare-name grep guesses. No repository execution, dependencies,
providers, reindex/schema change or changes to inspected repositories.

POST /api/navigation request (deny unknown fields):
- Source selector: {expectedRevision, path, line} (1-based whole source line).
- Member selector: {expectedRevision, classId, memberName, startByte, endByte}.
Selectors are exclusive. Member selector MUST match a recorded ClassMember by name+exact byte range;
never trust typeHint/name as resolution. Source selector must validate path/line against cached file.
This slice is LINE-BASED navigation, not compiler go-to-definition at arbitrary cursor words.

Response: {revision, targets: [{symbol: Symbol, action: "sequence"|"class", reason: "declaration"|
"enclosing"|"type"|"call", matchKind: string}], warnings: string[], truncated: bool, requireIndex: bool}.
Targets use original measured declaration IDs/path/ranges. Declared functions/methods can sequence;
classes only if supported cachedclassprojection. Source line candidates: declarations on line,
nearest enclosing method/class, class type refs intersecting line, and measured resolved call targets
on line when present. Java unresolved bare-name or plain `this` calls may additionally offer
`sameClassCandidate` methods after bounded cached-AST proof of the exact invocation, lexical caller
and named class owner. Targets must be actual method declarations in that exact class; constructors
and arbitrary/qualified receivers are excluded. All matching overloads stay choices, not resolution.
This narrow proof supports callers inside ordinary method declarations only; constructor callers,
field/static/instance initializers, local/anonymous classes and lambda scopes remain unsupported. No global name matching for unresolved
calls and no Graph/call/sequence resolution mutation. Member selector returns own
measured method + declared field/parameter/return type refs scoped to owner and member range;
NOT arbitrary relationships elsewhere in class. Multiple candidates stay labelled choices, not
silently resolved. Unmatched/builtin types have explicit no-indexed-target status.

Backend owner: src/navigation.rs (new), src/lib.rs, navigation methods in src/store.rs,
API route/handler in src/http.rs, tests/navigation_http.rs (new) ONLY. Coordinate root HTTP asset route.
One read transaction/revision guard. Cached only, oldclassprojection requireIndex guidance when relevant.
Bound request input, query rows, candidates, target count, warnings, and RESPONSE bytes (<=512KiB).
Evidence-record input is separately bounded at 256KiB. Java field association may parse ONE cached
source file with a SEPARATE 2MiB source-text budget and 250ms parser deadline; source text is never
included in the response. This is explicitly approved. SQL-side narrowing precedes large payload loads;
never full-deserialize huge stored class definitions. Associate exact measured Java variable-declarator
range/name to its parent declaration type range; never guess from typeHint or neighboring text.
Include auth/host/origin, stale revision, cache-only, crossfile types, ambiguous/unmatched types,
UTF8 line boundaries, overloaded methods/shared field ranges, exact member validation, limits tests.

Source UI owner: web/navigation.js (new), web/navigation.css (new), tests/navigation-ui.test.cjs(new).
window.BaleygNavigation.init({request,currentRevision,currentSession,openClass,selectMethod,showMenu}).
request(path,{method,body}) same interface asClasses. showMenu(event,actions) delegates existingaccessible
classcontextmenu. Public open(event,selector,{isCurrent?}={}) captures anchor/coords synchronously,
prevents native menu then fetches targets; callbacks must rechecksession/revision/request/scope.
Public attachSource(container,{path,revision,startLine,isCurrent}) binds delegated click/contextmenu/
ShiftF10/ContextMenu on existing .source-line rows with .line-number text; do NOT replace source content.
Return/reset/dispose invalidatespendingnavigation. A visible Navigate action and keyboard line movement
must work withoutthousandstabstops; preserve selection/copy/scroll and highlightedsource ranges.
Fetch only onexplicitnavigation, not everysource read/line selection. Include sourceSerial guard supplied
byroot andreset onclear/source change; late actionscannotrunafteranotherselection/logout/reindex.
No source fetch justtoopenmenu. Menu candidates distinctreason+certainty+qualifiedidentity whenavailable.
No guessing arbitrarywordcall targets. Empty/loading/errors visible and recoverable; safe text nodes.
Root integrates assets and init/attach/reset in app.js/index.html/style hooks, CI and docs.

Class UI owner: web/classes.js web/classes.css tests/classes-ui.test.cjs ONLY.
Keep direct method-name click -> sequence viaexisting selectMethod exactmeasuredmember. Add member
contextmenu/touch action and clickabletypehint -> navigation lookup. Call optionalapi.navigateMember
(event,{classId,memberName,startByte,endByte},{isCurrent}) wiredbyroot toNavigation.open. Predicatecaptures
snapshot/generation/displayTicket AND currentlyvisiblepanel. No arbitraryglobaltypehintsearch. Reuse
sharednavigationmenu; don't duplicate async/auth handling. Fields/methodsremainfolded bydefault,
avoidnestedbuttons, keep originalclassnode contextmenu fromhandlingmember event too. Exact callbacks,
source callcount0 untilintentionalread, asyncstale, keyboard/touch/Unicode/safe-text tests.

Root handles integration/review/native+UI gates/staging browser/deployment. Preserve existing
workspace snapshots; providers stay disabled and closed budgets stay untouched.
Keep private validation artifacts outside the public repository. All public fixtures are synthetic.
