"use strict";
// Synthetic cached declarations only. Run: node --test tests/classes-ui.test.cjs
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../web/classes.js"), "utf8");
const pinSource = fs.readFileSync(path.join(__dirname, "../web/app.js"), "utf8").match(/const IndexPin = Object.freeze\(\{[\s\S]*?\n\}\);\nwindow.BaleygIndexPin = IndexPin;/)[0];
const descendants = node => [node, ...node.children.flatMap(descendants)];
const text = node => descendants(node).map(item => item.textContent).join(" ");
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => {resolve = yes; reject = no;}); return {promise, resolve, reject}; };
const range = {startLine: 1, endLine: 8, startByte: 0, endByte: 120};
function definition(id) { return {symbol: {id, name: id, path: `src/${id}.java`, range, provenance: {parser: "syntax"}}, qualifiedName: `sample.${id}`, language: "java", declarationKind: "class", fields: [{name:"other",typeHint:"B",path:`src/${id}.java`,range}], methods: [{name:"execute",symbolId:`${id}.execute`,typeHint:"void",path:`src/${id}.java`,range}], truncated:false}; }
const node = id => ({id, label:id, kind:"class", expandable:true, class:definition(id)});
function diagram(ids = ["A", "B"], revision = {indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1}) { return {revision, seed: ids[0], nodes:ids.map(node), edges:ids.slice(1).map((id,i)=>({id:`e${i}`,owner:ids[0],target:id,typeName:id,kind:"field",matchKind:"syntaxCandidate",candidateIds:[id],path:"A.java",range})), warnings:[],truncated:false}; }
function harness({navigation = false} = {}) {
  const elements = new Map(), calls = [], reads = [], methods = [], changes = [], navigations = [], stale = [];
  let navigate = (event, selector, scope) => { navigations.push({event, selector, scope}); };
  const docListeners = {}, winListeners = {};
  let revision = {indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1}, session = "one";
  let request = async (url, options) => options ? diagram() : {revision,items:[definition("A")],nextOffset:null,warnings:[],truncated:false};
  const document = {activeElement:null, addEventListener(type, fn) { (docListeners[type] ||= []).push(fn); }};
  function element(tagName) {
    const n = {tagName, textContent:"", className:"", attrs:{},dataset:{},style:{},children:[],listeners:{},parentNode:null,value:"",checked:false,disabled:false,tabIndex:-1,
      setAttribute(k,v) { this.attrs[k] = String(v); if(k==="class")this.className=String(v); },
      getAttribute(k) {return this.attrs[k];},
      get isConnected() {return this === document.body || !!this.parentNode?.isConnected;},
      addEventListener(type, fn) {(this.listeners[type] ||= []).push(fn);},
      append(...items) {for(const item of items){item.remove();item.parentNode=this;this.children.push(item);}},
      replaceChildren(...items) {for(const item of this.children)item.parentNode=null;this.children=[];this.append(...items);},
      before(item) {item.remove();item.parentNode=this.parentNode;const i=this.parentNode.children.indexOf(this);this.parentNode.children.splice(i,0,item);},
      remove() {if(this.parentNode){this.parentNode.children=this.parentNode.children.filter(item=>item!==this);this.parentNode=null;}},
      contains(item) {return descendants(this).includes(item);},
      querySelector(selector) {return descendants(this).slice(1).find(item=>selector.startsWith(".") ? item.className.split(" ").includes(selector.slice(1)) : item.tagName===selector) || null;},
      focus() {document.activeElement=this; for(const fn of docListeners.focusin||[])fn({target:this});},
      getBoundingClientRect() {return this.className==="classes-context-menu"?{width:220,height:150,left:0,top:0,bottom:150}:{width:32,height:32,left:50,top:30,bottom:62};},
      async fire(type, details={}) {
        const event={type,target:this,currentTarget:this,preventDefault(){this.prevented=true;},stopPropagation(){this.stopped=true;},...details};
        for(let current=this;current;current=current.parentNode){
          event.currentTarget=current;
          for(const fn of current.listeners[type]||[]) await fn(event);
          if(event.stopped)break;
        }
        return event;
      }
    };return n;
  }
  document.body=element("body"); document.createElement=element;document.createElementNS=(_,tag)=>element(tag);
  document.querySelector=selector=>document.body.querySelector(selector);
  document.getElementById=id=>{if(!elements.has(id)){const item=element(id);elements.set(id,item);document.body.append(item);}return elements.get(id);};
  const window = {innerWidth:390,innerHeight:600,addEventListener(type,fn){(winListeners[type] ||= []).push(fn);}};
  vm.runInNewContext(pinSource + "\n" + source,{window,document,URLSearchParams,console});
  window.BaleygClasses.init({...(navigation ? {navigateMember:(...args)=>navigate(...args)} : {}),request:(url,options)=>{calls.push({url,options});return request(url,options);},readSource:(step,rev)=>reads.push({step,revision:rev}),selectMethod:symbol=>methods.push(symbol),currentRevision:()=>revision,currentSession:()=>session,onChange:()=>changes.push(true),onStale:message=>stale.push(message)});
  return {controller:window.BaleygClasses,document,window,calls,reads,methods,changes,navigations,stale,get:document.getElementById,
    setNavigate(fn){navigate=fn;},setRequest(fn){request=fn;},setRevision(value){revision=value;},setSession(value){session=value;},
    async event(type,target){for(const fn of docListeners[type]||[]) await fn({target});},
    menu(){return document.body.querySelector(".classes-context-menu");},
    card(id){return descendants(document.getElementById("classes-diagram")).find(n=>n.dataset.classId===id);},
    button(container,label){const button=descendants(container).find(n=>n.tagName==="button"&&n.textContent===label);assert.ok(button,`Missing ${label}`);return button;}
  };
}
function freeze(value) { if(value&&typeof value==="object"){Object.freeze(value);Object.values(value).forEach(freeze);}return value; }
async function related(h, from, ids) {
 await h.card(from).fire("contextmenu"); await h.button(h.menu(),"Show related classes").fire("click");
 const chooser=h.document.body.querySelector(".classes-chooser");
 for(const id of ids) { const check=descendants(chooser).find(n=>n.tagName==="input"&&n.value===id); assert.ok(check,`Missing choice ${id}`);check.checked=true;await check.fire("change"); }
 await h.button(chooser,"Add selected classes").fire("click");
}
async function allReturned(h) { await h.button(h.document.body,"Show all returned classes").fire("click"); }

