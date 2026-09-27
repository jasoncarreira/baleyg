# Publication-rejoin v1 documentation vectors

**Prospective docs-only checks, not an executable decoder/corpus.** [Contract](contract-v1.md#publication-time-rejoin-of-captured-scip-evidence-11a) and [decision](decisions/0002-publication-rejoin.md) control these examples. Raw producer capture is separate from an independently measured native anchor and language-specific semantic proof. These exact digest rows were calculated independently with SHA-256 of the shown #22 one-line UTF-8 canonical bytes and checked against `tools/semantic-contract/json.mjs::canonicalBytes`; the binary examples were decoded with the local pinned `scip = 0.10.0` Rust crate, `protobuf = 3`.

## Complete two-document captured input

The admitted source set is `app` (root `root-app`, language `javascript`, no dependencies), captured revision `r1`, selected semantic producer `scip-test` version `1.0.0`, decoder `scip-0.10.0/CapturedScipFactV1/1`, declared producer position encoding `utf8`. Native r2 is an independently measured revision (`r2 != r1`), e.g. native extractor/config capture changed but A and B source bytes unchanged. For a changed-B subcase, use `B2="function g(){return 1;}\n"` (hex `66756e6374696f6e206728297b72657475726e20313b7d0a`, byte length 24, SHA-256 `47ae88cf464c589aaa9adf0441a6d7c63245b417a3e78a975aefab2f4af1c53f`); A bytes stay identical. The r2 source manifest and revision ID must change; the B.g stable descriptor/ID below remains the same when uniquely remeasured. The complete two-document authenticated source manifest is the following #22 canonical array, in language/path order; no other documents are in this admitted fixture:

```json
[{"contentHash":"8026dced2c17cbbfb8563d8a7f07e250a141be61cd497e6ba88caeb52a6de8f2","document":{"language":"javascript","path":"src/A.js","sourceSetId":"app"}},{"contentHash":"ae7703c040471d7d8cf8032c1d4acf32d03fb3ea089cc341f72e91cfcb3f24b5","document":{"language":"javascript","path":"src/B.js","sourceSetId":"app"}}]
```

`sourceManifestHash` (empty-prefix SHA-256 of the exact array above) = `5578e3d23b2cba76685271de456413c9b207379613d72f1b55b25d84a487cf95`. Source bytes and hashes, with **no** newline added outside the shown hex:

| Document | UTF-8 source | Source hex | SHA-256 | byte length |
|---|---|---|---|---:|
| A | `function f(){g();}\n` | `66756e6374696f6e206628297b6728293b7d0a` | `8026dced2c17cbbfb8563d8a7f07e250a141be61cd497e6ba88caeb52a6de8f2` | 19 |
| B | `function g(){}\n` | `66756e6374696f6e206728297b7d0a` | `ae7703c040471d7d8cf8032c1d4acf32d03fb3ea089cc341f72e91cfcb3f24b5` | 15 |

Additional authenticated captured manifest components (the component hashes are empty-domain SHA-256 of the exact bytes): producer executable bytes `736369702d746573742d62696e` => `f615261a2fb15f6d51cac723cdbe8c3fe3fb495090f43b3cffc226bb3be278ed`; toolchain bytes `746f6f6c2d7631` => `dc0d766981c85d12f41dec89bf8cddb4a1629265e25265ec02189226b1deebdd`; config bytes `636f6e6669672d7631` => `e3155b20e134632816c8611c4e9ee5cbd0e00689f7c4c955ee9f896580d02fdb`; dependency bytes `646570732d7631` => `48ce14bab85b92d43dca7de13bff5155ddae7c9017930ecbf0f10c20cd028529`. The captured `SemanticBasis` binds these hashes, `producerId=scip-test`, `producerVersion=1.0.0`, `language=javascript`, `sourceSetId=app`, `revisionId=r1`, and the artifact and manifest hashes below; `lookupDependencies=[]` for this fixture. The authenticated source admission binds both `DocumentKey`s to `root-app` and its exact source bytes; the externally authenticated manifest, **not** protobuf metadata guessed by a decoder, attests these components. Native source revision `r1` itself does not include semantic artifacts. This fixture does not invent an on-disk importer manifest wire schema.

SCIP protobuf `Index` wire hex below has A with three occurrences (legacy definition and typed single-line/multiline uses), one `SymbolInformation` with one relationship (both `is_reference` and `is_implementation` true), and B with no facts. `Index.metadata` and `Document.text` are absent; index/SCIP source binding comes from the authenticated manifest above. It is **116 bytes** and was decoded with `scip 0.10.0` as two documents, A `position_encoding=1`, legacy `[0,9,10]`, typed single `(0,13,14)`, typed multi `(0,13,0,14)`, symbol kind `17` (Function), flags `(true,true,false,false)`, B `position_encoding=1`.

```text
12580a087372632f412e6a73220a6a617661736372697074120b0a0300090a120241231801120c42060800100d180e12024223120e4a080800100d1800200e120242231a130a02412322080a024223100118012811320166300112180a087372632f422e6a73220a6a6176617363726970743001
```

`artifactHash = SHA-256(binary)` = `8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b`. The source manifest, binary, producer bytes/profile and pinned decoder must all authenticate **before** facts are selected or joined. An omitted relationship flag is its actual decoded protobuf default `false`, not an optional null. Present and decoded-default numeric values are represented equally in v1 canonical envelopes.

## Exact producer envelopes, canonical bytes and full hashes

For each row, the JSON line is the **complete** #22 `canonicalBytes(E)`; no newline belongs to the digest. Compute `D(E)=SHA-256(UTF8("baleyg.semantic-fact.v1\0")||canonicalBytes(E))`. Compute captured proof ID with `SHA-256(UTF8("baleyg.capture-proof.v1\0")||canonicalBytes(C))`, where `C` is the exact seven-key tuple below. The first five rows are the complete deduplicated envelope inventory of the authenticated 116-byte binary (three occurrences, one symbolInformation, one separate relationship). The final index row belongs only to the separate 129-byte index-level binary in the next section. These rows are not destination records and do not assert a call or relationship classification.
### call-occurrence
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":null,"range":{"encoding":"singleLine","endCharacter":14,"line":0,"startCharacter":13},"symbol":"B#","symbolRoles":0,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}
```
`factDigest = 26a1761afd7123e6a3e89587345a14a21dfbb3ddbb7e314b937b703f9168aab0`.
`canonicalBytes(C)`:

```json
{"artifactHash":"8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b","factDigest":"26a1761afd7123e6a3e89587345a14a21dfbb3ddbb7e314b937b703f9168aab0","language":"javascript","producerId":"scip-test","relativePath":"src/A.js","sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = 19814b705caecee5379f3a30371346536372847618ae9ffba627f13988888fe0`.
### reference-occurrence
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":null,"range":{"encoding":"multiLine","endCharacter":14,"endLine":0,"startCharacter":13,"startLine":0},"symbol":"B#","symbolRoles":0,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}
```
`factDigest = e11b51d2a31f320d727896a4badbeba4e68df1c206c49db8179d15ba4e489b3c`.
`canonicalBytes(C)`:

```json
{"artifactHash":"8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b","factDigest":"e11b51d2a31f320d727896a4badbeba4e68df1c206c49db8179d15ba4e489b3c","language":"javascript","producerId":"scip-test","relativePath":"src/A.js","sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = d1ca0cc69aed1f6d4b6e009f84d992bf415f4f4f41dc75c4ea7aa6863dfbb9ed`.
### definition-occurrence
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":null,"range":{"encoding":"legacy","values":[0,9,10]},"symbol":"A#","symbolRoles":1,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}
```
`factDigest = 2fc0653df4bc9ae7fbee043ce308352135c19feea776adfe14cf151c7831c18b`.
`canonicalBytes(C)`:

```json
{"artifactHash":"8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b","factDigest":"2fc0653df4bc9ae7fbee043ce308352135c19feea776adfe14cf151c7831c18b","language":"javascript","producerId":"scip-test","relativePath":"src/A.js","sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = 197f20e205f6891df885ef0e022f4e1ecb76074b4b12532cc51b6d1fb719ea03`.
### symbol-information
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"displayName":"f","enclosingSymbol":"","kind":17,"symbol":"A#"},"recordKind":"symbolInformation","relativePath":"src/A.js","scope":"document"}
```
`factDigest = b1de1d3d59e8e4785b01125b43b689032461749bfe6a94d2104a553ee73aef41`.
`canonicalBytes(C)`:

```json
{"artifactHash":"8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b","factDigest":"b1de1d3d59e8e4785b01125b43b689032461749bfe6a94d2104a553ee73aef41","language":"javascript","producerId":"scip-test","relativePath":"src/A.js","sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = 0167792d9317f3169d5cc5c38f28eee62515f2eefab3eded91031073e77b7082`.
### relationship
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"isDefinition":false,"isImplementation":true,"isReference":true,"isTypeDefinition":false,"sourceSymbol":"A#","targetSymbol":"B#"},"recordKind":"relationship","relativePath":"src/A.js","scope":"document"}
```
`factDigest = fd84734141891f294fd4c10668dd2c65118b3c9fd4da65e5af32a0834f08ac9b`.
`canonicalBytes(C)`:

```json
{"artifactHash":"8e2f72f58e2d6a1cc0f1177522693069a57c36d67b8ca405965de42b3d7afc5b","factDigest":"fd84734141891f294fd4c10668dd2c65118b3c9fd4da65e5af32a0834f08ac9b","language":"javascript","producerId":"scip-test","relativePath":"src/A.js","sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = 75c9e5a53eafd5bcc48154d3422513a0c62517e70b027d1781062e8ab4ec56d4`.
### index-symbol
`canonicalBytes(E)`:

```json
{"formatVersion":1,"positionEncoding":null,"raw":{"displayName":"","enclosingSymbol":"","kind":0,"symbol":"external#"},"recordKind":"symbolInformation","relativePath":null,"scope":"index"}
```
`factDigest = 0c6d4766288e61f80d873ff458abbf5947ea653b6725f256eee66f181572814f`.
`canonicalBytes(C)`:

```json
{"artifactHash":"19d1e39011338e4cae9f73289c851db75beebd69f221e5295c57540eb9afd23f","factDigest":"0c6d4766288e61f80d873ff458abbf5947ea653b6725f256eee66f181572814f","language":null,"producerId":"scip-test","relativePath":null,"sourceRevision":"r1","sourceSetId":"app"}
```
`capturedProofId = fca7a765c910b4782690aa62bd1a3b8afff4613b30c882c3b9be279b6f43e82f`.
## Index-scope and range-branch controls

An otherwise identical Index with `external_symbols=[{symbol:"external#"}]` appends exactly `1a0b0a0965787465726e616c23` to the 116-byte binary. The complete 129-byte Index wire and SHA-256 are:

```text
12580a087372632f412e6a73220a6a617661736372697074120b0a0300090a120241231801120c42060800100d180e12024223120e4a080800100d1800200e120242231a130a02412322080a024223100118012811320166300112180a087372632f422e6a73220a6a61766173637269707430011a0b0a0965787465726e616c23
```

`19d1e39011338e4cae9f73289c851db75beebd69f221e5295c57540eb9afd23f`. It adds the `index-symbol` envelope/inventory claim above (not a document `Provenance` row), with `scope:"index"`, `language:null,relativePath:null` in its captured claim tuple and `relativePath:null,positionEncoding:null` in its envelope. **Positive:** authenticated capture retains the index symbol even though it has no document. **Negative:** trying to derive a local A declaration/TypeRelationship, A coverage, or an r2 local semantic provenance from it fails; neither attaching it to the first Document nor inferring a language is allowed. Nested index-level relationships likewise are captured raw but cannot make document-local facts.

The corrected **44-byte** legacy-only Index wire is:

```text
122a0a087372632f412e6a73220a6a61766173637269707412100a03000a0e12076c6f63616c203018013001
```

Its empty-prefix artifact hash is `1f62fd7188854b19ebad46d5d1048193263fcd1b37cfef7fb7d1d6c35d417409`. `scip 0.10.0` decodes one JavaScript `src/A.js` Document, encoding 1 and packed legacy `[0,10,14]` (`local 0`, role 1). For this standalone demonstration the authenticated source is `0123456789call\n` (hex `3031323334353637383963616c6c0a`; hash `09e4f3066833d4d73ff18e98cbb31d0e5674d3bf7619f789b69e916eab49dc9a`). Its canonical envelope and digest after the **new** scope field are:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":null,"range":{"encoding":"legacy","values":[0,10,14]},"symbol":"local 0","symbolRoles":1,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}
```

`D(E) = 7a7d3aaa3aeb5b837ba57eae6e89b0e740db0bc284f1e8453c7160a6beb8353b`. This is not the earlier draft digest without `scope`; never mix those domains/shapes or assume `local 0` is a call.

The following two additional admitted branch inputs are carried by a separate complete one-document Index. Its admitted document `src/A.js` has exactly the same A source bytes and hash from above; its entire manifest is `[{"contentHash":"8026dced2c17cbbfb8563d8a7f07e250a141be61cd497e6ba88caeb52a6de8f2","document":{"language":"javascript","path":"src/A.js","sourceSetId":"app"}}]` with empty-prefix manifest SHA-256 `52b4124248408e1a37e7ff17b786ff57a998b54b3ebfe252e478330d203506ee`. Its exact 70-byte `scip 0.10.0`-decoded binary (legacy four with typed single-line enclosing; typed single-line primary with typed multi-line enclosing) is:

```text
12440a087372632f412e6a73220a6a61766173637269707412120a04000d000e1202422352060800100c1810121642060800100d180e120242235a080800100c180020103001
```

`artifactHash = 2b3c07f5e087eecb1b0ccb18627b0b66c60eb2bbde5a083e918323d4034e6d2b`. The other authenticated profile/component bytes are unchanged, but this is **not** the 116-byte artifact and proof tuples must bind this artifact hash. `enclosingRange` is either exactly one validated branch or null. Each typed zero is a present canonical JSON numeric field even where protobuf omitted the zero on the wire.

- `legacy-four` canonical: `{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":{"encoding":"singleLine","endCharacter":16,"line":0,"startCharacter":12},"range":{"encoding":"legacy","values":[0,13,0,14]},"symbol":"B#","symbolRoles":0,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}`; `D(E)=50178bf071519261d395005561dab614ac25870048220640ea5c16e5ec6b0a25`.

- `enclosing-multi` canonical: `{"formatVersion":1,"positionEncoding":1,"raw":{"enclosingRange":{"encoding":"multiLine","endCharacter":16,"endLine":0,"startCharacter":12,"startLine":0},"range":{"encoding":"singleLine","endCharacter":14,"line":0,"startCharacter":13},"symbol":"B#","symbolRoles":0,"syntaxKind":0},"recordKind":"occurrence","relativePath":"src/A.js","scope":"document"}`; `D(E)=eb3eef61efe5f3aa935b81ba18e79094e4304a36ebe89174e422ae8b318aa7ce`.

Reject primary empty/missing range, legacy length 2 or 5, negative/out-of-int32 coordinate, mixed legacy and typed (including identical-looking values), two competing typed oneof tags even if a protobuf decoder silently overwrites one, and mixed legacy/typed enclosing range. Detect competing oneof tags from authenticated wire before trusting the parsed last value. Reject omitted/0/unknown `Document.position_encoding`; never silently call zero UTF-16. UTF-16 coordinate splitting a surrogate pair, UTF-8 byte coordinate splitting a scalar, UTF-32 coordinate past line end, reversed span, scalar boundary outside source or a CRLF coordinate that includes the `\r` as a phantom part of the next line fails before join. Example source `"😀\r\nx"`: UTF-16 line 0 offset 1 splits the surrogate; UTF-8 line 0 offset 1 splits the four-byte scalar; line 1 starts only after both CRLF bytes and `x` is at line 1 column 0. A typed range with `{line:0,startCharacter:0,endCharacter:0}` is zero-length and cannot support an ordinary nonempty reference anchor. A valid converted primary span with no compatible measured destination candidate is a join failure, not an invalid range.

Duplicate `Document.relative_path=src/A.js` in one Index rejects the entire captured artifact before extraction even when one entry declares another language (it must not overwrite/merge). Change only presentation-only documentation or diagnostic: envelope digest stays the same, binary artifact hash and proof context change; an old artifact manifest hash now fails. Unknown protobuf fields anywhere, including omitted nested diagnostics/signatures/metadata; unknown enum numeric values/role bits (e.g. role 128); nonmatching `Document.text`; fake manifest path/source/producer bytes; or a new semantic extension under an omitted message rejects or requires an explicit envelope/domain version change. No unknown field can vanish in a protobuf round-trip before validation. Changing raw symbol, role, range or encoding changes the envelope digest; doing so without updating binary/manifest/inventory rejects the artifact. Index metadata/display fields remain authenticated via original binary hash, never silently promoted into local semantic facts.

## Measured native anchors and five destination-family gates

Native extraction of the authenticated **source strings**, independent of the SCIP role bits, can measure A top-level function `f` name `[9,10)`, invocation `g()` `[13,16)`, callee/reference `g` `[13,14)` with owning callable `f`; B top-level function `g` name `[9,10)`. All are zero-based half-open UTF-8 byte offsets and scalar boundaries. The native measurements used here are controlled fixtures; a producer's raw symbol does not prove that a source adapter actually emitted them. Stable descriptor inputs (`Key={kind:"function",name,signature:null,ordinal:0}`, empty ancestors, sourceSet `app`, language `javascript`) yield:

| Measured declaration | #22 canonical stable input | Full SHA-256 / emitted syntax ID |
|---|---|---|
| `A.f` | `{"ancestors":[],"declaration":{"kind":"function","name":"f","ordinal":0,"signature":null},"language":"javascript","path":"src/A.js","sourceSet":"app"}` | `6cce6099437ddb2256f7ae368d29c0b564d1c518f3a0373364d6ee95e7f76e64` / `sid:v1:6cce6099437ddb2256f7ae368d29c0b5` |
| `B.g` | `{"ancestors":[],"declaration":{"kind":"function","name":"g","ordinal":0,"signature":null},"language":"javascript","path":"src/B.js","sourceSet":"app"}` | `c6bd4d73134cc2ef17b8a89aac625b10bd76e32d6d683f233f8bd050968f0407` / `sid:v1:c6bd4d73134cc2ef17b8a89aac625b10` |

For A's one measured invocation and one measured use-reference (ordinal 0 within each occurrence kind), the exact #22 occurrence inputs/outputs are:

| Revision/kind | canonical input | SHA-256 / `OccurrenceId` |
|---|---|---|
| `r1/call` | `{"kind":"call","ordinal":0,"ownerSyntaxId":"sid:v1:6cce6099437ddb2256f7ae368d29c0b5","revisionId":"r1"}` | `ccc4d599ce44449011bc2940733b404d7f45cee08075db53bb80525ce86418c1` / `occ:v1:ccc4d599ce44449011bc2940733b404d` |
| `r2/call` | `{"kind":"call","ordinal":0,"ownerSyntaxId":"sid:v1:6cce6099437ddb2256f7ae368d29c0b5","revisionId":"r2"}` | `059293315079942eb72e4942e2a578d6662f8ac5f10a9eeb721fdb5f80c91f16` / `occ:v1:059293315079942eb72e4942e2a578d6` |
| `r1/reference` | `{"kind":"reference","ordinal":0,"ownerSyntaxId":"sid:v1:6cce6099437ddb2256f7ae368d29c0b5","revisionId":"r1"}` | `2bc25eae3e1135b41ea3b91b23a36d013ba688f2d284c443d3c58d94d5aafa5c` / `occ:v1:2bc25eae3e1135b41ea3b91b23a36d01` |
| `r2/reference` | `{"kind":"reference","ordinal":0,"ownerSyntaxId":"sid:v1:6cce6099437ddb2256f7ae368d29c0b5","revisionId":"r2"}` | `ee89d25b42ad734b5d9fce3072779276d994fbde88b12d32623d18f8cc292f54` / `occ:v1:ee89d25b42ad734b5d9fce3072779276` |

Only a separately established semantic binding can select B.g as an internal callee/reference. A source candidate is exactly one **distinct** compatible r2 native ID at its converted span/kind/document/source set; two raw facts matching that one native candidate remain two proofs, not two measured anchors. A selected direct dispatch requires additional independent producer/language evidence; raw SCIP symbol roles 0 or 1 cannot establish direct dispatch, and even an independently proven direct binding with r1 basis cannot expand because it is `possiblyStale`. The following positives are **conditional** on the explicitly stated independent semantic evidence, not inferred from the wire:

| Family | Raw witness + independent measurement/semantic assertion | r2 outcome | Negative control |
|---|---|---|---|
| `CallBinding` | `call-occurrence`; measured A `[13,16)` invocation and `[13,14)` callee; independently established resolved B.g target and supported dispatch | r2 call ID above, owner A.f, r2 B.g target, minted derived proof (`recordKind:occurrence`); `possiblyStale`, `staleTarget=false`, no expansion | Two distinct measured A calls compatible at callee span → A selected `failed`; B missing while independent A reference succeeds → omit call alone, A `partial`. |
| `Reference` | `reference-occurrence`; independently measured A use at `[13,14)` plus proven B.g resolution | r2 reference ID above and r2 B.g target, proof from raw occurrence | No compatible measured A reference → A `failed`; B target missing/nonunique alone → omit dependent reference and A `partial`. |
| `DeclarationBinding` | `definition-occurrence` role 1, **independently** measured A.f declaration name `[9,10)` and semantic symbol A# | exact r2 A.f syntax ID and binding; raw role alone does not supply ID | Two distinct compatible measured A declarations at `[9,10)` → A `failed`, not first-wins. |
| `Symbol` internal declaration target | `symbol-information` together with independently joined `definition-occurrence` and A.f measurement; source `A#` alone is insufficient | r2 A.f target; both raw proofs remain distinct, each minted derived link uses its own raw kind/digest | Missing A.f source → A `failed`; a dependent missing B target omits only that dependent claim and makes A `partial`. |
| `TypeRelationship` | `relationship` flags `(true,true,false,false)` preserved; **only** in a separate language/source fixture that independently measures compatible subtype/base declarations and establishes actual `extends|implements|overrides` classification/direction | r2 same-ID source/target and raw relationship proof (`recordKind:relationship`), `possiblyStale`; no type relation follows from this function-only binary | This JavaScript A.f→B.g binary has no verified type relationship, so it must **not** create one. An independently proven type fixture with missing/nonunique B target omits only its dependent relationship, makes A `partial`; ambiguous A type source fails A. |

A single raw `occurrence` can support both a call and reference **only when** independent native measurements and family-specific semantic facts support both: reuse its `factDigest`/captured proof ID, never add a destination-family discriminator or duplicate claim. If two distinct envelopes share one compatible native source candidate, join remains exact and each provenance survives; conflicting proven targets produce `resolution:ambiguous` with sorted candidates, not first-wins. A raw relationship containing both reference and implementation flags is valid, but those bits cannot select a Baleyg relation kind. The TypeRelationship positive above is a conditional gate vector, not a claimed type proof from the shown function-only source. No fabricated call or class declaration enters the authenticated fixture.

## Separate source-derived type-relationship positive

This **independent** admitted fixture has the same source set/profile/component bytes as above, revision r1, but its complete manifest consists solely of `src/types/A.js` and `src/types/B.js` (no paths from the function fixture). The exact source bytes are `636c617373204120657874656e64732042207b7d0a` (`class A extends B {}\n`, SHA-256 `7bc0517d8dbd4f7c8578e866904f32fb64ed6fb4f7dd23ff94cfc7170d1185ce`) and `636c6173732042207b7d0a` (`class B {}\n`, SHA-256 `18ba722e8c0cbb2aa3d7d5a8c79f0e65b50318e1391aae91b462444b6cb39f60`). Its complete #22 manifest and hash are:

```json
[{"contentHash":"7bc0517d8dbd4f7c8578e866904f32fb64ed6fb4f7dd23ff94cfc7170d1185ce","document":{"language":"javascript","path":"src/types/A.js","sourceSetId":"app"}},{"contentHash":"18ba722e8c0cbb2aa3d7d5a8c79f0e65b50318e1391aae91b462444b6cb39f60","document":{"language":"javascript","path":"src/types/B.js","sourceSetId":"app"}}]
```

`sourceManifestHash = 1474916bdab5c2922bff462165013f38a8f17c58afedd3f1bc93762faa4abd05`. The full **122-byte** protobuf Index below has A definition `[0,6,7]`, A use of B `[0,16,17]`, A symbol information with a separate raw relationship to B preserving `(isReference,isImplementation,isTypeDefinition,isDefinition)=(true,true,false,false)`, and B definition `[0,6,7]`; both documents explicitly declare position encoding 1. No other facts are in this fixture. The exact wire and artifact hash are:

```text
124b0a0e7372632f74797065732f412e6a73220a6a617661736372697074120b0a0300060712024123180112090a03001011120242231a130a02412322080a0242231001180128073201413001122b0a0e7372632f74797065732f422e6a73220a6a617661736372697074120b0a030006071202422318013001
```

`9943b2b703c03c7112edad95a26d99656d106054d2d1ab498bd6edd708473516`. The relation envelope's complete #22 canonical bytes, digest and proof tuple are:

```json
{"formatVersion":1,"positionEncoding":1,"raw":{"isDefinition":false,"isImplementation":true,"isReference":true,"isTypeDefinition":false,"sourceSymbol":"A#","targetSymbol":"B#"},"recordKind":"relationship","relativePath":"src/types/A.js","scope":"document"}
```

`factDigest = 0b4a7c80ed964f827deb5ea8fb4fa1150d089920bca0f7e62f3607455afaecfb`.

```json
{"artifactHash":"9943b2b703c03c7112edad95a26d99656d106054d2d1ab498bd6edd708473516","factDigest":"0b4a7c80ed964f827deb5ea8fb4fa1150d089920bca0f7e62f3607455afaecfb","language":"javascript","producerId":"scip-test","relativePath":"src/types/A.js","sourceRevision":"r1","sourceSetId":"app"}
```

`capturedProofId = 07e88a555d72a75a90c4dfe18b9d9761678ae980b95625898ca46248e2192aef`. Native parsing **independently** measures A class name byte `[6,7)`, B class name byte `[6,7)`, and A `extends B` type-syntax reference `[16,17)`; the raw flags alone do not prove an `extends` destination kind. Native descriptor inputs and full SHA-256 / stable ID pairs are:

- `{"ancestors":[],"declaration":{"kind":"type","name":"A","ordinal":0,"signature":null},"language":"javascript","path":"src/types/A.js","sourceSet":"app"}` → `c842b1aba16762e894d8304d25c0db4bdcbcd47544c406646e1af46fb3cc9a2a` / `sid:v1:c842b1aba16762e894d8304d25c0db4b`.
- `{"ancestors":[],"declaration":{"kind":"type","name":"B","ordinal":0,"signature":null},"language":"javascript","path":"src/types/B.js","sourceSet":"app"}` → `5535a303c2d5aeb6df14cb52dcc959bc4c8e0a439b051964d23f751c9ef4cc7d` / `sid:v1:5535a303c2d5aeb6df14cb52dcc959bc`.

When the language-specific semantic assertion independently establishes that A extends B (consistent with native `extends` syntax and verified symbol-resolution) and uniquely measures both same-ID r2 declarations, mint r2 `TypeRelationship{kind:extends,source:A,target:B}` with r2 internal targets, **new** A provenance derived from captured relationship proof above, captured r1 basis, `possiblyStale`, and no expansion. If B changes but exactly one compatible same-ID r2 declaration survives, A can retarget it. If B fails same-ID measured verification, omit only dependent relation, keep independent A definition/binding and A `partial` with diagnostic; if A source class anchor is missing/ambiguous, fail all A. The flags `(true,true,false,false)` are valid but do **not** mechanically imply `extends`; absent independent language-specific relationship proof, emit **no** TypeRelationship even though the captured envelope and digest remain valid.

## Publication, failure, history and warnings

For each minted r2 semantic provenance, use a **new** unique destination ID, `revisionId=r2`, A destination key/hash, original `SemanticBasis(revisionId=r1)` unchanged and `derivedFrom:{provenanceId:<listed capturedProofId>,recordKind:<listed raw kind>,factDigest:<listed D(E)>}`. Captured proofs retain `derivedFrom:null`. Reject a claim using another row's digest, unknown ID, forged tuple, missing envelope or two distinct proof bindings; identical raw envelopes within one authenticated tuple collapse before proof lookup and do not cause ambiguity. Distinct raw bytes/context do not collapse.

- Unchanged A, changed B bytes and **one uniquely measured compatible B.g declaration with the same verified stable ID**: rejoin A's dependent target into B at r2; B itself is not rejoined. A minted proof is `possiblyStale`, `staleTarget=false`, no traversal expansion. If B.g is absent or not uniquely measured, omit only dependent A call/reference/relationship claims; keep independent A declaration fact and `partial` A coverage with `observedRoles=[definition]` if definition was the only delivered role; diagnostic names omitted roles. Do not fabricate external B. A malformed source anchor instead selects `failed` A, zero A facts.
- A malformed binary, source admission, manifest hash, inventory, component hash, encoding, or conversion rejects the **whole producer artifact**. If another path publishes r2, each requested attributable P document tuple is selected `failed`, diagnosed, with zero P facts; another producer can succeed independently. Cancellation exposes no partial revision. A valid destination failure may coexist with successful tuples in one native publication transaction. A later explicit import races via CAS and on loss publishes no partial overlay.
- Changed A bytes: no A occurrence rejoin. If A r2 tuple is `failed|omitted`, #47 returns only latest earlier eligible `complete|partial` declaration-keyed provenance for **returned** r2 same-ID declarations in that document, plus that earlier tuple's coverage row even if no fact names a returned declaration. Older-than-latest, cross-document, unrelated declaration and occurrence-keyed evidence remain absent. Changed bytes → `stale` historical provenance; unchanged bytes but different revision → `possiblyStale`. If r1 latest is `partial`, do not reach back to an earlier complete tuple. No old call binding returns.
- Warning keys for selected A `partial` or `failed` with returned minted possibly-stale proof: `[(coverageIncomplete,null),(staleEvidence,null)]`; if stale historical proof `pH` also returned: `[(coverageIncomplete,null),(staleEvidence,null),(staleEvidence,pH)]`. Selected `complete` plus only minted possibly-stale: `[(staleEvidence,null)]`. A current unselected `omitted` row alone adds no `coverageIncomplete`. `staleTarget=true` and its warning are invalid, not a positive vector. Warning ordering follows warnings-v1 (code enum, then null-before-text); `partial`/boundary behavior follows the unchanged graph contract.
