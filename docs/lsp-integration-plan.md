# Optional LSP integration — assessment direction

Status: recommendation for a separately gated investigation, **not implemented**. No server
installation/startup, project import, repository build or provider call is authorized by this doc.

## Why both SCIP and LSP?

SCIP gives Baleyg a repeatable semantic artifact tied to a published source snapshot. LSP is a
live request/response protocol for interactive navigation and diagnostics, with optional richer
capabilities. Keep both behind language-aware evidence adapters; neither replaces the syntax
extractors that measure call sites, evaluation order, control scopes and deferred callbacks.

| Need | Preferred starting point |
| --- | --- |
| Reproducible indexed evidence and diagrams | Validated SCIP import |
| Interactive definition/type-definition/reference queries | Optional LSP server |
| Declaration/call-site ranges, control and evaluation order | Existing native syntax adapters |
| Refactoring or edits | Later explicit write workflow; outside the read-only pilot |

LSP must not become a prerequisite for MCP, browsing, cached source, or static diagrams. Ordinary
cached reads must not silently start a server or switch from snapshot evidence to live results.

## Server candidates

These are candidates, not tested configurations or pinned compatibility claims. Confirm protocol,
language/JDK versions, packaging, license, resource use and build behavior before choosing a version.

| Language | Candidate | Main investigation |
| --- | --- | --- |
| Java | [Eclipse JDT LS](https://github.com/eclipse-jdtls/eclipse.jdt.ls) | Correct JDK, source roots, ordered classpath, generated sources and build import policy |
| Rust | [rust-analyzer](https://rust-analyzer.github.io/book/) | Cargo configuration, features/target, build scripts, proc macros and server-version settings |
| Python | [Pyright](https://github.com/microsoft/pyright) | Interpreter/import roots, stubs, environment identity and limits of dynamic inference |
| JavaScript/TypeScript | [typescript-language-server](https://github.com/typescript-language-server/typescript-language-server) over tsserver | Project config and plugins; TypeScript extraction remains separate unsupported work in Baleyg |

Start with Java because its current same-class navigation candidates leave clear cross-class gaps.
Continue the [Java investigation](java-semantic-indexing-plan.md); JDT LS acquisition remains
unapproved. SCIP artifact import can proceed independently; choosing a server does not select or
execute a SCIP producer.

## Read-only bridge boundaries

1. **Explicit lifecycle.** The user selects the project and enables a pinned server profile. Bind the
   process to one workspace/worktree and configuration identity. Do not share mutable server state
   across projects or attach to an editor-owned server without a supported isolation contract.
2. **No implicit project execution.** Server startup/import may run build tooling, download
   dependencies, execute plugins/processors or load build outputs. Verify configuration by version;
   do not assume a generic “read-only” LSP client disables these actions. Require an explicit policy
   and observe actual child processes/network access before any real-project pilot.
3. **Narrow protocol.** Begin with supported definition, type-definition and reference queries.
   Negotiate capabilities. Call/type hierarchy is optional and server-dependent. Reject unsolicited
   workspace edits and unapproved command execution; do not advertise write capability. Hover content
   is untrusted text, not authoritative structure or safe HTML. Diagnostics are signals, not a proof
   that every reference in a project resolved correctly.
4. **Two source bases.** Distinguish a published cached snapshot from live working-tree/server state.
   Match exact bytes, negotiated position encoding, document version/hash, root, server configuration
   and relevant dependency basis before joining a result to a measured ID. Matching the queried file
   alone does not prove its dependency graph matches. If basis cannot be established, return a labelled
   live navigation result outside the cached graph; never silently promote it into a published diagram.
5. **Validated targets.** Normalize Location/LocationLink and validate paths/ranges. A result can point
   outside the indexed workspace, into generated sources, an archive or a server-specific URI. Keep
   those as external/unavailable boundaries unless a separately authorized source reader exists.
   Opening a result is explicit; never fall back to arbitrary filesystem access.
6. **Honest claims.** A definition location is navigation evidence. It is not a universal stable symbol
   ID, complete overload proof or runtime dispatch result. Preserve syntax candidates, compiler/profile
   claims, unresolved targets and ambiguity separately. Implementation lists are not executed receivers.
   Third-party behavior remains terminal even if its declaration/source is available.
7. **Bounded work.** Cap response bytes/results, requests, process resources and diagnostics queues;
   enforce timeouts/cancellation. Discard late results after project/version/session changes. On crash,
   show unavailable status; no automatic download, build or unbounded restart loop.

Existing `Internal` call traversal must not automatically follow an LSP declaration target. The
[SCIP plan](scip-multilanguage-plan.md) describes the same binding-versus-dispatch gate. New overlays
must preserve current measured IDs and propagate provenance/warnings into views and evidence packets.

## Small Java pilot, after explicit tooling approval

- First use a fake LSP transport with synthetic fixtures to test capabilities, encoding, lifecycle,
  stale responses, external URIs, ambiguity, bounds and rejected edit/command requests.
- Then run the approved pinned server against a disposable Java fixture, not the inspected application.
  Include overloads, inheritance/interfaces, missing classpath, Unicode, edits and dependency drift.
- Compare navigation with existing syntax evidence and admitted SCIP bindings where available. Record
  source bytes, latency, startup/resource cost, incorrect targets and unresolved results—not just success.
- Prove that startup/import executes no unapproved build, processor, download or repository code.
- Only after that choose whether a larger project's classpath/import policy is worth the added cost.

This investigation is independent of the [four-tool MCP pilot](mcp-readonly-pilot-contract.md).
Do not expand those tools into implicit live-LSP operations; a later capability needs its own schema,
source-basis labels and grant. Terminal hosting and ACP transport are also independent of LSP.
