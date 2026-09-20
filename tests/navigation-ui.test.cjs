"use strict";
// Synthetic source and declarations only. Run: node --test tests/navigation-ui.test.cjs
const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const vm = require("node:vm");
const source = fs.readFileSync(path.join(__dirname, "../web/navigation.js"), "utf8");
const deferred = () => { let resolve, reject; const promise = new Promise((a,b) => {resolve=a;reject=b;}); return {promise,resolve,reject}; };
const symbol = (id="A.run", name="run") => ({id,name,path:"src/A.java",range:{startLine:4,endLine:7,startByte:10,endByte:60},qualifiedName:`sample.${id}`});
const target = (action="sequence", reason="declaration", id="A.run") => ({symbol:symbol(id),action,reason,matchKind:"syntaxCandidate"});
const response = (targets=[target()], extras={}) => ({revision:1,targets,warnings:[],truncated:false,requireIndex:false,...extras});
const descendants = node => [node,...node.children.flatMap(descendants)];
function harness({openSource = false} = {}) {
  let revision=1, session="one", request=async()=>response(), activeMenu=null;
  const calls=[],menus=[],selected=[],classes=[],openedSources=[];
  const document={activeElement:null,listeners:{},addEventListener(type,fn){(this.listeners[type] ||= []).push(fn);}};
  function element(tagName) {
    const node={tagName,children:[],parentNode:null,attrs:{},className:"",textContent:"",listeners:{},scrollTop:27,scrollLeft:0,
      setAttribute(k,v){this.attrs[k]=String(v);},getAttribute(k){return this.attrs[k]??null;},removeAttribute(k){delete this.attrs[k];},
      append(...nodes){for(const n of nodes){n.remove();n.parentNode=this;this.children.push(n);}},
      before(n){n.remove();n.parentNode=this.parentNode;this.parentNode.children.splice(this.parentNode.children.indexOf(this),0,n);},
      remove(){if(this.parentNode){this.parentNode.children=this.parentNode.children.filter(n=>n!==this);this.parentNode=null;}},
      contains(n){return descendants(this).includes(n);},
      get isConnected(){return document.body.contains(this);},
      matches(selector){return selector.startsWith(".") ? this.className.split(" ").includes(selector.slice(1)) : this.tagName===selector;},
      closest(selector){return this.matches(selector) ? this : this.parentNode?.closest(selector);},
      querySelectorAll(selector){return descendants(this).slice(1).filter(n=>n.matches(selector));},
      querySelector(selector){return this.querySelectorAll(selector)[0] || null;},
      addEventListener(type,fn){(this.listeners[type] ||= []).push(fn);},
      removeEventListener(type,fn){this.listeners[type]=(this.listeners[type]||[]).filter(f=>f!==fn);},
      getBoundingClientRect(){return {left:20,top:30,bottom:50,width:200,height:20};},
      scrollIntoView(options){this.scrolled=options;},focus(){document.activeElement=this;},
      set innerHTML(_){throw new Error("Unsafe HTML assignment");},
      fire(type,details={}) {
        const event={type,target:this,currentTarget:this,preventDefault(){this.prevented=true;},stopPropagation(){this.stopped=true;},...details};
        for(const fn of document.listeners[type]||[]) fn(event);
        const pending=[];
        for(let n=this;n&&!event.stopped;n=n.parentNode){event.currentTarget=n;for(const fn of n.listeners[type]||[]) pending.push(fn(event));}
        event.currentTarget=null; event.done=Promise.all(pending); return event;
      }
    };
    node.classList={add(name){node.className=[...new Set([...node.className.split(" ").filter(Boolean),name])].join(" ");},remove(name){node.className=node.className.split(" ").filter(n=>n!==name).join(" ");},contains(name){return node.className.split(" ").includes(name);}};
    return node;
  }
  document.body=element("body");document.createElement=element;
  document.querySelector=selector=>document.body.querySelector(selector);
  const window={listeners:{},addEventListener(type,fn){(this.listeners[type] ||= []).push(fn);}}; vm.runInNewContext(source,{window,document,console});
  const nav=window.BaleygNavigation;
  function showMenu(event,actions,{onClose}={}) {
    activeMenu?.close("replace");
    const menu=element("div");menu.className="classes-context-menu";document.body.append(menu);
    const record={event,actions,menu,close(reason="dismiss") {
      if(activeMenu!==record)return;
      activeMenu=null;menu.remove();onClose?.(reason);
    }};
    activeMenu=record;menus.push(record);
    return {close:()=>record.close()};
  }
  nav.init({request:(url,options)=>{calls.push({url,options});return request(url,options);},currentRevision:()=>revision,currentSession:()=>session,
    selectMethod:s=>selected.push(s),openClass:s=>classes.push(s),showMenu,
    ...(openSource ? {openSource:(symbol,revision)=>openedSources.push({symbol,revision})} : {})});
  const anchor=element("button");document.body.append(anchor);anchor.focus();
  function event(details={}){return {type:"contextmenu",target:anchor,currentTarget:anchor,clientX:120,clientY:80,preventDefault(){this.prevented=true;},stopPropagation(){this.stopped=true;},...details};}
  function pane(numbers=[1,2,3],options={}) {
    const pre=element("pre");pre.setAttribute("tabindex","0");pre.setAttribute("aria-label","Original source");document.body.append(pre);
    const rows=numbers.map(n=>{const row=element("span");row.className="source-line";const num=element("span");num.className="line-number";num.textContent=String(n);const code=element("span");code.textContent="<script>λ café</script>";row.append(num,code);pre.append(row);return row;});
    if(rows[1]) rows[1].classList.add("highlight");
    const binding=nav.attachSource(pre,{path:"src/A.java",revision:1,startLine:numbers[0],...options});
    const toolbar=pre.parentNode.children[pre.parentNode.children.indexOf(pre)-1];
    return {pre,rows,binding,toolbar,button:toolbar.children[0],status:toolbar.children[1]};
  }
  return {nav,document,window,element,anchor,event,pane,calls,menus,selected,classes,openedSources,showMenu,
    set request(fn){request=fn;},set revision(n){revision=n;},set session(s){session=s;},get latest(){return menus.at(-1);}};
}
const selector={path:"src/A.java",line:1};

