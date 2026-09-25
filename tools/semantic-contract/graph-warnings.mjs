import {types} from './schema.mjs';
import {canonicalBytes} from './json.mjs';

const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
const codes=types.Warning.object.code.enum;
function fail(assertion,field,message){const error=new Error(`${assertion} ${field}: ${message}`);Object.assign(error,{assertion,code:'invalidRecord',field});throw error;}
function warningKeys(result){
 const keys=new Map();
 const add=(code,id)=>keys.set(JSON.stringify([code,id]),{code,provenanceId:id});
 if(result.request.semanticProducerId===null)add('syntaxOnly',null);
 if(result.coverage.some(row=>row.selected&&['failed','partial'].includes(row.state)))add('coverageIncomplete',null);
 for(const proof of result.provenance){
  if(proof.freshness==='stale')add('staleEvidence',proof.id);
  if(proof.freshness==='possiblyStale')add('staleEvidence',null);
 }
 for(const edge of result.edges){
  const binding=edge.binding;if(!binding)continue;
  if(binding.staleTarget===true)add('staleTarget',binding.provenanceId);
  if(binding.resolution==='ambiguous')add('bindingAmbiguous',binding.provenanceId);
 }
 return [...keys.values()].sort((a,b)=>codes.indexOf(a.code)-codes.indexOf(b.code)||
  (a.provenanceId===null?(b.provenanceId===null?0:-1):b.provenanceId===null?1:order(a.provenanceId,b.provenanceId)));
}
export function deriveWarnings(result){return warningKeys(result).map(({code,provenanceId})=>({code,provenanceId,message:code}));}
export function checkWarnings(result){
 if(!Array.isArray(result.warnings))fail('WARNING.KEYS','warnings','warnings must be an array');
 const expected=warningKeys(result),seen=new Set(),returned=new Set(result.provenance.map(row=>row.id));
 for(const edge of result.edges)if(edge.binding&& !returned.has(edge.binding.provenanceId))
  fail('WARNING.PROVENANCE','edges.binding.provenanceId','binding proof absent from returned provenance');
 for(let i=0;i<result.warnings.length;i++){
  const warning=result.warnings[i],key=JSON.stringify([warning.code,warning.provenanceId]);
  if(!codes.includes(warning.code)||typeof warning.message!=='string'||!warning.message.length||
    !(warning.provenanceId===null||typeof warning.provenanceId==='string'&&warning.provenanceId.length>0))
   fail('WARNING.SHAPE',`warnings[${i}]`,'invalid code, provenance or empty message');
  if(seen.has(key))fail('WARNING.DUPLICATE',`warnings[${i}]`,'duplicate warning key');
  seen.add(key);
 }
 const actualKeys=result.warnings.map(({code,provenanceId})=>({code,provenanceId}));
 if(canonicalBytes(actualKeys).toString('hex')!==canonicalBytes(expected).toString('hex'))
  fail('WARNING.KEYS','warnings','missing, extra or enum-order misordered warning key');
 return true;
}