test("init is inert; global search and file open only read catalog/diagram",async()=>{
 const h=harness();assert.equal(h.calls.length,0);
 await h.get("classes-search").fire("click");assert.match(h.calls[0].url,/^\/api\/classes\?/);assert.equal(h.calls.length,1);
 assert.equal(new URL(h.calls[0].url,"http://local").searchParams.has("path"),false);
 await h.controller.open({path:"src/A.java"});assert.equal(h.calls.length,3);assert.equal(h.calls[2].options.body.seed,"A");assert.equal(h.reads.length,0);assert.equal(h.methods.length,0);
 assert.equal(new URL(h.calls[1].url,"http://local").searchParams.get("path"),"src/A.java");
 assert.match(text(h.get("classes-diagram")),/Members · 1 field · 1 method/);
 assert.equal(h.card("B"),undefined); assert.equal(h.card("A").querySelector(".classes-member-detail").hidden,true);
 assert.equal(h.get("classes-results").hidden,true);
});
test("immutable definitions render compartments, dashed arrows and text-only hostile labels",async()=>{
 const h=harness(), data=diagram();data.nodes[0].label="<img src=x onerror=alert(1)>";data.nodes[0].class.fields[0].name="<script>unsafe</script>";freeze(data);const before=JSON.stringify(data);
 h.setRequest(async()=>data);await h.controller.open({seed:"A"});assert.equal(JSON.stringify(data),before);
 assert.match(text(h.card("A")),/<script>unsafe<\/script>/);assert.equal(descendants(h.get("classes-diagram")).some(n=>n.tagName==="script"||n.tagName==="img"),false);
 await allReturned(h);
 assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-edge").length,1);
 assert.equal(JSON.stringify(data),before);
 assert.match(fs.readFileSync(path.join(__dirname,"../web/classes.css"),"utf8"),/stroke-dasharray: 6 4/);
});
test("source is explicit and method click passes a measured symbol ID",async()=>{
 const h=harness();await h.controller.open({seed:"A"});assert.equal(h.reads.length,0);
 await h.card("A").fire("contextmenu",{clientX:100,clientY:100});await h.button(h.menu(),"Read class source").fire("click");
 assert.equal(h.reads.length,1);assert.equal(h.reads[0].revision.indexRevision,1);assert.equal(h.reads[0].step.path,"src/A.java");
 await h.button(h.card("A"),"execute()").fire("click");assert.equal(h.methods[0].id,"A.execute");assert.equal(h.methods[0].parent,"A");assert.equal(h.calls.length,1);
});
test("related chooser adds only chosen neighbors and retains earlier roots and DOM slots",async()=>{
 const h=harness();h.setRequest(async(_,options)=>diagram(options.body.expanded.length?["A","B","C","D"]:["A","B","C"]));
 await h.controller.open({seed:"A"});const a=h.card("A"),left=a.style.left;
 await related(h,"A",["B"]);
 assert.deepEqual(Array.from(h.calls[1].options.body.expanded),["B"]);assert.equal(h.card("A"),a);assert.equal(a.style.left,left);assert.ok(h.card("B"));assert.equal(h.card("C"),undefined);
 const b=h.card("B"), bleft=b.style.left;
 await related(h,"A",["C"]);assert.deepEqual(Array.from(h.calls[2].options.body.expanded),["B","C"]);
 assert.equal(h.card("B"),b);assert.equal(b.style.left,bleft);assert.equal(h.card("D"),undefined);
 assert.equal(h.reads.length,0);
});
test("Focus class resets expansions and explicit unmatched hints remain terminal",async()=>{
 const h=harness();h.setRequest(async(_,options)=>{
  const data=diagram(options.body.seed==="B"?["B","A"]:["A","B"]);
  data.nodes.push({id:"hint",label:"Unknown",kind:"unmatched",class:null,expandable:false});
  data.edges.push({...data.edges[0],id:"hint-edge",target:"hint",matchKind:"unmatched"});return data;
 });
 await h.controller.open({seed:"A"});assert.equal(h.card("hint"),undefined);
 h.get("classes-unmatched").checked=true;await h.get("classes-unmatched").fire("change");assert.equal(h.calls.at(-1).options.body.includeUnmatched,true);
 assert.match(text(h.card("hint")),/terminal hint/);assert.equal(h.card("hint").listeners.contextmenu,undefined);
 await related(h,"A",["B"]);
 await h.card("B").fire("contextmenu");await h.button(h.menu(),"Focus this class").fire("click");assert.equal(h.calls.at(-1).options.body.seed,"B");assert.equal(h.calls.at(-1).options.body.expanded.length,0);
 assert.equal(h.card("A"),undefined);assert.ok(h.card("B"));
});
test("menu keyboard navigation skips disabled items, clamps viewport, and restores focus",async()=>{
 const h=harness(),data=diagram();data.nodes[0].expandable=false;h.setRequest(async()=>data);await h.controller.open({seed:"A"});const anchor=h.card("A");anchor.focus();
 const event=await anchor.fire("keydown",{key:"F10",shiftKey:true});assert.equal(event.prevented,true);const menu=h.menu();assert.equal(h.document.activeElement.textContent,"Read class source");
 await menu.fire("keydown",{key:"End"});assert.equal(h.document.activeElement.textContent,"Focus this class");
 await menu.fire("keydown",{key:"ArrowDown"});assert.equal(h.document.activeElement.textContent,"Read class source");
 await menu.fire("keydown",{key:"ArrowUp"});assert.equal(h.document.activeElement.textContent,"Focus this class");
 await menu.fire("keydown",{key:"Home"});assert.equal(h.document.activeElement.textContent,"Read class source");
 await menu.fire("keydown",{key:"Escape"});assert.equal(h.menu(),null);assert.equal(h.document.activeElement,anchor);
 await anchor.fire("contextmenu",{clientX:389,clientY:599});assert.equal(h.menu().style.left,"162px");assert.equal(h.menu().style.top,"442px");
 await h.event("pointerdown",h.get("classes-query"));assert.equal(h.menu(),null);
});
test("visible touch menu and ContextMenu key work; scroll dismisses",async()=>{
 const h=harness();await h.controller.open({seed:"A"});await h.button(h.card("A"),"⋯").fire("click");assert.ok(h.menu());
 await h.event("scroll",h.get("classes-diagram"));assert.equal(h.menu(),null);
 await h.card("A").fire("keydown",{key:"ContextMenu"});assert.ok(h.menu());await h.menu().fire("keydown",{key:"Tab"});assert.equal(h.menu(),null);
});
test("stale open responses, revision responses and old actions do not affect new view",async()=>{
 const h=harness(), first=deferred();h.setRequest((_,options)=>options.body.seed==="A"?first.promise:Promise.resolve(diagram(["B"])));
 const old=h.controller.open({seed:"A"});await h.controller.open({seed:"B"});first.resolve(diagram());await old;assert.ok(h.card("B"));assert.equal(h.card("A"),undefined);
 await h.card("B").fire("contextmenu");const source=h.button(h.menu(),"Read class source");h.setRevision({indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:2});await source.fire("click");assert.equal(h.reads.length,0);
 h.setRequest(async()=>diagram(["C"],1));await h.controller.open({seed:"C"});assert.equal(h.get("classes-state").dataset.state,"stale");assert.equal(h.card("C"),undefined);
});
test("reset invalidates pending lookup and source callbacks; session guards reject late failures",async()=>{
 const h=harness(), pending=deferred();h.setRequest(()=>pending.promise);const work=h.controller.open({});h.controller.reset();pending.resolve({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},items:[definition("A")],warnings:[]});await work;assert.equal(h.get("classes-results").children.length,0);
 const next=deferred();h.setRequest(()=>next.promise);const work2=h.controller.open({seed:"A"});h.setSession("two");next.reject(Error("old failure"));await work2;assert.doesNotMatch(text(h.get("classes-state")),/old failure/);
 assert.ok(h.changes.length>=4);
});
test("loading a new view invalidates keyboard member/source callbacks in old diagram",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const member=h.button(h.card("A"),"execute()");
 const pending=deferred();h.setRequest(()=>pending.promise);const work=h.controller.open({seed:"B"});await member.fire("click");assert.equal(h.methods.length,0);
 pending.resolve(diagram(["B"]));await work;
});
test("unsupported, unindexed, empty, partial and error states are explicit",async()=>{
 const h=harness();await h.controller.open({path:"src/lib.rs"});assert.equal(h.get("classes-state").dataset.state,"unsupported");assert.equal(h.calls.length,0);
 h.setRequest(async()=>({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},items:[],warnings:["Index workspace to populate class declarations."],requireIndex:true}));await h.controller.open({});assert.equal(h.get("classes-state").dataset.state,"unindexed");
 h.setRequest(async()=>({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},items:[],warnings:[]}));await h.controller.open({});assert.equal(h.get("classes-state").dataset.state,"empty");
 h.setRequest(async()=>({...diagram(),truncated:true,warnings:["Bound reached"]}));await h.controller.open({seed:"A"});assert.equal(h.get("classes-state").dataset.state,"partial");
 h.setRequest(async()=>{throw Error("Not a class or enclosing method");});await h.controller.open({seed:"bad"});assert.match(text(h.get("classes-state")),/Not a class/);assert.equal(h.get("classes-state").dataset.state,"error");
});
test("renderer caps nodes and edges, including repeated hint toggles",async()=>{
 const h=harness(), data=diagram(Array.from({length:30},(_,i)=>`C${i}`));data.edges=Array.from({length:80},(_,i)=>({...data.edges[0],id:`e${i}`}));h.setRequest(async()=>data);await h.controller.open({seed:"C0"});
 assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.dataset.classId).length,1);
 await allReturned(h);
 assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.dataset.classId).length,24);assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-edge").length,1);
 assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-relationship").length,64);
 assert.match(text(h.get("classes-diagram")),/field/);
 assert.equal(h.get("classes-diagram").querySelector(".classes-relationships").open,false);
 assert.match(text(h.get("classes-state")),/Partial diagram/);
});
test("public context menu actions share guards and return focus to root anchors",async()=>{
 const h=harness(), anchor=h.get("root-method");let calls=0;
 h.controller.showContextMenu({type:"keydown",key:"ContextMenu",currentTarget:anchor,preventDefault(){},stopPropagation(){}},[{label:"Show enclosing class",run:()=>calls++}]);
 await h.button(h.menu(),"Show enclosing class").fire("click");assert.equal(calls,1);assert.equal(h.document.activeElement,anchor);
});

