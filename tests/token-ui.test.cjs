"use strict";
// Synthetic tokens and in-memory files only. Run: node --test tests/token-ui.test.cjs
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../web/sequence.js"), "utf8") + "\n" + fs.readFileSync(path.join(__dirname, "../web/app.js"), "utf8");
const KEY = "baleyg.daemonToken.v1";
const ROOT = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SCOPED = `/api/checkouts/${ROOT}`;
const response = data => ({ok:true, status:200, json:async () => data});
const flush = () => new Promise(resolve => setImmediate(resolve));
function harness({saved, blocked = false, routes} = {}) {
  const elements = new Map(), requests = [], writes = [], stored = new Map(saved === undefined ? [] : [[KEY, saved]]);
  function node(tagName) {
    const classes = new Set();
    return {tagName, hidden:true, disabled:false, checked:false, value:"", textContent:"", children:[], listeners:{}, attrs:{},
      classList:{add(value) { classes.add(value); }, contains(value) { return classes.has(value); }},
      focus() {}, addEventListener(type, handler) { this.listeners[type] = handler; },
      replaceChildren(...items) { this.children = items; }, append(...items) { this.children.push(...items); },
      querySelector() { return null; }, setAttribute(key, value) { this.attrs[key] = String(value); }, remove() {}, click() {}};
  }
  function get(id) { if (!elements.has(id)) elements.set(id, node()); return elements.get(id); }
  get("connect-form").hidden = false;
  const storage = {
    getItem(key) { if (blocked) throw new Error("Storage blocked"); return stored.get(key) ?? null; },
    setItem(key, value) { if (blocked) throw new Error("Storage blocked"); writes.push([key,value]); stored.set(key,value); },
    removeItem(key) { if (blocked) throw new Error("Storage blocked"); stored.delete(key); }
  };
  const context = vm.createContext({console, TextEncoder, DOMException, Blob, AbortController,
    setTimeout() { return 1; }, clearTimeout() {}, localStorage:storage, window:{localStorage:storage, confirm:() => true},
    document:{getElementById:get, createElement:node, createElementNS:(_,tag) => node(tag), createDocumentFragment:node, createTextNode:text => ({textContent:text}), body:node()},
    fetch:async (url, options) => {
      requests.push({url,options});
      if (routes) return routes(url, options);
      if (url === "/api/checkouts") return response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state:"available",active:false}]});
      if (url === "/api/daemon/status") return response({activeCheckouts:0});
      assert.ok(url.startsWith(`${SCOPED}/`), `Unexpected unscoped route: ${url}`);
      url = "/api" + url.slice(SCOPED.length);
      if (url === "/api/status") return response({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},workspaceRoot:"/synthetic",stats:{}});
      if (url.startsWith("/api/tree?")) return response({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},path:"",root:"/synthetic",items:[],nextOffset:null});
      if (["/api/views","/api/annotations"].includes(url)) return response([]);
      if (["/api/jev/status","/api/acp/status"].includes(url)) return response({enabled:false});
      throw new Error(`Unexpected request: ${url}`);
    }});
  vm.runInContext(source, context);
  async function connect(value = "synthetic-new", remember = false, select = true) {
    get("token").value = value; get("remember-token").checked = remember;
    get("connect-form").listeners.submit({preventDefault() {}});
    await flush();
    if (select && !get("checkout-select").disabled) {
      get("checkout-select").value = ROOT;
      await get("checkout-select").listeners.change({target:get("checkout-select")});
      await flush();
    }
  }
  async function file(text, size = Buffer.byteLength(text)) {
    get("token-file").value = "synthetic-selection";
    get("token-file").files = [{size, text:async () => text}];
    await get("token-file").listeners.change();
  }
  return {get, context, run:code=>vm.runInContext(code,context), requests, writes, stored, connect, file};
}

