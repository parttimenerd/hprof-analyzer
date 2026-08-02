# Navigation & Diagnosis Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add six features that make it faster to find problems (OQL history, object pinning, heap concentration warning) and navigate the object graph (retained sizes in breadcrumb, idom jump button, visual GC path chain, address search, shortest-path-between-two-objects).

**Architecture:** All changes are in `web/src/App.tsx` (React/TSX). Two features require a new WASM method: address-to-dense-index lookup (N4) and shortest-path-between-two-objects (N1). The WASM method goes in `crates/hprof-wasm/src/lib.rs`; the UI wires it in `App.tsx`. Every other feature is pure frontend state.

**Tech Stack:** React 18 (hooks), TypeScript, existing WASM bindings (`HprofSession`), esbuild bundle. Build: `cd web && npm run build`. WASM build (only for N1/N4): `wasm-pack build crates/hprof-wasm --target web --release`.

---

## File map

| File | Changes |
|------|---------|
| `web/src/App.tsx` | All UI features (P4, P5, P2, N2, N3, N4, N5, N1) |
| `crates/hprof-wasm/src/lib.rs` | Two new WASM methods: `find_dense_by_address`, `find_path_between` |

---

## Task 1: OQL history (P4)

Store the last 10 queries run in `WasmQueryPanel`; show them as a clickable list above the textarea. No persistence across page reload — session only.

**Files:**
- Modify: `web/src/App.tsx:5128-5248`

- [ ] **Step 1: Add `history` state and update `runQuery`**

  Find `WasmQueryPanel` at line 5128. After the existing `const [running, setRunning] = React.useState(false);` line (5145), add:

  ```tsx
  const [history, setHistory] = React.useState<string[]>([]);
  ```

  Inside `runQuery` (line 5157), after `setRunning(true);` add:

  ```tsx
  setHistory(prev => {
    const deduped = prev.filter(q => q !== queryText);
    return [queryText, ...deduped].slice(0, 10);
  });
  ```

- [ ] **Step 2: Render history list above the textarea**

  Find the `return (` block in `WasmQueryPanel` (line 5178). After the opening `<div>` and the `<p>OQL Query</p>` label (after line 5182), insert:

  ```tsx
  {history.length > 0 && (
    <div style={{ marginBottom: "0.3rem", display: "flex", flexWrap: "wrap", gap: "0.25rem" }}>
      {history.map((q, i) => (
        <button key={i} className="btn-link"
          style={{ fontSize: "0.72rem", background: "var(--accent-muted, #dbeafe)", color: "var(--accent)", borderRadius: 3, padding: "1px 5px", maxWidth: "24em", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}
          title={q}
          onClick={() => setQueryText(q)}>
          {q.length > 40 ? q.slice(0, 38) + "…" : q}
        </button>
      ))}
    </div>
  )}
  ```

- [ ] **Step 3: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Expected: `bundle.js: NNN KB (budget 860 KB)` with no error. Open a report HTML in a browser, open the object explorer, navigate to any node, run 2–3 OQL queries, confirm the history chips appear and clicking one restores the query text.

