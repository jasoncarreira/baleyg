import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {mkdtemp,mkdir,writeFile,rm} from 'node:fs/promises';
import {dirname,join} from 'node:path';
import {tmpdir} from 'node:os';
import {loadFixture} from '../load.mjs';
import {checkCoverage} from '../record-check/coverage.mjs';
import {checkMeasurement,orderedEnvelope} from '../record-check/measurement.mjs';
import {checkJoins} from '../record-check/joins.mjs';
import {checkBindings} from '../record-check/bindings.mjs';
import {registerControls,runControl} from './mutations.mjs';

const source='function main() { target(); }\nfunction target() { return 1; }\nfunction other() { return 2; }\nfunction third() { return 3; }\n';
const document={sourceSetId:'main',language:'javascript',path:'src/main.js'};
const sha=value=>createHash('sha256').update(value).digest('hex');
const canon=value=>value===null||typeof value!=='object'?JSON.stringify(value):Array.isArray(value)?`[${value.map(canon).join(',')}]`:`{${Object.keys(value).sort((a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b))).map(x=>`${canon(x)}:${canon(value[x])}`).join(',')}}`;
const digest=(kind,value)=>createHash('sha256').update(`baleyg.${kind}.v1\0`).update(canon(value)).digest('hex').slice(0,32);
const order=rows=>rows.sort((a,b)=>Buffer.compare(Buffer.from(canon(a)),Buffer.from(canon(b))));
const span=(start,end)=>({start,end,encoding:'utf8'});
const range=(start,end)=>({start,end});
const witness=(field,start,end,text)=>({field,witness:{range:span(start,end),text}});
const names=['main','target','other','third'];
const syntax=name=>`sid:v1:${digest('syntax',{sourceSet:'main',path:document.path,language:'javascript',ancestors:[],declaration:{kind:'function',name,signature:null,ordinal:0}})}`;
const callId=rev=>`occ:v1:${digest('occurrence',{revisionId:rev,ownerSyntaxId:syntax('main'),kind:'call',ordinal:0})}`;
const internal=(name,rev='r1')=>({kind:'internal',syntaxId:syntax(name),document,revisionId:rev});
const external={kind:'external',symbol:{scheme:'scip',symbol:'pkg external',scope:'global',document:null}};
const header=name=>({kind:'function',name,modifiers:[],typeParameters:[],parameters:[],resultType:null,bases:[]});
const nativeProducer={id:'native',version:'1',executableHash:sha('native executable'),kind:'native',languages:['javascript'],positionEncoding:'utf8'};
const semanticProducer={id:'semantic',version:'1',executableHash:sha('semantic executable'),kind:'semantic',languages:['javascript'],positionEncoding:'utf8'};
function declarations(text,rev='r1') {
 return names.map(name=>{
  const start=text.indexOf(`function ${name}()`),end=text.indexOf('}',start)+1,begin=start+9;
  return {ref:`${rev}:${name}`,nativeId:null,document,revisionId:rev,parentRef:null,kind:'function',name,
   range:span(start,end),nameRange:span(begin,begin+name.length),header:header(name),signature:null,
   witnesses:[witness('name',begin,begin+name.length,name),witness('header.name',begin,begin+name.length,name)]};
 });
}
function call(text,rev='r1') {
 const start=text.indexOf('target();');
 return {ref:`${rev}:call`,nativeId:null,document,revisionId:rev,ownerRef:`${rev}:main`,range:span(start,start+8),
  calleeRange:span(start,start+6),spelling:'target',regionRefs:[],witnesses:[witness('spelling',start,start+6,'target')]};
}
const empty=()=>({formatVersion:1,comparison:null,producers:[],sourceSets:[],revisions:[],coverage:[],provenance:[],declarations:[],symbols:[],declarationBindings:[],typeRelationships:[],calls:[],controlRegions:[],references:[],referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]});
// Expected IDs, target identities and rows are authored from source bytes, not from any checker output.
async function specimen(t,{claims=['target'],dispatch='direct',resolution='resolved',status='exact',history=false,staleTarget=false,
 producers=false,possibleDispatch=[],externalSymbol=external.symbol,raw=null,rawAfterProof=null,output=null,captureMissing=false}={}) {
 const root=await mkdtemp(join(tmpdir(),'binding-u5-'));t.after(()=>rm(root,{recursive:true,force:true}));
 const files=new Map(),put=(path,value)=>files.set(path,typeof value==='string'?value:JSON.stringify(value));
 const r2text=staleTarget?source.replace('function target() { return 1; }','function target() { return 9; }'):source;
 const revisions=history?['r1','r2']:['r1'],texts={r1:source,r2:r2text};
 const producersList=producers?[nativeProducer,semanticProducer,{...semanticProducer,id:'other-semantic',executableHash:sha('other semantic executable')}]:[nativeProducer,semanticProducer];
 const allDeclarations=revisions.flatMap(rev=>declarations(texts[rev],rev)),allCalls=revisions.map(rev=>call(texts[rev],rev));
 put('captures/native.json',{formatVersion:1,producerId:'native',declarations:allDeclarations,calls:allCalls,controls:[],references:[]});
 for(const [name,value] of [['native','native executable'],['semantic','semantic executable'],...(producers?[['other-semantic','other semantic executable']]:[]),['toolchain','toolchain'],['config','config'],['dependency','dependency']])put(`captures/${name}.txt`,value);
 const nativeCaptures=[['native','executable','captures/native.txt'],['semantic','executable','captures/semantic.txt'],...(producers?[['other-semantic','executable','captures/other-semantic.txt']]:[]),['toolchain','toolchain','captures/toolchain.txt'],['config','config','captures/config.txt'],['dependency','dependency','captures/dependency.txt']].map(([ref,kind,file])=>({ref,kind,file,hash:sha(files.get(file))}));
 const artifactPaths=producers?['captures/semantic.json','captures/other-semantic.json']:['captures/semantic.json'];
 const facts=claims.map((name,i)=>{
  const producerId=producers&&i===claims.length-1?'other-semantic':'semantic';
  const rev='r1',text=texts[rev],begin=call(text,rev).range.start;
  const anchorRange=status==='exact'?span(begin,begin+6):span(text.indexOf('return')>=0?text.indexOf('return'):text.indexOf('main'),text.indexOf('return')>=0?text.indexOf('return')+6:text.indexOf('main')+4);
  return {producerId,fact:{kind:'callBinding',ref:`binding-${i}`,anchor:{document,revisionId:rev,contentHash:sha(text),kind:'callee',range:anchorRange,ownerRef:`${rev}:main`},record:{resolution,declaredTarget:resolution==='resolved'?{kind:'internal',declarationRef:`${rev}:${name}`,revisionId:rev}:resolution==='external'?{kind:'external',symbol:externalSymbol}:null,candidates:resolution==='ambiguous'?['target','other'].sort((a,b)=>Buffer.compare(Buffer.from(canon(internal(a))),Buffer.from(canon(internal(b))))).map(name=>({kind:'internal',declarationRef:`r1:${name}`,revisionId:'r1'})):[],dispatch,possibleDispatch:possibleDispatch.map(name=>({kind:'internal',declarationRef:`r1:${name}`,revisionId:'r1'})),possibleDispatchComplete:false,provenanceId:`proof-${i}`}}};
 });
 if(status!=='exact')for(const {fact} of facts){fact.anchor.range=span(0,8);fact.anchor.ownerRef='r1:main';}
 raw?.(facts);
 for(const id of producersList.filter(x=>x.kind==='semantic').map(x=>x.id))put(`captures/${id}.json`,{formatVersion:1,producerId:id,facts:facts.filter(x=>x.producerId===id).map(x=>x.fact)});
 const captures=[...nativeCaptures,...artifactPaths.map((file,i)=>({ref:`artifact-${i}`,kind:'semanticArtifact',file,hash:sha(files.get(file))}))];
 const cap=(ref)=>captures.find(x=>x.ref===ref).hash;
 const revision=rev=>({id:rev,sourceSetId:'main',documents:[{key:document,revisionId:rev,sourceFile:`snapshots/${rev}/main.js`}],toolchainHash:cap('toolchain'),configHash:cap('config'),dependencyHash:cap('dependency')});
 const basis=(producerId,rev)=>({producerId,producerVersion:'1',producerHash:producersList.find(x=>x.id===producerId).executableHash,artifactHash:captures.find(x=>x.file===`captures/${producerId}.json`).hash,language:'javascript',sourceSetId:'main',revisionId:rev,sourceManifestHash:sha(canon([{document,contentHash:sha(texts[rev])}])),toolchainHash:cap('toolchain'),configHash:cap('config'),dependencyHash:cap('dependency'),lookupDependencies:[]});
 const proofs=facts.map(({fact,producerId})=>({id:fact.record.provenanceId,producerId,document,revisionId:'r1',contentHash:sha(source),evidenceKind:'semanticReference',basis:basis(producerId,'r1'),freshness:history?(staleTarget?'stale':'possiblyStale'):'fresh'}));
 if(rawAfterProof) {
  rawAfterProof(facts);
  for(const id of producersList.filter(x=>x.kind==='semantic').map(x=>x.id)) {
   const file=`captures/${id}.json`;
   put(file,{formatVersion:1,producerId:id,facts:facts.filter(x=>x.producerId===id).map(x=>x.fact)});
   const artifactHash=sha(files.get(file));
   captures.find(x=>x.file===file).hash=artifactHash;
   for(const proof of proofs)if(proof.producerId===id)proof.basis.artifactHash=artifactHash;
  }
 }
 // A byte-identical r2 caller with a different revision is possiblyStale, not fresh, for old r1 evidence.
 const coverage=(producerId,rev)=>({producerId,language:'javascript',sourceSetId:'main',documentPath:document.path,revisionId:rev,requested:true,selected:true,state:'complete',supportedRoles:['read'],observedRoles:['read'],diagnostic:null});
 const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers:producersList,
  revisions:revisions.map(revision),comparison:{sourceSetId:'main',revisionId:revisions.at(-1),producers:producersList},
  coverageIntents:revisions.flatMap(rev=>producersList.map(producer=>({producerId:producer.id,document,revisionId:rev,requestedRoles:['read'],measurementSupport:['declarationName','callee','invocation','reference'].map(kind=>({kind,available:status!=='unsupported'||kind!=='callee',diagnostic:status==='unsupported'&&kind==='callee'?'unsupported':null}))}))),
  nativeArtifact:'captures/native.json',semanticArtifacts:artifactPaths,annotationFiles:revisions.map(rev=>`snapshots/${rev}/main.js.annotations.json`),answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
 for(const rev of revisions) {
  put(`snapshots/${rev}/main.js`,texts[rev]);
  put(`snapshots/${rev}/main.js.annotations.json`,{formatVersion:1,document,revisionId:rev,scenarios:[],facts:[
   ...producersList.map(p=>({kind:'coverage',ref:`coverage-${p.id}-${rev}`,record:coverage(p.id,rev)})),
   ...(rev==='r1'?proofs.map((record,i)=>({kind:'provenance',ref:`provenance-${i}`,record})):[]),
   ...(rev==='r1'?facts.map(x=>x.fact):[])]});
 }
 if(captureMissing) {
  const capture=JSON.parse(files.get('captures/semantic.json'));
  capture.facts.shift();
  put('captures/semantic.json',capture);
  const artifactHash=sha(files.get('captures/semantic.json'));
  captures.find(x=>x.file==='captures/semantic.json').hash=artifactHash;
  for(const proof of proofs)if(proof.producerId==='semantic')proof.basis.artifactHash=artifactHash;
  const annotation=JSON.parse(files.get('snapshots/r1/main.js.annotations.json'));
  for(const fact of annotation.facts)if(fact.kind==='provenance'&&fact.record.producerId==='semantic')fact.record.basis.artifactHash=artifactHash;
  put('snapshots/r1/main.js.annotations.json',annotation);
 }
 put('fixture.json',fixture);put('expected/answers.json',{formatVersion:1,answers:[]});put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});put('expected/anchors.json',{formatVersion:1,cases:[]});
 for(const [path,value] of files){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}
 const loaded=await loadFixture(root),records=empty();records.comparison=fixture.comparison;records.producers=order(structuredClone(producersList));records.sourceSets=structuredClone(fixture.sourceSets);
 records.revisions=revisions.map(rev=>({...revision(rev),documents:[{key:document,revisionId:rev,contentHash:sha(texts[rev]),byteLength:Buffer.byteLength(texts[rev])}]}));
 records.revisions=order(records.revisions);
 records.coverage=order(revisions.flatMap(rev=>producersList.map(p=>coverage(p.id,rev))));
 records.declarations=allDeclarations.map(d=>({syntaxId:syntax(d.name),document,revisionId:d.revisionId,kind:'function',name:d.name,lookupKey:d.name,ancestors:[],key:{kind:'function',name:d.name,signature:null,ordinal:0},range:range(d.range.start,d.range.end),nameRange:range(d.nameRange.start,d.nameRange.end),header:d.header,provenanceId:`native:${d.revisionId}:${syntax(d.name)}`})).sort((a,b)=>Buffer.compare(Buffer.from(a.syntaxId),Buffer.from(b.syntaxId))||Buffer.compare(Buffer.from(a.revisionId),Buffer.from(b.revisionId)));
 records.calls=allCalls.map(d=>({id:callId(d.revisionId),ownerSyntaxId:syntax('main'),ordinal:0,document,revisionId:d.revisionId,range:range(d.range.start,d.range.end),calleeRange:range(d.calleeRange.start,d.calleeRange.end),spelling:'target',regionIds:[],provenanceId:`native:${d.revisionId}:${callId(d.revisionId)}`})).sort((a,b)=>Buffer.compare(Buffer.from(a.id),Buffer.from(b.id)));
 records.provenance=[...proofs,...[...allDeclarations,...allCalls].map(d=>({id:`native:${d.revisionId}:${d.kind==='function'?syntax(d.name):callId(d.revisionId)}`,producerId:'native',document,revisionId:d.revisionId,contentHash:sha(texts[d.revisionId]),evidenceKind:'measuredSyntax',basis:null,freshness:history&&d.revisionId==='r1'?(staleTarget?'stale':'possiblyStale'):'fresh'}))];
 const prechecks=()=>{const C=checkCoverage(loaded,records),M=checkMeasurement(loaded,records),J=checkJoins(loaded,records,C,M);return {C,M,J};};
 const expectedJoin=fact=>({anchor:{document,revisionId:'r1',contentHash:sha(source),range:range(fact.anchor.range.start,fact.anchor.range.end),kind:'callee'},status,candidateIds:status==='exact'?[callId('r1')]:[],diagnostic:status==='exact'?null:status});
 const targets=order([...new Map(claims.map(name=>[name,internal(name)])).values()]);
 const contradiction=resolution==='resolved'&&status==='exact'&&new Set(claims).size>1&&!producers;
 records.callBindings=facts.map(({fact},i)=>{
  const join=expectedJoin(fact),proof=proofs[i],declaredTarget=resolution==='resolved'?internal(claims[i]):resolution==='external'?{kind:'external',symbol:externalSymbol}:null;
  const stale=declaredTarget?.kind==='internal'?history&&staleTarget:null;
  return {callId:status==='exact'?callId('r1'):null,join,resolution:contradiction?'ambiguous':resolution,declaredTarget:contradiction?null:declaredTarget,
   candidates:contradiction?targets:resolution==='ambiguous'?order([internal('target'),internal('other')]):[],dispatch,possibleDispatch:order(possibleDispatch.map(name=>internal(name))),possibleDispatchComplete:false,
   staleTarget:contradiction?null:stale,provenanceId:proof.id};
 });
 records.callBindings=order(records.callBindings);
 output?.(records.callBindings,records);
 return {loaded,records,prechecks,facts,proofs,claims,expectedJoin,root,files};
}
function check(s) {const {C,M,J}=s.prechecks();return checkBindings(s.loaded,s.records,C,M,J);}

