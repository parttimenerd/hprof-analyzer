import React from "react";
import type { HistRow, PackageNode, Report, Suspect, ThreadInfo } from "./types";
import { fmtCount, fmtExactBytes, formatBytes, formatEpochMs, pctOf } from "./format";
import {
  ConcentrationChart,
  DepthHistogramChart,
  GcRootsChart,
  HeapCompositionChart,
  LeakShareChart,
  TopClassesChart,
} from "./charts";

// ── Navigation ───────────────────────────────────────────────────────────────
// A sticky in-page table of contents so long reports (hundreds of threads,
// thousands of histogram rows) stay navigable — MAT's report has an equivalent
// left-hand section index.
function Nav() {
  const items: [string, string][] = [
    ["triage", "OOM Triage"],
    ["overview", "System Overview"],
    ["leaks", "Leak Suspects"],
    ["top", "Top Consumers"],
    ["threads", "Threads"],
  ];
  return (
    <nav className="toc">
      {items.map(([id, label]) => (
        <a key={id} href={`#${id}`}>
          {label}
        </a>
      ))}
    </nav>
  );
}

// ── OOM Triage lead-in ──────────────────────────────────────────────────────
// Re-projects already-modeled fields (matches render_markdown's render_oom_triage).
function OomTriage({ report }: { report: Report }) {
  const total = report.leaks.total_shallow;
  const lines: React.ReactNode[] = [];

  const first = report.leaks.suspects[0];
  if (first) {
    const kind = first.is_single ? "a single object" : "a class group";
    lines.push(
      <>
        <strong>Headline retainer:</strong> <code>{first.pretty_class}</code> ({kind}) retains{" "}
        {formatBytes(first.retained)} ({pctOf(first.retained, total).toFixed(1)}% of reachable heap).
      </>,
    );
  } else if (report.top.biggest_objects[0]) {
    const o = report.top.biggest_objects[0];
    lines.push(
      <>
        <strong>Headline retainer:</strong> <code>{o.display_class}</code> (object #{o.obj_index_1based}) retains{" "}
        {formatBytes(o.retained)} ({pctOf(o.retained, total).toFixed(1)}% of reachable heap).
      </>,
    );
  } else {
    lines.push(
      <>
        <strong>Headline retainer:</strong> No dominant retainer found.
      </>,
    );
  }

  if (first && pctOf(first.retained, total) >= 50) {
    lines.push(
      <>
        <strong>Concentration:</strong> A single object/class group dominates the heap (
        {pctOf(first.retained, total).toFixed(1)}%).
      </>,
    );
  } else if (first) {
    lines.push(
      <>
        <strong>Concentration:</strong> Retention is spread across multiple roots.
      </>,
    );
  } else {
    lines.push(
      <>
        <strong>Concentration:</strong> No suspect exceeds the threshold; retention is spread across many roots.
      </>,
    );
  }

  const hist = report.overview.dominator_depth_histogram;
  if (hist.length > 0) {
    const totObj = hist.reduce((s, b) => s + b.objects, 0);
    const maxDepth = hist.reduce((m, b) => Math.max(m, b.depth), 0);
    let cum = 0;
    let p90 = maxDepth;
    for (const b of hist) {
      cum += b.objects;
      if (cum * 10 >= totObj * 9) {
        p90 = b.depth;
        break;
      }
    }
    const shape =
      p90 <= 3
        ? "shallow (most objects are held within a few hops of a GC root)"
        : "deep (retention flows through long dominator chains — often nested collections or linked structures)";
    lines.push(
      <>
        <strong>Shape:</strong> {shape} — 90% of objects within depth {p90}, max depth {maxDepth}.
      </>,
    );
  }

  const rc = report.overview.retention_concentration;
  if (rc.top1_bp > 0 || rc.num_objects_ge_1pct > 0) {
    lines.push(
      <>
        <strong>One leak or many:</strong> the single biggest object retains {(rc.top1_bp / 100).toFixed(1)}% and the
        top 10 retain {(rc.top10_bp / 100).toFixed(1)}% of the heap; {fmtCount(rc.num_objects_ge_1pct)} object(s) each
        hold ≥1%.
      </>,
    );
  }

  return (
    <div className="oom" id="triage">
      <h2>OOM Triage</h2>
      <p className="subtitle">Where the reachable heap is concentrated, at a glance.</p>
      <ul>
        {lines.map((l, i) => (
          <li key={i}>{l}</li>
        ))}
      </ul>
    </div>
  );
}

// ── Sortable / filterable class histogram ────────────────────────────────────
type HistKey = "instances" | "shallow" | "retained";
const HIST_COLS: { key: HistKey; label: string }[] = [
  { key: "instances", label: "Instances" },
  { key: "shallow", label: "Shallow" },
  { key: "retained", label: "Retained" },
];

function ClassHistogramTable({ rows }: { rows: HistRow[] }) {
  const [sortKey, setSortKey] = React.useState<HistKey>("retained");
  const [filter, setFilter] = React.useState("");

  const view = React.useMemo(() => {
    const needle = filter.trim().toLowerCase();
    const filtered = needle
      ? rows.filter((r) => r.pretty_class.toLowerCase().includes(needle))
      : rows;
    // Stable sort descending by the chosen numeric column. Copy first so we
    // never mutate the report model.
    return [...filtered].sort((a, b) => b[sortKey] - a[sortKey]);
  }, [rows, sortKey, filter]);

  const CAP = 500;
  const shown = view.slice(0, CAP);

  return (
    <details>
      <summary>Show full class histogram ({fmtCount(rows.length)} rows)</summary>
      <div className="tools">
        <input
          type="text"
          className="filter"
          placeholder="Filter by class name…"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          aria-label="Filter histogram by class name"
        />
        <span className="hint">
          {fmtCount(view.length)} match{view.length === 1 ? "" : "es"}
          {view.length > CAP ? ` (showing first ${CAP})` : ""} — click a column to sort
        </span>
      </div>
      <table>
        <thead>
          <tr>
            <th>#</th>
            <th>Class</th>
            {HIST_COLS.map((c) => (
              <th
                key={c.key}
                className={"num sortable" + (sortKey === c.key ? " active" : "")}
                onClick={() => setSortKey(c.key)}
                title={`Sort by ${c.label} (descending)`}
              >
                {c.label} {sortKey === c.key ? "▾" : ""}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {shown.map((h, i) => (
            <tr key={i}>
              <td className="num">{i + 1}</td>
              <td>
                <code>{h.pretty_class}</code>
              </td>
              <td className="num">{fmtCount(h.instances)}</td>
              <td className="num">{formatBytes(h.shallow)}</td>
              <td className="num">{formatBytes(h.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

// ── System Overview ─────────────────────────────────────────────────────────
function SystemOverviewSection({ report }: { report: Report }) {
  const o = report.overview;
  const threadCount = report.threads?.threads?.length ?? 0;
  return (
    <section id="overview">
      <h2>System Overview</h2>
      <p className="subtitle">Reachable-heap totals and the largest classes by retained heap.</p>

      <div className="card">
        <dl className="summary-grid">
          <dt>Source file</dt>
          <dd>
            <code title={o.file_path}>{o.source_name}</code>
            {o.file_path && o.file_path !== o.source_name && (
              <span className="hint" style={{ display: "block" }}>
                {o.file_path}
              </span>
            )}
          </dd>
          <dt>HPROF format</dt>
          <dd>{o.format}</dd>
          <dt>File size</dt>
          <dd>{formatBytes(o.file_size)}</dd>
          <dt>Identifier size</dt>
          <dd>{o.identifier_size_bits}-bit</dd>
          {o.compressed_oops !== null && (
            <>
              <dt>Compressed OOPs</dt>
              <dd>{o.compressed_oops ? "yes" : "no"}</dd>
            </>
          )}
          {o.dump_creation !== null && (
            <>
              <dt>Dump created</dt>
              <dd>{formatEpochMs(o.dump_creation)}</dd>
            </>
          )}
          <dt>Total objects</dt>
          <dd>{fmtCount(o.total_objects)}</dd>
          <dt>Total shallow heap</dt>
          <dd>{formatBytes(o.total_shallow)}</dd>
          <dt>GC roots</dt>
          <dd>{fmtCount(o.gc_roots)}</dd>
          <dt>Classes loaded</dt>
          <dd>{fmtCount(o.classes_loaded)}</dd>
          <dt>Class loaders</dt>
          <dd>{fmtCount(o.classloaders_loaded)}</dd>
          {threadCount > 0 && (
            <>
              <dt>Threads (with call stacks)</dt>
              <dd>
                <a href="#threads">{fmtCount(threadCount)}</a>
              </dd>
            </>
          )}
          {o.unreachable_count > 0 && (
            <>
              <dt>Unreachable (excluded)</dt>
              <dd>
                {fmtCount(o.unreachable_count)} ({formatBytes(o.unreachable_shallow)})
              </dd>
            </>
          )}
        </dl>
      </div>

      {o.heap_composition.by_kind.length > 0 && (
        <>
          <h3>Heap Composition</h3>
          <HeapCompositionChart data={o.heap_composition.by_kind} />
          <table>
            <thead>
              <tr>
                <th>Kind</th>
                <th className="num">Objects</th>
                <th className="num">Shallow Heap</th>
              </tr>
            </thead>
            <tbody>
              {o.heap_composition.by_kind.map((k, i) => (
                <tr key={i}>
                  <td>{k.kind}</td>
                  <td className="num">{fmtCount(k.objects)}</td>
                  <td className="num">{formatBytes(k.shallow_heap)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {o.gc_roots_by_type.length > 0 && (
        <>
          <h3>GC Roots by Type</h3>
          <GcRootsChart data={o.gc_roots_by_type} />
          <table>
            <thead>
              <tr>
                <th>Root Type</th>
                <th className="num">Count</th>
              </tr>
            </thead>
            <tbody>
              {o.gc_roots_by_type.map((r, i) => (
                <tr key={i}>
                  <td>{r.root_type}</td>
                  <td className="num">{fmtCount(r.count)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {o.dominator_depth_histogram.length > 0 && (
        <>
          <h3>Dominator-Depth Distribution</h3>
          <p className="subtitle">
            How many hops each object sits below a GC root. A tall left side means shallow retention; a long tail means
            deep, chained structures.
          </p>
          <DepthHistogramChart data={o.dominator_depth_histogram} />
        </>
      )}

      {(o.retention_concentration.top1_bp > 0 || o.retention_concentration.num_objects_ge_1pct > 0) && (
        <>
          <h3>Retention Concentration</h3>
          <p className="subtitle">Share of the heap held by the top 1 / 10 / 100 single objects — high bars on the left mean one big leak.</p>
          <ConcentrationChart rc={o.retention_concentration} />
          <dl className="summary-grid">
            <dt>Total retained (top-level dominators)</dt>
            <dd>{formatBytes(o.retention_concentration.total_retained)}</dd>
            <dt>Top-1 share</dt>
            <dd>{(o.retention_concentration.top1_bp / 100).toFixed(2)}%</dd>
            <dt>Top-10 share</dt>
            <dd>{(o.retention_concentration.top10_bp / 100).toFixed(2)}%</dd>
            <dt>Top-100 share</dt>
            <dd>{(o.retention_concentration.top100_bp / 100).toFixed(2)}%</dd>
            <dt>Objects each holding ≥1%</dt>
            <dd>{fmtCount(o.retention_concentration.num_objects_ge_1pct)}</dd>
          </dl>
        </>
      )}

      <h3>Class Histogram (by Retained Heap)</h3>
      {o.histogram_truncated_to != null && (
        <p className="subtitle">
          Histogram capped to the largest {fmtCount(o.histogram_truncated_to)} classes.
        </p>
      )}
      <TopClassesChart data={o.histogram} />
      <ClassHistogramTable rows={o.histogram} />
    </section>
  );
}

// ── Leak Suspects ───────────────────────────────────────────────────────────
// Renders the accumulation "shortest path" (MAT's signature view) plus the
// per-class breakdown of what piles up at the accumulation point.
function AccumulationPath({ s }: { s: Suspect }) {
  if (s.path.length === 0) return null;
  return (
    <details open>
      <summary>Shortest path to the accumulation point ({s.path.length} steps)</summary>
      <ol className="accum-path">
        {s.path.map((p, i) => (
          <li key={i}>
            <code>{p.display_class}</code> <span className="pill">obj #{p.obj_index_1based}</span>
            <span className="path-ret">retains {formatBytes(p.retained)}</span>
          </li>
        ))}
      </ol>
    </details>
  );
}

function DominatedByClass({ rows }: { rows: HistRow[] }) {
  if (rows.length === 0) return null;
  return (
    <details>
      <summary>Accumulated objects grouped by class ({rows.length})</summary>
      <table>
        <thead>
          <tr>
            <th>Class</th>
            <th className="num">Instances</th>
            <th className="num">Shallow</th>
            <th className="num">Retained</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((r, i) => (
            <tr key={i}>
              <td>
                <code>{r.pretty_class}</code>
              </td>
              <td className="num">{fmtCount(r.instances)}</td>
              <td className="num">{formatBytes(r.shallow)}</td>
              <td className="num">{formatBytes(r.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </details>
  );
}

function SuspectCard({ s, total, rank }: { s: Suspect; total: number; rank: number }) {
  const share = pctOf(s.retained, total);
  return (
    <div className="suspect">
      <h3 style={{ margin: "0 0 0.25rem" }}>
        <span className="rank">Problem Suspect {rank}</span> <code>{s.pretty_class}</code>
        <span className="pill">{s.is_single ? "single object" : `class group ×${fmtCount(s.instance_count)}`}</span>
      </h3>
      <p style={{ margin: "0.25rem 0" }}>
        Retains <strong title={fmtExactBytes(s.retained)}>{formatBytes(s.retained)}</strong>{" "}
        <span className="mat-exact">
          {fmtExactBytes(s.retained)} ({share.toFixed(2)}%)
        </span>
        {s.shallow > 0 && <> · shallow {formatBytes(s.shallow)}</>}.
      </p>
      <p style={{ margin: "0.25rem 0" }}>
        <span className="label">Held by:</span>{" "}
        {s.root_type_label ? (
          <>
            a <strong>{s.root_type_label}</strong> GC root
          </>
        ) : (
          <span style={{ color: "var(--muted)" }}>multiple / ambiguous roots (no single holding root identified)</span>
        )}
      </p>
      {s.keywords.length > 0 && (
        <p style={{ margin: "0.25rem 0" }}>
          <span className="label">Keywords:</span>{" "}
          {s.keywords.map((k, i) => (
            <span key={i} className="pill keyword" title="Class involved in this suspect">
              {k}
            </span>
          ))}
        </p>
      )}
      {s.accumulation_class && (
        <p style={{ margin: "0.25rem 0", color: "var(--muted)", fontSize: "0.86rem" }}>
          Accumulation point: <code>{s.accumulation_class}</code>
          {s.accumulation_obj_1based != null && <> (obj #{s.accumulation_obj_1based})</>}
          {s.accumulation_retained != null && <> retaining {formatBytes(s.accumulation_retained)}</>}.
        </p>
      )}
      <AccumulationPath s={s} />
      <DominatedByClass rows={s.dominated_by_class} />
      {s.dominated.length > 0 && (
        <details>
          <summary>Accumulated objects in dominator tree ({s.dominated.length})</summary>
          <table>
            <thead>
              <tr>
                <th>Object</th>
                <th>Class</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {s.dominated.map((d, i) => (
                <tr key={i}>
                  <td className="num">#{d.obj_index_1based}</td>
                  <td>
                    <code>{d.display_class}</code>
                  </td>
                  <td className="num">{formatBytes(d.shallow)}</td>
                  <td className="num">{formatBytes(d.retained)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </details>
      )}
    </div>
  );
}

function LeakSuspectsSection({ report }: { report: Report }) {
  const l = report.leaks;
  return (
    <section id="leaks">
      <h2>Leak Suspects</h2>
      <p className="subtitle">Ranked accumulation points holding the most retained heap.</p>
      {l.suspects.length === 0 ? (
        <p>No suspect exceeds the leak threshold; retention is spread across many roots.</p>
      ) : (
        <>
          <h3>Overview — retained-heap share</h3>
          <p className="subtitle">
            How concentrated the leak is: each slice is one suspect&apos;s retained heap; the remainder is everything
            else on the reachable heap.
          </p>
          <LeakShareChart suspects={l.suspects} total={l.total_shallow} />
          {l.suspects.map((s, i) => (
            <SuspectCard key={i} s={s} total={l.total_shallow} rank={i + 1} />
          ))}
        </>
      )}
    </section>
  );
}

// ── Top Consumers ───────────────────────────────────────────────────────────
// A recursive, expandable package tree (MAT PackageTreeResult drill-down). Each
// node shows cumulative # objects / shallow / retained over its subtree.
function PackageTreeRow({ node, depth, maxRetained }: { node: PackageNode; depth: number; maxRetained: number }) {
  const [open, setOpen] = React.useState(depth < 1);
  const hasChildren = node.children.length > 0;
  const label = node.name || "(default package)";
  const pct = maxRetained > 0 ? (node.retained_heap / maxRetained) * 100 : 0;
  return (
    <>
      <tr>
        <td>
          <span style={{ paddingLeft: `${depth * 1.1}rem` }}>
            {hasChildren ? (
              <button className="tree-toggle" onClick={() => setOpen(!open)} aria-expanded={open}>
                {open ? "▾" : "▸"}
              </button>
            ) : (
              <span className="tree-leaf">•</span>
            )}
            <code>{label}</code>
          </span>
        </td>
        <td className="num">{fmtCount(node.top_dominator_count)}</td>
        <td className="num">{formatBytes(node.shallow_heap)}</td>
        <td className="num bar-cell">
          <span className="bar-bg">
            <span className="bar-fill" style={{ width: `${pct}%` }} />
          </span>
          {formatBytes(node.retained_heap)}
        </td>
      </tr>
      {open &&
        node.children.map((c, i) => (
          <PackageTreeRow key={i} node={c} depth={depth + 1} maxRetained={maxRetained} />
        ))}
    </>
  );
}

function TopConsumersSection({ report }: { report: Report }) {
  const t = report.top;
  const total = report.leaks.total_shallow;
  const pkgRoot = t.biggest_packages;
  const maxPkgRetained = pkgRoot.children.reduce((m, c) => Math.max(m, c.retained_heap), 0);
  return (
    <section id="top">
      <h2>Top Consumers</h2>
      <p className="subtitle">Biggest individual objects, classes, and packages by retained heap.</p>

      <h3>Biggest Objects</h3>
      <table>
        <thead>
          <tr>
            <th>#</th>
            <th>Class</th>
            <th className="num">Shallow</th>
            <th className="num">Retained</th>
            <th className="num">% Heap</th>
          </tr>
        </thead>
        <tbody>
          {t.biggest_objects.map((o, i) => (
            <tr key={i}>
              <td className="num">{i + 1}</td>
              <td>
                <code>{o.display_class}</code> <span className="pill">obj #{o.obj_index_1based}</span>
              </td>
              <td className="num">{formatBytes(o.shallow)}</td>
              <td className="num" title={fmtExactBytes(o.retained)}>
                {formatBytes(o.retained)}
              </td>
              <td className="num">{pctOf(o.retained, total).toFixed(1)}%</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h3>Biggest Classes</h3>
      <table>
        <thead>
          <tr>
            <th>Class</th>
            <th className="num">Instances</th>
            <th className="num">Retained</th>
            <th className="num">% Heap</th>
          </tr>
        </thead>
        <tbody>
          {t.biggest_classes.map((c, i) => (
            <tr key={i}>
              <td>
                <code>{c.pretty_class}</code>
              </td>
              <td className="num">{fmtCount(c.instances)}</td>
              <td className="num" title={fmtExactBytes(c.retained)}>
                {formatBytes(c.retained)}
              </td>
              <td className="num">{pctOf(c.retained, total).toFixed(1)}%</td>
            </tr>
          ))}
        </tbody>
      </table>

      {pkgRoot.children.length > 0 && (
        <>
          <h3>Biggest Packages</h3>
          <p className="subtitle">
            Expand a package to drill into its sub-packages. Totals are cumulative over the subtree. Only top-level
            dominators retaining at least {(t.threshold_bp / 100).toFixed(t.threshold_bp % 100 === 0 ? 0 : 2)}% of the
            heap are included (smaller ones are pruned, MAT-style).
          </p>
          <table className="tree-table">
            <thead>
              <tr>
                <th>Package</th>
                <th className="num"># Objects</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {pkgRoot.children.map((p, i) => (
                <PackageTreeRow key={i} node={p} depth={0} maxRetained={maxPkgRetained} />
              ))}
            </tbody>
          </table>
        </>
      )}
    </section>
  );
}

// ── Threads ─────────────────────────────────────────────────────────────────
// One collapsible block per thread; frames rendered verbatim in a monospace
// <pre>. A filter box keeps large thread sets (hundreds) navigable. Preserves
// the upstream (thread_serial-sorted) order for determinism.
function ThreadCard({ t }: { t: ThreadInfo }) {
  const cls = t.class_name ?? "<unresolved>";
  return (
    <details className="thread">
      <summary>
        Thread {t.thread_serial} (<code>{cls}</code>) — {fmtCount(t.frames.length)} frame
        {t.frames.length === 1 ? "" : "s"}
      </summary>
      <pre className="stack">{t.frames.join("\n")}</pre>
    </details>
  );
}

function ThreadsSection({ report }: { report: Report }) {
  const threads = report.threads?.threads ?? [];
  const [filter, setFilter] = React.useState("");
  const view = React.useMemo(() => {
    const needle = filter.trim().toLowerCase();
    if (!needle) return threads;
    return threads.filter(
      (t) =>
        (t.class_name ?? "").toLowerCase().includes(needle) ||
        String(t.thread_serial).includes(needle) ||
        t.frames.some((f) => f.toLowerCase().includes(needle)),
    );
  }, [threads, filter]);
  return (
    <section id="threads">
      <h2>Threads</h2>
      <p className="subtitle">Per-thread call stacks recorded in the dump.</p>
      {threads.length === 0 ? (
        <p>No thread call stacks were recorded in this dump.</p>
      ) : (
        <>
          <div className="tools">
            <input
              type="text"
              className="filter"
              placeholder="Filter threads (class, serial, or stack frame)…"
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
              aria-label="Filter threads"
            />
            <span className="hint">
              {fmtCount(view.length)} of {fmtCount(threads.length)} thread{threads.length === 1 ? "" : "s"}
            </span>
          </div>
          {view.map((t, i) => (
            <ThreadCard key={i} t={t} />
          ))}
        </>
      )}
    </section>
  );
}

export default function App({ report }: { report: Report }) {
  return (
    <div className="app">
      <h1>
        Heap Dump Analysis: <code>{report.overview.source_name}</code>
      </h1>
      <p className="subtitle">Generated by hprof-analyzer — {report.generated}</p>
      <Nav />
      <OomTriage report={report} />
      <SystemOverviewSection report={report} />
      <LeakSuspectsSection report={report} />
      <TopConsumersSection report={report} />
      <ThreadsSection report={report} />
    </div>
  );
}