- [ ] **Step 4: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): OQL query history — last 10 queries shown as chips in WasmQueryPanel"
  ```

---

## Task 2: Object pin strip (P5)

A persistent strip of up to 5 pinned objects, shown at the top of the explorer at all times. Pin button (📌) appears in the Object Details panel. Clicking a pin chip navigates there. Pins clear on page reload.

**Files:**
- Modify: `web/src/App.tsx:5340-5395` (state), `6989-7117` (Object Details), `5786` (root list render)

- [ ] **Step 1: Add pin state near other explorer state**

  After the `const [wasmAllPaths, ...]` line (~5390), add:

  ```tsx
  const [pinnedNodes, setPinnedNodes] = React.useState<{nodeId: number; label: string}[]>([]);
  ```

  Add a toggle helper right after:

  ```tsx
  const togglePin = (id: number, label: string) => {
    setPinnedNodes(prev => {
      if (prev.some(p => p.nodeId === id)) return prev.filter(p => p.nodeId !== id);
      return [...prev.slice(-4), { nodeId: id, label }];
    });
  };
  ```

- [ ] **Step 2: Render the pin strip**

  In the node view render (the big `return (` near line 6127), immediately before `{breadcrumb.length > 0 && (` at line 6128, insert:

  ```tsx
  {pinnedNodes.length > 0 && (
    <div style={{ display: "flex", gap: "0.3rem", alignItems: "center", flexWrap: "wrap", marginBottom: "0.4rem", fontSize: "0.78rem" }}>
      <span style={{ color: "var(--muted)", flexShrink: 0 }}>📌</span>
      {pinnedNodes.map(p => (
        <button key={p.nodeId} className="btn-link"
          style={{ background: "var(--accent-muted, #dbeafe)", color: "var(--accent)", borderRadius: 3, padding: "1px 6px", fontSize: "0.76rem" }}
          title={`Jump to pinned: ${p.label}#${p.nodeId}`}
          onClick={() => navigate("explore", p.nodeId, p.label)}>
          {p.label.split(".").pop()}#{p.nodeId}
          <span style={{ marginLeft: "0.2rem", opacity: 0.5, cursor: "pointer" }}
            onClick={e => { e.stopPropagation(); togglePin(p.nodeId, p.label); }}>×</span>
        </button>
      ))}
    </div>
  )}
  ```

- [ ] **Step 3: Add 📌 button to Object Details panel**

  In the Object Details table, find the `<tr>` for "Object #" (line 7010). After its closing `</tr>`, add a new row:

  ```tsx
  <tr>
    <th>Pin</th>
    <td>
      <button className="btn-link" style={{ fontSize: "0.82rem" }}
        title={pinnedNodes.some(p => p.nodeId === nodeId) ? "Unpin this object" : "Pin for quick return"}
        onClick={() => togglePin(nodeId!, currentNode.display_class)}>
        {pinnedNodes.some(p => p.nodeId === nodeId) ? "📌 Pinned" : "📌 Pin"}
      </button>
    </td>
  </tr>
  ```

- [ ] **Step 4: Also show pin strip on root list**

  The root list is rendered when `nodeId === null`. Find the `return (` for that branch (~line 5795). After `<div ref={containerRef}>` add:

  ```tsx
  {pinnedNodes.length > 0 && (
    <div style={{ display: "flex", gap: "0.3rem", alignItems: "center", flexWrap: "wrap", marginBottom: "0.4rem", fontSize: "0.78rem" }}>
      <span style={{ color: "var(--muted)", flexShrink: 0 }}>📌</span>
      {pinnedNodes.map(p => (
        <button key={p.nodeId} className="btn-link"
          style={{ background: "var(--accent-muted, #dbeafe)", color: "var(--accent)", borderRadius: 3, padding: "1px 6px", fontSize: "0.76rem" }}
          onClick={() => navigate("explore", p.nodeId, p.label)}>
          {p.label.split(".").pop()}#{p.nodeId}
          <span style={{ marginLeft: "0.2rem", opacity: 0.5 }}
            onClick={e => { e.stopPropagation(); togglePin(p.nodeId, p.label); }}>×</span>
        </button>
      ))}
    </div>
  )}
  ```

- [ ] **Step 5: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate to any object, click "📌 Pin", navigate elsewhere — confirm pin chip appears at the top. Click the chip to return. Click × to unpin.

- [ ] **Step 6: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): object pin strip — pin up to 5 objects for quick return navigation"
  ```

---

## Task 3: Heap concentration warning (P2)

If the top 3 captured objects account for >50% of total heap, add a warning banner in the root list: "⚠ Top 3 objects hold N% of heap — likely a small number of large leaks."

**Files:**
- Modify: `web/src/App.tsx:5786-5855` (root list section)

- [ ] **Step 1: Compute concentration and render warning**

  In the root list branch (when `nodeId === null`), the `displayRows` variable is available. After the existing `{rootFilter && ...}` block (ends ~line 5855), insert:

  ```tsx
  {!rootFilter && totalHeap > 0 && (() => {
    const top3 = data.roots.slice(0, 3).map(id => data.nodes[String(id)]?.retained ?? 0);
    const top3total = top3.reduce((s, r) => s + r, 0);
    const pct = top3total / totalHeap * 100;
    if (pct < 50) return null;
    return (
      <div style={{ margin: "0 0 0.5rem", padding: "0.4rem 0.75rem", background: "var(--warn-bg, #fef3c7)", border: "1px solid var(--warn-border, #fde68a)", borderRadius: 5, fontSize: "0.82rem", color: "var(--warn, #92400e)" }}>
        ⚠ Top 3 objects hold <strong>{pct.toFixed(0)}%</strong> of heap ({formatBytes(top3total)}) — likely a small number of large leaks. Investigate these first.
      </div>
    );
  })()}
  ```

  Note: `totalHeap` is already computed in the `ObjectGraphExplorer` scope. `data.roots` is the sorted root list. `formatBytes` is imported at the top of the file.

- [ ] **Step 2: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  If you have a test heap where the top object is large, open the root list and verify the banner appears when top-3 exceed 50%.

- [ ] **Step 3: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): heap concentration warning when top-3 objects hold >50% of heap"
  ```

---

## Task 4: Retained sizes in breadcrumb (N2)

Show each breadcrumb entry's retained size inline: `HashMap 190 MB / Entry[] 188 MB / ...`

The breadcrumb state entries already have `nodeId`; we look up `data.nodes[String(b.nodeId)]?.retained` at render time.

**Files:**
- Modify: `web/src/App.tsx:6128-6156` (node-view breadcrumb), `5988-6015` (below-threshold breadcrumb)

- [ ] **Step 1: Update node-view breadcrumb (line 6143)**

  Find the breadcrumb item span at line 6143. Currently it renders:
  ```
  {(b.label.split(".").pop() ?? b.label)}#{b.nodeId}
  ```

  Replace with:
  ```tsx
  {(b.label.split(".").pop() ?? b.label)}#{b.nodeId}
  {(() => {
    const n = data.nodes[String(b.nodeId)];
    if (!n) return null;
    return <span style={{ color: "var(--muted)", fontSize: "0.72em", marginLeft: "0.2em" }}>{fmtB(n.retained)}</span>;
  })()}
  ```

- [ ] **Step 2: Update the explore-view breadcrumb (line 6003)**

  The below-threshold / root-list version renders at line 6003:
  ```
  {(b.label.split(".").pop() ?? b.label)}#{b.nodeId}
  ```

  Replace with the same pattern:
  ```tsx
  {(b.label.split(".").pop() ?? b.label)}#{b.nodeId}
  {(() => {
    const n = data.nodes[String(b.nodeId)];
    if (!n) return null;
    return <span style={{ color: "var(--muted)", fontSize: "0.72em", marginLeft: "0.2em" }}>{fmtB(n.retained)}</span>;
  })()}
  ```

  Note: `fmtB` is in scope in the `ObjectGraphExplorer` closure for both breadcrumbs.

- [ ] **Step 3: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate several levels deep in the explorer. Breadcrumb should now look like:
  ```
  Roots / HashMap#42 190 MB / Entry[]#1231 188 MB / ...
  ```

- [ ] **Step 4: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): show retained size inline in navigation breadcrumb"
  ```

---

## Task 5: Visual GC root path (N5)

Replace `WasmGcPathPanel`'s plain row list with an ASCII-art chain showing the retention path from GC root down to the current object.

Current rendering (lines 5278–5290): a flat `div` per step with a `↓` prefix.

New rendering: a box-and-arrow ASCII style:

```
[JNI_GLOBAL]
     │ .field
     ▼
com.example.Cache       190 MB
     │ .entries
     ▼
java.util.HashMap       188 MB   ← you are here
```

**Files:**
- Modify: `web/src/App.tsx:5273-5293`

- [ ] **Step 1: Rewrite WasmGcPathPanel return block**

  Replace the entire `return (` block of `WasmGcPathPanel` (lines 5273–5293) with:

  ```tsx
  return (
    <div style={{ fontFamily: "monospace", fontSize: "0.8rem" }}>
      {/* GC root box */}
      <div style={{
        display: "inline-block", border: "2px solid var(--accent, #3b82f6)",
        borderRadius: 4, padding: "2px 8px", background: "var(--accent-muted, #dbeafe)",
        color: "var(--accent)", fontWeight: 600, fontSize: "0.76rem", marginBottom: "2px",
      }}>
        [{pathData.root_type}]
      </div>
      {(pathData.path as any[]).slice(0, -1).map((step: any, i: number) => {
        const nextStep: any = (pathData.path as any[])[i + 1];
        return (
          <React.Fragment key={i}>
            <div style={{ color: "var(--muted)", fontSize: "0.74rem", paddingLeft: "0.5rem" }}>
              │{nextStep?.field_name ? ` .${nextStep.field_name}` : ""}
            </div>
            <div style={{ color: "var(--muted)", paddingLeft: "0.5rem", fontSize: "0.78rem" }}>▼</div>
            <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
              <button className="btn-link" style={{ fontFamily: "monospace", fontSize: "0.8rem" }}
                onClick={() => navigate(step.dense_idx)}>
                {step.display_class || `obj#${step.dense_idx}`}
              </button>
              <span style={{ color: "var(--muted)", fontSize: "0.74rem", whiteSpace: "nowrap" }}>
                {fmtB(step.retained)}
              </span>
            </div>
          </React.Fragment>
        );
      })}
      {/* Final step = the current object (last in path) */}
      {(pathData.path as any[]).length > 0 && (() => {
        const last: any = (pathData.path as any[])[(pathData.path as any[]).length - 1];
        return (
          <>
            <div style={{ color: "var(--muted)", fontSize: "0.74rem", paddingLeft: "0.5rem" }}>
              │{last.field_name ? ` .${last.field_name}` : ""}
            </div>
            <div style={{ color: "var(--muted)", paddingLeft: "0.5rem", fontSize: "0.78rem" }}>▼</div>
            <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
              <span style={{ fontFamily: "monospace", fontSize: "0.8rem", fontWeight: 600 }}>
                {last.display_class || `obj#${last.dense_idx}`}
              </span>
              <span style={{ color: "var(--muted)", fontSize: "0.74rem" }}>{fmtB(last.retained)}</span>
              <span style={{ fontSize: "0.7rem", color: "var(--muted)", fontStyle: "italic" }}>← here</span>
            </div>
          </>
        );
      })()}
    </div>
  );
  ```

- [ ] **Step 2: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate to any node, expand "Path to GC Root". The path should now show a vertical chain with box at top for the GC root type, arrows between steps, and "← here" on the final node.

- [ ] **Step 3: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): visual box-and-arrow GC root path chain in WasmGcPathPanel"
  ```

