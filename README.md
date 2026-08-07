<h1 align="center">Taskfleet</h1>

<p align="center"><strong>Agent-first task orchestration across trackers, agents, and harnesses.</strong></p>

<p align="center">
  <a href="#quick-install-with-your-agent">Quick Install</a> |
  <a href="#getting-started">Getting Started</a> |
  <a href="#mcp">MCP</a> |
  <a href="ARCHITECTURE.md">Architecture</a>
</p>

<p align="center">
  <a href="https://github.com/lucasrgt/taskfleet/actions/workflows/ci.yml"><img src="https://github.com/lucasrgt/taskfleet/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-2EA44F?style=flat-square" alt="MIT License"></a>
  <img src="https://img.shields.io/badge/runtime-single%20Rust%20binary-B7410E?style=flat-square&logo=rust&logoColor=white" alt="Single Rust binary">
  <img src="https://img.shields.io/badge/storage-local%20SQLite%2FWAL-5B3FD8?style=flat-square" alt="Local SQLite WAL storage">
</p>

Taskfleet turns tasks collected by any agent from Fibery, Jira, ClickUp,
Monday.com, or another source into a safe multi-agent execution queue. One task
store can project many saved views, each task can follow a gated workflow, and
completed branches are consolidated in dependency order through integration
gates.

It is a standalone product. It has no dependency on AeroFortress, NYA, RTW, a
specific tracker, or a specific coding agent. Any of them can compose with it
through JSON, MCP, Git, and ordinary commands. The command is `taskfleet`;
repository policy lives in `taskfleet.toml`, while disposable operational state
lives in `.taskfleet/state.sqlite`.

<table>
<tr><td><b>One store, many boards</b></td><td>Saved structured views project the same tasks without duplication.</td></tr>
<tr><td><b>Safe multi-agent work</b></td><td>Atomic claims, bounded leases, heartbeats, dependencies, and reconciliation coordinate workers.</td></tr>
<tr><td><b>Repository-owned policy</b></td><td>Ordered workflows may require command or approval gates before every step.</td></tr>
<tr><td><b>Isolated implementation</b></td><td>Workspace capsules keep concurrent tasks separated and sync ready dependency branches.</td></tr>
<tr><td><b>Typed handoffs</b></td><td>TaskReceipts in a local CAS let dependents recover declared outputs without old worktrees.</td></tr>
<tr><td><b>Proven consolidation</b></td><td>Candidate branches merge sequentially and gates run again on the combined tree.</td></tr>
<tr><td><b>Tracker and agent independent</b></td><td>Arbitrary source metadata enters through stable JSON contracts exposed equally by CLI and MCP.</td></tr>
</table>

---

## Quick install with your agent

Copy this prompt into any coding agent with terminal access:

```text
Set up Taskfleet in this Git repository.

Download the latest stable binary for this machine from
https://github.com/lucasrgt/taskfleet/releases and verify its published
SHA256SUMS entry. Use no third-party package and do not build from source.

Install `taskfleet` in a user-local PATH location without administrator access
or adding runtime dependencies to the repository. Confirm with
`taskfleet --version`.

At the repository root, run `taskfleet init` and preserve existing content.
Inspect the repository's real task metadata and quality commands before editing
`taskfleet.toml`. Configure only useful saved views, routes, workflow steps, and
per-step gates. Reuse the repository's own gates; do not weaken or replace them.
Keep credentials and tracker-specific SDK configuration outside Taskfleet.

Validate the configuration with:
taskfleet view.list --input '{}'

If an authenticated tracker or connector is already available, normalize its
tasks through `task.ingest` using stable provider URIs. Do not invent, delete,
or mutate remote tracker tasks.

Do not commit, push, or modify unrelated files. Report the installed version,
changed files, configured views and gates, and any action still required.
```

### Manual installation

