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
import {checkCounts,assertCorpusFloors} from '../counts.mjs';
import {normalizeFixture} from '../normalize.mjs';
import {registerControls,runControl} from './mutations.mjs';

// Independently authored source offsets, digest inputs and expected normalized rows.
const source='function main() { target(); callback; }\nfunction target() {}\n';
const alternateSource=kind=>`${source}// admitted ${kind} snapshot\n`;
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
async function specimen(t,{family='reference',status='exact',facts=null,unsupportedFamily=family,encoding='utf8',emojiPrefix=false,control=false,declarationSite=false,callbackSite=false,twoProducers=false,selectSecond=false,mutateFact=null,mutateNative=null,mutateSupport=null,alternateTuples=false,normalized=false,sourceOverride=null,mutateSecond=null,secondEncoding=null}={}) {
 const parent=await mkdtemp(join(tmpdir(),'count-u5-')),root=join(parent,'example');await mkdir(root);t.after(()=>rm(parent,{recursive:true,force:true}));
 const fs=new Map(),put=(path,value)=>fs.set(path,typeof value==='string'?value:JSON.stringify(value));
 const shifted=emojiPrefix||encoding!=='utf8',offset=shifted?5:0,positionOffset=encoding==='utf16'?3:encoding==='unicodeScalar'?2:offset;
 const sourceText=shifted?'😀\n'+(sourceOverride??source):(sourceOverride??source),sourceHash=sha(sourceText);
 const alternateText=kind=>`${sourceText}// admitted ${kind} snapshot\n`;
 const measuredNative=structuredClone(native);
 if(shifted)for(const row of [...measuredNative.declarations,...measuredNative.calls,...measuredNative.references]){
  for(const key of ['range','nameRange','calleeRange'])if(row[key]){row[key].start+=offset;row[key].end+=offset;}
  for(const entry of row.witnesses){entry.witness.range.start+=offset;entry.witness.range.end+=offset;}
 }
 if(control){measuredNative.controls.push({ref:'region',nativeId:null,document,revisionId:'r1',ownerRef:'main',parentRef:null,kind:'if',range:span(18+offset,26+offset),arm:null,witnesses:[]});measuredNative.calls[0].regionRefs=['region'];}
 if(declarationSite)measuredNative.references.push({ref:'declaration-ref',nativeId:null,document,revisionId:'r1',ownerRef:'target',range:span(49+offset,55+offset),spelling:'target',witnesses:[witness('spelling',49+offset,55+offset,'target')]});
 mutateNative?.(measuredNative);
 put('snapshots/src/main.js',sourceText);put('captures/native.json',measuredNative);
 for(const [name,value] of [['native','native-executable'],['semantic','semantic-executable'],['toolchain','toolchain'],['config','config'],['dependency','dependency']])put(`captures/${name}.txt`,value);
 const authored=structuredClone(facts??[makeFact(family,status)]);
 for(const fact of authored){fact.anchor.contentHash=sourceHash;if(shifted){fact.anchor.range.start+=positionOffset;fact.anchor.range.end+=positionOffset;fact.anchor.range.encoding=encoding;}if(callbackSite&&fact.kind==='reference'){fact.anchor.range=span(28+positionOffset,36+positionOffset,encoding);fact.record.roles=['read'];}if(declarationSite&&fact.kind==='reference'&&fact.anchor.range.start===18+positionOffset){fact.anchor.range=span(49+positionOffset,55+positionOffset,encoding);fact.anchor.ownerRef='target';fact.record.site='declaration';fact.record.roles=['definition'];}mutateFact?.(fact);}
 for(let i=1;i<authored.length;i++)authored[i].record.provenanceId=`proof-${authored[i].ref}`;
 if(twoProducers){const other=structuredClone(authored[0]);other.ref='fact-b';other.record.provenanceId='proof-b';mutateSecond?.(other);authored.push(other);}
 put('captures/semantic.json',{formatVersion:1,producerId:'semantic',facts:authored.filter(f=>f.ref!=='fact-b')});
 if(twoProducers){put('captures/semantic-b.txt','semantic-b-executable');put('captures/semantic-b.json',{formatVersion:1,producerId:'semantic-b',facts:authored.filter(f=>f.ref==='fact-b')});}
 const captures=[['native','executable','captures/native.txt'],['semantic','executable','captures/semantic.txt'],['toolchain','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['dependency','dependency','captures/dependency.txt'],['artifact','semanticArtifact','captures/semantic.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(fs.get(file))}));
 if(twoProducers)captures.push(...[['semantic-b-executable','executable','captures/semantic-b.txt'],['semantic-b-artifact','semanticArtifact','captures/semantic-b.json']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(fs.get(file))})));
 const revision={id:'r1',sourceSetId:'main',documents:[{key:document,revisionId:'r1',sourceFile:'snapshots/src/main.js'}],toolchainHash:captures[2].hash,configHash:captures[3].hash,dependencyHash:captures[4].hash};
 const basis={producerId:'semantic',producerVersion:'1',producerHash:semanticProducer.executableHash,artifactHash:captures[5].hash,language:'javascript',sourceSetId:'main',revisionId:'r1',sourceManifestHash:sha(canonical(alternateTuples?[{document,contentHash:sourceHash},{document:{...document,path:'src/other.js'},contentHash:sha(alternateText('document'))}]:[{document,contentHash:sourceHash}])),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash,lookupDependencies:[]};
 const proof={id:'proof',producerId:'semantic',document,revisionId:'r1',contentHash:sourceHash,evidenceKind:family==='declarationName'?'declarationBinding':'semanticReference',basis,freshness:'fresh'};
 const proofs=authored.map(fact=>fact.ref==='fact-b'?{...proof,id:'proof-b',producerId:'semantic-b',basis:{...basis,producerId:'semantic-b',producerHash:sha('semantic-b-executable'),artifactHash:sha(fs.get('captures/semantic-b.json'))},freshness:'possiblyStale'}:{...proof,id:fact.record.provenanceId});
 const proofFacts=proofs.map((record,i)=>({kind:'provenance',ref:`proof-fact-${i}`,record}));
 const support=['declarationName','callee','invocation','reference'].map(kind=>({kind,available:!(status==='unsupported'&&kind===unsupportedFamily),diagnostic:status==='unsupported'&&kind===unsupportedFamily?'native family unavailable':null}));
 mutateSupport?.(support);
 const coverage=(producerId)=>({producerId,language:document.language,sourceSetId:document.sourceSetId,documentPath:document.path,revisionId:'r1',requested:true,selected:producerId!=='semantic-b'||selectSecond,state:producerId==='semantic-b'?(selectSecond?'partial':'omitted'):status==='unsupported'&&unsupportedFamily==='reference'?'partial':'complete',supportedRoles:['read'],observedRoles:producerId==='semantic-b'||status==='unsupported'&&unsupportedFamily==='reference'?[]:['read'],diagnostic:producerId==='semantic-b'?(selectSecond?'reference not observed':'producer not selected'):status==='unsupported'&&unsupportedFamily==='reference'?'reference unavailable':null});
 put('snapshots/src/main.js.annotations.json',{formatVersion:1,document,revisionId:'r1',scenarios:[],facts:[...(twoProducers?['native','semantic','semantic-b']:['native','semantic']).map(id=>({kind:'coverage',ref:`coverage-${id}`,record:coverage(id)})),...proofFacts,...authored]});
 const selectedSemantic={...semanticProducer,positionEncoding:encoding};
 const unselectedSemantic={...semanticProducer,id:'semantic-b',executableHash:sha('semantic-b-executable'),positionEncoding:secondEncoding??encoding};
 const producers=twoProducers?[nativeProducer,selectedSemantic,unselectedSemantic]:[nativeProducer,selectedSemantic];
 const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers,revisions:[revision],comparison:{sourceSetId:'main',revisionId:'r1',producers:[nativeProducer,selectedSemantic]},coverageIntents:producers.map(producer=>({producerId:producer.id,document,revisionId:'r1',requestedRoles:['read'],measurementSupport:support})),nativeArtifact:'captures/native.json',semanticArtifacts:twoProducers?['captures/semantic.json','captures/semantic-b.json']:['captures/semantic.json'],annotationFiles:['snapshots/src/main.js.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 if(alternateTuples){
  const alternatives=[
   {document:{...document,path:'src/other.js'},revisionId:'r1',file:'snapshots/src/other.js',kind:'document'},
   {document,revisionId:'r2',file:'snapshots/r2/src/main.js',kind:'revision'},
   {document:{...document,sourceSetId:'alternate'},revisionId:'r1',file:'snapshots/alternate/src/main.js',kind:'source set'}
  ];
  fixture.revisions[0].documents.push({key:alternatives[0].document,revisionId:'r1',sourceFile:alternatives[0].file});
  fixture.revisions.push({...revision,id:'r2',documents:[{key:document,revisionId:'r2',sourceFile:alternatives[1].file}]});
  fixture.revisions.push({...revision,sourceSetId:'alternate',documents:[{key:alternatives[2].document,revisionId:'r1',sourceFile:alternatives[2].file}]});
  fixture.sourceSets.push({id:'alternate',rootId:'alternate-root',languages:['javascript'],dependencies:[]});
  for(const alt of alternatives){
   put(alt.file,alternateText(alt.kind));
   const rows=producers.map(producer=>({kind:'coverage',ref:`coverage-${producer.id}-${alt.revisionId}-${alt.document.path}`,record:{...coverage(producer.id),sourceSetId:alt.document.sourceSetId,documentPath:alt.document.path,revisionId:alt.revisionId}}));
   const annotation=`${alt.file}.annotations.json`;
   put(annotation,{formatVersion:1,document:alt.document,revisionId:alt.revisionId,scenarios:[],facts:rows});
   fixture.annotationFiles.push(annotation);
   for(const producer of producers)fixture.coverageIntents.push({producerId:producer.id,document:alt.document,revisionId:alt.revisionId,requestedRoles:['read'],measurementSupport:structuredClone(support)});
  }
  // The captured semantic proof's manifest includes both documents of r1/main.
 }
 put('fixture.json',fixture);put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 for(const [path,value] of fs){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}
 const loaded=await loadFixture(root),records=absent();records.comparison=fixture.comparison;records.producers=ordered(structuredClone(fixture.producers));records.sourceSets=ordered(structuredClone(fixture.sourceSets));
 records.revisions=ordered(fixture.revisions.map(rev=>({...rev,documents:rev.documents.map(item=>({key:item.key,revisionId:item.revisionId,contentHash:sha(fs.get(item.sourceFile)),byteLength:Buffer.byteLength(fs.get(item.sourceFile))}))})));records.provenance=proofs;
 records.coverage=ordered(loaded.annotations.flatMap(annotation=>annotation.facts.filter(f=>f.kind==='coverage').map(f=>f.record)));
 const asDeclaration=(name,begin,end,nameStart)=>({syntaxId:syntax(name),document,revisionId:'r1',kind:'function',name,lookupKey:name,ancestors:[],key:{kind:'function',name,signature:null,ordinal:0},range:range(begin,end),nameRange:range(nameStart,nameStart+name.length),header:header(name),provenanceId:`native:r1:${syntax(name)}`});
 records.declarations=[asDeclaration('main',0+offset,39+offset,9+offset),asDeclaration('target',40+offset,60+offset,49+offset)].sort((a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId)));
 records.calls=[{id:callId,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',range:range(18+offset,26+offset),calleeRange:range(18+offset,24+offset),spelling:'target',regionIds:control?[occurrence(mainId,'control',0)]:[],provenanceId:`native:r1:${callId}`}];
 if(control){const id=occurrence(mainId,'control',0);records.controlRegions=[{id,ownerSyntaxId:mainId,ordinal:0,document,revisionId:'r1',kind:'if',range:range(18+offset,26+offset),parentId:null,arm:null,provenanceId:`native:r1:${id}`}];}
 if(normalized){const normalizedRecords=normalizeFixture(loaded).records;for(const name of ['producers','sourceSets','revisions'])normalizedRecords[name]=ordered(normalizedRecords[name]);
  return {loaded,records:normalizedRecords,authored};}
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

test('source-backed counts: exact read and measured call',async t=>{
 const s=await specimen(t);
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.measuredCalls,1);
 assert.equal(actual.references,1);
 assert.deepEqual(actual.observedRoles,['read','call']);
});

const result=(loaded,records)=>checkCounts(loaded,records);
const dispositionKind={resolved:'resolved',external:'provenExternal',ambiguous:'ambiguous',unresolved:'unresolved'};
for(const [resolution,disposition] of Object.entries(dispositionKind))test(`captured ${disposition} disposition`,async t=>{
 const specimenResult=await specimen(t,{mutateFact:fact=>{
  fact.record.resolution=resolution;
  fact.record.declaredTarget=resolution==='resolved'?declaredTarget:resolution==='external'?{kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}}:null;
  fact.record.candidates=resolution==='ambiguous'?[declaredTarget,{kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}}]:[];
 }});
 specimenResult.loaded.dispositions.assertions.push({kind:'resolution',factRef:'fact',disposition});
 assert.equal(result(specimenResult.loaded,specimenResult.records).outcomes.find(x=>x.disposition===disposition).count,1);
});
test('unsupported is checked against native source capability',async t=>{
 const s=await specimen(t,{status:'unsupported',family:'reference',mutateFact:fact=>{fact.anchor.range=span(0,8);}});
 s.loaded.dispositions.assertions.push({kind:'join',factRef:'fact',disposition:'unsupported'});
 assert.equal(result(s.loaded,s.records).outcomes.find(x=>x.disposition==='unsupported').count,1);
});
test('callable reference outside callee counts as negative, not a second call',async t=>{
 const s=await specimen(t,{callbackSite:true});
 const anchor=s.authored[0].anchor;
 s.loaded.annotations[0].scenarios.push({id:'callback',category:'callableValues',anchors:[anchor],factRefs:['fact']});
 s.loaded.dispositions.callableValueNegatives.push({scenarioId:'callback',referenceRef:'fact',ownerRef:'main',range:anchor.range});
 const value=result(s.loaded,s.records);
 assert.equal(value.references,1);assert.equal(value.callableValueNegatives,1);assert.equal(value.measuredCalls,1);
});
test('unsupported source capability rejects an authored label',async t=>{
 const s=await specimen(t);
 s.loaded.dispositions.assertions.push({kind:'join',factRef:'fact',disposition:'unsupported'});
 assert.throws(()=>result(s.loaded,s.records),e=>e.assertion==='COUNT.DISPOSITION'&&e.code==='invalidRecord'&&e.field==='assertions');
});
test('corpus profile enforces all floors, not only totals',async t=>{
 const s=await specimen(t);s.loaded.fixture.profile='corpus';
 assert.throws(()=>result(s.loaded,s.records),e=>e.assertion==='COUNT.FLOOR'&&e.code==='invalidRecord'&&e.field==='counts');
});

