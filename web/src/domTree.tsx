// Unreachable dominator-tree SVG visualization.
//
// Uses d3-dag `graphHierarchy` + `sugiyama` for layout of each garbage-root
// subtree. Nodes are boxes sized proportionally to retained heap, labeled with
// class + retained. Edges are straight lines between parent and child boxes.
// Each subtree is rendered independently in its own <svg>.

import React from "react";
import { graphHierarchy, sugiyama, coordGreedy, decrossDfs, layeringLongestPath } from "d3-dag";
import type { UnreachableGarbageRoot } from "./types";
import { formatBytes, fmtCount } from "./format";

// ── Layout constants ──────────────────────────────────────────────────────────
const NODE_W = 180;   // box width (px)
const NODE_H = 52;    // box height (px)
const H_GAP = 40;     // horizontal gap between nodes
const V_GAP = 60;     // vertical gap between layers
const FONT_SIZE = 11;
const PALETTE = [
  "#2563eb", "#16a34a", "#d97706", "#dc2626",
  "#7c3aed", "#0891b2", "#db2777", "#65a30d",
];

// ── Helpers ───────────────────────────────────────────────────────────────────

/** Assign a stable colour to a class name. */
function classColor(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (Math.imul(31, h) + name.charCodeAt(i)) | 0;
  return PALETTE[Math.abs(h) % PALETTE.length];
}

/** Truncate a class name to fit the box. */
function truncateClass(name: string, maxLen = 22): string {
  if (name.length <= maxLen) return name;
  const short = name.split(".").pop() ?? name;
  return short.length <= maxLen ? short : short.slice(0, maxLen - 1) + "…";
}

// ── d3-dag-based layout for one tree ─────────────────────────────────────────

// Wrapper that carries a stable string id alongside the UnreachableGarbageRoot
// data, so LayoutNode lookups by id stay O(1).
interface TaggedNode {
  id: string;
  node: UnreachableGarbageRoot;
  children: TaggedNode[];
}

function tagTree(node: UnreachableGarbageRoot, prefix: string): TaggedNode {
  return {
    id: prefix,
    node,
    children: node.children.map((c, i) => tagTree(c, `${prefix}.${i}`)),
  };
}

function countNodes(t: TaggedNode): number {
  return 1 + t.children.reduce((s, c) => s + countNodes(c), 0);
}

// If the tree exceeds MAX_LAYOUT_NODES, prune it to depth 1 (root + direct
// children only) to keep the SVG a reasonable width.
const MAX_LAYOUT_NODES = 25;

function pruneToDepth1(t: TaggedNode): TaggedNode {
  return { ...t, children: t.children.map((c) => ({ ...c, children: [] })) };
}

interface LayoutNode {
  id: string;
  x: number;
  y: number;
  data: UnreachableGarbageRoot;
}
interface LayoutEdge {
  source: string;
  target: string;
}

function layoutTree(root: UnreachableGarbageRoot): { nodes: LayoutNode[]; edges: LayoutEdge[]; width: number; height: number } {
  let tagged = tagTree(root, "root");
  if (countNodes(tagged) > MAX_LAYOUT_NODES) tagged = pruneToDepth1(tagged);

  if (tagged.children.length === 0) {
    return {
      nodes: [{ id: "root", x: NODE_W / 2, y: NODE_H / 2, data: root }],
      edges: [],
      width: NODE_W + H_GAP,
      height: NODE_H + V_GAP,
    };
  }

  try {
    // graphHierarchy traverses a root node via the `.children` accessor —
    // TaggedNode already has exactly that shape.
    const builder = graphHierarchy<TaggedNode>().children((d) => d.children);
    const dag = builder(tagged);

    const layout = sugiyama()
      .layering(layeringLongestPath())
      .decross(decrossDfs())
      .coord(coordGreedy())
      .nodeSize(() => [NODE_W + H_GAP, NODE_H + V_GAP]);

    const { width, height } = layout(dag as Parameters<typeof layout>[0]);

    const nodes: LayoutNode[] = [];
    for (const n of (dag as any).nodes()) {
      nodes.push({ id: n.data.id, x: n.x, y: n.y, data: n.data.node });
    }
    const edges: LayoutEdge[] = [];
    for (const link of (dag as any).links()) {
      edges.push({ source: link.source.data.id, target: link.target.data.id });
    }
    return { nodes, edges, width: Math.max(width, NODE_W + H_GAP), height: Math.max(height, NODE_H + V_GAP) };
  } catch {
    // Fallback: collect all nodes in BFS order and lay them out as a vertical chain.
    const allNodes: TaggedNode[] = [];
    const queue: TaggedNode[] = [tagged];
    while (queue.length) {
      const cur = queue.shift()!;
      allNodes.push(cur);
      queue.push(...cur.children);
    }
    const nodes: LayoutNode[] = allNodes.map((n, i) => ({
      id: n.id,
      x: NODE_W / 2 + H_GAP / 2,
      y: i * (NODE_H + V_GAP) + NODE_H / 2,
      data: n.node,
    }));
    const edges: LayoutEdge[] = allNodes
      .flatMap((n) => n.children.map((c) => ({ source: n.id, target: c.id })));
    return { nodes, edges, width: NODE_W + H_GAP * 2, height: allNodes.length * (NODE_H + V_GAP) };
  }
}

