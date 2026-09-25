import test from 'node:test';
import assert from 'node:assert/strict';
import {mkdtemp,mkdir,writeFile,rm,symlink} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
import {contentHash} from '../identity.mjs';
import {loadFixture,discoverFixtures} from '../load.mjs';

const hash=x=>contentHash(Buffer.from(x));
async function specimen() {
  const root=await mkdtemp(join(tmpdir(),'admission-'));
  const files={}; const put=(path,value)=>{files[path]=typeof value==='string'?value:JSON.stringify(value);};
  const source='export function go() { return 1; }\n';
  const document={sourceSetId:'main',language:'javascript',path:'src/go.js'};
  const native={id:'native',version:'1',executableHash:hash('native 1'),kind:'native',languages:['javascript'],positionEncoding:'utf8'};
  const semantic={...native,id:'semantic',kind:'semantic',executableHash:hash('semantic 1')};
  const captures=[];
  for(const [ref,kind,file,text] of [['native','executable','captures/native.bin','native 1'],['semantic','executable','captures/semantic.bin','semantic 1'],['tool','toolchain','captures/tool.txt','tool'],['config','config','captures/config.txt','config'],['deps','dependency','captures/deps.txt','deps'],['fact','semanticArtifact','captures/fact.json',JSON.stringify({formatVersion:1,producerId:'semantic',facts:[]})]]) {
    put(file,text); captures.push({ref,kind,file,hash:hash(text)});
  }
  put('captures/native.json',{formatVersion:1,producerId:'native',declarations:[],calls:[],controls:[],references:[]});
  put('src/go.js',source);
  put('src/go.js.annotations.json',{formatVersion:1,document,revisionId:'r1',scenarios:[],facts:[]});
  put('expected/answers.json',{formatVersion:1,answers:[]}); put('expected/dispositions.json',{formatVersion:1,assertions:[],callableValueNegatives:[]});
  put('expected/anchors.json',{formatVersion:1,cases:[]});
  const fixture={formatVersion:1,profile:'example',language:'javascript',sourceSets:[{id:'main',rootId:'root',languages:['javascript'],dependencies:[]}],producers:[native,semantic],revisions:[{id:'r1',sourceSetId:'main',documents:[{key:document,revisionId:'r1',sourceFile:'src/go.js'}],toolchainHash:hash('tool'),configHash:hash('config'),dependencyHash:hash('deps')}],comparison:{sourceSetId:'main',revisionId:'r1',producers:[native,semantic]},coverageIntents:[],nativeArtifact:'captures/native.json',semanticArtifacts:['captures/fact.json'],annotationFiles:['src/go.js.annotations.json'],answersFile:'expected/answers.json',dispositionsFile:'expected/dispositions.json',anchorCasesFile:'expected/anchors.json',captures};
  async function flush(){put('fixture.json',fixture); for(const [path,value] of Object.entries(files)){await mkdir(dirname(join(root,path)),{recursive:true});await writeFile(join(root,path),value);}}
  await flush();return {root,files,fixture,document,source,flush,cleanup:()=>rm(root,{recursive:true,force:true})};
}
export {specimen};
test('IDENTITY.ADMISSION verifies source-derived snapshot and independent producers',async t=>{
  const s=await specimen(); t.after(s.cleanup);
  const loaded=await loadFixture(s.root);
  assert.equal(loaded.selected.documents[0].contentHash,hash(s.source));
  assert.equal(loaded.sources.size,1);
  assert.notEqual(loaded.captureBytes.get('semantic').toString(),loaded.captureBytes.get('native').toString());
});
test('IDENTITY.DIGEST rejects changed capture and unlisted snapshot file',async t=>{
  const s=await specimen(); t.after(s.cleanup);
  await writeFile(join(s.root,'captures/config.txt'),'changed');
  await assert.rejects(loadFixture(s.root),/IDENTITY.DIGEST/);
  await s.flush();await writeFile(join(s.root,'src/unlisted.js'),'x');
  await assert.rejects(loadFixture(s.root),/IDENTITY.INVENTORY/);
  await rm(join(s.root,'src/unlisted.js'));await writeFile(join(s.root,'root-unlisted.js'),'x');
  await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.INVENTORY'});
});
test('IDENTITY.PATH rejects symlink source and traversal',async t=>{
  const s=await specimen();t.after(s.cleanup);
  await rm(join(s.root,'src/go.js')); await symlink('/etc/passwd',join(s.root,'src/go.js'));
  await assert.rejects(loadFixture(s.root),/IDENTITY.PATH/);
  await rm(join(s.root,'src/go.js'));
  s.fixture.revisions[0].documents[0].sourceFile='../outside.js';
  await s.flush(); await assert.rejects(loadFixture(s.root),/FORMAT.SHAPE/);
});
test('DISCOVERY.JAVASCRIPT_COEXISTENCE does not conflate example and corpus',async t=>{
  const root=await mkdtemp(join(tmpdir(),'discover-'));t.after(()=>rm(root,{recursive:true,force:true}));
  for(const [path,profile] of [['example','example'],['javascript','corpus']]){
    await mkdir(join(root,path),{recursive:true});await writeFile(join(root,path,'fixture.json'),JSON.stringify({formatVersion:1,profile,language:'javascript',sourceSets:[],producers:[],revisions:[],comparison:{sourceSetId:'x',revisionId:'r',producers:[]},coverageIntents:[],nativeArtifact:'x',semanticArtifacts:[],annotationFiles:[],answersFile:'x',dispositionsFile:'x',anchorCasesFile:'x',captures:[]}));
  }
  assert.deepEqual((await discoverFixtures(root)).map(x=>x.slice(root.length+1)),['example','javascript']);
});