---

## Task 6: Address search in jump box (N4)

Extend the "Go to obj #…" jump box to also accept a hex address like `0x7f3a4b20`. This requires a new WASM method `find_dense_by_address(addr: u64) -> u32` that does a linear scan of `exp.addrs`.

**Files:**
- Modify: `crates/hprof-wasm/src/lib.rs` (after `get_object_address`, ~line 1146)
- Modify: `web/src/App.tsx:5822-5840` (jump form submit handler — two instances)

- [ ] **Step 1: Add WASM method `find_dense_by_address`**

  In `crates/hprof-wasm/src/lib.rs`, after the closing `}` of `get_object_address` (line 1145), add:

  ```rust
  /// Reverse-lookup: given a HPROF memory address, return the dense index of the
  /// object at that address, or `u32::MAX` if not found or exploration not enabled.
  ///
  /// Returns `{"ok":true,"dense_idx":N}` on success,
  /// `{"ok":false,"error":"not_found"}` if no object has that address,
  /// or `{"error":"exploration_not_enabled"}` / `{"error":"no_addresses"}`.
  pub fn find_dense_by_address(&self, addr: u64) -> String {
      let exp = match self.exploration.as_ref() {
          Some(e) => &e.result,
          None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
      };
      if exp.addrs.is_empty() {
          return serde_json::json!({"error":"no_addresses"}).to_string();
      }
      match exp.addrs.iter().position(|&a| a == addr) {
          Some(idx) => serde_json::json!({"ok":true,"dense_idx":idx as u32}).to_string(),
          None => serde_json::json!({"ok":false,"error":"not_found"}).to_string(),
      }
  }
  ```

