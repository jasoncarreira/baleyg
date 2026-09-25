import test from "node:test";
import assert from "node:assert/strict";
import {
  toByteRange,
  verifyWitness,
  verifyCallSpelling,
} from "../coordinates.mjs";
import { sourceWitness } from "./helpers.mjs";
test("COORD.CONVERSION unicode scalar, UTF-16 and UTF-8 on CRLF source", () => {
  const source = "A😀é\r\ncall()";
  for (const encoding of ["utf8", "utf16", "unicodeScalar"]) {
    const witness = sourceWitness(source, "é", { encoding });
    assert.deepEqual(toByteRange(source, witness.range), { start: 5, end: 7 });
    assert.deepEqual(verifyWitness(source, witness), { start: 5, end: 7 });
  }
  const after = {
    utf8: { start: 9, end: 13 },
    utf16: { start: 6, end: 10 },
    unicodeScalar: { start: 5, end: 9 },
  };
  for (const encoding of Object.keys(after)) {
    const position = after[encoding];
    assert.deepEqual(toByteRange(source, { encoding, ...position }), {
      start: 9,
      end: 13,
    });
  }
  assert.deepEqual(
    toByteRange(source, { encoding: "utf8", start: 15, end: 15 }),
    { start: 15, end: 15 },
  );
  assert.deepEqual(
    toByteRange(source, { encoding: "utf16", start: 12, end: 12 }),
    { start: 15, end: 15 },
  );
  assert.deepEqual(
    toByteRange(source, { encoding: "unicodeScalar", start: 11, end: 11 }),
    { start: 15, end: 15 },
  );
  assert.throws(
    () => toByteRange(source, { encoding: "utf16", start: 2, end: 3 }),
    /invalidRange/,
  );
  assert.throws(
    () => toByteRange(source, { encoding: "utf8", start: 2, end: 5 }),
    /invalidRange/,
  );
  assert.deepEqual(
    toByteRange("", { encoding: "unicodeScalar", start: 0, end: 0 }),
    { start: 0, end: 0 },
  );
});
test("WITNESS.SPELLING validates separate spelling when calleeRange is null", () => {
  const source = "schedule(handler)";
  const invocation = { start: 0, end: 17 };
  const spelling = sourceWitness(source, "handler", { encoding: "utf16" });
  const call = { range: invocation, calleeRange: null, spelling: "handler" };
  assert.deepEqual(verifyCallSpelling(source, call, spelling), {
    start: 9,
    end: 16,
  });
  assert.equal(call.calleeRange, null);
  assert.throws(
    () => verifyCallSpelling(source, call, null),
    /WITNESS.MISSING/,
  );
  assert.throws(
    () => verifyCallSpelling(source, call, { ...spelling, text: "other" }),
    /WITNESS.BYTES/,
  );
  assert.throws(
    () => verifyCallSpelling(source, call, sourceWitness(source, "schedule")),
    /WITNESS.BYTES/,
  );
  assert.throws(
    () =>
      verifyCallSpelling(source, call, {
        ...spelling,
        range: { encoding: "utf16", start: 9, end: 17 },
      }),
    /WITNESS.BYTES/,
  );
  assert.throws(
    () =>
      verifyCallSpelling(
        source,
        { ...call, range: { start: 0, end: 8 } },
        spelling,
      ),
    /WITNESS.CONTAINMENT/,
  );
  assert.deepEqual(verifyWitness(source, spelling, { within: invocation }), {
    start: 9,
    end: 16,
  });
  assert.throws(
    () =>
      verifyWitness(
        source,
        { ...spelling, text: "other" },
        { within: invocation },
      ),
    /WITNESS.BYTES/,
  );
  assert.throws(
    () => verifyWitness(source, spelling, { within: { start: 0, end: 8 } }),
    /WITNESS.CONTAINMENT/,
  );
});
test("COORD.INVALID_UTF8 rejects malformed exact source bytes", () => {
  assert.throws(
    () =>
      toByteRange(Buffer.from([0xff]), { encoding: "utf8", start: 0, end: 1 }),
    /not valid for encoding utf-8/,
  );
});
