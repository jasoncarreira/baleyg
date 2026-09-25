import test from 'node:test';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {join} from 'node:path';
import {loadFixture} from '../load.mjs';
import {normalizeFixture} from '../normalize.mjs';
import {materializeAnswers} from '../answers.mjs';
import {checkAnswers,checkGraphAnswer} from '../graph-check.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {checkMeasurement} from '../record-check/measurement.mjs';
import {checkJoins} from '../record-check/joins.mjs';
import {checkRelationships} from '../record-check/relationships.mjs';
import {checkBindings} from '../record-check/bindings.mjs';
import {compareEnvelope,checkEnvelopeOrder,orderedEnvelope} from '../record-check/measurement.mjs';
import {registerControls,runControl} from './mutations.mjs';

const root=fileURLToPath(new URL('../../../tests/fixtures/semantic-evidence/v1/example/',import.meta.url));
const fixture=await loadFixture(root);
const normalized=normalizeFixture(fixture);
const {records}=normalized;
const answers=materializeAnswers(fixture.answers,normalized);
const coverage=checkCoverage(fixture,records);
const measurement=checkMeasurement(fixture,records);
const joins=checkJoins(fixture,records,coverage,measurement);
checkRelationships(fixture,records,coverage,measurement,joins);
checkBindings(fixture,records,coverage,measurement,joins);
const answer=id=>answers.answers.find(row=>row.id===id);

// Expected IDs and bytes are independently pinned from the literal source and
// #22 domain-separated canonical digest, not copied from a checker result.
test('example source bytes, immutable IDs and captured revision-local occurrences',()=>{
 assert.equal(records.declarations.find(row=>row.revisionId==='r2'&&row.name==='same').syntaxId,'sid:v1:2f420adfd3ee5d0fb3cc49cad3a76625');
 assert.equal(records.declarations.find(row=>row.revisionId==='r2'&&row.name==='change').syntaxId,'sid:v1:f2b6e998da22cfa687148e62362a00e4');
 assert.equal(records.calls.find(row=>row.revisionId==='r2').id,'occ:v1:56fac677947ade31ace83067fd89c569');
 assert.notEqual(records.calls.find(row=>row.revisionId==='r1').id,records.calls.find(row=>row.revisionId==='r2').id);
 assert.equal(fixture.revisions.get(JSON.stringify(['main','r1'])).documents.find(row=>row.key.path==='src/changed.js').contentHash,'0321caed7db4a2341355061cdb709b00a83226792a91288a85ecdca2270149d1');
 assert.equal(fixture.revisions.get(JSON.stringify(['main','r2'])).documents.find(row=>row.key.path==='src/changed.js').contentHash,'3d45199342e9522ccdccb92712de06e94fa823a9a4feb5c2aba7eb197a8518b2');
 assert.equal(records.coverage.length,45);
 assert.deepEqual(fixture.revisionChronology.get('main').map(row=>row.id),['r0','r1','r2']);
});

test('authored graph answers select latest declaration proofs, never old occurrences',()=>{
 for(const entry of answers.answers)assert.equal(checkGraphAnswer(fixture,records,coverage,entry),true,entry.id);
 const result=id=>answer(id).answer.result;
 assert.deepEqual(result('history-unchanged').provenance.filter(p=>p.producerId==='P').map(p=>p.id),['proof:latest-unchanged','proof:symbol-unchanged']);
 assert.deepEqual(result('history-changed').provenance.filter(p=>p.producerId==='P').map(p=>[p.id,p.freshness]),[['proof:latest-changed','stale'],['proof:relationship-changed','stale']]);
 assert.equal(result('history-unchanged').provenance.find(p=>p.id==='proof:latest-unchanged').freshness,'possiblyStale');
 assert.equal(result('history-zero').provenance.some(p=>p.id==='proof:old-zero'),false);
 assert.deepEqual(result('history-zero').coverage.filter(c=>c.producerId==='P').map(c=>[c.revisionId,c.state]),[['r1','partial'],['r2','failed']]);
 assert.equal(result('history-foreign').warnings.some(w=>w.code==='coverageIncomplete'),false);
 assert.equal(result('history-unchanged').edges[0].binding,null);
 assert.equal(records.references.some(ref=>ref.revisionId==='r2'),false);
 assert.deepEqual(records.callBindings.map(binding=>binding.join.anchor.revisionId).sort(),['r1','r2']);
 assert.deepEqual(result('history-foreign').nodes.map(node=>node.declaration.syntaxId),['sid:v1:e976f46fdd13c71c3616d596ff531f3e','sid:v1:c0834332f03e9a936f94aa462d15d1e2']);
 assert.equal(result('history-foreign').edges[0].to,'sid:v1:c0834332f03e9a936f94aa462d15d1e2');
 assert.equal(result('history-foreign').edges[0].visit,'new');
 assert.equal(result('history-foreign').edges[0].binding.provenanceId,'proof:fresh-call');
 assert.deepEqual(result('history-foreign').coverage.map(c=>[c.producerId,c.revisionId,c.state]),[['P','r2','complete'],['native','r2','complete']]);
 assert.deepEqual(result('history-foreign').warnings,[]);
 assert.equal(result('history-other-producer').provenance.some(p=>p.producerId==='Q'),false);
 assert.equal(records.provenance.some(p=>p.id==='proof:Q-anchor'&&p.producerId==='Q'&&p.revisionId==='r1'),true);
 assert.equal(records.declarationBindings.find(b=>b.provenanceId==='proof:Q-anchor').syntaxId,result('history-other-producer').nodes[0].declaration.syntaxId);
 assert.equal(records.provenance.find(p=>p.id==='proof:Q-anchor').document.path,'src/anchors.js');
 assert.equal(records.coverage.some(c=>c.producerId==='Q'&&c.documentPath==='src/anchors.js'&&c.revisionId==='r1'&&c.selected&&c.state==='complete'),true);
 assert.deepEqual(result('history-other-producer').coverage.map(c=>[c.producerId,c.revisionId,c.state]),[['P','r1','complete'],['P','r2','failed'],['native','r2','complete']]);
 assert.equal(result('history-other-producer').warnings[0].code,'coverageIncomplete');
 for(const id of ['old-call','old-reference','unreturned-hidden','foreign-only'])
  assert.equal(result('history-unchanged').provenance.some(proof=>proof.id===`proof:${id}`),false);
 assert.deepEqual(result('syntax-only').warnings.map(row=>row.code),['syntaxOnly']);
});

