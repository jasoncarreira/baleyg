import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {dirname,join} from 'node:path';
import {loadFixture} from '../load.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {checkMeasurement} from '../record-check/measurement.mjs';
import {checkJoins} from '../record-check/joins.mjs';
import {checkRelationships} from '../record-check/relationships.mjs';
import {registerControls,runControl} from './mutations.mjs';

const source='class Child extends Parent {}\nclass Parent {}\nclass Impl implements Face {}\ninterface Face {}\nclass Base { void method() {} }\nclass Derived extends Base { @Override void method() {} }\n';
const document={sourceSetId:'main',language:'java',path:'src/Main.java'};
const sha=value=>createHash('sha256').update(value).digest('hex');
const canon=value=>value===null||typeof value!=='object'?JSON.stringify(value):Array.isArray(value)?`[${value.map(canon).join(',')}]`:`{${Object.keys(value).sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b))).map(k=>`${canon(k)}:${canon(value[k])}`).join(',')}}`;
const digest=(domain,value)=>createHash('sha256').update(`baleyg.${domain}.v1\0`).update(canon(value)).digest('hex').slice(0,32);
const span=(start,end)=>({start,end,encoding:'utf8'});
const range=(start,end)=>({start,end});
const witness=(field,start,end,text)=>({field,witness:{range:span(start,end),text}});
const globalKey={scheme:'scip',symbol:'pkg Child',scope:'global',document:null};
const externalKey={scheme:'scip',symbol:'dependency External',scope:'global',document:null};
const targetRef=declarationRef=>({kind:'internal',declarationRef,revisionId:'r1'});
const names=['Child','Parent','Impl','Face','Base','Derived'];
function authoredDeclarations(){
 const declarations=[];
 for(const name of names){
  const beginning=source.indexOf(`${name==='Face'?'interface':'class'} ${name} `),nameStart=beginning+(name==='Face'?10:6);
  const end=source.indexOf('}',beginning)+1+(name==='Base'||name==='Derived'?2:0);
  const bases=name==='Child'?['Parent']:name==='Impl'?['Face']:name==='Derived'?['Base']:[];
  const header={kind:'type',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases};
  const witnesses=[witness('name',nameStart,nameStart+name.length,name),witness('header.name',nameStart,nameStart+name.length,name)];
  if(bases.length){const start=source.indexOf(bases[0],nameStart+name.length);witnesses.push(witness('header.bases[0]',start,start+bases[0].length,bases[0]));}
  declarations.push({ref:name,nativeId:null,document,revisionId:'r1',parentRef:null,kind:'type',name,range:span(beginning,end),nameRange:span(nameStart,nameStart+name.length),header,signature:null,witnesses});
 }
 for(const parent of ['Base','Derived']){
  const beginning=source.indexOf('method()',source.indexOf(`class ${parent} `)),name='method',ref=`${parent}.method`;
  const header={kind:'method',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]};
  declarations.push({ref,nativeId:null,document,revisionId:'r1',parentRef:parent,kind:'method',name,range:span(beginning,beginning+11),nameRange:span(beginning,beginning+6),header,signature:null,witnesses:[witness('name',beginning,beginning+6,name),witness('header.name',beginning,beginning+6,name)]});
 }
 return declarations;
}
const declarations=authoredDeclarations(),byRef=new Map(declarations.map(d=>[d.ref,d]));
function idOf(ref){const row=byRef.get(ref),parent=row.parentRef?byRef.get(row.parentRef):null;
 const key=d=>({kind:d.kind,name:d.name,signature:null,ordinal:0});
 return `sid:v1:${digest('syntax',{sourceSet:'main',path:document.path,language:'java',ancestors:parent?[key(parent)]:[],declaration:key(row)})}`;
}
const internal=ref=>({kind:'internal',syntaxId:idOf(ref),document,revisionId:'r1'});
const external={kind:'external',symbol:externalKey};
const relationships=[
 {kind:'typeRelationship',ref:'extends',relationshipKind:'extends',source:targetRef('Child'),target:targetRef('Parent'),provenanceRef:'proof-extends'},
 {kind:'typeRelationship',ref:'implements',relationshipKind:'implements',source:targetRef('Impl'),target:targetRef('Face'),provenanceRef:'proof-implements'},
 {kind:'typeRelationship',ref:'overrides',relationshipKind:'overrides',source:targetRef('Derived.method'),target:targetRef('Base.method'),provenanceRef:'proof-overrides'}
];
const symbol={kind:'symbol',ref:'symbol',record:{key:globalKey,displayName:'Child',declarations:[targetRef('Child'),external],provenanceId:'proof-symbol'}};
const binding={kind:'declarationBinding',ref:'binding',anchor:{document,revisionId:'r1',contentHash:sha(source),kind:'declarationName',range:span(byRef.get('Child').nameRange.start,byRef.get('Child').nameRange.end),ownerRef:'Child'},record:{symbols:[globalKey],provenanceId:'proof-binding'}};
const nativeProducer={id:'native',version:'1',executableHash:sha('native executable'),kind:'native',languages:['java'],positionEncoding:'utf8'};
const semanticProducer={id:'semantic',version:'1',executableHash:sha('semantic executable'),kind:'semantic',languages:['java'],positionEncoding:'utf8'};
const rowOrder=(rows)=>rows.sort((a,b)=>a.syntaxId&&b.syntaxId?Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId))||Buffer.compare(Buffer.from(a.revisionId),Buffer.from(b.revisionId)):a.id&&b.id?Buffer.compare(Buffer.from(a.id),Buffer.from(b.id))||Buffer.compare(Buffer.from(canon(a)),Buffer.from(canon(b))):Buffer.compare(Buffer.from(canon(a)),Buffer.from(canon(b))));
async function specimen(t,change={}){
 const root=await mkdtemp(join(tmpdir(),'relationships-u4-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const files=new Map(),put=(name,value)=>files.set(name,typeof value==='string'?value:JSON.stringify(value));
 const facts=structuredClone([symbol,binding,...relationships]);
 if(change.raw)change.raw(facts);
 put('snapshots/src/Main.java',source);
 put('captures/native.json',{formatVersion:1,producerId:'native',declarations,calls:[],controls:[],references:[]});
 put('captures/semantic.json',{formatVersion:1,producerId:'semantic',facts});
 for(const [name,value] of [['native','native executable'],['semantic','semantic executable'],['toolchain','toolchain'],['config','config'],['dependency','dependency']])put(`captures/${name}.txt`,value);
 const captures=[['native','executable','captures/native.txt'],['semantic','executable','captures/semantic.txt'],['toolchain','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['dependency','dependency','captures/dependency.txt'],['artifact','semanticArtifact','captures/semantic.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(files.get(file))}));
 const revision={id:'r1',sourceSetId:'main',documents:[{key:document,revisionId:'r1',sourceFile:'snapshots/src/Main.java'}],toolchainHash:captures[2].hash,configHash:captures[3].hash,dependencyHash:captures[4].hash};
 const basis={producerId:'semantic',producerVersion:'1',producerHash:semanticProducer.executableHash,artifactHash:captures[5].hash,language:'java',sourceSetId:'main',revisionId:'r1',sourceManifestHash:sha(canon([{document,contentHash:sha(source)}])),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash,lookupDependencies:[]};
 const proofs=facts.map(fact=>({id:fact.provenanceRef??fact.record.provenanceId,producerId:'semantic',document,revisionId:'r1',contentHash:sha(source),evidenceKind:fact.kind==='typeRelationship'?'typeRelationship':'declarationBinding',basis,freshness:'fresh'}));
 const coverage=id=>({producerId:id,language:'java',sourceSetId:'main',documentPath:document.path,revisionId:'r1',requested:true,selected:true,state:'complete',supportedRoles:['definition'],observedRoles:['definition'],diagnostic:null});
 put('snapshots/src/Main.java.annotations.json',{formatVersion:1,document,revisionId:'r1',scenarios:[],facts:[...['native','semantic'].map(id=>({kind:'coverage',ref:`coverage-${id}`,record:coverage(id)})),...proofs.map((record,i)=>({kind:'provenance',ref:`proof-fact-${i}`,record})),...facts]});
 const producers=[nativeProducer,semanticProducer];
 const fixture={formatVersion:1,profile:'example',language:'java',sourceSets:[{id:'main',rootId:'root',languages:['java'],dependencies:[]}],producers,revisions:[revision],comparison:{sourceSetId:'main',revisionId:'r1',producers},coverageIntents:producers.map(producer=>({producerId:producer.id,document,revisionId:'r1',requestedRoles:['definition'],measurementSupport:['declarationName','callee','invocation','reference'].map(kind=>({kind,available:true,diagnostic:null}))})),nativeArtifact:'captures/native.json',semanticArtifacts:['captures/semantic.json'],annotationFiles:['snapshots/src/Main.java.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 put('fixture.json',fixture);put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 for(const [name,contents] of files){await mkdir(dirname(join(root,name)),{recursive:true});await writeFile(join(root,name),contents);}
 const loaded=await loadFixture(root);
 const normalizedDeclarations=declarations.map(d=>({syntaxId:idOf(d.ref),document,revisionId:'r1',kind:d.kind,name:d.name,lookupKey:d.name,ancestors:d.parentRef?[{kind:'type',name:d.parentRef,signature:null,ordinal:0}]:[],key:{kind:d.kind,name:d.name,signature:null,ordinal:0},range:range(d.range.start,d.range.end),nameRange:range(d.nameRange.start,d.nameRange.end),header:d.header,provenanceId:`native:r1:${idOf(d.ref)}`}));
 const records={formatVersion:1,comparison:fixture.comparison,producers:structuredClone(producers).sort((a,b)=>Buffer.compare(Buffer.from(canon(a)),Buffer.from(canon(b)))),sourceSets:fixture.sourceSets,revisions:[{...revision,documents:[{key:document,revisionId:'r1',contentHash:sha(source),byteLength:Buffer.byteLength(source)}]}],coverage:rowOrder(['native','semantic'].map(coverage)),provenance:[...proofs,...declarations.map(d=>({id:`native:r1:${idOf(d.ref)}`,producerId:'native',document,revisionId:'r1',contentHash:sha(source),evidenceKind:'measuredSyntax',basis:null,freshness:'fresh'}))],declarations:rowOrder(normalizedDeclarations),symbols:[],declarationBindings:[],typeRelationships:[],calls:[],controlRegions:[],references:[],referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]};
 for(const fact of facts){if(fact.kind==='symbol')records.symbols.push({...fact.record,declarations:fact.record.declarations.map(x=>x.kind==='external'?x:internal(x.declarationRef))});if(fact.kind==='typeRelationship')records.typeRelationships.push({kind:fact.relationshipKind,source:internal(fact.source.declarationRef),target:fact.target.kind==='external'?fact.target:internal(fact.target.declarationRef==='absent'?'Parent':fact.target.declarationRef),provenanceId:fact.provenanceRef});}
 const anchor={document,revisionId:'r1',contentHash:sha(source),range:range(byRef.get('Child').nameRange.start,byRef.get('Child').nameRange.end),kind:'declarationName'};
 records.declarationBindings.push({syntaxId:idOf('Child'),symbols:binding.record.symbols,join:{anchor,status:'exact',candidateIds:[idOf('Child')],diagnostic:null},provenanceId:'proof-binding'});
 for(const field of ['symbols','declarationBindings','typeRelationships'])rowOrder(records[field]);
 change.records?.(records);
 const C=checkCoverage(loaded,records),M=checkMeasurement(loaded,records),J=checkJoins(loaded,records,C,M);
 if(change.nonExact){records.declarationBindings[0].syntaxId=null;records.declarationBindings[0].join=J.joined.get('binding').join;}
 return {loaded,records,C,M,J};
}
const check=({loaded,records,C,M,J})=>checkRelationships(loaded,records,C,M,J);
test('captured extends, implements and overrides; explicit external targets and exact declaration-name binding',async t=>{
 const value=await specimen(t),S=check(value);
 assert.deepEqual(S.typeRelationships.map(x=>x.kind).sort(),['extends','implements','overrides']);
 assert.equal(canon(S.recordByFactRef.get('symbol').declarations[1]),canon(external));
 assert.equal(S.recordByFactRef.get('binding').syntaxId,idOf('Child'));
 assert.equal(S.recordByFactRef.size,5);
});
const controls=registerControls([
 ...[
  ['SYMBOL.FACT','symbols',r=>r.symbols[0].displayName='Changed','SYMBOL.FACT','symbols'],
  ['SYMBOL.SCOPE','symbols',r=>r.symbols[0].key.document=document,'SYMBOL.SCOPE','key'],
  ['SYMBOL.TARGET','symbols',r=>r.symbols[0].declarations[0]=internal('Parent'),'SYMBOL.TARGET','declarations'],
  ['DECLARATION_BINDING.JOIN','declarationBindings',r=>r.declarationBindings[0].syntaxId=idOf('Parent'),'DECLARATION_BINDING.JOIN','join'],
  ['DECLARATION_BINDING.PROOF','declarationBindings',r=>r.declarationBindings[0].provenanceId='proof-symbol','DECLARATION_BINDING.PROOF','provenanceId'],
  ['RELATIONSHIP.KIND','typeRelationships',r=>r.typeRelationships[0].kind='implements','RELATIONSHIP.KIND','kind'],
  ['RELATIONSHIP.SOURCE','typeRelationships',r=>r.typeRelationships[0].source=internal('Parent'),'RELATIONSHIP.SOURCE','source'],
  ['RELATIONSHIP.TARGET','typeRelationships',r=>r.typeRelationships[0].target=internal('Face'),'RELATIONSHIP.TARGET','target'],
  ['RELATIONSHIP.PROOF','typeRelationships',r=>r.typeRelationships[0].provenanceId='proof-binding','RELATIONSHIP.PROOF','provenanceId'],
  ['RELATIONSHIP.TARGET.document','typeRelationships',r=>r.typeRelationships[0].target.document={...document,path:'src/other.js'},'RELATIONSHIP.TARGET','target'],
  ['RECORDS.MEMBERSHIP.missing','typeRelationships',r=>r.typeRelationships.pop(),'RECORDS.MEMBERSHIP','typeRelationships'],
  ['RECORDS.MEMBERSHIP.extra','symbols',r=>r.symbols.push({...r.symbols[0],displayName:'extra'}),'RECORDS.MEMBERSHIP','symbols'],
  ['RECORDS.MEMBERSHIP.duplicate','symbols',r=>r.symbols.push(structuredClone(r.symbols[0])),'RECORDS.MEMBERSHIP','symbols'],
  ['RECORDS.ORDER','typeRelationships',r=>r.typeRelationships.reverse(),'RECORDS.ORDER','typeRelationships']
 ].map(([id,field,mutate,expectedAssertion,expectedField])=>({id,baseline:()=>({}),mutate:v=>{v.change={records:mutate};return v;},check:async()=>true,expectedAssertion,expectedCode:'invalidRecord',expectedField})),
]);
for(const row of controls)test(row.id,async t=>runControl({...row,check:async value=>check(await specimen(t,value.change))}));

test('unlocated symbol and non-exact declaration-name diagnostic do not install a declaration',async t=>{
 const unlocated=await specimen(t,{raw:facts=>facts[0].record.declarations=[]});
 assert.deepEqual(check(unlocated).symbols[0].declarations,[]);
 const unmatched=await specimen(t,{raw:facts=>{facts[1].anchor.range=span(7,11);},nonExact:true});
 assert.equal(check(unmatched).declarationBindings[0].syntaxId,null);
 assert.equal(check(unmatched).declarationBindings[0].join.status,'unmatched');
});
test('explicit external relationship target and document-scoped key remain source facts',async t=>{
 const value=await specimen(t,{raw:facts=>{
  facts[0].record.key={scheme:'scip',symbol:'src/Main.java Child',scope:'document',document};
  facts.push({kind:'typeRelationship',ref:'external-base',relationshipKind:'extends',source:targetRef('Child'),target:external,provenanceRef:'proof-external'});
 }});
 const checked=check(value);
 assert.equal(checked.typeRelationships.length,4);
 assert.equal(checked.recordByFactRef.get('external-base').target.kind,'external');
 assert.equal(checked.symbols[0].key.scope,'document');
});
const nonExactControls=registerControls([{
 id:'DECLARATION_BINDING.JOIN.nonExactSyntaxId',baseline:()=>({}),
 mutate:value=>({...value,mutated:true}),
 check:async()=>true,expectedAssertion:'DECLARATION_BINDING.JOIN',expectedCode:'invalidRecord',expectedField:'join'
}]);
for(const row of nonExactControls)test(row.id,async t=>runControl({...row,check:async value=>{
 const item=await specimen(t,{raw:facts=>{facts[1].anchor.range=span(7,11);},nonExact:true});
 if(value.mutated)item.records.declarationBindings[0].syntaxId=idOf('Child');
 return check(item);
}}));
const sourceControls=registerControls([
 ['SYMBOL.SCOPE.raw',facts=>facts[0].record.key={...globalKey,scope:'document',document:{...document,path:'src/not-here.js'}},'SYMBOL.SCOPE','key'],
 ['SYMBOL.TARGET.raw',facts=>facts[0].record.declarations[0].revisionId='r2','SYMBOL.TARGET','declarations'],
 ['DECLARATION_BINDING.JOIN.raw',facts=>facts[1].record.symbols=[],'DECLARATION_BINDING.JOIN','symbols'],
 ['RELATIONSHIP.KIND.raw',facts=>facts[2].relationshipKind='implements','RELATIONSHIP.KIND','kind'],
 ['RELATIONSHIP.SOURCE.raw',facts=>{facts[2].source=targetRef('Parent');facts[2].target=targetRef('Child');},'RELATIONSHIP.SOURCE','source'],
 ['RELATIONSHIP.TARGET.raw',facts=>facts[2].target={kind:'internal',declarationRef:'absent',revisionId:'r1'},'RELATIONSHIP.TARGET','target'],
 ['RELATIONSHIP.TARGET.revision',facts=>facts[2].target.revisionId='r2','RELATIONSHIP.TARGET','target'],
].map(([id,raw,expectedAssertion,expectedField])=>({id,baseline:()=>({}),mutate:value=>({...value,change:{raw}}),check:async()=>true,expectedAssertion,expectedCode:'invalidRecord',expectedField})));
for(const row of sourceControls)test(row.id,async t=>runControl({...row,check:async value=>check(await specimen(t,value.change))}));
for(const [label,mutate] of [
 ['missing',fact=>{delete fact.relationshipKind;}],
 ['default',fact=>{fact.relationshipKind=null;}],
 ['discriminator-substituted',fact=>{fact.kind='extends';delete fact.relationshipKind;}]
])test(`${label} relationshipKind fails the format guard`,async t=>{
 await assert.rejects(()=>specimen(t,{raw:facts=>mutate(facts[2])}),error=>{
  assert.equal(error.assertion,'FORMAT.SHAPE');assert.equal(error.code,'invalidRecord');return true;
 });
});
