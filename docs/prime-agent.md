# Prime Agent integration

Taskfleet stays independent from Prime Agent. Prime Agent 0.7.0 does not mount
local stdio MCP servers from `mcpServers`; its generic integration path currently
accepts remote HTTP servers only. Taskfleet therefore ships a small Python-backed
skill that overrides only the MCP transport and starts the packaged
`taskfleet mcp` process. All task, lease, workflow, gate, and Git behavior remains
in the Rust service.

## Install

Install the Taskfleet binary and confirm it is on `PATH`:

```bash
taskfleet --version
```

Release archives include the skill as `prime-agent-skill/taskfleet`. Install it
for the current user:

```bash
mkdir -p ~/.prime/agent/skills
cp -R /path/to/archive/prime-agent-skill/taskfleet ~/.prime/agent/skills/taskfleet
```

From a source checkout, copy `.prime/agent/skills/taskfleet` instead. Start a new
Prime Agent session from the repository that owns `taskfleet.toml`. For a
non-default location or a development binary, set these before starting Prime:

```bash
export TASKFLEET_CONFIG=/absolute/path/to/taskfleet.toml
export TASKFLEET_BIN=/absolute/path/to/taskfleet  # optional when it is on PATH
prime-agent
```

Prime Agent installs the skill's Python MCP dependency into its managed kernel.
After installing during an existing interactive session, use `/reload` or start a
new session.

## Verify and operate

The skill is auto-imported as `taskfleet`. Discover the live server contract
instead of assuming tool arguments:

```python
tools = await taskfleet.list_tools()
[name["name"] for name in tools]
await taskfleet.taskfleet_view_list()
await taskfleet.taskfleet_task_query(ready=True)
```

A model-driven orchestrator uses the existing Prime RLM and `agent_message`
capabilities to create and steer subagents. It uses Taskfleet MCP methods to
claim dependency-ready work, prepare worktrees, heartbeat leases, run gates,
advance steps, and integrate candidates. Taskfleet MCP deliberately does not
spawn or control models.

After an interruption or restart, run:

```python
await taskfleet.taskfleet_reconcile()
await taskfleet.taskfleet_task_query(full=True)
```

The database, branches, receipts, claims, and worktree paths are durable. A dead
lease returns to `backlog`; reclaim it explicitly and call
`taskfleet_worktree_prepare`, which reuses or recreates its deterministic branch.
No Prime Agent core modification or native extension is required for this flow.
