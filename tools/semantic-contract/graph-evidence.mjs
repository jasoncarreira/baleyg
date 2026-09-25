import { canonicalBytes } from "./json.mjs";
import { graphProjection } from "./graph-projection.mjs";

const key = (value) => JSON.stringify(value);
const docKey = (document) =>
  key([document.sourceSetId, document.language, document.path]);
const tupleKey = (producerId, document, revisionId) =>
  key([producerId, ...JSON.parse(docKey(document)), revisionId]);
const byteOrder = (a, b) => Buffer.compare(Buffer.from(a), Buffer.from(b));
const canonical = (value) => canonicalBytes(value).toString("hex");
function fail(assertion, field, message) {
  const error = new Error(`${assertion} ${field}: ${message}`);
  Object.assign(error, { assertion, code: "invalidRecord", field });
  throw error;
}
function documentFor(row) {
  return {
    sourceSetId: row.sourceSetId,
    language: row.language,
    path: row.documentPath,
  };
}
function linkedFact(fact, proof, loaded, document, revisionId) {
  const captured = loaded.semanticProofs.get(proof.id);
  const proofId =
    fact.kind === "typeRelationship"
      ? fact.provenanceRef
      : fact.record?.provenanceId;
  return (
    proofId === proof.id &&
    captured?.factRef === fact.ref &&
    captured.factKind === fact.kind &&
    canonical(captured.fact) === canonical(fact) &&
    proof.basis?.artifactHash === captured.hash &&
    docKey(proof.document) === docKey(document) &&
    proof.revisionId === revisionId
  );
}