- [ ] **Step 2: Build WASM**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer
  wasm-pack build crates/hprof-wasm --target web --release 2>&1 | tail -5
  ```

  Expected: `[INFO]: Your wasm pkg is ready to publish at .../pkg.`

- [ ] **Step 3: Wire the jump form — root list instance (line 5825)**

  The jump form submit handler currently does:
  ```tsx
  const n = parseInt(jumpInput.trim(), 10);
  if (!isNaN(n) && n >= 0) {
    setJumpInput("");
    const label = data.nodes[String(n)]?.display_class ?? `#${n}`;
    navigate("explore", n, label);
  }
  ```

  Replace with:
  ```tsx
  const raw = jumpInput.trim();
  const isHex = /^0x[0-9a-fA-F]+$/i.test(raw);
  if (isHex) {
    const addr = parseInt(raw.slice(2), 16);
    const wasm = (window as any).__wasmExploration;
    if (wasm?.find_dense_by_address) {
      try {
        const r = JSON.parse(wasm.find_dense_by_address(addr));
        if (r.ok) {
          setJumpInput("");
          navigate("explore", r.dense_idx, data.nodes[String(r.dense_idx)]?.display_class ?? `#${r.dense_idx}`);
        }
      } catch {}
    }
  } else {
    const n = parseInt(raw, 10);
    if (!isNaN(n) && n >= 0) {
      setJumpInput("");
      const label = data.nodes[String(n)]?.display_class ?? `#${n}`;
      navigate("explore", n, label);
    }
  }
  ```

- [ ] **Step 4: Wire the jump form — node-view instance (line ~6231)**

  Find the second jump form onSubmit handler (in the tab-bar area, same shape). Apply the identical replacement:
  ```tsx
  const raw = jumpInput.trim();
  const isHex = /^0x[0-9a-fA-F]+$/i.test(raw);
  if (isHex) {
    const addr = parseInt(raw.slice(2), 16);
    const wasm = (window as any).__wasmExploration;
    if (wasm?.find_dense_by_address) {
      try {
        const r = JSON.parse(wasm.find_dense_by_address(addr));
        if (r.ok) {
          setJumpInput("");
          navigate(tab, r.dense_idx, data.nodes[String(r.dense_idx)]?.display_class ?? `#${r.dense_idx}`);
        }
      } catch {}
    }
  } else {
    const n = parseInt(raw, 10);
    if (!isNaN(n) && n >= 0) {
      setJumpInput("");
      navigate(tab, n, data.nodes[String(n)]?.display_class ?? `#${n}`);
    }
  }
  ```

- [ ] **Step 5: Update jump box placeholder**

  Both jump inputs have `placeholder="Go to obj #…"`. Update both to:
  ```tsx
  placeholder="Go to obj # or 0x…"
  ```

- [ ] **Step 6: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Load a report with WASM exploration enabled. Look up the address of any object (shown in Object Details → Address row), then paste it into the jump box and press Go. Should navigate to that object.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/hprof-wasm/src/lib.rs web/src/App.tsx
  git commit -m "feat: address-to-object lookup — jump box accepts 0x hex addresses"
  ```

