# Eval-loop findings — hprof MCP/heap gaps surfaced by blind diagnosis

Loop: generate heap dumps from `leak-demos/` (ground truth in each demo header) →
drive the `hprof` MCP via opencode (`localproxy/gpt-5-mini` through a local
`translate_proxy.py` on :6656 that rewrites `max_tokens`→`max_completion_tokens`)
→ compare the model's blind diagnosis to ground truth → fix the gap in
hprof-analyzer. See `run_one.sh` / `run_all.sh` / `opencode.json`.

## Environment notes (so this is reproducible)
- External models: litellm host = NXDOMAIN (VPN-gated, unreachable here);
  opencode-go cloud = 402 no credits; localproxy :6655 chat path is
  `/openai/v1/chat/completions` (NOT `/openai/chat/completions`), and its GPT-5/6
  models reject `max_tokens` (need `max_completion_tokens`). Fixed with the tiny
  `translate_proxy.py` shim; harness `opencode.json` points at it.
- Pre-warm each dump's cache (`heap report <dump> --section triage`) before an
  eval so the MCP `load_dump` is ~1s not minutes.

## GAP 1 (confirmed) — static-field leak misattributed to `Thread`; owner not named

Dump: `01_StaticCacheLeak` (ground truth: static `CACHE` HashMap in
`_01_StaticCacheLeak` retains ~100% of heap).

What the tools say (`get_report({section:leaks})`):
- Headline suspect = **`java.lang.Thread` — retained 62 MB**. `root_path` is a
  single `[Thread]` entry (`root_type_label: "Thread"`). The real culprit — a
  **static field** holding the HashMap — is never named. `path` is
  Thread → HashMap(6449) → Node[], treating the main thread as the retainer.
- No `via_field` / owning-class on the root-path step (the LeakCanary
  `category`/field plumbing doesn't populate the static-field name here).
- The analyst must guess the owner via raw `SELECT @displayName FROM
  java.lang.Class` — gpt-5-mini floundered exactly here and never produced a
  final VERDICT/CULPRIT/MECHANISM.
- `find_incoming_references` (which would show the static-field edge) requires
  `with_graph=true`, which the default `load_dump` does NOT build → extra
  friction for the single most common leak pattern.

Fix direction (to verify + implement):
1. Static-field-held accumulation points should be surfaced as the suspect (or
   at least named in the root path as `<Class>.<field>`), not collapsed to the
   dominating `Thread`.
2. The root path / suspect should name the owning class + static field for a
   GC-root-via-static-field retention, so "which static field leaks?" is
   answered without manual Class-table spelunking.

CONFIRMED with graph (`find_incoming_references(6449)`): the leaking HashMap has
exactly 2 referrers — `java.lang.Thread`(4755, a transient stack local) and
`java.lang.Class`(6451, edge_count 1 = a STATIC FIELD). The static-field Class
ref is the real durable GC root; the Thread is a red herring (main still on the
stack at dump time). Yet the report's suspect/root_path = "Thread".
Additional facet: `inspect_object` on the Class(6451) shows inbound refs but
NOT the class's own name nor its static-field table — so even after finding the
Class you can't read "static field CACHE → HashMap" from the tools.

(More gaps to be appended as dumps 04/05/21/… are evaluated.)
