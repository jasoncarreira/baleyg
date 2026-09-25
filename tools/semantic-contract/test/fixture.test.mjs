import test from 'node:test';
import assert from 'node:assert/strict';
import {cp, mkdtemp, readFile, rm, rename, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join, resolve} from 'node:path';
import {fileURLToPath} from 'node:url';
import {discoverFixtures} from '../load.mjs';
import {checkPublication} from '../publish.mjs';
import {runFixtures,parseRunnerArgs} from './run.mjs';

const defaultRoot=resolve(fileURLToPath(new URL('../../../tests/fixtures/semantic-evidence/v1/',import.meta.url)));
const fixturesRoot=process.env.SEMANTIC_FIXTURES_ROOT ? resolve(process.env.SEMANTIC_FIXTURES_ROOT) : defaultRoot;
async function copyExample() {
 const root=await mkdtemp(join(tmpdir(),'baleyg-fixture-discovery-'));
 await cp(join(defaultRoot,'example'),join(root,'example'),{recursive:true});
 return {root,cleanup:()=>rm(root,{recursive:true,force:true})};
}
async function descriptor(root,folder,change) {
 const path=join(root,folder,'fixture.json'),value=JSON.parse(await readFile(path,'utf8'));
 change(value);
 await writeFile(path,JSON.stringify(value));
}
const error=(assertion,field)=>value=>{
 assert.equal(value.assertion,assertion);
 assert.equal(value.code,'invalidRecord');
 assert.equal(value.field,field);
 assert.ok(value.message.includes(assertion));
 return true;
};

test('discovered fixtures are validated from the caller-selected root without a language registry',async()=>{
 const paths=await discoverFixtures(fixturesRoot);
 assert.ok(paths.length>0,'at least one fixture is required');
 for(const path of paths)assert.ok(path.startsWith(fixturesRoot));
 assert.equal(parseRunnerArgs(['--fixtures-root',fixturesRoot]),fixturesRoot);
 assert.throws(()=>parseRunnerArgs(['--fixtures-root']),/RUNNER.ARGS/);
});

test('runner verifies a valid copied example and does not silently skip it',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 const outcome=await runFixtures(root).then(count=>count,error=>error.assertion);
 assert.equal(outcome,1);
});

test('the committed example publication passes no-write checks',async()=>{
 if(fixturesRoot!==defaultRoot)return; // Temporary roots may intentionally omit publication.
 await checkPublication(join(fixturesRoot,'example'));
});

test('a copied corpus-profile language is discovered from data only',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 await rename(join(root,'example'),join(root,'javascript'));
 await descriptor(root,'javascript',value=>{value.profile='corpus';});
 assert.deepEqual(await discoverFixtures(root),[join(root,'javascript')]);
 // The minimal example is not a full corpus: it must NOT bypass the corpus floors.
 await assert.rejects(runFixtures(root),value=>{
  assert.match(value.message,/Fixture .*javascript.*COUNT\./s);
  return true;
 });
});

test('discovery rejects unknown directory name with stable assertion and path',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 await rename(join(root,'example'),join(root,'kotlin'));
 await assert.rejects(discoverFixtures(root),error('DISCOVERY.LANGUAGE','kotlin'));
});

test('discovery rejects a corpus descriptor with the wrong language or profile',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 await rename(join(root,'example'),join(root,'javascript'));
 await descriptor(root,'javascript',value=>{value.profile='corpus';value.language='python';});
 await assert.rejects(discoverFixtures(root),value=>{
  assert.equal(value.assertion,'DISCOVERY.PROFILE');
  assert.equal(value.field,join(root,'javascript'));
  assert.equal(value.code,'invalidRecord');
  assert.match(value.message,/fixture descriptor mismatch/);
  return true;
 });
 await descriptor(root,'javascript',value=>{value.language='javascript';value.profile='example';});
 await assert.rejects(discoverFixtures(root),error('DISCOVERY.PROFILE',join(root,'javascript')));
});

test('duplicate nested language fixture and absent descriptor are rejected',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 await cp(join(defaultRoot,'example'),join(root,'example','javascript'),{recursive:true});
 await assert.rejects(discoverFixtures(root),error('DISCOVERY.PROFILE','example'));
 await rm(join(root,'example','javascript'),{recursive:true});
 await rm(join(root,'example','fixture.json'));
 await assert.rejects(discoverFixtures(root),error('DISCOVERY.PROFILE','example'));
});

test('absent declared input fails a concrete inventory assertion',async t=>{
 const {root,cleanup}=await copyExample();t.after(cleanup);
 const value=JSON.parse(await readFile(join(root,'example','fixture.json'),'utf8'));
 const absent=value.answersFile;
 await rm(join(root,'example',absent));
 await assert.rejects(runFixtures(root),value=>{
  assert.equal(value.assertion,'IDENTITY.INVENTORY');
  assert.equal(value.code,'invalidRecord');
  assert.equal(value.field,absent);
  assert.match(value.message,/Fixture .*example.*declared input missing/s);
  return true;
 });
});
