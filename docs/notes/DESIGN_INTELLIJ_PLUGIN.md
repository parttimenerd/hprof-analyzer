# IntelliJ Plugin Integration — Implementation Spec

> **Status:** implementation-ready plan (2026-11-22). No code written yet; the
> plugin lives in a **separate repo** (`parttimenerd/intellij-profiler-plugin`),
> not cloned on this machine. This spec is grounded in the working sibling
> plugin `parttimenerd/jstall-intellij-plugin` (same author, same IntelliJ
> patterns) and the hprof-analyzer surface shipped as of 2026-11-22
> (`mcp --http-port`, `heap server`, `--format html`, redacted reports).
>
> Read alongside `docs/missing-features.md` → "IntelliJ plugin integration"
> (the high-level entry points) and `docs/notes/DESIGN_UNIFIED_SERVING.md`
> (why `mcp --http-port` exists — it is the backbone of this integration).

## 0. Goal

Open a `.hprof` dump in IntelliJ → get the full hprof-analyzer HTML report in a
JCEF tool window, with class names that jump to source on click, **and** expose
the same dump to the IDE's AI assistant over MCP — from one launched process
(`mcp --dump <file> --http-port <port>`), which serves the REST endpoints
(report HTML/JSON, OQL) over HTTP and the MCP tools over stdio simultaneously.

## 1. Reference code to mirror (jstall-intellij-plugin)

Every pattern below is already working in the JStall plugin; copy its structure.

| JStall file | Role | hprof analogue |
|---|---|---|
| `JStallRecordingEditorProvider.kt` | `FileEditorProvider`, `accept()` by extension + content sniff, `getPolicy()=HIDE_DEFAULT_EDITOR` | `HprofFileEditorProvider` — accept `*.hprof` / `*.hprof.gz` / `*.hprof.zip` |
| `JStallRecordingEditor.kt` | `FileEditor` that launches the CLI in the background (`AppExecutorUtil`) and shows output in a panel (`invokeLater` to EDT) | `HprofReportEditor` — launch `heap report --format html`, show the HTML in a JCEF browser instead of a console |
| `ide/IdeJumpServer.kt` | `@Service(APP)` `HttpServer` on `127.0.0.1:<port>`, `POST /ide/<fqn>` → `JavaPsiFacade.findClass` → `OpenFileDescriptor(...).navigate(true)`, CORS headers, 1-based→0-based line | **reuse almost verbatim** — only the POST path payload differs (fqn, optionally line) |
| `ide/IdeJumpStartupActivity.kt` | `ProjectActivity` that starts the jump server if enabled | same |
| `settings/JStallSettings.kt` + `JStallConfigurable.kt` | `@Service` persistent state + `applicationConfigurable` UI (binary path, ports, flags) | `HprofSettings` — binary path override, jump-server port, MCP auto-register toggle |
| `actions/ActionUtils.kt` `runJStallCaptured` / `analyzeRecording` | resolve + launch the bundled binary with args, capture output, exit code | `runHprof(args…)` — same shape |
| `showFlameWindow` (ActionUtils) | registers a `ToolWindow`, shows **HTML** content | precedent that JCEF-HTML display is fine in this plugin family |
| `plugin.xml` `<extensions>` | `fileEditorProvider`, `applicationService`, `applicationConfigurable`, `notificationGroup`, `iconProvider` | same set |

**JCEF note:** per `docs/missing-features.md` the JCEF wrapper component must be
written in **Java** (a known JCEF-in-Kotlin issue in the existing plugin family);
keep `HprofReportEditor`'s `JBCefBrowser` host in Java, call it from Kotlin.

## 2. Entry points

### 2a. Open `.hprof` → HTML report in JCEF (human path)

1. `HprofFileEditorProvider.accept(project, file)`: extension is `hprof`
   (or `.hprof.gz`/`.hprof.zip`) — optionally sniff the NUL-terminated
   `JAVA PROFILE 1.0.` header prefix for robustness (mirror `isJStallRecording`).
   `getPolicy() = HIDE_DEFAULT_EDITOR`.
2. `HprofReportEditor` (background thread, progress via `NotificationGroup`
   "hprof-analyzer Notifications"):
   - Resolve the binary (§4).
   - Run `hprof-analyzer heap report <file> --format html` → capture stdout
     (the self-contained HTML). First run is slow (5–15 min on big dumps):
     show an indeterminate progress bar in the panel until the HTML is ready;
     subsequent runs hit the on-disk cache (`<dump>.hprof-cache/`) and return
     in ~1 s.
   - **Inject the IDE jump port** into the HTML before handing it to JCEF so
     the report's `IdeBtn` activates (see §3 for the exact one-line injection
     and why nothing else is needed).
   - `invokeLater` → load the HTML into `JBCefBrowser.loadHTML(html)`.