Download the archive for your operating system and architecture from
[GitHub Releases](https://github.com/lucasrgt/taskfleet/releases), verify it
against `SHA256SUMS`, and place `taskfleet` (or `taskfleet.exe`) in your `PATH`.

```bash
taskfleet --version
```

Build from source with the stable Rust toolchain:

```bash
cargo install --git https://github.com/lucasrgt/taskfleet --locked
```

Taskfleet is one native binary. It needs Git, but no daemon, hosted account,
Node.js runtime, Python runtime, or tracker SDK.

---

## Getting started

Initialize a repository:

```bash
taskfleet init
```

An agent or connector normalizes tasks and ingests them idempotently:

```bash
taskfleet task.ingest --input '{
  "tasks": [{
    "uri": "fibery://hostpoint/tasks/42",
    "title": "Add municipality landing page",
    "description": "Complete tracker description and acceptance context",
    "tags": ["website", "seo"],
    "priority": "high",
    "source": {"provider": "fibery", "space": "Hostpoint"},
    "meta": {"platform": "website", "points": 3},
    "depends_on": []
  }]
}'
```

Claim one ready task from a saved view:

```bash
taskfleet task.claim --input '{"owner":"codex-website-1","view":"website"}'
```

Prepare its isolated workspace, implement and commit the change, run the active
step's gates, then advance. Publish a TaskReceipt so dependents can call
`task.context` without reopening this workspace:

```bash
taskfleet workspace.prepare --input \
  '{"task":"fibery://hostpoint/tasks/42","base":"origin/main"}'

taskfleet gate.run --input \
  '{"task":"fibery://hostpoint/tasks/42","gate":"repository-verify"}'

taskfleet step.advance --input \
  '{"task":"fibery://hostpoint/tasks/42","owner":"codex-website-1"}'
```

Dependencies unblock at `candidate` or `done`. `workspace.prepare` merges each
ready dependency candidate branch into the new workspace. `worktree.prepare`
remains a compatibility alias.
Repeat gates and advancement for each step. After the final step, the task is a
`candidate`. Consolidate all matching candidates:

```bash
taskfleet integration.run --input \
  '{"view":"website","base":"origin/main","branch":"integration/website"}'
```

Every command emits JSON. Input may be supplied through `--input` or standard
input, which keeps it practical for both agents and scripts.

## Configuration

`taskfleet.toml` is repository policy. All relative paths resolve from its
directory.

```toml
schema = 1

[project]
repository = "."
database = ".taskfleet/state.sqlite"
worktree_root = "../.taskfleet-worktrees"
cas_root = ".taskfleet/cas"
workspace_provider = "git-worktree"
cas_retention_seconds = 604800
default_workflow = "delivery"

[[view]]
id = "all"
filter = { op = "true" }

[[view]]
id = "website"
filter = { op = "eq", path = "meta.platform", value = "website" }

[[view]]
id = "urgent-website"
filter = { op = "and", args = [
  { op = "eq", path = "meta.platform", value = "website" },
  { op = "in", path = "priority", values = ["urgent", "high"] }
] }

[[gate]]
id = "repository-verify"
kind = "command"
command = ["cargo", "xtask", "verify"]
events = ["step.complete", "integration.merge"]
timeout_seconds = 1200
required = true

[[gate]]
id = "product-approval"
kind = "approval"
required = true

[[workflow]]
id = "delivery"

[[workflow.step]]
id = "implement"
title = "Implement and prove the change"
gates = ["repository-verify"]

[[workflow.step]]
id = "review"
title = "Review product behavior"
gates = ["repository-verify", "product-approval"]

[[route]]
workflow = "delivery"
when = { op = "true" }
```

The complete five-view Hostpoint-style example is in
[`examples/hostpoint.toml`](examples/hostpoint.toml).

### Filters

Filters are data, not SQL or executable expressions. Available operations are:

| Operation | Fields | Behavior |
| --- | --- | --- |
| `true` | none | Match everything |
| `eq`, `ne`, `gt`, `gte`, `lt`, `lte` | `path`, `value` | Scalar equality or ordered comparison |
| `contains` | `path`, `value` | Array member, string substring, or object key |
| `in` | `path`, `values` | Field equals one listed value |
| `exists` | `path` | Dotted path exists |
| `and`, `or` | `args` | Recursive composition |
| `not` | `arg` | Recursive negation |

Paths address the normalized task, such as `tags`, `meta.platform`,
`source.provider`, or `execution.state`. A command may combine a saved `view`
with one extra `filter`; both must match.

### Workflows and gates

A task with no selected workflow receives one implicit `execute` step, so the
simple case needs no workflow configuration. Selection precedence is explicit
task `workflow`, first matching `route`, project `default_workflow`, then the
implicit step.

Each configured step lists its own gates. A gate may be conditional through
`when`, mandatory or advisory through `required`, and may run at
`step.complete`, `integration.merge`, or both.

Command gates:

- execute the exact argument array without a shell;
- run in the task or integration worktree;
- receive task, step, event, and working directory as JSON on standard input;
- time out, capture output, and fail closed;
- must leave tracked files unchanged;
- produce a receipt bound to the exact Git tree.

Because receipts prove a committed tree, agents commit their implementation
before running a gate. Any later commit invalidates the old proof naturally.

Approval gates use `gate.approve` with `task`, `gate`, `by`, `approved`, and an
optional `note`. They are appropriate where a person must deliberately allow a
step to advance.

Repository tools compose without special integration. A gate command can be
`cargo xtask verify`, `npm test`, `./scripts/quality`, or a wrapper that invokes
NYA, RTW, AVP, security scanners, deployment previews, or internal policy.

## Task dossier

```json
{
  "uri": "jira://acme/WEB-42",
  "title": "Stable human-readable title",
  "description": "Complete task context",
  "tags": ["frontend", "accessibility"],
  "priority": "high",
  "source": {"provider": "jira", "project": "WEB"},
  "meta": {"platform": "website", "estimate": 3, "assignee": null},
  "depends_on": ["jira://acme/API-10"],
  "workflow": "delivery"
}
```

Only `uri` and `title` are mandatory. `source` and `meta` preserve arbitrary
tracker data without forcing Taskfleet to know provider schemas. Re-ingestion
of a URI updates its imported dossier while preserving runtime ownership,
progress, branch, and receipts. Dependency cycles are rejected atomically.

## CLI methods

Run `taskfleet methods` for machine-readable tool metadata.

| Method | Purpose |
| --- | --- |
| `view.list` | List configured saved views |
| `task.ingest` | Insert or update normalized task dossiers |
| `task.query` | Filter by view, extra filter, state, readiness, and limit |
| `task.get` | Read the full dossier, active step, tree, and gate status |
| `task.claim` | Atomically lease ready tasks to one worker |
| `task.heartbeat` | Extend a worker's lease |
| `workspace.prepare` | Create a pooled workspace (cap via `max_parallel_workspaces`) and merge ready dependency branches |
| `worktree.prepare` | Compatibility alias for `workspace.prepare` |
| `workspace.status` | Report workspace cleanliness, branch, and tree |
| `workspace.diff` | Show porcelain status and patch |
| `workspace.destroy` | Destroy a workspace while preserving receipts and CAS |
| `workspace.gc` | Garbage-collect unreachable CAS blobs |
| `gate.run` | Execute and record a command gate |
| `gate.approve` | Record an explicit approval decision |
| `step.advance` | Advance only with current required proofs |
| `task.block` | Stop work with a visible reason |
| `task.retry` | Return a blocked task to the backlog |
| `task.context` | Recover declared dependency receipts without old workspaces |
| `artifact.publish` | Store bytes or a file in the local CAS |
| `artifact.resolve` | Resolve a CAS digest to its storage path |
| `artifact.materialize` | Materialize a CAS blob to a path (CoW when available) |
| `receipt.publish` | Publish a validated TaskReceipt |
| `receipt.get` | Get a TaskReceipt by digest or latest for a task |
| `receipt.resolve_dependencies` | Walk dependency receipts linked from a TaskReceipt |
| `integration.run` | Merge candidates, re-run gates; drop integration worktree unless retained |
| `reconcile` | Expire leases, destroy leftover task worktrees, prune, and GC the CAS |

`task.query` accepts `view`, `filter`, `states`, `ready`, `full`, and `limit`.
`task.claim` accepts the same selection fields plus `owner`, `lease_seconds`,
and a maximum `limit` of 32. Query results default to compact summaries; use
`full: true` for complete dossiers.

## MCP

Start the local stdio server with the same configuration:

```bash
taskfleet mcp --config /path/to/repository/taskfleet.toml
```

Generic MCP host configuration:

```json
{
  "mcpServers": {
    "taskfleet": {
      "command": "taskfleet",
      "args": ["mcp", "--config", "/path/to/repository/taskfleet.toml"]
    }
  }
}
```

The server implements MCP `2025-06-18` over newline-delimited stdio and exposes
the CLI methods as `taskfleet_view_list`, `taskfleet_task_ingest`, and so on.
Tool errors are returned as structured MCP errors without terminating the
server.

## Failure and recovery

- Claims last 15 minutes by default and may be renewed with heartbeats.
- `reconcile` returns expired claims to `backlog` and preserves their branches.
- Required red gates block a task; optional red gates remain visible but allow
  advancement.
- `task.retry` is explicit so failures are never silently hidden.
- A merge conflict or failed integration gate blocks only that candidate.
- All claim, dependency, receipt, and state mutations are transactional.

## Security

`taskfleet.toml` is trusted repository code. Command gates execute the exact
configured process with the current user's permissions and should never be run
from an unreviewed repository. Imported task fields cannot select executables
or inject shell fragments; they are delivered to gates only as JSON on standard
input. Taskfleet does not invoke a shell around command arrays.

The MCP server is local stdio and has no network listener or authentication
layer. Protect the ignored SQLite database if task descriptions or metadata are
sensitive, and configure gate commands according to the repository's own
secret-handling policy. See [`SECURITY.md`](SECURITY.md) for reporting.

## Architecture and scope

Read [`ARCHITECTURE.md`](ARCHITECTURE.md) for the normative state machine,
receipt model, and integration behavior.

| Taskfleet does | Taskfleet does not |
| --- | --- |
| Normalize task execution behind stable JSON contracts | Embed Fibery, Jira, ClickUp, or Monday SDKs |
| Project many boards from one task store | Copy tasks into per-view databases |
| Coordinate agent claims and Git worktrees | Choose or run an AI model |
| Enforce step and integration gates | Define a repository's quality policy |
| Consolidate candidate branches safely | Push or merge the final integration branch |
| Work from any shell or MCP harness | Require a hosted Taskfleet service |

## Build and contribute

```bash
cargo build --release --locked
cargo install cargo-llvm-cov tokei --locked
cargo xtask verify
```

### Miniapp production smoke

[`examples/miniapp`](examples/miniapp) is a tiny product checkout with a real
`scripts/verify` gate. The smoke test materializes it, fails the gate when a
required feature is missing, then runs a multi-agent delivery (auth → health →
ship) with receipts, `task.context`, workspace dependency merges, and
integration:

```bash
cargo test --package taskfleet --test smoke_miniapp -- --nocapture
```

`cargo xtask verify` is the canonical local, CI, and release gate:

| Invariant | Gate |
| --- | --- |
| Maintained production code | At most 500 tokei code lines per production file |
| Shared runtime line coverage | At least 95 percent without rounding |
| Code quality | rustfmt and Clippy with warnings denied |
| Packaged surface | End-to-end CLI binary and MCP protocol tests |
| Miniapp smoke | Production-shaped delivery against `examples/miniapp` |
| Storage | Transactional SQLite with dependency and lease tests |
| Git execution | Real temporary repositories and worktrees |

## License

Taskfleet is available under the [MIT License](LICENSE).
