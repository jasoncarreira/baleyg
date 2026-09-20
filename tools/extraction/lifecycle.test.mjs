import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { fixturePath, generatedPath } from './paths.mjs';
import { join } from 'node:path';
import { DatabaseSync } from 'node:sqlite';
import { canonicalRows, digest, limitations, openCache, openWorkspace, publishRevision, readSnapshot, resolveAnnotation } from './lifecycle.mjs';

const graph = JSON.parse(readFileSync(fixturePath('feature-factory.graph.json'), 'utf8'));
const rows = canonicalRows(graph);
const changedGraph = structuredClone(graph);
changedGraph.nodes[0].name += '-revision-b';
const changedRows = canonicalRows(changedGraph);
const observations = [];
const expectedTests = 5;

function sandbox(t) {
  const dir = mkdtempSync(join(tmpdir(), 'baleyg-lifecycle-'));
  const connections = new Set();
  const track = db => { connections.add(db); return db; };
  const close = db => { db.close(); connections.delete(db); };
  t.after(() => {
    for (const db of connections) db.close();
    rmSync(dir, {recursive: true, force: true});
  });
  return {dir, track, close, path: join(dir, 'cache.db')};
}

function checkedTest(name, fn) {
  test(name, t => {
    try {
      const evidence = fn(t);
      observations.push({name, status: 'passed', ...evidence});
    } catch (error) {
      observations.push({name, status: 'failed', error: String(error)});
      throw error;
    }
  });
}

checkedTest('WAL pinned snapshot survives whole-revision replacement', t => {
  const s = sandbox(t);
  const writer = s.track(openCache(s.path));
  publishRevision(writer, 'revision-a', rows);
  const reader = s.track(new DatabaseSync(s.path));
  assert.equal(writer.prepare('PRAGMA journal_mode').get().journal_mode, 'wal');
  reader.exec('BEGIN');
  const initial = readSnapshot(reader); // First SELECT pins the WAL snapshot.
  assert.equal(initial.revision, 'revision-a');
  let checkpoints = 0;
  publishRevision(writer, 'revision-b', changedRows, () => {
    assert.deepEqual(readSnapshot(reader), initial);
    checkpoints++;
  });
  assert.deepEqual(readSnapshot(reader), initial);
  const freshReader = s.track(new DatabaseSync(s.path));
  freshReader.exec('BEGIN');
  const published = readSnapshot(freshReader);
  freshReader.exec('COMMIT');
  assert.equal(published.revision, 'revision-b');
  assert.deepEqual(published.rows, changedRows);
  reader.exec('COMMIT; BEGIN');
  assert.deepEqual(readSnapshot(reader), published);
  reader.exec('COMMIT');
  assert.equal(checkpoints, 3);
  return {journalMode: 'wal', independentConnections: 3, checkpoints,
    pinnedDigest: initial.digest, publishedDigest: published.digest,
    newTransactionSeesPublishedRevision: true};
});

for (const failure of ['exception', 'injected-cancellation']) {
  checkedTest(`${failure} rolls back deleted and partially inserted rows`, t => {
    const s = sandbox(t);
    let writer = s.track(openCache(s.path));
    publishRevision(writer, 'revision-a', rows);
    const before = readSnapshot(writer);
    const stages = ['after-delete', 'mid-insert', 'before-commit'];
    for (const stage of stages) {
      const error = new Error(`injected ${failure} at ${stage}`);
      if (failure === 'injected-cancellation') error.name = 'AbortError';
      assert.throws(() => publishRevision(writer, 'revision-b', changedRows, current => {
        if (current === stage) throw error;
      }), actual => actual === error);
      assert.deepEqual(readSnapshot(writer), before);
      s.close(writer);
      writer = s.track(openCache(s.path));
      assert.deepEqual(readSnapshot(writer), before);
    }
    publishRevision(writer, 'revision-b', changedRows);
    assert.equal(readSnapshot(writer).revision, 'revision-b');
    return {injectedAt: stages, reopenedAfterEachRollback: true,
      recoveredWriterPublishes: true, actualWorkerCancellation: false};
  });
}

checkedTest('deleted cache rebuilds identical canonical rows from the same graph', t => {
  const s = sandbox(t);
  let cache = s.track(openCache(s.path));
  publishRevision(cache, 'revision-a', rows);
  const before = readSnapshot(cache);
  s.close(cache);
  for (const suffix of ['', '-wal', '-shm']) rmSync(s.path + suffix, {force: true});
  cache = s.track(openCache(s.path));
  assert.equal(readSnapshot(cache).revision, null);
  const reordered = {...graph, nodes: [...graph.nodes].reverse()};
  const rebuiltRows = canonicalRows(reordered);
  assert.deepEqual(rebuiltRows, rows);
  publishRevision(cache, 'revision-a', rebuiltRows);
  assert.deepEqual(readSnapshot(cache), before);
  return {canonicalRowCount: rows.length, beforeDigest: before.digest,
    rebuiltDigest: readSnapshot(cache).digest, deletedCacheFiles: true};
});

checkedTest('workspace annotation survives cache rebuild and becomes explicit orphan after target rename', t => {
  const s = sandbox(t);
  const workspacePath = join(s.dir, 'workspace.db');
  let workspace = s.track(openWorkspace(workspacePath));
  let cache = s.track(openCache(s.path));
  const targetId = graph.nodes[0].id;
  workspace.prepare('INSERT INTO annotations VALUES (?, ?, ?)').run('note-1', targetId, 'Keep this durable user note');
  publishRevision(cache, 'revision-a', rows);
  const original = resolveAnnotation(workspace, cache, 'note-1');
  assert.equal(original.status, 'resolved');
  s.close(workspace);
  s.close(cache);
  for (const suffix of ['', '-wal', '-shm']) rmSync(s.path + suffix, {force: true});
  workspace = s.track(openWorkspace(workspacePath));
  cache = s.track(openCache(s.path));
  publishRevision(cache, 'revision-a', rows);
  assert.deepEqual(resolveAnnotation(workspace, cache, 'note-1'), original);
  const renamed = structuredClone(graph);
  const target = renamed.nodes.find(node => node.id === targetId);
  target.id += ':renamed';
  target.name += '-renamed';
  publishRevision(cache, 'revision-renamed', canonicalRows(renamed));
  const orphan = resolveAnnotation(workspace, cache, 'note-1');
  assert.deepEqual(orphan, {...original, status: 'orphan', reason: 'target-not-in-current-revision'});
  assert.equal(workspace.prepare('SELECT COUNT(*) AS n FROM annotations').get().n, 1);
  s.close(workspace);
  workspace = s.track(openWorkspace(workspacePath));
  assert.deepEqual(resolveAnnotation(workspace, cache, 'note-1'), orphan);
  return {targetId, separateWorkspaceDatabase: true, annotationSurvivesReopen: true,
    survivesSameGraphRebuild: true, renamedTargetStatus: orphan.status,
    orphanReason: orphan.reason, automaticRetargeting: false};
});

after(() => {
  const passed = observations.filter(item => item.status === 'passed').length;
  mkdirSync(generatedPath(''), {recursive: true});
  writeFileSync(generatedPath('lifecycle-results.json'), JSON.stringify({
    schemaVersion: 1, scope: 'Bounded SQLite revision-publication spike; not a production schema',
    command: 'node --test lifecycle.test.mjs', runtime: process.version,
    input: 'feature-factory.graph.json', canonicalInputDigest: digest(rows),
    canonicalRowCount: rows.length, expectedTests, passed,
    status: passed === expectedTests ? 'passed' : 'failed', observations, limitations,
  }, null, 2) + '\n');
});
