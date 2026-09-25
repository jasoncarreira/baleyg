import test from 'node:test';
import assert from 'node:assert/strict';
import {traverseGraph} from '../graph-traversal.mjs';
import {registerControls,runControl} from './mutations.mjs';

const sid=n=>`sid:v1:${n.toString(16).padStart(32,'0')}`;
const oid=n=>`occ:v1:${n.toString(16).padStart(32,'0')}`;
const doc={sourceSetId:'main',language:'javascript',path:'src/main.js'};
function specimen(spec={}){
 const ids=Object.fromEntries('ABCDEFGS'.split('').map((name,i)=>[name,sid(i+1)]));
 const declarations=Object.entries(ids).map(([name,syntaxId])=>({syntaxId,document:doc,revisionId:'r2',name}));
 const records={sourceSets:[{id:'main'}],revisions:[{id:'r2',sourceSetId:'main'}],producers:[{id:'P',kind:'semantic'}],
  declarations,calls:[],callBindings:[],provenance:[],coverage:[{producerId:'P',...{sourceSetId:'main',language:'javascript',documentPath:doc.path,revisionId:'r2'},selected:true,state:'complete'}]};
 const request={sourceSetId:'main',revisionId:'r2',rootSyntaxId:ids.A,semanticProducerId:'P',...spec.request};
 let counter=0;
 function add(owner,target,{start=counter*10,dispatch='direct',resolution='resolved',freshness='fresh',staleTarget=false,revision='r2',targetRevision=revision,callRevision='r2',possibleDispatch=[],coverage=true}={}){
  const number=++counter,call={id:oid(number),ownerSyntaxId:ids[owner],ordinal:number,document:doc,revisionId:callRevision,range:{start,end:start+5}};
  records.calls.push(call);
  const provenanceId=`proof-${number}`;
  const binding={callId:call.id,join:{status:'exact',candidateIds:[call.id],anchor:{document:doc,revisionId:revision,contentHash:'bytes',kind:'callee',range:call.range}},
   resolution,declaredTarget:resolution==='resolved'?{kind:'internal',syntaxId:ids[target],document:doc,revisionId:targetRevision}:resolution==='external'?{kind:'external',symbol:{symbol:'external'}}:null,
   candidates:[],dispatch,possibleDispatch:possibleDispatch.map(name=>({kind:'internal',syntaxId:ids[name],document:doc,revisionId:'r2'})),possibleDispatchComplete:false,staleTarget,provenanceId};
  records.callBindings.push(binding);
  records.provenance.push({id:provenanceId,producerId:'P',document:doc,revisionId:revision,contentHash:'bytes',freshness});
  if(!coverage)records.coverage=[];
  return call;
 }
 return {ids,records,request,add,run:()=>traverseGraph({request,records})};
}
const summary=result=>({nodes:result.nodes.map(n=>[n.declaration.name,n.depth]),edges:result.edges.map(e=>[e.call.id,e.from,e.to,e.visit,e.boundaryReason]),frontier:result.frontier});
function frontier(reason,nodeId,callId=null,targetId=null,nextOrdinal=null,omittedCalls=0){return {reason,nodeId,callId,targetId,nextOrdinal,omittedCalls};}

