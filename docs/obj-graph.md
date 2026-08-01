# Object Graph Explorer

The Object Graph Explorer provides MAT-style click-through navigation of the Java heap directly inside the generated HTML report — no network connection or external tool required.

## Enabling it

Pass `--obj-graph` when analyzing a heap dump:

```bash
hprof-analyzer heap.hprof report.html --obj-graph
```

The flag accepts an optional tier to trade off report size against coverage:

| Flag | Edges / object | Typical HTML delta |
|------|---------------|--------------------|
| `--obj-graph` or `--obj-graph=small` | 100 | +1–3 MB |
| `--obj-graph=medium` | 150 | +2–5 MB |
| `--obj-graph=large` | 300 | +5–15 MB |

The delta is on top of the base report. For the bundled test fixture the base report is ~477 KB compressed; all tiers are the same size because that fixture is small.

## Features

### Outbound References tab

Shows every reference field pointing **out** of the selected object — the same view as Eclipse MAT's "Outgoing References". Fields are grouped by `<field name> → <target class>` and can be expanded to reveal the target object's shallow and retained heap sizes.

The capture tier indicator at the top of the panel shows how many edges per object were captured (`Capture: 100 edges/obj (small)`). When an object has more outbound edges than the tier allows, the panel shows a truncation notice.

### Inbound References tab

Shows objects that hold a reference **to** the selected object — the "Incoming References" view from MAT. Inbound references are captured by a second pass over the outbound edge table before it is consumed, so no extra heap scanning is required at report time.

When no inbound references are captured for an object (e.g., GC roots that are held by the VM, not by another object), the tab shows "No inbound references captured".

### Path to GC Root

Expands a collapsible panel showing the **dominator chain** from the selected object up to the GC root that keeps it alive. Each step shows the retaining class and the retained heap it contributes. This is the fastest path to understanding *why* an object is not being collected.

### Navigation

- Click any object row in **Top Consumers → Biggest Objects** to open the explorer.
- The URL updates to `#explore/<object-id>` so you can bookmark or share a specific object.
- Use the back button or breadcrumb trail to return to a parent object.
- Objects with no captured edges show `(deeper nodes not captured)`.

## Browser WASM mode

When you open a static report in a modern browser, the explorer automatically upgrades to full WASM-powered navigation. The WASM module can resolve any object in the heap, not just the top-N that were pre-captured at analysis time. An upgrade banner is shown when WASM is available.

To use WASM mode, open the report via the hprof-analyzer browser tool (requires a running local server) or drag-drop the `.hprof` file into the report page.

## Field names

By default, edges are shown without field names. Pass `--ref-paths` alongside `--obj-graph` to record field names for every reference in the heap:

```bash
hprof-analyzer heap.hprof report.html --obj-graph --ref-paths
```

`--ref-paths` adds approximately 2 bytes per reference (100–500 MB extra RSS on multi-GB dumps) and labels edges as `ParentClass.fieldName → ChildClass`.

## Capture details

- The top ~10,000 objects by shallow heap size are pre-captured as edge sources.
- Only objects in this set can be navigated in static mode; deeper objects show `(deeper nodes not captured)`.
- In WASM mode there is no such limit.
- Cycles are handled: the explorer stops following a path once it revisits an object already in the current path.
- The dominator chain (Path to GC Root) is always available for any object in **Top Consumers**, regardless of tier.
