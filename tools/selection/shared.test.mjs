import { extractionPath, researchPath } from './paths.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {makePacket,baseline,validateDecisions,assembleView,LABELS} from './shared.mjs';
import {requestFor,parseResponse} from './jev.mjs';
import {reserve,settle} from './budget.mjs';
const graph=JSON.parse(fs.readFileSync(extractionPath('feature-factory.graph.json'),'utf8'));
const questions=JSON.parse(fs.readFileSync(researchPath('questions.json'),'utf8'));
const packet=makePacket(graph,questions[0]);
test('provider packets exclude human rubric and remain deterministic',()=>{
 assert.deepEqual(makePacket(graph,questions[0]),packet);
 assert.ok(!('must_show' in packet));assert.ok(!('distractors' in packet));
 assert.ok(packet.candidates.some(c=>c.id===packet.seedId));
 assert.equal(new Set(packet.candidates.map(c=>c.id)).size,packet.candidates.length);
});
test('validators reject invented IDs, duplicate decisions, missing candidates and invalid labels',()=>{
 const good=baseline(packet);validateDecisions(packet,good);
 assert.throws(()=>validateDecisions(packet,[...good,{candidateId:'invented',relevance:'essential'}]));
 assert.throws(()=>validateDecisions(packet,[good[0],...good]));
 assert.throws(()=>validateDecisions(packet,good.slice(1)));
 assert.throws(()=>validateDecisions(packet,good.map((d,i)=>i===0?{...d,relevance:'maybe'}:d)));
 assert.throws(()=>validateDecisions(packet,good.map((d,i)=>i===0?{...d,explanation:'extra field'}:d)));
});
test('assembly retains seed, never invents edges, and flags display overload rather than silently truncating',()=>{
 const hidden=packet.candidates.map(c=>({candidateId:c.id,relevance:'incidental'}));
 const view=assembleView(packet,hidden);assert.equal(view.visible.length,1);assert.equal(view.seedForced,true);assert.ok(!('edges' in view));
 const all=assembleView(packet,packet.candidates.map(c=>({candidateId:c.id,relevance:'essential'})));
 assert.equal(all.visible.length,packet.candidates.length);assert.equal(all.overDisplayBudget,true);
});
test('Jev request includes visible candidate identity in instructions and separate calibrated values',()=>{
 const body=requestFor(packet);assert.equal(body.model,'jev-1.13.0');
 assert.ok(Object.entries(body.questions).every(([id,q])=>q.instructions.includes(id)&&q.type==='choice'));
 const answers=Object.fromEntries(packet.candidates.map(c=>[c.id,{type:'choice',choice:'supporting',probabilities:{essential:.1,supporting:.7,incidental:.1,uncertain:.1},confidence:.6}]));
 const parsed=parseResponse(packet,{model:'jev-1.13.0',answers});assert.equal(parsed.decisions.length,packet.candidates.length);
 assert.equal(parsed.probabilities[packet.seedId].confidence,.6);
 assert.equal(parsed.probabilities[packet.seedId].distribution.supporting,.7);
 answers[packet.seedId].probabilities.supporting=4;
 assert.throws(()=>parseResponse(packet,{model:'jev-1.13.0',answers}));
});
test('budget fails closed on concurrent lock, repeated reservation, excess spend and unknown costs',()=>{
 const dir=fs.mkdtempSync(path.join(os.tmpdir(),'baleyg-budget-')),ledger=path.join(dir,'ledger.json');
 try {
  reserve(ledger,{id:'a',provider:'test',maxUsd:2});
  assert.throws(()=>reserve(ledger,{id:'a',provider:'test',maxUsd:1}));
  settle(ledger,'a');
  assert.throws(()=>reserve(ledger,{id:'b',provider:'test',maxUsd:9}));
  fs.writeFileSync(ledger+'.lock','');assert.throws(()=>reserve(ledger,{id:'b',provider:'test',maxUsd:1}));fs.unlinkSync(ledger+'.lock');
  reserve(ledger,{id:'c',provider:'test',maxUsd:1});
  settle(ledger,'c',{actualUsd:2,note:'test overrun'});
  assert.throws(()=>reserve(ledger,{id:'d',provider:'test',maxUsd:.1}));
 } finally {fs.rmSync(dir,{recursive:true,force:true});}
});
