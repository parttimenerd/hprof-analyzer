// Formatting helpers mirroring src/report.rs (format_bytes, fmt_count) so the
// HTML matches the Markdown/JSON views.

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

export function fmtCount(n: number): string {
  return n.toLocaleString("en-US");
}

// Percent of a total (retained / total * 100), matching the OOM-triage basis.
export function pctOf(part: number, total: number): number {
  return total > 0 ? (part / total) * 100 : 0;
}

// A dump-creation timestamp: millis since epoch -> ISO date (UTC, second res).
export function formatEpochMs(ms: number): string {
  if (ms <= 0) return "";
  const d = new Date(ms);
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}
