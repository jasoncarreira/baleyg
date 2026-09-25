import test from 'node:test';
import assert from 'node:assert/strict';
import {checkGraphAnswer,expectedGraph} from '../graph-check.mjs';
import {registerControls,runControl} from './mutations.mjs';

const sid=n=>`sid:v1:${String(n).padStart(32,'0')}`;
const oid=n=>`occ:v1:${String(n).padStart(32,'0')}`;
const D={sourceSetId:'main',language:'javascript',path:'src/go.js'};
const coverage=(p,r,state)=>({producerId:p,sourceSetId:'main',language:'javascript',documentPath:D.path,revisionId:r,
 selected:state!=='omitted',requested:true,state,supportedRoles:[],observedRoles:[],diagnostic:state==='complete'?null:'diagnostic'});
function specimen({state='complete',old=false,changed=false}={}){
 const r0={id:'r0',sourceSetId:'main',documents:[{key:D,contentHash:'same'}]};
 const r1={id:'r1',sourceSetId:'main',documents:[{key:D,contentHash:'same'}]};
 const r2={id:'r2',sourceSetId:'main',documents:[{key:D,contentHash:changed?'changed':'same'}]};
 const declarations='ABCD'.split('').map((name,i)=>({name,syntaxId:sid(i+1),document:D,revisionId:'r2',provenanceId:`native:${name}`}));
 const nativeProofs=declarations.map(d=>({id:d.provenanceId,producerId:'N',document:D,revisionId:'r2',freshness:'fresh'}));
 const records={sourceSets:[{id:'main'}],revisions:[r0,r1,r2],producers:[{id:'N',kind:'native'},{id:'P',kind:'semantic'}],
  declarations,calls:[],callBindings:[],references:[],symbols:[],declarationBindings:[],typeRelationships:[],
  coverage:[coverage('N','r2','complete'),coverage('P','r2',state),coverage('P','r1','complete'),coverage('P','r0','complete')],provenance:nativeProofs};
 const loaded={native:{producerId:'N'},annotations:[],semanticProofs:new Map(),revisionChronology:new Map([['main',[r0,r1,r2]]])};
 const checked={checkUse:({producerId,revisionId,provenanceIds})=>{for(const id of provenanceIds)
  assert.ok(records.provenance.some(p=>p.id===id&&p.producerId===producerId&&p.revisionId===revisionId));}};
 const request={sourceSetId:'main',revisionId:'r2',rootSyntaxId:sid(1),semanticProducerId:'P',depth:2,maxNodes:150,maxCalls:500};
 let counter=0;
 function add(from,to,{start=counter*10,dispatch='direct',resolution='resolved',freshness='fresh',staleTarget=false,
  oldBinding=false,possibleDispatch=[],targetRevision='r2'}={}){
  const id=oid(++counter),call={id,ownerSyntaxId:sid(from),ordinal:counter,document:D,revisionId:'r2',range:{start,end:start+4},provenanceId:`native:call:${counter}`};
  records.calls.push(call);records.provenance.push({id:call.provenanceId,producerId:'N',document:D,revisionId:'r2',freshness:'fresh'});
  const proofId=`P:${counter}`,rev=oldBinding?'r1':'r2';
  const binding={callId:id,join:{status:'exact',candidateIds:[id],anchor:{document:D,revisionId:rev,contentHash:changed?'changed':'same'}},
   resolution,declaredTarget:resolution==='resolved'?{kind:'internal',syntaxId:sid(to),document:D,revisionId:targetRevision}:null,
   candidates:[],dispatch,possibleDispatch:possibleDispatch.map(n=>({kind:'internal',syntaxId:sid(n),document:D,revisionId:'r2'})),
   possibleDispatchComplete:false,staleTarget,provenanceId:proofId};
  records.callBindings.push(binding);records.provenance.push({id:proofId,producerId:'P',document:D,revisionId:rev,
   contentHash:changed?'changed':'same',freshness});return {call,binding};
 }
 // Every expected output below is built from stated IDs/rows, not expectedGraph.
 const authored=({names=['A'],depths=names.map(()=>0),edges=[],frontier=[],proofIds=[],extraCoverage=[],warnings=[],partial=false,truncated=false,
  req=request}={})=>({id:'case',attemptedRequest:req,answer:{ok:true,result:{request:req,resolvedRevisionId:'r2',
   nodes:names.map((name,i)=>({declaration:declarations['ABCD'.indexOf(name)],depth:depths[i]})),
   edges,frontier,coverage:[records.coverage[0],...extraCoverage,...(req.semanticProducerId===null?[]:[records.coverage[1]])]
    .sort((a,b)=>a.producerId.localeCompare(b.producerId)||a.revisionId.localeCompare(b.revisionId)),
   provenance:records.provenance.filter(p=>[...names.map(name=>`native:${name}`),...edges.map(e=>e.call.provenanceId),...proofIds].includes(p.id))
    .sort((a,b)=>Buffer.compare(Buffer.from(a.id),Buffer.from(b.id))),
   partial,truncated,warnings}}});
 const edge=(pair,from,{reason='none',visit='new',to=pair.binding.declaredTarget?.syntaxId??null,binding=pair.binding}={})=>
  ({call:pair.call,from:sid(from),to:reason==='none'?to:null,binding,visit:reason==='none'?visit:'boundary',boundaryReason:reason});
 const front=(reason,node,callId=null,targetId=null,nextOrdinal=null,omittedCalls=0)=>({reason,nodeId:sid(node),callId,targetId,nextOrdinal,omittedCalls});
 return {records,loaded,checked,request,authored,add,edge,front};
}
const verify=(s,entry)=>checkGraphAnswer(s.loaded,s.records,s.checked,entry);

