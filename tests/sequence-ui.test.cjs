"use strict";
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const vm = require("node:vm");
class Element {
  constructor(tag) { this.tag = tag; this.children = []; this.attrs = {}; this.textContent = ""; this.listeners = {}; }
  setAttribute(key, value) { this.attrs[key] = value; }
  append(...nodes) { this.children.push(...nodes); }
  replaceChildren(...nodes) { this.children = nodes; }
  addEventListener(name, handler) { this.listeners[name] = handler; }
  focus() { this.focused = true; }
}
function render(participants) {
  const window = {};
  vm.runInNewContext(fs.readFileSync("web/sequence.js", "utf8"), {
    window, document: { createElementNS: (_, tag) => new Element(tag) }
  });
  const container = new Element("div");
  const steps = participants.filter(p=>p.id !== "seed").map(p=>({id:`call-${p.id}`,kind:"call",label:"sample",target:p.id,path:"main.rs",range:{startLine:1,endLine:1}}));
  window.BaleygSequence.render(container, {seed: {id:"seed",name:"sample"},participants,steps}, () => {});
  function all(node) { return [node, ...node.children.flatMap(all)]; }
  return all(container);
}
test("external participant provenance and labels remain plain text, not resolution claims", () => {
  const nodes = render([
    {id:"seed",label:"sample",kind:"method"},
    {id:"json",label:"JSON",kind:"builtin",identification:"Unshadowed name, not runtime proof <script>"},
    {id:"client",label:"Client",kind:"import",identification:"Imported from package"},
    {id:"receiver",label:"client",kind:"receiver"},
    {id:"unknown",label:"Unknown",kind:"boundary"},
  ]);
  const texts = nodes.map(n => n.textContent);
  for (const label of ["Built-in name", "Imported binding", "Receiver hint", "Unknown target"]) assert.ok(nodes.some(n=>n.tag === "title" && n.textContent.includes(label)));
  assert.ok(nodes.some(n=>n.tag === "text" && n.textContent === "source hint"));
  assert.ok(nodes.some(n=>n.tag === "text" && n.textContent === "unknown"));
  assert.ok(texts.includes("JSON — Unshadowed name, not runtime proof <script>"));
  assert.equal(nodes.some(n => n.tag === "script"), false);
});
test("older participant DTOs still render without identification", () => {
  const nodes = render([{id:"seed",label:"sample",kind:"method"},{id:"other",label:"other",kind:"internal"}]);
  assert.ok(nodes.some(n => n.textContent === "Indexed target"));
});

test("call groups collapse, expand by keyboard, preserve source and scroll, and reset for new views", () => {
  const window = {};
  vm.runInNewContext(fs.readFileSync("web/sequence.js", "utf8"), {window, document:{createElementNS:(_,tag)=>new Element(tag)}});
  const container = new Element("div"); container.scrollTop=30; container.scrollLeft=12;
  const source=[]; const range={startLine:1,endLine:2};
  const leaf=id=>({id,kind:"call",label:id,path:"main.rs",range,target:"other",callId:id});
  const group={id:"chain",kind:"group",label:"Builder::new → open (3 calls)",path:"main.rs",range,children:[leaf("one"),leaf("two"),leaf("three")]};
  const view={seed:{id:"seed",name:"sample"},participants:[{id:"seed",label:"sample",kind:"method"},{id:"other",label:"other",kind:"boundary"}],steps:[group]};
  const all=n=>[n,...n.children.flatMap(all)];
  const nodes=()=>all(container);
  const count=()=>nodes().filter(n=>n.attrs["data-kind"]==="call").length;
  const control=()=>nodes().find(n=>n.attrs.class==="sequence-group-control");
  window.BaleygSequence.render(container,view,s=>source.push(s));
  assert.equal(count(),0); assert.equal(control().attrs["aria-expanded"],"false");
  nodes().find(n=>n.attrs.class==="sequence-source").listeners.click(); assert.equal(source[0],group);
  let prevented=false;
  control().listeners.keydown({key:"Enter",preventDefault(){prevented=true;}});
  assert.ok(prevented); assert.equal(count(),3); assert.equal(control().attrs["aria-expanded"],"true");
  assert.ok(control().focused); assert.equal(container.scrollTop,30); assert.equal(container.scrollLeft,12);
  nodes().filter(n=>n.attrs.class==="sequence-source")[1].listeners.click(); assert.equal(source[1].callId,"one");
  control().listeners.click(); assert.equal(count(),0);
  control().listeners.keydown({key:" ",preventDefault(){}}); assert.equal(count(),3);
  window.BaleygSequence.render(container,view,s=>source.push(s)); assert.equal(count(),0);
});


