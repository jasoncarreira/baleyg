# Feature Factory operations

Baleyg uses GitHub Issues as the durable intake queue for Feature Factory runs.
The tracked [`.factory.json`](../.factory.json) recognizes these exact references:

- `123`
- `#123`
- `https://github.com/jasoncarreira/baleyg/issues/123`

Other text remains a normal free-text feature request. A recognized reference is resolved with
`gh issue view` in `jasoncarreira/baleyg`; its issue number becomes the stable factory run ID and its
title and body become untrusted story input.

## Operator prerequisites

- Prime Agent with `prime-agent-feature-factory` installed.
- Node.js 22 or newer, Git, Rust, and GitHub CLI.
- GitHub CLI credentials for the `jasoncarreira` account.
- An inherited `GH_TOKEN` for `jasoncarreira` when the factory reaches an identity or publication gate.
- `FACTORY_PUBLISHING_IDENTITY=jasoncarreira` in the environment before starting a fresh run.

Before a publishing-capable run, verify the inherited token without printing it:

```sh
export FACTORY_PUBLISHING_IDENTITY=jasoncarreira
gh api --method GET /user --jq .login
```

The command must print `jasoncarreira`. Never commit `GH_TOKEN`, derive the declared publishing identity
from the active credential, or put credentials in `.factory.json`.

The publishing identity is deliberately not stored in `.factory.json`. Feature Factory records the
inherited declaration in the run and compares it with the authenticated GitHub login before external
publication.

## Starting a run

From a clean Baleyg checkout on the desired PR base, invoke Feature Factory with one issue reference:

```text
/feature #123
```

The factory creates a private sandbox under `.factory-sandboxes/`, preserves its control plane under
`.factory/`, decomposes the approved issue into tested slices, and creates a draft PR after all gates
pass. Both directories are ignored by the tracked root `.gitignore`; they must never be committed.

Issue intake reads the issue body once when the run is created. Put requirement corrections in the
body, not comments. Resume is for transient external blockers; a changed requirement needs a deliberate
fresh run so the story and plan are regenerated.

A good factory issue contains:

1. a concrete outcome and user value;
2. explicit scope and exclusions;
3. testable acceptance criteria;
4. dependencies on earlier issues;
5. security, provenance, and compatibility constraints;
6. validation evidence required before the issue is complete.

## Repository commands

The repository configuration has four operational boundaries:

- `resolve` reads a recognized GitHub Issue and emits the canonical factory payload.
- `bootstrap` installs the pinned ACP runtime packages in a fresh sandbox without lifecycle scripts.
- `verify` runs the same Rust, ACP, JavaScript syntax, and browser test commands as CI.
- `publish` declares the future branch-push command. The current factory release still performs its
  guarded Git and GitHub publication flow directly.

Configuration and `.gitignore` are privileged factory paths. Feature runs cannot modify them. Change
either only through a separately reviewed repository-maintenance PR.

## GitHub issue policy

The current semantic-index program is tracked in [#8](https://github.com/jasoncarreira/baleyg/issues/8),
with dependency-ordered stage epics [#9–#18](https://github.com/jasoncarreira/baleyg/milestone/1).

The staged semantic-index issues use the `epic`, `semantic-index`, and where applicable `mcp` labels.
Run one stage at a time after its dependencies are closed. Do not ask one run to implement later stages
implicitly. UI work is intentionally excluded from this program; the deliverable is a revision-pinned
semantic evidence index exposed to coding agents through read-only MCP tools.
