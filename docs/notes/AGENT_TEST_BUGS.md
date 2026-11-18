# Bugs found by agent testing — 2026-11-16

Testing setup: four Claude Code agents ran headless (`claude -p`) against the installed
`hprof-analyzer` binary (v0.3.1 at `/opt/homebrew/bin/hprof-analyzer`), two via the MCP server,
one via the `heap` CLI subcommands, one MCP + OQL. Agent transcripts/reports:
`/var/folders/4c/yz9tjkbx3k980r1ftjqp_xrh0000gn/T/opencode/agent-test/report-{A,B,C,D}.md`.
A raw JSON-RPC probe script is at the same dir (`mcp_probe.py`).

Environment caveats for whoever takes over:
- Repo source is 0.3.4; the only working binary is the stale 0.3.1 homebrew install.
  Reproduce against 0.3.4 before assuming anything below still applies.
- Local `cargo build --release` hung >10 min (Mac disk 96% full); ThinkStation unreachable
  (ssh timeout) at test time. Builds must happen on the ThinkStation per CLAUDE.md.
- Fixture dumps used (all with warm `<dump>.hprof-cache/`):
  `tests/fixtures/dump_4_philosophers.hprof`, `tests/fixtures/dump_2_scala-doku.hprof`,
  `tests/fixtures/dump_7_gauss-mix.hprof`, `demo/spring-leak-clinic/leak-clinic.hprof`.

---

## 1. Project-scope `.mcp.json` points at a nonexistent binary → MCP fails with ENOENT

The repo's `.mcp.json` (the setup the README/CLAUDE.md tell people to use) references
`target/release/hprof-analyzer`, which doesn't exist unless you happen to have built it.

Repro (from repo root):
```
claude mcp list
# hprof-analyzer: .../target/release/hprof-analyzer mcp - ✘ Failed to connect —
#   ENOENT: no such file or directory
```
Meanwhile a *second*, separate project-scoped server named `hprof` exists in
`~/.claude.json` (repo + Downloads projects only) pointing at the homebrew binary. So a
Claude agent in the repo sees two similarly-named servers, one broken and one working, and
agents in any other directory see neither. Consolidate: fix `.mcp.json` to resolve a
installed binary and drop the duplicate `hprof` entry.

## 2. Installed 0.3.1 MCP server is missing the graph-traversal tools entirely

`tools/list` from `/opt/homebrew/bin/hprof-analyzer mcp` returns only:
`browse_dominators, get_histogram, get_oql_docs, get_report, get_session_info,
inspect_object, list_views, load_dump, query, redact`.

Missing vs. CLAUDE.md's advertised workflow: `get_dominator_path`, `get_retained_set`,
`get_outbound_references`, `find_incoming_references`, `find_incoming_references_by_class`.
Agents therefore cannot walk retention chains via MCP at all (only
`browse_dominators` downward). If these exist in 0.3.4 source, the release/install story is
lagging the code badly.

Repro:
```
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n' \
  | hprof-analyzer mcp
```

## 3. MCP server answers pipelined requests out of order (stateful-tool race)

The server processes `tools/call` requests concurrently and can answer out of order. With
`load_dump` (id=2) and `get_session_info` (id=3) written back-to-back into the same stdin
pipe, `get_session_info` was answered *before* `load_dump` finished and reported
`"loaded": false` even though the load then succeeded. A client that pipelines (or a future
concurrent MCP client) gets incoherent session state for a server whose whole model is
"one session, one loaded dump".

Repro (three JSON-RPC lines on one stdin write, observe response order):
```
printf '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}\n{"jsonrpc":"2.0","method":"notifications/initialized"}\n{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"load_dump","arguments":{"path":"<DUMP>"}}}\n{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"get_session_info"}}\n' | hprof-analyzer mcp
# observed: id=3 ("loaded": false) is written BEFORE id=2 ("Loaded: ...")
```
Expected: strict in-order request processing (or document/serialize stateful calls).

## 4. Off-heap `DirectByteBuffer` indicator badly misleads agents (read-only mmap treated as OOM risk)

All four agents seized on the triage/indicators "off-heap" signal as *the* OOM cause:
"134–141 MB of native memory … most likely cause of an OS-level OOM kill" (agents even
blamed Netty arenas and recommended `-XX:MaxDirectMemorySize`). The actual data is one
file-backed, read-only *mapped* buffer (the Renaissance/classpath JAR) plus 8-byte
administrative buffers:

Verified on `tests/fixtures/dump_4_philosophers.hprof`:
```
hprof-analyzer query tests/fixtures/dump_4_philosophers.hprof \
  --query 'SELECT @objectId, capacity, position, limit FROM java.nio.DirectByteBuffer'
# one buffer: capacity=140845582, position=0, limit=28  (mmap of the shared fat jar)
# seven buffers: capacity=8
```
An mmap'd jar inflates RSS but is backed by the jar file and is not native OOM fuel the way
allocated direct memory is. The indicator should split allocated vs. file-mapped (or at
least read-only/`limit==capacity` vs read-only buffers), otherwise agents confidently
produce wrong OOM remediation advice. Affects `dump_2_scala-doku.hprof` and
`dump_7_gauss-mix.hprof` identically (same jar → same sums: 140,862,014 /
140,894,781 bytes).

## 5. Triage report contradicts itself in one session (clinic dump)