test("collapsed chain previews the first measured arrow, hides ghost lanes and never attributes all calls to its type", () => {
  const window = {};
  vm.runInNewContext(fs.readFileSync("web/sequence.js", "utf8"), {window, document:{createElementNS:(_,tag)=>new Element(tag)}});
  const container = new Element("div"), sources=[];
  const range={startLine:8,endLine:12};
  const call=(id,target)=>({id,kind:"call",label:id,path:"main.rs",range,callId:id,target,resolution:"unresolved",children:[],alternate:[]});
  const group={id:"chain",kind:"group",label:"Builder chain",path:"main.rs",range,children:[call("Builder::new","type"),call("open","chain-result"),call("finish","chain-result")]};
  const view={seed:{id:"seed",name:"sample"},participants:[
    {id:"seed",label:"sample",kind:"method"},
    {id:"ghost",label:"Unused old hint",kind:"boundary"},
    {id:"chain-result",label:"Chain results",kind:"unresolvedReceiver"},
    {id:"type",label:"Builder",kind:"externalCandidate"}
  ],steps:[group]};
  const before=JSON.stringify(view), all=n=>[n,...n.children.flatMap(all)], nodes=()=>all(container);
  window.BaleygSequence.render(container,view,s=>sources.push(s));
  assert.equal(nodes().filter(n=>n.attrs["data-kind"]==="call-preview").length,1);
  assert.equal(nodes().find(n=>n.attrs["data-kind"]==="call-preview").attrs["data-entry-call-id"],"Builder::new");
  assert.ok(nodes().some(n=>n.textContent.includes("+2 calls collapsed")));
  assert.ok(nodes().some(n=>n.textContent.includes("Other calls are collapsed; their receivers and return types may differ")));
  assert.ok(!nodes().some(n=>n.textContent==="Chain results" || n.textContent==="Unused old hint"));
  assert.equal(nodes().filter(n=>n.tag==="line" && n.attrs.class==="sequence-arrow").length,1);
  assert.equal(nodes().find(n=>n.tag==="line" && n.attrs.class==="sequence-arrow").attrs.x2,"340");
  nodes().find(n=>n.attrs.class==="sequence-source").listeners.click();assert.equal(sources[0],group);
  nodes().find(n=>n.attrs.class==="sequence-group-control").listeners.click();
  assert.equal(nodes().filter(n=>n.attrs["data-kind"]==="call-preview").length,0);
  assert.equal(nodes().filter(n=>n.attrs["data-kind"]==="call").length,3);
  assert.ok(nodes().some(n=>n.textContent==="Chain results"));
  assert.ok(nodes().some(n=>n.textContent==="Receiver · type unresolved"));
  assert.ok(!nodes().some(n=>n.textContent==="Unused old hint"));
  assert.equal(JSON.stringify(view),before);
});

test("groups without a flat, visible, targeted entry call do not fabricate arrows", () => {
  const window = {};
  vm.runInNewContext(fs.readFileSync("web/sequence.js", "utf8"), {window, document:{createElementNS:(_,tag)=>new Element(tag)}});
  const range={startLine:1,endLine:3};const leaf={id:"c",kind:"call",label:"call",path:"a.rs",range,target:"type"};
  const cases=[
    [{...leaf,hidden:true}],
    [{...leaf,target:"absent"}],
    [{...leaf,kind:"note"},leaf],
    [{...leaf,children:[leaf]}],
  ];
  const all=n=>[n,...n.children.flatMap(all)];
  for(const children of cases){
    const container=new Element("div");
    window.BaleygSequence.render(container,{seed:{id:"s",name:"s"},participants:[{id:"s",label:"s",kind:"method"},{id:"type",label:"Type",kind:"externalCandidate"}],steps:[{id:"g",kind:"group",label:"group",path:"a.rs",range,children}]},()=>{});
    assert.equal(all(container).filter(n=>n.attrs["data-kind"]==="call-preview").length,0);
    assert.equal(all(container).filter(n=>n.attrs.class==="sequence-arrow").length,0);
  }
});


