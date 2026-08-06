import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { findConfig, TaskfleetClient } from "../src/client.ts";
import type { Exec } from "../src/types.ts";

test("findConfig honors explicit paths and walks ancestors", () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-prime-"));
  const nested = join(root, "a", "b");
  mkdirSync(nested, { recursive: true });
  writeFileSync(join(root, "taskfleet.toml"), "schema=1\n");
  assert.equal(findConfig(nested, undefined, {}), join(root, "taskfleet.toml"));
  assert.equal(findConfig(nested, "custom.toml"), join(nested, "custom.toml"));
});

test("client executes exact no-shell Taskfleet CLI contract", async () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-prime-"));
  const config = join(root, "taskfleet.toml");
  writeFileSync(config, "schema=1\n");
  const calls: unknown[][] = [];
  const exec: Exec = async (command, args, options) => {
    calls.push([command, args, options]);
    return { code: 0, killed: false, stderr: "", stdout: JSON.stringify({ ok: true }) };
  };
  const client = new TaskfleetClient({ cwd: root, config, binary: "/opt/taskfleet", exec });
  assert.deepEqual(await client.call("task.pause", { task: "task://one" }), { ok: true });
  assert.deepEqual(calls[0], [
    "/opt/taskfleet",
    ["task.pause", "--config", config, "--input", '{"task":"task://one"}'],
    { cwd: root, timeout: 10_000, signal: undefined },
  ]);
});

test("client bounds failures and rejects malformed JSON", async () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-prime-"));
  const config = join(root, "taskfleet.toml");
  writeFileSync(config, "schema=1\n");
  let exec: Exec = async () => ({ code: 1, killed: false, stdout: "", stderr: "x".repeat(3_000) });
  let client = new TaskfleetClient({ cwd: root, config, exec });
  await assert.rejects(client.call("task.query"), (error: Error) => error.message.length < 2_100);
  exec = async () => ({ code: 0, killed: false, stdout: "not-json", stderr: "" });
  client = new TaskfleetClient({ cwd: root, config, exec });
  await assert.rejects(client.call("task.query"), /invalid JSON/);
});

test("snapshot enriches only active cards and keeps compact dependencies", async () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-prime-"));
  const config = join(root, "taskfleet.toml");
  writeFileSync(config, "schema=1\n");
  const methods: string[] = [];
  const exec: Exec = async (_command, args) => {
    methods.push(args[0]);
    const output = args[0] === "task.query"
      ? [{ uri: "task://queued", title: "Queued", state: "backlog", ready: false, depends_on: ["task://dep"] }, { uri: "task://live", title: "Live", state: "running", ready: false }]
      : { task: { uri: "task://live", title: "Live", depends_on: [] }, execution: { state: "running", active_step: { id: "build" }, gates: [] } };
    return { code: 0, killed: false, stderr: "", stdout: JSON.stringify(output) };
  };
  const snapshot = await new TaskfleetClient({ cwd: root, config, exec }).snapshot();
  assert.deepEqual(methods, ["task.query", "task.get"]);
  assert.deepEqual(snapshot.cards[0].depends_on, ["task://dep"]);
  assert.equal(snapshot.cards[1].step_name, "build");
});