test("large warning sets stay collapsed and bounded outside the diagram status",async()=>{
 const h=harness(), data=diagram();data.warnings=Array.from({length:100},(_,i)=>`Notice ${i}: `+"x".repeat(3000));
 h.setRequest(async()=>data);await h.controller.open({seed:"A"});
 const notices=h.document.body.querySelector(".classes-warnings");assert.equal(notices.open,false);assert.equal(notices.hidden,false);
 assert.match(text(notices),/100 indexing \/ diagram notices/);assert.match(text(notices),/notice shortened/);assert.match(text(notices),/50 more notices omitted/);
 assert.ok(h.get("classes-state").textContent.length<500);assert.ok(text(notices).length<110000);
 h.controller.reset();assert.equal(notices.hidden,true);
});

test("failed expansion restores the prior view controls and can be retried",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const a=h.card("A");
 h.setRequest(async()=>{const error=Error("Expansion unavailable");error.status=400;throw error;});
 await related(h,"A",["B"]);
 assert.equal(h.card("A"),a);assert.match(text(h.get("classes-state")),/Previous diagram retained/);
 await h.button(a,"execute()").fire("click");assert.equal(h.methods.length,1);
 await a.fire("contextmenu");await h.button(h.menu(),"Read class source").fire("click");assert.equal(h.reads.length,1);
 h.setRequest(async()=>diagram(["A","B","C"]));await related(h,"A",["B"]);
 assert.deepEqual(Array.from(h.calls.at(-1).options.body.expanded),["B"]);assert.ok(h.card("B"));assert.equal(h.card("C"),undefined);
});
test("non-revision 409 retains the current class diagram and its callbacks",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const card=h.card("A");
 h.setRequest(async()=>{const error=Error("Storage is busy");error.status=409;error.code="storage_busy";throw error;});
 await related(h,"A",["B"]);
 assert.equal(h.card("A"),card);
 assert.equal(h.get("classes-state").dataset.state,"error");
 assert.match(text(h.get("classes-state")),/Storage is busy.*Previous diagram retained/);
 assert.equal(h.stale.length,0);
 await h.button(card,"execute()").fire("click");assert.equal(h.methods.length,1);
});
test("revision conflict clears old diagram and never restores its callbacks",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const member=h.button(h.card("A"),"execute()");
 h.setRequest(async()=>{const error=Error("Index changed");error.status=409;error.code="revision_conflict";throw error;});
 await related(h,"A",["B"]);
 assert.equal(h.card("A"),undefined);assert.equal(h.get("classes-state").dataset.state,"stale");
 await member.fire("click");assert.equal(h.methods.length,0);
});

test("card headings use short names and retain accessible qualified identities",async()=>{
 const h=harness(), data=diagram();data.nodes[0].label="com.example.very.long.namespace.A";data.nodes[0].class.qualifiedName=data.nodes[0].label;
 data.truncated=true;h.setRequest(async()=>data);await h.controller.open({seed:"A"});
 assert.equal(h.card("A").querySelector("h3").textContent,"A");
 const qualified=h.card("A").querySelector(".classes-qualified");assert.equal(qualified.textContent,data.nodes[0].label);assert.equal(qualified.title,data.nodes[0].label);
 assert.match(h.card("A").getAttribute("aria-label"),/com.example.very.long.namespace.A/);
 assert.match(text(h.get("classes-state")),/some index or diagram details are omitted/);assert.doesNotMatch(text(h.get("classes-state")),/display limits reached/);
});


test("file context menus preserve the existing inline-expansion aria state", async () => {
  const h=harness(), anchor=h.get("file-trigger");anchor.setAttribute("aria-expanded","true");
  h.controller.showContextMenu({currentTarget:anchor,preventDefault(){},stopPropagation(){}},[{label:"Class diagram",run(){}}]);
  h.controller.closeContextMenu();assert.equal(anchor.getAttribute("aria-expanded"),"true");
});

test("queued pre-menu scroll is ignored but actual subsequent scrolling dismisses", async () => {
  const h=harness(), anchor=h.get("file-trigger"), parent=anchor.parentNode;
  parent.scrollTop=100;parent.scrollLeft=0;
  h.controller.showContextMenu({currentTarget:anchor,preventDefault(){},stopPropagation(){}},[{label:"Class diagram",run(){}}]);
  await h.event("scroll",parent);assert.ok(h.menu());
  parent.scrollTop=101;await h.event("scroll",parent);assert.equal(h.menu(),null);
});


