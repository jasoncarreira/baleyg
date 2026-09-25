import {validate} from './formats.mjs';
import {canonicalBytes} from './json.mjs';
import {identityRegistry,assignOrdinals,assignOccurrenceOrdinals,headerHash,siblingGroupHash} from './identity.mjs';
import {toByteRange,verifyWitness,verifyCallSpelling} from './coordinates.mjs';
import {lookupKey,validateRoles} from './lookup.mjs';
import {expectedFreshness,expectedStaleTarget,checkCapturedBasis} from './check-freshness.mjs';

const encode=x=>canonicalBytes(x).toString('hex');
const compare=(a,b)=>Buffer.compare(canonicalBytes(a),canonicalBytes(b));
const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
const tuple=(set,rev)=>JSON.stringify([set,rev]);
const sourceKey=row=>JSON.stringify([row.document.sourceSetId,row.revisionId,row.document.path]);
function fail(id,field,message){const e=new Error(`${id} ${field}: ${message}`);e.assertion=id;e.code='invalidRecord';e.field=field;throw e;}
function requireValue(value,id,field){if(value===undefined)fail(id,field,'missing source-derived value');return value;}
function unique(rows,key,id,field){const seen=new Set();for(const row of rows){const k=key(row);if(seen.has(k))fail(id,field,'duplicate measured row');seen.add(k);}}
function indexed(rows){return new Map(rows.map(x=>[x.ref,x]));}
function sortRecords(rows){return rows.sort((a,b)=>a.syntaxId&&b.syntaxId?order(a.syntaxId,b.syntaxId):a.id&&b.id?order(a.id,b.id):compare(a,b));}
function insert(map,ref,value){if(map.has(ref)&&encode(map.get(ref))!==encode(value))fail('NORMALIZE.RECORD_REF','recordRef','conflicting record reference');map.set(ref,value);}
function resolveTarget(ref,ids,decls){if(ref===null)return null;if(ref.kind==='external')return {kind:'external',symbol:ref.symbol};const row=requireValue(decls.get(ref.declarationRef),'NORMALIZE.TARGET','declarationRef');if(row.revisionId!==ref.revisionId)fail('NORMALIZE.TARGET','revisionId','target revision differs');return {kind:'internal',syntaxId:requireValue(ids.get(ref.declarationRef),'NORMALIZE.TARGET','declarationRef'),document:row.document,revisionId:row.revisionId};}
function distinctTargets(targets){return [...new Map(targets.map(x=>[encode(x),x])).values()].sort(compare);}
function resolution(target,candidates,status){if(status==='resolved'&&target?.kind==='internal'&&!candidates.length)return;if(status==='external'&&target?.kind==='external'&&!candidates.length)return;if(status==='ambiguous'&&target===null&&distinctTargets(candidates).length>=2)return;if(status==='unresolved'&&target===null&&!candidates.length)return;fail('NORMALIZE.RESOLUTION','resolution','target cardinality differs');}

// The unit seam accepts distinct measured candidates, but fixture compilation always
// rejects duplicate owner/kind/range measurements before constructing this index.
export function buildAnchorIndex(measurements){const index=new Map();for(const x of measurements){const k=encode([x.anchor,x.ownerRef]);if(!index.has(k))index.set(k,[]);index.get(k).push(x);}return index;}
export function joinAnchor(selector,index,loaded,ids=new Map()){
 validate('AnchorSelector',selector);
 const source=requireValue(loaded.sources.get(JSON.stringify([selector.document.sourceSetId,selector.revisionId,selector.document.path])),'JOIN.DOCUMENT','document');
 const snapshot=loaded.revisions.get(tuple(selector.document.sourceSetId,selector.revisionId));
 const document=snapshot?.documents.find(x=>encode(x.key)===encode(selector.document));
 if(!document||document.contentHash!==selector.contentHash)fail('JOIN.DOCUMENT','contentHash','anchor source tuple differs');
 const range=toByteRange(source,selector.range);
 const anchor={document:selector.document,revisionId:selector.revisionId,contentHash:selector.contentHash,range,kind:selector.kind};
 const intent=loaded.fixture.coverageIntents.find(x=>x.revisionId===selector.revisionId&&encode(x.document)===encode(selector.document));
 const support=intent?.measurementSupport.find(x=>x.kind===selector.kind);
 const matches=index.get(encode([anchor,selector.ownerRef]))??[];
 const candidateIds=[...new Set(matches.map(x=>requireValue(ids.get(x.ref)??x.id,'JOIN.CANDIDATE','candidateIds')))].sort(order);
 const status=support?.available===false?'unsupported':candidateIds.length===0?'unmatched':candidateIds.length===1?'exact':'ambiguous';
 return {anchor,status,candidateIds:status==='unsupported'?[]:candidateIds,diagnostic:status==='exact'?null:status==='unsupported'?support.diagnostic:status};
}

