import test from 'node:test';
import assert from 'node:assert/strict';
import {readFile,mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join,dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {loadFixture} from '../load.mjs';
import {createHash} from 'node:crypto';
import {checkMeasurement,compareEnvelope,checkEnvelopeOrder,measuredHeaderHash,measuredSiblingGroupHash} from '../record-check/measurement.mjs';
import {registerControls,runControl} from './mutations.mjs';

// This test-side canonical encoder and the expected rows do not call the checker or normalizer.
const canonical = value => {if(typeof value==='string')return JSON.stringify(value).replace(/\\[nrtbf]/g, match => '\\u00'+({n:'0a',r:'0d',t:'09',b:'08',f:'0c'})[match[1]]);if(value===null||typeof value!=='object')return JSON.stringify(value);if(Array.isArray(value))return '['+value.map(canonical).join(',')+']';return '{'+Object.keys(value).sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b))).map(k=>canonical(k)+':'+canonical(value[k])).join(',')+'}';};
const hash = (domain,value) => createHash('sha256').update(`baleyg.${domain}.v1\0`).update(canonical(value)).digest('hex');
const orderSyntax=(a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId))||Buffer.compare(Buffer.from(a.revisionId),Buffer.from(b.revisionId))||Buffer.compare(Buffer.from(canonical(a.document)),Buffer.from(canonical(b.document)));
const orderId=(a,b)=>Buffer.compare(Buffer.from(a.id),Buffer.from(b.id))||Buffer.compare(Buffer.from(canonical(a)),Buffer.from(canonical(b)));
const sha = value => createHash('sha256').update(value).digest('hex');
const syntax = value => `sid:v1:${hash('syntax',value).slice(0,32)}`;
const occurrence = value => `occ:v1:${hash('occurrence',value).slice(0,32)}`;
const span=(start,end,encoding='utf8')=>({start,end,encoding});
const source='function main() { target(); }\nfunction target() {}\n';
const doc={sourceSetId:'main',language:'javascript',path:'src/main.js'};
const header=name=>({kind:'function',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]});
const witness=(field,start,end,text)=>({field,witness:{range:span(start,end),text}});
const nativeDeclaration=(ref,name,start,end,nameStart)=>({ref,nativeId:null,document:doc,revisionId:'r1',parentRef:null,kind:'function',name,range:span(start,end),nameRange:span(nameStart,nameStart+name.length),header:header(name),signature:null,witnesses:[witness('name',nameStart,nameStart+name.length,name),witness('header.name',nameStart,nameStart+name.length,name)]});
function sample(){
 const declarations=[nativeDeclaration('main','main',0,29,9),nativeDeclaration('target','target',30,50,39)];
 const call={ref:'call',nativeId:null,document:doc,revisionId:'r1',ownerRef:'main',range:span(18,26),calleeRange:span(18,24),spelling:'target',regionRefs:['block'],witnesses:[witness('spelling',18,24,'target')]};
 const control={ref:'block',nativeId:null,document:doc,revisionId:'r1',ownerRef:'main',parentRef:null,kind:'block',range:span(16,29),arm:null,witnesses:[]};
 const reference={ref:'reference',nativeId:null,document:doc,revisionId:'r1',ownerRef:'main',range:span(18,24),spelling:'target',witnesses:[witness('spelling',18,24,'target')]};
 const native={formatVersion:1,producerId:'native',declarations,calls:[call],controls:[control],references:[reference]};
 const producer={id:'native',kind:'native',positionEncoding:'utf8'};
 const sourceBytes=Buffer.from(source),contentHash=sha(sourceBytes);
 const loaded={native,fixture:{producers:[producer]},sources:new Map([[JSON.stringify(['main','r1',doc.path]),sourceBytes]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key:doc,contentHash}]}]])};
 const identity=name=>syntax({sourceSet:'main',path:doc.path,language:'javascript',ancestors:[],declaration:{kind:'function',name,signature:null,ordinal:0}});
 const main=identity('main'),target=identity('target');
 const decl=(name,start,end,nameStart,id)=>({syntaxId:id,document:doc,revisionId:'r1',kind:'function',name,lookupKey:name,ancestors:[],key:{kind:'function',name,signature:null,ordinal:0},range:{start,end},nameRange:{start:nameStart,end:nameStart+name.length},header:header(name),provenanceId:`native:r1:${id}`});
 const declarationRows=[decl('main',0,29,9,main),decl('target',30,50,39,target)].sort(orderSyntax);
 const callId=occurrence({revisionId:'r1',ownerSyntaxId:main,kind:'call',ordinal:0}),controlId=occurrence({revisionId:'r1',ownerSyntaxId:main,kind:'control',ordinal:0});
 const callRow={id:callId,ownerSyntaxId:main,ordinal:0,document:doc,revisionId:'r1',range:{start:18,end:26},calleeRange:{start:18,end:24},spelling:'target',regionIds:[controlId],provenanceId:`native:r1:${callId}`};
 const controlRow={id:controlId,ownerSyntaxId:main,ordinal:0,document:doc,revisionId:'r1',kind:'block',range:{start:16,end:29},parentId:null,arm:null,provenanceId:`native:r1:${controlId}`};
 const records={declarations:declarationRows,calls:[callRow],controlRegions:[controlRow]};
 return {loaded,records,ids:{main,target,callId,controlId},contentHash};
}
test('independent measured source baseline, ownership, candidates, and native proof inventory',()=>{
 const s=sample(),result=checkMeasurement(s.loaded,s.records);
 assert.equal(result.identityByRef.get('main'),s.ids.main);
 assert.equal(result.identityByRef.get('reference'),occurrence({revisionId:'r1',ownerSyntaxId:s.ids.main,kind:'reference',ordinal:0}));
 assert.deepEqual(result.candidateRows.map(x=>x.anchor.kind).sort(),['callee','declarationName','declarationName','invocation','reference']);
 assert.equal(result.nativeProofRows.length,5);
 assert.deepEqual(result.groupsByDeclarationRef.get('main').memberRefs,['main']);
 assert.equal(result.nativeReferenceDescriptors[0].lookupKey,'target');
 assert.equal(result.recordByNativeRef.has('reference'),false);
});
async function admitted(){
 const expected=sample(),root=await mkdtemp(join(tmpdir(),'measurement-u2-'));
 const material={
  'src/main.js':source,
  'src/main.js.annotations.json':{formatVersion:1,document:doc,revisionId:'r1',scenarios:[],facts:[]},
  'captures/native.json':expected.loaded.native,
  'captures/native-producer.txt':'native measured producer',
  'captures/toolchain.txt':'toolchain',
  'captures/config.txt':'config',
  'captures/dependencies.txt':'dependencies',
  'expected/answers.json':{formatVersion:1,answers:[]},
  'expected/dispositions.json':{formatVersion:1,assertions:[],callableValueNegatives:[]},
  'expected/anchors.json':{formatVersion:1,cases:[]}
 };
 const producer={id:'native',version:'1',executableHash:sha(Buffer.from(material['captures/native-producer.txt'])),kind:'native',languages:['javascript'],positionEncoding:'utf8'};
 const captures=[['native','executable','captures/native-producer.txt'],['tool','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['deps','dependency','captures/dependencies.txt']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(Buffer.from(material[file]))}));
 material['fixture.json']={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers:[producer],revisions:[{id:'r1',sourceSetId:'main',documents:[{key:doc,revisionId:'r1',sourceFile:'src/main.js'}],toolchainHash:captures[1].hash,configHash:captures[2].hash,dependencyHash:captures[3].hash}],comparison:{sourceSetId:'main',revisionId:'r1',producers:[producer]},coverageIntents:[],nativeArtifact:'captures/native.json',semanticArtifacts:[],annotationFiles:['src/main.js.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 try{
  for(const [path,value] of Object.entries(material)){const target=join(root,path);await mkdir(dirname(target),{recursive:true});await writeFile(target,typeof value==='string'?value:JSON.stringify(value));}
  const loaded=await loadFixture(root);delete loaded.sourceManifestHash;
  return {loaded,records:expected.records};
 }finally{await rm(root,{recursive:true,force:true});}
}
test('admitted fixture/capture bytes independently match measured source projection',async()=>{
 const {loaded,records}=await admitted(),m=checkMeasurement(loaded,records);
 assert.equal(m.nativeProofRows.length,5);
 assert.equal(m.nativeProofRows.every(x=>x.freshness==='fresh'),true);
});
const controls=registerControls([
 {id:'MEASUREMENT.WITNESS.spelling',mutate:s=>{s.loaded.native.calls[0].witnesses=[];return s;},expectedAssertion:'MEASUREMENT.WITNESS',expectedCode:'invalidRecord',expectedField:'spelling'},
 {id:'MEASUREMENT.WITNESS.header',mutate:s=>{s.loaded.native.declarations[0].witnesses.pop();return s;},expectedAssertion:'MEASUREMENT.WITNESS',expectedCode:'invalidRecord',expectedField:'header.name'},
 {id:'MEASUREMENT.WITNESS.extra',mutate:s=>{s.loaded.native.calls[0].witnesses.push(witness('extra',18,24,'target'));return s;},expectedAssertion:'MEASUREMENT.WITNESS',expectedCode:'invalidRecord',expectedField:'field'},
 {id:'MEASUREMENT.ENCODING.witness',mutate:s=>{s.loaded.native.calls[0].witnesses[0].witness.range.encoding='utf16';return s;},expectedAssertion:'MEASUREMENT.ENCODING',expectedCode:'invalidRecord',expectedField:'witnesses.spelling'},
 {id:'MEASUREMENT.RANGE.outside',mutate:s=>{s.loaded.native.calls[0].range.end=10000;return s;},expectedAssertion:'MEASUREMENT.RANGE',expectedCode:'invalidRange',expectedField:'range'},
 {id:'MEASUREMENT.RANGE.reversed',mutate:s=>{s.loaded.native.calls[0].range.start=27;s.loaded.native.calls[0].range.end=18;return s;},expectedAssertion:'MEASUREMENT.RANGE',expectedCode:'invalidRange',expectedField:'range'},
 {id:'MEASUREMENT.OWNER.cross',mutate:s=>{s.loaded.native.calls[0].ownerRef='target';return s;},expectedAssertion:'MEASUREMENT.OWNER',expectedCode:'invalidRecord',expectedField:'range'},
 {id:'MEASUREMENT.REGION.missing',mutate:s=>{s.loaded.native.calls[0].regionRefs=['missing'];return s;},expectedAssertion:'MEASUREMENT.REGION',expectedCode:'invalidRecord',expectedField:'regionRefs'},
 {id:'RECORDS.MEMBERSHIP.call',mutate:s=>{s.records.calls=[];return s;},expectedAssertion:'RECORDS.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'calls'},
 {id:'RECORDS.MEMBERSHIP.id',mutate:s=>{s.records.calls[0].id='occ:v1:'+'0'.repeat(32);return s;},expectedAssertion:'RECORDS.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'calls'},
 {id:'ID.ORDINAL.call',mutate:s=>{s.loaded.native.calls.push({...structuredClone(s.loaded.native.calls[0]),ref:'another'});return s;},expectedAssertion:'ID.ORDINAL',expectedCode:'invalidRecord',expectedField:'range'}
].map(row=>({baseline:admitted,check:s=>checkMeasurement(s.loaded,s.records),...row})));
for(const row of controls)test(row.id,()=>runControl(row));
test('native diagnostic ID and nullable callee spelling never change measured identity',()=>{
 const s=sample(),original=checkMeasurement(s.loaded,s.records);s.loaded.native.calls[0].nativeId='new';
 assert.equal(checkMeasurement(s.loaded,s.records).identityByRef.get('call'),original.identityByRef.get('call'));
 s.loaded.native.calls[0].spelling=null;s.loaded.native.calls[0].witnesses=[];s.records.calls[0].spelling=null;
 assert.equal(checkMeasurement(s.loaded,s.records).recordByNativeRef.get('call').calleeRange.start,18);
 s.loaded.native.calls[0].calleeRange=null;s.loaded.native.calls[0].spelling='target';s.loaded.native.calls[0].witnesses=[witness('spelling',18,24,'target')];s.records.calls[0].spelling='target';s.records.calls[0].calleeRange=null;
 assert.equal(checkMeasurement(s.loaded,s.records).recordByNativeRef.get('call').calleeRange,null);
});
test('64 independent vector descriptor encodings, full domain hashes and literal anchors',async()=>{
 const {cases}=JSON.parse(await readFile(new URL('../../../docs/semantic-evidence/id-test-vectors/stable-ids.json',import.meta.url),'utf8'));
 assert.equal(cases.length,64);
 for(const c of cases){
  const d=c.descriptor,syntaxInput={sourceSet:d.sourceSet,path:d.path,language:d.language,ancestors:d.ancestors,declaration:d.declaration};
  const headerHashes=d.siblingHeaders.map(header=>hash('header',header));
  const inputs={'syntax':syntaxInput,'sibling-group':{headers:headerHashes}};
  headerHashes.forEach((_,i)=>{inputs[`header-${i}`]=d.siblingHeaders[i];});
  for(const entry of c.digests){const domain=entry.label.startsWith('header-')?'header':entry.label;
   assert.equal(Buffer.from(canonical(inputs[entry.label])).toString('hex'),entry.inputHex,c.caseId+' '+entry.label+' canonical input');
   assert.equal(Buffer.from(`baleyg.${domain}.v1\0`).toString('hex'),entry.domainHex,c.caseId+' domain');
   assert.equal(hash(domain,inputs[entry.label]),entry.sha256,c.caseId+' digest');
  }
  const id=syntax(syntaxInput),anchor=c.expected.anchor;
  assert.equal(id,c.expected.stableId,c.caseId+' id');assert.equal(anchor.syntaxId,id);
  assert.deepEqual(anchor.document,{sourceSetId:d.sourceSet,language:d.language,path:d.path});
  assert.equal(anchor.capturedRevisionId,d.revisionId);
  assert.equal(anchor.headerHash,hash('header',d.header));
  assert.equal(anchor.siblingGroupHash,hash('sibling-group',{headers:headerHashes}));
  assert.equal(anchor.siblingCount,headerHashes.length);
  assert.equal(anchor.identicalHeaderCount,headerHashes.filter(x=>x===anchor.headerHash).length);
  assert.equal(measuredHeaderHash(d.header),anchor.headerHash);
  assert.equal(measuredSiblingGroupHash(headerHashes),anchor.siblingGroupHash);
 }
});

function shifted(encoding){
 const s=sample(),prefix='// 😀\n',delta=Buffer.byteLength(prefix),text=prefix+source;
 const convert=n=>{const before=Buffer.from(text).subarray(0,n+delta).toString('utf8');return encoding==='utf16'?before.length:encoding==='unicodeScalar'?[...before].length:n+delta;};
 const shiftRange=r=>{if(!r)return;r.start=convert(r.start);r.end=convert(r.end);r.encoding=encoding;};
 for(const row of [...s.loaded.native.declarations,...s.loaded.native.calls,...s.loaded.native.controls,...s.loaded.native.references]){
  shiftRange(row.range);if(row.nameRange)shiftRange(row.nameRange);if(row.calleeRange)shiftRange(row.calleeRange);
  for(const item of row.witnesses)shiftRange(item.witness.range);
 }
 for(const row of [...s.records.declarations,...s.records.calls,...s.records.controlRegions]){
  for(const r of [row.range,row.nameRange,row.calleeRange])if(r){r.start+=delta;r.end+=delta;}
 }
 s.loaded.fixture.producers[0].positionEncoding=encoding;
 s.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),Buffer.from(text));
 s.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(Buffer.from(text));
 return s;
}
test('UTF-8, UTF-16 and scalar positions convert independently from actual source bytes',()=>{
 for(const encoding of ['utf8','utf16','unicodeScalar']){
  const s=shifted(encoding),m=checkMeasurement(s.loaded,s.records);
  assert.deepEqual(m.recordByNativeRef.get('call').range,{start:26,end:34});
  assert.equal(m.identityByRef.get('main'),s.ids.main);
 }
});
for(const [encoding,offset] of [['utf8',4],['utf16',4]])test(`${encoding} split-scalar position is invalidRange`,()=>{
 const s=shifted(encoding);s.loaded.native.declarations[0].range.start=offset;
 assert.throws(()=>checkMeasurement(s.loaded,s.records),e=>e.assertion==='MEASUREMENT.RANGE'&&e.code==='invalidRange'&&e.field==='range');
});
function lexical(language,spelling,expected){
 const ext={java:'java',javascript:'js',rust:'rs',python:'py'}[language];
 const body={java:`class Example { void go() { int result = ${spelling}; } }\n`,javascript:`function go() { const result = ${spelling}; }\n`,rust:`fn go() { let result = ${spelling}; }\n`,python:`def go():\n    result = ${spelling}\n`}[language];
 const key={sourceSetId:'main',language,path:`src/main.${ext}`};
 const start=Buffer.byteLength(body.slice(0,body.indexOf(spelling))),end=start+Buffer.byteLength(spelling);
 const nameStart=Buffer.byteLength(body.slice(0,body.indexOf('go'))),name='go',kind=language==='java'?'method':'function';
 const signature=language==='java'?{parameterTypes:[],typeParameterCount:0,variadic:false}:null;
 const declaration={ref:'go',nativeId:null,document:key,revisionId:'r1',parentRef:null,kind,name,range:span(0,Buffer.byteLength(body)),nameRange:span(nameStart,nameStart+2),header:{...header(name),kind},signature,witnesses:[witness('name',nameStart,nameStart+2,name),witness('header.name',nameStart,nameStart+2,name)]};
 const reference={ref:'lexical',nativeId:null,document:key,revisionId:'r1',ownerRef:'go',range:span(start,end),spelling,witnesses:[witness('spelling',start,end,spelling)]};
 const id=syntax({sourceSet:'main',path:key.path,language,ancestors:[],declaration:{kind,name,signature,ordinal:0}});
 const row={syntaxId:id,document:key,revisionId:'r1',kind,name,lookupKey:name,ancestors:[],key:{kind,name,signature,ordinal:0},range:{start:0,end:Buffer.byteLength(body)},nameRange:{start:nameStart,end:nameStart+2},header:declaration.header,provenanceId:`native:r1:${id}`};
 const data=Buffer.from(body),loaded={native:{formatVersion:1,producerId:'native',declarations:[declaration],calls:[],controls:[],references:[reference]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',key.path]),data]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key,contentHash:sha(data)}]}]])};
 return {loaded,records:{declarations:[row],calls:[],controlRegions:[]},expected};
}
for(const [language,spelling,expected] of [
 ['java','\\u0074arget','target'],['java','target','target'],['javascript','\\u{74}arget','target'],
 ['rust','r#café','café'],['rust','café','café'],['rust','café','café'],
 ['python','ｆｏｏ','foo'],['python','foo','foo']
])test(`measured ${language} lexical spelling ${spelling}`,()=>{
 const {loaded,records}=lexical(language,spelling,expected),m=checkMeasurement(loaded,records);
 assert.equal(m.nativeReferenceDescriptors[0].spelling,spelling);
 assert.equal(m.nativeReferenceDescriptors[0].lookupKey,expected);
});
test('Java method projection, forbidden ordinary function, and non-Java signature exclusion',()=>{
 const s=lexical('java','go','go');assert.equal(checkMeasurement(s.loaded,s.records).recordByNativeRef.get('go').key.signature.typeParameterCount,0);
 const forbidden=structuredClone(s);forbidden.loaded.native.declarations[0].kind='function';forbidden.loaded.native.declarations[0].header.kind='function';forbidden.loaded.native.declarations[0].signature=null;
 assert.throws(()=>checkMeasurement(forbidden.loaded,forbidden.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.code==='invalidRecord'&&e.field==='kind');
 s.loaded.native.declarations[0].signature.typeParameterCount=1;
 assert.throws(()=>checkMeasurement(s.loaded,s.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='signature');
 const js=lexical('javascript','go','go');js.loaded.native.declarations[0].signature={parameterTypes:[],typeParameterCount:0,variadic:false};
 assert.throws(()=>checkMeasurement(js.loaded,js.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='signature');
});
test('every non-null header leaf needs an exact independent witness',()=>{
 const s=sample(),row=s.loaded.native.declarations[0],out=s.records.declarations.find(x=>x.name==='main');
 row.header={...row.header,modifiers:['main'],typeParameters:['main'],parameters:[{name:'main',type:'main',variadic:false}],resultType:'main',bases:['main']};
 out.header=structuredClone(row.header);
 const fields=['header.modifiers[0]','header.typeParameters[0]','header.parameters[0].name','header.parameters[0].type','header.resultType','header.bases[0]'];
 for(const field of fields)row.witnesses.push(witness(field,9,13,'main'));
 checkMeasurement(s.loaded,s.records);
 for(const field of fields){const bad=structuredClone(s);bad.loaded.native.declarations[0].witnesses=bad.loaded.native.declarations[0].witnesses.filter(x=>x.field!==field);
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field===field,field);
  const changed=structuredClone(s);changed.loaded.native.declarations[0].witnesses.find(x=>x.field===field).witness.text='other';
  assert.throws(()=>checkMeasurement(changed.loaded,changed.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field===field,field+' text');
 }
});
function nested(){
 const text='function outer() { function inner() { go(); } go(); }\n';
 const key={sourceSetId:'main',language:'javascript',path:'src/main.js'},data=Buffer.from(text);
 const outer={ref:'outer',nativeId:null,document:key,revisionId:'r1',parentRef:null,kind:'function',name:'outer',range:span(0,54),nameRange:span(9,14),header:header('outer'),signature:null,witnesses:[witness('name',9,14,'outer'),witness('header.name',9,14,'outer')]};
 const innerStart=text.indexOf('function inner'),innerEnd=text.indexOf('}',innerStart)+1,inner={...structuredClone(outer),ref:'inner',parentRef:'outer',name:'inner',range:span(innerStart,innerEnd),nameRange:span(innerStart+9,innerStart+14),header:header('inner'),witnesses:[witness('name',innerStart+9,innerStart+14,'inner'),witness('header.name',innerStart+9,innerStart+14,'inner')]};
 const callStart=text.indexOf('go();'),call={ref:'nested-call',nativeId:null,document:key,revisionId:'r1',ownerRef:'inner',range:span(callStart,callStart+4),calleeRange:span(callStart,callStart+2),spelling:'go',regionRefs:[],witnesses:[witness('spelling',callStart,callStart+2,'go')]};
 const outerKey={kind:'function',name:'outer',signature:null,ordinal:0},innerKey={kind:'function',name:'inner',signature:null,ordinal:0};
 const outerId=syntax({sourceSet:'main',path:key.path,language:key.language,ancestors:[],declaration:outerKey});
 const innerId=syntax({sourceSet:'main',path:key.path,language:key.language,ancestors:[outerKey],declaration:innerKey});
 const record=(row,id,ancestors)=>({syntaxId:id,document:key,revisionId:'r1',kind:row.kind,name:row.name,lookupKey:row.name,ancestors,key:{kind:row.kind,name:row.name,signature:null,ordinal:0},range:{start:row.range.start,end:row.range.end},nameRange:{start:row.nameRange.start,end:row.nameRange.end},header:row.header,provenanceId:`native:r1:${id}`});
 const cid=occurrence({revisionId:'r1',ownerSyntaxId:innerId,kind:'call',ordinal:0});
 const loaded={native:{formatVersion:1,producerId:'native',declarations:[outer,inner],calls:[call],controls:[],references:[]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',key.path]),data]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key,contentHash:sha(data)}]}]])};
 const records={declarations:[record(outer,outerId,[]),record(inner,innerId,[outerKey])].sort(orderSyntax),calls:[{id:cid,ownerSyntaxId:innerId,ordinal:0,document:key,revisionId:'r1',range:{start:callStart,end:callStart+4},calleeRange:{start:callStart,end:callStart+2},spelling:'go',regionIds:[],provenanceId:`native:r1:${cid}`}],controlRegions:[]};
 return {loaded,records,innerId,outerId};
}
test('nested source owners and ancestor descriptors, not IDs',()=>{
 const s=nested(),m=checkMeasurement(s.loaded,s.records);
 assert.deepEqual(m.recordByNativeRef.get('inner').ancestors,[{kind:'function',name:'outer',signature:null,ordinal:0}]);
 assert.equal(m.recordByNativeRef.get('nested-call').ownerSyntaxId,s.innerId);
 const skipped=structuredClone(s);skipped.loaded.native.declarations[1].parentRef=null;
 assert.throws(()=>checkMeasurement(skipped.loaded,skipped.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.field==='parentRef');
 const owner=structuredClone(s);owner.loaded.native.calls[0].ownerRef='outer';
 assert.throws(()=>checkMeasurement(owner.loaded,owner.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.field==='ownerRef');
 const cycle=structuredClone(s);cycle.loaded.native.declarations[0].parentRef='inner';
 assert.throws(()=>checkMeasurement(cycle.loaded,cycle.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.field==='parentRef');
});

test('control containment, parent order and region membership are source-derived',()=>{
 const s=sample(),control=s.loaded.native.controls[0];
 const inner={...structuredClone(control),ref:'inner',parentRef:'block',kind:'if',range:span(18,27)};
 s.loaded.native.controls.push(inner);s.loaded.native.calls[0].regionRefs=['block','inner'];
 const innerId=occurrence({revisionId:'r1',ownerSyntaxId:s.ids.main,kind:'control',ordinal:1});
 s.records.controlRegions.push({id:innerId,ownerSyntaxId:s.ids.main,ordinal:1,document:doc,revisionId:'r1',kind:'if',range:{start:18,end:27},parentId:s.ids.controlId,arm:null,provenanceId:`native:r1:${innerId}`});
 s.records.controlRegions.sort(orderId);s.records.calls[0].regionIds.push(innerId);
 checkMeasurement(s.loaded,s.records);
 for(const [change,field,assertion] of [
  [v=>{v.loaded.native.calls[0].regionRefs=['inner','block'];},'regionRefs','MEASUREMENT.REGION'],
  [v=>{v.loaded.native.calls[0].regionRefs=['block','block'];},'regionRefs','MEASUREMENT.REGION'],
  [v=>{v.loaded.native.calls[0].regionRefs=['block'];},'regionRefs','MEASUREMENT.REGION'],
  [v=>{v.loaded.native.controls[1].parentRef=null;},'parentRef','MEASUREMENT.CONTROL']
 ]){const bad=structuredClone(s);change(bad);assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion===assertion&&e.field===field);}
});
test('declared sibling ordinal comes from numeric source order, never adapter order',()=>{
 const text='function go() {}\nfunction go() {}\n',data=Buffer.from(text),key=doc,starts=[0,17];
 const rows=starts.map((start,i)=>nativeDeclaration(`sibling-${i}`,'go',start,start+16,start+9));
 const expected=starts.map((start,i)=>{const identity=syntax({sourceSet:'main',path:key.path,language:key.language,ancestors:[],declaration:{kind:'function',name:'go',signature:null,ordinal:i}});return {syntaxId:identity,document:key,revisionId:'r1',kind:'function',name:'go',lookupKey:'go',ancestors:[],key:{kind:'function',name:'go',signature:null,ordinal:i},range:{start,end:start+16},nameRange:{start:start+9,end:start+11},header:header('go'),provenanceId:`native:r1:${identity}`};}).sort(orderSyntax);
 const loaded={native:{formatVersion:1,producerId:'native',declarations:rows,calls:[],controls:[],references:[]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',key.path]),data]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key,contentHash:sha(data)}]}]])};
 const records={declarations:expected,calls:[],controlRegions:[]};
 const m=checkMeasurement(loaded,records);assert.deepEqual(m.groupsByDeclarationRef.get('sibling-0').memberRefs,['sibling-0','sibling-1']);
 loaded.native.declarations.reverse();assert.deepEqual(checkMeasurement(loaded,records).identityByRef,m.identityByRef);
 const bad=structuredClone({loaded,records});bad.records.declarations[0].key.ordinal=55;
 assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='RECORDS.MEMBERSHIP'&&e.field==='declarations');
 const unsorted=structuredClone({loaded,records});unsorted.records.declarations.reverse();
 assert.throws(()=>checkMeasurement(unsorted.loaded,unsorted.records),e=>e.assertion==='RECORDS.ORDER'&&e.field==='declarations');
});
test('same syntax ID across revisions, but revision-local call/control/reference IDs and proof freshness',()=>{
 const s=sample(),old=checkMeasurement(s.loaded,s.records),revised=structuredClone(s);
 for(const group of ['declarations','calls','controls','references'])for(const row of revised.loaded.native[group])row.revisionId='r2';
 const data=revised.loaded.sources.get(JSON.stringify(['main','r1',doc.path]));
 revised.loaded.sources=new Map([[JSON.stringify(['main','r2',doc.path]),data]]);
 revised.loaded.selected={id:'r1',documents:[{key:doc,contentHash:sha(data)}]};
 revised.loaded.comparison={sourceSetId:'main',revisionId:'r1'};
 revised.loaded.revisions=new Map([[JSON.stringify(['main','r2']),{id:'r2',documents:[{key:doc,contentHash:sha(data)}]}]]);
 const nextId=(kind,ordinal=0)=>occurrence({revisionId:'r2',ownerSyntaxId:s.ids.main,kind,ordinal});
 for(const d of revised.records.declarations){d.revisionId='r2';d.provenanceId=`native:r2:${d.syntaxId}`;}
 revised.records.calls[0]={...revised.records.calls[0],id:nextId('call'),regionIds:[nextId('control')],revisionId:'r2',provenanceId:`native:r2:${nextId('call')}`};
 revised.records.controlRegions[0]={...revised.records.controlRegions[0],id:nextId('control'),revisionId:'r2',provenanceId:`native:r2:${nextId('control')}`};
 const measured=checkMeasurement(revised.loaded,revised.records);
 assert.equal(measured.identityByRef.get('main'),old.identityByRef.get('main'));
 assert.notEqual(measured.identityByRef.get('call'),old.identityByRef.get('call'));
 assert.equal(measured.nativeProofRows[0].freshness,'possiblyStale');
});

test('document module owns top-level measured calls; only empty module may have an empty range',()=>{
 const text='go();',bytes=Buffer.from(text),module={ref:'module',nativeId:null,document:doc,revisionId:'r1',parentRef:null,kind:'module',name:null,range:span(0,5),nameRange:null,header:{kind:'module',name:null,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]},signature:null,witnesses:[]};
 const call={ref:'top',nativeId:null,document:doc,revisionId:'r1',ownerRef:'module',range:span(0,4),calleeRange:span(0,2),spelling:'go',regionRefs:[],witnesses:[witness('spelling',0,2,'go')]};
 const id=syntax({sourceSet:'main',path:doc.path,language:doc.language,ancestors:[],declaration:{kind:'module',name:null,signature:null,ordinal:0}}),cid=occurrence({revisionId:'r1',ownerSyntaxId:id,kind:'call',ordinal:0});
 const loaded={native:{formatVersion:1,producerId:'native',declarations:[module],calls:[call],controls:[],references:[]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',doc.path]),bytes]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key:doc,contentHash:sha(bytes)}]}]])};
 const records={declarations:[{syntaxId:id,document:doc,revisionId:'r1',kind:'module',name:null,lookupKey:null,ancestors:[],key:{kind:'module',name:null,signature:null,ordinal:0},range:{start:0,end:5},nameRange:null,header:module.header,provenanceId:`native:r1:${id}`}],calls:[{id:cid,ownerSyntaxId:id,ordinal:0,document:doc,revisionId:'r1',range:{start:0,end:4},calleeRange:{start:0,end:2},spelling:'go',regionIds:[],provenanceId:`native:r1:${cid}`}],controlRegions:[]};
 assert.equal(checkMeasurement(loaded,records).recordByNativeRef.get('top').ownerSyntaxId,id);
 const empty=structuredClone({loaded,records});empty.loaded.native.calls=[];empty.records.calls=[];empty.loaded.native.declarations[0].range=span(0,0);empty.records.declarations[0].range={start:0,end:0};empty.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),Buffer.alloc(0));empty.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(Buffer.alloc(0));
 assert.equal(checkMeasurement(empty.loaded,empty.records).recordByNativeRef.get('module').range.end,0);
});

function javaSignatures(){
 const source='class Example { public <T> T go(T... values) { return values[0]; } Example() {} }\n';
 const key={sourceSetId:'main',language:'java',path:'src/Example.java'},data=Buffer.from(source);
 const at=(text,from=0)=>source.indexOf(text,from),methodStart=at('public'),methodEnd=at('}',methodStart)+1,constructorStart=at('Example()',methodEnd),constructorEnd=at('}',constructorStart)+1;
 const build=(ref,kind,name,start,end,signature,extra)=>{
  const nameStart=at(name,start),h={kind,name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]};
  const row={ref,nativeId:null,document:key,revisionId:'r1',parentRef:null,kind,name,range:span(start,end),nameRange:span(nameStart,nameStart+name.length),header:h,signature,witnesses:[witness('name',nameStart,nameStart+name.length,name),witness('header.name',nameStart,nameStart+name.length,name)]};
  extra?.(row);const descriptor={kind,name,signature,ordinal:0},id=syntax({sourceSet:'main',path:key.path,language:'java',ancestors:[],declaration:descriptor});
  const out={syntaxId:id,document:key,revisionId:'r1',kind,name,lookupKey:name,ancestors:[],key:descriptor,range:{start,end},nameRange:{start:nameStart,end:nameStart+name.length},header:structuredClone(row.header),provenanceId:`native:r1:${id}`};
  return [row,out];
 };
 const method=build('method','method','go',methodStart,methodEnd,{parameterTypes:['T'],typeParameterCount:1,variadic:true},row=>{
  row.header.modifiers=['public'];row.header.typeParameters=['T'];row.header.resultType='T';row.header.parameters=[{name:'values',type:'T',variadic:true}];
  const typeParameter=at('T>',methodStart),resultType=at('T go',methodStart),parameterType=at('T...',methodStart),parameterName=at('values',parameterType);
  for(const [field,pos,text] of [['header.modifiers[0]',methodStart,'public'],['header.typeParameters[0]',typeParameter,'T'],['header.resultType',resultType,'T'],['header.parameters[0].type',parameterType,'T'],['signature.parameterTypes[0]',parameterType,'T'],['header.parameters[0].name',parameterName,'values']])row.witnesses.push(witness(field,pos,pos+text.length,text));
 });
 const ctor=build('constructor','constructor','Example',constructorStart,constructorEnd,{parameterTypes:[],typeParameterCount:0,variadic:false});
 const loaded={native:{formatVersion:1,producerId:'native',declarations:[method[0],ctor[0]],calls:[],controls:[],references:[]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',key.path]),data]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key,contentHash:sha(data)}]}]])};
 return {loaded,records:{declarations:[method[1],ctor[1]].sort((a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId))),calls:[],controlRegions:[]}};
}
test('Java method and constructor signatures have distinct real source leaves',()=>{
 const s=javaSignatures(),m=checkMeasurement(s.loaded,s.records);
 assert.equal(m.recordByNativeRef.get('method').key.signature.variadic,true);
 assert.equal(m.recordByNativeRef.get('constructor').key.signature.typeParameterCount,0);
 for(const field of ['header.modifiers[0]','header.typeParameters[0]','header.resultType','header.parameters[0].type','signature.parameterTypes[0]','header.parameters[0].name']){
  const bad=structuredClone(s);bad.loaded.native.declarations[0].witnesses=bad.loaded.native.declarations[0].witnesses.filter(x=>x.field!==field);
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.code==='invalidRecord'&&e.field===field,field);
 }
 for(const [mutate,field] of [[v=>v.signature.typeParameterCount=0,'signature'],[v=>v.signature.variadic=false,'signature'],[v=>v.header.parameters.push({name:'values',type:'T',variadic:false}),'header.parameters']]){
  const bad=structuredClone(s);mutate(bad.loaded.native.declarations[0]);assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field===field);
 }
 const forbidden=structuredClone(s);forbidden.loaded.native.declarations[1].kind='function';forbidden.loaded.native.declarations[1].header.kind='function';forbidden.loaded.native.declarations[1].signature=null;
 assert.throws(()=>checkMeasurement(forbidden.loaded,forbidden.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='kind');
});
test('source witness changed, wrong encoding, absent null-callee spelling, and empty occurrences reject precisely',()=>{
 const s=sample();
 for(const [editRow,assertion,field] of [
  [v=>v.loaded.native.references[0].witnesses[0].witness.text='other','MEASUREMENT.WITNESS','spelling'],
  [v=>v.loaded.native.declarations[0].name='else','MEASUREMENT.WITNESS','header'],
  [v=>v.loaded.native.calls[0].range.encoding='utf16','MEASUREMENT.ENCODING','range'],
  [v=>{v.loaded.native.calls[0].calleeRange=null;v.loaded.native.calls[0].witnesses=[];},'MEASUREMENT.WITNESS','spelling'],
  [v=>{v.loaded.native.controls[0].range=span(18,18);},'MEASUREMENT.RANGE','range']
 ]){const bad=structuredClone(s);editRow(bad);assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion===assertion&&e.field===field);}
});

test('changed selected bytes mark historical native proofs stale, even with matching declarations',()=>{
 const s=sample();s.loaded.selected={id:'r2',documents:[{key:doc,contentHash:sha(Buffer.from(source+'// edited'))}]};
 s.loaded.comparison={sourceSetId:'main',revisionId:'r2'};
 const m=checkMeasurement(s.loaded,s.records);assert.equal(m.nativeProofRows.every(x=>x.freshness==='stale'),true);
});
test('reference measurement alone never fabricates a Call or normalized Reference',()=>{
 const s=lexical('rust','café','café'),m=checkMeasurement(s.loaded,s.records);
 assert.equal(m.recordByNativeRef.has('lexical'),false);
 assert.equal(m.measuredOccurrence.size,1);
 assert.equal(m.nativeReferenceDescriptors[0].lookupKey,'café');
 assert.deepEqual(s.records.calls,[]);
});

function equalSpanModule(){
 const text='function go() {}',data=Buffer.from(text);
 // The module and its child cover exactly the same independently authored bytes.
 const s=sample();s.loaded.native.declarations=[];s.loaded.native.calls=[];s.loaded.native.controls=[];s.loaded.native.references=[];
 const module={ref:'module',nativeId:null,document:doc,revisionId:'r1',parentRef:null,kind:'module',name:null,range:span(0,data.length),nameRange:null,header:{kind:'module',name:null,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]},signature:null,witnesses:[]};
 const child=nativeDeclaration('child','go',0,data.length,9);child.parentRef='module';
 s.loaded.native.declarations.push(module,child);s.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),data);s.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(data);
 const make=(row)=>{const key={kind:row.kind,name:row.name,signature:null,ordinal:0},id=syntax({sourceSet:'main',path:doc.path,language:doc.language,ancestors:[],declaration:key});return {syntaxId:id,document:doc,revisionId:'r1',kind:row.kind,name:row.name,lookupKey:row.name,ancestors:[],key,range:{start:0,end:data.length},nameRange:row.nameRange?{start:9,end:11}:null,header:row.header,provenanceId:`native:r1:${id}`};};
 s.records={declarations:[make(module),make(child)].sort(orderSyntax),calls:[],controlRegions:[]};return s;
}
test('equal-span module is a required immediate parent, without joining child ancestry',()=>{
 const s=equalSpanModule(),m=checkMeasurement(s.loaded,s.records);
 assert.deepEqual(m.recordByNativeRef.get('child').ancestors,[]);
 const omitted=structuredClone(s);omitted.loaded.native.declarations[1].parentRef=null;
 assert.throws(()=>checkMeasurement(omitted.loaded,omitted.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.code==='invalidRecord'&&e.field==='parentRef');
 for(const change of [v=>v.loaded.native.declarations[0].revisionId='r2',v=>v.loaded.native.declarations[0].document={...doc,path:'other.js'},v=>v.loaded.native.declarations[0].document={...doc,sourceSetId:'other'}]){
  const wrong=structuredClone(s);change(wrong);
  // Admit the other snapshot's same bytes: the parent still cannot own this child.
  const d=wrong.loaded.native.declarations[0].document,r=wrong.loaded.native.declarations[0].revisionId;
  wrong.loaded.sources.set(JSON.stringify([d.sourceSetId,r,d.path]),Buffer.from('function go() {}'));
  const revisionKey=JSON.stringify([d.sourceSetId,r]);
  const revision=wrong.loaded.revisions.get(revisionKey)??{documents:[]};
  if(!revision.documents.some(x=>canonical(x.key)===canonical(d)))revision.documents.push({key:d,contentHash:sha(Buffer.from('function go() {}'))});
  wrong.loaded.revisions.set(revisionKey,revision);
  assert.throws(()=>checkMeasurement(wrong.loaded,wrong.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.code==='invalidRecord'&&e.field==='parentRef');
 }
});
test('a controlled digest collision rejects full descriptors before map insertion',()=>{
 const s=sample(),colliding=(domain,input)=>'0'.repeat(32)+hash(domain,input).slice(32);
 const descriptor=name=>({sourceSet:'main',path:doc.path,language:'javascript',ancestors:[],declaration:{kind:'function',name,signature:null,ordinal:0}});
 assert.notEqual(canonical(descriptor('main')),canonical(descriptor('target')));
 assert.notEqual(colliding('syntax',descriptor('main')),colliding('syntax',descriptor('target')));
 assert.equal(colliding('syntax',descriptor('main')).slice(0,32),colliding('syntax',descriptor('target')).slice(0,32));
 assert.equal(checkMeasurement(s.loaded,s.records).declaration.size,2);
 assert.throws(()=>checkMeasurement(s.loaded,s.records,{handleDigest:colliding}),e=>e.assertion==='ID.SOURCE'&&e.code==='invalidRecord'&&e.field==='id');
 const one=sample();one.loaded.native.declarations.splice(1);one.records.declarations=one.records.declarations.filter(x=>x.name==='main');
 assert.throws(()=>checkMeasurement(one.loaded,one.records,{handleDigest:colliding}),e=>e.assertion==='ID.SOURCE'&&e.code==='invalidRecord'&&e.field==='id');
 assert.equal(checkMeasurement(s.loaded,s.records).identityByRef.get('main'),s.ids.main);
});
function secondCallAndControl(){
 const s=sample(),text=source.replace('target();','target(); target();'),data=Buffer.from(text),delta=10;
 s.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),data);s.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(data);
 s.loaded.native.declarations[0].range.end+=delta;s.loaded.native.controls[0].range.end+=delta;
 s.records.declarations.find(x=>x.name==='main').range.end+=delta;s.records.controlRegions[0].range.end+=delta;
 const target=s.loaded.native.declarations[1];for(const field of ['range','nameRange']){target[field].start+=delta;target[field].end+=delta;}for(const w of target.witnesses){w.witness.range.start+=delta;w.witness.range.end+=delta;}
 const targetRecord=s.records.declarations.find(x=>x.name==='target');for(const field of ['range','nameRange']){targetRecord[field].start+=delta;targetRecord[field].end+=delta;}
 const second={...structuredClone(s.loaded.native.calls[0]),ref:'second',range:span(28,36),calleeRange:span(28,34),regionRefs:['block'],witnesses:[witness('spelling',28,34,'target')]};s.loaded.native.calls.push(second);
 const secondId=occurrence({revisionId:'r1',ownerSyntaxId:s.ids.main,kind:'call',ordinal:1});
 s.records.calls.push({id:secondId,ownerSyntaxId:s.ids.main,ordinal:1,document:doc,revisionId:'r1',range:{start:28,end:36},calleeRange:{start:28,end:34},spelling:'target',regionIds:[s.ids.controlId],provenanceId:`native:r1:${secondId}`});s.records.calls.sort(orderId);
 const inner={...structuredClone(s.loaded.native.controls[0]),ref:'inner',parentRef:'block',kind:'if',range:span(17,27)};s.loaded.native.controls.push(inner);s.loaded.native.calls[0].regionRefs.push('inner');
 const innerId=occurrence({revisionId:'r1',ownerSyntaxId:s.ids.main,kind:'control',ordinal:1});
 s.records.controlRegions.push({id:innerId,ownerSyntaxId:s.ids.main,ordinal:1,document:doc,revisionId:'r1',kind:'if',range:{start:17,end:27},parentId:s.ids.controlId,arm:null,provenanceId:`native:r1:${innerId}`});s.records.controlRegions.sort(orderId);
 s.records.calls.find(x=>x.ordinal===0).regionIds.push(innerId);
 return s;
}
test('two source calls and two controls use independent ASCII-id storage order',()=>{
 const s=secondCallAndControl();checkMeasurement(s.loaded,s.records);
 for(const field of ['calls','controlRegions']){
  const wrong=structuredClone(s);wrong.records[field].reverse();
  assert.throws(()=>checkMeasurement(wrong.loaded,wrong.records),e=>e.assertion==='RECORDS.ORDER'&&e.code==='invalidRecord'&&e.field===field);
  assert.equal(compareEnvelope(s.records[field][0],s.records[field][1])<0,true);
  assert.equal(orderId(s.records[field][0],s.records[field][1])<0,true);
 }
});

test('source rename, document path, container, and ordinal shifts cannot retain stale stable IDs',()=>{
 const initial=sample(),renamed=structuredClone(initial),newText=source.replace('main()','rain()'),data=Buffer.from(newText);
 renamed.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),data);renamed.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(data);
 const row=renamed.loaded.native.declarations[0];row.name='rain';row.header.name='rain';row.witnesses.forEach(x=>x.witness.text='rain');
 assert.throws(()=>checkMeasurement(renamed.loaded,renamed.records),e=>e.assertion==='RECORDS.MEMBERSHIP'&&e.code==='invalidRecord'&&e.field==='declarations');
 const renamedKey={kind:'function',name:'rain',signature:null,ordinal:0},renamedId=syntax({sourceSet:'main',path:doc.path,language:'javascript',ancestors:[],declaration:renamedKey});
 assert.notEqual(renamedId,initial.ids.main);
 const out=renamed.records.declarations.find(x=>x.name==='main');out.name='rain';out.lookupKey='rain';out.header.name='rain';out.key=renamedKey;out.syntaxId=renamedId;out.provenanceId=`native:r1:${renamedId}`;renamed.records.declarations.sort(orderSyntax);
 for(const kind of ['call','control']){
  const key=kind==='call'?'calls':'controlRegions',record=renamed.records[key][0],id=occurrence({revisionId:'r1',ownerSyntaxId:renamedId,kind,ordinal:0});record.ownerSyntaxId=renamedId;record.id=id;record.provenanceId=`native:r1:${id}`;
  if(kind==='control')renamed.records.calls[0].regionIds=[id];
 }
 assert.equal(checkMeasurement(renamed.loaded,renamed.records).identityByRef.get('main'),renamedId);
 const path=structuredClone(initial),other={...doc,path:'src/renamed.js'};
 path.loaded.native.declarations.concat(path.loaded.native.calls,path.loaded.native.controls,path.loaded.native.references).forEach(x=>x.document=other);
 path.loaded.sources=new Map([[JSON.stringify(['main','r1',other.path]),Buffer.from(source)]]);
 path.loaded.revisions.get(JSON.stringify(['main','r1'])).documents=[{key:other,contentHash:sha(Buffer.from(source))}];
 assert.throws(()=>checkMeasurement(path.loaded,path.records),e=>e.assertion==='RECORDS.MEMBERSHIP'&&e.code==='invalidRecord'&&e.field==='declarations');
 for(const row of path.records.declarations){const id=syntax({sourceSet:'main',path:other.path,language:'javascript',ancestors:[],declaration:row.key});row.syntaxId=id;row.document=other;row.provenanceId=`native:r1:${id}`;}
 path.records.declarations.sort(orderSyntax);
 const pathMain=path.records.declarations.find(x=>x.name==='main').syntaxId;
 for(const kind of ['call','control']){const v=path.records[kind==='call'?'calls':'controlRegions'][0],id=occurrence({revisionId:'r1',ownerSyntaxId:pathMain,kind,ordinal:0});v.id=id;v.ownerSyntaxId=pathMain;v.document=other;v.provenanceId=`native:r1:${id}`;if(kind==='control')path.records.calls[0].regionIds=[id];}
 assert.equal(checkMeasurement(path.loaded,path.records).identityByRef.get('main'),pathMain);
 assert.notEqual(pathMain,initial.ids.main);
 const container=nested();container.loaded.native.declarations[1].parentRef=null;
 assert.throws(()=>checkMeasurement(container.loaded,container.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.code==='invalidRecord'&&e.field==='parentRef');
 const ordinal=secondCallAndControl();ordinal.records.calls.find(x=>x.ordinal===1).ordinal=0;
 assert.throws(()=>checkMeasurement(ordinal.loaded,ordinal.records),e=>e.assertion==='RECORDS.MEMBERSHIP'&&e.code==='invalidRecord'&&e.field==='calls');
});
test('cross-snapshot owners, control cycles, invalid regions, and duplicate occurrences reject precisely',()=>{
 const s=secondCallAndControl();checkMeasurement(s.loaded,s.records);
 const cases=[
  [v=>{v.loaded.native.controls[1].parentRef='inner';},'MEASUREMENT.CONTROL','parentRef'],
  [v=>{v.loaded.native.controls[1].range=span(17,18);},'MEASUREMENT.REGION','regionRefs'],
  [v=>{v.loaded.native.calls[0].regionRefs=['block','inner','inner'];},'MEASUREMENT.REGION','regionRefs'],
  [v=>{v.loaded.native.controls.push({...structuredClone(v.loaded.native.controls[0]),ref:'duplicate-control'});},'ID.ORDINAL','range'],
  [v=>{v.loaded.native.references.push({...structuredClone(v.loaded.native.references[0]),ref:'duplicate-reference'});},'ID.ORDINAL','range']
 ];
 for(const [change,assertion,field] of cases){const bad=structuredClone(s);change(bad);assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion===assertion&&e.code==='invalidRecord'&&e.field===field,assertion+':'+field);}
 for(const other of [{...doc,path:'src/other.js'},doc]){
  const bad=structuredClone(s),revisionId=other===doc?'r2':'r1',key=JSON.stringify([other.sourceSetId,revisionId,other.path]),revisionKey=JSON.stringify([other.sourceSetId,revisionId]);
  bad.loaded.sources.set(key,Buffer.from(source.replace('target();','target(); target();')));
  if(other!==doc)bad.loaded.revisions.get(revisionKey).documents.push({key:other,contentHash:sha(bad.loaded.sources.get(key))});
  else bad.loaded.revisions.set(revisionKey,{documents:[{key:other,contentHash:sha(bad.loaded.sources.get(key))}]});
  bad.loaded.native.declarations.push({...structuredClone(bad.loaded.native.declarations[0]),ref:'foreign',document:other,revisionId});
  bad.loaded.native.calls[0].ownerRef='foreign';
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.OWNER'&&e.code==='invalidRecord'&&e.field==='ownerRef');
  // A valid local control with a foreign parent must reject the foreign edge.
  bad.loaded.native.calls[0].ownerRef='main';
  bad.loaded.native.controls.push({...structuredClone(bad.loaded.native.controls[0]),ref:'foreign-control',document:other,revisionId,ownerRef:'foreign'});
  bad.loaded.native.controls[1].parentRef='foreign-control';
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.CONTROL'&&e.code==='invalidRecord'&&e.field==='parentRef');
  bad.loaded.native.controls[1].parentRef='block';bad.loaded.native.calls[0].regionRefs=['foreign-control','inner'];
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.REGION'&&e.code==='invalidRecord'&&e.field==='regionRefs');
 }
});

// Independently authored admitted-source controls; each baseline must pass before its one-bit edit.
const sourceControls=registerControls([
 {id:'ID.SOURCE.rename',baseline:sample,mutate:v=>{const data=Buffer.from(source.replace('main()','rain()'));v.loaded.sources.set(JSON.stringify(['main','r1',doc.path]),data);v.loaded.revisions.get(JSON.stringify(['main','r1'])).documents[0].contentHash=sha(data);const row=v.loaded.native.declarations[0];row.name='rain';row.header.name='rain';row.witnesses.forEach(w=>w.witness.text='rain');return v;},expectedAssertion:'RECORDS.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'declarations'},
 {id:'ID.SOURCE.path',baseline:sample,mutate:v=>{const key={...doc,path:'src/renamed.js'};for(const row of [...v.loaded.native.declarations,...v.loaded.native.calls,...v.loaded.native.controls,...v.loaded.native.references])row.document=key;v.loaded.sources=new Map([[JSON.stringify(['main','r1',key.path]),Buffer.from(source)]]);v.loaded.revisions.get(JSON.stringify(['main','r1'])).documents=[{key,contentHash:sha(Buffer.from(source))}];return v;},expectedAssertion:'RECORDS.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'declarations'},
 {id:'ID.SOURCE.container',baseline:nested,mutate:v=>{v.loaded.native.declarations[1].parentRef=null;return v;},expectedAssertion:'MEASUREMENT.OWNER',expectedCode:'invalidRecord',expectedField:'parentRef'},
 {id:'ID.ORDINAL.source-call',baseline:secondCallAndControl,mutate:v=>{v.records.calls.find(x=>x.ordinal===1).ordinal=0;return v;},expectedAssertion:'RECORDS.MEMBERSHIP',expectedCode:'invalidRecord',expectedField:'calls'},
 {id:'MEASUREMENT.CONTROL.cycle',baseline:secondCallAndControl,mutate:v=>{v.loaded.native.controls[1].parentRef='inner';return v;},expectedAssertion:'MEASUREMENT.CONTROL',expectedCode:'invalidRecord',expectedField:'parentRef'},
 {id:'MEASUREMENT.REGION.noncontaining',baseline:secondCallAndControl,mutate:v=>{v.loaded.native.controls[1].range=span(17,18);return v;},expectedAssertion:'MEASUREMENT.REGION',expectedCode:'invalidRecord',expectedField:'regionRefs'},
 {id:'MEASUREMENT.REGION.adapter-order',baseline:secondCallAndControl,mutate:v=>{v.loaded.native.calls[0].regionRefs=['inner','block'];return v;},expectedAssertion:'MEASUREMENT.REGION',expectedCode:'invalidRecord',expectedField:'regionRefs'},
 {id:'ID.ORDINAL.reference',baseline:sample,mutate:v=>{v.loaded.native.references.push({...structuredClone(v.loaded.native.references[0]),ref:'duplicate-reference'});return v;},expectedAssertion:'ID.ORDINAL',expectedCode:'invalidRecord',expectedField:'range'},
 {id:'ID.ORDINAL.control',baseline:sample,mutate:v=>{v.loaded.native.controls.push({...structuredClone(v.loaded.native.controls[0]),ref:'duplicate-control'});return v;},expectedAssertion:'ID.ORDINAL',expectedCode:'invalidRecord',expectedField:'range'},
 {id:'RECORDS.ORDER.calls',baseline:secondCallAndControl,mutate:v=>{v.records.calls.reverse();return v;},expectedAssertion:'RECORDS.ORDER',expectedCode:'invalidRecord',expectedField:'calls'},
 {id:'RECORDS.ORDER.controls',baseline:secondCallAndControl,mutate:v=>{v.records.controlRegions.reverse();return v;},expectedAssertion:'RECORDS.ORDER',expectedCode:'invalidRecord',expectedField:'controlRegions'}
].map(row=>({check:s=>checkMeasurement(s.loaded,s.records),...row})));
for(const row of sourceControls)test(row.id,()=>runControl(row));

function foreignOwnerControl(){
 const s=sample(),foreign={ref:'target-control',nativeId:null,document:doc,revisionId:'r1',ownerRef:'target',parentRef:null,kind:'block',range:span(48,50),arm:null,witnesses:[]};
 s.loaded.native.controls.push(foreign);
 const id=occurrence({revisionId:'r1',ownerSyntaxId:s.ids.target,kind:'control',ordinal:0});
 s.records.controlRegions.push({id,ownerSyntaxId:s.ids.target,ordinal:0,document:doc,revisionId:'r1',kind:'block',range:{start:48,end:50},parentId:null,arm:null,provenanceId:`native:r1:${id}`});
 s.records.controlRegions.sort(orderId);return s;
}
const foreignControls=registerControls([
 {id:'MEASUREMENT.REGION.cross-owner',mutate:v=>{v.loaded.native.calls[0].regionRefs=['target-control'];return v;},expectedAssertion:'MEASUREMENT.REGION',expectedCode:'invalidRecord',expectedField:'regionRefs'},
 {id:'MEASUREMENT.CONTROL.cross-owner',mutate:v=>{v.loaded.native.controls[0].parentRef='target-control';return v;},expectedAssertion:'MEASUREMENT.CONTROL',expectedCode:'invalidRecord',expectedField:'parentRef'}
].map(row=>({baseline:foreignOwnerControl,check:s=>checkMeasurement(s.loaded,s.records),...row})));
for(const row of foreignControls)test(row.id,()=>runControl(row));
