import test from "node:test";
import assert from "node:assert/strict";
import { dispatchFleet, fleetCompletions, type FleetCommandRuntime } from "../src/commands.ts";
import type { TaskfleetClient } from "../src/client.ts";
import type { FleetLocation, FleetSnapshot } from "../src/types.ts";

function fixture() {
  const calls: Array<[string, unknown]> = [];
  const notices: Array<[string, string | undefined]> = [];
  const cancelled: Array<[string, string]> = [];
  const externalCalls: string[] = [];
  let reactivated = 0;
  const location: FleetLocation = { mode: "local", repository: "/repo", config: "/repo/taskfleet.toml", state: null, enabled: true };
  const snapshot: FleetSnapshot = { at: 1, cards: [{ uri: "task://one", title: "One", state: "running", ready: false, depends_on: [], gates: [] }] };
  const client = { call: async (method: string, input: unknown) => {
    calls.push([method, input]);
    if (method === "task.pause" || method === "task.cancel") return { owner: "tf-one" };
    if (method === "task.get") return { task: { uri: "task://one", title: "One", depends_on: [] }, execution: { state: "running", owner: "tf-one", gates: [] } };
    if (method === "task.query") return [];
    return {};
  } } as unknown as TaskfleetClient;
  const runtime: FleetCommandRuntime = {
    client,
    location,
    interactive: true,
    external: async (action) => { externalCalls.push(action); return { mode: "external", repository: "/repo", config: "/state/taskfleet.toml", state: "/state", enabled: action === "enable", purged: action === "purge" }; },
    reactivate: async () => { reactivated += 1; },
    childController: { cancel: async (session, owner) => { cancelled.push([session, owner]); return { found: true, cancelled: true, childId: "child" }; } },
    sessionId: "parent",
    snapshot: async () => snapshot,
    refresh: async () => {},
    setBoard: async () => {},
    notify: (message, level) => notices.push([message, level]),
    confirm: async () => true,
  };
  return { runtime, calls, notices, cancelled, snapshot, externalCalls, reactivated: () => reactivated };
}

test("pause and cancel forward native controls and target only the associated child", async () => {
  const { runtime, calls, cancelled } = fixture();
  await dispatchFleet("pause task://one", runtime);
  await dispatchFleet("cancel task://two obsolete", runtime);
  assert.deepEqual(calls, [
    ["task.get", { task: "task://one" }],
    ["task.pause", { task: "task://one" }],
    ["task.query", { limit: 500 }],
    ["task.get", { task: "task://two" }],
    ["task.cancel", { task: "task://two", reason: "obsolete" }],
    ["task.query", { limit: 500 }],
  ]);
  assert.deepEqual(cancelled, [["parent", "tf-one"], ["parent", "tf-one"]]);
});


test("shared Taskfleet owners are not treated as an exact child binding", async () => {
  const { runtime, cancelled, notices } = fixture();
  const call = runtime.client!.call.bind(runtime.client);
  runtime.client = { call: async (method: string, input: unknown) =>
    method === "task.query" ? [{ uri: "task://other", title: "Other", state: "running", ready: false, owner: "tf-one" }] : call(method, input)
  } as unknown as TaskfleetClient;
  await dispatchFleet("pause task://one", runtime);
  assert.equal(cancelled.length, 0);
  assert.match(notices.at(-2)?.[0] ?? "", /still owns another/);
});

test("resume, retry, and priority use explicit core methods", async () => {
  const { runtime, calls } = fixture();
  await dispatchFleet("resume task://one", runtime);
  await dispatchFleet("retry task://two", runtime);
  await dispatchFleet("reprioritize task://one 7", runtime);
  assert.deepEqual(calls, [
    ["task.resume", { task: "task://one" }],
    ["task.retry", { task: "task://two" }],
    ["task.reprioritize", { task: "task://one", priority: 7 }],
  ]);
});

test("cancel confirmation and completion are fail closed", async () => {
  const { runtime, calls, notices, snapshot } = fixture();
  runtime.confirm = async () => false;
  await dispatchFleet("cancel task://one", runtime);
  assert.equal(calls.length, 0);
  await dispatchFleet("priority task://one nope", runtime);
  assert.equal(calls.length, 0);
  assert.equal(notices.at(-1)?.[1], "error");
  assert.ok(fleetCompletions("pause task://", snapshot)?.some((item) => item.value.includes("task://one")));
});

test("external lifecycle works without an operational client and purge fails closed", async () => {
  const { runtime, externalCalls, notices, reactivated } = fixture();
  runtime.client = undefined;
  runtime.location = { mode: "external", repository: "/repo", config: "/state/taskfleet.toml", state: "/state", enabled: false };
  await dispatchFleet("enable external", runtime);
  await dispatchFleet("disable", runtime);
  assert.deepEqual(externalCalls, ["enable", "disable"]);
  assert.equal(reactivated(), 2);

  runtime.interactive = false;
  await dispatchFleet("purge", runtime);
  assert.deepEqual(externalCalls, ["enable", "disable"]);
  assert.match(notices.at(-1)?.[0] ?? "", /interactive confirmation/);
  runtime.interactive = true;
  runtime.confirm = async () => false;
  await dispatchFleet("purge", runtime);
  assert.deepEqual(externalCalls, ["enable", "disable"]);
  runtime.confirm = async () => true;
  await dispatchFleet("purge", runtime);
  assert.deepEqual(externalCalls, ["enable", "disable", "purge"]);
});

test("external enable refuses to shadow a local config", async () => {
  const { runtime, externalCalls, notices } = fixture();
  await dispatchFleet("enable external", runtime);
  assert.equal(externalCalls.length, 0);
  assert.match(notices.at(-1)?.[0] ?? "", /local taskfleet.toml/);
  assert.ok(fleetCompletions("en", undefined)?.some((item) => item.value === "enable"));
});
