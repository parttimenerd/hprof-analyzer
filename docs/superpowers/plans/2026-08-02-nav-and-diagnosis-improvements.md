# Navigation & Diagnosis Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eight features that make it faster to find problems (OQL history, object pinning, heap concentration warning) and navigate the object graph (retained sizes in breadcrumb, visual GC path chain with inline expand, address search, shortest-path-between-two-objects).

**Architecture:** All changes are in `web/src/App.tsx` (React/TSX). Two features require new WASM methods: address-to-dense-index lookup (Task 6) and shortest-path-between-two-objects (Task 7). The WASM methods go in `crates/hprof-wasm/src/lib.rs`. Task 8 adds a shared `RetentionChain` component that replaces four path-display sites with an interactive expand-to-see-outbound-refs chain.

**Tech Stack:** React 18 (hooks), TypeScript, existing WASM bindings (`HprofSession`), esbuild bundle. Build: `cd web && npm run build`. WASM build (Tasks 6 & 7 only): `wasm-pack build crates/hprof-wasm --target web --release`.

---

## File map

| File | Changes |
|------|---------|
| `web/src/App.tsx` | All UI features (Tasks 1–5, 7–8) |
| `crates/hprof-wasm/src/lib.rs` | Two new WASM methods: `find_dense_by_address`, `find_path_between` (Tasks 6 & 7) |

---

## Task 1: OQL history (P4)

Store the last 10 queries run in `WasmQueryPanel`; show them as clickable chips above the textarea. No persistence across page reload.

**Files:**
- Modify: `web/src/App.tsx:5128-5248`

- [ ] **Step 1: Add `history` state and update `runQuery`**

  Find `WasmQueryPanel` at line 5128. After the existing `const [running, setRunning] = React.useState(false);` line (~5145), add:

  ```tsx
  const [history, setHistory] = React.useState<string[]>([]);
  ```

  Inside `runQuery` (line ~5157), after `setRunning(true);` add:

  ```tsx
  setHistory(prev => {
    const deduped = prev.filter(q => q !== queryText);
    return [queryText, ...deduped].slice(0, 10);
  });
  ```

- [ ] **Step 2: Render history chips above the textarea**

  In the `return (` block of `WasmQueryPanel` (~line 5178), after the `<p>OQL Query</p>` label, insert:

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

  Expected: `bundle.js: NNN KB (budget 860 KB)`. Open a report in a browser, run 2–3 OQL queries, confirm history chips appear and clicking one restores the query text.

