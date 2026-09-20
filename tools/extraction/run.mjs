import fs from 'node:fs';
import { fixturePath, generatedPath } from './paths.mjs';
import path from 'node:path';
import crypto from 'node:crypto';
import { spawnSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { fileURLToPath } from 'node:url';
process.chdir(path.dirname(fileURLToPath(import.meta.url)));
fs.mkdirSync(generatedPath(''), {recursive:true});
const hash=p=>crypto.createHash('sha256').update(fs.readFileSync(p)).digest('hex');
const sourceArg=process.argv.indexOf('--source');
if(sourceArg>=0) {
  const source=path.resolve(process.argv[sourceArg+1]);
  const destination=fixturePath('inputs/feature-factory');
  fs.mkdirSync(destination,{recursive:true});
  const files={};
  for(const dir of ['bin','core','observe','state']) {
    fs.rmSync(path.join(destination,dir),{recursive:true,force:true});
    fs.mkdirSync(path.join(destination,dir),{recursive:true});
    for(const name of fs.readdirSync(path.join(source,dir)).sort()) {
      if(!name.endsWith('.js')) continue;
      const relative=`${dir}/${name}`;
      fs.copyFileSync(path.join(source,relative),path.join(destination,relative));
      files[relative]=hash(path.join(source,relative));
    }
  }
  fs.copyFileSync(path.join(source,'package.json'),path.join(destination,'package.json'));
  fs.writeFileSync(generatedPath('source-manifest.json'),JSON.stringify({source,files},null,2)+'\n');
}
if(!fs.existsSync(fixturePath('inputs/feature-factory/package.json'))) throw new Error('First run requires --source /path/to/feature-factory/packages/feature-factory');
fs.writeFileSync(fixturePath('inputs/feature-factory/tsconfig.json'),JSON.stringify({compilerOptions:{allowJs:true,checkJs:false,noEmit:true,target:'ES2022',module:'NodeNext',moduleResolution:'NodeNext'},include:['bin/**/*.js','core/**/*.js','observe/**/*.js','state/**/*.js']},null,2));
function sources(root) {
  return Object.fromEntries(fs.readdirSync(root,{recursive:true,withFileTypes:true}).filter(e=>e.isFile()&&(e.name.endsWith('.js')||['package.json','tsconfig.json','jsconfig.json'].includes(e.name))).map(e=>path.join(e.parentPath,e.name)).sort().map(p=>[path.relative(root,p),hash(p)]));
}
const steps=[];
function run(label,args) {
  const start=performance.now();
  const result=spawnSync(process.execPath,args,{encoding:'utf8',timeout:120000,maxBuffer:16*1024*1024});
  const step={label,args,exitCode:result.status,elapsedMs:performance.now()-start,stdout:result.stdout??'',stderr:result.stderr??'',error:result.error?.message};
  steps.push(step);console.log(`[${label}] ${Math.round(step.elapsedMs)} ms, exit ${step.exitCode}\n${step.stdout}${step.stderr}`);
  fs.writeFileSync(generatedPath('run-results.json'),JSON.stringify({node:process.version,steps},null,2)+'\n');
  if(result.status!==0) process.exit(1);
}
for(const [name,relative] of [['feature-factory','inputs/feature-factory'],['fixture','fixture'],['edge-fixture','edge-fixture']]) {
  const input=fixturePath(relative);
  const before=sources(input);
  run(`${name}:index`,['node_modules/@sourcegraph/scip-typescript/dist/src/main.js','index','--cwd',input,'--output',fixturePath(`${name}.scip`),'--no-progress-bar']);
  if(JSON.stringify(before)!==JSON.stringify(sources(input))) throw new Error('source changed during indexing');
  fs.writeFileSync(fixturePath(`${name}.hashes.json`),JSON.stringify(before,null,2)+'\n');
  run(`${name}:extract`,['extract.mjs',input,fixturePath(`${name}.scip`),fixturePath(`${name}.hashes.json`),fixturePath(`${name}.graph.json`)]);
}
run('extraction:tests',['--test','extract.test.mjs']);
if(fs.existsSync('lifecycle.test.mjs')) run('lifecycle:tests',['--test','lifecycle.test.mjs']);
if(fs.existsSync('viewer-smoke.mjs')) run('viewer:smoke',['viewer-smoke.mjs']);
