import {validate} from './formats.mjs';
import {canonicalBytes} from './json.mjs';
import {toByteRange} from './coordinates.mjs';

const categories=['sameNameOverload','importsAliases','callableValues','recursion','relationshipsDispatch','unicodeCoordinates','coverageFreshness','compatibilityControl'];
const outcomes=['resolved','provenExternal','ambiguous','unresolved','unsupported'];
const roles=['definition','read','write','call','type','import','alias'];
const identity=value=>canonicalBytes(value).toString('hex');
const same=(a,b)=>identity(a)===identity(b);
function fail(assertion,field,message) {
 const error=new Error(`${assertion} ${field}: ${message}`);
 Object.assign(error,{assertion,field,code:'invalidRecord'});throw error;
}
function unique(rows,selector,field) {
 const seen=new Set();
 for(const row of rows) {
  const key=selector(row);
  if(seen.has(key))fail('COUNT.DUPLICATE',field,'repeated authored evidence');
  seen.add(key);
 }
}
function source(loaded,row) {
 return loaded.sources.get(JSON.stringify([row.document.sourceSetId,row.revisionId,row.document.path]));
}
function nativeKey(row) {return identity([row.document,row.revisionId,row.range]);}
function anchorMatches(loaded,anchor) {
 const bytes=source(loaded,anchor);
 if(!bytes)return false;
 const range=toByteRange(bytes,anchor.range);
 const native=loaded.native;
 const candidates=anchor.kind==='declarationName'?native.declarations.filter(x=>x.nameRange!==null).map(x=>({...x,range:x.nameRange})):anchor.kind==='reference'?native.references:anchor.kind==='callee'?native.calls.filter(x=>x.calleeRange!==null).map(x=>({...x,range:x.calleeRange})):native.calls;
 return candidates.some(x=>same(x.document,anchor.document)&&x.revisionId===anchor.revisionId&&
  same(toByteRange(source(loaded,x),x.range),range) &&
  (x.ref===anchor.ownerRef || x.ownerRef===anchor.ownerRef));
}

