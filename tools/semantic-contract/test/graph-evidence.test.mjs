import test from 'node:test';
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
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
test('r1 call or reference never authorizes an r2 occurrence, and relabelled or unlinked proofs fail',()=>{
 const s=specimen(),old=s.add('r1','oldCall','callBinding');
 s.records.callBindings=[{callId:s.result.edges[0].call.id,provenanceId:old,
  join:{anchor:{revisionId:'r1',document:s.D}}}];
 s.records.references=[{id:s.result.edges[0].call.id,revisionId:'r1',provenanceId:old}];
 assert.equal(select(s).provenance.some(x=>x.id===old),false);
 s.result.edges[0].binding=s.records.callBindings[0];
 assert.throws(()=>select(s),{assertion:'GRAPH.OCCURRENCE',field:'edges.binding'});
 s.result.edges[0].binding=null;
 s.loaded.semanticProofs.delete('proof:binding');
 assert.throws(()=>select(s),{assertion:'GRAPH.HISTORY',field:'provenance'});
 const relabel=specimen();relabel.records.provenance.find(x=>x.id==='proof:binding').revisionId='r2';
 assert.throws(()=>select(relabel),{assertion:'GRAPH.HISTORY',field:'provenance'});
});
test('independent evidence checker rejects extra, omitted and reordered rows',()=>{
 const s=specimen(),correct=select(s);
 assert.deepEqual(checkGraphEvidence(s.loaded,s.records,s.checked,{...s.result,...correct},s.result),correct);
 for(const [field,value] of [['coverage',correct.coverage.slice(1)],['provenance',correct.provenance.slice(1)],
  ['coverage',[...correct.coverage].reverse()],['provenance',[...correct.provenance].reverse()]]){
  assert.throws(()=>checkGraphEvidence(s.loaded,s.records,s.checked,{...s.result,...correct,[field]:value},s.result),
   {assertion:field==='coverage'?'GRAPH.COVERAGE':'GRAPH.PROVENANCE',field});
 }
});
// Mutation changes the production checker, not its fixture or expected assertion.
const controls=registerControls([{id:'GRAPH.HISTORY.latestTuple',baseline:()=>({source:readFileSync(new URL('../graph-evidence.mjs',import.meta.url),'utf8')}),
 mutate:input=>{const old='[...chronology.slice(0,position)].reverse().find';assert.equal(input.source.split(old).length,2);
  input.source=input.source.replace(old,'[...chronology.slice(0,position)].find');return input;},
 check:async input=>{
  const source=input.source.replace("from './json.mjs'",`from '${new URL('../json.mjs',import.meta.url).href}'`);
  const {selectGraphEvidence:selectActual}=await import(`data:text/javascript;base64,${Buffer.from(source).toString('base64')}`);
  const s=specimen({zero:true}),out=selectActual(s.loaded,s.records,s.checked,s.result);
  const actual=out.coverage.filter(x=>x.producerId==='P').map(x=>x.revisionId);
  if(actual.join(',')!=='r1,r2'){
   const error=new Error(`GRAPH.HISTORY coverage: expected r1,r2, got ${actual}`);
   Object.assign(error,{assertion:'GRAPH.HISTORY',code:'invalidRecord',field:'coverage'});throw error;
  }return true;
 },expectedAssertion:'GRAPH.HISTORY',expectedCode:'invalidRecord',expectedField:'coverage'}]);
for(const row of controls)test(`source baseline → one production mutation → assertion: ${row.id}`,async()=>assert.equal(await runControl(row),row.id));
