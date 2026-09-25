import { schemas } from "./schema.mjs";
import { validate } from "./formats.mjs";
import { canonicalBytes } from "./json.mjs";

const recordFields = Object.freeze({
  Declaration: "declarations",
  Call: "calls",
  CallBinding: "callBindings",
  Coverage: "coverage",
  Provenance: "provenance",
});

export class AnswerRefError extends Error {
  constructor(path, message) {
    super(`ANSWER.REF ${path}: ${message}`);
    this.name = "AnswerRefError";
    this.assertion = "ANSWER.REF";
    this.code = "invalidRecord";
    this.field = path;
  }
}

function materialize(spec, value, path, normalized) {
  if (typeof spec === "string")
    return Object.hasOwn(schemas, spec)
      ? materialize(schemas[spec], value, path, normalized)
      : value;
  if (spec.nullable)
    return value === null
      ? null
      : materialize(spec.nullable, value, path, normalized);
  if (spec.either) {
    const [type, reference] = spec.either;
    if (
      reference === "IdentityRef" &&
      typeof value === "object" &&
      value !== null &&
      Object.hasOwn(value, "ref")
    ) {
      const id = normalized.identityMap.get(value.ref);
      if (id === undefined)
        throw new AnswerRefError(path, `unknown identity ref ${value.ref}`);
      try {
        validate(type, id);
      } catch {
        throw new AnswerRefError(
          path,
          `identity ref ${value.ref} is not ${type}`,
        );
      }
      return id;
    }
    if (
      reference === "RecordRef" &&
      typeof value === "object" &&
      value !== null &&
      Object.hasOwn(value, "recordRef")
    ) {
      const row = normalized.recordMap.get(value.recordRef);
      if (row === undefined)
        throw new AnswerRefError(path, `unknown record ref ${value.recordRef}`);
      try {
        validate(type, row);
      } catch {
        throw new AnswerRefError(
          path,
          `record ref ${value.recordRef} is not ${type}`,
        );
      }
      const collection = normalized.records[recordFields[type]];
      if (
        !collection?.some((item) =>
          canonicalBytes(item).equals(canonicalBytes(row)),
        )
      )
        throw new AnswerRefError(
          path,
          `record ref ${value.recordRef} is not a returned normalized ${type}`,
        );
      return row;
    }
    return value;
  }
  if (spec.array)
    return value.map((item, index) =>
      materialize(spec.array, item, `${path}[${index}]`, normalized),
    );
  if (spec.union)
    return materialize(
      spec.union.variants[String(value[spec.union.tag])],
      value,
      path,
      normalized,
    );
  if (spec.object)
    return Object.fromEntries(
      Object.entries(spec.object).map(([field, child]) => [
        field,
        materialize(child, value[field], `${path}.${field}`, normalized),
      ]),
    );
  return value;
}

// The authored template is the only input: no traversal/checker output is consulted here.
export function materializeAnswers(input, normalized) {
  validate("AnswersInputV1", input);
  const output = materialize(
    "AnswersInputV1",
    input,
    "AnswersInputV1",
    normalized,
  );
  validate("AnswersV1", output);
  return output;
}
