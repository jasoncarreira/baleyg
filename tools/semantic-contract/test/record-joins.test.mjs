import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {join,dirname} from 'node:path';
import {tmpdir} from 'node:os';
import {loadFixture} from '../load.mjs';
import {checkMeasurement} from '../record-check/measurement.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {checkJoins} from '../record-check/joins.mjs';

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
async function specimen(t,{family='reference',status='exact',facts=null,unsupportedFamily=family,encoding='utf8',emojiPrefix=false,control=false,declarationSite=false,unselectedProducer=false,mutateFact=null}={}) {
 const root=await mkdtemp(join(tmpdir(),'join-u3-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const fs=new Map(),put=(path,value)=>fs.set(path,typeof value==='string'?value:JSON.stringify(value));
 const shifted=emojiPrefix||encoding!=='utf8',offset=shifted?5:0,positionOffset=encoding==='utf16'?3:encoding==='unicodeScalar'?2:offset;
 const sourceText=shifted?'😀\n'+source:source,sourceHash=sha(sourceText);
 const measuredNative=structuredClone(native);
 if(shifted)for(const row of [...measuredNative.declarations,...measuredNative.calls,...measuredNative.references]){
  for(const key of ['range','nameRange','calleeRange'])if(row[key]){row[key].start+=offset;row[key].end+=offset;}
  for(const entry of row.witnesses){entry.witness.range.start+=offset;entry.witness.range.end+=offset;}
 }
 if(control){measuredNative.controls.push({ref:'region',nativeId:null,document,revisionId:'r1',ownerRef:'main',parentRef:null,kind:'if',range:span(18+offset,26+offset),arm:null,witnesses:[]});measuredNative.calls[0].regionRefs=['region'];}
 if(declarationSite)measuredNative.references.push({ref:'declaration-ref',nativeId:null,document,revisionId:'r1',ownerRef:'target',range:span(49+offset,55+offset),spelling:'target',witnesses:[witness('spelling',49+offset,55+offset,'target')]});
 put('snapshots/src/main.js',sourceText);put('captures/native.json',measuredNative);
 for(const [name,value] of [['native','native-executable'],['semantic','semantic-executable'],['toolchain','toolchain'],['config','config'],['dependency','dependency']])put(`captures/${name}.txt`,value);
 const authored=structuredClone(facts??[makeFact(family,status)]);
 for(const fact of authored){fact.anchor.contentHash=sourceHash;if(shifted){fact.anchor.range.start+=positionOffset;fact.anchor.range.end+=positionOffset;fact.anchor.range.encoding=encoding;}if(declarationSite&&fact.kind==='reference'&&fact.anchor.range.start===18+positionOffset){fact.anchor.range=span(49+positionOffset,55+positionOffset,encoding);fact.anchor.ownerRef='target';fact.record.site='declaration';fact.record.roles=['definition'];}mutateFact?.(fact);}
 for(let i=1;i<authored.length;i++)authored[i].record.provenanceId=`proof-${authored[i].ref}`;
 if(unselectedProducer){const other=structuredClone(authored[0]);other.ref='fact-b';other.record.provenanceId='proof-b';authored.push(other);}
 put('captures/semantic.json',{formatVersion:1,producerId:'semantic',facts:authored.filter(f=>f.ref!=='fact-b')});
 if(unselectedProducer){put('captures/semantic-b.txt','semantic-b-executable');put('captures/semantic-b.json',{formatVersion:1,producerId:'semantic-b',facts:authored.filter(f=>f.ref==='fact-b')});}
 const captures=[['native','executable','captures/native.txt'],['semantic','executable','captures/semantic.txt'],['toolchain','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['dependency','dependency','captures/dependency.txt'],['artifact','semanticArtifact','captures/semantic.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(fs.get(file))}));
 if(unselectedProducer)captures.push(...[['semantic-b-executable','executable','captures/semantic-b.txt'],['semantic-b-artifact','semanticArtifact','captures/semantic-b.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(fs.get(file))})));
 const revision={id:'r1',sourceSetId:'main',documents:[{key:document,revisionId:'r1',sourceFile:'snapshots/src/main.js'}],toolchainHash:captures[2].hash,configHash:captures[3].hash,dependencyHash:captures[4].hash};
 const basis={producerId:'semantic',producerVersion:'1',producerHash:semanticProducer.executableHash,artifactHash:captures[5].hash,language:'javascript',sourceSetId:'main',revisionId:'r1',sourceManifestHash:sha(canonical([{document,contentHash:sourceHash}])),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash,lookupDependencies:[]};
 const proof={id:'proof',producerId:'semantic',document,revisionId:'r1',contentHash:sourceHash,evidenceKind:family==='declarationName'?'declarationBinding':'semanticReference',basis,freshness:'fresh'};
 const proofs=authored.map(fact=>fact.ref==='fact-b'?{...proof,id:'proof-b',producerId:'semantic-b',basis:{...basis,producerId:'semantic-b',producerHash:sha('semantic-b-executable'),artifactHash:sha(fs.get('captures/semantic-b.json'))},freshness:'possiblyStale'}:{...proof,id:fact.record.provenanceId});
 const proofFacts=proofs.map((record,i)=>({kind:'provenance',ref:`proof-fact-${i}`,record}));
 const support=['declarationName','callee','invocation','reference'].map(kind=>({kind,available:!(status==='unsupported'&&kind===unsupportedFamily),diagnostic:status==='unsupported'&&kind===unsupportedFamily?'native family unavailable':null}));
 const coverage=(producerId)=>({producerId,language:document.language,sourceSetId:document.sourceSetId,documentPath:document.path,revisionId:'r1',requested:true,selected:producerId!=='semantic-b',state:producerId==='semantic-b'?'omitted':status==='unsupported'&&unsupportedFamily==='reference'?'partial':'complete',supportedRoles:['read'],observedRoles:producerId==='semantic-b'||status==='unsupported'&&unsupportedFamily==='reference'?[]:['read'],diagnostic:producerId==='semantic-b'?'producer not selected':status==='unsupported'&&unsupportedFamily==='reference'?'reference unavailable':null});
 put('snapshots/src/main.js.annotations.json',{formatVersion:1,document,revisionId:'r1',scenarios:[],facts:[...(unselectedProducer?['native','semantic','semantic-b']:['native','semantic']).map(id=>({kind:'coverage',ref:`coverage-${id}`,record:coverage(id)})),...proofFacts,...authored]});
 const selectedSemantic={...semanticProducer,positionEncoding:encoding};
 const unselectedSemantic={...semanticProducer,id:'semantic-b',executableHash:sha('semantic-b-executable'),positionEncoding:encoding};
 const producers=unselectedProducer?[nativeProducer,selectedSemantic,unselectedSemantic]:[nativeProducer,selectedSemantic];
 const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers,revisions:[revision],comparison:{sourceSetId:'main',revisionId:'r1',producers:[nativeProducer,selectedSemantic]},coverageIntents:producers.map(producer=>({producerId:producer.id,document,revisionId:'r1',requestedRoles:['read'],measurementSupport:support})),nativeArtifact:'captures/native.json',semanticArtifacts:unselectedProducer?['captures/semantic.json','captures/semantic-b.json']:['captures/semantic.json'],annotationFiles:['snapshots/src/main.js.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 put('fixture.json',fixture);put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 for(const [path,value] of fs){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}
 const loaded=await loadFixture(root),records=absent();records.comparison=fixture.comparison;records.producers=ordered(structuredClone(fixture.producers));records.sourceSets=ordered(structuredClone(fixture.sourceSets));
 records.revisions=[{...revision,documents:[{key:document,revisionId:'r1',contentHash:sourceHash,byteLength:Buffer.byteLength(sourceText)}]}];records.provenance=proofs;records.coverage=ordered((unselectedProducer?['native','semantic','semantic-b']:['native','semantic']).map(coverage));
 const asDeclaration=(name,begin,end,nameStart)=>({syntaxId:syntax(name),document,revisionId:'r1',kind:'function',name,lookupKey:name,ancestors:[],key:{kind:'function',name,signature:null,ordinal:0},range:range(begin,end),nameRange:range(nameStart,nameStart+name.length),header:header(name),provenanceId:`native:r1:${syntax(name)}`});
 records.declarations=[asDeclaration('main',0+offset,39+offset,9+offset),asDeclaration('target',40+offset,60+offset,49+offset)].sort((a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId)));
 records.calls=[{id:callId,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',range:range(18+offset,26+offset),calleeRange:range(18+offset,24+offset),spelling:'target',regionIds:control?[occurrence(mainId,'control',0)]:[],provenanceId:`native:r1:${callId}`}];
 if(control){const id=occurrence(mainId,'control',0);records.controlRegions=[{id,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',kind:'if',range:range(18+offset,26+offset),parentId:null,arm:null,provenanceId:`native:r1:${id}`}];}
 const coverageResult=checkCoverage(loaded,records);
 const measurement=checkMeasurement(loaded,records);
 const expectedJoin=authored.map(fact=>{const bytes=[fact.anchor.range.start,fact.anchor.range.end].map(n=>n-positionOffset+offset);const id=fact.anchor.kind==='reference'&&bytes[0]===28+offset?callbackId:declarationSite&&fact.anchor.kind==='reference'&&bytes[0]===49+offset?occurrence(targetId,'reference',0):{declarationName:targetId,callee:callId,invocation:callId,reference:refId}[fact.anchor.kind];return {anchor:{document,revisionId:'r1',contentHash:sourceHash,range:range(...bytes),kind:fact.anchor.kind},status,candidateIds:status==='exact'?[id]:[],diagnostic:status==='exact'?null:status==='unsupported'?'native family unavailable':'unmatched'};});
 for(const [i,fact] of authored.entries())if(fact.kind==='reference') {
  if(status==='exact'){const callback=fact.anchor.range.start===28+positionOffset,atDeclaration=declarationSite&&fact.anchor.range.start===49+positionOffset;const measuredRange=expectedJoin[i].anchor.range;const external={kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}};const resolution=fact.record.resolution;records.references.push({id:atDeclaration?occurrence(targetId,'reference',0):callback?callbackId:refId,ownerSyntaxId:atDeclaration?targetId:mainId,ordinal:callback?1:0,document,revisionId:'r1',range:measuredRange,spelling:callback?'callback':'target',lookupKey:callback?'callback':'target',site:fact.record.site,roles:fact.record.roles,resolution,declaredTarget:resolution==='resolved'?target:resolution==='external'?external:null,candidates:resolution==='ambiguous'?ordered([target,external]):[],provenanceId:fact.record.provenanceId});}
  else records.referenceJoinDiagnostics.push({factRef:fact.ref,provenanceId:fact.record.provenanceId,join:expectedJoin[i]});
 }
 records.references=ordered([...new Map(records.references.map(row=>[canonical(row),row])).values()]);
 records.referenceJoinDiagnostics.sort((a,b)=>Buffer.compare(Buffer.from(a.factRef),Buffer.from(b.factRef)));
 return {loaded,records,coverageResult,measurement,authored,expectedJoin};
}
function check(s){return checkJoins(s.loaded,s.records,s.coverageResult,s.measurement);}

test('admitted source bytes produce exact/unmatched/unsupported joins for all four families',async t=>{
 for(const family of ['declarationName','callee','invocation','reference'])for(const status of ['exact','unmatched','unsupported']){
  const s=await specimen(t,{family,status}),out=check(s);
  assert.equal(canonical(out.joined.get('fact').join),canonical(s.expectedJoin[0]),`${family} ${status}`);
  if(family==='reference')assert.equal(s.records.references.length,status==='exact'?1:0);
 }
});
// Raw-fact controls are authored before loading. Normalized controls change only
// records after admission; neither path borrows an oracle from the validator.
async function control(t,{name,options={},raw=null,normalized=null,assertion,field,code='invalidRecord'}){
 const baseline=await specimen(t,options);assert.ok(check(baseline),`${name}: admitted baseline`);
 const changed=raw?await specimen(t,{...options,mutateFact:raw}):await specimen(t,options);
 normalized?.(changed.records);
 assert.doesNotThrow(()=>checkCoverage(changed.loaded,changed.records),`${name}: admitted coverage`);
 assert.doesNotThrow(()=>checkMeasurement(changed.loaded,changed.records),`${name}: admitted measurement`);
 assert.throws(()=>check(changed),error=>{
  assert.equal(error.assertion,assertion,`${name}: assertion`);
  assert.equal(error.code,code,`${name}: code`);
  assert.equal(error.field,field,`${name}: field`);return true;
 });
}

test('per-fact installed identity, source proof, native refs, and diagnostic-only mapping',async t=>{
 for(const family of ['declarationName','callee','invocation','reference'])for(const status of ['exact','unmatched','unsupported']){
  const s=await specimen(t,{family,status}),result=check(s),row=result.joined.get('fact');
  assert.equal(canonical(row.join),canonical(s.expectedJoin[0]));
  assert.equal(row.installedId,status==='exact'?s.expectedJoin[0].candidateIds[0]:null);
  assert.equal(row.producerId,'semantic');assert.deepEqual(row.provenanceIds,['proof']);
  assert.deepEqual(row.nativeRefs,status==='exact'?[{declarationName:'target',callee:'call',invocation:'call',reference:'target-ref'}[family]]:[]);
  assert.equal(result.recordByFactRef.has('fact'),true);
  if(family==='reference'){
   assert.equal(result.expectedReferences.length,status==='exact'?1:0);
   assert.equal(result.expectedDiagnostics.length,status==='exact'?0:1);
   assert.equal(result.recordByFactRef.get('fact').factRef,status==='exact'?undefined:'fact');
  }
 }
});

test('native controls overlap invocations but cannot join any family',async t=>{
 for(const family of ['callee','invocation','reference']){
  const s=await specimen(t,{family,control:true}),result=check(s);
  assert.equal(s.loaded.native.controls[0].range.start,s.loaded.native.calls[0].range.start);
  assert.equal(result.joined.get('fact').installedId,s.expectedJoin[0].candidateIds[0]);
  assert.equal(result.joined.get('fact').nativeRefs.includes('region'),false);
 }
});

test('real emoji prefix maps UTF-8, UTF-16 and scalar positions to the same native byte span',async t=>{
 const rows=[];
 for(const encoding of ['utf8','utf16','unicodeScalar']){
  const s=await specimen(t,{encoding,emojiPrefix:true}),actual=check(s).joined.get('fact');
  rows.push(actual.join.anchor.range);
  assert.deepEqual(actual.join.anchor.range,{start:23,end:29});
  assert.deepEqual(actual.nativeRefs,['target-ref']);
 }
 assert.deepEqual(rows,[{start:23,end:29},{start:23,end:29},{start:23,end:29}]);
});

test('bad byte/scalar boundaries and tuple, family, owner, support facts',async t=>{
 const rawCases=[
  ['hash',{},f=>{f.anchor.contentHash=sha('wrong');},'JOIN.TUPLE','anchor'],
  ['encoding',{},f=>{f.anchor.range.encoding='utf16';},'JOIN.TUPLE','anchor.range'],
  ['family',{},f=>{f.anchor.kind='callee';},'JOIN.FAMILY','anchor.kind'],
  ['owner',{},f=>{f.anchor.ownerRef='absent';},'JOIN.OWNER','anchor.ownerRef'],
  ['utf8 split',{encoding:'utf8',emojiPrefix:true},f=>{f.anchor.range=span(1,4);},'JOIN.TUPLE','anchor.range','invalidRange'],
  ['utf16 surrogate',{encoding:'utf16'},f=>{f.anchor.range=span(1,3,'utf16');},'JOIN.TUPLE','anchor.range','invalidRange'],
  ['scalar out of bounds',{encoding:'unicodeScalar'},f=>{f.anchor.range=span(1,1000,'unicodeScalar');},'JOIN.TUPLE','anchor.range','invalidRange'],
  ['scalar reversed',{encoding:'unicodeScalar'},f=>{f.anchor.range=span(22,20,'unicodeScalar');},'JOIN.TUPLE','anchor.range','invalidRange']
 ];
 for(const [name,options,raw,assertion,field,code] of rawCases)await control(t,{name,options,raw,assertion,field,code});
});

test('reference site, roles and measured identity require independently admitted source',async t=>{
 const declaration=await specimen(t,{declarationSite:true});
 assert.equal(check(declaration).expectedReferences[0].site,'declaration');
 for(const [name,raw,assertion,field,options] of [
  ['empty roles',f=>{f.record.roles=[];},'REFERENCE.ROLES','roles',{}],
  ['false declaration',f=>{f.record.site='declaration';f.record.roles=['definition'];},'REFERENCE.ROLES','site',{}],
  ['alias lacks definition',f=>{f.record.roles=['alias'];},'REFERENCE.ROLES','site',{}],
  ['misordered roles',f=>{f.record.roles=['call','read'];},'REFERENCE.ROLES','roles',{}],
  ['false callback call',f=>{f.anchor.range=span(28,36);f.record.roles=['call'];},'REFERENCE.ROLES','roles',{}],
  ['bad resolution cardinality',f=>{f.record.resolution='ambiguous';},'REFERENCE.RESOLUTION','resolution',{}],
  ['bad target',f=>{f.record.declaredTarget.revisionId='r2';},'REFERENCE.RESOLUTION','declaredTarget',{}]
 ]){
  await control(t,{name,options,raw,assertion,field});
 }
 for(const field of ['id','ownerSyntaxId','ordinal','range','spelling','lookupKey','provenanceId']){
  const value={id:callbackId,ownerSyntaxId:targetId,ordinal:1,range:{start:28,end:36},spelling:'callback',lookupKey:'callback',provenanceId:'other'}[field];
  await control(t,{name:`measured reference ${field}`,normalized:records=>{records.references[0][field]=value;},assertion:'REFERENCE.SOURCE',field:'references'});
 }
});

test('valid external, ambiguous semantic resolution, unresolved and target cardinality',async t=>{
 const external={kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}};
 for(const resolution of ['resolved','external','ambiguous','unresolved']){
  const options={mutateFact:f=>{f.record.resolution=resolution;f.record.declaredTarget=resolution==='resolved'?declaredTarget:resolution==='external'?external:null;f.record.candidates=resolution==='ambiguous'?[declaredTarget,external]:[];}};
  const s=await specimen(t,options),result=check(s);
  assert.equal(result.expectedReferences[0].resolution,resolution);
  assert.equal(result.expectedReferences[0].candidates.length,resolution==='ambiguous'?2:0);
 }
 const ambiguous={mutateFact:f=>{f.record.resolution='ambiguous';f.record.declaredTarget=null;f.record.candidates=[declaredTarget,external];}};
 await control(t,{name:'duplicate targets',options:ambiguous,raw:f=>{f.record.candidates=[declaredTarget,declaredTarget];},assertion:'REFERENCE.RESOLUTION',field:'candidates'});
 await control(t,{name:'unsorted targets',options:ambiguous,raw:f=>{f.record.candidates=[external,declaredTarget];},assertion:'REFERENCE.RESOLUTION',field:'candidates'});
 await control(t,{name:'wrong target',options:ambiguous,raw:f=>{f.record.candidates[0]={...declaredTarget,declarationRef:'missing'};},assertion:'REFERENCE.RESOLUTION',field:'declaredTarget'});
});

test('identical repeated facts keep distinct proof contributors; conflicting facts reject',async t=>{
 const facts=[makeFact('reference','exact'),makeFact('reference','exact','fact2')];
 const s=await specimen(t,{facts}),result=check(s);
 assert.equal(result.joined.size,2);assert.equal(result.expectedReferences.length,2);
 assert.deepEqual(result.joined.get('fact2').provenanceIds,['proof-fact2']);
 const external={kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}};
 for(const [name,mutate,options] of [
  ['roles',f=>{f.record.roles=['read'];},{}],
  ['site',f=>{f.record.site='use';f.record.roles=['read'];},{declarationSite:true}],
  ['resolution',f=>{f.record.resolution='unresolved';f.record.declaredTarget=null;},{}],
  ['target',f=>{f.record.resolution='external';f.record.declaredTarget=external;},{}]
 ])await control(t,{name:`conflicting ${name}`,options:{facts,...options},raw:f=>{if(f.ref==='fact2')mutate(f);},assertion:'REFERENCE.SOURCE',field:'references'});
});

test('diagnostics derive candidate and support state, never install unmatched reference',async t=>{
 for(const status of ['unmatched','unsupported']){
  for(const [name,normalized,assertion,field] of [
   ['missing',r=>{r.referenceJoinDiagnostics=[];},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
   ['forged ambiguous',r=>{r.referenceJoinDiagnostics[0].join.status='ambiguous';r.referenceJoinDiagnostics[0].join.diagnostic='ambiguous';},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
   ['candidate',r=>{r.referenceJoinDiagnostics[0].join.candidateIds=[refId];},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
   ['status swap',r=>{r.referenceJoinDiagnostics[0].join.status=status==='unmatched'?'unsupported':'unmatched';r.referenceJoinDiagnostics[0].join.diagnostic=status==='unmatched'?'native family unavailable':'unmatched';},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
   ['extra',r=>{r.referenceJoinDiagnostics.push({...r.referenceJoinDiagnostics[0],factRef:'extra'});r.referenceJoinDiagnostics.sort((a,b)=>Buffer.compare(Buffer.from(a.factRef),Buffer.from(b.factRef)));},'JOIN.DIAGNOSTIC','referenceJoinDiagnostics'],
   ['install',r=>{r.references.push({id:refId,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',range:range(18,24),spelling:'target',lookupKey:'target',site:'use',roles:['read'],resolution:'unresolved',declaredTarget:null,candidates:[],provenanceId:'proof'});},'REFERENCE.SOURCE','references']
  ])await control(t,{name:`${status} ${name}`,options:{status},normalized,assertion,field});
 }
});

test('captured-but-unselected semantic producer cannot use valid proof, including unmatched joins',async t=>{
 const selected=await specimen(t);assert.equal(check(selected).joined.get('fact').installedId,refId);
 for(const status of ['exact','unmatched']){
  const s=await specimen(t,{unselectedProducer:true,status});
  assert.equal(s.loaded.semanticProofs.has('proof-b'),true);
  assert.equal(s.coverageResult.semanticProofsById.has('proof-b'),true);
  assert.equal(s.coverageResult.checkUse({producerId:'semantic',document,revisionId:'r1',provenanceIds:['proof']}).coverage.selected,true);
  assert.throws(()=>check(s),error=>{assert.equal(error.assertion,'FRESHNESS.USE');assert.equal(error.code,'invalidRecord');assert.equal(error.field,'coverage');return true;});
 }
});
