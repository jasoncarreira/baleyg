import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { checkWarnings } from "../graph-warnings.mjs";
import { registerControls, runControl } from "./mutations.mjs";

function specimen() {
  const binding = {
    provenanceId: "binding-z",
    staleTarget: true,
    resolution: "ambiguous",
  };
  return {
    request: { semanticProducerId: "P" },
    coverage: [
      { selected: true, state: "failed" },
      { selected: true, state: "partial" },
      { selected: false, state: "omitted" },
    ],
    provenance: [
      { id: "binding-z", freshness: "fresh" },
      { id: "stale-z", freshness: "stale" },
      { id: "stale-a", freshness: "stale" },
      ...["p1", "p2", "p3"].map((id) => ({ id, freshness: "possiblyStale" })),
    ],
    edges: [{ binding }, { binding }],
    frontier: [{ reason: "callLimit" }],
    partial: true,
    truncated: true,
    warnings: [],
  };
}
// Hand-authored warning keys; the checker must never supply its own expectation.
const expectedKeys = [
  ["coverageIncomplete", null],
  ["staleEvidence", null],
  ["staleEvidence", "stale-a"],
  ["staleEvidence", "stale-z"],
  ["staleTarget", "binding-z"],
  ["bindingAmbiguous", "binding-z"],
];
const authored = (keys) =>
  keys.map(([code, provenanceId]) => ({ code, provenanceId, message: code }));
test("all warnings-v1 keys use enum ordering, stale aggregation and shared-edge dedup", () => {
  const s = specimen();
  s.warnings = authored(expectedKeys);
  assert.equal(checkWarnings(s), true);
  // Message text is independent of warning membership; Unicode is preserved.
  s.warnings.forEach((row, i) => (row.message = `Unicode 🦊 ${i}`));
  assert.equal(checkWarnings(s), true);
});
test("unselected omitted, limits and boundary do not imply coverage warning; syntax-only is exact", () => {
  const s = specimen();
  s.coverage = [{ state: "omitted", selected: false }];
  s.provenance = [];
  s.edges = [{ binding: null, boundaryReason: "missingEvidence" }];
  s.warnings = [];
  assert.equal(checkWarnings(s), true);
  s.request.semanticProducerId = null;
  assert.throws(() => checkWarnings(s), { assertion: "WARNING.KEYS" });
  s.warnings = authored([["syntaxOnly", null]]);
  assert.equal(checkWarnings(s), true);
});
test("warning negatives run exact checker controls", async (t) => {
  const s = specimen();
  s.warnings = authored(expectedKeys);
  const rows = registerControls(
    [
      [
        "missing",
        (x) => {
          x.warnings.shift();
          return x;
        },
        "WARNING.KEYS",
        "warnings",
      ],
      [
        "extra",
        (x) => {
          x.warnings.push({
            code: "syntaxOnly",
            provenanceId: null,
            message: "extra",
          });
          return x;
        },
        "WARNING.KEYS",
        "warnings",
      ],
      [
        "duplicate",
        (x) => {
          x.warnings.push({ ...x.warnings[0] });
          return x;
        },
        "WARNING.DUPLICATE",
        `warnings[${s.warnings.length}]`,
      ],
      [
        "order",
        (x) => {
          x.warnings.reverse();
          return x;
        },
        "WARNING.KEYS",
        "warnings",
      ],
      [
        "shape",
        (x) => {
          x.warnings[0].message = "";
          return x;
        },
        "WARNING.SHAPE",
        "warnings[0]",
      ],
      [
        "foreignBindingProof",
        (x) => {
          x.edges[0].binding.provenanceId = "unreturned";
          return x;
        },
        "WARNING.PROVENANCE",
        "edges.binding.provenanceId",
      ],
    ].map(([id, mutate, expectedAssertion, expectedField]) => ({
      id: `WARNING.${id}`,
      baseline: () => s,
      mutate,
      check: checkWarnings,
      expectedAssertion,
      expectedCode: "invalidRecord",
      expectedField,
    })),
  );
  for (const row of rows) await t.test(row.id, () => runControl(row));
  const omitted = specimen();
  omitted.coverage = [{ state: "omitted", selected: false }];
  omitted.provenance = [];
  omitted.edges = [];
  omitted.warnings = [];
  const selected = registerControls([
    {
      id: "WARNING.selectedOnly",
      baseline: () => omitted,
      mutate: (x) => {
        x.warnings = [
          {
            code: "coverageIncomplete",
            provenanceId: null,
            message: "not selected",
          },
        ];
        return x;
      },
      check: checkWarnings,
      expectedAssertion: "WARNING.KEYS",
      expectedCode: "invalidRecord",
      expectedField: "warnings",
    },
  ])[0];
  await t.test(selected.id, () => runControl(selected));
});
