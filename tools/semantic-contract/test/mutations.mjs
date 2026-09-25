import assert from 'node:assert/strict';
import { schemas } from '../schema.mjs';
import { validate } from '../formats.mjs';

// Tests own their rows. This module enforces baseline → one mutation → precise failure.
export const controls = Object.freeze([]);
export function registerControls(rows) {
  const ids = new Set();
  for (const row of rows) {
    for (const field of ['id','baseline','mutate','expectedAssertion','expectedCode','expectedField']) {
      if (!Object.hasOwn(row,field)) throw new TypeError(`Control missing ${field}`);
    }
    if (ids.has(row.id)) throw new TypeError(`Duplicate control ${row.id}`);
    ids.add(row.id);
  }
  return rows;
}
export async function runControl(row) {
  const original = await row.baseline();
  const changed = await row.mutate(structuredClone(original));
  await assert.rejects(async () => row.check(changed), error => {
    assert.equal(error.assertion,row.expectedAssertion,`${row.id}: assertion`);
    assert.equal(error.code,row.expectedCode,`${row.id}: code`);
    assert.equal(error.field,row.expectedField,`${row.id}: field`);
    return true;
  },row.id);
  return row.id;
}
export function schemaFieldControls(typeName, specimen) {
  const shape=schemas[typeName];
  if (!shape?.object) throw new TypeError('Schema field controls require an object');
  return registerControls(Object.keys(shape.object).map(field => ({
    id:`FORMAT.${typeName}.${field}.missing`,baseline:() => { validate(typeName,specimen); return specimen; },
    mutate:value => { delete value[field]; return value; },check:value => validate(typeName,value),
    expectedAssertion:'FORMAT.SHAPE',expectedCode:'invalidRecord',expectedField:`${typeName}.${field}`
  })));
}