test('exact mutation controls on source-backed native membership and captured dispositions',async t=>{
 const sample=await specimen(t);
 const disposition=await specimen(t);disposition.loaded.dispositions.assertions.push({kind:'resolution',factRef:'fact',disposition:'resolved'});
 const controls=registerControls([
  {id:'COUNT.calls.native-membership',baseline:()=>sample.records,mutate:records=>{
   records.calls[0].id=`occ:v1:${'a'.repeat(32)}`;return records;
  },check:records=>result(sample.loaded,records),expectedAssertion:'COUNT.CALLS',expectedCode:'invalidRecord',expectedField:'measuredCalls'},
  {id:'COUNT.disposition.source-proof',baseline:()=>disposition.loaded.dispositions,mutate:rows=>{
   rows.assertions[0].disposition='unresolved';return rows;
  },check:rows=>checkCounts(disposition.loaded,disposition.records,rows),
  expectedAssertion:'COUNT.DISPOSITION',expectedCode:'invalidRecord',expectedField:'assertions'},
  {id:'COUNT.reference.native-id',baseline:()=>sample.records,mutate:records=>{
   records.references[0].id=`occ:v1:${'b'.repeat(32)}`;return records;
  },check:records=>result(sample.loaded,records),expectedAssertion:'REFERENCE.SOURCE',expectedCode:'invalidRecord',expectedField:'references'}
 ]);
 for(const row of controls)await runControl(row);
});