test("members reveal is reversible and preserves measured source/method actions without requests", async () => {
 const h=harness();await h.controller.open({seed:"A"});const a=h.card("A"),calls=h.calls.length;
 let reveal=a.querySelector(".classes-members-toggle");assert.equal(reveal.getAttribute("aria-expanded"),"false");
 assert.equal(a.querySelector(".classes-member-detail").hidden,true);assert.equal(a.style.height,"116px");
 await reveal.fire("click");assert.equal(h.card("A"),a);assert.equal(a.querySelector(".classes-member-detail").hidden,false);
 assert.equal(a.querySelector(".classes-members-toggle").getAttribute("aria-expanded"),"true");assert.equal(a.style.height,"390px");
 assert.equal(h.document.activeElement,a.querySelector(".classes-members-toggle"));
 await h.button(a,"execute()").fire("click");assert.equal(h.methods[0].range.startLine,range.startLine);assert.equal(h.methods[0].id,"A.execute");
 await a.querySelector(".classes-members-toggle").fire("click");assert.equal(a.querySelector(".classes-member-detail").hidden,true);
 assert.equal(h.calls.length,calls);assert.equal(h.reads.length,0);
});
test("one arrow per ordered pair bundles kinds but preserves every original identity and range", async () => {
 const h=harness(),data=diagram();data.edges.push({...data.edges[0],id:"return-2",kind:"returns",range:{startLine:20,endLine:23}},
  {...data.edges[0],id:"reverse",owner:"B",target:"A",kind:"parameter",typeName:"A"},
  {...data.edges[0],id:"field-duplicate",range:{startLine:30,endLine:31}});
 freeze(data);const before=JSON.stringify(data);h.setRequest(async()=>data);await h.controller.open({seed:"A"});await related(h,"A",["B"]);
 const arrows=descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-edge");assert.equal(arrows.length,2);
 const labels=descendants(h.get("classes-diagram")).filter(n=>n.tagName==="text").map(n=>n.textContent);
 assert.deepEqual(labels,["field / returns","parameter"]);
 const ledger=h.get("classes-diagram").querySelector(".classes-relationships");assert.equal(ledger.open,false);
 const rows=descendants(ledger).filter(n=>n.className==="classes-relationship");assert.deepEqual(rows.map(n=>n.dataset.edgeId),data.edges.map(e=>e.id));
 const originals=descendants(ledger).filter(n=>n.className==="classes-relation-record").map(n=>JSON.parse(n.textContent));assert.deepEqual(originals,data.edges);
 assert.equal(JSON.stringify(data),before);assert.equal(h.reads.length,0);
});
test("full returned graph is explicit and reversible; selected roots and member state survive", async () => {
 const h=harness();h.setRequest(async()=>diagram(["A","B","C"]));await h.controller.open({seed:"A"});await related(h,"A",["B"]);
 await h.card("B").querySelector(".classes-members-toggle").fire("click");const calls=h.calls.length;
 await allReturned(h);assert.ok(h.card("C"));assert.equal(h.button(h.document.body,"Show hierarchy and selected classes").getAttribute("aria-pressed"),"true");
 await h.button(h.document.body,"Show hierarchy and selected classes").fire("click");assert.equal(h.card("C"),undefined);assert.ok(h.card("B"));
 assert.equal(h.card("B").querySelector(".classes-member-detail").hidden,false);assert.equal(h.calls.length,calls);
 h.get("classes-unmatched").checked=true;await h.get("classes-unmatched").fire("change");assert.deepEqual(Array.from(h.calls.at(-1).options.body.expanded),["B"]);
 await h.controller.open({seed:"A"});assert.equal(h.card("B"),undefined);assert.equal(h.card("A").querySelector(".classes-member-detail").hidden,true);
});
test("search results collapse after selection and Change class restores them without another request", async () => {
 const h=harness();await h.controller.open({path:"src/A.java"});const results=h.get("classes-results"),calls=h.calls.length;
 assert.equal(results.hidden,true);const change=h.button(h.document.body,"Change class");assert.equal(change.getAttribute("aria-expanded"),"false");
 await change.fire("click");assert.equal(results.hidden,false);assert.equal(change.getAttribute("aria-expanded"),"true");assert.equal(h.document.activeElement,h.get("classes-query"));
 assert.equal(h.calls.length,calls);await results.children[0].fire("click");assert.equal(results.hidden,true);
});
test("chooser presents bounded qualified identities and directions and supports keyboard cancel", async () => {
 const h=harness(),data=diagram();data.nodes[1].class.symbol.name="SameName";data.nodes[1].class.qualifiedName="other.SameName";
 data.edges.push({...data.edges[0],id:"reverse",owner:"B",target:"A",kind:"returns"});h.setRequest(async()=>data);await h.controller.open({seed:"A"});
 await h.card("A").fire("keydown",{key:"F10",shiftKey:true});await h.button(h.menu(),"Show related classes").fire("click");
 const chooser=h.document.body.querySelector(".classes-chooser");assert.match(text(chooser),/outgoing field.*incoming returns.*other.SameName.*src\/B.java/);
 assert.equal(h.calls.length,1);assert.equal(h.button(chooser,"Add selected classes").disabled,true);assert.equal(h.document.activeElement.tagName,"input");
 await chooser.fire("keydown",{key:"Escape"});assert.equal(h.document.body.querySelector(".classes-chooser"),null);assert.equal(h.document.activeElement,h.card("A"));
});
test("chooser enforces the twelve selected-root limit before any request", async () => {
 const h=harness();h.setRequest(async()=>diagram(Array.from({length:24},(_,i)=>`C${i}`)));await h.controller.open({seed:"C0"});
 await h.card("C0").fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");const chooser=h.document.body.querySelector(".classes-chooser");
 const checks=descendants(chooser).filter(n=>n.tagName==="input");assert.equal(checks.length,23);
 for(const check of checks.slice(0,13)){check.checked=true;await check.fire("change");}
 const add=h.button(chooser,"Add selected classes");assert.equal(add.disabled,true);await add.fire("click");assert.equal(h.calls.length,1);
 checks[12].checked=false;await checks[12].fire("change");assert.equal(add.disabled,false);await add.fire("click");assert.equal(h.calls.at(-1).options.body.expanded.length,12);
 assert.equal(descendants(h.get("classes-diagram")).filter(n=>n.dataset.classId).length,13);
});
test("adding from full returned view includes its actual bridge root before its chosen neighbors", async () => {
 const h=harness(),data=diagram(["A","B","C"]);data.edges.push({...data.edges[0],id:"bc",owner:"B",target:"C"});h.setRequest(async()=>data);
 await h.controller.open({seed:"A"});await allReturned(h);await related(h,"B",["C"]);
 assert.deepEqual(Array.from(h.calls.at(-1).options.body.expanded),["B","C"]);
 await h.button(h.document.body,"Show hierarchy and selected classes").fire("click");assert.ok(h.card("B"));assert.ok(h.card("C"));
});
test("stale chooser and full-graph controls cannot publish across session or revision changes", async () => {
 const h=harness();await h.controller.open({seed:"A"});await h.card("A").fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");
 const chooser=h.document.body.querySelector(".classes-chooser"),check=descendants(chooser).find(n=>n.tagName==="input");check.checked=true;await check.fire("change");
 const add=h.button(chooser,"Add selected classes"),all=h.button(h.document.body,"Show all returned classes");h.setSession("other");await add.fire("click");await all.fire("click");assert.equal(h.calls.length,1);assert.equal(h.card("B"),undefined);
 h.controller.reset();await add.fire("click");assert.equal(h.calls.length,1);assert.equal(h.document.body.querySelector(".classes-chooser"),null);
});
for (const status of [401,403]) test(`authentication failure ${status} clears class state and old actions`,async()=>{
 const h=harness();await h.controller.open({seed:"A"});const member=h.button(h.card("A"),"execute()");
 h.setRequest(async()=>{const e=Error("Not authorized");e.status=status;throw e;});await related(h,"A",["B"]);
 assert.equal(h.card("A"),undefined);assert.equal(h.document.body.querySelector(".classes-controls").hidden,true);await member.fire("click");assert.equal(h.methods.length,0);
});


test("late search pagination cannot replace a selected diagram status or notices", async () => {
 const h=harness(),page=deferred();h.setRequest(async(url,options)=>{
  if(options)return {...diagram(),warnings:["CURRENT DIAGRAM NOTICE"]};
  const offset=new URL(url,"http://local").searchParams.get("offset");
  return offset==="100"?page.promise:{revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},items:[definition("A")],nextOffset:100,warnings:[]};
 });
 await h.controller.open({});const results=h.get("classes-results"),choose=results.children[0];
 const more=h.button(results,"More classes").fire("click");await choose.fire("click");
 const status=h.get("classes-state").textContent;
 page.resolve({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},items:[definition("B")],warnings:["OLD SEARCH WARNING"],truncated:true});await more;
 assert.equal(h.get("classes-state").textContent,status);assert.equal(results.children.length,1);assert.equal(results.hidden,true);
 const notices=h.document.body.querySelector(".classes-warnings");assert.match(text(notices),/CURRENT DIAGRAM NOTICE/);assert.doesNotMatch(text(notices),/OLD SEARCH WARNING/);
});