test("lookup captures anchor synchronously, fetches only navigation and exposes labelled choices",async()=>{
  const h=harness(),gate=deferred();h.request=()=>gate.promise;
  const event=h.event();const pending=h.nav.open(event,selector);event.currentTarget=null;
  assert.equal(event.prevented,true);assert.equal(event.stopped,true);
  assert.match(h.latest.actions[0].label,/Finding cached/);assert.ok(h.latest.actions.some(a=>!a.disabled));
  gate.resolve(response([target(),target("class","type","A"),target("sequence","call","B.run")]));await pending;
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].url,"/api/navigation");assert.equal(h.calls[0].options.method,"POST");
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].options.body)),{expectedRevision:1,path:"src/A.java",line:1});
  assert.equal(h.latest.event.currentTarget,h.anchor);assert.equal(h.latest.event.clientX,120);assert.equal(h.latest.event.clientY,80);
  assert.match(h.latest.actions[1].label,/Sequence · sample.A.run · declaration · syntax candidate · src\/A.java:4/);
  assert.match(h.latest.actions[2].label,/Class.*type/);assert.match(h.latest.actions[0].label,/call/);
  assert.equal(h.selected.length,0);h.latest.actions[2].run();assert.equal(h.classes[0].id,"A");
});

test("only exact member selectors are submitted; invalid and mixed boundaries never fetch",async()=>{
  const h=harness();
  for(const bad of [null,{},[],{...selector,line:0},{...selector,line:-1},{...selector,line:1.5},{...selector,line:Number.MAX_SAFE_INTEGER+1},{...selector,path:""},{...selector,path:"x".repeat(4097)},{...selector,classId:"A"},{...selector,typeHint:"A"},{...selector,expectedRevision:2},
    {classId:"A",memberName:"run",startByte:-1,endByte:5},{classId:"A",memberName:"run",startByte:4,endByte:3},{classId:"A",memberName:"run",startByte:1.2,endByte:5},{classId:"A",memberName:"",startByte:0,endByte:5}]) await h.nav.open(h.event(),bad);
  assert.equal(h.calls.length,0);
  await h.nav.open(h.event(),{classId:"A",memberName:"méthode",startByte:0,endByte:16});
  assert.deepEqual(JSON.parse(JSON.stringify(h.calls[0].options.body)),{expectedRevision:1,classId:"A",memberName:"méthode",startByte:0,endByte:16});
});