function diagram(view, options = {}, expanded = new Set()) {
  const window = {}, selected = [], sources = [];
  vm.runInNewContext(fs.readFileSync("web/sequence.js", "utf8"), {window, document:{createElementNS:(_, tag)=>new Element(tag)}});
  const container = new Element("div"); container.scrollTop = 71; container.scrollLeft = 19;
  const all = n => [n, ...n.children.flatMap(all)];
  const render = (nextOptions = options, nextView = view) => window.BaleygSequence.render(container, nextView, step=>sources.push(step), expanded, nextOptions);
  render();
  return {container, sources, selected, render, nodes:()=>all(container),
    row:id=>all(container).find(n=>n.attrs["data-source-step-id"] === id),
    toggle:()=>all(container).find(n=>n.attrs.class === "sequence-group-control")};
}
const range = {startLine:3, startColumn:2, endLine:7, endColumn:9};
const call = (id, target = "type", resolution = "unresolved") => ({id, kind:"call", label:id, path:"main.rs", range, callId:id, target, resolution, children:[], alternate:[]});
const viewWith = steps => ({seed:{id:"seed", name:"sample"}, participants:[
  {id:"seed", label:"sample", kind:"method"},
  {id:"type", label:"Builder", kind:"externalCandidate", identification:"Syntax candidate; not proven receiver identity"},
  {id:"internal", label:"helper", kind:"internal"},
  {id:"unknown", label:"Unknown", kind:"unresolvedReceiver"}
], steps});
const freeze = value => { if (value && typeof value === "object") { Object.freeze(value); Object.values(value).forEach(freeze); } return value; };
const visibleText = nodes => nodes.filter(n=>n.tag !== "title").map(n=>n.textContent).filter(Boolean);

test("selection uses original steps, not source fetches, and survives keyboard group toggles without mutating DTO/options", () => {
  const first = call("Builder::new"), second = call("open", "unknown");
  const group = {id:"chain", kind:"group", label:"Builder chain", path:"main.rs", range, children:[first, second]};
  const view = freeze(viewWith([group])), selected = [];
  const options = Object.freeze({onSelect:step=>selected.push(step), showDetails:false});
  const before = JSON.stringify(view), d = diagram(view, options);
  d.row("chain").listeners.click();
  assert.equal(selected[0], group); assert.equal(d.sources.length, 0);
  assert.equal(d.row("chain").attrs["aria-pressed"], "true");
  assert.match(d.row("chain").attrs["aria-label"], /Inspect: group: Builder chain/);
  assert.match(d.row("chain").attrs["aria-label"], /main.rs:3:2–7:9/);
  for (const key of ["Enter", " "]) {
    let prevented = false;
    d.toggle().listeners.keydown({key, preventDefault(){prevented=true;}});
    assert.ok(prevented); assert.ok(d.toggle().focused);
    assert.equal(d.container.scrollTop, 71); assert.equal(d.container.scrollLeft, 19);
    assert.equal(d.row("chain").attrs["aria-pressed"], "true");
  }
  d.toggle().listeners.click();
  for (const key of ["Enter", " "]) {
    let prevented = false;
    d.row(first.id).listeners.keydown({key, preventDefault(){prevented=true;}});
    assert.ok(prevented); assert.equal(selected.at(-1), first);
  }
  assert.equal(d.row("chain").attrs["aria-pressed"], "false");
  assert.equal(d.row(first.id).attrs["aria-pressed"], "true");
  d.toggle().listeners.click(); d.toggle().listeners.click();
  assert.equal(d.row(first.id).attrs["aria-pressed"], "true");
  d.row(second.id).listeners.keydown({key:"Escape", preventDefault(){assert.fail("unrelated key prevented");}});
  assert.equal(selected.at(-1), first);
  assert.equal(d.sources.length, 0); assert.equal(JSON.stringify(view), before);
  d.render(); assert.equal(d.row(first.id).attrs["aria-pressed"], "false");
});

test("legacy source callback still works with keyboard and omitted fifth argument", () => {
  const step = call("open"), d = diagram(viewWith([step]));
  for (const key of ["Enter", " "]) d.row(step.id).listeners.keydown({key, preventDefault(){}});
  assert.deepEqual(d.sources, [step, step]);
  assert.match(d.row(step.id).attrs["aria-label"], /^Read source:/);
});

