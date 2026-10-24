// Units shared by both modes. Binary prefixes throughout (1 KiB = 1024 B).

export function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / 1024 / 1024).toFixed(2)} MiB`;
}

export function formatRate(kibPerSecond) {
  if (!Number.isFinite(kibPerSecond) || kibPerSecond <= 0) return "0 KiB/s";
  if (kibPerSecond >= 1024) return `${(kibPerSecond / 1024).toFixed(2)} MiB/s`;
  return `${kibPerSecond.toFixed(kibPerSecond < 100 ? 1 : 0)} KiB/s`;
}

export function formatDuration(seconds) {
  if (!Number.isFinite(seconds)) return "—";
  if (seconds < 10) return `${Math.max(0, seconds).toFixed(1)} s`;
  // Round once, up front: rounding the parts separately prints "1 min 60 s".
  const whole = Math.round(seconds);
  if (whole < 60) return `${whole} s`;
  return `${Math.floor(whole / 60)} min ${String(whole % 60).padStart(2, "0")} s`;
}
