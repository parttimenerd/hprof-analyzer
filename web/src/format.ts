// Formatting helpers mirroring src/report/format.rs (format_bytes, fmt_count,
// fmt_pct) so the HTML matches the Markdown/JSON views byte-for-byte.

// Binary (1024-based) byte sizes: B/KB/MB/GB/TB/PB. Mirrors Rust format_bytes,
// including the boundary guard so a value just under a unit threshold does not
// round up into "1024.0 KB".
export function formatBytes(n: number): string {
  const KB = 1024;
  const MB = 1024 * KB;
  const GB = 1024 * MB;
  const TB = 1024 * GB;
  const PB = 1024 * TB;
  if (n < KB) return `${n} B`;
  const kb = n / KB;
  if (n < MB && Math.round(kb * 10) < 1024 * 10) return `${kb.toFixed(1)} KB`;
  const mb = n / MB;
  if (n < GB && Math.round(mb * 10) < 1024 * 10) return `${mb.toFixed(1)} MB`;
  const gb = n / GB;
  if (n < TB && Math.round(gb * 100) < 1024 * 100) return `${gb.toFixed(2)} GB`;
  const tb = n / TB;
  if (n < PB && Math.round(tb * 100) < 1024 * 100) return `${tb.toFixed(2)} TB`;
  return `${(n / PB).toFixed(2)} PB`;
}

export function fmtCount(n: number): string {
  return n.toLocaleString("en-US");
}

// One-decimal percent with a "<0.1%" floor for tiny-but-nonzero shares, so a
// nonzero contributor is never printed as exactly "0.0%". Mirrors Rust fmt_pct.
export function fmtPct(p: number): string {
  if (p > 0 && p < 0.05) return "<0.1%";
  return `${p.toFixed(1)}%`;
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

// A dump-creation timestamp: millis since epoch -> ISO date (UTC, second res).
export function formatEpochMs(ms: number): string {
  if (ms <= 0) return "";
  const d = new Date(ms);
  return d.toISOString().replace(/\.\d{3}Z$/, "Z");
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

