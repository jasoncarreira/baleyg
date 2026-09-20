# Automatic dependency indexing

Status: architecture and staged implementation plan. The first bounded Rust catalog and candidate-only participants are implemented; see [current behavior and limits](automatic-libraries.md). Exact semantic receiver/dispatch resolution and additional ecosystem adapters remain unimplemented.

## User requirement

Baleyg must automatically discover and index included libraries. The design must support multiple languages and ecosystems on multiple operating systems, not depend on a particular developer machine, Homebrew layout, or manually entered library roots.

Classes and types must be usable as diagram participants. Calls into third-party libraries must stop at those participants. Baleyg must not expand a third-party implementation into sequence behavior, even when source or semantic evidence is available.

The existing manual Rust source browser does **not** fulfill this requirement. It is useful infrastructure for source inspection only. Automatic source discovery also does not, by itself, resolve receiver types or call targets.

The first implementation is bounded to Rust/Cargo. The catalog, source identity, evidence, and diagram contracts must remain language-neutral. Further language adapters are separate delivery stages, not implied support.

## Recommended approach

Recommendation: implement an automatic, language-neutral dependency/symbol catalog, NOT automatic full dependency behavior indexing. Ship Rust as its first ecosystem adapter. Keep all external participants terminal in sequence diagrams at the backend, regardless of depth/showAll/UI actions. Do not promise semantic Rust dispatch yet.

## Repository findings

The research phase was read-only. No workspace commands, builds, providers, installs, or edits were run during that phase. This document is a planning artifact, not an implementation.

- src/rust_sources.rs currently supports only explicit roots (max 8), on-demand single-file snapshots and candidate definitions. Snapshot IDs include label, physical root, relative path and contents. /api/rust-sources{,/tree,/file} is authenticated and runs reads on spawn_blocking. Definitions never enter workspace graph/provider context. This is a useful source-view substrate, not automatic indexing.
- extract_external in src/indexer_rust.rs still walks function bodies and creates calls/regions before discarding them; bounded at 50k AST visits / 8k records, snapshot trims to 2k definitions. A dedicated declaration-only extractor is needed. Current SymbolKind::Class conflates struct/enum/union/trait/impl; parent is lexical container, NOT semantic type ownership. Symbol IDs lack package identity, so copying them into one external catalog would collide across equal relative paths/content in different packages.
- Rust calls are always unresolved with semantic=unavailable; modules/imports/cfg/traits/macros are explicitly not resolved. behavior_rust.rs:299 groups every non-internal call into one unknown participant. sequence_at only accepts workspace function/method symbols. These guardrails should remain until evidence supports more.
- file_tree::SourceDir is secure descriptor-relative on Unix but explicitly unsupported on non-Unix. “Any platform” cannot be claimed from current implementation; Windows reparse/junction-safe handle-relative reads are a real prerequisite, not merely replacing /opt/homebrew with an environment variable.
- Cargo.toml is a straightforward single-package manifest with 23 normal + 3 dev dependency entries; Cargo.lock v4 has 235 packages, all external sources crates.io. There is no TOML parser crate in manifest or lock. Add a proper parser as an implementation dependency, not handwritten TOML parsing. Lockfile presence does not establish enabled features, active target dependencies, or source availability.

## Architecture
1. Generic Catalog service with ecosystem adapters: discover(local read-only inputs) -> PackageRefs + dependency edges; locate(package, trusted source roots) -> source availability; extract declarations -> CatalogSymbols; optional resolve(workspace occurrences, semantic snapshot) -> EvidenceLinks. Rust/Cargo, later JS/package managers, Python, JVM, .NET etc use the same contract without pretending discovery rules are universal.
2. Keep workspace behavior graph and external declaration catalog physically/logically separate. Catalog owns packages, modules, types/classes/interfaces/traits, callable signatures, declared return types, visibility, import/export/re-export syntax, lexical impl blocks and separate ownership relations. External bodies may be read by tree-sitter parsing but are not traversed for behavior or stored as calls/regions. Source snippets/files fetched locally only on explicit UI viewing; provider payloads do not silently gain library text.
3. Participant identity is type/package identity, not a function label and not an object-instance claim. Add ownerTypeId/packageId/symbolRef, scope(workspace/dependency/stdlib), evidence and terminal=true. Keep callee method on message. Methods in `impl Foo` have a syntactic owner expression Foo; linking it to an exact Foo declaration is a separate relation with evidence. Trait impl/default/blanket impl ownership must not be conflated with dispatch. Functions without type owner use module/package participants. Candidate type participants must visibly say candidate, and must never set CallSite.target/Resolution::External as if dispatch was proved.

