import { dirname, isAbsolute, join, resolve } from "node:path";
import { existsSync } from "node:fs";
import type { Exec, FleetCard, FleetSnapshot, TaskStatus, TaskSummary } from "./types.ts";

export interface ClientOptions {
  cwd: string;
  exec: Exec;
  binary?: string;
  config?: string;
  env?: NodeJS.ProcessEnv;
  timeoutMs?: number;
}

export function findConfig(cwd: string, explicit?: string, env: NodeJS.ProcessEnv = process.env): string | undefined {
  const selected = explicit || env.TASKFLEET_CONFIG;
  if (selected) return isAbsolute(selected) ? selected : resolve(cwd, selected);
  let current = resolve(cwd);
  while (true) {
    const candidate = join(current, "taskfleet.toml");
    if (existsSync(candidate)) return candidate;
    const parent = dirname(current);
    if (parent === current) return undefined;
    current = parent;
  }
}

export class TaskfleetClient {
  readonly binary: string;
  readonly config: string;
  private readonly cwd: string;
  private readonly exec: Exec;
  private readonly timeoutMs: number;

  constructor(options: ClientOptions) {
    const env = options.env ?? process.env;
    const config = findConfig(options.cwd, options.config, env);
    if (!config) throw new Error(`no taskfleet.toml found from ${options.cwd}`);
    this.config = config;
    this.binary = options.binary || env.TASKFLEET_BIN || "taskfleet";
    this.cwd = dirname(config);
    this.exec = options.exec;
    this.timeoutMs = options.timeoutMs ?? 10_000;
  }

  async call<T>(method: string, input: unknown = {}, signal?: AbortSignal): Promise<T> {
    const result = await this.exec(
      this.binary,
      [method, "--config", this.config, "--input", JSON.stringify(input)],
      { cwd: this.cwd, timeout: this.timeoutMs, signal },
    );
    if (result.code !== 0) {
      const detail = (result.stderr || result.stdout || `exit ${result.code}`).trim().slice(-2_000);
      throw new Error(`taskfleet ${method} failed: ${detail}`);
    }
    try {
      return JSON.parse(result.stdout) as T;
    } catch (error) {
      throw new Error(`taskfleet ${method} returned invalid JSON: ${String(error)}`);
    }
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
        cards.push({
          ...summary,
          paused: status?.execution.paused ?? summary.paused ?? false,
          queue_priority: status?.execution.queue_priority ?? summary.queue_priority ?? 0,
          depends_on: status?.task.depends_on ?? summary.depends_on ?? [],
          step_name: status?.execution.active_step?.id,
          gates: status?.execution.gates ?? [],
        });
      }
    }
    return { at: Date.now(), view, cards };
  }
}
