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
 const declarationRows=[decl('main',0,29,9,main),decl('target',30,50,39,target)].sort((a,b)=>compareEnvelope(a,b));
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
 const body=`function go() { ${spelling}; }\n`,key={sourceSetId:'main',language,path:'src/main.js'},start=body.indexOf(spelling),end=start+Buffer.byteLength(spelling);
 const nameStart=body.indexOf('go'),signature=language==='java'?{parameterTypes:[],typeParameterCount:0,variadic:false}:null,kind=language==='java'?'method':'function';
 const declaration={ref:'go',nativeId:null,document:key,revisionId:'r1',parentRef:null,kind,name:'go',range:span(0,Buffer.byteLength(body)),nameRange:span(nameStart,nameStart+2),header:{...header('go'),kind},signature,witnesses:[witness('name',nameStart,nameStart+2,'go'),witness('header.name',nameStart,nameStart+2,'go')]};
 const reference={ref:'lexical',nativeId:null,document:key,revisionId:'r1',ownerRef:'go',range:span(start,end),spelling,witnesses:[witness('spelling',start,end,spelling)]};
 const sid=syntax({sourceSet:'main',path:key.path,language,ancestors:[],declaration:{kind,name:'go',signature,ordinal:0}});
 const row={syntaxId:sid,document:key,revisionId:'r1',kind,name:'go',lookupKey:'go',ancestors:[],key:{kind,name:'go',signature,ordinal:0},range:{start:0,end:Buffer.byteLength(body)},nameRange:{start:nameStart,end:nameStart+2},header:declaration.header,provenanceId:`native:r1:${sid}`};
 const bytes=Buffer.from(body),loaded={native:{formatVersion:1,producerId:'native',declarations:[declaration],calls:[],controls:[],references:[reference]},fixture:{producers:[{id:'native',kind:'native',positionEncoding:'utf8'}]},sources:new Map([[JSON.stringify(['main','r1',key.path]),bytes]]),revisions:new Map([[JSON.stringify(['main','r1']),{documents:[{key,contentHash:sha(bytes)}]}]])};
 return {loaded,records:{declarations:[row],calls:[],controlRegions:[]},expected};
}
for(const [language,spelling,expected] of [
 ['java','\\u0074arget','target'],['javascript','\\u{74}arget','target'],
 ['rust','r#café','café'],['rust','café','café'],['python','ｆｏｏ','foo']
])test(`measured ${language} lexical spelling ${spelling}`,()=>{
 const {loaded,records}=lexical(language,spelling,expected),m=checkMeasurement(loaded,records);
 assert.equal(m.nativeReferenceDescriptors[0].spelling,spelling);
 assert.equal(m.nativeReferenceDescriptors[0].lookupKey,expected);
});
test('Java signature projection and non-Java signature exclusion',()=>{
 const s=lexical('java','go','go');assert.equal(checkMeasurement(s.loaded,s.records).recordByNativeRef.get('go').key.signature.typeParameterCount,0);
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
 const records={declarations:[record(outer,outerId,[]),record(inner,innerId,[outerKey])].sort(compareEnvelope),calls:[{id:cid,ownerSyntaxId:innerId,ordinal:0,document:key,revisionId:'r1',range:{start:callStart,end:callStart+4},calleeRange:{start:callStart,end:callStart+2},spelling:'go',regionIds:[],provenanceId:`native:r1:${cid}`}],controlRegions:[]};
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
 s.records.controlRegions.sort(compareEnvelope);s.records.calls[0].regionIds.push(innerId);
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
 const expected=starts.map((start,i)=>{const identity=syntax({sourceSet:'main',path:key.path,language:key.language,ancestors:[],declaration:{kind:'function',name:'go',signature:null,ordinal:i}});return {syntaxId:identity,document:key,revisionId:'r1',kind:'function',name:'go',lookupKey:'go',ancestors:[],key:{kind:'function',name:'go',signature:null,ordinal:i},range:{start,end:start+16},nameRange:{start:start+9,end:start+11},header:header('go'),provenanceId:`native:r1:${identity}`};}).sort(compareEnvelope);
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

test('Java signature types, type-parameter count and final-varargs are measured projections',()=>{
 const s=lexical('java','go','go'),row=s.loaded.native.declarations[0],out=s.records.declarations[0];
 row.header.typeParameters=['go'];row.header.parameters=[{name:'go',type:'go',variadic:true}];
 row.signature={parameterTypes:['go'],typeParameterCount:1,variadic:true};
 for(const field of ['header.typeParameters[0]','header.parameters[0].name','header.parameters[0].type','signature.parameterTypes[0]'])row.witnesses.push(witness(field,9,11,'go'));
 out.header=structuredClone(row.header);out.key.signature=structuredClone(row.signature);
 out.syntaxId=syntax({sourceSet:'main',path:doc.path,language:'java',ancestors:[],declaration:out.key});
 out.provenanceId=`native:r1:${out.syntaxId}`;
 checkMeasurement(s.loaded,s.records);
 for(const field of ['header.typeParameters[0]','header.parameters[0].name','header.parameters[0].type','signature.parameterTypes[0]']){
  const bad=structuredClone(s);bad.loaded.native.declarations[0].witnesses=bad.loaded.native.declarations[0].witnesses.filter(x=>x.field!==field);
  assert.throws(()=>checkMeasurement(bad.loaded,bad.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field===field);
  const changed=structuredClone(s);changed.loaded.native.declarations[0].witnesses.find(x=>x.field===field).witness.text='other';
  assert.throws(()=>checkMeasurement(changed.loaded,changed.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field===field);
 }
 const count=structuredClone(s);count.loaded.native.declarations[0].signature.typeParameterCount=0;
 assert.throws(()=>checkMeasurement(count.loaded,count.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='signature');
 const varargs=structuredClone(s);varargs.loaded.native.declarations[0].signature.variadic=false;
 assert.throws(()=>checkMeasurement(varargs.loaded,varargs.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='signature');
 const nonFinal=structuredClone(s);nonFinal.loaded.native.declarations[0].header.parameters.push({name:'go',type:'go',variadic:false});
 assert.throws(()=>checkMeasurement(nonFinal.loaded,nonFinal.records),e=>e.assertion==='MEASUREMENT.WITNESS'&&e.field==='header.parameters');
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