test("compact control summaries preserve every guard, alternate and exit; Show all restores full original labels", () => {
  const label = "only if the preceding path continues normally; this guard has a distinct original source range and a long predicate <unsafe>";
  const exit = {id:"exit", kind:"return", label:"?: early return on residual; conversion not expanded", path:"main.rs", range};
  const alternate = {id:"alternate", kind:"throw", label:"throw different_error", path:"main.rs", range};
  const inner = {id:"second-guard", kind:"branch", label, path:"main.rs", range:{...range,startLine:5}, children:[call("checked"), exit], alternate:[alternate]};
  const guard = {id:"first-guard", kind:"branch", label, path:"main.rs", range, children:[inner], alternate:[]};
  const selected = [], view = freeze(viewWith([guard])), before = JSON.stringify(view);
  const d = diagram(view, {onSelect:step=>selected.push(step)});
  const ids = () => d.nodes().filter(n=>n.attrs["data-step-id"]).map(n=>n.attrs["data-step-id"]);
  assert.deepEqual(ids(), ["first-guard", "second-guard", "checked", "exit", "alternate"]);
  assert.ok(visibleText(d.nodes()).includes("Compact labels · select for details"));
  const compactGuards = d.nodes().filter(n=>n.attrs.class === "sequence-control-label").flatMap(n=>n.children.filter(c=>c.tag === "tspan").map(c=>c.textContent));
  assert.equal(compactGuards.length, 2);
  assert.ok(compactGuards.every(t=>t.startsWith("[") && t.endsWith("…]") && !t.includes("branch ·") && !t.includes("summary")));
  assert.ok(visibleText(d.nodes()).includes("else / alternate"));
  assert.ok(d.nodes().filter(n=>n.tag === "title" && n.textContent.includes(label)).length >= 2);
  d.row(inner.id).listeners.click(); assert.equal(selected[0], inner); assert.equal(selected[0].range.startLine, 5);
  const compactHeight = Number(d.nodes().find(n=>n.tag === "svg").attrs.height);
  d.render({showDetails:true, onSelect:step=>selected.push(step)});
  assert.deepEqual(ids(), ["first-guard", "second-guard", "checked", "exit", "alternate"]);
  assert.equal(visibleText(d.nodes()).filter(t=>t.includes("summary")).length, 0);
  for (const id of [guard.id, inner.id]) {
    const text = d.row(id).children.find(n=>n.attrs.class === "sequence-control-label");
    const restored = text.children.filter(n=>n.tag === "tspan").map(n=>n.textContent).join(" ");
    assert.equal(restored, `[${label}]`);
  }
  assert.ok(Number(d.nodes().find(n=>n.tag === "svg").attrs.height) > compactHeight);
  assert.equal(JSON.stringify(view), before);
  assert.equal(d.nodes().some(n=>n.tag === "unsafe"), false);
});

test("call provenance has distinct strokes and labels, without invented confidence, return arrows or activation bars", () => {
  const steps = [call("call · Builder::new"), call("helper", "internal", "internal"), call("unresolved", "unknown")];
  const d = diagram(viewWith(steps));
  const arrows = d.nodes().filter(n=>n.tag === "line" && n.attrs.class === "sequence-arrow");
  assert.deepEqual(arrows.map(n=>n.attrs["data-provenance"]), ["candidate", "resolved", "syntax"]);
  assert.deepEqual(arrows.map(n=>n.attrs["stroke-dasharray"]), ["7 3", "none", "2 4"]);
  const text = visibleText(d.nodes());
  for (const label of ["candidate", "indexed", "unresolved", "Builder::new"]) assert.ok(text.includes(label));
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-provenance").length, 0);
  assert.ok(!text.includes("call · Builder::new"));
  assert.ok(d.nodes().some(n=>n.tag === "title" && n.textContent.includes("call · Builder::new") && n.textContent.includes("Syntax candidate; not proven receiver identity")));
  assert.ok(!text.some(t=>/confidence|1\.0|runtime return/.test(t)));
  assert.equal(arrows.length, steps.length);
  assert.ok(!d.nodes().some(n=>/activation|return-arrow/.test(n.attrs.class || "")));
  d.render({showDetails:true});
  assert.deepEqual(d.nodes().filter(n=>n.attrs.class === "sequence-provenance").map(n=>n.textContent), ["Candidate", "Indexed target", "Syntax only"]);
  assert.ok(visibleText(d.nodes()).includes("call · Builder::new"));
});

test("long chain entry names keep the measured arrow, collapsed count, full tooltip and original group callback", () => {
  const first = call("Builder::" + "very_long_measured_name".repeat(8));
  const group = {id:"chain", kind:"group", label:"Original full group expression", path:"main.rs", range, children:[first, call("finish", "unknown")]};
  const selected = [], d = diagram(viewWith([group]), {onSelect:step=>selected.push(step)});
  const row = d.row("chain"), text = row.children.find(n=>n.tag === "text");
  const label = text.children.filter(n=>n.tag === "tspan").map(n=>n.textContent).join(" ");
  assert.match(label, /\+1 calls collapsed \(entry preview\)/);
  assert.ok(d.nodes().some(n=>n.tag === "title" && n.textContent.includes(first.label)));
  assert.equal(d.nodes().filter(n=>n.tag === "line" && n.attrs.class === "sequence-arrow").length, 1);
  row.listeners.click(); assert.equal(selected[0], group);
});

test("hidden-only lanes are omitted and visible lanes follow first measured appearance", () => {
  const hidden = {...call("hidden", "internal", "internal"), hidden:true};
  const d = diagram(viewWith([call("unknown-first", "unknown"), hidden, call("candidate-second")]));
  const names = d.nodes().filter(n=>n.attrs.class === "sequence-participant-label").map(n=>n.textContent);
  assert.deepEqual(names, ["sample", "Unknown", "Builder"]);
  assert.equal(d.nodes().filter(n=>n.attrs["data-step-id"] === "hidden").length, 0);
});


