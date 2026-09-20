"use strict";
// Synthetic data only. Run: node --test tests/question-ui.test.cjs
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../web/app.js"), "utf8");

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return {promise, resolve, reject};
}
function harness() {
  const elements = new Map(), blobs = [], downloads = [], timers = new Map();
  let nextTimer = 0;
  function node(tagName) {
    const classes = new Set();
    return {tagName, classList: {add(value) { classes.add(value); }, contains(value) { return classes.has(value); }}, focus() {}, hidden: true, disabled: false, value: "", textContent: "", children: [], listeners: {},
      addEventListener(type, handler) { this.listeners[type] = handler; },
      replaceChildren(...items) { this.children = items; },
      append(...items) { this.children.push(...items); },
      querySelector() { return null; },
      setAttribute() {}, remove() {},
      click() { downloads.push(this.download); }};
  }
  function get(id) { if (!elements.has(id)) elements.set(id, node()); return elements.get(id); }
  const context = vm.createContext({console, TextEncoder, DOMException, Blob, AbortController,
    setTimeout(callback, delay) { const id = ++nextTimer; timers.set(id, {callback, delay}); return id; }, clearTimeout(id) { timers.delete(id); },
    window: {confirm: () => true},
    URL: {createObjectURL(blob) { blobs.push(blob); return "blob:synthetic"; }, revokeObjectURL() {}},
    document: {getElementById: get, createElement: node, createDocumentFragment: node, createTextNode: text => ({textContent:text}), body: node()},
    fetch() { throw new Error("Unexpected request"); }});
  const run = code => vm.runInContext(code, context);
  run(source);
  run(`token = 'synthetic'; seed = 'root'; status = {revision:1};`);
  const preserveNewFocus = () => {
    run(`querySerial++; questionSerial++; status = {revision:status.revision+1};
      packet = {packetId:'new'}; focused = {marker:'new'};
      $('focus-state').textContent = 'new valid focus'; $('error').hidden = true;`);
    return run("packet");
  };
  const assertPreserved = newer => {
    assert.equal(run("packet"), newer);
    assert.equal(get("focus-state").textContent, "new valid focus");
    assert.equal(get("error").hidden, true);
  };
  return {context, run, get, blobs, downloads, timers, preserveNewFocus, assertPreserved};
}

for (const [name, route, status] of [
  ["preview", "/api/questions/preview", 422],
  ["import", "/api/questions/p/jev-response", 409],
  ["export", "/api/questions/p/jev-request", 422],
  ["branch/query", "/api/query", 409],
  ["source", "/api/source?path=synthetic&revision=1", 409],
]) {
  test(`obsolete ${name} HTTP ${status} cannot replace a newer focused packet`, async () => {
    const h = harness(), response = deferred();
    h.context.fetch = () => response.promise;
    const pending = h.run(`perform(() => api(${JSON.stringify(route)}))`);
    const newer = h.preserveNewFocus();
    response.resolve({status, ok: false, json: async () => ({error: {message: "obsolete failure"}})});
    await pending;
    h.assertPreserved(newer);
  });
}

test("obsolete network rejection is silent", async () => {
  const h = harness(), response = deferred();
  h.context.fetch = () => response.promise;
  const pending = h.run(`perform(() => api('/api/questions/preview'))`);
  const newer = h.preserveNewFocus();
  response.reject(new Error("obsolete network failure"));
  await pending;
  h.assertPreserved(newer);
});

test("obsolete response JSON rejection is silent", async () => {
  const h = harness(), json = deferred(), entered = deferred();
  h.context.fetch = async () => ({status: 422, ok: false, json() { entered.resolve(); return json.promise; }});
  const pending = h.run(`perform(() => api('/api/questions/preview'))`);
  await entered.promise;
  const newer = h.preserveNewFocus();
  json.reject(new Error("obsolete JSON failure"));
  await pending;
  h.assertPreserved(newer);
});

test("a newer source selection suppresses an old source conflict without a query change", async () => {
  const h = harness(), response = deferred();
  h.context.fetch = () => response.promise;
  const pending = h.run(`perform(() => api('/api/source?path=synthetic&revision=1'))`);
  h.run("sourceSerial++;");
  response.resolve({status: 409, ok: false, json: async () => ({error: {message: "old source"}})});
  await pending;
  assert.equal(h.get("error").hidden, true);
  assert.equal(h.get("stale").hidden, true);
});

