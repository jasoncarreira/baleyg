import test from 'node:test';
import assert from 'node:assert/strict';
import {specimen} from './load.test.mjs';
import {loadFixture} from '../load.mjs';
import {contentHash} from '../identity.mjs';
import {writeFile} from 'node:fs/promises';
import {join} from 'node:path';
import {checkCapturedBasis,checkFreshness,checkStaleTarget,expectedFreshness,expectedStaleTarget} from '../check-freshness.mjs';
const hash=x=>contentHash(Buffer.from(x));
async function prepared(t) {
 const s=await specimen();t.after(s.cleanup);
 const initial=await loadFixture(s.root), captured=initial.selected;
 const base={id:'proof1',producerId:'semantic',document:s.document,revisionId:'r1',contentHash:captured.documents[0].contentHash,evidenceKind:'semanticReference',basis:{producerId:'semantic',producerVersion:'1',producerHash:s.fixture.producers[1].executableHash,artifactHash:s.fixture.captures.find(x=>x.ref==='fact').hash,language:'javascript',sourceSetId:'main',revisionId:'r1',sourceManifestHash:initial.sourceManifestHash(captured),toolchainHash:captured.toolchainHash,configHash:captured.configHash,dependencyHash:captured.dependencyHash,lookupDependencies:[]},freshness:'fresh'};
 const raw={formatVersion:1,producerId:'semantic',facts:[{kind:'provenance',ref:'proof',record:{...structuredClone(base),basis:{...base.basis,artifactHash:'0'.repeat(64)}}}]};
 const encoded=JSON.stringify(raw);s.files['captures/fact.json']=encoded;
 const artifactHash=hash(encoded);s.fixture.captures.find(x=>x.ref==='fact').hash=artifactHash;base.basis.artifactHash=artifactHash;
 s.files['src/go.js.annotations.json']=JSON.stringify({formatVersion:1,document:s.document,revisionId:'r1',scenarios:[],facts:[{kind:'provenance',ref:'proof',record:base}]});
 await s.flush();const r1=await loadFixture(s.root);
 return {s,r1,base};
}
test('BASIS.* validates each captured descriptor and byte claim before freshness',async t=>{
 const {r1,base}=await prepared(t); assert.ok(checkCapturedBasis(base,r1));
 for(const field of Object.keys(base.basis)){
  if(field==='lookupDependencies') continue;
  const bad=structuredClone(base);bad.basis[field]=field.endsWith('Hash')?'0'.repeat(64):field==='language'?'rust':'changed';
  assert.throws(()=>checkFreshness(bad,r1),error=>error.assertion?.startsWith('BASIS.')===true,field);
 }
 const invalidLookup=structuredClone(base);invalidLookup.basis.lookupDependencies=['sid:v1:'+'a'.repeat(32)];
 assert.throws(()=>checkCapturedBasis(invalidLookup,r1),{assertion:'BASIS.LOOKUP_DEPENDENCIES'});
 const absent=structuredClone(base);absent.basis=null;
 assert.throws(()=>checkFreshness(absent,r1),{assertion:'BASIS.SEMANTIC_PAIRING'});
 const native={...base,producerId:'native',evidenceKind:'measuredSyntax',basis:null};
 assert.equal(checkFreshness(native,r1),'fresh');
 assert.throws(()=>checkCapturedBasis({...native,basis:base.basis},r1),{assertion:'BASIS.NATIVE_PAIRING'});
});
test('FRESHNESS.TWO_REVISION_CONTEXT retains r1 proof but compares pinned r2 snapshot',async t=>{
 const {s,r1,base}=await prepared(t);assert.equal(checkFreshness(base,r1),'fresh');
 s.fixture.revisions.push({...s.fixture.revisions[0],id:'r2',documents:[{key:s.document,revisionId:'r2',sourceFile:'r2/go.js'}]});
 s.files['r2/go.js']=s.source;s.fixture.comparison.revisionId='r2';await s.flush();
 const r2=await loadFixture(s.root);
 assert.equal(expectedFreshness(base,r2),'possiblyStale');
 assert.throws(()=>checkFreshness(base,r2),{assertion:'FRESHNESS.LABEL',field:'freshness'});
 const historical={...base,freshness:'possiblyStale'};
 assert.equal(checkFreshness(historical,r2),'possiblyStale');
 s.files['r2/go.js']='export function go() { return 2; }\n';await s.flush();
 const changed=await loadFixture(s.root);
 assert.equal(expectedFreshness(base,changed),'stale');
 assert.equal(checkFreshness({...base,freshness:'stale'},changed),'stale');
 assert.throws(()=>checkFreshness(historical,changed),{assertion:'FRESHNESS.LABEL'});
});
test('FRESHNESS.COMPONENTS requested producer, manifest, toolchain and config cannot borrow capture',async t=>{
 const {s,base}=await prepared(t);
 s.fixture.comparison.producers=[];await s.flush();
 assert.equal(expectedFreshness(base,await loadFixture(s.root)),'possiblyStale');
 s.fixture.comparison.producers=structuredClone(s.fixture.producers);
 s.fixture.comparison.producers[1].version='2';await s.flush();
 assert.equal(expectedFreshness(base,await loadFixture(s.root)),'possiblyStale');
 s.fixture.comparison.producers[1].version='1';
 s.fixture.revisions.push({...s.fixture.revisions[0],id:'r2',documents:[{key:s.document,revisionId:'r2',sourceFile:'r2/go.js'}]});
 s.files['r2/go.js']=s.source;s.fixture.comparison.revisionId='r2';
 for(const field of ['configHash','dependencyHash','toolchainHash']){
  const old=s.fixture.revisions[0][field];const kind=field==='dependencyHash'?'dependency':field.slice(0,-4).toLowerCase();
  const file=`captures/updated-${field}.txt`,bytes=`updated-${field}`;
  s.files[file]=bytes;s.fixture.captures.push({ref:`updated-${field}`,kind,file,hash:hash(bytes)});
  s.fixture.revisions[1][field]=hash(bytes);await s.flush();
  assert.equal(expectedFreshness(base,await loadFixture(s.root)),'possiblyStale',field);
  s.fixture.revisions[1][field]=old;
 }
});
test('TARGET_STALENESS.TWO_REVISION_CONTEXT checks captured target bytes, not reused r1 label',async t=>{
 const {s,r1,base}=await prepared(t);
 const target={sourceSetId:'main',language:'javascript',path:'src/target.js'};
 s.files['src/target.js']='export const target = 1;\n';
 s.fixture.revisions[0].documents.push({key:target,revisionId:'r1',sourceFile:'src/target.js'});
 await s.flush();const snapshot=await loadFixture(s.root);
 const declarations=new Map([[JSON.stringify(['main','r1']),new Map([['sid:v1:'+'a'.repeat(32),target]])],[JSON.stringify(['main','r2']),new Map([['sid:v1:'+'a'.repeat(32),target]])]]);
 const binding={callId:null,join:{anchor:{document:s.document,revisionId:'r1',contentHash:base.contentHash,range:{start:0,end:1},kind:'invocation'},status:'unmatched',candidateIds:[],diagnostic:null},resolution:'resolved',declaredTarget:{kind:'internal',syntaxId:'sid:v1:'+'a'.repeat(32),document:target,revisionId:'r1'},candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,staleTarget:false,provenanceId:'proof1'};
 assert.equal(checkStaleTarget(binding,{...base,producerId:'native',evidenceKind:'measuredSyntax',basis:null},snapshot,declarations),false);
 const proof={...base,producerId:'native',evidenceKind:'measuredSyntax',basis:null};
 s.fixture.revisions.push({...s.fixture.revisions[0],id:'r2',documents:s.fixture.revisions[0].documents.map(x=>({...x,revisionId:'r2',sourceFile:x.key.path==='src/target.js'?'r2/target.js':'r2/go.js'}))});
 s.files['r2/go.js']=s.source;s.files['r2/target.js']=s.files['src/target.js'];s.fixture.comparison.revisionId='r2';await s.flush();
 assert.equal(expectedStaleTarget(binding,proof,await loadFixture(s.root),declarations),false);
 s.files['r2/target.js']='export const target = 2;\n';await s.flush();
 const changed=await loadFixture(s.root);
 assert.equal(expectedStaleTarget(binding,proof,changed,declarations),true);
 assert.throws(()=>checkStaleTarget(binding,proof,changed,declarations),{assertion:'TARGET_STALENESS.LABEL',field:'staleTarget'});
 assert.equal(checkStaleTarget({...binding,staleTarget:true},proof,changed,declarations),true);
});