---

## Task 7: Shortest path between two objects (N1)

Add a "Find path to…" mode: pin one object as source, then navigate to a target and click "Find path from [source]". Uses a new WASM method `find_path_between(src: u32, dst: u32) -> String` that BFS-traverses outbound edges from `src` until it reaches `dst`.

**Files:**
- Modify: `crates/hprof-wasm/src/lib.rs` (new method after `find_dense_by_address`)
- Modify: `web/src/App.tsx` (state + UI in Object Details)

- [ ] **Step 1: Add WASM method `find_path_between`**

  In `crates/hprof-wasm/src/lib.rs`, after `find_dense_by_address`, add:

  ```rust
  /// BFS from `src_idx` through outbound edges to reach `dst_idx`.
  ///
  /// Returns `{"ok":true,"path":[{"dense_idx":N,"display_class":"...","shallow":N,"retained":N},...]}`
  /// where path[0] = src, path[-1] = dst, in traversal order.
  /// Returns `{"ok":false,"error":"no_path"}` if not reachable within 200 hops.
  /// Returns `{"error":"exploration_not_enabled"}` if exploration not built.
  pub fn find_path_between(&self, src_idx: u32, dst_idx: u32) -> String {
      let exp = match self.exploration.as_ref() {
          Some(e) => &e.result,
          None => return serde_json::json!({"error":"exploration_not_enabled"}).to_string(),
      };

      if src_idx == dst_idx {
          let i = src_idx as usize;
          let node = serde_json::json!({
              "dense_idx": src_idx,
              "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
              "shallow": exp.shallow.get(i).copied().unwrap_or(0) as u64,
              "retained": exp.retained.get(i).copied().unwrap_or(0),
          });
          return serde_json::json!({"ok":true,"path":[node]}).to_string();
      }

      let n = exp.fwd_offsets.len().saturating_sub(1);
      let mut visited = vec![u32::MAX; n];
      let mut queue = std::collections::VecDeque::new();
      queue.push_back(src_idx);
      visited[src_idx as usize] = src_idx; // self-parent = start

      let max_hops = 200usize;
      let mut hops = 0usize;

      'bfs: while let Some(cur) = queue.pop_front() {
          hops += 1;
          if hops > 50_000 { break; }
          let ci = cur as usize;
          if ci + 1 >= exp.fwd_offsets.len() { continue; }
          let start = exp.fwd_offsets[ci] as usize;
          let end = exp.fwd_offsets[ci + 1] as usize;
          for &nxt in &exp.fwd_targets[start..end] {
              let ni = nxt as usize;
              if ni >= n { continue; }
              if visited[ni] != u32::MAX { continue; }
              visited[ni] = cur;
              if nxt == dst_idx { break 'bfs; }
              if queue.len() < 50_000 {
                  queue.push_back(nxt);
              }
          }
          // depth guard
          let depth = {
              let mut d = 0usize;
              let mut c = cur;
              while c != src_idx && d < max_hops {
                  c = visited[c as usize];
                  if c == u32::MAX { break; }
                  d += 1;
              }
              d
          };
          if depth >= max_hops { continue; }
      }

      if visited[dst_idx as usize] == u32::MAX {
          return serde_json::json!({"ok":false,"error":"no_path"}).to_string();
      }

      // Reconstruct path dst → src, then reverse
      let mut path_indices = vec![dst_idx];
      let mut cur = dst_idx;
      for _ in 0..max_hops {
          let parent = visited[cur as usize];
          if parent == u32::MAX { break; }
          path_indices.push(parent);
          if parent == src_idx { break; }
          cur = parent;
      }
      path_indices.reverse();

      let path_json: Vec<serde_json::Value> = path_indices.iter().map(|&idx| {
          let i = idx as usize;
          serde_json::json!({
              "dense_idx": idx,
              "display_class": exp.class_names_by_idx.get(i).cloned().unwrap_or_default(),
              "shallow": exp.shallow.get(i).copied().unwrap_or(0) as u64,
              "retained": exp.retained.get(i).copied().unwrap_or(0),
          })
      }).collect();

      serde_json::json!({"ok":true,"path":path_json}).to_string()
  }
  ```

