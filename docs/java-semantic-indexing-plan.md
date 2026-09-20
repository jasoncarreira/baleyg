# Java semantic indexing: feasibility and first-slice plan

## Status and recommendation

Design only. This public version omits private application and environment inventory.
It is not a successful semantic-index run or a claim of available local tooling.
The design authorizes no application execution, Gradle, Maven, annotation
processor, provider, dependency installation or download.

**Keep the existing tree-sitter graph as the identity/range authority. Add an
opt-in, offline compiler-evidence overlay.** Start with explicitly selected,
source-only compilation units plus a pinned JDK. Promote only exact, validated
bindings. Do not replace the extractor or infer application runtime targets.

**Recommended first investigation: the JDK 25 compiler API for a small,
source-only cached overlay, subject to confirming this engine choice and tool availability.**
JDK 25 is the proposed target, not a disclosed application requirement. An existing verified installation would avoid a download. This is not LSP, and it is not a silent substitution for the possible
JDT Language Server integration discussed with the user. If LSP itself is the requirement, stop after this
plan and obtain separate permission to acquire a pinned JDT LS distribution.

JDT ASTParser remains suitable for a later direct-engine adapter. JDT LS can
provide an LSP adapter, but has a larger runtime and project-import safety
surface. Tool availability must be verified separately.
Both routes need a verified Java-25-capable distribution and approved tool
acquisition. An older JDT does not gain Java 25 support merely by running on
JDK 25. Useful application-wide coverage with any engine still depends on a
trustworthy classpath; a language server does not remove that requirement.

## Tooling prerequisites

- Pin a verified JDK compatible with the explicitly selected source level. This plan uses Java 25.
- Verify compiler-module availability without executing the inspected application.
- Pin an absolute tool path rather than relying on cwd-sensitive launchers.
- Declare preview use explicitly; disable it by default.
- Verify any JDT/JDT LS release and its dependencies separately before acquisition.
- Existing cached jars do not prove a resolved compile classpath. Require an ordered, attested manifest.
- Keep application build/config inventories and local artifact metadata private.

## Existing contracts that must remain true

| Area | Current contract | Required integration |
| --- | --- | --- |
| `src/indexer_java.rs` | Syntax-only Java; symbol/call/region IDs use path, content hash, byte range and tree-sitter kind | Retain these IDs and ranges byte-for-byte; retain callers, parents, regions, callbacks and ordinals |
| `src/model.rs` | Graph schema 1; `Resolution` is internal/external/unresolved/ambiguous; provenance has only source and freshness | Add explicit optional compiler evidence; resolution alone cannot describe dispatch or completeness |
| `src/indexer.rs` | Optional SCIP applies only to JavaScript; aggregate freshness currently describes its flat manifest | Keep SCIP isolated; introduce Java-specific input attestation/status; do not reuse JS freshness as Java proof |
| `src/store.rs` | Cache schema 3; JSON payloads; exact byte/line validation; atomic revision publication | Persist overlay and input manifest in the same revision; round-trip them on restart |
| `src/classes.rs` | Class/member projection derives from cached source and graph IDs; type links are syntax candidates | Preserve projection joins; do not silently relabel existing class relations as compiler-proven |
| `src/navigation.rs` | Revision-bound, bounded reads from cached evidence; exact member selectors | No on-demand compiler, filesystem search, or classpath access from navigation |
| `src/planning.rs` | Packet hashes request, revision, context, full cached source files and warnings; 1 MiB cap | Compiler claims and limitations must be in the hashed packet; no hidden live evidence |
| `src/answer.rs`, `src/acp.rs` | Provider uses packet-only evidence and exact citations | No new provider call for resolution; retain consent/attempt boundaries and source privacy |
| `src/behavior_java.rs` | Measured calls join by caller/path/exact range; sequence participants are currently unresolved hints | Do not convert a declaration binding into an executed body or silently mislabel it as unresolved |

Two current gaps matter before publishing semantic edges:

1. `Store::query_view` currently emits truncation warnings, not the persisted
   graph diagnostics. A diagnostic alone will **not** reach question packets.
   Add bounded, source-free Java semantic warnings from the same read snapshot.
2. `query_view` automatically expands every `Internal` target. Compiler-selected
   instance methods may dispatch to an override at runtime. Separate declaration
   navigation from body expansion before enabling such edges.

