---
name: taskfleet
description: Operate a local Taskfleet task DAG, claims, leases, worktrees, gates, workflow pipelines, linked objectives, integration, and recovery through its packaged MCP stdio server. Use when orchestrating or observing multi-agent repository work with Taskfleet.
---

# Taskfleet

Taskfleet resolves an explicit `TASKFLEET_CONFIG`, the nearest ancestor
`taskfleet.toml`, or an external enrollment created by the human command
`/fleet enable external`. It never auto-enables an unconfigured repository.
Optionally set `TASKFLEET_BIN` when `taskfleet` is not on `PATH`.

Discover the packaged server contract before calling it:

```python
tools = await taskfleet.list_tools()
```

Call MCP tools as async methods, for example:

```python
await taskfleet.taskfleet_task_query(ready=True)
await taskfleet.taskfleet_task_claim(owner="worker-1", limit=1)
```

## Default linked-objectives ritual

After claiming (or before implementing) any task, expand linked objectives.
Taskfleet does not encode domain roles; connectors put a shared value on an
arbitrary dossier path (conventionally `meta.bundle`) and optional free-form
`meta.role` labels.

1. Claim or inspect a task.
2. Call `taskfleet_task_related(task="<uri>")` (optional `path` overrides
   `meta.bundle`).
3. Use the returned set to plan the full objective package before coding.
4. For each ready dependency, call `taskfleet_task_context` / receipt tools.
5. Implement only this claim's role; keep the lease alive with heartbeats.
6. Advance only after committed-tree gates pass. Aggregator tasks that list the
   package in `depends_on` stay unclaimable until those edges are
   `candidate` or `done`.

If `task.related` fails closed because the seed task has no shared path value,
do not invent siblings — ask the operator or re-ingest with a bundle key.

## Pipeline runs

Workflows are domain-agnostic pipelines. After claim or spawn, read
`execution.active_step.instruction` and `execution.active_step.args` (merged as
workflow defaults < step defaults < `meta.args`). Create runs with
`taskfleet_task_spawn(workflow="...", args={...})`. Repeat with
`taskfleet_task_rerun(task="...")` — this always creates a new uri and respects
`max_runs` (`0` = unlimited). Never reset a finished uri in place.

Taskfleet remains the source of execution truth. Use Prime Agent subagents for
implementation, keep claims alive with `taskfleet_task_heartbeat`, and advance
only after committed-tree gates pass. Use `taskfleet_reconcile()` after an
interruption or restart.
