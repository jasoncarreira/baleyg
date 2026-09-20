import test from 'node:test';
import { fixturePath } from './paths.mjs';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { extract } from './extract.mjs';
const expected=JSON.parse(fs.readFileSync(fixturePath('fixture-expected.json'),'utf8'));
const hashes=JSON.parse(fs.readFileSync(fixturePath('fixture.hashes.json'),'utf8'));
const get=()=>extract(fixturePath('fixture'),fixturePath('fixture.scip'),hashes).graph;
const g=get();
const byId=new Map(g.nodes.map(n=>[n.id,n]));

test('hand-labelled fixture: exact call counts and semantic targets',()=>{
  assert.equal(g.stats.parseErrors,0);
  assert.equal(g.calls.length,expected.expected_total_callsites);
  for(const a of expected.assertions) {
    const matches=g.calls.filter(c=>c.path===a.file && byId.get(c.caller)?.name===a.caller && c.calleeText===a.callee);
    assert.equal(matches.length,a.expected_count,JSON.stringify(a));
    for(const c of matches) {
      if(a.resolved_target) {
        const target=byId.get(c.target);
        assert.ok(target,`missing internal target ${JSON.stringify(c)}`);
        assert.equal(`${target.path}#${target.name}`,a.resolved_target);
      } else {assert.equal(c.target,null);assert.equal(c.resolution,'unresolved');}
    }
  }
  for(const [qualified,count] of Object.entries(expected.expected_callsites_by_caller)) {
    assert.equal(g.calls.filter(c=>`${c.path}#${byId.get(c.caller)?.name}`===qualified).length,count,qualified);
  }
});
test('callback reference is symbol-resolved but is not a call',()=>{
  const owner=g.nodes.find(n=>n.name==='callbackReference');
  const target=g.nodes.find(n=>n.name==='transform');
  const refs=g.references.filter(r=>r.path===owner.path && r.symbol===target.id && r.range[0]>=owner.line-1 && r.range[0]<owner.endLine);
  assert.equal(refs.length,1);
  const call=g.calls.find(c=>c.caller===owner.id);
  assert.deepEqual(call.callbackArguments,[target.id]);
});
test('repeated call sites have unique identities and distinct ordinals',()=>{
  const matches=g.calls.filter(c=>byId.get(c.caller)?.name==='repeated');
  assert.equal(new Set(matches.map(c=>c.id)).size,2);
  assert.deepEqual(matches.map(c=>c.ordinal),[1,2]);
});
test('nested branch, else and loop regions remain distinguishable',()=>{
  const calls=g.calls.filter(c=>byId.get(c.caller)?.name==='branchLoop');
  const regions=new Map(g.regions.map(r=>[r.id,r]));
  assert.deepEqual(calls.map(c=>c.regions.map(id=>regions.get(id).kind)),[['if','if'],['if','else'],['else'],['loop']]);
  assert.notEqual(calls[1].regions[1],calls[2].regions[0]);
  const guarded=g.calls.filter(c=>byId.get(c.caller)?.name==='guarded');
  assert.ok(guarded[0].regions.some(id=>regions.get(id).kind==='try_statement'));
  assert.ok(guarded[1].regions.some(id=>regions.get(id).kind==='catch_clause'));
});
test('same inputs produce byte-identical normalized graphs',()=>{
  assert.equal(JSON.stringify(g),JSON.stringify(get()));
});
test('input edit invalidates all SCIP resolutions; original index stays intact',()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'baleyg-stale-'));
  try {
    fs.cpSync(fixturePath('fixture'),dir,{recursive:true});
    fs.appendFileSync(path.join(dir,'helpers.js'),'\n// changed source\n');
    const stale=extract(dir,fixturePath('fixture.scip'),hashes).graph;
    assert.equal(stale.stats.semanticFresh,false);
    assert.deepEqual(stale.stats.changedFiles,['helpers.js']);
    assert.equal(stale.calls.length,expected.expected_total_callsites);
    assert.ok(stale.calls.every(c=>c.target===null && c.resolution==='unresolved'));
    assert.equal(get().stats.semanticFresh,true);
  } finally {fs.rmSync(dir,{recursive:true,force:true});}
});
test('reference enclosing ranges cannot supply callers for this indexer',()=>{
  assert.equal(g.stats.referenceEnclosingRanges,0);
  assert.ok(g.stats.definitionEnclosingRanges>0);
});
test('real imported cross-file calls resolve in feature-factory',()=>{
  const graph=extract(fixturePath('inputs/feature-factory'),fixturePath('feature-factory.scip'),JSON.parse(fs.readFileSync(fixturePath('feature-factory.hashes.json'),'utf8'))).graph;
  const ids=new Map(graph.nodes.map(n=>[n.id,n]));
  for(const [caller,callee,target] of [['readRun','validateRun','validateRun'],['transition','coordinateRunJsonTransition','coordinateRunJsonTransition'],['coordinateRunJsonTransition','withRunJsonLock','withRunJsonLock']]) {
    const call=graph.calls.find(c=>ids.get(c.caller)?.name===caller&&c.calleeText===callee);
    assert.equal(call?.resolution,'internal');
    assert.equal(ids.get(call.target)?.name,target);
  }
  const coordinate=graph.nodes.find(n=>n.name==='coordinateRunJsonTransition');
  const lock=graph.calls.find(c=>c.caller===coordinate.id&&c.calleeText==='withRunJsonLock');
  assert.equal(lock.callbackArguments.length,1);
  assert.ok(graph.calls.some(c=>c.caller===lock.callbackArguments[0]));
});

