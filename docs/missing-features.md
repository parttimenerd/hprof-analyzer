# Missing / Planned Features

Catalogue of features that are planned, deferred, or known gaps vs Eclipse MAT OQL.
Updated 2026-11-20.

---

## OQL — gaps vs Eclipse MAT

### Ready to implement (small–medium scope)

| Feature | MAT uses | Notes |
|---|---|---|
| ~~Numeric literal suffixes (`100L`, `1.5F`, `2.0D`)~~ | ~~23~~ | **DONE** — lexer already strips `l/L` from ints and `f/F/d/D` from floats |
| ~~`FROM OBJECTS <id>` / `FROM OBJECTS 0x…`~~ | ~~52~~ | **DONE** — `FromSource::Object` wired in parse.rs and execute.rs |
| ~~`@referenceArray` / `@valueArray` late resolution~~ | — | **DONE** — `@referenceArray` wired in stage_runner (commit a65deb30) |

### Larger scope (need explicit go-ahead)

| Feature | MAT uses | Notes |
|---|---|---|
| ~~`get(n)` indexed element access~~ | ~~74~~ | **DONE** — `get(n)` implemented in `--collections` mode; rejected at plan time without it |
| ~~`FROM ENTRIES ClassName`~~ | — | **DONE** — `EntriesExpand` in stage_runner (see LeakCanary task 9 below) |
| `toString()` on non-String objects | ~20 | MAT calls each object's `toString()` reflectively; not statically replicable in full generality; could cover common cases (Integer, Long, Enum) |
| `${snapshot}.getClasses()` | — | Class-object iteration as a FROM source |
| `s[1:3]` array-slice syntax | — | MAT-only extension; no known high-demand use |

### Already tracked in the LeakCanary plan

| Feature | Plan task |
|---|---|
| ~~`FROM ENTRIES ClassName` (collection entry iteration)~~ | ~~Task 9~~ — **DONE** |

---

## OQL — engine improvements

| Feature | Notes |
|---|---|
| **Query plan optimizer** | Predicate pushdown, early filtering, index selection. Currently no optimizer; all queries do a full heap scan. Goal: make selective queries (by class + predicate) fast without reading the whole heap. No prior implementation work. |

---

## LeakCanary-inspired OQL attributes and functions

From `docs/notes/LEAKCANARY_INSPIRED_PROPOSALS.md`.

| Task | Feature | Status |
|---|---|---|
| 1 | Scenario tests for shipped features (`@inbounds`, `@outbounds`, `UNION`, `dominators()`, `classof()`) | **DONE** — 9 OQL-result scenario tests in `tests/scenarios.rs` covering `classof()`, `@category`, `@superclass`, `@dominatorClass`, `@dominatorDepth`, `UNION`, `@inbounds`, `@outbounds` |
| 2 | `class_super_idx` on `Graph` | **DONE** — populated in pass2/mod.rs; used by `@superclass` |
| 3 | `@category` OQL attr | **DONE** — wired in stage_runner; uses per-class category Vec |
| 4 | `@dominatorClass`, `@dominatorDepth` OQL attrs | **DONE** — wired in stage_runner |
| 5 | `@superclass` OQL attr | **DONE** — wired via `class_super_idx`; Null for java.lang.Object |
| 6 | `pathToRoot(alias)` OQL function | **DONE** — `StageOp::PathToRoot` in stage_runner |
| 7 | LIKE `%`/`_` SQL glob auto-translate | **DONE** — auto-translates when pattern has `%`/`_` but no regex metacharacters |
| 8 | `@outbounds` scenario tests | **DONE** — `oql_outbounds_expands_to_referenced_objects` in `tests/scenarios.rs`; verifies ObjRef expansion on a linked-list chain |
| 9 | `FROM ENTRIES ClassName` — collection entry iteration | **DONE** — `EntriesExpand` in stage_runner; supports ArrayList/HashMap |

---

## Report generation

| Feature | Notes |
|---|---|
| ~~**Redacted reports** (`heap report --redact`)~~ | **DONE** — `report::redact_report()` blanks data values (string contents, system-property values, constant-array values) while keeping class names, counts, retained sizes, dominator tree, and triage; exposed on all transports: `heap report --redact` CLI, `GET /report?redact=1` HTTP, `get_report({redact:true})` MCP; `redacted_report: true` flag + a Markdown banner. Leak surface + the planted-sentinel guardrail test are in `docs/notes/DESIGN_REDACTED_REPORTS.md` and `src/report/redact_report.rs`. Distinct from `hprof-redact` (which zeros the raw dump binary). Remaining: HTML renderer has no redaction banner yet (nor for `redacted_input`). |

