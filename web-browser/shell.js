// shell.js — injected into index.html after WASM init block.
// Outer scope provides: namedQueries, wasmReady, wasmComplete, HprofSession

const PROMPT = 'oql> ';
const HISTORY_KEY = 'hprof-analyzer.oql-history';
const SETTINGS_KEY = 'hprof-analyzer.settings';

// Display settings (persisted to localStorage)
const defaultSettings = { rowLimit: 200, bytesRaw: false, nullStr: 'null' };
let settings = Object.assign({}, defaultSettings,
  JSON.parse(localStorage.getItem(SETTINGS_KEY) || '{}'));

let serverUrl = null;
let term = null;
let classNames = [];  // populated after session loads (server may expose /class-names later)
let hasRetained = false;
let selectedFile = null;  // File object selected on the upload screen

// ── Screen helpers ────────────────────────────────────────────────────────────
function showScreen(id) {
  for (const sid of ['upload-screen', 'connect-screen', 'report-screen', 'shell-screen']) {
    const el = document.getElementById(sid);
    if (el) el.style.display = sid === id ? 'flex' : 'none';
  }
}

// ── Upload screen ─────────────────────────────────────────────────────────────
(function initUploadScreen() {
  const dropZone = document.getElementById('drop-zone');
  const fileInput = document.getElementById('file-input');
  const modeButtons = document.getElementById('mode-buttons');

  function onFileSelected(file) {
    if (!file) return;
    if (!file.name.endsWith('.hprof')) {
      // Accept anyway — the server validates; just warn visually
    }
    selectedFile = file;
    dropZone.classList.add('file-selected');
    document.getElementById('drop-zone-text').innerHTML =
      `<strong>${file.name}</strong> (${(file.size / 1024 / 1024).toFixed(1)} MB) — choose a mode below`;
    modeButtons.style.display = 'flex';
  }

  fileInput.addEventListener('change', () => {
    if (fileInput.files.length > 0) onFileSelected(fileInput.files[0]);
  });

  dropZone.addEventListener('dragover', e => {
    e.preventDefault();
    dropZone.classList.add('drag-over');
  });
  dropZone.addEventListener('dragleave', () => dropZone.classList.remove('drag-over'));
  dropZone.addEventListener('drop', e => {
    e.preventDefault();
    dropZone.classList.remove('drag-over');
    const file = e.dataTransfer.files[0];
    if (file) onFileSelected(file);
  });

  document.getElementById('btn-oql-shell').addEventListener('click', () => {
    showScreen('connect-screen');
    if (wasmReady) populateOfflineList();
  });

  document.getElementById('btn-analyze-report').addEventListener('click', () => {
    const msg = document.getElementById('report-message');
    msg.textContent =
      'Browser WASM analysis is not yet supported. ' +
      'Start the local server and use the OQL Shell mode: ' +
      'hprof-analyzer query heap.hprof --server';
    showScreen('report-screen');
  });
})();

// ── Report screen ─────────────────────────────────────────────────────────────
document.getElementById('btn-new-file').addEventListener('click', () => {
  showScreen('upload-screen');
});

document.getElementById('btn-to-shell').addEventListener('click', () => {
  showScreen('connect-screen');
  if (wasmReady) populateOfflineList();
});

