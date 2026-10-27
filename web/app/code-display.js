// The code stream on a canvas: grid layout, frame queue and the flip clock.
//
// Display rules that are decode requirements, not styling:
//  * The canvas raster is an integer number of device pixels per module, and
//    CSS stretches the remainder smoothly. Nearest-neighbour stretching makes
//    adjacent modules differ by a whole pixel, which at grid densities is the
//    difference between every code decoding and none (measured upstream).
//  * Every code stays up a whole number of display frames, the fewest that
//    are not much shorter than 1000/fps, so fps is a ceiling. A phone filming
//    at 60 fps reads a code only while it holds still for most of two of its
//    frames. 60 fps on a 144 Hz screen held codes three and two refreshes in
//    turn (21 and 14 ms), and an iPhone read the 14 ms ones about one time in
//    eight against two in three for the rest; at 36 fps every code is up four
//    refreshes there, and two on a 60 Hz screen. Time is counted in display
//    frames, measured from animation-frame spacing, so a frame the browser
//    skips lengthens a code and never shortens the next.
//  * With several codes on screen, cell j flips j/N of the way through the
//    hold rather than all together, so a camera exposure straddling a flip
//    loses only the codes that change on that frame instead of all of them.
//  * A module byte's bits 0–2 say which of red, green and blue are dark there:
//    7 is black and 0 white, so a plain code is painted exactly as before and
//    a colour code (three codes, one per channel) needs nothing else.
//  * Every cell that has come due flips on the same animation frame: deferring
//    to one flip per frame would cap codes × fps at the display refresh rate.
//    No cell flips twice in one frame, though: the first code would be painted
//    over before any camera saw it. Asking for more fps than the display
//    refreshes therefore runs at the refresh rate, and the flip count says so.

/** How much shorter than 1000/fps a code may stay up, so that a refresh
 *  measured a hair long does not add a whole frame to every code. */
const HOLD_SLACK = 0.95;
/** Animation-frame spacing assumed until a few frames have been measured. */
const ASSUMED_PERIOD_MS = 1000 / 60;
/** Codes up for less than this lose most of themselves to a 60 fps camera. */
export const SHORT_HOLD_MS = 25;

/** Display frames a code stays up at `fps` on a display that shows a frame
 *  every `period` ms: the fewest whole frames not much shorter than 1000/fps.
 *  60 fps on a 144 Hz screen is three refreshes each, 48 codes a second;
 *  alternating three and two would make 60, at 14 ms for most of them. */
export function holdFrames(fps, period) {
  return Math.max(1, Math.ceil((HOLD_SLACK * 1000) / (fps * period)));
}

/** Milliseconds between this display's frames, the median of `frames`
 *  animation-frame gaps; null if none arrive, as in a hidden tab. */
export function measureRefresh(frames = 20) {
  return new Promise((resolve) => {
    const gaps = [];
    let last = null;
    let handle = 0;
    const timeout = setTimeout(() => {
      cancelAnimationFrame(handle);
      resolve(null);
    }, 2000);
    const step = (now) => {
      if (last !== null) gaps.push(now - last);
      last = now;
      if (gaps.length < frames) {
        handle = requestAnimationFrame(step);
        return;
      }
      clearTimeout(timeout);
      gaps.sort((a, b) => a - b);
      resolve(gaps[gaps.length >> 1] || null);
    };
    handle = requestAnimationFrame(step);
  });
}

/** The `cols × rows` tiling of `codes` square cells of `cell` modules that
 *  gives each module the most CSS pixels in a `width × height` box. `fit` is
 *  CSS pixels per module. */
export function bestGrid(codes, cell, width, height) {
  let best = null;
  for (let cols = 1; cols <= codes; cols++) {
    if (codes % cols) continue;
    const rows = codes / cols;
    const fit = Math.min(width / (cols * cell), height / (rows * cell));
    if (!best || fit > best.fit) best = { cols, rows, fit };
  }
  return best;
}

/** Owns the canvas: layout, frame queue and the flip clock. */
export class CodeDisplay {
  constructor(canvas, stage) {
    this.canvas = canvas;
    this.stage = stage;
    this.staging = document.createElement("canvas");
    this.handle = 0;
  }

  configure(stream, streamInfo) {
    const resized = !this.info || this.info.qrSize !== streamInfo.qrSize;
    this.info = streamInfo;
    this.codes = stream.codes;
    this.fps = stream.fps;
    this.lookahead = Math.max(6, this.codes * 3);
    if (resized || !this.cells || this.cells.length !== this.codes) {
      this.cells = new Array(this.codes).fill(null);
      this.upSince = new Array(this.codes).fill(-Infinity);
      this.cursor = 0;
    }
    this.layout();
  }

  start(requestFrames) {
    this.requestFrames = requestFrames;
    this.reset(performance.now());
    this.refill();
    const tick = (now) => {
      this.handle = requestAnimationFrame(tick);
      this.tick(now);
    };
    this.handle = requestAnimationFrame(tick);
  }

  /** An empty queue, zeroed counters and the clock at `now`. */
  reset(now) {
    this.queue = [];
    this.requested = 0;
    this.shown = 0;
    this.flipTimes = [];
    this.tickGaps = [];
    this.stall = 0;
    this.lastTick = now;
    // Display frames so far, the frame the next cell is due on (fractional,
    // so cells spread evenly through a hold), and the frame each cell's code
    // went up on.
    this.refreshes = 0;
    this.dueAt = 1;
    this.upSince = new Array(this.codes).fill(-Infinity);
  }

