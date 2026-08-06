import test from "node:test";
import assert from "node:assert/strict";
import { columnFor, groupCards, renderBoard, statusLine } from "../src/board.ts";
import type { FleetCard, FleetSnapshot } from "../src/types.ts";

const card = (state: string, extra: Partial<FleetCard> = {}): FleetCard => ({
  uri: `task://${state}`,
  title: state,
  state,
  ready: false,
  depends_on: [],
  gates: [],
  ...extra,
});

test("board projects every operational column deterministically", () => {
  const cards = [
    card("backlog"), card("backlog", { uri: "task://ready", ready: true }), card("running"),
    card("backlog", { uri: "task://paused", paused: true }), card("blocked"),
    card("candidate"), card("blocked", { uri: "task://failed", error: "gate failed" }),
    card("cancelled"), card("done"),
  ];
  assert.deepEqual(cards.map(columnFor), ["backlog", "ready", "running", "paused", "blocked", "candidate", "failed", "cancelled", "done"]);
  const snapshot: FleetSnapshot = { at: 1, cards };
  const grouped = groupCards(snapshot);
  assert.equal(Object.values(grouped).reduce((sum, items) => sum + items.length, 0), cards.length);
  assert.match(statusLine(snapshot), /1 ready.*1 running.*1 paused.*2 blocked/);
});

test("board renders within terminal width", () => {
  const snapshot: FleetSnapshot = { at: 1, view: "all", location: { mode: "external", repository: "/repo", config: "/state/taskfleet.toml", state: "/state", enabled: true }, cards: [card("running", { title: "x".repeat(100), owner: "worker" }), card("done")] };
  assert.match(renderBoard(snapshot, 80)[0], /EXTERNAL/);
  assert.match(statusLine(snapshot), /TF external/);
  for (const width of [24, 80, 140]) {
    const lines = renderBoard(snapshot, width);
    assert.ok(lines.length > 2);
    assert.ok(lines.every((line) => line.length <= width));
  }
});
