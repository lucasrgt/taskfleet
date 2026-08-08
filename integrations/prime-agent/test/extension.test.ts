import test from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import taskfleetExtension from "../src/index.ts";

test("extension registers one fleet command and paints/clears the TUI", async () => {
  const root = mkdtempSync(join(tmpdir(), "taskfleet-extension-"));
  writeFileSync(join(root, "taskfleet.toml"), "schema=1\n");
  const commands = new Map<string, any>();
  const events = new Map<string, any>();
  const flags = new Map<string, unknown>([["taskfleet-board", true], ["taskfleet-refresh-ms", "0"]]);
  const widgets: unknown[] = [];
  const statuses: unknown[] = [];
  const pi: any = {
    registerFlag(name: string, options: any) { if (!flags.has(name)) flags.set(name, options.default); },
    getFlag(name: string) { return flags.get(name); },
    registerCommand(name: string, options: any) { commands.set(name, options); },
    on(name: string, handler: any) { events.set(name, handler); },
    exec: async (_command: string, args: string[]) => {
      const method = args[0];
      const stdout = method === "locate"
        ? JSON.stringify({ mode: "local", repository: root, config: join(root, "taskfleet.toml"), state: null, enabled: true })
        : method === "task.query"
          ? JSON.stringify([{ uri: "task://one", title: "One", state: "backlog", ready: true }])
          : JSON.stringify({ task: { uri: "task://one", title: "One", depends_on: [] }, execution: { state: "backlog", gates: [], paused: false, queue_priority: 0 } });
      return { code: 0, killed: false, stderr: "", stdout };
    },
  };
  taskfleetExtension(pi);
  assert.deepEqual([...commands.keys()], ["fleet"]);
  assert.ok(events.has("session_start") && events.has("session_shutdown"));
  const ctx: any = {
    cwd: root,
    hasUI: true,
    signal: undefined,
    sessionManager: { getSessionId: () => "parent" },
    ui: {
      setWidget: (...args: unknown[]) => widgets.push(args),
      setStatus: (...args: unknown[]) => statuses.push(args),
      notify() {},
      confirm: async () => true,
    },
  };
  await events.get("session_start")({}, ctx);
  assert.ok(widgets.length > 0 && statuses.length > 0);
  events.get("session_shutdown")({}, ctx);
  assert.equal((widgets.at(-1) as unknown[])[1], undefined);
  assert.equal((statuses.at(-1) as unknown[])[1], undefined);
});

test("unconfigured project stays silent and external enable activates without reload", async () => {
  let enabled = false;
  const commands = new Map<string, any>();
  const events = new Map<string, any>();
  const widgets: unknown[][] = [];
  const notices: string[] = [];
  const location = () => ({ mode: "external", repository: "/repo", config: "/state/taskfleet.toml", state: "/state", enabled });
  const pi: any = {
    registerFlag() {}, getFlag() { return undefined; },
    registerCommand(name: string, options: any) { commands.set(name, options); },
    on(name: string, handler: any) { events.set(name, handler); },
    exec: async (_command: string, args: string[]) => {
      if (args[0] === "external") enabled = args[1] === "enable";
      const value = args[0] === "task.query" ? [] : location();
      return { code: 0, killed: false, stderr: "", stdout: JSON.stringify(value) };
    },
  };
  taskfleetExtension(pi);
  const ctx: any = {
    cwd: "/repo", hasUI: true, signal: undefined, sessionManager: { getSessionId: () => "parent" },
    ui: { setWidget: (...args: unknown[]) => widgets.push(args), setStatus() {}, notify: (message: string) => notices.push(message), confirm: async () => true },
  };
  await events.get("session_start")({}, ctx);
  assert.ok(widgets.every((entry) => entry[1] === undefined));
  await commands.get("fleet").handler("enable external", ctx);
  assert.ok(widgets.some((entry) => typeof entry[1] === "function"));
  assert.match(notices.at(-1) ?? "", /No repository files/);
  await commands.get("fleet").handler("disable", ctx);
  assert.equal(widgets.at(-1)?.[1], undefined);
});