test("current failures still display and only a current conflict invalidates focus", async () => {
  const h = harness();
  h.context.fetch = async () => ({status: 422, ok: false, json: async () => ({error: {message: "current validation failure"}})});
  await h.run(`perform(() => api('/api/questions/preview'))`);
  assert.equal(h.get("error").hidden, false);
  assert.equal(h.get("error").textContent, "current validation failure");
  h.run("packet = {packetId:'current'}; focused = {marker:'current'};");
  h.context.fetch = async () => ({status: 409, ok: false, json: async () => ({error: {message: "current conflict"}})});
  await h.run(`perform(() => api('/api/questions/current/jev-response'))`);
  assert.equal(h.run("packet"), null);
  assert.equal(h.run("focused"), null);
  assert.equal(h.get("stale").hidden, false);
  assert.equal(h.get("error").textContent, "current conflict");
});

test("API errors alone have no UI side effects", async () => {
  const h = harness();
  h.context.fetch = async () => ({status: 409, ok: false, json: async () => ({error: {message: "conflict"}})});
  h.run("packet = {packetId:'current'};");
  const packet = h.run("packet");
  await assert.rejects(h.run(`api('/api/questions/current/jev-request')`), error => error.status === 409);
  assert.equal(h.run("packet"), packet);
  assert.equal(h.get("error").hidden, true);
  assert.equal(h.get("stale").hidden, true);
});

for (const size of [176000, 176001]) {
  test(`actual export Blob enforces the ${size}-byte UTF-8 boundary without truncation`, async () => {
    const h = harness();
    // JSON wrapper is 11 bytes; multibyte characters catch string-length guards.
    const payload = {code: "é".repeat(87994) + "x".repeat(size - 175999)};
    const compact = JSON.stringify(payload);
    assert.equal(new TextEncoder().encode(compact).byteLength, size);
    h.run("packet = {packetId:'synthetic', revision:1, request:{seed:'root'}};");
    h.context.fetch = async () => ({status: 200, ok: true, json: async () => payload});
    await h.get("export-jev").listeners.click();
    if (size === 176000) {
      assert.equal(h.blobs.length, 1);
      assert.equal(h.blobs[0].size, 176000);
      assert.equal(await h.blobs[0].text(), compact);
      assert.deepEqual(h.downloads, ["baleyg-jev-request.json"]);
      assert.equal(h.get("error").hidden, true);
    } else {
      assert.equal(h.blobs.length, 0);
      assert.equal(h.downloads.length, 0);
      assert.equal(h.get("error").hidden, false);
      assert.match(h.get("error").textContent, /176001 bytes.*176000 bytes.*Nothing was downloaded/);
    }
  });
}

