# Class diagrams and related classes

Class diagrams use the indexed Java/Python snapshot. They do not load or execute the inspected
application, run package tools, fetch dependencies, or send source to a provider.

## Use the view

- Right-click an indexed `.java` or `.py` file and choose **Show class diagram**.
- Right-click a method and choose **Show enclosing class diagram**, or use its **Class diagram** button.
- Alternatively open **Classes**, search a class or qualified name, and choose a result.
- The view starts with the focus and its indexed inheritance hierarchy: ancestors, subclasses and
  implemented interfaces. Traversal does not expose unrelated siblings through a shared ancestor.
- Right-click a class and choose **Show related classes**, select other type references, then
  **Add selected classes**. A visible hierarchy class does not need to be manually added first.
  The chooser uses the current bounded result, not an exhaustive workspace graph.
- **Members** reveals fields, methods, qualified name and path. Click a method name to open its sequence.
  Click a type hint, right-click a member, or use its `…` button to navigate recorded type candidates
  and the member's method. See [source and member navigation](member-navigation.md).
- **Show all returned classes** reveals the bounded API result; **Show hierarchy and selected classes**
  restores inheritance plus the chosen associations. **Change class** reveals search results. **Focus this class** starts over.
- Use **Read class source** for the cached declaration. Selection, member disclosure, menus and
  related-class exploration do not read source.

The visible `…` menu works on touch. A focused class supports Shift+F10 / the Context Menu key.
Menus support arrow keys, Home/End and Escape. Diagrams scroll inside the canvas on narrow screens.
Switching between Classes and Sequence retains the current class view until the workspace/index changes.

## What the links mean

Class cards show direct declared fields and methods. Edges come from explicit inheritance and
field/parameter/return type syntax. Both incoming and outgoing references can reveal related classes.
Dashed edges are **scoped syntax candidates**, not compiler-resolved types, runtime object relationships,
method dispatch, ownership/composition or multiplicity. No generated methods are invented.

Matching uses declaration, package/import and lexical context. Ambiguous or unmatched names stay
terminal hints; **View options → Include unmatched type hints** reveals them. They cannot be expanded as known classes.
Python module names follow workspace-relative source paths, without guessing runtime import roots.
Wildcards, inferred local types, dynamic factories, evaluated aliases, function-body/conditional/anonymous
classes and third-party catalogs are not resolved. Rust and JavaScript sequences are unchanged; this
class projection currently supports Java and Python only.

Repeated same-kind references between the same endpoints use a deterministic representative edge in
the diagram. Its ID and source range are real. The full reference catalog remains stored. The relationship
evidence disclosure identifies every returned reference. The renderer bundles these references into
one arrow per ordered class pair, labelled with its declaration kinds; opposite directions stay distinct.
Original returned IDs, ranges and candidate evidence remain in the folded disclosure. Read the class
source for omitted repetitions and declaration detail.

## Bounds and continuity

Views show at most 24 nodes / 64 edges and accept at most 12 additional connected expansion roots.
Fields/methods, source traversal, registry text, detail text, references and response sizes are bounded.
Partial results have visible notices. Detail truncation does not suppress otherwise valid candidate links;
incomplete declaration discovery disables unique matching rather than inventing certainty.

Class projections are stored atomically with the source snapshot. Schema 3 adds projection tables and
preserves cached source and durable views/notes. Old snapshots show **Index workspace** guidance until
explicitly reindexed; class requests never scan live source. Workspace/session/revision guards invalidate
old menus, callbacks, diagrams and pending source reads. Provider packets and their authorization budgets
are unchanged. Class layout itself is not saved as a durable view in this slice.

## API

- `GET /api/classes?revision=N&q=NAME&path=RELATIVE_PATH&offset=0&limit=100`
  lists indexed declarations; omit `path` for workspace search.
- `POST /api/class-diagram` with `{seed, expectedRevision, expanded: [], includeUnmatched: false}`
  returns a bounded diagram. `seed` may be a class or a method with a lexical enclosing class.
  Optional `includeHierarchy: true` prioritizes bounded transitive ancestors/descendants; the UI sends
  this flag. Omission retains the original neighborhood API behavior. It does not change stored certainty.

Both endpoints require existing bearer/Host/Origin guards. Responses carry their snapshot revision,
warnings, truncation and `requireIndex` state. Unsupported or invalid requests fail explicitly.