- [ ] **Step 2: Build WASM**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer
  wasm-pack build crates/hprof-wasm --target web --release 2>&1 | tail -5
  ```

  Expected: `[INFO]: Your wasm pkg is ready to publish at .../pkg.`

- [ ] **Step 3: Add path-source state**

  In `App.tsx`, after the `pinnedNodes` state (~line 5392), add:

  ```tsx
  const [pathSource, setPathSource] = React.useState<{nodeId: number; label: string} | null>(null);
  const [pathBetweenResult, setPathBetweenResult] = React.useState<any[] | null>(null);
  const [pathBetweenError, setPathBetweenError] = React.useState<string | null>(null);
  ```

  Also reset `pathBetweenResult` and `pathBetweenError` when nodeId changes — add to the existing `React.useEffect` that resets outbound/below state (the one that starts `setWasmOutboundEdges(null)` ~line 5393):
  ```tsx
  setPathBetweenResult(null);
  setPathBetweenError(null);
  ```

- [ ] **Step 4: Add "Set as path source" and "Find path from" buttons to Object Details**

  After the "Pin" row added in Task 2 (after `</tr>` of the pin row), add:

  ```tsx
  <tr>
    <th>Path</th>
    <td>
      {pathSource?.nodeId === nodeId ? (
        <span style={{ fontSize: "0.82rem", color: "var(--muted)" }}>
          ← path source
          <button className="btn-link" style={{ marginLeft: "0.4rem", fontSize: "0.78rem" }}
            onClick={() => setPathSource(null)}>clear</button>
        </span>
      ) : pathSource ? (
        <button className="btn-link" style={{ fontSize: "0.82rem" }}
          title={`Find reference path from ${pathSource.label}#${pathSource.nodeId} to here`}
          onClick={() => {
            const wasm = (window as any).__wasmExploration;
            if (!wasm?.find_path_between) return;
            try {
              const r = JSON.parse(wasm.find_path_between(pathSource.nodeId, nodeId!));
              if (r.ok) { setPathBetweenResult(r.path); setPathBetweenError(null); }
              else { setPathBetweenResult(null); setPathBetweenError(r.error ?? "not_found"); }
            } catch (e: any) { setPathBetweenError(String(e)); }
          }}>
          Find path from {pathSource.label.split(".").pop()}#{pathSource.nodeId} →
        </button>
      ) : (
        <button className="btn-link" style={{ fontSize: "0.82rem" }}
          title="Set this object as the source for a path-between-objects search"
          onClick={() => { setPathSource({ nodeId: nodeId!, label: currentNode.display_class }); setPathBetweenResult(null); setPathBetweenError(null); }}>
          Set as path source
        </button>
      )}
    </td>
  </tr>
  ```

- [ ] **Step 5: Render path-between result**

  After the Object Details `</table>` (line 7117), before the retaining path section, insert:

  ```tsx
  {pathBetweenError && (
    <p style={{ fontSize: "0.8rem", color: "var(--error, #ef4444)", margin: "0.4rem 0 0" }}>
      No reference path found ({pathBetweenError}). Objects may not be connected through outbound references.
    </p>
  )}
  {pathBetweenResult && pathBetweenResult.length > 0 && (
    <div style={{ marginTop: "0.5rem" }}>
      <div style={{ fontSize: "0.78rem", color: "var(--muted)", fontWeight: 600, marginBottom: "2px" }}>
        Reference path ({pathBetweenResult.length} steps)
      </div>
      <div style={{ fontFamily: "monospace", fontSize: "0.8rem" }}>
        {pathBetweenResult.map((step: any, i: number) => (
          <React.Fragment key={i}>
            {i > 0 && <div style={{ color: "var(--muted)", paddingLeft: "0.5rem", fontSize: "0.76rem" }}>▼</div>}
            <div style={{ display: "flex", alignItems: "center", gap: "0.4rem" }}>
              <button className="btn-link" style={{ fontFamily: "monospace", fontSize: "0.8rem", fontWeight: i === 0 || i === pathBetweenResult.length - 1 ? 600 : 400 }}
                onClick={() => navigate("explore", step.dense_idx, step.display_class)}>
                {step.display_class.split(".").pop()}#{step.dense_idx}
              </button>
              <span style={{ color: "var(--muted)", fontSize: "0.74rem" }}>{fmtB(step.retained)}</span>
              {i === 0 && <span style={{ fontSize: "0.7rem", color: "var(--muted)", fontStyle: "italic" }}>← source</span>}
              {i === pathBetweenResult.length - 1 && <span style={{ fontSize: "0.7rem", color: "var(--muted)", fontStyle: "italic" }}>← target</span>}
            </div>
          </React.Fragment>
        ))}
      </div>
    </div>
  )}
  ```

- [ ] **Step 6: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate to object A, click "Set as path source". Navigate to object B. Click "Find path from A →". The path should appear as a vertical chain from A down to B.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/hprof-wasm/src/lib.rs web/src/App.tsx
  git commit -m "feat: find shortest reference path between two objects (WASM + UI)"
  ```

