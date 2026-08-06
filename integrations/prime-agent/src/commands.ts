import type { TaskfleetClient } from "./client.ts";
import type { ChildController } from "./controller.ts";
import { snapshotText } from "./board.ts";
import type { FleetLocation, FleetSnapshot, TaskStatus, TaskSummary } from "./types.ts";

export interface FleetCommandRuntime {
  client?: TaskfleetClient;
  location?: FleetLocation;
  interactive: boolean;
  external(action: "enable" | "disable" | "purge"): Promise<FleetLocation>;
  reactivate(): Promise<void>;
  childController: ChildController;
  sessionId: string;
  snapshot(view?: string): Promise<FleetSnapshot>;
  refresh(view?: string): Promise<void>;
  setBoard(visible: boolean, view?: string): Promise<void>;
  notify(message: string, level?: "info" | "warning" | "error"): void;
  confirm(title: string, message: string): Promise<boolean>;
}

const HELP = "Usage: /fleet enable external | disable | purge | status [task] | board [on|off|refresh] [view] | pause <task> | resume <task> | cancel <task> [reason] | retry <task> | reprioritize <task> <integer>";

function requireClient(runtime: FleetCommandRuntime): TaskfleetClient {
  if (!runtime.client) throw new Error("Taskfleet is not enabled; use /fleet enable external");
  return runtime.client;
}

function locationText(location?: FleetLocation): string {
  if (!location || location.mode === "none") return "Taskfleet is not configured";
  return `mode=${location.mode} enabled=${location.enabled}\nrepository=${location.repository}\nconfig=${location.config}${location.state ? `\nstate=${location.state}` : ""}`;
}

function taskText(status: TaskStatus): string {
  const execution = status.execution;
  const gates = (execution.gates ?? []).map((gate) => `${gate.id}:${gate.status}${gate.required ? "!" : ""}`).join(", ") || "none";
  return [
    `${status.task.title} (${status.task.uri})`,
    `state=${execution.state}${execution.paused ? " paused" : ""} owner=${execution.owner ?? "-"} priority=${execution.queue_priority ?? 0}`,
    `step=${execution.active_step?.id ?? execution.step_index ?? "-"} lease=${execution.lease_until ?? "-"}`,
    `depends=${(status.task.depends_on ?? []).join(", ") || "none"}`,
    `gates=${gates}`,
    execution.error ? `error=${execution.error}` : "",
  ].filter(Boolean).join("\n");
}

async function stopAssociatedChild(runtime: FleetCommandRuntime, client: TaskfleetClient, owner: unknown): Promise<void> {
  if (typeof owner !== "string" || !owner) return;
  try {
    const active = await client.call<TaskSummary[]>("task.query", { limit: 500 });
    if (active.some((task) => task.owner === owner)) {
      runtime.notify(`Task controlled, but Prime child ${owner} still owns another Taskfleet task`, "warning");
      return;
    }
    const result = await runtime.childController.cancel(runtime.sessionId, owner);
    if (result.cancelled) runtime.notify(`Cancelled associated Prime child ${owner}`, "info");
    else runtime.notify(`Task controlled, but Prime child was not cancelled: ${result.reason ?? owner}`, "warning");
  } catch (error) {
    runtime.notify(`Task controlled, but Prime child cancellation failed: ${String(error)}`, "warning");
  }
}

