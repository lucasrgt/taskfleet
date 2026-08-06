---
name: taskfleet
description: Operate a local Taskfleet task DAG, claims, leases, worktrees, gates, workflow steps, integration, and recovery through its packaged MCP stdio server. Use when orchestrating or observing multi-agent repository work with Taskfleet.
---

# Taskfleet

Set `TASKFLEET_CONFIG` to the repository's `taskfleet.toml`. Optionally set
`TASKFLEET_BIN` when `taskfleet` is not on `PATH`.

Discover the packaged server contract before calling it:

```python
tools = await taskfleet.list_tools()
```

Call MCP tools as async methods, for example:

```python
await taskfleet.taskfleet_task_query(ready=True)
await taskfleet.taskfleet_task_claim(owner="worker-1", limit=1)
```

Taskfleet remains the source of execution truth. Use Prime Agent subagents for
implementation, keep claims alive with `taskfleet_task_heartbeat`, and advance
only after committed-tree gates pass. Use `taskfleet_reconcile()` after an
interruption or restart.