3. Errors (bad exit code, binary missing) → show in the panel + a balloon.

### 2b. MCP server for the AI assistant (AI path)

On opening the dump (or via an explicit action "Analyze heap with AI"):

- Launch **one** process: `hprof-analyzer mcp --dump <file> --http-port <port>`
  (this is the multiplexing shipped 2026-11-21: MCP on stdio **and** the heap
  REST API on `<port>`, sharing one preloaded `CachedSession`). Reuse the
  JStall process-lifecycle pattern (start / track / dispose with the editor).
- Two modes, gated by a setting (default = handoff, least surprising):

  | Mode | Action |
  |---|---|
  | **Managed** | Register the stdio child as an MCP server with the IDE's assistant (JetBrains AI Assistant / Junie MCP API, or write the IDE's MCP config entry: `command=<binary>`, `args=["mcp","--dump",<file>,"--http-port",<port>]`). |
  | **Handoff** | Show a tool-window panel with a ready-to-paste snippet: `claude mcp add hprof -- <binary> mcp --dump <file>` (and the HTTP port for curl). |

- The HTTP side (`<port>`) is what the JCEF report/OQL-shell talk to (§2c);
  the stdio side is what the assistant talks to. One process, both consumers.

### 2c. (Optional, later) OQL shell tab

Serve the pre-built `hprof-analyzer-browser.html` (WASM shell) in a second JCEF
tab, opened with `?backend=http://127.0.0.1:<port>` so it proxies to the HTTP
server instead of running WASM (dumps can exceed the wasm32 4 GB cap). The
`?backend=` auto-connect already exists (`feat(shell): auto-connect to HTTP
backend via ?backend= URL param`, commit 3825ebb4), and `serve`/`heap server`
already send permissive CORS. This is additive; ship 2a+2b first.

## 3. hprof-analyzer-side: clickable class links — ALREADY DONE

Jump-to-source is **already implemented in this repo** (commit c3d39b37,
`IdeBtn` in `web/src/App.tsx:1643`). Verified 2026-11-22:

- `IdeBtn({cls})` renders a `↗` "Jump to source" button next to the class name
  in **every** class cell — Top Consumers, leak suspects, histogram/retained
  tables, dominator analysis, duplicate classes, resource handles, etc.
- It reads `window.__HPROF_IDE_PORT__` and, on click,
  `fetch(\`http://localhost:${port}/ide/${encodeURIComponent(cls)}\`, {method:"POST"})`.
- When `__HPROF_IDE_PORT__` is **absent** (standalone report), `IdeBtn` returns
  `null` — no button, no network, offline guarantee intact. So the
  `html_selfcontained` offline test already passes with this in place (the URL
  is built at runtime from a JS var, never a static reference).

**Therefore the ONLY hprof-analyzer-side requirement is met.** The POST path is
`/ide/<fqn>` with an **empty body** (no line number) — which the JStall
`IdeJumpServer` already handles (it defaults `line<=0` to the class
declaration). The plugin does not need any new report-renderer change.

### What the plugin must do (the whole class-link integration)

Inject the jump-server port into the served HTML before JCEF loads it, so
`IdeBtn` activates. The report HTML is a single self-contained document; the
cleanest injection is to append one script tag right before `</head>` or at the
top of `<body>`:

```kotlin
val wired = html.replaceFirst("<body>", "<body><script>window.__HPROF_IDE_PORT__=$port;</script>")
jcefBrowser.loadHTML(wired)
```

where `$port` is the `IdeJumpServer` port. That is the entire wiring — the
report's own `IdeBtn` handlers do the rest.

## 4. Binary resolution & bundling

Mirror JStall's resolver precedence:
1. Settings override path, if set and executable.
2. Bundled platform binary under `src/main/resources/binaries/<triple>/hprof-analyzer[.exe]`
   (downloaded at Gradle build time from GitHub Releases, same as JStall's
   femtocli/microscope pattern). Select by `SystemInfo.OS + os.arch`:
   - `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`
   - `x86_64-apple-darwin`, `aarch64-apple-darwin`
   - `x86_64-pc-windows-msvc`
   Stripped release builds ≈10 MB each (~50 MB total pre-JAR-compression).
   On first use, copy the bundled binary to a cache dir and `chmod +x` (JAR
   entries are not executable in place).
