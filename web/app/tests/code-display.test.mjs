import assert from "node:assert/strict";
import test from "node:test";

// CodeDisplay only touches the DOM in layout() and paint(); stub the rest.
globalThis.document ??= { createElement: () => ({}) };
const { CodeDisplay, bestGrid, holdFrames } = await import("../code-display.js");

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
  view.reset(0);
  view.flips = [];
  view.paint = (cell) => view.flips.push(cell);
  view.requestFrames = (count) => view.receive(new Uint8Array(count), count, 1);
  view.refill();
  return view;
}

function run(view, hz, seconds) {
  const step = 1000 / hz;
  for (let now = 0; now < seconds * 1000; now += step) view.tick(now);
}

/** Each flip as [display frame, cell, time], from here on. */
function logFlips(view) {
  const log = [];
  view.paint = (cell) => log.push([view.refreshes, cell, view.lastTick]);
  return log;
}

/** For every cell, how many display frames each of its codes stayed up,
 *  leaving out flips before `from` ms while the refresh is still measured. */
function holds(log, from = 200) {
  const last = new Map();
  const frames = [];
  for (const [refresh, cell, at] of log) {
    if (last.has(cell) && at >= from) frames.push(refresh - last.get(cell));
    last.set(cell, refresh);
  }
  return frames;
}

test("codes × fps is not capped by the refresh rate: due cells share a frame", () => {
  const view = display(4, 60);
  run(view, 60, 2);
  // 240 codes a second on a 60 Hz screen; a one-flip-per-frame clock gives 120.
  // The first frame flips three cells, every later one all four.
  assert.ok(view.shown >= 478 && view.shown <= 480, `shown ${view.shown}`);
  assert.deepEqual(view.flips.slice(0, 8), [0, 1, 2, 3, 0, 1, 2, 3], "cells flip in turn");
});

test("a rate above the refresh rate runs at the refresh rate, never painting over a code", () => {
  const view = display(1, 120);
  const flipsAt = [];
  view.paint = (cell) => flipsAt.push(view.lastTick);
  run(view, 60, 2);
  assert.equal(new Set(flipsAt).size, flipsAt.length, "a cell flipped twice in one frame");
  assert.ok(flipsAt.length >= 118 && flipsAt.length <= 121, `flips ${flipsAt.length}`);
  // One 144 Hz refresh is short of 1/120 s, so each code stays up two: 72 a
  // second, evenly, rather than 120 alternating one refresh and two.
  const fast = display(1, 120);
  const log = logFlips(fast);
  run(fast, 144, 2);
  assert.ok(fast.shown >= 143 && fast.shown <= 145, `a 144 Hz screen holds 120 fps codes two refreshes: ${fast.shown}`);
  assert.ok(holds(log).every((frames) => frames === 2));
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
  view.reset(0);
  view.requestFrames = (count) => requests.push(count);
  view.refill();
  assert.equal(requests.length, 1);
  assert.ok(requests[0] >= 4);
  view.refill();
  assert.equal(requests.length, 1, "nothing more while a request is outstanding");
});

test("a code stays up the fewest whole refreshes not much shorter than 1000/fps", () => {
  const frames = (fps, hz) => holdFrames(fps, 1000 / hz);
  assert.equal(frames(36, 144), 4, "28 ms");
  assert.equal(frames(36, 60), 2, "33 ms: 30 a second");
  assert.equal(frames(36, 30), 1, "a device drawing 30 frames a second");
  assert.equal(frames(36, 120), 4);
  assert.equal(frames(36, 165), 5);
  assert.equal(frames(36, 75), 2);
  assert.equal(frames(60, 144), 3, "never three and two in turn");
  assert.equal(frames(60, 60), 1);
  assert.equal(frames(24, 60), 3);
  assert.equal(frames(30, 144), 5);
  assert.equal(frames(72, 144), 2);
  assert.equal(frames(120, 60), 1, "never less than one refresh");
});

test("at 144 Hz every 36 fps code stays up four refreshes, and 60 fps three", () => {
  for (const [fps, expected, perSecond] of [[36, 4, 36], [60, 3, 48]]) {
    const view = display(1, fps);
    const log = logFlips(view);
    run(view, 144, 2);
    const up = holds(log);
    assert.ok(up.length > 0 && up.every((frames) => frames === expected), `${fps} fps: ${up.join(" ")}`);
    assert.ok(Math.abs(view.shown - 2 * perSecond) <= 2, `${fps} fps: ${view.shown} in 2 s`);
  }
});

test("36 fps runs at 30 on a 60 Hz screen and on one drawing 30 frames a second", () => {
  for (const [hz, expected] of [[60, 2], [30, 1]]) {
    const view = display(1, 36);
    const log = logFlips(view);
    run(view, hz, 2);
    assert.ok(holds(log).every((frames) => frames === expected), `${hz} Hz`);
    assert.ok(Math.abs(view.shown - 60) <= 2, `${hz} Hz: ${view.shown} in 2 s`);
  }
});

test("four codes at 144 Hz flip one a refresh, each up four refreshes", () => {
  const view = display(4, 36);
  const log = logFlips(view);
  run(view, 144, 2);
  const steady = log.filter(([, , at]) => at >= 200);
  const perFrame = new Map();
  for (const [refresh] of steady) perFrame.set(refresh, (perFrame.get(refresh) ?? 0) + 1);
  assert.ok([...perFrame.values()].every((count) => count === 1), "two cells flipped together");
  assert.ok(holds(log).every((frames) => frames === 4));
  assert.ok(Math.abs(view.shown - 288) <= 4, `shown ${view.shown}`);
});

test("a frame the browser skips lengthens a code and never shortens the next", () => {
  const view = display(1, 36);
  const log = logFlips(view);
  const step = 1000 / 144;
  // Every seventh animation frame never arrives.
  for (let index = 0; index < 288; index++) if (index % 7 !== 6) view.tick(index * step);
  const up = holds(log);
  assert.ok(up.every((frames) => frames >= 4), up.join(" "));
  const times = log.map(([, , at]) => at).filter((at) => at >= 200);
  for (let index = 1; index < times.length; index++) {
    assert.ok(times[index] - times[index - 1] >= 4 * step - 0.01, `a code up ${times[index] - times[index - 1]} ms`);
  }
});