test("choosing a class moves focus from hidden search results to its focus card",async()=>{
 const h=harness();await h.controller.open({});const choose=h.get("classes-results").children[0];choose.focus();await choose.fire("click");
 assert.equal(h.get("classes-results").hidden,true);assert.equal(h.document.activeElement,h.card("A"));
});
test("Add selected returns focus to its originating card after success and recoverable failure",async()=>{
 const h=harness(),data=diagram(["A","B","C"]);data.edges.push({...data.edges[0],id:"bc",owner:"B",target:"C"});h.setRequest(async()=>data);
 await h.controller.open({seed:"A"});await related(h,"A",["B"]);assert.equal(h.document.activeElement,h.card("A"));
 await related(h,"B",["C"]);assert.equal(h.document.activeElement,h.card("B"));
 await h.controller.open({seed:"A"});const a=h.card("A");h.setRequest(async()=>{const e=Error("Temporary failure");e.status=400;throw e;});
 await related(h,"A",["B"]);assert.equal(h.document.activeElement,a);assert.equal(h.card("A"),a);assert.equal(h.get("classes-state").dataset.state,"error");
});
test("late Add responses never steal focus from a newer diagram",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const pending=deferred();h.setRequest((_,options)=>options.body.seed==="C"?Promise.resolve(diagram(["C"])):pending.promise);
 await h.card("A").fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");const chooser=h.document.body.querySelector(".classes-chooser");
 const check=descendants(chooser).find(n=>n.tagName==="input");check.checked=true;await check.fire("change");const add=h.button(chooser,"Add selected classes").fire("click");
 await h.controller.open({seed:"C"});const c=h.card("C");assert.equal(h.document.activeElement,c);pending.resolve(diagram());await add;assert.equal(h.document.activeElement,c);
});
test("Add does not steal focus when the user navigates elsewhere while loading",async()=>{
 const h=harness();await h.controller.open({seed:"A"});const pending=deferred();h.setRequest(()=>pending.promise);
 await h.card("A").fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");const chooser=h.document.body.querySelector(".classes-chooser");
 const check=descendants(chooser).find(n=>n.tagName==="input");check.checked=true;await check.fire("change");const add=h.button(chooser,"Add selected classes").fire("click");
 h.get("classes-query").focus();pending.resolve(diagram());await add;assert.equal(h.document.activeElement,h.get("classes-query"));
});


test("same-column reverse relationships use opposite gutters and preserve direction",async()=>{
 const h=harness(),data=diagram(["A","B","C"]);
 data.edges.push({...data.edges[1],id:"reverse-ca",owner:"C",target:"A",kind:"returns"}, {...data.edges[0],id:"self-a",target:"A"});
 freeze(data);h.setRequest(async()=>data);await h.controller.open({seed:"A"});await related(h,"A",["B","C"]);await allReturned(h);
 const paths=descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-edge");
 const down=paths.find(n=>text(n).includes("A → C")),up=paths.find(n=>text(n).includes("C → A")),self=paths.find(n=>text(n).includes("A → A"));
 assert.match(down.getAttribute("d"),/^M 324 82 H (3[4-9]\d) V 298 H 324$/);
 assert.equal(up.getAttribute("d"),"M 24 298 H 8 V 82 H 24");
 assert.match(self.getAttribute("d"),/^M 324 64 H 344 V 180 H 174 V 140$/);
 const labels=descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-edge-label");
 const downLabel=labels.find(n=>text(n).includes("A → C")),upLabel=labels.find(n=>text(n).includes("C → A"));
 assert.equal(downLabel.querySelector("rect").getAttribute("y"),"159");assert.equal(upLabel.querySelector("rect").getAttribute("y"),"199");
 const records=descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-relation-record").map(n=>JSON.parse(n.textContent));assert.deepEqual(records,data.edges);
});


// Public synthetic hierarchy: no source reads or compiler-resolution assumptions.
const relation = (id, owner, target, kind) => ({id, owner, target, kind, typeName:target,
 matchKind:"syntaxCandidate",candidateIds:[target],path:`src/${owner}.java`,range});
function hierarchyDiagram() {
 const data=diagram(["Seed","Base","Root","Child","Leaf","Face","Peer","Assoc","AssocBase","Other","OtherBase"]);
 data.nodes.find(n=>n.id==="Face").class.declarationKind="interface";
 data.edges=[relation("seed-base","Seed","Base","extends"),relation("base-root","Base","Root","extends"),
  relation("child-seed","Child","Seed","extends"),relation("leaf-child","Leaf","Child","extends"),
  relation("seed-face","Seed","Face","implements"),relation("child-face","Child","Face","implements"),
  relation("peer-face","Peer","Face","implements"),relation("leaf-assoc","Leaf","Assoc","field"),
  relation("assoc-base","Assoc","AssocBase","extends"),relation("assocbase-root","AssocBase","Root","field"),
  relation("seedbase-field","Seed","Base","field"),relation("leafbase-return","Leaf","Base","returns"),
  relation("childface-param","Child","Face","parameter"),relation("other-base","Other","OtherBase","extends")];
 return data;
}
const shownIds = h => descendants(h.get("classes-diagram")).filter(n=>n.dataset.classId).map(n=>n.dataset.classId);
const shownRecords = h => descendants(h.get("classes-diagram")).filter(n=>n.className==="classes-relation-record").map(n=>JSON.parse(n.textContent));
const hierarchyIds=["Seed","Base","Root","Child","Leaf","Face"];
test("initial indexed hierarchy shows transitive parents, subtypes and interfaces, not associations",async()=>{
 const h=harness(),data=freeze(hierarchyDiagram()),before=JSON.stringify(data);h.setRequest(async()=>data);
 await h.controller.open({seed:"Seed"});assert.deepEqual(shownIds(h),hierarchyIds);
 assert.deepEqual(shownRecords(h),data.edges.slice(0,6));
 assert.match(text(h.get("classes-state")),/^6 shown · 6 relationships/);
 assert.equal(h.calls[0].options.body.includeHierarchy,true);assert.equal(h.calls[0].options.body.expanded.length,0);
 for(const id of hierarchyIds)assert.equal(h.card(id).querySelector(".classes-member-detail").hidden,true);
 assert.equal(h.get("classes-results").hidden,true);assert.equal(h.get("classes-diagram").querySelector(".classes-relationships").open,false);
 assert.equal(h.reads.length,0);assert.equal(h.methods.length,0);assert.equal(JSON.stringify(data),before);
});
test("deep hierarchy chooser excludes already visible classes and adds only the chosen association root",async()=>{
 const h=harness(),data=freeze(hierarchyDiagram());h.setRequest(async()=>data);await h.controller.open({seed:"Seed"});
 const leaf=h.card("Leaf"),left=leaf.style.left,top=leaf.style.top;
 await leaf.fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");
 const chooser=h.document.body.querySelector(".classes-chooser");
 assert.deepEqual(descendants(chooser).filter(n=>n.tagName==="input").map(n=>n.value),["Assoc"]);
 assert.match(text(chooser),/Indexed hierarchy is already shown/);assert.doesNotMatch(text(chooser),/Include Leaf/);
 await h.button(chooser,"Cancel").fire("click");await related(h,"Leaf",["Assoc"]);
 assert.deepEqual(Array.from(h.calls.at(-1).options.body.expanded),["Assoc"]);
 assert.equal(h.calls.at(-1).options.body.includeHierarchy,true);
 assert.deepEqual(shownIds(h),[...hierarchyIds,"Assoc","AssocBase"]);
 assert.deepEqual(shownRecords(h),[...data.edges.slice(0,6),...data.edges.slice(7,9)]);
 assert.equal(h.card("Leaf"),leaf);assert.equal(leaf.style.left,left);assert.equal(leaf.style.top,top);
 assert.equal(h.document.activeElement,leaf);assert.equal(h.reads.length,0);
});
test("full returned hierarchy view restores original association evidence and reverses without a request",async()=>{
 const h=harness(),data=freeze(hierarchyDiagram()),before=JSON.stringify(data);h.setRequest(async()=>data);
 await h.controller.open({seed:"Seed"});await related(h,"Leaf",["Assoc"]);
 await h.card("Base").querySelector(".classes-members-toggle").fire("click");const calls=h.calls.length;
 await allReturned(h);assert.deepEqual(shownRecords(h),data.edges);assert.equal(shownIds(h).length,data.nodes.length);
 await h.button(h.document.body,"Show hierarchy and selected classes").fire("click");
 assert.deepEqual(shownIds(h),[...hierarchyIds,"Assoc","AssocBase"]);assert.deepEqual(shownRecords(h),[...data.edges.slice(0,6),...data.edges.slice(7,9)]);
 assert.equal(h.card("Base").querySelector(".classes-member-detail").hidden,false);
 assert.equal(h.calls.length,calls);assert.equal(h.reads.length,0);assert.equal(JSON.stringify(data),before);
 await h.controller.open({seed:"Seed"});assert.deepEqual(shownIds(h),hierarchyIds);
 assert.equal(h.card("Base").querySelector(".classes-member-detail").hidden,true);
 assert.equal(h.calls.at(-1).options.body.expanded.length,0);assert.equal(h.calls.at(-1).options.body.includeHierarchy,true);
});
test("hierarchy cycles terminate and hints never bridge to indexed classes",async()=>{
 const h=harness(),data=hierarchyDiagram();data.edges.unshift(relation("cycle","Root","Leaf","extends"),relation("self","Seed","Seed","extends"));
 data.nodes.push({id:"hint",label:"UnknownBase",kind:"ambiguous",class:null,expandable:false},node("BehindHint"));
 data.edges.push({...relation("hint-edge","Root","hint","extends"),matchKind:"ambiguous",candidateIds:["BehindHint"]},
  relation("not-an-actual-bridge","BehindHint","hint","extends"),{...relation("field-hint","Leaf","hint","field"),matchKind:"unmatched"});
 freeze(data);const before=JSON.stringify(data);h.setRequest(async()=>data);await h.controller.open({seed:"Seed"});
 assert.deepEqual(shownIds(h),hierarchyIds);assert.equal(shownRecords(h).length,8);
 h.get("classes-unmatched").checked=true;await h.get("classes-unmatched").fire("change");
 assert.deepEqual(shownIds(h),[...hierarchyIds,"hint"]);assert.equal(h.card("BehindHint"),undefined);
 assert.deepEqual(shownRecords(h).map(e=>e.id),["cycle","self",...data.edges.slice(2,8).map(e=>e.id),"hint-edge","field-hint"]);
 assert.equal(h.card("hint").listeners.contextmenu,undefined);
 assert.equal(h.calls.at(-1).options.body.includeHierarchy,true);assert.equal(h.calls.at(-1).options.body.includeUnmatched,true);
 assert.equal(h.reads.length,0);assert.equal(JSON.stringify(data),before);
});
test("every diagram entry and reload requests indexed hierarchy without reading source",async()=>{
 const h=harness();await h.controller.open({path:"src/A.java"});await related(h,"A",["B"]);
 h.get("classes-unmatched").checked=true;await h.get("classes-unmatched").fire("change");
 await h.card("A").fire("contextmenu");await h.button(h.menu(),"Focus this class").fire("click");
 await h.controller.open({seed:"A.execute"});
 const requests=h.calls.filter(call=>call.options);assert.equal(requests.length,5);
 for(const call of requests)assert.equal(call.options.body.includeHierarchy,true);
 assert.equal(h.reads.length,0);
});
test("automatic hierarchy obeys 24 nodes and 64 edges without consuming manual root allowance",async()=>{
 const h=harness(),data=diagram(Array.from({length:30},(_,i)=>`C${i}`));
 data.edges=data.nodes.slice(1).map((n,i)=>relation(`hierarchy-${i}`,`C${i}`,n.id,"extends"));
 data.edges.push(...Array.from({length:50},(_,i)=>relation(`repeat-${i}`,"C1","C0","implements")));
 h.setRequest(async()=>data);await h.controller.open({seed:"C0"});
 assert.equal(shownIds(h).length,24);assert.equal(shownRecords(h).length,58);assert.match(text(h.get("classes-state")),/Partial diagram/);
 assert.equal(h.calls[0].options.body.expanded.length,0);assert.equal(h.reads.length,0);
 const boundary=diagram(["A","B"]);boundary.edges=Array.from({length:64},(_,i)=>relation(`field-${i}`,"A","B","field"));
 boundary.edges.push(relation("out-of-bound-hierarchy","A","B","extends"));h.setRequest(async()=>boundary);
 await h.controller.open({seed:"A"});assert.deepEqual(shownIds(h),["A"]);assert.deepEqual(shownRecords(h),[]);
});

