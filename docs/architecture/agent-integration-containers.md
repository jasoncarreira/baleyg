# Proposed agent integration — containers

Proposal only. The Mimir Baleyg provider, scoped grants, MCP adapter, registry and artifact
service shown here must be implemented. Existing Hands v1 does not accept arbitrary tools.
This is the target architecture, not the four-tool pilot: registry, artifact storage, embedded
terminal streaming and ACP client sessions are separate slices. The pilot binds one existing daemon.

```mermaid
C4Container
  title Baleyg agent integration - proposed containers
  Person(user, "Developer", "Chooses what to inspect")
  Container_Ext(agent, "Agent client", "Existing harness", "Calls Baleyg tools directly")
  Container_Ext(mimirRuntime, "Mimir runtime", "Existing daemon and ACP", "Runs Mimir agent turns")
  Container_Ext(proxy, "Mimir local proxy", "Existing ACP proxy plus new provider", "Mediates local tool permissions")
  Container_Ext(herdr, "Herdr server", "Optional local terminal service", "Owns panes and lifecycle metadata")
  System_Boundary(baleyg, "Baleyg") {
    Container(ui, "Workbench", "Local browser UI", "Source, diagrams, terminal and ACP tabs")
    Container(mcp, "Tool adapter", "Local stdio MCP", "Exposes bounded project-scoped tools")
    Container(core, "Daemon", "Rust", "Validates grants and evidence; stores artifacts")
    ContainerDb(registry, "Project registry", "Local durable store", "Maps independent projects and integrations")
    ContainerDb(stores, "Per-project stores", "SQLite cache and durable data", "Keep evidence separate from artifacts")
  }
  Rel(user, ui, "Inspects code and operates agent tabs", "Local HTTP")
  Rel(core, agent, "Optionally launches an explicitly owned child", "Real PTY; existing harness")
  Rel(core, ui, "Streams owned or validated attached terminals", "Authenticated bounded channel")
  Rel(core, proxy, "Optionally acts as the chosen ACP client", "Negotiated ACP")
  Rel(agent, mcp, "Calls approved tools", "MCP stdio")
  Rel(mimirRuntime, proxy, "Requests local tools through existing channel", "ACP tool bridge")
  Rel(proxy, mcp, "Invokes versioned Baleyg capability", "Proposed provider adapter")
  Rel(mcp, core, "Requests evidence or artifact operations", "Scoped local API")
  Rel(ui, core, "Queries projects and artifacts", "Authenticated local API")
  Rel(core, ui, "Notifies of available artifact versions", "Proposed metadata event stream")
  Rel(core, registry, "Looks up authorized project bindings", "Local storage")
  Rel(core, stores, "Reads snapshots and persists artifacts", "SQLite")
  Rel(core, herdr, "Optionally discovers and reconciles metadata", "Versioned local socket adapter")
```

There is no new ACP observer connection beside an existing editor client. The local proxy and
Baleyg run on the machine holding the inspected workspace; a remote daemon's paths are different.
The Herdr adapter is a daemon component, not another agent harness or replacement pane manager.
Herdr metadata/snapshot APIs do not establish support for raw per-pane terminal embedding. Validate
a supported attach transport before adding that route. ACP protocol stdout is not terminal output.
See the [terminal contract](../terminal-workbench-contract.md).

[Plan and boundaries](../agent-integration-plan.md).