test('default request, pinned failures and strict integer limits',()=>{
 const s=specimen(),result=s.run();assert.equal(result.ok,true);assert.deepEqual(result.result.request,{...s.request,depth:2,maxNodes:150,maxCalls:500});
 for(const [change,code,field] of [[{depth:6},'invalidRequest','depth'],[{depth:1.5},'invalidRequest','depth'],[{maxNodes:0},'invalidRequest','maxNodes'],[{maxCalls:501},'invalidRequest','maxCalls'],[{sourceSetId:'foreign'},'sourceSetDenied','sourceSetId'],[{revisionId:'r1'},'revisionUnavailable','revisionId'],[{rootSyntaxId:sid(99)},'rootMissing','rootSyntaxId'],[{semanticProducerId:'Q'},'producerUnavailable','semanticProducerId']]){
  const answer=traverseGraph({request:{...s.request,...change},records:s.records});assert.deepEqual(Object.keys(answer),['ok','error']);assert.equal(answer.error.code,code);assert.equal(answer.error.field,field);
 }
});
test('FIFO admission, source byte order, nested-owner isolation, cycle and diamond',()=>{
 const s=specimen({request:{depth:3}});
 const late=s.add('A','C',{start:30}),early=s.add('A','B',{start:10});
 const b=s.add('B','D'),c=s.add('C','D'),cycle=s.add('D','A');
 const r=s.run().result;
 assert.deepEqual(r.nodes.map(n=>[n.declaration.name,n.depth]),[['A',0],['B',1],['C',1],['D',2]]);
 assert.deepEqual(r.edges.map(e=>e.call.id),[early.id,late.id,b.id,c.id,cycle.id]);
 assert.deepEqual(r.edges.map(e=>e.visit),['new','new','new','seen','seen']);
 assert.deepEqual(r.frontier,[]);assert.equal(r.partial,false);assert.equal(r.truncated,false);
});
test('depth zero and one, empty node and exact cap do not invent frontier',()=>{
 const s=specimen();const a=s.add('A','B'),b=s.add('B','C');
 let r=traverseGraph({records:s.records,request:{...s.request,depth:0}}).result;
 assert.deepEqual(r.frontier,[frontier('depth',s.ids.A,null,null,0,1)]);assert.equal(r.edges.length,0);
 r=traverseGraph({records:s.records,request:{...s.request,depth:1}}).result;
 assert.deepEqual(r.frontier,[frontier('depth',s.ids.B,null,null,0,1)]);assert.deepEqual(r.edges.map(e=>e.call.id),[a.id]);
 s.records.calls.pop();s.records.callBindings.pop();s.records.provenance.pop();
 r=traverseGraph({records:s.records,request:{...s.request,maxCalls:1,depth:1}}).result;
 assert.deepEqual(r.frontier,[]);assert.equal(r.truncated,false);assert.equal(r.edges.length,1);
});
test('node cap emits one frontier per refused call and later seen target proceeds',()=>{
 const s=specimen({request:{maxNodes:2}}),a=s.add('A','B'),b=s.add('A','C'),c=s.add('A','A');const r=s.run().result;
 assert.deepEqual(r.edges.map(e=>[e.to,e.visit,e.boundaryReason]),[[s.ids.B,'new','none'],[null,'boundary','nodeLimit'],[s.ids.A,'seen','none']]);
 assert.deepEqual(r.frontier,[frontier('nodeLimit',s.ids.A,b.id,s.ids.C,null,0)]);
 assert.equal(r.edges.length,3);assert.equal(r.partial,true);assert.equal(r.truncated,true);
});
test('global call cap drains queued nodes in FIFO order, and zero cap stops before first call',()=>{
 const s=specimen({request:{maxCalls:1}}),a=s.add('A','B'),b=s.add('A','C'),c=s.add('B','D');
 let r=s.run().result;
 assert.deepEqual(r.edges.map(e=>e.call.id),[a.id]);assert.deepEqual(r.frontier,[frontier('callLimit',s.ids.A,null,null,b.ordinal,1),frontier('callLimit',s.ids.B,null,null,0,1)]);
 r=traverseGraph({records:s.records,request:{...s.request,maxCalls:0}}).result;
 assert.deepEqual(r.frontier,[frontier('callLimit',s.ids.A,null,null,a.ordinal,2)]);
 assert.equal(r.edges.length,0);
});
test('boundary precedence and fresh exact static expansion, not possible dispatch',()=>{
 const s=specimen({request:{maxNodes:1}});
 s.add('A','B',{dispatch:'virtual',possibleDispatch:['B']});s.add('A','B',{staleTarget:true});s.add('A','B',{resolution:'external'});
 s.add('A','B',{resolution:'unresolved'});s.add('A','B',{resolution:'ambiguous'});
 const missing=s.add('A','B',{revision:'r1'});
 const direct=s.add('A','B');
 const r=s.run().result;
 assert.deepEqual(r.edges.map(e=>e.boundaryReason),['dispatch','stale','external','unresolved','ambiguous','missingEvidence','nodeLimit']);
 assert.deepEqual(r.frontier,[frontier('nodeLimit',s.ids.A,direct.id,s.ids.B,null,0)]);
 assert.equal(r.nodes.length,1);assert.equal(r.edges.find(e=>e.call.id===missing.id).binding,null);
});
test('failed refresh and syntax-only cannot reuse old occurrence proof; incomplete coverage sets partial',()=>{
 const s=specimen();const old=s.add('A','B',{revision:'r1',callRevision:'r1'});
 const current={...old,id:oid(101),revisionId:'r2',ordinal:0};s.records.calls.push(current);
 s.records.references=[{id:oid(102),ownerSyntaxId:s.ids.A,revisionId:'r1',declaredTarget:{kind:'internal',syntaxId:s.ids.B,document:doc,revisionId:'r1'}}];
 s.records.coverage[0].state='failed';
 let r=s.run().result;assert.equal(r.edges.length,1);assert.equal(r.edges[0].call.id,current.id);
 assert.notEqual(old.id,current.id);assert.equal(r.edges[0].binding,null);assert.equal(r.edges[0].boundaryReason,'missingEvidence');assert.equal(r.partial,true);
 s.records.coverage[0].state='complete';s.request.semanticProducerId=null;r=s.run().result;
 assert.equal(r.edges[0].binding,null);assert.equal(r.edges[0].boundaryReason,'missingEvidence');
 const empty=specimen();assert.equal(traverseGraph({records:empty.records,request:empty.request,selectedCoverageIncomplete:true}).result.partial,true);
});

