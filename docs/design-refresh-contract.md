# Workbench presentation contract

## Layout and assets

The approved reference is the user-provided `Baleyg UI.html`. The implementation uses its
warm charcoal/flame-orange palette and locally served Space Grotesk/JetBrains Mono fonts.
It does not copy fictional compiler, class-diagram, agent or terminal capabilities.

The explorer stays beside the sequence canvas on desktop. Libraries and Tools have their
own tabs. Selected calls and controls populate the inspector. Explicit source actions open
a source dock. Small screens use exclusive explorer/inspector drawers and scroll diagrams
inside the canvas. Selecting a method or opening source closes obstructing drawers.

Existing form/control IDs, authentication, token opt-in and provider confirmations remain.
All content is rendered as text. Font and script routes retain the existing Host/Origin/CSP
checks. Font licenses ship under `web/fonts/`.

## Controller interface

`window.BaleygShell` is presentation-only. It never fetches, indexes or calls a provider:

- `setConnected(bool)`, `updateWorkspace(status)`
- `showView("sequence" | "libraries" | "tools")`
- `showSource("workspace" | "library")`
- `selectStep(step, view, openSourceCallback)`
- `resetInspector()`, `reset()`

Application hooks are optional for legacy harnesses. Source callbacks retain the application's
session, revision and selected-method guards. Disconnect/revision/method changes clear stale
inspector callbacks. Pending/failed library reads remain visible in Libraries even if the dock
is closed. Successful reads reveal the selected cached source and highlighted range.

## Renderer interface

`render(container, view, readSource, expandedGroups = new Set(), options = {})`

`options.onSelect` inspects the original step. Without it, the legacy `readSource` callback
still runs. `options.showDetails` restores original verbose control labels. Group toggles
preserve keyboard focus, scroll position and selection. DTOs and measured evidence are immutable.

Control frames use clipped-corner `loop`, `alt` and `try` tabs, with bracketed guard/description
text beside the tab. Other structured containers use `block`. This is UML-style presentation,
not inferred UML semantics or an invented predicate. Clicking the tab or guard selects the
original step; Enter/Space do the same. Long full-detail labels wrap beside the tab, and the
canvas widens for deep input rather than letting guard text escape its frame.

Compact labels abbreviate known explanatory prose; they do not merge or remove guard,
alternate or exit nodes. Original labels/ranges/call IDs and nested evidence remain available
in titles, accessible labels, the inspector and Show all. Provenance uses labels/stroke patterns,
not color alone. No return values, activation durations, runtime identity or confidence are invented.

A collapsed flat chain previews only its first measured call. Its source action uses the original
full group range. Later receivers/return types may differ. Candidate library types remain unresolved
terminal boundaries; source hints are not type or dispatch resolution.