test("empty and fully hidden views keep only the selected method lane", () => {
  for (const steps of [[], [{...call("hidden"), hidden:true}]]) {
    const d = diagram(viewWith(steps));
    assert.deepEqual(d.nodes().filter(n=>n.attrs.class === "sequence-participant-label").map(n=>n.textContent), ["sample"]);
    assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-arrow").length, 0);
  }
});


test("known guard prose becomes concise display text without merging nodes or losing original evidence", () => {
  const continuation = "only if the preceding path continues normally";
  const residual = "?: early return on residual; conversion not expanded";
  const success = "?: continue only on success; otherwise early return";
  const exit = {id:"residual",kind:"return",label:residual,path:"option.rs",range};
  const check = {id:"check",kind:"branch",label:success,path:"option.rs",range,children:[],alternate:[exit]};
  const guard2 = {id:"guard2",kind:"branch",label:continuation,path:"option.rs",range:{...range,startLine:5},children:[check]};
  const guard1 = {id:"guard1",kind:"branch",label:continuation,path:"option.rs",range,children:[guard2]};
  const view = freeze(viewWith([guard1])), before = JSON.stringify(view), selected = [];
  const d = diagram(view, {onSelect:step=>selected.push(step)});
  const rowText = id => d.row(id).children.find(n=>n.tag === "text" && n.attrs.class !== "sequence-fragment-operator").children.filter(n=>n.tag === "tspan").map(n=>n.textContent).join(" ");
  assert.equal(rowText("guard1"), "[if prior path continues]"); assert.equal(rowText("guard2"), "[if prior path continues]");
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-guard-rail").length, 2);
  assert.equal(rowText("check"), "[success / early return]");
  assert.equal(rowText("residual"), "early return");
  assert.ok(!visibleText(d.nodes()).some(t=>/summary|branch ·|return ·|error/i.test(t)));
  for (const step of [guard1, guard2, check, exit]) {
    assert.ok(d.row(step.id).attrs["aria-label"].includes(step.label));
    assert.ok(d.row(step.id).children.some(n=>n.tag === "text" && n.children.some(n=>n.tag === "title" && n.textContent.includes(step.label))));
    const hit = d.row(step.id).children.find(n=>n.attrs.class === "sequence-row-hit");
    assert.ok(Number(hit.attrs.height) >= 24);
    d.row(step.id).listeners.click(); assert.equal(selected.at(-1), step);
  }
  d.render({showDetails:true});
  for (const step of [guard1, guard2, check, exit]) assert.equal(rowText(step.id), step.kind === "branch" ? `[${step.label}]` : `${step.kind} · ${step.label}`);
  assert.equal(JSON.stringify(view), before);
});


test("control frames have clipped operator tabs and bracketed original guards with accessible source selection", () => {
  const branch = {id:"conditional",kind:"branch",label:"total > 0 <unsafe>",path:"main.rs",range,children:[call("authorize")],alternate:[call("decline")]};
  const loop = {id:"iteration",kind:"loop",label:"for each line",path:"main.rs",range,children:[branch]};
  const attempt = {id:"attempt",kind:"try",label:"read storage",path:"main.rs",range,children:[call("read")],alternate:[]};
  const block = {id:"opaque-container",kind:"note",label:"explicit container",path:"main.rs",range,children:[call("contained")]};
  const view = freeze(viewWith([loop, attempt, block])), before = JSON.stringify(view), selected = [];
  const d = diagram(view, {onSelect:step=>selected.push(step)});
  assert.deepEqual(d.nodes().filter(n=>n.attrs.class === "sequence-fragment-operator").map(n=>n.textContent), ["loop","alt","try","block"]);
  for (const step of [loop,branch,attempt,block]) {
    const row = d.row(step.id), tab = row.children.find(n=>n.attrs.class === "sequence-fragment-tab");
    assert.equal(tab.tag, "path"); assert.match(tab.attrs.d, /^M [\d.]+ [\d.]+ H [\d.]+ V [\d.]+ L [\d.]+ [\d.]+ H [\d.]+ Z$/);
    assert.equal(row.attrs.role, "button"); assert.equal(row.attrs.tabindex, "0");
    assert.ok(row.attrs["aria-label"].includes(step.label));
    assert.ok(visibleText(d.nodes()).includes(`[${step.label}]`));
    for (const key of ["Enter", " "]) { let prevented=false;row.listeners.keydown({key,preventDefault(){prevented=true;}});assert.ok(prevented);assert.equal(selected.at(-1), step); }
    assert.equal(row.attrs["aria-pressed"], "true");
  }
  assert.deepEqual(d.nodes().filter(n=>n.attrs["data-step-id"]).map(n=>n.attrs["data-step-id"]), ["iteration","conditional","authorize","decline","attempt","read","opaque-container","contained"]);
  assert.equal(d.sources.length,0); assert.equal(JSON.stringify(view),before);
  assert.equal(d.nodes().some(n=>n.tag === "unsafe"),false);
});

