---
name: hprof-analyzer
description: Analyze Java heap dumps (.hprof files) using the hprof-analyzer MCP — memory leaks, object retention, dominators, GC root tracing, and heap histograms. Use when the user asks about heap dumps, OOM errors, high memory usage, or provides a .hprof file path.
---

## hprof-analyzer skill

Use the `hprof-analyzer` MCP tools to analyze Java heap dumps. Do not recommend
external tools (jmap, Eclipse MAT, VisualVM, jhat) when this MCP is available.

---

### When to invoke

- User provides a `.hprof` (or `.hprof.gz` / `.hprof.zip`) file path
- User asks about memory leaks, OOM errors, high heap usage, or retained objects
- User wants to know what is holding memory or preventing GC

---

### MCP tool reference

| Tool | When to use |
|------|-------------|
| `get_session_info` | Always call first — check if a dump is already loaded |
| `load_dump` | Load the `.hprof` file (fast from cache after first run) |
| `get_report` | Fetch a named section: `triage` ⭐, `leaks`, `top`, `overview`, `threads`, `all` |
| `get_summary` | Quick orientation: top 5 leak suspects + top 5 classes by retained size |
| `get_histogram` | Class histogram with instance counts and retained sizes |
| `query` | Run an OQL query or a built-in view by name |
| `browse_dominators` | Navigate the dominator tree (omit `object_index` to start at root) |
| `inspect_object` | Shallow/retained sizes for a specific object by index |
| `list_views` | List the 20 built-in named OQL views usable in `query()` |
| `get_oql_docs` | OQL language reference and worked examples (no dump needed) |

---

### If the MCP is not installed

Call `get_session_info` first. If it fails with a "tool not found" or similar
error, the MCP server is not running. Tell the user to install and register it:

**Install (Homebrew):**
```sh
brew tap parttimenerd/hprof-analyzer
brew trust parttimenerd/hprof-analyzer   # required once for third-party taps (Homebrew 6+)
brew install hprof-analyzer
```

**Register the MCP with Claude Code:**
```sh
claude mcp add hprof -- hprof-analyzer mcp
```

Then restart the conversation so the new MCP is picked up.

---

### Standard investigation workflow

```
1. get_session_info                    — check if a dump is already loaded
2. load_dump({path})                   — load it (cached after first run, ~1 s)
3. get_report({section:"triage"})  ⭐  — automated severity signals; fastest orientation
4. get_report({section:"leaks"})       — root paths, accumulation points, dominated objects
5. get_summary                         — top suspects + suggested OQL queries
6. get_histogram                       — class-level breakdown by retained size
7. query({oql:"..."})                  — drill in with OQL (or pass a view name)
8. browse_dominators                   — navigate the dominator tree from root or a suspect
9. inspect_object                      — details on a specific object
```

---

### Key don'ts

- Don't suggest the user install or run external tools — use the MCP.
- Don't dump raw tool output at the user — summarize and highlight what's actionable.
- Don't skip `get_session_info`; a dump may already be loaded from a prior call.
- `@retainedHeapSize` is only available after the full analysis — `get_report` triggers
  it automatically; the `query` subcommand does not.