---

## Serving architecture (unified-serving redesign)

See `docs/notes/DESIGN_UNIFIED_SERVING.md` for the full design.

| Phase | Scope | Status |
|---|---|---|
| 1 | Extract `heap_ops.rs` — one pure-logic layer behind both the `heap server` HTTP adapter and the MCP tools; eliminate the copy-pasted per-transport logic | **DONE** — `src/heap_ops.rs`; both `heap_server.rs` and `mcp/mod.rs` delegate |
| 2 | `mcp --http-port` — one process exposes MCP over stdio **and** the heap REST endpoints over HTTP, sharing a single preloaded `CachedSession` | **DONE** (2026-11-21) — `run_mcp_server_with_http`; `tests/cli_mcp_http.rs` |
| 3 | Merge `serve.rs` (lazy-analysis `server` subcommand) into `heap_ops` / disk cache | **Deferred** — different analysis lifecycle (instant OQL while full analysis runs in the background); low priority per the design doc |

### Transport follow-ups

| Feature | Notes |
|---|---|
| **MCP over HTTP/SSE** | `mcp --http-port` multiplexes *stdio MCP + REST HTTP*; it does not yet speak the **MCP protocol** over HTTP/SSE. Needed only if a client can attach to an MCP HTTP endpoint but cannot spawn a stdio child (e.g. some IDE assistants). Required by the IntelliJ-plugin "managed MCP server" mode's open question (below). |
| ~~**`explain`/`plan` on heap HTTP**~~ | **DONE** — `heap_ops::explain()`, `heap plan` CLI, `POST /plan`+`/explain` HTTP, `explain_query` MCP tool. |
| ~~**`compare_dumps` in `heap_ops`**~~ | **DONE** — `heap_ops::compare_reports()`/`compare_dumps()`, `heap compare` CLI, `GET /compare` HTTP; MCP `compare_dumps` delegates its diff to `compare_reports`. |

---

## Correctness bugs (pre-existing, deferred)

| Bug | Location | Notes |
|---|---|---|
| Byte field sign-extension | `src/query/execute.rs` `decode_field` | `0xFF` decoded as `255` (zero-extend) instead of `-1` (sign-extend). Affects all byte-field OQL output. Fix needs its own fixture-regen pass. |

---

## IntelliJ plugin integration

Integrate hprof-analyzer into `parttimenerd/intellij-profiler-plugin` (20k+
downloads; already ships JCEF + jump-to-source + file-editor pattern for `.jfr`).

### Entry points

| Action | Behaviour |
|---|---|
| Open `.hprof` file (default action) | Run `hprof-analyzer heap report <file> --format html` async (progress notification); serve resulting HTML via embedded Javalin; display in JCEF tool window |
| Open OQL shell | Serve the self-contained `hprof-analyzer-browser.html` (WASM shell) via embedded HTTP; auto-load the selected `.hprof` on open |

### Jump-to-class callback

The report HTML and OQL shell POST to the existing `JeffreyIdeJumpService`
pattern: a minimal raw HTTP server on a random port. The JS side POSTs to
`/ide/<fqn>` and the service calls `PsiUtils.navigateToClass()` — no JCEF
message router needed, matches the existing plugin architecture exactly.

### Binary bundling

Bundle platform-specific `hprof-analyzer` binaries for all major platforms
directly in the plugin JAR (same pattern as `microscope.jar` — downloaded into
`src/main/resources/binaries/` at Gradle build time from GitHub Releases):

| Platform | Target triple |
|---|---|
| Linux x86-64 | `x86_64-unknown-linux-gnu` |
| Linux aarch64 | `aarch64-unknown-linux-gnu` |
| macOS x86-64 | `x86_64-apple-darwin` |
| macOS aarch64 (Apple Silicon) | `aarch64-apple-darwin` |
| Windows x86-64 | `x86_64-pc-windows-msvc` |