## Minimum safe automatic Rust slice
A. At workspace indexing/start, read bounded Cargo.toml/Cargo.lock plus supported workspace member manifests. Discover locked direct/transitive package references and dependencies, preserving name+version+source identity, dependency aliases/package rename, dependency kind and textual target/feature conditions. Report enabled/active status unknown unless established. Never index every crate in a machine cache; only lock/manifest-referenced packages. Handle no/malformed lockfiles by reporting unresolved versions, not choosing latest installed.
B. Discover Cargo home from trusted daemon CARGO_HOME or OS home + .cargo, not Homebrew. Locate matching extracted registry sources using package manifests and local registry configuration/metadata; do not assume hashed directory names identify the registry, and do not confuse same name/version from separate registries. For crates.io mapping that cannot be validated, label candidates. Honor bounded, declarative local vendor/source-replacement config only when supported; unsupported patch/config/git sources get explicit status rather than a silent wrong match. No network, archive unpacking, cargo metadata, cargo check, rustup component install, build.rs, proc macros, or source-specified commands. Workspace aliases/rustc wrappers/config runner settings must never be executed.
C. Auto-discover std/core/alloc under a trusted configured sysroot + lib/rustlib/src/rust/library. Use previously established trusted toolchain configuration (not a repository-selected rust-toolchain command or PATH executable). If user/daemon has explicitly trusted an absolute rustc executable, a later discovery helper can run only that binary with --print sysroot from a neutral directory, sanitized environment, timeout; this is not required for the no-process initial slice. No download if rust-src absent. Missing trusted toolchain => std source status unknown/missing with a clear fix. Multiple installations => no arbitrary claim that one matches project.
D. Initial safe filesystem scope: workspace-contained path dependencies + trusted Cargo cache/sysroot roots. A Cargo path/vendor reference outside granted roots reports blocked until root approval; never lets an inspected manifest grant itself arbitrary filesystem access. Preserve this distinction even though automatic dependency discovery is enabled by default. Add proper Windows secure reader or explicitly stage/label Windows capability as unavailable until tested.
E. Automatically index bounded declarations in matched local packages. Default suggested budgets: 2 MiB/file, per-package/total byte and file caps, cancellation, AST/time/depth caps, bounded worker concurrency, per-package partial/error state. Use metadata filtering and skip target/examples/tests where appropriate but do not claim an exhaustive public API (cfg/re-exports/macros can change it). Budget choices are product configuration, not completeness assertions. Prioritize directly referenced packages, then transitives. Separate package source presence from indexing completeness and semantic resolution.
F. First links: qualified paths and explicit imports/aliases can produce source-navigation candidates after lexical scope/shadowing checks. std::fs::OpenOptions::new can get a candidate OpenOptions participant plus candidates for new; do NOT claim fluent .write().open() receivers all share that type. Merely parsing return syntax is not Rust inference; allow a clearly marked visual source-chain group separately, or leave method receiver unresolved. Bare same-name matching is search only, not binding.

## Semantic boundary
Exact declaration/call links require module path + re-export/name lookup, lexical scopes/shadowing and edition/extern prelude; active package/features/target/cfg and std toolchain; type aliases/generics/substitution; receiver inference with autoderef/autoref/coercions; inherent vs trait method selection, associated items/UFCS/blanket impls; return types across fluent chains; macro-generated definitions and generated OUT_DIR code where relevant. LSP definition itself may find a trait declaration rather than concrete runtime dispatch, so record what was resolved. rust-analyzer unavailable is not a reason to fake this. Later integrate optional imported semantic artifacts or a separately approved, restricted RA service; keep build scripts/proc macros disabled and do not automatically invoke cargo/providers. Missing semantics must degrade to candidates. Even semantically confirmed external calls stay terminal.

