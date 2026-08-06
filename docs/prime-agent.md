# Prime Agent integration

Taskfleet stays independent from Prime Agent. The optional package at
`integrations/prime-agent` contains two thin adapters for the standalone Rust
binary:

- a Python skill that starts `taskfleet mcp` and exposes its live stdio tools to
  the orchestrating model;
- a native `pi.extensions` TypeScript extension that renders a Kanban widget and
  forwards `/fleet` controls to the Taskfleet CLI.

Neither adapter opens SQLite, schedules work, or reimplements lifecycle rules.

## Install

Install `taskfleet` on `PATH`, then install the package from a source checkout:

```bash
prime-agent package install /absolute/path/to/taskfleet/integrations/prime-agent
```

Release archives contain the same package in `prime-agent/`. Use `/reload` after
installing into an active session, or start a new session. Prime should run from
the project containing `taskfleet.toml`. The TypeScript extension resolves configuration in this order: Prime flags
`--taskfleet-config`/`--taskfleet-bin`, then `TASKFLEET_CONFIG`/`TASKFLEET_BIN`,
then the nearest `taskfleet.toml` and `taskfleet` on `PATH`. The Python skill
runs in a separate process and reads the environment (falling back to
`taskfleet.toml` in its working directory), so use the environment variables
when both adapters must share an explicit non-default path.

## Model-facing MCP skill

The skill is auto-imported as `taskfleet`. Discover the live server contract
instead of assuming tool arguments:

```python
tools = await taskfleet.list_tools()
[name["name"] for name in tools]
await taskfleet.taskfleet_view_list()
await taskfleet.taskfleet_task_query(ready=True)
```

A model-driven orchestrator uses Prime RLM and `agent_message` to create and
steer workers. Taskfleet MCP claims dependency-ready work, prepares worktrees,
heartbeats leases, runs gates, advances steps, and integrates candidates.
Taskfleet deliberately does not spawn models.

## Operator commands and Kanban

```text
/fleet status [task]
/fleet board [on|off|refresh] [view]
/fleet pause <task>
/fleet resume <task>
/fleet cancel <task> [reason]
/fleet retry <task>
/fleet reprioritize <task> <integer>
```

`/fleet priority` is a short alias for `/fleet reprioritize`.

The status line and widget refresh on session start and agent completion. The
board is an observational projection of Rust responses: backlog, ready, running,
paused, blocked, candidate, cancelled, and done, including owner, lease,
operational priority, dependencies, gates, and the last error. Closing a session
clears timers and UI state.

Pause revokes a running lease but preserves progress and Git artifacts. Resume
makes backlog work claimable again; it does not spawn a replacement worker.
Cancellation is terminal and also retains Git artifacts for audit. Retry and
priority call their dedicated Rust methods; controls are not aliases for another
transition.

## Associated Prime child cancellation

To bind a claimed task to a Prime RLM child, use the exact stable child name
returned by `await rlm(..., name=...)` as the Taskfleet claim `owner`, with one
active task per owner. Shared owners are not cancelled. Before a pause or cancellation, the extension reads that owner, applies the authoritative
Taskfleet transition, then attempts targeted child cancellation.

Prime Agent 0.7 does not expose child control through the generic extension
context. In a resident daemon session the adapter attaches through Prime's public
`DaemonClient`/`DaemonAgentConnection`, resolves the name only in the current
parent session's child snapshot, and calls `cancelRlmChild(id)`. It never uses
`ctx.abort()` or `prime-agent stop`, which have broader or different semantics.
In in-process or client-owned sessions, or if the name is absent/ambiguous, the
Taskfleet transition remains committed and the UI explicitly reports that child
cancellation could not be confirmed.

## Recovery

After an interruption or restart:

```python
await taskfleet.taskfleet_reconcile()
await taskfleet.taskfleet_task_query(full=True)
```

The database, branches, receipts, controls, priorities, claims, and worktree
paths are durable. A dead lease returns to backlog unless held or cancelled;
reclaim it explicitly and prepare its deterministic worktree again.