Stripped release builds (~10 MB each; ~50 MB total before JAR compression).
The plugin selects the right binary at runtime from `OS + arch`.

### Changes needed in hprof-analyzer itself

#### 1 — Report: IDE jump-to-source support

The HTML report renderer needs to emit clickable class-name links for IDE
integration. When the report is served by the plugin, it injects a snippet that
wires them up:

- Wrap every class name in the report (leak suspects, histogram rows, dominator
  tree, thread stacks, root paths) in
  `<span class="ide-class-link" data-class="com.example.Foo">…</span>`
- The plugin injects a small `<script>` at serve time:
  ```js
  document.addEventListener('click', e => {
    const el = e.target.closest('.ide-class-link');
    if (el) fetch(`http://localhost:${IDE_PORT}/ide/${el.dataset.class}`, {method:'POST'});
  });
  ```
  where `IDE_PORT` is substituted by the plugin into the HTML before serving.
- When no IDE port is injected (standalone report), the spans are inert — no
  behaviour change for non-plugin users.

**Scope:** report renderer (`web/src/`) — add the CSS class to class-name cells
in histogram, leak suspects, dominator tree, thread table, root path display.
No Rust changes needed.

#### 2 — OQL shell: HTTP backend mode

The browser WASM shell currently runs the full analysis in-process (WASM).
Inside the plugin, dumps may exceed the 4 GB wasm32 linear-memory cap and we
want the full CLI analysis pipeline (not the WASM subset). The shell needs an
**HTTP backend mode**:

- A new flag/URL param `?backend=http://localhost:<port>` switches the shell
  from WASM execution to proxying all analysis calls to a local
  `hprof-analyzer serve` HTTP/JSON-RPC server.
- `hprof-analyzer serve` already exists as the MCP server (`src/serve.rs`).
  It needs a thin HTTP-JSON wrapper (or the shell speaks MCP over HTTP directly
  — MCP already supports HTTP transport).
- The plugin launches `hprof-analyzer serve <dump>` on a random port, waits
  for it to be ready, then opens the shell with `?backend=http://localhost:<port>`.
- The WASM module is still bundled in the shell HTML but skipped when
  `?backend=` is present — so the same HTML works both standalone (WASM) and
  plugin-backed (HTTP).

**Scope:**
- `web-browser/shell.js` — detect `?backend=http://localhost:<port>` URL param,
  route all analysis calls (load dump, run OQL, fetch report sections) to the
  HTTP server instead of WASM
- `src/serve.rs` — add `Access-Control-Allow-Origin: *` CORS header to all
  responses (currently absent; required because the shell page is served on a
  different port than the backend)
- Plugin side: launch `hprof-analyzer serve <dump>`, wait for `/status` 200,
  open the shell HTML with `?backend=http://localhost:<port>`

The existing `serve` routes already cover everything the shell needs:
`POST /` (OQL query), `GET /report` (full report), `GET /status`,
`POST /analyze` (run analysis). No new endpoints required.

#### 3 — MCP support: expose the IDE's heap dump to AI assistants

The plugin should also surface the loaded dump to any MCP-speaking AI assistant
in the IDE (JetBrains AI Assistant / Junie, or an external Claude/Cursor client
the developer has configured), so a developer can ask "why is this heap
leaking?" in natural language and have the model drive the same analysis tools
the human UI uses.

This reuses the `mcp --http-port` multiplexing (shipped 2026-11-21): a single
`hprof-analyzer` process serves **both** transports from **one** preloaded
`CachedSession`:

```
hprof-analyzer mcp --dump <file> --http-port <port>
```

- **stdio (MCP):** the full tool set (`load_dump`, `get_report`, `query`,
  `browse_dominators`, `get_dominator_path`, `get_retained_set`, …) for the AI
  assistant.
- **HTTP (`<port>`):** the same `heap server` REST endpoints the JCEF report and
  OQL shell already consume (`/summary`, `/report`, `/query`, `/browse`, …).

**Two integration modes, pick per IDE capability:**

| Mode | How | When |
|---|---|---|
| **Managed MCP server** | Plugin writes an MCP server entry (`command: hprof-analyzer`, `args: ["mcp","--dump",<file>,"--http-port",<port>]`) into the IDE's MCP config, or registers it via the JetBrains MCP API, so the IDE's built-in assistant can call the tools. | IDE has a native MCP client (recent IntelliJ AI Assistant / Junie). |
| **External client handoff** | Plugin shows the ready-to-paste `claude mcp add` / config snippet (with the resolved dump path + port) in the tool window, so the developer wires up their own Claude Code / Cursor. | No native MCP client, or developer prefers their own agent. |