The final candidate classifier also recognizes external SCIP callables by the
`().` suffix. Java bindings must have their own explicit classifier. Do not
forge SCIP-shaped IDs or feed Java candidates through that suffix heuristic.

## Meaning of a binding

A compiler can identify the **compile-time selected declaration** after import,
type, generic and overload resolution. This is not observation of an execution.
An interface method binding is not proof of the implementation selected by a
container, proxy, reflection, factory, or actual receiver instance.

- Keep `Internal` for a uniquely mapped, indexed declaration. Add binding meaning
  `declaredTarget`, not `runtimeTarget`.
- Record dispatch separately: `static`, `special`, `virtual`, `interface`, or
  `unknown`. Use `special` only when the engine and supported syntax establish
  it, such as an explicit constructor/super call. Leave difficult cases unknown.
- Allow navigation to the selected declaration. Do not automatically expand a
  virtual/interface/unknown target as its runtime body in question or behavior
  traversal. First slice can conservatively stop all instance dispatch.
- `External` means a compiler-proven declaration outside the indexed graph and
  from an attested binary origin. It does not mean a missing workspace source or
  failed range join. Keep missing/generated/unmapped source targets unresolved
  with an explicit diagnostic.
- A null/recovered/error binding stays `Unresolved`. A compiler-reported overload
  ambiguity may be `Ambiguous` without fabricating candidates. Populate
  candidates only with independently measured compiler evidence.
- Runtime dispatch, injection wiring, proxies, reflection, annotation expansion,
  Lombok, generated members, implicit calls, and points-to analysis are out of
  scope. Do not synthesize constructors/accessors or new graph nodes for them.

## Engine options and dependencies

### LSP / JDT Language Server versus the compiler API

LSP is a client/server protocol, not an independent Java resolver. JDT LS uses
JDT compiler and project-model machinery. A definition request can identify a
compile-time declaration, but does not report actual runtime dispatch, a full
classpath proof, or a cryptographic provenance record. It may return no result,
a recovered result, multiple locations, or a binary/decompiled URI. A single
`Location` is not sufficient proof for `Resolution::Internal`.

A future LSP adapter would need to:

- Pin a Java-25-capable JDT LS release, its bundled JDT/tool dependencies and
  launcher JDK. Install/download permission is not currently granted.
- Launch against a private immutable snapshot workspace and private server data,
  never import the live application. Disable automatic builds, Maven/Gradle
  import, wrapper execution, dependency/source downloads and processor support
  in that specific server version. Verify those settings with synthetic tripwire
  tests. Settings are not a sandbox; prevent forbidden child processes, network
  access and writes to the application independently.
- Supply source roots and classpath explicitly for an unmanaged/source-only
  project. Do not depend on automatic build-file discovery for correctness.
- Wait for the server's initialization/indexing lifecycle and verify the actual
  analyzed snapshot. LSP document versions alone do not attest dependent units,
  classpath, asynchronous diagnostics, or index completion.
- Negotiate/record position encoding and map `Location`/`LocationLink` ranges
  to exact cached syntax declarations. Definition ranges often select names,
  not full method ranges. Reject generated/decompiled/outside-snapshot URIs and
  unresolved/recovered outcomes. Do not treat `implementation` or call-hierarchy
  results as runtime traces.
- Add a trusted engine-specific evidence/diagnostics channel if vanilla LSP
  cannot establish binding quality and input completeness. Cache all validated
  results before publication. Navigation HTTP requests must never contact LSP.

This is useful when editor-like navigation and an ongoing language-server
session are desired. It is **not the smallest first slice** for an offline,
revision-pinned graph. The direct javac API has fewer runtime dependencies,
explicit source inputs, synchronous analysis completion and direct diagnostics
and type access. It can be a no-download proof-of-concept path when a verified JDK is already available. If
the user specifically requires LSP rather than the semantic capability, do not
implement javac without confirming that change.

### JDT ASTParser (direct-engine option after approved acquisition)

Use a small Baleyg-owned Java helper, not Eclipse IDE, JDT Language Server,
Gradle tooling, or a project import. Configure `ASTParser` in compilation-unit
mode with a verified Java 25 DOM/compliance level, explicit compiler options,
UTF-8 encodings, source roots and a deterministic binary environment. Enable
binding resolution. `createASTs` supports a bounded batch with shared bindings.
Use `resolveMethodBinding` / `resolveConstructorBinding`, normalize generic
method bindings to their declaration, and map declaring source nodes to the
existing graph. Recovery can provide diagnostics, but `isRecovered()` results
must never become resolved graph edges.

