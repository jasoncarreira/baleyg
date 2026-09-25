const assert = require("node:assert/strict");
const { chmodSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } = require("node:fs");
const { tmpdir } = require("node:os");
const { delimiter, join } = require("node:path");
const { spawnSync } = require("node:child_process");
const test = require("node:test");

const ROOT = join(__dirname, "..");
const CONFIG_PATH = join(ROOT, ".factory.json");
const REQUIRED = ["resolve", "verify"];
const ALLOWED = new Set([
  ...REQUIRED,
  "publish",
  "pr_draft",
  "verify_timeout_ms",
  "bootstrap",
  "bootstrap_timeout_ms",
  "max_retries",
]);

function loadConfig() {
  return JSON.parse(readFileSync(CONFIG_PATH, "utf8"));
}

function makeFakeGh() {
  const dir = mkdtempSync(join(tmpdir(), "baleyg-factory-gh-"));
  const command = join(dir, "gh");
  writeFileSync(
    command,
    `#!/bin/sh
printf '%s\n' "$@" > "$FAKE_GH_LOG"
if [ "\${FAKE_GH_EXIT:-0}" -ne 0 ]; then exit "$FAKE_GH_EXIT"; fi
printf '%s\n' '{"run_id":"123","number":123,"title":"Story","body":"Body","url":"https://github.com/jasoncarreira/baleyg/issues/123","state":"OPEN","labels":[],"assignees":[]}'
`,
  );
  chmodSync(command, 0o755);
  return dir;
}

function resolve(config, fakeDir, input, extraEnv = {}) {
  const log = join(fakeDir, "args.log");
  const result = spawnSync("/bin/sh", ["-c", config.resolve], {
    cwd: ROOT,
    encoding: "utf8",
    env: {
      ...process.env,
      PATH: `${fakeDir}${delimiter}${process.env.PATH ?? ""}`,
      FACTORY_INPUT: input,
      FAKE_GH_LOG: log,
      ...extraEnv,
    },
  });
  let args = null;
  try {
    args = readFileSync(log, "utf8").trimEnd().split("\n");
  } catch {}
  return { ...result, args };
}

test("factory config uses the closed supported schema", () => {
  const config = loadConfig();
  assert.deepEqual(Object.keys(config).filter((key) => !ALLOWED.has(key)), []);
  for (const key of REQUIRED) {
    assert.equal(typeof config[key], "string");
    assert.notEqual(config[key].trim(), "");
  }
  assert.equal(Object.hasOwn(config, "publish"), false, "use the default PR publisher, not a push-only override");
  assert.equal(typeof config.pr_draft, "boolean");
  assert.equal(Number.isSafeInteger(config.verify_timeout_ms) && config.verify_timeout_ms > 0, true);
  assert.equal(typeof config.bootstrap, "string");
  assert.notEqual(config.bootstrap.trim(), "");
  assert.equal(Number.isSafeInteger(config.bootstrap_timeout_ms) && config.bootstrap_timeout_ms > 0, true);
  assert.equal(config.max_retries, 5);
});

test("resolver accepts only canonical Baleyg issue references", () => {
  const config = loadConfig();
  const fakeDir = makeFakeGh();
  try {
    const accepted = [
      "123",
      "#123",
      "https://github.com/jasoncarreira/baleyg/issues/123",
    ];
    for (const input of accepted) {
      const result = resolve(config, fakeDir, input);
      assert.equal(result.status, 0, input);
      assert.equal(JSON.parse(result.stdout).run_id, "123", input);
      assert.deepEqual(result.args, [
        "issue", "view", "123", "--repo", "jasoncarreira/baleyg", "--json",
        "number,title,body,url,state,labels,assignees", "--jq",
        "{run_id:(.number|tostring)} + .",
      ], input);
    }

    const rejected = [
      "", "0", "01", "+123", "-123", " 123", "123 ", "#abc", "123\n456",
      "https://github.com/other/baleyg/issues/123",
      "https://github.com/jasoncarreira/baleyg/issues/123/extra",
      "https://github.com/jasoncarreira/baleyg/issues/123?state=open",
      "not an issue",
    ];
    for (const input of rejected) {
      rmSync(join(fakeDir, "args.log"), { force: true });
      const result = resolve(config, fakeDir, input);
      assert.equal(result.status, 0, input);
      assert.equal(result.stdout, "", input);
      assert.equal(result.args, null, input);
    }
  } finally {
    rmSync(fakeDir, { recursive: true, force: true });
  }
});

test("recognized issue lookup failures remain failures", () => {
  const config = loadConfig();
  const fakeDir = makeFakeGh();
  try {
    const result = resolve(config, fakeDir, "123", { FAKE_GH_EXIT: "23" });
    assert.equal(result.status, 23);
    assert.deepEqual(result.args?.slice(0, 3), ["issue", "view", "123"]);
  } finally {
    rmSync(fakeDir, { recursive: true, force: true });
  }
});

test("verify is one executable command and retains all checks", () => {
  const config = loadConfig();
  assert.equal(config.verify, "./tools/verify");
  assert.notEqual(statSync(join(ROOT, "tools/verify")).mode & 0o111, 0);
  const fakeDir = mkdtempSync(join(tmpdir(), "baleyg-factory-verify-"));
  const log = join(fakeDir, "verify.log");
  const fakeTool = `#!/bin/sh
printf '%s' "\${0##*/}" >> "$FAKE_VERIFY_LOG"
printf '\\t%s' "$@" >> "$FAKE_VERIFY_LOG"
printf '\\n' >> "$FAKE_VERIFY_LOG"
if [ "\${FAKE_VERIFY_FAIL_CLIPPY:-0}" = 1 ] && [ "$1" = clippy ]; then exit 23; fi
`;
  try {
    for (const name of ["cargo", "node"]) {
      const command = join(fakeDir, name);
      writeFileSync(command, fakeTool);
      chmodSync(command, 0o755);
    }
    function runVerify(extraEnv = {}) {
      const result = spawnSync(config.verify, [], {
        cwd: ROOT,
        encoding: "utf8",
        env: {
          ...process.env,
          PATH: `${fakeDir}${delimiter}${process.env.PATH ?? ""}`,
          FAKE_VERIFY_LOG: log,
          ...extraEnv,
        },
      });
      const commands = readFileSync(log, "utf8").trimEnd().split("\n").map((line) => line.split("\t"));
      return { result, commands };
    }
    const expected = [
      ["cargo", "fmt", "--all", "--", "--check"],
      ["cargo", "clippy", "--locked", "--all-targets", "--", "-D", "warnings"],
      ["cargo", "test", "--locked", "--all-targets"],
      ["node", "--test", "runtime/acp/runner.test.mjs"],
      ["node", "--test", "tests/factory-config.test.cjs"],
      ["node", "--check", "web/app.js"],
      ["node", "--test", "tests/question-ui.test.cjs", "tests/browse-ui.test.cjs", "tests/sequence-ui.test.cjs", "tests/token-ui.test.cjs", "tests/external-source-ui.test.cjs", "tests/dependency-ui.test.cjs", "tests/shell-ui.test.cjs", "tests/classes-ui.test.cjs", "tests/navigation-ui.test.cjs"],
    ];
    const success = runVerify();
    assert.equal(success.result.status, 0, success.result.stderr);
    assert.deepEqual(success.commands, expected);
    rmSync(log);
    const failure = runVerify({ FAKE_VERIFY_FAIL_CLIPPY: "1" });
    assert.equal(failure.result.status, 23);
    assert.deepEqual(failure.commands, expected.slice(0, 2));
  } finally {
    rmSync(fakeDir, { recursive: true, force: true });
  }
});
