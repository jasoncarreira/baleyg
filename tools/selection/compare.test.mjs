import { fixturePath } from './paths.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { assembleView } from './shared.mjs';
import { measuredEdges, renderComparison, validateInputs, writeComparison } from './compare.mjs';
const root=path.dirname(fileURLToPath(import.meta.url));
function fixture() {
  const packet={schemaVersion:1,questionId:'q',question:'What does <entry> do?',graphHash:'a'.repeat(64),seedId:'c1',candidates:['c1','c2','c3'].map((id,i)=>({id,symbolId:`symbol-${id}`,name:`function${i}`,path:'src/<x>.ts',startLine:1,endLine:5,source:'const x = "<script>alert(1)</script> & hi";',calls:[]}))};
  packet.candidates[0].calls=[{id:'direct-hidden',line:2,callee:'middle',target:'c2',resolution:'internal',callbackArguments:[]},{id:'unknown',line:3,callee:'external(callback)',target:null,resolution:'external',callbackArguments:['c3']}];
  packet.candidates[1].calls=[{id:'hidden-visible',line:4,callee:'last',target:'c3',resolution:'internal',callbackArguments:[]}];
  const decisions=[{candidateId:'c1',relevance:'uncertain'},{candidateId:'c2',relevance:'supporting'},{candidateId:'c3',relevance:'essential'}];
  return {packet,results:['baseline','jev','claude-acp'].map(provider=>({provider,graphHash:packet.graphHash,questionId:'q',decisions:structuredClone(decisions),view:assembleView(packet,decisions),rawResponse:'PROVIDER SECRET',model:'SECRET MODEL'}))};
}
test('never invents transitive, unresolved or callback call edges; preserves boundaries',()=>{
  const {packet,results}=fixture();
  assert.deepEqual(measuredEdges(packet,['c1','c3']),[]);
  const {html}=renderComparison(packet,results);
  assert.equal((html.match(/<path data-call=/g)||[]).length,0);
  assert.match(html,/direct-hidden, line 2/);
  assert.match(html,/unknown, line 3/);
  assert.match(html,/Callback references — not calls/);
  assert.match(html,/forced seed/);
  packet.candidates[0].calls.push({id:'real-direct',line:5,callee:'last',target:'c3',resolution:'internal',callbackArguments:[]});
  assert.deepEqual(measuredEdges(packet,['c1','c3']),[{from:'c1',to:'c3',callId:'real-direct',line:5}]);
});
test('escapes source and question; provider metadata stays outside HTML',()=>{
  const {packet,results}=fixture();
  const {html,key}=renderComparison(packet,results);
  assert.match(html,/&lt;script&gt;alert\(1\)&lt;\/script&gt; &amp; hi/);
  assert.match(html,/What does &lt;entry&gt; do\?/);
  assert.doesNotMatch(html,/<script>|baseline|jev|claude|PROVIDER SECRET|SECRET MODEL/);
  assert.deepEqual(Object.keys(key.panels),['A','B','C']);
  assert.deepEqual(Object.values(key.panels).sort(),['baseline','claude-acp','jev']);
});
test('rejects stale hash, wrong question, malformed decisions and inconsistent view',()=>{
  for(const mutate of [
    ({packet})=>packet.graphHash='bad',
    ({results})=>results[0].graphHash='b'.repeat(64),
    ({results})=>results[0].questionId='other',
    ({results})=>results[0].decisions.pop(),
    ({results})=>results[0].view.visible.push('unknown'),
    ({results})=>results[0].view.graphHash='b'.repeat(64),
    ({packet})=>packet.candidates[0].calls[0].target='missing',
  ]) {const data=fixture();mutate(data);assert.throws(()=>validateInputs(data.packet,data.results));}
});
test('CLI writer uses ignored outputs, private separate mapping, and refuses overwrite',()=>{
  const dir=fs.mkdtempSync(fixturePath('outputs/compare-test-'));
  try {
    const {packet,results}=fixture();
    const packetFile=path.join(dir,'packet.json');fs.writeFileSync(packetFile,JSON.stringify(packet));
    const files=results.map((r,i)=>{const f=path.join(dir,`${i}.json`);fs.writeFileSync(f,JSON.stringify(r));return f;});
    const output=path.join(dir,'review.html');
    writeComparison(packetFile,output,files);
    assert.match(fs.readFileSync(output,'utf8'),/Panel A/);
    assert.equal(JSON.parse(fs.readFileSync(path.join(dir,'blind-key.json'),'utf8')).graphHash,packet.graphHash);
    assert.equal(fs.statSync(path.join(dir,'blind-key.json')).mode & 0o777,0o600);
    assert.throws(()=>writeComparison(packetFile,output,files),/EEXIST/);
    assert.throws(()=>writeComparison(packetFile,path.join(root,'review.html'),files),/Output directory/);
  } finally {fs.rmSync(dir,{recursive:true,force:true});}
});