test("long nested guards wrap beside tabs and adjacent control frames do not overlap", () => {
  let nested = call("deepest");
  for (let i=0;i<50;i++) nested={id:`nested-${i}`,kind:"loop",label:"condition with long source text ".repeat(10).trim(),path:"main.rs",range,children:[nested]};
  const tail={id:"last",kind:"branch",label:"next predicate",path:"main.rs",range,children:[]};
  const d=diagram(freeze(viewWith([nested,tail])),{showDetails:true});
  const topFrames = [nested.id,tail.id].map(id=>d.nodes().find(n=>n.attrs["data-step-id"]===id).children.find(n=>n.attrs.class==="sequence-fragment"));
  assert.ok(Number(topFrames[1].attrs.y)>Number(topFrames[0].attrs.y)+Number(topFrames[0].attrs.height));
  for(const row of d.nodes().filter(n=>n.attrs.class==="sequence-source" && n.children.some(c=>c.attrs.class==="sequence-fragment-tab"))) {
    const op=row.children.find(n=>n.attrs.class==="sequence-fragment-operator");
    const guard=row.children.find(n=>n.attrs.class==="sequence-control-label");
    assert.ok(Number(guard.attrs.x)>Number(op.attrs.x)+60);
    const spans=guard.children.filter(n=>n.tag==="tspan");
    assert.ok(spans.every(n=>Number(n.attrs.x)===Number(guard.attrs.x)));
    const canvasWidth = Number(d.nodes().find(n=>n.tag==="svg").attrs.width);
    assert.ok(spans.every(n=>Number(n.attrs.x)+n.textContent.length*7.8 < canvasWidth-18));
    assert.ok(spans.map(n=>n.textContent).join(" ").startsWith("["));
    assert.ok(spans.at(-1).textContent.endsWith("]"));
    const hit=row.children.find(n=>n.attrs.class==="sequence-row-hit");
    assert.ok(Number(hit.attrs.height)>=30+(spans.length-1)*16);
  }
  assert.ok(d.nodes().filter(n=>n.attrs.class==="sequence-control-label").some(n=>n.children.filter(c=>c.tag==="tspan").length>1));
});


test("loop tabs abbreviate parser prose without inventing an iteration predicate", () => {
  for(const [kind,caption] of [["for","for · possible iterations"],["while","while · possible iterations"],["loop","loop · termination unknown"]]) {
    const label=`${kind}_expression: possible iterations, not unrolled; exit/termination unknown`;
    const step={id:kind,kind:"loop",label,path:"main.rs",range,children:[call("work")]};
    const d=diagram(freeze(viewWith([step])));
    assert.ok(visibleText(d.nodes()).includes(`[${caption}]`));
    assert.ok(d.row(kind).attrs["aria-label"].includes(label));
    d.render({showDetails:true});
    const guard=d.row(kind).children.find(n=>n.attrs.class==="sequence-control-label");
    assert.equal(guard.children.filter(n=>n.tag==="tspan").map(n=>n.textContent).join(" "),`[${label}]`);
  }
});


const rowLabel = row => row.children.filter(n=>n.tag === "text" && n.attrs.class !== "sequence-fragment-operator")
  .flatMap(n=>n.children.filter(c=>c.tag === "tspan").map(c=>c.textContent)).join(" ");