test('eight hand-authored pinned FIFO, cycle, diamond, callback, depth, node and call cap cases',()=>{
 {const s=specimen();assert.equal(verify(s,s.authored()),true);}
 {const s=specimen();const b=s.add(1,2,{start:20}),c=s.add(1,3,{start:1}),d=s.add(3,4),diamond=s.add(2,4),cycle=s.add(4,1);
  const e=s.authored({names:['A','C','B','D'],depths:[0,1,1,2],edges:[s.edge(c,1),s.edge(b,1),s.edge(d,3),s.edge(diamond,2,{visit:'seen'}),s.edge(cycle,4,{visit:'seen'})],
   proofIds:[c.binding.provenanceId,b.binding.provenanceId,d.binding.provenanceId,diamond.binding.provenanceId,cycle.binding.provenanceId]});
  e.attemptedRequest.depth=e.answer.result.request.depth=3;assert.equal(verify(s,e),true);}
 {const s=specimen();const loop=s.add(1,1);s.records.references.push({ownerSyntaxId:sid(1),declaredTarget:{kind:'internal',syntaxId:sid(4)}});
  assert.equal(verify(s,s.authored({edges:[s.edge(loop,1,{visit:'seen'})],proofIds:[loop.binding.provenanceId]})),true);}
 {const s=specimen();const b=s.add(1,2),c=s.add(2,3);const e=s.authored({names:['A','B'],depths:[0,1],edges:[s.edge(b,1)],
   proofIds:[b.binding.provenanceId],frontier:[s.front('depth',2,null,null,0,1)],partial:true,truncated:true});
  e.attemptedRequest.depth=e.answer.result.request.depth=1;assert.equal(verify(s,e),true);}
 {const s=specimen();const b=s.add(1,2),c=s.add(1,3),a=s.add(1,1);const e=s.authored({names:['A','B'],depths:[0,1],
   edges:[s.edge(b,1),s.edge(c,1,{reason:'nodeLimit'}),s.edge(a,1,{visit:'seen'})],proofIds:[b.binding.provenanceId,c.binding.provenanceId,a.binding.provenanceId],
   frontier:[s.front('nodeLimit',1,c.call.id,sid(3))],partial:true,truncated:true});
  e.attemptedRequest.maxNodes=e.answer.result.request.maxNodes=2;assert.equal(verify(s,e),true);}
 {const s=specimen();const b=s.add(1,2),c=s.add(1,3),d=s.add(2,4);const e=s.authored({names:['A','B'],depths:[0,1],edges:[s.edge(b,1)],
   proofIds:[b.binding.provenanceId],frontier:[s.front('callLimit',1,null,null,c.call.ordinal,1),s.front('callLimit',2,null,null,0,1)],partial:true,truncated:true});
  e.attemptedRequest.maxCalls=e.answer.result.request.maxCalls=1;assert.equal(verify(s,e),true);}
 {const s=specimen();s.add(1,2);const e=s.authored({frontier:[s.front('callLimit',1,null,null,1,1)],partial:true,truncated:true});
  e.attemptedRequest.maxCalls=e.answer.result.request.maxCalls=0;assert.equal(verify(s,e),true);}
 {const s=specimen();const dispatch=s.add(1,2,{dispatch:'virtual',possibleDispatch:[2]}),stale=s.add(1,2,{staleTarget:true});
  const e=s.authored({edges:[s.edge(dispatch,1,{reason:'dispatch'}),s.edge(stale,1,{reason:'stale'})],
   proofIds:[dispatch.binding.provenanceId,stale.binding.provenanceId],warnings:[{code:'staleTarget',provenanceId:stale.binding.provenanceId,message:'stale target'}],partial:true});
  assert.equal(verify(s,e),true);}
});

