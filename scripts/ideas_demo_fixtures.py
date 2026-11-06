"""Illustrative (not measured) examples for high-priority review cards.

No values here are read from a dump. Each fixture describes the intended UI,
the actual/proposed source, and the condition under which that UI is hidden.
Rows contain display strings only; implementation must compute from the report.
"""


def example(source, question, columns, rows, takeaway, unavailable):
    return dict(source=source, question=question, columns=columns, rows=rows,
                takeaway=takeaway, unavailable=unavailable)


PRIORITY = {
    "A2": example(
        "Existing: dominator_analysis.immediate_dominators.pairs (capped; pairs_truncated)",
        "Who immediately dominates the byte arrays?",
        ("Owner class", "Dominated class", "Pairs", "Dominated shallow"),
        (("SessionStore", "byte[]", "7,800", "412 MiB"),
         ("HashMap$Node", "byte[]", "1,400", "51 MiB")),
        "Follow SessionStore first. These pairs do not describe direct field edges.",
        "No immediate-dominator pairs captured; hide the table, not a fabricated zero."),
    "B2": example(
        "Existing: obj_graph_flat.inbound_edges (only when graph captured); proposed: bounded on-demand referrers query",
        "Will cutting one reference free the object?",
        ("Observed referrer", "Field", "Evidence"),
        (("SessionCache", "table[3]", "observed"),
         ("ActiveSessions", "latest", "observed")),
        "Two observed referrers: cutting one edge is not proven to release the object. Scope is bounded.",
        "No inbound graph or query result: hide alternate-referrer evidence; never infer a unique owner."),
    "B4": example(
        "Existing: leak_indicators.nio_buffer_owners + direct_byte_buffer_capacity_sum; P4 is proposed",
        "Which heap owners keep direct buffers alive?",
        ("Owner path", "Buffer capacity", "Heap retained"),
        (("root → NettyPool → DirectByteBuffer", "256 MiB", "1.2 KiB"),
         ("root → Worker → DirectByteBuffer", "64 MiB", "1.1 KiB")),
        "Capacity is reported native capacity, not measured RSS; slices may double-count until P4 is verified.",
        "No decoded direct buffers: hide owner/capacity breakdown."),
    "D4": example(
        "Existing: collection_attribution (only when collections attribution was requested)",
        "Which field owns many tiny collections?",
        ("Holding field", "Tiny/empty containers", "Own shallow"),
        (("Session#attributes", "12,000", "1.1 MiB"),
         ("Order#notes", "4,100", "0.4 MiB")),
        "Inspect the holding field before suggesting lazy initialization; own shallow is not a savings promise.",
        "Collection attribution absent: hide; do not treat an unrun pass as zero collections."),
    "E2": example(
        "Existing: thread_local_analysis (requires duplicates pass); null-key counts in leak_indicators",
        "Which pooled threads retain stale ThreadLocal values?",
        ("Value class", "Stale entries", "Retained attribution"),
        (("RequestContext", "8,200", "380 MiB"),
         ("byte[]", "120", "94 MiB")),
        "Null keys with live values justify inspecting thread lifetimes; per-row retained may overlap.",
        "Value-class breakdown not computed (duplicates pass skipped): do not display a zero count."),
    "E3": example(
        "Existing: threads.threads[].retained; thread-local roots when recorded",
        "Which thread dominates large memory?",
        ("Thread", "Retained heap", "Dominant class"),
        (("worker-12", "812 MiB", "SessionStore"),
         ("worker-8", "74 MiB", "byte[]")),
        "Sort by thread retained; thread totals can overlap and must not be summed.",
        "No thread/stack records: hide per-thread ranking."),
    "G1": example(
        "Existing: diff_reports.rs; requires ≥2 compatible captured reports",
        "Which class population grew between captures?",
        ("Class", "Earlier", "Later", "Δ shallow"),
        (("SessionStore", "130 MiB", "540 MiB", "+410 MiB"),
         ("byte[]", "210 MiB", "246 MiB", "+36 MiB")),
        "Two captures give a delta, not a trend or proof of a leak; compare capture phase and class loader.",
        "Fewer than two compatible dumps: hide comparison."),
    "H2": example(
        "Existing: triage[]; producer order is rule registry order, not severity order",
        "What should I inspect first?",
        ("Signal", "Evidence", "Next step"),
        (("Warning: stale ThreadLocal", "8,320 values", "Open E2"),
         ("Info: direct buffers", "320 MiB capacity", "Open B4")),
        "Keep the real rule order or explicitly sort in the UI; severity alone is not priority.",
        "Triage was not evaluated: show coverage/error, not 'no signals'."),
    "H3": example(
        "Existing: analysis_flags, truncated_input, redacted_input, and presence of optional sections",
        "Which conclusions are supported by this report?",
        ("Evidence", "Status", "Meaning"),
        (("Collections", "not run", "No collection attribution"),
         ("Allocation traces", "absent", "No allocation sites")),
        "Missing evidence is not a negative finding. Coverage remains visible even when optional views are hidden.",
        "Report unavailable: show an input/read error instead of presenting empty coverage."),
    "I4": example(
        "Existing: alloc_sites.traces_present + alloc_sites.sites; allocation traces must be in HPROF",
        "Where were surviving objects allocated?",
        ("Captured allocation stack", "Surviving instances", "Weight"),
        (("SessionStore.put → new Entry", "3,800", "count"),
         ("BufferPool.acquire → allocate", "1,200", "count")),
        "Flame width should use additive instance count (or verified shallow), never overlapping retained.",
        "traces_present=false or no usable frames: hide allocation view; Full cannot recover traces."),
    "J5": example(
        "Existing: waste_breakdown, waste_summary and independent duplicate/collection evidence",
        "Which opportunities merit a closer look?",
        ("Candidate", "Observed footprint", "Go to"),
        (("Oversized HashMaps", "190 MiB", "D2"),
         ("Repeated byte[]", "84 MiB", "J3")),
        "These categories can overlap; sort by evidence but do not sum them into a guaranteed savings figure.",
        "Opportunity analysis not available: hide list and keep coverage visible."),
    "K1": example(
        "Existing: top_retainers[] (at most 20; field/frame holder attribution)",
        "Which holders should I open first?",
        ("Holder", "Kind", "Attributed retained"),
        (("SessionCache#entries", "field", "812 MiB"),
         ("worker-12 run()", "frame", "74 MiB")),
        "Rank attributed retained but do not add rows together: holder scopes may overlap.",
        "No retained holders: hide ranking and retain the basic top-consumers view."),
    "K2": example(
        "Existing: fields_by_size (requires collections pass)",
        "Which field is responsible for the retained graph?",
        ("Holding field", "Pointees", "Attributed retained"),
        (("SessionCache#entries", "1", "812 MiB"),
         ("Service#pending", "14", "63 MiB")),
        "Values are attribution, not an additive field budget; link to the owner.",
        "fields_by_size absent: hide; do not display zeros for missing analysis."),
    "K5": example(
        "Proposed: bounded on-demand group-by-field query; not in the current report model",
        "Which tenant dominates the selected sessions?",
        ("Tenant ID", "Sessions", "Own shallow", "Attributed retained"),
        (("42", "1,200", "2.4 MiB", "590 MiB"),
         ("18", "90", "0.2 MiB", "27 MiB")),
        "Compare groups; retained totals can overlap and sensitive field values must be masked.",
        "No field decode, unsafe field, or query not run: do not show tenant ranking."),
    "L3": example(
        "Existing: collections.top_prim_arrays/top_obj_arrays.top_individual; owner is first-wins",
        "Which individual allocation is biggest?",
        ("Array", "Length", "Own shallow", "First field owner"),
        (("byte[]", "134,217,728", "128 MiB", "Cache#payload"),
         ("Object[]", "16,000,000", "61 MiB", "Queue#elements")),
        "First observed field owner is not necessarily the unique retainer; inspect paths.",
        "No top-array rows: hide array list."),
    "P1": example(
        "Proposed: decoded pool fields for verified JDK/framework layouts; currently no pools[]",
        "Is work accumulating in a pool?",
        ("Pool class", "Workers", "Queued", "Decode status"),
        (("ThreadPoolExecutor", "16", "4,120", "known layout"),
         ("HikariPool", "?", "?", "not decoded")),
        "Queued tasks are a snapshot, not evidence of queue growth or measured busy/idle time.",
        "No pool instances: hide. Unknown layout: show 'not decoded', not zero workers."),
    "P2": example(
        "Proposed: referent class of recognized Finalizer records; existing count is not a queue metric",
        "Which class has many registered finalizable objects?",
        ("Referent class", "Registered", "Referent own shallow"),
        (("LegacyResource", "14,000", "0.9 MiB"),
         ("TempFile", "900", "0.1 MiB")),
        "Registered is not pending or stuck; follow owners before recommending close().",
        "No recognized finalizer referents: hide breakdown; keep any observed count separate."),
    "P3": example(
        "Proposed: supported Cleaner/PhantomCleanableRef layouts; no cleaners[] in report today",
        "Which cleanup registrations remain live?",
        ("Action class", "Referent class", "Registrations", "Decode"),
        (("Deallocator", "DirectByteBuffer", "5,000", "supported"),
         ("unknown thunk", "?", "800", "partial")),
        "Registrations ≠ pending cleanup; phantom referents may be cleared or unavailable.",
        "No supported cleaner layout: hide resource attribution; mark decode unavailable."),
    "P4": example(
        "Existing: leak_indicators.direct_byte_buffer_capacity_sum; proposed: unique capacity via cleaner/att",
        "Are duplicate/sliced buffers inflating capacity?",
        ("Measure", "Capacity", "Caveat"),
        (("All live buffer views", "520 MiB", "may overlap"),
         ("Unique backing (verified)", "260 MiB", "only if decoded")),
        "Never show a unique figure without verifying backing identity for this JDK layout.",
        "cleaner/att unresolved: show only all-views capacity, labelled may include slices."),
}

