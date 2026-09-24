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

// JSON.parse validates syntax below. This bounded lexical pass only records object keys.
// Parsing each key string with JSON.parse gives the same Unicode/escape identity as
// the full parse (including escaped surrogate pairs), without reimplementing JSON.
const MAX_VECTOR_BYTES = 8 * 1024 * 1024;
const MAX_JSON_DEPTH = 128;
function duplicateObjectKey(source) {
  const stack = [];
  for (let index = 0; index < source.length; index++) {
    const char = source[index];
    if (char === '"') {
      const start = index;
      while (++index < source.length) {
        if (source[index] === "\\") { index++; continue; }
        if (source[index] === '"') break;
      }
      // Leave malformed strings to the full JSON.parse syntax check.
      if (index === source.length) return null;
      const object = stack.at(-1);
      if (object?.expectKey) {
        let key;
        try { key = JSON.parse(source.slice(start, index + 1)); }
        catch { return null; }
        if (object.keys.has(key)) return {key, offset: start};
        object.keys.add(key);
        object.expectKey = false;
      }
    } else if (char === "{" || char === "[") {
      if (stack.length >= MAX_JSON_DEPTH) fail("stable-ids.json", "JSON nesting exceeds limit");
      stack.push(char === "{" ? {keys: new Set(), expectKey: true} : null);
    } else if (char === "}" || char === "]") {
      stack.pop();
    } else if (char === "," && stack.at(-1)?.keys) {
      stack.at(-1).expectKey = true;
    }
  }
  return null;
}

function selfTest() {
  assert.equal(duplicateObjectKey('{"a":1,"a":2}').key, "a");
  assert.equal(duplicateObjectKey('{"a":1,"\\u0061":2}').key, "a");
  assert.equal(duplicateObjectKey('{"😀":1,"\\uD83D\\uDE00":2}').key, "😀");
  assert.equal(duplicateObjectKey('{"outer":[{"x":1,"x":2}]}').key, "x");
  const distinct = '{"x":1,"nested":{"x":2},"s":"\\"x,{}"}';
  assert.doesNotThrow(() => JSON.parse(distinct));
  assert.equal(duplicateObjectKey(distinct), null);
  assert.throws(() => decodeUtf8(Buffer.from([0xff])), /invalid UTF-8/);
  console.log("JSON input self-test passed");
}

function fail(path, message) { throw new Error(`${path}: ${message}`); }
function decodeUtf8(bytes) {
  const source = bytes.toString("utf8");
  if (!Buffer.from(source, "utf8").equals(bytes)) fail("stable-ids.json", "invalid UTF-8");
  return source;
}
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
  if (type === "syntaxId") return typeof value === "string" && /^sid:v1:[0-9a-f]{32}$/.test(value) || fail(path, "must be a SyntaxId");
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

if (process.argv.includes("--self-test")) {
  selfTest();
} else {
  try {
    const file = new URL("./stable-ids.json", import.meta.url);
    if (fs.statSync(file).size > MAX_VECTOR_BYTES) fail("stable-ids.json", "file exceeds size limit");
    const bytes = fs.readFileSync(file);
    if (bytes.length > MAX_VECTOR_BYTES) fail("stable-ids.json", "file exceeds size limit");
    const source = decodeUtf8(bytes);
    const duplicate = duplicateObjectKey(source);
    let document;
    try {
      document = JSON.parse(source);
    } catch (error) {
      fail("stable-ids.json", `parse failed: ${error.message}`);
    }
    if (duplicate) fail("stable-ids.json", `duplicate object key ${JSON.stringify(duplicate.key)} at offset ${duplicate.offset}`);
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
}