// These expected rows come from authored source offsets, declaration identities and proof IDs.
// They do not read the binding checker result to calculate any expected member.
function expectedMembers(names,{possibleDispatch=[],history=false,staleTarget=false}={}) {
 const targets=order([...new Set(names)].map(name=>internal(name)));
 const contradictory=targets.length>1;
 const start=source.indexOf('target();');
 const join={anchor:{document,revisionId:'r1',contentHash:sha(source),range:range(start,start+6),kind:'callee'},status:'exact',candidateIds:[callId('r1')],diagnostic:null};
 return order(names.map((name,i)=>({callId:callId('r1'),join,
  resolution:contradictory?'ambiguous':'resolved',declaredTarget:contradictory?null:internal(name),
  candidates:contradictory?targets:[],dispatch:'direct',possibleDispatch:order(possibleDispatch.map(x=>internal(x))),
  possibleDispatchComplete:false,staleTarget:contradictory?null:history&&staleTarget,provenanceId:`proof-${i}`})));
}
test('loaded exact claims retain full member/proof closure and canonical bytes for one, two and three targets',async t=>{
 for(const names of [['target','target'],['target','other'],['target','other','third']]) {
  const options={claims:names,possibleDispatch:['other','third'],history:true,staleTarget:true};
  const s=await specimen(t,options),B=check(s),members=expectedMembers(names,options);
  assert.equal(canon(B.callBindings),canon(members));
  assert.equal(canon(s.records.callBindings),canon(members));
  assert.equal(Buffer.compare(Buffer.from(canon(B.callBindings)),Buffer.from(canon(members))),0);
  assert.equal(B.groups.size,1);
  const group=[...B.groups.values()][0];
  const proofIds=names.map((_,i)=>`proof-${i}`),factRefs=names.map((_,i)=>`binding-${i}`);
  assert.equal(canon(group.members),canon(members));
  assert.deepEqual(group.factRefs,factRefs);
  assert.deepEqual(group.provenanceIds,proofIds);
  assert.equal(canon(group.targetProofs),canon(names.map((name,i)=>({factRef:`binding-${i}`,provenanceId:`proof-${i}`,
   declaredTarget:internal(name),candidates:[],possibleDispatch:order(['other','third'].map(x=>internal(x))),proof:s.proofs[i]}))));
  assert.equal(canon(group.historicalTuples),canon(names.map((_,i)=>({factRef:`binding-${i}`,producerId:'semantic',
   document,revisionId:'r1',freshness:'stale'}))));
  assert.equal(canon([...B.recordByFactRef.entries()]),canon(names.map((_,i)=>[`binding-${i}`,members.find(x=>x.provenanceId===`proof-${i}`)])));
  assert.deepEqual([...s.loaded.semanticProofs.keys()],proofIds);
 }
});