// Select only evidence associated with returned nodes and measured emitted calls.
// The traversal itself never applies old declaration facts to current syntax.
export function selectGraphEvidence(loaded, records, checked, result) {
  const revisionId = result.request.revisionId,
    producerId = result.request.semanticProducerId;
  const nativeId = loaded.native.producerId;
  const coverageByTuple = new Map(
    records.coverage.map((row) => [
      tupleKey(row.producerId, documentFor(row), row.revisionId),
      row,
    ]),
  );
  const proofById = new Map(records.provenance.map((row) => [row.id, row]));
  const projection = graphProjection(loaded, records, result.request);
  const coverage = new Map(),
    provenance = new Map(),
    documents = new Map(),
    returned = new Map();
  const addDocument = (document) => documents.set(docKey(document), document);
  for (const node of result.nodes) {
    const declaration = node.declaration;
    addDocument(declaration.document);
    const k = docKey(declaration.document);
    if (!returned.has(k)) returned.set(k, new Set());
    returned.get(k).add(declaration.syntaxId);
  }
  for (const edge of result.edges) addDocument(edge.call.document);
  function addCoverage(id, document, revision) {
    const row = coverageByTuple.get(tupleKey(id, document, revision));
    if (!row)
      fail(
        "GRAPH.COVERAGE",
        "coverage",
        `missing ${id}/${revision}/${document.path} tuple`,
      );
    coverage.set(tupleKey(id, document, revision), row);
    return row;
  }
  function addProof(id, producer, document, revision) {
    const proof = proofById.get(id);
    if (
      !proof ||
      proof.producerId !== producer ||
      docKey(proof.document) !== docKey(document) ||
      proof.revisionId !== revision
    )
      fail("GRAPH.PROVENANCE", "provenance", `unmatched proof ${id}`);
    checked.checkUse({
      producerId: producer,
      document,
      revisionId: revision,
      provenanceIds: [id],
    });
    provenance.set(id, projection.proof(proof));
  }
  for (const document of documents.values()) {
    addCoverage(nativeId, document, revisionId);
    if (producerId !== null) addCoverage(producerId, document, revisionId);
  }
  for (const node of result.nodes)
    addProof(
      node.declaration.provenanceId,
      nativeId,
      node.declaration.document,
      revisionId,
    );
  for (const edge of result.edges) {
    addProof(edge.call.provenanceId, nativeId, edge.call.document, revisionId);
    if (edge.binding !== null) {
      const current = coverageByTuple.get(
        tupleKey(producerId, edge.call.document, revisionId),
      );
      if (
        current &&
        (!current.selected || !["complete", "partial"].includes(current.state))
      )
        fail(
          "GRAPH.OCCURRENCE",
          "edges.binding",
          "failed or omitted current tuple cannot authorize a call binding",
        );
      if (
        producerId === null ||
        edge.call.revisionId !== revisionId ||
        edge.binding.callId !== edge.call.id ||
        edge.binding.join.anchor.revisionId !== revisionId ||
        docKey(edge.binding.join.anchor.document) !== docKey(edge.call.document)
      )
        fail(
          "GRAPH.OCCURRENCE",
          "edges.binding",
          "old or foreign binding on current occurrence",
        );
      // A contradictory group is ambiguous on every member; each member's proof
      // remains relevant evidence even though the edge carries one of them.
      const group = records.callBindings.filter(
        (b) =>
          b.callId === edge.binding.callId &&
          b.resolution === "ambiguous" &&
          edge.binding.resolution === "ambiguous" &&
          proofById.get(b.provenanceId)?.producerId === producerId &&
          b.join.anchor.revisionId === revisionId,
      );
      for (const id of new Set([
        edge.binding.provenanceId,
        ...group.map((b) => b.provenanceId),
      ]))
        addProof(id, producerId, edge.call.document, revisionId);
    }
  }
  if (producerId !== null) {
    const chronology = loaded.revisionChronology.get(
      result.request.sourceSetId,
    );
    const position =
      chronology?.findIndex((row) => row.id === revisionId) ?? -1;
    if (position < 0)
      fail(
        "GRAPH.CHRONOLOGY",
        "request.revisionId",
        "snapshot absent from admitted chronology",
      );
    for (const [documentKey, syntaxIds] of returned) {
      const document = documents.get(documentKey),
        current = coverageByTuple.get(
          tupleKey(producerId, document, revisionId),
        );
      let selectedRevision = revisionId;
      if (["failed", "omitted"].includes(current.state)) {
        const previous = [...chronology.slice(0, position)]
          .reverse()
          .find((snapshot) => {
            const row = coverageByTuple.get(
              tupleKey(producerId, document, snapshot.id),
            );
            return row && ["complete", "partial"].includes(row.state);
          });
        if (!previous) continue;
        selectedRevision = previous.id;
        const captured = addCoverage(producerId, document, selectedRevision);
        if (!captured.selected)
          fail("GRAPH.HISTORY", "coverage", "historical tuple is not selected");
      } else if (!current.selected) continue;
      for (const annotation of loaded.annotations) {
        if (
          annotation.revisionId !== selectedRevision ||
          docKey(annotation.document) !== documentKey
        )
          continue;
        for (const fact of annotation.facts) {
          if (
            !["declarationBinding", "symbol", "typeRelationship"].includes(
              fact.kind,
            )
          )
            continue;
          const normalized =
            records[
              fact.kind === "declarationBinding"
                ? "declarationBindings"
                : fact.kind === "symbol"
                  ? "symbols"
                  : "typeRelationships"
            ];
          const proofId =
            fact.kind === "typeRelationship"
              ? fact.provenanceRef
              : fact.record.provenanceId;
          const proof = proofById.get(proofId);
          if (proof?.producerId !== producerId) continue;
          if (!linkedFact(fact, proof, loaded, document, selectedRevision))
            fail(
              "GRAPH.HISTORY",
              "provenance",
              "historical fact lacks captured producer/document proof",
            );
          const names = normalized.some(
            (row) =>
              row.provenanceId === proofId &&
              (fact.kind === "declarationBinding"
                ? row.syntaxId !== null && syntaxIds.has(row.syntaxId)
                : fact.kind === "symbol"
                  ? row.declarations.some(
                      (target) =>
                        target.kind === "internal" &&
                        syntaxIds.has(target.syntaxId) &&
                        docKey(target.document) === documentKey,
                    )
                  : row.source.kind === "internal" &&
                    syntaxIds.has(row.source.syntaxId) &&
                    docKey(row.source.document) === documentKey),
          );
          if (!names) continue;
          if (selectedRevision !== revisionId) {
            const capturedDocument = chronology
              .find((snapshot) => snapshot.id === selectedRevision)
              ?.documents.find((row) => docKey(row.key) === documentKey);
            const requestedDocument = chronology[position].documents.find(
              (row) => docKey(row.key) === documentKey,
            );
            const expectedFreshness =
              requestedDocument?.contentHash === capturedDocument?.contentHash
                ? "possiblyStale"
                : "stale";
            if (
              !capturedDocument ||
              proof.contentHash !== capturedDocument.contentHash ||
              projection.proof(proof).freshness !== expectedFreshness
            )
              fail(
                "GRAPH.HISTORY",
                "provenance",
                "historical proof bytes or requested freshness differ",
              );
          }
          addProof(proofId, producerId, document, selectedRevision);
        }
      }
    }
  }
  const coverageRows = [...coverage.values()].sort(
    (a, b) =>
      byteOrder(a.producerId, b.producerId) ||
      byteOrder(a.sourceSetId, b.sourceSetId) ||
      byteOrder(a.language, b.language) ||
      byteOrder(a.documentPath, b.documentPath) ||
      byteOrder(a.revisionId, b.revisionId),
  );
  const proofRows = [...provenance.values()].sort((a, b) =>
    byteOrder(a.id, b.id),
  );
  return {
    coverage: coverageRows,
    provenance: proofRows,
    selectedCoverageIncomplete: coverageRows.some(
      (row) => row.selected && ["failed", "partial"].includes(row.state),
    ),
  };
}

export function checkGraphEvidence(
  loaded,
  records,
  checked,
  actual,
  expectedTraversal,
) {
  if (!expectedTraversal)
    fail(
      "GRAPH.TRAVERSAL",
      "result",
      "independent expected traversal required",
    );
  const expected = selectGraphEvidence(
    loaded,
    records,
    checked,
    expectedTraversal,
  );
  for (const field of ["coverage", "provenance"])
    if (canonical(actual[field]) !== canonical(expected[field]))
      fail(
        field === "coverage" ? "GRAPH.COVERAGE" : "GRAPH.PROVENANCE",
        field,
        `returned ${field} differs from source-derived evidence`,
      );
  return expected;
}
