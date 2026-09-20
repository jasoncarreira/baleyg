# Navigate from source and class members

Navigation uses the current cached snapshot. It does not execute the inspected repository, fetch
library source, call a provider, or resolve a type by globally matching its name.

## Source pane

- Right-click a source line, or use **Navigate line N**.
- With the source pane focused, Up/Down or Home/End chooses a line. Shift+F10 opens its menu.
  Shift-arrow selection and normal copy remain available.
- Menus can offer declarations on that line, the nearest enclosing method/class, recorded declared
  type candidates, and measured internal call targets when available.
- Choose a class to open its class diagram. Methods/functions offer separate **Go to source** and
  **Open sequence** choices. Source-only navigation highlights the measured declaration without
  changing the selected sequence.

This is **line-based navigation**, not compiler go-to-definition for any word under the pointer.
Unresolved calls are not globally name-matched. For Java bare-name and plain `this` calls, exact
cached syntax can offer methods declared in the same measured class. These are labelled
**same class candidate**, not compiler-resolved calls. All matching overloads remain explicit choices;
arbitrary receivers and inheritance lookup are not guessed. This first slice supports calls in
ordinary method declarations, not constructor bodies, initializers, lambdas, or local/anonymous classes.
External/library source stays a separate read-only surface; this navigation uses workspace snapshots.

## Class members

Expand **Members**. Method names still open their sequences directly. Click a type hint, right-click
its member row, or use the row's visible menu button to inspect navigation choices. A method menu
can include both its own sequence and its declared parameter/return types. Primitive, unmatched,
unsupported or omitted types do not acquire fictional targets.

Java field names and types have separate syntax ranges. Their association is checked against a
bounded cached Java syntax tree and existing recorded type evidence, not neighboring text guesses.

## Continuity and limits

Lookups are explicit. Opening source, selecting a line or expanding members does not fetch navigation
or another source file. Menus are invalidated by scope/session/revision changes or dismissal. Opening a
new diagram clears stale source actions. Existing authentication, source guards and provider allowances
remain unchanged.

`POST /api/navigation` accepts either `{expectedRevision, path, line}` or
`{expectedRevision, classId, memberName, startByte, endByte}`. Member names and byte ranges must match
one stored member exactly. Results contain original measured symbols with action/reason/certainty,
plus warnings and partial-index/limit status. Type links remain syntax candidates, not resolved types.
The response is bounded to 512KiB. Evidence records and Java cached-source parsing have separate bounds.

See [implementation contract](member-navigation-contract.md).

Validation: [public historical summary](member-navigation-validation.json). Private-workspace browser
and deployment evidence is omitted; no validation rerun is claimed. No reindex is required.

Follow-up: [same-class calls and source/sequence action validation](call-navigation-validation.json).
