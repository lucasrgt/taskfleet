# Taskfleet Architecture

## Boundaries

Taskfleet owns execution state, not tracker synchronization and not agent
reasoning. An agent or connector reads Fibery, Jira, ClickUp, Monday.com, or
another source and normalizes each item into the `Task` contract. Taskfleet
then owns deduplication, views, dependencies, claims, gates, workspaces,
content-addressed receipts, and integration until a task reaches `done`.

```text
tracker / connector / agent
            |
            | task.ingest
            v
  CLI ---- shared Service ---- MCP
                 |
        +--------+---------+----------+
        |                  |          |
   SQLite/WAL          workspaces    CAS
        |                  |          |
 views, leases,       gates,       TaskReceipt
 state, gate proofs   branches     artifacts
```

The CLI accepts one JSON object from `--input` or standard input. The stdio MCP
server exposes the same methods as tools and delegates to the same `Service`.
There is no transport-specific business logic.

## Task identity and projection

`uri` is the stable identity supplied by the source, for example
`fibery://hostpoint/task/42`. Re-ingesting that URI updates imported fields but
preserves execution state. A task exists once and may match any number of
saved views; views never copy or own tasks.

Filters form a safe recursive AST with `eq`, `ne`, ordered comparisons,
`contains`, `in`, `exists`, `and`, `or`, and `not`. Dotted paths address the
normalized task dossier, including `meta.*`, `source.*`, and `execution.*`.
Unknown or incompatible values do not match.

## Execution state machine

```text
backlog --claim--> running --step.advance--> running
   ^                  |                         |
   |                  +--failure------------> blocked
   |                                             |
   +---------------------task.retry--------------+

last step --step.advance--> candidate --integration.run--> done
                                      \--conflict/gate----> blocked
```

A claim is an atomic lease. Dependencies unblock when blockers reach
`candidate` or `done`, so a downstream wave can start after upstream workflows
finish without waiting for final integration. Routes choose a workflow from
task data; an explicit task workflow wins, then the first matching route, then
the project default. With no workflow, Taskfleet uses one implicit `execute`
step with no gates.

Before an agent advances a step, every applicable required gate referenced by
that step must have a green gate receipt for the current clean Git tree.
Command gates receive a JSON execution context on standard input, run without a
shell, have a timeout, capture bounded output, and fail if they modify tracked
files. Approval gates require an explicit actor and decision. Optional red
gates are recorded but do not block advancement.

## Workspace capsules

`workspace.prepare` (aliased by `worktree.prepare`) creates an isolated
workspace through the configured provider (`git-worktree` by default, `agentfs`
when the CLI is available, or `reflink` to advertise CoW-friendly materialize).
It then merges each ready dependency's candidate branch so downstream agents
start with upstream code. When `max_parallel_workspaces` is set, prepare fails
closed once that many live worktrees exist and reuses `pool-{n}` slots after
destroy. Shared cache paths from `project.shared_caches` are created on prepare
and returned as environment hints; Taskfleet never shares arbitrary mutable
directories between agents. Artifact materialize prefers filesystem CoW
(`cp --reflink` / `cp -c`) when the OS supports it.

Gates remain bound to a clean Git tree. Destroying a workspace never deletes
CAS blobs or TaskReceipt records. `workspace.gc` and `reconcile` remove
unreachable blobs by mark-and-sweep against open tasks, pins, and retention.
`reconcile` also destroys leftover task worktrees on `candidate`, `done`, and
`backlog` rows.

## Task receipts and context

Completed work publishes a `TaskReceipt` into the local CAS. Downstream tasks
call `task.context` to recover declared exports, paths, proofs, and artifacts
without reopening predecessor workspaces. Missing dependency receipts fail
closed.

## Integration

`integration.run` creates a dedicated integration branch and worktree, orders
candidates by their dependencies, and merges them one at a time. After each
staged merge, all matching `integration.merge` command gates run against the
combined tree. A conflict or failed required gate aborts only that merge and
blocks that task; already integrated tasks remain committed and marked `done`.
Unless `retain_worktree` / `retain_integration_worktree` is true, the
integration worktree is deleted after consolidation; the branch remains.

## Persistence and recovery

SQLite runs in WAL mode. Task ingestion, dependency replacement, cycle checks,
claims, gate receipts, TaskReceipt metadata, and transitions use transactions.
Gate receipts include task, step, gate, Git tree, verdict, output, and
timestamp. Changing the tree makes a previous gate receipt irrelevant without
mutating history.

`reconcile` expires dead leases back to `backlog`, prunes stale Git worktree
metadata, and garbage-collects unreachable CAS content. Blocked tasks require
an explicit `task.retry`, keeping failures visible to every agent and harness.

## Deliberate exclusions

Taskfleet does not embed tracker clients, model providers, agent runtimes,
prompt logic, dashboards, webhooks, or project-specific quality tools. Those
systems use the stable CLI/MCP boundary; repository-specific tools compose as
ordinary command gates.