test('six authored anchor branches retain actual source declarations',()=>{
 assert.deepEqual(fixture.anchors.cases.map(row=>[row.id,row.expectedResult.reason]),[
  ['unique-unchanged-header-changed-sibling','none'],['duplicate-proven','none'],
  ['duplicate-unknown','unprovenContinuity'],['duplicate-changed','groupChanged'],
  ['missing','missing'],['header-mismatch','headerMismatch']]);
 assert.equal(records.durableAnchors.length,6);
});

test('durable anchor order uses captured revision and document without collapsing duplicates',()=>{
 const captured=records.durableAnchors[0];
 const older={...captured,capturedRevisionId:'r0'};
 const earlierDocument={...captured,document:{...captured.document,path:'src/aaa.js'}};
 assert.equal(compareEnvelope(older,captured)<0,true);
 assert.equal(compareEnvelope(captured,older)>0,true);
 assert.equal(compareEnvelope(earlierDocument,captured)<0,true);
 assert.deepEqual(orderedEnvelope('durableAnchors',[captured,older]),[older,captured]);
 assert.doesNotThrow(()=>checkEnvelopeOrder('durableAnchors',[older,captured]));
 assert.throws(()=>checkEnvelopeOrder('durableAnchors',[captured,older]),{
  assertion:'RECORDS.ORDER',code:'invalidRecord',field:'durableAnchors'
 });
 assert.throws(()=>orderedEnvelope('durableAnchors',[captured,captured],{collapseIdentical:true}),{
  assertion:'RECORDS.MEMBERSHIP',code:'invalidRecord',field:'durableAnchors'
 });
 const declaration=records.declarations.find(row=>row.revisionId==='r1'&&row.name==='same');
 const newer=records.declarations.find(row=>row.revisionId==='r2'&&row.name==='same');
 assert.equal(compareEnvelope(declaration,newer)<0,true);
});

function control(id,mutate,expectedAssertion,expectedField){
 return {id,baseline:()=>answer(id.split('/')[0]),mutate,check:entry=>checkGraphAnswer(fixture,records,coverage,entry),
  expectedAssertion,expectedCode:'invalidRecord',expectedField};
}
const controls=registerControls([
 control('history-zero/older-r0-fallback',entry=>{entry.answer.result.provenance.push(records.provenance.find(p=>p.id==='proof:old-zero'));return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-unchanged/unreturned-declaration',entry=>{entry.answer.result.provenance.push(records.provenance.find(p=>p.id==='proof:unreturned-hidden'));return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-unchanged/foreign-document',entry=>{entry.answer.result.provenance.push(records.provenance.find(p=>p.id==='proof:foreign-only'));return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-unchanged/old-call-binding',entry=>{entry.answer.result.edges[0].binding=records.callBindings[0];return entry},'GRAPH.TRAVERSAL','answers.history-unchanged.result.edges'),
 control('history-unchanged/old-reference-proof',entry=>{entry.answer.result.provenance.push(records.provenance.find(p=>p.id==='proof:old-reference'));return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-other-producer/Q-proof-in-P-answer',entry=>{entry.answer.result.provenance.push(records.provenance.find(p=>p.id==='proof:Q-anchor'));return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-unchanged/promoted-r1-proof-as-r2',entry=>{const proof=entry.answer.result.provenance.find(p=>p.id==='proof:latest-unchanged');Object.assign(proof,{revisionId:'r2',freshness:'fresh'});return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-zero/missing-zero-fact-coverage',entry=>{entry.answer.result.coverage=entry.answer.result.coverage.filter(row=>row.revisionId!=='r1');return entry},'GRAPH.COVERAGE','coverage'),
 control('history-changed/relabelled-stale',entry=>{entry.answer.result.provenance.find(p=>p.id==='proof:latest-changed').freshness='possiblyStale';return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-unchanged/relabelled-possibly-stale',entry=>{entry.answer.result.provenance.find(p=>p.id==='proof:latest-unchanged').freshness='fresh';return entry},'GRAPH.PROVENANCE','provenance'),
 control('history-foreign/forbidden-coverage-warning',entry=>{entry.answer.result.warnings.unshift({code:'coverageIncomplete',provenanceId:null,message:'coverageIncomplete'});return entry},'WARNING.KEYS','warnings')
]);
for(const row of controls)test(`negative input ${row.id} fails at ${row.expectedAssertion}`,()=>runControl(row));

test('complete real fixture check, including source-backed durable anchors',()=>{
 assert.equal(checkAnswers(fixture,records,coverage,answers),true);
});
