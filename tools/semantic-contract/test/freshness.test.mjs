import test from 'node:test';
import assert from 'node:assert/strict';
import {specimen} from './load.test.mjs';
import {loadFixture} from '../load.mjs';
import {contentHash} from '../identity.mjs';
import {checkCapturedBasis,checkFreshness,checkStaleTarget,expectedFreshness,expectedStaleTarget} from '../check-freshness.mjs';
const hash=x=>contentHash(Buffer.from(x));
async function prepared(t) {
 const s=await specimen();t.after(s.cleanup);
 const r1=await loadFixture(s.root), captured=r1.selected;
 const base={id:'proof1',producerId:'semantic',document:s.document,revisionId:'r1',contentHash:captured.documents[0].contentHash,evidenceKind:'semanticReference',basis:{producerId:'semantic',producerVersion:'1',producerHash:s.fixture.producers[1].executableHash,artifactHash:s.fixture.captures.find(x=>x.ref==='fact').hash,language:'javascript',sourceSetId:'main',revisionId:'r1',sourceManifestHash:r1.sourceManifestHash(captured),toolchainHash:captured.toolchainHash,configHash:captured.configHash,dependencyHash:captured.dependencyHash,lookupDependencies:[]},freshness:'fresh'};
 return {s,r1,base};
}
test('BASIS.* validates each captured descriptor and byte claim before freshness',async t=>{
 const {r1,base}=await prepared(t); assert.ok(checkCapturedBasis(base,r1));
 for(const field of Object.keys(base.basis)){
  if(field==='lookupDependencies') continue;
  const bad=structuredClone(base);bad.basis[field]=field.endsWith('Hash')?'0'.repeat(64):field==='language'?'rust':'changed';
  assert.throws(()=>checkFreshness(bad,r1),error=>error.assertion?.startsWith('BASIS.')===true,field);
 }
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
 const binding={callId:null,join:{anchor:{document:s.document,revisionId:'r1',contentHash:base.contentHash,range:{start:0,end:1},kind:'invocation'},status:'unmatched',candidateIds:[],diagnostic:null},resolution:'resolved',declaredTarget:{kind:'internal',syntaxId:'sid:v1:'+'a'.repeat(32),document:target,revisionId:'r1'},candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,staleTarget:false,provenanceId:'proof1'};
 assert.equal(checkStaleTarget(binding,{...base,basis:{...base.basis,sourceManifestHash:snapshot.sourceManifestHash(snapshot.selected)}},snapshot),false);
 const proof={...base,basis:{...base.basis,sourceManifestHash:snapshot.sourceManifestHash(snapshot.selected)}};
 s.fixture.revisions.push({...s.fixture.revisions[0],id:'r2',documents:s.fixture.revisions[0].documents.map(x=>({...x,revisionId:'r2',sourceFile:x.key.path==='src/target.js'?'r2/target.js':'r2/go.js'}))});
 s.files['r2/go.js']=s.source;s.files['r2/target.js']=s.files['src/target.js'];s.fixture.comparison.revisionId='r2';await s.flush();
 assert.equal(expectedStaleTarget(binding,proof,await loadFixture(s.root)),false);
 s.files['r2/target.js']='export const target = 2;\n';await s.flush();
 const changed=await loadFixture(s.root);
 assert.equal(expectedStaleTarget(binding,proof,changed),true);
 assert.throws(()=>checkStaleTarget(binding,proof,changed),{assertion:'TARGET_STALENESS.LABEL',field:'staleTarget'});
 assert.equal(checkStaleTarget({...binding,staleTarget:true},proof,changed),true);
});