test("a shared base does not reveal peers until that base is focused or the peer is selected",async()=>{
 const h=harness(),data=diagram(["Derived","Base","Peer","Child","Grandchild","PeerChild","OtherFace"]);
 data.edges=[relation("derived-base","Derived","Base","extends"),relation("peer-base","Peer","Base","extends"),
  relation("child-derived","Child","Derived","extends"),relation("grandchild-child","Grandchild","Child","extends"),
  relation("peerchild-peer","PeerChild","Peer","extends"),relation("child-face","Child","OtherFace","implements")];
 freeze(data);h.setRequest(async(_,options)=>({...data,seed:options.body.seed}));
 await h.controller.open({seed:"Derived"});assert.deepEqual(shownIds(h),["Derived","Base","Child","Grandchild"]);
 assert.deepEqual(shownRecords(h).map(e=>e.id),["derived-base","child-derived","grandchild-child"]);
 await related(h,"Base",["Peer"]);assert.deepEqual(Array.from(h.calls.at(-1).options.body.expanded),["Peer"]);
 assert.deepEqual(shownIds(h),["Derived","Base","Child","Grandchild","Peer","PeerChild"]);
 await h.card("Base").fire("contextmenu");await h.button(h.menu(),"Focus this class").fire("click");
 assert.deepEqual(shownIds(h),["Derived","Base","Peer","Child","Grandchild","PeerChild"]);
 assert.equal(h.card("OtherFace"),undefined);assert.equal(h.calls.at(-1).options.body.expanded.length,0);
 assert.equal(h.reads.length,0);
});

test("full returned-only bridge choices identify links to automatic hierarchy without duplicate self choices",async()=>{
 const h=harness(),data=hierarchyDiagram();data.edges.push(relation("assoc-self","Assoc","Assoc","field"));
 h.setRequest(async()=>data);await h.controller.open({seed:"Seed"});await allReturned(h);
 await h.card("Assoc").fire("contextmenu");await h.button(h.menu(),"Show related classes").fire("click");
 const chooser=h.document.body.querySelector(".classes-chooser"),checks=descendants(chooser).filter(n=>n.tagName==="input");
 assert.deepEqual(checks.map(n=>n.value),["Assoc","AssocBase"]);assert.equal(checks[0].checked,true);
 assert.match(text(checks[0].parentNode),/incoming field/);assert.equal(h.reads.length,0);
});


