# ACP setup for the selection smoke experiment

> Publication copy: local absolute paths are replaced with labelled placeholders. This is not a verbatim capture or a new run. Byte-exact originals are retained in an ignored private archive; measured results and source hashes are unchanged.

## Investigation scope

Read-only provider investigation. No inference, login, authentication switch, adapter session, or paid request was run. No `.env` or credential files were read. Versions below came from local `--version`/`--help`, public npm metadata, official documentation, and pinned adapter source. Availability of an existing subscription login remains **unverified**.

## Recommendation

Use **Claude ACP**, pinned to `@agentclientprotocol/claude-agent-acp@0.79.0`, with `@agentclientprotocol/sdk@1.4.0`. Its underlying Claude Agent SDK is pinned to `0.3.274`. It exposes SDK budget/turn limits and can remove built-in tools while retaining the experiment's MCP tools.

After approval to install dependencies (not performed by this investigation):

```sh
npm install --save-exact @agentclientprotocol/claude-agent-acp@0.79.0
```

Launch the adapter, not `claude -p` (that speaks a different protocol):

```js
const child = spawn(process.execPath, [
  '<historical-selection-dir>/node_modules/@agentclientprotocol/claude-agent-acp/dist/index.js'
], {
  cwd: privateEmptyScratchDirectory,
  env: filteredEnvironment,
  stdio: ['pipe', 'pipe', 'pipe'],
});
```

Use the adapter's bundled SDK CLI for compatibility. Do not set `CLAUDE_CODE_EXECUTABLE` to the older installed CLI without a separate compatibility check. Do not forward Claude CLI flags to the ACP executable; options go in `session/new` metadata.

Build `filteredEnvironment` from a small reviewed allowlist such as `HOME`, `PATH`, `TMPDIR`, and locale variables. Preserve access to the existing subscription harness; do not copy or inspect credentials. Exclude API-key variables, auth-token overrides, provider/base-URL overrides, custom gateway settings, `NODE_OPTIONS`, and debug logging overrides. In particular, do not inherit `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `OPENAI_API_KEY`, `CODEX_API_KEY`, or third-party-provider selection flags. This does **not** prove subscription billing: managed settings or an existing console login can select another source. Stop if the harness reports API billing, requires authentication, or cannot establish the intended existing subscription path. Never call `authenticate` or perform login automatically.

## ACP wire interface

ACP uses newline-delimited UTF-8 JSON-RPC 2.0 over stdin/stdout, not LSP `Content-Length` framing. Keep diagnostic output on stderr. Use piped stdio, not a PTY. Consume stdout continuously because updates and permission requests can arrive before the prompt response.

SDK 1.4.0 integration follows the official adapter example:

```js
import { Readable, Writable } from 'node:stream';
import { client, methods, ndJsonStream, PROTOCOL_VERSION } from '@agentclientprotocol/sdk';

const connection = client({ name: 'baleyg-selection-smoke' })
  .onNotification(methods.client.session.update, ({ params }) => {
    // Record bounded, redacted agent_message_chunk/tool_call/tool_call_update data.
  })
  .onRequest(methods.client.session.requestPermission, () => ({
    outcome: { outcome: 'cancelled' },
  }))
  .connect(ndJsonStream(
    Writable.toWeb(child.stdin), Readable.toWeb(child.stdout),
  ));
const agent = connection.agent;
```

1. Send `initialize`, using SDK `PROTOCOL_VERSION` (currently wire version 1):

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientInfo":{"name":"baleyg-selection-smoke","version":"0.1.0"},"clientCapabilities":{"fs":{"readTextFile":false,"writeTextFile":false},"terminal":false}}}
```

Check negotiated protocol and capabilities. Do not advertise client filesystem or terminal access. Do not enable gateway auth, subagents, goals, or background tasks.

