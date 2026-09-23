# Coding-agent integration plan

Status: **proposal, not implemented**. Revised 2026-09-23 for the
[local topology](local-topology.md): agents use a per-checkout stdio MCP server with no grants.
This work inspected Baleyg, the local Mimir checkout,
and the installed Herdr CLI/schema. It did not start or prompt Herdr/Mimir coding agents,
call project-configured providers, change either runtime, index projects, or change the
running Baleyg deployment.

## Recommendation

Make Baleyg a **code evidence and diagram workbench** that any coding agent can use.
Keep Herdr **optional**. Reuse Mimir for Mimir-managed agent execution and permissions.
Add terminal windows/tabs inside Baleyg as an accepted UI target, without creating another agent
harness or provider router or taking ownership of Herdr-managed terminals.

Four independent integration surfaces:

1. **Agent tools:** structured navigation, cached source, and text search through a portable
   Baleyg tool API, initially exposed through a local stdio MCP adapter.
2. **Diagram artifacts:** agents can request deterministic diagrams or publish clearly
   labelled explanatory/proposed diagrams into Baleyg.
3. **Optional Herdr adapter:** discover and associate workspaces/panes/agents, then add
   explicitly authorized focus/prompt actions later. Tools and artifacts work without Herdr.
4. **Embedded terminal windows/tabs:** show real PTY-backed sessions for direct agents explicitly
   launched by Baleyg. This accepted UI target is a separate delivery slice, not part of MCP reads.

**MCP is the primary public agent integration contract.** It must work for non-Mimir agents.
Mimir integration is a separate adapter, not a prerequisite or owner of the tool catalog.
A Herdr workspace does not have to contain a Mimir agent, and a Mimir session does not have
to be recognized as a Herdr agent.

### Two peer agent entry paths

Agents may run **directly in a terminal**, optionally inside Herdr, or **through ACP**, mostly
using Mimir. Both paths consume the **same MCP catalog and evidence contract**. Neither is a
fallback for the other. ACP is an optional client/session UI integration, not the tool transport
or an authorization mechanism. No new Baleyg harness loop is proposed.

Baleyg may launch an owned adapter/session connection when the user explicitly requests it.
It must not create a second Mimir ACP client beside an editor that already owns that connection.
An existing editor/client should keep ownership; cooperate through supported integration points.
Herdr discovery must not silently edit any agent's MCP configuration.

### Portable server configuration

The agent client launches `baleyg mcp` in the checkout it works in. The server resolves its
workspace from its working directory and serves only that checkout's index at
`<root>/.baleyg/index.db`. It needs no daemon, port, descriptor, token or grant, and it exits with
its client. There is no project registry or `--project PROJECT_ID` prerequisite. See the
[MCP contract](mcp-readonly-pilot-contract.md) for tool schemas, pins, errors and tests, and the
[local topology](local-topology.md) for index placement, the shared fact cache, writer election,
real-time native refresh and cleanup.

Illustrative configuration, **not a current working command**:

```json
{ "mcpServers": { "baleyg": { "command": "baleyg", "args": ["mcp"] } } }
```

Stdout is MCP-only. The same configuration works in every checkout and worktree because the
workspace comes from the launch directory, not from configuration.

A remote MCP client cannot reach local stdio automatically. Mimir's proposed local proxy extension
is one bridge. MCP standardizes tool transport; it does not replace runtime sandboxing or disclosure
approval. No network listener or model/provider call is needed for tests.

## Ownership

| Concern | Owner |
| --- | --- |
| Herdr-managed panes, tabs, layout and terminal-agent lifecycle | Herdr, if connected; Baleyg must not take over |
| Real PTY and embedded terminal tabs for direct agents explicitly launched by Baleyg | Baleyg owns only these launched sessions; existing runtime owns its agent loop |
| Mimir agent loop, model/provider choice, ACP session, Hands execution and permission policy | Mimir |
| Other coding-agent runtimes | Their existing harness |
| Per-checkout indexes, native refresh and indexed evidence | Baleyg |
| Lifetime of each `baleyg mcp` process | The agent client that launched it |
| Structural queries, static sequence/class projections, bounded snapshot text search | Baleyg |
| Diagram validation, durable versions, evidence references, browser display | Baleyg |
| Permission to disclose local information to an agent | Configuring `baleyg mcp` for that agent (whole checkout); remote bridges add runtime/destination policy |