test('normalizer fixture call candidate',async t=>{
 const s=await specimen(t,{family:'callee'});
 const normal=normalizeFixture(s.loaded);
 for(const key of ['producers','sourceSets','revisions'])normal.records[key]=ordered(normal.records[key]);
 const actual=checkCounts(s.loaded,normal.records);
 assert.equal(actual.measuredCalls,1);
});

test('category-specific source witnesses and unique scenario anchors',async t=>{
 const sample=await specimen(t,{callbackSite:true});
 const annotation=sample.loaded.annotations[0];
 const one={id:'callback',category:'callableValues',anchors:[sample.authored[0].anchor],factRefs:['fact']};
 annotation.scenarios.push(one);
 assert.equal(checkCounts(sample.loaded,sample.records).scenariosByCategory.find(x=>x.category==='callableValues').count,1);
 const rows=registerControls([
  {id:'COUNT.scenario.category-relabel',baseline:()=>sample.loaded.annotations,mutate:annotations=>{
   annotations[0].scenarios[0].category='recursion';return annotations;
  },check:annotations=>checkCounts({...sample.loaded,annotations},sample.records),
  expectedAssertion:'COUNT.SCENARIO',expectedCode:'invalidRecord',expectedField:'category'},
  {id:'COUNT.scenario.anchor-copy',baseline:()=>sample.loaded.annotations,mutate:annotations=>{
   annotations[0].scenarios.push({...annotations[0].scenarios[0],id:'copied'});return annotations;
  },check:annotations=>checkCounts({...sample.loaded,annotations},sample.records),
  expectedAssertion:'COUNT.SCENARIO',expectedCode:'invalidRecord',expectedField:'anchors'},
  {id:'COUNT.scenario.unconnected-fact',baseline:()=>sample.loaded.annotations,mutate:annotations=>{
   annotations[0].scenarios[0].factRefs=['coverage-native'];return annotations;
  },check:annotations=>checkCounts({...sample.loaded,annotations},sample.records),
  expectedAssertion:'COUNT.SCENARIO',expectedCode:'invalidRecord',expectedField:'factRefs'}
 ]);
 for(const row of rows)await runControl(row);
});
test('captured import usage supports only importsAliases, not callableValues',async t=>{
 const s=await specimen(t,{mutateFact:fact=>{fact.record.roles=['import'];}});
 const anchor=s.authored[0].anchor;
 s.loaded.annotations[0].scenarios.push({id:'import',category:'importsAliases',anchors:[anchor],factRefs:['fact']});
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.scenariosByCategory.find(x=>x.category==='importsAliases').count,1);
 assert.deepEqual(actual.observedRoles,['import']);
});