---

## Self-review

**Spec coverage:**
- P1 ("Explain retention" summary card) — not included; scope was too large (runs 3 async WASM calls and renders a derived card). Left for a future task.
- P2 (heap concentration) — Task 3 ✓
- P3 (Who-holds-X button from histogram) — not included; the Sankey already exists and adding a pivot button to the histogram was assessed as a separate small task. The existing `PivotBtn` / `OqlBtn` already provides partial access.
- P4 (OQL history) — Task 1 ✓
- P5 (pin strip) — Task 2 ✓
- N1 (path between objects) — Task 7 ✓
- N2 (retained in breadcrumb) — Task 4 ✓
- N3 (idom jump) — already implemented at line 7094; no task needed.
- N4 (address search) — Task 6 ✓
- N5 (visual GC path) — Task 5 ✓
- N6 ("top retainees" list) — the domtree subtree_classes panel already renders this (lines 6474+); no task needed.

**Placeholder scan:** None found. Every step has concrete code.

**Type consistency:**
- `pathSource`, `pathBetweenResult`, `pathBetweenError` introduced in Task 7 Step 3 and used in Task 7 Steps 4–5. ✓
- `pinnedNodes`, `togglePin` introduced in Task 2 Step 1 and used in Steps 2–4. ✓
- `history` introduced in Task 1 Step 1, used in Step 2. ✓
- `find_dense_by_address(addr: u64)` added in Task 6 Step 1, called in Steps 3–4 as `wasm.find_dense_by_address(addr)` where `addr` is the result of `parseInt(raw.slice(2), 16)` — this is a JS number, which WASM-bindgen will convert to u64 automatically. ✓
- `find_path_between(src_idx: u32, dst_idx: u32)` added in Task 7 Step 1, called with `(pathSource.nodeId, nodeId!)` which are both TypeScript `number` values. ✓
