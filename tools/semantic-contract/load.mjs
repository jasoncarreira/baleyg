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
  const listed=new Set(paths);
  unique(paths,x=>x,'files');
  const directories=new Set(paths.map(x=>dirname(x)).filter(x=>x!=='.'));  
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
    if (!set.languages.includes(fixture.language)) continue;
    // Dependency identifiers describe evidence only; they do not grant root access.
  }
  for (const revision of fixture.revisions) {
    const set=fixture.sourceSets.find(x=>x.id===revision.sourceSetId);
    if (!set || !set.languages.includes(fixture.language)) reject('IDENTITY.REVISION','revisions','unadmitted source set/language');
    if (!revision.documents.length) reject('IDENTITY.REVISION','documents','empty snapshot');
    unique(revision.documents,x=>key(x.key),'documents');
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
    if (!matching('executable',producer.executableHash)) reject('IDENTITY.PRODUCER',producer.id,'executable capture unavailable');
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
    if (!snapshot?.documents.some(x=>key(x.key)===key(row.document))) reject('IDENTITY.NATIVE',row.ref,'native row outside admitted snapshot');
  }
  if (!fixture.producers.some(x=>x.id===native.producerId && x.kind==='native')) reject('IDENTITY.PRODUCER','nativeArtifact','unknown native producer');
  const annotations=[];
  for (const path of fixture.annotationFiles) {
    const annotation=(await jsonFile(root,path,'AnnotationFile')).value;
    if (!sources.has(key([annotation.document.sourceSetId,annotation.revisionId,annotation.document.path])) || !fixture.revisions.some(r=>r.id===annotation.revisionId && r.sourceSetId===annotation.document.sourceSetId && r.documents.some(d=>key(d.key)===key(annotation.document) && path===`${d.sourceFile}.annotations.json`)))
      reject('IDENTITY.ANNOTATION',path,'not adjacent to admitted source');
    annotations.push(annotation);
  }
  const answers=(await jsonFile(root,fixture.answersFile,'AnswersInputV1')).value;
  const dispositions=(await jsonFile(root,fixture.dispositionsFile,'DispositionsV1')).value;
  const anchors=(await jsonFile(root,fixture.anchorCasesFile,'AnchorCasesV1')).value;
  await inventory(root,[...new Set(sourceFiles.concat(fixture.captures.map(x=>x.file),[fixture.nativeArtifact,...fixture.annotationFiles,fixture.answersFile,fixture.dispositionsFile,fixture.anchorCasesFile]))]);
  return {root,fixture,comparison:fixture.comparison,selected,revisions,sources,captures,captureBytes,semanticBytes,native,annotations,answers,dispositions,anchors,
    sourceManifestHash: revision => sourceManifestHash(revision.documents.map(x=>({document:x.key,contentHash:x.contentHash})))};
}

export async function discoverFixtures(root) {
  const fixtures=[];
  for (const name of (await readdir(root,{withFileTypes:true})).filter(x=>x.isDirectory()).map(x=>x.name).sort(order)) {
    if (name==='example') {
      const children=await readdir(join(root,name),{withFileTypes:true});
      if (children.length!==1) reject('DISCOVERY.PROFILE',name,'expected sole example/javascript');
      for (const child of children) {
        if (!child.isDirectory() || child.name!=='javascript') reject('DISCOVERY.PROFILE',child.name,'only example/javascript is exempt');
        fixtures.push(join(root,name,child.name));
      }
    } else {
      if (!languages.includes(name)) reject('DISCOVERY.LANGUAGE',name,'unknown language directory');
      const corpus=join(root,name,'corpus');
      const children=await readdir(join(root,name),{withFileTypes:true});
      if (children.length!==1 || children[0].name!=='corpus' || !children[0].isDirectory()) reject('DISCOVERY.PROFILE',name,'expected corpus directory');
      fixtures.push(corpus);
    }
  }
  const seen=new Set();
  for (const path of fixtures) {
    const {value}=await jsonFile(path,'fixture.json','FixtureV1');
    const pieces=relative(root,path).split('/');
    const profile=pieces[0]==='example'?'example':'corpus';
    const language=profile==='example'?'javascript':pieces[0];
    if (value.profile!==profile || value.language!==language) reject('DISCOVERY.PROFILE',path,'fixture descriptor mismatch');
    const pair=key([profile,language]);
    if (seen.has(pair)) reject('DISCOVERY.DUPLICATE',path,'duplicate fixture');
    seen.add(pair);
  }
  return fixtures;
}