test("no-result, warnings, partial and requireIndex states are honest bounded text",async()=>{
  const h=harness();h.request=async()=>response([],{requireIndex:true,truncated:true,warnings:["<img src=x onerror=evil()>".repeat(100),...Array(20).fill("warning")]});
  await h.nav.open(h.event(),selector);
  const labels=h.latest.actions.map(a=>a.label);
  assert.match(labels.join(" "),/No indexed target.*not guessed/);assert.match(labels.join(" "),/Index the workspace/);assert.match(labels.join(" "),/Partial/);
  assert.ok(labels.some(s=>s.startsWith("<img")));assert.ok(labels.every(s=>s.length<=700));assert.ok(labels.length<=12);assert.ok(h.latest.actions.some(a=>!a.disabled));
});

test("errors have explicit retry; malformed and stale responses never navigate",async()=>{
  const h=harness();h.request=async()=>{throw new Error("<script>offline</script>");};await h.nav.open(h.event(),selector);
  assert.match(h.latest.actions[0].label,/<script>offline<\/script>/);
  const retry=h.latest.actions.find(a=>a.label==="Try navigation again");h.request=async()=>response();await retry.run();assert.equal(h.calls.length,2);
  h.request=async()=>response(undefined,{revision:2});await h.nav.open(h.event(),selector);assert.match(h.latest.actions[0].label,/stale/);
  h.request=async()=>({revision:1});await h.nav.open(h.event(),selector);assert.match(h.latest.actions[0].label,/Invalid navigation response/);
  assert.equal(h.selected.length,0);
});

test("out-of-order response, reset, session, revision and scope changes suppress late results",async()=>{
  for(const invalidate of [h=>h.nav.reset(),h=>{h.session="two";},h=>{h.revision=2;},(_h,scope)=>{scope.current=false;}]) {
    const h=harness(),gate=deferred(),scope={current:true};h.request=()=>gate.promise;
    const pending=h.nav.open(h.event(),selector,{isCurrent:()=>scope.current});invalidate(h,scope);gate.resolve(response());await pending;assert.equal(h.menus.length,1);
  }
  const h=harness(),gate=deferred();h.request=()=>gate.promise;const old=h.nav.open(h.event(),selector);
  h.request=async()=>response([target("class","type","New")]);await h.nav.open(h.event(),selector);gate.resolve(response());await old;
  assert.match(h.latest.actions[0].label,/New/);
});

test("already-open candidate and retry callbacks recheck every scope boundary",async()=>{
  for(const invalidate of [h=>h.nav.reset(),h=>{h.session="two";},h=>{h.revision=2;},(_h,scope)=>{scope.current=false;},h=>h.nav.open(h.event(),selector)]) {
    const h=harness(),scope={current:true};await h.nav.open(h.event(),selector,{isCurrent:()=>scope.current});const action=h.latest.actions[0];
    await invalidate(h,scope);action.run();assert.equal(h.selected.length,0);
  }
  const h=harness();await h.nav.open(h.event(),selector);const action=h.latest.actions[0];action.run();action.run();assert.equal(h.selected.length,1);
  h.request=async()=>{throw new Error("offline");};await h.nav.open(h.event(),selector);const retry=h.latest.actions[1];h.nav.reset();await retry.run();assert.equal(h.calls.length,2);
});

