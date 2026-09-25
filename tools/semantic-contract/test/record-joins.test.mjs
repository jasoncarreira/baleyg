import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join,dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {loadFixture} from '../load.mjs';
import {checkMeasurement} from '../record-check/measurement.mjs';
import {checkJoins} from '../record-check/joins.mjs';
import {registerControls,runControl} from './mutations.mjs';

// Independently authored source offsets, digest inputs and expected normalized rows.
const source='function main() { target(); callback; }\nfunction target() {}\n';
const document={sourceSetId:'main',language:'javascript',path:'src/main.js'};
const sha=value=>createHash('sha256').update(value).digest('hex');
const canonical=value=>value===null||typeof value!=='object'?JSON.stringify(value):Array.isArray(value)?'['+value.map(canonical).join(',')+']':'{'+Object.keys(value).sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b))).map(k=>canonical(k)+':'+canonical(value[k])).join(',')+'}';
const domain=(name,input)=>createHash('sha256').update(`baleyg.${name}.v1\0`).update(canonical(input)).digest('hex').slice(0,32);
const span=(start,end,encoding='utf8')=>({start,end,encoding});
const range=(start,end)=>({start,end});
const witness=(field,start,end,text)=>({field,witness:{range:span(start,end),text}});
const header=name=>({kind:'function',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]});
const syntax=name=>`sid:v1:${domain('syntax',{sourceSet:'main',path:document.path,language:'javascript',ancestors:[],declaration:{kind:'function',name,signature:null,ordinal:0}})}`;
const occurrence=(owner,kind,ordinal)=>`occ:v1:${domain('occurrence',{revisionId:'r1',ownerSyntaxId:owner,kind,ordinal})}`;
const mainId=syntax('main'),targetId=syntax('target');
const callId=occurrence(mainId,'call',0),refId=occurrence(mainId,'reference',0),callbackId=occurrence(mainId,'reference',1);
const declaration=(ref,name,start,end,nameStart)=>({ref,nativeId:null,document,revisionId:'r1',parentRef:null,kind:'function',name,range:span(start,end),nameRange:span(nameStart,nameStart+name.length),header:header(name),signature:null,witnesses:[witness('name',nameStart,nameStart+name.length,name),witness('header.name',nameStart,nameStart+name.length,name)]});
const native={formatVersion:1,producerId:'native',declarations:[declaration('main','main',0,39,9),declaration('target','target',40,60,49)],calls:[{ref:'call',nativeId:null,document,revisionId:'r1',ownerRef:'main',range:span(18,26),calleeRange:span(18,24),spelling:'target',regionRefs:[],witnesses:[witness('spelling',18,24,'target')]}],controls:[],references:[{ref:'target-ref',nativeId:null,document,revisionId:'r1',ownerRef:'main',range:span(18,24),spelling:'target',witnesses:[witness('spelling',18,24,'target')]},{ref:'callback-ref',nativeId:null,document,revisionId:'r1',ownerRef:'main',range:span(28,36),spelling:'callback',witnesses:[witness('spelling',28,36,'callback')]}]};
const nativeProducer={id:'native',version:'1',executableHash:sha('native-executable'),kind:'native',languages:['javascript'],positionEncoding:'utf8'};
const semanticProducer={id:'semantic',version:'1',executableHash:sha('semantic-executable'),kind:'semantic',languages:['javascript'],positionEncoding:'utf8'};
const contentHash=sha(source);
const declaredTarget={kind:'internal',declarationRef:'target',revisionId:'r1'};
const target={kind:'internal',syntaxId:targetId,document,revisionId:'r1'};
const templates={
 declarationBinding:{symbols:[{scheme:'scip',symbol:'pkg target',scope:'global',document:null}],provenanceId:'proof'},
 callBinding:{resolution:'resolved',declaredTarget, candidates:[],dispatch:'direct',possibleDispatch:[],possibleDispatchComplete:false,provenanceId:'proof'},
 reference:{site:'use',roles:['read','call'],resolution:'resolved',declaredTarget,candidates:[],provenanceId:'proof'}
};
const owned={declarationName:'target',callee:'main',invocation:'main',reference:'main'};
const exactSpans={declarationName:[49,55],callee:[18,24],invocation:[18,26],reference:[18,24]};
const unmatchedSpans={declarationName:[40,48],callee:[28,36],invocation:[28,36],reference:[0,8]};
const factType={declarationName:'declarationBinding',callee:'callBinding',invocation:'callBinding',reference:'reference'};
const ordered=rows=>rows.sort((a,b)=>Buffer.compare(Buffer.from(canonical(a)),Buffer.from(canonical(b))));
const absent=()=>({formatVersion:1,comparison:null,producers:[],sourceSets:[],revisions:[],coverage:[],provenance:[],declarations:[],symbols:[],declarationBindings:[],typeRelationships:[],calls:[],controlRegions:[],references:[],referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]});
function makeFact(family,status,ref='fact') {
 const coordinates=(status==='exact'?exactSpans:unmatchedSpans)[family];
 return {kind:factType[family],ref,anchor:{document,revisionId:'r1',contentHash,kind:family,range:span(...coordinates),ownerRef:owned[family]},record:structuredClone(templates[factType[family]])};
}
async function specimen(t,{family='reference',status='exact',facts=null,unsupportedFamily=family,unicode=false}={}) {
 const root=await mkdtemp(join(tmpdir(),'join-u3-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const fs=new Map(),put=(path,value)=>fs.set(path,typeof value==='string'?value:JSON.stringify(value));
 const sourceText=unicode?source+'😀':source,sourceHash=sha(sourceText);
 put('snapshots/src/main.js',sourceText);put('captures/native.json',native);
 for(const [name,value] of [['native','native-executable'],['semantic','semantic-executable'],['toolchain','toolchain'],['config','config'],['dependency','dependency']])put(`captures/${name}.txt`,value);
 const authored=structuredClone(facts??[makeFact(family,status)]);
 if(unicode)for(const fact of authored){fact.anchor.contentHash=sourceHash;fact.anchor.range.encoding='utf16';}
 for(let i=1;i<authored.length;i++)authored[i].record.provenanceId=`proof-${authored[i].ref}`;
 put('captures/semantic.json',{formatVersion:1,producerId:'semantic',facts:authored});
 const captures=[['native','executable','captures/native.txt'],['semantic','executable','captures/semantic.txt'],['toolchain','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['dependency','dependency','captures/dependency.txt'],['artifact','semanticArtifact','captures/semantic.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(fs.get(file))}));
 const revision={id:'r1',sourceSetId:'main',documents:[{key:document,revisionId:'r1',sourceFile:'snapshots/src/main.js'}],toolchainHash:captures[2].hash,configHash:captures[3].hash,dependencyHash:captures[4].hash};
 const basis={producerId:'semantic',producerVersion:'1',producerHash:semanticProducer.executableHash,artifactHash:captures[5].hash,language:'javascript',sourceSetId:'main',revisionId:'r1',sourceManifestHash:sha('baleyg.source-manifest.v1\0'+canonical([{document,contentHash:sourceHash}])),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash,lookupDependencies:[]};
 const proof={id:'proof',producerId:'semantic',document,revisionId:'r1',contentHash:sourceHash,evidenceKind:family==='declarationName'?'declarationBinding':'semanticReference',basis,freshness:'fresh'};
 const proofs=authored.map(fact=>({...proof,id:fact.record.provenanceId}));
 const proofFacts=proofs.map((record,i)=>({kind:'provenance',ref:`proof-fact-${i}`,record}));
 put('snapshots/src/main.js.annotations.json',{formatVersion:1,document,revisionId:'r1',scenarios:[],facts:[...proofFacts,...authored]});
 const support=['declarationName','callee','invocation','reference'].map(kind=>({kind,available:!(status==='unsupported'&&kind===unsupportedFamily),diagnostic:status==='unsupported'&&kind===unsupportedFamily?'native family unavailable':null}));
 const selectedSemantic=unicode?{...semanticProducer,positionEncoding:'utf16'}:semanticProducer;
 const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers:[nativeProducer,selectedSemantic],revisions:[revision],comparison:{sourceSetId:'main',revisionId:'r1',producers:[nativeProducer,selectedSemantic]},coverageIntents:[nativeProducer,selectedSemantic].map(producer=>({producerId:producer.id,document,revisionId:'r1',requestedRoles:[],measurementSupport:support})),nativeArtifact:'captures/native.json',semanticArtifacts:['captures/semantic.json'],annotationFiles:['snapshots/src/main.js.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 put('fixture.json',fixture);put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 for(const [path,value] of fs){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}
 const loaded=await loadFixture(root),records=absent();delete loaded.sourceManifestHash;records.comparison=fixture.comparison;records.producers=fixture.producers;records.sourceSets=fixture.sourceSets;
 records.revisions=[{...revision,documents:[{key:document,revisionId:'r1',contentHash:sourceHash,byteLength:Buffer.byteLength(sourceText)}]}];records.provenance=proofs;
 const asDeclaration=(name,begin,end,nameStart)=>({syntaxId:syntax(name),document,revisionId:'r1',kind:'function',name,lookupKey:name,ancestors:[],key:{kind:'function',name,signature:null,ordinal:0},range:range(begin,end),nameRange:range(nameStart,nameStart+name.length),header:header(name),provenanceId:`native:r1:${syntax(name)}`});
 records.declarations=[asDeclaration('main',0,39,9),asDeclaration('target',40,60,49)].sort((a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId)));
 records.calls=[{id:callId,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',range:range(18,26),calleeRange:range(18,24),spelling:'target',regionIds:[],provenanceId:`native:r1:${callId}`}];
 const measurement=checkMeasurement(loaded,records);
 const expectedJoin=authored.map(fact=>{const bytes=[fact.anchor.range.start,fact.anchor.range.end];const id=fact.anchor.kind==='reference'&&bytes[0]===28?callbackId:{declarationName:targetId,callee:callId,invocation:callId,reference:refId}[fact.anchor.kind];return {anchor:{document,revisionId:'r1',contentHash:sourceHash,range:range(...bytes),kind:fact.anchor.kind},status,candidateIds:status==='exact'?[id]:[],diagnostic:status==='exact'?null:status==='unsupported'?'native family unavailable':'unmatched'};});
 for(const [i,fact] of authored.entries())if(fact.kind==='reference') {
  if(status==='exact'){const callback=fact.anchor.range.start===28;records.references.push({id:callback?callbackId:refId,ownerSyntaxId:mainId,ordinal:callback?1:0,document,revisionId:'r1',range:range(fact.anchor.range.start,fact.anchor.range.end),spelling:callback?'callback':'target',lookupKey:callback?'callback':'target',site:fact.record.site,roles:fact.record.roles,resolution:fact.record.resolution,declaredTarget:target,candidates:[],provenanceId:fact.record.provenanceId});}
  else records.referenceJoinDiagnostics.push({factRef:fact.ref,provenanceId:fact.record.provenanceId,join:expectedJoin[i]});
 }
 records.references=ordered([...new Map(records.references.map(row=>[canonical(row),row])).values()]);
 records.referenceJoinDiagnostics.sort((a,b)=>Buffer.compare(Buffer.from(a.factRef),Buffer.from(b.factRef)));
 return {loaded,records,measurement,authored,expectedJoin};
}
function check(s){return checkJoins(s.loaded,s.records,s.measurement);}

test('admitted source bytes produce exact/unmatched/unsupported joins for all four families',async t=>{
 for(const family of ['declarationName','callee','invocation','reference'])for(const status of ['exact','unmatched','unsupported']){
  const s=await specimen(t,{family,status}),out=check(s);
  assert.equal(canonical(out.joined.get('fact')),canonical(s.expectedJoin[0]),`${family} ${status}`);
  if(family==='reference')assert.equal(s.records.references.length,status==='exact'?1:0);
 }
});
const cases=[
 ['JOIN.TUPLE.hash',s=>{s.loaded.annotations[0].facts[1].anchor.contentHash=sha('wrong');},'JOIN.TUPLE','anchor'],
 ['JOIN.TUPLE.revision',s=>{s.loaded.annotations[0].facts[1].anchor.revisionId='r2';},'JOIN.TUPLE','anchor'],
 ['JOIN.TUPLE.document',s=>{s.loaded.annotations[0].facts[1].anchor.document.path='other.js';},'JOIN.TUPLE','anchor'],
 ['JOIN.TUPLE.encoding',s=>{s.loaded.annotations[0].facts[1].anchor.range.encoding='utf16';},'JOIN.TUPLE','anchor.range'],
 ['JOIN.TUPLE.sourceSet',s=>{s.loaded.annotations[0].facts[1].anchor.document.sourceSetId='other';},'JOIN.TUPLE','anchor'],
 ['JOIN.TUPLE.proof',s=>{s.records.provenance[0].contentHash=sha('other');},'JOIN.TUPLE','provenanceId'],
 ['JOIN.TUPLE.outOfBounds',s=>{s.loaded.annotations[0].facts[1].anchor.range.end=200;},'JOIN.TUPLE','anchor.range','invalidRange'],
 ['JOIN.TUPLE.reversed',s=>{s.loaded.annotations[0].facts[1].anchor.range.start=25;s.loaded.annotations[0].facts[1].anchor.range.end=18;},'JOIN.TUPLE','anchor.range','invalidRange'],
 ['JOIN.FAMILY.reference',s=>{s.loaded.annotations[0].facts[1].anchor.kind='callee';},'JOIN.FAMILY','anchor.kind'],
 ['JOIN.OWNER.absent',s=>{s.loaded.annotations[0].facts[1].anchor.ownerRef='missing';},'JOIN.OWNER','anchor.ownerRef'],
 ['JOIN.OWNER.wrong',s=>{s.loaded.annotations[0].facts[1].anchor.ownerRef='target';},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.SUPPORT.unavailable',s=>{s.loaded.fixture.coverageIntents[0].measurementSupport[3].available=false;s.loaded.fixture.coverageIntents[0].measurementSupport[3].diagnostic='unavailable';},'JOIN.SUPPORT','measurementSupport'],
 ['JOIN.CARDINALITY.duplicateNative',s=>{s.loaded.native.references.push({...s.loaded.native.references[0],ref:'duplicate'});},'JOIN.CARDINALITY','anchor'],
 ['JOIN.DIAGNOSTIC.missing',s=>{s.records.referenceJoinDiagnostics=[];},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.DIAGNOSTIC.extra',s=>{s.records.referenceJoinDiagnostics.push({factRef:'extra',provenanceId:'proof',join:s.expectedJoin[0]});s.records.referenceJoinDiagnostics.sort((a,b)=>Buffer.compare(Buffer.from(a.factRef),Buffer.from(b.factRef)));},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.DIAGNOSTIC.ambiguousForgery',s=>{s.records.referenceJoinDiagnostics[0].join.status='ambiguous';s.records.referenceJoinDiagnostics[0].join.diagnostic='ambiguous';},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.DIAGNOSTIC.statusSwap',s=>{s.records.referenceJoinDiagnostics[0].join.status='unsupported';s.records.referenceJoinDiagnostics[0].join.diagnostic='native family unavailable';},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.DIAGNOSTIC.duplicate',s=>{s.records.referenceJoinDiagnostics.push({...s.records.referenceJoinDiagnostics[0]});},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['JOIN.DIAGNOSTIC.candidateForgery',s=>{s.records.referenceJoinDiagnostics[0].join.candidateIds=[refId];},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['REFERENCE.SOURCE.nonexactInstall',s=>{s.records.references.push({id:refId,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',range:range(18,24),spelling:'target',lookupKey:'target',site:'use',roles:['read'],resolution:'unresolved',declaredTarget:null,candidates:[],provenanceId:'proof'});},'REFERENCE.SOURCE','references'],
 ['JOIN.DIAGNOSTIC.exactForgery',s=>{s.records.referenceJoinDiagnostics.push({factRef:'fact',provenanceId:'proof',join:{...s.expectedJoin[0],status:'unmatched',candidateIds:[],diagnostic:'unmatched'}});},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
 ['REFERENCE.SOURCE.exactMissing',s=>{s.records.references=[];},'REFERENCE.SOURCE','references'],
 ['REFERENCE.SOURCE.extra',s=>{s.records.references.push({...s.records.references[0],id:callbackId});s.records.references.sort((a,b)=>Buffer.compare(Buffer.from(a.id),Buffer.from(b.id)));},'REFERENCE.SOURCE','references'],
 ['REFERENCE.SOURCE.proofSwap',s=>{s.records.references[0].provenanceId='other';},'REFERENCE.SOURCE','references'],
 ['REFERENCE.SOURCE.spelling',s=>{s.records.references[0].spelling='callback';},'REFERENCE.SOURCE','references'],
 ['REFERENCE.ROLES.callCallback',s=>{const fact=s.loaded.annotations[0].facts[1];fact.anchor.range=span(28,36);fact.record.roles=['call'];s.records.references=[{...s.records.references[0],id:callbackId,ordinal:1,range:range(28,36),spelling:'callback',lookupKey:'callback',roles:['call']}];},'REFERENCE.ROLES','roles'],
 ['REFERENCE.ROLES.site',s=>{s.loaded.annotations[0].facts[1].record.roles=['definition'];},'REFERENCE.ROLES','site'],
 ['REFERENCE.ROLES.alias',s=>{s.loaded.annotations[0].facts[1].record.roles=['alias'];},'REFERENCE.ROLES','site'],
 ['REFERENCE.ROLES.order',s=>{s.loaded.annotations[0].facts[1].record.roles=['call','read'];},'REFERENCE.ROLES','roles'],
 ['REFERENCE.RESOLUTION.cardinality',s=>{s.loaded.annotations[0].facts[1].record.resolution='ambiguous';},'REFERENCE.RESOLUTION','resolution'],
 ['REFERENCE.RESOLUTION.target',s=>{s.loaded.annotations[0].facts[1].record.declaredTarget.revisionId='r2';},'REFERENCE.RESOLUTION','declaredTarget']
];
const controls=registerControls(cases.map(([id,mutate,expectedAssertion,expectedField,expectedCode='invalidRecord'])=>({
 id,baseline:()=>specimen(testContext,{status:id.includes('nonexact')||id.includes('DIAGNOSTIC.')?'unmatched':'exact'}),
 check,mutate:s=>(mutate(s),s),expectedAssertion,expectedCode,expectedField
})));
let testContext;
test('one-property source, cardinality, diagnostic and reference controls',async t=>{
 testContext=t;
 for(const row of controls)try{await runControl(row);}catch(error){error.message+=` [${row.id}]`;throw error;}
});

const unicodeControls=registerControls([
 {id:'JOIN.TUPLE.utf16Surrogate',baseline:()=>specimen(testContext,{unicode:true}),check,
  mutate:s=>{s.loaded.annotations[0].facts[1].anchor.range=span(source.length,source.length+1,'utf16');return s;},
  expectedAssertion:'JOIN.TUPLE',expectedCode:'invalidRange',expectedField:'anchor.range'},
 {id:'REFERENCE.SOURCE.conflictingFacts',baseline:()=>specimen(testContext,{facts:[makeFact('reference','exact'),makeFact('reference','exact','fact2')]}),check,
  mutate:s=>{s.loaded.annotations[0].facts.at(-1).record.roles=['read'];return s;},
  expectedAssertion:'REFERENCE.SOURCE',expectedCode:'invalidRecord',expectedField:'references'}
]);
test('UTF-16 scalar boundary and identical versus conflicting semantic facts',async t=>{
 testContext=t;
 const unicode=await specimen(t,{unicode:true});assert.equal(check(unicode).joined.get('fact').status,'exact');
 const repeated=await specimen(t,{facts:[makeFact('reference','exact'),makeFact('reference','exact','fact2')]});
 assert.equal(check(repeated).joined.size,2);assert.equal(repeated.records.references.length,2);
 for(const row of unicodeControls)await runControl(row);
});

test('callback reference joins as reference only; a call role cannot be inferred',async t=>{
 const callback=makeFact('reference','exact');callback.anchor.range=span(28,36);callback.record.roles=['read'];
 const s=await specimen(t,{facts:[callback]});const out=check(s);
 assert.equal(out.joined.get('fact').candidateIds[0],callbackId);
 assert.equal(s.records.references[0].id,callbackId);assert.equal(s.records.references[0].roles.includes('call'),false);
});

const familyControls=registerControls([
 {id:'JOIN.FAMILY.declaration',baseline:()=>specimen(testContext,{family:'declarationName'}),check,
  mutate:s=>{s.loaded.annotations[0].facts[1].anchor.kind='reference';return s;},
  expectedAssertion:'JOIN.FAMILY',expectedCode:'invalidRecord',expectedField:'anchor.kind'},
 {id:'JOIN.FAMILY.call',baseline:()=>specimen(testContext,{family:'invocation'}),check,
  mutate:s=>{s.loaded.annotations[0].facts[1].anchor.kind='reference';return s;},
  expectedAssertion:'JOIN.FAMILY',expectedCode:'invalidRecord',expectedField:'anchor.kind'},
 {id:'JOIN.SUPPORT.noCandidate',baseline:()=>specimen(testContext,{family:'invocation',status:'unmatched'}),check,
  mutate:s=>{s.loaded.fixture.coverageIntents[0].measurementSupport[2].available=false;return s;},
  expectedAssertion:'JOIN.SUPPORT',expectedCode:'invalidRecord',expectedField:'measurementSupport'}
]);
test('family boundaries and native-only measurement capability',async t=>{
 testContext=t;for(const row of familyControls)await runControl(row);
 const s=await specimen(t,{family:'callee',status:'unmatched'});
 s.measurement.candidateRows.push({ref:'fabricated',id:callId,ownerRef:'main',anchor:{...s.expectedJoin[0].anchor}});
 assert.equal(check(s).joined.get('fact').status,'unmatched');
});
