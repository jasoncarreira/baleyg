import test from "node:test";
import assert from "node:assert/strict";
import {
  chmod,
  cp,
  mkdtemp,
  readdir,
  readFile,
  rm,
  writeFile,
  mkdir,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import {
  generateFixture,
  checkPublication,
  compileFixture,
} from "../publish.mjs";
import { parseArgs, main } from "../cli.mjs";
import { contentHash } from "../identity.mjs";
import { registerControls, runControl } from "./mutations.mjs";

const example = fileURLToPath(
  new URL(
    "../../../tests/fixtures/semantic-evidence/v1/example/",
    import.meta.url,
  ),
);
async function copyFixture() {
  const parent = await mkdtemp(join(tmpdir(), "baleyg-publication-"));
  const root = join(parent, "example");
  await cp(example, root, { recursive: true });
  return {
    parent,
    root,
    cleanup: () => rm(parent, { recursive: true, force: true }),
  };
}
async function publicationBytes(root, manifest) {
  return Promise.all(
    [
      "manifest.json",
      ...["records", "answers", "counts"].map((name) => manifest[name].path),
    ].map(async (path) => [
      path,
      await readFile(join(root, "generated", path)),
    ]),
  );
}
async function tree(root) {
  const paths = [];
  async function visit(folder) {
    for (const entry of await readdir(folder, { withFileTypes: true })) {
      const path = join(folder, entry.name);
      if (entry.isDirectory()) await visit(path);
      else paths.push([path.slice(root.length + 1), await readFile(path)]);
    }
  }
  await visit(root);
  return paths.sort(([a], [b]) =>
    Buffer.compare(Buffer.from(a), Buffer.from(b)),
  );
}
async function addWhitespace(root) {
  const path = join(root, "expected/answers.json");
  await writeFile(
    path,
    Buffer.concat([await readFile(path), Buffer.from(" ")]),
  );
}
function rejects(error, assertion, field) {
  assert.equal(error.assertion, assertion);
  assert.equal(error.code, "invalidRecord");
  assert.equal(error.field, field);
  return true;
}

test("publication: independent pristine generations are byte-identical; both check commands never write", async () => {
  const firstCopy = await copyFixture(),
    secondCopy = await copyFixture();
  try {
    const first = await generateFixture(firstCopy.root);
    const firstBytes = await publicationBytes(firstCopy.root, first);
    const second = await generateFixture(secondCopy.root);
    assert.deepEqual(second, first);
    assert.deepEqual(
      await publicationBytes(secondCopy.root, second),
      firstBytes,
    );
    const rerun = await generateFixture(firstCopy.root);
    assert.deepEqual(rerun, first);
    assert.deepEqual(await publicationBytes(firstCopy.root, rerun), firstBytes);
    const before = await tree(firstCopy.root);
    assert.deepEqual(
      await main(["generate", "--check", "--fixtures-root", firstCopy.parent]),
      first,
    );
    assert.deepEqual(
      await main([
        "check",
        "--fixtures-root",
        firstCopy.parent,
        "--fixture",
        "example",
      ]),
      first,
    );
    assert.deepEqual(await tree(firstCopy.root), before);
  } finally {
    await Promise.all([firstCopy.cleanup(), secondCopy.cleanup()]);
  }
});

test("publication: admitted native raw order and native IDs do not change semantic IDs", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    const initial = await compileFixture(root);
    const file = join(root, "captures/native.json"),
      raw = JSON.parse(await readFile(file, "utf8"));
    for (const field of ["declarations", "calls", "controls", "references"]) {
      raw[field].reverse();
      for (const [index, row] of raw[field].entries())
        row.nativeId = `new-native-${field}-${index}`;
    }
    await writeFile(file, JSON.stringify(raw));
    const changed = await compileFixture(root);
    assert.deepEqual(changed.values.records, initial.values.records);
    assert.notEqual(changed.manifest.inputHash, initial.manifest.inputHash);
    assert.equal(
      changed.manifest.bundleHash === initial.manifest.bundleHash,
      false,
    );
  } finally {
    await cleanup();
  }
});

test("publication: changed admitted answer bytes invalidate manifest with zero writes", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    await generateFixture(root);
    await addWhitespace(root);
    const before = await tree(root);
    await assert.rejects(
      () => checkPublication(root),
      (error) => rejects(error, "PUBLICATION.MANIFEST", "manifest"),
    );
    await assert.rejects(
      () => generateFixture(root, { check: true }),
      (error) => rejects(error, "PUBLICATION.MANIFEST", "manifest"),
    );
    assert.deepEqual(await tree(root), before);
  } finally {
    await cleanup();
  }
});