test("Escape, Tab, outside pointerdown and Close cancel loading without resurfacing",async()=>{
  for(const dismiss of [h=>h.anchor.fire("keydown",{key:"Escape"}),h=>h.anchor.fire("keydown",{key:"Tab"}),h=>h.anchor.fire("pointerdown"),h=>h.latest.actions.at(-1).run()]) {
    const h=harness(),gate=deferred();h.request=()=>gate.promise;const pending=h.nav.open(h.event(),selector);dismiss(h);gate.resolve(response());await pending;assert.equal(h.menus.length,1);
  }
  const h=harness();await h.nav.open(h.event(),selector);const action=h.latest.actions[0];const menu=h.element("div");menu.className="classes-context-menu";h.document.body.append(menu);menu.fire("pointerdown");action.run();assert.equal(h.selected.length,1);
});

test("source attachment preserves rows, highlights, copy, selection and scroll without fetching",async()=>{
  const h=harness(),p=h.pane([10,11,12],{startLine:11});const original=p.pre.children.slice();
  assert.equal(h.calls.length,0);assert.equal(p.pre.scrollTop,27);assert.equal(p.rows[1].classList.contains("highlight"),true);
  assert.equal(p.button.textContent,"Navigate line 11");assert.ok(p.rows.every(row=>row.getAttribute("tabindex")===null));
  const click=p.rows[2].children[1].fire("click");assert.equal(click.prevented,undefined);assert.equal(h.calls.length,0);assert.equal(p.button.textContent,"Navigate line 12");
  for(const details of [{key:"c",ctrlKey:true},{key:"ArrowUp",shiftKey:true},{key:"PageDown"}]) assert.equal(p.pre.fire("keydown",details).prevented,undefined);
  assert.deepEqual(p.pre.children,original);assert.equal(p.rows[2].children[1].textContent,"<script>λ café</script>");
  await p.button.fire("click",{clientX:20,clientY:30,pointerType:"touch"}).done;
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].options.body.line,12);assert.equal(h.calls[0].options.body.path,"src/A.java");
});

test("keyboard moves active line at boundaries and both context shortcuts look up whole lines",async()=>{
  const h=harness(),p=h.pane([50,51,52]);
  p.pre.fire("keydown",{key:"ArrowUp"});assert.equal(p.button.textContent,"Navigate line 50");
  p.pre.fire("keydown",{key:"ArrowDown"});assert.equal(p.button.textContent,"Navigate line 51");assert.equal(p.rows[1].scrolled.block,"nearest");
  p.pre.fire("keydown",{key:"End"});p.pre.fire("keydown",{key:"ArrowDown"});assert.equal(p.button.textContent,"Navigate line 52");
  p.pre.fire("keydown",{key:"Home"});assert.equal(p.button.textContent,"Navigate line 50");assert.equal(h.calls.length,0);
  await p.pre.fire("keydown",{key:"F10",shiftKey:true}).done;assert.equal(h.calls[0].options.body.line,50);
  await p.pre.fire("keydown",{key:"ContextMenu"}).done;assert.equal(h.calls[1].options.body.line,50);
  assert.equal(p.pre.getAttribute("tabindex"),"0");assert.match(p.pre.getAttribute("aria-label"),/Active line 50/);
});

test("context menu uses clicked source row, never a guessed cursor word or gap",async()=>{
  const h=harness(),p=h.pane([1,2,3]);await p.rows[2].children[1].fire("contextmenu",{clientX:88,clientY:99}).done;
  assert.equal(h.calls[0].options.body.line,3);assert.equal(h.latest.event.currentTarget,p.pre);assert.equal(h.latest.event.clientY,99);
  await p.pre.fire("contextmenu").done;assert.equal(h.calls.length,1);
});

