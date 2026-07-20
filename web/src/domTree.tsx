// Dominator-tree SVG visualization — shared between the unreachable garbage-root
// trees and the leak-suspect dominator subtrees.
//
// Uses d3-dag `graphHierarchy` + `sugiyama` for layout. Nodes are boxes sized
// proportionally to retained heap, labeled with class + retained. Edges are
// bezier curves with arrowheads. Each tree is rendered in its own <svg>.

import React from "react";
import { graphHierarchy, sugiyama, coordGreedy, decrossDfs, layeringLongestPath } from "d3-dag";
import type { DomTreeNode, UnreachableGarbageRoot } from "./types";
import { formatBytes, fmtCount } from "./format";

// ── Layout constants ──────────────────────────────────────────────────────────
const NODE_W = 180;
const NODE_H = 52;
const H_GAP = 40;
const V_GAP = 60;
const FONT_SIZE = 11;
const PALETTE = [
  "#2563eb", "#16a34a", "#d97706", "#dc2626",
  "#7c3aed", "#0891b2", "#db2777", "#65a30d",
];

// ── Helpers ───────────────────────────────────────────────────────────────────

function classColor(name: string): string {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (Math.imul(31, h) + name.charCodeAt(i)) | 0;
  return PALETTE[Math.abs(h) % PALETTE.length];
}

function truncateClass(name: string, maxLen = 22): string {
  if (name.length <= maxLen) return name;
  const short = name.split(".").pop() ?? name;
  return short.length <= maxLen ? short : short.slice(0, maxLen - 1) + "…";
}

// ── Generic tree node used by the layout engine ───────────────────────────────

interface GenNode {
  label: string;      // class name
  retained: number;
  sublabel: string;   // second line (e.g. "N objects" or "shallow X")
  children: GenNode[];
}

// ── Tagged wrapper carrying a stable id for d3-dag ───────────────────────────

interface TaggedNode {
  id: string;
  node: GenNode;
  children: TaggedNode[];
}

function tagTree(node: GenNode, prefix: string): TaggedNode {
  return {
    id: prefix,
    node,
    children: node.children.map((c, i) => tagTree(c, `${prefix}.${i}`)),
  };
}

function countNodes(t: TaggedNode): number {
  return 1 + t.children.reduce((s, c) => s + countNodes(c), 0);
}

// If the tree has too many nodes the sugiyama layout becomes very wide.
// Prune to depth 1 (root + direct children, no grandchildren).
const MAX_LAYOUT_NODES = 25;

function pruneToDepth1(t: TaggedNode): TaggedNode {
  return { ...t, children: t.children.map((c) => ({ ...c, children: [] })) };
}

// ── Layout ────────────────────────────────────────────────────────────────────

interface LayoutNode { id: string; x: number; y: number; node: GenNode; }
interface LayoutEdge { source: string; target: string; }

function layoutTree(root: GenNode): { nodes: LayoutNode[]; edges: LayoutEdge[]; width: number; height: number } {
  let tagged = tagTree(root, "root");
  if (countNodes(tagged) > MAX_LAYOUT_NODES) tagged = pruneToDepth1(tagged);

  if (tagged.children.length === 0) {
    return {
      nodes: [{ id: "root", x: NODE_W / 2, y: NODE_H / 2, node: root }],
      edges: [],
      width: NODE_W + H_GAP,
      height: NODE_H + V_GAP,
    };
  }

  try {
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
      nodes.push({ id: n.data.id, x: n.x, y: n.y, node: n.data.node });
    }
    const edges: LayoutEdge[] = [];
    for (const link of (dag as any).links()) {
      edges.push({ source: link.source.data.id, target: link.target.data.id });
    }
    return { nodes, edges, width: Math.max(width, NODE_W + H_GAP), height: Math.max(height, NODE_H + V_GAP) };
  } catch {
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
      node: n.node,
    }));
    const edges: LayoutEdge[] = allNodes.flatMap((n) =>
      n.children.map((c) => ({ source: n.id, target: c.id }))
    );
    return { nodes, edges, width: NODE_W + H_GAP * 2, height: allNodes.length * (NODE_H + V_GAP) };
  }
}

// ── Core SVG renderer (works on GenNode) ─────────────────────────────────────

