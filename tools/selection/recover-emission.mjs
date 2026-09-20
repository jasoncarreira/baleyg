import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { digest,validateDecisions,assembleView } from './shared.mjs';
export function recoverEmission(packet,terminal,emission) {
 if(terminal.status!=='failed'||terminal.graphHash!==packet.graphHash||terminal.questionId!==packet.questionId||terminal.packetHash!==digest(packet)) throw new Error('Not a matching failed terminal record');
 if(!terminal.toolCallAudit?.names?.includes('mcp__selection__emit_view')) throw new Error('No audited emit_view call');
 const decisions=validateDecisions(packet,emission.decisions);
 return {...terminal,status:'validated-emission-terminal-error',terminalStatus:'failed',selectionCaveat:'A validated selection was captured, but its session ended with an error. This panel is a partial run, not a normal completion.',decisions,view:assembleView(packet,decisions)};
}
if(process.argv[1]===fileURLToPath(import.meta.url)) {
 const [packetPath,terminalPath,outputPath]=process.argv.slice(2);
 if(!outputPath)throw new Error('usage: node recover-emission.mjs PACKET FAILED_TERMINAL DERIVED_OUTPUT');
 const packet=JSON.parse(fs.readFileSync(packetPath)),terminal=JSON.parse(fs.readFileSync(terminalPath)),emission=JSON.parse(fs.readFileSync(terminalPath+'.decisions.json'));
 const result=recoverEmission(packet,terminal,emission);
 result.derivation={terminalArtifact:path.basename(terminalPath),emissionArtifact:path.basename(terminalPath+'.decisions.json'),emissionSha256:digest(JSON.stringify(emission)),newInference:false};
 fs.writeFileSync(outputPath,JSON.stringify(result,null,2)+'\n',{flag:'wx',mode:0o600});
 console.log('Preserved validated emission separately from failed terminal status; no inference made.');
}