2. Send `session/new`. Replace absolute paths and MCP tool names with the experiment's actual names. ACP stdio MCP entries have **no `type` field**; `env` is an array of name/value pairs, not a mapping.

```json
{
  "jsonrpc": "2.0", "id": 2, "method": "session/new",
  "params": {
    "cwd": "/absolute/private/empty/scratch",
    "mcpServers": [{
      "name": "selection",
      "command": "/absolute/path/to/node",
      "args": ["/absolute/path/to/selection-mcp.mjs"],
      "env": []
    }],
    "_meta": {"claudeCode": {"options": {
      "tools": [],
      "allowedTools": ["mcp__selection__EXACT_TOOL_NAME"],
      "settingSources": [],
      "allowDangerouslySkipPermissions": false,
      "maxBudgetUsd": 0.5,
      "maxTurns": 3,
      "effort": "low",
      "persistSession": false,
      "enableFileCheckpointing": false
    }}}
  }
}
```

`tools: []` removes built-in tools, not supplied MCP tools. List only the exact harmless experiment tools in `allowedTools` so they can run without permission prompts. All requests that still reach `session/request_permission` are denied. Do not use an unrestricted `mcp__selection` wildcard if that server exposes other tools. Metadata `permissionMode` and `canUseTool` are deliberately overridden by the adapter; setting these fields there does not replace its permission handler. Do not select bypass mode.

The adapter starts the MCP server and handles its MCP initialize/list/call lifecycle. The selection client itself speaks ACP, not MCP, to this subprocess. MCP stdout is reserved for MCP protocol; server diagnostics go to stderr. An empty MCP env list does not by itself prove that the server has no inherited environment.

The `session/new` result supplies `sessionId`, plus available model/mode/config information. Select a model supported by that result if needed; do not guess a newly introduced model ID. `options.model` can pin a known supported model. No model selection was tested here.

3. Send one fixed text prompt, with no slash commands:

```json
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"RETURNED_ID","prompt":[{"type":"text","text":"FIXED EXPERIMENT PROMPT"}]}}
```

The adapter streams `session/update` notifications. Assemble `agent_message_chunk` text and record tool-call results. Wait for the response to request 3 with its `stopReason`. Do not mistake the first chunk for completion. Reject unexpected tool names. Use the experiment MCP server to validate all tool arguments and limit result size.

On timeout send `session/cancel` as a notification with `{sessionId}`, then allow only a short grace period before terminating the owned process tree. Killing only the npm launcher can leave descendants. Closing ACP stdin triggers adapter cleanup, but process-tree cleanup still needs verification.

## Budget and usage controls

The user permits **$10 total**, not $10 per session.

- Set `maxBudgetUsd: 0.5` and `maxTurns: 3` per fresh session for the initial smoke. Start with one session. A larger experiment must enforce a fixed global session count (for example, at most 10 sessions gives a $5 **estimated threshold allocation**, not a billing guarantee).
- Add a wall-clock timeout, input/output size bounds, MCP call-count limit, and no automatic retries. Do not start more sessions when usage is unknown.
- Keep a global ledger of reported estimated usage; do not reset the allowance after failures. Disable autonomous goals, subagents, fallback runs, and `/clear` or session restarts that silently reset the budget.
- `maxBudgetUsd` is a **client-side estimated-cost stop threshold**, compared with `total_cost_usd`. It is not an authoritative invoice cap. Official docs warn that local price tables and billing rules can diverge. A completed model request can reach/exceed a threshold before the next stop check. `/clear` resets that running budget.
- Subscription included usage and metered extra usage differ. Removing API-key env vars does not disable account-level extra usage. Existing subscription authentication was not verified and account settings were not changed.
- A strict guarantee of no charge above $10 requires an authoritative provider/account billing cap or confirmed included-only subscription operation with paid overage disabled. Neither was established here. If a hard invoice guarantee is required, **do not run inference until the user confirms that account control**. Do not describe the SDK threshold as that guarantee.