## API/DTO proposal (additive generic API; keep existing rust-sources compatibility)
GET /api/dependencies?revision=... (packages, discovery state, warnings); GET /api/dependencies/:packageId/symbols?kind=&q=&cursor=...; GET /api/catalog/symbols/:id; GET /api/catalog/source?sourceRef=...; explicit refresh job API with progress/cancel. Never accept arbitrary root paths in request URLs. Responses include workspaceRevision + catalogRevision; reject stale cross-revision operations.
Package: id/ecosystem/name/version/source(kind, canonical locator or local source id), dependency edges(alias/kind/condition), sourceState(present/missing/blocked/ambiguous), indexState(pending/partial/complete/failed), sourceRootId (opaque), discoveryEvidence[].
Symbol: id/packageId/language/kind/qualifiedName/name/lexicalParentId/ownerTypeRef or candidate owner expression/signature/visibility, sourceRef+hash+range, evidence.
Evidence: method(manifest/lock/syntax/semantic artifact), producer/version, confidenceKind(declared/candidate/definition-resolved/dispatch-resolved), input hashes, target/features/toolchain assumptions, freshness, diagnostics. Do not reuse SemanticState::Fresh to imply syntax has become semantic. Candidate links live apart from CallSite.target.
Store per-package catalog snapshots keyed by package identity + manifest/config/lock hashes + source file hashes + adapter/parser versions + toolchain and applicable feature/target context. Retain logical IDs separately from content snapshot IDs; stale evidence does not become a valid new link. Negative-cache missing sources with bounded expiry or refresh invalidation. Source growth/removal/edits and lock/toolchain/config changes invalidate dependent links. Publish catalog revision atomically, discard canceled/obsolete jobs; do not bump/contaminate workspace source-sharing scope.

## Phased delivery
1) Generic DTO/store + safe auto Cargo manifest/lock/local source discovery + declaration-only catalog and searchable library tree. Proves automatic indexing, not dispatch. Fixtures first.
2) Owner/type-aware diagram participants, candidate navigation and explicit external terminal backend policy. Preserve workspace behavior evaluation order and unresolved fallback.
3) Opt-in semantic adapter when trusted tool available; resolve exact identities incrementally while external behavior remains off. Add other ecosystems through same adapter seam, not Rust-specific UI.

## Required tests
Synthetic Cargo homes/sysroots on Linux/macOS/Windows; no Homebrew assumptions. Renamed deps, duplicate versions/registries, workspace inheritance, target/dev/build/optional deps, patch/vendor/missing lock/source/unsupported git. Assert inactive target is not stated active. No process/network calls, malicious build.rs/proc macros/config commands remain inert. Outside-root path dependencies, symlinks/reparse points, races, FIFO/oversized/non-UTF8 files, parser/cancel/budget bounds. Correct package namespacing and impl/type separation. OpenOptions navigation candidate vs unresolved receiver chain, local shadowing, glob imports/re-exports/cfg ambiguity and trait methods. Every external node terminal even showAll/depth/callback mode; missing body never makes catalog type unusable. No external text in provider payload or workspace graph. Revision conflicts, freshness downgrade on source/toolchain/lock changes, pagination, auth, and retained existing Rust sequence regressions.

Bottom line: replace manual-root-first product direction with automatic local-only catalog discovery. Reuse the secure reader and viewer as implementation details. Deliver type participants and source navigation honestly now; reserve exact receiver/dispatch resolution for a real semantic layer, never for a name heuristic.

## Staged acceptance criteria

### Stage 1: automatic local discovery and declaration catalog

- Opening or reindexing a supported Rust workspace starts bounded discovery without manual library-root entry for trusted standard locations.
- The UI lists manifest/lock-referenced packages and distinguishes present, missing, blocked, ambiguous, pending, partial, and failed states. A missing local source never triggers a download.
- Matching local dependencies expose searchable classes/types, traits, modules, and callable declarations. They do not expose extracted behavior edges.
- The library tree and source viewer identify package/version and evidence. A class can be inspected even when no workspace call resolves to it.
- No process or network invocation is needed for this stage. Fixtures prove that build scripts, proc macros, Cargo configuration commands, and repository toolchain commands remain inert.
- Unsupported manifest/configuration forms produce diagnostics rather than guessed package identities.
- Source-reader capability is explicit per operating system. Linux/macOS success is not Windows acceptance. Windows requires secure reparse/junction-aware tests before the feature is described as cross-platform.
- Workspace graph contents and source-sharing/provider payloads do not expand merely because the catalog exists.

