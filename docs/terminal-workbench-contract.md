# Unified terminal and agent workbench — design contract

Status: accepted target UX, **not implemented**. This document does not authorize launching
agents, attaching to existing Herdr panes, installing terminal libraries or running repository
commands. The four-tool [MCP pilot](mcp-readonly-pilot-contract.md) remains a smaller first slice.

## User outcome

Keep source, diagrams and agent terminals in one Baleyg window. Provide docked, resizable terminal
tabs; maximize/restore a terminal without losing the selected method or diagram. Agents can still
run outside Baleyg and use the exact same MCP tools. Herdr and ACP remain optional connection modes.

## Three different surfaces

| Surface | Presentation | Lifecycle owner |
| --- | --- | --- |
| Direct agent launched from Baleyg | Real interactive terminal tab over an owned PTY | Baleyg supervises the launched child; the harness owns its agent loop |
| Existing Herdr-managed agent | Embedded attachment only if a supported terminal transport is validated | Herdr; Baleyg is an explicitly attached client, not a replacement pane manager |
| ACP adapter, primarily Mimir | Structured conversation, tool activity, progress and approval tab | Existing harness/runtime; Baleyg owns only an adapter process it explicitly launched |

ACP protocol stdout is not terminal output. Hands shell results are tool results, not necessarily
a live PTY. Show them honestly. A Mimir CLI can separately run in a real terminal if that is the
chosen interaction mode; do not duplicate a runtime just to make an ACP transcript look terminal-like.

## Native terminal slice

Use a real PTY backend and local terminal renderer (candidate: Rust `portable-pty` and xterm.js).
Choose/pin dependencies during implementation. A terminal ID, project/worktree binding, ownership,
launch configuration and read/input permissions are independent from MCP access and index revision.

- Launch only from explicit user action and a trusted command/profile. No model-supplied automatic
  launch string, source-selection side effect, auto-start on deep link, or provider retry.
- Terminal data stays on a dedicated authenticated, bounded channel, not MCP or ACP JSON stdout.
  Verify Host/Origin; use an authenticated connection or short-lived single-use attach ticket outside
  URLs. Authenticate before exposing output or permitting input. Do not expose the owner's token.
- Bound output history, buffering, frame sizes, input queues and resize rate. Backpressure or a visible
  dropped-history indicator is required; unbounded buffering and per-byte UI messages are not.
- Correctly support interactive programs, Unicode, ANSI state, alternate screen, resize, keyboard
  shortcuts and bracketed paste. Escape sequences/OSC hyperlinks/clipboard operations are untrusted.
  Disable automatic clipboard writes and external resource access; opening links is user-mediated.
- Distinguish read-only attach from input control. Revalidate project/session identity and permissions
  on attach/reconnect. Multiple viewers do not automatically get concurrent keyboard control.
- Closing a tab/disconnecting the browser detaches by default; terminating an owned process requires
  a separate action. Define daemon restart semantics honestly—durable transcript metadata alone does
  not preserve a PTY process. Do not claim kill/revoke always stops detached descendants.
- Show index freshness per checkout. Agent source edits reach the cached index only through
  published revisions; the accepted watcher design refreshes native evidence automatically
  ([local topology](local-topology.md#watcher-leadership-and-publication)), and until it is
  implemented the explicit reindex operation remains the only refresh.

## Optional Herdr attachment

The inspected Herdr metadata API offers workspace/pane/agent discovery, events and snapshot reads.
That observation does **not** establish a web-compatible raw PTY attach/stream interface. Validate
its supported client transport, input/resize authority, multi-client behavior and session lifetime
before promising per-pane embedding. Never approximate a terminal by polling `pane.read` and
forwarding arbitrary keys as if that were a reliable attach protocol.

A possible experiment is an explicit Herdr TUI client in a Baleyg-owned PTY, attached to a user-chosen
existing session. That embeds Herdr as a whole, not an individual pane, and needs usability/security
validation. Detaching or terminating that client must not terminate the Herdr server or its agents.
Until a supported attachment works, provide honest association/focus links rather than a fake terminal.

## ACP and MCP boundaries

Both terminal and ACP agents get the same portable Baleyg MCP tool schemas, served by a stdio
`baleyg mcp` launched in the agent's checkout ([local topology](local-topology.md)). Terminal input
authority is not MCP artifact-write authority, and neither grants arbitrary ACP
filesystem/terminal capabilities. Negotiate implemented capabilities; preserve Mimir's provider
admission, local/remote host distinction, permission gates and one-client constraint. Baleyg may be
the chosen ACP client; it must not connect as a second observer beside an occupied editor session.

## Acceptance before shipping

- A direct coding agent can use MCP, publish a diagram, and remain usable in the same terminal tab.
- Repaint/resize, large output, alternate screen, Unicode and bracketed paste remain correct/bounded.
- Reload/detach does not kill owned sessions unexpectedly; explicit terminate targets only the owner.
- Cross-project/stale attachment, denied input, invalid origin and replayed attach tickets fail closed.
- Malicious OSC/links/text cannot execute JavaScript, write clipboard silently, or leak credentials.
- Herdr is absent/offline without breaking native terminals, MCP, diagrams or ACP.
- An attached Herdr client cannot accidentally terminate or take ownership of the external session.
- ACP and terminal streams remain separate; no JSON protocol bytes are interpreted as terminal input.
- No project/source selection, artifact preview/publication or reconnect silently runs an agent/build.
