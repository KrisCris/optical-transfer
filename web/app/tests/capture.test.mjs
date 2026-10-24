import assert from "node:assert/strict";
import test from "node:test";
import {
  ACQUIRE_SCAN_MS, DEGRADED_SCAN_MS, DecodePool, EXPECTED_DECAY_MS, LOCKED_SCAN_MS,
  REGION_TTL_MS, RegionTracker,
} from "../capture.js";

const W = 1280;
const H = 960;
const box = (x, y, size = 300) => ({ x, y, w: size, h: size });

test("with nothing locked, every 100 ms is a full scan and nothing is cropped", () => {
  const tracker = new RegionTracker();
  assert.deepEqual(tracker.plan(0, 4, W, H), { full: true, crops: [] });
  assert.equal(tracker.plan(ACQUIRE_SCAN_MS - 1, 4, W, H).full, false);
  assert.equal(tracker.plan(ACQUIRE_SCAN_MS, 4, W, H).full, true);
  assert.equal(tracker.mode(), "acquiring");
});

test("a due full scan takes one worker and the locked codes keep the rest", () => {
  const tracker = new RegionTracker();
  tracker.plan(0, 4, W, H);
  tracker.noteDecoded(box(100, 100), 10, [1, 2, 3, 4], 177);
  tracker.noteDecoded(box(700, 100), 10, null, 177);
  // Every code it has seen is decoding: locked, so crops fill every slot and
  // the next full scan waits the long interval.
  const locked = tracker.plan(16, 4, W, H);
  assert.equal(tracker.mode(), "locked");
  assert.equal(locked.full, false);
  assert.equal(locked.crops.length, 2);
  const tracked = locked.crops.find((crop) => crop.quad);
  assert.deepEqual(tracked.quad, [1, 2, 3, 4], "the tracked fast path is armed");
  assert.equal(tracked.modules, 177);
  assert.equal(tracker.plan(LOCKED_SCAN_MS - 1, 2, W, H).full, false);
  const due = tracker.plan(LOCKED_SCAN_MS, 2, W, H);
  assert.equal(due.full, true);
  assert.equal(due.crops.length, 1, "the scan takes one of two workers, not both");
});

test("crops are padded, clamped to the frame and rotate when slots run short", () => {
  const tracker = new RegionTracker();
  tracker.plan(0, 1, W, H);
  tracker.noteDecoded(box(0, 0), 1);
  tracker.noteDecoded(box(900, 600), 1);
  const first = tracker.plan(16, 1, W, H).crops;
  const second = tracker.plan(32, 1, W, H).crops;
  assert.equal(first.length, 1);
  assert.equal(second.length, 1);
  assert.notEqual(first[0].region, second[0].region, "one slot alternates between codes");
  for (const crop of [...first, ...second]) {
    assert.ok(crop.x >= 0 && crop.y >= 0 && crop.x + crop.w <= W && crop.y + crop.h <= H);
    assert.ok(crop.w > 300, "a crop is wider than the code it chases");
  }
});

test("a moving code widens its crop", () => {
  const still = new RegionTracker();
  const moving = new RegionTracker();
  for (let step = 0; step < 5; step++) {
    still.noteDecoded(box(400, 300), step * 16);
    moving.noteDecoded(box(400 + step * 40, 300), step * 16);
  }
  const width = (tracker) => tracker.plan(100, 2, W, H).crops[0].w;
  assert.ok(width(moving) > width(still));
});

test("sightings keep regions alive but never move or invent them", () => {
  const tracker = new RegionTracker();
  tracker.noteSighting(box(100, 100), 0);
  assert.equal(tracker.regions.length, 0, "no decoded reference: full scans own acquisition");

  const locked = tracker.noteDecoded(box(100, 100), 0);
  tracker.noteSighting({ x: 120, y: 110, w: 200, h: 200 }, 1000);
  assert.deepEqual([locked.x, locked.y, locked.w], [100, 100, 300], "a sighting does not move a region");
  assert.equal(locked.seen, 1000, "but it does keep it alive");

  tracker.noteSighting(box(800, 500, 1000), 1000);
  assert.equal(tracker.regions.length, 1, "a sighting 3x the stream's code size is not a code");
  tracker.noteSighting(box(800, 500, 320), 1000);
  assert.equal(tracker.regions.length, 2);
  assert.equal(tracker.decodedCount(), 1);
});

test("losing one of two codes rescans faster, and the expectation ages out", () => {
  const tracker = new RegionTracker();
  tracker.noteDecoded(box(100, 100), 0);
  tracker.noteDecoded(box(700, 100), 0);
  tracker.plan(0, 4, W, H);
  // Only the first code keeps decoding.
  tracker.noteDecoded(box(100, 100), REGION_TTL_MS);
  tracker.plan(REGION_TTL_MS + 1, 4, W, H);
  assert.equal(tracker.mode(), "degraded");
  const at = REGION_TTL_MS + 1;
  assert.equal(tracker.plan(at + DEGRADED_SCAN_MS, 4, W, H).full, true);

  // A sender restarted with one code: after the decay, one code is "locked".
  const later = at + EXPECTED_DECAY_MS + 1;
  tracker.noteDecoded(box(100, 100), later);
  tracker.plan(later, 4, W, H);
  assert.equal(tracker.mode(), "locked");
});

test("regions expire after their TTL and the tracker falls back to acquiring", () => {
  const tracker = new RegionTracker();
  tracker.noteDecoded(box(100, 100), 0);
  assert.equal(tracker.plan(REGION_TTL_MS, 2, W, H).crops.length, 1);
  assert.equal(tracker.plan(REGION_TTL_MS + 1, 2, W, H).crops.length, 0);
  assert.equal(tracker.mode(), "acquiring");
});

class FakeWorker {
  constructor() {
    this.posted = [];
    this.terminated = false;
  }
  postMessage(message) {
    this.posted.push(message);
  }
  terminate() {
    this.terminated = true;
  }
  reply(data) {
    this.onmessage({ data });
  }
}

test("the decode pool drops work when busy and ignores stale replies", () => {
  const workers = [];
  const replies = [];
  const pool = new DecodePool(() => {
    const worker = new FakeWorker();
    workers.push(worker);
    return worker;
  }, (job, reply) => replies.push([job?.id ?? null, reply]));
  pool.resize(2);
  assert.equal(pool.free, 2);
  assert.equal(pool.submit({ id: 1, message: "a" }), true);
  assert.equal(pool.submit({ id: 2, message: "b" }), true);
  assert.equal(pool.submit({ id: 3, message: "c" }), false, "a busy pool drops, never queues");

  workers[0].reply({ id: -1, version: "1.2", build: "simd" });
  assert.equal(pool.version, "1.2 (simd)");
  workers[0].reply({ id: 99 });
  assert.equal(replies.length, 0, "a reply for a job the slot no longer holds is dropped");
  workers[0].reply({ id: 1, symbols: [] });
  assert.deepEqual(replies.map(([id]) => id), [1]);
  assert.equal(pool.free, 1);

  pool.forget();
  workers[1].reply({ id: 2 });
  assert.equal(replies.length, 1, "forgotten jobs do not report");

  pool.resize(1);
  assert.equal(workers[1].terminated, true);
  assert.equal(workers[0].terminated, false, "shrinking keeps the first slots");
  workers[0].reply({ id: -1, fatal: true });
  assert.deepEqual(replies.at(-1)[0], null);
});