test("workflow-shaped sequence keeps all measured calls, analysis rows and conditional scope with full-detail restoration", () => {
  const at = line => ({startLine:line,startColumn:4,endLine:line,endColumn:70});
  const event = (id, kind, label, line) => ({id,kind,label,path:"sample/workflow.py",range:at(line),children:[],alternate:[]});
  const measured = (id, target, line) => ({...event(id,"call",id,line),target,callId:`original-${id}`,resolution:"unresolved"});
  const create = measured("create_agent", "agent", 10);
  const write = event("write", "effect", "write: self.agent = create_agent(config={\"long_original_rhs\": value}) (implicit setters/unpacking unresolved)", 11);
  const definition = event("definition", "boundary", "Definition boundary: decorators/defaults/annotations and binding effects not expanded; callable body does not execute here.", 12);
  const stepCall = measured("Step", "step-type", 15), workflowCall = measured("Workflow", "workflow-type", 16);
  const protocol = event("lookup", "boundary", "Attribute lookup: descriptor/__getattribute__ effects and type unresolved.", 17);
  const exit = event("return", "return", "return Workflow(steps=[self.first_step, self.second_step], metadata={\"full_return_source\": original})", 18);
  const guard = {...event("guard", "branch", "only if the preceding path continues normally", 14),children:[stepCall,workflowCall,protocol,exit]};
  const after = event("following-sibling", "note", "separate source boundary", 20);
  const view = freeze({seed:{id:"seed",name:"build_workflow"},participants:[
    {id:"seed",label:"build_workflow",kind:"method"},
    {id:"agent",label:"create_agent",kind:"unresolvedCallee"},
    {id:"step-type",label:"Step",kind:"unresolvedCallee"},
    {id:"workflow-type",label:"Workflow",kind:"unresolvedCallee"}
  ],steps:[create,write,definition,guard,after]});
  const original = [create,write,definition,guard,stepCall,workflowCall,protocol,exit,after];
  const before = JSON.stringify(view), selected = [];
  const compactOptions = Object.freeze({showDetails:false,onSelect:step=>selected.push(step)});
  const d = diagram(view, compactOptions);
  const stepNodes = () => d.nodes().filter(n=>n.attrs["data-step-id"]);
  const group = id => stepNodes().find(n=>n.attrs["data-step-id"] === id);
  const checkOriginals = () => {
    assert.deepEqual(stepNodes().map(n=>n.attrs["data-step-id"]),original.map(s=>s.id));
    assert.deepEqual(d.nodes().filter(n=>n.attrs["data-kind"] === "call").map(n=>n.attrs["data-step-id"]),["create_agent","Step","Workflow"]);
    assert.equal(d.nodes().filter(n=>n.tag === "line" && n.attrs.class === "sequence-arrow").length,3);
    assert.deepEqual(group("guard").children.filter(n=>n.attrs["data-step-id"]).map(n=>n.attrs["data-step-id"]),["Step","Workflow","lookup","return"]);
    assert.equal(group("guard").children.includes(group(after.id)),false);
    for (const step of original) {
      assert.ok(d.row(step.id).attrs["aria-label"].includes(step.label));
      assert.ok(d.row(step.id).attrs["aria-label"].includes(`sample/workflow.py:${step.range.startLine}:4`));
      assert.ok(d.row(step.id).children.some(n=>n.tag === "text" && n.children.some(c=>c.tag === "title" && c.textContent.includes(step.label))));
      for (const key of ["Enter"," "]) {
        let prevented = false;
        d.row(step.id).listeners.keydown({key,preventDefault(){prevented=true;}});
        assert.ok(prevented); assert.equal(selected.at(-1),step); assert.equal(selected.at(-1).range,step.range);
      }
      d.row(step.id).listeners.click(); assert.equal(selected.at(-1),step);
      if (step.callId) assert.equal(selected.at(-1).callId,`original-${step.id}`);
    }
    assert.equal(d.sources.length,0); assert.equal(JSON.stringify(view),before);
  };
  checkOriginals();
  assert.equal(rowLabel(d.row(write.id)),"write · setters/unpacking unresolved");
  assert.equal(rowLabel(d.row(definition.id)),"definition · effects not expanded");
  assert.equal(rowLabel(d.row(protocol.id)),"attribute lookup · effects/type unresolved");
  assert.equal(rowLabel(d.row(exit.id)),"return");
  assert.equal(rowLabel(d.row(guard.id)),"[if prior path continues]");
  assert.ok(!visibleText(d.nodes()).join(" ").includes("long_original_rhs"));
  assert.ok(!visibleText(d.nodes()).join(" ").includes("full_return_source"));
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-note" || n.attrs.class === "sequence-fragment").length,0);
  assert.equal(d.nodes().filter(n=>n.attrs["data-presentation"] === "quiet-note").length,5);
  const rail = group(guard.id).children.find(n=>n.attrs.class === "sequence-guard-rail");
  assert.ok(rail); assert.match(rail.attrs.d,/^M \d+ \d+ H \d+ V \d+ H \d+$/);
  const railBottom = Number(rail.attrs.d.match(/ V (\d+)/)[1]);
  const returnTop = Number(d.row(exit.id).children.find(n=>n.attrs.class === "sequence-row-hit").attrs.y);
  const siblingTop = Number(d.row(after.id).children.find(n=>n.attrs.class === "sequence-row-hit").attrs.y);
  assert.ok(railBottom > returnTop + 24); assert.ok(railBottom < siblingTop);
  const compactHeight = Number(d.nodes().find(n=>n.tag === "svg").attrs.height);
  d.render(Object.freeze({...compactOptions,showDetails:true}));
  checkOriginals();
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-guard-rail").length,0);
  assert.ok(group(guard.id).children.some(n=>n.attrs.class === "sequence-fragment"));
  assert.equal(rowLabel(d.row(guard.id)),`[${guard.label}]`);
  for (const step of [write,definition,protocol,exit,after]) {
    assert.equal(rowLabel(d.row(step.id)),`${step.kind} · ${step.label}`);
    assert.ok(group(step.id).children.some(n=>n.attrs.class === "sequence-note"));
  }
  assert.ok(Number(d.nodes().find(n=>n.tag === "svg").attrs.height) > compactHeight);
  d.render(compactOptions); checkOriginals();
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-guard-rail").length,1);
  assert.equal(rowLabel(d.row(exit.id)),"return");
});

