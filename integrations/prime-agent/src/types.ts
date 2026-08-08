export interface ExecOptions {
  cwd?: string;
  timeout?: number;
  signal?: AbortSignal;
}

export interface ExecResult {
  stdout: string;
  stderr: string;
  code: number;
  killed: boolean;
}

export type Exec = (command: string, args: string[], options?: ExecOptions) => Promise<ExecResult>;

export interface TaskSummary {
  uri: string;
  title: string;
  priority?: string | null;
  depends_on?: string[];
  state: string;
  owner?: string | null;
  lease_until?: number | null;
  error?: string | null;
  ready: boolean;
  paused?: boolean;
  queue_priority?: number;
  step?: number;
  /** Active pipeline workflow id (0.2.1+). */
  workflow?: string | null;
}

export interface TaskStatus {
  task: {
    uri: string;
    title: string;
    priority?: string | null;
    depends_on?: string[];
    workflow?: string | null;
    meta?: Record<string, unknown>;
  };
  execution: {
    state: string;
    owner?: string | null;
    lease_until?: number | null;
    error?: string | null;
    paused?: boolean;
    queue_priority?: number;
    step_index?: number;
    active_step?: { id: string; instruction?: string; args?: Record<string, unknown> } | null;
    gates?: Array<{ id: string; required: boolean; status: string }>;
  };
}

/** One task sharing the same dossier meta path value (task.related). */
export interface RelatedObjective {
  uri: string;
  title: string;
  role?: unknown;
  tags?: string[];
  priority?: string | null;
  depends_on?: string[];
  queue_priority?: number;
  state: string;
  paused?: boolean;
  ready: boolean;
  owner?: string | null;
  error?: string | null;
  revision?: number;
}

export interface RelatedResult {
  task: string;
  path: string;
  value: unknown;
  count: number;
  related: RelatedObjective[];
}

export interface FleetCard extends TaskSummary {
  depends_on: string[];
  step_name?: string;
  gates: Array<{ id: string; required: boolean; status: string }>;
}

export interface FleetLocation {
  mode: "none" | "local" | "external";
  repository: string;
  config: string;
  state: string | null;
  enabled: boolean;
  changed?: boolean;
  purged?: boolean;
}

export interface FleetSnapshot {
  at: number;
  location?: FleetLocation;
  view?: string;
  cards: FleetCard[];
}

export interface ChildCancelResult {
  found: boolean;
  cancelled: boolean;
  childId?: string;
  reason?: string;
}

export interface PrimeUi {
  notify(message: string, level?: "info" | "warning" | "error"): void;
  confirm(title: string, message: string): Promise<boolean>;
  setStatus(key: string, value: string | undefined): void;
  setWidget(
    key: string,
    content: undefined | ((...args: unknown[]) => { render(width: number): string[]; invalidate(): void }),
    options?: { placement: "aboveEditor" | "belowEditor" },
  ): void;
}

export interface PrimeContext {
  cwd: string;
  hasUI: boolean;
  signal?: AbortSignal;
  ui: PrimeUi;
  sessionManager: { getSessionId(): string };
}

export interface PrimeExtensionApi {
  registerFlag(name: string, options: { description?: string; type: "boolean" | "string"; default?: boolean | string }): void;
  getFlag(name: string): boolean | string | undefined;
  exec: Exec;
  registerCommand(name: string, options: {
    description?: string;
    getArgumentCompletions?(prefix: string): Array<{ value: string; label: string; description?: string }> | null;
    handler(args: string, context: PrimeContext): Promise<void> | void;
  }): void;
  on(event: string, handler: (event: unknown, context: PrimeContext) => Promise<void> | void): void;
}
