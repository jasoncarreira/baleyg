import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {deriveWarnings,checkWarnings} from '../graph-warnings.mjs';
import {registerControls,runControl} from './mutations.mjs';

function specimen(){
 const binding={provenanceId:'binding-z',staleTarget:true,resolution:'ambiguous'};
 return {request:{semanticProducerId:'P'},coverage:[{selected:true,state:'failed'},{selected:true,state:'partial'},
  {selected:false,state:'omitted'}],provenance:[{id:'binding-z',freshness:'fresh'},{id:'stale-z',freshness:'stale'},
  {id:'stale-a',freshness:'stale'},...['p1','p2','p3'].map(id=>({id,freshness:'possiblyStale'}))],
  edges:[{binding},{binding}],frontier:[{reason:'callLimit'}],partial:true,truncated:true,warnings:[]};
}
const keys=warnings=>warnings.map(x=>[x.code,x.provenanceId]);
test('all warnings-v1 keys use enum ordering, stale aggregation and shared-edge dedup',()=>{
 const s=specimen();s.warnings=deriveWarnings(s);
 assert.deepEqual(keys(s.warnings),[['coverageIncomplete',null],['staleEvidence',null],['staleEvidence','stale-a'],
  ['staleEvidence','stale-z'],['staleTarget','binding-z'],['bindingAmbiguous','binding-z']]);
 assert.equal(checkWarnings(s),true);
 // Message text is independent of warning membership; Unicode is preserved.
 s.warnings.forEach((row,i)=>row.message=`Unicode 🦊 ${i}`);assert.equal(checkWarnings(s),true);
});
test('unselected omitted, limits and boundary do not imply coverage warning; syntax-only is exact',()=>{
 const s=specimen();s.coverage=[{state:'omitted',selected:false}];s.provenance=[];s.edges=[{binding:null,boundaryReason:'missingEvidence'}];
 s.warnings=[];assert.equal(checkWarnings(s),true);assert.deepEqual(deriveWarnings(s),[]);
 s.request.semanticProducerId=null;s.warnings=deriveWarnings(s);assert.deepEqual(keys(s.warnings),[['syntaxOnly',null]]);
});
test('forbidden, missing, duplicate and out-of-order keys fail exact checker',()=>{
 const s=specimen();s.warnings=deriveWarnings(s);
 const variants=[s.warnings.slice(1),[...s.warnings,{code:'syntaxOnly',provenanceId:null,message:'extra'}],
  [...s.warnings,s.warnings[0]],[...s.warnings].reverse()];
 for(const warnings of variants)assert.throws(()=>checkWarnings({...s,warnings}),{code:'invalidRecord'});
 assert.throws(()=>checkWarnings({...s,warnings:[{...s.warnings[0],message:''},...s.warnings.slice(1)]}),
  {assertion:'WARNING.SHAPE',field:'warnings[0]'});
});
const controls=registerControls([{id:'WARNING.selectedOnly',baseline:()=>({source:readFileSync(new URL('../graph-warnings.mjs',import.meta.url),'utf8')}),
 mutate:input=>{const old="row.selected&&['failed','partial'].includes(row.state)";assert.equal(input.source.split(old).length,2);
  input.source=input.source.replace(old,"['failed','partial','omitted'].includes(row.state)");return input;},
 check:async input=>{
  const source=input.source.replace("from './schema.mjs'",`from '${new URL('../schema.mjs',import.meta.url).href}'`)
   .replace("from './json.mjs'",`from '${new URL('../json.mjs',import.meta.url).href}'`);
  const {deriveWarnings:derived}=await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
  const s=specimen();s.coverage=[{selected:false,state:'omitted'}];s.provenance=[];s.edges=[];
  const actual=keys(derived(s));if(actual.length){const error=new Error('WARNING.KEYS warnings: unselected omitted created an incomplete warning');
   Object.assign(error,{assertion:'WARNING.KEYS',code:'invalidRecord',field:'warnings'});throw error;}return true;
 },expectedAssertion:'WARNING.KEYS',expectedCode:'invalidRecord',expectedField:'warnings'}]);
for(const row of controls)test(`source baseline → one production mutation → assertion: ${row.id}`,async()=>assert.equal(await runControl(row),row.id));
