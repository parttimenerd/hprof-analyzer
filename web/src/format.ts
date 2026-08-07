// Formatting helpers mirroring src/report.rs (format_bytes, fmt_count) so the
// HTML matches the Markdown/JSON views.

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

// In KB mode: plain number with 2 decimals + thousands separators. No "KB" suffix —
// the column header shows "(KB)" as the unit indicator.
export function formatBytesKB(n: number): string {
  return (n / 1024).toLocaleString("en-US", { minimumFractionDigits: 2, maximumFractionDigits: 2 });
}

export function fmtCount(n: number): string {
  return n.toLocaleString("en-US");
}

// Exact byte count with thousands separators, e.g. "509,972,304". MAT's Leak
// Suspects report shows the precise retained byte total alongside the percent
// ("509,972,304 (41.08%)"); this is the analogue for that exact figure.
export function fmtExactBytes(n: number): string {
  return `${n.toLocaleString("en-US")} B`;
}

// Percent of a total (retained / total * 100), matching the OOM-triage basis.
export function pctOf(part: number, total: number): number {
  return total > 0 ? (part / total) * 100 : 0;
}

// Format a percentage with one decimal place and a `<0.1%` floor, mirroring
// Rust's `fmt_pct`. Use this wherever a "% Heap" or "% of total" figure is
// displayed so all formats agree on precision and the floor.
export function fmtPct(p: number): string {
  if (p <= 0) return "0%";
  if (p < 0.1) return "< 0.1%";
  const s = p.toFixed(1);
  return s.endsWith(".0") ? s.slice(0, -2) + "%" : s + "%";
}

// A dump-creation timestamp: millis since epoch -> ISO date (UTC, second res).
export function formatEpochMs(ms: number): string {
  if (ms <= 0) return "";
  const d = new Date(ms);
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
}

export function formatDateNice(ms: number): string {
  if (ms <= 0) return "";
  const d = new Date(ms);
  return d.toLocaleDateString(undefined, {
    year: "numeric", month: "long", day: "numeric",
  }) + " at " + d.toLocaleTimeString(undefined, {
    hour: "2-digit", minute: "2-digit",
  });
}

// Compact display for a class-loader label. Labels are JVM-internal binary
// names using '/' as the package separator (e.g.
// "jdk/internal/loader/ClassLoaders$AppClassLoader"). We show just the final
// simple name for the table cell and keep the full label as a tooltip. The
// boot loader ("<boot>") is passed through verbatim. Returns null when there is
// nothing meaningful to show.
export function shortLoader(label: string | null | undefined): string | null {
  if (!label) return null;
  if (label === "<boot>") return "<boot>";
  // Strip " @ 0xADDR" suffix, normalize slashes to dots, then take last segment.
  const clean = label.replace(/\s*@\s*0x[0-9a-fA-F]+$/, "").replaceAll("/", ".");
  const dot = clean.lastIndexOf(".");
  return dot >= 0 ? clean.slice(dot + 1) : clean;
}