export function assignKeys(loaded,registry=identityRegistry()){
 const native=loaded.native, declarations=indexed(native.declarations), ids=new Map(), entries=new Map(), active=new Set();
 const measured=new Map();
 const position=(row)=>{const source=requireValue(loaded.sources.get(sourceKey(row)),'NORMALIZE.SOURCE','document');const range=toByteRange(source,row.range);const nameRange=row.nameRange===null?null:toByteRange(source,row.nameRange);if((row.name===null)!==(nameRange===null))fail('NORMALIZE.NAME','nameRange','name and range nullability differ');for(const item of row.witnesses){const actual=verifyWitness(source,item.witness);if(item.field==='name'&&(item.witness.text!==row.name||encode(actual)!==encode(nameRange)))fail('NORMALIZE.WITNESS','name','name witness differs');}if(row.name!==null&&!row.witnesses.some(x=>x.field==='name'))fail('NORMALIZE.WITNESS','name','name witness missing');if(row.header.kind!==row.kind||row.header.name!==row.name)fail('NORMALIZE.HEADER','header','header differs from declaration');return {range,nameRange};};
 for(const row of native.declarations)measured.set(row.ref,position(row));
 const records=[], pending=new Set(native.declarations.map(x=>x.ref));
 while(pending.size){const ready=native.declarations.filter(x=>pending.has(x.ref)&&(x.parentRef===null||!pending.has(x.parentRef)));
  if(!ready.length)fail('NORMALIZE.OWNER','parentRef','declaration cycle');
  const ordinalRows=ready.map(row=>{let ancestors=[];if(row.parentRef!==null){const parent=requireValue(declarations.get(row.parentRef),'NORMALIZE.OWNER','parentRef');if(sourceKey(parent)!==sourceKey(row))fail('NORMALIZE.OWNER','parentRef','cross-document parent');const p=requireValue(entries.get(parent.ref),'NORMALIZE.OWNER','parentRef');const childRange=measured.get(row.ref).range;if(childRange.start<p.range.start||childRange.end>p.range.end)fail('NORMALIZE.OWNER','range','child outside parent');ancestors=parent.kind==='module'?p.ancestors:[...p.ancestors,p.key];}return {row,sourceSetId:row.document.sourceSetId,language:row.document.language,documentPath:row.document.path,revisionId:row.revisionId,container:ancestors,kind:row.kind,name:row.name,signature:row.signature,range:measured.get(row.ref).range};});
  const ordinals=assignOrdinals(ordinalRows);
  for(const entry of ordinalRows){const {row}=entry,key={kind:row.kind,name:row.name,signature:row.signature,ordinal:ordinals.get(entry)},ancestors=entry.container;const syntaxId=registry.register('syntax',{sourceSet:row.document.sourceSetId,path:row.document.path,language:row.document.language,ancestors,declaration:key});ids.set(row.ref,syntaxId);entries.set(row.ref,{key,ancestors,range:entry.range});pending.delete(row.ref);const out={syntaxId,document:row.document,revisionId:row.revisionId,kind:row.kind,name:row.name,lookupKey:row.name===null?null:lookupKey(row.document.language,row.name),ancestors,key,range:entry.range,nameRange:measured.get(row.ref).nameRange,header:row.header,provenanceId:`native:${row.revisionId}:${syntaxId}`};validate('Declaration',out);records.push(out);}
 }
 const verified=new Map();for(const record of records){const k=tuple(record.document.sourceSetId,record.revisionId);if(!verified.has(k))verified.set(k,new Map());const entries=verified.get(k);if(entries.has(record.syntaxId)&&encode(entries.get(record.syntaxId))!==encode(record.document))fail('NORMALIZE.COLLISION','syntaxId','conflicting declaration');entries.set(record.syntaxId,record.document);}
 return {records:sortRecords(records),ids,verified,registry};
}

