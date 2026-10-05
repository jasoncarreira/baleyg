# Saved-item browser acceptance

This test drives the real Baleyg web shell in Playwright Chromium. It starts the compiled daemon with an isolated workspace, token, home, and XDG directories. It does not mock or intercept application requests.

## Verify from a clean checkout

From the repository root, run `./tools/verify`. Its sequential setup step installs the
locked `runtime/acp` and `tests/browser` Node packages when missing or stale, then checks
and repairs the pinned Playwright Chromium bundles. Setup completes before the parallel verification
lanes start. The package lock pins `playwright` to exactly `1.55.1`; setup never uses `npx`,
a system browser, or `--with-deps`. A first run needs npm registry and browser-download access.

By default, setup uses an owner-private, stable **per-checkout** cache under the OS temp
directory, outside the repository. Set `BALEYG_RUN_CACHE` to an absolute, user-owned,
non-group-writable external directory to keep the npm and browser cache at a known location.
An existing absolute external `PLAYWRIGHT_BROWSERS_PATH` (including CI's) is honored when
it has the same safe ownership and permissions. Invalid, symlinked, or in-checkout cache
paths fail verification; neither missing packages nor a missing browser are skipped.
Run only one verifier per checkout at a time because `npm ci` replaces ignored dependencies.

Linux still needs Chromium's host libraries. CI installs them with `install --with-deps
chromium` before `./tools/verify`; local setup does not request privileged OS changes.
The macOS CI job installs Chromium without `--with-deps`.

## Run the browser test alone

A direct test invocation does **not** run verifier setup. Provision once per checkout with
the same external cache and pinned installer:

```sh
export BALEYG_RUN_CACHE=/path/to/external/run-cache
mkdir -p "$BALEYG_RUN_CACHE/npm-cache" "$BALEYG_RUN_CACHE/playwright-browsers"
npm_config_cache="$BALEYG_RUN_CACHE/npm-cache" \
  npm ci --prefix tests/browser --ignore-scripts
PLAYWRIGHT_BROWSERS_PATH="$BALEYG_RUN_CACHE/playwright-browsers" \
  node tests/browser/node_modules/playwright/cli.js install chromium
PLAYWRIGHT_BROWSERS_PATH="$BALEYG_RUN_CACHE/playwright-browsers" \
  node tests/browser/saved-items.test.cjs
```

The harness fails if the environment variable, locked package, managed Chromium executable, Cargo build, daemon, or any browser assertion is unavailable. It has no synthetic-DOM fallback and never skips acceptance.
