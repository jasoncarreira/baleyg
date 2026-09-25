import test from 'node:test';
import assert from 'node:assert/strict';
import {checkCounts} from '../counts.mjs';
import {registerControls,runControl} from './mutations.mjs';

const context={sourceSetId:'main',revisionId:'r1',producers:[]};
const fixture=(profile='example',language='javascript')=>({fixture:{profile,language},root:profile==='example'?'/tmp/example':'/tmp/javascript',
 native:{declarations:[],calls:[],controls:[],references:[]},annotations:[],sources:new Map(),semanticProofs:new Map(),
 dispositions:{formatVersion:1,assertions:[],callableValueNegatives:[]}});
const records=()=>({formatVersion:1,comparison:context,producers:[],sourceSets:[],revisions:[],coverage:[],provenance:[],
 declarations:[],symbols:[],declarationBindings:[],typeRelationships:[],calls:[],controlRegions:[],references:[],
 referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]});

test('small literal example keeps every count check but bypasses corpus floors',()=>{
 const actual=checkCounts(fixture(),records());
 assert.equal(actual.scenariosTotal,0);
 assert.equal(actual.measuredCalls,0);
 assert.equal(actual.references,0);
 assert.equal(actual.floorsEnforced,false);
 assert.deepEqual(actual.scenariosByCategory.map(x=>x.count),Array(8).fill(0));
 assert.deepEqual(actual.outcomes.map(x=>x.count),Array(5).fill(0));
});
test('corpus profile enforces each floor',()=>{
 assert.throws(()=>checkCounts(fixture('corpus'),records()),e=>e.assertion==='COUNT.FLOOR'&&e.field==='counts');
});
test('an example profile outside literal example/ is rejected',()=>{
 const loaded=fixture();loaded.root='/tmp/corpus';
 assert.throws(()=>checkCounts(loaded,records()),e=>e.assertion==='COUNT.PROFILE'&&e.field==='profile');
});
const rows=registerControls([{id:'COUNT.floor.corpus',baseline:()=>({loaded:fixture(),records:records()}),
 mutate:x=>{x.loaded.fixture.profile='corpus';return x;},
 check:x=>checkCounts(x.loaded,x.records),expectedAssertion:'COUNT.FLOOR',expectedCode:'invalidRecord',expectedField:'counts'}]);
test('count assertion mutation control',async()=>{for(const row of rows)await runControl(row);});

function measuredExample() {
 const loaded=fixture(),out=records(),doc={sourceSetId:'main',language:'javascript',path:'a.js'},raw=Buffer.from('function f(){ value; }');
 const range={encoding:'utf8',start:14,end:19},byteRange={start:14,end:19};
 const reference={ref:'native-value',nativeId:null,document:doc,revisionId:'r1',ownerRef:'owner',range,spelling:'value',witnesses:[]};
 loaded.sources.set(JSON.stringify(['main','r1','a.js']),raw);
 loaded.native.references.push(reference);
 const targetId=`sid:v1:${'3'.repeat(32)}`;
 const target={kind:'internal',syntaxId:targetId,document:doc,revisionId:'r1'};
 out.declarations.push({syntaxId:targetId,document:doc,revisionId:'r1',kind:'function',name:'value',lookupKey:'value',ancestors:[],
  key:{kind:'function',name:'value',signature:null,ordinal:0},range:{start:0,end:22},nameRange:{start:14,end:19},
  header:{kind:'function',name:'value',modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]},provenanceId:'native:r1:target'});
 const proof={id:'p1',producerId:'semantic',document:doc,revisionId:'r1',evidenceKind:'semanticReference'};
 out.provenance.push({...proof,contentHash:'a'.repeat(64),basis:null,freshness:'fresh'});
 loaded.semanticProofs.set('p1',{factRef:'fact-value'});
 const anchor={document:doc,revisionId:'r1',contentHash:'a'.repeat(64),kind:'reference',range,ownerRef:'owner'};
 const fact={kind:'reference',ref:'fact-value',anchor,record:{provenanceId:'p1'}};
 loaded.annotations.push({document:doc,revisionId:'r1',scenarios:[{id:'callable-read',category:'callableValues',anchors:[anchor],factRefs:['fact-value']}],facts:[fact]});
 out.references.push({id:`occ:v1:${'1'.repeat(32)}`,ownerSyntaxId:`sid:v1:${'2'.repeat(32)}`,ordinal:0,document:doc,revisionId:'r1',range:byteRange,
  spelling:'value',lookupKey:'value',site:'use',roles:['read'],resolution:'resolved',declaredTarget:target,candidates:[],provenanceId:'p1'});
 loaded.dispositions.callableValueNegatives.push({scenarioId:'callable-read',referenceRef:'fact-value',ownerRef:'owner',range});
 loaded.dispositions.assertions.push({kind:'resolution',factRef:'fact-value',disposition:'resolved'});
 return {loaded,out};
}
test('count installed reference once; its callable read is not a measured call',()=>{
 const {loaded,out}=measuredExample(),counts=checkCounts(loaded,out);
 assert.equal(counts.scenariosByCategory.find(x=>x.category==='callableValues').count,1);
 assert.equal(counts.references,1);
 assert.equal(counts.callableValueNegatives,1);
 assert.equal(counts.measuredCalls,0);
 assert.equal(counts.outcomes.find(x=>x.disposition==='resolved').count,1);
 assert.deepEqual(counts.observedRoles,['read']);
});
test('reject wrong resolution and an invented measured call',()=>{
 const {loaded,out}=measuredExample();loaded.dispositions.assertions[0].disposition='unresolved';
 assert.throws(()=>checkCounts(loaded,out),e=>e.assertion==='COUNT.DISPOSITION'&&e.field==='assertions');
 const copy=measuredExample();copy.out.calls.push({id:'invented'});
 assert.throws(()=>checkCounts(copy.loaded,copy.out));
});
const evidenceRows=registerControls([{id:'COUNT.disposition.proof',baseline:measuredExample,
 mutate:x=>{x.loaded.dispositions.assertions[0].disposition='unresolved';return x;},
 check:x=>checkCounts(x.loaded,x.out),expectedAssertion:'COUNT.DISPOSITION',expectedCode:'invalidRecord',expectedField:'assertions'}]);
test('proof-backed resolution mutation control',async()=>{for(const row of evidenceRows)await runControl(row);});
