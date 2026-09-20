# Baleyg

A diagram-first code browser with native JavaScript, Rust, Java and Python indexing, SQLite snapshots,
and an authenticated local inspector. Browse files, expand their functions/methods, and
open source-linked static sequence diagrams. These are possible paths, not runtime traces.
Optional Jev selection and ACP answers are separate, explicitly authorized provider features.

## Start here

Requires stable Rust/Cargo and a native C toolchain (tree-sitter and bundled SQLite).
The authenticated daemon currently requires Unix private-file permissions.

```sh
cargo test --locked --all-targets
cargo run --locked -- index
cargo run --locked -- serve
```

Open **http://127.0.0.1:8877/**. Paste the token or load the private token file whose path
is printed by `serve`. Optional **Remember token on this browser** stores it locally;
Disconnect forgets it. See [token storage](docs/browser-token.md).
The workspace defaults to cwd; `--workspace PATH` changes both the browser and index roots.
The daemon does not index automatically: use **Index workspace** after source changes.
Stop the daemon with Ctrl-C.

Without SCIP, calls are visible as unresolved syntax; Baleyg does not guess their targets.
For the existing feature-factory snapshot with semantic resolution, see
[the daemon guide](docs/daemon-v1.md#use-the-existing-semantic-snapshot).

State defaults to a per-workspace OS application-data directory **outside the repository**.
Use `--state-dir /path/to/state` to override it. A state directory belongs to one canonical
workspace. Indexing never runs package scripts, installs source dependencies, or edits source.

## What works

- Native tree-sitter JavaScript (`.js`, `.mjs`, `.cjs`), Rust (`.rs`), Java (`.java`) and Python (`.py`) extraction.
- Optional JavaScript SCIP import; Rust, Java and Python currently have syntax-only, unresolved call evidence.
- Source hashes, provenance, explicit unresolved boundaries, callbacks and control regions.
- Atomic full-index publication to SQLite; background refresh, progress and cancellation.
- Snapshot-consistent source reads and bounded, cycle-safe static call queries.
- Durable saved queries, pins, hidden IDs and annotations; explicit orphan reporting.
- Loopback-only API with bearer authentication, Host/Origin checks and a cached-source viewer.
- Separate offline question focus: bounded evidence, literal-name preview, and strict display limits.
- Jev request export, unverified response import, and explicit budget-controlled live selection.
- Inline file → method browsing and source-linked static sequence diagrams for all four supported languages.
- Java/Python class diagrams with scoped candidate links, right-click related-class expansion, and source/method navigation.
- Reversible fluent Rust chain grouping, with every measured call retained.
- Automatic local Rust/Cargo library declaration catalog and explicit source viewing. Candidate type lanes are terminal; compiler receiver/dispatch resolution is still unavailable.
- Compact outgoing call rows with deliberate one-level branch expansion.
- Opt-in ACP answers with source-quote validation and a separate attempt allowance.

The workbench starts with immediate calls. Select a call or control to inspect its evidence,
then open its cached source. Expand deeper only when needed. Unsupported language constructs
remain explicit boundaries; parsing is not type resolution or an observed execution trace.
Java/Python indexing never runs Gradle, Maven, Python imports, decorators or package commands.

## Planned agent and semantic integration

The accepted design supports agents running directly in a terminal (including terminal tabs in
Baleyg or optional Herdr panes), and agents connected through ACP, primarily Mimir. Both use the
same portable MCP tools. MCP, general coding-agent ACP sessions and embedded terminals are
**not implemented yet**; the existing one-shot ACP answer feature is separate.

Start with a four-tool read-only MCP pilot against one existing workspace and server-enforced scoped
grants. Add snapshot text search, versioned diagram artifacts, the unified terminal workbench and
multi-project routing as separate slices. Extend SCIP import beyond JavaScript through tested
language adapters, initially Java; producing semantic artifacts remains explicitly authorized work.

See the [integration plan](docs/agent-integration-plan.md), [pilot contract](docs/mcp-readonly-pilot-contract.md),
[terminal workbench](docs/terminal-workbench-contract.md), [SCIP roadmap](docs/scip-multilanguage-plan.md),
and [optional LSP assessment](docs/lsp-integration-plan.md).

## Development

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
node --test tests/question-ui.test.cjs
cargo run --locked -- --help
```

The browser client is embedded in the executable. Rebuild after editing `web/`.
Tests are offline and use temporary workspaces; no provider credentials or inference is used.
The existing selection-experiment budget is closed and is not changed by this implementation.

## Repository layout

- `src/`, `web/`, `runtime/`: daemon, browser and ACP answer adapter.
- `tests/fixtures/`: synthetic fixtures and retained feature-factory evidence.
- `tools/extraction/`, `tools/selection/`: standalone offline research harnesses.
- `docs/research/`: historical reports and screenshots, separate from runtime implementation.

See [research evidence](docs/research/README.md) before running historical tooling. Provider trials
and semantic-index generation require separate authorization. Private application artifacts are
excluded; local archives and runtime state must not be committed.

## License

MIT, see [LICENSE](LICENSE). Bundled fonts keep their own SIL OFL licenses under `web/fonts/`;
the Feature Factory fixtures under `tests/fixtures/` carry their original project's terms.

## Read more

- [Architecture and accepted direction](SPEC.md)
- [MCP-first direct/ACP agent integration plan](docs/agent-integration-plan.md)
- [Single-workspace read-only MCP pilot and scoped grants](docs/mcp-readonly-pilot-contract.md)
- [Pilot implementation slices and authorization boundary](docs/mcp-pilot-implementation-plan.md)
- [Slice 0 storage and cancellation findings](docs/mcp-pilot-slice0-findings.md)
- [Unified terminal and ACP workbench design](docs/terminal-workbench-contract.md)
- [Multi-language SCIP semantic import roadmap](docs/scip-multilanguage-plan.md)
- [Optional LSP bridge: Java-first assessment](docs/lsp-integration-plan.md)
- [Agent integration architecture diagrams](docs/architecture/agent-integration-context.md)
- [Daemon usage, data contract, security and limitations](docs/daemon-v1.md)
- [First-pass validation record](docs/daemon-validation.md)
- [Offline question focus and provider boundary](docs/question-workflow.md)
- [Question-slice validation](docs/question-validation.md)
- [Live Jev opt-in, source sharing and durable budget](docs/live-jev.md)
- [Live validation and retained failures](docs/live-jev-validation.md)
- [Source-backed ACP answers and separate authorization](docs/acp-answers.md)
- [ACP validation and current authentication blocker](docs/acp-answer-validation.md)
- [Automatic local libraries and resolution limits](docs/automatic-libraries.md)
- [Rust indexing and static sequences](docs/rust-support.md)
- [Java and Python indexing and static sequences](docs/java-python-support.md)
- [Class diagrams and right-click related classes](docs/class-diagrams.md)
- [Current-directory file tree and external participants](docs/cwd-file-browser.md)
- [File → method → sequence workflow](docs/file-method-sequence.md)
- [Extraction spike](docs/research/EXTRACTION-RESULTS.md)
- [Jev/Opus exploratory comparison](docs/research/selection/HARD-RESULTS.md)

Next: the scoped read-only MCP pilot, multi-language SCIP import, and separately gated snapshot
search, diagram artifacts and embedded terminal/ACP integration. Continue static sequence coverage
and library adapters beyond Rust/Cargo; source-backed provider validation still requires authorization
and working authentication.
The local literal preview does not understand questions. TypeScript parsing, file watching,
terminals, React/Tauri and database diagrams remain outside the current implementation.
