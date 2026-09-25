import {readFile, writeFile, mkdir, mkdtemp, rename, rm, lstat, link} from 'node:fs/promises';
import {join} from 'node:path';
import {loadFixture, confinedFile} from './load.mjs';
import {normalizeFixture} from './normalize.mjs';
import {materializeAnswers} from './answers.mjs';
import {checkAnswers} from './graph-check.mjs';
import {checkCounts} from './counts.mjs';
import {checkCoverage} from './record-check/coverage.mjs';
import {validate} from './formats.mjs';
import {parseJson, canonicalBytes} from './json.mjs';
import {contentHash} from './identity.mjs';

const names=['records','answers','counts'];
const order=(a,b)=>Buffer.compare(Buffer.from(a),Buffer.from(b));
function fail(assertion,field,message) {
 const error=new Error(`${assertion} ${field}: ${message}`);
 Object.assign(error,{assertion,field,code:'invalidRecord'});
 throw error;
}
async function present(path) {
 try {return await lstat(path);} catch(error) {if(error.code==='ENOENT')return null;throw error;}
}
function inputPaths(fixture) {
 const paths=['fixture.json',...fixture.revisions.flatMap(revision=>revision.documents.map(document=>document.sourceFile)),
  ...fixture.captures.map(capture=>capture.file),fixture.nativeArtifact,
  ...fixture.semanticArtifacts,...fixture.annotationFiles,
  fixture.answersFile,fixture.dispositionsFile,fixture.anchorCasesFile];
 return [...new Set(paths)].sort(order);
}
export async function inputDigest(root,fixture) {
 const rows=[];
 for(const path of inputPaths(fixture))rows.push({path,hash:contentHash(await confinedFile(root,path))});
 validate('InputDigestInput',rows);
 return contentHash(canonicalBytes(rows));
}

// Compilation deliberately checks authored answers before any output is staged.
export async function compileFixture(root) {
 const loaded=await loadFixture(root);
 const normalized=normalizeFixture(loaded);
 const records=normalized.records;
 const answers=materializeAnswers(loaded.answers,normalized);
 checkAnswers(loaded,records,checkCoverage(loaded,records),answers);
 const counts=checkCounts(loaded,records,loaded.dispositions);
 const values={records,answers,counts};
 const bytes=Object.fromEntries(names.map(name=>[name,canonicalBytes(values[name])]));
 for(const [name,type] of [['records','NormalizedRecordsV1'],['answers','AnswersV1'],['counts','CountsV1']])validate(type,parseJson(bytes[name]));
 const hashes=Object.fromEntries(names.map(name=>[name,contentHash(bytes[name])]));
 const inputHash=await inputDigest(root,loaded.fixture);
 const digest={recordsHash:hashes.records,answersHash:hashes.answers,countsHash:hashes.counts,inputHash};
 validate('BundleDigestInput',digest);
 const bundleHash=contentHash(canonicalBytes(digest));
 const manifest={formatVersion:1,contractVersion:1,warningsVersion:'warnings-v1',
  language:loaded.fixture.language,profile:loaded.fixture.profile,inputHash,bundleHash,
  ...Object.fromEntries(names.map(name=>[name,{path:`bundles/${bundleHash}/${name}.json`,hash:hashes[name]}]))};
 validate('ManifestV1',manifest);
 return {loaded,values,bytes,manifest};
}