export async function dispatchFleet(args: string, runtime: FleetCommandRuntime): Promise<void> {
  const tokens = args.trim().split(/\s+/).filter(Boolean);
  const action = tokens.shift() ?? "status";
  try {
    if (action === "enable") {
      if (tokens.length !== 1 || tokens[0] !== "external") throw new Error("Usage: /fleet enable external");
      if (runtime.location?.mode === "local" && runtime.location.enabled) throw new Error("A local taskfleet.toml is already active");
      const result = await runtime.external("enable");
      await runtime.reactivate();
      runtime.notify(`External mode enabled\nrepository=${result.repository}\nstate=${result.state}\nNo repository files were created.`, "info");
      return;
    }
    if (action === "disable") {
      if (tokens.length) throw new Error("Usage: /fleet disable");
      if (runtime.location?.mode === "local" && runtime.location.enabled) throw new Error("The active Taskfleet configuration is local, not external");
      const result = await runtime.external("disable");
      await runtime.reactivate();
      runtime.notify(`External mode disabled; state retained at ${result.state}. Use /fleet enable external to restore it.`, "info");
      return;
    }
    if (action === "purge") {
      if (tokens.length) throw new Error("Usage: /fleet purge");
      if (!runtime.interactive) throw new Error("External purge requires interactive confirmation");
      const confirmed = await runtime.confirm("Permanently purge external Taskfleet state?", "This deletes the external config, SQLite history, and managed worktrees. Disable external mode first. This cannot be undone.");
      if (!confirmed) return;
      const result = await runtime.external("purge");
      await runtime.reactivate();
      runtime.notify(result.purged ? `Purged external state at ${result.state}` : "No external state existed", "info");
      return;
    }
    if (action === "help") {
      runtime.notify(HELP);
      return;
    }
    if (action === "status") {
      const task = tokens[0];
      if (!runtime.client) runtime.notify(locationText(runtime.location));
      else if (task) runtime.notify(`${locationText(runtime.location)}\n${taskText(await runtime.client.call<TaskStatus>("task.get", { task }))}`);
      else runtime.notify(`${locationText(runtime.location)}\n${snapshotText(await runtime.snapshot())}`);
      return;
    }
    if (action === "board") {
      requireClient(runtime);
      const mode = tokens[0] ?? "on";
      const view = tokens[1];
      if (mode === "off") await runtime.setBoard(false, view);
      else if (mode === "refresh") await runtime.refresh(view);
      else if (mode === "on") await runtime.setBoard(true, view);
      else await runtime.setBoard(true, mode);
      return;
    }
    const task = tokens.shift();
    if (!task) throw new Error(HELP);
    const client = requireClient(runtime);
    if (action === "retry" || action === "resume") {
      const method = action === "retry" ? "task.retry" : "task.resume";
      await client.call(method, { task });
      await runtime.refresh();
      runtime.notify(`${task} ${action === "retry" ? "returned to backlog" : "resumed"}`, "info");
      return;
    }
    if (action === "priority" || action === "reprioritize") {
      const priority = Number(tokens[0]);
      if (!Number.isSafeInteger(priority)) throw new Error("priority must be an integer");
      await client.call("task.reprioritize", { task, priority });
      await runtime.refresh();
      runtime.notify(`${task} queue priority is now ${priority}`, "info");
      return;
    }
    if (action === "pause" || action === "cancel") {
      if (!runtime.interactive) throw new Error(`${action} requires interactive confirmation`);
      const reason = action === "cancel" ? tokens.join(" ") || "cancel requested from Prime Agent" : undefined;
      if (action === "pause" && tokens.length) throw new Error("Usage: /fleet pause <task>");
      if (!(await runtime.confirm(`${action} ${task}?`, action === "cancel" ? "Cancellation is terminal." : "The task must be reclaimed after resume."))) return;
      const before = await client.call<TaskStatus>("task.get", { task });
      await client.call<Record<string, unknown>>(`task.${action}`, reason ? { task, reason } : { task });
      await stopAssociatedChild(runtime, client, before.execution.owner);
      await runtime.refresh();
      runtime.notify(`${task} ${action === "pause" ? "paused" : "cancelled"}`, "info");
      return;
    }
    throw new Error(HELP);
  } catch (error) {
    runtime.notify(error instanceof Error ? error.message : String(error), "error");
  }
}

export function fleetCompletions(prefix: string, snapshot?: FleetSnapshot): Array<{ value: string; label: string; description?: string }> | null {
  const actions = ["enable", "disable", "purge", "status", "board", "pause", "resume", "cancel", "retry", "reprioritize", "priority", "help"];
  const tokens = prefix.split(/\s+/);
  if (tokens.length <= 1) {
    const items = actions.filter((action) => action.startsWith(tokens[0] ?? "")).map((action) => ({ value: action, label: action }));
    return items.length ? items : null;
  }
  const action = tokens[0];
  if (!["status", "pause", "resume", "cancel", "retry", "reprioritize", "priority"].includes(action)) return null;
  const fragment = tokens.at(-1) ?? "";
  const items = (snapshot?.cards ?? []).filter((card) => card.uri.startsWith(fragment)).map((card) => ({ value: `${action} ${card.uri}`, label: card.uri, description: card.title }));
  return items.length ? items : null;
}
