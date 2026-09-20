# Navigation decisions from IntelliJ IDEA

Reference read: [Source code hierarchy](https://www.jetbrains.com/help/idea/viewing-structure-and-hierarchy-of-the-source-code.html)
(IntelliJ IDEA 2026.2 documentation). The user supplied it as a product reference.

Adopt the interaction principles, not every feature at once:

- Keep a selected root visible and expand individual branches deliberately.
- Distinguish incoming callers from outgoing callees. Current Baleyg traversal is outgoing
  only; label it accurately rather than implying both directions are implemented.
- Keep method names prominent, path/type metadata secondary, and source navigation immediate.
- Preserve exact call-site evidence even when a future display groups repeated method calls.
- Keep question-focused selection separate from deterministic hierarchy exploration.
- Depth 1 is the default. Evidence retrieval may go deeper without expanding the display.

The JetBrains page also describes Project/Test/All/This-class scopes, alphabetical sorting,
pinned hierarchy tabs, type hierarchies, and method override hierarchies. Those are distinct
capabilities. Do not fake them with filename guesses, unresolved targets or call edges.
They can follow after the core question/view loop is validated.

The supplied screenshot is a private reference, not a benchmark fixture or provider input.
Its code names must not be embedded in the application or sent to selection providers.
