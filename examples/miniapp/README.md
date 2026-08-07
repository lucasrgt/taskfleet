# Miniapp — production-shaped smoke fixture

Tiny product repository used by Taskfleet smoke tests. It is not part of the
Taskfleet runtime; it exists so CI can exercise a realistic harness loop:

1. ingest product tasks with dependencies;
2. claim and prepare workspaces;
3. implement behind a real command gate (`scripts/verify`);
4. publish TaskReceipts and recover them with `task.context`;
5. integrate candidates.

## Layout

```text
app/manifest.json   product identity and required features
app/features/       one file per shipped feature surface
scripts/verify.rs   gate program (compile with rustc)
taskfleet.toml      example project policy
```

## Local compile of the gate

```bash
rustc scripts/verify.rs -O -o scripts/verify
```

On Windows the smoke test writes `scripts/verify.exe`.
