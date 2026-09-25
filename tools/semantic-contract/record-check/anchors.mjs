import {canonicalBytes} from '../json.mjs';
import {validate} from '../formats.mjs';
import {measuredHeaderHash, measuredSiblingGroupHash, orderedEnvelope, checkEnvelopeOrder} from './measurement.mjs';

const key = value => canonicalBytes(value).toString('hex');
const same = (a,b) => key(a) === key(b);
function reject(assertion,field,message) {
 const error = new Error(`${assertion} ${field}: ${message}`);
 Object.assign(error,{assertion,field,code:'invalidRecord'});
 throw error;
}
function closure(field,expected,actual) {
 checkEnvelopeOrder(field,actual);
 if(!same(orderedEnvelope(field,expected),actual))
  reject('ANCHOR.MEMBERSHIP',field,'measured anchor inventory or result differs');
}
function documentAt(loaded,document,revisionId) {
 return loaded.revisions.get(JSON.stringify([document.sourceSetId,revisionId]))?.documents
  .some(row => same(row.key,document)) ?? false;
}
function groupFor(measurement,ref) {
 const group = measurement.groupsByDeclarationRef.get(ref);
 if(!group || !group.memberRefs.length || group.memberRefs.length!==group.headers.length ||
    !group.memberRefs.includes(ref)) reject('ANCHOR.GROUP','groupsByDeclarationRef','missing measured sibling group');
 const headers = group.memberRefs.map(member => {
  const row=measurement.recordByNativeRef.get(member);
  if(!row?.header)reject('ANCHOR.GROUP','groupsByDeclarationRef','missing measured sibling');
  return measuredHeaderHash(row.header);
 });
 if(!same(headers,group.headers))reject('ANCHOR.GROUP','groupsByDeclarationRef','sibling hashes differ from measured headers');
 return headers;
}

export function checkAnchors(loaded,records,measurement) {
 if(!(measurement?.recordByNativeRef instanceof Map) ||
    !(measurement.groupsByDeclarationRef instanceof Map))
  reject('ANCHOR.GROUP','groupsByDeclarationRef','verified measurement inventory required');
 const declarations=loaded.native.declarations;
 const expectedAnchors=[],expectedContinuities=[],expectedResults=[];
 for(const anchorCase of loaded.anchors.cases) {
  const captured=declarations.find(row=>row.ref===anchorCase.capturedDeclarationRef);
  const currentRevisionId=anchorCase.currentRevisionId;
  if(!captured || !documentAt(loaded,captured.document,captured.revisionId) ||
     !documentAt(loaded,captured.document,currentRevisionId) ||
     loaded.comparison.revisionId!==currentRevisionId ||
     anchorCase.continuity.fromRevisionId!==captured.revisionId ||
     anchorCase.continuity.toRevisionId!==currentRevisionId)
   reject('ANCHOR.TUPLE','continuity','unadmitted document, revision or continuity');
  if(anchorCase.continuity.state==='unknown' && anchorCase.continuity.evidence!==null ||
     currentRevisionId!==captured.revisionId && anchorCase.continuity.state==='unchanged' &&
     (typeof anchorCase.continuity.evidence!=='string' || !anchorCase.continuity.evidence.trim()))
   reject('ANCHOR.CONTINUITY','continuity.evidence','unknown cannot assert proof; cross-revision unchanged needs independent proof');
  const old=measurement.recordByNativeRef.get(captured.ref);
  if(!old?.syntaxId || old.revisionId!==captured.revisionId || !same(old.document,captured.document))
   reject('ANCHOR.TUPLE','capturedDeclarationRef','missing captured measured declaration');
  const capturedHeaders=groupFor(measurement,captured.ref);
  const headerHash=measuredHeaderHash(old.header);
  const durable={syntaxId:old.syntaxId,document:old.document,capturedRevisionId:old.revisionId,
   headerHash,siblingGroupHash:measuredSiblingGroupHash(capturedHeaders),
   siblingCount:capturedHeaders.length,identicalHeaderCount:capturedHeaders.filter(x=>x===headerHash).length};
  validate('DurableAnchor',durable);
  expectedAnchors.push(durable);
  expectedContinuities.push(anchorCase.continuity);
  // Identity is the only candidate key. Never look up a replacement by name.
  const candidate=declarations.filter(row=>row.revisionId===currentRevisionId &&
    same(row.document,captured.document) &&
    measurement.recordByNativeRef.get(row.ref)?.syntaxId===old.syntaxId);
  if(candidate.length>1)reject('ANCHOR.TUPLE','syntaxId','ambiguous current measured identity');
  let reason='none';
  if(!candidate.length)reason='missing';
  else {
   const current=measurement.recordByNativeRef.get(candidate[0].ref);
   const currentHash=measuredHeaderHash(current.header);
   if(currentHash!==headerHash)reason='headerMismatch';
   else {
    const headers=groupFor(measurement,candidate[0].ref);
    const currentCount=headers.filter(x=>x===headerHash).length;
    if(durable.identicalHeaderCount>1 || currentCount>1) {
     if(durable.siblingGroupHash!==measuredSiblingGroupHash(headers) ||
        durable.siblingCount!==headers.length || durable.identicalHeaderCount!==currentCount ||
        currentRevisionId!==captured.revisionId && anchorCase.continuity.state==='changed') reason='groupChanged';
     else if(currentRevisionId!==captured.revisionId && anchorCase.continuity.state!=='unchanged')
      reason='unprovenContinuity';
    }
   }
  }
  const result={status:reason==='none'?'attached':'orphaned',
   targetId:reason==='none'?old.syntaxId:null,reason};
  validate('AnchorResult',result);
  expectedResults.push(result);
  const authored=anchorCase.expectedResult;
  const targetId=typeof authored.targetId==='object' && authored.targetId!==null
   ? measurement.identityByRef.get(authored.targetId.ref) : authored.targetId;
  if(!same({...authored,targetId},result))
   reject('ANCHOR.RESULT','expectedResult','authored case differs from source-derived result');
 }
 closure('durableAnchors',expectedAnchors,records.durableAnchors);
 closure('groupContinuities',expectedContinuities,records.groupContinuities);
 closure('anchorResults',expectedResults,records.anchorResults);
 return {durableAnchors:expectedAnchors,groupContinuities:expectedContinuities,anchorResults:expectedResults};
}
