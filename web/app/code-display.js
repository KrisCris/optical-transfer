// The code stream on a canvas: grid layout, frame queue and the flip clock.
//
// Display rules that are decode requirements, not styling:
//  * The canvas raster is an integer number of device pixels per module, and
//    CSS stretches the remainder smoothly. Nearest-neighbour stretching makes
//    adjacent modules differ by a whole pixel, which at grid densities is the
//    difference between every code decoding and none (measured upstream).
//  * With several codes on screen, cell j flips at phase j/N of the frame
//    interval rather than all together, so a camera exposure straddling a
//    flip loses at most one code instead of all of them.
//  * Every cell that has come due flips on the same animation frame: deferring
//    to one flip per frame would cap codes × fps at the display refresh rate.

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
    this.interval = 1000 / stream.fps;
    this.subInterval = this.interval / this.codes;
    this.lookahead = Math.max(6, this.codes * 3);
    if (resized || !this.cells || this.cells.length !== this.codes) {
      this.cells = new Array(this.codes).fill(null);
      this.cursor = 0;
    }
    this.layout();
  }

  start(requestFrames) {
    this.requestFrames = requestFrames;
    this.queue = [];
    this.requested = 0;
    this.shown = 0;
    this.flipTimes = [];
    this.tickGaps = [];
    this.stall = 0;
    this.nextAt = performance.now();
    this.lastTick = performance.now();
    this.refill();
    const tick = (now) => {
      this.handle = requestAnimationFrame(tick);
      this.tick(now);
    };
    this.handle = requestAnimationFrame(tick);
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
    if (now < this.nextAt) return;
    if (now - this.nextAt > this.interval) this.nextAt = now;
    while (now >= this.nextAt) {
      const frame = this.queue.shift();
      if (!frame) {
        this.nextAt = now + this.subInterval;
        break;
      }
      this.cells[this.cursor] = frame;
      this.paint(this.cursor);
      this.cursor = (this.cursor + 1) % this.codes;
      this.shown++;
      this.flipTimes.push(now);
      this.nextAt += this.subInterval;
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
      const shade = frame[module] ? 0 : 255;
      const offset = module * 4;
      pixels[offset] = pixels[offset + 1] = pixels[offset + 2] = shade;
      pixels[offset + 3] = 255;
    }
    const x = (index % this.cols) * cell;
    const y = Math.floor(index / this.cols) * cell;
    this.staging.getContext("2d").putImageData(this.image, x, y);
    const context = this.canvas.getContext("2d");
    context.imageSmoothingEnabled = false;
    context.drawImage(this.staging, x, y, cell, cell, x * this.scale, y * this.scale, cell * this.scale, cell * this.scale);
  }

  /** Display refresh estimated from animation-frame spacing. */
  refreshHz() {
    if (this.tickGaps.length < 10) return null;
    const sorted = [...this.tickGaps].sort((a, b) => a - b);
    return 1000 / sorted[Math.floor(sorted.length / 2)];
  }
}
