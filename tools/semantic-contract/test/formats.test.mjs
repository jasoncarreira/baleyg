import test from 'node:test';
import assert from 'node:assert/strict';
import { schemas } from '../schema.mjs';
import { validate } from '../formats.mjs';
import { canonicalBytes, parseJson } from '../json.mjs';
import { registerControls, runControl, schemaFieldControls } from './mutations.mjs';

function sample(spec, trail=[]) {
  if (typeof spec === 'string' && Object.hasOwn(schemas,spec)) {
    if (trail.includes(spec)) throw new Error(`recursive schema: ${spec}`);
    return sample(schemas[spec],[...trail,spec]);
  }
  if (typeof spec === 'string') return ({text:'sample',uint:1,boolean:false,hash:'a'.repeat(64),path:'src/a.js',syntaxId:'sid:v1:'+'a'.repeat(32),occurrenceId:'occ:v1:'+'a'.repeat(32)})[spec];
  if (spec.nullable) return null;
  if (spec.either) return sample(spec.either[0],trail);
  if (Object.hasOwn(spec,'literal')) return spec.literal;
  if (spec.enum) return spec.enum[0];
  if (spec.array) return [];
  if (spec.union) return sample(Object.values(spec.union.variants)[0],trail);
  if (spec.object) return Object.fromEntries(Object.entries(spec.object).map(([k,v]) => [k,sample(v,trail)]));
  throw Error(`unknown schema ${String(spec)}`);
}
function failure(type,value,field) { assert.throws(() => validate(type,value),e => e.assertion === 'FORMAT.SHAPE' && e.field === field); }
test('FORMAT.REGISTRY: every type and every required object field is closed',async t => {
  for (const [name,spec] of Object.entries(schemas)) {
    await t.test(name,async () => {
      const value=sample(name); validate(name,value);
      if (spec.object) {
        for (const control of schemaFieldControls(name,value)) await runControl(control);
        failure(name,{...value,unexpected:true},`${name}.unexpected`);
        for (const [field,fieldSpec] of Object.entries(spec.object)) {
          const bad={...value,[field]:null};
          if (!fieldSpec?.nullable && !(fieldSpec?.literal && fieldSpec.literal === null)) failure(name,bad,`${name}.${field}`);
          if (fieldSpec?.nullable) failure(name,{...value,[field]:undefined},`${name}.${field}`);
        }
      }
    });
  }
});

test('FORMAT.UNIONS: every variant has closed fields',() => {
  for (const [name,spec] of Object.entries(schemas)) {
    if (!spec.union) continue;
    for (const [tag,variant] of Object.entries(spec.union.variants)) {
      const value=sample(variant); validate(name,value);
      failure(name,{...value,legacyRelationship:'extends'},`${name}.legacyRelationship`);
      const unknown={...value,[spec.union.tag]: '__unknown__'};
      failure(name,unknown,`${name}.${spec.union.tag}`);
    }
  }
});
test('RELATIONSHIP_FORMAT: explicit and independent relationshipKind for all three values',() => {
  const relationship=sample('TypeRelationshipFact');
  for (const relationshipKind of ['extends','implements','overrides']) validate('TypeRelationshipFact',{...relationship,relationshipKind});
  for (const [value,field] of [
    [{...relationship,relationshipKind:null},'relationshipKind'],
    [{...relationship,relationshipKind:'typeRelationship'},'relationshipKind'],
    [{...relationship,kind:'extends'},'kind'],
    [{...relationship,source:{kind:'external',symbol:sample('SymbolKey')}},'source.kind'],
    [{...relationship,relationship:'extends'},'relationship'],
    [{...relationship,target:{...relationship.target,relationshipKind:'extends'}},'target.relationshipKind'],
    [{...relationship,target:{...relationship.target,kind:'overrides'}},'target.kind']
  ]) failure('TypeRelationshipFact',value,`TypeRelationshipFact.${field}`);
  const missing={...relationship}; delete missing.relationshipKind; failure('TypeRelationshipFact',missing,'TypeRelationshipFact.relationshipKind');
  assert.throws(() => parseJson('{"kind":"typeRelationship","relationshipKind":"extends","relationshipKind":"overrides"}'),/duplicate key relationshipKind/);
});
test('FORMAT.ENVELOPES: comparison, diagnostics, requested limits and exclusive answers',() => {
  const fixture=sample('FixtureV1'); validate('FixtureV1',fixture);
  for (const field of ['comparison','coverageIntents','anchorCasesFile']) { const v={...fixture}; delete v[field]; failure('FixtureV1',v,`FixtureV1.${field}`); }
  const records=sample('NormalizedRecordsV1'); validate('NormalizedRecordsV1',records);
  for (const field of ['comparison','referenceJoinDiagnostics']) {const v={...records};delete v[field];failure('NormalizedRecordsV1',v,`NormalizedRecordsV1.${field}`);}
  const requested=sample('AttemptedGraphRequest'); requested.depth=6;requested.maxNodes=0;requested.maxCalls=501;validate('AttemptedGraphRequest',requested);
  failure('GraphRequest',requested,'GraphRequest.request');
  for (const extra of ['cursor','pagination','filter']) failure('GraphRequest',{...sample('GraphRequest'),[extra]:1},`GraphRequest.${extra}`);
  validate('AnswersInputV1',sample('AnswersInputV1'));
  const bad=sample('Failure');failure('Failure',{...bad,result:sample('GraphResult')},'Failure.result');
  const diagnostic=sample('ReferenceJoinDiagnostic'); diagnostic.join.status='exact';failure('ReferenceJoinDiagnostic',diagnostic,'ReferenceJoinDiagnostic.join.status');
});
test('CANONICAL.INTAKE: duplicate-safe strict Unicode and integer tokens',() => {
  for (const bad of ['{"a":1,"a":2}','{"a":1,"\u0061":2}','-0','1.0','1e0','9007199254740992','"\ud800"','"\udc00"','{"a":1,}']) assert.throws(() => parseJson(bad));
  assert.throws(() => parseJson(Buffer.from([0xff])),/encoded|UTF-8/i);
  assert.deepEqual({...parseJson('{"a":[0,true,null,"é"]}')},{a:[0,true,null,'é']});
  assert.equal(canonicalBytes({z:'\n\t\0"\\\u2028\u2029',a:[0,true,null]}).toString(),'{"a":[0,true,null],"z":"\\u000a\\u0009\\u0000\\"\\\\\u2028\u2029"}');
  for (const value of [-0,1.5,Number.MAX_SAFE_INTEGER+1,'\ud800',{x:undefined}]) assert.throws(() => canonicalBytes(value));
});
