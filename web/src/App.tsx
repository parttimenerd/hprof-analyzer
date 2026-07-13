import React from "react";
import type { Report, Suspect } from "./types";
import { fmtCount, formatBytes, formatEpochMs, pctOf } from "./format";
import {
  ConcentrationChart,
  DepthHistogramChart,
  GcRootsChart,
  HeapCompositionChart,
  LeakShareChart,
  TopClassesChart,
} from "./charts";

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
    <div className="oom">
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

// ── System Overview ─────────────────────────────────────────────────────────
function SystemOverviewSection({ report }: { report: Report }) {
  const o = report.overview;
  return (
    <section>
      <h2>System Overview</h2>
      <p className="subtitle">Reachable-heap totals and the largest classes by retained heap.</p>

      <div className="card">
        <dl className="summary-grid">
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

      {o.heap_composition.by_kind.length > 1 && (
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

      {o.gc_roots_by_type.length > 1 && (
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
          <DepthHistogramChart data={o.dominator_depth_histogram} />
        </>
      )}

      {o.retention_concentration.top1_bp > 0 && (
        <>
          <h3>Retention Concentration</h3>
          <ConcentrationChart rc={o.retention_concentration} />
        </>
      )}

      <h3>Class Histogram (by Retained Heap)</h3>
      <TopClassesChart data={o.histogram} />
      <details>
        <summary>Show full class histogram ({fmtCount(o.histogram.length)} rows)</summary>
        <table>
          <thead>
            <tr>
              <th>#</th>
              <th>Class</th>
              <th className="num">Instances</th>
              <th className="num">Shallow</th>
              <th className="num">Retained</th>
            </tr>
          </thead>
          <tbody>
            {o.histogram.map((h, i) => (
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
    </section>
  );
}

// ── Leak Suspects ───────────────────────────────────────────────────────────
function SuspectCard({ s, total }: { s: Suspect; total: number }) {
  const share = pctOf(s.retained, total);
  return (
    <div className="suspect">
      <h3 style={{ margin: "0 0 0.25rem" }}>
        <code>{s.pretty_class}</code>
        <span className="pill">{s.is_single ? "single object" : `class group ×${fmtCount(s.instance_count)}`}</span>
      </h3>
      <p style={{ margin: "0.25rem 0" }}>
        Retains <strong>{formatBytes(s.retained)}</strong> ({share.toFixed(1)}% of the reachable heap).
        {s.root_type_label && (
          <>
            {" "}
            Kept alive via a <strong>{s.root_type_label}</strong> root.
          </>
        )}
      </p>
      {s.accumulation_class && (
        <p style={{ margin: "0.25rem 0", color: "var(--muted)", fontSize: "0.86rem" }}>
          Accumulation point: <code>{s.accumulation_class}</code>
          {s.accumulation_retained != null && <> retaining {formatBytes(s.accumulation_retained)}</>}.
        </p>
      )}
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
    <section>
      <h2>Leak Suspects</h2>
      <p className="subtitle">Ranked accumulation points holding the most retained heap.</p>
      {l.suspects.length === 0 ? (
        <p>No suspect exceeds the leak threshold; retention is spread across many roots.</p>
      ) : (
        <>
          <LeakShareChart suspects={l.suspects} total={l.total_shallow} />
          {l.suspects.map((s, i) => (
            <SuspectCard key={i} s={s} total={l.total_shallow} />
          ))}
        </>
      )}
    </section>
  );
}

// ── Top Consumers ───────────────────────────────────────────────────────────
function TopConsumersSection({ report }: { report: Report }) {
  const t = report.top;
  const total = report.leaks.total_shallow;
  return (
    <section>
      <h2>Top Consumers</h2>
      <p className="subtitle">Biggest individual objects and classes by retained heap.</p>

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
              <td className="num">{formatBytes(o.retained)}</td>
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
          </tr>
        </thead>
        <tbody>
          {t.biggest_classes.map((c, i) => (
            <tr key={i}>
              <td>
                <code>{c.pretty_class}</code>
              </td>
              <td className="num">{fmtCount(c.instances)}</td>
              <td className="num">{formatBytes(c.retained)}</td>
            </tr>
          ))}
        </tbody>
      </table>

      {t.biggest_packages.children.length > 0 && (
        <>
          <h3>Biggest Packages</h3>
          <table>
            <thead>
              <tr>
                <th>Package</th>
                <th className="num"># Objects</th>
                <th className="num">Shallow</th>
                <th className="num">Retained</th>
              </tr>
            </thead>
            <tbody>
              {t.biggest_packages.children.map((p, i) => (
                <tr key={i}>
                  <td>{p.name || "(root)"}</td>
                  <td className="num">{fmtCount(p.top_dominator_count)}</td>
                  <td className="num">{formatBytes(p.shallow_heap)}</td>
                  <td className="num">{formatBytes(p.retained_heap)}</td>
                </tr>
              ))}
            </tbody>
          </table>
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
      <OomTriage report={report} />
      <SystemOverviewSection report={report} />
      <LeakSuspectsSection report={report} />
      <TopConsumersSection report={report} />
    </div>
  );
}