test('typed request failures preserve pinned order and no fallback',()=>{
 for(const [override,code,field] of [[{depth:6},'invalidRequest','depth'],[{maxNodes:0},'invalidRequest','maxNodes'],
  [{maxCalls:501},'invalidRequest','maxCalls'],[{sourceSetId:'other'},'sourceSetDenied','sourceSetId'],
  [{revisionId:'not-admitted'},'revisionUnavailable','revisionId'],[{rootSyntaxId:sid(9)},'rootMissing','rootSyntaxId'],
  [{semanticProducerId:'other'},'producerUnavailable','semanticProducerId']]){
  const s=specimen(),req={...s.request,...override},entry={id:'failure',attemptedRequest:req,
   answer:{ok:false,error:{code,field,message:'not available'}}};assert.equal(verify(s,entry),true);
 }
});

test('failed and omitted r2 keep r2 measured call, withhold even fresh r2 binding and old r1 occurrence evidence',()=>{
 for(const state of ['failed','omitted'])for(const changed of [false,true]){
  const s=specimen({state,changed}),old=s.add(1,2,{oldBinding:true});
  const current=s.add(1,2);s.records.references.push({revisionId:'r1',ownerSyntaxId:sid(1),provenanceId:'old-reference',declaredTarget:{kind:'internal',syntaxId:sid(2)}});
  // Only the current measured call is retained; remove the synthetic earlier call.
  s.records.calls.shift();s.records.provenance=s.records.provenance.filter(row=>row.id!==old.call.provenanceId);
  const warning=state==='failed'?[{code:'coverageIncomplete',provenanceId:null,message:'refresh failed'}]:[];
  const entry=s.authored({edges:[s.edge(current,1,{reason:'missingEvidence',binding:null})],extraCoverage:[s.records.coverage[2]],partial:true,warnings:warning});
  assert.equal(verify(s,entry),true);assert.equal(entry.answer.result.edges[0].binding,null);
  assert.equal(entry.answer.result.provenance.some(row=>row.id===old.binding.provenanceId||row.id==='old-reference'||row.id===current.binding.provenanceId),false);
 }
});

test('graph checker negative controls compare exact fields, proof and warning keys',async t=>{
 const s=specimen(),b=s.add(1,2),base=s.authored({names:['A','B'],depths:[0,1],edges:[s.edge(b,1)],proofIds:[b.binding.provenanceId]});
 const rows=registerControls([
  {id:'GRAPH.authored-edge',baseline:()=>base,mutate:x=>{x.answer.result.edges[0].to=sid(4);return x;},
   check:x=>verify(s,x),expectedAssertion:'GRAPH.TRAVERSAL',expectedCode:'invalidRecord',expectedField:'answers.case.result.edges'},
  {id:'GRAPH.authored-proof',baseline:()=>base,mutate:x=>{x.answer.result.provenance.pop();return x;},
   check:x=>verify(s,x),expectedAssertion:'GRAPH.PROVENANCE',expectedCode:'invalidRecord',expectedField:'provenance'},
  {id:'GRAPH.authored-warning',baseline:()=>base,mutate:x=>{x.answer.result.warnings.push({code:'syntaxOnly',provenanceId:null,message:'extra'});return x;},
   check:x=>verify(s,x),expectedAssertion:'WARNING.KEYS',expectedCode:'invalidRecord',expectedField:'warnings'},
 ]);
 for(const row of rows)await t.test(row.id,()=>runControl(row));
});