test("saved token is loaded with opt-in checked but makes no startup request", () => {
  const h = harness({saved:"synthetic-saved"});
  assert.equal(h.get("token").value, "synthetic-saved");
  assert.equal(h.get("remember-token").checked, true);
  assert.equal(h.get("connect-form").hidden, false);
  assert.equal(h.requests.length, 0);
  assert.equal(h.writes.length, 0);
});
test("fresh browser starts with empty token and persistence off", () => {
  const h = harness();
  assert.equal(h.get("token").value, "");
  assert.equal(h.get("remember-token").checked, false);
  assert.equal(h.requests.length, 0);
});
test("successful explicit connect persists trimmed token only with opt-in", async () => {
  const h = harness(); await h.connect("  synthetic-new\n", true);
  assert.equal(h.get("workspace").hidden, false);
  assert.deepEqual(h.writes, [[KEY,"synthetic-new"]]);
  assert.equal(h.requests[0].options.headers.Authorization, "Bearer synthetic-new");
});
test("successful connect without opt-in removes existing saved token", async () => {
  const h = harness({saved:"synthetic-old"}); await h.connect("synthetic-new", false);
  assert.equal(h.get("workspace").hidden, false);
  assert.equal(h.stored.has(KEY), false); assert.equal(h.writes.length, 0);
});
test("unchecking persistence immediately forgets the saved token", () => {
  const h = harness({saved:"synthetic-old"});
  h.get("remember-token").checked = false; h.get("remember-token").listeners.change();
  assert.equal(h.stored.has(KEY), false); assert.equal(h.requests.length, 0);
});
for (const status of [401,403,500]) test(`failed ${status} connect ${status === 500 ? "retains" : "removes"} saved token`, async () => {
  const h = harness({saved:"synthetic-old"});
  h.context.fetch = async () => ({ok:false,status,json:async () => ({error:{message:"Synthetic rejection"}})});
  await h.connect("synthetic-new", true);
  assert.equal(h.stored.get(KEY), status === 500 ? "synthetic-old" : undefined);
  assert.equal(h.writes.length, 0); assert.equal(h.get("workspace").hidden, true);
  if (status === 500) assert.match(h.get("checkout-state").textContent, /unavailable/i);
  else assert.equal(h.get("error").hidden, false);
});
test("network failure retains existing saved token and never saves the rejected replacement", async () => {
  const h = harness({saved:"synthetic-old"}); h.context.fetch = async () => { throw new Error("Synthetic offline"); };
  await h.connect("synthetic-new", true);
  assert.equal(h.stored.get(KEY), "synthetic-old"); assert.equal(h.writes.length, 0);
});
test("Disconnect removes saved token and resets token, checkbox and file selection", async () => {
  const h = harness(); await h.connect("synthetic-new", true);
  h.get("token").value = "synthetic-pending"; h.get("token-file").value = "synthetic-selection";
  h.get("logout").listeners.click();
  assert.equal(h.stored.has(KEY), false); assert.equal(h.get("remember-token").checked, false);
  assert.equal(h.get("token").value, ""); assert.equal(h.get("token-file").value, "");
  assert.equal(h.get("workspace").hidden, true); assert.equal(h.get("connect-form").hidden, false);
});
test("blocked storage does not prevent startup or normal connect and reports save failure", async () => {
  const h = harness({blocked:true}); assert.equal(h.requests.length, 0);
  await h.connect("synthetic-new", true);
  assert.equal(h.get("workspace").hidden, false);
  assert.match(h.get("notice").textContent, /storage.*unavailable|not saved/i);
  assert.doesNotThrow(() => h.get("logout").listeners.click());
});
test("local token file trims and fills input only, with no request or persistence", async () => {
  const h = harness(); await h.file(" \nsynthetic-file\r\n");
  assert.equal(h.get("token").value, "synthetic-file"); assert.equal(h.get("token-file").value, "");
  assert.equal(h.get("remember-token").checked, false);
  assert.equal(h.requests.length, 0); assert.equal(h.writes.length, 0);
});
test("file accepts 512 characters and exactly 1024 bytes before trimming", async () => {
  const h = harness(); await h.file(" ".repeat(512) + "s".repeat(512));
  assert.equal(h.get("token").value, "s".repeat(512)); assert.equal(h.get("token-file").value, "");
});
for (const [label,text,size] of [["blank"," \n",2],["overlong","s".repeat(513),513],["oversize","synthetic-file",1025]])
  test(`invalid ${label} file leaves token unchanged and clears file selection`, async () => {
    const h = harness(); h.get("token").value = "synthetic-existing";
    await h.file(text,size);
    assert.equal(h.get("token").value, "synthetic-existing"); assert.equal(h.get("token-file").value, "");
    assert.equal(h.get("error").hidden, false); assert.equal(h.requests.length, 0); assert.equal(h.writes.length, 0);
  });
