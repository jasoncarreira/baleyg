import {canonicalBytes} from '../json.mjs';
import {validate} from '../formats.mjs';
import {checkEnvelopeOrder,orderedEnvelope} from './measurement.mjs';

const bytes=value=>canonicalBytes(value);
const key=value=>bytes(value).toString('hex');
const same=(a,b)=>key(a)===key(b);
const compare=(a,b)=>Buffer.compare(bytes(a),bytes(b));
function reject(assertion,field,reason) {
 const error=new Error(`${assertion} ${field}: ${reason}`);
 Object.assign(error,{assertion,code:'invalidRecord',field});
 throw error;
}
function uniqueTargets(rows,field) {
 const values=[...rows].sort(compare);
 if(values.some((row,i)=>i>0&&compare(values[i-1],row)===0))
  reject('BINDING.TARGET',field,'duplicate target');
 return values;
}
function cardinality(value) {
 const valid=value.resolution==='resolved'&&value.declaredTarget?.kind==='internal'&&!value.candidates.length ||
  value.resolution==='external'&&value.declaredTarget?.kind==='external'&&!value.candidates.length ||
  value.resolution==='ambiguous'&&value.declaredTarget===null&&value.candidates.length>=2 ||
  value.resolution==='unresolved'&&value.declaredTarget===null&&!value.candidates.length;
 if(!valid)reject('BINDING.CARDINALITY','resolution','resolution and target cardinality differ');
}

