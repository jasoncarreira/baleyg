import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {loadFixture} from '../load.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {contentHash,sourceManifestHash} from '../identity.mjs';
import {canonicalBytes} from '../json.mjs';
import {registerControls,runControl} from './mutations.mjs';

const hash=text=>contentHash(Buffer.from(text));
const sort=rows=>rows.sort((a,b)=>Buffer.compare(canonicalBytes(a),canonicalBytes(b)));
const document=(path,language='javascript')=>({sourceSetId:'main',language,path});
const symbol={scheme:'scip',symbol:'pkg target',scope:'global',document:null};
const plans=[
 {state:'complete',requestedRoles:['read'],supportedRoles:['read'],observedRoles:['read'],requested:true,selected:true,diagnostic:null},
 {state:'partial',requestedRoles:['read','type'],supportedRoles:['read','type'],observedRoles:['read'],requested:true,selected:true,diagnostic:'some roles missing'},
 {state:'omitted',requestedRoles:['read'],supportedRoles:['read'],observedRoles:[],requested:true,selected:false,diagnostic:'deliberate omission'},
 {state:'unsupported',requestedRoles:['read'],supportedRoles:[],observedRoles:[],requested:true,selected:false,diagnostic:'unsupported'},
 {state:'failed',requestedRoles:['read'],supportedRoles:['read'],observedRoles:[],requested:true,selected:true,diagnostic:'capture failed'},
 {state:'notRequested',requestedRoles:[],supportedRoles:[],observedRoles:[],requested:false,selected:false,diagnostic:null}
];
async function specimen(t,{comparisonRevision="r2",sameCallerBytes=false,requestedChange=null,language="javascript"}={}){
 const root=await mkdtemp(join(tmpdir(),'coverage-u1-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const files=new Map(),put=(name,data)=>files.set(name,typeof data==='string'?data:JSON.stringify(data));
 const natives='native-executable', sem1='semantic-one',sem2='semantic-two';
 const producers=[['native','native',natives],['one','semantic',sem1],['two','semantic',sem2]].map(([id,kind,bytes])=>({id,version:'1',executableHash:hash(bytes),kind,languages:[language],positionEncoding:'utf8'}));
 const captures=[];
 const add=(ref,kind,path,bytes)=>{put(path,bytes);captures.push({ref,kind,file:path,hash:hash(bytes)});};
 for(const [i,bytes] of [natives,sem1,sem2].entries())add(`executable-${i}`,'executable',`captures/executable-${i}`,bytes);
 for(const [kind,text] of [['toolchain','tool'],['config','config'],['dependency','deps']])add(kind,kind,`captures/${kind}`,text);
 const docs=[document('a.js',language),document('b.js',language)],source=language==='java'?{r1:['class A {}\n','class B {}\n'],r2:['class A { int a; }\n','class B {}\n']}:{r1:['function a() {}\n','function b() {}\n'],r2:[sameCallerBytes?'function a() {}\n':'function a() { return 1; }\n',sameCallerBytes?'function b() { return 2; }\n':'function b() {}\n']};
 const revisions=['r1','r2'].map(revisionId=>({id:revisionId,sourceSetId:'main',documents:docs.map((key,i)=>{const sourceFile=`sources/${revisionId}/${key.path}`;put(sourceFile,source[revisionId][i]);return {key,revisionId,sourceFile};}),toolchainHash:hash('tool'),configHash:hash('config'),dependencyHash:hash('deps')}));
 const coverageIntents=[];const annotationFacts=new Map();
 let count=0;
 for(const revision of revisions)for(const doc of revision.documents){
  const annotationFile=`${doc.sourceFile}.annotations.json`;annotationFacts.set(annotationFile,[]);
  for(const producer of producers){
   const plan=producer.id==='one'&&revision.id==='r2'&&doc.key.path==='a.js'?plans[4]:plans[count%plans.length];count++;
   const record={producerId:producer.id,language:doc.key.language,sourceSetId:doc.key.sourceSetId,documentPath:doc.key.path,revisionId:revision.id,requested:plan.requested,selected:plan.selected,state:plan.state,supportedRoles:plan.supportedRoles,observedRoles:plan.observedRoles,diagnostic:plan.diagnostic};
   const ref=`coverage-${producer.id}-${revision.id}-${doc.key.path}`;
   annotationFacts.get(annotationFile).push({kind:'coverage',ref,record});
   coverageIntents.push({producerId:producer.id,document:doc.key,revisionId:revision.id,requestedRoles:plan.requestedRoles,measurementSupport:['declarationName','callee','invocation','reference'].map(kind=>({kind,available:true,diagnostic:null}))});
  }
 }
 const rawFacts=[];
 for(const producer of producers.slice(1)){
  const ref=`symbol-${producer.id}`,proofId=`proof-${producer.id}`,fact={kind:'symbol',ref,record:{key:symbol,displayName:'target',declarations:[],provenanceId:proofId}};
  rawFacts.push([producer,ref,proofId,fact]);
  const raw=JSON.stringify({formatVersion:1,producerId:producer.id,facts:[fact]});add(`artifact-${producer.id}`,'semanticArtifact',`captures/${producer.id}.json`,raw);
 }
 put('captures/native.json',{formatVersion:1,producerId:'native',declarations:[],calls:[],controls:[],references:[]});
 const historical=annotationFacts.get('sources/r1/a.js.annotations.json');
 const expectedProofs=[];
 for(const [producer,ref,id,fact] of rawFacts){
  const revision=revisions[0],doc=revision.documents[0].key;
  const basis={producerId:producer.id,producerVersion:producer.version,producerHash:producer.executableHash,artifactHash:captures.find(x=>x.ref===`artifact-${producer.id}`).hash,language,sourceSetId:'main',revisionId:'r1',sourceManifestHash:sourceManifestHash(revision.documents.map((x,i)=>({document:x.key,contentHash:hash(source.r1[i])}))),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash,lookupDependencies:['target']};
  const proof={id,producerId:producer.id,document:doc,revisionId:'r1',contentHash:hash(source.r1[0]),evidenceKind:'declarationBinding',basis,freshness:comparisonRevision==='r1'?(requestedChange&&producer.id==='one'?'possiblyStale':'fresh'):sameCallerBytes?'possiblyStale':'stale'};
  historical.push({kind:'provenance',ref:`fact-${id}`,record:proof},fact);
  expectedProofs.push(proof);
 }
 for(const [name,facts] of annotationFacts){const [revisionId,path]=name.split('/').slice(1);put(name,{formatVersion:1,document:document(path.replace('.annotations.json',''),language),revisionId,scenarios:[],facts});}
 put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 const requested=structuredClone(producers);
 if(requestedChange){
  if(requestedChange==='absent')requested.splice(1,1);
  if(requestedChange==='version')requested[1].version='2';
  if(requestedChange==='hash')requested[1].executableHash=hash('different executable');
  if(requestedChange==='encoding')requested[1].positionEncoding='utf16';
  if(requestedChange==='languages')requested[1].languages=['javascript','python'];
 }
 const fixture={formatVersion:1,profile:'example',language,sourceSets:[{id:'main',rootId:'root',languages:[language],dependencies:[]}],producers,revisions,comparison:{sourceSetId:'main',revisionId:comparisonRevision,producers:requested},coverageIntents,nativeArtifact:'captures/native.json',semanticArtifacts:rawFacts.map(x=>`captures/${x[0].id}.json`),annotationFiles:[...annotationFacts.keys()],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 put('fixture.json',fixture);
 for(const [name,value] of files){await mkdir(dirname(join(root,name)),{recursive:true});await writeFile(join(root,name),value);}
 const loaded=await loadFixture(root);
 const records={formatVersion:1,comparison:structuredClone(fixture.comparison),producers:sort(structuredClone(producers)),sourceSets:sort(structuredClone(fixture.sourceSets)),revisions:sort(revisions.map(revision=>({...revision,documents:revision.documents.map((item,i)=>({key:item.key,revisionId:revision.id,contentHash:hash(source[revision.id][i]),byteLength:Buffer.byteLength(source[revision.id][i])}))}))),coverage:sort([...annotationFacts.values()].flatMap(facts=>facts.filter(x=>x.kind==='coverage').map(x=>x.record))),provenance:sort(expectedProofs),declarations:[],symbols:[],declarationBindings:[],typeRelationships:[],calls:[],controlRegions:[],references:[],referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]};
 return {loaded,records,fixture,source,docs,proofs:expectedProofs};
}
let admitted;
function check({records}){return checkCoverage(admitted,records);}
function change(mutator){return specimenStub=>{mutator(specimenStub);return specimenStub;};}
const cases=[
 ['missing-coverage',x=>x.records.coverage.pop(),'COVERAGE.TUPLE','coverage'],
 ['extra-coverage',x=>x.records.coverage.push({...x.records.coverage[0],documentPath:'extra.js'}),'COVERAGE.TUPLE','coverage'],
 ['duplicate-coverage',x=>x.records.coverage.push({...x.records.coverage[0]}),'COVERAGE.TUPLE','coverage'],
 ['swapped-coverage-producer',x=>x.records.coverage[0].producerId='foreign','COVERAGE.TUPLE','coverage'],
 ['swapped-coverage-document',x=>x.records.coverage[0].documentPath='changed.js','COVERAGE.TUPLE','coverage'],
 ['swapped-coverage-revision',x=>x.records.coverage[0].revisionId='r3','COVERAGE.TUPLE','coverage'],
 ['swapped-coverage-set',x=>x.records.coverage[0].sourceSetId='other','COVERAGE.TUPLE','coverage'],
 ['requested-bit',x=>x.records.coverage.find(y=>y.state==='complete').requested=false,'COVERAGE.STATE','coverage'],
 ['selected-bit',x=>x.records.coverage.find(y=>y.state==='complete').selected=false,'COVERAGE.STATE','coverage'],
 ['diagnostic-bit',x=>x.records.coverage.find(y=>y.state==='complete').diagnostic='unexpected','COVERAGE.STATE','coverage'],
 ['state-bit',x=>x.records.coverage.find(y=>y.state==='partial').state='complete','COVERAGE.STATE','coverage'],
 ['role-order',x=>{const y=x.records.coverage.find(y=>y.state==='partial');y.supportedRoles.reverse();},'COVERAGE.ROLES','coverage.supportedRoles'],
 ['role-duplicate',x=>x.records.coverage.find(y=>y.state==='complete').observedRoles.push('read'),'COVERAGE.ROLES','coverage.observedRoles'],
 ['role-subset',x=>x.records.coverage.find(y=>y.state==='complete').observedRoles=['type'],'COVERAGE.ROLES','coverage.observedRoles'],
 ['identity-format',x=>x.records.formatVersion=2,'FORMAT.SHAPE','NormalizedRecordsV1.formatVersion'],
 ['identity-comparison',x=>x.records.comparison.revisionId='r1','RECORDS.IDENTITY','comparison'],
 ['identity-producer',x=>x.records.producers[0].version='changed','RECORDS.IDENTITY','producers'],
 ['identity-source-set',x=>x.records.sourceSets[0].rootId='changed','RECORDS.IDENTITY','sourceSets'],
 ['identity-revision',x=>x.records.revisions[0].dependencyHash=hash('changed'),'RECORDS.IDENTITY','revisions'],
 ['identity-document-hash',x=>x.records.revisions[0].documents[0].contentHash=hash('changed'),'RECORDS.IDENTITY','revisions'],
 ['identity-document-length',x=>x.records.revisions[0].documents[0].byteLength++,'RECORDS.IDENTITY','revisions'],
 ['roles-extra-supported',x=>x.records.coverage.find(y=>y.state==='complete').supportedRoles=['read','type'],'COVERAGE.ROLES','coverage.supportedRoles'],
 ['roles-unsupported-request',x=>x.records.coverage.find(y=>y.state==='partial').supportedRoles=['read'],'COVERAGE.ROLES','coverage.supportedRoles'],
 ['basis-producer-id',x=>x.records.provenance[0].basis.producerId='other','FRESHNESS.BASIS','basis.producerId'],
 ['basis-language',x=>x.records.provenance[0].basis.language='python','FRESHNESS.BASIS','basis.language'],
 ['basis-source-set',x=>x.records.provenance[0].basis.sourceSetId='other','FRESHNESS.BASIS','basis.sourceSetId'],
 ['basis-revision',x=>x.records.provenance[0].basis.revisionId='r2','FRESHNESS.BASIS','basis.revisionId'],
 ['basis-version',x=>x.records.provenance[0].basis.producerVersion='2','FRESHNESS.BASIS','basis.producerVersion'],
 ['basis-producer-hash',x=>x.records.provenance[0].basis.producerHash=hash('changed'),'FRESHNESS.BASIS','basis.producerHash'],
 ['basis-artifact-hash',x=>x.records.provenance[0].basis.artifactHash=hash('changed'),'FRESHNESS.BASIS','basis.artifactHash'],
 ['basis-manifest',x=>x.records.provenance[0].basis.sourceManifestHash=hash('changed'),'FRESHNESS.BASIS','basis.sourceManifestHash'],
 ['basis-toolchain',x=>x.records.provenance[0].basis.toolchainHash=hash('changed'),'FRESHNESS.BASIS','basis.toolchainHash'],
 ['basis-config',x=>x.records.provenance[0].basis.configHash=hash('changed'),'FRESHNESS.BASIS','basis.configHash'],
 ['basis-dependency',x=>x.records.provenance[0].basis.dependencyHash=hash('changed'),'FRESHNESS.BASIS','basis.dependencyHash'],
 ['basis-lookup',x=>x.records.provenance[0].basis.lookupDependencies=['z','a'],'FRESHNESS.BASIS','basis.lookupDependencies'],
 ['fresh-promotion',x=>x.records.provenance[0].freshness='fresh','FRESHNESS.STATE','freshness'],
];
const controls=registerControls(cases.map(([id,mutate,assertion,field])=>({id:`U1.${id}`,baseline:async()=>{throw new Error('set by test closure');},check,mutate:change(mutate),expectedAssertion:assertion,expectedCode:'invalidRecord',expectedField:field})));
test('source-derived three-producer, two-document, r1/r2 coverage and historical proofs',async t=>{
 const baseline=await specimen(t);admitted=baseline.loaded;const C=check(baseline);
 assert.equal(C.coverageByTuple.size,12);
 assert.equal(C.semanticProofsById.size,2);
 assert.equal(baseline.records.coverage.find(x=>x.producerId==='one'&&x.revisionId==='r2'&&x.documentPath==='a.js').state,'failed');
 assert.deepEqual([...new Set(baseline.records.coverage.map(x=>x.state))].sort(),plans.map(x=>x.state).sort());
 const proof=baseline.records.provenance[0];
 assert.equal(C.checkUse({producerId:proof.producerId,document:proof.document,revisionId:'r1',provenanceIds:[proof.id]}).proofs[0].freshness,'stale');
 for(const row of controls)row.baseline=()=>({records:baseline.records});
 for(const row of controls)await t.test(row.id,()=>runControl(row));
});

test('FRESHNESS.USE keeps captured historical tuple and rejects wrong producer, revision and missing proof',async t=>{
 const baseline=await specimen(t),C=checkCoverage(baseline.loaded,baseline.records),proof=baseline.records.provenance[0];
 const use={producerId:proof.producerId,document:proof.document,revisionId:proof.revisionId,provenanceIds:[proof.id]};
 assert.equal(C.checkUse(use).proofs[0].freshness,'stale');
 const rows=registerControls([
  {id:'U1.use-wrong-producer',mutate:x=>{x.producerId='native';return x;},expectedField:'provenanceIds'},
  {id:'U1.use-wrong-revision',mutate:x=>{x.revisionId='r2';return x;},expectedField:'provenanceIds'},
  {id:'U1.use-wrong-proof',mutate:x=>{x.provenanceIds=['unknown'];return x;},expectedField:'provenanceIds'},
  {id:'U1.use-missing-tuple',mutate:x=>{x.document.path='absent.js';return x;},expectedField:'coverage'},
 ].map(row=>({...row,baseline:()=>use,check:x=>C.checkUse(x),expectedAssertion:'FRESHNESS.USE',expectedCode:'invalidRecord'})));
 for(const row of rows)await t.test(row.id,()=>runControl(row));
});
test('FRESHNESS.TARGET verifies captured declaration and independent current bytes',async t=>{
 const baseline=await specimen(t),C=checkCoverage(baseline.loaded,baseline.records),proof=baseline.records.provenance[0];
 const syntaxId=`sid:v1:${'a'.repeat(32)}`;
 const target={kind:'internal',syntaxId,document:baseline.docs[0],revisionId:'r1'};
 const anchor={document:baseline.docs[0],revisionId:'r1',contentHash:proof.contentHash,range:{start:0,end:1},kind:'callee'};
 const binding={callId:null,join:{anchor,status:'unmatched',candidateIds:[],diagnostic:'unmatched'},resolution:'resolved',declaredTarget:target,candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,staleTarget:true,provenanceId:proof.id};
 const declarations=new Map([['["main","r1"]',new Map([[syntaxId,target.document]])]]);
 assert.equal(C.checkTarget(binding,proof,declarations),true);
 const rows=registerControls([
  {id:'U1.target-false-fresh',mutate:x=>{x.binding.staleTarget=false;return x;},expectedField:'staleTarget'},
  {id:'U1.target-missing-captured',mutate:x=>{x.declarations=new Map();return x;},expectedField:'declaredTarget'},
  {id:'U1.target-wrong-proof',mutate:x=>{x.binding.provenanceId='unknown';return x;},expectedField:'provenanceId'},
 ].map(row=>({...row,baseline:()=>({binding,declarations}),check:x=>C.checkTarget(x.binding,proof,x.declarations),expectedAssertion:'FRESHNESS.TARGET',expectedCode:'invalidRecord'})));
 for(const row of rows)await t.test(row.id,()=>runControl(row));
 const external={...binding,declaredTarget:null,staleTarget:null,resolution:'unresolved'};
 assert.equal(C.checkTarget(external,proof,declarations),null);
});

test('identical caller bytes with changed revision or manifest are possiblyStale, not fresh',async t=>{
 const baseline=await specimen(t,{sameCallerBytes:true}),C=checkCoverage(baseline.loaded,baseline.records);
 assert.equal(C.semanticProofsById.size,2);
 assert.equal(baseline.records.provenance[0].freshness,'possiblyStale');
 const first=baseline.records.provenance[0];
 assert.equal(C.checkUse({producerId:first.producerId,document:first.document,revisionId:'r1',provenanceIds:[first.id]}).proofs[0].freshness,'possiblyStale');
 const promoted={...baseline,records:structuredClone(baseline.records)};promoted.records.provenance[0].freshness='fresh';
 assert.throws(()=>checkCoverage(promoted.loaded,promoted.records),e=>e.assertion==='FRESHNESS.STATE'&&e.field==='freshness');
});
test('captured semantic proof is fresh when every requested component matches',async t=>{
 const baseline=await specimen(t,{comparisonRevision:'r1'}),C=checkCoverage(baseline.loaded,baseline.records);
 assert.equal(baseline.records.provenance[0].freshness,'fresh');
 const proof=baseline.records.provenance[0];
 assert.equal(C.checkUse({producerId:proof.producerId,document:proof.document,revisionId:'r1',provenanceIds:[proof.id]}).proofs[0].freshness,'fresh');
});

test('fresh caller and target staleness are independent, including missing current declaration',async t=>{
 const baseline=await specimen(t,{sameCallerBytes:true}),doc=baseline.docs[0];
 const caller={id:`native:r2:sid:v1:${'a'.repeat(32)}`,producerId:'native',document:doc,revisionId:'r2',contentHash:hash(baseline.source.r2[0]),evidenceKind:'measuredSyntax',basis:null,freshness:'fresh'};
 baseline.records.provenance.push(caller);
 const C=checkCoverage(baseline.loaded,baseline.records);
 const syntaxId=`sid:v1:${'b'.repeat(32)}`,historical=baseline.docs[1],target={kind:'internal',syntaxId,document:historical,revisionId:'r1'};
 const binding={callId:null,join:{anchor:{document:doc,revisionId:'r2',contentHash:caller.contentHash,range:{start:0,end:1},kind:'callee'},status:'unmatched',candidateIds:[],diagnostic:'unmatched'},resolution:'resolved',declaredTarget:target,candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,staleTarget:true,provenanceId:caller.id};
 const declarations=new Map([['["main","r1"]',new Map([[syntaxId,historical]])],['["main","r2"]',new Map([[syntaxId,historical]])]]);
 assert.equal(C.checkTarget(binding,caller,declarations),true);
 declarations.get('["main","r2"]').delete(syntaxId);
 assert.equal(C.checkTarget(binding,caller,declarations),true);
 const sameTarget={...binding,declaredTarget:{...target,document:doc,revisionId:'r2'},staleTarget:false};
 declarations.get('["main","r2"]').set(syntaxId,doc);
 assert.equal(C.checkTarget(sameTarget,caller,declarations),false);
});

test('requested producer components do not fall back to captured identity',async t=>{
 for(const change of ['absent','version','hash','encoding','languages'])await t.test(change,async t=>{
  const baseline=await specimen(t,{comparisonRevision:'r1',requestedChange:change});
  const C=checkCoverage(baseline.loaded,baseline.records);
  const proof=baseline.records.provenance.find(x=>x.producerId==='one');
  assert.equal(proof.freshness,'possiblyStale');
  assert.equal(C.checkUse({producerId:'one',document:proof.document,revisionId:'r1',provenanceIds:[proof.id]}).proofs[0].freshness,'possiblyStale');
  const promoted=structuredClone(baseline.records);promoted.provenance.find(x=>x.id===proof.id).freshness='fresh';
  assert.throws(()=>checkCoverage(baseline.loaded,promoted),e=>e.assertion==='FRESHNESS.STATE'&&e.code==='invalidRecord'&&e.field==='freshness');
 });
});

test('role-family support cannot be replaced by a coverage label',async t=>{
 const baseline=await specimen(t);
 const validateFamily=x=>checkCoverage({...baseline.loaded,fixture:{...baseline.loaded.fixture,coverageIntents:x.intents}},x.records);
 const rows=registerControls([
  {id:'U1.reference-family-unavailable',mutate:x=>{const intent=x.intents.find(y=>y.producerId==='native'&&y.revisionId==='r1'&&y.document.path==='a.js');intent.measurementSupport.find(y=>y.kind==='reference').available=false;intent.measurementSupport.find(y=>y.kind==='reference').diagnostic='unavailable';return x;},expectedAssertion:'COVERAGE.ROLES',expectedField:'coverage.observedRoles'},
  {id:'U1.requested-role-swap',mutate:x=>{const intent=x.intents.find(y=>y.producerId==='native'&&y.revisionId==='r1'&&y.document.path==='a.js');intent.requestedRoles=['type'];return x;},expectedAssertion:'COVERAGE.ROLES',expectedField:'coverage.observedRoles'},
 ].map(row=>({...row,baseline:()=>({records:baseline.records,intents:baseline.fixture.coverageIntents}),check:validateFamily,expectedCode:'invalidRecord'})));
 for(const row of rows)await t.test(row.id,()=>runControl(row));
});

test('Java alias is inapplicable even when a coverage claim names it',async t=>{
 const baseline=await specimen(t,{language:'java'});
 assert.equal(checkCoverage(baseline.loaded,baseline.records).coverageByTuple.size,12);
 const control=registerControls([{id:'U1.java-alias',baseline:()=>({records:baseline.records}),check:x=>checkCoverage(baseline.loaded,x.records),mutate:x=>{x.records.coverage.find(y=>y.state==='complete').supportedRoles=['read','alias'];return x;},expectedAssertion:'COVERAGE.ROLES',expectedCode:'invalidRecord',expectedField:'coverage.supportedRoles'}])[0];
 await runControl(control);
});
