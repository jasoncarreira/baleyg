import { fixturePath } from './paths.mjs';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { randomInt } from 'node:crypto';
import { assembleView, validateDecisions } from './shared.mjs';

const esc = value => String(value).replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));
const assert = (condition, message) => { if (!condition) throw new Error(message); };
export function validateInputs(packet, results) {
  assert(packet?.schemaVersion === 1 && /^[a-f0-9]{64}$/.test(packet.graphHash), 'Invalid packet graph hash or schema');
  assert(typeof packet.question === 'string' && typeof packet.questionId === 'string' && Array.isArray(packet.candidates) && packet.candidates.length > 0, 'Invalid packet');
  const ids = new Set(packet.candidates.map(c => c.id));
  assert(ids.size === packet.candidates.length && ids.has(packet.seedId), 'Invalid candidate IDs or seed');
  assert(new Set(packet.candidates.map(c => c.symbolId)).size === ids.size, 'Duplicate symbol IDs');
  for (const c of packet.candidates) {
    assert(['id','symbolId','name','path','source'].every(k => typeof c[k] === 'string') && Array.isArray(c.calls), 'Invalid candidate');
    for (const call of c.calls) assert(typeof call.id === 'string' && typeof call.callee === 'string' && typeof call.resolution === 'string' && Number.isInteger(call.line) && (call.target === null || ids.has(call.target)) && Array.isArray(call.callbackArguments) && call.callbackArguments.every(id => ids.has(id)), 'Invalid measured call');
  }
  assert(Array.isArray(results) && results.length === 3, 'Exactly three results required');
  assert(new Set(results.map(r => r?.provider)).size === 3, 'Three distinct providers required');
  for (const r of results) {
    assert(typeof r.provider === 'string' && r.provider.length > 0 && r.graphHash === packet.graphHash && r.questionId === packet.questionId, 'Result graph hash or question mismatch');
    if(r.status === 'failed' && !Object.hasOwn(r,'decisions')) continue; // Render an explicit failed panel, never an empty selection.
    validateDecisions(packet, r.decisions);
    const expected = assembleView(packet, r.decisions);
    for (const key of ['visible','collapsed','hidden']) {
      const actual = r.view?.[key];
      assert(Array.isArray(actual) && actual.length === expected[key].length && new Set(actual).size === actual.length && actual.every(id => expected[key].includes(id)), `Invalid result view: ${key}`);
    }
    assert((r.view.graphHash === undefined || r.view.graphHash === packet.graphHash) && (r.view.questionId === undefined || r.view.questionId === packet.questionId), 'View identity mismatch');
  }
}

