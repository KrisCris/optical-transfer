// Camera frames → QR decode jobs → decoded symbols.
//
// Three pieces, split so the scheduling policy can be tested without a camera:
//
//  * RegionTracker decides, for each captured frame, which pixels are worth
//    decoding: one padded crop per code it has seen recently, plus a periodic
//    full-frame scan to acquire or reacquire codes. Pure logic, no DOM.
//  * DecodePool owns the workers. A busy pool drops frames instead of queueing
//    them: a queued camera frame is a stale one, and the fountain does not
//    care which frames it misses.
//  * CaptureLoop runs once per camera frame, reads pixels back and feeds both.
//
// The scheduling rules follow upstream's field-measured receiver, with one
// deliberate difference: a due full scan takes one worker and crops still get
// the rest, instead of the whole capture going to the scan.

/** How long a region survives without a decode or sighting. Longer TTLs were
 *  measured worse upstream: a dead region squats on crop slots. */
export const REGION_TTL_MS = 1500;
/** Full-scan cadence: no lock, a code missing, everything locked. */
export const ACQUIRE_SCAN_MS = 100;
export const DEGRADED_SCAN_MS = 250;
export const LOCKED_SCAN_MS = 1500;
/** The "how many codes does this stream show" high-water mark ages out, so a
 *  sender restarted with fewer codes stops triggering rescans. */
export const EXPECTED_DECAY_MS = 10_000;
export const REGION_PAD = 0.35;
export const MAX_REGIONS = 9;
const MIN_CROP = 32;

export class RegionTracker {
  constructor() {
    this.reset();
  }

  reset() {
    this.regions = [];
    this.lastFullScan = -Infinity;
    this.expected = 0;
    this.expectedAt = 0;
    this.rotate = 0;
    this.nextId = 1;
  }

  decodedCount() {
    let count = 0;
    for (const region of this.regions) if (region.decoded) count++;
    return count;
  }

  /** "acquiring" | "degraded" | "locked" — what the scan cadence is doing. */
  mode() {
    const live = this.decodedCount();
    if (live === 0) return "acquiring";
    return live < this.expected ? "degraded" : "locked";
  }

  /** A code decoded at `box` (capture pixels). `quad`/`modules` arm the
   *  worker's tracked fast path for the next crop of this region. */
  noteDecoded(box, now, quad, modules) {
    const match = this.find(box);
    if (match) {
      const dx = box.x + box.w / 2 - (match.x + match.w / 2);
      const dy = box.y + box.h / 2 - (match.y + match.h / 2);
      // Half-life blend of per-decode motion: a steady hand decays to zero,
      // a moving one keeps the crop padding wide so it leads the target.
      match.drift = 0.5 * match.drift + 0.5 * Math.hypot(dx, dy);
      Object.assign(match, box, { seen: now, decoded: true, hits: match.hits + 1 });
      if (quad) match.quad = quad;
      if (modules) match.modules = modules;
      return match;
    }
    return this.add({ ...box, seen: now, decoded: true, quad, modules });
  }

  /** A code detected but not decoded. Keeps a region alive, never moves it:
   *  failed quads are often clipped or mis-sized, and one overwriting a
   *  decode-proven box aims every following crop at garbage. */
  noteSighting(box, now) {
    const match = this.find(box);
    if (match) {
      match.seen = now;
      return;
    }
    // A sighting may only found a region when it is sized like the codes
    // this stream already decodes; with no reference, full scans own
    // acquisition and phantom regions would only starve them.
    const reference = this.regions.find((region) => region.decoded);
    if (!reference) return;
    const ratio = Math.max(box.w, box.h) / Math.max(reference.w, reference.h);
    if (ratio < 0.5 || ratio > 2) return;
    this.add({ ...box, seen: now, decoded: false });
  }

  find(box) {
    for (const region of this.regions) {
      const dx = Math.abs(box.x + box.w / 2 - (region.x + region.w / 2));
      const dy = Math.abs(box.y + box.h / 2 - (region.y + region.h / 2));
      if (dx < Math.max(box.w, region.w) / 2 && dy < Math.max(box.h, region.h) / 2) return region;
    }
    return null;
  }

  add(fields) {
    const region = { id: this.nextId++, drift: 0, hits: 0, quad: undefined, modules: 0, ...fields };
    this.regions.push(region);
    if (this.regions.length > MAX_REGIONS) {
      this.regions.sort((a, b) => Number(b.decoded) - Number(a.decoded) || b.seen - a.seen);
      this.regions.length = MAX_REGIONS;
    }
    return region;
  }

