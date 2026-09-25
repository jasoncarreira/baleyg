// Graph-only view of validated normalized facts against the requested admitted revision.
// Normalized records keep their fixture comparison-relative freshness labels; this
// relabels them with the same rules for another pinned revision.
import { expectedFreshness, expectedStaleTarget } from "./check-freshness.mjs";

const tuple = (set, revision) => JSON.stringify([set, revision]);
function missing(field) {
  const error = new Error(
    `GRAPH.PROJECTION ${field}: normalized graph records are incomplete`,
  );
  Object.assign(error, {
    assertion: "GRAPH.PROJECTION",
    code: "invalidRecord",
    field,
  });
  throw error;
}

export function graphProjection(loaded, records, request) {
  if (!records?.comparison || !Array.isArray(records.comparison.producers))
    missing("comparison");
  if (
    !Array.isArray(records.revisions) ||
    !records.revisions.every((r) => Array.isArray(r.documents))
  )
    missing("revisions.documents");
  if (!Array.isArray(records.declarations)) missing("declarations");
  if (!Array.isArray(records.producers)) missing("producers");
  if (!Array.isArray(records.provenance)) missing("provenance");
  if (
    !records.revisions.some(
      (r) =>
        r.sourceSetId === request.sourceSetId && r.id === request.revisionId,
    )
  )
    missing("revisionId");
  // Normalized labels already use these rules against the comparison revision.
  if (
    records.comparison.sourceSetId === request.sourceSetId &&
    records.comparison.revisionId === request.revisionId
  )
    return { proof: (row) => row, binding: (row) => row };
  if (!loaded.revisions?.get(tuple(request.sourceSetId, request.revisionId)))
    missing("revisionId");
  const declarations = new Map();
  for (const d of records.declarations) {
    const k = tuple(d.document.sourceSetId, d.revisionId);
    if (!declarations.has(k)) declarations.set(k, new Map());
    declarations.get(k).set(d.syntaxId, d.document);
  }
  const proofs = new Map(records.provenance.map((row) => [row.id, row]));
  return {
    proof: (row) => ({
      ...row,
      freshness: expectedFreshness(row, loaded, request),
    }),
    binding: (row) =>
      row.declaredTarget?.kind !== "internal"
        ? row
        : {
            ...row,
            staleTarget: expectedStaleTarget(
              row,
              proofs.get(row.provenanceId),
              loaded,
              declarations,
              request,
            ),
          },
  };
}