  stop() {
    cancelAnimationFrame(this.handle);
    this.handle = 0;
    this.queue = [];
    this.cells = null;
    this.info = null;
  }

  receive(modules, count, frameSize) {
    this.requested = Math.max(0, this.requested - count);
    for (let index = 0; index < count; index++) {
      this.queue.push(modules.subarray(index * frameSize, (index + 1) * frameSize));
    }
  }

  refill() {
    const missing = this.lookahead - this.queue.length - this.requested;
    if (missing <= 0) return;
    const batch = Math.max(missing, this.codes);
    this.requested += batch;
    this.requestFrames(batch);
  }

  tick(now) {
    const gap = now - this.lastTick;
    this.lastTick = now;
    this.tickGaps.push(gap);
    if (this.tickGaps.length > 60) this.tickGaps.shift();
    // Browsers throttle animation frames in hidden or occluded windows. The
    // stream then freezes mid-flip, which looks like a receiver failure.
    if (gap > 1000) this.stall = Math.max(this.stall, gap);
    // A gap of several frames is frames the browser skipped: they count.
    const period = this.period();
    this.refreshes += period ? Math.max(1, Math.round(gap / period)) : 1;
    const hold = holdFrames(this.fps, period ?? ASSUMED_PERIOD_MS);
    for (let placed = 0; placed < this.codes && this.refreshes >= this.dueAt - 0.5; placed++) {
      // A code stays up its whole hold, even when the schedule runs late.
      if (this.refreshes - this.upSince[this.cursor] < hold) break;
      const frame = this.queue.shift();
      if (!frame) break;
      this.cells[this.cursor] = frame;
      this.paint(this.cursor);
      this.upSince[this.cursor] = this.refreshes;
      this.cursor = (this.cursor + 1) % this.codes;
      this.shown++;
      this.flipTimes.push(now);
      // A late flip (a stall, an empty queue) moves the schedule with it, so
      // the stream resumes at rate instead of bursting out a backlog.
      if (this.refreshes - this.dueAt >= 0.5) this.dueAt = this.refreshes;
      this.dueAt += hold / this.codes;
    }
    while (this.flipTimes.length && this.flipTimes[0] < now - 1000) this.flipTimes.shift();
    this.refill();
  }

  /** Pick the grid shape that gives each module the most pixels, so two
   *  codes sit side by side on a landscape screen and stacked on a phone. */
  layout() {
    if (!this.info) return;
    const fullscreen = document.body.classList.contains("qr-full");
    const style = getComputedStyle(this.stage);
    const padX = parseFloat(style.paddingLeft) + parseFloat(style.paddingRight);
    const padY = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
    const width = fullscreen ? window.innerWidth : this.stage.clientWidth - padX;
    const height = fullscreen ? window.innerHeight : Math.max(280, Math.min(window.innerHeight * 0.72, width * 1.1)) - padY;
    const cell = this.info.qrSize;
    const best = bestGrid(this.codes, cell, width, height);
    const ratio = window.devicePixelRatio || 1;
    this.cols = best.cols;
    this.rows = best.rows;
    this.scale = Math.max(1, Math.floor(best.fit * ratio));
    this.staging.width = this.cols * cell;
    this.staging.height = this.rows * cell;
    this.canvas.width = this.staging.width * this.scale;
    this.canvas.height = this.staging.height * this.scale;
    const stretch = Math.max(1, best.fit * ratio / this.scale);
    this.canvas.style.width = `${(this.canvas.width / ratio) * stretch}px`;
    this.canvas.style.height = `${(this.canvas.height / ratio) * stretch}px`;
    this.image = new ImageData(cell, cell);
    const context = this.canvas.getContext("2d");
    context.fillStyle = "#fff";
    context.fillRect(0, 0, this.canvas.width, this.canvas.height);
    this.cells?.forEach((frame, index) => frame && this.paint(index));
  }

  paint(index) {
    const cell = this.info.qrSize;
    const frame = this.cells[index];
    const pixels = this.image.data;
    for (let module = 0; module < frame.length; module++) {
      const dark = frame[module];
      const offset = module * 4;
      pixels[offset] = dark & 1 ? 0 : 255;
      pixels[offset + 1] = dark & 2 ? 0 : 255;
      pixels[offset + 2] = dark & 4 ? 0 : 255;
      pixels[offset + 3] = 255;
    }
    const x = (index % this.cols) * cell;
    const y = Math.floor(index / this.cols) * cell;
    this.staging.getContext("2d").putImageData(this.image, x, y);
    const context = this.canvas.getContext("2d");
    context.imageSmoothingEnabled = false;
    context.drawImage(this.staging, x, y, cell, cell, x * this.scale, y * this.scale, cell * this.scale, cell * this.scale);
  }

  /** Milliseconds between display frames: the median animation-frame gap,
   *  which an occasional skipped frame does not move. Null for the first few
   *  frames, which count one each. */
  period() {
    if (this.tickGaps.length < 3) return null;
    const sorted = [...this.tickGaps].sort((a, b) => a - b);
    return sorted[Math.floor(sorted.length / 2)] || null;
  }

  /** Display refresh estimated from animation-frame spacing. */
  refreshHz() {
    if (this.tickGaps.length < 10) return null;
    return 1000 / this.period();
  }

  /** How long each code stays up on this display: frames and milliseconds. */
  hold() {
    const period = this.period() ?? ASSUMED_PERIOD_MS;
    const frames = holdFrames(this.fps, period);
    return { frames, ms: frames * period };
  }
}
