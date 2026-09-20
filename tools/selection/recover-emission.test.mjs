import { fixturePath } from './paths.mjs';
import test from 'node:test';import assert from 'node:assert/strict';import fs from 'node:fs';
import {recoverEmission} from './recover-emission.mjs';import {digest,baseline} from './shared.mjs';import {renderComparison} from './compare.mjs';
const p=JSON.parse(fs.readFileSync(fixturePath('inputs/q01-smoke.json'),'utf8'));
const ds=baseline(p);
const failed={status:'failed',graphHash:p.graphHash,questionId:p.questionId,packetHash:digest(p),provider:'partial',toolCallAudit:{names:['mcp__selection__emit_view'],count:1}};
test('accepted emission stays distinct from successful terminal completion',()=>{
 const result=recoverEmission(p,failed,{decisions:ds});assert.equal(result.status,'validated-emission-terminal-error');assert.equal(result.terminalStatus,'failed');assert.equal(result.decisions.length,p.candidates.length);
 assert.throws(()=>recoverEmission(p,{...failed,packetHash:'wrong'},{decisions:ds}));
 assert.throws(()=>recoverEmission(p,{...failed,toolCallAudit:{names:[]}},{decisions:ds}));
 assert.throws(()=>recoverEmission(p,failed,{decisions:ds.slice(1)}));
});
test('failed panel is not fabricated as an empty selection',()=>{
 const valid=recoverEmission(p,failed,{decisions:ds});
 const html=renderComparison(p,[{...valid,provider:'one'},{...valid,provider:'two'},{...failed,provider:'three'}]).html;
 assert.ok(html.includes('No validated selection was produced.'));assert.ok(html.includes('Do not score this panel'));
 assert.ok(html.includes('Run caveat:'));
});
