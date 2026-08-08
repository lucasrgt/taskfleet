import { readFileSync, realpathSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import type { ChildCancelResult } from "./types.ts";

export interface ChildController {
  cancel(sessionId: string, owner: string): Promise<ChildCancelResult>;
}

function bundledPrimeEntrypoint(): string {
  if (!process.argv[1]) throw new Error("Prime Agent executable is unavailable");
  let directory = dirname(realpathSync(process.argv[1]));
  while (true) {
    try {
      const manifest = JSON.parse(readFileSync(resolve(directory, "package.json"), "utf8"));
      if (manifest.name === "prime-agent" && typeof manifest.exports?.["."]?.import === "string") {
        return pathToFileURL(resolve(directory, manifest.exports["."].import)).href;
      }
    } catch {}
    const parent = dirname(directory);
    if (parent === directory) throw new Error("cannot locate the running Prime Agent package");
    directory = parent;
  }
}

async function loadPrime(): Promise<any> {
  try {
    const packageName = "prime-agent";
    return await import(packageName);
  } catch {
    return import(bundledPrimeEntrypoint());
  }
}

export class PrimeRlmController implements ChildController {
  async cancel(sessionId: string, owner: string): Promise<ChildCancelResult> {
    const prime = await loadPrime();
    const socket = process.env.PRIME_AGENT_INTERNAL_DAEMON_SUPERVISOR_SOCKET || prime.defaultDaemonSocketPath();
    const client = new prime.DaemonClient(socket);
    await client.connect();
    let connection: InstanceType<typeof prime.DaemonAgentConnection> | undefined;
    try {
      connection = await prime.DaemonAgentConnection.attach(client, sessionId, { closeClientOnDispose: true });
      const snapshot = await connection.getInitialSnapshot();
      const matches = (snapshot.children ?? []).filter((child: any) =>
        child.sessionName === owner || child.id === owner || child.activeSessionId === owner,
      );
      if (matches.length === 0) return { found: false, cancelled: false, reason: `no RLM child matches ${owner}` };
      if (matches.length > 1) return { found: false, cancelled: false, reason: `multiple RLM children match ${owner}` };
      const child = matches[0];
      const cancelled = await connection.cancelRlmChild(child.id);
      return { found: true, cancelled, childId: child.id, reason: cancelled ? undefined : "child was already terminal" };
    } finally {
      if (connection) await connection.dispose();
      else client.close();
    }
  }
}