export function normalizeOccurrences(loaded,keys){
 const {native}=loaded, decl=indexed(native.declarations), controls=indexed(native.controls), ids=new Map(), measurements=[], rows=[];
 for(const [kind,items] of [['call',native.calls],['control',native.controls],['reference',native.references]])for(const row of items){
  const owner=requireValue(decl.get(row.ownerRef),'NORMALIZE.OWNER','ownerRef');if(sourceKey(owner)!==sourceKey(row))fail('NORMALIZE.OWNER','ownerRef','cross-document occurrence');const source=requireValue(loaded.sources.get(sourceKey(row)),'NORMALIZE.SOURCE','document');const range=toByteRange(source,row.range);const ownerRange=toByteRange(source,owner.range);if(range.start<ownerRange.start||range.end>ownerRange.end)fail('NORMALIZE.OWNER','range','outside owner');for(const x of row.witnesses)verifyWitness(source,x.witness);
  const own=keys.ids.get(row.ownerRef);let calleeRange=null;
  if(kind==='call'){calleeRange=row.calleeRange===null?null:toByteRange(source,row.calleeRange);if(calleeRange&&(calleeRange.start<range.start||calleeRange.end>range.end))fail('NORMALIZE.CALLEE','calleeRange','outside invocation');if(calleeRange&&row.spelling!==null&&source.subarray(calleeRange.start,calleeRange.end).toString('utf8')!==row.spelling)fail('NORMALIZE.CALLEE','spelling','callee bytes differ');if(!calleeRange&&row.spelling!==null){const witness=row.witnesses.find(x=>x.field==='spelling')?.witness;verifyCallSpelling(source,{calleeRange:null,spelling:row.spelling,range},witness);}}
  if(kind==='reference'&&source.subarray(range.start,range.end).toString('utf8')!==row.spelling)fail('NORMALIZE.REFERENCE','spelling','reference bytes differ');
  rows.push({row,kind,range,calleeRange,revisionId:row.revisionId,ownerSyntaxId:own});
 }
 const ordinals=assignOccurrenceOrdinals(rows);for(const item of rows){item.ordinal=ordinals.get(item);item.id=keys.registry.register('occurrence',{revisionId:item.revisionId,ownerSyntaxId:item.ownerSyntaxId,kind:item.kind,ordinal:item.ordinal});ids.set(item.row.ref,item.id);}
 const all=rows.map(x=>{const {row,kind,range,id}=x,snapshot=loaded.revisions.get(tuple(row.document.sourceSetId,row.revisionId)),document=snapshot.documents.find(d=>encode(d.key)===encode(row.document));const anchor=(family,r)=>({document:row.document,revisionId:row.revisionId,contentHash:document.contentHash,range:r,kind:family});if(kind!=='control')measurements.push({ref:row.ref,id,ownerRef:row.ownerRef,anchor:anchor(kind==='call'?'invocation':'reference',range)});if(kind==='call'&&x.calleeRange!==null)measurements.push({ref:row.ref,id,ownerRef:row.ownerRef,anchor:anchor('callee',x.calleeRange)});return x;});
 for(const row of native.declarations){if(row.nameRange===null)continue;const snap=loaded.revisions.get(tuple(row.document.sourceSetId,row.revisionId));const doc=snap.documents.find(d=>encode(d.key)===encode(row.document));measurements.push({ref:row.ref,id:keys.ids.get(row.ref),ownerRef:row.parentRef??'',anchor:{document:row.document,revisionId:row.revisionId,contentHash:doc.contentHash,range:toByteRange(loaded.sources.get(sourceKey(row)),row.nameRange),kind:'declarationName'}});}
 const callRows=all.filter(x=>x.kind==='call'),controlRows=all.filter(x=>x.kind==='control'),referenceRows=all.filter(x=>x.kind==='reference');
 const byControl=indexed(native.controls),visiting=new Set(),complete=new Set();function controlChain(ref){if(complete.has(ref))return;if(visiting.has(ref))fail('NORMALIZE.CONTROL','parentRef','cycle');visiting.add(ref);const row=requireValue(byControl.get(ref),'NORMALIZE.CONTROL','parentRef');if(row.parentRef!==null){const p=requireValue(byControl.get(row.parentRef),'NORMALIZE.CONTROL','parentRef');if(p.ownerRef!==row.ownerRef||sourceKey(p)!==sourceKey(row))fail('NORMALIZE.CONTROL','parentRef','cross-owner parent');controlChain(p.ref);const inner=controlRows.find(x=>x.row===row).range,outer=controlRows.find(x=>x.row===p).range;if(inner.start<outer.start||inner.end>outer.end)fail('NORMALIZE.CONTROL','range','outside parent');}visiting.delete(ref);complete.add(ref);}for(const row of native.controls)controlChain(row.ref);
 const regions=controlRows.map(x=>({id:x.id,ownerSyntaxId:x.ownerSyntaxId,ordinal:x.ordinal,document:x.row.document,revisionId:x.revisionId,kind:x.row.kind,range:x.range,parentId:x.row.parentRef===null?null:ids.get(x.row.parentRef),arm:x.row.arm,provenanceId:`native:${x.revisionId}:${x.id}`}));
 const calls=callRows.map(x=>{for(const ref of x.row.regionRefs){const region=requireValue(byControl.get(ref),'NORMALIZE.REGION','regionRefs');const area=controlRows.find(y=>y.row===region).range;if(region.ownerRef!==x.row.ownerRef||x.range.start<area.start||x.range.end>area.end)fail('NORMALIZE.REGION','regionRefs','call outside control');}return {id:x.id,ownerSyntaxId:x.ownerSyntaxId,ordinal:x.ordinal,document:x.row.document,revisionId:x.revisionId,range:x.range,calleeRange:x.calleeRange,spelling:x.row.spelling,regionIds:x.row.regionRefs.map(ref=>ids.get(ref)),provenanceId:`native:${x.revisionId}:${x.id}`};});
 for(const x of [...regions,...calls])validate(x.regionIds?'Call':'ControlRegion',x);
 return {calls:sortRecords(calls),controlRegions:sortRecords(regions),referenceRows,ids,index:buildAnchorIndex(measurements)};
}

