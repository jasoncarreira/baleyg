import fs from "node:fs";
import crypto from "node:crypto";
import assert from "node:assert/strict";

const LANGUAGES = ["java", "rust", "python", "javascript"];
const KINDS = ["module", "namespace", "type", "implementation", "function", "method",
  "constructor", "field", "variable", "parameter", "typeParameter", "alias", "anonymousFunction"];
const DOMAINS = new Set([
  Buffer.from("baleyg.syntax.v1\0").toString("hex"),
  Buffer.from("baleyg.header.v1\0").toString("hex"),
  Buffer.from("baleyg.sibling-group.v1\0").toString("hex"),
]);
const ENUMS = {
  language: LANGUAGES,
  kind: KINDS,
  algorithm: ["sha256"],
  continuityState: ["unchanged", "changed", "unknown"],
  anchorStatus: ["attached", "orphaned"],
  anchorReason: ["none", "missing", "headerMismatch", "groupChanged", "unprovenContinuity"],
};
const SHAPES = {
  Root: {version: "version", cases: ["Case"]},
  Case: {caseId: "text", language: "language", scenario: "text", descriptor: "Descriptor",
    previousCaseId: "text?", continuity: "GroupContinuity?", digests: ["Digest"], expected: "Expected"},
  Descriptor: {sourceSet: "text", path: "path", language: "language", ancestors: ["Key"],
    declaration: "Key", header: "Header", revisionId: "text", siblingHeaders: ["Header"]},
  Key: {kind: "kind", name: "text?", signature: "Signature?", ordinal: "uint"},
  Signature: {parameterTypes: ["text"], typeParameterCount: "uint", variadic: "boolean"},
  Header: {kind: "kind", name: "text?", modifiers: ["text"], typeParameters: ["text"],
    parameters: ["Parameter"], resultType: "text?", bases: ["text"]},
  Parameter: {name: "text?", type: "text?", variadic: "boolean"},
  GroupContinuity: {fromRevisionId: "text", toRevisionId: "text", state: "continuityState", evidence: "text?"},
  Digest: {label: "text", algorithm: "algorithm", domainHex: "hex", inputHex: "hex", sha256: "hash"},
  Expected: {stableId: "syntaxId", anchor: "DurableAnchor", previousAnchorResult: "AnchorResult?"},
  DurableAnchor: {syntaxId: "syntaxId", document: "DocumentKey", capturedRevisionId: "text",
    headerHash: "hash", siblingGroupHash: "hash", siblingCount: "uint", identicalHeaderCount: "uint"},
  DocumentKey: {sourceSetId: "text", language: "language", path: "path"},
  AnchorResult: {status: "anchorStatus", targetId: "syntaxId?", reason: "anchorReason"},
};

function fail(path, message) { throw new Error(`${path}: ${message}`); }
function scalar(value, type, path) {
  if (type === "version") return value === 1 || fail(path, "must equal 1");
  if (type === "boolean") return typeof value === "boolean" || fail(path, "must be boolean");
  if (type === "uint") return Number.isSafeInteger(value) && value >= 0 || fail(path, "must be UInt");
  if (type === "text") {
    if (typeof value !== "string" || value.length === 0) fail(path, "must be nonempty text");
    for (const ch of value) {
      const cp = ch.codePointAt(0);
      if (cp >= 0xd800 && cp <= 0xdfff) fail(path, "contains a lone surrogate");
    }
    return true;
  }
  if (type === "path") {
    scalar(value, "text", path);
    const parts = value.split("/");
    return !value.startsWith("/") && !value.includes("\\") && !value.includes("\0") &&
      parts.every((part) => part !== "" && part !== "." && part !== "..") || fail(path, "must be a relative POSIX path");
  }
  if (type === "hex") return typeof value === "string" && /^(?:[0-9a-f]{2})*$/.test(value) || fail(path, "must be lowercase byte hex");
  if (type === "hash") return typeof value === "string" && /^[0-9a-f]{64}$/.test(value) || fail(path, "must be a SHA-256 hex digest");
  if (type === "syntaxId") return typeof value === "string" && /^sid:v1:[0-9a-f]{64}$/.test(value) || fail(path, "must be a SyntaxId");
  if (ENUMS[type]) return ENUMS[type].includes(value) || fail(path, `must be ${ENUMS[type].join("|")}`);
  fail(path, `unknown checker type ${type}`);
}

function check(value, type, path) {
  if (Array.isArray(type)) {
    if (!Array.isArray(value)) fail(path, "must be an array");
    value.forEach((item, index) => check(item, type[0], `${path}[${index}]`));
    return;
  }
  if (type.endsWith("?")) {
    if (value === null) return;
    return check(value, type.slice(0, -1), path);
  }
  const fields = SHAPES[type];
  if (!fields) return scalar(value, type, path);
  if (value === null || typeof value !== "object" || Array.isArray(value)) fail(path, "must be an object");
  const actual = Object.keys(value);
  for (const key of actual) if (!Object.hasOwn(fields, key)) fail(`${path}.${key}`, "unknown field");
  for (const key of Object.keys(fields)) if (!(key in value)) fail(`${path}.${key}`, "missing field");
  for (const [key, childType] of Object.entries(fields)) check(value[key], childType, `${path}.${key}`);
}

try {
  let document;
  try {
    document = JSON.parse(fs.readFileSync(new URL("./stable-ids.json", import.meta.url), "utf8"));
  } catch (error) {
    fail("stable-ids.json", `parse failed: ${error.message}`);
  }
  check(document, "Root", "$");
  assert.equal(document.cases.length, 64, "$.cases: must contain exactly 64 cases");
  const ids = new Set();
  const counts = Object.fromEntries(LANGUAGES.map((language) => [language, 0]));
  let digestCount = 0;
  for (const item of document.cases) {
    const at = `case ${item.caseId}`;
    if (ids.has(item.caseId)) fail(at, "duplicate caseId");
    ids.add(item.caseId);
    counts[item.language] += 1;
    if (item.digests.length === 0) fail(`${at}.digests`, "must not be empty");
    if (item.descriptor.siblingHeaders.length === 0) fail(`${at}.descriptor.siblingHeaders`, "must not be empty");
    const labels = new Set();
    for (const digest of item.digests) {
      if (labels.has(digest.label)) fail(`${at}.digests.${digest.label}`, "duplicate label");
      labels.add(digest.label);
      if (!DOMAINS.has(digest.domainHex)) fail(`${at}.digests.${digest.label}.domainHex`, "unknown domain");
      const actual = crypto.createHash("sha256")
        .update(Buffer.from(digest.domainHex, "hex"))
        .update(Buffer.from(digest.inputHex, "hex")).digest("hex");
      if (actual !== digest.sha256) fail(`${at}.digests.${digest.label}.sha256`, `expected ${actual}`);
      digestCount += 1;
    }
  }
  for (const language of LANGUAGES) assert.equal(counts[language], 16, `$.cases: ${language} must have 16 cases`);
  console.log(`stable ID vectors: ${document.cases.length} cases, ${digestCount} digests`);
} catch (error) {
  console.error(`stable ID vector check failed: ${error.message}`);
  process.exitCode = 1;
}