test("source selection, sourceSerial predicate, reset and disposal invalidate opened and pending menus",async()=>{
  for(const invalidate of [(h,p)=>p.rows[1].fire("click"),(h,p)=>p.binding.reset(),(h,p)=>p.binding.dispose(),(h,p,s)=>{s.current=false;},h=>h.nav.reset()]) {
    const h=harness(),scope={current:true},p=h.pane([1,2],{isCurrent:()=>scope.current});
    await p.button.fire("click").done;const action=h.latest.actions[0];invalidate(h,p,scope);action.run();assert.equal(h.selected.length,0);
  }
  const h=harness(),p=h.pane(),gate=deferred();h.request=()=>gate.promise;const event=p.button.fire("click");p.binding.dispose();gate.resolve(response());await event.done;assert.equal(h.menus.length,1);
  assert.equal(p.toolbar.parentNode,null);assert.equal(p.pre.getAttribute("aria-label"),"Original source");assert.equal(p.pre.getAttribute("aria-haspopup"),null);
  p.pre.fire("keydown",{key:"ContextMenu"});p.rows[0].fire("contextmenu");assert.equal(h.calls.length,1);
});

test("empty/invalid line numbers disable navigation; reattachment does not duplicate listeners",async()=>{
  const h=harness(),empty=h.pane([]);assert.equal(empty.button.disabled,true);await empty.button.fire("click").done;assert.equal(h.calls.length,0);
  const p=h.pane(["01","-1","1.5","x"]);assert.equal(p.button.disabled,true);await p.rows[0].fire("contextmenu").done;assert.equal(h.calls.length,0);
  const good=h.pane();const originalToolbar=good.toolbar;h.nav.attachSource(good.pre,{path:"src/B.java",revision:1,startLine:2});
  assert.equal(originalToolbar.parentNode,null);await good.pre.fire("keydown",{key:"ContextMenu"}).done;assert.equal(h.calls.length,1);assert.equal(h.calls[0].options.body.path,"src/B.java");assert.equal(h.calls[0].options.body.line,2);
});

test("target choices are bounded, safe text and retain exact measured symbol identity",async()=>{
  const h=harness(),danger=target();danger.symbol.name="<img src=x> λ";delete danger.symbol.qualifiedName;
  h.request=async()=>response([danger,...Array.from({length:100},(_,i)=>target("sequence","call",`B.${i}`))]);
  await h.nav.open(h.event(),selector);assert.match(h.latest.actions[63].label,/<img src=x> λ/);assert.equal(h.latest.actions.filter(a=>!a.disabled).length,65);assert.match(h.latest.actions.at(-2).label,/Partial/);
  h.latest.actions[63].run();assert.equal(h.selected[0],danger.symbol);
});


test("scroll dismissal cancels pending lookup but queued unchanged ancestor/menu scroll does not",async()=>{
  const h=harness(),gate=deferred();h.request=()=>gate.promise;const pending=h.nav.open(h.event(),selector);
  h.anchor.scrollTop++;h.anchor.fire("scroll");gate.resolve(response());await pending;assert.equal(h.menus.length,1);
  const keep=harness(),later=deferred();keep.request=()=>later.promise;const allowed=keep.nav.open(keep.event(),selector);
  keep.anchor.fire("scroll");const menu=keep.element("div");menu.className="classes-context-menu";keep.document.body.append(menu);menu.fire("scroll");
  later.resolve(response());await allowed;assert.equal(keep.menus.length,2);
});


