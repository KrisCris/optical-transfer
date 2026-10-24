// Canvas drawing for the receiver: throughput history, the source-block map,
// and the per-code brackets over the viewfinder. Colors come from CSS custom
// properties so the charts follow the theme.

function token(element, name, fallback) {
  return getComputedStyle(element).getPropertyValue(name).trim() || fallback;
}

/** Size a canvas's backing store to its CSS box at device resolution. */
export function fitCanvas(canvas) {
  const ratio = window.devicePixelRatio || 1;
  const width = Math.max(1, Math.round(canvas.clientWidth * ratio));
  const height = Math.max(1, Math.round(canvas.clientHeight * ratio));
  if (canvas.width !== width || canvas.height !== height) {
    canvas.width = width;
    canvas.height = height;
  }
  return ratio;
}

/** Area chart of `points` (KiB/s, oldest first) over a fixed-length axis. */
export function drawThroughput(canvas, points, capacity) {
  const ratio = fitCanvas(canvas);
  const context = canvas.getContext("2d");
  const { width, height } = canvas;
  context.clearRect(0, 0, width, height);
  const accent = token(canvas, "--accent", "#4fe3c1");
  const grid = token(canvas, "--line", "#2a2f37");
  const max = Math.max(8, ...points) * 1.15;
  const step = width / Math.max(1, capacity - 1);
  const offset = (capacity - points.length) * step;
  const y = (value) => height - (value / max) * (height - 4 * ratio) - ratio;

  context.strokeStyle = grid;
  context.lineWidth = ratio;
  context.setLineDash([2 * ratio, 4 * ratio]);
  for (const fraction of [0.25, 0.5, 0.75]) {
    const level = Math.round(height * fraction) + 0.5;
    context.beginPath();
    context.moveTo(0, level);
    context.lineTo(width, level);
    context.stroke();
  }
  context.setLineDash([]);
  if (points.length < 2) return max;

  context.beginPath();
  context.moveTo(offset, height);
  points.forEach((value, index) => context.lineTo(offset + index * step, y(value)));
  context.lineTo(offset + (points.length - 1) * step, height);
  context.closePath();
  const fill = context.createLinearGradient(0, 0, 0, height);
  fill.addColorStop(0, `${accent}55`);
  fill.addColorStop(1, `${accent}00`);
  context.fillStyle = fill;
  context.fill();

  context.beginPath();
  points.forEach((value, index) => {
    const x = offset + index * step;
    if (index === 0) context.moveTo(x, y(value));
    else context.lineTo(x, y(value));
  });
  context.strokeStyle = accent;
  context.lineWidth = 1.5 * ratio;
  context.lineJoin = "round";
  context.stroke();
  return max;
}

/**
 * One pixel per source block, scaled up without smoothing. Blocks solved in
 * the last `GLOW_MS` are drawn bright, so peeling cascades and elimination
 * show up as bursts across the map.
 */
const GLOW_MS = 700;

export class BlockMap {
  constructor(canvas) {
    this.canvas = canvas;
    this.total = 0;
    this.solvedAt = null;
  }

  reset() {
    this.total = 0;
    this.solvedAt = null;
    this.canvas.style.aspectRatio = "";
    this.canvas.getContext("2d").clearRect(0, 0, this.canvas.width, this.canvas.height);
  }

  /** Columns for `total` blocks at roughly a 4:1 aspect. */
  static columns(total) {
    return Math.max(1, Math.min(total, Math.ceil(Math.sqrt(total * 4))));
  }

