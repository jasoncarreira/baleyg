import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {loadFixture} from '../load.mjs';
import {normalizeFixture} from '../normalize.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {checkMeasurement} from '../record-check/measurement.mjs';
import {checkJoins} from '../record-check/joins.mjs';
import {checkBindings} from '../record-check/bindings.mjs';
import {deriveWarnings,checkWarnings} from '../graph-warnings.mjs';
import {traverseGraph} from '../graph-traversal.mjs';
import {contentHash,sourceManifestHash,syntaxId,occurrenceId} from '../identity.mjs';
import {canonicalBytes} from '../json.mjs';
import {selectGraphEvidence,checkGraphEvidence} from '../graph-evidence.mjs';
import {registerControls,runControl} from './mutations.mjs';

const doc=path=>({sourceSetId:'main',language:'javascript',path});
const coverage=(producerId,document,revisionId,state)=>({producerId,sourceSetId:document.sourceSetId,language:document.language,
 documentPath:document.path,revisionId,requested:state!=='notRequested',selected:['failed','partial','complete'].includes(state),state,
 supportedRoles:[],observedRoles:[],diagnostic:state==='complete'?null:'diagnostic'});
const proof=(id,producerId,document,revisionId,freshness,contentHash)=>({id,producerId,document,revisionId,
 contentHash,evidenceKind:producerId==='N'?'measuredSyntax':'declarationBinding',basis:producerId==='N'?null:{artifactHash:'captured'},freshness});
