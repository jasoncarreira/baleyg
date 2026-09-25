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
  for(const [path,profile] of [['example/javascript','example'],['javascript/corpus','corpus']]){
    await mkdir(join(root,path),{recursive:true});await writeFile(join(root,path,'fixture.json'),JSON.stringify({formatVersion:1,profile,language:'javascript',sourceSets:[],producers:[],revisions:[],comparison:{sourceSetId:'x',revisionId:'r',producers:[]},coverageIntents:[],nativeArtifact:'x',semanticArtifacts:[],annotationFiles:[],answersFile:'x',dispositionsFile:'x',anchorCasesFile:'x',captures:[]}));
  }
  assert.deepEqual((await discoverFixtures(root)).map(x=>x.slice(root.length+1)),['example/javascript','javascript/corpus']);
});

test('IDENTITY.UTF8 rejects malformed authored JSON without replacement decoding',async t=>{
 const s=await specimen();t.after(s.cleanup);
 await writeFile(join(s.root,'captures/fact.json'),Buffer.from([0x7b,0xff,0x7d]));
 // Keep the outer captured hash honest: malformed UTF-8 remains an intake failure.
 s.fixture.captures.find(x=>x.ref==='fact').hash=contentHash(Buffer.from([0x7b,0xff,0x7d]));
 s.files['fixture.json']=JSON.stringify(s.fixture);await writeFile(join(s.root,'fixture.json'),s.files['fixture.json']);
 await assert.rejects(loadFixture(s.root),/UTF-8|utf-8|encoded data/i);
});
