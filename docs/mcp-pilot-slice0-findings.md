# MCP pilot slice 0 — storage and cancellation findings

> **Superseded context (2026-09-23).** These findings were gathered for the grant-based pilot. The SQLite
> observations (a cold read-only open creates `-wal`/`-shm` sidecars; cancellation behaviour) remain
> useful facts for any reader of the index. The enrollment identity design they supported is
> replaced by the `indexGeneration` basis in the [stdio MCP contract](mcp-readonly-pilot-contract.md).

Status (historical): **slice 0 complete. Verdict at the time: GO — the grant-based pilot contract as
it stood before 2026-09-23 was implementable as written.** That verdict applies only to the
superseded contract (recoverable from Git history, commit `7122ed4`), not to the stdio contract now
at `mcp-readonly-pilot-contract.md`.

These are measured results from a throwaway spike, not shipped code. The spike used disposable
fixture databases in a scratch directory. No inspected repository, provider, credential, running
daemon or state directory was touched, and the spike source was not merged.

## Method

A single Rust program exercising `rusqlite` 0.40.2 against the bundled SQLite **3.53.2** — the exact
dependency set in `Cargo.lock`. Run on both platforms in the existing CI matrix
(`.github/workflows/rust.yml:9`): macOS natively, Linux in a `rust:1-slim` container, each as a
non-root user. `SQLITE_FCNTL_HAS_MOVED` was reached through `rusqlite::ffi` and
`unsafe Connection::handle()`; no new dependency was needed.

## Results

Identical on macOS and Linux.

| # | Case | Result |
| --- | --- | --- |
| 1 | Cold read-only open, no `-wal`/`-shm` present | Succeeds; **creates both sidecars** |
| 2 | Warm read-only open, writer holding `-shm` | Succeeds |
| 3 | Read-only open of a missing database | **Refused**; no file created |
| 4 | `HAS_MOVED` after replace (rename over) | **1 — detected** |
| 5 | `HAS_MOVED` after unlink | **1 — detected** |
| 6 | `HAS_MOVED` after rename away | **1 — detected** |
| 7 | `HAS_MOVED` after inode-preserving overwrite | **0 — not detected** |
| 8 | Cold read-only open in a mode-0500 directory | **Refused**: `attempt to write a readonly database` |
| 9 | Interrupt during a read inside a transaction | `SQLITE_INTERRUPT`; connection remains usable |
| 10 | Stray `interrupt()` with no statement running | No effect on later reads |

Reads continued to succeed after every mutation in cases 4–7, including after unlink. Detection
depends entirely on the identity checks; a failed read is not the signal.

## The three open questions

### 1. Post-interrupt connection reusability — RESOLVED, no amendment needed

An interrupted read returns `SQLITE_INTERRUPT` (extended code 9). The explicit transaction stays
open (`autocommit` remains false), but **the connection is immediately reusable — a subsequent read
succeeded before any rollback**. `ROLLBACK` then succeeds, `autocommit` returns to true, `HAS_MOVED`
still reads 0, and a fresh read transaction works normally.

Recovery is therefore ordinary error handling: catch `SQLITE_INTERRUPT`, roll back, continue serving.
The contract's rule against lazily replacing a connection or rebinding after an error is satisfiable,
and the feared outcome — one cancelled request latching MCP off for the daemon's lifetime — does not
occur. `Connection::get_interrupt_handle()` returns a `Send + Sync` handle usable from a watchdog
thread, so deadline enforcement can interrupt in-flight work rather than only abandoning the response.

A stray interrupt arriving with no statement running did not affect subsequent reads. The
implementation should still scope interrupts to a known in-flight request rather than relying on that.

### 2. Cold-start sidecar permissions — CONFIRMED as specified

A read-only main connection performs auxiliary writes: opening a WAL database with no sidecars
present creates `-wal` and `-shm`. With the containing directory non-writable, enrollment is refused
with a clear error rather than silently degrading. The daemon's own 0700 state directory
(`src/store.rs:44`, `secure_state_dir`) provides the needed permission, so this is not a blocker.

The contract's hedged wording — "Do not assume a read-only main connection means zero auxiliary
filesystem writes, nor that every WAL read always requires writable sidecars" — is correct as written
and should stay. Both halves were observed.

### 3. File-control availability — SUPPORTED on both platforms

`SQLITE_FCNTL_HAS_MOVED` returned `SQLITE_OK` on every call on both platforms with the bundled
SQLite. The `SQLITE_NOTFOUND` branch cannot be reached naturally here and must be covered by
injection, as the contract already anticipates.

The documented detection boundary is confirmed exactly: persistent replacement, deletion and rename
are detected; an inode-preserving overwrite is not. The contract's "Detection boundary" paragraph
needs no change.

## Recommended additions to the (superseded) contract

Both were test additions to the grant-based contract. The first still applies to any test that checks
a non-writable directory; the second applies to any cancellation test of a SQLite read.

1. **Acceptance test 7** should require the non-writable-directory case to run as a **non-root**
   user. Run as root, the same case silently succeeds, because root bypasses the directory
   permission check — the test would pass while proving nothing. This was observed directly: the
   containerised run reported success as root and refusal as uid 1000.
2. **Acceptance test 10** may record the measured interrupt contract — `SQLITE_INTERRUPT`, open
   transaction, reusable connection — so the implementation asserts it rather than rediscovering it.

## Spike limitations

- The inode-preserving overwrite fixture had the same row count as the original, so case 7 proves
  `HAS_MOVED` does not fire but does not demonstrate which content was subsequently served.
- One Linux distribution over overlayfs and one macOS filesystem were covered. Network filesystems
  were not, and SQLite's locking behavior there is known to differ.
- Only the bundled SQLite was exercised, which is what the project ships.

## Consequences for slices 1–5

No slice boundary moves. Slice 1 gains a concrete recovery policy — interrupt, expect
`SQLITE_INTERRUPT`, roll back, keep the connection — instead of an open question, and its enrollment
step must treat refusal to establish WAL coordination as a closed failure rather than a fallback.