// ── Populate offline named-query list on connect screen ───────────────────────
function populateOfflineList() {
  const list = document.getElementById('offline-query-list');
  if (list.hasChildNodes()) return;  // already populated
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

// ── Format a QueryValue cell for terminal display ─────────────────────────────
function isNumericKind(cell) {
  if (cell === null || cell === undefined) return false;
  if (typeof cell !== 'object') return false;
  return cell.kind === 'int' || cell.kind === 'float';
}

function fmtBytes(n) {
  if (n < 1024) return n + ' B';
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KB';
  if (n < 1024 * 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MB';
  return (n / 1024 / 1024 / 1024).toFixed(2) + ' GB';
}

function fmtCell(cell, colName) {
  if (cell === null || cell === undefined) return settings.nullStr;
  if (typeof cell !== 'object') return String(cell);
  const kind = cell.kind;
  const v = cell.v;
  if (kind === 'null') return settings.nullStr;
  if (kind === 'bool') return v ? 'true' : 'false';
  if (kind === 'int') {
    if (typeof v !== 'number') return String(v);
    // Address-like columns shown as hex
    if (colName && /address|addr|ptr/i.test(colName)) {
      return '0x' + v.toString(16).toUpperCase().padStart(8, '0');
    }
    // Byte-size columns shown as human-readable (unless bytesRaw)
    if (!settings.bytesRaw && colName && /bytes$|_size$|heap_size$/i.test(colName)) {
      return fmtBytes(v);
    }
    return v.toLocaleString('en-US');
  }
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

// Pad/truncate a string to a fixed column width (right-align if numeric)
function padTo(s, w, rightAlign) {
  if (s.length > w) return s.slice(0, w - 1) + '…';
  return rightAlign ? s.padStart(w) : s.padEnd(w);
}

// ── Connect screen ────────────────────────────────────────────────────────────
document.getElementById('btn-connect').addEventListener('click', connectToServer);
document.getElementById('server-url').addEventListener('keydown', e => {
  if (e.key === 'Enter') connectToServer();
});

async function connectToServer() {
  const url = document.getElementById('server-url').value.trim().replace(/\/$/, '');
  const status = document.getElementById('connect-status');
  document.getElementById('btn-connect').disabled = true;
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
    document.getElementById('btn-connect').disabled = false;
  }
}

// ── Shell screen ──────────────────────────────────────────────────────────────
function showShell() {
  showScreen('shell-screen');
  document.getElementById('server-url-display').textContent = serverUrl;
  buildSidebar(false);
  startTerminal();
  pollAnalysisStatus();
  // Fetch class names for tab-completion (non-blocking)
  fetch(serverUrl + '/help').then(r => r.json()).then(data => {
    if (Array.isArray(data.classes)) classNames = data.classes;
  }).catch(() => {});
}

document.getElementById('btn-disconnect').addEventListener('click', () => {
  serverUrl = null;
  hasRetained = false;
  if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
  if (term) { term.dispose(); term = null; }
  document.getElementById('named-query-list').innerHTML = '';
  document.getElementById('connect-status').textContent = '';
  document.getElementById('connect-status').className = '';
  document.getElementById('btn-analyze').disabled = false;
  document.getElementById('analyze-status').textContent = '';
  showScreen('upload-screen');
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
        if (term && window._hprofRunQuery) window._hprofRunQuery(q.oql);
        else if (term && window._hprofSetLine) window._hprofSetLine(q.oql);
      });
    }
    list.appendChild(card);
  });
}

