# Design: Redacted Reports (`heap report --redact`)

> Status: **IMPLEMENTED 2026-11-22** (CLI + HTTP + MCP). `report::redact_report()`
> + `heap report --redact` / `GET /report?redact=1` / `get_report({redact:true})`,
> guarded by the planted-sentinel test `redacted_report_suppresses_values`.
> Distinct from `hprof-redact` (which zeros the raw **dump**); this redacts the
> **report output** so a production heap report can be shared without leaking
> data values, while keeping everything needed to reason about a leak: class
> names, counts, retained sizes, dominator tree, triage signals, thread stacks.
> Remaining: no HTML redaction banner yet.

## Goal

`hprof-analyzer heap report <dump> --redact [--format html|json|md]` produces a
report that is structurally identical to the normal report but with every
field that carries **actual heap data content** blanked, replaced with a fixed
placeholder (`"<redacted>"` for strings, `0`/`null` for sampled scalar values),
and a top-level `redacted: true` flag + a visible banner.

What MUST survive (needed for diagnosis, not sensitive):
- Class names, field names, method names, thread names, source files
- Instance/retained/shallow sizes, counts, histograms
- Dominator tree, retention paths, GC roots, leak suspects
- Triage signals, collection fill ratios, waste stats (the *numbers*, not the
  sampled values)

What MUST be removed (actual data values):
- Decoded `String` contents
- Collection entry / map key-value sample contents
- Sampled scalar field values, repeated-array values

## Leak surface — every value-bearing field in `Report`

Enumerated from `src/report/model.rs` + `src/pass2/model.rs`. **Implementation
MUST cover all of these; a miss is a data leak.** Verify with a test that greps
the rendered output for a known-secret value planted in a fixture.

| Location | Field | Redaction |
|---|---|---|
| `pass2::DupStrings.top_duplicated[]` | `DupStringSample.text` | → `"<redacted>"` (keep `count`, `len`, `wasted_bytes`) |
| `pass2::DupStrings.top_by_length[]` | `DupStringSample.text` | → `"<redacted>"` |
| `report::model` `{key, value}` pair (line ~208) | `value: String` (and possibly `key`) | → `"<redacted>"` (key may be a field name — KEEP; value is data — blank) — **CHECK which this is before deciding** |
| `report::model RepeatedValueArray` (~1366) | `value: i64` | → `0` (keep `count`, `len`, class) |
| `report::model` GC-thread-local root samples (~1008) | sampled object identity/value | keep class + size, blank any value text |
| `ValueTypeShare` / `dominant_value_type` (~1672/1694) | `String` = runtime **class name**, NOT data | KEEP (it's a type name) |
| `duplicate_classes` / `DuplicateClass` | class names + loader names | KEEP (structural) |

Also audit (grep for `text`, `value`, `sample`, `preview`, `content`, `decoded`
in `src/report/`): any field added since this note. The redaction function
should be a single `redact_report(&mut Report)` in `src/report/` so new
value-bearing fields are added in one obvious place.

## Approach

1. `pub fn redact_report(r: &mut Report)` in `src/report/mod.rs` (or a new
   `src/report/redact_report.rs`): walk the owned `Report`, blank each field in
   the table above, set `r.redacted = true` (new bool field, `#[serde(default)]`
   so cached reports deserialize).
2. CLI: add `--redact` to `HeapCmd::Report`; after building/loading the
   `Report`, clone + `redact_report(&mut clone)` before rendering. Do NOT write
   the redacted form back to the on-disk cache (cache always holds the full
   report; redaction is a render-time transform).
3. Renderers: HTML/MD show a "⚠ Redacted report — data values suppressed"
   banner when `redacted`. JSON carries `"redacted": true`.
4. **(Done)** exposed on all transports via `heap_ops::report_section_redacted`
   (clones the report, runs `redact_report`, then sections it): `GET
   /report?redact=1` and the MCP `get_report({redact: true})` param.

## Test (the important part)

`tests/` integration: build a synthetic dump (or reuse a fixture) containing a
recognizable sentinel string value (e.g. `"SUPERSECRET-DO-NOT-LEAK"`), render
`heap report --redact --format json` AND `--format html` AND `--format md`,
assert the sentinel does **not** appear in any output, while a known class name
(e.g. `java.lang.String`) DOES. This catches both missed fields and
format-specific rendering paths.

## Why not done autonomously yet

The failure mode is a silent data leak (one un-redacted field). The field
enumeration above needs to be confirmed exhaustive against the *current*
`Report` (fields may have been added), and the grep-for-sentinel test is what
makes it trustworthy. Worth a focused implementation session with that test
written first (TDD).
