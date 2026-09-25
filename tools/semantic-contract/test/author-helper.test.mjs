import test from 'node:test';
import assert from 'node:assert/strict';
import {cp, mkdtemp, readFile, readdir, rm, writeFile} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {fileURLToPath} from 'node:url';
import {authorValues, main, parseArgs} from '../author-helper.mjs';
import {contentHash, sourceManifestHash, headerHash, siblingGroupHash, syntaxId, occurrenceId} from '../identity.mjs';
import {parseJson, canonicalBytes} from '../json.mjs';

const same = (a, b) => canonicalBytes(a).equals(canonicalBytes(b));
const example = fileURLToPath(new URL('../../../tests/fixtures/semantic-evidence/v1/example/', import.meta.url));

async function tree(root) {
  const files = [];
  async function walk(dir, prefix = '') {
    for (const entry of await readdir(dir, {withFileTypes: true})) {
      const path = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await walk(join(dir, entry.name), path);
      else files.push([path, contentHash(await readFile(join(dir, entry.name)))]);
    }
  }
  await walk(root);
  return files.sort((a, b) => a[0].localeCompare(b[0]));
}

test('AUTHOR.IDS computes native declaration, header and occurrence identities without answers', async t => {
  const root = await mkdtemp(join(tmpdir(), 'semantic-author-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  await cp(example, root, {recursive: true});
  const fixture = parseJson(await readFile(join(root, 'fixture.json')));
  await rm(join(root, fixture.answersFile));
  await rm(join(root, fixture.dispositionsFile));
  await rm(join(root, fixture.anchorCasesFile));
  const before = await tree(root);
  const ids = await authorValues(root, 'ids');
  const recordsManifest = parseJson(await readFile(join(example, 'generated/manifest.json')));
  const records = parseJson(await readFile(join(example, 'generated', recordsManifest.records.path)));
  for (const row of ids.declarations) {
    const declaration = records.declarations.find(x => x.syntaxId === row.syntaxId && x.revisionId === row.revisionId && x.document.path === row.document.path);
    assert.ok(declaration, row.ref);
    assert.deepEqual(row.key, {...declaration.key}, row.ref);
    assert.equal(row.headerHash, headerHash(declaration.header), row.ref);
    const siblings = records.declarations.filter(x => x.revisionId === row.revisionId &&
      same(x.document, row.document) &&
      same(x.ancestors, row.ancestors) &&
      x.key.kind === row.key.kind && x.key.name === row.key.name &&
      same(x.key.signature, row.key.signature))
      .sort((a, b) => a.range.start - b.range.start || a.range.end - b.range.end);
    const headers = siblings.map(x => headerHash(x.header));
    assert.equal(row.siblingGroupHash, siblingGroupHash(headers), row.ref);
    assert.equal(row.siblingCount, siblings.length, row.ref);
    assert.equal(row.identicalHeaderCount, headers.filter(x => x === row.headerHash).length, row.ref);
    assert.equal(row.syntaxId, syntaxId({sourceSet: row.document.sourceSetId, path: row.document.path,
      language: row.document.language, ancestors: row.ancestors, declaration: row.key}), row.ref);
  }
  for (const [kind, recordsKey] of [['calls', 'calls'], ['controls', 'controlRegions'], ['references', 'references']]) {
    for (const row of ids[kind]) {
      assert.equal(row.occurrenceId, occurrenceId({revisionId: row.revisionId,
        ownerSyntaxId: ids.declarations.find(x => x.ref === row.ownerRef).syntaxId,
        kind: kind === 'calls' ? 'call' : kind === 'controls' ? 'control' : 'reference', ordinal: row.ordinal}));
      if (kind !== 'references') assert.ok(records[recordsKey].some(x => x.id === row.occurrenceId), row.ref);
    }
  }
  assert.deepEqual(await tree(root), before, 'helper must not create or edit any fixture file');
});

test('AUTHOR.HASHES prints expected and actual captured bytes without writing expected files', async t => {
  const root = await mkdtemp(join(tmpdir(), 'semantic-hashes-'));
  t.after(() => rm(root, {recursive: true, force: true}));
  await cp(example, root, {recursive: true});
  const fixture = parseJson(await readFile(join(root, 'fixture.json')));
  const first = fixture.captures[0];
  await writeFile(join(root, first.file), 'different captured bytes');
  await rm(join(root, fixture.answersFile));
  const before = await tree(root);
  const hashes = await authorValues(root, 'hashes');
  assert.deepEqual(hashes.captures.find(x => x.ref === first.ref), {ref: first.ref, kind: first.kind,
    file: first.file, declaredHash: first.hash, actualHash: contentHash(Buffer.from('different captured bytes')), matches: false});
  for (const snapshot of hashes.snapshots) {
    assert.equal(snapshot.sourceManifestHash, sourceManifestHash(snapshot.documents.map(x => ({document: x.key, contentHash: x.contentHash}))));
    for (const document of snapshot.documents) assert.equal(document.contentHash, contentHash(await readFile(join(root, document.sourceFile))));
  }
  let output = '';
  await main(['hashes', root], {write: text => {output += text;}});
  assert.deepEqual(JSON.parse(output), JSON.parse(JSON.stringify(hashes)));
  assert.deepEqual(await tree(root), before);
});

test('AUTHOR.ARGS rejects any generation or arbitrary extra flags', () => {
  assert.throws(() => parseArgs(['generate']), /AUTHOR.ARGS/);
  assert.throws(() => parseArgs(['ids', 'example', '--write']), /AUTHOR.ARGS/);
});