test('selected failed coverage alone sets partial; omitted coverage alone does not',()=>{
 const s=specimen();s.records.coverage[0].state='failed';assert.equal(s.run().result.partial,true);
 s.records.coverage[0].state='omitted';s.records.coverage[0].selected=false;assert.equal(s.run().result.partial,false);
});
test('self-loop remains seen, and a callback reference creates no call edge',()=>{
 const s=specimen({request:{depth:1}});s.add('A','A');
 let r=s.run().result;assert.deepEqual(r.nodes.map(n=>n.declaration.name),['A']);assert.deepEqual(r.edges.map(e=>e.visit),['seen']);
 const t=specimen({request:{depth:1}});t.add('A','S');
 t.records.references=[{ownerSyntaxId:t.ids.A,declaredTarget:{kind:'internal',syntaxId:t.ids.C,document:doc,revisionId:'r2'}}];
 r=t.run().result;assert.deepEqual(r.nodes.map(n=>n.declaration.name),['A','S']);assert.equal(r.edges.length,1);
});
test('fresh direct/constructor bindings expand while a stale proof and absent coverage cannot',()=>{
 const s=specimen({request:{depth:1}});s.add('A','B',{dispatch:'constructor'});s.add('A','C',{freshness:'possiblyStale'});s.add('A','D',{coverage:false});
 const r=s.run().result;assert.deepEqual(r.nodes.map(n=>n.declaration.name),['A']);
 assert.deepEqual(r.edges.map(e=>e.boundaryReason),['missingEvidence','missingEvidence','missingEvidence']);
 const t=specimen({request:{depth:1}});t.add('A','B',{dispatch:'constructor'});t.add('A','C',{freshness:'stale'});
 assert.deepEqual(t.run().result.edges.map(e=>e.boundaryReason),['none','stale']);
});

test('fresh r2 caller expands stable r1 internal target only when target bytes match',()=>{
 const s=specimen();const call=s.add('A','B',{revision:'r2',targetRevision:'r1'});
 const result=s.run().result;assert.deepEqual(result.nodes.map(n=>n.declaration.name),['A','B']);
 assert.deepEqual(result.edges.map(e=>[e.call.id,e.to,e.visit,e.boundaryReason]),[[call.id,s.ids.B,'new','none']]);
 const binding=s.records.callBindings[0];binding.staleTarget=true;
 const changed=s.run().result;assert.deepEqual(changed.nodes.map(n=>n.declaration.name),['A']);
 assert.deepEqual(changed.edges.map(e=>[e.to,e.visit,e.boundaryReason]),[[null,'boundary','stale']]);
 binding.staleTarget=false;s.records.provenance[0].freshness='possiblyStale';
 assert.equal(s.run().result.edges[0].boundaryReason,'stale');
});

// Control expectations are authored constants; each check re-runs production on the
// current source specimen, including the one-member mutation made by runControl.
function decisionCheck(input,{assertion,field,expected,select}){
 const answer=traverseGraph(input);
 const actual=answer.ok?select(answer.result):undefined;
 if(!Object.is(actual,expected)){
  const error=new Error(`${assertion} ${field}: expected ${String(expected)}, got ${String(actual)}`);
  Object.assign(error,{assertion,code:'invalidRecord',field});throw error;
 }
 return true;
}
const controls=registerControls([
 {id:'GRAPH.nodeLimit.to',baseline:()=>{const s=specimen({request:{maxNodes:1}});s.add('A','B');return {records:s.records,request:s.request};},
  mutate:input=>{input.request.maxNodes=2;return input;},
  check:input=>decisionCheck(input,{assertion:'GRAPH.NODE_LIMIT',field:'edges[0].to',expected:null,select:r=>r.edges[0]?.to}),
  expectedAssertion:'GRAPH.NODE_LIMIT',expectedCode:'invalidRecord',expectedField:'edges[0].to'},
 {id:'GRAPH.failedRefresh.reason',baseline:()=>{const s=specimen();s.add('A','B',{revision:'r1',callRevision:'r1'});
   s.records.calls.push({id:oid(101),ownerSyntaxId:s.ids.A,ordinal:0,document:doc,revisionId:'r2',range:{start:0,end:5}});
   s.records.coverage[0].state='failed';return {records:s.records,request:s.request};},
  mutate:input=>{input.records.calls[1].revisionId='r1';return input;},
  check:input=>decisionCheck(input,{assertion:'GRAPH.FAILED_REFRESH',field:'edges[0].boundaryReason',expected:'missingEvidence',select:r=>r.edges[0]?.boundaryReason}),
  expectedAssertion:'GRAPH.FAILED_REFRESH',expectedCode:'invalidRecord',expectedField:'edges[0].boundaryReason'}
]);
for(const row of controls)test(`source baseline → single mutation → production check: ${row.id}`,async()=>{
 assert.equal(await runControl(row),row.id);
});