function historical(s,{revision='r1',name='A',kind='declarationBinding',producer='P',document=D,id=`${revision}:${kind}:${name}`}={}){
 const provenanceId=`proof:${id}`,syntaxId=sid('ABCD'.indexOf(name)+1);
 const fact=kind==='typeRelationship'?{kind,ref:id,provenanceRef:provenanceId}:
  {kind,ref:id,record:{provenanceId}};
 const row=kind==='typeRelationship'?{provenanceId,source:{kind:'internal',syntaxId,document,revisionId:revision}}:
  kind==='symbol'?{provenanceId,declarations:[{kind:'internal',syntaxId,document,revisionId:revision}]}:
   {provenanceId,syntaxId};
 s.records[({declarationBinding:'declarationBindings',symbol:'symbols',typeRelationship:'typeRelationships'})[kind]].push(row);
 s.records.provenance.push({id:provenanceId,producerId:producer,document,revisionId:revision,
  contentHash:'same',basis:{artifactHash:'captured'},freshness:s.loaded.revisionChronology.get('main')[2].documents[0].contentHash==='same'?'possiblyStale':'stale'});
 s.loaded.annotations.push({revisionId:revision,document,facts:[fact]});
 s.loaded.semanticProofs.set(provenanceId,{fact,hash:'captured',factRef:id,factKind:kind});return provenanceId;
}
test('latest prior tuple selects only captured returned declaration proofs; changed bytes and partial warnings',()=>{
 for(const changed of [false,true]){
  const s=specimen({state:'failed',changed}),old=historical(s,{revision:'r0'});
  const selected=['declarationBinding','symbol','typeRelationship'].map(kind=>historical(s,{kind}));
  historical(s,{name:'B'});
  const req=s.request,entry=s.authored({extraCoverage:[s.records.coverage[2]],proofIds:selected,
   partial:true,warnings:[{code:'coverageIncomplete',provenanceId:null,message:'r2 failed'},
    {code:'staleEvidence',provenanceId:changed?selected[0]:null,message:'historical proof'},
    ...(changed?selected.slice(1).map(id=>({code:'staleEvidence',provenanceId:id,message:'historical proof'})):[])]});
  assert.equal(verify(s,entry),true);assert.equal(entry.answer.result.provenance.some(row=>row.id===old),false);
  assert.equal(entry.answer.result.provenance.length,4);
 }
 const zero=specimen({state:'failed'}),older=historical(zero,{revision:'r0'});
 const empty=zero.authored({extraCoverage:[zero.records.coverage[2]],partial:true,
  warnings:[{code:'coverageIncomplete',provenanceId:null,message:'failed'}]});
 assert.equal(verify(zero,empty),true);assert.equal(empty.answer.result.provenance.some(p=>p.id===older),false);
 const s=specimen({state:'omitted'}),id=historical(s);
 s.records.coverage[2].state='partial';s.records.coverage[2].diagnostic='partial';
 const entry=s.authored({extraCoverage:[s.records.coverage[2]],proofIds:[id],
  partial:true,warnings:[{code:'coverageIncomplete',provenanceId:null,message:'r1 partial'},
   {code:'staleEvidence',provenanceId:null,message:'possibly stale'}]});
 // Omitted alone is not incomplete, but the selected historical partial row is.
 assert.equal(verify(s,entry),true);
});
test('captured chain rejects relabelled and unlinked historical proofs with precise fields',async t=>{
 const make=()=>{const s=specimen({state:'failed'}),id=historical(s);
  const entry=s.authored({extraCoverage:[s.records.coverage[2]],proofIds:[id],partial:true,
   warnings:[{code:'coverageIncomplete',provenanceId:null,message:'failed'},
    {code:'staleEvidence',provenanceId:null,message:'possibly stale'}]});return {loaded:s.loaded,records:s.records,id,entry};};
 const check=x=>checkGraphAnswer(x.loaded,x.records,{checkUse:()=>true},x.entry);
 const controls=registerControls([
  {id:'GRAPH.history.relabel',baseline:make,mutate:x=>{x.records.provenance.find(p=>p.id===x.id).revisionId='r2';return x;},
   check,expectedAssertion:'GRAPH.HISTORY',expectedCode:'invalidRecord',expectedField:'provenance'},
  {id:'GRAPH.history.unlinked',baseline:make,mutate:x=>{x.loaded.semanticProofs.delete(x.id);return x;},
   check,expectedAssertion:'GRAPH.HISTORY',expectedCode:'invalidRecord',expectedField:'provenance'},
 ]);
 for(const row of controls)await t.test(row.id,()=>runControl(row));
});
