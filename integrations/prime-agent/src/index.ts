import { TaskfleetHost, type TaskfleetClient } from "./client.ts";
import { renderBoard, statusLine } from "./board.ts";
import { dispatchFleet, fleetCompletions, type FleetCommandRuntime } from "./commands.ts";
import { PrimeRlmController } from "./controller.ts";
import type { FleetLocation, FleetSnapshot, PrimeContext, PrimeExtensionApi } from "./types.ts";

export default function taskfleetExtension(pi: PrimeExtensionApi): void {
  pi.registerFlag("taskfleet-config", { description: "Path to taskfleet.toml", type: "string" });
  pi.registerFlag("taskfleet-bin", { description: "Path to the taskfleet binary", type: "string" });
  pi.registerFlag("taskfleet-view", { description: "Saved view shown by the Kanban", type: "string" });
  pi.registerFlag("taskfleet-refresh-ms", { description: "Kanban refresh interval", type: "string", default: "10000" });
  pi.registerFlag("taskfleet-board", { description: "Show the Taskfleet Kanban widget", type: "boolean", default: true });

  let host: TaskfleetHost | undefined;
  let client: TaskfleetClient | undefined;
  let location: FleetLocation | undefined;
  let snapshot: FleetSnapshot | undefined;
  let boardVisible = pi.getFlag("taskfleet-board") !== false;
  let currentView = String(pi.getFlag("taskfleet-view") || "") || undefined;
  let timer: NodeJS.Timeout | undefined;
  let generation = 0;
  let refreshPromise: Promise<void> | undefined;
  let refreshQueued = false;
  let queuedView: string | undefined;
  let activeContext: PrimeContext | undefined;
  let lifecycleController = new AbortController();
  const childController = new PrimeRlmController();

  function configureHost(ctx: PrimeContext): TaskfleetHost {
    if (!host) host = new TaskfleetHost({
      cwd: ctx.cwd,
      exec: (command, args, options) => pi.exec(command, args, options),
      binary: String(pi.getFlag("taskfleet-bin") || "") || undefined,
      config: String(pi.getFlag("taskfleet-config") || "") || undefined,
    });
    return host;
  }

  function clearUi(ctx?: PrimeContext): void {
    if (!ctx?.hasUI) return;
    ctx.ui.setWidget("taskfleet", undefined);
    ctx.ui.setStatus("taskfleet", undefined);
  }

  function paint(ctx: PrimeContext): void {
    if (!ctx.hasUI || !snapshot) return;
    ctx.ui.setStatus("taskfleet", statusLine(snapshot));
    if (!boardVisible) { ctx.ui.setWidget("taskfleet", undefined); return; }
    const captured = snapshot;
    ctx.ui.setWidget("taskfleet", () => ({
      render(width: number) { return renderBoard(captured, width); },
      invalidate() {},
    }), { placement: "aboveEditor" });
  }

  async function refresh(ctx: PrimeContext, view = currentView): Promise<void> {
    currentView = view;
    queuedView = view;
    refreshQueued = true;
    if (refreshPromise) return refreshPromise;
    refreshPromise = (async () => {
      while (refreshQueued) {
        refreshQueued = false;
        if (!client) throw new Error("Taskfleet is not enabled; use /fleet enable external");
        const requested = ++generation;
        const next = await client.snapshot(queuedView, ctx.signal ?? lifecycleController.signal);
        if (requested === generation) { snapshot = next; paint(ctx); }
      }
    })();
    try { await refreshPromise; }
    finally {
      refreshPromise = undefined;
      if (refreshQueued) await refresh(ctx, queuedView);
    }
  }

  function schedule(ctx: PrimeContext): void {
    if (timer) clearInterval(timer);
    timer = undefined;
    const interval = Number(pi.getFlag("taskfleet-refresh-ms"));
    if (client && ctx.hasUI && Number.isFinite(interval) && interval >= 1000) {
      timer = setInterval(() => void refresh(ctx).catch(() => {}), interval);
      timer.unref();
    }
  }

  async function reactivate(ctx: PrimeContext): Promise<void> {
    generation += 1;
    if (timer) clearInterval(timer);
    timer = undefined;
    if (refreshPromise) await refreshPromise.catch(() => {});
    client = undefined;
    snapshot = undefined;
    clearUi(ctx);
    try {
      location = await configureHost(ctx).locate(lifecycleController.signal);
      if (!location.enabled) return;
      client = configureHost(ctx).client(location);
      await refresh(ctx);
      schedule(ctx);
    } catch (error) {
      location = undefined;
      throw error;
    }
  }

  function commandRuntime(ctx: PrimeContext): FleetCommandRuntime {
    return {
      client,
      location,
      interactive: ctx.hasUI,
      external: (action) => configureHost(ctx).external(action, lifecycleController.signal),
      reactivate: () => reactivate(ctx),
      childController,
      sessionId: ctx.sessionManager.getSessionId(),
      snapshot: async (view) => {
        if (!client) throw new Error("Taskfleet is not enabled; use /fleet enable external");
        return client.snapshot(view ?? currentView);
      },
      refresh: async (view) => refresh(ctx, view ?? currentView),
      setBoard: async (visible, view) => {
        boardVisible = visible;
        if (!visible) paint(ctx);
        else await refresh(ctx, view ?? currentView);
      },
      notify: (message, level = "info") => ctx.ui.notify(message, level),
      confirm: (title, message) => ctx.hasUI ? ctx.ui.confirm(title, message) : Promise.resolve(false),
    };
  }

  pi.registerCommand("fleet", {
    description: "Inspect and control Taskfleet",
    getArgumentCompletions: (prefix: string) => fleetCompletions(prefix, snapshot),
    handler: async (args: string, ctx: PrimeContext) => dispatchFleet(args, commandRuntime(ctx)),
  });

  pi.on("session_start", async (_event: unknown, ctx: PrimeContext) => {
    if (lifecycleController.signal.aborted) lifecycleController = new AbortController();
    activeContext = ctx;
    await reactivate(ctx).catch((error) => ctx.ui.notify(`Taskfleet unavailable: ${String(error)}`, "warning"));
  });

  pi.on("agent_end", async (_event: unknown, ctx: PrimeContext) => {
    if (boardVisible && client) await refresh(ctx).catch(() => {});
  });

  pi.on("session_shutdown", () => {
    generation += 1;
    lifecycleController.abort();
    if (timer) clearInterval(timer);
    timer = undefined;
    clearUi(activeContext);
    activeContext = undefined;
    host = undefined;
    client = undefined;
    location = undefined;
    snapshot = undefined;
  });
}
