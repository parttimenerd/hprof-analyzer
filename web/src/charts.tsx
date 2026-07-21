import React from "react";
import { hierarchy, treemap, treemapSquarify } from "d3-hierarchy";
import type {
  DepthBucket,
  GcRootTypeRow,
  HistRow,
  KindStat,
  LoaderRollup,
  PackageNode,
  RetentionSummary,
  Suspect,
} from "./types";
import { fmtCount, formatBytes, shortLoader } from "./format";
import { Pie as ChartPie, Bar as ChartBar } from "react-chartjs-2";
import { themeColors, useThemeKey } from "./chartSetup";
import "./chartSetup";

// Chart.js-based charts (via react-chartjs-2, over the tree-shaken chart.js
// core registered in chartSetup.ts). Each chart renders ONLY when its backing
// data is present; the paired table in App.tsx is the accessibility fallback.
// TreemapBar is intentionally kept as a bespoke non-Chart.js flex-div bar.

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

function Pie({ data, fmt, donut, titles, onSlice }: { data: Slice[]; fmt: (n: number) => string; donut?: boolean; titles?: string[]; onSlice?: (i: number) => void }) {
  const total = data.reduce((s, d) => s + d.value, 0);
  if (total <= 0) return null;
  const themeKey = useThemeKey();
  const t = themeColors();
  const bg = data.map((_, i) => color(i));
  const chartData = {
    labels: data.map((d) => d.name),
    datasets: [
      {
        data: data.map((d) => d.value),
        backgroundColor: bg,
        borderColor: t.bg,
        borderWidth: 1,
      },
    ],
  };
  const options = {
    responsive: true,
    maintainAspectRatio: false,
    cutout: donut ? "50%" : 0,
    onClick: onSlice
      ? (_e: unknown, els: { index: number }[]) => {
          if (els.length) onSlice(els[0].index);
        }
      : undefined,
    plugins: {
      legend: {
        position: "right" as const,
        labels: { color: t.fg, boxWidth: 12, font: { size: 12 } },
      },
      tooltip: {
        callbacks: {
          label: (ctx: { dataIndex: number }) => {
            const i = ctx.dataIndex;
            if (titles?.[i]) return titles[i];
            const v = data[i].value;
            return `${data[i].name} — ${fmt(v)} (${((v / total) * 100).toFixed(1)}%)`;
          },
        },
      },
    },
  };
  return (
    <div key={themeKey} className="chart-wrap" role="img" aria-label="Pie chart" style={{ position: "relative", height: 240, maxWidth: 520 }}>
      <ChartPie data={chartData} options={options} />
    </div>
  );
}

