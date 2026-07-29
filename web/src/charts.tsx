import React from "react";
import { hierarchy, treemap, treemapSquarify } from "d3-hierarchy";
import type {
  DepthBucket,
  GcRootTypeRow,
  HistRow,
  KindStat,
  LoaderRollup,
  PackageNode,
  QueryColumn,
  QueryResult,
  QueryValue,
  RetentionSummary,
  SeriesClassRow,
  Suspect,
  VizSpec,
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

// ── FlatTreemap — lightweight squarify treemap for flat slice data ───────────
// Used in place of pie charts: shows proportions + labels without wasted space.
function FlatTreemap({
  data, fmt, height = 220, onSlice,
}: {
  data: Slice[]; fmt: (n: number) => string; height?: number; onSlice?: (i: number) => void;
}) {
  const ref = React.useRef<HTMLDivElement>(null);
  const [w, setW] = React.useState(600);
  React.useLayoutEffect(() => {
    if (!ref.current) return;
    const ro = new ResizeObserver((entries) => {
      const bw = entries[0]?.contentRect.width;
      if (bw && bw > 0) setW(Math.floor(bw));
    });
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, []);

  const positive = data.filter((d) => d.value > 0);
  const total = positive.reduce((s, d) => s + d.value, 0) || 1;

  const nodes = React.useMemo(() => {
    if (positive.length === 0 || w < 10) return [];
    const root = hierarchy<{ name: string; value: number; children?: unknown[] }>(
      { name: "", value: 0, children: positive },
      (d) => d.children as { name: string; value: number }[] | undefined,
    )
      .sum((d) => (d.children ? 0 : d.value))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));
    treemap<{ name: string; value: number }>()
      .tile(treemapSquarify)
      .size([w, height])
      .paddingOuter(2)
      .paddingInner(1)(root as never);
    return root.leaves();
  }, [positive, w, height]);

  if (nodes.length === 0) return null;

  return (
    <div className="chart-wrap" ref={ref}>
      <div style={{ position: "relative", width: "100%", height, overflow: "hidden" }}>
        {nodes.map((leaf, i) => {
          const x0 = (leaf as any).x0 as number;
          const y0 = (leaf as any).y0 as number;
          const x1 = (leaf as any).x1 as number;
          const y1 = (leaf as any).y1 as number;
          const lw = x1 - x0;
          const lh = y1 - y0;
          if (lw < 1 || lh < 1) return null;
          const label = (leaf.data as { name: string }).name;
          const value = leaf.value ?? 0;
          const pct = ((value / total) * 100).toFixed(1);
          const origIdx = data.findIndex((d) => d.name === label);
          const clickable = onSlice != null && origIdx !== -1 && origIdx < data.length - (data.length > positive.length ? 0 : 0);
          return (
            <div
              key={i}
              title={`${label}: ${fmt(value)} (${pct}%)`}
              onClick={clickable ? () => onSlice!(origIdx) : undefined}
              style={{
                position: "absolute",
                left: x0, top: y0, width: lw, height: lh,
                background: PALETTE[i % PALETTE.length],
                opacity: 0.85,
                boxSizing: "border-box",
                overflow: "hidden",
                cursor: clickable ? "pointer" : "default",
              }}
            >
              {lw > 44 && lh > 22 && (
                <span style={{
                  display: "block", padding: "2px 4px",
                  fontSize: Math.min(12, lw / 7),
                  color: "#fff", whiteSpace: "nowrap",
                  overflow: "hidden", textOverflow: "ellipsis",
                }}>
                  {label}
                </span>
              )}
              {lw > 44 && lh > 38 && (
                <span style={{
                  display: "block", padding: "0 4px",
                  fontSize: Math.min(11, lw / 8),
                  color: "rgba(255,255,255,0.8)", whiteSpace: "nowrap",
                  overflow: "hidden", textOverflow: "ellipsis",
                }}>
                  {fmt(value)} ({pct}%)
                </span>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

// Export for use outside the bundle (shell.js /viz command and dashboard)
export { FlatTreemap };

// ── ZoomableTreemap — interactive squarify treemap with drill-down + flame view ──
// Generic over node type T. Clicking a tile with children zooms into that subtree;
// a breadcrumb bar lets users navigate back up. A toggle switches to flamegraph view.

function buildColorMap<T>(root: T, getChildren: (n: T) => T[], getLabel: (n: T) => string): Map<string, number> {
  const map = new Map<string, number>();
  getChildren(root).forEach((c, i) => map.set(getLabel(c), i));
  return map;
}

function findAncestorLabel<T>(
  node: T,
  getChildren: (n: T) => T[],
  getLabel: (n: T) => string,
  target: T,
  depth: number,
): string | null {
  if (node === target) return getLabel(node);
  if (depth === 0) return null;
  for (const c of getChildren(node)) {
    if (c === target || findDescendant(c, getChildren, target)) {
      return getLabel(c);
    }
  }
  return null;
}

function findDescendant<T>(node: T, getChildren: (n: T) => T[], target: T): boolean {
  for (const c of getChildren(node)) {
    if (c === target || findDescendant(c, getChildren, target)) return true;
  }
  return false;
}

// For a node in the tree, find the label of the top-level child of `root` that
// is an ancestor of `node` (or `node` itself if it IS a top-level child).
function topLevelAncestorLabel<T>(
  root: T,
  getChildren: (n: T) => T[],
  getLabel: (n: T) => string,
  node: T,
): string {
  const topChildren = getChildren(root);
  for (const c of topChildren) {
    if (c === node || findDescendant(c, getChildren, node)) return getLabel(c);
  }
  return getLabel(node);
}

// Build flat levels for the flamegraph: each level is an array of { node, pct, color }
interface FlameCell<T> { node: T; pct: number; colorIdx: number; }

function buildFlameLevels<T>(
  currentNode: T,
  getChildren: (n: T) => T[],
  getValue: (n: T) => number,
  colorMap: Map<string, number>,
  getLabel: (n: T) => string,
  root: T,
  maxDepth = 8,
): FlameCell<T>[][] {
  const levels: FlameCell<T>[][] = [];
  const totalValue = getValue(currentNode);
  if (totalValue <= 0) return levels;

  // Level 0: the current node itself (full width)
  const rootColorIdx = colorMap.get(topLevelAncestorLabel(root, getChildren, getLabel, currentNode)) ?? 0;
  levels.push([{ node: currentNode, pct: 100, colorIdx: rootColorIdx }]);

  // Subsequent levels: children proportional to their parent's fraction of total
  // We track segments: { node, startPct, widthPct } then resolve children
  type Seg = { node: T; startPct: number; widthPct: number };
  let currentSegs: Seg[] = [{ node: currentNode, startPct: 0, widthPct: 100 }];

  for (let d = 0; d < maxDepth - 1; d++) {
    const nextSegs: Seg[] = [];
    const level: FlameCell<T>[] = [];
    for (const seg of currentSegs) {
      const kids = [...getChildren(seg.node)].sort((a, b) => getValue(b) - getValue(a));
      const kidTotal = kids.reduce((s, k) => s + getValue(k), 0);
      if (kidTotal <= 0 || kids.length === 0) continue;
      let cursor = seg.startPct;
      for (const kid of kids) {
        const kidPct = (getValue(kid) / kidTotal) * seg.widthPct;
        if (kidPct < 0.1) continue;
        const ci = colorMap.get(topLevelAncestorLabel(root, getChildren, getLabel, kid)) ?? 0;
        level.push({ node: kid, pct: kidPct, colorIdx: ci });
        nextSegs.push({ node: kid, startPct: cursor, widthPct: kidPct });
        cursor += kidPct;
      }
    }
    if (level.length === 0) break;
    levels.push(level);
    currentSegs = nextSegs;
  }
  return levels;
}

export function ZoomableTreemap<T>({
  root,
  getChildren,
  getValue,
  getLabel,
  fmt,
  height = 320,
  renderLeaf,
  extraLeaves,
}: {
  root: T;
  getChildren: (n: T) => T[];
  getValue: (n: T) => number;
  getLabel: (n: T) => string;
  fmt: (n: number) => string;
  height?: number;
  renderLeaf?: (node: T, pathLabels: string[]) => React.ReactNode;
  /** Extra non-navigable tiles to mix into the treemap alongside real children (e.g. direct classes). */
  extraLeaves?: (node: T, pathLabels: string[]) => { label: string; value: number }[];
}) {
  const [path, setPath] = React.useState<T[]>([]);
  const [mode, setMode] = React.useState<"treemap" | "flame">("treemap");
  const ref = React.useRef<HTMLDivElement>(null);
  const [w, setW] = React.useState(600);

  React.useLayoutEffect(() => {
    if (!ref.current) return;
    const ro = new ResizeObserver((entries) => {
      const bw = entries[0]?.contentRect.width;
      if (bw && bw > 0) setW(Math.floor(bw));
    });
    ro.observe(ref.current);
    return () => ro.disconnect();
  }, []);

  // Reset zoom when root changes
  React.useEffect(() => { setPath([]); }, [root]);

  const currentNode = path.length > 0 ? path[path.length - 1] : root;
  const children = getChildren(currentNode).filter((c) => getValue(c) > 0);
  // Build the dotted package path for the current node (skip root's empty label)
  const pathLabels = path.map((n) => getLabel(n)).filter(Boolean);

  // Color map: keyed by top-level-child label of the ORIGINAL root (stable across zooms)
  const colorMap = React.useMemo(
    () => buildColorMap(root, getChildren, getLabel),
    [root],
  );

  const getColor = (node: T) => {
    const lbl = topLevelAncestorLabel(root, getChildren, getLabel, node);
    return PALETTE[(colorMap.get(lbl) ?? 0) % PALETTE.length];
  };

  // d3 treemap layout for treemap mode — includes both sub-package children and extra class tiles
  const extras = React.useMemo(
    () => extraLeaves ? extraLeaves(currentNode, pathLabels) : [],
    [extraLeaves, currentNode, pathLabels],
  );
  const nodes = React.useMemo(() => {
    const hasAny = children.length > 0 || extras.length > 0;
    if (!hasAny || w < 10) return [];
    type Leaf = { node: T | null; extra: { label: string; value: number } | null; value: number };
    const leaves: Leaf[] = [
      ...children.map((c) => ({ node: c, extra: null, value: getValue(c) })),
      ...extras.map((e) => ({ node: null, extra: e, value: e.value })),
    ];
    const hierarchyRoot = hierarchy<{ node: T | null; extra: { label: string; value: number } | null; value: number; children?: Leaf[] }>(
      { node: null, extra: null, value: 0, children: leaves },
      (d) => d.children,
    )
      .sum((d) => (d.children ? 0 : d.value))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));
    treemap<{ node: T | null; extra: { label: string; value: number } | null; value: number }>()
      .tile(treemapSquarify)
      .size([w, height])
      .paddingOuter(2)
      .paddingInner(1)(hierarchyRoot as never);
    return hierarchyRoot.leaves() as unknown as (ReturnType<typeof hierarchy> & { x0: number; y0: number; x1: number; y1: number; data: Leaf })[];
  }, [children, extras, w, height]);

  const total = (children.reduce((s, c) => s + getValue(c), 0) + extras.reduce((s, e) => s + e.value, 0)) || 1;

  // Flame levels
  const flameLevels = React.useMemo(
    () => mode === "flame" ? buildFlameLevels(currentNode, getChildren, getValue, colorMap, getLabel, root) : [],
    [mode, currentNode, getChildren, getValue, colorMap, getLabel, root],
  );

  const zoomTo = (node: T) => {
    setPath((prev) => [...prev, node]);
  };

  const crumbs = [root, ...path];

  if (children.length === 0 && path.length === 0) return null;

  return (
    <div className="chart-wrap" ref={ref}>
      {/* Breadcrumb toolbar */}
      <div className="zm-toolbar">
        {crumbs.map((crumb, i) => {
          const isCurrent = i === crumbs.length - 1;
          const label = i === 0 ? (getLabel(crumb) || "⬛ root") : getLabel(crumb);
          return (
            <React.Fragment key={i}>
              {i > 0 && <span className="zm-sep">›</span>}
              {isCurrent ? (
                <span className="zm-crumb-cur">{label}</span>
              ) : (
                <button className="zm-crumb" onClick={() => setPath(path.slice(0, i))}>
                  {label}
                </button>
              )}
            </React.Fragment>
          );
        })}
        <span className="zm-spacer" />
        <button className={`zm-mode-btn${mode === "treemap" ? " active" : ""}`} onClick={() => setMode("treemap")} title="Squarify treemap view">
          ⬛ Treemap
        </button>
        <button className={`zm-mode-btn${mode === "flame" ? " active" : ""}`} onClick={() => setMode("flame")} title="Flamegraph (icicle) view">
          🔥 Flame
        </button>
      </div>

      {/* Treemap mode */}
      {mode === "treemap" && (
        <div style={{ position: "relative", width: "100%", height, overflow: "hidden" }}>
          {nodes.map((leaf, i) => {
            const { x0, y0, x1, y1, data: ld } = leaf;
            const lw = x1 - x0;
            const lh = y1 - y0;
            if (lw < 1 || lh < 1) return null;
            // Extra (class) tile
            if (ld.extra !== null) {
              const val = ld.extra.value;
              const pct = ((val / total) * 100).toFixed(1);
              const bg = getColor(currentNode);
              return (
                <div
                  key={`x${i}`}
                  title={`${ld.extra.label}: ${fmt(val)} (${pct}%) — class`}
                  style={{
                    position: "absolute", left: x0, top: y0, width: lw, height: lh,
                    background: bg, opacity: 0.55, boxSizing: "border-box", overflow: "hidden",
                    cursor: "default",
                    border: "1px dashed rgba(255,255,255,0.3)",
                  }}
                >
                  {lw > 44 && lh > 18 && (
                    <span style={{ display: "block", padding: "2px 4px", fontSize: Math.min(11, lw / 7), color: "#fff", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                      {ld.extra.label}
                    </span>
                  )}
                  {lw > 44 && lh > 34 && (
                    <span style={{ display: "block", padding: "0 4px", fontSize: Math.min(10, lw / 8), color: "rgba(255,255,255,0.75)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                      {fmt(val)}
                    </span>
                  )}
                </div>
              );
            }
            // Sub-package tile
            const node = ld.node as T;
            const val = ld.value;
            const pct = ((val / total) * 100).toFixed(1);
            const hasKids = getChildren(node).filter((c) => getValue(c) > 0).length > 0;
            const isClickable = hasKids || !!renderLeaf;
            const bg = getColor(node);
            return (
              <div
                key={i}
                title={`${getLabel(node)}: ${fmt(val)} (${pct}%)${hasKids ? " — click to drill in" : isClickable ? " — click to see classes" : ""}`}
                onClick={isClickable ? () => zoomTo(node) : undefined}
                style={{
                  position: "absolute", left: x0, top: y0, width: lw, height: lh,
                  background: bg, opacity: 0.87, boxSizing: "border-box", overflow: "hidden",
                  cursor: isClickable ? (hasKids ? "zoom-in" : "pointer") : "default",
                  border: isClickable ? "1px solid rgba(255,255,255,0.25)" : "none",
                }}
              >
                {lw > 44 && lh > 22 && (
                  <span style={{ display: "block", padding: "2px 4px", fontSize: Math.min(12, lw / 7), color: "#fff", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                    {getLabel(node)}{hasKids ? " ›" : (isClickable ? " ≡" : "")}
                  </span>
                )}
                {lw > 44 && lh > 38 && (
                  <span style={{ display: "block", padding: "0 4px", fontSize: Math.min(11, lw / 8), color: "rgba(255,255,255,0.8)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                    {fmt(val)} ({pct}%)
                  </span>
                )}
              </div>
            );
          })}
          {children.length === 0 && extras.length === 0 && (
            <div style={{ display: "flex", alignItems: "center", justifyContent: "center", height: "100%", color: "var(--muted)", fontSize: "0.9rem" }}>
              No sub-packages
            </div>
          )}
          {children.length === 0 && (extras.length > 0 || nodes.length === 0) && extras.length === 0 && (
            // Pure leaf with no children and no extras: single full-size tile
            <div
              style={{
                position: "absolute", left: 0, top: 0, width: "100%", height: "100%",
                background: getColor(currentNode), opacity: 0.87, boxSizing: "border-box",
                overflow: "hidden",
              }}
            >
              {w > 44 && height > 22 && (
                <span style={{ display: "block", padding: "2px 4px", fontSize: 12, color: "#fff", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {getLabel(currentNode)}
                </span>
              )}
              {w > 44 && height > 38 && (
                <span style={{ display: "block", padding: "0 4px", fontSize: 11, color: "rgba(255,255,255,0.8)", whiteSpace: "nowrap", overflow: "hidden", textOverflow: "ellipsis" }}>
                  {fmt(getValue(currentNode))}
                </span>
              )}
            </div>
          )}
        </div>
      )}

      {/* Flamegraph (icicle) mode */}
      {mode === "flame" && (
        <div className="flame-container" style={{ maxHeight: height + 40 }}>
          {flameLevels.map((level, lvl) => (
            <div key={lvl} className="flame-level">
              {level.map((cell, ci) => {
                const hasKids = getChildren(cell.node).filter((c) => getValue(c) > 0).length > 0;
                const isClickable = lvl > 0 && (hasKids || !!renderLeaf);
                const val = getValue(cell.node);
                const pct = ((val / getValue(currentNode)) * 100).toFixed(1);
                return (
                  <div
                    key={ci}
                    className={`flame-cell${!isClickable ? " flame-cell-leaf" : ""}`}
                    style={{ width: `${cell.pct}%`, background: PALETTE[cell.colorIdx % PALETTE.length] }}
                    title={`${getLabel(cell.node)}: ${fmt(val)} (${pct}%)${hasKids && lvl > 0 ? " — click to drill in" : isClickable ? " — click to see classes" : ""}`}
                    onClick={isClickable ? () => zoomTo(cell.node) : undefined}
                  >
                    <span className="flame-label">{getLabel(cell.node)}</span>
                  </div>
                );
              })}
            </div>
          ))}
          {extras.length > 0 && (
            <div className="flame-level">
              {extras.map((e, ci) => {
                const pct = ((e.value / (getValue(currentNode) || 1)) * 100).toFixed(1);
                const bg = getColor(currentNode);
                return (
                  <div
                    key={ci}
                    className="flame-cell flame-cell-leaf"
                    style={{ width: `${((e.value / (getValue(currentNode) || 1)) * 100)}%`, background: bg, opacity: 0.6 }}
                    title={`${e.label}: ${fmt(e.value)} (${pct}%) — class`}
                  >
                    <span className="flame-label">{e.label}</span>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      )}

      {/* Classes in this package (shown at any level when renderLeaf is provided) */}
      {renderLeaf && renderLeaf(currentNode, pathLabels)}
    </div>
  );
}


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
  return <FlatTreemap data={data.map((k) => ({ name: k.kind, value: k.shallow_heap }))} fmt={formatBytes} height={180} />;
}

export function TopClassesChart({ data, totalRetained }: { data: HistRow[]; totalRetained?: number }) {
  if (data.length === 0) return null;
  const total = totalRetained ?? data.reduce((s, r) => s + r.retained, 0);
  // Show classes with >= 1% retained heap; always include top 2 so there's always something
  const threshold = total * 0.01;
  const significant = data.filter((r) => r.retained >= threshold);
  const shown = significant.length >= 2 ? significant : data.slice(0, 2);
  const rest = data.filter((r) => !shown.includes(r)).reduce((s, r) => s + r.retained, 0);
  const slices: Slice[] = shown.map((r) => ({ name: r.pretty_class, value: r.retained }));
  if (rest > 0) slices.push({ name: "(rest)", value: rest });
  return <FlatTreemap data={slices} fmt={formatBytes} height={220} />;
}

export function LoaderRollupChart({ data }: { data: LoaderRollup[] }) {
  if (data.length === 0) return null;
  const rows: Slice[] = data.map((r) => ({
    name: shortLoader(r.loader_label) ?? `loader@${r.loader_id}`,
    value: r.retained,
  }));
  return <FlatTreemap data={rows} fmt={formatBytes} height={180} />;
}

export function LeakShareChart({ suspects, total, onSlice }: { suspects: Suspect[]; total: number; onSlice?: (i: number) => void }) {
  if (suspects.length === 0 || total <= 0) return null;
  const rows: Slice[] = suspects.map((s) => ({ name: s.pretty_class, value: s.retained }));
  const sum = suspects.reduce((s, x) => s + x.retained, 0);
  if (total > sum) rows.push({ name: "(remainder)", value: total - sum });
  return <FlatTreemap data={rows} fmt={formatBytes} height={220} onSlice={onSlice} />;
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
  return <FlatTreemap data={data.map((r) => ({ name: r.root_type, value: r.count }))} fmt={fmtCount} height={180} />;
}

export function GcRootsRetainedChart({ data }: { data: { root_type: string; count: number; retained: number }[] }) {
  if (data.length < 2 || data.every((r) => r.retained === 0)) return null;
  return <FlatTreemap data={data.map((r) => ({ name: r.root_type, value: r.retained }))} fmt={formatBytes} height={180} />;
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

// ── Custom-query visualization (OQL `-- @viz` directive) ─────────────────────
// Renders a QueryResult's chart per its resolved VizSpec, reusing Pie/HBar and a
// flat d3 treemap. Mirrors the column resolution in src/query/viz.rs; the Rust
// side only attaches `viz` when resolution already succeeded, but we resolve
// defensively and fall back to `null` (the paired table stays visible in App).

function qvNum(v: QueryValue | undefined): number | null {
  if (!v) return null;
  return v.kind === "int" || v.kind === "float" ? v.v : null;
}

function qvLabel(v: QueryValue | undefined): string {
  if (!v) return "(null)";
  switch (v.kind) {
    case "null":
      return "(null)";
    case "bool":
    case "int":
    case "float":
      return String(v.v);
    case "str":
      return v.v;
    case "obj_ref":
      return `${v.v.class}@${v.v.index}`;
  }
}

function qvColMatch(colName: string, want: string): boolean {
  const strip = (s: string) => (s.startsWith("@") ? s.slice(1) : s);
  return strip(colName).toLowerCase() === strip(want).toLowerCase();
}

function qvColumnIsNumeric(idx: number, rows: QueryValue[][]): boolean {
  let sawNumber = false;
  for (const row of rows) {
    const cell = row[idx];
    if (!cell || cell.kind === "null") continue;
    if (cell.kind === "int" || cell.kind === "float") sawNumber = true;
    else return false;
  }
  return sawNumber;
}

// Returns [labelIdx, valueIdx] or null when the query cannot be charted.
function qvResolveColumns(spec: VizSpec, columns: QueryColumn[], rows: QueryValue[][]): [number, number] | null {
  if (columns.length === 0) return null;
  let valueIdx: number;
  if (spec.value_col) {
    const i = columns.findIndex((c) => qvColMatch(c.name, spec.value_col!));
    if (i < 0) return null;
    valueIdx = i;
  } else {
    const i = columns.findIndex((_, ci) => qvColumnIsNumeric(ci, rows));
    if (i < 0) return null;
    valueIdx = i;
  }
  if (!qvColumnIsNumeric(valueIdx, rows)) return null;

  let labelIdx: number;
  if (spec.label_col) {
    const i = columns.findIndex((c) => qvColMatch(c.name, spec.label_col!));
    if (i < 0) return null;
    labelIdx = i;
  } else {
    const i = columns.findIndex((_, ci) => ci !== valueIdx);
    if (i < 0) return null;
    labelIdx = i;
  }
  return [labelIdx, valueIdx];
}

// Flat (single-level) treemap for arbitrary label/value slices.
function QueryTreemap({ data }: { data: Slice[] }) {
  const positive = data.filter((d) => d.value > 0);
  const nodes = React.useMemo(() => {
    if (positive.length === 0) return [];
    const root = hierarchy<{ name: string; value: number; children?: unknown[] }>(
      { name: "", value: 0, children: positive },
      (d) => d.children as { name: string; value: number }[] | undefined,
    )
      .sum((d) => (d.children ? 0 : d.value))
      .sort((a, b) => (b.value ?? 0) - (a.value ?? 0));
    treemap<{ name: string; value: number }>()
      .tile(treemapSquarify)
      .size([TREEMAP_W, TREEMAP_H])
      .paddingOuter(2)
      .paddingInner(1)(root as never);
    return root.leaves();
  }, [positive]);

  if (nodes.length === 0) return null;
  const total = positive.reduce((s, d) => s + d.value, 0) || 1;
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
        const label = (leaf.data as { name: string }).name;
        const value = leaf.value ?? 0;
        return (
          <div
            key={i}
            title={`${label}: ${value} (${((value / total) * 100).toFixed(1)}%)`}
            style={{
              position: "absolute",
              left: x0,
              top: y0,
              width: w,
              height: h,
              background: PALETTE[i % PALETTE.length],
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
    </div>
  );
}

export function QueryViz({ query }: { query: QueryResult }) {
  const spec = query.viz;
  if (!spec || spec.kind === "table") return null;
  const resolved = qvResolveColumns(spec, query.columns, query.rows);
  if (!resolved) return null;
  const [labelIdx, valueIdx] = resolved;

  let slices: Slice[] = [];
  for (const row of query.rows) {
    const value = qvNum(row[valueIdx]);
    if (value == null) continue;
    slices.push({ name: qvLabel(row[labelIdx]), value });
  }
  if (spec.cap != null) slices = slices.slice(0, spec.cap);
  if (slices.length === 0) return null;

  const fmt = (n: number) => String(n);
  let chart: React.ReactNode;
  switch (spec.kind) {
    case "piechart":
      chart = <Pie data={slices} fmt={fmt} />;
      break;
    case "treemap":
      chart = <QueryTreemap data={slices} />;
      break;
    case "histogram":
    default:
      chart = <HBar data={slices} fmt={fmt} />;
      break;
  }
  return (
    <>
      {spec.title && <h4>{spec.title}</h4>}
      {chart}
    </>
  );
}

// ── RetainedGrowthChart — horizontal bar chart for top growth leaders ─────────
export function RetainedGrowthChart({ rows }: { rows: SeriesClassRow[] }) {
  const themeKey = useThemeKey();
  const t = themeColors();
  if (rows.length === 0) return null;
  const top = rows.slice().sort((a, b) => Math.abs(b.delta_retained) - Math.abs(a.delta_retained)).slice(0, 10);
  const labels = top.map((r) => {
    const cls = r.pretty_class;
    return cls.length > 35 ? cls.slice(0, 34) + "…" : cls;
  });
  const values = top.map((r) => r.delta_retained);
  const bgColors = values.map((v) => v >= 0 ? "rgba(34,197,94,0.7)" : "rgba(239,68,68,0.7)");
  const chartData = {
    labels,
    datasets: [
      {
        data: values,
        backgroundColor: bgColors,
        borderRadius: 3,
      },
    ],
  };
  const options = {
    indexAxis: "y" as const,
    responsive: true,
    maintainAspectRatio: false,
    scales: {
      x: {
        ticks: { color: t.muted, callback: (v: number | string) => formatBytes(Math.abs(Number(v))) },
        grid: { color: t.border },
      },
      y: {
        ticks: { color: t.fg, font: { size: 11 } },
        grid: { display: false },
      },
    },
    plugins: {
      legend: { display: false },
      title: {
        display: true,
        text: "Top Retained Growth (Δ bytes)",
        color: t.fg,
        font: { size: 13 },
      },
      tooltip: {
        callbacks: {
          label: (ctx: { dataIndex: number }) => {
            const r = top[ctx.dataIndex];
            const sign = r.delta_retained >= 0 ? "+" : "−";
            return `${r.pretty_class} — ${sign}${formatBytes(Math.abs(r.delta_retained))}`;
          },
        },
      },
    },
  };
  const height = Math.min(10, top.length) * 32 + 60;
  return (
    <div key={themeKey} className="chart-wrap" role="img" aria-label="Retained growth bar chart" style={{ position: "relative", height, maxWidth: 720 }}>
      <ChartBar data={chartData} options={options} />
    </div>
  );
}
