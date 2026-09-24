# Baleyg product context

Audience: developers trying to understand a repository without reading every helper.
Primary action: expand a file in the tree, choose a method beneath it, and inspect its static sequence diagram.
Keep call hierarchy and scoped questions available as secondary tools.
Default detail: one level. Deeper evidence must not automatically become displayed detail.

Navigation reference: IntelliJ IDEA call hierarchy. Keep an anchored root, compact method
rows, source links, and deliberate per-branch expansion. Distinguish callers from callees;
do not label an outgoing-only implementation as bidirectional. Question-focused selection
is a separate mode, not a substitute for deterministic navigation.

The approved visual direction is the user-provided `~/Downloads/Baleyg UI.html` reference:
a diagram-first charcoal/flame-orange application shell, compact explorer, large canvas,
and selected-call inspector. See DESIGN.md. Use local fonts, readable spacing, secondary
path metadata, explicit loading/error/stale states, and collapsed detailed evidence.
Source and annotations are untrusted text. No external runtime fonts, scripts, telemetry,
or automatic inference. Token persistence is explicit opt-in on a trusted browser profile;
Disconnect, unchecking Remember, or an authentication failure clears the stored token. A call list is not a sequence diagram. Sequence views must preserve
measured control structure and mark unknown behavior; never present them as observed runtime traces.
Language-specific parsing feeds a shared behavior model; navigation and rendering stay language-neutral.

Reference: https://www.jetbrains.com/help/idea/viewing-structure-and-hierarchy-of-the-source-code.html


## Dependency browsing direction

Discover and index included libraries automatically through ecosystem adapters. The shared
catalog stores package identity, declarations, class/type ownership, signatures and source
references—not third-party execution graphs. Sequence diagrams stop at external library calls.
Source candidates and syntax hints must not be presented as confirmed type/dispatch resolution.
Rust is the first implementation target; contracts must not assume Cargo or one host OS.
Manual external source browsing is a fallback, not completion of automatic dependency support.


## Declared class structure

Java/Python class diagrams complement the method sequence workflow. Enter from a file/method
context menu or Classes search; expand one-hop related classes deliberately from node menus.
Class cards show declared members, and method links return to sequences. Dashed relationships
are scoped syntax candidates, never compiler/runtime proof. Unmatched type hints stay terminal.
Keep source reads explicit, display limits visible, menus keyboard/touch accessible and all state
bound to the indexed workspace revision. See [class diagrams](docs/class-diagrams.md).

## Planned agent workflow

Keep agent terminals, source and diagrams in one workbench. Direct terminal agents and optional
ACP agents use the same portable MCP tools; neither Mimir nor Herdr is mandatory. Real PTY tabs
and ACP conversation/tool/approval tabs are separate surfaces. These integrations are not shipped.
Every checkout and worktree gets its own index, kept current as agents edit, and agents reach it
through a stdio MCP server launched in that checkout.
See the [integration plan](docs/agent-integration-plan.md) and
[terminal contract](docs/terminal-workbench-contract.md).