test("oversize file is rejected before its contents are read", async () => {
  const h = harness(); let reads = 0;
  h.get("token-file").files = [{size:1025,text:async () => { reads++; return "synthetic"; }}];
  await h.get("token-file").listeners.change(); assert.equal(reads, 0);
});
test("file read error is caught and file selection resets", async () => {
  const h = harness(); h.get("token").value = "synthetic-existing"; h.get("token-file").value = "synthetic-selection";
  h.get("token-file").files = [{size:10,text:async () => { throw new Error("Synthetic read failure"); }}];
  await h.get("token-file").listeners.change();
  assert.equal(h.get("token").value, "synthetic-existing"); assert.equal(h.get("token-file").value, "");
  assert.equal(h.get("error").hidden, false); assert.equal(h.requests.length, 0);
});
test("cancelled file selection is a no-op", async () => {
  const h = harness(); h.get("token").value = "synthetic-existing"; h.get("token-file").files = [];
  await h.get("token-file").listeners.change(); assert.equal(h.get("token").value, "synthetic-existing");
  assert.equal(h.requests.length, 0);
});

for (const text of ["synthetic\nsecret", "<script>synthetic</script>"]) test("file rejects unsafe token text without echoing it", async () => {
  const h = harness(); h.get("token").value = "synthetic-existing";
  await h.file(text);
  assert.equal(h.get("token").value, "synthetic-existing");
  assert.equal(h.get("error").hidden, false);
  assert.ok(!h.get("error").textContent.includes(text));
});
test("file read exceptions do not expose raw error details", async () => {
  const h = harness();
  h.get("token-file").files = [{size:10,text:async () => { throw new Error("synthetic-sensitive-detail"); }}];
  await h.get("token-file").listeners.change();
  assert.equal(h.get("error").hidden, false);
  assert.doesNotMatch(h.get("error").textContent, /synthetic-sensitive-detail/);
});
test("older file read cannot overwrite newer selection", async () => {
  const h = harness(); let finish;
  h.get("token-file").files = [{size:10,text:() => new Promise(resolve => { finish = resolve; })}];
  const pending = h.get("token-file").listeners.change();
  await h.file("synthetic-newer"); finish("synthetic-older"); await pending;
  assert.equal(h.get("token").value, "synthetic-newer"); assert.equal(h.requests.length, 0);
});
test("pending file read cannot restore credentials after Disconnect", async () => {
  const h = harness(); let finish;
  h.get("token-file").files = [{size:10,text:() => new Promise(resolve => { finish = resolve; })}];
  const pending = h.get("token-file").listeners.change();
  h.get("logout").listeners.click(); finish("synthetic-old"); await pending;
  assert.equal(h.get("token").value, ""); assert.equal(h.stored.has(KEY), false);
});

