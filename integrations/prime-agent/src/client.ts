import { isAbsolute, resolve } from "node:path";
import type { Exec, FleetCard, FleetLocation, FleetSnapshot, TaskStatus, TaskSummary } from "./types.ts";

export interface ClientOptions {
  cwd: string;
  exec: Exec;
  binary?: string;
  config?: string;
  env?: NodeJS.ProcessEnv;
  timeoutMs?: number;
}

async function executeJson<T>(exec: Exec, binary: string, args: string[], cwd: string, timeout: number, signal?: AbortSignal): Promise<T> {
  const result = await exec(binary, args, { cwd, timeout, signal });
  if (result.code !== 0) {
    const detail = (result.stderr || result.stdout || `exit ${result.code}`).trim().slice(-2_000);
    throw new Error(`taskfleet ${args.slice(0, 2).join(" ")} failed: ${detail}`);
  }
  try { return JSON.parse(result.stdout) as T; }
  catch (error) { throw new Error(`taskfleet ${args[0]} returned invalid JSON: ${String(error)}`); }
}

function validateLocation(value: FleetLocation): FleetLocation {
  if (!value || !["none", "local", "external"].includes(value.mode) || typeof value.repository !== "string" ||
      typeof value.config !== "string" || typeof value.enabled !== "boolean" ||
      !(value.state === null || typeof value.state === "string")) throw new Error("taskfleet returned an invalid location");
  return value;
}

export class TaskfleetHost {
  readonly binary: string;
  readonly explicitConfig?: string;
  private readonly cwd: string;
  private readonly exec: Exec;
  private readonly timeoutMs: number;

  constructor(options: ClientOptions) {
    const env = options.env ?? process.env;
    this.binary = options.binary || env.TASKFLEET_BIN || "taskfleet";
    const selected = options.config || env.TASKFLEET_CONFIG;
    this.explicitConfig = selected ? isAbsolute(selected) ? selected : resolve(options.cwd, selected) : undefined;
    this.cwd = options.cwd;
    this.exec = options.exec;
    this.timeoutMs = options.timeoutMs ?? 10_000;
  }

  async locate(signal?: AbortSignal): Promise<FleetLocation> {
    const args = ["locate"];
    if (this.explicitConfig) args.push("--config", this.explicitConfig);
    return validateLocation(await executeJson(this.exec, this.binary, args, this.cwd, this.timeoutMs, signal));
  }

  async external(action: "enable" | "disable" | "purge", signal?: AbortSignal): Promise<FleetLocation> {
    return validateLocation(await executeJson(this.exec, this.binary, ["external", action], this.cwd, this.timeoutMs, signal));
  }

  client(location: FleetLocation): TaskfleetClient {
    if (!location.enabled) throw new Error("Taskfleet is not enabled; use /fleet enable external");
    return new TaskfleetClient(this.cwd, this.exec, this.binary, this.timeoutMs, location);
  }
}

export class TaskfleetClient {
  readonly config: string;
  readonly location: FleetLocation;

  constructor(private readonly cwd: string, private readonly exec: Exec, private readonly binary: string,
              private readonly timeoutMs: number, location: FleetLocation) {
    this.location = location;
    this.config = location.config;
  }

  async call<T>(method: string, input: unknown = {}, signal?: AbortSignal): Promise<T> {
    return executeJson(this.exec, this.binary, [method, "--config", this.config, "--input", JSON.stringify(input)], this.cwd, this.timeoutMs, signal);
  }

  async snapshot(view?: string, signal?: AbortSignal): Promise<FleetSnapshot> {
    const input: Record<string, unknown> = { limit: 500 };
    if (view) input.view = view;
    const summaries = await this.call<TaskSummary[]>("task.query", input, signal);
    const cards: FleetCard[] = [];
    for (let index = 0; index < summaries.length; index += 8) {
      const batch = summaries.slice(index, index + 8);
      const active = batch.filter((item) => item.paused || ["running", "blocked", "candidate"].includes(item.state));
      const statuses = new Map((await Promise.all(active.map(async (item) =>
        [item.uri, await this.call<TaskStatus>("task.get", { task: item.uri }, signal)] as const
      ))));
      for (const summary of batch) {
        const status = statuses.get(summary.uri);
        cards.push({ ...summary,
          paused: status?.execution.paused ?? summary.paused ?? false,
          queue_priority: status?.execution.queue_priority ?? summary.queue_priority ?? 0,
          depends_on: status?.task.depends_on ?? summary.depends_on ?? [],
          step_name: status?.execution.active_step?.id,
          gates: status?.execution.gates ?? [],
        });
      }
    }
    return { at: Date.now(), view, cards, location: this.location };
  }
}