function TreeSvg({ root, maxRetained, ariaLabel }: { root: GenNode; maxRetained: number; ariaLabel: string }) {
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
      aria-label={ariaLabel}
    >
      <defs>
        <marker id="arrow" markerWidth="6" markerHeight="6" refX="5" refY="3" orient="auto">
          <path d="M0,0 L6,3 L0,6 Z" fill="var(--border, #94a3b8)" />
        </marker>
      </defs>
      <g transform={`translate(${PAD},${PAD})`}>
        {edges.map((e, i) => {
          const src = byId[e.source];
          const tgt = byId[e.target];
          if (!src || !tgt) return null;
          const x1 = src.x, y1 = src.y + NODE_H / 2;
          const x2 = tgt.x, y2 = tgt.y - NODE_H / 2;
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
        {nodes.map((node) => {
          const x = node.x - NODE_W / 2;
          const y = node.y - NODE_H / 2;
          const col = classColor(node.node.label);
          const barW = maxRetained > 0 ? Math.round((node.node.retained / maxRetained) * (NODE_W - 8)) : 0;
          return (
            <g key={node.id} transform={`translate(${x},${y})`}>
              <rect width={NODE_W} height={NODE_H} rx={5} fill="var(--surface, #f8fafc)" stroke={col} strokeWidth={2} />
              <rect x={4} y={NODE_H - 8} width={barW} height={4} rx={2} fill={col} opacity={0.6} />
              <text x={NODE_W / 2} y={16} textAnchor="middle" fontSize={FONT_SIZE} fontWeight="bold" fill={col} fontFamily="monospace">
                {truncateClass(node.node.label)}
              </text>
              <text x={NODE_W / 2} y={30} textAnchor="middle" fontSize={FONT_SIZE - 1} fill="var(--text-muted, #64748b)" fontFamily="system-ui, sans-serif">
                {formatBytes(node.node.retained)} retained
              </text>
              <text x={NODE_W / 2} y={43} textAnchor="middle" fontSize={FONT_SIZE - 2} fill="var(--text-muted, #94a3b8)" fontFamily="system-ui, sans-serif">
                {node.node.sublabel}
              </text>
            </g>
          );
        })}
      </g>
    </svg>
  );
}

// ── Adapters: convert model types → GenNode ───────────────────────────────────

function garbageRootToGen(r: UnreachableGarbageRoot): GenNode {
  return {
    label: r.pretty_class,
    retained: r.retained,
    sublabel: `${fmtCount(r.objects)} objects`,
    children: r.children.map(garbageRootToGen),
  };
}

function domTreeNodeToGen(n: DomTreeNode): GenNode {
  // Merge consecutive same-class leaf children into one summary node.
  const rawChildren = n.children.map(domTreeNodeToGen);
  const merged: GenNode[] = [];
  let i = 0;
  while (i < rawChildren.length) {
    const c = rawChildren[i];
    if (c.children.length === 0) {
      let j = i + 1;
      while (j < rawChildren.length && rawChildren[j].children.length === 0 && rawChildren[j].label === c.label) j++;
      const count = j - i;
      if (count > 1) {
        const totalRetained = rawChildren.slice(i, j).reduce((s, x) => s + x.retained, 0);
        const totalShallow = n.children.slice(i, j).reduce((s, x) => s + x.shallow, 0);
        merged.push({ label: c.label, retained: totalRetained, sublabel: `×${count} · shallow ${formatBytes(totalShallow)}`, children: [] });
        i = j;
        continue;
      }
    }
    merged.push(c);
    i++;
  }
  return {
    label: n.display_class,
    retained: n.retained,
    sublabel: `shallow ${formatBytes(n.shallow)}`,
    children: merged,
  };
}

// ── Public: Unreachable garbage-root trees ────────────────────────────────────

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
            <TreeSvg
              root={garbageRootToGen(root)}
              maxRetained={maxRetained}
              ariaLabel={`Garbage-root dominator subtree: ${root.pretty_class}`}
            />
          </div>
        </details>
      ))}
    </>
  );
}

// ── Public: Leak-suspect dominator subtree ────────────────────────────────────

export function DomSubtreeSvg({ node }: { node: DomTreeNode }) {
  const gen = domTreeNodeToGen(node);
  return (
    <div style={{ overflowX: "auto", margin: "8px 0" }}>
      <TreeSvg
        root={gen}
        maxRetained={node.retained}
        ariaLabel={`Dominator subtree: ${node.display_class}`}
      />
    </div>
  );
}
