import {readFile, readdir, realpath, lstat} from 'node:fs/promises';
import {resolve, relative, join, dirname} from 'node:path';
import {validate} from './formats.mjs';
import {parseJson} from './json.mjs';
import {contentHash, sourceManifestHash} from './identity.mjs';

function reject(id, field, message) {
  const error = new Error(`${id} ${field}: ${message}`);
  error.assertion=id; error.code='invalidRecord'; error.field=field;
  throw error;
}
const key = value => JSON.stringify(value);
const documentKey = x => key([x.sourceSetId,x.language,x.path]);
const roles=['definition','read','write','call','type','import','alias'];
const applicable={native:['definition','read','write','call','type','import','alias'],semantic:roles};
const order = (a,b) => Buffer.compare(Buffer.from(a),Buffer.from(b));
const languages=['java','rust','python','javascript'];
const documentOrder=(a,b) => languages.indexOf(a.key.language)-languages.indexOf(b.key.language) || order(a.key.path,b.key.path);

// Resolve every byte path from the physical fixture root. This also rejects symlinked
// parent directories and files, including links whose target happens to remain inside.
export async function confinedFile(root, path) {
  validate('Path',path);
  const physical=await realpath(root), target=resolve(physical,path);
  if (relative(physical,target).startsWith('..') || target===physical) reject('IDENTITY.PATH',path,'outside fixture');
  let cursor=physical;
  for (const part of path.split('/')) {
    cursor=join(cursor,part);
    const info=await lstat(cursor);
    if (info.isSymbolicLink() || (cursor===target && !info.isFile())) reject('IDENTITY.PATH',path,'symlink or non-file');
  }
  if (await realpath(target)!==target) reject('IDENTITY.PATH',path,'escaped fixture');
  return readFile(target);
}
async function jsonFile(root,path,type) {
  const bytes=await confinedFile(root,path);
  const value=parseJson(bytes);
  validate(type,value);
  return {bytes,value};
}
function unique(rows,selector,field) {
  const seen=new Set();
  for (const row of rows) {
    const id=selector(row);
    if (seen.has(id)) reject('IDENTITY.DUPLICATE',field,`duplicate ${id}`);
    seen.add(id);
  }
}
async function inventory(root,paths) {
  unique(paths,x=>x,'files');
  const listed=new Set(paths);
  const directories=new Set(paths.map(x=>dirname(x)).filter(x=>x!=='.'));  
  {
    for (const entry of await readdir(root,{withFileTypes:true})) {
      if (entry.name==='fixture.json' || entry.isDirectory()) continue;
      if (entry.isSymbolicLink() || (!listed.has(entry.name) && /(?:\.json|\.js|\.ts|\.py|\.rs|\.java|\.bin|\.txt)$/.test(entry.name)))
        reject('IDENTITY.INVENTORY',entry.name,'unlisted root snapshot file or symlink');
    }
  }
  for (const dir of directories) {
    // Walk only declared snapshot directories, never the whole fixture root.
    const sample=paths.find(x=>dirname(x)===dir);
    await confinedFile(root,sample);
    async function walk(folder) {
      for (const entry of await readdir(resolve(root,folder),{withFileTypes:true})) {
        const path=folder==='.'?entry.name:`${folder}/${entry.name}`;
        if (entry.isSymbolicLink()) reject('IDENTITY.INVENTORY',path,'symlink');
        if (entry.isDirectory()) await walk(path);
        else if (!entry.isFile() || !listed.has(path)) reject('IDENTITY.INVENTORY',path,'unlisted snapshot file');
      }
    }
    await walk(dir);
  }
}

