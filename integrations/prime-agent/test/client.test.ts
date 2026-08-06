import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { TaskfleetHost } from "../src/client.ts";
import type { Exec, FleetLocation } from "../src/types.ts";

const location: FleetLocation = { mode: "external", repository: "/repo", config: "/state/taskfleet.toml", state: "/state", enabled: true };

test("host delegates discovery and management to exact no-shell core commands", async () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-prime-"));
  const calls: unknown[][] = [];
  const exec: Exec = async (command, args, options) => {
    calls.push([command, args, options]);
    return { code: 0, killed: false, stderr: "", stdout: JSON.stringify(location) };
  };
  const host = new TaskfleetHost({ cwd: root, config: "custom.toml", binary: "/opt/taskfleet", exec });
  assert.deepEqual(await host.locate(), location);
  await host.external("enable");
  assert.deepEqual(calls, [
    ["/opt/taskfleet", ["locate", "--config", join(root, "custom.toml")], { cwd: root, timeout: 10_000, signal: undefined }],
    ["/opt/taskfleet", ["external", "enable"], { cwd: root, timeout: 10_000, signal: undefined }],
  ]);
});

test("located config is forwarded exactly to operational calls", async () => {
  const calls: unknown[][] = [];
  const exec: Exec = async (command, args, options) => {
    calls.push([command, args, options]);
    return { code: 0, killed: false, stderr: "", stdout: JSON.stringify({ ok: true }) };
  };
  const client = new TaskfleetHost({ cwd: "/repo", exec, binary: "/opt/taskfleet" }).client(location);
  assert.deepEqual(await client.call("task.pause", { task: "task://one" }), { ok: true });
  assert.deepEqual(calls[0], ["/opt/taskfleet", ["task.pause", "--config", location.config, "--input", '{"task":"task://one"}'], { cwd: "/repo", timeout: 10_000, signal: undefined }]);
});

test("host bounds failures and rejects malformed locations", async () => {
  let exec: Exec = async () => ({ code: 1, killed: false, stdout: "", stderr: "x".repeat(3_000) });
  let host = new TaskfleetHost({ cwd: "/repo", exec });
  await assert.rejects(host.locate(), (error: Error) => error.message.length < 2_100);
  exec = async () => ({ code: 0, killed: false, stdout: "not-json", stderr: "" });
  host = new TaskfleetHost({ cwd: "/repo", exec });
  await assert.rejects(host.locate(), /invalid JSON/);
  exec = async () => ({ code: 0, killed: false, stdout: JSON.stringify({ enabled: true }), stderr: "" });
  host = new TaskfleetHost({ cwd: "/repo", exec });
  await assert.rejects(host.locate(), /invalid location/);
});

test("snapshot enriches only active cards and keeps compact dependencies", async () => {
  const methods: string[] = [];
  const exec: Exec = async (_command, args) => {
    methods.push(args[0]);
    const output = args[0] === "task.query"
      ? [{ uri: "task://queued", title: "Queued", state: "backlog", ready: false, depends_on: ["task://dep"] }, { uri: "task://live", title: "Live", state: "running", ready: false }]
      : { task: { uri: "task://live", title: "Live", depends_on: [] }, execution: { state: "running", active_step: { id: "build" }, gates: [] } };
    return { code: 0, killed: false, stderr: "", stdout: JSON.stringify(output) };
  };
  const snapshot = await new TaskfleetHost({ cwd: "/repo", exec }).client(location).snapshot();
  assert.deepEqual(methods, ["task.query", "task.get"]);
  assert.deepEqual(snapshot.cards[0].depends_on, ["task://dep"]);
  assert.equal(snapshot.cards[1].step_name, "build");
  assert.equal(snapshot.location?.mode, "external");
});
