import {mkdtemp,writeFile,mkdir,rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join,dirname} from 'node:path';
export const clone = value => structuredClone(value);
export async function temporaryFixture(files={},overrides={}) {
  const root=await mkdtemp(join(tmpdir(),'semantic-contract-'));
  for (const [path,contents] of Object.entries({...files,...overrides})) {
    const target=join(root,path);
    await mkdir(dirname(target),{recursive:true});
    await writeFile(target,contents);
  }
  return {root,cleanup:()=>rm(root,{recursive:true,force:true})};
}
export function sourceWitness(source,text,{encoding='utf8',from=0}={}) {
  const index=source.indexOf(text,from);
  if (index<0) throw new Error('WITNESS.MISSING source spelling');
  const start=encoding==='utf8' ? Buffer.byteLength(source.slice(0,index)) : encoding==='utf16' ? index : [...source.slice(0,index)].length;
  const end=start+(encoding==='utf8' ? Buffer.byteLength(text) : encoding==='utf16' ? text.length : [...text].length);
  const witness={range:{encoding,start,end},text};
  return witness;
}
