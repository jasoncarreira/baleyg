# D0004 small-Limits catalog oracles

These checked-in JSON files are **expected bytes**, not snapshots emitted by
`Catalog::compose`. The 16 equality/+1 cases were authored from the D0004
source/registry/class/record/text/input limit rules. They include every class
symbol, class-level truncation bit, member, terminal relation, warning, and
catalog truncation bit. `tests/classes.rs` compares raw `serde_json::to_vec`
bytes, not an extracted name-only summary or normalized JSON value.

The fixed synthetic symbol skeleton and field range/provenance are copied from
the frozen pre-edit `below-cap/java_members_inheritance_generics_and_nested_classes_are_source_bound-0.json`
fixture. For each A/B synthetic file, the authored symbol ID is `class-A.java`
/ `class-B.java`; path/name/qualifiedName are A.java/A or B.java/B. Field
names and file paths are authored `first`/A.java and `second`/B.java. The
zero-cost terminal reference ID is `ref-A.java` or `ref-B.java`; owner is its
class ID, typeName is `Target`, kind `field`, empty candidateIds and null
target. No native authority or inferred target is fabricated by the composer.

| Cut | Admitted classes | Fields | Admitted refs | Per-class truncated |
| --- | --- | --- | --- | --- |
| source-equal, registry-equal, classes-equal, input-equal | A | none | A | none |
| source-over, registry-over, input-over | none | none | none | none |
| classes-over, moving-after | A | none | A | none |
| moving-before | A, B | none | A, B | none |
| records-equal, output-equal | A, B | first, second | A, B | none |
| records-over, output-over | A, B | first | A, B | B |
| records-inside, output-inside | A, B | first | A | A, B |

Warnings and whole-catalog truncation are literal per-file JSON values. The
`moving-later-*` files are separate B.java class/relation projections: B is
present before A.java grows from one to two source bytes and absent after the
same source cap, even though the B.java F input is unchanged. This is a
composition-cut test, not a claim about persisted deltas or old revisions.
