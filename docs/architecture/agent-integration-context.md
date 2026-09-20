# Proposed agent integration — system context

Proposal only. Direct terminal agents and optional ACP agents use the same MCP contract.
Herdr and Mimir are independent optional integrations. Terminal and ACP tabs in Baleyg are an
accepted target UI, not implemented functionality.

```mermaid
C4Context
  title Baleyg agent integration - proposed system context
  Person(user, "Developer", "Reviews code and agent explanations")
  System(baleyg, "Baleyg", "Code evidence and diagram workbench")
  System_Ext(agent, "Coding agent", "Existing local or remote harness")
  System_Ext(mimir, "Mimir", "Optional managed runtime and local Hands")
  System_Ext(herdr, "Herdr", "Optional terminal workspaces and agents")
  Rel(user, baleyg, "Inspects artifacts and source", "Local browser")
  Rel(user, agent, "Assigns coding or inspection tasks", "Workbench terminal or existing agent UI")
  Rel(baleyg, mimir, "Optionally acts as the chosen ACP client", "Negotiated ACP adapter")
  Rel(agent, baleyg, "Queries evidence and publishes artifacts", "Scoped MCP tools")
  Rel(user, mimir, "Configures managed sessions and permissions", "Existing Mimir UI")
  Rel(mimir, baleyg, "Routes approved local tools", "Proposed provider bridge")
  Rel(user, herdr, "Organizes workspaces and panes", "Herdr TUI")
  Rel(baleyg, herdr, "Discovers optional workspace and agent links", "Local socket adapter")
```

[Plan and boundaries](../agent-integration-plan.md).