test("publication: failed compile or bundle write preserves prior valid manifest and bundle", async () => {
  for (const failure of ["compile", "write"]) {
    const { root, cleanup } = await copyFixture();
    try {
      const previous = await generateFixture(root),
        bytes = await publicationBytes(root, previous);
      const expected = join(root, "expected/answers.json"),
        original = await readFile(expected);
      const bundles = join(root, "generated/bundles");
      try {
        if (failure === "compile") await writeFile(expected, "{");
        else {
          await addWhitespace(root);
          await chmod(bundles, 0o555);
        }
        const before = await tree(root);
        await assert.rejects(
          () => generateFixture(root),
          failure === "compile" ? /JSON\.INTAKE / : { code: "EACCES" },
        );
        await chmod(bundles, 0o755);
        assert.deepEqual(await tree(root), before, failure);
      } finally {
        await chmod(bundles, 0o755);
        await writeFile(expected, original);
      }
      assert.deepEqual(await checkPublication(root), previous, failure);
      assert.deepEqual(await publicationBytes(root, previous), bytes, failure);
      assert.deepEqual(
        (await readdir(join(root, "generated"))).sort(),
        ["bundles", "manifest.json"],
        failure,
      );
      assert.deepEqual(await readdir(bundles), [previous.bundleHash], failure);
    } finally {
      await cleanup();
    }
  }
});

test("publication: conflicting existing immutable bundle is rejected without replacement", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    const previous = await generateFixture(root),
      original = await publicationBytes(root, previous);
    await addWhitespace(root);
    const future = (await compileFixture(root)).manifest;
    const directory = join(root, "generated/bundles", future.bundleHash);
    await mkdir(directory);
    await writeFile(join(directory, "records.json"), "collision");
    const before = await tree(root);
    await assert.rejects(
      () => generateFixture(root),
      (error) => rejects(error, "PUBLICATION.IMMUTABLE", "records"),
    );
    assert.deepEqual(await tree(root), before);
    assert.deepEqual(await publicationBytes(root, previous), original);
  } finally {
    await cleanup();
  }
});

test("publication: published hash changes fail exact hash assertion without writes", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    const manifest = await generateFixture(root);
    const file = join(root, "generated", manifest.counts.path);
    await writeFile(file, "{}");
    const before = await tree(root);
    await assert.rejects(
      () => checkPublication(root),
      (error) =>
        rejects(error, "PUBLICATION.HASH", "counts") &&
        error.message.includes(`expected ${manifest.counts.hash}`) &&
        error.message.includes(`actual ${contentHash(Buffer.from("{}"))}`),
    );
    assert.deepEqual(await tree(root), before);
  } finally {
    await cleanup();
  }
});

test("publication: manifest paths, hashes and canonical bytes are checked without writes", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    const manifest = await generateFixture(root),
      file = join(root, "generated/manifest.json");
    const original = await readFile(file);
    assert.notEqual(original.at(-1), 10);
    for (const name of ["records", "answers", "counts"]) {
      const bytes = await readFile(
        join(root, "generated", manifest[name].path),
      );
      assert.notEqual(bytes.at(-1), 10);
    }
    const altered = structuredClone(manifest);
    altered.records.path = `bundles/${manifest.bundleHash}/wrong.json`;
    await writeFile(file, JSON.stringify(altered));
    const before = await tree(root);
    await assert.rejects(
      () => checkPublication(root),
      (error) => rejects(error, "PUBLICATION.PATH", "records"),
    );
    assert.deepEqual(await tree(root), before);
    await writeFile(file, Buffer.concat([original, Buffer.from(" ")]));
    await assert.rejects(
      () => checkPublication(root),
      (error) => rejects(error, "PUBLICATION.MANIFEST", "manifest"),
    );
  } finally {
    await cleanup();
  }
});

test("CLI rejects unknown/duplicate flags, missing values and unknown fixtures", () => {
  for (const args of [
    ["generate", "--other"],
    ["check", "--check"],
    ["generate", "--fixture"],
    ["generate", "--fixture", "example", "--fixture", "example"],
    ["generate", "--fixture", "else"],
  ])
    assert.throws(() => parseArgs(args), /CLI\./);
});

test("publication control: independently edited input rejects the exact manifest", async () => {
  const { root, cleanup } = await copyFixture();
  try {
    const rows = registerControls([
      {
        id: "PUBLICATION.MANIFEST.changed-expected-bytes",
        baseline: async () => {
          await generateFixture(root);
          return { root };
        },
        check: (value) => checkPublication(value.root),
        mutate: async (value) => {
          await addWhitespace(value.root);
          return value;
        },
        expectedAssertion: "PUBLICATION.MANIFEST",
        expectedCode: "invalidRecord",
        expectedField: "manifest",
      },
    ]);
    assert.equal(await runControl(rows[0]), rows[0].id);
  } finally {
    await cleanup();
  }
});
