import {validate} from './formats.mjs';
import {contentHash} from './identity.mjs';

const tuple = (sourceSetId,revisionId) => JSON.stringify([sourceSetId,revisionId]);
const docKey = (sourceSetId,revisionId,path) => JSON.stringify([sourceSetId,revisionId,path]);
function assert(condition,id,field,message) {
  if (condition) return;
  const error=new Error(`${id} ${field}: ${message}`);
  error.assertion=id; error.code='invalidRecord'; error.field=field;
  throw error;
}
function capturedDocument(provenance,loaded) {
  const {sourceSetId,path}=provenance.document;
  const revision=loaded.revisions.get(tuple(sourceSetId,provenance.revisionId));
  const document=revision?.documents.find(x=>x.key.path===path && x.key.language===provenance.document.language);
  assert(document,'BASIS.DOCUMENT','document','captured document not admitted');
  assert(document.contentHash===provenance.contentHash,'BASIS.CONTENT_HASH','contentHash','captured content differs');
  return {revision,document};
}
function captureExists(loaded,kind,hash) {
  return loaded.fixture.captures.some(x=>x.kind===kind && x.hash===hash && contentHash(loaded.captureBytes.get(x.ref))===hash);
}

// Check captured claims before examining any requested snapshot or deciding freshness.
export function checkCapturedBasis(provenance,loaded) {
  validate('Provenance',provenance);
  const {revision}=capturedDocument(provenance,loaded);
  const producer=loaded.fixture.producers.find(x=>x.id===provenance.producerId);
  assert(producer,'BASIS.PRODUCER','producerId','captured producer not admitted');
  assert(captureExists(loaded,'executable',producer.executableHash),'BASIS.EXECUTABLE','producerHash','missing or changed executable bytes');
  if (producer.kind==='native') {
    assert(provenance.evidenceKind==='measuredSyntax' && provenance.basis===null,'BASIS.NATIVE_PAIRING','basis','native syntax requires null semantic basis');
    return {producer,revision};
  }
  assert(provenance.evidenceKind!=='measuredSyntax' && provenance.basis!==null,'BASIS.SEMANTIC_PAIRING','basis','semantic evidence requires basis');
  const basis=provenance.basis;
  const checks={producerId:producer.id,producerVersion:producer.version,producerHash:producer.executableHash,
    language:provenance.document.language,sourceSetId:provenance.document.sourceSetId,revisionId:provenance.revisionId,
    sourceManifestHash:loaded.sourceManifestHash(revision),toolchainHash:revision.toolchainHash,configHash:revision.configHash,dependencyHash:revision.dependencyHash};
  for (const [field,expected] of Object.entries(checks)) assert(basis[field]===expected,`BASIS.${field.toUpperCase()}`,field,'captured claim differs');
  assert(producer.languages.includes(basis.language),'BASIS.LANGUAGE','language','producer does not support captured language');
  for (const [field,kind] of [['toolchainHash','toolchain'],['configHash','config'],['dependencyHash','dependency']])
    assert(captureExists(loaded,kind,basis[field]),`BASIS.${field.toUpperCase()}`,field,'captured bytes unavailable');
  assert(loaded.semanticBytes.some(({bytes,value})=>value.producerId===producer.id && contentHash(bytes)===basis.artifactHash),
    'BASIS.ARTIFACTHASH','artifactHash','semantic artifact bytes differ');
  const deps=basis.lookupDependencies;
  for(let i=1;i<deps.length;i++) assert(Buffer.compare(Buffer.from(deps[i-1]),Buffer.from(deps[i]))<0,
    'BASIS.LOOKUP_DEPENDENCIES','lookupDependencies','keys must be sorted and unique');
  return {producer,revision};
}

export function expectedFreshness(provenance,loaded) {
  const {producer}=checkCapturedBasis(provenance,loaded);
  const selected=loaded.selected;
  const wanted=selected.documents.find(x=>x.key.path===provenance.document.path && x.key.language===provenance.document.language && x.key.sourceSetId===provenance.document.sourceSetId);
  if (!wanted || wanted.contentHash!==provenance.contentHash) return 'stale';
  if (producer.kind==='native') return selected.id===provenance.revisionId ? 'fresh' : 'possiblyStale';
  const basis=provenance.basis;
  const requested=loaded.comparison.producers.find(x=>x.id===basis.producerId);
  if (!requested || requested.kind!=='semantic' || !requested.languages.includes(basis.language) ||
      requested.version!==basis.producerVersion || requested.executableHash!==basis.producerHash ||
      !captureExists(loaded,'executable',requested.executableHash) ||
      loaded.comparison.sourceSetId!==basis.sourceSetId || loaded.comparison.revisionId!==basis.revisionId ||
      loaded.sourceManifestHash(selected)!==basis.sourceManifestHash ||
      selected.toolchainHash!==basis.toolchainHash || selected.configHash!==basis.configHash || selected.dependencyHash!==basis.dependencyHash)
    return 'possiblyStale';
  return 'fresh';
}
export function checkFreshness(provenance,loaded) {
  const expected=expectedFreshness(provenance,loaded);
  assert(provenance.freshness===expected,'FRESHNESS.LABEL','freshness',`expected ${expected}`);
  return expected;
}

export function expectedStaleTarget(binding,provenance,loaded) {
  validate('CallBinding',binding);
  checkCapturedBasis(provenance,loaded);
  assert(binding.provenanceId===provenance.id,'TARGET_STALENESS.PROVENANCE','provenanceId','binding proof differs');
  const target=binding.declaredTarget;
  if (!target || target.kind!=='internal') return null;
  const captured=loaded.revisions.get(tuple(target.document.sourceSetId,target.revisionId))?.documents.find(x=>JSON.stringify(x.key)===JSON.stringify(target.document));
  assert(captured,'TARGET_STALENESS.CAPTURED','declaredTarget','target not present in captured snapshot');
  const requested=loaded.selected.documents.find(x=>JSON.stringify(x.key)===JSON.stringify(target.document));
  return !requested || requested.contentHash!==captured.contentHash;
}
export function checkStaleTarget(binding,provenance,loaded) {
  const expected=expectedStaleTarget(binding,provenance,loaded);
  assert(binding.staleTarget===expected,'TARGET_STALENESS.LABEL','staleTarget',`expected ${expected}`);
  return expected;
}
