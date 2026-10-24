// Receiver telemetry: everything the details panel and charts show.
//
// Two clocks. Pipeline counters (captures, decodes, worker time) run for the
// whole camera session; transfer counters restart with each stream and drive
// speed, ETA and the history chart. Nothing here touches the DOM.

const WINDOW_MS = 2000;
export const BUCKET_MS = 250;
export const HISTORY_BUCKETS = 240; // one minute

class Rolling {
  constructor() {
    this.samples = [];
  }
  push(at, value = 1) {
    this.samples.push(at, value);
  }
  /** Sum of values in the last `window` ms, pruning older samples. */
  sum(now, window = WINDOW_MS) {
    const cutoff = now - window;
    let start = 0;
    while (start < this.samples.length && this.samples[start] < cutoff) start += 2;
    if (start) this.samples.splice(0, start);
    let total = 0;
    for (let index = 1; index < this.samples.length; index += 2) total += this.samples[index];
    return total;
  }
  clear() {
    this.samples.length = 0;
  }
}

class Latency {
  constructor() {
    this.count = 0;
    this.total = 0;
    this.recent = 0;
  }
  add(ms) {
    this.count++;
    this.total += ms;
    // Exponential moving average: the live number should follow the phone
    // warming up, not the first second's average.
    this.recent = this.count === 1 ? ms : this.recent * 0.9 + ms * 0.1;
  }
}

export class Telemetry {
  constructor() {
    this.reset();
  }

  reset() {
    this.sessionStart = null;
    this.captures = new Rolling();
    this.decodes = new Rolling();
    this.symbols = new Rolling();
    this.busy = new Rolling();
    this.totals = {
      captures: 0, dropped: 0, fullScans: 0, crops: 0,
      decodes: 0, symbols: 0, sightings: 0, trackedAttempts: 0, trackedHits: 0,
      workerMs: 0,
    };
    this.latency = { full: new Latency(), crop: new Latency(), tracked: new Latency() };
    this.resetTransfer();
  }

  resetTransfer() {
    this.startAt = null;
    this.endAt = null;
    this.stats = null;
    this.usefulFrames = 0;
    this.useful = new Rolling();
    this.history = [];
    this.historyStart = null;
    this.peakKiBs = 0;
    this.peakScanned = 0;
    this.blockLen = null;
  }

  // ---- pipeline -----------------------------------------------------------

  capture(now, plan) {
    this.sessionStart ??= now;
    this.totals.captures++;
    this.captures.push(now);
    if (plan.full) this.totals.fullScans++;
    this.totals.crops += plan.crops.length;
  }

  dropped(now) {
    this.sessionStart ??= now;
    this.totals.captures++;
    this.totals.dropped++;
    this.captures.push(now);
  }

  reply(now, job, reply) {
    this.totals.decodes++;
    this.decodes.push(now);
    const symbols = reply.symbols?.length ?? 0;
    this.totals.symbols += symbols;
    this.symbols.push(now, symbols);
    this.totals.sightings += reply.sightings?.length ?? 0;
    if (reply.trackedAttempted) this.totals.trackedAttempts++;
    if (reply.trackedHit) this.totals.trackedHits++;
    if (Number.isFinite(reply.decodeMs)) {
      this.totals.workerMs += reply.decodeMs;
      this.busy.push(now, reply.decodeMs);
      const path = job.full ? "full" : reply.trackedHit ? "tracked" : "crop";
      this.latency[path].add(reply.decodeMs);
    }
  }

  // ---- transfer -----------------------------------------------------------

  /** A frame of our stream reached the receiver. `capturedAt` starts the
   *  transfer clock at the capture that produced the first frame, so aiming
   *  time is not counted. `useful` frames carried new information. */
  accepted(now, capturedAt, useful, newStream, blockLen) {
    if (newStream || this.startAt === null) {
      this.resetTransfer();
      this.startAt = capturedAt;
      this.historyStart = capturedAt;
    }
    if (blockLen) this.blockLen = blockLen;
    if (!useful) return;
    this.usefulFrames++;
    this.useful.push(now, 1);
    this.record(now, this.blockLen ?? 0);
  }

