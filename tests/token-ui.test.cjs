"use strict";
// Synthetic tokens and in-memory files only. Run: node --test tests/token-ui.test.cjs
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../web/sequence.js"), "utf8") + "\n" + fs.readFileSync(path.join(__dirname, "../web/app.js"), "utf8");
const KEY = "baleyg.daemonToken.v1";
const response = data => ({ok:true, status:200, json:async () => data});
const flush = () => new Promise(resolve => setImmediate(resolve));
function harness({saved, blocked = false} = {}) {
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
      if (url === "/api/status") return response({revision:1,workspaceRoot:"/synthetic",stats:{}});
      if (url.startsWith("/api/tree?")) return response({revision:1,path:"",root:"/synthetic",items:[],nextOffset:null});
      if (["/api/views","/api/annotations"].includes(url)) return response([]);
      if (["/api/jev/status","/api/acp/status"].includes(url)) return response({enabled:false});
      throw new Error(`Unexpected request: ${url}`);
    }});
  vm.runInContext(source, context);
  async function connect(value = "synthetic-new", remember = false) {
    get("token").value = value; get("remember-token").checked = remember;
    get("connect-form").listeners.submit({preventDefault() {}});
    await flush();
  }
  async function file(text, size = Buffer.byteLength(text)) {
    get("token-file").value = "synthetic-selection";
    get("token-file").files = [{size, text:async () => text}];
    await get("token-file").listeners.change();
  }
  return {get, context, requests, writes, stored, connect, file};
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
  assert.equal(h.get("error").hidden, false);
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
