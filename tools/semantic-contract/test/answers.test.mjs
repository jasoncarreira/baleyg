import test from 'node:test';
import assert from 'node:assert/strict';
import {materializeAnswers} from '../answers.mjs';
import {validate} from '../formats.mjs';

const sid = `sid:v1:${'a'.repeat(32)}`;
const oid = `occ:v1:${'b'.repeat(32)}`;
const hash = 'a'.repeat(64);
const document = {sourceSetId:'main',language:'javascript',path:'src/go.js'};
const header = {kind:'function',name:'go',modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]};
const declaration = {syntaxId:sid,document,revisionId:'r2',kind:'function',name:'go',lookupKey:'go',ancestors:[],
  key:{kind:'function',name:'go',signature:null,ordinal:0},range:{start:0,end:20},nameRange:{start:9,end:11},header,provenanceId:'native:r2:go'};
const call = {id:oid,ownerSyntaxId:sid,ordinal:0,document,revisionId:'r2',range:{start:12,end:16},
  calleeRange:{start:12,end:14},spelling:'go',regionIds:[],provenanceId:'native:r2:call'};
const provenance = {id:'native:r2:go',producerId:'native',document,revisionId:'r2',contentHash:hash,
  evidenceKind:'measuredSyntax',basis:null,freshness:'fresh'};
const coverage = {producerId:'native',language:'javascript',sourceSetId:'main',documentPath:'src/go.js',revisionId:'r2',
  requested:true,selected:true,state:'complete',supportedRoles:[],observedRoles:[],diagnostic:null};
const normalized = {identityMap:new Map([['source-function',sid],['measured-invocation',oid],['impostor',oid]]),
  recordMap:new Map([['source-function',declaration],['measured-invocation',call],['captured-coverage',coverage],
    ['captured-proof',provenance],['not-returned-proof',{...provenance,id:'unreturned'}]]),
  records:{declarations:[declaration],calls:[call],callBindings:[],coverage:[coverage],provenance:[provenance]}};
const request = {sourceSetId:'main',revisionId:'r2',rootSyntaxId:{ref:'source-function'},semanticProducerId:null,
  depth:2,maxNodes:150,maxCalls:500};
const result = {request:{...request},resolvedRevisionId:'r2',nodes:[{declaration:{recordRef:'source-function'},depth:0}],
  edges:[{call:{recordRef:'measured-invocation'},from:{ref:'source-function'},to:null,binding:null,
    visit:'boundary',boundaryReason:'missingEvidence'}],
  frontier:[{reason:'depth',nodeId:{ref:'source-function'},callId:{ref:'measured-invocation'},targetId:null,
    nextOrdinal:1,omittedCalls:0}],coverage:[{recordRef:'captured-coverage'}],
  provenance:[{recordRef:'captured-proof'}],partial:true,truncated:false,
  warnings:[{code:'syntaxOnly',message:'Author supplied this warning',provenanceId:'native:r2:go'}]};
const authored = {formatVersion:1,answers:[{id:'hand-authored-graph',attemptedRequest:request,
  answer:{ok:true,result}}]};
const clone = value => structuredClone(value);

test('authored source refs materialize only typed slots without computing traversal or rewriting arrays', () => {
  const source = clone(authored);
  const output = materializeAnswers(source, normalized);
  validate('AnswersV1', output);
  assert.equal(output.answers[0].attemptedRequest.rootSyntaxId, sid);
  assert.equal(output.answers[0].answer.result.edges[0].call.id, oid);
  assert.deepEqual(output.answers[0].answer.result.frontier,[{...result.frontier[0],nodeId:sid,callId:oid}]);
  assert.deepEqual(output.answers[0].answer.result.nodes,[{declaration,depth:0}]);
  assert.deepEqual(output.answers[0].answer.result.coverage,[coverage]);
  assert.deepEqual(output.answers[0].answer.result.provenance,[provenance]);
  assert.deepEqual(output.answers[0].answer.result.warnings,result.warnings);
  assert.equal(output.answers[0].answer.result.partial,true);
  assert.equal(output.answers[0].answer.result.truncated,false);
  assert.deepEqual(source,authored,'input remains authored and unchanged');
});

test('failure keeps invalid attempted request and authored error without graph calculation', () => {
  const input={formatVersion:1,answers:[{id:'bad-limit',attemptedRequest:{...request,depth:6},
    answer:{ok:false,error:{code:'invalidRequest',message:'depth out of bounds',field:'depth'}}}]};
  assert.deepEqual(materializeAnswers(input,normalized),{formatVersion:1,answers:[{
    ...input.answers[0],attemptedRequest:{...input.answers[0].attemptedRequest,rootSyntaxId:sid}}]});
});

test('missing, cross-kind and unreturned normalized proof references fail at precise fields', () => {
  const cases=[
    ['attemptedRequest.rootSyntaxId',{ref:'missing'},/unknown identity ref missing/],
    ['attemptedRequest.rootSyntaxId',{ref:'impostor'},/not SyntaxId/],
    ['answer.result.edges[0].call',{recordRef:'source-function'},/not Call/],
    ['answer.result.edges[0].call',{recordRef:'missing'},/unknown record ref missing/],
    ['answer.result.provenance[0]',{recordRef:'not-returned-proof'},/not a returned normalized Provenance/]
  ];
  for (const [field,value,reason] of cases) {
    const input=clone(authored);
    if (field==='attemptedRequest.rootSyntaxId') input.answers[0].attemptedRequest.rootSyntaxId=value;
    else if (field.includes('.call')) input.answers[0].answer.result.edges[0].call=value;
    else input.answers[0].answer.result.provenance[0]=value;
    assert.throws(()=>materializeAnswers(input,normalized),error => error.field===`AnswersInputV1.answers[0].${field}` && reason.test(error.message),field);
  }
});

test('refs cannot escape schema slots; malformed authored answer rejects before resolution', () => {
  const input=clone(authored);
  input.answers[0].answer.result.warnings[0].provenanceId={ref:'source-function'};
  assert.throws(()=>materializeAnswers(input,normalized),/FORMAT.SHAPE.*warnings\[0\].provenanceId/);
  const wrong=clone(authored);
  wrong.answers[0].answer.result.edges[0].call={ref:'measured-invocation'};
  assert.throws(()=>materializeAnswers(wrong,normalized),/FORMAT.SHAPE.*edges\[0\].call/);
});