  /** Latest receiver counters, read at render rate rather than per frame. */
  setStats(stats) {
    if (stats) this.stats = stats;
  }

  completed(now, stats) {
    if (stats) this.stats = stats;
    this.endAt = now;
  }

  record(now, bytes) {
    const bucket = Math.floor((now - this.historyStart) / BUCKET_MS);
    while (this.history.length <= bucket) this.history.push(0);
    this.history[bucket] += bytes;
  }

  /** Recovered-data rate history in KiB/s, one point per bucket, newest last. */
  series(now) {
    if (this.historyStart === null) return [];
    const end = Math.floor(((this.endAt ?? now) - this.historyStart) / BUCKET_MS);
    const points = [];
    for (let bucket = Math.max(0, end - HISTORY_BUCKETS + 1); bucket <= end; bucket++) {
      points.push(((this.history[bucket] ?? 0) / 1024) * (1000 / BUCKET_MS));
    }
    return points;
  }

  snapshot(now) {
    const seconds = (ms) => ms / 1000;
    const window = Math.min(WINDOW_MS, this.sessionStart === null ? 0 : now - this.sessionStart);
    const perSecond = (rolling) => (window > 0 ? rolling.sum(now) / seconds(window) : 0);
    const stats = this.stats;
    const end = this.endAt ?? now;
    const elapsed = this.startAt === null ? 0 : seconds(Math.max(0, end - this.startAt));

    const transferWindow = this.startAt === null ? 0 : Math.min(WINDOW_MS, now - this.startAt);
    const usefulRate = this.endAt === null && transferWindow > 0
      ? this.useful.sum(now) / seconds(transferWindow) : 0;
    const blockLen = stats?.blockLen ?? this.blockLen ?? 0;
    const liveKiBs = (usefulRate * blockLen) / 1024;
    // Peak over a one-second window, from the bucketed history. Windows are
    // scanned once each; the newest bucket is still filling, so it waits.
    const perWindow = Math.round(1000 / BUCKET_MS);
    const settled = this.endAt === null ? this.history.length - 1 : this.history.length;
    for (let index = this.peakScanned; index + perWindow <= settled; index++) {
      this.peakScanned = index + 1;
      let total = 0;
      for (let offset = 0; offset < perWindow; offset++) total += this.history[index + offset];
      this.peakKiBs = Math.max(this.peakKiBs, total / 1024);
    }

    let eta = null;
    if (stats && this.endAt === null && elapsed >= 1 && usefulRate > 0) {
      // Elimination finishes as soon as there is one useful frame per block,
      // so the floor is k; redundancy beyond that shows up as a lower rate.
      eta = Math.max(1, stats.totalBlocks - this.usefulFrames) / usefulRate;
    }

    // Recovered so far, not the payload's size: until the end only solved
    // blocks count, and the last block may be short.
    const done = this.endAt !== null;
    const solved = done ? stats?.totalBlocks ?? 0 : stats?.solvedBlocks ?? 0;
    const recovered = done ? stats?.totalBytes ?? 0 : Math.min(stats?.totalBytes ?? 0, solved * blockLen);

    const workers = this.workers ?? 1;
    return {
      elapsed,
      done,
      stats,
      usefulFrames: this.usefulFrames,
      liveKiBs,
      averageKiBs: elapsed > 0 ? recovered / 1024 / elapsed : 0,
      peakKiBs: this.peakKiBs,
      eta,
      catchRate: stats?.seqSpan ? stats.uniqueFrames / stats.seqSpan : null,
      // Frames taken per block recovered; 1.0 is a perfect fountain.
      overhead: solved ? stats.uniqueFrames / solved : null,
      captureFps: perSecond(this.captures),
      decodeFps: perSecond(this.decodes),
      symbolFps: perSecond(this.symbols),
      // Share of the pool's wall time spent decoding over the window.
      utilization: window > 0 ? Math.min(1, this.busy.sum(now) / (window * workers)) : 0,
      latency: {
        full: this.latency.full.recent,
        crop: this.latency.crop.recent,
        tracked: this.latency.tracked.recent,
      },
      totals: this.totals,
    };
  }
}