export async function loadFixture(root) {
  root=await realpath(root);
  const {value:fixture}=await jsonFile(root,'fixture.json','FixtureV1');
  unique(fixture.sourceSets,x=>x.id,'sourceSets');
  unique(fixture.producers,x=>x.id,'producers');
  unique(fixture.comparison.producers,x=>x.id,'comparison.producers');
  unique(fixture.revisions,x=>key([x.sourceSetId,x.id]),'revisions');
  unique(fixture.captures,x=>x.ref,'captures.ref');
  unique(fixture.captures,x=>x.file,'captures.file');
  const sources=new Map(), sourceFiles=[], revisions=new Map(), captureBytes=new Map(), captures=new Map();
  for (const set of fixture.sourceSets) {
    if (!set.languages.length || !set.languages.includes(fixture.language)) reject('IDENTITY.SOURCE_SET','languages','fixture language not admitted');
    unique(set.languages,x=>x,'sourceSets.languages');
    unique(set.dependencies,x=>x,'sourceSets.dependencies');
    for (const dependency of set.dependencies) if (!fixture.sourceSets.some(x=>x.id===dependency)) reject('IDENTITY.SOURCE_SET','dependencies','unknown source set');
  }
  for (const producer of [...fixture.producers,...fixture.comparison.producers]) {
    if (!producer.languages.length) reject('IDENTITY.PRODUCER','languages','empty producer languages');
    unique(producer.languages,x=>x,'producers.languages');
    if (!producer.languages.includes(fixture.language)) reject('IDENTITY.PRODUCER','languages','fixture language unsupported');
  }
  unique(fixture.semanticArtifacts,x=>x,'semanticArtifacts');
  unique(fixture.annotationFiles,x=>x,'annotationFiles');
  unique([fixture.nativeArtifact,fixture.answersFile,fixture.dispositionsFile,fixture.anchorCasesFile,...fixture.annotationFiles,...fixture.semanticArtifacts],x=>x,'inputFiles');
  const intents=new Set();
  for (const intent of fixture.coverageIntents) {
    const id=key([intent.producerId,documentKey(intent.document),intent.revisionId]);
    if (intents.has(id)) reject('IDENTITY.COVERAGE','coverageIntents','duplicate tuple');
    intents.add(id);
    const producer=fixture.producers.find(x=>x.id===intent.producerId);
    const revision=fixture.revisions.find(x=>x.id===intent.revisionId && x.sourceSetId===intent.document.sourceSetId);
    if (!producer || !producer.languages.includes(intent.document.language) || intent.document.language!==fixture.language ||
        !revision?.documents.some(x=>documentKey(x.key)===documentKey(intent.document)))
      reject('IDENTITY.COVERAGE','coverageIntents','unadmitted producer/document/revision');
    unique(intent.requestedRoles,x=>x,'requestedRoles');
    if (intent.requestedRoles.some(x=>!applicable[producer.kind].includes(x))) reject('IDENTITY.COVERAGE','requestedRoles','inapplicable role');
    if (intent.measurementSupport.length!==4) reject('IDENTITY.COVERAGE','measurementSupport','four families required');
    unique(intent.measurementSupport,x=>x.kind,'measurementSupport');
    for (const entry of intent.measurementSupport) if (entry.available !== (entry.diagnostic===null))
      reject('IDENTITY.COVERAGE','measurementSupport','availability/diagnostic mismatch');
  }
  for (const revision of fixture.revisions) {
    const set=fixture.sourceSets.find(x=>x.id===revision.sourceSetId);
    if (!set || !set.languages.includes(fixture.language)) reject('IDENTITY.REVISION','revisions','unadmitted source set/language');
    if (!revision.documents.length) reject('IDENTITY.REVISION','documents','empty snapshot');
    unique(revision.documents,x=>documentKey(x.key),'documents');
    for (let i=1;i<revision.documents.length;i++) if (documentOrder(revision.documents[i-1],revision.documents[i])>=0)
      reject('IDENTITY.MANIFEST','documents','unsorted snapshot');
    const documents=[];
    for (const item of revision.documents) {
      if (item.key.sourceSetId!==revision.sourceSetId || item.revisionId!==revision.id || item.key.language!==fixture.language || !item.key.path)
        reject('IDENTITY.DOCUMENT','documents','document tuple/source path mismatch');
      const bytes=await confinedFile(root,item.sourceFile);
      try { new TextDecoder('utf-8',{fatal:true}).decode(bytes); } catch { reject('IDENTITY.UTF8',item.sourceFile,'invalid UTF-8 source bytes'); }
      sourceFiles.push(item.sourceFile);
      const document={key:item.key,revisionId:item.revisionId,contentHash:contentHash(bytes),byteLength:bytes.length};
      documents.push(document);
      sources.set(key([revision.sourceSetId,revision.id,item.key.path]),bytes);
    }
    const complete={...revision,documents};
    revisions.set(key([revision.sourceSetId,revision.id]),complete);
  }
  const selected=revisions.get(key([fixture.comparison.sourceSetId,fixture.comparison.revisionId]));
  if (!selected) reject('IDENTITY.COMPARISON','comparison','snapshot not admitted');
  for (const capture of fixture.captures) {
    const bytes=await confinedFile(root,capture.file);
    if (contentHash(bytes)!==capture.hash) reject('IDENTITY.DIGEST',capture.file,'captured bytes differ');
    captures.set(capture.ref,capture); captureBytes.set(capture.ref,bytes);
  }
  const matching=(kind,hash) => fixture.captures.some(x=>x.kind===kind && x.hash===hash);
  for (const producer of [...fixture.producers,...fixture.comparison.producers]) {
    if (fixture.producers.includes(producer) && !matching('executable',producer.executableHash)) reject('IDENTITY.PRODUCER',producer.id,'executable capture unavailable');
  }
  for (const revision of revisions.values()) for (const [field,kind] of [['toolchainHash','toolchain'],['configHash','config'],['dependencyHash','dependency']])
    if (!matching(kind,revision[field])) reject('IDENTITY.DIGEST',field,'revision capture unavailable');
  const semanticBytes=[];
  for (const path of fixture.semanticArtifacts) {
    const capture=fixture.captures.find(x=>x.file===path && x.kind==='semanticArtifact');
    if (!capture) reject('IDENTITY.INVENTORY',path,'semantic artifact lacks capture');
    const artifact=await jsonFile(root,path,'SemanticCapture');
    if (!fixture.producers.some(x=>x.id===artifact.value.producerId && x.kind==='semantic')) reject('IDENTITY.PRODUCER',path,'unknown semantic producer');
    semanticBytes.push(artifact);
  }
  if (fixture.captures.filter(x=>x.kind==='semanticArtifact').length!==semanticBytes.length)
    reject('IDENTITY.INVENTORY','semanticArtifacts','unlisted semantic capture');
  const native=(await jsonFile(root,fixture.nativeArtifact,'NativeArtifact')).value;
  unique([...native.declarations,...native.calls,...native.controls,...native.references],x=>x.ref,'native.ref');
  for (const row of [...native.declarations,...native.calls,...native.controls,...native.references]) {
    const snapshot=revisions.get(key([row.document.sourceSetId,row.revisionId]));
    if (!snapshot?.documents.some(x=>documentKey(x.key)===documentKey(row.document))) reject('IDENTITY.NATIVE',row.ref,'native row outside admitted snapshot');
  }
  if (!fixture.producers.some(x=>x.id===native.producerId && x.kind==='native')) reject('IDENTITY.PRODUCER','nativeArtifact','unknown native producer');
  const annotations=[];
  for (const path of fixture.annotationFiles) {
    const annotation=(await jsonFile(root,path,'AnnotationFile')).value;
    if (!sources.has(key([annotation.document.sourceSetId,annotation.revisionId,annotation.document.path])) || !fixture.revisions.some(r=>r.id===annotation.revisionId && r.sourceSetId===annotation.document.sourceSetId && r.documents.some(d=>documentKey(d.key)===documentKey(annotation.document) && path===`${d.sourceFile}.annotations.json`)))
      reject('IDENTITY.ANNOTATION',path,'not adjacent to admitted source');
    annotations.push(annotation);
  }
  const semanticFacts=new Map();
  for (const {bytes,value} of semanticBytes) for (const fact of value.facts) {
    const id=key([value.producerId,fact.ref]);
    if (semanticFacts.has(id)) reject('IDENTITY.SEMANTIC','facts','duplicate raw fact');
    semanticFacts.set(id,{fact,hash:contentHash(bytes)});
  }
  for (const annotation of annotations) for (const fact of annotation.facts) {
    if (fact.kind==='coverage') continue;
    if (fact.kind==='provenance' && fact.record.evidenceKind==='measuredSyntax') continue;
    const provenance=fact.kind==='provenance'?fact.record:
      annotation.facts.find(x=>x.kind==='provenance' && x.record.id===fact.record?.provenanceId)?.record;
    if (!provenance) reject('IDENTITY.SEMANTIC',fact.ref,'missing semantic provenance');
    const raw=semanticFacts.get(key([provenance.producerId,fact.ref]));
    const normalized=x=>{const copy=structuredClone(x); if (copy.kind==='provenance') { delete copy.record.freshness; if(copy.record.basis) delete copy.record.basis.artifactHash; } return key(copy);};
    if (!raw || normalized(raw.fact)!==normalized(fact)) reject('IDENTITY.SEMANTIC',fact.ref,'fact absent or contradicts raw capture');
    if (provenance.basis?.artifactHash!==raw.hash || documentKey(provenance.document)!==documentKey(annotation.document) ||
        provenance.revisionId!==annotation.revisionId ||
        (fact.anchor && (documentKey(fact.anchor.document)!==documentKey(annotation.document) || fact.anchor.revisionId!==annotation.revisionId)))
      reject('IDENTITY.SEMANTIC',fact.ref,'capture basis/document mismatch');
  }
  const answers=(await jsonFile(root,fixture.answersFile,'AnswersInputV1')).value;
  const dispositions=(await jsonFile(root,fixture.dispositionsFile,'DispositionsV1')).value;
  const anchors=(await jsonFile(root,fixture.anchorCasesFile,'AnchorCasesV1')).value;
  await inventory(root,[...new Set(sourceFiles),...fixture.captures.map(x=>x.file),fixture.nativeArtifact,...fixture.annotationFiles,fixture.answersFile,fixture.dispositionsFile,fixture.anchorCasesFile]);
  return {root,fixture,comparison:fixture.comparison,selected,revisions,sources,captures,captureBytes,semanticBytes,native,annotations,answers,dispositions,anchors,
    sourceManifestHash: revision => sourceManifestHash(revision.documents.map(x=>({document:x.key,contentHash:x.contentHash})))};
}