// ── Single-tree SVG ───────────────────────────────────────────────────────────

function GarbageRootTreeSvg({ root, maxRetained }: { root: UnreachableGarbageRoot; maxRetained: number }) {
  if (root.children.length === 0) return null;
  const { nodes, edges, width, height } = layoutTree(root);
  const PAD = 12;
  const svgW = width + PAD * 2;
  const svgH = height + PAD * 2;

  const byId = Object.fromEntries(nodes.map((n) => [n.id, n]));

  return (
    <svg
      width={svgW}
      height={svgH}
      viewBox={`0 0 ${svgW} ${svgH}`}
      style={{ display: "block", overflow: "visible" }}
      role="img"
      aria-label={`Garbage-root dominator subtree: ${root.pretty_class}`}
    >
      <g transform={`translate(${PAD},${PAD})`}>
        {/* Edges */}
        {edges.map((e, i) => {
          const src = byId[e.source];
          const tgt = byId[e.target];
          if (!src || !tgt) return null;
          const x1 = src.x;
          const y1 = src.y + NODE_H / 2;
          const x2 = tgt.x;
          const y2 = tgt.y - NODE_H / 2;
          const mx = (x1 + x2) / 2;
          const my = (y1 + y2) / 2;
          return (
            <path
              key={i}
              d={`M${x1},${y1} C${x1},${my} ${x2},${my} ${x2},${y2}`}
              fill="none"
              stroke="var(--border, #cbd5e1)"
              strokeWidth={1.5}
              markerEnd="url(#arrow)"
            />
          );
        })}
        {/* Arrowhead marker */}
        <defs>
          <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
            <path d="M0,0 L6,3 L0,6 Z" fill="var(--border, #94a3b8)" />
          </marker>
        </defs>
        {/* Nodes */}
        {nodes.map((node) => {
          const x = node.x - NODE_W / 2;
          const y = node.y - NODE_H / 2;
          const col = classColor(node.data.pretty_class);
          const barW = maxRetained > 0 ? Math.round((node.data.retained / maxRetained) * (NODE_W - 8)) : 0;
          return (
            <g key={node.id} transform={`translate(${x},${y})`}>
              <rect
                width={NODE_W}
                height={NODE_H}
                rx={5}
                fill="var(--surface, #f8fafc)"
                stroke={col}
                strokeWidth={2}
              />
              {/* Retained bar at bottom of box */}
              <rect x={4} y={NODE_H - 8} width={barW} height={4} rx={2} fill={col} opacity={0.6} />
              <text
                x={NODE_W / 2}
                y={16}
                textAnchor="middle"
                fontSize={FONT_SIZE}
                fontWeight="bold"
                fill={col}
                fontFamily="monospace"
              >
                {truncateClass(node.data.pretty_class)}
              </text>
              <text
                x={NODE_W / 2}
                y={30}
                textAnchor="middle"
                fontSize={FONT_SIZE - 1}
                fill="var(--text-muted, #64748b)"
                fontFamily="system-ui, sans-serif"
              >
                {formatBytes(node.data.retained)} retained
              </text>
              <text
                x={NODE_W / 2}
                y={43}
                textAnchor="middle"
                fontSize={FONT_SIZE - 2}
                fill="var(--text-muted, #94a3b8)"
                fontFamily="system-ui, sans-serif"
              >
                {fmtCount(node.data.objects)} objects
              </text>
            </g>
          );
        })}
      </g>
    </svg>
  );
}

// ── Public component ──────────────────────────────────────────────────────────

export function UnreachableDomTreeSection({ roots }: { roots: UnreachableGarbageRoot[] }) {
  if (roots.length === 0) return null;
  const maxRetained = roots.reduce((m, r) => Math.max(m, r.retained), 0);
  return (
    <>
      <h3>Garbage-Root Dominator Trees</h3>
      <p className="subtitle">
        Top garbage-root subtrees by retained heap — unreachable objects with no
        reachable predecessor. Each node shows retained heap within its subtree.
      </p>
      {roots.map((root, i) => (
        <details key={i} open={i < 3}>
          <summary>
            <strong>{root.pretty_class}</strong>
            {" — "}{formatBytes(root.retained)} retained, {fmtCount(root.objects)} objects
          </summary>
          <div style={{ overflowX: "auto", margin: "8px 0" }}>
            <GarbageRootTreeSvg root={root} maxRetained={maxRetained} />
          </div>
        </details>
      ))}
    </>
  );
}