test("only the exact generated continuation guard without alternatives uses a conditional rail", () => {
  const label = "only if the preceding path continues normally";
  const variants = [
    {id:"exact",kind:"branch",label,children:[call("a")]},
    {id:"with-alternate",kind:"branch",label,children:[call("b")],alternate:[call("c")]},
    {id:"different",kind:"branch",label:`${label}; extra condition`,children:[call("d")]},
    {id:"space",kind:"branch",label:`${label} `,children:[call("e")]},
    {id:"loop",kind:"loop",label,children:[call("f")]},
    {id:"try",kind:"try",label,children:[call("g")]}
  ].map(s=>({...s,path:"main.rs",range}));
  const d=diagram(freeze(viewWith(variants)));
  assert.deepEqual(d.nodes().filter(n=>n.attrs["data-presentation"] === "continuation-guard").map(n=>n.attrs["data-step-id"]),["exact"]);
  assert.deepEqual(d.nodes().filter(n=>n.attrs.class === "sequence-fragment-operator").map(n=>n.textContent),["alt","alt","alt","loop","try"]);
  assert.ok(visibleText(d.nodes()).includes("else / alternate"));
  assert.equal(d.nodes().filter(n=>n.attrs["data-kind"] === "call").length,7);
  d.render({showDetails:true});
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-guard-rail").length,0);
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-fragment").length,6);
});

test("quiet protocol and unsupported boundaries remain individual evidence controls", () => {
  const labels = [
    ["Nested function/callback/class boundary: bodies are not executed here; class definition effects (base, computed keys, static initialization) are not expanded.","nested definition · effects not expanded"],
    ["Argument unpacking boundary: iteration/mapping protocol not expanded.","argument unpacking · protocol not expanded"],
    ["Formatted string boundary: interpolation and formatting protocol not expanded.","formatted string · protocol not expanded"],
    ["Unsupported custom behavior: evaluation remains unknown.","Unsupported custom behavior: evaluation remains unknown."]
  ];
  const steps=labels.map(([label],i)=>({id:`boundary-${i}`,kind:"boundary",label,path:"main.rs",range}));
  const d=diagram(freeze(viewWith(steps)));
  assert.deepEqual(steps.map(s=>rowLabel(d.row(s.id))),labels.map(([,compact])=>compact));
  assert.equal(d.nodes().filter(n=>n.attrs["data-presentation"] === "quiet-note").length,steps.length);
  for(const step of steps) { d.row(step.id).listeners.keydown({key:"Enter",preventDefault(){}}); assert.equal(d.sources.at(-1),step); }
  d.render({showDetails:true});
  assert.deepEqual(steps.map(s=>rowLabel(d.row(s.id))),labels.map(([label])=>`boundary · ${label}`));
});


test("proven name bindings and deferred definitions stay explicit without invented guards", () => {
  const binding = {id:"binding",kind:"effect",label:"bind name: agent",path:"factory.py",range};
  const definition = {id:"definition",kind:"definition",label:"define _executor · async body deferred",path:"factory.py",range};
  const steps = [call("make_agent"),binding,definition,call("Step"),call("Workflow"),{id:"return",kind:"return",label:"return Workflow(steps=[Step(executor=_executor)])",path:"factory.py",range}];
  const view = freeze(viewWith(steps)), before = JSON.stringify(view), selected = [];
  const d = diagram(view,{onSelect:step=>selected.push(step)});
  assert.equal(rowLabel(d.row(binding.id)),"bind agent");
  assert.equal(rowLabel(d.row(definition.id)),definition.label);
  assert.equal(d.nodes().filter(n=>n.attrs.class === "sequence-guard-rail" || n.attrs.class === "sequence-fragment").length,0);
  assert.equal(d.nodes().filter(n=>n.attrs["data-kind"] === "call").length,3);
  for(const step of [binding,definition]) {
    d.row(step.id).listeners.keydown({key:"Enter",preventDefault(){}});
    assert.equal(selected.at(-1),step);
    assert.ok(d.row(step.id).attrs["aria-label"].includes(step.label));
  }
  assert.equal(d.sources.length,0);
  d.render({showDetails:true,onSelect:step=>selected.push(step)});
  assert.equal(rowLabel(d.row(binding.id)),`effect · ${binding.label}`);
  assert.equal(rowLabel(d.row(definition.id)),`definition · ${definition.label}`);
  assert.equal(JSON.stringify(view),before);
});