test('a byte-identical output duplicate is rejected; two distinct admitted proofs cannot collapse',async t=>{
 const s=await specimen(t,{claims:['target','target']}),B=check(s);
 assert.equal(canon(B.callBindings),canon(expectedMembers(['target','target'])));
 s.records.callBindings.splice(1,0,structuredClone(s.records.callBindings[0]));
 assert.throws(()=>check(s),e=>e.assertion==='BINDING.CONTRIBUTORS'&&e.code==='invalidRecord'&&e.field==='callBindings');
 // A second raw fact with the same ref cannot represent another byte-identical
 // member: loadFixture rejects it before a binding group can be formed.
 await assert.rejects(specimen(t,{claims:['target','target'],raw:facts=>{facts[1].fact.ref=facts[0].fact.ref;}}),
  e=>e.assertion==='IDENTITY.SEMANTIC'&&e.code==='invalidRecord'&&e.field==='facts');
});

test('all resolution cardinalities, six dispatch classes, producer isolation and nonexact groups',async t=>{
 for(const dispatch of ['direct','constructor','virtual','interface','dynamic','unknown']){
  const s=await specimen(t,{dispatch});assert.equal(check(s).callBindings[0].dispatch,dispatch);
 }
 for(const resolution of ['resolved','external','ambiguous','unresolved']){
  const s=await specimen(t,{resolution});assert.equal(check(s).callBindings[0].resolution,resolution);
 }
 const nonexact=await specimen(t,{status:'unmatched'}),B=check(nonexact);
 assert.equal(B.callBindings[0].callId,null);assert.equal([...B.groups.values()][0].anchor.kind,'callee');
 const independent=await specimen(t,{claims:['target','other'],producers:true});
 assert.equal(check(independent).groups.size,2);
});