// ── Horizontal bar ──────────────────────────────────────────────────────────
function HBar({ data, fmt, barColor, titles, onBar }: { data: Slice[]; fmt: (n: number) => string; barColor?: number; titles?: string[]; onBar?: (i: number) => void }) {
  const max = data.reduce((m, d) => Math.max(m, d.value), 0);
  if (max <= 0) return null;
  const themeKey = useThemeKey();
  const t = themeColors();
  const barCol = barColor != null ? color(barColor) : undefined;
  const chartData = {
    labels: data.map((d) => d.name),
    datasets: [
      {
        data: data.map((d) => d.value),
        backgroundColor: barCol ?? data.map((_, i) => color(i)),
        borderRadius: 3,
      },
    ],
  };
  const options = {
    indexAxis: "y" as const,
    responsive: true,
    maintainAspectRatio: false,
    onClick: onBar
      ? (_e: unknown, els: { index: number }[]) => {
          if (els.length) onBar(els[0].index);
        }
      : undefined,
    scales: {
      x: {
        ticks: { color: t.muted, callback: (v: number | string) => fmt(Number(v)) },
        grid: { color: t.border },
      },
      y: {
        ticks: { color: t.fg, font: { size: 11 } },
        grid: { display: false },
      },
    },
    plugins: {
      legend: { display: false },
      tooltip: {
        callbacks: {
          label: (ctx: { dataIndex: number }) => titles?.[ctx.dataIndex] ?? `${data[ctx.dataIndex].name} — ${fmt(data[ctx.dataIndex].value)}`,
        },
      },
    },
  };
  const height = Math.max(140, data.length * 26 + 40);
  return (
    <div key={themeKey} className="chart-wrap" role="img" aria-label="Horizontal bar chart" style={{ position: "relative", height, maxWidth: 720 }}>
      <ChartBar data={chartData} options={options} />
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
  const themeKey = useThemeKey();
  const t = themeColors();
  const chartData = {
    labels: data.map((d) => d.label),
    datasets: [
      {
        data: data.map((d) => d.value),
        backgroundColor: color(barColor ?? 0),
        borderRadius: 3,
      },
    ],
  };
  const options = {
    responsive: true,
    maintainAspectRatio: false,
    scales: {
      x: {
        ticks: { color: t.muted, font: { size: 10 } },
        grid: { display: false },
      },
      y: {
        min: 0,
        max: yMaxPct,
        ticks: { color: t.muted, callback: (v: number | string) => fmt(Number(v)) },
        grid: { color: t.border },
      },
    },
    plugins: {
      legend: { display: false },
      tooltip: {
        callbacks: {
          label: (ctx: { dataIndex: number }) => `${data[ctx.dataIndex].label}: ${fmt(data[ctx.dataIndex].value)}`,
        },
      },
    },
  };
  return (
    <div key={themeKey} className="chart-wrap" role="img" aria-label="Bar chart" style={{ position: "relative", height: 200, maxWidth: 720 }}>
      <ChartBar data={chartData} options={options} />
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
  const titles = top.map((row) => `${row.name} — ${formatBytes(row.value)}`);
  return <HBar data={top} fmt={formatBytes} titles={titles} />;
}

export function LoaderRollupChart({ data }: { data: LoaderRollup[] }) {
  if (data.length === 0) return null;
  const rows: Slice[] = data.map((r) => ({
    name: shortLoader(r.loader_label) ?? `loader@${r.loader_id}`,
    value: r.retained,
  }));
  const titles = data.map(
    (r) => `${shortLoader(r.loader_label) ?? `loader@${r.loader_id}`} — ${fmtCount(r.class_count)} classes, ${formatBytes(r.retained)} retained`,
  );
  return <HBar data={rows} fmt={formatBytes} barColor={4} titles={titles} />;
}

export function LeakShareChart({ suspects, total }: { suspects: Suspect[]; total: number }) {
  if (suspects.length === 0 || total <= 0) return null;
  const rows: Slice[] = suspects.map((s) => ({ name: s.pretty_class, value: s.retained }));
  const sum = suspects.reduce((s, x) => s + x.retained, 0);
  if (total > sum) rows.push({ name: "(remainder)", value: total - sum });
  const titles = rows.map((row) => `${row.name} — ${formatBytes(row.value)} (${((row.value / total) * 100).toFixed(1)}%)`);
  return (
    <Pie
      data={rows}
      fmt={formatBytes}
      titles={titles}
      onSlice={(i) => {
        if (i < suspects.length) {
          document.getElementById(`suspect-${i + 1}`)?.scrollIntoView({ behavior: "smooth", block: "center" });
        }
      }}
    />
  );
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
  // Summary: smallest depth holding a cumulative 50% of objects, plus the
  // deepest bucket. Derived here from the counts (not carried in the model).
  const total = data.reduce((s, b) => s + b.objects, 0);
  let running = 0;
  let median = data[data.length - 1].depth;
  for (const b of data) {
    running += b.objects;
    if (running * 2 >= total) {
      median = b.depth;
      break;
    }
  }
  const maxDepth = data[data.length - 1].depth;
  return (
    <>
      <VBar data={bars} fmt={fmtCount} barColor={4} />
      <p className="subtitle" style={{ marginTop: "0.4rem" }}>
        Half of all live objects sit within {median} hop{median === 1 ? "" : "s"} of a GC root; the deepest chain is{" "}
        {maxDepth} hop{maxDepth === 1 ? "" : "s"}.
      </p>
    </>
  );
}


export function GcRootsChart({ data }: { data: GcRootTypeRow[] }) {
  if (data.length < 2) return null;
  return <HBar data={data.map((r) => ({ name: r.root_type, value: r.count }))} fmt={fmtCount} barColor={2} />;
}

// ── Stacked horizontal bar ───────────────────────────────────────────────────
function StackedBar({ segments, fmt }: {
  segments: { label: string; value: number; colorIdx?: number }[];
  fmt: (n: number) => string;
}) {
  const total = segments.reduce((s, x) => s + x.value, 0);
  if (total <= 0) return null;
  const themeKey = useThemeKey();
  const t = themeColors();
  const chartData = {
    labels: [""],
    datasets: segments.map((s, i) => ({
      label: s.label,
      data: [s.value],
      backgroundColor: color(s.colorIdx ?? i),
    })),
  };
  const options = {
    indexAxis: "y" as const,
    responsive: true,
    maintainAspectRatio: false,
    scales: {
      x: {
        stacked: true,
        ticks: { color: t.muted, callback: (v: number | string) => fmt(Number(v)) },
        grid: { color: t.border },
      },
      y: {
        stacked: true,
        ticks: { display: false },
        grid: { display: false },
      },
    },
    plugins: {
      legend: {
        display: true,
        position: "bottom" as const,
        labels: { color: t.fg, boxWidth: 12, font: { size: 12 } },
      },
      tooltip: {
        callbacks: {
          label: (ctx: { dataset: { label?: string }; parsed: { x: number } }) =>
            `${ctx.dataset.label}: ${fmt(ctx.parsed.x)} (${((ctx.parsed.x / total) * 100).toFixed(1)}%)`,
        },
      },
    },
  };
  return (
    <div key={themeKey} className="chart-wrap" role="img" aria-label="Stacked bar chart" style={{ position: "relative", height: 90, maxWidth: 720 }}>
      <ChartBar data={chartData} options={options} />
    </div>
  );
}

export function CompositionStackedBar({ data }: { data: KindStat[] }) {
  if (data.length < 2) return null;
  return <StackedBar segments={data.map((k) => ({ label: k.kind, value: k.shallow_heap }))} fmt={formatBytes} />;
}

export function ConcentrationStackedBar({ rc }: { rc: RetentionSummary }) {
  const top1 = rc.top1_bp;
  const next9 = Math.max(0, rc.top10_bp - rc.top1_bp);
  const next90 = Math.max(0, rc.top100_bp - rc.top10_bp);
  const rest = Math.max(0, 10000 - rc.top100_bp);
  if (rc.top1_bp === 0 && rc.top10_bp === 0 && rc.top100_bp === 0) return null;
  const fmtPct = (bp: number) => `${(bp / 100).toFixed(1)}%`;
  return (
    <StackedBar
      segments={[
        { label: "Top 1", value: top1, colorIdx: 3 },
        { label: "Next 9", value: next9, colorIdx: 2 },
        { label: "Next 90", value: next90, colorIdx: 0 },
        { label: "Rest of heap", value: rest, colorIdx: 10 },
      ]}
      fmt={fmtPct}
    />
  );
}

// ── Package treemap-lite bar ─────────────────────────────────────────────────
export function TreemapBar({ root, onSelect }: { root: PackageNode; onSelect: (idx: number) => void }) {
  const children = root.children;
  if (children.length === 0) return null;
  const N = 12;
  const head = children.slice(0, N);
  const segs = head.map((c, i) => ({ name: c.name || "(default package)", value: c.retained_heap, idx: i }));
  if (children.length > N) {
    const rest = children.slice(N).reduce((s, c) => s + c.retained_heap, 0);
    if (rest > 0) segs.push({ name: "(rest)", value: rest, idx: -1 });
  }
  const total = segs.reduce((s, x) => s + x.value, 0);
  if (total <= 0) return null;
  return (
    <div className="chart-wrap">
      <div style={{ display: "flex", width: "100%", height: 28, borderRadius: 4, overflow: "hidden", border: "1px solid var(--border)" }}>
        {segs.map((s, i) => {
          const pct = (s.value / total) * 100;
          if (pct <= 0) return null;
          const clickable = s.idx !== -1;
          return (
            <div
              key={i}
              onClick={clickable ? () => onSelect(s.idx) : undefined}
              title={`${s.name}: ${formatBytes(s.value)} (${pct.toFixed(1)}%)`}
              style={{ width: `${pct}%`, background: color(i), cursor: clickable ? "pointer" : "default" }}
            />
          );
        })}
      </div>
      <ul style={{ listStyle: "none", padding: 0, margin: "0.4rem 0 0", display: "flex", flexWrap: "wrap", gap: "0.75rem", fontSize: "0.8rem" }}>
        {segs.map((s, i) => (
          <li key={i} style={{ display: "flex", alignItems: "center", gap: "0.35rem" }}>
            <span style={{ width: 12, height: 12, background: color(i), display: "inline-block", borderRadius: 2 }} />
            <span
              onClick={s.idx !== -1 ? () => onSelect(s.idx) : undefined}
              style={{ cursor: s.idx !== -1 ? "pointer" : "default" }}
            >
              {s.name} — {formatBytes(s.value)} ({((s.value / total) * 100).toFixed(1)}%)
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}

// ── Retained-Heap Treemap ────────────────────────────────────────────────────
// Squarified treemap of the package tree from report.top.biggest_packages.
// Uses d3-hierarchy for layout; renders with absolute-positioned divs (no SVG).
const TREEMAP_W = 700;
const TREEMAP_H = 420;

export function RetainedTreemap({ root }: { root: PackageNode }) {
  const [tooltip, setTooltip] = React.useState<{
    name: string;
    retained: number;
    x: number;
    y: number;
  } | null>(null);

  const nodes = React.useMemo(() => {
    const h = hierarchy<PackageNode>(root, (d) => d.children)
      .sum((d) => (d.children && d.children.length > 0 ? 0 : d.retained_heap))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));

    const layout = treemap<PackageNode>()
      .tile(treemapSquarify)
      .size([TREEMAP_W, TREEMAP_H])
      .paddingOuter(2)
      .paddingInner(1);

    layout(h);
    return h.leaves();
  }, [root]);

  // Assign colors by top-level package (depth-1 ancestor).
  const topLevelNames = React.useMemo(() => {
    const seen = new Map<string, number>();
    for (const leaf of nodes) {
      const topName = leaf.ancestors().slice(-2)[0]?.data.name ?? leaf.data.name;
      if (!seen.has(topName)) seen.set(topName, seen.size);
    }
    return seen;
  }, [nodes]);

  const totalRetained = root.retained_heap || 1;

  return (
    <div style={{ position: "relative", width: TREEMAP_W, height: TREEMAP_H, overflow: "hidden" }}>
      {nodes.map((leaf, i) => {
        const x0 = (leaf as any).x0 as number;
        const y0 = (leaf as any).y0 as number;
        const x1 = (leaf as any).x1 as number;
        const y1 = (leaf as any).y1 as number;
        const w = x1 - x0;
        const h = y1 - y0;
        if (w < 1 || h < 1) return null;
        const topName = leaf.ancestors().slice(-2)[0]?.data.name ?? leaf.data.name;
        const colorIdx = topLevelNames.get(topName) ?? 0;
        const leafColor = PALETTE[colorIdx % PALETTE.length];
        const label = leaf.data.name;
        const retained = leaf.data.retained_heap;
        return (
          <div
            key={i}
            title={`${label}: ${formatBytes(retained)} (${((retained / totalRetained) * 100).toFixed(1)}%)`}
            onMouseEnter={(e) => {
              const rect = (e.currentTarget.closest("[data-treemap]") as HTMLElement | null)?.getBoundingClientRect();
              setTooltip({ name: label, retained, x: x0 + w / 2, y: y0 });
            }}
            onMouseLeave={() => setTooltip(null)}
            style={{
              position: "absolute",
              left: x0,
              top: y0,
              width: w,
              height: h,
              background: leafColor,
              opacity: 0.82,
              boxSizing: "border-box",
              overflow: "hidden",
              cursor: "default",
            }}
          >
            {w > 40 && h > 20 && (
              <span
                style={{
                  display: "block",
                  padding: "2px 3px",
                  fontSize: Math.min(11, w / 8),
                  color: "#fff",
                  whiteSpace: "nowrap",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                }}
              >
                {label}
              </span>
            )}
          </div>
        );
      })}
      {tooltip && (
        <div
          style={{
            position: "absolute",
            left: Math.min(tooltip.x, TREEMAP_W - 160),
            top: Math.max(0, tooltip.y - 36),
            background: "rgba(0,0,0,0.8)",
            color: "#fff",
            padding: "4px 8px",
            borderRadius: 4,
            fontSize: 12,
            pointerEvents: "none",
            whiteSpace: "nowrap",
            zIndex: 10,
          }}
        >
          <strong>{tooltip.name}</strong>
          <br />
          {formatBytes(tooltip.retained)} ({((tooltip.retained / totalRetained) * 100).toFixed(1)}%)
        </div>
      )}
    </div>
  );
}
