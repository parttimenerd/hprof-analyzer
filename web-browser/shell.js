// shell.js — injected into index.html after WASM init block.
// Outer scope provides: namedQueries, wasmReady, wasmComplete, HprofSession

const PROMPT = 'oql> ';
const HISTORY_KEY = 'hprof-analyzer.oql-history';

let serverUrl = null;
let term = null;
let classNames = [];  // populated after session loads (server may expose /class-names later)
let hasRetained = false;

// ── Populate offline named-query list on connect screen ───────────────────────
function populateOfflineList() {
  const list = document.getElementById('offline-query-list');
  let curGroup = '';
  namedQueries.forEach(q => {
    if (q.group !== curGroup) {
      curGroup = q.group;
      const lbl = document.createElement('div');
      lbl.className = 'nq-offline-group';
      lbl.textContent = curGroup;
      list.appendChild(lbl);
    }
    const item = document.createElement('div');
    item.className = 'nq-offline-item';
    item.title = q.oql;
    const nameEl = document.createElement('strong');
    nameEl.textContent = q.name;
    const descEl = document.createElement('span');
    descEl.textContent = q.display;
    item.appendChild(nameEl);
    item.appendChild(descEl);
    list.appendChild(item);
  });
}
if (wasmReady) populateOfflineList();

// ── Format a QueryValue cell for terminal display ─────────────────────────────
function fmtCell(cell) {
  if (cell === null || cell === undefined) return 'null';
  if (typeof cell !== 'object') return String(cell);
  const kind = cell.kind;
  const v = cell.v;
  if (kind === 'null') return 'null';
  if (kind === 'bool') return v ? 'true' : 'false';
  if (kind === 'int') return String(v);
  if (kind === 'float') return typeof v === 'number' ? v.toPrecision(6) : String(v);
  if (kind === 'str') return String(v);
  if (kind === 'obj_ref') {
    const cls = v && v.class ? v.class.split('.').pop() : '?';
    const idx = v && v.index !== undefined ? v.index : '?';
    return `<${cls}#${idx}>`;
  }
  // fallback
  return JSON.stringify(cell);
}

// Pad/truncate a string to a fixed column width
function padTo(s, w) {
  if (s.length > w) return s.slice(0, w - 1) + '…';
  return s.padEnd(w);
}

// ── Connect screen ────────────────────────────────────────────────────────────
document.getElementById('btn-connect').addEventListener('click', connectToServer);
document.getElementById('server-url').addEventListener('keydown', e => {
  if (e.key === 'Enter') connectToServer();
});