test('added, deleted, and configuration-changed inputs cannot retain a fresh index',()=>{
  for(const scenario of ['add','delete','config']) {
    const dir=fs.mkdtempSync(path.join(os.tmpdir(),'baleyg-input-'));
    try {
      fs.cpSync(fixturePath('fixture'),dir,{recursive:true});
      if(scenario==='add') fs.writeFileSync(path.join(dir,'added.js'),'export function added() { unknown(); }');
      if(scenario==='delete') fs.rmSync(path.join(dir,'helpers.js'));
      if(scenario==='config') fs.appendFileSync(path.join(dir,'tsconfig.json'),' ');
      const changed=extract(dir,fixturePath('fixture.scip'),hashes).graph;
      assert.equal(changed.stats.semanticFresh,false,scenario);
      assert.ok(changed.calls.every(c=>c.target===null),scenario);
      assert.ok(changed.references.every(r=>r.stale),scenario);
      if(scenario==='add') assert.ok(changed.nodes.some(n=>n.name==='added'));
      if(scenario==='delete') assert.ok(changed.files.every(f=>f.path!=='helpers.js'));
    } finally {fs.rmSync(dir,{recursive:true,force:true});}
  }
});
test('accessors, computed keys, guarded arms, loop initialization and wrapped callbacks',()=>{
  const edge=extract(fixturePath('edge-fixture'),fixturePath('edge-fixture.scip'),JSON.parse(fs.readFileSync(fixturePath('edge-fixture.hashes.json'),'utf8'))).graph;
  const nodes=new Map(edge.nodes.map(n=>[n.id,n]));
  const regions=new Map(edge.regions.map(r=>[r.id,r]));
  const getterCall=edge.calls.find(c=>c.calleeText==='obj.f');
  assert.equal(getterCall.resolution,'unresolved');assert.equal(getterCall.target,null);
  const computed=edge.calls.find(c=>c.calleeText==='key');
  assert.equal(nodes.get(computed.caller).kind,'module');
  const branch=edge.calls.filter(c=>nodes.get(c.caller)?.name==='branching');
  const kinds=c=>c.regions.map(id=>regions.get(id).kind);
  assert.deepEqual(kinds(branch[0]),['conditional-true']);
  assert.deepEqual(kinds(branch[1]),['conditional-false']);
  assert.deepEqual(kinds(branch[2]),['short-circuit']);
  assert.deepEqual(kinds(branch[3]),[]); // for initializer is once, not loop body
  assert.deepEqual(kinds(branch[4]),['loop']);
  const callbacks=edge.calls.filter(c=>nodes.get(c.caller)?.name==='callbacks');
  assert.equal(callbacks.length,2);
  assert.ok(callbacks.every(c=>c.callbackArguments.length===1));
});

