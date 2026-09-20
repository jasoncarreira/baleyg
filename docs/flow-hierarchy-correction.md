# Python setup flow and automatic class hierarchy correction

Two user corrections after quiet defaults:

1. A Python factory with a plain name binding and deferred nested async function must not acquire
   setter/unpacking warnings or an invented continuation branch. Model only a narrow proven header:
   no defaults/decorators/generics, and annotations require bounded AST proof of a valid module
   future-annotations header. Bodies remain deferred. Other unknown definitions keep boundaries.
   Passing a callback is not invoking it. Argument construction runs before the outer invocation.
2. Class inheritance is automatic, associations are opt-in. Show indexed ancestors and descendants
   via extends/implements, not unrelated peers reached through an ancestor. Keep member lists folded.
   Unknown types remain terminal hints under explicit existing options; no semantic type claim.

Backend class request adds includeHierarchy (default false for compatibility); the UI sends true.
Response DTO stays unchanged. Hierarchy work precedes ordinary association neighborhoods, preserves
real connected mandatory paths and existing 24-node/64-edge/12-manual-root/4MiB response bounds.
No schema change, provider changes, inspected-project execution or reindex is required.

Workers own Python behavior/indexer diagnostic/tests, class projection/store/API tests, and class UI/tests
respectively. Root owns sequence renderer/test integration, docs, verification and deployment.
Keep private workspace screenshots outside the public repository; public fixtures are synthetic.

## Validation

The [public historical summary](flow-hierarchy-validation.json) retains Baleyg test totals and
implementation limits. Private-workspace browser examples, measurements and deployment details
are omitted. No tests were rerun while preparing that summary, and no synthetic example is
presented as an observed run.