for (const code of [200, 409, 429, 502]) {
  test(`obsolete live Jev ${code} cannot replace newer focus`, async () => {
    const h = harness(), response = deferred();
    h.context.fetch = () => response.promise;
    const pending = h.run(`perform(() => api('/api/questions/p/jev-run', 'POST', {}))`);
    const newer = h.preserveNewFocus();
    response.resolve({status: code, ok: code === 200, json: async () => ({error:{message:'obsolete'}})});
    await pending;
    h.assertPreserved(newer);
  });
}
function readyJev(h) {
  h.run(`packet = {packetId:'synthetic', revision:1, request:{seed:'root'}};
    jevStatus = {enabled:true,budget:{remainingCents:500}}; syncFocusControls();`);
}
test('Run Jev requires per-click confirmation and sends no source override', async () => {
  const h = harness(), calls = [];
  readyJev(h);
  let warning = '';
  h.context.window.confirm = text => { warning = text; return false; };
  h.context.fetch = async url => { calls.push(url); throw new Error('unexpected'); };
  await h.get('run-jev').listeners.click();
  assert.deepEqual(calls, []);
  assert.match(warning, /entire prepared evidence packet/);
  assert.match(warning, /complete indexed source files, not just visible calls/);
  h.context.window.confirm = () => true;
  h.context.fetch = async (url, options) => {
    calls.push(url);
    if (url === '/api/jev/status') return {status:200,ok:true,json:async()=>({enabled:true,budget:{capCents:500,reservedCents:10,remainingCents:490,attempts:1}})};
    assert.equal(options.method, 'POST');
    assert.equal(options.body, '{}');
    return {status:200,ok:true,json:async()=>({view:{revision:1,nodes:[],calls:[],selectionSource:'liveJev'},latencyMs:20})};
  };
  await h.get('run-jev').listeners.click();
  assert.deepEqual(calls, ['/api/questions/synthetic/jev-run','/api/jev/status']);
  assert.equal(h.run('focused.selectionSource'), 'liveJev');
  assert.match(h.get('focus-state').textContent, /provider selection, not proof/);
  assert.match(h.get('jev-status').textContent, /not invoiced cost/);
});
test('status is local-only and disabled or exhausted providers disable Run Jev', async () => {
  const h = harness(), calls = [];
  h.context.fetch = async url => { calls.push(url); return {status:200,ok:true,json:async()=>({enabled:false,budget:null})}; };
  await h.run('refreshJevStatus()');
  assert.deepEqual(calls, ['/api/jev/status']);
  assert.equal(h.get('run-jev').disabled, true);
  readyJev(h);
  h.run('jevStatus.budget.remainingCents = 0; syncFocusControls();');
  assert.equal(h.get('run-jev').disabled, true);
});
test('typing and preparing settings never trigger live inference', () => {
  const h = harness();
  readyJev(h);
  h.get('question-form').listeners.input();
  assert.equal(h.run('packet'), null);
  assert.equal(h.get('run-jev').disabled, true);
});