Do not duplicate credentials, permission caches, run retries, or pane ownership across systems.
An external-agent integration must not reset or bypass existing Baleyg Jev/ACP accounting.

Architecture: [context](architecture/agent-integration-context.md) and
[containers](architecture/agent-integration-containers.md).

## What exists and what must be added

### Baleyg

Reusable today: cached source and symbol lookup, file methods, outgoing call graph, static
sequences, Java/Python class diagrams including hierarchy, and source/member navigation.
Method candidates retain their original IDs/ranges and uncertainty. Same-class Java candidates
are **not** compiler bindings. Unresolved graph edges remain unresolved.

Missing: a portable tool server, bounded snapshot grep, uniform revision guards, per-checkout
indexes, a file watcher and delta publication, versioned diagram artifacts, artifact
notifications, and deep links. Existing saved views are raw graph queries, not a general
sequence/class/authored-diagram artifact system. Their writes have no artifact CAS/ownership.
Incoming **call** hierarchy is not implemented; class relations/hierarchy are a different capability.
Current Baleyg ACP is a tools-disabled evidence-answer adapter, not the agent manager to extend.

### Mimir: important integration constraint

The reviewed local checkout points to `jasoncarreira/mimir`, local main ref
`b0759004427f510a620142fc997f4553f4b88280`. This is not a clean-worktree or deployed-version claim.

Implemented path: ACP client -> local credential-aware proxy -> local Unix socket or SSH relay
-> existing Mimir daemon/runtime. Hands executes on the client/proxy host; native tools execute
on the daemon host. Those hosts and their paths are not interchangeable.

**The current ACP provider accepts exactly one `mimir-hands` provider with five exact tools**:
read, edit, shell, Python, and scope request. An extra Baleyg MCP provider/tool is rejected today.
Mimir also has a generic MCP client, but that runs tools on the **daemon host**. It does not
magically expose local Baleyg to a remote daemon.

The right integration is a **versioned, server-owned Baleyg provider profile** through the
existing local proxy, backed by the same Baleyg tool service used by other agents. Review
provider admission, routing, schema digests, resource/taint policy, and permission behavior.
Do not weaken Hands v1 or hide a new capability inside arbitrary shell commands.

A temporary demo could invoke a bounded Baleyg CLI using already-approved Hands shell/Python,
or read an explicitly exported artifact through Hands read. That is an execution-capability
bridge, not the desired typed read-only interface or a new permission bypass.

Also: ACP currently allows **one client per daemon home**. Baleyg must not open a second ACP
connection to observe an editor's session. The initial workflow is agent-initiated tool use;
future reverse task dispatch must cooperate with the existing client/proxy or another supported
Mimir control surface. Terminal output scraping is not a substitute for structured run events.

### Herdr: useful but optional

Read-only inspection found Herdr **0.8.2**, socket protocol **20**, schema version **1**.
The installed schema has `session.snapshot`, workspace/pane/agent metadata, and `events.subscribe`.
Workspace/worktree metadata, pane cwd, and optional agent-session references can suggest bindings.
No new Herdr agent process or workspace was created during this review.

Initial adapter:
- Explicitly attach to a chosen local Herdr connection; support no connection as normal behavior.
- Discover metadata and suggest project/worktree associations. The user confirms ambiguous mappings.
- Subscribe to lifecycle changes. Resnapshot on reconnect/gaps; do not assume replay guarantees
  from the existence of a subscription API.
- Display an associated pane/agent and its **observed** status. `done` is not proof a Baleyg task
  succeeded, and `unknown` is not failure or completion.
- Never auto-import/reindex every Herdr workspace, switch the active browser project, change
  terminal focus, read transcripts, or execute commands merely because a workspace appears.

Later, explicit user controls can focus a known pane or prompt a supported recognized agent.
Revalidate its current identity, readiness, and project binding first. Do not route arbitrary
socket RPC or key injection from a remote agent through Baleyg. The current supported start kinds
do not list Mimir/Prime by those names; a pane association does not manufacture that support.

