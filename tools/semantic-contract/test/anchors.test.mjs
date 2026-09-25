import test from 'node:test';
import assert from 'node:assert/strict';
import {checkAnchors} from '../record-check/anchors.mjs';
import {measuredHeaderHash,measuredSiblingGroupHash} from '../record-check/measurement.mjs';
import {registerControls,runControl} from './mutations.mjs';

const document={sourceSetId:'core',language:'java',path:'src/A.java'};
const id=n=>`sid:v1:${n.toString(16).padStart(32,'0')}`;
const header=name=>({kind:'method',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]});
const h0=header('method'), hx=header('different');
const row=(ref,revisionId,syntaxId,head)=>({ref,revisionId,document,syntaxId,header:head});
function specimen({captured=[row('old','r1',id(0),h0)],current=[row('new','r2',id(0),h0)],
 state='unknown',expected='none',currentRevisionId=current[0].revisionId}={}) {
 const all=currentRevisionId==='r1'?captured:[...captured,...current],byRef=new Map(all.map(x=>[x.ref,x]));
 const groups=new Map();
 for(const group of [captured,current])for(const member of group)
  groups.set(member.ref,{memberRefs:group.map(x=>x.ref),headers:group.map(x=>measuredHeaderHash(x.header))});
 const hash=measuredHeaderHash(h0),headers=captured.map(x=>measuredHeaderHash(x.header));
 const durable={syntaxId:id(0),document,capturedRevisionId:'r1',headerHash:hash,
  siblingGroupHash:measuredSiblingGroupHash(headers),siblingCount:headers.length,
  identicalHeaderCount:headers.filter(x=>x===hash).length};
 const result={status:expected==='none'?'attached':'orphaned',targetId:expected==='none'?id(0):null,reason:expected};
 const continuity={fromRevisionId:'r1',toRevisionId:currentRevisionId,state,evidence:state==='unknown'?null:'independent member/order proof'};
 const loaded={native:{declarations:all},comparison:{revisionId:currentRevisionId},
  revisions:new Map(['r1','r2'].map(rev=>[JSON.stringify(['core',rev]),{documents:[{key:document}]}])),
  anchors:{cases:[{id:'A',capturedDeclarationRef:'old',currentRevisionId,continuity,expectedResult:result}]}};
 const records={durableAnchors:[durable],groupContinuities:[continuity],anchorResults:[result]};
 const measurement={recordByNativeRef:byRef,groupsByDeclarationRef:groups,
  identityByRef:new Map(all.map(x=>[x.ref,x.syntaxId]))};
 return {loaded,records,measurement};
}
const cases=[
 ['unique body edit',{},'none'],
 ['duplicate body edit with proven continuity',{captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('new','r2',id(0),h0),row('peer2','r2',id(1),h0)],state:'unchanged'},'none'],
 ['inserted earlier same-name sibling changes focused header',{current:[row('new','r2',id(0),hx),row('moved','r2',id(2),h0)],state:'changed'},'headerMismatch'],
 ['different-header sibling inserted after unique member',{current:[row('new','r2',id(0),h0),row('extra','r2',id(3),hx)],state:'changed'},'none'],
 ['duplicate count changes',{captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('new','r2',id(0),h0),row('peer2','r2',id(1),h0),row('extra','r2',id(2),h0)],state:'changed'},'groupChanged'],
 ['same-count duplicate membership unknown',{captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('new','r2',id(0),h0),row('peer2','r2',id(1),h0)]},'unprovenContinuity'],
 ['same-count duplicate membership changed',{captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('new','r2',id(0),h0),row('peer2','r2',id(1),h0)],state:'changed'},'groupChanged'],
 ['same-revision duplicate ignores contradictory continuity state',{captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],state:'changed'},'none'],
 ['missing old identity never name-searches',{current:[row('replacement','r2',id(4),h0)]},'missing']
];
for(const [name,inputs,reason] of cases)test(name,()=>{
 const value=specimen({...inputs,expected:reason});
 assert.deepEqual(checkAnchors(value.loaded,value.records,value.measurement).anchorResults,[value.records.anchorResults[0]]);
});
const duplicate={captured:[row('old','r1',id(0),h0),row('peer1','r1',id(1),h0)],current:[row('new','r2',id(0),h0),row('peer2','r2',id(1),h0)]};
const controls=registerControls([
 {id:'ANCHOR.unique.changedSibling',baseline:()=>specimen({current:[row('new','r2',id(0),h0),row('extra','r2',id(3),hx)],state:'changed'}),
  mutate:value=>{value.records.anchorResults[0]={status:'orphaned',targetId:null,reason:'groupChanged'};value.loaded.anchors.cases[0].expectedResult=value.records.anchorResults[0];return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.RESULT',expectedCode:'invalidRecord',expectedField:'expectedResult'},
 {id:'ANCHOR.duplicate.unknown',baseline:()=>specimen({...duplicate,expected:'unprovenContinuity'}),
  mutate:value=>{value.records.anchorResults[0]={status:'attached',targetId:id(0),reason:'none'};value.loaded.anchors.cases[0].expectedResult=value.records.anchorResults[0];return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.RESULT',expectedCode:'invalidRecord',expectedField:'expectedResult'},
 {id:'ANCHOR.duplicate.changed',baseline:()=>specimen({...duplicate,state:'changed',expected:'groupChanged'}),
  mutate:value=>{value.loaded.anchors.cases[0].expectedResult.reason='unprovenContinuity';return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.RESULT',expectedCode:'invalidRecord',expectedField:'expectedResult'},
 {id:'ANCHOR.missing.old',baseline:()=>specimen({current:[row('replacement','r2',id(4),h0)],expected:'missing'}),
  mutate:value=>{value.loaded.anchors.cases[0].expectedResult={status:'attached',targetId:id(4),reason:'none'};return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.RESULT',expectedCode:'invalidRecord',expectedField:'expectedResult'},
 {id:'ANCHOR.continuity.unchangedWithoutEvidence',baseline:()=>specimen({...duplicate,state:'unchanged'}),
  mutate:value=>{value.loaded.anchors.cases[0].continuity.evidence=null;value.records.groupContinuities[0].evidence=null;return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.CONTINUITY',expectedCode:'invalidRecord',expectedField:'continuity.evidence'},
 {id:'ANCHOR.continuity.unknownWithEvidence',baseline:()=>specimen({...duplicate,expected:'unprovenContinuity'}),
  mutate:value=>{value.loaded.anchors.cases[0].continuity.evidence='unrelated assertion';value.records.groupContinuities[0].evidence='unrelated assertion';return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.CONTINUITY',expectedCode:'invalidRecord',expectedField:'continuity.evidence'},
 {id:'ANCHOR.inventory.hash',baseline:()=>specimen(),
  mutate:value=>{value.records.durableAnchors[0].siblingGroupHash='0'.repeat(64);return value;},
  check:value=>checkAnchors(value.loaded,value.records,value.measurement),expectedAssertion:'ANCHOR.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'durableAnchors'}
]);
for(const control of controls)test(control.id,()=>runControl(control));