# Visual widths are relative to the largest row WITHIN this illustrative table.
# Each metric is stated; a bar never implies a fraction of the heap or an
# additive total. Examples without a meaningful scalar remain tables.
VISUALS = {
    "A2": ("Dominated own shallow", "MiB", (412, 51)),
    "B4": ("Reported native capacity per buffer", "MiB", (256, 64)),
    "D4": ("Observed tiny/empty containers", "containers", (12000, 4100)),
    "E2": ("Stale/null-key entries by value class", "entries", (8200, 120)),
    "E3": ("Per-thread attributed retained", "MiB", (812, 74)),
    "G1": ("Positive delta in own shallow", "MiB", (410, 36)),
    "I4": ("Captured surviving allocations", "instances", (3800, 1200)),
    "J5": ("Candidate observed footprint (overlap possible)", "MiB", (190, 84)),
    "K1": ("Holder attributed retained (overlap possible)", "MiB", (812, 74)),
    "K2": ("Field attributed retained (overlap possible)", "MiB", (812, 63)),
    "K5": ("Group attributed retained (overlap possible)", "MiB", (590, 27)),
    "L3": ("Individual array own shallow", "MiB", (128, 61)),
    "P2": ("Registered finalizable objects", "objects", (14000, 900)),
    "P3": ("Cleanup registrations", "registrations", (5000, 800)),
    "P4": ("Reported capacity (views can share backing)", "MiB", (520, 260)),
}
for _id, (_metric, _unit, _values) in VISUALS.items():
    PRIORITY[_id]["visual"] = dict(metric=_metric, unit=_unit, values=_values)

ZERO_STATES = {
    "E2": "Duplicates pass ran: zero stale entries observed. Show 0 with the pass coverage, not a leak-free verdict.",
    "H2": "Triage ran: no signals fired. Keep H3 coverage visible; do not conclude 'no leaks'.",
    "J5": "Opportunity-producing analyses ran: no supported candidates. Keep the coverage summary visible.",
    "P1": "Supported pool layout decoded and queued=0. Show 0 queued, not 'not decoded' or idle throughput.",
    "P2": "Finalizer records checked: zero recognized registrations. Show 0, not a pending-queue count.",
}
for _id, _message in ZERO_STATES.items():
    PRIORITY[_id]["zero"] = _message