## Accepted UI target: terminal windows inside Baleyg

The user should be able to work in one place. Deliver embedded terminal windows/tabs backed by a
**real PTY** for direct agents that the user explicitly asks Baleyg to launch. Input, resize, output,
exit state and reconnect behavior follow the proposed
[terminal workbench contract](terminal-workbench-contract.md) and need a separate permission review.
This is a terminal presentation and owned-process lifecycle feature, not a new agent harness loop.
The agent consumes the same MCP catalog whether shown here or in a standalone terminal.

For a Herdr-managed terminal, embed only through an actual supported attach/stream interface whose
input, resizing and lifecycle ownership are explicit. Metadata, `pane.read` or polled text is not a
PTY stream. Do not invent terminal visibility from those APIs, synthesize a fake polling TTY, or take
ownership of Herdr's process. Until a supported attach interface is verified, show associations and
explicit focus controls as available; do not claim the embedded Herdr terminal exists.

ACP exposes conversation, tool and approval UI, **not a terminal stream**. Give ACP sessions their
own conversation/tool/approval tabs. Show a terminal only when an actual PTY is separately provided.
Do not launch a redundant Mimir ACP client to fill a tab already owned by an editor. A user-requested
Baleyg-owned adapter/session is allowed where the runtime supports it.

Embedded terminals can execute/edit far beyond the read-only MCP catalog. Their launch/input authority,
transport security and effective runtime permissions require a separate slice and explicit approval.
No terminal implementation or agent launch is part of this documentation change or MCP Phase 1.

## Project and run identity

Each checkout is its own workspace, identified by its index. `evidenceBasis` is
`{indexGeneration, indexRevision}`; the generation is created with the index and changes when it is
rebuilt from nothing. These are proposed fields, not current store columns. Separate worktrees are
separate workspaces with separate indexes; they share only content-addressed extraction results. No
global registry, stable project catalog or workspace picker is needed for agents.

A future browser project picker can list known checkouts without merging databases. See
[multi-project viability](multi-project-viability.md). A moved checkout carries its index with it;
the store verifies relocation instead of refusing it.

Keep runtime session/run IDs, optional Herdr connection/pane associations, and future artifact IDs
and artifact revisions separate from source revision. Client-supplied labels do not authenticate
principals. Matching paths on different hosts, branches or equal numeric revisions are not identity.
Scope Herdr IDs to a connection lifetime and reconcile pane moves/restarts.

Every request, delayed result and cache entry retains its admitted evidence basis. Never route via
a mutable global “current project.” Future registry enumeration must itself be authorized.

## Proposed agent tools

**Phase 1 exposes exactly four MCP tools**, with portable ASCII underscore wire names:

| Tool | Phase 1 purpose |
| --- | --- |
| `baleyg_workspace_describe` | Describe the launch checkout, current basis, index state, coverage and limits; no indexing on request |
| `baleyg_find_symbols` | Bounded literal name/ID lookup in the current cached snapshot |
| `baleyg_inspect` | One declaration or its bounded depth-one outgoing calls; no arbitrary graph query |
| `baleyg_read_source` | Bounded cached source range, with hash and exact range |

All reads **except describe** carry `indexGeneration` and `expectedRevision`, validated in one
transaction-pinned snapshot under the revision churn rule. See the
[MCP contract](mcp-readonly-pilot-contract.md) for exact limits and schemas. No hidden navigation,
diagram, text-search, artifact, live-file, provider or runtime-control tools belong to this slice.
Tool allowlists and MCP read-only annotations are not a security boundary; the server simply has no
code path to those operations.

Later tools may add `baleyg_search_text`, `baleyg_navigate`, `baleyg_diagram_preview` and
`baleyg_artifact_create`, `baleyg_artifact_update`, `baleyg_artifact_publish`,
`baleyg_artifact_get`, `baleyg_artifact_list`. They are roadmap names, not Phase 1 capabilities.
Artifact reads and conceptual drafts will need their own version/authorization contract.

Return original IDs/ranges, evidence basis, certainty, warnings and explicit truncation. A capped
result is not “no results.” Uniform guards require implementation: `/api/symbols` and `/api/query`
do not currently require an expected revision. Agents never use the browser's owner HTTP routes.

