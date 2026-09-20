# Automatic dependency catalog v1 implementation

Scope: automatically build a LOCAL declaration-only catalog for Rust/Cargo at server startup
and after explicit workspace indexing. Shared DTOs/API are ecosystem-neutral. Source libraries
stay separate from workspace graphs and model packets. All external diagram participants are
terminal. No compiler/type/trait/dispatch claims: first links are clearly marked syntax candidates.
No project processes, build scripts, proc macros, network, archive extraction or installs.
Unix secure readers currently supported; no Windows portability claim yet.

Ownership:
- catalog-core: src/dependencies.rs + src/dependency_rust.rs + tests/dependencies.rs (types/discovery/index).
- catalog-declarations: src/dependency_rust_symbols.rs + tests/dependency_declarations.rs.
- catalog-http: src/http.rs + tests/dependency_http.rs (async lifetime/API/revision guards).
- catalog-ui: web/app.js/index.html/style.css + tests/dependency-ui.test.cjs.
- root: Cargo deps/lib/main integration, participant annotation src/dependency_links.rs/tests, renderer, docs/deploy.
Coordinate directly. Freeze shared DTOs early; don't edit another owner files.

Shared src/dependencies.rs PUBLIC types (serde camelCase Serialize/Deserialize Clone):
Package {id, ecosystem, name, version, source:String, aliases:Vec<String>, source_state:String,
 index_state:String, warnings:Vec<String>} all text fields String. sourceState present/missing/blocked/
 ambiguous; indexState pending/partial/complete/failed/skipped. Alias known direct manifest names.
CatalogSymbol {id,package_id,name,qualified_name,kind,parent:Option<String>,owner_expression:Option<String>,
 signature:String,source_ref:String,path:String,range:SourceRange}. kind module/struct/enum/union/trait/impl/
 function/method/type. Lexical ownership NOT semantic binding. Full body text not in signature.
Catalog {id:String,workspace_revision:u64,packages:Vec<Package>,symbols:Vec<CatalogSymbol>,
 warnings:Vec<String>,sources:HashMap<String,CatalogSource>} sources #[serde(skip)] private capabilities.
CatalogSource {root:PathBuf,path:String,hash:String,package_id:String} Clone, internal only noJSONroot.
CatalogOptions {cargo_home:Option<PathBuf>,rust_library:Option<PathBuf>} Default explicitNone,
 environment-config captured main (CARGO_HOME or OS home/.cargo; rust_library configured trusted root).
Catalog::build(workspace:&Path, revision:u64, options:&CatalogOptions,cancel:&AtomicBool)->Result<Catalog>.
No persistent DB migration for v1: catalog immutable in-memory, rebuilt startup/Index (honest limitation).
Catalog id hashes workspace revision + manifest/lock/source hashes + parser version. Per-file source refs
include package identity + path + hash. Never namespace by filename alone.

Declaration extractor PUBLIC function in dependency_rust_symbols:
extract(package_id:&str,crate_name:&str,path:&str,text:&str,source_ref:&str)->Result<Declarations>
Declarations {symbols:Vec<CatalogSymbol>,warnings:Vec<String>}.
Dedicated tree-sitter declaration walk; DO NOT descend into function/method bodies or closures,
macro token trees, expression/const bodies. Parse full file but no call/region/behavior creation.
Module qualified names derived source layout + inline modules are syntax candidate paths, not
validated exports; mark path attributes/cfg/reexports unknown. Impl owner expression preserved.
IDs package/source/span/kind. Depth128, ASTvisit50000, declarations2000 perfile, bounded signatures.

Discovery: parse bounded Cargo.toml/Cargo.lock with real toml crate; no metadata commands.
Only lock/manifest referenced packages, rootpackage itself excluded. Directaliases and normal/dev/build/
 target feature conditions recorded as warnings/unknownactive (do not assert allcompiled).
Localregistry lookup under configuredCargoHome registry/src; match name/version manifests, sourcekind
 crates.io only firstslice; unknownregistries/configpatch/source replacements explicitblocked/ambiguous.