`get_report({section:"triage"})` on `demo/spring-leak-clinic/leak-clinic.hprof` emits, in the
same array: a **critical** `headline-retainer` ("ReportEngine … retains 15.7 MB, 38.3% of
reachable heap") followed by a **concentration: diffuse — "no suspect exceeds the
threshold"** signal. Either the threshold is wrong or the two signals disagree; agents have
to guess. Repro: load clinic dump via MCP, call `get_report` section triage.

## 6. `heap query` and top-level `query` use different flags and ordering rules

- Top-level: `hprof-analyzer query <DUMP> --query '<OQL>'`
- Heap:     `hprof-analyzer heap query --oql '<OQL>' <DUMP>` (flag must precede `<DUMP>`;
  wrong order → `error: unexpected argument '--query' found` / `tip: to pass '--query' …`).

Agent C tripped over this twice. One flag name (`--oql` or `--query`) and one accepted
ordering would fix it.

## 7. OQL: class globs do not match inner classes

`SELECT COUNT(*) FROM java.util.zip.ZipFile*` returns **0** although there are 19
`java.util.zip.ZipFile$Source` instances in the dump (and agent D confirmed the doc says
globs don't match `$`-inner classes). This silently hides real classes and steers agents to
wrong conclusions (they had to spell `ZipFile$Source` explicitly). Either match `$` by
default or error/loudly warn that globs ignore inner classes; at minimum document it
prominently in `get_oql_docs` output (agent D only found it by trial).

Repro:
```
hprof-analyzer heap query tests/fixtures/dump_4_philosophers.hprof \
  --oql 'SELECT COUNT(*) FROM java.util.zip.ZipFile*'     # -> 0
hprof-analyzer heap query tests/fixtures/dump_4_philosophers.hprof \
  --oql 'SELECT COUNT(*) FROM java.util.zip.ZipFile$Source'  # -> 19
```

## 8. OQL: `AS retained` collides with the `RETAINED SET` keyword

```
hprof-analyzer heap query tests/fixtures/dump_4_philosophers.hprof \
  --oql 'SELECT COUNT(*) AS retained FROM java.util.zip.ZipFile*'
# error: expected SET after 'AS RETAINED' (usage: SELECT <expr> AS RETAINED SET FROM ...)
```
`retained` should be a legal alias (or the error should suggest quoting/renaming). Agent D
lost time to this.

## 9. OQL: unknown class name returns 0 rows silently

`SELECT * FROM com.does.not.Exist` → `{"columns":["*"],"row_count":0}` — no "class not
found" hint. Agents can mistake a typo'd class name for "no instances in heap". Should
report "class `X` not present (did you mean …)" like SQL does. Repro via MCP `query` tool or
`hprof-analyzer query <DUMP> --query '...'`.

## 10. OQL: unknown field in SELECT produces empty output, exit 0

```
hprof-analyzer heap query tests/fixtures/dump_4_philosophers.hprof \
  --oql 'SELECT capacity, nonexistent_field FROM java.nio.DirectByteBuffer'
# (no output at all, exit 0)
```
A valid class + one bad field silently swallows the whole result instead of erroring.

## 11. `inspect_object` / `heap inspect` negative index → raw serde error

MCP `inspect_object({object_index: -1})`:
```
failed to deserialize parameters: invalid value: integer `-1`, expected u64
```
(isError=true). Should be a normal tool error like the out-of-range message
(`object_index 999999999 out of range (0..482360)` — which is fine).

## 12. Dense-index range shown doesn't match the "Objects" count (confusing, maybe by design)

Clinic load reports `Objects: 437704`, but the inspect range error says `0..482360`. The
extra ~45k are class/other entries — undocumented; agents can't tell whether an index is
valid. Worth one clarifying sentence in the error or in `get_session_info`.

## 13. CLI rough edges reported by the CLI-driving agent (agent C)

- `heap report --help` does not list valid `--section` names (MCP's error message does
  enumerate them — copy that list into the help).
- `heap summary` hard-caps at 5 classes with no `--limit`; no path to more without switching
  to `histogram`.
- Naming mismatch: `heap browse` takes `--index`, MCP takes `object_index`.

## 14. UNCONFIRMED: `load_dump` without `with_graph:true` failed once with "Unknown frame descriptor"

Agent B's transcript claims `load_dump` on `tests/fixtures/dump_4_philosophers.hprof`
*without* `with_graph: true` first failed with `Unknown frame descriptor`, then succeeded on
retry with `with_graph: true`. I could not reproduce (both default and `with_graph:true`
loads succeed, cache warm). If real it suggests a parse path that only runs with graph
building enabled and no error recovery; needs repro on a cold cache (`heap cache-clear`) and
against a 0.3.4 build.

## 15. Positive notes (for calibration)

- Cached loads are fast (~1 s) and `load_dump`'s self-describing `_next` hints work well —
  agents navigated the toolchain unaided and produced largely correct leak diagnoses
  (clinic: ReportEngine/NotificationHub/EventLog correctly identified and explained).
- OQL parse errors are excellent (caret-diagnostics with "did you mean SELECT?" suggestions).
- Missing-path errors are immediate and clean ("No such file or directory (os error 2)").
- `heap summary`/`histogram`/`cache-list` worked as advertised for a consumer of the CLI.