### Structured tools complement grep

Use grep for discovery: strings, error messages, configuration, comments and unfamiliar code.
Use structure for narrowing: exact declarations, owner classes, members, hierarchy, source ranges,
measured calls and diagrams. Then read source to verify the explanation.

A separate next slice after the four-tool pilot adds a **bounded literal scan of indexed snapshot
text**. FTS is not a prerequisite or an assumed existing facility; benchmark a bounded scan first.
Regex is later work. Snapshot search does not cover all repository files or automatically execute `rg`. Mimir Hands or another harness can continue to supply approved
working-tree grep. A future typed `baleyg_search_live` can extend coverage with an approved root,
fixed safe execution/library interface, no shell command string, and time/byte/result caps.

Live hits must say `workingTree`, capture time and per-file hash; they are not an atomic indexed
snapshot. Do not attach live line numbers to cached symbols unless hashes match. Real-time native
refresh narrows the gap between cached and live text, but a changed file still needs a new
published revision before its cached evidence changes; never rebind silently.
Compiler/LSP resolution can later enrich these tools. It is not required to ship their first version.

## Diagrams: two deliberately different products

### 1. Evidence views

The agent selects a method/class and bounded view options. **Baleyg computes the diagram** from
its existing sequence/class projectors. The agent may choose focus, fold details, and add separately
labelled commentary. It cannot supply “measured” edges, erase underlying uncertainty/control flow,
or rewrite a call's resolution. Full underlying returned evidence remains available.

### 2. Agent-authored explanations and proposals

Examples: “how ingestion fits together,” a cross-service explanation, or a proposed refactor.
Accept a constrained versioned JSON graph: boxes, groups, directed labelled edges and evidence refs.
Use the local renderer and palette; do not accept executable diagram code, raw SVG/HTML/JavaScript,
CSS, external images/fonts/URLs, or unrestricted Mermaid from tool calls in the first version.

Always show an **Agent-authored explanation** or **Proposal** banner, the author/runtime session,
source basis, assumptions, and freshness. Individual claims distinguish evidence-linked statements,
assumptions, and proposals. A valid source citation proves anchoring, not entailment. Even an
all-cited explanation does not become a deterministic compiler/static-analysis result. Authored
edges never enter the indexed graph or class-relation tables.

### Artifact contract and lifecycle

A proposed artifact contains:
`schemaVersion, artifactId, projectId, ownerPrincipalId, authorSessionId, artifactRevision,
contentHash, kind, title, evidenceBases, body, assumptions, status, timestamps`.

Evidence refs identify project/store generation/index revision, file hash, measured byte range,
and optional symbol/call IDs. Validate before acceptance, with server-derived certainty labels.

- Create a draft with an idempotency key; server assigns identity and ownership.
- Update through bounded replacement with `expectedArtifactRevision` and content hash. Conflicts
  return 409, not last-write-wins. Explicit grants govern shared editing or forks.
- Publish exactly one immutable version. Editing it creates a new draft/version; do not mutate a
  previously published artifact. Replayed publication is idempotent.
- Emit a scoped metadata-only event. Baleyg shows an inbox item/badge; it does not force navigation,
  open browser tabs, change project, or steal focus. The user follows a token-free local deep link.
- “Publish” means local Baleyg publication only. Export/upload is a separate permission.

Persist artifacts outside the disposable index cache, with size/count/retention quotas. Suggested
MVP authored-graph bounds: 64 nodes, 128 edges, 256 KiB payload, bounded labels/groups/depth. Validate
IDs, dangling edges, enum values and group cycles; reject unknown fields. Existing deterministic
class/sequence bounds are not increased by this proposal.

Baleyg currently retains only the current index. Freeze the rendered DTO/graph and provenance with
an artifact if it must remain viewable after reindexing. Old source links become stale/unavailable
unless separate evidence retention is implemented. Do not claim historical source retention or
silently resolve an old symbol by name. Explicit restore/re-registration must rotate store generation;
manifest/hash validation also guards accidental revision reuse.

## Security and lifecycle rules