export async function discoverFixtures(root) {
  const fixtures=[];
  for (const entry of (await readdir(root,{withFileTypes:true})).filter(x=>x.isDirectory()).sort((a,b)=>order(a.name,b.name))) {
    if (entry.name!=='example' && !languages.includes(entry.name)) reject('DISCOVERY.LANGUAGE',entry.name,'unknown language directory');
    const path=join(root,entry.name);
    const children=await readdir(path,{withFileTypes:true});
    if (!children.some(x=>x.name==='fixture.json' && x.isFile()) ||
        children.some(x=>x.isDirectory() && (x.name==='corpus' || x.name==='javascript')))
      reject('DISCOVERY.PROFILE',entry.name,'expected immediate fixture.json');
    fixtures.push(path);
  }
  const seen=new Set();
  for (const path of fixtures) {
    const {value}=await jsonFile(path,'fixture.json','FixtureV1');
    const name=relative(root,path), profile=name==='example'?'example':'corpus';
    const language=name==='example'?'javascript':name;
    if (value.profile!==profile || value.language!==language) reject('DISCOVERY.PROFILE',path,'fixture descriptor mismatch');
    const pair=key([profile,language]);
    if (seen.has(pair)) reject('DISCOVERY.DUPLICATE',path,'duplicate fixture');
    seen.add(pair);
  }
  return fixtures;
}
