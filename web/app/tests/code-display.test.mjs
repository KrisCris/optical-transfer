import assert from "node:assert/strict";
import test from "node:test";

// CodeDisplay only touches the DOM in layout() and paint(); stub the rest.
globalThis.document ??= { createElement: () => ({}) };
const { CodeDisplay, bestGrid } = await import("../code-display.js");

test("the grid follows the screen: side by side when wide, stacked when tall", () => {
  assert.deepEqual([bestGrid(2, 177, 1600, 900).cols, bestGrid(2, 177, 1600, 900).rows], [2, 1]);
  assert.deepEqual([bestGrid(2, 177, 390, 800).cols, bestGrid(2, 177, 390, 800).rows], [1, 2]);
  assert.deepEqual([bestGrid(4, 177, 1000, 1000).cols, bestGrid(4, 177, 1000, 1000).rows], [2, 2]);
  const six = bestGrid(6, 177, 1920, 1080);
  assert.deepEqual([six.cols, six.rows], [3, 2]);
  assert.equal(six.fit, Math.min(1920 / (3 * 177), 1080 / (2 * 177)));
});

/** A display with a deep queue and no canvas: counts which cell flips when. */
function display(codes, fps) {
  const view = new CodeDisplay(null, null);
  view.info = { qrSize: 177 };
  view.layout = () => {};
  view.configure({ codes, fps }, view.info);
  view.flips = [];
  view.paint = (cell) => view.flips.push(cell);
  view.requestFrames = (count) => view.receive(new Uint8Array(count), count, 1);
  view.queue = [];
  view.requested = 0;
  view.shown = 0;
  view.flipTimes = [];
  view.tickGaps = [];
  view.stall = 0;
  view.nextAt = 0;
  view.lastTick = 0;
  return view;
}

function run(view, hz, seconds) {
  const step = 1000 / hz;
  for (let now = 0; now < seconds * 1000; now += step) view.tick(now);
}

test("codes × fps is not capped by the refresh rate: due cells share a frame", () => {
  const view = display(4, 60);
  run(view, 60, 2);
  // 240 codes a second on a 60 Hz screen; a one-flip-per-frame clock gives 120.
  // A frame that lands a hair early flips three cells, and the fourth cannot be
  // made up without painting one cell twice, so a few flips go missing.
  assert.ok(view.shown >= 460 && view.shown <= 484, `shown ${view.shown}`);
  assert.deepEqual(view.flips.slice(0, 8), [0, 1, 2, 3, 0, 1, 2, 3], "cells flip in turn");
});

test("a rate above the refresh rate runs at the refresh rate, never painting over a code", () => {
  const view = display(1, 120);
  const flipsAt = [];
  view.paint = (cell) => flipsAt.push(view.lastTick);
  run(view, 60, 2);
  assert.equal(new Set(flipsAt).size, flipsAt.length, "a cell flipped twice in one frame");
  assert.ok(flipsAt.length >= 118 && flipsAt.length <= 121, `flips ${flipsAt.length}`);
  const fast = display(1, 120);
  run(fast, 144, 2);
  assert.ok(fast.shown >= 236 && fast.shown <= 242, `a 144 Hz screen reaches 120: ${fast.shown}`);
});

test("cells flip staggered, not together, when the refresh rate allows it", () => {
  const view = display(2, 30);
  const flipsAt = [];
  view.paint = (cell) => flipsAt.push([view.lastTick, cell]);
  run(view, 120, 1);
  // At 120 Hz a 30 fps pair flips one cell every ~16.7 ms: never both at once.
  const perTick = new Map();
  for (const [at] of flipsAt) perTick.set(at, (perTick.get(at) ?? 0) + 1);
  assert.ok([...perTick.values()].every((count) => count === 1));
  assert.ok(Math.abs(flipsAt.length - 60) <= 2, `flips ${flipsAt.length}`);
});

test("a stalled window resumes at rate instead of flushing a backlog", () => {
  const view = display(1, 30);
  run(view, 60, 1);
  const before = view.shown;
  view.tick(3000);
  assert.equal(view.shown - before, 1, "one frame after the stall, not ninety");
  assert.equal(view.stall >= 2000, true);
});

test("the queue refills ahead of the clock in batches of at least one grid", () => {
  const requests = [];
  const view = display(4, 60);
  view.requestFrames = (count) => requests.push(count);
  view.refill();
  assert.equal(requests.length, 1);
  assert.ok(requests[0] >= 4);
  view.refill();
  assert.equal(requests.length, 1, "nothing more while a request is outstanding");
});