test('historical proof remains linked to r1 and target freshness is independent',async t=>{
 for(const staleTarget of [false,true]) {
  const s=await specimen(t,{history:true,staleTarget}),B=check(s),member=B.callBindings[0];
  assert.equal(member.callId,callId('r1'));
  assert.equal(member.staleTarget,staleTarget);
  assert.equal(B.groups.size,1);
  assert.equal([...B.groups.values()][0].historicalTuples[0].revisionId,'r1');
  assert.equal(s.proofs[0].freshness,staleTarget?'stale':'possiblyStale');
 }
});

test('finite loaded binding controls run valid baseline before each one-property mutation',async t=>{
 const cases=[
  {id:'BINDING.FACT.proof',rawAfterProof:f=>{f[0].fact.record.provenanceId='proof-missing';},assertion:'IDENTITY.SEMANTIC',field:'binding-0'},
  {id:'BINDING.FACT.missing-capture',options:{},captureMissing:true,assertion:'IDENTITY.SEMANTIC',field:'binding-0'},
  {id:'BINDING.JOIN.call',output:r=>{r[0].callId=callId('r2');},assertion:'BINDING.JOIN',field:'join'},
  {id:'BINDING.JOIN.nonexact-call',options:{status:'unmatched'},output:r=>{r[0].callId=callId('r1');},assertion:'BINDING.JOIN',field:'join'},
  {id:'BINDING.JOIN.unsupported-call',options:{status:'unsupported'},output:r=>{r[0].callId=callId('r1');},assertion:'BINDING.JOIN',field:'join'},
  {id:'BINDING.CONTRIBUTORS.omitted',options:{claims:['target','other']},output:r=>{r.pop();},assertion:'BINDING.CONTRIBUTORS',field:'callBindings'},
  {id:'BINDING.CONTRIBUTORS.extra',output:r=>{r.push({...r[0],provenanceId:'invented'});r.splice(0,r.length,...orderedEnvelope('callBindings',r));},assertion:'BINDING.CONTRIBUTORS',field:'callBindings'},
  {id:'BINDING.CONTRIBUTORS.cross-producer',options:{claims:['target','other'],producers:true},output:r=>{r.find(x=>x.provenanceId==='proof-0').provenanceId='proof-1';r.splice(0,r.length,...orderedEnvelope('callBindings',r));},assertion:'BINDING.CONTRIBUTORS',field:'callBindings'},
  {id:'BINDING.FACT.cross-producer-proof',options:{claims:['target','other'],producers:true},rawAfterProof:f=>{f[0].fact.record.provenanceId='proof-1';},assertion:'IDENTITY.SEMANTIC',field:'binding-0'},
  {id:'BINDING.CONTRIBUTORS.proof-swap',options:{claims:['target','target']},output:r=>{r[0].provenanceId='proof-1';},assertion:'BINDING.CONTRIBUTORS',field:'callBindings'},
  {id:'BINDING.CONTRADICTION.third-union',options:{claims:['target','other','third']},output:r=>{r[0].candidates.pop();},assertion:'BINDING.CONTRADICTION',field:'candidates'},
  {id:'BINDING.CONTRADICTION.nonexact',options:{claims:['target','target'],status:'unmatched'},raw:f=>{f[1].fact.record.declaredTarget.declarationRef='r1:other';},assertion:'BINDING.CONTRADICTION',field:'declaredTarget'},
  {id:'BINDING.CONTRADICTION.dispatch',options:{claims:['target','other']},raw:f=>{f[1].fact.record.dispatch='virtual';},assertion:'BINDING.CONTRADICTION',field:'callBindings'},
  {id:'BINDING.CONTRADICTION.possibleDispatch',options:{claims:['target','other'],possibleDispatch:['third']},raw:f=>{f[1].fact.record.possibleDispatch=[];},assertion:'BINDING.CONTRADICTION',field:'callBindings'},
  {id:'BINDING.CARDINALITY.forged-ambiguity',output:r=>{r[0].resolution='ambiguous';},assertion:'BINDING.CARDINALITY',field:'resolution'},
  {id:'BINDING.CARDINALITY.forged-target',options:{resolution:'ambiguous'},output:r=>{r[0].declaredTarget=internal('target');},assertion:'BINDING.CARDINALITY',field:'candidates'},
  {id:'BINDING.CARDINALITY.forged-candidate',options:{resolution:'ambiguous'},output:r=>{r[0].candidates[1]=internal('third');},assertion:'BINDING.CARDINALITY',field:'candidates'},
  {id:'BINDING.CARDINALITY.raw-resolution',raw:f=>{f[0].fact.record.resolution='ambiguous';},assertion:'BINDING.CARDINALITY',field:'resolution'},
  {id:'BINDING.DISPATCH.output',output:r=>{r[0].dispatch='virtual';},assertion:'BINDING.DISPATCH',field:'dispatch'},
  {id:'BINDING.DISPATCH.raw-complete',raw:f=>{f[0].fact.record.possibleDispatchComplete=true;},assertion:'FORMAT.SHAPE',field:'SemanticCapture.facts[0].record.possibleDispatchComplete'},
  {id:'BINDING.DISPATCH.output-complete',output:r=>{r[0].possibleDispatchComplete=true;},assertion:'FORMAT.SHAPE',field:'NormalizedRecordsV1.callBindings[0].possibleDispatchComplete'},
  {id:'BINDING.DISPATCH.output-possible',options:{possibleDispatch:['other']},output:r=>{r[0].possibleDispatch=[];},assertion:'BINDING.DISPATCH',field:'dispatch'},
  {id:'BINDING.TARGET.stale',output:r=>{r[0].staleTarget=true;},assertion:'BINDING.TARGET',field:'staleTarget'},
  {id:'BINDING.TARGET.historical-attachment',options:{history:true,staleTarget:true},output:r=>{r[0].declaredTarget.revisionId='r2';},assertion:'BINDING.CARDINALITY',field:'resolution'},
  {id:'BINDING.TARGET.duplicate-possible',options:{possibleDispatch:['target']},raw:f=>{f[0].fact.record.possibleDispatch.push(structuredClone(f[0].fact.record.possibleDispatch[0]));},assertion:'BINDING.TARGET',field:'possibleDispatch'},
  {id:'BINDING.TARGET.duplicate-candidates',options:{resolution:'ambiguous'},raw:f=>{f[0].fact.record.candidates[1]=structuredClone(f[0].fact.record.candidates[0]);},assertion:'BINDING.TARGET',field:'candidates'},
  {id:'BINDING.TARGET.wrong-revision',options:{history:true},raw:f=>{f[0].fact.record.declaredTarget.revisionId='r2';},assertion:'BINDING.TARGET',field:'declaredTarget'},
  {id:'BINDING.TARGET.measured-document',output:r=>{r[0].declaredTarget.document={...document,path:'src/other.js'};},assertion:'BINDING.CARDINALITY',field:'resolution'},
  {id:'BINDING.TARGET.measured-source-set',output:r=>{r[0].declaredTarget.document={...document,sourceSetId:'other'};},assertion:'BINDING.CARDINALITY',field:'resolution'},
  {id:'BINDING.TARGET.wrong-document',options:{resolution:'external',externalSymbol:{...external.symbol,scope:'document',document}},raw:f=>{f[0].fact.record.declaredTarget.symbol.document={...document,path:'src/other.js'};},assertion:'BINDING.TARGET',field:'declaredTarget'},
  {id:'BINDING.TARGET.wrong-source-set',options:{resolution:'external',externalSymbol:{...external.symbol,scope:'document',document}},raw:f=>{f[0].fact.record.declaredTarget.symbol.document={...document,sourceSetId:'other'};},assertion:'BINDING.TARGET',field:'declaredTarget'}
 ];
 const rows=registerControls(cases.map(item=>({id:item.id,baseline:()=>({mutated:false}),mutate:state=>({...state,mutated:true}),
  check:async state=>{
   const options={...(item.options??{})};
   if(state.mutated&&item.raw)options.raw=item.raw;
   if(state.mutated&&item.rawAfterProof)options.rawAfterProof=item.rawAfterProof;
   if(state.mutated&&item.captureMissing)options.captureMissing=true;
   if(state.mutated&&item.output)options.output=item.output;
   const s=await specimen(t,options),{C,M,J}=s.prechecks();
   return checkBindings(s.loaded,s.records,C,M,J);
  },expectedAssertion:item.assertion,expectedCode:'invalidRecord',expectedField:item.field})));
 for(const row of rows)await runControl(row);
});