  draw(map, now) {
    const total = map.length;
    if (!total) return;
    if (total !== this.total) {
      this.total = total;
      this.solvedAt = new Float64Array(total).fill(-1);
      const columns = BlockMap.columns(total);
      this.canvas.width = columns;
      this.canvas.height = Math.ceil(total / columns);
      this.canvas.style.aspectRatio = `${columns} / ${this.canvas.height}`;
    }
    const css = getComputedStyle(this.canvas);
    const solved = parseColor(css.getPropertyValue("--accent")) ?? [79, 227, 193];
    const empty = parseColor(css.getPropertyValue("--cell")) ?? [34, 39, 46];
    const glow = [255, 255, 255];
    const context = this.canvas.getContext("2d");
    const image = context.createImageData(this.canvas.width, this.canvas.height);
    const pixels = image.data;
    for (let index = 0; index < total; index++) {
      if (map[index] && this.solvedAt[index] < 0) this.solvedAt[index] = now;
      const age = now - this.solvedAt[index];
      let color = empty;
      if (map[index]) {
        const heat = Math.max(0, 1 - age / GLOW_MS);
        color = heat > 0 ? mix(solved, glow, heat * 0.85) : solved;
      }
      const offset = index * 4;
      pixels[offset] = color[0];
      pixels[offset + 1] = color[1];
      pixels[offset + 2] = color[2];
      pixels[offset + 3] = 255;
    }
    context.putImageData(image, 0, 0);
  }
}

function mix(a, b, amount) {
  return [0, 1, 2].map((channel) => Math.round(a[channel] + (b[channel] - a[channel]) * amount));
}

function parseColor(value) {
  const text = value.trim();
  const hex = text.match(/^#([0-9a-f]{6})$/i);
  if (hex) return [0, 2, 4].map((start) => parseInt(hex[1].slice(start, start + 2), 16));
  const rgb = text.match(/^rgb\((\d+)[ ,]+(\d+)[ ,]+(\d+)/);
  return rgb ? rgb.slice(1, 4).map(Number) : null;
}

/** One color per code, assigned in reading order so a code keeps its color
 *  through dropouts: "the amber one is not decoding" beats counting boxes. */
export const CODE_COLORS = [
  "#42e8ff", "#54ff7e", "#ffd94a", "#ff6ad5", "#c08bff",
  "#ff9a4d", "#8dff4a", "#ff5f5f", "#ffffff",
];
const BRACKET_FADE_MS = 700;

function readingOrder(a, b) {
  const dy = a.y + a.h / 2 - (b.y + b.h / 2);
  if (Math.abs(dy) > Math.max(a.h, b.h) / 2) return dy;
  return a.x + a.w / 2 - (b.x + b.w / 2);
}

/** Corner brackets around each code the decoder is reading, mapped from
 *  capture pixels through the video's `object-fit: contain` letterbox. */
export function drawBrackets(canvas, video, regions, now) {
  const ratio = fitCanvas(canvas);
  const context = canvas.getContext("2d");
  context.clearRect(0, 0, canvas.width, canvas.height);
  const vw = video.videoWidth;
  const vh = video.videoHeight;
  if (!vw || !vh) return;
  const scale = Math.min(canvas.width / vw, canvas.height / vh);
  const offsetX = (canvas.width - vw * scale) / 2;
  const offsetY = (canvas.height - vh * scale) / 2;
  context.lineWidth = 2.5 * ratio;
  context.lineCap = "round";
  const ordered = regions.filter((region) => region.decoded).sort(readingOrder);
  ordered.forEach((region, slot) => {
    const age = now - region.seen;
    if (age > BRACKET_FADE_MS) return;
    const color = CODE_COLORS[slot % CODE_COLORS.length];
    const pad = 0.06 * Math.max(region.w, region.h) * scale;
    const x = offsetX + region.x * scale - pad;
    const y = offsetY + region.y * scale - pad;
    const w = region.w * scale + 2 * pad;
    const h = region.h * scale + 2 * pad;
    const arm = 0.22 * Math.min(w, h);
    context.globalAlpha = 1 - age / BRACKET_FADE_MS;
    context.strokeStyle = color;
    context.shadowColor = color;
    context.shadowBlur = 6 * ratio;
    context.beginPath();
    for (const [cx, cy, sx, sy] of [[x, y, 1, 1], [x + w, y, -1, 1], [x + w, y + h, -1, -1], [x, y + h, 1, -1]]) {
      context.moveTo(cx, cy + sy * arm);
      context.lineTo(cx, cy);
      context.lineTo(cx + sx * arm, cy);
    }
    context.stroke();
  });
  context.globalAlpha = 1;
  context.shadowBlur = 0;
}
