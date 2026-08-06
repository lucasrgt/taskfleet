import type { FleetCard, FleetSnapshot } from "./types.ts";

export const COLUMN_ORDER = ["backlog", "ready", "running", "paused", "blocked", "candidate", "failed", "cancelled", "done"] as const;
export type FleetColumn = (typeof COLUMN_ORDER)[number];

export function columnFor(card: FleetCard): FleetColumn {
  if (card.state === "cancelled") return "cancelled";
  if (card.paused) return "paused";
  if (card.state === "done") return "done";
  if (card.state === "candidate") return "candidate";
  if (card.state === "running") return "running";
  if (card.state === "blocked") return /fail|red|timeout/i.test(card.error ?? "") ? "failed" : "blocked";
  if (card.state === "backlog" && card.ready) return "ready";
  return "backlog";
}

function truncate(text: string, width: number): string {
  if (width <= 0) return "";
  if (text.length <= width) return text.padEnd(width);
  if (width === 1) return "…";
  return `${text.slice(0, width - 1)}…`;
}

function cardLine(card: FleetCard): string {
  const owner = card.owner ? ` @${card.owner}` : "";
  const priority = card.queue_priority ? ` p${card.queue_priority > 0 ? "+" : ""}${card.queue_priority}` : "";
  const deps = card.depends_on.length ? ` ←${card.depends_on.map((uri) => uri.split("/").at(-1)).join(",")}` : "";
  const step = card.step_name ? ` ${card.step_name}` : "";
  const gate = card.gates.length ? ` g:${card.gates.map((item) => item.status[0] ?? "?").join("")}` : "";
  const lease = card.lease_until ? ` l:${card.lease_until}` : "";
  const error = card.error ? ` !${card.error}` : "";
  return `• ${card.title}${priority}${owner}${lease}${deps}${step}${gate}${error}`;
}

export function groupCards(snapshot: FleetSnapshot): Record<FleetColumn, FleetCard[]> {
  const grouped = Object.fromEntries(COLUMN_ORDER.map((column) => [column, []])) as unknown as Record<FleetColumn, FleetCard[]>;
  for (const card of snapshot.cards) grouped[columnFor(card)].push(card);
  return grouped;
}

export function renderBoard(snapshot: FleetSnapshot, width: number): string[] {
  const grouped = groupCards(snapshot);
  const active = COLUMN_ORDER.filter((column) => grouped[column].length > 0);
  if (active.length === 0) return ["TASKFLEET", "No tasks"];
  const columnsPerRow = Math.max(1, Math.min(3, Math.floor((Math.max(width, 24) + 1) / 32)));
  const gap = " │ ";
  const columnWidth = Math.max(20, Math.floor((width - gap.length * (columnsPerRow - 1)) / columnsPerRow));
  const lines: string[] = [];
  const title = `TASKFLEET${snapshot.view ? ` · ${snapshot.view}` : ""} · ${snapshot.cards.length} tasks`;
  lines.push(truncate(title, width).trimEnd());
  for (let start = 0; start < active.length; start += columnsPerRow) {
    const row = active.slice(start, start + columnsPerRow);
    const height = Math.max(...row.map((column) => grouped[column].length));
    lines.push(row.map((column) => truncate(`${column.toUpperCase()} (${grouped[column].length})`, columnWidth)).join(gap).trimEnd());
    for (let index = 0; index < height; index += 1) {
      lines.push(row.map((column) => truncate(grouped[column][index] ? cardLine(grouped[column][index]) : "", columnWidth)).join(gap).trimEnd());
    }
    if (start + columnsPerRow < active.length) lines.push("");
  }
  return lines;
}

export function statusLine(snapshot: FleetSnapshot): string {
  const grouped = groupCards(snapshot);
  return `TF ${grouped.ready.length} ready · ${grouped.running.length} running · ${grouped.paused.length} paused · ${grouped.blocked.length + grouped.failed.length} blocked`;
}

export function snapshotText(snapshot: FleetSnapshot): string {
  return renderBoard(snapshot, 100).join("\n");
}