- [ ] **Step 4: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): OQL query history — last 10 queries shown as chips in WasmQueryPanel"
  ```

---

## Task 2: Object pin strip (P5)

A persistent strip of up to 5 pinned objects at the top of the explorer. Pin button appears in Object Details. Clicking a pin chip navigates there. Pins clear on page reload.

**Files:**
- Modify: `web/src/App.tsx:5340-5395` (state), `6989-7117` (Object Details), `~5795` (root list render)

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

- [ ] **Step 2: Render the pin strip in node view**

  In the node view render (the big `return (` near line 6127), immediately before `{breadcrumb.length > 0 && (`, insert:

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

- [ ] **Step 3: Add 📌 button row to Object Details panel**

  In the Object Details `<tbody>`, after the `<tr>` for "Object #" (~line 7010), add:

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

  Find the root-list render branch (when `nodeId === null`, ~line 5795). After `<div ref={containerRef}>`, add the same pin strip:

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

  Navigate to any object, click "📌 Pin", navigate elsewhere — confirm the pin chip appears at the top. Click the chip to return. Click × to unpin.

- [ ] **Step 6: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): object pin strip — pin up to 5 objects for quick return navigation"
  ```

---

## Task 3: Heap concentration warning (P2)

If the top 3 captured objects account for >50% of total heap, show a warning banner at the root list.

**Files:**
- Modify: `web/src/App.tsx:5786-5855` (root list section)

- [ ] **Step 1: Compute concentration and render warning**

  In the root list render branch (`nodeId === null`), after the existing `{rootFilter && ...}` block (~line 5855), insert:

  ```tsx
  {!rootFilter && totalHeap > 0 && (() => {
    const top3 = data.roots.slice(0, 3).map(id => data.nodes[String(id)]?.retained ?? 0);
    const top3total = top3.reduce((s, r) => s + r, 0);
    const pct = top3total / totalHeap * 100;
    if (pct < 50) return null;
    return (
      <div style={{ margin: "0 0 0.5rem", padding: "0.4rem 0.75rem", background: "var(--warn-bg, #fef3c7)", border: "1px solid var(--warn-border, #fde68a)", borderRadius: 5, fontSize: "0.82rem", color: "var(--warn, #92400e)" }}>
        ⚠ Top 3 objects hold <strong>{pct.toFixed(0)}%</strong> of heap ({fmtB(top3total)}) — likely a small number of large leaks. Investigate these first.
      </div>
    );
  })()}
  ```

  Note: `totalHeap` is already in scope (computed from `data.nodes`). `fmtB` is in scope from `useFmtBytes()`. `data.roots` is the sorted root array.

- [ ] **Step 2: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Open a report where the top object is large. Verify the banner appears when top-3 exceed 50%.

- [ ] **Step 3: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): heap concentration warning when top-3 objects hold >50% of heap"
  ```

---

## Task 4: Retained sizes in breadcrumb (N2)

Show each breadcrumb entry's retained size inline: `HashMap#42 190 MB / Entry[]#1231 188 MB / …`

**Files:**
- Modify: `web/src/App.tsx:6128-6156` (node-view breadcrumb), `5988-6015` (below-threshold breadcrumb)

- [ ] **Step 1: Update node-view breadcrumb (~line 6143)**

  Currently renders:
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

- [ ] **Step 2: Update the explore-view breadcrumb (~line 6003)**

  The below-threshold / root-list version renders the same pattern at ~line 6003. Apply the same replacement.

- [ ] **Step 3: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate several levels deep. Breadcrumb should show `HashMap#42 190 MB / Entry[]#1231 188 MB / …`.

- [ ] **Step 4: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): show retained size inline in navigation breadcrumb"
  ```

---

## Task 5: Visual GC root path (N5 — aesthetic)

Replace `WasmGcPathPanel`'s flat row list with an ASCII box-and-arrow chain (this is the visual-only part; interactive expand comes in Task 8).

```
[JNI_GLOBAL]
     │ .field
     ▼
com.example.Cache    190 MB
     │ .entries
     ▼
java.util.HashMap    188 MB  ← here
```

**Files:**
- Modify: `web/src/App.tsx:5273-5293`

- [ ] **Step 1: Rewrite the `WasmGcPathPanel` return block**

  Replace the entire `return (` block of `WasmGcPathPanel` (lines 5273–5293) with:

  ```tsx
  return (
    <div style={{ fontFamily: "monospace", fontSize: "0.8rem" }}>
      <div style={{
        display: "inline-block", border: "2px solid var(--accent, #3b82f6)",
        borderRadius: 4, padding: "2px 8px", background: "var(--accent-muted, #dbeafe)",
        color: "var(--accent)", fontWeight: 600, fontSize: "0.76rem", marginBottom: "2px",
      }}>
        [{pathData.root_type}]
      </div>
      {(pathData.path as any[]).map((step: any, i: number) => {
        const isLast = i === (pathData.path as any[]).length - 1;
        const nextStep: any = (pathData.path as any[])[i + 1];
        return (
          <React.Fragment key={i}>
            <div style={{ color: "var(--muted)", fontSize: "0.74rem", paddingLeft: "0.5rem" }}>
              │{nextStep?.field_name ? ` .${nextStep.field_name}` : ""}
            </div>
            <div style={{ color: "var(--muted)", paddingLeft: "0.5rem", fontSize: "0.78rem" }}>▼</div>
            <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
              {isLast ? (
                <span style={{ fontFamily: "monospace", fontSize: "0.8rem", fontWeight: 600 }}>
                  {step.display_class || `obj#${step.dense_idx}`}
                </span>
              ) : (
                <button className="btn-link" style={{ fontFamily: "monospace", fontSize: "0.8rem" }}
                  onClick={() => navigate(step.dense_idx)}>
                  {step.display_class || `obj#${step.dense_idx}`}
                </button>
              )}
              <span style={{ color: "var(--muted)", fontSize: "0.74rem", whiteSpace: "nowrap" }}>
                {fmtB(step.retained)}
              </span>
              {isLast && <span style={{ fontSize: "0.7rem", color: "var(--muted)", fontStyle: "italic" }}>← here</span>}
            </div>
          </React.Fragment>
        );
      })}
    </div>
  );
  ```

- [ ] **Step 2: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Navigate to any node with WASM loaded, expand "Path to GC Root". Should show a vertical chain with a blue root-type box at the top, arrows between steps, and "← here" on the final node.

- [ ] **Step 3: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): visual box-and-arrow GC root path chain in WasmGcPathPanel"
  ```

---

## Task 6: Address search in jump box (N4)

Extend the "Go to obj #…" jump box to also accept hex addresses like `0x7f3a4b20`. Requires a new WASM method `find_dense_by_address`.

**Files:**
- Modify: `crates/hprof-wasm/src/lib.rs` (after `get_object_address`, ~line 1146)
- Modify: `web/src/App.tsx` — two jump-form submit handlers and their placeholders

- [ ] **Step 1: Add WASM method `find_dense_by_address`**

  In `crates/hprof-wasm/src/lib.rs`, after the closing `}` of `get_object_address` (~line 1145), add:

  ```rust
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

- [ ] **Step 3: Wire the jump form — root list instance (~line 5825)**

  The submit handler currently does:
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
      navigate("explore", n, data.nodes[String(n)]?.display_class ?? `#${n}`);
    }
  }
  ```

- [ ] **Step 4: Wire the jump form — node-view instance (~line 6231)**

  Find the second jump form `onSubmit` handler (same shape, in the tab-bar area). Apply the identical replacement, except the `navigate` call uses `tab` instead of `"explore"`:

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

- [ ] **Step 5: Update both jump box placeholders**

  Both jump inputs have `placeholder="Go to obj #…"`. Update both to:
  ```tsx
  placeholder="Go to obj # or 0x…"
  ```

- [ ] **Step 6: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Load a report with WASM. Look up any object's address (Object Details → Address row), paste it in the jump box, press Go. Should navigate to that object.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/hprof-wasm/src/lib.rs web/src/App.tsx
  git commit -m "feat: address-to-object lookup — jump box accepts 0x hex addresses"
  ```

