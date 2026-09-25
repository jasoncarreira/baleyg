import {graphProjection} from './graph-projection.mjs';

// Structural graph decisions only. Evidence selection and warning completeness are separate checks.
const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
const documentKey=d=>JSON.stringify([d.sourceSetId,d.language,d.path]);
const failure=(code,field,message)=>({ok:false,error:{code,field,message}});
const numeric=[['depth',2,0,5],['maxNodes',150,1,150],['maxCalls',500,0,500]];

export function traverseGraph({request,records,selectedCoverageIncomplete=false}) {
 const effective={...request};
 for(const [field,defaultValue,min,max] of numeric){
  if(effective[field]===undefined)effective[field]=defaultValue;
  if(!Number.isSafeInteger(effective[field])||effective[field]<min||effective[field]>max)
   return failure('invalidRequest',field,`${field} must be an integer from ${min} to ${max}`);
 }
 if(typeof effective.sourceSetId!=='string'||!effective.sourceSetId||
    typeof effective.revisionId!=='string'||!effective.revisionId||
    typeof effective.rootSyntaxId!=='string'||!/^sid:v1:[0-9a-f]{32}$/.test(effective.rootSyntaxId)||
    !(effective.semanticProducerId===null||typeof effective.semanticProducerId==='string'&&effective.semanticProducerId.length>0))
  return failure('invalidRequest','request','A source set, revision, root syntax ID and nullable producer are required');
 if(!records.sourceSets.some(s=>s.id===effective.sourceSetId))
  return failure('sourceSetDenied','sourceSetId','Source set is not admitted');
 if(!records.revisions.some(r=>r.id===effective.revisionId&&r.sourceSetId===effective.sourceSetId))
  return failure('revisionUnavailable','revisionId','Pinned revision is unavailable');
 const declarations=records.declarations.filter(d=>d.revisionId===effective.revisionId&&d.document.sourceSetId===effective.sourceSetId);
 const byId=new Map(declarations.map(d=>[d.syntaxId,d]));
 const root=byId.get(effective.rootSyntaxId);
 if(!root)return failure('rootMissing','rootSyntaxId','Root is absent from the pinned revision');
 if(effective.semanticProducerId!==null&&!records.producers.some(p=>p.id===effective.semanticProducerId&&p.kind==='semantic'))
  return failure('producerUnavailable','semanticProducerId','Semantic producer is unavailable');
 const projection=graphProjection(records,effective);
 const proof=new Map(records.provenance.map(row=>[row.id,projection.proof(row)]));
 const coverage=new Map(records.coverage.map(row=>[JSON.stringify([row.producerId,row.sourceSetId,row.language,row.documentPath,row.revisionId]),row]));
 const rowFor=(producerId,document)=>coverage.get(JSON.stringify([producerId,document.sourceSetId,document.language,document.path,effective.revisionId]));
 const calls=new Map();
 for(const call of records.calls){
  if(call.revisionId!==effective.revisionId||call.document.sourceSetId!==effective.sourceSetId||!byId.has(call.ownerSyntaxId))continue;
  const list=calls.get(call.ownerSyntaxId)??[];list.push(call);calls.set(call.ownerSyntaxId,list);
 }
 for(const list of calls.values())list.sort((a,b)=>order(a.document.path,b.document.path)||a.range.start-b.range.start||a.range.end-b.range.end||order(a.id,b.id));
 const bindings=new Map();
 for(const binding of records.callBindings){
  if(binding.callId===null||effective.semanticProducerId===null)continue;
  const p=proof.get(binding.provenanceId);
  if(p?.producerId!==effective.semanticProducerId||p.revisionId!==effective.revisionId||
     binding.join.status!=='exact'||binding.join.candidateIds.length!==1||
     binding.join.candidateIds[0]!==binding.callId||binding.join.anchor.revisionId!==effective.revisionId)continue;
  const members=bindings.get(binding.callId)??[];members.push(binding);bindings.set(binding.callId,members);
 }
 const nodes=[{declaration:root,depth:0}],edges=[],frontier=[],admitted=new Set([root.syntaxId]);
 let stopped=false,head=0;
 while(head<nodes.length){
  const {declaration,depth}=nodes[head++],local=calls.get(declaration.syntaxId)??[];
  if(!local.length)continue;
  if(depth===effective.depth){
   frontier.push({reason:'depth',nodeId:declaration.syntaxId,callId:null,targetId:null,nextOrdinal:0,omittedCalls:local.length});
   continue;
  }
  if(stopped){
   frontier.push({reason:'callLimit',nodeId:declaration.syntaxId,callId:null,targetId:null,nextOrdinal:0,omittedCalls:local.length});
   continue;
  }
  for(let i=0;i<local.length;i++){
   if(edges.length===effective.maxCalls){
    frontier.push({reason:'callLimit',nodeId:declaration.syntaxId,callId:null,targetId:null,nextOrdinal:local[i].ordinal,omittedCalls:local.length-i});
    stopped=true;break;
   }
   const call=local[i],members=bindings.get(call.id)??[];
   const selected=members.find(binding=>documentKey(binding.join.anchor.document)===documentKey(call.document)&&
    binding.join.anchor.contentHash===proof.get(binding.provenanceId)?.contentHash)??null;
   const covered=effective.semanticProducerId!==null&&rowFor(effective.semanticProducerId,call.document);
   const binding=covered?.selected&&['complete','partial'].includes(covered.state)&&selected?projection.binding(selected):null;
   const p=binding&&proof.get(binding.provenanceId);
   let reason='none';
   if(!binding)reason='missingEvidence';
   else if(p?.freshness!=='fresh'||(binding.declaredTarget?.kind==='internal'&&binding.staleTarget!==false))reason='stale';
   else if(binding.resolution==='ambiguous')reason='ambiguous';
   else if(binding.resolution==='unresolved')reason='unresolved';
   else if(binding.resolution==='external')reason='external';
   else if(!['direct','constructor'].includes(binding.dispatch))reason='dispatch';
   const target=reason==='none'?binding.declaredTarget:null;
   // A captured target can precede the pinned revision: staleTarget checks its bytes,
   // while stable syntax identity and document membership locate its current body.
   if(reason==='none'&&(!target||target.kind!=='internal'||
      !byId.has(target.syntaxId)||documentKey(byId.get(target.syntaxId).document)!==documentKey(target.document)))reason='stale';
   let to=null,visit='boundary';
   if(reason==='none'){
    if(admitted.has(target.syntaxId)){to=target.syntaxId;visit='seen';}
    else if(nodes.length===effective.maxNodes){
     reason='nodeLimit';
     frontier.push({reason:'nodeLimit',nodeId:declaration.syntaxId,callId:call.id,targetId:target.syntaxId,nextOrdinal:null,omittedCalls:0});
    }else{
     to=target.syntaxId;visit='new';admitted.add(to);
     nodes.push({declaration:byId.get(to),depth:depth+1});
    }
   }
   edges.push({call,from:declaration.syntaxId,to,binding,visit,boundaryReason:reason});
  }
 }
 const truncated=frontier.length>0;
 const relevantDocuments=new Set([...nodes.map(n=>documentKey(n.declaration.document)),...edges.map(e=>documentKey(e.call.document))]);
 const incomplete=selectedCoverageIncomplete||records.coverage.some(row=>row.revisionId===effective.revisionId&&
  row.sourceSetId===effective.sourceSetId&&row.selected&&['failed','partial'].includes(row.state)&&
  (effective.semanticProducerId===row.producerId||records.producers.some(p=>p.id===row.producerId&&p.kind==='native'))&&
  relevantDocuments.has(documentKey({sourceSetId:row.sourceSetId,language:row.language,path:row.documentPath})));
 return {ok:true,result:{request:effective,resolvedRevisionId:effective.revisionId,nodes,edges,frontier,
  coverage:[],provenance:[],partial:truncated||incomplete||edges.some(e=>e.visit==='boundary'),truncated,warnings:[]}};
}