JDT's DOM parser resolves class-file metadata without executing application
classes. Do not invoke its batch compiler, annotation-processing implementation,
Eclipse extension registry, project builders, language-server project import,
or agent hooks. There is no useful `-proc:none` switch to pass to ASTParser as
if it were javac: the safety boundary is the selected DOM-only API and isolated
runtime. Do not register processors or compiler extensions.

Required tool dependencies: a **pinned, verified Java-25-capable**
`org.eclipse.jdt:org.eclipse.jdt.core`, its matching compiler artifact if that
release requires one, and the exact transitive Eclipse runtime closure declared
by that version. Common dependencies include resources/runtime/filesystem/text,
Equinox/OSGi support and jobs/preferences/content-type components. This list is
illustrative, not a verified lockfile. JDT core alone or ECJ alone is not an
assumed standalone ASTParser installation.

Before acquisition, select and verify the exact release and Java 25 support,
including any release-specific support patch. Record coordinates, license,
origin, SHA-256, Java runtime requirement and full dependency closure. No exact
JDT version was verified during this offline inventory. Reject the wrong DOM
level instead of parsing Java 25 with older recovery and calling it semantic.

### javac compiler API (no-download alternative when already installed)

Use JDK 25's `JavaCompiler`, `JavacTask.parse()` and `analyze()`, `Trees`,
`SourcePositions`, `Elements` and `Types`. Stop before `generate()` and never
call application entry points. A Baleyg-owned file manager supplies approved
source snapshots and reads approved binary metadata only.

Set `-proc:none`, `-implicit:none`, `-encoding UTF-8`, and `--release 25`.
Leave preview disabled; add `--enable-preview` only for an explicitly declared,
tested preview mode on the matching JDK. Set `CLASS_PATH`, `SOURCE_PATH`,
`MODULE_PATH`, `ANNOTATION_PROCESSOR_PATH` and
`ANNOTATION_PROCESSOR_MODULE_PATH` to explicit empty locations for source-only
mode; never omit classpath and inherit the current directory. Supply all selected
snapshot units directly through controlled `JavaFileObject` inputs, not implicit
workspace lookup. Do not configure patch modules or extra module source paths.
Only the pinned JDK platform is available through the controlled platform/system
locations. Set an empty processor collection as additional defense. Do not accept user-supplied arbitrary compiler arguments,
`-Xplugin`, processors, Java agents or compiler plugins. `-implicit:none` alone
is not a no-discovery guarantee: restrict file-manager source lookup as well.
Parse/analyze only; the helper compilation itself also uses `-proc:none`.

**Do not trust non-null `Trees.getElement` alone.** javac can expose a selected
or recovery element after overload ambiguity, missing dependencies or erroneous
argument attribution. A valid-looking name or executable element is not proof.
The first slice should use the following conservative acceptance gate:

1. Analyze the complete selected source batch once. Do not accept any call
   bindings if the batch has any compiler `ERROR`, a fatal failure, missing
   required input, truncated diagnostics or incomplete analysis. Keep diagnostics
   open through all subsequent type/element queries because lazy symbol
   completion can report later errors. Do not run one isolated method to hide
   errors in the selected dependency universe.
2. Require a supported explicit call AST, an `ExecutableElement` of the expected
   kind, exact source/range joins, and a selected method/constructor signature
   that agrees with the normalized declaration. Reject synthetic/unmapped
   declarations and unsupported attribution forms.
3. Inspect receiver, argument, invocation and selected executable types, including
   owner, generic arguments/bounds, array components, parameters, result and
   thrown types as applicable. Reject `TypeKind.ERROR`, unexpected missing/
   `NONE` types, failed type queries or unsupported proof cases. `void` results,
   null literals and constructors need explicit tested handling, not a blanket
   non-null-type shortcut. Recursive generic bounds need bounded cycle-aware
   validation. Inspect invocation attribution as well as declaration signatures.
4. Require an attested selected source or binary origin and record the selected
   input universe. Only after the final error check may Rust apply the overlay.
   No name/arity fallback, recovered candidate or guessed overload is promoted.

