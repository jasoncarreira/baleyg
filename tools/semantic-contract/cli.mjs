#!/usr/bin/env node
import {resolve,join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {generateFixture,checkPublication} from './publish.mjs';

export function parseArgs(args) {
 const [command,...rest]=args;
 if(!['generate','check'].includes(command))throw new Error('CLI.COMMAND expected generate or check');
 let fixture='example',root=fileURLToPath(new URL('../../tests/fixtures/semantic-evidence/v1/',import.meta.url)),check=false;
 const seen=new Set();
 for(let i=0;i<rest.length;i++) {
  const flag=rest[i];
  if(!['--check','--fixture','--fixtures-root'].includes(flag)||seen.has(flag))throw new Error(`CLI.FLAG unexpected or duplicate flag ${flag}`);
  seen.add(flag);
  if(flag==='--check') {if(command!=='generate')throw new Error('CLI.FLAG --check requires generate');check=true;continue;}
  if(++i>=rest.length||rest[i].startsWith('--'))throw new Error(`CLI.FLAG ${flag} requires a value`);
  if(flag==='--fixture')fixture=rest[i];else root=resolve(rest[i]);
 }
 if(!['example','java','rust','python','javascript'].includes(fixture))throw new Error(`CLI.FIXTURE unknown fixture ${fixture}`);
 return {command,fixture,root,check};
}
export async function main(args=process.argv.slice(2)) {
 const {command,fixture,root,check}=parseArgs(args);
 const path=join(root,fixture);
 return command==='check'?checkPublication(path):generateFixture(path,{check});
}
if(process.argv[1]&&resolve(process.argv[1])===fileURLToPath(import.meta.url)) {
 main().catch(error=>{console.error(error.message);process.exitCode=1;});
}