  /**
   * Decide what to decode from one captured frame.
   *
   * Returns `{ full, crops }`: whether to submit a full-frame scan, and up to
   * `slots` crops (minus one if scanning). Crops are rotated across calls so a
   * short pool does not starve the same region every frame.
   */
  plan(now, slots, width, height) {
    this.regions = this.regions.filter((region) => now - region.seen <= REGION_TTL_MS);
    const live = this.decodedCount();
    if (live >= this.expected || now - this.expectedAt > EXPECTED_DECAY_MS) {
      this.expected = live;
      this.expectedAt = now;
    }
    const interval = live === 0 ? ACQUIRE_SCAN_MS : live < this.expected ? DEGRADED_SCAN_MS : LOCKED_SCAN_MS;
    const full = slots > 0 && now - this.lastFullScan >= interval;
    if (full) this.lastFullScan = now;

    const crops = [];
    let free = slots - (full ? 1 : 0);
    const count = this.regions.length;
    for (let index = 0; index < count && free > 0; index++) {
      const region = this.regions[(index + this.rotate) % count];
      const size = Math.max(region.w, region.h);
      const pad = Math.round(size * REGION_PAD + Math.min(size, 2 * region.drift));
      const x = Math.max(0, Math.floor(region.x - pad));
      const y = Math.max(0, Math.floor(region.y - pad));
      const w = Math.min(width - x, Math.ceil(region.w + 2 * pad));
      const h = Math.min(height - y, Math.ceil(region.h + 2 * pad));
      if (w < MIN_CROP || h < MIN_CROP) continue;
      crops.push({ x, y, w, h, quad: region.quad, modules: region.modules, region: region.id });
      free--;
    }
    this.rotate++;
    return { full, crops };
  }
}

/** Fixed-slot decode workers. Slot identity is stable across resizes:
 *  shrinking pops from the end, so surviving workers keep their index. */
export class DecodePool {
  constructor(createWorker, onReply) {
    this.createWorker = createWorker;
    this.onReply = onReply;
    this.workers = [];
    this.jobs = [];
    this.version = null;
  }

  get size() {
    return this.workers.length;
  }

  get free() {
    let count = 0;
    for (const job of this.jobs) if (!job) count++;
    return count;
  }

  resize(count) {
    while (this.workers.length > count) {
      this.workers.pop().terminate();
      this.jobs.pop();
    }
    while (this.workers.length < count) {
      const slot = this.workers.length;
      const worker = this.createWorker();
      worker.onmessage = (event) => {
        const reply = event.data;
        if (reply.id === -1) {
          if (reply.version) this.version = `${reply.version} (${reply.build})`;
          if (reply.fatal) this.onReply(null, reply);
          return;
        }
        const job = this.jobs[slot];
        if (!job || job.id !== reply.id) return;
        this.jobs[slot] = null;
        this.onReply(job, reply);
      };
      this.workers.push(worker);
      this.jobs.push(null);
    }
  }

  /** Hand a job to a free worker. False when all are busy. */
  submit(job, transfer) {
    const slot = this.jobs.indexOf(null);
    if (slot === -1) return false;
    this.jobs[slot] = job;
    this.workers[slot].postMessage(job.message, transfer);
    return true;
  }

  /** Forget in-flight jobs without killing workers; their replies are dropped. */
  forget() {
    this.jobs.fill(null);
  }
}

/**
 * One capture per camera frame. `requestVideoFrameCallback` fires once per
 * frame the camera delivers; animation frames would fire at display rate and
 * capture the same camera frame twice on a 120 Hz screen.
 */
export class CaptureLoop {
  constructor({ video, pool, tracker, onCapture, onDropped }) {
    this.video = video;
    this.pool = pool;
    this.tracker = tracker;
    this.onCapture = onCapture ?? (() => {});
    this.onDropped = onDropped ?? (() => {});
    this.generation = 0;
    this.nextId = 1;
    this.canvas = null;
    this.context = null;
  }

  start() {
    const generation = ++this.generation;
    const schedule = () => {
      if (generation !== this.generation) return;
      if (this.video.requestVideoFrameCallback) this.video.requestVideoFrameCallback(tick);
      else requestAnimationFrame(tick);
    };
    // rVFC chains survive a stopped stream and resume on the next one; the
    // generation check is what keeps a restarted loop from running twice.
    const tick = () => {
      if (generation !== this.generation) return;
      this.capture(generation);
      schedule();
    };
    schedule();
  }

  stop() {
    this.generation++;
  }

  capture(generation) {
    const video = this.video;
    const width = video.videoWidth;
    const height = video.videoHeight;
    if (!width || !height) return;
    const now = performance.now();
    const slots = this.pool.free;
    if (slots === 0) {
      this.onDropped(now);
      return;
    }
    const plan = this.tracker.plan(now, slots, width, height);
    if (!plan.full && plan.crops.length === 0) return;

    if (!this.canvas) {
      this.canvas = document.createElement("canvas");
      this.context = this.canvas.getContext("2d", { willReadFrequently: true });
    }
    if (this.canvas.width !== width || this.canvas.height !== height) {
      this.canvas.width = width;
      this.canvas.height = height;
    }
    this.context.drawImage(video, 0, 0);
    this.onCapture(now, plan);

    const submit = (x, y, w, h, full, quad, modules) => {
      const image = this.context.getImageData(x, y, w, h);
      const id = this.nextId++;
      const message = { id, buffer: image.data.buffer, width: w, height: h, offsetX: x, offsetY: y, full, quad, modules };
      return this.pool.submit({ id, generation, capturedAt: now, full, area: w * h, message }, [image.data.buffer]);
    };
    if (plan.full) submit(0, 0, width, height, true);
    for (const crop of plan.crops) {
      if (!submit(crop.x, crop.y, crop.w, crop.h, false, crop.quad, crop.modules)) break;
    }
  }
}