test('FRESHNESS.REQUESTED_DESCRIPTOR compares independently without requiring requested executable',async t=>{
 const {s,base}=await prepared(t);
 for(const mutate of [
  x=>x.version='changed',x=>x.executableHash='f'.repeat(64),x=>x.positionEncoding='utf16',x=>x.languages=['javascript','rust']]){
  s.fixture.comparison.producers=structuredClone(s.fixture.producers);mutate(s.fixture.comparison.producers[1]);await s.flush();
  const loaded=await loadFixture(s.root);assert.equal(expectedFreshness(base,loaded),'possiblyStale');
  assert.equal(checkFreshness({...base,freshness:'possiblyStale'},loaded),'possiblyStale');
  assert.throws(()=>checkFreshness(base,loaded),{assertion:'FRESHNESS.LABEL'});
 }
 s.fixture.comparison.producers=[];await s.flush();assert.equal(expectedFreshness(base,await loadFixture(s.root)),'possiblyStale');
});
test('FRESHNESS.SOURCE_SET treats identical caller bytes on another admitted set as possiblyStale',async t=>{
 const {s,base}=await prepared(t);s.fixture.sourceSets.push({...s.fixture.sourceSets[0],id:'other'});
 s.fixture.revisions.push({...s.fixture.revisions[0],sourceSetId:'other',documents:[{key:{...s.document,sourceSetId:'other'},revisionId:'r1',sourceFile:'other/go.js'}]});
 s.files['other/go.js']=s.source;s.fixture.comparison.sourceSetId='other';await s.flush();
 const loaded=await loadFixture(s.root);assert.equal(expectedFreshness(base,loaded),'possiblyStale');
 assert.equal(checkFreshness({...base,freshness:'possiblyStale'},loaded),'possiblyStale');
});
test('BASIS.RAW_FACT rejects missing and contradictory captured semantic proof',async t=>{
 const {s,base}=await prepared(t);
 const artifact=JSON.parse(s.files['captures/fact.json']);artifact.facts=[];
 s.files['captures/fact.json']=JSON.stringify(artifact);s.fixture.captures.find(x=>x.ref==='fact').hash=hash(s.files['captures/fact.json']);await s.flush();
 await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.SEMANTIC'});
 const loaded=await loadFixture((await prepared(t)).s.root);
 assert.throws(()=>checkCapturedBasis({...base,id:'fabricated'},loaded),{assertion:'BASIS.RAW_FACT'});
});
test('TARGET_STALENESS requires verified declaration in selected snapshot despite identical bytes',async t=>{
 const {s,base}=await prepared(t);
 const id='sid:v1:'+'a'.repeat(32),target={sourceSetId:'main',language:'javascript',path:'src/target.js'};
 s.files['src/target.js']='target';s.fixture.revisions[0].documents.push({key:target,revisionId:'r1',sourceFile:'src/target.js'});
 await s.flush();const loaded=await loadFixture(s.root);
 const proof={...base,producerId:'native',evidenceKind:'measuredSyntax',basis:null};
 const binding={callId:null,join:{anchor:{document:s.document,revisionId:'r1',contentHash:base.contentHash,range:{start:0,end:1},kind:'invocation'},status:'unmatched',candidateIds:[],diagnostic:null},resolution:'resolved',declaredTarget:{kind:'internal',syntaxId:id,document:{path:'src/target.js',language:'javascript',sourceSetId:'main'},revisionId:'r1'},candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,staleTarget:false,provenanceId:'proof1'};
 const declarations=new Map([[JSON.stringify(['main','r1']),new Map([[id,target]])]]);
 assert.equal(expectedStaleTarget(binding,proof,loaded,declarations),false);
 assert.throws(()=>expectedStaleTarget(binding,proof,loaded),{assertion:'TARGET_STALENESS.DECLARATIONS'});
 s.fixture.revisions.push({...s.fixture.revisions[0],id:'r2',documents:s.fixture.revisions[0].documents.map(x=>({...x,revisionId:'r2',sourceFile:x.key.path==='src/target.js'?'r2/target.js':'r2/go.js'}))});
 s.files['r2/go.js']=s.source;s.files['r2/target.js']='target';s.fixture.comparison.revisionId='r2';await s.flush();
 const selected=await loadFixture(s.root);
 assert.equal(expectedStaleTarget(binding,proof,selected,declarations),true);
 assert.throws(()=>checkStaleTarget(binding,proof,selected,declarations),{assertion:'TARGET_STALENESS.LABEL'});
 declarations.get(JSON.stringify(['main','r1'])).clear();
 assert.throws(()=>expectedStaleTarget(binding,proof,selected,declarations),{assertion:'TARGET_STALENESS.CAPTURED'});
});
