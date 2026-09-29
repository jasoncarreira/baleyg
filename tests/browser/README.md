# Saved-item browser acceptance

This test drives the real Baleyg web shell in Playwright Chromium. It starts the compiled daemon with an isolated workspace, token, home, and XDG directories. It does not mock or intercept application requests.

## Provision once per checkout

From the repository root, use the run-local cache and browser directory:

```sh
npm_config_cache=/Users/jcarreira/projects/odin/baleyg-factory-65-operator/.factory-sandboxes/65/.factory/65/npm-cache \
  npm ci --prefix tests/browser --ignore-scripts
PLAYWRIGHT_BROWSERS_PATH=/Users/jcarreira/projects/odin/baleyg-factory-65-operator/.factory-sandboxes/65/.factory/65/playwright-browsers \
  node tests/browser/node_modules/playwright/cli.js install chromium
```

The package lock pins `playwright` to exactly `1.55.1`. Do not use `npx`, `--with-deps`, a system browser install, or an external browser cache.

The ACP verifier dependency is separate and still requires:

```sh
npm ci --prefix runtime/acp --ignore-scripts
```

## Run

Keep the explicit browser path in the test environment:

```sh
PLAYWRIGHT_BROWSERS_PATH=/Users/jcarreira/projects/odin/baleyg-factory-65-operator/.factory-sandboxes/65/.factory/65/playwright-browsers \
  node tests/browser/saved-items.test.cjs
```

The harness fails if the environment variable, locked package, managed Chromium executable, Cargo build, daemon, or any browser assertion is unavailable. It has no synthetic-DOM fallback and never skips acceptance.