// Member navigation delegates cached resolution; the class UI never searches type text.
const memberRows = (h, id = "A") => descendants(h.card(id)).filter(n=>n.className==="classes-member");
async function revealMembers(h, id = "A") { await h.card(id).querySelector(".classes-members-toggle").fire("click"); }
test("member navigation is optional; method name stays direct and type text is inert without a hook",async()=>{
 const h=harness();await h.controller.open({seed:"A"});await revealMembers(h);
 const [field,method]=memberRows(h);
 assert.equal(field.querySelector(".classes-member-type").tagName,"span");assert.equal(field.querySelector(".classes-member-menu"),null);
 await method.querySelector(".classes-member-name").fire("click");assert.equal(h.methods[0].id,"A.execute");
 assert.deepEqual(JSON.parse(JSON.stringify(h.methods[0].range)),range);assert.equal(h.calls.length,1);assert.equal(h.reads.length,0);
});
test("opening, revealing and refolding members never fetches navigation or source",async()=>{
 const h=harness({navigation:true});await h.controller.open({seed:"A"});assert.equal(h.navigations.length,0);
 await revealMembers(h);await revealMembers(h);assert.equal(h.navigations.length,0);assert.equal(h.calls.length,1);assert.equal(h.reads.length,0);
 await revealMembers(h);const [field,method]=memberRows(h);
 await field.querySelector(".classes-member-name").fire("click");assert.equal(h.navigations.length,0);
 await method.querySelector(".classes-member-name").fire("click");assert.equal(h.methods[0].id,"A.execute");assert.equal(h.navigations.length,0);
 await method.querySelector(".classes-member-type").fire("click");assert.equal(h.methods.length,1);assert.equal(h.navigations.length,1);
 assert.equal(h.calls.length,1);assert.equal(h.reads.length,0);
});
test("type hint and member menu pass only exact recorded owner/name/byte selectors, including overloads",async()=>{
 const h=harness({navigation:true}),data=diagram();
 data.nodes[0].class.fields[0]={name:"数据",typeHint:"same.Name<Δ>",path:"src/A.java",range:{startByte:13,endByte:39,startLine:2,endLine:2}};
 data.nodes[0].class.methods=[{name:"运行",symbolId:"A.运行#1",path:"src/A.java",range:{startByte:40,endByte:70,startLine:3,endLine:5}},
 {name:"运行",symbolId:"A.运行#2",typeHint:"same.Name<Δ>",path:"src/A.java",range:{startByte:75,endByte:115,startLine:6,endLine:8}}];
 freeze(data);const before=JSON.stringify(data);h.setRequest(async()=>data);await h.controller.open({seed:"A"});await revealMembers(h);
 const [field,one,two]=memberRows(h);await field.querySelector(".classes-member-type").fire("click");
 await one.querySelector(".classes-member-menu").fire("click");await two.querySelector(".classes-member-type").fire("click");
 assert.deepEqual(h.navigations.map(n=>JSON.parse(JSON.stringify(n.selector))),[
 {classId:"A",memberName:"数据",startByte:13,endByte:39},{classId:"A",memberName:"运行",startByte:40,endByte:70},{classId:"A",memberName:"运行",startByte:75,endByte:115}]);
 for(const item of h.navigations)assert.equal(item.scope.isCurrent(),true);
 await one.querySelector(".classes-member-name").fire("click");await two.querySelector(".classes-member-name").fire("click");
 assert.deepEqual(h.methods.map(m=>m.id),["A.运行#1","A.运行#2"]);
 assert.deepEqual(h.methods.map(m=>JSON.parse(JSON.stringify(m.range))),data.nodes[0].class.methods.map(m=>m.range));
 assert.equal(JSON.stringify(data),before);assert.equal(h.calls.length,1);assert.equal(h.reads.length,0);
});
test("member right-click, keyboard and visible touch action stop bubbling into class actions",async()=>{
 const h=harness({navigation:true});await h.controller.open({seed:"A"});await revealMembers(h);
 const [field,method]=memberRows(h);
 for(const [target,type,details] of [[field,"contextmenu",{clientX:88,clientY:99}],
  [method.querySelector(".classes-member-name"),"keydown",{key:"F10",shiftKey:true}],
  [field.querySelector(".classes-member-type"),"keydown",{key:"ContextMenu"}],
  [method.querySelector(".classes-member-menu"),"click",{pointerType:"touch"}]]){
  const event=await target.fire(type,details);assert.equal(event.prevented,true);assert.equal(event.stopped,true);assert.equal(h.menu(),null);
 }
 assert.equal(h.navigations.length,4);assert.equal(h.navigations[0].event.clientX,88);
 assert.equal(h.navigations[3].event.currentTarget,method.querySelector(".classes-member-menu"));
 assert.equal(h.methods.length,0);assert.equal(h.reads.length,0);
 await field.querySelector(".classes-member-type").fire("keydown",{key:"ArrowDown"});assert.equal(h.navigations.length,4);
 await h.card("A").fire("contextmenu");assert.ok(h.menu());assert.equal(h.navigations.length,4);
});
test("shared member menu preserves focus and safe labels without nested buttons",async()=>{
 const h=harness({navigation:true}),data=diagram();data.nodes[0].class.fields[0].typeHint="<img src=x onerror=bad()>";
 data.nodes[0].class.methods[0].name="<script>运行</script>";h.setRequest(async()=>freeze(data));await h.controller.open({seed:"A"});await revealMembers(h);
 let run=0;h.setNavigate((event,selector,{isCurrent})=>h.controller.showContextMenu(event,[{label:`Declared type ${data.nodes[0].class.fields[0].typeHint}`,run:()=>{if(isCurrent())run++;}}]));
 const anchor=memberRows(h)[0].querySelector(".classes-member-type");anchor.focus();await anchor.fire("click");
 assert.match(text(h.menu()),/<img src=x onerror=bad\(\)>/);
 assert.equal(descendants(h.document.body).some(n=>n.tagName==="script"||n.tagName==="img"),false);
 for(const button of descendants(h.card("A")).filter(n=>n.tagName==="button"))assert.equal(descendants(button).slice(1).some(n=>n.tagName==="button"),false);
 await h.menu().fire("keydown",{key:"Escape"});assert.equal(h.document.activeElement,anchor);assert.equal(run,0);
 await anchor.fire("click");await h.menu().children[0].fire("click");assert.equal(run,1);assert.equal(h.document.activeElement,anchor);assert.equal(anchor.getAttribute("aria-expanded"),"false");
 assert.equal(h.calls.length,1);assert.equal(h.reads.length,0);
});
for(const change of ["session","revision","reset","hidden","fold","rerender","new diagram","pending diagram"])test(`member scope rejects stale ${change} and old triggers`,async()=>{
 const h=harness({navigation:true});await h.controller.open({seed:"A"});await revealMembers(h);
 const trigger=memberRows(h)[0].querySelector(".classes-member-type");await trigger.fire("click");const {isCurrent}=h.navigations[0].scope;assert.equal(isCurrent(),true);
 let work,pending;
 if(change==="session")h.setSession("two");
 if(change==="revision")h.setRevision({indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:2});
 if(change==="reset")h.controller.reset();
 if(change==="hidden")h.get("classes-panel").hidden=true;
 if(change==="fold")await revealMembers(h);
 if(change==="rerender")await allReturned(h);
 if(change==="new diagram")await h.controller.open({seed:"A"});
 if(change==="pending diagram"){pending=deferred();h.setRequest(()=>pending.promise);work=h.controller.open({seed:"B"});}
 assert.equal(isCurrent(),false);await trigger.fire("click");assert.equal(h.navigations.length,1);assert.equal(h.reads.length,0);
 if(pending){pending.resolve(diagram(["B"]));await work;assert.equal(isCurrent(),false);}
});
test("recoverable requests cannot revive a captured navigation ticket; fresh member intents can retry",async()=>{
 const h=harness({navigation:true});await h.controller.open({seed:"A"});await revealMembers(h);
 const trigger=memberRows(h)[0].querySelector(".classes-member-type");await trigger.fire("click");const {isCurrent}=h.navigations[0].scope;
 h.setRequest(async()=>{const error=Error("Temporary failure");error.status=400;throw error;});await h.controller.open({seed:"B"});
 assert.equal(isCurrent(),false);await trigger.fire("click");assert.equal(h.navigations.length,2);assert.equal(h.navigations[1].scope.isCurrent(),true);
 assert.equal(h.reads.length,0);
});
test("delayed shared navigation work cannot publish or act after panel changes",async()=>{
 const h=harness({navigation:true});await h.controller.open({seed:"A"});await revealMembers(h);
 const pending=deferred();let published=0;
 h.setNavigate(async(event,selector,{isCurrent})=>{await pending.promise;if(isCurrent())published++;});
 const work=memberRows(h)[0].querySelector(".classes-member-type").fire("click");
 h.get("classes-panel").hidden=true;pending.resolve();await work;assert.equal(published,0);assert.equal(h.menu(),null);assert.equal(h.reads.length,0);
});


