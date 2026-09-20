# Codex ACP compatibility check

User authorized trying Codex as an alternative. No Codex model call was made.

Checked installed Codex CLI 0.150.1 and public source for the Zed adapter
`@zed-industries/codex-acp@0.16.0`, which embeds Codex 0.137.0 rather than using
that installed CLI. The adapter accepts repeated `-c key=value` overrides, not
`codex exec` flags.

The present answer runner deliberately gives the model no repository tools. The checked
Codex adapter/core could not be shown to meet this boundary: disabling shell execution
still leaves other tool paths, including conditional image-reading and patch tools. A
read-only filesystem mode does not prevent private-file reads, and denying permission
requests is not proof that every read operation requires permission. Ambient config/MCP
also needs isolation. Codex is therefore not silently substituted for the Claude runner.

Native structured-output enforcement is absent in the checked ACP prompt paths, but that
alone is **not a blocker**: Baleyg can validate prompted JSON text independently. The open
issue is limiting source/file/tool access without weakening the declared evidence-only
boundary. A separately reviewed isolated runtime or a tool-disable adapter change is
needed before enabling Codex under the same promise.

Sources:
- https://github.com/zed-industries/codex-acp/tree/v0.16.0
- https://github.com/openai/codex/blob/rust-v0.137.0/codex-rs/core/src/tools/spec_plan.rs
- https://github.com/openai/codex/blob/rust-v0.137.0/codex-rs/core/config.schema.json


## Subsequent authorized CLI smoke test

Codex CLI 0.150.1 successfully returned `CODEX_OK` with `gpt-5.5`, low reasoning,
in an empty read-only workspace. No repository source was supplied; the event stream
showed only an agent message and a completed turn, with no tool calls. The session was
ephemeral. Earlier launches rejected unsupported retry overrides and then the obsolete
`gpt-5.2` model choice; the successful choice came from the CLI's current model catalog.

This verifies CLI authentication and inference, **not ACP integration or a general
no-filesystem-tools guarantee**. The ACP compatibility concerns above remain open.
[Smoke metadata](codex-cli-smoke.json). Neither existing budget ledger was modified.
