# Taskfleet Engineering Guide

All repository artifacts must be written in English.

## Product contract

Taskfleet is an agent-first task execution engine for any Git repository and
any tracker or harness. Its public model is deliberately small:

1. Tasks are normalized dossiers with stable provider URIs.
2. Views are saved structured filters over one task store.
3. Workflows are ordered steps; each step may require gates.
4. Agents claim tasks through leases and work in isolated Git worktrees.
5. Candidate branches are integrated sequentially and re-proved by gates.

CLI and MCP are equivalent transports over the same service. Provider-specific
SDKs, agent SDKs, and harness policy do not belong in the core.

## Engineering constitution

1. Production code under `src/` must remain at or below 1,100 code lines as
   measured by `tokei`; no production file may exceed 400 code lines.
2. Shared runtime line coverage must remain at or above 95 percent without
   rounding. The process entrypoint is covered by packaged-binary tests.
3. Test code is unlimited and must live under `tests/`.
4. Production behavior may not be moved into scripts, generated files,
   integrations, or test helpers to evade the line budget.
5. SQLite is the local operational source of truth; external trackers remain
   the source of imported task content.
6. Filters are structured data, never executable query fragments.
7. Claims, leases, dependencies, and state transitions must remain atomic.
8. Required gates fail closed and their receipts are valid only for the exact
   Git tree they proved.
9. CLI and MCP must call the same core operations.

The 1,100-line ceiling covers the three irreducible domains in this product:
task projection, gated workflow execution, and Git integration. Raise it only
after demonstrating that deletion or consolidation cannot absorb a required
behavior.

## Change discipline

Prefer the smallest complete implementation. Add a dependency or abstraction
only when it removes more maintained behavior than it introduces.

Before reporting implementation work complete, run `cargo xtask verify`. This
is the canonical local, CI, and release gate.