const menuEvent = anchor => ({type:"keydown",currentTarget:anchor,target:anchor,preventDefault(){},stopPropagation(){}});
test("shared menu reports replacement, action and dismissal once and preserves action focus order",async()=>{
 const h=harness(),anchor=h.get("lifecycle-anchor"),events=[];
 anchor.setAttribute("aria-expanded","true");
 const first=h.controller.showContextMenu(menuEvent(anchor),[{label:"First",run(){}}],{onClose:reason=>events.push(`first:${reason}`)});
 h.controller.showContextMenu(menuEvent(anchor),[{label:"Second",run(){events.push(`run:${h.document.activeElement===anchor}:${h.menu()===null}`);}}],{onClose:reason=>events.push(`second:${reason}`)});
 assert.deepEqual(events,["first:replace"]);first.close();assert.ok(h.menu());
 await h.button(h.menu(),"Second").fire("click");
 assert.deepEqual(events,["first:replace","second:action","run:true:true"]);assert.equal(anchor.getAttribute("aria-expanded"),"true");
 const last=h.controller.showContextMenu(menuEvent(anchor),[{label:"Last",run(){}}],{onClose:reason=>events.push(`last:${reason}`)});
 last.close();last.close();h.controller.closeContextMenu();
 assert.deepEqual(events,["first:replace","second:action","run:true:true","last:dismiss"]);
});
test("shared menu clears lifecycle callback before reentrant close and ignores retained menu actions",async()=>{
 const h=harness(),anchor=h.get("lifecycle-anchor");let closed=0,runs=0;
 h.controller.showContextMenu(menuEvent(anchor),[{label:"Old",run(){runs++;}}],{onClose(){closed++;h.controller.closeContextMenu();}});
 const oldMenu=h.menu(),oldAction=oldMenu.children[0];
 h.controller.showContextMenu(menuEvent(anchor),[{label:"New",run(){}}]);assert.equal(closed,1);
 await oldAction.fire("click");await oldMenu.fire("keydown",{key:"Escape"});
 assert.equal(runs,0);assert.equal(h.menu().children[0].textContent,"New");assert.equal(closed,1);
});
for(const dismissal of ["focus","escape","outside","scroll","reset"])test(`shared menu ${dismissal} dismissal notifies its owner`,async()=>{
 const h=harness(),anchor=h.get("lifecycle-anchor"),reasons=[];
 h.controller.showContextMenu(menuEvent(anchor),[{label:"Active",run(){}}],{onClose:reason=>reasons.push(reason)});
 if(dismissal==="focus")h.get("other-anchor").focus();
 if(dismissal==="escape")await h.menu().fire("keydown",{key:"Escape"});
 if(dismissal==="outside")await h.event("pointerdown",h.get("other-anchor"));
 if(dismissal==="scroll")await h.event("scroll",h.get("classes-diagram"));
 if(dismissal==="reset")h.controller.reset();
 assert.equal(h.menu(),null);assert.deepEqual(reasons,["dismiss"]);
});
function integratedNavigation() {
 const h=harness({navigation:true}),navigationSource=fs.readFileSync(path.join(__dirname,"../web/navigation.js"),"utf8");
 vm.runInNewContext(navigationSource,{window:h.window,document:h.document,console});
 const nav=h.window.BaleygNavigation,selected=[];
 let request=async()=>({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},targets:[{symbol:{id:"A.run",name:"run",path:"A.java",range},action:"sequence",reason:"declaration",matchKind:"measured"}],warnings:[]});
 nav.init({currentRevision:()=>({indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1}),currentSession:()=>"one",request:(...args)=>request(...args),
  showMenu:(event,actions,options)=>h.controller.showContextMenu(event,actions,options),
  selectMethod:symbol=>selected.push(symbol),openClass:symbol=>selected.push(symbol)});
 h.setNavigate((...args)=>nav.open(...args));
 return {...h,nav,selected,setNavigationRequest(fn){request=fn;}};
}
for(const replacement of [false,true])test(`actual shared navigation never reopens after focus dismissal${replacement?" and newer class menu":""}`,async()=>{
 const h=integratedNavigation(),pending=deferred();h.setNavigationRequest(()=>pending.promise);
 const anchor=h.get("source-navigation-anchor"),other=h.get("other-class-anchor");
 const work=h.nav.open(menuEvent(anchor),{path:"A.java",line:1});assert.match(text(h.menu()),/Finding cached/);
 other.focus();assert.equal(h.menu(),null);
 if(replacement)h.controller.showContextMenu(menuEvent(other),[{label:"Other class menu",run(){}}]);
 pending.resolve({revision:{indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1},targets:[],warnings:[]});await work;
 if(replacement){assert.equal(h.menu().children[0].textContent,"Other class menu");assert.equal(h.document.activeElement,h.menu().children[0]);}
 else{assert.equal(h.menu(),null);assert.equal(h.document.activeElement,other);}
 assert.equal(h.selected.length,0);assert.equal(h.reads.length,0);
});
test("actual shared navigation action survives close-before-invoke, then cannot run again",async()=>{
 const h=integratedNavigation();await h.controller.open({seed:"A"});await revealMembers(h);
 const trigger=memberRows(h)[0].querySelector(".classes-member-type");await trigger.fire("click");
 const action=h.menu().children[0];assert.match(action.textContent,/Sequence/);
 await action.fire("click");assert.equal(h.selected.length,1);assert.equal(h.selected[0].id,"A.run");
 assert.equal(h.document.activeElement,trigger);assert.equal(h.menu(),null);
 await action.fire("click");assert.equal(h.selected.length,1);assert.equal(h.reads.length,0);
});
test("actual navigation reset closes its menu but cannot close a replacement class menu",async()=>{
 const h=integratedNavigation(),anchor=h.get("source-navigation-anchor");
 await h.nav.open(menuEvent(anchor),{path:"A.java",line:1});assert.ok(h.menu());h.nav.reset();assert.equal(h.menu(),null);
 await h.nav.open(menuEvent(anchor),{path:"A.java",line:1});
 h.controller.showContextMenu(menuEvent(h.get("other-class-anchor")),[{label:"Other class menu",run(){}}]);
 h.nav.reset();assert.equal(h.menu().children[0].textContent,"Other class menu");
});

test("class results cannot cross generation with the same revision",async()=>{
  const h=harness(),gate=deferred();h.setRequest(()=>gate.promise);
  const work=h.controller.open({seed:'A'});
  h.setRevision({indexGeneration:'87654321-4321-4321-8321-abcdef123456',indexRevision:1});
  gate.resolve(diagram());await work;
  assert.equal(h.card('A'),undefined);
});

test("class search, page, diagram, and expansion reject a reused revision and ask for status refresh",async()=>{
 const old={indexGeneration:'12345678-1234-4123-8123-123456789abc',indexRevision:1};
 const next={indexGeneration:'87654321-4321-4321-8321-abcdef123456',indexRevision:1};
 for(const action of ['search','page','diagram','expansion']) {
   const h=harness();
   if(action==='expansion') await h.controller.open({seed:'A'});
   h.setRequest((url,options)=>options ? diagram(['A'],next) : {revision:next,items:[definition('A')],nextOffset:1});
   if(action==='search') await h.controller.open({});
   if(action==='page') { h.setRequest((url,options)=>options ? diagram() : {revision:old,items:[definition('A')],nextOffset:100}); await h.controller.open({}); h.setRequest(()=>({revision:next,items:[],nextOffset:null})); await h.button(h.get('classes-results'),'More classes').fire('click'); }
   if(action==='diagram') await h.controller.open({seed:'A'});
   if(action==='expansion') {h.get('classes-unmatched').checked=true;await h.get('classes-unmatched').fire('change');}
   const call=h.calls.at(-1);
   if(call.options) assert.deepEqual(JSON.parse(JSON.stringify(call.options.body.expectedRevision)),old,action);
   else {const params=new URL(call.url,'http://local').searchParams;
     assert.equal(params.get('indexGeneration'),old.indexGeneration,action);
     assert.equal(params.get('indexRevision'),'1',action);
   }
   assert.equal(h.stale.length,1,action);
   assert.equal(h.card('A'),undefined,action);
   assert.match(h.get('classes-state').textContent,/revision changed/i,action);
 }
});