### Stage 2: type participants and terminal library calls

- The UI can show a catalog class/type as a participant, with a separate identity from its methods. Method names stay on messages.
- A selected class/type or exact catalog declaration is not presented as proof that a workspace call reaches it.
- When syntax supports only a possible link, the UI labels it **candidate** and gives its reason. The call remains unresolved in the authoritative call graph. Search matches alone do not create bindings.
- When no safe type evidence exists, the UI retains an unresolved receiver/operation. It does not replace uncertainty with an unlabeled class inferred from a method name.
- For `std::fs::OpenOptions::new`, syntax-based navigation may show a candidate `OpenOptions` type. This must not silently resolve every later fluent-chain call to that type.
- Every dependency/standard-library participant is terminal. The backend rejects external sequence seeds or expansion requests. Depth, show-all, callback options, and UI clicks cannot bypass this rule.
- Users can open a participant's definition separately without creating a sequence traversal into its body.
- Existing Rust evaluation order, flow boundaries, sequence limits, and grouping behavior remain intact.

### Stage 3: exact semantic links

- **Current blocker:** rust-analyzer is unavailable in the researched environment, and Baleyg has no Rust semantic integration. This plan does not claim exact Rust receiver or dispatch resolution.
- A trusted, explicitly approved semantic adapter or imported artifact must supply source-matched evidence before links are labeled resolved.
- Tests cover module/import/re-export lookup, aliases and shadowing, feature/target/cfg assumptions, receiver inference, generics, autoderef/autoref, trait selection, and fluent return types.
- Definition resolution and concrete dispatch resolution have different evidence labels. A trait declaration returned by a definition service is not proof of a concrete runtime receiver.
- Stale, incomplete, or absent evidence degrades to candidates/unresolved states rather than retaining a resolved badge.
- Build scripts/proc macros remain disabled by default. A separately approved semantic service must not become implicit permission to execute workspace tools or fetch dependencies.
- Confirmed external calls still terminate at library type/module participants. Semantic support never enables external behavior traversal.

### Stage 4: additional ecosystems and operating systems

- Each ecosystem adapter supplies the same package, symbol, evidence, source-presence, and terminal-participant contracts.
- Each adapter documents its supported local package sources and unsupported cases. No single package manager's layout becomes the generic contract.
- Each supported OS passes native filesystem safety tests and synthetic-home discovery tests. No hardcoded Homebrew path or developer home is allowed.
- Acceptance for Rust is not advertised as support for all languages. Broader support is claimed only after the corresponding adapters and tests exist.

## Implementation boundaries

Suggested modules are a generic catalog model/service/store, a Cargo discovery adapter, a declaration-only Rust extractor, and an optional semantic adapter. They are design seams, not a requirement to perform a large refactor before the first slice.

Keep the current `/api/rust-sources` endpoints compatible while introducing generic catalog APIs. Do not change the current workspace `Symbol` or `Resolution` meaning silently. Add versioned or additive DTO fields and separate candidate-evidence records. Store and HTTP validation must enforce the external traversal boundary; UI-only disabling is insufficient.

The catalog refresh job must carry the workspace revision that started it. Before publishing, check that the workspace, dependency inputs, and source snapshots still match. A stale or canceled job cannot overwrite a newer catalog. Package IDs identify origin and version; source snapshot IDs identify bytes; neither is proof that a package is active for the current build target.

## Out of scope for the first slice

- Downloading missing dependencies, installing rust-src or rust-analyzer, or unpacking arbitrary archives.
- Executing Cargo, build scripts, proc macros, repository commands, or providers to discover libraries.
- Reimplementing all of Cargo resolution or Rust type inference.
- Claiming a complete public API when cfg, macros, generated sources, re-exports, or budget limits prevent it.
- Full behavior indexing or sequence traversal inside any third-party library.
- Claiming the manual source browser or qualified-name search is semantic resolution.