// Never bridge hidden nodes or turn callback references into calls.
export function measuredEdges(packet, shownIds) {
  const shown = new Set(shownIds);
  return packet.candidates.filter(c => shown.has(c.id)).flatMap(c => c.calls
    .filter(call => call.resolution === 'internal' && shown.has(call.target))
    .map(call => ({from:c.id, to:call.target, callId:call.id, line:call.line})));
}
function panel(packet, result, label) {
  if(result.status === 'failed' && !Object.hasOwn(result,'decisions')) return `<section aria-labelledby="panel-${label}"><h2 id="panel-${label}">Panel ${label}</h2><p><strong>No validated selection was produced.</strong></p><p>This is a run failure, not a decision to omit all functions. Do not score this panel for selection quality.</p><details><summary>Run boundary</summary><p>${esc(result.toolCallAudit?.count ?? 'Unknown')} tool calls were recorded before the session ended. The failure artifact is retained in the experiment report.</p></details></section>`;
  const decisions = new Map(result.decisions.map(d => [d.candidateId, d.relevance]));
  const shown = packet.candidates.filter(c => c.id === packet.seedId || decisions.get(c.id) === 'essential');
  const shownIds = new Set(shown.map(c => c.id));
  const byId = new Map(packet.candidates.map(c => [c.id,c]));
  const positions = new Map(shown.map((c,i) => [c.id, 35 + i * 65]));
  const edges = measuredEdges(packet, shownIds);
  const svgEdges = edges.map((e,i) => {
    const y1 = positions.get(e.from), y2 = positions.get(e.to), bend = 360 + (i % 6) * 14;
    return `<path data-call="${esc(e.callId)}" d="M 335 ${y1} C ${bend} ${y1 - 24}, ${bend} ${y2 + 24}, 335 ${y2}" marker-end="url(#arrow-${label})"><title>${esc(e.from)} → ${esc(e.to)}, line ${e.line}</title></path>`;
  }).join('');
  const svgNodes = shown.map(c => `<g><title>${esc(c.name)} — ${esc(c.path)}:${esc(c.startLine)}</title><rect x="12" y="${positions.get(c.id)-20}" width="323" height="40" rx="4"/><text x="22" y="${positions.get(c.id)+5}">${esc(c.id)} · ${esc(c.name.length > 28 ? c.name.slice(0,27)+'…' : c.name)}</text></g>`).join('');
  const sources = shown.map(c => {
    const boundaries = c.calls.filter(call => call.resolution !== 'internal' || !shownIds.has(call.target));
    const callbacks = c.calls.filter(call => call.callbackArguments.length);
    return `<details><summary>${esc(c.id)} · ${esc(c.name)} — ${esc(decisions.get(c.id))}${c.id === packet.seedId && decisions.get(c.id) !== 'essential' ? ' (forced seed)' : ''}</summary><p>${esc(c.path)}:${esc(c.startLine)}–${esc(c.endLine)}${c.sourceTruncated ? ' · excerpt truncated' : ''}</p><pre><code>${esc(c.source)}</code></pre><details><summary>Call boundaries (${boundaries.length})</summary><ul>${boundaries.map(call => `<li>${esc(call.id)}, line ${call.line}: ${esc(call.callee)} → ${esc(call.target ? byId.get(call.target).name+' (not shown)' : 'unresolved / outside packet')} · ${esc(call.resolution)}</li>`).join('')}</ul></details><details><summary>Callback references — not calls (${callbacks.length})</summary><ul>${callbacks.map(call => `<li>${esc(call.id)}, line ${call.line}: ${call.callbackArguments.map(id => esc(byId.get(id).name)).join(', ')}</li>`).join('')}</ul></details></details>`;
  }).join('');
  const rows = candidates => candidates.map(c => `<li>${esc(c.id)} · ${esc(c.name)} — <strong>${esc(decisions.get(c.id))}</strong> · ${esc(c.path)}:${esc(c.startLine)}</li>`).join('');
  return `<section aria-labelledby="panel-${label}"><h2 id="panel-${label}">Panel ${label}</h2>${result.selectionCaveat ? `<p role="note"><strong>Run caveat:</strong> ${esc(result.selectionCaveat)}</p>` : ''}<p>${shown.length} shown · ${edges.length} measured direct calls</p><svg viewBox="0 0 460 ${Math.max(80,shown.length*65)}" role="img" aria-label="Panel ${label}: function nodes and measured direct calls"><defs><marker id="arrow-${label}" markerWidth="7" markerHeight="7" refX="6" refY="3" orient="auto"><polygon points="0 0, 6 3, 0 6"/></marker></defs>${svgEdges}${svgNodes}</svg><h3>Shown functions and source</h3>${sources}<details><summary>Collapsed / uncertain candidates</summary><ul>${rows(packet.candidates.filter(c => result.view.collapsed.includes(c.symbolId)))}</ul></details><details><summary>Hidden candidates</summary><ul>${rows(packet.candidates.filter(c => result.view.hidden.includes(c.symbolId)))}</ul></details></section>`;
}