// The same revision and relative path cannot prove checkout identity. Only the
// exact selected root URL and generation fence can do that.
test("explicit A to B switch keeps exact B URLs and rejects late A 401 with equal revision and path", async () => {
  const A = ROOT, B = "b".repeat(64), pair = {indexGeneration:"12345678-1234-4123-8123-123456789abc",indexRevision:1};
  const rows = [A,B].map((rootKey,i) => ({rootKey,workspaceRoot:i ? "/checkout/B" : "/checkout/A",state:"available",active:false}));
  let holdA = false, releaseA;
  const h = harness({routes:async (url, options) => {
    if (url === "/api/checkouts") return response({checkouts:rows});
    if (url === "/api/daemon/status") return response({activeCheckouts:0});
    const match = /^\/api\/checkouts\/([ab]{64})\/([^?]+)(?:\?.*)?$/.exec(url);
    assert.ok(match, `A request leaked to a global/default route: ${url}`);
    const [,root,suffix] = match, label = root === A ? "A" : "B";
    if (suffix === "status") return response({revision:pair,workspaceRoot:`/checkout/${label}`,stats:{files:1},diagnostics:[`diagnostic-${label}`]});
    if (suffix === "tree") {
      if (root === A && holdA) return new Promise(resolve => {releaseA = resolve;});
      return response({revision:pair,path:"",root:`/checkout/${label}`,items:[{kind:"file",name:"same.rs",path:"same.rs",methodCount:1}],nextOffset:null});
    }
    if (suffix === "source") return response({revision:pair,file:{path:"same.rs",text:`source-${label}`}});
    if (suffix === "jev/status" || suffix === "acp/status") return response({enabled:root===B});
    if (suffix === "views" || suffix === "annotations") return response([]);
    if (suffix === "jobs/current") return response(null);
    if (suffix === "dependencies") return response({state:"disabled",workspaceRevision:pair,catalogId:null,packages:[],warnings:[]});
    if (["query","index","views/item","annotations/item"].includes(suffix)) return response({});
    throw new Error(`Unexpected ${url} ${options.method}`);
  }});
  await h.connect("synthetic",false,false);
  assert.equal(h.run("selectedRootKey"),null);
  assert.deepEqual(h.requests.map(r => r.url),["/api/checkouts","/api/daemon/status"]);
  h.get("checkout-select").value=A;
  await h.get("checkout-select").listeners.change({target:h.get("checkout-select")});
  await flush(); await flush();
  assert.equal(h.get("checkout-root").textContent,"/checkout/A");
  holdA=true;const old=h.run("loadTreeRoot()");
  assert.ok(releaseA,"A tree request must remain deferred");
  const switchAt=h.requests.length;
  h.get("checkout-select").value=B;
  await h.get("checkout-select").listeners.change({target:h.get("checkout-select")});
  await flush(); await flush();
  await h.run("showSource({path:'same.rs',range:{startLine:1,endLine:1}},status.revision)");
  for (const [route,method] of [["/api/query","POST"],["/api/views/item","PUT"],["/api/annotations/item","DELETE"]]) {
    await h.run(`api('${route}','${method}',{})`);
  }
  const afterB=h.requests.slice(switchAt);
  assert.ok(afterB.length>4);
  assert.ok(afterB.every(({url}) => url.startsWith(`/api/checkouts/${B}/`)), JSON.stringify(afterB.map(r=>r.url)));
  assert.ok(afterB.some(({url,options}) => url === `/api/checkouts/${B}/source?path=same.rs&indexGeneration=${pair.indexGeneration}&indexRevision=1` && options.method === "GET"));
  assert.ok(afterB.some(({url,options}) => url === `/api/checkouts/${B}/query` && options.method === "POST"));
  assert.ok(afterB.some(({url,options}) => url === `/api/checkouts/${B}/views/item` && options.method === "PUT"));
  assert.ok(afterB.some(({url,options}) => url === `/api/checkouts/${B}/annotations/item` && options.method === "DELETE"));
  releaseA({ok:false,status:401,json:async()=>({error:{message:"late-A-auth"}})});
  await old;
  assert.equal(h.run("selectedRootKey"),B);
  assert.equal(h.get("checkout-root").textContent,"/checkout/B");
  assert.match(h.get("diagnostics").textContent,/diagnostic-B/);
  assert.doesNotMatch(h.get("diagnostics").textContent,/diagnostic-A/);
  const sourceText = function walk(node) { return [node.textContent,...(node.children || []).flatMap(walk)].join(" "); };
  assert.match(sourceText(h.get("source")),/source-B/);
  assert.match(h.get("acp-status").textContent,/enabled|available/i);
  assert.equal(h.get("connect-form").hidden,true);
  assert.equal(h.get("error").hidden,true);
});

test("one available checkout never auto-selects; list retry recovers without scoped traffic", async () => {
  let fail=true;
  const h=harness({routes:(url,opts)=>{
    if(url==="/api/checkouts") return fail ? {ok:false,status:503,json:async()=>({error:{message:"list temporarily unavailable"}})}
      : response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state:"available",active:false}]});
    if(url==="/api/daemon/status")return response({activeCheckouts:0});
    throw new Error(`Scoped call before explicit selection: ${url}`);
  }});
  await h.connect("synthetic",false,false);
  assert.equal(h.run("selectedRootKey"),null);
  assert.equal(h.get("checkout-select").disabled,true);
  assert.equal(h.get("checkout-retry").hidden,false);
  fail=false;h.get("checkout-retry").listeners.click();await flush();
  assert.equal(h.get("checkout-select").disabled,false);
  assert.equal(h.get("workspace").hidden,true);
  assert.equal(h.run("selectedRootKey"),null);
  assert.ok(h.requests.every(({url,options})=>["/api/checkouts","/api/daemon/status"].includes(url) && options.method==="GET"));
});

test("unavailable or corrupt checkout rows cannot be selected", async () => {
  const bad="c".repeat(64), h=harness({routes:(url)=>{
    if(url==="/api/checkouts")return response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state:"unavailable",active:false},{rootKey:bad,state:"corrupt",active:false}]});
    if(url==="/api/daemon/status")return response({activeCheckouts:0});
    throw new Error(`Unavailable selection made a scoped call: ${url}`);
  }});
  await h.connect("synthetic",false,false);
  assert.equal(h.get("checkout-select").disabled,true);
  h.get("checkout-select").value=ROOT;h.get("checkout-select").listeners.change({target:h.get("checkout-select")});
  await flush();
  assert.equal(h.run("selectedRootKey"),null);
  assert.equal(h.get("workspace").hidden,true);
  assert.match(h.get("checkout-state").textContent,/available checkout/i);
});