---

## Task 7: Shortest path between two objects (N1)

"Find path to…" mode: set one object as source, navigate to target, click "Find path from [source]". New WASM method `find_path_between` BFS-traverses outbound edges.

**Files:**
- Modify: `crates/hprof-wasm/src/lib.rs` (after `find_dense_by_address`)
- Modify: `web/src/App.tsx` — state block, Object Details panel

- [ ] **Step 1: Add WASM method `find_path_between`**

  In `crates/hprof-wasm/src/lib.rs`, after `find_dense_by_address`, add:

  ```rust
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
      visited[src_idx as usize] = src_idx;

      'bfs: while let Some(cur) = queue.pop_front() {
          let ci = cur as usize;
          if ci + 1 >= exp.fwd_offsets.len() { continue; }
          let start = exp.fwd_offsets[ci] as usize;
          let end = exp.fwd_offsets[ci + 1] as usize;
          for &nxt in &exp.fwd_targets[start..end] {
              let ni = nxt as usize;
              if ni >= n || visited[ni] != u32::MAX { continue; }
              visited[ni] = cur;
              if nxt == dst_idx { break 'bfs; }
              if queue.len() < 50_000 { queue.push_back(nxt); }
          }
      }

      if visited[dst_idx as usize] == u32::MAX {
          return serde_json::json!({"ok":false,"error":"no_path"}).to_string();
      }

      let mut path_indices = vec![dst_idx];
      let mut cur = dst_idx;
      for _ in 0..500 {
          let parent = visited[cur as usize];
          if parent == u32::MAX || parent == cur { break; }
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

- [ ] **Step 3: Add path-source state in `App.tsx`**

  After the `pinnedNodes` / `togglePin` block added in Task 2 (~line 5394), add:

  ```tsx
  const [pathSource, setPathSource] = React.useState<{nodeId: number; label: string} | null>(null);
  const [pathBetweenResult, setPathBetweenResult] = React.useState<any[] | null>(null);
  const [pathBetweenError, setPathBetweenError] = React.useState<string | null>(null);
  ```

  In the existing `React.useEffect` that resets outbound/below state when `nodeId` changes (the one that starts `setWasmOutboundEdges(null)`, ~line 5393), add:

  ```tsx
  setPathBetweenResult(null);
  setPathBetweenError(null);
  ```

- [ ] **Step 4: Add "Set as path source" / "Find path from" row to Object Details**

  After the "Pin" row added in Task 2, add:

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
          title="Set this object as the source for a path-between search"
          onClick={() => { setPathSource({ nodeId: nodeId!, label: currentNode.display_class }); setPathBetweenResult(null); setPathBetweenError(null); }}>
          Set as path source
        </button>
      )}
    </td>
  </tr>
  ```