function readyAcp(h) {
  h.run(`packet = {packetId:'synthetic', revision:1, request:{seed:'root'}};
    acpStatus = {enabled:true,status:{remainingAttempts:2,maxAttempts:3,attempts:1,model:'sonnet',maxEstimatedUsdPerAttempt:1}}; syncFocusControls();`);
}
function acpAnswer() {
  return {packetId:'synthetic',revision:1,source:'liveAcp',attemptId:'attempt-1',latencyMs:12,estimatedUsd:null,
    answer:{packetId:'synthetic',summary:[{text:'<img src=x onerror=alert(1)>',citations:[{path:'src/<script>.rs',startLine:2,endLine:3,quote:'<b>source</b>\nnext'}]}],branches:[{text:'A branch',citations:[]}],limitations:['<svg onload=alert(1)>']}};
}
function acpFetch(calls, answer = acpAnswer()) {
  return async (url, options) => {
    calls.push({url,options});
    return {ok:true,status:200,json:async()=>url === '/api/acp/status'
      ? {enabled:true,status:{remainingAttempts:1,maxAttempts:3,attempts:2,model:'sonnet',maxEstimatedUsdPerAttempt:1}} : answer};
  };
}
test('ACP needs known status, separate allowance, and explicit full-source confirmation', async () => {
  const h = harness(), calls = [];
  h.run("packet = {packetId:'synthetic',revision:1,request:{seed:'root'}}; syncFocusControls();");
  assert.equal(h.get('explain-acp').disabled,true);
  readyAcp(h);
  h.context.fetch = acpFetch(calls);
  let warning;
  h.context.window.confirm = text => { warning = text; return false; };
  await h.get('explain-acp').listeners.click();
  assert.deepEqual(calls,[]);
  assert.match(warning,/FULL prepared source evidence/);
  assert.match(warning,/complete indexed source files, not just the five displayed calls/);
  assert.match(warning,/Claude subscription.*separate ACP attempt allowance, NOT Jev/);
  h.context.window.confirm = () => true;
  await h.get('explain-acp').listeners.click();
  assert.deepEqual(calls.map(c=>c.url),['/api/questions/synthetic/acp-answer','/api/acp/status']);
  assert.equal(calls[0].options.method,'POST'); assert.equal(calls[0].options.body,'{}');
  assert.equal(h.run('jevStatus'),null); // Preview alone suffices.
  assert.equal(h.get('answer').hidden,false);
  assert.match(h.get('answer-meta').textContent,/Live ACP.*cost estimate unavailable/);
  assert.match(h.get('acp-status').textContent,/separate from Jev.*not a bill or hard spending guarantee/);
});
test('ACP status and edits never infer; disabled or exhausted status disables action', async () => {
  const h = harness(), calls = [];
  readyAcp(h); h.context.fetch = acpFetch(calls);
  await h.run('refreshAcpStatus()');
  h.get('question-form').listeners.input();
  assert.deepEqual(calls.map(c=>c.url),['/api/acp/status']);
  assert.equal(h.get('explain-acp').disabled,true);
  readyAcp(h); h.run('acpStatus.status.remainingAttempts = 0; syncFocusControls();');
  assert.equal(h.get('explain-acp').disabled,true);
  h.run('acpStatus = {enabled:false,status:null}; syncFocusControls();');
  assert.equal(h.get('explain-acp').disabled,true);
});
test('answer claims, citations and caveats stay text; citation opens indexed source and highlights range', async () => {
  const h = harness(), calls = [];
  readyAcp(h); h.context.fetch = acpFetch(calls);
  await h.get('explain-acp').listeners.click();
  const content = h.get('answer-content');
  assert.equal(content.children[0].children[0].textContent,'<img src=x onerror=alert(1)>');
  const link = content.children[0].children[1].children[0];
  assert.equal(link.textContent,'src/<script>.rs:2–3');
  assert.equal(link.title,'<b>source</b>\nnext');
  assert.equal(content.children.at(-1).children[0].textContent,'<svg onload=alert(1)>');
  h.context.fetch = async url => {
    assert.equal(url,'/api/source?path=src%2F%3Cscript%3E.rs&revision=1');
    return {ok:true,status:200,json:async()=>({revision:1,file:{path:'src/<script>.rs',text:'first\n<b>source</b>\nnext\nlast'}})};
  };
  await link.listeners.click();
  const lines = h.get('source').children[0].children;
  assert.deepEqual(lines.map(n=>n.classList.contains('highlight')),[false,true,true,false]);
  assert.equal(lines[1].children[1].textContent,'<b>source</b>');
});
for (const mutation of [r=>r.source='local',r=>r.packetId='other',r=>r.answer.packetId='other',r=>r.revision=2]) {
  test('ACP refuses mismatched answer provenance', async () => {
    const h = harness(), answer = acpAnswer(); readyAcp(h); mutation(answer);
    h.context.fetch = acpFetch([],answer); await h.get('explain-acp').listeners.click();
    assert.equal(h.get('answer').hidden,true);
    assert.match(h.get('error').textContent,/provenance/);
  });
}
for (const outcome of ['success','409','422','network','json']) {
  test(`obsolete ACP ${outcome} cannot change newer answer or show an error`, async () => {
    const h = harness(), response = deferred(); readyAcp(h);
    h.context.fetch = url => url === '/api/acp/status' ? acpFetch([])(url) : response.promise;
    const pending = h.get('explain-acp').listeners.click();
    const newer = h.preserveNewFocus();
    h.run("clearAnswer(); $('answer-state').textContent='new answer state';");
    if (outcome === 'network') response.reject(new Error('obsolete'));
    else response.resolve({status:outcome==='success'||outcome==='json'?200:Number(outcome),ok:outcome==='success'||outcome==='json',json:async()=>{if(outcome==='json') throw new Error('bad json'); return outcome==='success'?acpAnswer():{error:{message:'obsolete'}};}});
    await pending; h.assertPreserved(newer);
    assert.equal(h.get('answer').hidden,true);
    assert.equal(h.get('answer-state').textContent,'new answer state');
  });
}
for (const change of ['question','packet','revision','session']) {
  test(`answer clears on ${change} change`, async () => {
    const h = harness(); readyAcp(h); h.context.fetch = acpFetch([]);
    await h.get('explain-acp').listeners.click(); assert.equal(h.get('answer').hidden,false);
    if (change==='question') h.get('question-form').listeners.input();
    if (change==='packet') h.run('invalidateFocus()');
    if (change==='revision') {
      h.context.fetch = async()=>({ok:true,status:200,json:async()=>({revision:2,stats:{}})});
      await h.run('refreshStatus()');
    }
    if (change==='session') h.get('logout').listeners.click();
    assert.equal(h.get('answer').hidden,true); assert.equal(h.get('answer-content').children.length,0);
  });
}
test('answer sits above calls with truthful source validation and static-boundary copy', () => {
  const html = fs.readFileSync(path.join(__dirname,'../web/index.html'),'utf8');
  assert.ok(html.indexOf('id="answer"') < html.indexOf('id="calls"'));
  assert.match(html,/not whether a claim logically follows or is true/);
  assert.match(html,/Unresolved, external, and callback targets remain boundaries/);
  assert.match(html,/id="max-visible"[^>]*value="5"/);
});