test("overloads sharing name and source line retain distinct measured identity labels",async()=>{
  const h=harness(),first=target(),second=target();
  delete first.symbol.qualifiedName;delete second.symbol.qualifiedName;
  first.symbol.id="run#1";second.symbol.id="run#2";first.symbol.range.startByte=10;first.symbol.range.endByte=30;
  second.symbol.range.startByte=40;second.symbol.range.endByte=60;
  h.request=async()=>response([first,second]);await h.nav.open(h.event(),selector);
  assert.notEqual(h.latest.actions[0].label,h.latest.actions[1].label);
  assert.match(h.latest.actions[0].label,/bytes 10–30 · run#1/);assert.match(h.latest.actions[1].label,/bytes 40–60 · run#2/);
  h.latest.actions[1].run();assert.equal(h.selected[0],second.symbol);
});


test("detached or replaced loading menus cannot be resurfaced by success or errors",async()=>{
  for(const replacement of [false,true]) for(const fail of [false,true]) {
    const h=harness(),gate=deferred();h.request=()=>gate.promise;const pending=h.nav.open(h.event(),selector);
    const loading=h.latest.menu;loading.remove();h.anchor.focus();
    let other;if(replacement){other=h.element("div");other.className="classes-context-menu";other.textContent="Other class menu";h.document.body.append(other);}
    if(fail)gate.reject(new Error("late error"));else gate.resolve(response());await pending;
    assert.equal(h.menus.length,1);assert.equal(h.document.querySelector(".classes-context-menu"),other||null);
  }
  const h=harness();await h.nav.open(h.event(),selector);const action=h.latest.actions[0];
  h.latest.menu.remove();h.anchor.focus();action.run();assert.equal(h.selected.length,1,"shared close-before-invoke keeps valid actions working");
});


test("shared menu lifecycle cancels focus dismissal and external replacement, but not its own results or actions",async()=>{
  for(const replace of [false,true]) {
    const h=harness(),gate=deferred();h.request=()=>gate.promise;const pending=h.nav.open(h.event(),selector);
    if(replace)h.showMenu(h.event(),[{label:"Other class menu",run(){}}]);else h.latest.close("dismiss");
    gate.resolve(response());await pending;assert.equal(h.menus.length,replace?2:1);
    if(replace)assert.equal(h.latest.actions[0].label,"Other class menu");
  }
  const h=harness();await h.nav.open(h.event(),selector);assert.equal(h.menus.length,2);
  const action=h.latest.actions[0];h.latest.close("action");h.anchor.focus();action.run();assert.equal(h.selected.length,1);
  await h.nav.open(h.event(),selector);const dismissed=h.latest.actions[0];h.latest.close("dismiss");dismissed.run();assert.equal(h.selected.length,1);
});

test("reset and disposal close only the owned shared menu handle",async()=>{
  const h=harness();await h.nav.open(h.event(),selector);h.nav.reset();assert.equal(h.document.querySelector(".classes-context-menu"),null);
  const p=h.pane();await p.button.fire("click").done;h.showMenu(h.event(),[{label:"Other menu",run(){}}]);const other=h.latest.menu;
  p.binding.dispose();assert.equal(h.document.querySelector(".classes-context-menu"),other);
  h.nav.reset();assert.equal(h.document.querySelector(".classes-context-menu"),other);
});


test("measured columns distinguish same-line overloads without hiding exact symbols",async()=>{
  const h=harness(),first=target(),second=target();
  delete first.symbol.qualifiedName;delete second.symbol.qualifiedName;
  first.symbol.id="run#1";second.symbol.id="run#2";first.symbol.range.startColumn=2;second.symbol.range.startColumn=40;
  h.request=async()=>response([first,second]);await h.nav.open(h.event(),selector);
  assert.match(h.latest.actions[0].label,/src\/A.java:4:2$/);assert.match(h.latest.actions[1].label,/src\/A.java:4:40$/);
  h.latest.actions[0].run();assert.equal(h.selected[0],first.symbol);
});


test("source toolbar remains sticky with line-reveal clearance at desktop and mobile widths",()=>{
  const css=fs.readFileSync(path.join(__dirname,"../web/navigation.css"),"utf8");
  const toolbar=css.match(/\.source-navigation \{([^}]+)\}/)[1];
  assert.match(toolbar,/position: sticky/);assert.match(toolbar,/top: 0/);assert.match(toolbar,/left: 0/);assert.match(toolbar,/z-index: 1/);
  assert.match(css,/\.source-navigation \+ pre \.source-line \{ scroll-margin-top: 4rem;/);
  assert.match(css,/@media \(max-width: 600px\), \(pointer: coarse\) \{\s*\.source-navigation \+ pre \.source-line \{ scroll-margin-top: 7rem;/);
});


test("optional source callback adds explicit dual choices without auto reading or selecting",async()=>{
  const h=harness({openSource:true}),method=target("sequence","call");
  method.matchKind="sameClassCandidate";method.symbol.range.startColumn=9;
  h.revision=7;h.request=async()=>response([method,target("class","type","A")],{revision:7});
  await h.nav.open(h.event(),selector);
  assert.equal(h.calls.length,1);assert.equal(h.calls[0].url,"/api/navigation");
  assert.equal(h.calls[0].options.body.expectedRevision,7);
  assert.equal(h.openedSources.length,0);assert.equal(h.selected.length,0);assert.equal(h.classes.length,0);
  const [source,sequence,klass]=h.latest.actions;
  assert.match(source.label,/^Go to source · sample.A.run · call · same class candidate · src\/A.java:4:9$/);
  assert.match(sequence.label,/^Open sequence · sample.A.run · call · same class candidate · src\/A.java:4:9$/);
  assert.match(klass.label,/^Class ·/);
  h.latest.close("action");h.anchor.focus();source.run();
  assert.equal(h.openedSources.length,1);assert.equal(h.openedSources[0].symbol,method.symbol);
  assert.equal(h.openedSources[0].symbol.range,method.symbol.range);assert.equal(h.openedSources[0].revision,7);
  assert.deepEqual(h.openedSources[0].symbol.range,{startLine:4,endLine:7,startByte:10,endByte:60,startColumn:9});
  assert.equal(h.selected.length,0);assert.equal(h.calls.length,1);
  source.run();sequence.run();klass.run();assert.equal(h.openedSources.length,1);assert.equal(h.selected.length,0);assert.equal(h.classes.length,0);
  await h.nav.open(h.event(),selector);
  const [retainedSource,openSequence]=h.latest.actions;
  h.latest.close("action");openSequence.run();retainedSource.run();openSequence.run();
  assert.equal(h.selected.length,1);assert.equal(h.selected[0],method.symbol);assert.equal(h.openedSources.length,1);
});

test("both dual-choice callbacks recheck session, revision, serial, scope and menu lifecycle",async()=>{
  const invalidations=[h=>h.nav.reset(),h=>h.nav.dispose(),h=>h.nav.init({}),h=>{for(const listener of h.window.listeners.resize)listener();},h=>{h.session="two";},h=>{h.revision=2;},(_h,scope)=>{scope.current=false;},
    h=>h.nav.open(h.event(),selector),h=>h.anchor.fire("keydown",{key:"Escape"}),h=>h.anchor.fire("keydown",{key:"Tab"}),
    h=>h.anchor.fire("pointerdown"),h=>{h.anchor.scrollTop++;h.anchor.fire("scroll");},h=>h.latest.actions.at(-1).run(),
    h=>h.latest.close("dismiss"),h=>h.showMenu(h.event(),[{label:"Other menu",run(){}}])];
  for(const invalidate of invalidations) for(const index of [0,1]) {
    const h=harness({openSource:true}),scope={current:true};
    await h.nav.open(h.event(),selector,{isCurrent:()=>scope.current});const action=h.latest.actions[index];
    await invalidate(h,scope);action.run();
    assert.equal(h.openedSources.length,0);assert.equal(h.selected.length,0);
  }
});

test("dual choices obey source line, source scope, disposal and pending-response guards",async()=>{
  for(const invalidate of [(h,p)=>p.rows[1].fire("click"),(h,p)=>p.binding.reset(),(h,p)=>p.binding.dispose(),(_h,_p,s)=>{s.current=false;}]) {
    for(const pending of [false,true]) {
      const h=harness({openSource:true}),scope={current:true},p=h.pane([1,2],{isCurrent:()=>scope.current}),gate=deferred();
      if(pending)h.request=()=>gate.promise;
      const event=p.button.fire("click");
      if(!pending)await event.done;
      const actions=h.latest.actions.slice(0,2);invalidate(h,p,scope);
      if(pending){gate.resolve(response());await event.done;assert.equal(h.menus.length,1);}
      for(const action of actions)action.run();
      assert.equal(h.openedSources.length,0);assert.equal(h.selected.length,0);
    }
  }
});

test("dual target choices stay bounded at 128 target actions",async()=>{
  const h=harness({openSource:true});h.request=async()=>response(Array.from({length:100},(_,i)=>target("sequence","call",`B.${i}`)));
  await h.nav.open(h.event(),selector);
  assert.equal(h.latest.actions.filter(a=>a.label.startsWith("Go to source ·")).length,64);
  assert.equal(h.latest.actions.filter(a=>a.label.startsWith("Open sequence ·")).length,64);
  assert.equal(h.latest.actions.filter(a=>!a.disabled).length,129);assert.match(h.latest.actions.at(-2).label,/Partial/);
  assert.equal(h.openedSources.length,0);assert.equal(h.selected.length,0);
});

test("both overload choices keep measured columns and byte/identity fallback labels",async()=>{
  for(const columns of [false,true]) for(const actionIndex of [0,1]) {
    const h=harness({openSource:true}),first=target(),second=target();
    delete first.symbol.qualifiedName;delete second.symbol.qualifiedName;
    first.symbol.id="run#1";second.symbol.id="run#2";
    first.symbol.range.endByte=30;second.symbol.range.startByte=40;
    if(columns){first.symbol.range.startColumn=2;second.symbol.range.startColumn=40;}
    h.request=async()=>response([first,second]);await h.nav.open(h.event(),selector);
    const actions=h.latest.actions.slice(0,4);
    assert.equal(new Set(actions.map(a=>a.label)).size,4);
    for(const index of [0,1]) {
      assert.match(actions[index].label,columns ? /src\/A.java:4:2$/ : /bytes 10–30 · run#1$/);
      assert.match(actions[index+2].label,columns ? /src\/A.java:4:40$/ : /bytes 40–60 · run#2$/);
    }
    actions[actionIndex+2].run();
    if(actionIndex===0){assert.equal(h.openedSources[0].symbol,second.symbol);assert.equal(h.openedSources[0].revision,1);assert.equal(h.selected.length,0);}
    else {assert.equal(h.selected[0],second.symbol);assert.equal(h.openedSources.length,0);}
  }
});


test("call choices come first with stable backend order, original symbols and bounded targets",async()=>{
  const enclosingClass=target("class","type","Context"),enclosingMethod=target("sequence","declaration","Context.run");
  const lastNamedCall=target("sequence","call","Z.run"),firstNamedCall=target("sequence","call","A.run");
  const targets=[enclosingClass,enclosingMethod,lastNamedCall,target("class","type","Other"),firstNamedCall];
  const expected=[lastNamedCall,firstNamedCall,enclosingClass,enclosingMethod,targets[3]];
  for(const openSource of [false,true]) {
    const labels=expected.flatMap(t=>t.action==="sequence" && openSource ? ["Go to source","Open sequence"].map(prefix=>`${prefix} · ${t.symbol.qualifiedName} ·`) : [`${t.action==="class" ? "Class" : "Sequence"} · ${t.symbol.qualifiedName} ·`]);
    for(let chosen=0;chosen<labels.length;chosen++) {
      const h=harness({openSource});h.request=async()=>response(targets);await h.nav.open(h.event(),selector);
      assert.equal(h.latest.actions.length,labels.length+1);
      for(let i=0;i<labels.length;i++)assert.ok(h.latest.actions[i].label.startsWith(labels[i]));
      assert.equal(h.openedSources.length,0);assert.equal(h.selected.length,0);assert.equal(h.classes.length,0);
      h.latest.actions[chosen].run();
      const expectedSymbol=expected.find(t=>labels[chosen].includes(` · ${t.symbol.qualifiedName} ·`)).symbol;
      assert.equal(h.openedSources[0]?.symbol || h.selected[0] || h.classes[0],expectedSymbol);
    }
  }
  const h=harness({openSource:true});h.request=async()=>response([enclosingClass,...Array(63).fill(enclosingMethod),lastNamedCall]);
  await h.nav.open(h.event(),selector);
  assert.ok(h.latest.actions.every(a=>!a.label.includes("sample.Z.run")),"ordering does not expand the original 64-target bound");
  assert.equal(h.latest.actions.filter(a=>!a.disabled).length,128);
});