export function renderComparison(packet, results) {
  validateInputs(packet, results);
  const shuffled = [...results];
  for (let i = shuffled.length - 1; i > 0; i--) { const j = randomInt(i + 1); [shuffled[i],shuffled[j]] = [shuffled[j],shuffled[i]]; }
  const labels = ['A','B','C'];
  const key = {questionId:packet.questionId, graphHash:packet.graphHash, panels:Object.fromEntries(shuffled.map((r,i) => [labels[i],r.provider]))};
  // Deliberately render no model metadata, rationale, timing, provider or input filenames.
  const html = `<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'"><title>Blind selection review</title><style>
  :root{color-scheme:light;--ink:oklch(.24 0 0);--bg:oklch(1 0 0);--muted:oklch(.43 0 0);--line:oklch(.78 0 0);--accent:oklch(.48 .16 20)}
  *{box-sizing:border-box}body{margin:0;padding:2rem;font:16px/1.5 system-ui,sans-serif;background:var(--bg);color:var(--ink)}main{max-width:1500px;margin:auto}h1{font-size:1.7rem}h2{font-size:1.3rem}h3{font-size:1rem}p{max-width:75ch}header{margin-bottom:2rem}.panels{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:2rem}section{min-width:0;border-top:2px solid var(--ink)}svg{width:100%;max-height:720px}svg path{fill:none;stroke:var(--muted);stroke-width:1.5}polygon{fill:var(--muted)}rect{fill:var(--bg);stroke:var(--line)}text{fill:var(--ink);font:14px system-ui}details{border-top:1px solid var(--line);padding:.7rem 0}summary{cursor:pointer;overflow-wrap:anywhere}summary:hover{color:var(--accent)}summary:focus-visible{outline:2px solid var(--accent);outline-offset:3px}pre{overflow:auto;padding:.7rem;background:oklch(.96 0 0);font-size:.8rem}li,p{overflow-wrap:anywhere}ul{padding-left:1.2rem}small{color:var(--muted);overflow-wrap:anywhere}@media(max-width:1000px){.panels{grid-template-columns:1fr}body{padding:1rem}svg{max-width:560px}}
  </style></head><body><main><header><h1>Blind selection review</h1><p><strong>${esc(packet.question)}</strong></p><p>Call-selection sketch — not a sequence diagram or an execution trace. Judge only what helps answer this question; adjacent detail is not useful by default. Panels are shuffled. Arrows show measured direct calls only; hidden intermediates are never bridged. Callback references do not establish execution.</p><small>Graph snapshot: ${esc(packet.graphHash)}. Fixed candidate scope; ${packet.candidates.some(c=>c.sourceTruncated) ? 'some source excerpts are truncated' : 'complete candidate source is available in the disclosures'}.</small></header><div class="panels">${shuffled.map((r,i) => panel(packet,r,labels[i])).join('')}</div></main></body></html>`;
  return {html,key};
}
export function writeComparison(packetFile, outputFile, resultFiles) {
  assert(resultFiles.length === 3, 'Exactly three result files required');
  const outputRoot = fs.realpathSync(fixturePath('outputs'));
  const parent = fs.realpathSync(path.dirname(path.resolve(outputFile)));
  const relative = path.relative(outputRoot, parent);
  assert(relative === '' || (!relative.startsWith('..') && !path.isAbsolute(relative)), 'Output directory must exist under tests/fixtures/selection/outputs');
  assert(path.extname(outputFile).toLowerCase() === '.html', 'Output must be an HTML file');
  const keyPath = path.join(parent,'blind-key.json');
  const inputs = [packetFile,...resultFiles].map(f => fs.realpathSync(f));
  assert(!inputs.includes(path.resolve(outputFile)) && !inputs.includes(keyPath), 'Output must not overwrite inputs');
  const packet = JSON.parse(fs.readFileSync(packetFile,'utf8'));
  const results = resultFiles.map(f => JSON.parse(fs.readFileSync(f,'utf8')));
  const {html,key} = renderComparison(packet,results);
  // Exclusive writes prevent accidental overwrite or following existing symlinks.
  fs.writeFileSync(keyPath,JSON.stringify(key,null,2)+'\n',{mode:0o600,flag:'wx'});
  try { fs.writeFileSync(outputFile,html,{mode:0o600,flag:'wx'}); }
  catch (error) { fs.unlinkSync(keyPath); throw error; }
  return {outputFile,keyPath};
}
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const [packet,output,...results] = process.argv.slice(2);
  if (!packet || !output || results.length !== 3) throw new Error('usage: node compare.mjs INPUT_PACKET OUTPUT_HTML RESULT1 RESULT2 RESULT3');
  writeComparison(packet,output,results);
  console.log('Wrote blinded smoke review. Keep blind-key.json private.');
}