for (const [route, deadline] of [['/api/acp/status',15000], ['/api/questions/synthetic/acp-answer',150000]]) {
  test(`API deadline bounds ${route} and reports a visible timeout without retry`, async () => {
    const h = harness(); let signal, calls = 0;
    h.context.fetch = (url, options) => { calls++; signal = options.signal; return new Promise(()=>{}); };
    const pending = h.run(`perform(() => api(${JSON.stringify(route)}))`);
    const timer = [...h.timers.values()][0]; assert.equal(timer.delay,deadline);
    timer.callback(); await pending;
    assert.equal(signal.aborted,true); assert.equal(calls,1);
    assert.equal(h.get('error').hidden,false);
    assert.match(h.get('error').textContent,/timed out.*No automatic retry/);
    assert.equal(h.timers.size,0);
  });
}
test('deadline also bounds stalled JSON; obsolete timeouts remain silent', async () => {
  const h = harness(), entered = deferred();
  h.context.fetch = async()=>({ok:true,status:200,json:()=>{entered.resolve(); return new Promise(()=>{});}});
  const pending = h.run("perform(() => api('/api/questions/synthetic/acp-answer'))");
  await entered.promise; const newer = h.preserveNewFocus();
  [...h.timers.values()][0].callback(); await pending;
  h.assertPreserved(newer);
});
test('post-answer stalled status clears busy state, fails closed, and finishes at deadline', async () => {
  const h = harness(), entered = deferred(), calls = []; readyAcp(h);
  h.context.fetch = async(url, options)=>{
    calls.push(url);
    if (url === '/api/acp/status') { entered.resolve(); return new Promise(()=>{}); }
    return {ok:true,status:200,json:async()=>acpAnswer()};
  };
  const pending = h.get('explain-acp').listeners.click();
  await entered.promise;
  assert.equal(h.run('acpRunning'),false);
  assert.equal(h.run('acpStatus'),null);
  assert.equal(h.get('explain-acp').disabled,true);
  assert.equal(h.get('answer').hidden,false);
  assert.match(h.get('acp-status').textContent,/Checking/);
  const timer = [...h.timers.values()][0]; assert.equal(timer.delay,15000);
  timer.callback(); await pending;
  assert.equal(h.run('acpRunning'),false);
  assert.equal(h.get('explain-acp').disabled,true);
  assert.match(h.get('acp-status').textContent,/unavailable.*Refresh status/);
  assert.deepEqual(calls,['/api/questions/synthetic/acp-answer','/api/acp/status']);
  // A later explicit status refresh can restore the action.
  h.context.fetch = acpFetch([]); await h.run('refreshAcpStatus()');
  assert.equal(h.get('explain-acp').disabled,false);
});

test('ACP answer status preserves actionable authentication error without retry', async () => {
  const h = harness(); readyAcp(h); let attempts = 0;
  const message = 'Claude subscription authentication is unavailable. Sign in manually. No automatic retry was made.';
  h.context.fetch = async (url) => {
    if (url === '/api/acp/status') return acpFetch([])(url);
    attempts++;
    return {ok:false,status:502,json:async()=>({error:{code:'acp_auth_required',message}})};
  };
  await h.get('explain-acp').listeners.click();
  assert.equal(attempts,1);
  assert.equal(h.get('answer').hidden,true);
  assert.equal(h.get('answer-state').textContent,message);
  assert.equal(h.get('error').textContent,message);
});