**Jump-to-source still works for the AI path:** when the model names a class in
its answer, the plugin can render it through the same `.ide-class-link` /
`POST /ide/<fqn>` mechanism used by the HTML report (feature #1), so "take me to
`com.example.LeakyCache`" navigates in the editor.

**Scope:**
- hprof-analyzer: **none** — `mcp --http-port` already does the multiplexing.
  (The HTTP side serves the dump preloaded at startup; the plugin owns one
  process per open `.hprof`, so the "MCP `load_dump` of a different dump only
  switches stdio" caveat does not apply here.)
- Plugin side: launch/lifecycle (reuse `JeffreyLauncher.kt` pattern), MCP-config
  registration (managed mode) or snippet display (handoff mode), and surfacing
  the stdio handle to the IDE's assistant.

**Open question (plugin-side, not hprof-analyzer):** whether the IDE's assistant
consumes MCP over the child's stdio directly or whether we also want an MCP
HTTP/SSE transport. `mcp --http-port` currently multiplexes *stdio MCP + REST
HTTP*; it does **not** yet speak the MCP protocol itself over HTTP/SSE. If a
target IDE can only attach to an MCP *HTTP* endpoint (not spawn a stdio child),
that would be a follow-up on the hprof-analyzer side (MCP-over-HTTP transport),
tracked separately below.

---



| File | Reuse |
|---|---|
| `WebViewWindow.java` | JCEF wrapper with CEF retry loop — drop-in |
| `JeffreyIdeJumpService.kt` | Jump-to-source HTTP server — reuse as-is, change POST path |
| `JFRFileEditorProvider.kt` / `JFRFileEditor.kt` | File-editor pattern — copy for `.hprof` |
| `PsiUtils.kt` | `navigateToClass`, `hasClass`, `hasMethod` — already implemented |
| `JeffreyLauncher.kt` | Process lifecycle (start/reuse/port management) — template for launching `hprof-analyzer` |

### Architecture

- **Stack:** Kotlin + Java (JCEF wrapper must be Java — same JCEF-in-Kotlin bug)
- **File type:** `HprofFileEditorProvider` with `HIDE_DEFAULT_EDITOR` policy
- **Report generation:** background thread + IntelliJ progress indicator; tool window opens once HTML is ready
- **OQL shell:** instant — just serve the pre-built `hprof-analyzer-browser.html`; pass dump path via URL param or postMessage
- **plugin.xml additions:** file-type handler for `.hprof`, tool window, editor popup action "Analyze heap dump"

---

## Redact feature polish (post-landing todos)

| Item | Notes |
|---|---|
| Browser E2E test for Redact button | Drop dump → click Redact → download → drag redacted file back → verify redacted banner appears |
| README / docs for `redact` subcommand | Cover `hprof-redact` binary, lean vs complete modes, `0xDE` marker format |

---

## REPL dispatch bugs

| Issue | Detail | Priority | Status |
|---|---|---|---|
| ~~**`!views` broken in interactive mode**~~ | ~~`"views"` is in `META_CMDS` but has no arm in `run_repl`'s match block~~ | Bug | **FIXED** |
| ~~**`!history` broken in piped mode**~~ | ~~Falls through to `unknown command` in piped mode~~ | Minor | **FIXED** — now says "not supported in non-interactive mode" |
| ~~**Missing Tab-completions**~~ | ~~`"h"` and `"reachable-only"` absent from `META_CMDS`~~ | Minor | **FIXED** |
| ~~**`"rename"` duplicated in did-you-mean list**~~ | ~~Appears twice in `handle_meta`'s CMDS list~~ | Cosmetic | **FIXED** |

---

## REPL error message quality

| Location | Input | Actual behaviour | Issue | Status |
|---|---|---|---|---|
| ~~`handle_set`~~ | ~~`!set null` (no value)~~ | ~~Silently resets~~ | ~~Looks like success~~ | **FIXED** — shows current value + usage hint |
| ~~`!history`~~ | ~~`!history 0`~~ | ~~Shows 0 rows~~ | ~~Confusing~~ | **FIXED** — rejects non-positive with error |
| ~~`!history`~~ | ~~`!history -1`~~ | ~~Silent fallback to 20~~ | ~~Silent wrong-input~~ | **FIXED** — rejects with error |
| ~~`handle_filter`~~ | ~~`!filter /unclosed`~~ | ~~"hint:" label~~ | ~~Too weak~~ | **FIXED** — now "invalid pattern:" |
| ~~`!count` wrap path~~ | ~~`!count java.lang.String extra words`~~ | ~~Caret on auto-generated OQL~~ | ~~Confusing~~ | **FIXED** — guards before constructing OQL |
| ~~Main query path~~ | ~~Any parse error~~ | ~~Double-wrapping ANSI~~ | ~~Rendering artifacts~~ | **FIXED** — detects "parse error:" prefix, skips outer wrap |
| ~~`!plan` with no args~~ | ~~`!plan`~~ | ~~"parse error: empty query"~~ | ~~No usage hint~~ | **FIXED** — shows `usage: !plan [--raw] [--verbose] [--json] <oql query>` |

---

## Open OQL engine bugs (from ALL_BUGS_LIST.md)

Bugs deferred from prior slices. Fix cost varies; listed in rough priority order.

| ID | Issue | Priority | Status |
|---|---|---|---|
| — | ~~**`FROM "byte[]"` regex escaping**~~ — ~~fails with "unclosed character class"~~ | High | **DONE** — `compile_from_regex` already escapes invalid patterns as literal fallback |
| — | ~~**LIKE NULL handling**~~ — ~~incorrect row-counts from LIKE filters involving NULL~~ | High | **FIXED** — NULL on either side returns UNKNOWN (SQL three-valued logic) |
| — | ~~**Boolean literals `true`/`false` not tokenized**~~ — ~~`WHERE true` → parse error~~ | Medium | **DONE** — already tokenized as `Value::Bool(true/false)` and `Predicate::Bool(true/false)` |
| — | ~~**Multiple UNION**~~ — ~~second `UNION` → parse error~~ | Medium | **DONE** — `union_with_all.repeated()` handles chains of any length |
| — | ~~**Array subscript always `Null`**~~ — ~~`array_index` need flag armed~~ | Medium | **DONE** — `x[n]` works; `get(n)` added in `--collections` mode |
| — | **`FROM INSTANCEOF <interface>` returns 0 rows** — HPROF CLASS_DUMP records don't include interface metadata | Medium | Out of scope (fundamental format limitation) |
| — | ~~**`--query-file` parse errors show no filename/line prefix**~~ | ~~Medium~~ | **DONE** — plan errors from `parse_plan_queries` now include `--query-file '<file>' line <N>` prefix |
| — | **Cartesian product FROM** (`FROM o1, o2`) | Low | Open |
| — | **Subquery in SELECT clause** | Low | Open |
| — | **CROSS JOIN** | Low | Open |
| — | ~~**`UNION ALL` syntax**~~ | ~~Low~~ | **DONE** — `UNION ALL` sets `query.union_all = true`; deduplication is distinct from UNION |
| — | ~~**Expressions in ORDER BY**~~ | ~~Low~~ | **DONE** — `OrderBy.keys` changed from `Vec<(Attr, SortDir)>` to `Vec<(Expr, SortDir)>`; parser and all execution sites updated |
| — | ~~**Nested CASE WHEN**~~ | ~~Low~~ | **DONE** — parses correctly (`nested_case_when_parses` test) |
| — | ~~**`SELECT` inside `IN` predicate**~~ | ~~Low~~ | **DONE** — `attr IN (SELECT ...)` subquery form is parsed and planned (plan.rs `Predicate::InSubquery`) |
| — | ~~**String concat operator `\|\|`**~~ | ~~Low~~ | **DONE** — parsed as `CONCAT(lhs, rhs)` method call (parse.rs line ~754) |
| — | **`ENTRIES` expansion in subqueries** | Low | Open |
| — | ~~**`@root` boolean attribute**~~ | ~~Low~~ | **DONE** — `Attr::IsGcRoot`; returns `true` for GC-root objects, `false` for non-roots; auto-escalates like `@GCRoots` |