async function readPublished(root,expected) {
 const target=join(root,'generated','manifest.json');
 if(!(await present(target)))fail('PUBLICATION.MANIFEST','manifest','published manifest missing');
 let manifest,manifestBytes;
 try {manifestBytes=await readFile(target);manifest=parseJson(manifestBytes);validate('ManifestV1',manifest);}
 catch(error) {fail('PUBLICATION.MANIFEST','manifest',error.message);}
 const digest={recordsHash:manifest.records.hash,answersHash:manifest.answers.hash,
  countsHash:manifest.counts.hash,inputHash:manifest.inputHash};
 if(contentHash(canonicalBytes(digest))!==manifest.bundleHash)
  fail('PUBLICATION.BUNDLE','bundleHash','manifest bundle digest differs from its linked hashes');
 for(const name of names) {
  const descriptor=manifest[name];
  if(descriptor.path!==`bundles/${manifest.bundleHash}/${name}.json`)
   fail('PUBLICATION.PATH',name,'published bundle path differs');
  const file=join(root,'generated',descriptor.path);
  let bytes;
  try {bytes=await readFile(file);} catch(error) {fail('PUBLICATION.FILE',name,`published file missing: ${error.message}`);}
  if(contentHash(bytes)!==descriptor.hash)fail('PUBLICATION.HASH',name,'published byte hash differs');
 }
 if(!manifestBytes.equals(canonicalBytes(expected)))fail('PUBLICATION.MANIFEST','manifest','published manifest bytes differ from admitted inputs');
 return manifest;
}

export async function checkPublication(root) {
 const built=await compileFixture(root);
 await readPublished(root,built.manifest);
 for(const name of names) {
  const actual=await readFile(join(root,'generated',built.manifest[name].path));
  if(!actual.equals(built.bytes[name]))fail('PUBLICATION.BYTES',name,'published bytes differ from checked canonical result');
 }
 return built.manifest;
}

async function verifyBundle(directory,built,assertion) {
 for(const name of names) {
  let actual;
  try {actual=await readFile(join(directory,`${name}.json`));}
  catch(error) {fail(assertion,name,`bundle file missing: ${error.message}`);}
  if(contentHash(actual)!==built.manifest[name].hash||!actual.equals(built.bytes[name]))
   fail(assertion,name,'bundle bytes conflict with checked canonical output');
  const type={records:'NormalizedRecordsV1',answers:'AnswersV1',counts:'CountsV1'}[name];
  try {validate(type,parseJson(actual));}
  catch(error) {fail(assertion,name,`bundle content invalid: ${error.message}`);}
 }
}

// Optional fault is a test seam, called only at publication boundaries.
export async function generateFixture(root,{check=false,fault}={}) {
 if(check)return checkPublication(root);
 const built=await compileFixture(root);
 const generated=join(root,'generated'),bundles=join(generated,'bundles');
 await mkdir(bundles,{recursive:true});
 const temporary=await mkdtemp(join(generated,'.publish-'));
 const staged=join(temporary,'bundle'),final=join(bundles,built.manifest.bundleHash);
 let installed=false;
 const step=async name=>{if(fault)await fault(name);};
 try {
  await mkdir(staged);
  for(const name of names) {
   await writeFile(join(staged,`${name}.json`),built.bytes[name],{flag:'wx'});
   await step(`stage:${name}`);
  }
  await verifyBundle(staged,built,'PUBLICATION.STAGE');
  await step('stage:verified');
  await step('install:bundle');
  try {await mkdir(final);installed=true;}
  catch(error) {if(error.code!=='EEXIST')throw error;}
  if(installed) {
   // Exclusive directory creation and atomic hard links cannot overwrite old bytes.
   // Only the final manifest rename makes a complete bundle visible to readers.
   for(const name of names) {
    await link(join(staged,`${name}.json`),join(final,`${name}.json`));
    await step(`install:${name}`);
   }
   await verifyBundle(final,built,'PUBLICATION.STAGE');
  } else await verifyBundle(final,built,'PUBLICATION.IMMUTABLE');
  await step('install:verified');
  const manifestFile=join(temporary,'manifest.json');
  await writeFile(manifestFile,canonicalBytes(built.manifest),{flag:'wx'});
  const stagedManifest=await readFile(manifestFile);
  if(!stagedManifest.equals(canonicalBytes(built.manifest)))fail('PUBLICATION.STAGE','manifest','staged manifest bytes differ');
  await verifyBundle(final,built,'PUBLICATION.IMMUTABLE');
  await step('install:manifest');
  await rename(manifestFile,join(generated,'manifest.json'));
  return built.manifest;
 } catch(error) {
  if(installed)await rm(final,{recursive:true,force:true});
  throw error;
 } finally {await rm(temporary,{recursive:true,force:true});}
}
