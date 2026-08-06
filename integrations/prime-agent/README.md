# Taskfleet for Prime Agent

This optional Prime Agent package contains two thin adapters for the standalone
Taskfleet Rust binary:

- a Python skill that exposes the packaged MCP stdio tools to the orchestrating
  model;
- a TypeScript extension that renders a live Kanban and forwards human controls
  to Taskfleet CLI methods.

Neither adapter opens SQLite or implements Taskfleet scheduling rules.

## Install

Install `taskfleet` on `PATH`, then install this package:

```bash
prime-agent package install /absolute/path/to/taskfleet/integrations/prime-agent
```

For a release archive, point the command at its `prime-agent` directory. Launch
Prime Agent from the repository containing `taskfleet.toml`, or set:

```bash
export TASKFLEET_CONFIG=/absolute/path/to/taskfleet.toml
export TASKFLEET_BIN=/absolute/path/to/taskfleet   # optional
```

The extension also accepts `--taskfleet-config`, `--taskfleet-bin`,
`--taskfleet-view`, `--taskfleet-refresh-ms`, and `--taskfleet-board`. Prefer
the environment for explicit paths that must also reach the Python skill.

Use `/reload` after installing into an active session.


## External mode

A globally installed package performs only read-only discovery. Projects with a
nearest `taskfleet.toml` activate normally; all other projects remain silent
until a person explicitly runs:

```text
/fleet enable external
```

The Rust core finds the enclosing Git repository and creates the configuration,
SQLite database, and managed worktrees under
`$TASKFLEET_STATE_HOME/projects/<repository-id>/`. Without that override it uses
the platform state directory, such as `~/.local/state/taskfleet` on Linux. No
file is created in the repository. The board and status line show `EXTERNAL`.

`/fleet disable` removes only the enrollment marker and preserves all external
state for a later re-enable. `/fleet purge` requires interactive confirmation,
works only after disable, deletes that retained external state, and prunes stale
Git worktree metadata. A local `taskfleet.toml` always shadows external mode.
Configuration precedence is explicit flag/environment, nearest local config,
then an enabled external enrollment.

## Commands

```text
/fleet enable external
/fleet disable
/fleet purge
/fleet status [task]
/fleet board [on|off|refresh] [view]
/fleet pause <task>
/fleet resume <task>
/fleet cancel <task> [reason]
/fleet retry <task>
/fleet reprioritize <task> <integer>
```

`/fleet priority` is a short alias for `/fleet reprioritize`.

The board appears as a TUI widget when a configuration is found. It is
observability only: backlog, ready, running, paused, blocked, candidate, failed,
cancelled, and done columns are projections of Taskfleet state.

For associated child cancellation, claim a task with the exact Prime RLM child
name returned by `await rlm(..., name=...)` as its Taskfleet `owner`, and assign
only one active task to that owner. Shared owners are never cancelled. In a
resident daemon session the extension resolves that name against the current
session's child snapshot and calls Prime's targeted `cancelRlmChild`. Outside a
reachable daemon session the Taskfleet transition still fails closed and the UI
reports that child cancellation could not be confirmed.

Pause revokes ownership but preserves the worktree. Resume makes the task
claimable again; the orchestrator must create or assign a replacement child.
Cancellation is terminal and retains Git artifacts for audit.
