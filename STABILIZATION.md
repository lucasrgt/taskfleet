# Stabilization record

This record describes the WSL-native stabilization exercise performed against
the post-v0.1.0 code. The canonical contract remains `ARCHITECTURE.md`.

## Environment and verification

- Native WSL stable Rust 1.97.1, Cargo 1.97.1
- `cargo-llvm-cov` 0.8.7 and `tokei` 14.0.0
- No Windows executable or `/mnt/c` build directory
- Initial `cargo xtask verify`: passed at 1,097 production lines and 95.48% line coverage
- Stabilized `cargo xtask verify`: passed at 1,100 production lines and 95.97% line coverage

## Prime Agent workload

Prime Agent 0.7.0 connected to the packaged `taskfleet mcp` binary through the
shipped stdio skill. Six durable dossiers formed a graph with one dependency.
Six separate Prime subagents worked in Taskfleet-prepared Git worktrees.
Every MCP call used a fresh stdio server process, while a separate CLI process
read the same state.

Observed results:

| Evidence | Result |
| --- | --- |
| Dependency ordering | The dependent claim returned no task before its prerequisite was integrated; it became the only ready backlog task afterward. |
| Duplicate prevention | Exact atomic claims assigned each ready dossier once; a concurrent two-Service regression test admits only one winner. |
| Heartbeat and expiry | A 30-second lease was extended, then deliberately abandoned; `reconcile` expired one lease and a replacement owner reclaimed it. |
| Worktree recovery | A registered worktree was forcibly removed, recreated on the same branch, then reused after lease recovery. |
| Tree receipts | A green receipt became pending after a subagent made another commit; advancement failed until the gate was rerun. |
| Required gates | A committed `gate.fail` marker made the required gate red and blocked the task; gate/advance were rejected until explicit retry. |
| Failure isolation | One candidate conflicted with the already integrated tree; four other candidates, including candidates ordered after it, still integrated. |
| Restart durability | Fresh MCP and CLI processes agreed byte-for-byte on the live task dossier, owner, branch, worktree, and gate state. |
| Final artifact | `integration/stabilization-final` is clean and contains prerequisite, recovery, retry, independent, and dependent integration commits. |

The measured wall-clock orchestration window was about 164 seconds from the
first claims to the clean final branch: five accepted tasks (about 1.8 accepted
tasks/minute), one deliberately isolated conflict, one deliberately expired
lease, no duplicate execution, and recovery within about seven seconds of
expiry. This is a functional workload measurement, not a speedup claim; there
is no serial baseline. Token/cost metrics belong to the orchestrator and are not
persisted by Taskfleet.

## Concrete gaps found and disposition

1. Prime Agent 0.7.0 ignores stdio entries in its generic `mcpServers` kernel
   path. The shipped skill supplies only that missing transport; no Prime core or
   extension SDK was added.
2. MCP `structuredContent` is object-only. Taskfleet previously put top-level
   arrays there, which the official Python MCP SDK rejected. MCP results now use
   standards-compatible text content and the thin skill JSON-decodes it.
3. A blocked task could rerun a green gate or advance with its retained owner,
   bypassing explicit retry. Gates and advancement now require `running` state.
4. Restart, corrupt-database diagnostics, simultaneous claims, and a real merge
   conflict followed by a good candidate were under-tested. Regression coverage
   now exercises each case.
5. Prime extensions do not currently receive direct child-session control, but
   the model-facing RLM/message APIs were sufficient for the demonstrated
   orchestration. A Prime core change is not justified unless an extension must
   deterministically own subagents without the orchestrating model.
6. At the time of this exercise, cancellation and Kanban controls were outside
   the normative v0.1 contract. The subsequent core contract added durable
   cancellation, pause/resume, operational queue priority, and opt-in external
   project state; the Prime adapter only forwards those core contracts.
   Tracker-specific code remains absent. Typed
   artifacts, progress history, and attempt ceilings remain excluded.
7. Integration gate process-spawn errors still abort the integration call rather
   than being converted into a per-candidate red verdict. Normal nonzero exits,
   timeouts, required red verdicts, and merge conflicts are isolated. This
   remaining operational-error policy should be resolved before advertising
   arbitrary missing executables as recoverable candidate failures.

Read-only Kanban remains optional future observability. No essential state was
introduced outside SQLite, Git, or gate receipts.