## Permission limits are not an OS sandbox

An empty scratch cwd, no built-in tools, false ACP fs/terminal capabilities, and deny-all permission responses reduce exposed actions. They do not contain the adapter process. The adapter/SDK can still access its auth state, run startup code, make network requests, and write operational files. Managed policy can still apply when `settingSources: []`; the adapter also resolves settings during startup. `persistSession: false` is not a promise of zero filesystem writes. MCP server processes are independently executable code and are not made safe by ACP permissions.

Use only the reviewed experiment MCP server. Keep the real repository and secrets out of model context. For OS-level containment, use a separately verified sandbox with read-only mounts and limited network access, while handling existing authentication explicitly. This investigation did not establish such a sandbox.

## Versions and alternative

| Component | Observed version | Notes |
| --- | --- | --- |
| Installed Claude Code | 2.1.266 | `<user-home>/.local/bin/claude` |
| Installed Codex CLI | 0.150.1 | `<user-home>/.local/bin/codex` |
| Node path | Node 24.11.1 installation | `<user-home>/.asdf/installs/nodejs/24.11.1/bin/node` |
| ACP TypeScript SDK | 1.4.0 | Current npm; already pinned in experiment |
| Claude ACP | 0.79.0 | Current `@agentclientprotocol/claude-agent-acp`; SDK 0.3.274 |
| Codex ACP | 1.12.0 | Current `@agentclientprotocol/codex-acp`; bundles `@openai/codex` `^0.154.0` |
| Legacy Claude ACP | 0.23.1 | `@zed-industries/claude-agent-acp`; prefer current namespace |
| Legacy Codex ACP | 0.16.0 | `@zed-industries/codex-acp`; repository explicitly directs new installs to current namespace |

Codex ACP supports existing ChatGPT authentication, stdio MCP servers, model/effort configuration, streaming/tool/usage events, and `CODEX_CONFIG`. Its exact launch is the installed package's `dist/index.js` via Node (or `npx -y @agentclientprotocol/codex-acp@1.12.0`). Leave `CODEX_PATH` unset to use its compatible bundled CLI. No reliable hard USD budget flag was identified in its public interface.

**Important Codex trap:** in pinned v1.12.0 `src/AgentMode.ts`, mode ID `read-only` is labelled “Ask for approval” and actually uses `sandboxMode: "workspace-write"` with a `workspaceWrite` sandbox policy. `INITIAL_AGENT_MODE=read-only` is **not** true read-only containment. The installed native Codex CLI offers `--sandbox read-only --ask-for-approval never`, but those CLI options are not automatically adapter options, and adapter session policy can override lower-level config. Claude's tool removal and estimated budget controls are a better fit for this small experiment.

## Primary sources

- [Claude ACP pinned source: session metadata and SDK option forwarding](https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/acp-agent.ts)
- [Claude ACP pinned entry point](https://github.com/agentclientprotocol/claude-agent-acp/blob/v0.79.0/src/index.ts)
- [Claude Agent SDK TypeScript reference](https://platform.claude.com/docs/en/agent-sdk/typescript)
- [Official cost accounting and budget caveats](https://platform.claude.com/docs/en/agent-sdk/cost-tracking)
- [ACP initialize](https://agentclientprotocol.com/protocol/initialization), [session setup and MCP](https://agentclientprotocol.com/protocol/session-setup), [prompt turn](https://agentclientprotocol.com/protocol/prompt-turn)
- [Current Codex adapter README](https://github.com/agentclientprotocol/codex-acp)
- [Codex v1.12.0 mode implementation](https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/src/AgentMode.ts)
- [SDK connection example](https://github.com/agentclientprotocol/codex-acp/blob/v1.12.0/examples/simple-client.ts) — do **not** copy its auto-approve permission handler.
- Public npm metadata queried with `npm view PACKAGE version bin dependencies --json`; package versions can change after this investigation.