// Count only measured occurrences and authenticated captured facts, never reported totals.
export function checkCounts(loaded,records,dispositions=loaded.dispositions) {
 validate('NormalizedRecordsV1',records);
 validate('DispositionsV1',dispositions);
 const {fixture,native,annotations}=loaded;
 const annotationsFacts=annotations.flatMap(a=>a.facts.filter(f=>f.kind!=='provenance').map(f=>({fact:f,annotation:a})));
 const facts=new Map(annotationsFacts.map(row=>[row.fact.ref,row]));
 unique(annotationsFacts,x=>x.fact.ref,'facts');
 const proofs=new Map(records.provenance.map(x=>[x.id,x]));
 const scenarios=annotations.flatMap(a=>a.scenarios.map(s=>({scenario:s,annotation:a})));
 unique(scenarios,x=>x.scenario.id,'scenarios');
 unique(scenarios,x=>identity([x.scenario.category,x.scenario.anchors.map(identity).sort()]),'scenarios.anchors');
 const scenarioById=new Map(scenarios.map(x=>[x.scenario.id,x]));
 for(const {scenario,annotation} of scenarios) {
  if(!scenario.anchors.length)fail('COUNT.SCENARIO','anchors','scenario has no measured anchor');
  unique(scenario.anchors,identity,'anchors');unique(scenario.factRefs,x=>x,'factRefs');
  for(const anchor of scenario.anchors) {
   if(!same(anchor.document,annotation.document)||anchor.revisionId!==annotation.revisionId||!anchorMatches(loaded,anchor))
    fail('COUNT.SCENARIO','anchors','scenario anchor is not an exact measured occurrence');
  }
  for(const ref of scenario.factRefs)if(!facts.has(ref))fail('COUNT.SCENARIO','factRefs','uncaptured fact');
 }
 const nativeCalls=new Map(native.calls.map(x=>[x.ref,x]));
 const installedCalls=new Set(records.calls.map(x=>identity([x.id,x.document,x.revisionId])));
 if(records.calls.length!==native.calls.length||installedCalls.size!==native.calls.length)
  fail('COUNT.CALLS','measuredCalls','calls must be measured native occurrences');
 const nativeRefs=new Map(native.references.map(x=>[x.ref,x]));
 const referenceFacts=annotationsFacts.filter(x=>x.fact.kind==='reference');
 const exactReferences=new Map();
 for(const {fact,annotation} of referenceFacts) {
  const proof=proofs.get(fact.record.provenanceId);
  if(!proof || loaded.semanticProofs.get(proof.id)?.factRef!==fact.ref || proof.evidenceKind!=='semanticReference'||
     !same(proof.document,annotation.document)||proof.revisionId!==annotation.revisionId)
   fail('COUNT.PROOF','references','reference lacks its captured proof');
  const candidates=[...nativeRefs.values()].filter(row=>same(row.document,annotation.document)&&row.revisionId===annotation.revisionId&&
    same(toByteRange(source(loaded,row),row.range),toByteRange(source(loaded,fact.anchor),fact.anchor.range))&&
    row.ownerRef===fact.anchor.ownerRef);
  const installed=records.references.filter(r=>r.provenanceId===proof.id);
  if(candidates.length===1 && installed.length===1 && same(installed[0].range,toByteRange(source(loaded,candidates[0]),candidates[0].range)))
   exactReferences.set(fact.ref,{row:candidates[0],record:installed[0]});
  else if(installed.length)fail('COUNT.REFERENCE','references','non-exact reference was installed');
 }
 for(const row of records.references)if(![...exactReferences.values()].some(x=>same(x.record,row)))
  fail('COUNT.REFERENCE','references','installed reference has no exact captured occurrence and proof');
 const referenceKeys=new Set([...exactReferences.values()].map(x=>nativeKey(x.row)));
 const negatives=new Set();
 unique(dispositions.callableValueNegatives,x=>identity([x.scenarioId,x.referenceRef]),'callableValueNegatives');
 for(const negative of dispositions.callableValueNegatives) {
  const scenario=scenarioById.get(negative.scenarioId)?.scenario;
  const exact=exactReferences.get(negative.referenceRef);
  const ref=exact?.row;
  // A callable-value read is a Reference, not a call, even if its spelling is callable.
  const ownerCall=ref && native.calls.some(c=>same(c.document,ref.document)&&c.revisionId===ref.revisionId&&
   c.ownerRef===ref.ownerRef&&same(toByteRange(source(loaded,c),c.calleeRange??c.range),toByteRange(source(loaded,ref),ref.range)));
  if(!ref||!exact||!scenario||scenario.category!=='callableValues'||
     !scenario.factRefs.includes(negative.referenceRef)||ref.ownerRef!==negative.ownerRef||
     !same(ref.range,negative.range)||ownerCall||!exact.record.roles.includes('read')||exact.record.roles.includes('call')||
   !records.declarations.some(d=>exact.record.declaredTarget?.kind==='internal'&&
    ['function','method','constructor','anonymousFunction'].includes(d.kind)&&
    d.syntaxId===exact.record.declaredTarget.syntaxId&&same(d.document,exact.record.declaredTarget.document)&&
    d.revisionId===exact.record.declaredTarget.revisionId))
   fail('COUNT.NEGATIVE','callableValueNegatives','negative needs an exact measured non-call read and callable-value scenario');
  negatives.add(nativeKey(ref));
 }
 const relationships=annotationsFacts.filter(({fact})=>fact.kind==='typeRelationship');
 const applicableRelationships=new Set();
 for(const {fact} of relationships) {
  const proof=proofs.get(fact.provenanceRef);
  if(!proof||loaded.semanticProofs.get(proof.id)?.factRef!==fact.ref||proof.evidenceKind!=='typeRelationship'||
     !records.typeRelationships.some(x=>x.provenanceId===proof.id&&x.kind===fact.relationshipKind))
   fail('COUNT.RELATIONSHIP','typeRelationships','relationship lacks a checked directed proof');
  applicableRelationships.add(identity([fact.relationshipKind,fact.source,fact.target]));
 }
 const authored=dispositions.assertions;
 unique(authored,x=>identity([x.kind,x.factRef]),'assertions');
 const outcomeKeys=new Map(outcomes.map(x=>[x,new Set()]));
 for(const row of authored) {
  const entry=facts.get(row.factRef);
  if(!entry||!['callBinding','reference','declarationBinding'].includes(entry.fact.kind))
   fail('COUNT.DISPOSITION','assertions','disposition does not name a captured join fact');
  const fact=entry.fact,proofId=fact.record.provenanceId,proof=proofs.get(proofId);
  if(!proof||loaded.semanticProofs.get(proofId)?.factRef!==fact.ref)
   fail('COUNT.PROOF','assertions','disposition has no authenticated proof');
  const diag=records.referenceJoinDiagnostics.find(x=>x.factRef===fact.ref);
  const record=entry.fact.kind==='reference'?records.references.find(x=>x.provenanceId===proofId):
   entry.fact.kind==='callBinding'?records.callBindings.find(x=>x.provenanceId===proofId):
   records.declarationBindings.find(x=>x.provenanceId===proofId);
  const join=record?.join??diag?.join??(exactReferences.has(fact.ref)?{status:'exact'}:null);
  if(row.kind==='join') {
   if(!join || row.disposition!==join.status)fail('COUNT.DISPOSITION','assertions','join does not match measured result');
   if(row.disposition==='unsupported') {
    const support=fixture.coverageIntents?.find(x=>x.producerId===proof.producerId&&same(x.document,fact.anchor.document)&&x.revisionId===fact.anchor.revisionId)?.measurementSupport.find(x=>x.kind===fact.anchor.kind);
    if(support?.available!==false)fail('COUNT.DISPOSITION','assertions','unsupported join needs independently declared unavailable measurement');
    outcomeKeys.get('unsupported').add(identity([fact.anchor.document,fact.anchor.revisionId,fact.anchor.kind,fact.anchor.range]));
   }
  } else {
   if(!record||!['reference','callBinding'].includes(fact.kind)||record.join?.status&&record.join.status!=='exact')
    fail('COUNT.DISPOSITION','assertions','resolution requires installed exact proof');
   const expected=record.resolution==='external'?'provenExternal':record.resolution;
   if(row.disposition!==expected)fail('COUNT.DISPOSITION','assertions','resolution contradicts checked proof');
   const id=fact.kind==='reference'?record.id:record.callId;
   if(id===null)fail('COUNT.DISPOSITION','assertions','uninstalled occurrence cannot count');
   outcomeKeys.get(expected).add(identity([fact.kind,entry.annotation.document,entry.annotation.revisionId,id]));
  }
 }
 const observed=new Set(records.references.flatMap(r=>r.roles));
 const count={formatVersion:1,language:fixture.language,profile:fixture.profile,
  floorsEnforced:!(fixture.profile==='example'&&loaded.root?.replaceAll('\\','/').split('/').at(-1)==='example'),
  scenariosTotal:scenarios.length,
  scenariosByCategory:categories.map(category=>({category,count:scenarios.filter(x=>x.scenario.category===category).length})),
  measuredCalls:installedCalls.size,references:referenceKeys.size,callableValueNegatives:negatives.size,
  typeRelationships:applicableRelationships.size,
  outcomes:outcomes.map(disposition=>({disposition,count:outcomeKeys.get(disposition).size})),
  observedRoles:roles.filter(role=>observed.has(role))};
 validate('CountsV1',count);
 if(fixture.profile==='example'&&count.floorsEnforced)fail('COUNT.PROFILE','profile','only literal example/ can bypass floors');
 if(count.floorsEnforced) {
  const required=roles.filter(x=>fixture.language!=='java'||x!=='alias');
  if(count.scenariosTotal<32||count.scenariosByCategory.some(x=>x.count<4)||count.measuredCalls<120||
     count.references<40||count.callableValueNegatives<20||count.typeRelationships<20||
     count.outcomes.some(x=>x.count<20)||required.some(x=>!observed.has(x)))
   fail('COUNT.FLOOR','counts','corpus-profile minimum not met');
 }
 return count;
}