This intentionally discards valid local calls when unrelated units in the batch
have errors. It is simpler and safer than a per-unit/per-method error-cone policy,
which is deferred. Use small **explicit** self-contained source batches, not
automatic exclusion of failing files until the compiler becomes quiet. Missing
framework/generated dependencies will often leave the application entirely
lexical in this slice. A clean, restricted batch means compiler validity for
those declared inputs, not classpath equivalence to the application's real build.
Warnings and absent build provenance still limit overall completeness.

For both engines: run only trusted helper/tool classes on the **JVM runtime
classpath**. Application jars, if later approved, are **compiler input paths**,
not JVM runtime classpath entries. Reading their class metadata is not class
loading. Remove inherited `CLASSPATH`, `JAVA_TOOL_OPTIONS`, `JDK_JAVA_OPTIONS`,
`_JAVA_OPTIONS` and equivalent launcher injection. Use an absolute Java binary,
a private working directory outside the application, bounded heap/time/output,
cancellation, no network, no application writes and no child build tools.
Flags alone are not a filesystem/network sandbox; enforce those restrictions
with the available OS/process boundary before untrusted-input production use.

## Minimal overlay protocol and cached provenance

Keep the engine protocol independent from graph identity. Input is a bounded,
versioned manifest plus immutable source snapshots. Output is a bounded,
versioned set of declaration/call facts and diagnostic codes. Rust validates all
facts before applying any changes. The helper cannot create arbitrary graph IDs.

Proposed cached metadata (implementation must finalize names and serde defaults):

- Optional `IndexStats.javaSemantics` summary, persisted in the existing revision
  stats JSON: engine/version, helper protocol/digest, manifest digest, requested
  source level/preview, JDK identity, source-set identity, classpath mode,
  completeness (`partial`/`complete`/`unavailable`), counters and reason codes.
  `complete` is relative to the declared input universe, never the running app.
- Optional `CallSite.javaBinding`: manifest digest, engine name, declaration
  descriptor, binding quality, dispatch kind, source/binary origin category,
  and explicit evidence meaning `declaredTarget`. Internal targets remain the
  original graph IDs. External IDs use a Java namespace plus attested artifact
  and declaration descriptors, not display text or a SCIP suffix.
- `Provenance.source` names the engine/overlay, for example
  `tree-sitter+jdt-declared-binding` or `tree-sitter+javac-declared-binding`.
  `semantic=Fresh` means the evidence matches attested inputs, **not** complete
  classpath coverage or resolved runtime dispatch. Syntax-only fallback remains
  unavailable. Do not relabel every symbol because another call resolved.
- The immutable private manifest records all selected source path/hash pairs,
  source-root/source-set membership and order, toolchain/helper hashes,
  source/target/preview/encoding options, ordered binary paths and hashes,
  module-path state, config inputs, and explicit exclusions. Include missing or
  skipped input categories. Unknown build-derived configuration stays unknown.
- For slice one, keep the legacy aggregate semantic-state behavior unchanged and
  expose Java state separately. Do not mark mixed-language indexing globally
  fresh because the Java helper succeeded. A later cross-language aggregate can
  be designed explicitly without changing SCIP's established meaning.

Use optional serde-defaulted fields so older lexical snapshots remain readable.
The cache stores stats/call JSON, so new SQL columns are not inherently needed.
Review graph API versioning and frontend types before landing; if consumers
cannot tolerate the additive contract, increment graph schema and rebuild the
cache explicitly rather than silently breaking it. Add round-trip tests either
way. The manifest must remain reachable for audit after restart, but its absolute
local paths must not be copied into provider packets.

Cache invalidation includes any selected source edit/add/remove, compiler/JDK
change, classpath entry/order/content change, source-root/source-set change,
module/preview/options change, or helper/protocol change. Reuse only an exact
manifest match. Do not attach stale targets to changed files. Unavailable/stale
artifacts produce lexical fallback and a reason; publish overlay + snapshot in
one transaction. Cached API reads must remain filesystem- and compiler-free.
They describe the indexed revision, not a live revalidation of disk state.

## Exact identity and range joins

1. Extract the current syntax graph first. Supply the compiler with the same
   bytes, not a reread of a changing working tree. Keep compiler snapshots
   private and delete temporary copies when no longer needed.