- Navigation, source reads, live search, artifact writes, publication, producer execution,
  shell/edit execution and runtime control remain separate capabilities. The read-only catalog has
  only the first two. A read-only tool may still disclose sensitive data.
- Remote access requires explicit provider/destination + project/source scope + output/budget policy.
  Keep local read-only operation available without any provider. Cancellation cannot unsend content.
  MCP itself cannot attest which downstream provider an arbitrary client uses. Configuring the
  server authorizes disclosure to that client; enforce known provider policy inside Mimir, and do
  not imply equal enforcement for every third-party client.
- Do not give agents the browser owner's daemon token. Keep tokens and ledgers outside every
  checkout, including `.baleyg/`; do not expose owner-authenticated HTTP or Herdr socket forwarding.
- A read-only MCP catalog does not sandbox an agent that also has shell/Python authority. The
  boundary is the OS user; a hostile-agent model needs a separate account or sandbox.
- Repository text, search matches, terminal output and diagram labels are untrusted data, not tool
  grants or instructions. Mimir's existing taint/approval gates must survive the provider extension.
- Request cancellation drops that response; session end closes stdin and ends the server. See the
  MCP contract for limits.
  Future writes must revoke pending capabilities at session cancellation. Do not retry ambiguous writes;
  use operation IDs and check recorded result. Cancellation does not roll back completed effects.
- Deduplicate Mimir replay by session plus journal sequence; journal replay is not effect replay.
  Herdr discovery resynchronization and Baleyg artifact-event replay are separate protocols.
- Keep source excerpts, credentials and sensitive titles out of general logs/OS notifications.
  Audit metadata must support attribution, revocation and retention without becoming a source dump.

## Delivery plan

### Phase 1 — per-checkout read-only MCP

