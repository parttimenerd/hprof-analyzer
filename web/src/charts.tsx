import React from "react";
import type {
  DepthBucket,
  GcRootTypeRow,
  HistRow,
  KindStat,
  RetentionSummary,
  Suspect,
} from "./types";
import { fmtCount, formatBytes } from "./format";

// Dependency-free SVG charts (kept lib-free to stay well under the bundle
// budget). Each chart renders ONLY when its backing data is present; the
// paired table in App.tsx is the accessibility fallback.

const PALETTE = [
  "#2563eb",
  "#16a34a",
  "#d97706",
  "#dc2626",
  "#7c3aed",
  "#0891b2",
  "#db2777",
  "#65a30d",
  "#ca8a04",
  "#9333ea",
  "#0d9488",
  "#e11d48",
];
const color = (i: number) => PALETTE[i % PALETTE.length];

// ── Pie / donut ─────────────────────────────────────────────────────────────
interface Slice {
  name: string;
  value: number;
}

function polar(cx: number, cy: number, r: number, ang: number): [number, number] {
  return [cx + r * Math.cos(ang), cy + r * Math.sin(ang)];
}

function Pie({ data, fmt, donut }: { data: Slice[]; fmt: (n: number) => string; donut?: boolean }) {
  const total = data.reduce((s, d) => s + d.value, 0);
  if (total <= 0) return null;
  const cx = 110;
  const cy = 110;
  const r = 100;
  const ir = donut ? 55 : 0;
  let a0 = -Math.PI / 2;
  const paths = data.map((d, i) => {
    const frac = d.value / total;
    const a1 = a0 + frac * Math.PI * 2;
    const large = frac > 0.5 ? 1 : 0;
    const [x0, y0] = polar(cx, cy, r, a0);
    const [x1, y1] = polar(cx, cy, r, a1);
    let path: string;
    if (ir > 0) {
      const [ix1, iy1] = polar(cx, cy, ir, a1);
      const [ix0, iy0] = polar(cx, cy, ir, a0);
      path = `M ${x0} ${y0} A ${r} ${r} 0 ${large} 1 ${x1} ${y1} L ${ix1} ${iy1} A ${ir} ${ir} 0 ${large} 0 ${ix0} ${iy0} Z`;
    } else {
      path = `M ${cx} ${cy} L ${x0} ${y0} A ${r} ${r} 0 ${large} 1 ${x1} ${y1} Z`;
    }
    a0 = a1;
    return <path key={i} d={path} fill={color(i)} stroke="var(--bg)" strokeWidth={1} />;
  });
  return (
    <div className="chart-wrap" style={{ display: "flex", gap: "1.5rem", alignItems: "center", flexWrap: "wrap" }}>
      <svg width={220} height={220} viewBox="0 0 220 220" role="img" aria-label="pie chart">
        {paths}
      </svg>
      <ul style={{ listStyle: "none", padding: 0, margin: 0, fontSize: "0.82rem" }}>
        {data.map((d, i) => (
          <li key={i} style={{ margin: "0.2rem 0", display: "flex", alignItems: "center", gap: "0.4rem" }}>
            <span style={{ width: 12, height: 12, background: color(i), display: "inline-block", borderRadius: 2 }} />
            <span>
              {d.name} — {fmt(d.value)} ({((d.value / total) * 100).toFixed(1)}%)
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ── Horizontal bar ──────────────────────────────────────────────────────────
function HBar({ data, fmt, barColor }: { data: Slice[]; fmt: (n: number) => string; barColor?: number }) {
  const max = data.reduce((m, d) => Math.max(m, d.value), 0);
  if (max <= 0) return null;
  return (
    <div className="chart-wrap">
      {data.map((d, i) => (
        <div key={i} style={{ display: "flex", alignItems: "center", gap: "0.5rem", margin: "0.18rem 0", fontSize: "0.8rem" }}>
          <span style={{ width: 220, textAlign: "right", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={d.name}>
            {d.name}
          </span>
          <span style={{ flex: 1, background: "var(--border)", borderRadius: 3, height: 16, position: "relative" }}>
            <span
              style={{
                position: "absolute",
                left: 0,
                top: 0,
                bottom: 0,
                width: `${(d.value / max) * 100}%`,
                background: color(barColor ?? i),
                borderRadius: 3,
              }}
            />
          </span>
          <span style={{ width: 90, textAlign: "right", fontVariantNumeric: "tabular-nums" }}>{fmt(d.value)}</span>
        </div>
      ))}
    </div>
  );
}

// ── Vertical bar (histogram / concentration) ────────────────────────────────
function VBar({
  data,
  fmt,
  barColor,
  yMaxPct,
}: {
  data: { label: string; value: number }[];
  fmt: (n: number) => string;
  barColor?: number;
  yMaxPct?: number;
}) {
  const max = yMaxPct ?? data.reduce((m, d) => Math.max(m, d.value), 0);
  if (max <= 0) return null;
  return (
    <div className="chart-wrap" style={{ display: "flex", alignItems: "flex-end", gap: 2, height: 200, borderLeft: "1px solid var(--border)", borderBottom: "1px solid var(--border)", padding: "0 4px" }}>
      {data.map((d, i) => (
        <div key={i} style={{ flex: 1, display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "flex-end", height: "100%", minWidth: 4 }} title={`${d.label}: ${fmt(d.value)}`}>
          <span
            style={{
              width: "100%",
              maxWidth: 40,
              height: `${(d.value / max) * 100}%`,
              background: color(barColor ?? 0),
              borderRadius: "2px 2px 0 0",
            }}
          />
          <span style={{ fontSize: "0.62rem", color: "var(--muted)", marginTop: 2, whiteSpace: "nowrap" }}>{d.label}</span>
        </div>
      ))}
    </div>
  );
}

// ── Chart wrappers keyed to model fields ────────────────────────────────────
export function HeapCompositionChart({ data }: { data: KindStat[] }) {
  if (data.length < 2) return null;
  return <Pie data={data.map((k) => ({ name: k.kind, value: k.shallow_heap }))} fmt={formatBytes} donut />;
}

export function TopClassesChart({ data }: { data: HistRow[] }) {
  if (data.length === 0) return null;
  const N = 10;
  const top: Slice[] = data.slice(0, N).map((r) => ({ name: r.pretty_class, value: r.retained }));
  if (data.length > N) {
    const rest = data.slice(N).reduce((s, r) => s + r.retained, 0);
    if (rest > 0) top.push({ name: "(rest)", value: rest });
  }
  return <HBar data={top} fmt={formatBytes} />;
}

export function LeakShareChart({ suspects, total }: { suspects: Suspect[]; total: number }) {
  if (suspects.length === 0 || total <= 0) return null;
  const rows: Slice[] = suspects.map((s) => ({ name: s.pretty_class, value: s.retained }));
  const sum = suspects.reduce((s, x) => s + x.retained, 0);
  if (total > sum) rows.push({ name: "(remainder)", value: total - sum });
  return <Pie data={rows} fmt={formatBytes} />;
}

export function ConcentrationChart({ rc }: { rc: RetentionSummary }) {
  if (rc.top1_bp === 0 && rc.top10_bp === 0 && rc.top100_bp === 0) return null;
  return (
    <VBar
      data={[
        { label: "Top 1", value: rc.top1_bp / 100 },
        { label: "Top 10", value: rc.top10_bp / 100 },
        { label: "Top 100", value: rc.top100_bp / 100 },
      ]}
      fmt={(v) => `${v.toFixed(1)}%`}
      yMaxPct={100}
    />
  );
}

export function DepthHistogramChart({ data }: { data: DepthBucket[] }) {
  if (data.length === 0) return null;
  // Deep dumps can produce hundreds of depth buckets; rendering one bar per
  // depth is unreadable. Cap the x-axis to the first MAX_BARS depths and fold
  // everything deeper into a single ">=N" bucket so the shape stays legible.
  const MAX_BARS = 40;
  let bars: { label: string; value: number }[];
  if (data.length <= MAX_BARS) {
    bars = data.map((b) => ({ label: String(b.depth), value: b.objects }));
  } else {
    const head = data.slice(0, MAX_BARS - 1);
    const tail = data.slice(MAX_BARS - 1);
    const tailStart = tail[0].depth;
    const tailSum = tail.reduce((s, b) => s + b.objects, 0);
    bars = head.map((b) => ({ label: String(b.depth), value: b.objects }));
    bars.push({ label: `≥${tailStart}`, value: tailSum });
  }
  return <VBar data={bars} fmt={fmtCount} barColor={4} />;
}

export function GcRootsChart({ data }: { data: GcRootTypeRow[] }) {
  if (data.length < 2) return null;
  return <HBar data={data.map((r) => ({ name: r.root_type, value: r.count }))} fmt={fmtCount} barColor={2} />;
}
