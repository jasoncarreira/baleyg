import { fixturePath, extractionPath, researchPath } from './paths.mjs';
import fs from 'node:fs';
import { makePacket,baseline,assembleView } from './shared.mjs';
const graph=JSON.parse(fs.readFileSync(extractionPath('feature-factory.graph.json'),'utf8'));
const questions=JSON.parse(fs.readFileSync(researchPath('questions.json'),'utf8'));
fs.mkdirSync(fixturePath('inputs'),{recursive:true});fs.mkdirSync(fixturePath('outputs'),{recursive:true});
const manifest=[];
for(const q of questions) {
  const packet=makePacket(graph,q);
  fs.writeFileSync(fixturePath(`inputs/${q.id}.json`),JSON.stringify(packet,null,2)+'\n');
  const decisions=baseline(packet),view=assembleView(packet,decisions);
  fs.writeFileSync(fixturePath(`outputs/${q.id}.baseline.json`),JSON.stringify({provider:'deterministic',questionId:q.id,graphHash:packet.graphHash,decisions,view},null,2)+'\n');
  manifest.push({id:q.id,question:q.question,candidates:packet.candidates.length,truncatedSources:packet.candidates.filter(c=>c.sourceTruncated).length,omittedCandidates:packet.scope.omittedCandidates,requestBytes:Buffer.byteLength(JSON.stringify(packet)),visible:view.visible.length});
}
fs.writeFileSync(fixturePath('inputs/manifest.json'),JSON.stringify(manifest,null,2)+'\n');
console.log(JSON.stringify(manifest,null,2));
