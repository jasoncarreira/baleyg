# Baleyg interface direction

Approved by the user from `~/Downloads/Baleyg UI.html`. The reference is a visual/interaction
model, not evidence of implemented compiler, agent, terminal or class-diagram capabilities.

## Planned unified workbench

Embedded terminal tabs are an accepted target, not implemented UI. Keep direct agents in real
PTY-backed tabs alongside source and diagrams; present ACP conversations/tools/approvals separately.
Optional Herdr attachment must use a verified supported transport and preserve external ownership.
See the [terminal contract](docs/terminal-workbench-contract.md). Do not add placeholder controls
that imply those capabilities already work.

## Product frame

- Compact explorer on the left; files expand to methods.
- Main tabbed workspace: Sequence, Classes, Libraries, Tools. No placeholder capability tabs.
- Classes uses the full canvas without the call inspector; class menus expose related classes and cached source.
- Selected-call inspector on the right. Move full source/provenance/control descriptions here.
- Explicit source reads open a collapsible source dock without losing the selected method.
- Source lines and class members expose revision-bound navigation menus. Use measured declarations
  and scoped type candidates, not global-name guesses. Source navigation is explicitly line-based.
- Workspace, revision, syntax/semantic state, indexing progress and failures remain discoverable.
- Desktop uses independently scrolling panes. Small screens expose explorer/inspector controls
  and contain the diagram scroll. Do not shrink a fixed desktop mockup into unreadable text.

## Visual tokens

Warm charcoal: page `#0E0D0C`, rail `#111010`, panel `#131211`, card `#161513`, node `#1A1816`.
Borders `#2E2A26` and `#3A3632`. Text `#EDE9E3`, secondary `#A79F95`, muted `#8C847B`.
Flame orange `#F0913C` is the single action/selection accent. Verify contrast on actual surfaces.
Use locally served Space Grotesk for interface text and JetBrains Mono for identifiers/source.
Body 13–14px; code labels 12–13px where practical. Keep focus, disabled and loading states visible.
The lowercase `baleyg` wordmark and flame-eye mark follow the reference.

## Diagram simplification, not semantic loss

Use short message names, compact control frames, quiet lifelines, and selection highlighting.
Analysis notes use short selectable rows; exact continuation guards stay visibly conditional with a rail.
Class views start with a focus and its indexed ancestors/descendants, collapsed members, and a chooser
for other related classes. Do not add unrelated peers merely because they share a base class.
Bundle returned class references by ordered pair; keep the original records in folded evidence.
Keep collapsed-chain entry arrows. A preview shows the first measured call, not a claim that
all calls share that receiver. Hide unused lanes and offer full detail through Show all.
Guard/alternative/exit evidence, original ranges and call IDs stay available. Summaries must
state when they represent multiple original elements. Never merge conditions by guessing
that identical labels mean identical semantics.

Resolved, syntactic, candidate and unavailable evidence remain distinguishable by labels/strokes,
not hue alone. No invented return values, activation durations, confidence scores, runtime traces,
Git branch names, provider activity or terminal sessions. Library calls remain terminal.

## Safety and continuity

No provider requests on connect, selection or source inspection. Existing explicit provider
confirmation/budgets and token opt-in behavior remain intact. Treat all content as text.
Keep source scopes separate and reset stale inspector callbacks across sessions/revisions.
Local assets stay within the existing CSP. Fonts include SIL OFL licenses under web/fonts/.