test('normalized source-backed recursive call has one call and one anchored recursion scenario',async t=>{
 const s=await specimen(t,{family:'callee',normalized:true,mutateFact:fact=>{
  fact.record.declaredTarget={kind:'internal',declarationRef:'main',revisionId:'r1'};
 }});
 s.loaded.annotations[0].scenarios.push({id:'self',category:'recursion',anchors:[s.authored[0].anchor],factRefs:['fact']});
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.scenariosByCategory.find(x=>x.category==='recursion').count,1);
 assert.equal(actual.measuredCalls,1);
});

test('nested declaration-name anchors use the measured parent, not the focused declaration',async t=>{
 const s=await specimen(t,{normalized:true,mutateNative:native=>{
  native.declarations[0].range.end=60;
  native.declarations[1].parentRef='main';
 }});
 const anchor={...s.authored[0].anchor,kind:'declarationName',range:span(49,55),ownerRef:'main'};
 s.loaded.annotations[0].scenarios.push({id:'nested',category:'compatibilityControl',
  anchors:[s.authored[0].anchor,anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosTotal,1);
 const copy=structuredClone(s.loaded.annotations);copy[0].scenarios[0].anchors[1].ownerRef='target';
 assert.throws(()=>checkCounts({...s.loaded,annotations:copy},s.records),e=>
  e.assertion==='COUNT.SCENARIO'&&e.code==='invalidRecord'&&e.field==='anchors');
});

test('member-name reference inside a larger measured callee cannot be a callable-value negative',async t=>{
 const s=await specimen(t,{normalized:true,mutateNative:native=>{
  native.calls[0].range.start=17;native.calls[0].calleeRange.start=17;
  native.calls[0].spelling=' target';
  native.calls[0].witnesses[0].witness={range:span(17,24),text:' target'};
 },mutateFact:fact=>{fact.record.roles=['read'];}});
 s.loaded.annotations[0].scenarios.push({id:'callee-member',category:'callableValues',anchors:[s.authored[0].anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosTotal,1);
 s.loaded.dispositions.callableValueNegatives.push({scenarioId:'callee-member',referenceRef:'fact',ownerRef:'main',range:s.authored[0].anchor.range});
 assert.throws(()=>checkCounts(s.loaded,s.records),e=>e.assertion==='COUNT.NEGATIVE'&&e.code==='invalidRecord'&&e.field==='callableValueNegatives');
});

test('UTF-16 callable negative converts to UTF-8 source before comparison',async t=>{
 const s=await specimen(t,{callbackSite:true,encoding:'utf16',emojiPrefix:true});
 const anchor=s.authored[0].anchor;
 s.loaded.annotations[0].scenarios.push({id:'unicode-read',category:'callableValues',anchors:[anchor],factRefs:['fact']});
 s.loaded.dispositions.callableValueNegatives.push({scenarioId:'unicode-read',referenceRef:'fact',ownerRef:'main',range:anchor.range});
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.callableValueNegatives,1);
 assert.equal(actual.references,1);
});

test('two independently captured producers cannot inflate one measured reference',async t=>{
 const s=await specimen(t,{twoProducers:true,selectSecond:true});
 s.loaded.fixture.comparison.producers.push(s.loaded.fixture.producers.find(p=>p.id==='semantic-b'));
 s.records.comparison=structuredClone(s.loaded.fixture.comparison);
 s.records.provenance.find(p=>p.id==='proof-b').freshness='fresh';
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.references,1);
});

test('same-name overload category requires two independently measured sibling names',async t=>{
 const renamed=source.replace('function target()','function main  ()');
 const s=await specimen(t,{family:'declarationName',normalized:true,sourceOverride:renamed,mutateNative:native=>{
  const row=native.declarations[1];row.name='main';row.nameRange.end=53;row.header.name='main';
  for(const witness of row.witnesses){witness.witness.text='main';witness.witness.range.end=53;}
 },mutateFact:fact=>{fact.anchor.range.end=53;}});
 const other={...s.authored[0].anchor,range:span(9,13),ownerRef:'main'};
 s.loaded.annotations[0].scenarios.push({id:'overload',category:'sameNameOverload',anchors:[other,s.authored[0].anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosByCategory.find(x=>x.category==='sameNameOverload').count,1);
});

test('unicode coordinates require source bytes; ASCII anchors cannot be relabelled',async t=>{
 const s=await specimen(t,{encoding:'utf16',emojiPrefix:true});
 s.loaded.annotations[0].scenarios.push({id:'unicode',category:'unicodeCoordinates',anchors:[s.authored[0].anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosByCategory.find(x=>x.category==='unicodeCoordinates').count,1);
 const ascii=await specimen(t);
 ascii.loaded.annotations[0].scenarios.push({id:'fake-unicode',category:'unicodeCoordinates',anchors:[ascii.authored[0].anchor],factRefs:['fact']});
 assert.throws(()=>checkCounts(ascii.loaded,ascii.records),e=>e.assertion==='COUNT.SCENARIO'&&e.field==='category');
});
test('coverageFreshness requires an authenticated partial tuple, not a relabelled scenario',async t=>{
 const s=await specimen(t);
 const intent=s.loaded.fixture.coverageIntents.find(x=>x.producerId==='semantic');
 intent.requestedRoles=['read','write'];
 const row=s.loaded.annotations[0].facts.find(x=>x.kind==='coverage'&&x.record.producerId==='semantic').record;
 row.state='partial';row.diagnostic='write not observed';
 const stored=s.records.coverage.find(x=>x.producerId==='semantic');
 stored.state='partial';stored.diagnostic='write not observed';
 s.loaded.annotations[0].scenarios.push({id:'partial',category:'coverageFreshness',anchors:[s.authored[0].anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosByCategory.find(x=>x.category==='coverageFreshness').count,1);
});

test('relationshipsDispatch requires a captured dispatch fact, not a reference name',async t=>{
 const s=await specimen(t,{family:'callee',normalized:true});
 s.loaded.annotations[0].scenarios.push({id:'dispatch',category:'relationshipsDispatch',anchors:[s.authored[0].anchor],factRefs:['fact']});
 assert.equal(checkCounts(s.loaded,s.records).scenariosByCategory.find(x=>x.category==='relationshipsDispatch').count,1);
 const ref=await specimen(t);
 ref.loaded.annotations[0].scenarios.push({id:'fake-dispatch',category:'relationshipsDispatch',anchors:[ref.authored[0].anchor],factRefs:['fact']});
 assert.throws(()=>checkCounts(ref.loaded,ref.records),e=>e.assertion==='COUNT.SCENARIO'&&e.field==='category');
});

for(const role of ['write','type'])test(`source-backed ${role} reference role`,async t=>{
 const s=await specimen(t,{mutateFact:fact=>{fact.record.roles=[role];}});
 assert.deepEqual(checkCounts(s.loaded,s.records).observedRoles,[role]);
});
test('measured declaration site supports definition and alias roles once per reference',async t=>{
 const s=await specimen(t,{declarationSite:true,mutateFact:fact=>{
  fact.record.site='declaration';fact.record.roles=['definition','alias'];
 }});
 const actual=checkCounts(s.loaded,s.records);
 assert.equal(actual.references,1);
 assert.deepEqual(actual.observedRoles,['definition','alias']);
});

test('each independent corpus minimum and each of eight category and five outcome floors',async()=>{
 const full={formatVersion:1,language:'javascript',profile:'corpus',floorsEnforced:true,scenariosTotal:32,
  scenariosByCategory:['sameNameOverload','importsAliases','callableValues','recursion','relationshipsDispatch','unicodeCoordinates','coverageFreshness','compatibilityControl'].map(category=>({category,count:4})),
  measuredCalls:120,references:40,callableValueNegatives:20,typeRelationships:20,
  outcomes:['resolved','provenExternal','ambiguous','unresolved','unsupported'].map(disposition=>({disposition,count:20})),
  observedRoles:['definition','read','write','call','type','import','alias']};
 assert.equal(assertCorpusFloors(full),full);
 const mutations=[...['scenariosTotal','measuredCalls','references','callableValueNegatives','typeRelationships'].map(field=>row=>{row[field]--;}),
  ...full.scenariosByCategory.map((_,i)=>row=>{row.scenariosByCategory[i].count--;}),
  ...full.outcomes.map((_,i)=>row=>{row.outcomes[i].count--;}),
  ...full.observedRoles.map(role=>row=>{row.observedRoles=row.observedRoles.filter(value=>value!==role);})];
 const controls=registerControls(mutations.map((mutate,i)=>({id:`COUNT.floor.boundary.${i}`,
  baseline:()=>full,mutate:row=>{mutate(row);return row;},check:assertCorpusFloors,
  expectedAssertion:'COUNT.FLOOR',expectedCode:'invalidRecord',expectedField:'counts'})));
 for(const row of controls)await runControl(row);
 const java={...full,language:'java',observedRoles:full.observedRoles.filter(role=>role!=='alias')};
 assert.equal(assertCorpusFloors(java),java);
});


test('captured proof cannot be relabelled to an unrelated fact',async t=>{
 const sample=await specimen(t);
 const rows=registerControls([{id:'COUNT.proof.capture-link',baseline:()=>sample.loaded.semanticProofs,
  mutate:proofs=>{proofs.get('proof').factRef='unrelated';return proofs;},
  check:proofs=>checkCounts({...sample.loaded,semanticProofs:proofs},sample.records),
  expectedAssertion:'COUNT.PROOF',expectedCode:'invalidRecord',expectedField:'provenanceId'}]);
 for(const row of rows)await runControl(row);
});

test('two captured semantic producers cannot inflate one unsupported source span',async t=>{
 const s=await specimen(t,{status:'unsupported',family:'reference',twoProducers:true,selectSecond:true,
  mutateFact:fact=>{fact.anchor.range=span(0,8);}});
 s.loaded.fixture.comparison.producers.push(s.loaded.fixture.producers.find(p=>p.id==='semantic-b'));
 s.records.comparison=structuredClone(s.loaded.fixture.comparison);
 s.records.provenance.find(p=>p.id==='proof-b').freshness='fresh';
 s.loaded.dispositions.assertions.push({kind:'join',factRef:'fact',disposition:'unsupported'},
  {kind:'join',factRef:'fact-b',disposition:'unsupported'});
 assert.equal(checkCounts(s.loaded,s.records).outcomes.find(x=>x.disposition==='unsupported').count,1);
});

test('unsupported copies in UTF-16 and scalar encodings share one source-byte span',async t=>{
 const s=await specimen(t,{status:'unsupported',family:'reference',encoding:'utf16',emojiPrefix:true,
  twoProducers:true,selectSecond:true,normalized:true,secondEncoding:'unicodeScalar',
  mutateFact:fact=>{fact.anchor.range=span(3,11,'utf16');},
  mutateSecond:fact=>{fact.anchor.range=span(2,10,'unicodeScalar');}});
 s.loaded.fixture.comparison.producers.push(s.loaded.fixture.producers.find(p=>p.id==='semantic-b'));
 s.records.comparison=structuredClone(s.loaded.fixture.comparison);
 s.records.provenance.find(p=>p.id==='proof-b').freshness='fresh';
 s.loaded.dispositions.assertions.push({kind:'join',factRef:'fact',disposition:'unsupported'},
  {kind:'join',factRef:'fact-b',disposition:'unsupported'});
 assert.equal(checkCounts(s.loaded,s.records).outcomes.find(x=>x.disposition==='unsupported').count,1);
});
