import { extractionPath, researchPath } from './paths.mjs';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { HARD_IDS, makeHardPacket, toProviderPacket, auditHardPacket, prepareHard } from './prepare-hard.mjs';
import { assembleView } from './shared.mjs';

const directory = path.dirname(fileURLToPath(import.meta.url));
const graph = JSON.parse(fs.readFileSync(extractionPath('feature-factory.graph.json'), 'utf8'));
const questions = JSON.parse(fs.readFileSync(researchPath('questions.json'), 'utf8'));
const hard = questions.filter(q => HARD_IDS.includes(q.id));

for (const question of hard) {
  test(`${question.id}: complete fixed scope and uncut function bodies`, () => {
    const packet = makeHardPacket(graph, question);
    const eligible = graph.nodes.filter(n => n.kind !== 'module' && question.candidate_paths.includes(n.path));
    assert.deepEqual(new Set(packet.candidates.map(c => c.symbolId)), new Set(eligible.map(n => n.id)));
    assert.equal(packet.scope.omittedCandidates, 0);
    assert.equal(packet.scope.retrieval, 'fixed-manual-candidate-paths');
    assert.equal(packet.sourceFiles.length, new Set(question.candidate_paths).size);
    for (const file of packet.sourceFiles) assert.equal(file.text, graph.files.find(f => f.path === file.path).text);
    for (const candidate of packet.candidates) {
      const text = packet.sourceFiles.find(f => f.path === candidate.path).text;
      assert.equal(candidate.source, text.split('\n').slice(candidate.startLine - 1, candidate.endLine).join('\n'));
      assert.equal(candidate.sourceTruncated, false);
    }
    const audit = auditHardPacket(packet, question);
    assert.equal(audit.passed, true);
    assert.equal(audit.evidence.length, question.must_show.flatMap(f => f.evidence).length);
    assert.ok(audit.symbols.every(s => s.classification === 'internal' && s.candidateIds.length));
  });
  test(`${question.id}: provider projection deduplicates source and excludes rubric and long IDs`, () => {
    const packet = makeHardPacket(graph, question);
    const dirty = { ...packet, ...Object.fromEntries(['must_show', 'distractors', 'limitations'].map(k => [k, question[k]])), audit: 'human only' };
    dirty.sourceFiles = [...packet.sourceFiles, packet.sourceFiles[0]];
    const provider = toProviderPacket(dirty);
    assert.deepEqual(Object.keys(provider).sort(), ['questionId', 'question', 'seedId', 'labels', 'candidates', 'sourceFiles'].sort());
    assert.equal(provider.sourceFiles.length, packet.sourceFiles.length);
    assert.equal(provider.candidates.length, packet.candidates.length);
    for (const c of provider.candidates) {
      assert.deepEqual(Object.keys(c).sort(), ['id', 'name', 'kind', 'path', 'startLine', 'endLine', 'distance', 'calls'].sort());
      const localCalls = packet.candidates.find(local => local.id === c.id).calls;
      assert.equal(c.calls.length, localCalls.length);
      for (const [i, call] of c.calls.entries()) {
        assert.ok(Object.keys(call).every(key => ['callee', 'target', 'resolution', 'callbackArguments', 'line'].includes(key)));
        assert.notEqual(call.target, null);
        assert.ok(!('callbackArguments' in call) || call.callbackArguments.length > 0);
        const { id, ...expected } = localCalls[i];
        assert.deepEqual({ ...call, target: call.target ?? null, callbackArguments: call.callbackArguments ?? [] }, expected);
      }
    }
    const serialized = JSON.stringify(provider);
    assert.ok(!serialized.includes('source excerpt omitted'));
    assert.ok(!serialized.includes('scip-typescript npm'));
    assert.ok(Buffer.byteLength(serialized) < 100000);
    for (const fact of question.must_show) assert.ok(!serialized.includes(fact.fact));
    for (const file of packet.sourceFiles) assert.equal(serialized.split(JSON.stringify(file.text)).length - 1, 1);
    assert.throws(() => toProviderPacket({ ...packet, sourceFiles: [...packet.sourceFiles, { path: packet.sourceFiles[0].path, text: 'conflict' }] }), /Conflicting source/);
  });
}

test('q04 includes all 73 candidates, injected rename body and both unchanged checks', () => {
  const question = hard.find(q => q.id === 'q04-transition-cas');
  const packet = makeHardPacket(graph, question);
  assert.equal(packet.candidates.length, 73);
  const rename = packet.candidates.find(c => c.name === 'rename' && c.path === 'core/write-core.js');
  const unchanged = packet.candidates.find(c => c.name === 'assertUnchanged' && c.path === 'core/write-core.js');
  assert.ok(rename);
  assert.ok(unchanged);
  assert.equal(rename.calls.filter(c => c.callee === 'assertUnchanged').length, 2);
  assert.ok(rename.source.includes('finalGuard'));
  assert.ok(rename.source.includes('contract.reobserve'));
  assert.ok(unchanged.source.includes('isDeepStrictEqual'));
  for (const missing of [rename, unchanged]) assert.throws(() => auditHardPacket({ ...packet, candidates: packet.candidates.filter(c => c.id !== missing.id) }, question), /Missing/);
});

test('human audit rejects missing evidence/internal symbols and classifies external/injected explicitly', () => {
  const question = hard[0];
  const packet = makeHardPacket(graph, question);
  assert.throws(() => auditHardPacket({ ...packet, sourceFiles: [] }, question), /Uncovered rubric evidence/);
  const synthetic = { ...question, must_show: [{ evidence: [], symbol_names: ['nativeRename', 'injectedHook'] }] };
  assert.throws(() => auditHardPacket(packet, synthetic), /Missing required internal symbol/);
  const audit = auditHardPacket(packet, synthetic, { externalSymbols: ['nativeRename'], injectedSymbols: ['injectedHook'] });
  assert.deepEqual(audit.symbols.map(s => s.classification), ['external', 'injected']);
  const oversizedGraph = { ...graph, nodes: [...graph.nodes, ...Array.from({ length: 129 }, (_, i) => ({ ...graph.nodes.find(n => n.path === question.seed.path && n.name === question.seed.name), id: `extra${i}` }))] };
  assert.throws(() => makeHardPacket(oversizedGraph, question), /Candidate limit exceeded/);
});

test('offline script output is deterministic and baseline preserves local graph bindings', () => {
  const outputRoot = fs.mkdtempSync(path.join(os.tmpdir(), 'baleyg-hard-'));
  try {
    const first = prepareHard({ outputRoot });
    const readArtifacts = () => HARD_IDS.flatMap(id => [`inputs/hard-v1/${id}.json`, `outputs/hard-v1/${id}.baseline.json`]).map(file => fs.readFileSync(path.join(outputRoot, file), 'utf8'));
    const before = readArtifacts();
    assert.deepEqual(prepareHard({ outputRoot }), first);
    assert.deepEqual(readArtifacts(), before);
    for (const id of HARD_IDS) {
      const packet = JSON.parse(fs.readFileSync(path.join(outputRoot, `inputs/hard-v1/${id}.json`), 'utf8'));
      const result = JSON.parse(fs.readFileSync(path.join(outputRoot, `outputs/hard-v1/${id}.baseline.json`), 'utf8'));
      assert.equal(result.provider, 'deterministic');
      assert.equal(result.graphHash, packet.graphHash);
      assert.deepEqual(result.view, assembleView(packet, result.decisions));
      assert.equal(result.decisions.length, packet.candidates.length);
    }
  } finally { fs.rmSync(outputRoot, { recursive: true, force: true }); }
});
