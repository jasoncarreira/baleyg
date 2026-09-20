// Dependency-free static traversal checks: node tools/extraction/viewer-smoke.mjs
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
const html = readFileSync(new URL('./viewer.html', import.meta.url), 'utf8');
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
// Load pure traversal functions without the page bootstrap. Browser interactions
// are checked separately in a real browser, not simulated by these checks.
const core = script.slice(0, script.indexOf("$('graph-choice').addEventListener"));
const run = test => runInNewContext(core + '\n' + test, { document: {} });
run(`
 nodes = new Map(['root','a','b','c','d','callback'].map(id => [id,{name:id}]));
 regions = new Map([['r',{kind:'branch',label:'if ready',parent:null}]]);
 const call=(id,caller,target,extra={})=>({id,caller,target,calleeText:target,resolution:'internal',regions:[],callbackArguments:[],...extra});
 byCaller = new Map([
 ['root',[call('one','root','a',{callbackArguments:['callback']}),call('two','root',null,{resolution:'external'})]],
 ['a',[call('recur','a','root'),call('three','a','b')]],
 ['b',[call('four','b','c')]],['c',[call('five','c','d')]],['d',[call('hidden-depth','d',null)]],
 ['callback',[call('hidden-callback','callback',null)]]]);
 const result=sequence('root');
 if(result.rows.map(r=>r.call.id).join(',')!=='one,recur,three,four,five,two')throw Error('Unexpected walk order');
 if(!result.rows[0].notes.join().includes('Callback boundary'))throw Error('Missing callback boundary');
 if(!result.rows[1].notes.join().includes('Recursion boundary'))throw Error('Missing recursion boundary');
 if(!result.rows[4].notes.join().includes('Depth boundary'))throw Error('Missing depth boundary');
 if(context({regions:['r']})!=='branch: if ready')throw Error('Missing control context');
 byCaller = new Map([['root',Array.from({length:65},(_,i)=>call(String(i),'root',null,{resolution:'external'}))]]);
 const bounded=sequence('root');if(bounded.rows.length!==60||!bounded.truncated)throw Error('Cap not enforced');
`);
assert.ok(!script.includes('innerHTML'), 'Graph strings must not be inserted as HTML');
assert.ok(html.includes('not an execution trace'));
console.log('PASS: source walk order, callbacks, recursion, depth, regions, 60-message cap, safe text rendering');