const sid='sid:v1:11111111111111111111111111111111';
function specimen({changed=false,previousState='complete',currentState='failed',zero=false}={}){
 const D=doc('src/main.js'),F=doc('src/foreign.js'),source=hash=>({key:D,contentHash:hash}),
  revisions=['r0','r1','r2'].map((id,i)=>({id,sourceSetId:'main',documents:[source(i===2&&changed?'changed':'same')]}));
 const coverageRows=[...revisions.flatMap(rev=>[
  coverage('N',D,rev.id,'complete'),coverage('P',D,rev.id,rev.id==='r2'?currentState:rev.id==='r1'?previousState:'complete'),
  coverage('Q',D,rev.id,'complete'),coverage('P',F,rev.id,'complete')])];
 const decl={syntaxId:sid,document:D,revisionId:'r2',provenanceId:'native:r2:declaration'};
 const call={id:'occ:v1:11111111111111111111111111111111',document:D,revisionId:'r2',provenanceId:'native:r2:call'};
 const proofs=[proof(decl.provenanceId,'N',D,'r2','fresh',changed?'changed':'same'),
  proof(call.provenanceId,'N',D,'r2','fresh',changed?'changed':'same')];
 const facts=[],annotations=[],semanticProofs=new Map(),declarationBindings=[],symbols=[],typeRelationships=[];
 function add(revisionId,id,kind,sourceId=sid,document=D,producerId='P'){
  const provenanceId=`proof:${id}`,record={provenanceId};
  const fact=kind==='callBinding'?{kind,ref:id,record}:kind==='typeRelationship'?{kind,ref:id,source:{kind:'internal',declarationRef:id,revisionId},target:{kind:'external',symbol:{symbol:'other'}},provenanceRef:provenanceId}:
   {kind,ref:id,record};
  const normalized=kind==='symbol'?{provenanceId,declarations:[{kind:'internal',syntaxId:sourceId,document,revisionId}]}:
   kind==='typeRelationship'?{provenanceId,source:{kind:'internal',syntaxId:sourceId,document,revisionId}}:
   {provenanceId,syntaxId:sourceId};
  if(kind!=='callBinding')({declarationBinding:declarationBindings,symbol:symbols,typeRelationship:typeRelationships})[kind].push(normalized);
  proofs.push({...proof(provenanceId,producerId,document,revisionId,revisionId==='r2'?'fresh':changed?'stale':'possiblyStale','same'),
   evidenceKind:kind==='callBinding'?'semanticReference':kind==='typeRelationship'?'typeRelationship':'declarationBinding'});
  annotations.push({document,revisionId,facts:[fact]});
  semanticProofs.set(provenanceId,{fact,hash:'captured',factRef:id,factKind:kind});return provenanceId;
 }
 add('r0','older','declarationBinding');
 if(!zero){add('r1','binding','declarationBinding');add('r1','symbol','symbol');add('r1','relationship','typeRelationship');}
 add('r1','unreturned','declarationBinding','sid:v1:22222222222222222222222222222222');
 add('r1','foreign','declarationBinding',sid,F);
 add('r1','other-producer','declarationBinding',sid,D,'Q');
 if(currentState==='complete'||currentState==='partial')add('r2','current','declarationBinding');
 const records={coverage:coverageRows,provenance:proofs,declarationBindings,symbols,typeRelationships};
 const loaded={native:{producerId:'N'},annotations,semanticProofs,
  revisionChronology:new Map([['main',revisions]])};
 const checked={checkUse:({producerId,document,revisionId,provenanceIds})=>{
  const row=coverageRows.find(x=>x.producerId===producerId&&x.documentPath===document.path&&x.revisionId===revisionId);
  assert.ok(row,'captured tuple required');for(const id of provenanceIds)assert.ok(proofs.some(x=>x.id===id&&x.producerId===producerId&&x.revisionId===revisionId&&x.document.path===document.path));
 }};
 const result={request:{sourceSetId:'main',revisionId:'r2',semanticProducerId:'P'},nodes:[{declaration:decl,depth:0}],edges:[{call,from:sid,to:null,binding:null,visit:'boundary',boundaryReason:'missingEvidence'}],frontier:[]};
 return {loaded,records,checked,result,add,D,F};
}
const select=s=>selectGraphEvidence(s.loaded,s.records,s.checked,s.result);
test('failed refresh selects latest declaration proofs only, plus zero-fact coverage and r2 measured call',()=>{
 const s=specimen(),out=select(s);
 assert.deepEqual(out.coverage.map(x=>[x.producerId,x.revisionId,x.documentPath]),[['N','r2','src/main.js'],['P','r1','src/main.js'],['P','r2','src/main.js']]);
 assert.deepEqual(out.provenance.map(x=>x.id),['native:r2:call','native:r2:declaration','proof:binding','proof:relationship','proof:symbol']);
 assert.equal(out.selectedCoverageIncomplete,true);assert.equal(out.provenance.some(x=>x.revisionId==='r0'),false);
 assert.equal(out.provenance.some(x=>x.id==='proof:unreturned'||x.id==='proof:foreign'||x.id==='proof:other-producer'),false);
 assert.ok(out.provenance.filter(x=>x.producerId==='P').every(x=>x.freshness==='possiblyStale'));
 const zero=select(specimen({zero:true}));assert.deepEqual(zero.coverage.map(x=>x.revisionId),['r2','r1','r2']);
 assert.equal(zero.provenance.some(x=>x.producerId==='P'),false);
});
test('changed bytes stale, partial earlier coverage warns, omitted alone does not',()=>{
 assert.ok(select(specimen({changed:true})).provenance.filter(x=>x.producerId==='P').every(x=>x.freshness==='stale'));
 assert.equal(select(specimen({previousState:'partial',currentState:'omitted'})).selectedCoverageIncomplete,true);
 assert.equal(select(specimen({currentState:'omitted'})).selectedCoverageIncomplete,false);
 const s=specimen({currentState:'complete'});const selected=select(s);assert.deepEqual(selected.coverage.map(x=>x.revisionId),['r2','r2']);
 assert.deepEqual(selected.provenance.filter(x=>x.producerId==='P').map(x=>x.id),['proof:current']);
});
function evidenceControl(id,s,mutate,assertion,field){
 const expected=select(s);
 return {id,baseline:()=>({loaded:s.loaded,records:s.records,result:s.result,actual:expected}),
  mutate,check:x=>checkGraphEvidence(x.loaded,x.records,s.checked,{...x.result,...x.actual},x.result),
  expectedAssertion:assertion,expectedCode:'invalidRecord',expectedField:field};
}
test('all graph evidence negatives use exact checker controls',async t=>{
 const s=specimen(),old=s.add('r1','oldCall','callBinding');
 const historical={callId:s.result.edges[0].call.id,provenanceId:old,join:{anchor:{revisionId:'r1',document:s.D}}};
 const current=specimen({currentState:'complete'}),currentProof=current.add('r2','call','callBinding');
 const binding={callId:current.result.edges[0].call.id,provenanceId:currentProof,
  join:{anchor:{revisionId:'r2',document:current.D}}};
 const rows=registerControls([
  evidenceControl('GRAPH.oldR1Call',s,x=>{x.result.edges[0].binding=historical;return x;},'GRAPH.OCCURRENCE','edges.binding'),
  evidenceControl('GRAPH.failedR2Binding',s,x=>{x.result.edges[0].binding={...binding,callId:x.result.edges[0].call.id};return x;},'GRAPH.OCCURRENCE','edges.binding'),
  evidenceControl('GRAPH.omittedR2Binding',specimen({currentState:'omitted'}),x=>{x.result.edges[0].binding={...binding,callId:x.result.edges[0].call.id};return x;},'GRAPH.OCCURRENCE','edges.binding'),
  evidenceControl('GRAPH.unlinkedProof',s,x=>{x.loaded.semanticProofs.delete('proof:binding');return x;},'GRAPH.HISTORY','provenance'),
  evidenceControl('GRAPH.relabelProof',s,x=>{x.records.provenance.find(row=>row.id==='proof:binding').revisionId='r2';return x;},'GRAPH.HISTORY','provenance'),
  evidenceControl('GRAPH.coverageMissing',s,x=>{x.actual.coverage.shift();return x;},'GRAPH.COVERAGE','coverage'),
  evidenceControl('GRAPH.coverageExtra',s,x=>{x.actual.coverage.push(x.records.coverage.find(row=>row.producerId==='Q'));return x;},'GRAPH.COVERAGE','coverage'),
  evidenceControl('GRAPH.coverageOrder',s,x=>{x.actual.coverage.reverse();return x;},'GRAPH.COVERAGE','coverage'),
  evidenceControl('GRAPH.proofMissing',s,x=>{x.actual.provenance.shift();return x;},'GRAPH.PROVENANCE','provenance'),
  evidenceControl('GRAPH.proofExtra',s,x=>{x.actual.provenance.push(x.records.provenance.find(row=>row.id==='proof:unreturned'));return x;},'GRAPH.PROVENANCE','provenance'),
  evidenceControl('GRAPH.proofOrder',s,x=>{x.actual.provenance.reverse();return x;},'GRAPH.PROVENANCE','provenance'),
 ]);
 for(const row of rows)await t.test(row.id,()=>runControl(row));
});

