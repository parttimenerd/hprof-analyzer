# --report-size Benchmarks

Measured with `./target/release/hprof-analyzer <dump> <out.html> --obj-graph --report-size <tier>`.

## Small test fixture (`tests/fixtures/dump_1_mnemonics.hprof`)

This fixture is synthetic and tiny (25 captured objects), so all tiers produce identical output.

| Tier   | Edge cap | HTML size | `obj_graph_flat` JSON | Wall time |
|--------|----------|-----------|-----------------------|-----------|
| small  | 100      | 477 KB    | 128 KB                | ~0.5 s    |
| medium | 150      | 477 KB    | 128 KB                | ~0.5 s    |
| large  | 300      | 477 KB    | 128 KB                | ~0.5 s    |

The fixture is too small to show size differences between tiers — all edges fit within the smallest cap. Differences appear on real-world dumps with dense object graphs.

## Expected impact on real-world dumps (500 MB+ HPROF)

The `obj_graph_flat` size scales with the number of captured objects × `edge_cap`. For a typical production heap with millions of objects:

| Tier   | Edge cap | Approx `obj_graph_flat` (uncompressed) | Approx HTML delta |
|--------|----------|-----------------------------------------|-------------------|
| small  | 100      | ~4–10 MB                               | +1–3 MB           |
| medium | 150      | ~6–15 MB                               | +2–5 MB           |
| large  | 300      | ~12–30 MB                              | +5–15 MB          |

The HTML report compresses the embedded JSON with deflate, so the on-disk file size is roughly 3–5× smaller than the uncompressed JSON figures above.

## Choosing a tier

- **`small` (default):** Best for most reports. Fast to load in the browser; covers 100 edges per object, which is enough to see the full reference graph for most objects.
- **`medium`:** Use when objects with high fan-out (large collections, caches) are truncated at 100 edges and you want broader coverage.
- **`large`:** Use for deep investigation of highly-connected objects. The larger report may be slower to open in some browsers.

Re-run with the chosen tier to regenerate the report:
```
hprof-analyzer my-dump.hprof report.html --obj-graph --report-size medium
```