export function collapseFacts(rows){const by=new Map();for(const row of rows){const key=encode(row);if(!by.has(key))by.set(key,row);}return sortRecords([...by.values()]);}

function nativeProof(row,loaded,id){const snapshot=loaded.revisions.get(tuple(row.document.sourceSetId,row.revisionId)),document=snapshot.documents.find(x=>encode(x.key)===encode(row.document));const proof={id,producerId:loaded.native.producerId,document:row.document,revisionId:row.revisionId,contentHash:document.contentHash,evidenceKind:'measuredSyntax',basis:null,freshness:'fresh'};proof.freshness=expectedFreshness(proof,loaded);validate('Provenance',proof);return proof;}
function normalizeRelationship(fact,ids,decls){const record={kind:fact.relationshipKind,source:resolveTarget(fact.source,ids,decls),target:resolveTarget(fact.target,ids,decls),provenanceId:fact.provenanceRef};if(record.source.kind!=='internal')fail('NORMALIZE.RELATIONSHIP','source','internal source required');validate('TypeRelationship',record);return record;}
export {normalizeRelationship};

export function normalizeFixture(loaded){
 if(!loaded?.native||!loaded?.fixture||!loaded?.sources||!loaded?.selected)fail('NORMALIZE.INPUT','loaded','admitted fixture required');
 validate('FixtureV1',loaded.fixture);validate('NativeArtifact',loaded.native);
 unique([...loaded.native.declarations,...loaded.native.calls,...loaded.native.controls,...loaded.native.references],x=>x.ref,'NORMALIZE.NATIVE','ref');
 const keys=assignKeys(loaded),occ=normalizeOccurrences(loaded,keys),decl=indexed(loaded.native.declarations),nativeRows=[...loaded.native.declarations,...loaded.native.calls,...loaded.native.controls,...loaded.native.references];
 const records={formatVersion:1,comparison:structuredClone(loaded.comparison),producers:sortRecords(structuredClone(loaded.fixture.producers)),sourceSets:sortRecords(structuredClone(loaded.fixture.sourceSets)),revisions:sortRecords([...loaded.revisions.values()].map(x=>structuredClone(x))),coverage:[],provenance:[],declarations:keys.records,symbols:[],declarationBindings:[],typeRelationships:[],calls:occ.calls,controlRegions:occ.controlRegions,references:[],referenceJoinDiagnostics:[],callBindings:[],durableAnchors:[],groupContinuities:[],anchorResults:[]};
 const recordMap=new Map(),identityMap=new Map([...keys.ids,...occ.ids]);for(const row of nativeRows){const output=records.declarations.find(x=>x.syntaxId===keys.ids.get(row.ref))??records.calls.find(x=>x.id===occ.ids.get(row.ref))??records.controlRegions.find(x=>x.id===occ.ids.get(row.ref));if(output)recordMap.set(row.ref,output);const proof=nativeProof(row,loaded,`native:${row.revisionId}:${identityMap.get(row.ref)}`);records.provenance.push(proof);}
 const factRefs=new Set(),provenances=new Map(records.provenance.map(x=>[x.id,x]));
 const facts=loaded.annotations.flatMap(x=>x.facts);
 for(const fact of facts){validate('Fact',fact);if(factRefs.has(fact.ref))fail('NORMALIZE.FACT_REF','ref','duplicate fixture-wide fact reference');factRefs.add(fact.ref);if(fact.kind!=='provenance')continue;const proof={...fact.record,freshness:expectedFreshness(fact.record,loaded)};checkCapturedBasis(proof,loaded);const old=provenances.get(proof.id);if(old&&encode(old)!==encode(proof))fail('NORMALIZE.PROVENANCE','id','conflicting proof');provenances.set(proof.id,proof);records.provenance.push(proof);recordMap.set(fact.ref,proof);}
 const getProof=id=>requireValue(provenances.get(id),'NORMALIZE.PROVENANCE','provenanceId');
 const target=ref=>resolveTarget(ref,keys.ids,decl);
 const bindingGroups=new Map(),referenceGroups=new Map();
 for(const fact of facts){if(fact.kind==='provenance')continue;
  if(fact.kind==='coverage'){validate('Coverage',fact.record);records.coverage.push(fact.record);recordMap.set(fact.ref,fact.record);continue;}
  const proof=getProof(fact.kind==='typeRelationship'?fact.provenanceRef:fact.record.provenanceId);
  if(fact.kind==='symbol'){const value={...fact.record,declarations:fact.record.declarations.map(target)};validate('Symbol',value);records.symbols.push(value);recordMap.set(fact.ref,value);continue;}
  if(fact.kind==='typeRelationship'){if(proof.evidenceKind!=='typeRelationship')fail('NORMALIZE.RELATIONSHIP','provenanceRef','wrong independent proof');const value=normalizeRelationship(fact,keys.ids,decl);records.typeRelationships.push(value);recordMap.set(fact.ref,value);continue;}
  if((fact.kind==='declarationBinding'&&fact.anchor.kind!=='declarationName')||(fact.kind==='reference'&&fact.anchor.kind!=='reference')||(fact.kind==='callBinding'&&!['callee','invocation'].includes(fact.anchor.kind)))fail('NORMALIZE.JOIN_FAMILY','anchor.kind','fact anchor family mismatch');
  const join=joinAnchor(fact.anchor,occ.index,loaded,identityMap);
  if(fact.kind==='declarationBinding'){const value={syntaxId:join.status==='exact'?join.candidateIds[0]:null,symbols:fact.record.symbols,join,provenanceId:proof.id};validate('DeclarationBinding',value);records.declarationBindings.push(value);recordMap.set(fact.ref,value);continue;}
  if(fact.kind==='reference'){
   if(proof.evidenceKind!=='semanticReference')fail('NORMALIZE.REFERENCE','provenanceId','semantic reference proof required');
   if(join.status!=='exact'){const diagnostic={factRef:fact.ref,provenanceId:proof.id,join};validate('ReferenceJoinDiagnostic',diagnostic);records.referenceJoinDiagnostics.push(diagnostic);recordMap.set(fact.ref,diagnostic);continue;}
   const native=occ.referenceRows.find(x=>x.id===join.candidateIds[0]);if(!native)fail('NORMALIZE.REFERENCE','anchor','reference measurement missing');const row=native.row;
   const value={id:native.id,ownerSyntaxId:native.ownerSyntaxId,ordinal:native.ordinal,document:row.document,revisionId:row.revisionId,range:native.range,spelling:row.spelling,lookupKey:lookupKey(row.document.language,row.spelling),site:fact.record.site,roles:fact.record.roles,resolution:fact.record.resolution,declaredTarget:target(fact.record.declaredTarget),candidates:distinctTargets(fact.record.candidates.map(target)),provenanceId:proof.id};
   validateRoles(row.document.language,value.roles,{site:value.site,callee:occ.calls.some(x=>x.ownerSyntaxId===value.ownerSyntaxId&&x.calleeRange&&encode(x.calleeRange)===encode(value.range))});resolution(value.declaredTarget,value.candidates,value.resolution);validate('Reference',value);
   if(!referenceGroups.has(value.id))referenceGroups.set(value.id,[]);referenceGroups.get(value.id).push({fact,value});recordMap.set(fact.ref,value);continue;
  }
  if(fact.kind==='callBinding'){
   const value={callId:join.status==='exact'?join.candidateIds[0]:null,join,resolution:fact.record.resolution,declaredTarget:target(fact.record.declaredTarget),candidates:distinctTargets(fact.record.candidates.map(target)),dispatch:fact.record.dispatch,possibleDispatch:distinctTargets(fact.record.possibleDispatch.map(target)),possibleDispatchComplete:false,staleTarget:null,provenanceId:proof.id};resolution(value.declaredTarget,value.candidates,value.resolution);value.staleTarget=expectedStaleTarget(value,proof,loaded,keys.verified);validate('CallBinding',value);
   const k=encode([proof.producerId,join.anchor]);if(!bindingGroups.has(k))bindingGroups.set(k,[]);bindingGroups.get(k).push({fact,value,proof});recordMap.set(fact.ref,value);
  }
 }
 for(const group of referenceGroups.values()){const uniqueValues=collapseFacts(group.map(x=>x.value));if(uniqueValues.length!==1)fail('NORMALIZE.REFERENCE_CONFLICT','resolution','conflicting reference facts');records.references.push(uniqueValues[0]);}
 for(const group of bindingGroups.values()){
  const items=collapseFacts(group.map(x=>x.value));if(items.length===1){records.callBindings.push(items[0]);continue;}
  const baseline=items[0],same=items.every(x=>x.callId===baseline.callId&&encode(x.join)===encode(baseline.join)&&x.dispatch===baseline.dispatch&&encode(x.possibleDispatch)===encode(baseline.possibleDispatch));
  if(!same||items.some(x=>x.join.status!=='exact'||x.resolution!=='resolved'||x.declaredTarget?.kind!=='internal'))fail('NORMALIZE.BINDING_CONFLICT','declaredTarget','incompatible duplicate facts');
  const targets=distinctTargets(items.map(x=>x.declaredTarget));if(targets.length<2)fail('NORMALIZE.BINDING_CONFLICT','declaredTarget','conflicting duplicate facts');
  for(const x of items){const value={...x,resolution:'ambiguous',declaredTarget:null,candidates:targets,staleTarget:null};validate('CallBinding',value);records.callBindings.push(value);for(const entry of group)if(entry.value.provenanceId===x.provenanceId)recordMap.set(entry.fact.ref,value);}
 }
 for(const anchorCase of loaded.anchors.cases){
  const captured=requireValue(decl.get(anchorCase.capturedDeclarationRef),'NORMALIZE.ANCHOR','capturedDeclarationRef');
  const declaration=requireValue(keys.records.find(x=>x.syntaxId===keys.ids.get(captured.ref)&&x.revisionId===captured.revisionId),'NORMALIZE.ANCHOR','capturedDeclarationRef');
  const siblings=keys.records.filter(x=>encode(x.document)===encode(declaration.document)&&x.revisionId===declaration.revisionId&&encode(x.ancestors)===encode(declaration.ancestors)&&x.key.kind===declaration.key.kind&&x.key.name===declaration.key.name&&encode(x.key.signature)===encode(declaration.key.signature)).sort((a,b)=>a.range.start-b.range.start||a.range.end-b.range.end);
  const focusedHash=headerHash(declaration.header);const headers=siblings.map(x=>headerHash(x.header));
  const durable={syntaxId:declaration.syntaxId,document:declaration.document,capturedRevisionId:declaration.revisionId,headerHash:focusedHash,siblingGroupHash:siblingGroupHash(headers),siblingCount:siblings.length,identicalHeaderCount:headers.filter(x=>x===focusedHash).length};
  validate('DurableAnchor',durable);records.durableAnchors.push(durable);records.groupContinuities.push(anchorCase.continuity);
  const answer=anchorCase.expectedResult;const targetId=answer.targetId===null?null:typeof answer.targetId==='string'?answer.targetId:requireValue(identityMap.get(answer.targetId.ref),'NORMALIZE.ANCHOR','targetId');
  const result={...answer,targetId};validate('AnchorResult',result);records.anchorResults.push(result);recordMap.set(anchorCase.id,durable);
 }
 for(const field of ['coverage','provenance','symbols','declarationBindings','typeRelationships','references','referenceJoinDiagnostics','callBindings'])records[field]=collapseFacts(records[field]);
 for(const field of ['durableAnchors','groupContinuities','anchorResults'])records[field].sort(compare);
 records.referenceJoinDiagnostics.sort((a,b)=>order(a.factRef,b.factRef));
 validate('NormalizedRecordsV1',records);
 return {records,identityMap,recordMap,declarations:keys.verified};
}
export const normalize=normalizeFixture;