// A closed two-snapshot capture: the semantic symbol is authored against r1 bytes.
// Expected output below is deliberately not derived from the selected graph rows.
async function admitted(t,{changed=false,state='failed',zero=false}={}){
 const root=await mkdtemp(join(tmpdir(),'graph-history-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const files=new Map(),put=(path,value)=>files.set(path,typeof value==='string'?value:JSON.stringify(value));
 const hash=text=>contentHash(Buffer.from(text));
 const source='export function go() { go(); }\n',r2=changed?source+'// changed bytes\n':source;
 const document={sourceSetId:'main',language:'javascript',path:'src/go.js'};
 const producers=[['native','native'],['semantic','semantic']].map(([id,kind])=>({id,kind,version:'1',executableHash:hash(`${id} executable`),languages:['javascript'],positionEncoding:'utf8'}));
 const captures=[];
 function capture(ref,kind,file,bytes){put(file,bytes);captures.push({ref,kind,file,hash:hash(bytes)});}
 for(const id of ['native','semantic'])capture(id,'executable',`captures/${id}.bin`,`${id} executable`);
 for(const kind of ['toolchain','config','dependency'])capture(kind,kind,`captures/${kind}.bin`,kind);
 const revisions=['r1','r2'].map(id=>({id,sourceSetId:'main',documents:[{key:document,revisionId:id,sourceFile:`sources/${id}/go.js`}],
  toolchainHash:hash('toolchain'),configHash:hash('config'),dependencyHash:hash('dependency')}));
 put('sources/r1/go.js',source);put('sources/r2/go.js',r2);
 const span=(start,end)=>({encoding:'utf8',start,end}),name=source.indexOf('go'),callee=source.lastIndexOf('go');
 const header={kind:'function',name:'go',modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]};
 const declarations=['r1','r2'].map(id=>({ref:`d${id}`,nativeId:null,document,revisionId:id,parentRef:null,kind:'function',name:'go',
  range:span(7,source.indexOf('}')+1),nameRange:span(name,name+2),header,signature:null,
  witnesses:[{field:'name',witness:{range:span(name,name+2),text:'go'}},{field:'header.name',witness:{range:span(name,name+2),text:'go'}}]}));
 const call={ref:'r2call',nativeId:null,document,revisionId:'r2',ownerRef:'dr2',range:span(callee,callee+4),
  calleeRange:span(callee,callee+2),spelling:'go',regionRefs:[],witnesses:[{field:'spelling',witness:{range:span(callee,callee+2),text:'go'}}]};
 const oldCall={...call,ref:'r1call',revisionId:'r1',ownerRef:'dr1'};
 const oldReference={ref:'r1reference',nativeId:null,document,revisionId:'r1',ownerRef:'dr1',
  range:span(callee,callee+2),spelling:'go',witnesses:[{field:'spelling',witness:{range:span(callee,callee+2),text:'go'}}]};
 put('captures/native.json',{formatVersion:1,producerId:'native',declarations,calls:[oldCall,call],controls:[],references:[oldReference]});
 const fact={kind:'symbol',ref:'r1symbol',record:{key:{scheme:'scip',symbol:'pkg go',scope:'global',document:null},displayName:'go',
  declarations:[{kind:'internal',declarationRef:'dr1',revisionId:'r1'}],provenanceId:'r1-proof'}};
 const oldBinding={kind:'callBinding',ref:'r1-binding',anchor:{document,revisionId:'r1',contentHash:hash(source),
  kind:'callee',range:span(callee,callee+2),ownerRef:'dr1'},record:{resolution:'resolved',
  declaredTarget:{kind:'internal',declarationRef:'dr1',revisionId:'r1'},candidates:[],dispatch:'direct',
  possibleDispatch:[],possibleDispatchComplete:false,provenanceId:'r1-call-proof'}};
 const oldUse={kind:'reference',ref:'r1-use',anchor:{document,revisionId:'r1',contentHash:hash(source),
  kind:'reference',range:span(callee,callee+2),ownerRef:'dr1'},record:{site:'use',roles:['read','call'],
  resolution:'resolved',declaredTarget:{kind:'internal',declarationRef:'dr1',revisionId:'r1'},candidates:[],provenanceId:'r1-reference-proof'}};
 const raw={formatVersion:1,producerId:'semantic',facts:zero?[]:[fact,oldBinding,oldUse]};
 capture('semantic-artifact','semanticArtifact','captures/semantic.json',JSON.stringify(raw));
 const basis={producerId:'semantic',producerVersion:'1',producerHash:producers[1].executableHash,
  artifactHash:captures.at(-1).hash,language:'javascript',sourceSetId:'main',revisionId:'r1',
  sourceManifestHash:sourceManifestHash([{document,contentHash:hash(source)}]),
  toolchainHash:hash('toolchain'),configHash:hash('config'),dependencyHash:hash('dependency'),lookupDependencies:[]};
 const provenance={kind:'provenance',ref:'r1-envelope',record:{id:'r1-proof',producerId:'semantic',document,revisionId:'r1',
  contentHash:hash(source),evidenceKind:'declarationBinding',basis,freshness:'fresh'}};
 const oldProofs=[['r1-call-proof','semanticReference'],['r1-reference-proof','semanticReference']].map(([id,evidenceKind])=>
  ({kind:'provenance',ref:`envelope-${id}`,record:{id,producerId:'semantic',document,revisionId:'r1',
   contentHash:hash(source),evidenceKind,basis,freshness:'fresh'}}));
 const coverageIntents=[],support=['declarationName','callee','invocation','reference'].map(kind=>({kind,available:true,diagnostic:null}));
 for(const id of ['r1','r2']){
  const facts=[];
  for(const producer of producers){
   const semantic=producer.id==='semantic',status=semantic&&id==='r2'?state:'complete';
   facts.push({kind:'coverage',ref:`coverage-${producer.id}-${id}`,record:{producerId:producer.id,sourceSetId:document.sourceSetId,language:document.language,documentPath:document.path,
    revisionId:id,requested:true,selected:status!=='omitted',state:status,supportedRoles:['read'],observedRoles:status==='complete'?['read']:[],diagnostic:status==='complete'?null:'refresh unavailable'}});
   coverageIntents.push({producerId:producer.id,document,revisionId:id,requestedRoles:['read'],measurementSupport:support});
  }
  if(id==='r1'&&!zero)facts.push(provenance,...oldProofs,fact,oldBinding,oldUse);
  put(`sources/${id}/go.js.annotations.json`,{formatVersion:1,document,revisionId:id,scenarios:[],facts});
 }
 put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});
 put('expected/anchors.json',{formatVersion:1,cases:[]});
 const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],
  producers,revisions,comparison:{sourceSetId:'main',revisionId:'r2',producers},coverageIntents,nativeArtifact:'captures/native.json',
  semanticArtifacts:['captures/semantic.json'],annotationFiles:['sources/r1/go.js.annotations.json','sources/r2/go.js.annotations.json'],
  answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 put('fixture.json',fixture);
 for(const [path,value] of files){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}
 const loaded=await loadFixture(root),records=normalizeFixture(loaded).records;
 records.producers.sort((a,b)=>Buffer.compare(canonicalBytes(a),canonicalBytes(b)));
 records.revisions.sort((a,b)=>Buffer.compare(canonicalBytes(a),canonicalBytes(b)));
 const checked=checkCoverage(loaded,records);
 const measured=checkMeasurement(loaded,records),joins=checkJoins(loaded,records,checked,measured);
 checkBindings(loaded,records,checked,measured,joins);
 const traversal=traverseGraph({request:{sourceSetId:'main',revisionId:'r2',rootSyntaxId:records.declarations.find(row=>row.revisionId==='r2').syntaxId,
  semanticProducerId:'semantic'},records});
 assert.equal(traversal.ok,true);return {loaded,records,checked,result:traversal.result};
}
test('admitted failed/omitted refresh selects source-backed declaration proofs, warning keys and r2 call only',async t=>{
 for(const options of [{},{changed:true},{state:'omitted'},{zero:true}]){
  const s=await admitted(t,options),r=s.result;
  const expectedSyntax=syntaxId({sourceSet:'main',path:'src/go.js',language:'javascript',ancestors:[],
   declaration:{kind:'function',name:'go',signature:null,ordinal:0}});
  const expectedCall=occurrenceId({revisionId:'r2',ownerSyntaxId:expectedSyntax,kind:'call',ordinal:0});
  assert.equal(r.nodes.length,1);assert.equal(r.edges.length,1);assert.equal(r.edges[0].binding,null);
  assert.equal(r.nodes[0].declaration.syntaxId,expectedSyntax);
  assert.equal(r.edges[0].call.id,expectedCall);
  assert.equal(r.edges[0].call.revisionId,'r2');assert.equal(r.edges[0].boundaryReason,'missingEvidence');
  const evidence=select(s);Object.assign(r,evidence);
  assert.deepEqual(evidence.coverage.map(row=>[row.producerId,row.revisionId]),[['native','r2'],['semantic','r1'],['semantic','r2']]);
  assert.deepEqual(evidence.provenance.map(row=>row.id),
   [`native:r2:${expectedCall}`,`native:r2:${expectedSyntax}`,...(options.zero?[]:['r1-proof'])]);
  assert.equal(s.records.callBindings.length,options.zero?0:1);
  assert.equal(s.records.references.some(row=>row.provenanceId==='r1-reference-proof'),!options.zero);
  assert.equal(evidence.provenance.some(row=>row.evidenceKind==='semanticReference'),false);
  assert.equal(evidence.provenance.find(row=>row.id==='r1-proof')?.freshness,
   options.zero?undefined:options.changed?'stale':'possiblyStale');
  r.warnings=deriveWarnings(r);
  assert.deepEqual(r.warnings.map(({code,provenanceId})=>[code,provenanceId]),
   options.state==='omitted'?[...(options.zero?[]:[['staleEvidence',options.changed?'r1-proof':null]])]:
   [['coverageIncomplete',null],...(options.zero?[]:[['staleEvidence',options.changed?'r1-proof':null]])]);
  assert.deepEqual(checkGraphEvidence(s.loaded,s.records,s.checked,r,r),evidence);
  assert.equal(checkWarnings(r),true);
  if(!options.zero){
   const control=registerControls([{id:`GRAPH.sourceFailedBinding.${options.state??'failed'}.${options.changed?'changed':'same'}`,
    baseline:()=>r,mutate:answer=>{answer.edges[0].binding={callId:answer.edges[0].call.id,
     provenanceId:'r1-proof',join:{anchor:{document:answer.edges[0].call.document,revisionId:'r2'}}};return answer;},
    check:answer=>checkGraphEvidence(s.loaded,s.records,s.checked,answer,answer),
    expectedAssertion:'GRAPH.OCCURRENCE',expectedCode:'invalidRecord',expectedField:'edges.binding'}])[0];
   await t.test(control.id,()=>runControl(control));
  }
 }
});
