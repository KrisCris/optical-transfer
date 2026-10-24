import assert from "node:assert/strict";
import test from "node:test";
import { formatBytes, formatDuration, formatRate } from "../format.js";

test("bytes use binary prefixes and switch units at 1024", () => {
  assert.equal(formatBytes(0), "0 B");
  assert.equal(formatBytes(1023), "1023 B");
  assert.equal(formatBytes(1024), "1.0 KiB");
  assert.equal(formatBytes(1536 * 1024), "1.50 MiB");
});

test("rates keep a decimal only while it still means something", () => {
  assert.equal(formatRate(0), "0 KiB/s");
  assert.equal(formatRate(Number.NaN), "0 KiB/s");
  assert.equal(formatRate(42.25), "42.3 KiB/s");
  assert.equal(formatRate(391.4), "391 KiB/s");
  assert.equal(formatRate(2048), "2.00 MiB/s");
});

test("durations round once, so a minute never shows 60 seconds", () => {
  assert.equal(formatDuration(Number.POSITIVE_INFINITY), "—");
  assert.equal(formatDuration(-1), "0.0 s");
  assert.equal(formatDuration(2.34), "2.3 s");
  assert.equal(formatDuration(42.4), "42 s");
  assert.equal(formatDuration(59.7), "1 min 00 s");
  assert.equal(formatDuration(119.7), "2 min 00 s");
  assert.equal(formatDuration(125), "2 min 05 s");
});
