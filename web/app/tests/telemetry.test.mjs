import assert from "node:assert/strict";
import test from "node:test";
import { BUCKET_MS, Telemetry } from "../telemetry.js";

const near = (actual, expected, tolerance = 1e-6) =>
  assert.ok(Math.abs(actual - expected) <= tolerance, `${actual} is not ${expected}`);

const stats = (overrides = {}) => ({
  totalBlocks: 100, blockLen: 1024, totalBytes: 102400, uniqueFrames: 12, solvedBlocks: 10, seqSpan: 15,
  ...overrides,
});

/** Eleven useful frames, 100 ms apart, from a capture at t = 50. */
function streaming() {
  const telemetry = new Telemetry();
  telemetry.workers = 1;
  telemetry.capture(0, { full: true, crops: [] });
  for (let index = 0; index <= 10; index++) {
    const now = 100 + index * 100;
    telemetry.accepted(now, index === 0 ? 50 : now - 20, true, index === 0, 1024);
  }
  telemetry.accepted(650, 630, false, false, 1024); // redundant
  telemetry.setStats(stats());
  return telemetry;
}

test("the transfer clock starts at the capture that produced the first frame", () => {
  const snap = streaming().snapshot(1100);
  near(snap.elapsed, 1.05);
  assert.equal(snap.usefulFrames, 11, "a redundant frame is not progress");
});

test("live rate, ETA, catch rate and overhead come from useful frames only", () => {
  const snap = streaming().snapshot(1100);
  const usefulPerSecond = 11 / 1.05;
  near(snap.liveKiBs, usefulPerSecond);
  near(snap.eta, (100 - 11) / usefulPerSecond);
  near(snap.catchRate, 12 / 15);
  near(snap.overhead, 12 / 10);
});

test("mid-transfer average and overhead count recovered blocks, not the payload", () => {
  const snap = streaming().snapshot(1100);
  near(snap.averageKiBs, 10 / 1.05, 1e-9);
  assert.equal(new Telemetry().snapshot(0).overhead, null);
});

test("a stall drops the live rate to zero and withdraws the ETA", () => {
  const snap = streaming().snapshot(4000);
  assert.equal(snap.liveKiBs, 0);
  assert.equal(snap.eta, null);
});

test("completion freezes elapsed time and the history", () => {
  const telemetry = streaming();
  telemetry.completed(1200, stats({ uniqueFrames: 100 }));
  const at = telemetry.snapshot(1200);
  const later = telemetry.snapshot(9000);
  assert.equal(later.done, true);
  near(later.elapsed, at.elapsed);
  near(later.elapsed, 1.15);
  assert.equal(later.liveKiBs, 0);
  assert.equal(later.eta, null);
  near(later.averageKiBs, 100 / 1.15);
  near(later.overhead, 1);
  assert.deepEqual(telemetry.series(9000), telemetry.series(1200));
});

test("history buckets add up to the data recovered, and the peak is a one-second window", () => {
  const telemetry = streaming();
  telemetry.completed(1200);
  const points = telemetry.series(1200);
  const kib = points.reduce((sum, rate) => sum + (rate * BUCKET_MS) / 1000, 0);
  near(kib, 11);
  const snap = telemetry.snapshot(1200);
  assert.ok(snap.peakKiBs >= 9 && snap.peakKiBs <= 11, String(snap.peakKiBs));
});

test("a new stream restarts the transfer counters but not the pipeline", () => {
  const telemetry = streaming();
  telemetry.accepted(2000, 1990, true, true, 512);
  const snap = telemetry.snapshot(2000);
  assert.equal(snap.usefulFrames, 1);
  near(snap.elapsed, 0.01);
  assert.equal(snap.totals.captures, 1);
});

test("pipeline latency is split by decode path", () => {
  const telemetry = new Telemetry();
  telemetry.reply(0, { full: true }, { symbols: [{}], decodeMs: 30 });
  telemetry.reply(0, { full: false }, { symbols: [{}], decodeMs: 3, trackedAttempted: true, trackedHit: true });
  telemetry.reply(0, { full: false }, { symbols: [], decodeMs: 9, trackedAttempted: true });
  const snap = telemetry.snapshot(0);
  assert.deepEqual(snap.latency, { full: 30, tracked: 3, crop: 9 });
  assert.equal(snap.totals.trackedAttempts, 2);
  assert.equal(snap.totals.trackedHits, 1);
  assert.equal(snap.totals.symbols, 2);
});
