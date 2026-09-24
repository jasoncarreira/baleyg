# Stable syntax-ID and durable-anchor vectors

This directory contains the sole normative descriptor vector set for the prospective v1 contract. It contains measured declaration descriptors, not source files, token spans, parser witnesses, or claims that a compiler accepts the illustrated declarations. The command below checks closed JSON shapes and declared raw digests only.

From the repository root, run:

```sh
node docs/semantic-evidence/id-test-vectors/check.mjs
```

The command uses Node built-ins, reads `stable-ids.json` relative to `check.mjs`, and needs no shell setup, package installation, environment assignment, or generated input. A successful run reports the case and digest counts. CI and factory verification do **not** invoke this command; reviewers must run it directly.

## Closed vector types

All object types are closed. Every listed field is required, arrays are non-null, and `T?` means either `T` or explicit `null`, never omission.

| Type | Exact fields |
|---|---|
| Root | `{version:1,cases:Case[]}` |
| Case | `{caseId:Text,language:Language,scenario:Text,descriptor:Descriptor,previousCaseId:Text?,continuity:GroupContinuity?,digests:Digest[],expected:Expected}` |
| Descriptor | `{sourceSet:Text,path:Path,language:Language,ancestors:Key[],declaration:Key,header:Header,revisionId:Text,siblingHeaders:Header[]}` |
| Key | `{kind:Kind,name:Text?,signature:Signature?,ordinal:UInt}` |
| Signature | `{parameterTypes:Text[],typeParameterCount:UInt,variadic:boolean}` |
| Header | `{kind:Kind,name:Text?,modifiers:Text[],typeParameters:Text[],parameters:Parameter[],resultType:Text?,bases:Text[]}` |
| Parameter | `{name:Text?,type:Text?,variadic:boolean}` |
| GroupContinuity | `{fromRevisionId:Text,toRevisionId:Text,state:unchanged\|changed\|unknown,evidence:Text?}` |
| Digest | `{label:Text,algorithm:sha256,domainHex:hex,inputHex:hex,sha256:Hash}` |
| Expected | `{stableId:SyntaxId,anchor:DurableAnchor,previousAnchorResult:AnchorResult?}` |
| DurableAnchor | `{syntaxId:SyntaxId,document:DocumentKey,capturedRevisionId:Text,headerHash:Hash,siblingGroupHash:Hash,siblingCount:UInt,identicalHeaderCount:UInt}` |
| DocumentKey | `{sourceSetId:Text,language:Language,path:Path}` |
| AnchorResult | `{status:attached\|orphaned,targetId:SyntaxId?,reason:none\|missing\|headerMismatch\|groupChanged\|unprovenContinuity}` |

`Language`, `Kind`, `Text`, `UInt`, `Hash`, `Path`, and `SyntaxId` have the exact meanings in `../contract-v1.md`. Hex is an even-length lowercase byte string. The only digest domains are the UTF-8 bytes of `baleyg.syntax.v1\0`, `baleyg.header.v1\0`, and `baleyg.sibling-group.v1\0`; their trailing NUL is present in `domainHex`.

A descriptor's `siblingHeaders` is the complete focused ordinal namespace in measured source order and includes the focused declaration. Each case has a `syntax` digest, one `header-N` digest for each byte-distinct header in that list, and a `sibling-group` digest. Labels are unique within the case. Repeated identical headers intentionally share one header digest row.

A null `previousCaseId` requires null `continuity` and null `previousAnchorResult`. A linked case names an earlier case and supplies both values. `unchanged` continuity across revisions has nonempty independent evidence; `unknown` has null evidence. Attached results repeat the captured prior syntax ID and use `none`; orphaned results have a null target and a non-`none` reason. These relations and all descriptor-to-expected relations require manual review; the checker does not evaluate them.

## Fixed allocation

Each language uses the same 16 slots, for exactly 64 cases and 16 cases per language.

| Slot | Scenario |
|---:|---|
| 1 | Unique named declaration baseline |
| 2 | Body-only edit of slot 1; stable ID and anchor survive |
| 3 | Rename from slot 1; ID changes and the old anchor is missing |
| 4 | Path move from slot 1; ID changes and the old document anchor is missing |
| 5–6 | Same-name declarations with distinct projected headers |
| 7 | Same-key, different-header insertion before slot 1; the old ordinal now names the inserted header |
| 8 | Same-key, different-header insertion after slot 1; the original ID/header survive |
| 9 | Two-member identical-header group baseline |
| 10 | Body edit of slot 9 with independently proven unchanged group membership/order |
| 11 | Third identical-header member changes slot 9's group count |
| 12 | Nested declaration with an outermost-to-parent ancestor chain |
| 13 | Anonymous declaration with a null name |
| 14 | Another non-ASCII measured name |
| 15 | NFC name containing U+00E9 |
| 16 | NFD name containing U+0065 U+0301, linked to slot 15 |