// Fact membership and target identities are derived from admitted captures, never normalized rows.
export function checkBindings(loaded,records,C,M,J) {
 validate('NormalizedRecordsV1',records);
 if(!(C?.semanticProofsById instanceof Map)||typeof C.checkUse!=='function'||typeof C.checkTarget!=='function'||
    !(M?.recordByNativeRef instanceof Map)||!(J?.joined instanceof Map))
  reject('BINDING.FACT','callBindings','checked coverage, measurement and joins required');
 const proofById=new Map(records.provenance.map(row=>[row.id,row]));
 const nativeDeclarations=new Map(loaded.native.declarations.map(row=>[row.ref,row]));
 const verifiedDeclarations=new Map();
 for(const native of loaded.native.declarations) {
  const measured=M.recordByNativeRef.get(native.ref),snapshot=JSON.stringify([native.document.sourceSetId,native.revisionId]);
  if(!measured?.syntaxId||!same(measured.document,native.document)||measured.revisionId!==native.revisionId)
   reject('BINDING.TARGET','declaredTarget','target declaration lacks measured identity');
  if(!verifiedDeclarations.has(snapshot))verifiedDeclarations.set(snapshot,new Map());
  verifiedDeclarations.get(snapshot).set(measured.syntaxId,measured.document);
 }
 const resolve=(ref,proof,field)=>{
  if(ref===null)return null;
  if(ref.kind==='external') {
   if((ref.symbol.scope==='global')!==(ref.symbol.document===null))
    reject('BINDING.TARGET',field,'external symbol scope and document differ');
   if(ref.symbol.scope==='document'&&!same(ref.symbol.document,proof.document))
    reject('BINDING.TARGET',field,'document symbol outside proven document');
   return {kind:'external',symbol:ref.symbol};
  }
  const native=nativeDeclarations.get(ref.declarationRef),measured=M.recordByNativeRef.get(ref.declarationRef);
  if(!native||!measured?.syntaxId||native.revisionId!==ref.revisionId||
     measured.revisionId!==native.revisionId||!same(native.document,measured.document)||
     native.document.sourceSetId!==proof.document.sourceSetId)
   reject('BINDING.TARGET',field,'target must name measured declaration in its source set and revision');
  return {kind:'internal',syntaxId:measured.syntaxId,document:measured.document,revisionId:measured.revisionId};
 };
 const groups=new Map(),recordByFactRef=new Map(),seen=new Set();
 for(const annotation of loaded.annotations)for(const fact of annotation.facts) {
  if(fact.kind!=='callBinding')continue;
  if(seen.has(fact.ref))reject('BINDING.FACT','factRef','duplicate call fact reference');
  seen.add(fact.ref);
  const joined=J.joined.get(fact.ref),proof=proofById.get(fact.record.provenanceId);
  const captured=C.semanticProofsById.get(fact.record.provenanceId);
  const strip=({freshness,...row})=>row;
  if(!proof||!captured||proof.evidenceKind!=='semanticReference'||
     !same(strip(proof),strip(captured))||!same(proof.document,annotation.document)||
     proof.revisionId!==annotation.revisionId||
     loaded.semanticProofs.get(proof.id)?.factRef!==fact.ref||
     loaded.semanticProofs.get(proof.id)?.factKind!=='callBinding')
   reject('BINDING.FACT','provenanceId','call fact lacks its admitted proof and source tuple');
  if(!joined||joined.producerId!==proof.producerId||!same(joined.provenanceIds,[proof.id])||
     !['callee','invocation'].includes(joined.join.anchor.kind)||
     !same(joined.join.anchor.document,proof.document)||joined.join.anchor.revisionId!==proof.revisionId||
     joined.join.anchor.contentHash!==proof.contentHash)
   reject('BINDING.JOIN','join','fact lacks producer-specific measured call join');
  C.checkUse({producerId:proof.producerId,document:proof.document,revisionId:proof.revisionId,provenanceIds:[proof.id]});
  const exact=joined.join.status==='exact',call=exact?
   loaded.native.calls.filter(row=>joined.nativeRefs.includes(row.ref)&&
    M.recordByNativeRef.get(row.ref)?.id===joined.installedId):[];
  if(exact&&(joined.join.candidateIds.length!==1||joined.installedId!==joined.join.candidateIds[0]||
      call.length!==1||!same(call[0].document,proof.document)||call[0].revisionId!==proof.revisionId)||
     !exact&&(joined.installedId!==null||joined.nativeRefs.length!==0))
   reject('BINDING.JOIN','join','non-exact join installed a call or exact join lacks one measured call');
  const raw=fact.record;
  if(raw.possibleDispatchComplete!==false)
   reject('BINDING.DISPATCH','possibleDispatchComplete','possible dispatch is never complete');
  if(!['direct','constructor','virtual','interface','dynamic','unknown'].includes(raw.dispatch))
   reject('BINDING.DISPATCH','dispatch','unknown dispatch class');
  const declaredTarget=resolve(raw.declaredTarget,proof,'declaredTarget');
  const candidates=uniqueTargets(raw.candidates.map(x=>resolve(x,proof,'candidates')),'candidates');
  const possibleDispatch=uniqueTargets(raw.possibleDispatch.map(x=>resolve(x,proof,'possibleDispatch')),'possibleDispatch');
  const value={callId:exact?joined.installedId:null,join:joined.join,resolution:raw.resolution,
   declaredTarget,candidates,dispatch:raw.dispatch,possibleDispatch,
   possibleDispatchComplete:false,staleTarget:null,provenanceId:proof.id};
  cardinality(value);
  if(value.resolution==='ambiguous'&&!same(value.candidates,raw.candidates.map(x=>resolve(x,proof,'candidates'))))
   reject('BINDING.CARDINALITY','candidates','ambiguous candidates must be sorted and unique');
  if(declaredTarget?.kind==='internal') {
   const capturedTarget=loaded.revisions.get(JSON.stringify([declaredTarget.document.sourceSetId,declaredTarget.revisionId]))?.documents.find(x=>same(x.key,declaredTarget.document));
   const selected=loaded.selected.documents.find(x=>same(x.key,declaredTarget.document));
   const current=verifiedDeclarations.get(JSON.stringify([loaded.comparison.sourceSetId,loaded.comparison.revisionId]))?.get(declaredTarget.syntaxId);
   if(!capturedTarget)reject('BINDING.TARGET','declaredTarget','target is outside admitted source snapshot');
   value.staleTarget=!selected||selected.contentHash!==capturedTarget.contentHash||!current||!same(current,selected.key);
  }
  C.checkTarget(value,proof,verifiedDeclarations);
  validate('CallBinding',value);
  const groupAnchor=exact?null:{...joined.join.anchor,ownerRef:fact.anchor.ownerRef};
  const groupKey=key([proof.producerId,proof.document,proof.revisionId,
   exact?joined.installedId:groupAnchor]);
  if(!groups.has(groupKey))groups.set(groupKey,{producerId:proof.producerId,document:proof.document,
   revisionId:proof.revisionId,callId:exact?joined.installedId:null,anchor:groupAnchor,
   factRefs:[],provenanceIds:[],targetProofs:[],historicalTuples:[],members:[],entries:[]});
  const group=groups.get(groupKey);
  group.entries.push({factRef:fact.ref,value,proof});
 }
 const expected=[];
 for(const group of groups.values()) {
  const entries=group.entries,first=entries[0].value;
  group.factRefs=entries.map(x=>x.factRef);
  group.provenanceIds=[...new Set(entries.map(x=>x.proof.id))];
  group.targetProofs=entries.map(x=>({factRef:x.factRef,provenanceId:x.proof.id,
   declaredTarget:x.value.declaredTarget,candidates:x.value.candidates,
   possibleDispatch:x.value.possibleDispatch,proof:x.proof}));
  group.historicalTuples=entries.map(x=>({factRef:x.factRef,producerId:x.proof.producerId,
   document:x.proof.document,revisionId:x.proof.revisionId,freshness:x.proof.freshness}));
  const unique=[...new Map(entries.map(x=>[key(x.value),x.value])).values()];
  if(unique.length>1) {
   const consistent=unique.every(x=>x.callId===first.callId&&same(x.join,first.join)&&
    x.dispatch===first.dispatch&&same(x.possibleDispatch,first.possibleDispatch));
   if(!consistent)reject('BINDING.CONTRADICTION','callBindings','contributor non-target claims differ');
   const claims=unique.map(x=>x.declaredTarget);
   const distinct=[...new Map(claims.map(x=>[key(x),x])).values()];
   const contradiction=distinct.length>1;
   if(contradiction) {
    if(group.callId===null||unique.some(x=>x.resolution!=='resolved'||x.declaredTarget?.kind!=='internal'||x.candidates.length)||
       new Set(unique.map(x=>x.provenanceId)).size!==unique.length)
     reject('BINDING.CONTRADICTION','declaredTarget','contradictory targets need distinct exact internal proofs');
    const union=distinct.sort(compare);
    for(const x of unique)Object.assign(x,{resolution:'ambiguous',declaredTarget:null,candidates:union,staleTarget:null});
   } else if(unique.some(x=>!same({...x,provenanceId:null},{...first,provenanceId:null})))
    reject('BINDING.CONTRADICTION','declaredTarget','incompatible duplicate semantic claims');
  }
  group.members=orderedEnvelope('callBindings',unique,{collapseIdentical:true});
  expected.push(...group.members);
  for(const entry of entries) {
   const member=group.members.find(x=>x.provenanceId===entry.proof.id);
   if(!member)reject('BINDING.CONTRIBUTORS','provenanceId','missing contributor proof member');
   recordByFactRef.set(entry.factRef,member);
  }
  delete group.entries;
 }
 for(const member of expected) {
  const supplied=records.callBindings.find(x=>x.provenanceId===member.provenanceId);
  if(!supplied)continue;
  if(!same(supplied.join,member.join)||supplied.callId!==member.callId)
   reject('BINDING.JOIN','join','output join or installed call differs');
  if(supplied.dispatch!==member.dispatch||!same(supplied.possibleDispatch,member.possibleDispatch)||
     supplied.possibleDispatchComplete!==false)
   reject('BINDING.DISPATCH','dispatch','output dispatch differs');
  if(supplied.resolution!==member.resolution||!same(supplied.candidates,member.candidates)||
     !same(supplied.declaredTarget,member.declaredTarget))
   reject(member.resolution==='ambiguous'&&groupHasContradiction(groups,member)?'BINDING.CONTRADICTION':'BINDING.CARDINALITY',
    member.resolution==='ambiguous'?'candidates':'resolution','output resolution or target union differs');
  if(supplied.staleTarget!==member.staleTarget)
   reject('BINDING.TARGET','staleTarget','target freshness differs');
 }
 checkEnvelopeOrder('callBindings',records.callBindings);
 if(!same(orderedEnvelope('callBindings',expected,{collapseIdentical:true}),records.callBindings))
  reject('BINDING.CONTRIBUTORS','callBindings','captured fact members and normalized binding inventory differ');
 return {callBindings:orderedEnvelope('callBindings',expected,{collapseIdentical:true}),recordByFactRef,groups};
}
function groupHasContradiction(groups,member) {
 return [...groups.values()].some(group=>group.members.includes(member)&&
  group.targetProofs.some(x=>x.declaredTarget?.kind==='internal'&&
   group.targetProofs.some(y=>y.declaredTarget?.kind==='internal'&&!same(x.declaredTarget,y.declaredTarget))));
}