- [ ] **Step 5: Render path-between result after Object Details table**

  After the Object Details `</table>` (~line 7117), before the "Retaining path ↑" section, insert:

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

  Navigate to object A, click "Set as path source". Navigate to object B. Click "Find path from A →". The path should appear as a vertical chain from A to B.

- [ ] **Step 7: Commit**

  ```bash
  git add crates/hprof-wasm/src/lib.rs web/src/App.tsx
  git commit -m "feat: find shortest reference path between two objects (WASM BFS + UI)"
  ```

---

## Task 8: Interactive path chain — inline expand/collapse (N5 interactive)

Add a `RetentionChain` component that replaces all four path-chain render sites with an interactive version: each node in the chain has a ▶/▼ toggle that expands inline to show its outbound refs. In offline mode, reads `data.edges`; in online mode, calls `wasm.outbound_refs`. Each expanded ref row has a → explore button.

**Design sketch:**

```
[JNI_GLOBAL]
     │ .contextClassLoader
     ▼
▶  Thread                 12 MB        ← ▶ click to expand
     │ .threadLocals
     ▼
▼  ThreadLocalMap         11 MB        ← expanded
   ├─ .table[0]  Entry   10 MB  → explore
   ├─ .table[1]  Entry    1 MB  → explore
   └─ … 14 more (click to show)
     │ .value
     ▼
●  RequestContext         10 MB   ← here
```

**Files:**
- Modify: `web/src/App.tsx`
  - Add `RetentionChain` component (after `WasmGcPathPanel`)
  - Replace `WasmGcPathPanel` rendering body to delegate to `RetentionChain`
  - Replace offline dominator chain IIFE (lines 7119–7171) with `RetentionChain`
  - Replace multi-path additional paths render (lines 6668–6685) with `RetentionChain`