3. `hprof-analyzer` on `PATH` as a fallback.

Launch via `com.intellij.execution.configurations.GeneralCommandLine`
(JStall uses its own `runJStallCaptured`; `GeneralCommandLine` +
`CapturingProcessHandler` is the idiomatic capture path). For the long-lived
`mcp --http-port` process use `OSProcessHandler` and dispose it with the editor.

## 5. plugin.xml (new registrations)

```xml
<depends>com.intellij.modules.platform</depends>
<depends>com.intellij.modules.lang</depends>
<depends>com.intellij.java</depends>   <!-- PSI class navigation -->
<extensions defaultExtensionNs="com.intellij">
  <notificationGroup id="hprof-analyzer Notifications" displayType="BALLOON"/>
  <applicationService serviceImplementation="…ide.IdeJumpServer"/>
  <applicationService serviceImplementation="…settings.HprofSettings"/>
  <applicationConfigurable parentId="tools" instance="…settings.HprofConfigurable"
      id="…HprofConfigurable" displayName="hprof-analyzer"/>
  <fileEditorProvider implementation="…HprofFileEditorProvider"/>
  <postStartupActivity implementation="…ide.IdeJumpStartupActivity"/>
</extensions>
<actions>
  <action id="Hprof.AnalyzeWithAI" class="…actions.HprofMcpAction"
          text="Analyze Heap with AI"
          description="Launch hprof-analyzer MCP (shared HTTP+stdio) for this dump"/>
</actions>
```

## 6. Settings (`HprofSettings` persistent state)

- `binaryPathOverride: String = ""`
- `ideServerEnabled: Boolean = true`, `ideServerPort: Int = <default>`
- `mcpHttpPort: Int = 0` (0 = pick a free port per dump)
- `mcpMode: enum { Handoff, Managed } = Handoff`
- `reportDetail: enum = default` (maps to `--detail`)

## 7. Build order (smallest shippable increments)

0. **hprof-analyzer repo: nothing required** for jump-to-source — `IdeBtn`
   (§3) is already shipped. (A later task, if OQL-shell tab is wanted, is only
   plugin-side.)
1. **plugin repo (clone `parttimenerd/intellij-profiler-plugin` first):**
   `HprofFileEditorProvider` + `HprofReportEditor` (JCEF) running
   `heap report --format html`, with `window.__HPROF_IDE_PORT__` injected into
   the HTML (§3). Copy `IdeJumpServer`/`IdeJumpStartupActivity` verbatim; rename
   the notification group. Verify click-to-source end to end.
2. **plugin repo:** `HprofMcpAction` + process lifecycle for
   `mcp --dump --http-port`; handoff-mode snippet panel first, managed-mode
   registration second.
3. Binary bundling (Gradle download task) + multi-platform selection.
4. (Later) OQL shell JCEF tab with `?backend=`.

## 8. Open questions (flagged, not resolved)

- **MCP transport:** `mcp --http-port` multiplexes *stdio MCP + REST HTTP*; it
  does **not** speak the MCP protocol over HTTP/SSE. If the target IDE assistant
  can only attach to an MCP **HTTP/SSE endpoint** (cannot spawn a stdio child),
  that is a follow-up **on the hprof-analyzer side** (add an MCP-over-HTTP/SSE
  transport), tracked in `docs/missing-features.md` → Serving architecture.
  Confirm the IDE assistant's MCP client capability before committing to Managed
  mode.
- **JCEF availability:** JCEF is bundled in IntelliJ but can be disabled; detect
  `JBCefApp.isSupported()` and fall back to opening the HTML in an external
  browser (JStall's flame window assumes JCEF — check how it degrades).
- **Windows:** no `chmod`; the `.exe` is directly executable. The jump server is
  loopback-only on all platforms.
- **`localhost` vs `127.0.0.1` mismatch:** `IdeBtn` fetches
  `http://localhost:${port}/ide/...`, but the JStall `IdeJumpServer` binds
  `InetSocketAddress("127.0.0.1", port)`. On hosts where `localhost` resolves to
  IPv6 `::1` first, the fetch can miss a v4-only bound server. Mitigate by
  binding the jump server to `localhost` (both families) or `0.0.0.0`-loopback,
  OR change `IdeBtn` to `127.0.0.1` (one-line hprof-analyzer change). Decide
  during step 1 — verify a click actually navigates before building further.
