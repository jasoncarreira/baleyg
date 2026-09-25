// Graph-only view of validated normalized facts against the requested admitted revision.
// Normalized records keep their fixture comparison-relative freshness labels.
import {sourceManifestHash} from './identity.mjs';

const tuple=(set,revision)=>JSON.stringify([set,revision]);
const documentKey=d=>JSON.stringify([d.sourceSetId,d.language,d.path]);
const same=(a,b)=>JSON.stringify(a)===JSON.stringify(b);
function missing(field){
 const error=new Error(`GRAPH.PROJECTION ${field}: normalized graph records are incomplete`);
 Object.assign(error,{assertion:'GRAPH.PROJECTION',code:'invalidRecord',field});
 throw error;
}

export function graphProjection(records,request){
 if(!records?.comparison||!Array.isArray(records.comparison.producers))missing('comparison');
 if(!Array.isArray(records.revisions)||!records.revisions.every(r=>Array.isArray(r.documents)))missing('revisions.documents');
 if(!Array.isArray(records.declarations))missing('declarations');
 if(!Array.isArray(records.producers))missing('producers');
 const requested=records.revisions.find(r=>r.sourceSetId===request.sourceSetId&&r.id===request.revisionId);
 if(!requested)missing('revisionId');
 const captures=new Map(records.revisions.map(r=>[tuple(r.sourceSetId,r.id),r]));
 const declarations=new Set(records.declarations.filter(d=>d.revisionId===request.revisionId&&
  d.document.sourceSetId===request.sourceSetId).map(d=>JSON.stringify([d.syntaxId,documentKey(d.document)])));
 const comparison=records.comparison;
 const matchesComparison=comparison?.sourceSetId===request.sourceSetId&&comparison?.revisionId===request.revisionId;
 const producers=matchesComparison?comparison.producers:records.producers;
 const document=d=>requested?.documents.find(row=>documentKey(row.key)===documentKey(d));
 function proof(row){
  if(matchesComparison)return row;
  const wanted=document(row.document);
  let freshness='fresh';
  if(!wanted||wanted.contentHash!==row.contentHash)freshness='stale';
  else if(row.revisionId!==request.revisionId||row.document.sourceSetId!==request.sourceSetId)freshness='possiblyStale';
  else if(row.basis){
   const basis=row.basis,producer=producers?.find(p=>p.id===basis.producerId);
   if(!producer||producer.kind!=='semantic'||!producer.languages.includes(basis.language)||
      producer.version!==basis.producerVersion||producer.executableHash!==basis.producerHash||
      !same(producer.languages,records.producers.find(p=>p.id===basis.producerId)?.languages)||
      producer.positionEncoding!==records.producers.find(p=>p.id===basis.producerId)?.positionEncoding||
      sourceManifestHash(requested.documents.map(d=>({document:d.key,contentHash:d.contentHash})))!==basis.sourceManifestHash||
      requested.toolchainHash!==basis.toolchainHash||requested.configHash!==basis.configHash||
      requested.dependencyHash!==basis.dependencyHash)freshness='possiblyStale';
  }
  return {...row,freshness};
 }
 function binding(row){
  if(matchesComparison||row.declaredTarget?.kind!=='internal')return row;
  const target=row.declaredTarget,captured=captures.get(tuple(target.document.sourceSetId,target.revisionId))
   ?.documents.find(d=>documentKey(d.key)===documentKey(target.document));
  const wanted=document(target.document);
  return {...row,staleTarget:!captured||!wanted||wanted.contentHash!==captured.contentHash||
   !declarations.has(JSON.stringify([target.syntaxId,documentKey(wanted.key)]))};
 }
 return {proof,binding};
}