- [ ] **Step 1: Add `RetentionChain` component**

  After the closing `}` of `WasmGcPathPanel` (~line 5293), insert this new component:

  ```tsx
  // ChainNode represents one step in a retention path.
  // `denseIdx`: the node's dense index in the exploration graph.
  // `displayClass`: human-readable class name.
  // `retained`: retained heap bytes.
  // `fieldName`: field name on the *incoming* edge (from the node above).
  // `isFirst`: true when this is the GC root or topmost anchor node.
  // `isCurrent`: true when this is the "you are here" node (last in chain).
  type ChainNode = {
    denseIdx: number;
    displayClass: string;
    retained: number;
    fieldName?: string;
    isFirst?: boolean;
    isCurrent?: boolean;
  };

  function RetentionChain({
    nodes,
    rootBadge,
    data,
    session,
    fmtB,
    navigate,
  }: {
    nodes: ChainNode[];
    rootBadge?: string;
    data: ObjGraphFlat;
    session?: any;
    fmtB: (b: number) => string;
    navigate: (denseIdx: number) => void;
  }) {
    const [expanded, setExpanded] = React.useState<Set<number>>(new Set());
    const [refs, setRefs] = React.useState<Map<number, { field: string; denseIdx: number; displayClass: string; retained: number }[]>>(new Map());
    const [showAllRefs, setShowAllRefs] = React.useState<Set<number>>(new Set());

    const fetchRefs = (denseIdx: number) => {
      if (refs.has(denseIdx)) return;
      // Offline: read from static edge table
      const staticEdges = data.edges[String(denseIdx)] ?? [];
      if (staticEdges.length > 0) {
        setRefs(m => {
          const next = new Map(m);
          next.set(denseIdx, staticEdges.map(e => ({
            field: e.field_name || "",
            denseIdx: e.child_idx,
            displayClass: data.nodes[String(e.child_idx)]?.display_class ?? `#${e.child_idx}`,
            retained: data.nodes[String(e.child_idx)]?.retained ?? 0,
          })));
          return next;
        });
        return;
      }
      // Online: call WASM
      if (!session?.outbound_refs) return;
      try {
        const r = JSON.parse(session.outbound_refs(denseIdx, 50));
        if (r.ok) {
          setRefs(m => {
            const next = new Map(m);
            next.set(denseIdx, (r.refs as any[]).map(ref => ({
              field: ref.field_name || "",
              denseIdx: ref.dst_idx,
              displayClass: ref.display_class ?? `#${ref.dst_idx}`,
              retained: ref.retained ?? 0,
            })));
            return next;
          });
        }
      } catch {}
    };

    const toggleExpand = (denseIdx: number) => {
      setExpanded(s => {
        const next = new Set(s);
        if (next.has(denseIdx)) { next.delete(denseIdx); }
        else { next.add(denseIdx); fetchRefs(denseIdx); }
        return next;
      });
    };

    return (
      <div style={{ fontFamily: "monospace", fontSize: "0.8rem" }}>
        {rootBadge && (
          <div style={{
            display: "inline-block", border: "2px solid var(--accent, #3b82f6)",
            borderRadius: 4, padding: "2px 8px", background: "var(--accent-muted, #dbeafe)",
            color: "var(--accent)", fontWeight: 600, fontSize: "0.76rem", marginBottom: "2px",
          }}>
            [{rootBadge}]
          </div>
        )}
        {nodes.map((node, i) => {
          const isExp = expanded.has(node.denseIdx);
          const nodeRefs = refs.get(node.denseIdx) ?? [];
          const showAll = showAllRefs.has(node.denseIdx);
          const visibleRefs = showAll ? nodeRefs : nodeRefs.slice(0, 8);
          const canExpand = !node.isCurrent;

          return (
            <React.Fragment key={`${node.denseIdx}-${i}`}>
              {/* Connector arrow from above */}
              {(!node.isFirst || rootBadge) && (
                <div style={{ color: "var(--muted)", fontSize: "0.74rem", paddingLeft: "0.5rem" }}>
                  │{node.fieldName ? ` .${node.fieldName}` : ""}
                </div>
              )}
              {(!node.isFirst || rootBadge) && (
                <div style={{ color: "var(--muted)", paddingLeft: "0.5rem", fontSize: "0.78rem" }}>▼</div>
              )}
              {/* Chain node row */}
              <div style={{ display: "flex", alignItems: "center", gap: "0.4rem" }}>
                {/* Expand toggle */}
                {canExpand ? (
                  <button
                    className="btn-link"
                    style={{ fontSize: "0.7rem", width: "1.2em", flexShrink: 0, color: "var(--muted)" }}
                    title={isExp ? "Collapse outbound refs" : "Expand outbound refs"}
                    onClick={() => toggleExpand(node.denseIdx)}
                  >
                    {isExp ? "▼" : "▶"}
                  </button>
                ) : (
                  <span style={{ fontSize: "0.7rem", width: "1.2em", flexShrink: 0, color: "var(--accent)" }}>●</span>
                )}
                {/* Class name — navigate on click, bold if current */}
                {node.isCurrent ? (
                  <span style={{ fontFamily: "monospace", fontSize: "0.8rem", fontWeight: 600 }}>
                    {node.displayClass}
                  </span>
                ) : (
                  <button className="btn-link" style={{ fontFamily: "monospace", fontSize: "0.8rem" }}
                    onClick={() => navigate(node.denseIdx)}>
                    {node.displayClass}
                  </button>
                )}
                <span style={{ color: "var(--muted)", fontSize: "0.74rem", whiteSpace: "nowrap" }}>
                  {fmtB(node.retained)}
                </span>
                {node.isCurrent && (
                  <span style={{ fontSize: "0.7rem", color: "var(--muted)", fontStyle: "italic" }}>← here</span>
                )}
              </div>
              {/* Expanded outbound refs */}
              {isExp && (
                <div style={{ paddingLeft: "2rem", marginTop: "1px", marginBottom: "2px" }}>
                  {visibleRefs.length === 0 ? (
                    <span style={{ fontSize: "0.74rem", color: "var(--muted)" }}>No refs captured</span>
                  ) : visibleRefs.map((ref, ri) => (
                    <div key={ri} style={{ display: "flex", alignItems: "center", gap: "0.3rem", fontSize: "0.76rem", padding: "1px 0" }}>
                      <span style={{ color: "var(--muted)", flexShrink: 0 }}>
                        {ri === visibleRefs.length - 1 && !(!showAll && nodeRefs.length > 8) ? "└─" : "├─"}
                      </span>
                      {ref.field && <code style={{ fontSize: "0.72rem", color: "var(--muted)", flexShrink: 0 }}>.{ref.field}</code>}
                      <button className="btn-link" style={{ fontSize: "0.76rem", fontFamily: "monospace" }}
                        onClick={() => navigate(ref.denseIdx)}>
                        {ref.displayClass.split(".").pop()}
                      </button>
                      <span style={{ color: "var(--muted)", fontSize: "0.72rem", whiteSpace: "nowrap" }}>{fmtB(ref.retained)}</span>
                      <button className="btn-link" style={{ fontSize: "0.72rem", opacity: 0.6 }}
                        title="Navigate to this object"
                        onClick={() => navigate(ref.denseIdx)}>→</button>
                    </div>
                  ))}
                  {!showAll && nodeRefs.length > 8 && (
                    <div style={{ fontSize: "0.74rem", color: "var(--muted)", paddingLeft: "1.2em" }}>
                      <button className="btn-link" style={{ fontSize: "0.74rem" }}
                        onClick={() => setShowAllRefs(s => { const n = new Set(s); n.add(node.denseIdx); return n; })}>
                        … {nodeRefs.length - 8} more
                      </button>
                    </div>
                  )}
                </div>
              )}
            </React.Fragment>
          );
        })}
      </div>
    );
  }
  ```

- [ ] **Step 2: Build to confirm TypeScript compiles**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Expected: no TypeScript errors. Bundle size should increase by ~2–3 KB.

- [ ] **Step 3: Wire `WasmGcPathPanel` to delegate to `RetentionChain`**

  Replace the `return (` block of `WasmGcPathPanel` (currently rewritten in Task 5) with:

  ```tsx
  const chainNodes: ChainNode[] = (pathData.path as any[]).map((step: any, i: number, arr: any[]) => ({
    denseIdx: step.dense_idx,
    displayClass: step.display_class || data.nodes[String(step.dense_idx)]?.display_class || `obj#${step.dense_idx}`,
    retained: step.retained ?? data.nodes[String(step.dense_idx)]?.retained ?? 0,
    fieldName: step.field_name || undefined,
    isFirst: i === 0,
    isCurrent: i === arr.length - 1,
  }));
  return (
    <RetentionChain
      nodes={chainNodes}
      rootBadge={pathData.root_type}
      data={data}
      session={session}
      fmtB={fmtB}
      navigate={navigate}
    />
  );
  ```

- [ ] **Step 4: Replace offline dominator chain IIFE with `RetentionChain` (lines 7119–7171)**

  The offline "Retaining path ↑" block walks `idom` links upward from the current node (lines 7119–7171). Replace the entire `{(() => { ... })()}` block with:

  ```tsx
  {(() => {
    const chainNodes: ChainNode[] = [];
    let cur = currentNode.idom;
    let childId: number = nodeId!;
    const seen = new Set<number>();
    while (cur != null && !seen.has(cur) && chainNodes.length < pathDepth) {
      seen.add(cur);
      const n = data.nodes[String(cur)];
      if (!n) break;
      const edgeToChild = (data.edges[String(cur)] ?? []).find(e => e.child_idx === childId);
      chainNodes.push({
        denseIdx: cur,
        displayClass: n.display_class,
        retained: n.retained,
        fieldName: edgeToChild?.field_name || undefined,
        isFirst: false,
        isCurrent: false,
      });
      childId = cur;
      cur = n.idom;
    }
    if (chainNodes.length === 0) return null;
    // Reverse so root is at top, then add current object at bottom
    chainNodes.reverse();
    chainNodes.push({
      denseIdx: nodeId!,
      displayClass: currentNode.display_class,
      retained: currentNode.retained,
      fieldName: undefined,
      isFirst: false,
      isCurrent: true,
    });
    chainNodes[0].isFirst = true;
    const hasMore = cur != null && chainNodes.length >= pathDepth + 1;
    const wasmSession = (window as any).__wasmExploration;
    return (
      <div style={{ marginTop: "0.5rem" }}>
        <div style={{ fontSize: "0.78rem", color: "var(--muted)", fontWeight: 600, marginBottom: "2px" }}>
          Retaining path (dominator chain)
          <span title="Objects that dominate this object's memory. Not necessarily the actual reference path." style={{ cursor: "help", borderBottom: "1px dotted var(--muted)", marginLeft: "0.3rem", fontSize: "0.74rem" }}>(?)</span>
        </div>
        <RetentionChain
          nodes={chainNodes}
          data={data}
          session={wasmSession}
          fmtB={fmtB}
          navigate={(id) => navigate("explore", id, data.nodes[String(id)]?.display_class ?? `#${id}`)}
        />
        {hasMore && (
          <button className="btn-link" style={{ fontSize: "0.74rem", marginTop: "2px" }}
            onClick={() => setPathDepth(d => d + 20)}>
            ↑ … show more
          </button>
        )}
      </div>
    );
  })()}
  ```

- [ ] **Step 5: Replace multi-path additional paths render with `RetentionChain` (lines 6668–6685)**

  The multi-path render inside `{wasmAllPaths.paths.map((p, pi) => ...)}` currently renders flat `↓` rows. Replace the inner `<div style={{ paddingLeft: "0.75rem", marginTop: "0.2rem" }}>` block (lines 6673–6684) with:

  ```tsx
  <div style={{ paddingLeft: "0.5rem", marginTop: "0.2rem" }}>
    <RetentionChain
      nodes={(p.path as any[]).map((step: any, si: number, arr: any[]) => ({
        denseIdx: step.dense_idx,
        displayClass: step.display_class ?? `obj#${step.dense_idx}`,
        retained: step.retained ?? 0,
        fieldName: undefined,
        isFirst: si === 0,
        isCurrent: si === arr.length - 1,
      }))}
      rootBadge={p.root_type}
      data={data}
      session={(window as any).__wasmExploration}
      fmtB={fmtB}
      navigate={(id) => navigate("explore", id, data.nodes[String(id)]?.display_class ?? `#${id}`)}
    />
  </div>
  ```

- [ ] **Step 6: Build and verify**

  ```bash
  cd /Users/i560383_1/code/experiments/hprof-analyzer/web && npm run build
  ```

  Open a report. For each of these, verify the chain renders and ▶/▼ works:
  1. WASM Path to GC Root (online) — expand a mid-chain node, confirm refs appear.
  2. Offline Retaining path — expand a node, see static edges (offline) or WASM edges (online).
  3. Additional retention paths — open one, expand a node.

- [ ] **Step 7: Commit**

  ```bash
  git add web/src/App.tsx
  git commit -m "feat(ui): interactive RetentionChain — expand any path node to see outbound refs"
  ```

---

## Self-review

**Spec coverage:**
- P2 (heap concentration) — Task 3 ✓
- P4 (OQL history) — Task 1 ✓
- P5 (pin strip) — Task 2 ✓
- N1 (path between objects) — Task 7 ✓
- N2 (retained in breadcrumb) — Task 4 ✓
- N3 (idom jump) — already implemented (~line 7094); no task needed.
- N4 (address search) — Task 6 ✓
- N5 (visual GC path, aesthetic) — Task 5 ✓
- N5 (interactive chain, expand/collapse) — Task 8 ✓
- N6 ("top retainees" list) — domtree subtree_classes panel already renders this; no task needed.

**Not in this plan:**
- P1 ("Explain retention" AI summary card) — left for future; needs async multi-step WASM calls.
- P3 (Who-holds-X button from histogram) — the existing Sankey + `PivotBtn` / `OqlBtn` already provides this; no gap.

**Placeholder scan:** None. Every step has concrete code.

**Type consistency:**
- `ChainNode` type defined in Task 8 Step 1 and used in Steps 3–5. ✓
- `RetentionChain` props match all call sites in Steps 3–5. ✓
- `pinnedNodes`, `togglePin` introduced in Task 2 Step 1, used in Steps 2–4. ✓
- `pathSource`, `pathBetweenResult`, `pathBetweenError` introduced in Task 7 Step 3, used in Steps 4–5. ✓
- `find_dense_by_address(addr: u64)` takes a JS `number`; WASM-bindgen converts number→u64. ✓
- `find_path_between(src_idx: u32, dst_idx: u32)` called with `(pathSource.nodeId, nodeId!)` — both TypeScript `number`. ✓
- `fmtB` used in Task 3 comes from `useFmtBytes()` hook already in scope in `ObjectGraphExplorer`. ✓

**Task ordering:** Tasks 1–5 are pure UI, independent of each other. Task 6 requires the WASM build step before the JS wiring. Task 7 requires the WASM build step before the JS wiring. Task 8 is pure UI but should come after Task 5 (which rewrites `WasmGcPathPanel`) to avoid a double-rewrite conflict. Recommended execution order: 1 → 2 → 3 → 4 → 5 → 6 → 7 → 8.