2. JDT and javac report character offsets in Java strings (UTF-16 code units).
   Convert offsets using a checked UTF-16-to-UTF-8 table built from the original
   source. Reject offsets inside a surrogate pair, outside the source, or with
   unavailable/synthetic positions. Recompute 1-based lines and **byte columns**
   from original bytes. End offsets remain exclusive.
3. Match supported call syntax by path/hash/kind/exact full call byte span.
   Nested/chained calls can share a start offset, so start-only joins are invalid.
   Never use nearest-line or name-only matches. Deliberately unsupported span
   differences remain unresolved rather than changing a cached call range.
4. Match source declarations to the existing declaration span and callable kind.
   Account explicitly for annotations/modifiers and named type ownership; use
   declaration-name anchors only to validate a unique exact syntax declaration,
   not to widen or invent its range. Reject ambiguous joins. Normalize generic
   substitutions to the declared method before joining overloads.
5. Keep graph IDs, ownership, ranges, ordering, regions and callback arguments
   unchanged. Change only validated resolution/target/candidates and evidence.
   Do not create nodes for compiler-synthetic/default methods or implicit calls.
6. If tree-sitter cannot extract a Java 25 feature, compiler support alone does
   not make that feature indexable. Report parser/coverage limitations and keep
   those units lexical. Never invent replacement IDs to bypass the limitation.

## Classpath, completeness and fallback policy

**Slice one:** explicit source set + JDK platform types; empty third-party
classpath and module path; no build-output roots or generated sources. Main and
test sources are separate source sets. No guessed source roots, implicit imports
from arbitrary folders, wildcard cache jars, or evaluation of build scripts.
Start with small synthetic fixtures owned by Baleyg, not the private application.

A source-only run may resolve self-contained source calls and JDK methods. It
cannot resolve most framework/generated behavior. Overall application
completeness remains `partial` even if selected fixtures type-check. Count
selected/skipped/error units, measured calls, promoted internal/external calls,
recovered/ambiguous/unmatched calls and excluded generated/dependency inputs.
Fail closed for the whole selected batch on compiler errors in the initial
slice; a future fine-grained error-cone policy requires separate tests and design.

Later, accept a user-supplied **ordered, explicit, hashed compile-classpath
manifest** containing existing approved local binary jars. Validate jar type,
size, zip limits, origin, duplicate class conflicts and path boundaries. Do not
include source/javadoc jars or processor-only jars automatically. Multiple
cached versions cannot all be added to improve resolution. Existing generated
sources/output classes need separate provenance and explicit approval; their
presence on disk is not evidence that they match current sources.

Static Gradle/Maven text is a hint, not resolved configuration. No dependency
resolution, repository access, tooling-model import, buildSrc execution,
annotation processing, or generator task is authorized by this plan. A complete
application classpath may remain unavailable without a user-provided manifest.

Emit bounded reason codes such as `java-tool-unavailable`,
`java-language-level-unsupported`, `java-classpath-partial`,
`java-generated-sources-excluded`, `java-binding-recovered`,
`java-compiler-errors`, `java-binding-range-unmatched`, `java-semantic-stale`,
`java-helper-timeout`, and `java-declared-target-not-runtime-dispatch`.
Keep source snippets, compiler stderr and absolute paths in private artifacts.
Public diagnostics use sanitized relative paths, counts and fixed messages.
Compiler failure must preserve the lexical graph; user cancellation must still
abort publication rather than publish a misleading complete revision.

## Implementation slices and acceptance tests

### A. Contract + fixture-only overlay

1. Finalize engine choice and exact authorization below.
2. Add a pure Rust overlay validator/manifest module and optional schema fields.
   Keep navigation workers' files untouched until their changes are integrated.
3. Add the isolated Java helper under a Baleyg runtime directory and tests using
   synthetic Java only. No build system is needed for the javac alternative.
4. Add an explicit opt-in index option and private artifact handling. Default
   indexing remains syntax-only; missing tooling has deterministic fallback.
5. Integrate after lexical extraction and before final counts/publication. Keep
   JS SCIP classification separate from explicit Java resolution.
6. Cache metadata atomically. Surface bounded same-revision warnings in views
   and packets. Add conservative dispatch-aware expansion before internal Java
   edges can affect multi-hop evidence. Keep existing class hierarchy links
   lexical. A sequence may remain a boundary, but must label declared binding
   versus runtime-unknown honestly; no cross-body sequence expansion is needed.