test('admitted target claims reject wrong measured refs and inconsistent contributors',async t=>{
 for(const [label,option,assertion,field] of [
  ['missing-target',{raw:facts=>{facts[0].fact.record.declaredTarget.declarationRef='r1:missing';}},'BINDING.TARGET','declaredTarget'],
  ['revision-target',{raw:facts=>{facts[0].fact.record.declaredTarget.revisionId='r2';}},'BINDING.TARGET','declaredTarget'],
  ['external-contradiction',{claims:['target','other'],raw:facts=>{facts[1].fact.record.resolution='external';facts[1].fact.record.declaredTarget={kind:'external',symbol:external.symbol};}},'BINDING.CONTRADICTION','declaredTarget'],
  ['non-target-dispatch',{claims:['target','other'],raw:facts=>{facts[1].fact.record.possibleDispatch=[{kind:'internal',declarationRef:'r1:third',revisionId:'r1'}];}},'BINDING.CONTRADICTION','callBindings']
 ]) {
  const s=await specimen(t,option),{C,M,J}=s.prechecks();
  assert.throws(()=>checkBindings(s.loaded,s.records,C,M,J),error=>{
   assert.equal(error.assertion,assertion,label);assert.equal(error.code,'invalidRecord',label);assert.equal(error.field,field,label);return true;
  });
 }
});
