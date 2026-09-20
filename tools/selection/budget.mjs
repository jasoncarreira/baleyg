import fs from 'node:fs';
import path from 'node:path';
const round=x=>Math.round(x*1e8)/1e8;
export function reserve(ledgerFile,{id,provider,maxUsd,capUsd=10}) {
  if(!(maxUsd>0)||!Number.isFinite(maxUsd)||!(capUsd>0&&capUsd<=10)) throw new Error('Invalid reservation or cap');
  fs.mkdirSync(path.dirname(ledgerFile),{recursive:true});
  const lock=ledgerFile+'.lock';let fd;
  try {fd=fs.openSync(lock,'wx',0o600);} catch {throw new Error('Budget ledger locked; refusing concurrent spend');}
  try {
    const ledger=fs.existsSync(ledgerFile)?JSON.parse(fs.readFileSync(ledgerFile,'utf8')):{schemaVersion:1,capUsd,entries:[]};
    if(ledger.entries.some(e=>e.id===id)) throw new Error('Duplicate request reservation');
    const committed=ledger.entries.reduce((sum,e)=>sum+(e.actualUsd??e.maxUsd),0);
    if(round(committed+maxUsd)>Math.min(capUsd,ledger.capUsd)) throw new Error('Budget cap would be exceeded');
    ledger.entries.push({id,provider,maxUsd,state:'reserved',createdAt:new Date().toISOString()});
    fs.writeFileSync(ledgerFile,JSON.stringify(ledger,null,2)+'\n',{mode:0o600});
    return ledger;
  } finally {fs.closeSync(fd);fs.unlinkSync(lock);}
}
export function settle(ledgerFile,id,{actualUsd=null,note='Unknown cost retains entire reservation'}={}) {
  const lock=ledgerFile+'.lock';let fd;
  try{fd=fs.openSync(lock,'wx',0o600);}catch{throw new Error('Budget ledger locked');}
  try {
    const ledger=JSON.parse(fs.readFileSync(ledgerFile,'utf8'));const e=ledger.entries.find(e=>e.id===id);
    if(!e||e.state!=='reserved') throw new Error('Reservation not pending');
    if(actualUsd!==null && (!(actualUsd>=0)||!Number.isFinite(actualUsd))) throw new Error('Invalid actual cost');
    e.actualUsd=actualUsd;e.state='settled';e.note=note;
    if(actualUsd!==null&&actualUsd>e.maxUsd) {e.overrun=true;ledger.capUsd=0;e.note+='; OVER RESERVATION: all future requests blocked';}
    fs.writeFileSync(ledgerFile,JSON.stringify(ledger,null,2)+'\n',{mode:0o600});return ledger;
  }finally{fs.closeSync(fd);fs.unlinkSync(lock);}
}
