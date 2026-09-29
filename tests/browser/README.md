# Saved-item browser acceptance

This test drives the real Baleyg web shell in Playwright Chromium. It starts the compiled daemon with an isolated workspace, token, home, and XDG directories. It does not mock or intercept application requests.

## Provision once per checkout

An absolute, writable run-cache directory outside the checkout is required. From the repository root, choose that directory and install the locked package and managed browser into it:

```sh
export BALEYG_RUN_CACHE=/path/to/external/run-cache
mkdir -p "$BALEYG_RUN_CACHE/npm-cache" "$BALEYG_RUN_CACHE/playwright-browsers"
npm_config_cache="$BALEYG_RUN_CACHE/npm-cache" \
  npm ci --prefix tests/browser --ignore-scripts
PLAYWRIGHT_BROWSERS_PATH="$BALEYG_RUN_CACHE/playwright-browsers" \
  node tests/browser/node_modules/playwright/cli.js install chromium
```

The package lock pins `playwright` to exactly `1.55.1`. Do not use `npx`, a system browser install, or an unrelated shared browser cache. Local provisioning must not use `--with-deps`; the Linux CI job is the only exception because it uses `--with-deps` to install the required system libraries. The macOS CI job uses the normal `install chromium` command.

The ACP verifier dependency is separate and still requires:

```sh
npm ci --prefix runtime/acp --ignore-scripts
```

## Run

Keep the same explicit browser path in the test environment:

```sh
export BALEYG_RUN_CACHE=/path/to/external/run-cache
PLAYWRIGHT_BROWSERS_PATH="$BALEYG_RUN_CACHE/playwright-browsers" \
  node tests/browser/saved-items.test.cjs
```

The harness fails if the environment variable, locked package, managed Chromium executable, Cargo build, daemon, or any browser assertion is unavailable. It has no synthetic-DOM fallback and never skips acceptance.