Required tests:

- Same-file and cross-file static calls, imports/static imports, overloads,
  generics, inheritance, explicit constructors, JDK external methods.
- An interface/virtual call binds to its declaration, not a guessed concrete
  implementation; no runtime body expansion. Unresolved/external calls stay
  bounded. Lambda arguments do not imply execution.
- UTF-8 non-ASCII, supplementary Unicode before calls, CRLF, raw Unicode escape
  syntax, annotation/modifier spans, nested calls with a shared start offset,
  constructors/records and supported Java 25 syntax. Preview off by default.
- Missing dependency, wrong JDK/source level, malformed syntax, overload error,
  recovered binding, generated member absence and unrepresentable source span
  all produce explicit fallback, not guessed targets. Include ambiguous overloads
  where `getElement` is non-null, an error-typed argument with a plausible method,
  missing superclass/generic bounds, errors in another selected unit, and lazy
  completion diagnostics. The selected batch must produce no promoted bindings
  after any compiler error, even if some calls appear individually valid.
- IDs/ranges/owners/regions/callbacks unchanged compared with the lexical graph.
  Existing Java indexer and behavior tests remain valid with semantics disabled.
- Source/classpath/tool/options changes invalidate evidence. Old snapshots read
  without new fields. New metadata survives store reopen and revision conflicts.
- Packet integrity includes binding evidence and caveats. Provider preparation,
  navigation, class diagrams and source lookup do not start a compiler or read
  application files. Source and secret paths do not enter diagnostics.
- Synthetic sentinel processor/plugin/initializer tests show no execution;
  helper invokes no build tools, creates no application outputs, and makes no
  network access. Exercise timeout, cancellation, malformed helper JSON,
  symlink/path rejection and output limits. Flags are not the sole assertion.

Run Baleyg's native Rust test commands after implementation, including targeted
`java_indexer`, `java_behavior`, `store`, `planning`, `questions_http` and
`java_python_http` suites. Add an offline optional helper integration suite with
an explicit JDK/tool path; ordinary lexical tests must not need Java or downloads.
No such tests were run as part of this read-only design task.

### B. Read-only application pilot (separate approval)

Analyze a small explicit source set from the indexed snapshot with platform-only
classpath. Publish a private coverage report with counts and fixed reason codes.
Expect low resolution where dependencies are absent. Do not weaken acceptance
rules to inflate coverage. Keep provider transmission disabled.

### C. Approved local classpath enrichment (later)

Consume an explicit manifest of selected existing binary jars and, if approved,
attested generated source snapshots. Repeat the same acceptance checks. Full
project import, downloads, generators and annotation processors remain excluded.
Runtime dispatch analysis is a separate future feature, not this slice.

## Exact next approvals / blockers

1. **Engine choice:** confirm the recommended JDK 25 javac helper using the
   verified existing installation, if available, with no downloads, as the initial semantic capability.
   If LSP is specifically required, select JDT LS instead and verify its exact
   Java-25-capable release, launcher JDK and bundled/transitive tool lockfile
   before acquisition. Direct ASTParser is a third option, not an LSP server.
2. **If JDT/JDT LS:** authorize acquisition of only the reviewed compiler/server
   tool runtime from named official artifact repositories or official release
   distributions into a Baleyg-owned cache, with pinned versions/hashes/license
   review. This does not authorize
   application dependency downloads. No acquisition is authorized yet.
3. **Implementation/synthetic execution:** authorize compiling and running only
   the Baleyg-owned helper against synthetic fixtures, outside the private app,
   with no processors/plugins/build tools/network and no application class loading.
4. **Application pilot:** separately approve compiler static analysis of specified
   cached application source snapshots. This permits parsing/type attribution,
   not running application code, build scripts or annotation processors. Keep
   all source-bearing output in the private Baleyg state directory.
5. **Coverage beyond source-only:** provide/approve an ordered classpath and
   source-set manifest. Selecting all cached jars is not acceptable. Generated
   outputs require their own explicit input/provenance decision.

Verify JDK availability before selecting the no-download route. The JDT route needs
tool acquisition/verification. Neither route may claim complete application coverage
without attested classpath/source-set/generated inputs. No implementation or validation
is claimed by this plan.
