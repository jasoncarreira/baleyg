import { fixturePath, extractionPath, researchPath } from './paths.mjs';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { makePacket, baseline, assembleView } from './shared.mjs';

export const HARD_IDS = ['q02-atomic-branches', 'q03-lock-reclaim', 'q04-transition-cas'];
const directory = path.dirname(fileURLToPath(import.meta.url));
const MAX_CANDIDATES = 128;
const MAX_SOURCE_CHARS = Number.MAX_SAFE_INTEGER;

export function makeHardPacket(graph, question) {
  const paths = [...new Set(question.candidate_paths)];
  const eligible = graph.nodes.filter(n => n.kind !== 'module' && paths.includes(n.path));
  if (eligible.length > MAX_CANDIDATES) throw new Error(`Candidate limit exceeded: ${eligible.length}`);
  const packet = makePacket(graph, question, { maxCandidates: MAX_CANDIDATES, maxSourceChars: MAX_SOURCE_CHARS });
  packet.sourceFiles = paths.map(filePath => {
    const file = graph.files.find(f => f.path === filePath);
    if (!file) throw new Error(`Missing complete source file: ${filePath}`);
    return { path: file.path, text: file.text };
  });
  packet.scope = {
    ...packet.scope,
    retrieval: 'fixed-manual-candidate-paths',
    candidatePaths: paths,
    limitations: 'Complete files from an explicit, manually selected fixed scope; not inferred retrieval. Syntax-level source ordering and measured references do not prove runtime dispatch or execution. Callback references are not executed calls.'
  };
  if (packet.scope.omittedCandidates || packet.candidates.some(c => c.sourceTruncated)) {
    throw new Error('Incomplete hard packet');
  }
  return packet;
}

// An allowlist projection: human rubric and repeated source excerpts never reach providers.
export function toProviderPacket(packet) {
  if (!Array.isArray(packet.sourceFiles)) throw new Error('Complete sourceFiles required');
  const seen = new Map();
  for (const file of packet.sourceFiles) {
    if (seen.has(file.path) && seen.get(file.path) !== file.text) throw new Error(`Conflicting source: ${file.path}`);
    seen.set(file.path, file.text);
  }
  return {
    questionId: packet.questionId,
    question: packet.question,
    seedId: packet.seedId,
    labels: [...packet.labels],
    candidates: packet.candidates.map(c => ({
      id: c.id, name: c.name, kind: c.kind, path: c.path,
      startLine: c.startLine, endLine: c.endLine, distance: c.distance,
      // Missing target means no indexed candidate target; missing callbackArguments means [].
      calls: c.calls.map(call => ({
        callee: call.callee, ...(call.target === null ? {} : { target: call.target }),
        resolution: call.resolution,
        ...(call.callbackArguments.length ? { callbackArguments: [...call.callbackArguments] } : {}),
        line: call.line
      }))
    })),
    sourceFiles: [...seen].map(([path, text]) => ({ path, text }))
  };
}

// Offline human-only rubric audit. Non-candidate names require explicit caller classification;
// never silently reinterpret a missing internal implementation as external/injected.
export function auditHardPacket(packet, question, { externalSymbols = [], injectedSymbols = [] } = {}) {
  const evidence = [], symbols = [];
  const files = new Map(packet.sourceFiles.map(f => [f.path, f.text]));
  for (const fact of question.must_show) {
    for (const interval of fact.evidence) {
      const text = files.get(interval.path);
      const covered = typeof text === 'string' && Number.isInteger(interval.startLine) &&
        Number.isInteger(interval.endLine) && interval.startLine >= 1 &&
        interval.endLine >= interval.startLine && interval.endLine <= text.split('\n').length;
      if (!covered) throw new Error(`Uncovered rubric evidence: ${interval.path}:${interval.startLine}-${interval.endLine}`);
      evidence.push({ ...interval, covered });
    }
    for (const name of fact.symbol_names) {
      const candidates = packet.candidates.filter(c => c.name === name);
      const classification = candidates.length ? 'internal' : externalSymbols.includes(name) ? 'external' : injectedSymbols.includes(name) ? 'injected' : null;
      if (!classification) throw new Error(`Missing required internal symbol: ${name}`);
      symbols.push({ name, classification, candidateIds: candidates.map(c => c.id) });
    }
  }
  if (question.id === 'q04-transition-cas') {
    for (const name of ['assertUnchanged', 'rename']) {
      if (!packet.candidates.some(c => c.path === 'core/write-core.js' && c.name === name)) throw new Error(`Missing q04 safeguard: ${name}`);
    }
  }
  return { questionId: question.id, passed: true, evidence, symbols };
}

export function prepareHard({ graphPath = extractionPath('feature-factory.graph.json'), questionsPath = researchPath('questions.json'), outputRoot = fixturePath('') } = {}) {
  const graph = JSON.parse(fs.readFileSync(graphPath, 'utf8'));
  const questions = JSON.parse(fs.readFileSync(questionsPath, 'utf8'));
  const inputs = path.join(outputRoot, 'inputs/hard-v1');
  const outputs = path.join(outputRoot, 'outputs/hard-v1');
  fs.mkdirSync(inputs, { recursive: true });
  fs.mkdirSync(outputs, { recursive: true });
  const manifest = [];
  for (const id of HARD_IDS) {
    const question = questions.find(q => q.id === id);
    if (!question) throw new Error(`Missing hard question: ${id}`);
    const packet = makeHardPacket(graph, question);
    const audit = auditHardPacket(packet, question);
    const decisions = baseline(packet);
    const view = assembleView(packet, decisions);
    const input = JSON.stringify(packet, null, 2) + '\n';
    fs.writeFileSync(path.join(inputs, `${id}.json`), input);
    fs.writeFileSync(path.join(outputs, `${id}.baseline.json`), JSON.stringify({ provider: 'deterministic', questionId: id, graphHash: packet.graphHash, decisions, view }, null, 2) + '\n');
    manifest.push({ id, candidates: packet.candidates.length, sourceFiles: packet.sourceFiles.length, omittedCandidates: packet.scope.omittedCandidates, truncatedSources: 0, localBytes: Buffer.byteLength(input), providerBytes: Buffer.byteLength(JSON.stringify(toProviderPacket(packet))), auditPassed: audit.passed });
  }
  return manifest;
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  if (process.argv.length !== 3 || process.argv[2] !== '--run') {
    console.error('Usage: node prepare-hard.mjs --run (offline preparation only; no inference)');
    process.exitCode = 1;
  } else console.log(JSON.stringify(prepareHard(), null, 2));
}
