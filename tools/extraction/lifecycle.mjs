// Bounded storage experiment, not an application or a production schema.
import { DatabaseSync } from 'node:sqlite';
import { createHash } from 'node:crypto';

export const limitations = [
  'Synchronous single-process experiment with separate SQLite connections; no worker or process crash was tested.',
  'Cancellation is an injected AbortError at a transaction checkpoint, not actual worker cancellation.',
  'cache.db and workspace.db have independent transactions; cross-database reads/writes are not atomic.',
  'Annotation resolution uses exact extracted node IDs only; renamed/removed targets become orphans, with no automatic identity migration.',
  'Canonical row equality is tested, not SQLite file-byte equality, scale, throughput, fsync durability under power loss, or production migrations.',
];

export function canonicalJSON(value) {
  if (Array.isArray(value)) return `[${value.map(canonicalJSON).join(',')}]`;
  if (value && typeof value === 'object') {
    return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonicalJSON(value[key])}`).join(',')}}`;
  }
  return JSON.stringify(value);
}

// Preserve every top-level graph value. Array rows are sorted by canonical
// payload; duplicate rows receive deterministic ordinal keys.
export function canonicalRows(graph) {
  const rows = [];
  for (const kind of Object.keys(graph).sort()) {
    const values = Array.isArray(graph[kind]) ? graph[kind] : [graph[kind]];
    const payloads = values.map(value => ({
      payload: canonicalJSON(value),
      entityId: value && typeof value === 'object' ? value.id ?? null : null,
    })).sort((a, b) => a.payload < b.payload ? -1 : a.payload > b.payload ? 1 : 0);
    payloads.forEach(({payload, entityId}, index) => rows.push({
      kind, rowKey: String(index).padStart(10, '0'), entityId, payload,
    }));
  }
  return rows;
}

export function digest(rows) {
  return createHash('sha256').update(canonicalJSON(rows)).digest('hex');
}

export function openCache(path) {
  const db = new DatabaseSync(path);
  db.exec(`PRAGMA journal_mode=WAL;
    CREATE TABLE IF NOT EXISTS revision (singleton INTEGER PRIMARY KEY CHECK(singleton = 1), id TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS graph_rows (
      kind TEXT NOT NULL, row_key TEXT NOT NULL, entity_id TEXT, payload TEXT NOT NULL,
      PRIMARY KEY(kind, row_key)
    );
    CREATE INDEX IF NOT EXISTS graph_entity ON graph_rows(kind, entity_id);`);
  return db;
}

export function publishRevision(db, revision, rows, checkpoint = () => {}) {
  db.exec('BEGIN IMMEDIATE');
  try {
    db.exec('DELETE FROM graph_rows; DELETE FROM revision;');
    checkpoint('after-delete');
    const insert = db.prepare('INSERT INTO graph_rows VALUES (?, ?, ?, ?)');
    rows.forEach((row, index) => {
      insert.run(row.kind, row.rowKey, row.entityId, row.payload);
      if (index === Math.floor(rows.length / 2)) checkpoint('mid-insert');
    });
    db.prepare('INSERT INTO revision VALUES (1, ?)').run(revision);
    checkpoint('before-commit');
    db.exec('COMMIT');
  } catch (error) {
    db.exec('ROLLBACK');
    throw error;
  }
}

// Call inside a caller-owned read transaction to pin the revision and all rows.
export function readSnapshot(db) {
  const revision = db.prepare('SELECT id FROM revision WHERE singleton = 1').get()?.id ?? null;
  const rows = db.prepare(`SELECT kind, row_key AS rowKey, entity_id AS entityId, payload
    FROM graph_rows ORDER BY kind, row_key`).all().map(row => ({...row}));
  return {revision, rows, digest: digest(rows)};
}

export function openWorkspace(path) {
  const db = new DatabaseSync(path);
  db.exec(`CREATE TABLE IF NOT EXISTS annotations (
    id TEXT PRIMARY KEY, target_id TEXT NOT NULL, body TEXT NOT NULL
  )`);
  return db;
}

export function resolveAnnotation(workspace, cache, id) {
  const annotation = workspace.prepare('SELECT id, target_id AS targetId, body FROM annotations WHERE id = ?').get(id);
  if (!annotation) return null;
  const found = cache.prepare("SELECT 1 FROM graph_rows WHERE kind = 'nodes' AND entity_id = ? LIMIT 1").get(annotation.targetId);
  return {...annotation, status: found ? 'resolved' : 'orphan', reason: found ? null : 'target-not-in-current-revision'};
}