test("cold checkout keeps its explicit selection and never invents evidence", async () => {
  const h=harness({routes:url=>{
    if(url==="/api/checkouts")return response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state:"available",active:false}]});
    if(url==="/api/daemon/status")return response({activeCheckouts:0});
    if(url===`${SCOPED}/status`)return {ok:false,status:503,json:async()=>({error:{code:"index_not_ready",message:"Cold checkout"}})};
    throw new Error(`Cold checkout leaked request: ${url}`);
  }});
  await h.connect("synthetic",false,false);
  h.get("checkout-select").value=ROOT;h.get("checkout-select").listeners.change({target:h.get("checkout-select")});await flush();
  assert.equal(h.run("selectedRootKey"),ROOT);
  assert.equal(h.run("status"),null);
  assert.match(h.get("checkout-state").textContent,/Index not ready/i);
  assert.equal(h.get("checkout-root").textContent.includes("/synthetic"),false,"unverified display path is not evidence");
  assert.deepEqual(h.requests.map(r=>r.url),["/api/checkouts","/api/daemon/status",`${SCOPED}/status`]);
});

test("catching-up prior head remains paired and clears on a refreshed status", async () => {
  const pair={indexGeneration:"12345678-1234-4123-8123-123456789abc",indexRevision:1};
  let catchingUp=true;
  const h=harness({routes:url=>{
    if(url==="/api/checkouts")return response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state:"available",active:false}]});
    if(url==="/api/daemon/status")return response({activeCheckouts:0});
    assert.ok(url.startsWith(`${SCOPED}/`),`Wrong root: ${url}`);
    if(url===`${SCOPED}/status`)return response({workspaceRoot:"/synthetic",revision:pair,catchingUp,stats:{}});
    if(url.startsWith(`${SCOPED}/tree?`))return response({revision:pair,path:"",root:"/synthetic",items:[],nextOffset:null});
    if(url===`${SCOPED}/views`||url===`${SCOPED}/annotations`)return response([]);
    if(url===`${SCOPED}/jev/status`||url===`${SCOPED}/acp/status`)return response({enabled:false});
    if(url===`${SCOPED}/jobs/current`)return response(null);
    if(url===`${SCOPED}/dependencies`)return response({state:"disabled",workspaceRevision:pair,catalogId:null,packages:[],warnings:[]});
    throw new Error(`Unexpected: ${url}`);
  }});
  await h.connect("synthetic",false,false);
  h.get("checkout-select").value=ROOT;h.get("checkout-select").listeners.change({target:h.get("checkout-select")});await flush();
  assert.equal(h.get("catching-up").hidden,false);
  assert.match(fs.readFileSync(path.join(__dirname,"../web/index.html"),"utf8"),/Catching up — evidence may be stale/);
  assert.equal(h.run("status.revision.indexRevision"),1);
  catchingUp=false;await h.run("refreshStatus()");
  assert.equal(h.get("catching-up").hidden,true);
  assert.equal(h.run("status.revision.indexRevision"),1);
  assert.equal(h.get("checkout-root").textContent,"/synthetic");
});

test("state change in refreshed checkout list clears a selected root", async () => {
  let state="available";
  const h=harness({routes:url=>{
    if(url==="/api/checkouts")return response({checkouts:[{rootKey:ROOT,workspaceRoot:"/synthetic",state,active:false}]});
    if(url==="/api/daemon/status")return response({activeCheckouts:0});
    if(url===`${SCOPED}/status`)return response({workspaceRoot:"/synthetic",revision:null,stats:{}});
    throw new Error(`Unexpected: ${url}`);
  }});
  await h.connect("synthetic",false,false);
  h.get("checkout-select").value=ROOT;h.get("checkout-select").listeners.change({target:h.get("checkout-select")});await flush();
  assert.equal(h.run("selectedRootKey"),ROOT);
  state="unavailable";await h.run("loadCheckouts()");await flush();
  assert.equal(h.run("selectedRootKey"),null);
  assert.equal(h.get("workspace").hidden,true);
  assert.match(h.get("checkout-state").textContent,/available checkouts|available checkout|choose/i);
});