async function connectToServer() {
  const url = document.getElementById('server-url').value.trim().replace(/\/$/, '');
  const status = document.getElementById('connect-status');
  status.textContent = 'Connecting…';
  status.className = '';
  try {
    const res = await fetch(url + '/version', {
      signal: AbortSignal.timeout(4000),
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    const v = await res.json();
    status.textContent = `Connected to ${v.name || 'hprof-analyzer'} ${v.version || ''}`;
    status.className = 'ok';
    serverUrl = url;
    showShell();
  } catch (e) {
    const hint = e.message.includes('timeout') || e.message.includes('fetch')
      ? ' — run: hprof-analyzer query heap.hprof --server'
      : '';
    status.textContent = `Cannot connect: ${e.message}${hint}`;
    status.className = 'err';
  }
}

// ── Shell screen ──────────────────────────────────────────────────────────────
function showShell() {
  document.getElementById('connect-screen').style.display = 'none';
  document.getElementById('shell-screen').style.display = 'flex';
  document.getElementById('server-url-display').textContent = serverUrl;
  buildSidebar(false);
  startTerminal();
  pollAnalysisStatus();
}

document.getElementById('btn-disconnect').addEventListener('click', () => {
  serverUrl = null;
  hasRetained = false;
  if (term) { term.dispose(); term = null; }
  document.getElementById('shell-screen').style.display = 'none';
  document.getElementById('connect-screen').style.display = 'flex';
  document.getElementById('named-query-list').innerHTML = '';
  document.getElementById('connect-status').textContent = '';
  document.getElementById('connect-status').className = '';
  document.getElementById('btn-analyze').disabled = false;
  document.getElementById('analyze-status').textContent = '';
});

// ── Analysis trigger ──────────────────────────────────────────────────────────
document.getElementById('btn-analyze').addEventListener('click', async () => {
  const btn = document.getElementById('btn-analyze');
  const statusEl = document.getElementById('analyze-status');
  btn.disabled = true;
  statusEl.textContent = 'Starting analysis…';
  try {
    const res = await fetch(serverUrl + '/analyze', { method: 'POST' });
    const data = await res.json();
    if (data.ok) {
      statusEl.textContent = 'Analyzing…';
      pollAnalysisStatus();
    } else {
      statusEl.textContent = `Error: ${data.error || 'unknown'}`;
      btn.disabled = false;
    }
  } catch (e) {
    statusEl.textContent = `Error: ${e.message}`;
    btn.disabled = false;
  }
});

let pollTimer = null;
async function pollAnalysisStatus() {
  if (pollTimer) clearTimeout(pollTimer);
  if (!serverUrl) return;
  try {
    const res = await fetch(serverUrl + '/status');
    const data = await res.json();
    const statusEl = document.getElementById('analyze-status');
    const btn = document.getElementById('btn-analyze');
    if (data.status === 'ready') {
      hasRetained = true;
      statusEl.textContent = 'Analysis ready';
      btn.disabled = true;
      buildSidebar(true);
      if (term) term.writeln('\r\n\x1b[32m[Analysis complete — @retainedHeapSize queries now available]\x1b[0m');
    } else if (data.status === 'analyzing') {
      statusEl.textContent = 'Analyzing…';
      btn.disabled = true;
      pollTimer = setTimeout(pollAnalysisStatus, 2000);
    } else if (data.status === 'failed') {
      statusEl.textContent = `Analysis failed: ${data.error || ''}`;
      btn.disabled = false;
    } else {
      // not_started — leave button enabled
      statusEl.textContent = '';
    }
  } catch (_) {
    // server gone
  }
}

// ── Sidebar ───────────────────────────────────────────────────────────────────
function buildSidebar(analysisReady) {
  const list = document.getElementById('named-query-list');
  list.innerHTML = '';
  let curGroup = '';
  namedQueries.forEach(q => {
    if (q.group !== curGroup) {
      curGroup = q.group;
      const hdr = document.createElement('div');
      hdr.className = 'nq-group-hdr';
      hdr.textContent = curGroup;
      list.appendChild(hdr);
    }
    const disabled = q.needs_retained && !analysisReady;
    const card = document.createElement('div');
    card.className = 'nq-card' + (disabled ? ' needs-analysis' : '');
    card.title = disabled
      ? `${q.oql}\n\n[Requires full analysis — click "Run Analysis" first]`
      : q.oql;
    const nameEl = document.createElement('div');
    nameEl.className = 'nq-name';
    nameEl.textContent = q.name;
    const descEl = document.createElement('div');
    descEl.className = 'nq-display';
    descEl.textContent = q.display;
    card.appendChild(nameEl);
    card.appendChild(descEl);
    if (!disabled) {
      card.addEventListener('click', () => {
        if (term && window._hprofSetLine) window._hprofSetLine(q.oql);
      });
    }
    list.appendChild(card);
  });
}

// ── Terminal ──────────────────────────────────────────────────────────────────
function startTerminal() {
  term = new Terminal({
    theme: {
      background: '#0a0a14',
      foreground: '#c8c8dc',
      cursor: '#7ab4ff',
      selectionBackground: '#2a3a5a',
      black: '#1a1a2a',
      brightBlack: '#3a3a5a',
      cyan: '#60c8e0',
      brightCyan: '#80e0f8',
      green: '#70d080',
      brightGreen: '#90f0a0',
      yellow: '#d0b060',
      brightYellow: '#f0d080',
      blue: '#5080d0',
      brightBlue: '#70a0f8',
      red: '#d06060',
      brightRed: '#f08080',
    },
    cursorBlink: true,
    fontSize: 13,
    fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', Menlo, Consolas, monospace",
    scrollback: 8000,
    allowProposedApi: true,
  });
  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(document.getElementById('terminal-container'));
  fitAddon.fit();

  const ro = new ResizeObserver(() => fitAddon.fit());
  ro.observe(document.getElementById('terminal-container'));

  term.writeln('\x1b[1;36mhprof-analyzer OQL Shell\x1b[0m');
  term.writeln(`Connected to \x1b[32m${serverUrl}\x1b[0m`);
  term.writeln('\x1b[2mType an OQL query and press Enter. Tab for completions. /help for named queries.\x1b[0m');
  term.writeln('');
  term.write(PROMPT);

  let line = '';
  let histIdx = -1;
  const history = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');

  function setLine(newLine) {
    term.write('\r\x1b[K' + PROMPT + newLine);
    line = newLine;
    histIdx = -1;
  }
  window._hprofSetLine = setLine;

  function handleTab() {
    if (!wasmReady) return;
    try {
      const cs = JSON.parse(wasmComplete(line, line.length, classNames));
      if (cs.length === 0) return;
      if (cs.length === 1) {
        // Complete to the single suggestion
        const current = line.trimEnd();
        const tokens = current.split(/[\s,(]+/);
        const lastToken = tokens[tokens.length - 1] || '';
        const suffix = cs[0].value.slice(lastToken.length);
        if (suffix) {
          line += suffix;
          term.write(suffix);
        }
      } else {
        // Show a list of completions below the current line
        term.writeln('');
        const COLS = 3;
        const colW = 26;
        const limited = cs.slice(0, 30);
        for (let i = 0; i < limited.length; i += COLS) {
          const row = limited.slice(i, i + COLS);
          term.writeln('  ' + row.map(c => {
            const g = c.group ? `\x1b[2m(${c.group})\x1b[0m` : '';
            return `\x1b[36m${padTo(c.value, colW)}\x1b[0m ${g}`;
          }).join('  '));
        }
        if (cs.length > 30) {
          term.writeln(`  \x1b[2m… ${cs.length - 30} more\x1b[0m`);
        }
        term.write(PROMPT + line);
      }
    } catch (_) { /* ignore */ }
  }

  async function handleEnter(text) {
    if (!text.trim()) {
      term.write(PROMPT);
      return;
    }
    // Persist history
    if (history[0] !== text) {
      history.unshift(text);
      if (history.length > 500) history.pop();
      localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
    }
    histIdx = -1;

    const cmd = text.trim();
    if (cmd === '/help') {
      printHelp();
      term.write(PROMPT);
      return;
    }
    if (cmd === '/clear') {
      term.clear();
      term.write(PROMPT);
      return;
    }
    if (cmd === '/analyze') {
      document.getElementById('btn-analyze').click();
      term.writeln('\x1b[33mAnalysis triggered (watch the toolbar for status).\x1b[0m');
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/run ')) {
      const name = cmd.slice(5).trim();
      const q = namedQueries.find(q => q.name === name);
      if (!q) {
        const close = namedQueries
          .filter(q => q.name.toLowerCase().includes(name.toLowerCase()))
          .slice(0, 3)
          .map(q => q.name);
        const hint = close.length ? `  Did you mean: ${close.join(', ')}?` : '';
        term.writeln(`\x1b[31mUnknown query: "${name}".${hint}\x1b[0m`);
        term.writeln('\x1b[2mUse /help to list named queries.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      if (q.needs_retained && !hasRetained) {
        term.writeln('\x1b[33mThis query requires full analysis. Click "Run Analysis" in the toolbar first.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      term.writeln(`\x1b[2m↳ ${q.oql.length > 90 ? q.oql.slice(0, 89) + '…' : q.oql}\x1b[0m`);
      await runQuery(q.oql);
      return;
    }
    await runQuery(cmd);
  }

  async function runQuery(oql) {
    const t0 = performance.now();
    try {
      const res = await fetch(serverUrl + '/', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: oql }),
      });
      const elapsed = ((performance.now() - t0) / 1000).toFixed(3);
      let data;
      try {
        data = await res.json();
      } catch (_) {
        term.writeln(`\x1b[31merror: server returned non-JSON (HTTP ${res.status})\x1b[0m`);
        term.write(PROMPT);
        return;
      }

      if (!data.ok) {
        const msg = data.error?.message || JSON.stringify(data.error) || 'unknown error';
        const kind = data.error?.kind ? `\x1b[2m[${data.error.kind}]\x1b[0m ` : '';
        term.writeln(`\x1b[31merror: ${kind}${msg}\x1b[0m`);
      } else {
        const r = data.result;
        if (r.error) {
          term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
        } else if (r.columns && r.columns.length > 0) {
          // Print header
          const colNames = r.columns.map(c => c.name || String(c));
          const colW = Math.max(16, Math.floor((term.cols - 2) / Math.max(1, colNames.length)));
          const header = colNames.map(n => padTo(n, colW)).join('  ');
          term.writeln('\x1b[1m' + header + '\x1b[0m');
          term.writeln('\x1b[2m' + '─'.repeat(Math.min(header.length, term.cols - 2)) + '\x1b[0m');

          const rows = r.rows || [];
          const displayRows = rows.slice(0, 200);
          displayRows.forEach(row => {
            const cells = row.map(cell => padTo(fmtCell(cell), colW));
            term.writeln(cells.join('  '));
          });
          if (rows.length > 200) {
            term.writeln(`\x1b[2m… ${rows.length - 200} more rows (truncated display)\x1b[0m`);
          }
          const note = r.note ? `  \x1b[33m[${r.note}]\x1b[0m` : '';
          const trunc = r.truncated ? '  \x1b[33m[truncated]\x1b[0m' : '';
          term.writeln(`\x1b[2m${r.row_count} row${r.row_count !== 1 ? 's' : ''}, ${elapsed}s${trunc}${note}\x1b[0m`);
        } else {
          // No columns — just show the raw result
          term.writeln(JSON.stringify(r, null, 2).split('\n').slice(0, 40).join('\r\n'));
          term.writeln(`\x1b[2m${elapsed}s\x1b[0m`);
        }
      }
    } catch (e) {
      term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
    }
    term.write(PROMPT);
  }

  function printHelp() {
    term.writeln('\r\n\x1b[1mBuilt-in commands:\x1b[0m');
    term.writeln('  \x1b[36m/help\x1b[0m              — this message');
    term.writeln('  \x1b[36m/clear\x1b[0m             — clear terminal');
    term.writeln('  \x1b[36m/analyze\x1b[0m           — trigger full heap analysis (enables @retainedHeapSize)');
    term.writeln('  \x1b[36m/run <name>\x1b[0m        — run a named query');
    term.writeln('  \x1b[36mTab\x1b[0m                — OQL completion');
    term.writeln('  \x1b[36mUp/Down\x1b[0m            — history');
    term.writeln('  \x1b[36mCtrl+C\x1b[0m             — cancel current line');
    term.writeln('');
    term.writeln('\x1b[1mNamed queries\x1b[0m (/run <name>):');
    let cur = '';
    namedQueries.forEach(q => {
      if (q.group !== cur) {
        cur = q.group;
        term.writeln(`\r\n  \x1b[33m${cur}\x1b[0m`);
      }
      const lock = q.needs_retained ? '  \x1b[2m[needs analysis]\x1b[0m' : '';
      term.writeln(`    \x1b[36m${q.name.padEnd(36)}\x1b[0m  \x1b[2m${q.display}\x1b[0m${lock}`);
    });
    term.writeln('');
  }

  // ── Key handler ──────────────────────────────────────────────────────────────
  term.onKey(({ key, domEvent: ev }) => {
    const code = ev.key;

    if (code === 'Enter') {
      const text = line;
      line = '';
      histIdx = -1;
      term.writeln('');
      handleEnter(text);
      return;
    }

    if (code === 'Backspace') {
      if (line.length > 0) {
        line = line.slice(0, -1);
        term.write('\b \b');
      }
      return;
    }

    if (code === 'Tab') {
      ev.preventDefault();
      handleTab();
      return;
    }

    if (code === 'ArrowUp') {
      if (histIdx + 1 < history.length) {
        histIdx++;
        setLine(history[histIdx]);
      }
      return;
    }

    if (code === 'ArrowDown') {
      if (histIdx > 0) {
        histIdx--;
        setLine(history[histIdx]);
      } else if (histIdx === 0) {
        histIdx = -1;
        setLine('');
      }
      return;
    }

    if (code === 'ArrowLeft') {
      // Basic: not tracking cursor within line, ignore
      return;
    }

    if (code === 'ArrowRight') {
      return;
    }

    if (ev.ctrlKey && code === 'c') {
      term.writeln('^C');
      line = '';
      histIdx = -1;
      term.write(PROMPT);
      return;
    }

    if (ev.ctrlKey && code === 'l') {
      term.clear();
      term.write(PROMPT + line);
      return;
    }

    if (ev.ctrlKey && code === 'u') {
      if (line.length > 0) {
        term.write('\r\x1b[K' + PROMPT);
        line = '';
      }
      return;
    }

    // Printable characters only
    if (key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
      line += key;
      term.write(key);
    }
  });
}