Do not blindly assume hashed registrydirectory proves registryidentity: localcachematches candidates,
source warning originnotattested. Multiplematches =>ambiguous no arbitrarywinner. Workspace-contained
path deps safe; outsideworkspace pathsblocked (manifestcannotgrantfilesystemauthority).
Sources read with SourceDir descriptor-relative nofollow, including manifests. Pinned root mustnotbe
expanded by clientpaths. Sysroot std/core/alloc roots discovered beneath rust_library config as known
package roots; root config obtained outside project/no command in indexer. Missing root warning.
Budget: max1024packages discovered, 2000sourcefiles total, 64MiB source total, 2MiB/file,
 500files/16MiB/package, 50000catalogsymbols total. Prioritize std then directdeps then transitives.
Skip target/tests/examples/benches/hidden dirs; only .rs in src subtree. Bounds/cancel explicit
partial perpackage andcatalog warnings, never silentlycomplete. No source texts retained in catalog.

HTTP lifecycle owns optionalCatalogOptions configured through newstate method/constructor rootcoords.
Old constructors defaultcatalog disabled so existingtests don't accessrealhomes. Main enables explicitly
but normalproductiondefault enableCargo discovery automatically; capturedtrusted roots only.
DaemonState::start_dependency_index(self:&Arc<Self>) starts nonblockingspawn_blocking at startup,
 after successful workspaceIndex, andon POST/api/dependencies/refresh. Cancelold generation and reject
 late publication; shutdown cancels. Never holdlockwhileparsing. Workspace revision muststillmatch
beforepublishandread. Sourcechanges on sourceview hashmismatch ->409refreshratherthannewbytesoldranges.
GET /api/dependencies -> {state:disabled|loading|ready|failed,workspaceRevision,catalogId:null|id,
 packages:[Package],symbolCount,warnings}. No symbols/source text bulk here.
GET /api/dependencies/symbols?catalogId=...&packageId=optional&q=optional&offset=0&limit=100
 -> {catalogId,workspaceRevision,items:[CatalogSymbol],nextOffset}. max200.
GET /api/dependencies/source?catalogId=...&sourceRef=... -> compatible externalSnapshot shape
 {id,rootId,rootLabel,path,hash,file:SourceFile,definitions:[{id,name,kind,parent,path,range}],warnings}.
Read file on explicit userrequest ONLY from catalogcapability, verifyhash; noarbitrarypaths. Unknown404,
 stale409, auth/host/origingates asusual. POSTrefresh uses strict {} andreturns202; explicitstatusretryUI.

UI replace manual-root-firstdirection: automatically fetch dependency status afterconnect/refresh/index;
 show package list/status with counts and warnings, clickpackage loads indexeddeclarations no manual
roots/fileknowledge needed; filterindexeddefinitions andclicksource opens existingdedicated external
candidatepane (bounded600lines prevnext). No fileGETuntilsourceclick. Keepmanualrootbrowsercollapsed
fallback. Stale catalog/workspace/session guards. Loadingstate manualRefresh status button orbounded
poll onlywhileloading, no inferredsource/model/index requests.

Sequence annotation root: calledafterstore.sequence_at usingCACHEDsourceofseed andcatalogsnapshot
onlyifcatalog/workspace revisions match. Optional candidate class participant for exact qualified
syntactic path matchingCatalogSymboltype. No bare-name global matching, no fluentreturntype inference.
Treat imports/aliases/shadowing conservatively; cannotresolve =>originalunresolved. Participant.kind
externalCandidate, label class/type, identification source/package + candidate/terminal warning.
Measured callIds/resolution stayunchanged; doNOTset CallSite.target. Onlypresentationstep target
maypointtoexternalcandidateparticipant. Neveradddependencybehavior; roots /api/sequence stillonly
workspace symbols soexternalIDsrejected. Candidateclicks source via dependencycatalog APIoptionallater.