Java slots 5 and 6 use distinct measured parameter-type signatures, so each overload signature defines its own singleton ordinal group. Only Java methods and constructors have a non-null `Signature`. Rust, Python, and JavaScript slots 5 and 6 use ordinary ordinal siblings with null signatures. Repeated callable descriptors elsewhere are measured syntax examples before semantic validation. They do not claim language-valid overload behavior. In particular, the Rust cases can represent configuration-selected syntax, and no descriptor proves corpus or compiler correctness.

## Independent hand derivation

Expected values were derived from the ratified text, separately from `check.mjs`:

1. Copy only `{sourceSet,path,language,ancestors,declaration}` from the descriptor. Serialize with the contract's canonical JSON rules: ASCII-sorted object keys, specified array order, required nulls, UTF-8 without normalization, and no whitespace or newline.
2. Record those literal bytes as `inputHex`. Prepend the literal syntax `domainHex`, calculate SHA-256, record its lowercase hex, and spell the stable ID as `sid:v1:<digest>`.
3. Canonically serialize every distinct projected `Header` without adding source text or omitted syntax. Hash each with the header domain.
4. Put the resulting header hashes in the descriptor's sibling order in canonical `{"headers":[...]}`. Hash that input with the sibling-group domain. Count all members and occurrences of the focused header hash.
5. Copy the literal stable ID and header/group values into the durable anchor. The revision is captured by the anchor but is excluded from stable-ID, header, and group inputs.
6. For linked cases, audit the prior anchor in contract order: old ID presence, header equality, then duplicate-group hash/counts and independent continuity when required. Record the literal result; never search by name or header for a replacement.

The byte review must decode every `inputHex` and compare it with its stated descriptor. In particular, slots 15 and 16 retain different UTF-8 byte sequences; lookup normalization never enters these bytes. Body-only slots 2 and 10 change revision IDs but keep their identity/header inputs. Slot 7 deliberately leaves the prior ordinal-zero ID on the inserted member, whose different header causes `headerMismatch`. Slot 8 keeps the old ordinal-zero ID and header. Slot 10 assumes independently proven unchanged membership/order. Slot 11 states changed continuity and has unequal group hash/counts, so it yields `groupChanged`. Slot 16 cannot find the prior NFC ID and yields `missing`.

## Deliberately limited checker

`check.mjs` checks JSON parsing, exact required/unknown fields, documented primitive and enum shapes, version 1, total and per-language counts, unique case IDs, unique per-case digest labels, lowercase byte hex, allowed domains, and raw SHA-256 of `domainBytes || inputBytes`.

It does **not** canonicalize descriptors; compute or compare stable IDs; derive ordinals, headers, groups, or counts; normalize names; evaluate continuity or anchor results; or model semantic records, joins, coverage, freshness, dispatch, traversal, cursors, or frontiers. A well-shaped but incorrect `expected.stableId` can therefore pass. A matching raw digest also proves only that the declared bytes hash as stated, not that those bytes encode the specified descriptor. This boundary is intentional so that a second implementation cannot bless the author's semantic error.

These vectors document prospective measured syntax identity and conservative anchor outcomes. They do not establish production implementation, compiler validity, semantic corpus quality, an execution trace, or a runtime-complete call graph.

## Final review procedure

1. Run the exact command above from the repository root and observe 64 cases and the reported digest count.
2. On disposable copies only, confirm failures for malformed JSON, an unknown field, a duplicate case ID, invalid hex, an unknown domain, and a changed digest. Do not commit those copies or add fixtures.
3. Independently inspect all 64 descriptors, canonical byte strings, literal digests and IDs, anchor fields, links, continuity assumptions, and prior results against the final contract. Green raw-digest output is insufficient.
4. Confirm the 16-slot allocation for each language, Java-only signature rule, sibling membership/order, NFC/NFD code points, and the absence of occurrence, reference, graph, source, or token suites.
5. Have the orchestrator-assigned independent test verifier perform the integrated contract/decision/README/vector/checker cross-check and record findings in run artifacts, not another repository file.
