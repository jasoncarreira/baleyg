// Opt-in integration test: generates SCIP indexes. Not part of npm test.
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { extract } from './extract.mjs';
import { fixturePath } from './paths.mjs';
const hashes=JSON.parse(fs.readFileSync(fixturePath('fixture.hashes.json'),'utf8'));
const g=extract(fixturePath('fixture'),fixturePath('fixture.scip'),hashes).graph;

test('real reindex preserves an exported symbol across body edits but not a rename', ()=>{
  const dir=fs.mkdtempSync(path.join(os.tmpdir(),'baleyg-identity-'));
  try {
    fs.cpSync(fixturePath('fixture'),dir,{recursive:true});
    const original=g.nodes.find(n=>n.name==='transform');
    function reindex() {
      const output=path.join(dir,'index.scip');
      const result=spawnSync(process.execPath,[path.resolve('node_modules/@sourcegraph/scip-typescript/dist/src/main.js'),'index','--cwd',dir,'--output',output,'--no-progress-bar'],{encoding:'utf8',timeout:30000});
      assert.equal(result.status,0,result.stdout+result.stderr);
      const manifest=Object.fromEntries(fs.readdirSync(dir).filter(n=>n.endsWith('.js')||['package.json','tsconfig.json','jsconfig.json'].includes(n)).map(n=>[n,createHash('sha256').update(fs.readFileSync(path.join(dir,n))).digest('hex')]));
      return extract(dir,output,manifest).graph;
    }
    fs.appendFileSync(path.join(dir,'helpers.js'),'\n// body-location edit: index must refresh without renaming exported symbols\n');
    // A real body edit, not merely an appended comment.
    const helper=path.join(dir,'helpers.js');
    const before=fs.readFileSync(helper,'utf8');
    fs.writeFileSync(helper,before.replace('return value + 1;', 'return value + 2;'));
    assert.notEqual(fs.readFileSync(helper,'utf8'),before,'fixture body edit must actually apply');
    const edited=reindex();
    assert.equal(edited.nodes.find(n=>n.name==='transform').id,original.id);
    for(const name of ['flow.js','helpers.js']) {
      const file=path.join(dir,name);
      fs.writeFileSync(file,fs.readFileSync(file,'utf8').replace(/\btransform\b/g,'transformRenamed'));
    }
    const renamed=reindex();
    assert.ok(!renamed.nodes.some(n=>n.id===original.id));
    const target=renamed.nodes.find(n=>n.name==='transformRenamed');
    assert.ok(target);assert.notEqual(target.id,original.id);
    assert.ok(renamed.calls.filter(c=>c.calleeText==='transformRenamed').every(c=>c.target===target.id));
  } finally {fs.rmSync(dir,{recursive:true,force:true});}
});