// ── Terminal ──────────────────────────────────────────────────────────────────
function startTerminal() {
  if (term) { term.dispose(); term = null; }
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
  term.writeln('\x1b[2mType an OQL query and press Enter. Tab for completions. /help for commands.\x1b[0m');
  term.writeln('');
  term.write(PROMPT);

  let line = '';
  let cursorPos = 0;  // index within line where the cursor sits
  let histIdx = -1;
  const history = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
  let killRing = '';  // text killed by Ctrl+K/W/U

  // Ctrl+R incremental search state
  let isearching = false;
  let isearchQuery = '';
  let isearchMatch = -1;  // index into history of current match

  // Redraw line and reposition cursor; does NOT change histIdx
  function redrawLine() {
    term.write('\r\x1b[K' + PROMPT + line);
    if (cursorPos < line.length) {
      // Move cursor left from end to cursorPos
      term.write(`\x1b[${line.length - cursorPos}D`);
    }
  }

  function isearchPrompt() {
    const q = isearchQuery;
    const match = isearchMatch >= 0 ? history[isearchMatch] : '';
    const hi = match.toLowerCase().indexOf(q.toLowerCase());
    let display = match;
    if (hi >= 0 && q) {
      // Highlight matched portion in bold
      display = match.slice(0, hi) + '\x1b[1m' + match.slice(hi, hi + q.length) + '\x1b[0m' + match.slice(hi + q.length);
    }
    const label = `\x1b[35m(reverse-i-search)\x1b[0m \`${q}\`: `;
    const maxContent = term.cols - (label.replace(/\x1b\[[^m]*m/g, '').length) - 1;
    const truncDisplay = match.length > maxContent
      ? display.slice(0, maxContent - 1) + '…' : display;
    term.write('\r\x1b[K' + label + truncDisplay);
  }

  function exitIsearch(acceptMatch) {
    isearching = false;
    if (acceptMatch && isearchMatch >= 0) {
      line = history[isearchMatch];
    } else if (!acceptMatch) {
      // Keep line unchanged
    }
    cursorPos = line.length;
    isearchQuery = '';
    isearchMatch = -1;
    redrawLine();
  }

  function isearchStep() {
    if (!isearchQuery) { isearchMatch = -1; isearchPrompt(); return; }
    const q = isearchQuery.toLowerCase();
    const start = isearchMatch >= 0 ? isearchMatch + 1 : 0;
    let found = -1;
    for (let i = start; i < history.length; i++) {
      if (history[i].toLowerCase().includes(q)) { found = i; break; }
    }
    if (found < 0 && isearchMatch < 0) {
      // No match at all — try from the beginning
      found = history.findIndex(h => h.toLowerCase().includes(q));
    }
    isearchMatch = found;
    isearchPrompt();
  }

  function setLine(newLine) {
    line = newLine;
    cursorPos = newLine.length;
    histIdx = -1;
    redrawLine();
    term.focus();
  }
  window._hprofSetLine = setLine;

  async function runQueryFromSidebar(oql) {
    term.focus();
    // Show a truncated echo so long queries don't wrap across lines
    const maxEcho = term.cols - PROMPT.length - 1;
    const echo = oql.length > maxEcho ? oql.slice(0, maxEcho - 1) + '…' : oql;
    term.write('\r\x1b[K' + PROMPT + echo);
    line = '';
    cursorPos = 0;
    histIdx = -1;
    term.writeln('');
    await handleEnter(oql);
  }
  window._hprofRunQuery = runQueryFromSidebar;

  function handleTab() {
    if (!wasmReady) return;
    try {
      const cs = JSON.parse(wasmComplete(line, cursorPos, classNames));
      if (cs.length === 0) return;
      if (cs.length === 1) {
        // Complete to the single suggestion at cursor
        const current = line.slice(0, cursorPos).trimEnd();
        const tokens = current.split(/[\s,(]+/);
        const lastToken = tokens[tokens.length - 1] || '';
        const suffix = cs[0].value.slice(lastToken.length);
        if (suffix) {
          line = line.slice(0, cursorPos) + suffix + line.slice(cursorPos);
          cursorPos += suffix.length;
          redrawLine();
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
        redrawLine();
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
    if (cmd === '/status') {
      try {
        const res = await fetch(serverUrl + '/status');
        const data = await res.json();
        const st = data.status;
        if (st === 'ready') {
          term.writeln('\x1b[32m● Analysis ready — @retainedHeapSize queries available\x1b[0m');
        } else if (st === 'analyzing') {
          term.writeln('\x1b[33m● Analyzing heap… (wait for completion)\x1b[0m');
        } else if (st === 'failed') {
          term.writeln(`\x1b[31m● Analysis failed: ${data.error || '(no details)'}\x1b[0m`);
        } else {
          term.writeln('\x1b[2m● Analysis not started — use /analyze to begin\x1b[0m');
        }
      } catch (e) {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/classes' || cmd.startsWith('/classes ')) {
      const pattern = cmd.slice(8).trim().toLowerCase();
      const all = classNames.length > 0 ? classNames
        : (await fetch(serverUrl + '/help').then(r => r.json()).then(d => {
            if (Array.isArray(d.classes)) classNames = d.classes;
            return classNames;
          }).catch(() => []));
      const matches = pattern ? all.filter(c => c.toLowerCase().includes(pattern)) : all;
      if (matches.length === 0) {
        term.writeln(pattern
          ? `\x1b[33mNo classes matching "${pattern}"\x1b[0m`
          : '\x1b[2m(no classes loaded — server may still be loading)\x1b[0m');
      } else {
        const COLS = 2;
        const colW = Math.floor((term.cols - 4) / COLS);
        for (let i = 0; i < matches.length; i += COLS) {
          const row = matches.slice(i, i + COLS);
          term.writeln('  ' + row.map(c => padTo(c, colW)).join('  '));
        }
        term.writeln(`\x1b[2m${matches.length} class${matches.length !== 1 ? 'es' : ''}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/set' || cmd.startsWith('/set ')) {      const args = cmd.slice(4).trim().split(/\s+/);
      if (!args[0]) {
        // Print current settings
        term.writeln('\x1b[1mCurrent settings:\x1b[0m');
        term.writeln(`  limit    ${settings.rowLimit}  \x1b[2m(rows displayed per query)\x1b[0m`);
        term.writeln(`  bytes    ${settings.bytesRaw ? 'raw' : 'human'}  \x1b[2m(raw = show bytes as numbers)\x1b[0m`);
        term.writeln(`  null     "${settings.nullStr}"  \x1b[2m(how null values display)\x1b[0m`);
        term.writeln('\x1b[2mUsage: /set limit 500 | /set bytes raw | /set bytes human | /set null ∅\x1b[0m');
      } else if (args[0] === 'limit') {
        const n = parseInt(args[1], 10);
        if (!n || n < 1 || n > 10000) {
          term.writeln('\x1b[31mUsage: /set limit <1–10000>\x1b[0m');
        } else {
          settings.rowLimit = n;
          localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
          term.writeln(`\x1b[32mrow limit set to ${n}\x1b[0m`);
        }
      } else if (args[0] === 'bytes') {
        if (args[1] === 'raw') { settings.bytesRaw = true; }
        else if (args[1] === 'human') { settings.bytesRaw = false; }
        else { term.writeln('\x1b[31mUsage: /set bytes raw|human\x1b[0m'); term.write(PROMPT); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32mbytes display: ${settings.bytesRaw ? 'raw numbers' : 'human-readable'}\x1b[0m`);
      } else if (args[0] === 'null') {
        settings.nullStr = args[1] || 'null';
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32mnull display: "${settings.nullStr}"\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mUnknown setting: ${args[0]}. Options: limit, bytes, null\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/export') {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result to export — run a query first.\x1b[0m');
      } else {
        const tsv = [lastResult.columns.join('\t')]
          .concat(lastResult.rows.map(row =>
            row.map((cell, i) => fmtCell(cell, lastResult.columns[i])).join('\t')
          ))
          .join('\n');
        try {
          await navigator.clipboard.writeText(tsv);
          term.writeln(`\x1b[32m✓ Copied ${lastResult.rows.length} rows as TSV to clipboard\x1b[0m`);
        } catch (_) {
          // Clipboard API unavailable — offer a download instead
          const blob = new Blob([tsv], { type: 'text/tab-separated-values' });
          const url = URL.createObjectURL(blob);
          const a = document.createElement('a');
          a.href = url; a.download = 'query-result.tsv'; a.click();
          URL.revokeObjectURL(url);
          term.writeln(`\x1b[32m✓ Downloaded result as query-result.tsv (${lastResult.rows.length} rows)\x1b[0m`);
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/history') {
      if (recent.length === 0) {
        term.writeln('\x1b[2m(no history yet)\x1b[0m');
      } else {
        recent.forEach((h, i) => {
          const num = String(i + 1).padStart(3);
          const truncated = h.length > term.cols - 6 ? h.slice(0, term.cols - 7) + '…' : h;
          term.writeln(`\x1b[2m${num}\x1b[0m  ${truncated}`);
        });
        term.writeln(`\x1b[2m  Use !N to re-run entry N\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    // !N — re-run history entry
    if (/^!\d+$/.test(cmd)) {
      const n = parseInt(cmd.slice(1), 10) - 1;
      if (n < 0 || n >= history.length) {
        term.writeln(`\x1b[31mNo history entry ${cmd.slice(1)}\x1b[0m`);
        term.write(PROMPT);
      } else {
        const recalled = history[n];
        const echo = recalled.length > term.cols - PROMPT.length - 1
          ? recalled.slice(0, term.cols - PROMPT.length - 2) + '…' : recalled;
        term.writeln(`\x1b[2m↳ ${echo}\x1b[0m`);
        await runQuery(recalled);
      }
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

  let currentAbort = null;  // AbortController for in-flight query
  let lastResult = null;    // { columns, rows } of last successful query for /export

  async function runQuery(oql) {
    const t0 = performance.now();
    const abortCtrl = new AbortController();
    currentAbort = abortCtrl;
    // Animated spinner while waiting for response
    const spinFrames = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];
    let spinIdx = 0;
    term.write('\x1b[2m' + spinFrames[0] + ' running\x1b[0m');
    const spinTimer = setInterval(() => {
      spinIdx = (spinIdx + 1) % spinFrames.length;
      term.write('\r\x1b[K\x1b[2m' + spinFrames[spinIdx] + ' running\x1b[0m');
    }, 80);
    try {
      const res = await fetch(serverUrl + '/', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ query: oql }),
        signal: abortCtrl.signal,
      });
      clearInterval(spinTimer);
      currentAbort = null;
      // Erase spinner
      term.write('\r\x1b[K');
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
        const report = data.error?.report;
        if (report) {
          // Ariadne caret diagnostic — show each line with proper \r\n
          report.split('\n').forEach(l => term.writeln(l));
        } else {
          term.writeln(`\x1b[31merror: ${kind}${msg}\x1b[0m`);
        }
      } else {
        const r = data.result;
        if (r.error) {
          term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
        } else if (r.columns && r.columns.length > 0) {
          const colNames = r.columns.map(c => c.name || String(c));
          const rows = r.rows || [];
          // Detect numeric columns (right-align) by checking first non-null value
          const isNumeric = colNames.map((_, i) => {
            const sample = rows.find(row => row[i] !== null && row[i] !== undefined);
            return sample ? isNumericKind(sample[i]) : false;
          });
          // Per-column width: max of header and content, capped so total fits terminal
          const colW = colNames.map((n, i) => {
            const contentMax = rows.slice(0, settings.rowLimit).reduce((m, row) => Math.max(m, fmtCell(row[i], n).length), 0);
            return Math.max(n.length, contentMax, 4);
          });
          // Scale down proportionally if total exceeds terminal width
          const gap = 2;
          const totalW = colW.reduce((s, w) => s + w + gap, 0) - gap;
          const maxW = term.cols - 2;
          const scale = totalW > maxW ? maxW / totalW : 1;
          const adjW = colW.map(w => Math.max(4, Math.floor(w * scale)));

          const header = colNames.map((n, i) => padTo(n, adjW[i], isNumeric[i])).join('  ');
          term.writeln('\x1b[1m' + header + '\x1b[0m');
          term.writeln('\x1b[2m' + '─'.repeat(Math.min(header.length, term.cols - 2)) + '\x1b[0m');

          const displayRows = rows.slice(0, settings.rowLimit);
          displayRows.forEach(row => {
            const cells = row.map((cell, i) => padTo(fmtCell(cell, colNames[i]), adjW[i], isNumeric[i]));
            term.writeln(cells.join('  '));
          });
          if (rows.length > settings.rowLimit) {
            term.writeln(`\x1b[2m… ${rows.length - settings.rowLimit} more rows (display limit ${settings.rowLimit} — use /set limit N)\x1b[0m`);
          }
          lastResult = { columns: colNames, rows };
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
      clearInterval(spinTimer);
      currentAbort = null;
      term.write('\r\x1b[K');
      if (e.name === 'AbortError') {
        term.writeln('\x1b[33mcancelled\x1b[0m');
      } else {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
    }
    term.write(PROMPT);
  }

  function printHelp() {
    term.writeln('\r\n\x1b[1mBuilt-in commands:\x1b[0m');
    term.writeln('  \x1b[36m/help\x1b[0m              — this message');
    term.writeln('  \x1b[36m/clear\x1b[0m             — clear terminal');
    term.writeln('  \x1b[36m/status\x1b[0m            — show analysis status');
    term.writeln('  \x1b[36m/analyze\x1b[0m           — trigger full heap analysis (enables @retainedHeapSize)');
    term.writeln('  \x1b[36m/history\x1b[0m           — show recent query history');
    term.writeln('  \x1b[36m/export\x1b[0m            — copy last result to clipboard as TSV');
    term.writeln('  \x1b[36m/set [key val]\x1b[0m     — view/change display settings (limit, bytes, null)');
    term.writeln('  \x1b[36m/classes [pat]\x1b[0m     — list class names (optionally filtered by pattern)');
    term.writeln('  \x1b[36m/run <name>\x1b[0m        — run a named query');
    term.writeln('');
    term.writeln('\x1b[1mKeyboard shortcuts:\x1b[0m');
    term.writeln('  \x1b[36mTab\x1b[0m                — OQL completion');
    term.writeln('  \x1b[36mUp/Down\x1b[0m            — history');
    term.writeln('  \x1b[36mCtrl+R\x1b[0m             — incremental history search');
    term.writeln('  \x1b[36mLeft/Right\x1b[0m         — move cursor (Ctrl/Alt: by word)');
    term.writeln('  \x1b[36mHome / Ctrl+A\x1b[0m      — beginning of line');
    term.writeln('  \x1b[36mEnd  / Ctrl+E\x1b[0m      — end of line');
    term.writeln('  \x1b[36mCtrl+K\x1b[0m             — kill to end of line');
    term.writeln('  \x1b[36mCtrl+W\x1b[0m             — kill previous word');
    term.writeln('  \x1b[36mCtrl+U\x1b[0m             — kill to beginning of line');
    term.writeln('  \x1b[36mCtrl+Y\x1b[0m             — yank (paste) killed text');
    term.writeln('  \x1b[36mCtrl+C\x1b[0m             — cancel current line / abort query');
    term.writeln('  \x1b[36mCtrl+L\x1b[0m             — clear screen');
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

  // ── Paste handler — xterm fires onData for pasted text ──────────────────────
  term.onData(data => {
    // Filter to printable ASCII range (avoid control sequences from paste)
    const printable = data.replace(/[^\x20-\x7E -￿]/g, '');
    if (!printable) return;
    if (printable.length > 1 || printable !== data) {
      // Multi-char or filtered: it's a paste
      line = line.slice(0, cursorPos) + printable + line.slice(cursorPos);
      cursorPos += printable.length;
      redrawLine();
    }
    // Single printable chars are handled by onKey
  });

  // ── Key handler ──────────────────────────────────────────────────────────────
  term.onKey(({ key, domEvent: ev }) => {
    const code = ev.key;

    // Ctrl+R incremental reverse search — intercept most keys while active
    if (isearching) {
      if (code === 'Enter') {
        exitIsearch(true);
        term.writeln('');
        const text = line;
        line = '';
        cursorPos = 0;
        handleEnter(text);
        return;
      }
      if (code === 'Escape' || (ev.ctrlKey && (code === 'g' || code === 'c'))) {
        exitIsearch(false);
        return;
      }
      if (code === 'Backspace') {
        isearchQuery = isearchQuery.slice(0, -1);
        isearchMatch = -1;
        isearchStep();
        return;
      }
      if (ev.ctrlKey && code === 'r') {
        // Deeper search — handled below
        isearchStep();
        return;
      }
      // Any other control key: accept match and fall through
      if (ev.ctrlKey || ev.metaKey || ev.altKey || code.length > 1) {
        exitIsearch(true);
        // fall through to normal handling below
      } else {
        isearchQuery += key;
        isearchMatch = -1;
        isearchStep();
        return;
      }
    }

    if (code === 'Enter') {
      const text = line;
      line = '';
      cursorPos = 0;
      histIdx = -1;
      term.writeln('');
      handleEnter(text);
      return;
    }

    if (code === 'Backspace') {
      if (cursorPos > 0) {
        line = line.slice(0, cursorPos - 1) + line.slice(cursorPos);
        cursorPos--;
        redrawLine();
      }
      return;
    }

    if (code === 'Delete') {
      if (cursorPos < line.length) {
        line = line.slice(0, cursorPos) + line.slice(cursorPos + 1);
        redrawLine();
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
      if (ev.ctrlKey || ev.altKey) {
        // Jump to previous word boundary
        let p = cursorPos;
        while (p > 0 && line[p - 1] === ' ') p--;
        while (p > 0 && line[p - 1] !== ' ') p--;
        if (p !== cursorPos) { cursorPos = p; redrawLine(); }
      } else if (cursorPos > 0) {
        cursorPos--;
        term.write('\x1b[D');
      }
      return;
    }

    if (code === 'ArrowRight') {
      if (ev.ctrlKey || ev.altKey) {
        // Jump to next word boundary
        let p = cursorPos;
        while (p < line.length && line[p] !== ' ') p++;
        while (p < line.length && line[p] === ' ') p++;
        if (p !== cursorPos) { cursorPos = p; redrawLine(); }
      } else if (cursorPos < line.length) {
        cursorPos++;
        term.write('\x1b[C');
      }
      return;
    }

    if (code === 'Home' || (ev.ctrlKey && code === 'a')) {
      if (cursorPos > 0) { cursorPos = 0; redrawLine(); }
      return;
    }

    if (code === 'End' || (ev.ctrlKey && code === 'e')) {
      if (cursorPos < line.length) { cursorPos = line.length; redrawLine(); }
      return;
    }

    if (ev.ctrlKey && code === 'r') {
      isearching = true;
      isearchQuery = '';
      isearchMatch = -1;
      isearchPrompt();
      return;
    }

    if (ev.ctrlKey && code === 'c') {
      if (currentAbort) {
        currentAbort.abort();
        currentAbort = null;
      } else {
        term.writeln('^C');
        line = '';
        cursorPos = 0;
        histIdx = -1;
        term.write(PROMPT);
      }
      return;
    }

    if (ev.ctrlKey && code === 'l') {
      term.clear();
      redrawLine();
      return;
    }

    if (ev.ctrlKey && code === 'u') {
      if (line.length > 0) {
        killRing = line.slice(0, cursorPos);
        line = line.slice(cursorPos);
        cursorPos = 0;
        redrawLine();
      }
      return;
    }

    if (ev.ctrlKey && code === 'k') {
      if (cursorPos < line.length) {
        killRing = line.slice(cursorPos);
        line = line.slice(0, cursorPos);
        redrawLine();
      }
      return;
    }

    if (ev.ctrlKey && code === 'w') {
      // Kill previous word
      let p = cursorPos;
      while (p > 0 && line[p - 1] === ' ') p--;
      while (p > 0 && line[p - 1] !== ' ') p--;
      if (p !== cursorPos) {
        killRing = line.slice(p, cursorPos);
        line = line.slice(0, p) + line.slice(cursorPos);
        cursorPos = p;
        redrawLine();
      }
      return;
    }

    if (ev.ctrlKey && code === 'y') {
      // Yank (paste) kill ring
      if (killRing) {
        line = line.slice(0, cursorPos) + killRing + line.slice(cursorPos);
        cursorPos += killRing.length;
        redrawLine();
      }
      return;
    }

    // Printable characters only — insert at cursorPos
    if (key.length === 1 && !ev.ctrlKey && !ev.metaKey && !ev.altKey) {
      line = line.slice(0, cursorPos) + key + line.slice(cursorPos);
      cursorPos++;
      if (cursorPos === line.length) {
        // Cursor at end — just append (no redraw flicker)
        term.write(key);
      } else {
        redrawLine();
      }
    }
  });
}
