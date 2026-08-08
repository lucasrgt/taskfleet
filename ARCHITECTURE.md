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

Linked objectives share an arbitrary dossier path value, conventionally
`meta.bundle`, with free-form roles in `meta.role`. `task.related` lists every
task that shares that value so agents can assemble a multi-objective package
without domain-specific core types. Dependencies still use `depends_on`; the
shared path is only for discovery and projection.

## Execution state machine

```text
backlog --claim--> running --step.advance--> running
   ^                  |                         |
   |                  +--failure------------> blocked
   |                                             |
   +---------------------task.retry--------------+

last step --step.advance--> candidate --integration.run--> done
                                      \--conflict/gate----> blocked

any non-done state --task.cancel--> cancelled
```

A claim is an atomic lease. A durable `paused` control flag is orthogonal to the
lifecycle state: paused tasks cannot be claimed, heartbeated, advanced, or
integrated. Pausing running work returns its lifecycle state to `backlog`,
revokes its lease, and preserves its step, branch, and worktree; resuming makes
it claimable again. Pausing backlog, blocked, or candidate work preserves that
state, so blocked work still needs `task.retry` and a resumed candidate remains
a candidate. Cancellation is terminal and also revokes ownership, but preserves
Git artifacts for inspection.

A task is ready only when it is unpaused `backlog` work and every dependency is
`candidate` or `done`; a cancelled dependency does not satisfy the edge. Ready
tasks are claimed by descending operational `queue_priority`, then URI. This
integer is stored independently of the provider's imported `priority` field.
Routes choose a workflow from task data; an explicit task workflow wins, then
the first matching route, then the project default. With no workflow, Taskfleet
uses one implicit `execute` step with no gates. Workflows are domain-agnostic
pipelines: each step may carry a free-form `instruction` and default `args`.
Resolved step args merge shallowly as workflow defaults, then step defaults,
then the run input in `meta.args`. `task.spawn` creates a backlog run from a
workflow id; `task.rerun` spawns a new uri (never resets the source). Workflow
`max_runs` defaults to 1; `0` means unlimited. Optional `meta.series` scopes
rerun counting within a pipeline.

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

`integration.run` creates a dedicated integration branch and worktree, ignores
paused and cancelled candidates, and merges candidates one at a time. Dependency
order takes precedence; otherwise descending `queue_priority` and URI decide the
order. After each staged merge, all matching `integration.merge` command gates
run against the combined tree. A conflict or failed required gate aborts only
that merge and blocks that task; already integrated tasks remain committed and
marked `done`. The returned branch is the artifact a harness may review, push,
or merge. Integration is a single-controller operation: cancellation cannot
undo a Git commit that has already won the race with its database transition.
Unless `retain_worktree` / `retain_integration_worktree` is true, the
integration worktree is deleted after consolidation; the branch remains.

## Persistence and recovery

Configuration discovery is owned by the Rust surface: explicit `--config`, the
nearest ancestor `taskfleet.toml`, then a canonical Git-root external enrollment.
External mode stores its generated config, SQLite database, worktrees, and an
enablement marker under the platform state directory; it creates no repository
files. Disable removes only the marker. Purge is rejected while enabled and
removes only that repository's external state before pruning Git worktree
metadata.

SQLite runs in WAL mode. Task ingestion, dependency replacement, cycle checks,
claims, gate receipts, TaskReceipt metadata, and transitions use transactions.
Gate receipts include task, step, gate, Git tree, verdict, output, and
timestamp. Changing the tree makes a previous gate receipt irrelevant without
mutating history.

`reconcile` expires dead leases back to `backlog`, destroys leftover task
workspaces on unpaused `candidate`/`done`/`backlog` rows, prunes stale Git
worktree metadata, and garbage-collects unreachable CAS content. It never
resumes paused or cancelled tasks. Blocked tasks require an explicit
`task.retry`, keeping failures visible to every agent and harness. Cancelled
worktrees and branches are retained rather than force-deleted because they may
contain uncommitted evidence; their eventual removal is an explicit operator
responsibility.

## Deliberate exclusions

Taskfleet does not embed tracker clients, model providers, agent runtimes,
prompt logic, dashboards, webhooks, or project-specific quality tools. Those
systems use the stable CLI/MCP boundary; repository-specific tools compose as
ordinary command gates.
