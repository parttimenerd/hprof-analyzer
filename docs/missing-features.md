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
| `toString()` on non-String objects | ~20 | MAT calls each object's `toString()` reflectively; not statically replicable in full generality; could cover common cases (Integer, Long, Enum) |
| `${snapshot}.getClasses()` | — | Class-object iteration as a FROM source |
| `s[1:3]` array-slice syntax | — | MAT-only extension; no known high-demand use |

### Already tracked in the LeakCanary plan

| Feature | Plan task |
|---|---|
| `FROM ENTRIES ClassName` (collection entry iteration) | Task 9 |

---

## OQL — engine improvements

| Feature | Notes |
|---|---|
| **Query plan optimizer** | Predicate pushdown, early filtering, index selection. Currently no optimizer; all queries do a full heap scan. Goal: make selective queries (by class + predicate) fast without reading the whole heap. No prior implementation work. |

---

## LeakCanary-inspired OQL attributes and functions

From `docs/superpowers/plans/2026-11-04-leakcanary-inspired-features.md`.

| Task | Feature | Key files |
|---|---|---|
| 1 | Scenario tests for shipped features (`@inbounds`, `@outbounds`, `UNION`, `dominators()`, `classof()`) | `tests/scenarios.rs` |
| 2 | `class_super_idx` on `Graph` | `src/pass2/model.rs`, `src/cache.rs` (bump VERSION) |
| 3 | `@category` OQL attr | `ast.rs`, `execute.rs`, `stage_runner.rs` |
| 4 | `@dominatorClass`, `@dominatorDepth` OQL attrs | same |
| 5 | `@superclass` OQL attr (depends on task 2) | same |
| 6 | `pathToRoot(alias)` OQL function | `ast.rs`, `plan.rs`, `stage_runner.rs` |
| 7 | LIKE `%`/`_` SQL glob auto-translate (currently errors) | `src/query/execute.rs` |
| 8 | `@outbounds` scenario tests | `tests/scenarios.rs` |
| 9 | `FROM ENTRIES ClassName` — collection entry iteration | new `src/query/collection_entries.rs`, `ast.rs`, `plan.rs`, `stage_runner.rs` |

---

## Report generation

| Feature | Notes |
|---|---|
| **Redacted reports** (`heap report --redact`) | Produce a self-contained HTML/JSON/MD report that shows class names, counts, retained sizes, dominator tree, and triage signals, but suppresses all actual field values, collection entry contents, and string values. For sharing reports from production heaps without leaking data. Distinct from `hprof-redact` (which zeros the raw dump binary). |

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
| — | **`--query-file` parse errors show no filename/line prefix** | Medium | Open |
| — | **Cartesian product FROM** (`FROM o1, o2`) | Low | Open |
| — | **Subquery in SELECT clause** | Low | Open |
| — | **CROSS JOIN** | Low | Open |
| — | **`UNION ALL` syntax** | Low | Open |
| — | **Expressions in ORDER BY** | Low | Open |
| — | **Nested CASE WHEN** | Low | Open |
| — | **`SELECT` inside `IN` predicate** | Low | Open |
| — | **String concat operator `\|\|`** | Low | Open |
| — | **`ENTRIES` expansion in subqueries** | Low | Open |
| — | **`@root` boolean attribute** | Low | Open |