test('IDENTITY.UTF8 rejects malformed authored JSON without replacement decoding',async t=>{
 const s=await specimen();t.after(s.cleanup);
 await writeFile(join(s.root,'captures/fact.json'),Buffer.from([0x7b,0xff,0x7d]));
 // Keep the outer captured hash honest: malformed UTF-8 remains an intake failure.
 s.fixture.captures.find(x=>x.ref==='fact').hash=contentHash(Buffer.from([0x7b,0xff,0x7d]));
 s.files['fixture.json']=JSON.stringify(s.fixture);await writeFile(join(s.root,'fixture.json'),s.files['fixture.json']);
 await assert.rejects(loadFixture(s.root),/UTF-8|utf-8|encoded data/i);
});

test('IDENTITY.COVERAGE rejects dangling tuples and incomplete support while retaining valid intent',async t=>{
 const s=await specimen();t.after(s.cleanup);
 const intent={producerId:'semantic',document:s.document,revisionId:'r1',requestedRoles:['read'],measurementSupport:['declarationName','callee','invocation','reference'].map(kind=>({kind,available:true,diagnostic:null}))};
 s.fixture.coverageIntents=[intent];await s.flush();assert.equal((await loadFixture(s.root)).fixture.coverageIntents.length,1);
 const mutations=[
  [x=>x.producerId='missing','IDENTITY.COVERAGE'],[x=>x.revisionId='missing','IDENTITY.COVERAGE'],
  [x=>x.document={...x.document,path:'missing.js'},'IDENTITY.COVERAGE'],
  [x=>x.requestedRoles=['read','read'],'IDENTITY.DUPLICATE'],
  [x=>x.measurementSupport.pop(),'IDENTITY.COVERAGE'],
  [x=>x.measurementSupport[1].kind='reference','IDENTITY.DUPLICATE'],
  [x=>x.measurementSupport[0].diagnostic='failed','IDENTITY.COVERAGE']];
 for(const [mutate,code] of mutations){const copy=structuredClone(intent);mutate(copy);s.fixture.coverageIntents=[copy];await s.flush();await assert.rejects(loadFixture(s.root),{assertion:code});}
 s.fixture.coverageIntents=[intent,intent];await s.flush();await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.COVERAGE'});
});
test('IDENTITY.SOURCE_SET and producer descriptors require supported unique admissions',async t=>{
 const s=await specimen();t.after(s.cleanup);
 for(const [mutate,assertion] of [
  [()=>s.fixture.sourceSets[0].dependencies=['absent'],'IDENTITY.SOURCE_SET'],
  [()=>s.fixture.sourceSets[0].dependencies=['main','main'],'IDENTITY.DUPLICATE'],
  [()=>s.fixture.sourceSets[0].languages=['javascript','javascript'],'IDENTITY.DUPLICATE'],
  [()=>s.fixture.producers[1].languages=['rust'],'IDENTITY.PRODUCER'],
  [()=>s.fixture.comparison.producers[1].languages=['javascript','javascript'],'IDENTITY.DUPLICATE']]){
  const original=structuredClone(s.fixture);mutate();await s.flush();await assert.rejects(loadFixture(s.root),{assertion});Object.assign(s.fixture,original);
 }
});
test('IDENTITY.INVENTORY rejects unlisted root files, duplicate annotation and root symlink',async t=>{
 const s=await specimen();t.after(s.cleanup);
 s.fixture.annotationFiles.push(s.fixture.annotationFiles[0]);await s.flush();await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.DUPLICATE'});
 s.fixture.annotationFiles.pop();s.fixture.revisions[0].documents[0].sourceFile='go.js';s.fixture.annotationFiles=['go.js.annotations.json'];s.files['go.js']=s.source;s.files['go.js.annotations.json']=JSON.stringify({formatVersion:1,document:s.document,revisionId:'r1',scenarios:[],facts:[]});delete s.files['src/go.js'];delete s.files['src/go.js.annotations.json'];await rm(join(s.root,'src'),{recursive:true});await s.flush();await writeFile(join(s.root,'unexpected.jsx'),'x');await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.INVENTORY'});
 await rm(join(s.root,'unexpected.jsx'));await writeFile(join(s.root,'rogue.dat'),'x');await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.INVENTORY'});
 await rm(join(s.root,'rogue.dat'));await symlink('/etc/passwd',join(s.root,'unexpected.js'));await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.INVENTORY'});
});
test('DISCOVERY.PROFILE rejects forged descriptors and nested fixture routing',async t=>{
 const root=await mkdtemp(join(tmpdir(),'discover-negative-'));t.after(()=>rm(root,{recursive:true,force:true}));
 await mkdir(join(root,'example'),{recursive:true});
 const s=await specimen();t.after(s.cleanup);await writeFile(join(root,'example','fixture.json'),JSON.stringify({...s.fixture,profile:'corpus'}));
 await assert.rejects(discoverFixtures(root),{assertion:'DISCOVERY.PROFILE'});
 await rm(join(root,'example','fixture.json'));await mkdir(join(root,'example','javascript'));await assert.rejects(discoverFixtures(root),{assertion:'DISCOVERY.PROFILE'});
});

test('IDENTITY.COVERAGE excludes Java alias but admits Java definition and JS alias',async t=>{
 const s=await specimen();t.after(s.cleanup);
 const intent={producerId:'semantic',document:s.document,revisionId:'r1',requestedRoles:['alias','definition'],measurementSupport:['declarationName','callee','invocation','reference'].map(kind=>({kind,available:true,diagnostic:null}))};
 s.fixture.coverageIntents=[intent];await s.flush();await loadFixture(s.root);
 const java={...s.document,language:'java'};
 s.fixture.language='java';s.fixture.sourceSets[0].languages=['java'];
 for(const producer of [...s.fixture.producers,...s.fixture.comparison.producers])producer.languages=['java'];
 s.fixture.revisions[0].documents[0].key=java;intent.document=java;
 s.files['src/go.js.annotations.json']=JSON.stringify({formatVersion:1,document:java,revisionId:'r1',scenarios:[],facts:[]});
 await s.flush();await assert.rejects(loadFixture(s.root),{assertion:'IDENTITY.COVERAGE',field:'requestedRoles'});
 intent.requestedRoles=['definition'];await s.flush();assert.equal((await loadFixture(s.root)).fixture.coverageIntents[0].requestedRoles[0],'definition');
});