Deliver the [local topology](local-topology.md): per-checkout `.baleyg/index.db`, the shared fact
cache, per-path delta publication, lock-elected writer with real-time native refresh, and
`baleyg mcp` over stdio exposing describe -> find_symbols -> inspect -> read_source. Test with a
synthetic MCP client across several concurrent worktrees, without Herdr, Mimir, a model or a
provider. The [MCP contract](mcp-readonly-pilot-contract.md) is the implementation/acceptance gate.
In the semantic-index program this is sequenced by
[#8](https://github.com/jasoncarreira/baleyg/issues/8): storage and delta publication in Stage 2,
watcher and writer election in Stage 8, the MCP server in Stage 9.

No registry, text scan, deterministic preview, artifacts, producer execution, live reads, ACP
provider extension or new semantic-resolution claims are in Phase 1.

### Next slice — bounded snapshot literal text scan

Add `baleyg_search_text` separately, with explicit byte/file/time/hit caps, snapshot guards and
truncation semantics. Scan cached text literally; do not assume FTS, regex, live grep or an index
schema migration. Measure this slice before expanding scope.

### Phase 2 — diagrams as versioned artifacts

Add draft/CAS/publish storage, evidence-view saves, simple authored graphs, local artifact inbox and
deep links. Demonstrate create -> publish -> user opens -> source navigation, then reindex and show
honest stale evidence. Establish limits before enabling agent writes.

### Phase 3 — first-class Mimir local tool bridge

Add the reviewed versioned provider profile using the existing proxy and runtime. Preserve Hands v1
compatibility and host distinction. Test schema drift, source/egress denial, revocation, cancellation
and duplicate updates. Only then run an explicitly authorized end-to-end remote-agent trial with a
bounded provider allowance. Do not repurpose/reset the old Baleyg ACP allowance.

### Phase 4 — optional Herdr adapter and multi-project UX

A read-only discovery prototype can run in parallel after identity contracts settle. Deliver explicit
workspace/worktree associations, agent/pane links and status. Add an explicit registry and project picker
while retaining per-project stores; neither was required for the single-workspace pilot. Later add
explicit focus/prompt actions for supported agents; no generic terminal control or automatic topology changes. Agent access never depends on the registry: each checkout's `baleyg mcp` works without it. Core tools/artifacts remain usable when
Herdr disconnects, restarts, or is not installed.

### Separate accepted UI slice — embedded terminals and ACP tabs

Implement real PTY-backed tabs for explicitly requested Baleyg-launched direct agents, with bounded
streaming, input/resize authorization and owned-session lifecycle. Add ACP conversation/tool/approval
tabs without treating ACP as a terminal transport. Gate Herdr terminal embedding on a verified native
attach/stream API; association/focus is not an embedded terminal. Keep the four-tool MCP pilot small
and independently shippable. Terminal launch/input is never added to the read-only catalog.

### Later, only if needed

Compiler/LSP enrichment; authorized live grep; cross-project search with a revision vector;
collaborative artifact editing; richer diagram types; and carefully designed reverse task dispatch.
Do not begin with a shared-database migration, a Baleyg agent manager, or automatic diagram-to-code.

## First useful user journey

**Phase 1:** From a direct terminal agent or an already supported MCP client, in any checkout or
worktree, describe the workspace, find a method, inspect its outgoing calls and read its cached
source. Edit the file and see the next revision reflect it within the refresh budget. Cite revision
and uncertainty. Do not run commands or publish anything. Mimir uses this same
catalog once its separately gated local provider extension is available.

**Later, after artifact and Mimir bridge slices:** From an existing Mimir or other coding-agent session:

> Explain this method and its collaborators. Use structured navigation plus text search.
> Publish a source-linked sequence view and a separate conceptual explanation in Baleyg.
> Do not edit code or run commands.

The agent reads only the admitted project, cites cached evidence, preserves unknowns, and saves
artifacts. Baleyg shows a notification. The user opens the diagram and navigates to source or a
method sequence. With Herdr attached, the artifact also links to the originating pane/session;
without Herdr, the same workflow works normally.

## Acceptance gates

Phase 1 must pass the concrete [MCP acceptance tests](mcp-readonly-pilot-contract.md#acceptance-tests).
The following additional gates apply as the later capabilities are delivered, not to enlarge Phase 1:

- Same file names, symbol IDs and revisions in two projects/worktrees cannot cross-contaminate.
- Project switching, logout, reconnect, stale revisions and index rebuilds reject late results.
- Many concurrent worktrees leave no orphaned processes and no unbounded storage growth.
- Live text is never presented as cached evidence without an exact hash match.
- Overloads/candidates remain labelled; diagrams do not promote them into runtime facts.
- Extra/changed Mimir tool schemas fail closed; existing Hands v1 sessions keep working.
- Owner spoofing, duplicate create/publish, concurrent edits, oversized graphs, unsafe labels/URLs,
  invalid refs and cancelled-session writes are rejected or safely idempotent.
- Diagram display/publication triggers no source read, index, provider request or focus change.
- Herdr offline/restart/pane movement does not destroy artifacts or redirect a tool's project.
- No second ACP observer connection and no terminal scraping dependency.
- Measure task success, wrong-target rate, source bytes/tool tokens, request count, latency and stale
  errors against grep-only tasks. Do not assume structured tools are universally better than grep.

## Evidence reviewed

- Baleyg: `src/auth.rs` token file handling, `src/main.rs` CLI/workspace resolution;
  `src/http.rs` owner-bearer guard, route table and handlers; `src/store.rs` guarded reads/publication/views;
  `src/model.rs` graph/view identities; `src/navigation.rs`; `src/class_diagram.rs`;
  `docs/member-navigation.md`, `docs/acp-answer-contract.md`, `docs/multi-project-viability.md`.
- Mimir checkout: `mimir/server.py:1509-1526`; `mimir/acp/agent.py:522-555,595-643,682-733,1230-1346`;
  `mimir/acp/proxy.py:136-153,798-846,1640-1678`; `mimir/acp/daemon.py:273-314`;
  `mimir/acp/journal.py:141-193`; `mimir/tools/client_provider.py`; `mimir/mcp_client.py:896-918`.
  Local implementation, not a remote deployment attestation; no Mimir source excerpts copied here.
- Herdr installed CLI/schema: version 0.8.2, protocol20/schema1. Discovery-only commands;
  schema observation does not promise future protocol compatibility or event replay.

No implementation, provider execution, tool installation, Herdr mutation, or deployment is part
of this plan. Existing Java semantic/JDT acquisition permission remains separate and unchanged.
