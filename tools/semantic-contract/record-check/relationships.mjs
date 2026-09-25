import {canonicalBytes} from '../json.mjs';
import {validate} from '../formats.mjs';
import {checkEnvelopeOrder,orderedEnvelope} from './measurement.mjs';

const key = value => canonicalBytes(value).toString('hex');
const same = (a,b) => key(a) === key(b);
function reject(assertion,field,reason) {
 const error = new Error(`${assertion} ${field}: ${reason}`);
 Object.assign(error,{assertion,field,code:'invalidRecord'});
 throw error;
}
function symbolKey(value,field) {
 if ((value.scope === 'global') !== (value.document === null))
  reject('SYMBOL.SCOPE',field,'global symbols have no document; document symbols have their exact document');
}
function close(field,expected,actual) {
 checkEnvelopeOrder(field,actual);
 if (!same(orderedEnvelope(field,expected,{collapseIdentical:true}),actual))
  reject('RECORDS.MEMBERSHIP',field,'captured fact inventory differs from normalized records');
}

export function checkRelationships(loaded,records,C,M,J) {
 validate('NormalizedRecordsV1',records);
 if (!(C?.semanticProofsById instanceof Map) || typeof C.checkUse !== 'function' ||
     !(M?.recordByNativeRef instanceof Map) || !(J?.joined instanceof Map))
  reject('RECORDS.MEMBERSHIP','typeRelationships','checked coverage, measurement and joins required');
 const proofById = new Map(records.provenance.map(row=>[row.id,row]));
 const nativeDeclarations = new Map(loaded.native.declarations.map(row=>[row.ref,row]));
 const symbols=[],declarationBindings=[],typeRelationships=[],recordByFactRef=new Map();
 const symbolClaims=new Map(),factRefs=new Set();
 function proofFor(fact,annotation,kind) {
  const id=fact.kind==='typeRelationship'?fact.provenanceRef:fact.record.provenanceId;
  const proof=proofById.get(id),captured=C.semanticProofsById.get(id);
  if (!proof || !captured || proof.evidenceKind!==kind || !same(proof.document,annotation.document) ||
      proof.revisionId!==annotation.revisionId)
   reject(fact.kind==='typeRelationship'?'RELATIONSHIP.PROOF':fact.kind==='symbol'?'SYMBOL.FACT':'DECLARATION_BINDING.PROOF',
    'provenanceId','wrong evidence kind or captured tuple');
  const strip=({freshness,...rest})=>rest;
  if (!same(strip(captured),strip(proof)) ||
      loaded.semanticProofs.get(id)?.factRef!==fact.ref ||
      loaded.semanticProofs.get(id)?.factKind!==fact.kind)
   reject(fact.kind==='typeRelationship'?'RELATIONSHIP.PROOF':fact.kind==='symbol'?'SYMBOL.FACT':'DECLARATION_BINDING.PROOF',
    'provenanceId','proof does not authenticate this source fact');
  C.checkUse({producerId:proof.producerId,document:annotation.document,revisionId:annotation.revisionId,provenanceIds:[id]});
  return proof;
 }
 function resolve(ref,context,field,assertion) {
  if(ref.kind==='external') {symbolKey(ref.symbol,field);return {kind:'external',symbol:ref.symbol};}
  const native=nativeDeclarations.get(ref.declarationRef),measured=M.recordByNativeRef.get(ref.declarationRef);
  if(!native || !measured?.syntaxId || ref.revisionId!==native.revisionId ||
     !same(native.document,measured.document) || native.document.sourceSetId!==context.sourceSetId)
   reject(assertion,field,'internal target must name a measured declaration in the source set and revision');
  return {kind:'internal',syntaxId:measured.syntaxId,document:measured.document,revisionId:measured.revisionId};
 }
 for(const annotation of loaded.annotations)for(const fact of annotation.facts) {
  if(!['symbol','declarationBinding','typeRelationship'].includes(fact.kind))continue;
  if(factRefs.has(fact.ref))reject('RECORDS.MEMBERSHIP','factRef','duplicate fact reference');
  factRefs.add(fact.ref);
  const proof=proofFor(fact,annotation,fact.kind==='typeRelationship'?'typeRelationship':'declarationBinding');
  if(fact.kind==='symbol') {
   symbolKey(fact.record.key,'key');
   if(fact.record.key.scope==='document' && !same(fact.record.key.document,proof.document))
    reject('SYMBOL.SCOPE','key','document-scoped symbol must match its captured document');
   const declarations=fact.record.declarations.map(ref=>resolve(ref,proof.document,'declarations','SYMBOL.TARGET'));
   if(declarations.some(row=>row.kind==='internal' && row.revisionId!==proof.revisionId))
    reject('SYMBOL.TARGET','declarations','symbol target is outside the proven revision');
   if(new Set(declarations.map(key)).size!==declarations.length)reject('SYMBOL.TARGET','declarations','duplicate explicit declaration target');
   const value={key:fact.record.key,displayName:fact.record.displayName,declarations,provenanceId:proof.id};
   const claim=key([proof.producerId,value.key]),prior=symbolClaims.get(claim);
   if(prior&&!same(prior,value))reject('SYMBOL.FACT','symbols','conflicting symbol key in producer');
   symbolClaims.set(claim,value);symbols.push(value);recordByFactRef.set(fact.ref,value);
  } else if(fact.kind==='declarationBinding') {
   const joined=J.joined.get(fact.ref);
   if(!joined || !same(joined.provenanceIds,[proof.id]) || joined.producerId!==proof.producerId ||
       joined.join.anchor.kind!=='declarationName' || !same(joined.join.anchor.document,annotation.document) ||
       joined.join.anchor.revisionId!==annotation.revisionId || joined.join.anchor.contentHash!==proof.contentHash)
    reject('DECLARATION_BINDING.JOIN','join','measured declaration-name join or source tuple differs');
   const exact=joined.join.status==='exact';
   if(exact && (joined.join.candidateIds.length!==1 || joined.installedId!==joined.join.candidateIds[0] ||
       !joined.nativeRefs.some(ref=>M.recordByNativeRef.get(ref)?.syntaxId===joined.installedId)))
    reject('DECLARATION_BINDING.JOIN','join','exact join needs its one measured declaration');
   if(!exact && joined.installedId!==null)reject('DECLARATION_BINDING.JOIN','join','non-exact join installed an identity');
   for(const symbol of fact.record.symbols)symbolKey(symbol,'symbols');
   if(exact&&!fact.record.symbols.length || new Set(fact.record.symbols.map(key)).size!==fact.record.symbols.length)
    reject('DECLARATION_BINDING.JOIN','symbols','exact binding requires unique explicit symbols');
   const value={syntaxId:exact?joined.installedId:null,symbols:fact.record.symbols,join:joined.join,provenanceId:proof.id};
   declarationBindings.push(value);recordByFactRef.set(fact.ref,value);
  } else {
   if(!['extends','implements','overrides'].includes(fact.relationshipKind) || fact.kind!=='typeRelationship')
    reject('RELATIONSHIP.KIND','kind','independent relationship kind required');
   if(fact.source.kind!=='internal')reject('RELATIONSHIP.SOURCE','source','relationship source must be internal');
   const source=resolve(fact.source,proof.document,'source','RELATIONSHIP.SOURCE');
   if(!same(source.document,annotation.document) || source.revisionId!==annotation.revisionId)
    reject('RELATIONSHIP.SOURCE','source','relationship source must be in the proven source snapshot');
   const target=resolve(fact.target,proof.document,'target','RELATIONSHIP.TARGET');
   // The captured producer supplies the relationship. Its evidence tuple names the
   // measured source, while the measured target may live in another document.
   // A header base, marker, name match, or language-specific spelling cannot
   // independently prove or disprove the semantic edge.
   if(target.kind==='internal' && target.revisionId!==proof.revisionId)
    reject('RELATIONSHIP.TARGET','target','internal target is outside the proven revision');
   const native=nativeDeclarations.get(fact.source.declarationRef);
   const typeSource=['type','implementation'].includes(native.kind);
   if(fact.relationshipKind==='overrides'?!['method','function'].includes(native.kind):!typeSource)
    reject('RELATIONSHIP.SOURCE','source','source declaration kind contradicts directed relationship');
   if(target.kind==='internal') {
    const base=nativeDeclarations.get(fact.target.declarationRef);
    if(fact.relationshipKind==='overrides'?!['method','function'].includes(base.kind):!['type','implementation'].includes(base.kind))
     reject('RELATIONSHIP.TARGET','target','target declaration kind contradicts directed relationship');
   }
   if(target.kind==='internal' && same(source,target))reject('RELATIONSHIP.TARGET','target','relationship cannot target itself');
   const value={kind:fact.relationshipKind,source,target,provenanceId:proof.id};
   typeRelationships.push(value);recordByFactRef.set(fact.ref,value);
  }
 }
 for(const expected of symbols) {
  const supplied=records.symbols.find(row=>row.provenanceId===expected.provenanceId);
  if(!supplied)continue;
  symbolKey(supplied.key,'key');
  if(supplied.key.scope==='document' && !same(supplied.key.document,expected.key.document))
   reject('SYMBOL.SCOPE','key','document scope disagrees with captured source');
  if(!same(supplied.key,expected.key) || supplied.displayName!==expected.displayName)
   reject('SYMBOL.FACT','symbols','key or display name contradicts fact');
  if(!same(supplied.declarations,expected.declarations))
   reject('SYMBOL.TARGET','declarations','declarations differ from captured targets');
 }
 for(const expected of declarationBindings) {
  const supplied=records.declarationBindings.find(row=>row.provenanceId===expected.provenanceId);
  if(!supplied) {
   if(records.declarationBindings.some(row=>same(row.join,expected.join)&&row.syntaxId===expected.syntaxId))
    reject('DECLARATION_BINDING.PROOF','provenanceId','wrong binding proof');
   continue;
  }
  if(!same(supplied.join,expected.join)||supplied.syntaxId!==expected.syntaxId||!same(supplied.symbols,expected.symbols))
   reject('DECLARATION_BINDING.JOIN','join','measured join or explicit symbol claim differs');
 }
 for(const expected of typeRelationships) {
  const supplied=records.typeRelationships.find(row=>row.provenanceId===expected.provenanceId);
  if(!supplied) {
   if(records.typeRelationships.some(row=>same(row.source,expected.source)&&same(row.target,expected.target)&&row.kind===expected.kind))
    reject('RELATIONSHIP.PROOF','provenanceId','wrong relationship proof');
   continue;
  }
  if(supplied.kind!==expected.kind)reject('RELATIONSHIP.KIND','kind','normalized relationshipKind differs');
  if(!same(supplied.source,expected.source))reject('RELATIONSHIP.SOURCE','source','normalized direction or source differs');
  if(!same(supplied.target,expected.target))reject('RELATIONSHIP.TARGET','target','normalized target differs');
 }
 close('symbols',symbols,records.symbols);
 close('declarationBindings',declarationBindings,records.declarationBindings);
 close('typeRelationships',typeRelationships,records.typeRelationships);
 return {symbols:orderedEnvelope('symbols',symbols,{collapseIdentical:true}),
  declarationBindings:orderedEnvelope('declarationBindings',declarationBindings,{collapseIdentical:true}),
  typeRelationships:orderedEnvelope('typeRelationships',typeRelationships,{collapseIdentical:true}),recordByFactRef};
}
