// shell.js — injected into index.html after WASM init block.
// Outer scope provides: namedQueries, wasmReady, wasmComplete, HprofSession

const PROMPT = 'oql> ';
const HISTORY_KEY = 'hprof-analyzer.oql-history';
const SETTINGS_KEY = 'hprof-analyzer.settings';
const LAST_URL_KEY = 'hprof-analyzer.last-url';
const BOOKMARKS_KEY = 'hprof-analyzer.bookmarks';

// Restore last-used server URL into the input on page load
(function restoreLastUrl() {
  const saved = localStorage.getItem(LAST_URL_KEY);
  if (saved) {
    const el = document.getElementById('server-url');
    if (el) el.value = saved;
  }
})();

// Display settings (persisted to localStorage)
const defaultSettings = { rowLimit: 200, bytesRaw: false, nullStr: 'null', color: true };
let settings = Object.assign({}, defaultSettings,
  JSON.parse(localStorage.getItem(SETTINGS_KEY) || '{}'));
// rowLimit: 0 stored means "unlimited" — convert to Infinity at runtime
if (settings.rowLimit === 0) settings.rowLimit = Infinity;

let serverUrl = null;
let serverVersion = null;  // { name, version } from /version endpoint
let term = null;
let classNames = [];  // populated after session loads (server may expose /class-names later)
let fieldNames = [];  // populated after session loads from /help endpoint
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
  if (n < 1024 * 1024) return (n / 1024).toFixed(1) + ' KiB';
  if (n < 1024 * 1024 * 1024) return (n / 1024 / 1024).toFixed(1) + ' MiB';
  return (n / 1024 / 1024 / 1024).toFixed(1) + ' GiB';
}

// Resolve a column specifier (name substring OR 1-based number) to an index.
// Returns -1 if not found.
function resolveCol(spec, columns) {
  const n = parseInt(spec, 10);
  if (!isNaN(n) && String(n) === spec && n >= 1 && n <= columns.length) return n - 1;
  const lo = spec.toLowerCase();
  return columns.findIndex(c => c.toLowerCase() === lo || c.toLowerCase().includes(lo));
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
    serverVersion = v;
    localStorage.setItem(LAST_URL_KEY, url);
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

function startKeepalive() {
  if (keepaliveTimer) return;
  keepaliveTimer = setInterval(async () => {
    if (!serverUrl) { clearInterval(keepaliveTimer); keepaliveTimer = null; return; }
    try {
      await fetch(serverUrl + '/status', { signal: AbortSignal.timeout(3000) });
    } catch (_) {
      clearInterval(keepaliveTimer);
      keepaliveTimer = null;
      // Server gone — show a banner in the terminal if it's open
      if (term) {
        term.writeln('\r\n\x1b[31m[Server connection lost — reconnect with a new session]\x1b[0m');
        const badge = document.getElementById('server-badge');
        if (badge) { badge.textContent = '● Disconnected'; badge.style.color = '#d06060'; }
      }
    }
  }, 15000);
}

// ── Shell screen ──────────────────────────────────────────────────────────────
function showShell() {
  showScreen('shell-screen');
  document.getElementById('server-url-display').textContent = serverUrl;
  buildSidebar(false);
  startTerminal();
  pollAnalysisStatus();
  startKeepalive();
  // Fetch class names for tab-completion (non-blocking)
  fetch(serverUrl + '/help').then(r => r.json()).then(data => {
    if (Array.isArray(data.classes)) classNames = data.classes;
    if (Array.isArray(data.fields)) fieldNames = data.fields;
  }).catch(() => {});
}

document.getElementById('btn-disconnect').addEventListener('click', () => {
  serverUrl = null;
  hasRetained = false;
  if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
  if (keepaliveTimer) { clearInterval(keepaliveTimer); keepaliveTimer = null; }
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
let keepaliveTimer = null;  // detect server disconnects

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

// ── Sidebar search ────────────────────────────────────────────────────────────
(function initSidebarSearch() {
  const input = document.getElementById('sidebar-search');
  if (!input) return;
  input.addEventListener('input', () => {
    const q = input.value.toLowerCase();
    const list = document.getElementById('named-query-list');
    if (!list) return;
    let lastGroupHdr = null;
    let groupHasVisible = false;
    for (const el of list.children) {
      if (el.classList.contains('nq-group-hdr')) {
        if (lastGroupHdr) lastGroupHdr.classList.toggle('nq-hidden', !groupHasVisible);
        lastGroupHdr = el;
        groupHasVisible = false;
      } else if (el.classList.contains('nq-card')) {
        const haystack = (el.textContent + ' ' + (el.dataset.oql || '')).toLowerCase();
        const match = !q || haystack.includes(q);
        el.classList.toggle('nq-hidden', !match);
        if (match) groupHasVisible = true;
      }
    }
    if (lastGroupHdr) lastGroupHdr.classList.toggle('nq-hidden', !groupHasVisible);
  });
  // Prevent sidebar search from grabbing arrow keys / Enter from terminal
  input.addEventListener('keydown', e => e.stopPropagation());
})();

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
    card.dataset.oql = q.oql;  // used by sidebar search to match OQL content
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
    } else {
      card.addEventListener('click', () => {
        if (term) {
          term.writeln(`\x1b[33m[${q.name}] requires full analysis — click "Run Analysis" in the toolbar first\x1b[0m`);
          term.focus();
        }
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

  const verStr = serverVersion ? ` \x1b[2mv${serverVersion.version || ''}\x1b[0m` : '';
  const histCount = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]').length;
  term.writeln('\x1b[1;36m hprof-analyzer\x1b[0m\x1b[36m OQL Shell\x1b[0m' + verStr);
  term.writeln(`\x1b[2m └─ ${serverUrl}`
    + (namedQueries.length ? `  ·  ${namedQueries.length} named queries` : '')
    + (histCount ? `  ·  ${histCount} history entries` : '')
    + '\x1b[0m');
  term.writeln('\x1b[2m    Tab = complete  ·  Ctrl+R = search history  ·  /help = commands\x1b[0m');
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

  let inputRowCount = 1;  // terminal rows occupied by current input (for internal tracking)
  let pendingLines = [];  // lines accumulated for multi-line query (via \ continuation)
  const CONT_PROMPT = '...> ';  // shown on continuation lines

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
    if (isearchMatch < 0) {
      const noMatch = q ? '\x1b[31m(no match)\x1b[0m' : '';
      const label = `\x1b[35m(reverse-i-search)\x1b[0m \`${q}\`: `;
      term.write('\r\x1b[K' + label + noMatch);
      return;
    }
    const match = history[isearchMatch];
    const hi = match.toLowerCase().indexOf(q.toLowerCase());
    let display = match;
    if (hi >= 0 && q) {
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
    if (found < 0) {
      // Wrap around: search from beginning up to (but not including) start
      for (let i = 0; i < start; i++) {
        if (history[i].toLowerCase().includes(q)) { found = i; break; }
      }
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
    // Complete !<bookmark>
    if (line.startsWith('!') && !line.includes(' ')) {
      const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
      const partial = line.slice(1).toLowerCase();
      const bNames = Object.keys(bookmarks);
      const bMatches = bNames.filter(n => n.toLowerCase().startsWith(partial));
      if (bMatches.length === 1) { setLine('!' + bMatches[0]); }
      else if (bMatches.length > 1) {
        term.writeln('');
        term.writeln('  ' + bMatches.map(n => `\x1b[35m!${n}\x1b[0m`).join('  '));
        redrawLine();
      }
      return;
    }
    // Complete / commands
    if (line.startsWith('/') && !line.includes(' ')) {
      const partial = line.slice(1).toLowerCase();
      const cmds = ['help','clear','status','analyze','history','export','set','classes','fields','plan','explain','filter','grep',
                    'sort','unique','pivot','stats','top','head','tail','row','undo','sample','cols','columns','select','drop','rename','wc','limit','not','exclude','distinct','dedup','obj','run','bookmark','save','forget','last','describe','count','watch','q','quit','disconnect'];
      const matches = cmds.filter(c => c.startsWith(partial));
      if (matches.length === 1) {
        setLine('/' + matches[0] + ' ');
      } else if (matches.length > 1) {
        term.writeln('');
        term.writeln('  ' + matches.map(c => '\x1b[36m/' + c + '\x1b[0m').join('  '));
        redrawLine();
      }
      return;
    }
    // Complete /run <name>
    if (line.startsWith('/run ')) {
      const partial = line.slice(5).toLowerCase();
      const matches = namedQueries.filter(q => q.name.toLowerCase().startsWith(partial));
      if (matches.length === 1) {
        setLine('/run ' + matches[0].name);
      } else if (matches.length > 1) {
        term.writeln('');
        matches.forEach(q => term.writeln(`  \x1b[36m${q.name.padEnd(36)}\x1b[0m  \x1b[2m${q.display}\x1b[0m`));
        redrawLine();
      }
      return;
    }
    // Complete /describe <class> and /count <class> and /obj <class>
    const classCmd = line.startsWith('/describe ') ? '/describe '
                   : line.startsWith('/count ')    ? '/count '
                   : line.startsWith('/obj ')      ? '/obj '
                   : null;
    if (classCmd && classNames.length > 0) {
      const partial = line.slice(classCmd.length);
      const matches = classNames.filter(c => c.toLowerCase().startsWith(partial.toLowerCase()));
      if (matches.length === 1) {
        setLine(classCmd + matches[0]);
      } else if (matches.length > 1 && matches.length <= 20) {
        term.writeln('');
        term.writeln('  ' + matches.map(c => `\x1b[36m${c}\x1b[0m`).join('  '));
        redrawLine();
      }
      return;
    }
    // Complete /fields <pattern>
    if (line.startsWith('/fields ') && fieldNames.length > 0) {
      const partial = line.slice(8).toLowerCase();
      const matches = fieldNames.filter(f => f.toLowerCase().startsWith(partial));
      if (matches.length === 1) {
        setLine('/fields ' + matches[0]);
      } else if (matches.length > 1 && matches.length <= 20) {
        term.writeln('');
        term.writeln('  ' + matches.map(f => `\x1b[36m${f}\x1b[0m`).join('  '));
        redrawLine();
      }
      return;
    }
    // Complete /export csv|tsv|json
    if (line.startsWith('/export ')) {
      const partial = line.slice(8).toLowerCase();
      const fmts = ['tsv', 'csv', 'json'].filter(f => f.startsWith(partial));
      if (fmts.length === 1) { setLine('/export ' + fmts[0]); }
      else if (fmts.length > 1) { term.writeln(''); term.writeln('  ' + fmts.join('  ')); redrawLine(); }
      return;
    }
    // Complete /set <key> and /set <key> <value>
    if (line.startsWith('/set ')) {
      const rest = line.slice(5);
      const parts = rest.trimStart().split(/\s+/);
      if (parts.length <= 1) {
        // completing the key
        const partial = rest.trim().toLowerCase();
        const keys = ['limit', 'bytes', 'null', 'color'].filter(k => k.startsWith(partial));
        if (keys.length === 1) { setLine('/set ' + keys[0] + ' '); }
        else if (keys.length > 1) { term.writeln(''); term.writeln('  ' + keys.join('  ')); redrawLine(); }
      } else {
        // completing the value
        const key = parts[0].toLowerCase();
        const partial = (parts[1] || '').toLowerCase();
        const valueMap = { bytes: ['raw', 'human'], color: ['on', 'off'] };
        const vals = (valueMap[key] || []).filter(v => v.startsWith(partial));
        if (vals.length === 1) { setLine('/set ' + key + ' ' + vals[0]); }
        else if (vals.length > 1) { term.writeln(''); term.writeln('  ' + vals.join('  ')); redrawLine(); }
      }
      return;
    }
    // Complete /forget <bookmark> and /bookmark <bookmark>
    if (line.startsWith('/forget ') || line.startsWith('/bookmark ') || line.startsWith('/save ')) {
      const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
      const pfxLen = line.startsWith('/forget ') ? 8 : line.startsWith('/save ') ? 6 : 10;
      const partial = line.slice(pfxLen).toLowerCase();
      const bNames = Object.keys(bookmarks).filter(n => n.toLowerCase().startsWith(partial));
      if (bNames.length === 1) { setLine(line.slice(0, pfxLen) + bNames[0]); }
      else if (bNames.length > 1) {
        term.writeln('');
        term.writeln('  ' + bNames.map(n => `\x1b[35m${n}\x1b[0m`).join('  '));
        redrawLine();
      }
      return;
    }
    // Complete column names from lastResult for result-manipulation commands
    if (lastResult && lastResult.columns.length > 0) {
      // /sort supports comma-separated multi-column: complete the last segment; strip - prefix for desc
      if (line.startsWith('/sort ')) {
        const afterCmd = line.slice(6);
        const lastComma = afterCmd.lastIndexOf(',');
        const prefix = lastComma >= 0 ? line.slice(0, 6 + lastComma + 1).trimEnd() + ' ' : '/sort ';
        const segment = (lastComma >= 0 ? afterCmd.slice(lastComma + 1) : afterCmd).trim();
        const isNeg = segment.startsWith('-');
        const partial = (isNeg ? segment.slice(1) : segment).toLowerCase();
        const cols = lastResult.columns.filter(c => c.toLowerCase().startsWith(partial));
        if (cols.length === 1) { setLine(prefix + (isNeg ? '-' : '') + cols[0]); }
        else if (cols.length > 1 && cols.length <= 20) {
          term.writeln('');
          term.writeln('  ' + cols.map(c => `\x1b[36m${isNeg ? '-' : ''}${c}\x1b[0m`).join('  '));
          redrawLine();
        }
        return;
      }
      // Single-column commands: /filter /grep /unique /stats /pivot /select /not /exclude /rename /sample /wc
      const singleColCmds = ['/filter ', '/grep ', '/unique ', '/stats ', '/pivot ', '/select ',
                              '/drop ', '/not ', '/exclude ', '/rename ', '/sample ', '/wc '];
      const matched = singleColCmds.find(p => line.startsWith(p));
      if (matched) {
        const rawArg = line.slice(matched.length);
        const isAtCol = rawArg.startsWith('@') && (matched === '/filter ' || matched === '/grep ' || matched === '/not ' || matched === '/exclude ');
        const partial = (isAtCol ? rawArg.slice(1) : rawArg).toLowerCase();
        const cols = lastResult.columns.filter(c => c.toLowerCase().startsWith(partial));
        if (cols.length === 1) { setLine(matched + (isAtCol ? '@' : '') + cols[0]); }
        else if (cols.length > 1 && cols.length <= 20) {
          term.writeln('');
          term.writeln('  ' + cols.map(c => `\x1b[36m${isAtCol ? '@' : ''}${c}\x1b[0m`).join('  '));
          redrawLine();
        }
        return;
      }
    }
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
      if (pendingLines.length > 0) {
        // Empty line on continuation — submit what we have
        const full = pendingLines.join('\n');
        pendingLines = [];
        if (history[0] !== full) {
          history.unshift(full);
          if (history.length > 500) history.pop();
          localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
        }
        histIdx = -1;
        await runQuery(full);
        return;
      }
      term.write(PROMPT);
      return;
    }
    // Check for \ continuation
    if (text.endsWith('\\')) {
      pendingLines.push(text.slice(0, -1));
      term.write(CONT_PROMPT);
      return;
    }
    // Merge with any pending lines
    const full = pendingLines.length > 0
      ? [...pendingLines, text].join('\n')
      : text;
    pendingLines = [];

    // Persist history
    if (history[0] !== full) {
      history.unshift(full);
      if (history.length > 500) history.pop();
      localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
    }
    histIdx = -1;

    const cmd = text.trim();
    if (cmd === '/help' || cmd === '/help oql' || cmd === '/?') {
      if (cmd === '/help oql') {
        await printOqlRef();
      } else {
        printHelp();
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/clear') {
      term.clear();
      term.write(PROMPT);
      return;
    }
    if (cmd === '/q' || cmd === '/quit' || cmd === '/disconnect') {
      document.getElementById('btn-disconnect').click();
      return;
    }
    if (cmd === '/analyze') {
      document.getElementById('btn-analyze').click();
      term.writeln('\x1b[33manalysis triggered — watch the toolbar for status\x1b[0m');
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
    if (cmd.startsWith('/describe ') || cmd === '/describe') {
      const cls = cmd.slice(9).trim();
      if (!cls) {
        term.writeln('\x1b[2musage: /describe <ClassName>  — show fields and instance count\x1b[0m');
        term.write(PROMPT);
        return;
      }
      term.write('\x1b[2m⠋ describing…\x1b[0m');
      try {
        const [fieldsRes, countRes] = await Promise.allSettled([
          fetch(serverUrl + '/', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ query: `SELECT * FROM ${cls} LIMIT 1` }),
            signal: AbortSignal.timeout(10000),
          }).then(r => r.json()),
          fetch(serverUrl + '/', {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ query: `SELECT COUNT(*) FROM INSTANCEOF ${cls}` }),
            signal: AbortSignal.timeout(10000),
          }).then(r => r.json()),
        ]);
        term.write('\r\x1b[K');
        const data = fieldsRes.status === 'fulfilled' ? fieldsRes.value : null;
        if (!data?.ok || !data.result?.columns) {
          const msg = data?.error?.message || 'class not found';
          term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
          if (classNames.length > 0) {
            const lower = cls.toLowerCase();
            const sugg = classNames.filter(c => c.toLowerCase().includes(lower)).slice(0, 5);
            if (sugg.length) term.writeln(`\x1b[2msimilar: ${sugg.map(c => c.split('.').pop()).join(', ')}\x1b[0m`);
          }
        } else {
          const colNames = data.result.columns.map(c => c.name || String(c));
          const rows = data.result.rows || [];
          // Extract count
          let instanceCount = null;
          if (countRes.status === 'fulfilled' && countRes.value?.ok) {
            const cell = countRes.value.result?.rows?.[0]?.[0];
            instanceCount = cell == null ? null : (typeof cell === 'object' ? cell.v : cell);
          }
          const countStr = instanceCount != null
            ? `  \x1b[2m(${instanceCount.toLocaleString()} instance${instanceCount === 1 ? '' : 's'})\x1b[0m`
            : '';
          term.writeln(`\x1b[1mFields of ${cls}\x1b[0m${countStr}`);
          const idxW = String(colNames.length).length;
          const nameW = Math.max(...colNames.map(c => c.length), 8);
          colNames.forEach((n, i) => {
            let typeTag = 'null';
            if (rows.length > 0) {
              const cell = rows[0][i];
              if (cell !== null && cell !== undefined) {
                if (typeof cell !== 'object') typeTag = typeof cell;
                else if (cell.kind && cell.kind !== 'null') typeTag = cell.kind;
              }
            }
            term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  \x1b[36m${n.padEnd(nameW)}\x1b[0m  \x1b[2m${typeTag}\x1b[0m`);
          });
          term.writeln(`\x1b[2m(${colNames.length} field${colNames.length !== 1 ? 's' : ''})\x1b[0m`);
        }
      } catch (e) {
        term.write('\r\x1b[K');
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/obj ') || cmd === '/obj') {
      // /obj <ClassName>#<idx>  or  /obj <ClassName> <idx>
      const arg = cmd.slice(4).trim();
      if (!arg) {
        term.writeln('\x1b[2musage: /obj <ClassName>#<idx>  — inspect a specific object by class + dense index\x1b[0m');
        term.write(PROMPT);
        return;
      }
      // Parse "<Class>#<n>" or "<Class> <n>" formats
      const m = arg.match(/^(.+?)#(\d+)$/) || arg.match(/^(.+?)\s+(\d+)$/);
      if (!m) {
        term.writeln('\x1b[2musage: /obj <ClassName>#<idx>  e.g. /obj java.lang.String#42\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const [, cls, idx] = m;
      const clsTrimmed = cls.trim();
      // Run the query; if exactly 1 row, show as key=value (nicer than a 1-row table)
      try {
        const res = await fetch(serverUrl + '/', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: `SELECT * FROM ${clsTrimmed} s WHERE s.@objectId = ${idx}` }),
          signal: AbortSignal.timeout(10000),
        }).then(r => r.json());
        const r = res.result || res;
        if (r.error) {
          term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
        } else if (!r.columns || r.columns.length === 0) {
          term.writeln(`\x1b[33m(no object ${clsTrimmed}#${idx} found)\x1b[0m`);
        } else {
          const colNames = r.columns.map(c => c.name || String(c));
          const rows = r.rows || [];
          if (rows.length === 1) {
            const keyW = Math.max(...colNames.map(n => n.length)) + 2;
            const idxW = String(colNames.length).length;
            term.writeln(`\x1b[1m── ${clsTrimmed}#${idx} ──\x1b[0m`);
            colNames.forEach((col, i) => {
              const cell = rows[0][i];
              const val = fmtCell(cell, col);
              const cc = cellColor(cell, col);
              const valStr = cc ? `${cc}${val}\x1b[0m` : val;
              term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  \x1b[36m${col.padEnd(keyW)}\x1b[0m  ${valStr}`);
            });
          } else if (rows.length === 0) {
            term.writeln(`\x1b[33m(no object ${clsTrimmed}#${idx} found)\x1b[0m`);
          } else {
            renderResult(r);
          }
          lastResult = { columns: colNames, rows, note: r.note, truncated: r.truncated, row_count: r.row_count };
          currentRowIdx = 0;
        }
      } catch (e) {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/plan ') || cmd === '/plan' ||
        cmd.startsWith('/explain ') || cmd === '/explain') {
      const isPlan = cmd.startsWith('/plan') || cmd === '/plan';
      const oql = cmd.slice(isPlan ? 5 : 8).trim();
      if (!oql) {
        term.writeln(`\x1b[2musage: /plan <oql>  — show query execution plan (no scan)\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      try {
        const res = await fetch(serverUrl + '/plan', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: oql }),
          signal: AbortSignal.timeout(5000),
        });
        const data = await res.json();
        if (data.ok && data.plan) {
          data.plan.split('\n').forEach(l => term.writeln(l));
        } else if (data.error) {
          const msg = data.error?.message || JSON.stringify(data.error);
          const report = data.error?.report;
          if (report) {
            report.split('\n').forEach(l => term.writeln(l));
          } else {
            term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
          }
        } else {
          term.writeln('\x1b[2m(server did not return a plan)\x1b[0m');
        }
      } catch (e) {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/watch' || cmd.startsWith('/watch ')) {
      const args = cmd.slice(6).trim();
      // /watch stop
      if (args === 'stop' || args === '') {
        if (watchTimer) {
          clearInterval(watchTimer);
          watchTimer = null;
          term.writeln('\x1b[32m✓ watch stopped\x1b[0m');
        } else if (args === '') {
          term.writeln('\x1b[2musage: /watch <seconds> <oql>  — refresh query every N seconds; /watch stop\x1b[0m');
        } else {
          term.writeln('\x1b[2m(no active watch)\x1b[0m');
        }
        term.write(PROMPT);
        return;
      }
      const m = args.match(/^(\d+(?:\.\d+)?)\s+(.+)$/s);
      if (!m) {
        term.writeln('\x1b[2musage: /watch <seconds> <oql>\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const secs = parseFloat(m[1]);
      const watchOql = m[2].trim();
      if (secs < 1) {
        term.writeln('\x1b[31mminimum interval is 1 second\x1b[0m');
        term.write(PROMPT);
        return;
      }
      if (watchTimer) { clearInterval(watchTimer); watchTimer = null; }
      term.writeln(`\x1b[2mwatching every ${secs}s — Ctrl+C or /watch stop to cancel\x1b[0m`);
      const tick = async () => {
        const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
        term.writeln(`\x1b[2m── ${ts} ──────────────────────────────────────────\x1b[0m`);
        await runQuery(watchOql, { showHint: false });
        term.write(PROMPT);
      };
      await tick();
      watchTimer = setInterval(tick, secs * 1000);
      return;
    }
    if (cmd.startsWith('/count ') || cmd === '/count') {
      const arg = cmd.slice(6).trim();
      if (!arg) {
        if (lastResult) {
          const n = lastResult.rows.length;
          const m = lastResult.columns.length;
          term.writeln(`\x1b[32m${n.toLocaleString()}\x1b[0m row${n !== 1 ? 's' : ''} × \x1b[32m${m}\x1b[0m col${m !== 1 ? 's' : ''}`);
          term.write(PROMPT); return;
        }
        term.writeln('\x1b[2musage: /count <ClassName|oql>  — count instances or rows\x1b[0m');
        term.write(PROMPT);
        return;
      }
      // If it looks like a full OQL query (has SELECT or FROM), wrap it in COUNT;
      // otherwise treat as a class name and use INSTANCEOF
      const lc = arg.toLowerCase().trim();
      const isOql = /^select\s|^from\s/.test(lc);
      const countQuery = isOql
        ? (lc.includes('count(*)') ? arg : `SELECT COUNT(*) FROM ( ${arg} )`)
        : `SELECT COUNT(*) FROM INSTANCEOF ${arg}`;
      const label = isOql ? 'rows matching query' : `instances of \x1b[36m${arg}\x1b[0m`;
      term.write('\x1b[2m⠋ counting…\x1b[0m');
      try {
        const res = await fetch(serverUrl + '/', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: countQuery }),
        });
        const data = await res.json();
        term.write('\r\x1b[K');
        if (data.ok) {
          const cell = data.result?.rows?.[0]?.[0];
          const n = cell == null ? null : (typeof cell === 'object' ? cell.v : cell);
          const nFmt = n != null ? n.toLocaleString() : '?';
          const dynLabel = isOql ? label
            : `instance${n === 1 ? '' : 's'} of \x1b[36m${arg}\x1b[0m`;
          term.writeln(`\x1b[32m${nFmt}\x1b[0m ${dynLabel}`);
        } else {
          const msg = data.error?.message || data.error || 'unknown error';
          term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
        }
      } catch (e) {
        term.write('\r\x1b[K');
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/last') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        renderResult(lastResult);
        term.writeln(`\x1b[2m${lastResult.rows.length} rows (re-displayed)\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/wc' || cmd.startsWith('/wc ')) {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const colArg = cmd.slice(3).trim();
        if (!colArg) {
          const n = lastResult.rows.length;
          const m = lastResult.columns.length;
          term.writeln(`\x1b[32m${n.toLocaleString()}\x1b[0m row${n !== 1 ? 's' : ''} × \x1b[32m${m}\x1b[0m col${m !== 1 ? 's' : ''}`);
        } else {
          const ci = resolveCol(colArg, lastResult.columns);
          if (ci < 0) {
            term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
          } else {
            const total = lastResult.rows.length;
            const nonNull = lastResult.rows.filter(row => row[ci] !== null && row[ci] !== undefined && !(typeof row[ci] === 'object' && row[ci]?.kind === 'null')).length;
            term.writeln(`\x1b[32m${nonNull.toLocaleString()}\x1b[0m non-null / \x1b[32m${total.toLocaleString()}\x1b[0m total in "${lastResult.columns[ci]}"`);
          }
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/undo') {
      if (!prevResult) {
        term.writeln('\x1b[33m(nothing to undo)\x1b[0m');
      } else {
        lastResult = prevResult;
        prevResult = null;
        term.writeln(`\x1b[32m✓ undone\x1b[0m  \x1b[2m(restored ${lastResult.rows.length} row${lastResult.rows.length !== 1 ? 's' : ''})\x1b[0m`);
        renderResult({ columns: lastResult.columns, rows: lastResult.rows, row_count: lastResult.rows.length });
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/row ') || cmd === '/row') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      if (lastResult.rows.length === 0) {
        term.writeln('\x1b[33m(result has no rows)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const arg = cmd.slice(4).trim();
      let n;
      if (!arg || arg === 'first') {
        n = 1; currentRowIdx = 0;
      } else if (arg === 'next' || arg === '+') {
        currentRowIdx = Math.min(currentRowIdx + 1, lastResult.rows.length - 1);
        n = currentRowIdx + 1;
      } else if (arg === 'prev' || arg === '-') {
        currentRowIdx = Math.max(currentRowIdx - 1, 0);
        n = currentRowIdx + 1;
      } else if (arg === 'last') {
        currentRowIdx = lastResult.rows.length - 1;
        n = currentRowIdx + 1;
      } else {
        n = parseInt(arg, 10);
        if (!isNaN(n) && (n < 1 || n > lastResult.rows.length)) {
          term.writeln(`\x1b[31mrow ${n} out of range\x1b[0m  \x1b[2mresult has ${lastResult.rows.length} rows\x1b[0m`);
          term.write(PROMPT);
          return;
        } else if (isNaN(n)) {
          term.writeln(`\x1b[2musage: /row [N|first|last|next|prev]  — show row as key=value pairs\x1b[0m`);
          term.write(PROMPT);
          return;
        }
        currentRowIdx = n - 1;
      }
      const row = lastResult.rows[n - 1];
      const keyW = Math.max(...lastResult.columns.map(c => c.length)) + 2;
      const idxW = String(lastResult.columns.length).length;
      const total = lastResult.rows.length;
      const navHint = total > 1 ? `\x1b[2m  (use /row next / /row prev to navigate)\x1b[0m` : '';
      term.writeln(`\x1b[2m── row ${n} of ${total} ──\x1b[0m${navHint}`);
      lastResult.columns.forEach((col, i) => {
        const key = col.padEnd(keyW);
        const cell = row[i];
        const val = fmtCell(cell, col);
        const cc = cellColor(cell, col);
        const valStr = cc ? `${cc}${val}\x1b[0m` : val;
        term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  \x1b[36m${key}\x1b[0m  ${valStr}`);
      });
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/limit ') || cmd === '/limit') {
      const arg = cmd.slice(6).trim();
      if (!arg || arg === '0' || arg === 'unlimited') {
        settings.rowLimit = Infinity;
        localStorage.setItem(SETTINGS_KEY, JSON.stringify({ ...settings, rowLimit: 0 }));
        term.writeln('\x1b[32m✓ row limit: unlimited\x1b[0m');
      } else {
        const n = parseInt(arg, 10);
        if (isNaN(n) || n <= 0) {
          term.writeln('\x1b[2musage: /limit <N>  (0 or "unlimited" removes limit)\x1b[0m');
          term.write(PROMPT);
          return;
        }
        settings.rowLimit = n;
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ row limit: ${n}\x1b[0m`);
      }
      if (lastResult) {
        renderResult(lastResult);
        term.writeln(`\x1b[2m${lastResult.rows.length} rows\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/cols' || cmd === '/columns') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const fields = lastResult.columns;
        const total = lastResult.rows.length;
        const idxW = String(fields.length).length;
        const colW = Math.max(...fields.map(f => f.length));
        fields.forEach((f, i) => {
          let typeTag = 'null';
          let nonNull = 0;
          for (const row of lastResult.rows) {
            const cell = row[i];
            if (cell === null || cell === undefined) continue;
            if (typeof cell !== 'object') { nonNull++; if (typeTag === 'null') typeTag = typeof cell; continue; }
            if (cell.kind === 'null') continue;
            nonNull++;
            if (typeTag === 'null') typeTag = cell.kind || typeof cell;
          }
          const fill = total > 0 ? `  ${nonNull}/${total} (${Math.round(nonNull / total * 100)}%)` : '';
          const allNull = total > 0 && nonNull === 0;
          const nameColor = allNull ? '\x1b[2;33m' : '\x1b[36m';
          const dimSuffix = allNull ? ' \x1b[33m(all null)\x1b[0m' : '';
          term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  ${nameColor}${f.padEnd(colW)}\x1b[0m  \x1b[2m${typeTag.padEnd(8)}${fill}\x1b[0m${dimSuffix}`);
        });
        term.writeln(`\x1b[2m(${fields.length} column${fields.length !== 1 ? 's' : ''})\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/select ') || cmd === '/select') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const args = cmd.slice(7).trim().split(/\s+/).filter(Boolean);
        if (args.length === 0) {
          const fields = lastResult.columns;
          if (fields && fields.length > 0) {
            term.writeln(`\x1b[2musage: /select <col1> [col2] …  — available: ${fields.join(', ')}\x1b[0m`);
          } else {
            term.writeln('\x1b[2musage: /select <col1> [col2] …  (names, 1-based numbers, or ranges like 1-3)\x1b[0m');
          }
        } else {
          const fields = lastResult.columns;
          const indices = [];
          let ok = true;
          for (const arg of args) {
            // Accept N-M ranges
            const rangeM = arg.match(/^(\d+)-(\d+)$/);
            if (rangeM) {
              const lo = Math.max(1, parseInt(rangeM[1], 10));
              const hi = Math.min(fields.length, parseInt(rangeM[2], 10));
              if (lo <= hi) { for (let i = lo; i <= hi; i++) indices.push(i - 1); continue; }
            }
            const ci = resolveCol(arg, fields);
            if (ci < 0) {
              term.writeln(`\x1b[31mcolumn ${JSON.stringify(arg)} not found\x1b[0m  \x1b[2mavailable: ${fields.join(', ')}\x1b[0m`);
              ok = false;
              break;
            }
            indices.push(ci);
          }
          if (ok) {
            const newCols = indices.map(i => fields[i]);
            const newRows = lastResult.rows.map(r => indices.map(i => r[i]));
            prevResult = lastResult;
            lastResult = { columns: newCols, rows: newRows };
            renderResult({ columns: lastResult.columns, rows: lastResult.rows, row_count: lastResult.rows.length });
          }
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/drop ') || cmd === '/drop') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const args = cmd.slice(5).trim().split(/\s+/).filter(Boolean);
        if (args.length === 0) {
          term.writeln(`\x1b[2musage: /drop <col1> [col2] …  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        } else {
          const fields = lastResult.columns;
          const dropSet = new Set();
          let ok = true;
          for (const arg of args) {
            const rangeM = arg.match(/^(\d+)-(\d+)$/);
            if (rangeM) {
              const lo = Math.max(1, parseInt(rangeM[1], 10));
              const hi = Math.min(fields.length, parseInt(rangeM[2], 10));
              if (lo <= hi) { for (let i = lo; i <= hi; i++) dropSet.add(i - 1); continue; }
            }
            const ci = resolveCol(arg, fields);
            if (ci < 0) {
              term.writeln(`\x1b[31mcolumn ${JSON.stringify(arg)} not found\x1b[0m  \x1b[2mavailable: ${fields.join(', ')}\x1b[0m`);
              ok = false; break;
            }
            dropSet.add(ci);
          }
          if (ok) {
            if (dropSet.size >= fields.length) {
              term.writeln('\x1b[31mcannot drop all columns\x1b[0m');
            } else {
              const keep = fields.map((_, i) => i).filter(i => !dropSet.has(i));
              const newCols = keep.map(i => fields[i]);
              const newRows = lastResult.rows.map(r => keep.map(i => r[i]));
              prevResult = lastResult;
              lastResult = { columns: newCols, rows: newRows };
              renderResult({ columns: lastResult.columns, rows: lastResult.rows, row_count: lastResult.rows.length });
            }
          }
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/rename ') || cmd === '/rename') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const parts = cmd.slice(7).trim().split(/\s+/);
        if (parts.length < 2 || !parts[0] || !parts[1]) {
          term.writeln(`\x1b[2musage: /rename <col> <newname>  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        } else {
          const [oldArg, newName] = parts;
          const i = resolveCol(oldArg, lastResult.columns);
          if (i < 0) {
            term.writeln(`\x1b[31mcolumn ${JSON.stringify(oldArg)} not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
          } else {
            const oldName = lastResult.columns[i];
            prevResult = { columns: [...lastResult.columns], rows: lastResult.rows };
            lastResult.columns[i] = newName;
            term.writeln(`\x1b[32m✓\x1b[0m \x1b[2m${JSON.stringify(oldName)}\x1b[0m → \x1b[32m${JSON.stringify(newName)}\x1b[0m`);
          }
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/bookmark ') || cmd === '/bookmark' ||
        cmd.startsWith('/save ') || cmd === '/save') {
      const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
      const rest = cmd.startsWith('/save') ? cmd.slice(5).trim() : cmd.slice(9).trim();
      if (!rest) {
        // Show all bookmarks
        const entries = Object.entries(bookmarks);
        if (entries.length === 0) {
          term.writeln('\x1b[2m(no bookmarks yet — use /bookmark <name> to save last query)\x1b[0m');
        } else {
          term.writeln('\x1b[1mBookmarks\x1b[0m');
          entries.forEach(([name, oql]) => {
            const truncated = oql.length > term.cols - name.length - 6
              ? oql.slice(0, term.cols - name.length - 7) + '…' : oql;
            term.writeln(`  \x1b[36m${name.padEnd(20)}\x1b[0m  \x1b[2m${truncated}\x1b[0m`);
          });
          term.writeln('\x1b[2m  Use /bookmark <name> to save, /forget <name> to delete, !<name> to run\x1b[0m');
        }
      } else {
        // history[0] is the /bookmark command itself; history[1] is the last real query
        const toSave = history.find(h => !h.startsWith('/bookmark') && !h.startsWith('/save'));
        if (!toSave) {
          term.writeln('\x1b[33m(no query to bookmark — run a query first)\x1b[0m');
        } else {
          bookmarks[rest] = toSave;
          localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(bookmarks));
          term.writeln(`\x1b[32m✓ saved as "${rest}": \x1b[2m${toSave.length > 60 ? toSave.slice(0, 59) + '…' : toSave}\x1b[0m`);
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/forget ')) {
      const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
      const name = cmd.slice(8).trim();
      if (bookmarks[name]) {
        delete bookmarks[name];
        localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(bookmarks));
        term.writeln(`\x1b[32m✓ removed bookmark "${name}"\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mno bookmark named "${name}"\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/unique ') || cmd === '/unique') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const rawArg = cmd.slice(7).trim();
      if (!rawArg) {
        term.writeln(`\x1b[2musage: /unique <col> [N]  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      // Parse optional top-N: "classname 10" or "classname top 10"
      let colArg = rawArg, topN = null;
      const topMatch = rawArg.match(/^(\S+)\s+(?:top\s+)?(\d+)$/i);
      if (topMatch) { colArg = topMatch[1]; topN = parseInt(topMatch[2], 10); }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const colName = lastResult.columns[ci];
      const seen = new Map();
      lastResult.rows.forEach(row => {
        const key = fmtCell(row[ci], colName);
        seen.set(key, (seen.get(key) || 0) + 1);
      });
      const totalDistinct = seen.size;
      let entries = [...seen.entries()].sort((a, b) => b[1] - a[1]);
      const showN = topN !== null ? topN : entries.length;
      const shown = Math.min(entries.length, showN);
      entries = entries.slice(0, shown);
      const total = lastResult.rows.length;
      const maxCnt = entries.length > 0 ? entries[0][1] : 0;
      const cntW = Math.max(5, String(maxCnt).length);
      const pctW = 6;  // "100.0%"
      const colW = Math.max(colName.length, ...entries.map(([v]) => v.length), 4);
      const barCap = Math.max(0, term.cols - colW - cntW - pctW - 8);
      const showBar = barCap >= 8;
      const hdr = `${colName.padEnd(colW)}  ${'count'.padStart(cntW)}  ${'%'.padStart(pctW)}${showBar ? '  bar' : ''}`;
      term.writeln(`\x1b[1m${hdr}\x1b[0m`);
      term.writeln('\x1b[2m' + '─'.repeat(Math.min(hdr.length, term.cols - 2)) + '\x1b[0m');
      entries.forEach(([val, cnt]) => {
        const pct = total > 0 ? (cnt / total * 100).toFixed(1) + '%' : '—';
        let bar = '';
        if (showBar && maxCnt > 0) {
          const filled = Math.round((cnt / maxCnt) * barCap);
          bar = '  \x1b[2m' + '█'.repeat(filled) + '░'.repeat(barCap - filled) + '\x1b[0m';
        }
        term.writeln(`${val.padEnd(colW)}  \x1b[32m${String(cnt).padStart(cntW)}\x1b[0m  \x1b[2m${pct.padStart(pctW)}\x1b[0m${bar}`);
      });
      if (shown < totalDistinct) {
        term.writeln(`\x1b[2m(${shown} of ${totalDistinct} distinct values, top ${showN} shown  ·  ${total} total rows)\x1b[0m`);
      } else {
        term.writeln(`\x1b[2m(${totalDistinct} distinct value${totalDistinct !== 1 ? 's' : ''} in ${lastResult.rows.length} rows)\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/pivot ') || cmd === '/pivot') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const rawArg = cmd.slice(6).trim();
      if (!rawArg) {
        term.writeln(`\x1b[2musage: /pivot <col> [N]  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      let colArg = rawArg, topN = null;
      const topMatch = rawArg.match(/^(\S+)\s+(?:top\s+)?(\d+)$/i);
      if (topMatch) { colArg = topMatch[1]; topN = parseInt(topMatch[2], 10); }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const colName = lastResult.columns[ci];
      const counts = new Map();
      lastResult.rows.forEach(row => {
        const key = fmtCell(row[ci], colName);
        counts.set(key, (counts.get(key) || 0) + 1);
      });
      let entries = [...counts.entries()].sort((a, b) => b[1] - a[1]);
      const totalGroups = entries.length;
      if (topN !== null) entries = entries.slice(0, topN);
      const pivotRows = entries.map(([v, c]) => [v, c]);
      const note = (topN !== null && pivotRows.length < totalGroups)
        ? `top ${pivotRows.length} of ${totalGroups} groups` : null;
      const pivotResult = { columns: [colName, 'count'], rows: pivotRows, row_count: pivotRows.length, note };
      renderResult(pivotResult);
      prevResult = lastResult;
      lastResult = pivotResult;
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/stats ') || cmd === '/stats') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const colArgRaw = cmd.slice(6).trim();
      let colArg = colArgRaw;
      if (!colArg) {
        // Auto-select if exactly one numeric column
        const numericCols = lastResult.columns.map((_, i) => {
          const sample = lastResult.rows.find(row => row[i] !== null && row[i] !== undefined);
          return (sample && isNumericKind(sample[i])) ? i : -1;
        }).filter(i => i >= 0);
        if (numericCols.length === 1) {
          colArg = lastResult.columns[numericCols[0]];
        } else if (numericCols.length > 1) {
          for (const colIdx of numericCols) {
            const cName = lastResult.columns[colIdx];
            const allVals2 = lastResult.rows.map(row => {
              const cell = row[colIdx];
              if (cell === null || cell === undefined) return null;
              const v = typeof cell === 'object' ? cell.v : cell;
              return typeof v === 'number' ? v : null;
            });
            const vals2 = allVals2.filter(v => v !== null).sort((a, b) => a - b);
            if (vals2.length === 0) continue;
            const nullCount2 = allVals2.length - vals2.length;
            const sum2 = vals2.reduce((s, v) => s + v, 0);
            const mean2 = sum2 / vals2.length;
            const variance2 = vals2.reduce((s, v) => s + (v - mean2) ** 2, 0) / vals2.length;
            const stddev2 = Math.sqrt(variance2);
            const p50_2 = vals2[Math.floor(vals2.length * 0.5)];
            const p90_2 = vals2[Math.floor(vals2.length * 0.9)];
            const p99_2 = vals2[Math.floor(vals2.length * 0.99)];
            const fmtV2 = v => {
              if (!settings.bytesRaw && cName && /bytes$|_size$|heap_size$/i.test(cName)) return fmtBytes(v);
              return v.toLocaleString('en-US');
            };
            const nullNote2 = nullCount2 > 0 ? `  \x1b[2m(${nullCount2} null)\x1b[0m` : '';
            term.writeln(`\x1b[1m${cName}\x1b[0m  \x1b[2m(${vals2.length} non-null values)\x1b[0m${nullNote2}`);
            term.writeln(`  min    \x1b[32m${fmtV2(vals2[0])}\x1b[0m`);
            term.writeln(`  max    \x1b[32m${fmtV2(vals2[vals2.length - 1])}\x1b[0m`);
            term.writeln(`  mean   \x1b[32m${fmtV2(mean2)}\x1b[0m`);
            term.writeln(`  stddev \x1b[32m${fmtV2(stddev2)}\x1b[0m`);
            term.writeln(`  p50    \x1b[32m${fmtV2(p50_2)}\x1b[0m`);
            term.writeln(`  p90    \x1b[32m${fmtV2(p90_2)}\x1b[0m`);
            term.writeln(`  p99    \x1b[32m${fmtV2(p99_2)}\x1b[0m`);
            term.writeln(`  sum    \x1b[33m${fmtV2(sum2)}\x1b[0m`);
            if (vals2.length >= 2) {
              const lo2 = vals2[0], hi2 = vals2[vals2.length - 1];
              if (hi2 > lo2) {
                const NBUCKETS2 = 10, BAR_MAX2 = Math.max(16, Math.floor(term.cols / 3));
                const buckets2 = new Array(NBUCKETS2).fill(0);
                const range2 = hi2 - lo2;
                vals2.forEach(v => { const b = Math.min(NBUCKETS2-1, Math.floor((v-lo2)/range2*NBUCKETS2)); buckets2[b]++; });
                const maxB2 = Math.max(...buckets2, 1);
                term.writeln('  \x1b[2mdist:\x1b[0m');
                buckets2.forEach((b, i) => {
                  const barLen2 = Math.round(b / maxB2 * BAR_MAX2);
                  term.writeln(`  \x1b[2m${fmtV2(lo2 + i * range2 / NBUCKETS2).padStart(10)}\x1b[0m  \x1b[32m${'█'.repeat(barLen2).padEnd(BAR_MAX2)}\x1b[0m  \x1b[2m${b}\x1b[0m`);
                });
              }
            }
          }
          term.write(PROMPT);
          return;
        } else {
          term.writeln(`\x1b[2musage: /stats <col>  — no numeric columns found  available: ${lastResult.columns.join(', ')}\x1b[0m`);
          term.write(PROMPT);
          return;
        }
      }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const colName = lastResult.columns[ci];
      const allVals = lastResult.rows.map(row => {
        const cell = row[ci];
        if (cell === null || cell === undefined) return null;
        const v = typeof cell === 'object' ? cell.v : cell;
        return typeof v === 'number' ? v : null;
      });
      const vals = allVals.filter(v => v !== null).sort((a, b) => a - b);
      const nullCount = allVals.length - vals.length;
      if (vals.length === 0) {
        term.writeln(`\x1b[33m(no numeric values in column "${colName}")\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const sum = vals.reduce((s, v) => s + v, 0);
      const mean = sum / vals.length;
      const variance = vals.reduce((s, v) => s + (v - mean) ** 2, 0) / vals.length;
      const stddev = Math.sqrt(variance);
      const p50 = vals[Math.floor(vals.length * 0.5)];
      const p90 = vals[Math.floor(vals.length * 0.9)];
      const p99 = vals[Math.floor(vals.length * 0.99)];
      const fmtV = v => {
        if (!settings.bytesRaw && colName && /bytes$|_size$|heap_size$/i.test(colName)) return fmtBytes(v);
        return v.toLocaleString('en-US');
      };
      const nullInfo = nullCount > 0 ? `  \x1b[2m(${nullCount} null)\x1b[0m` : '';
      term.writeln(`\x1b[1m${colName}\x1b[0m  \x1b[2m(${vals.length} non-null values)\x1b[0m${nullInfo}`);
      term.writeln(`  min    \x1b[32m${fmtV(vals[0])}\x1b[0m`);
      term.writeln(`  max    \x1b[32m${fmtV(vals[vals.length - 1])}\x1b[0m`);
      term.writeln(`  mean   \x1b[32m${fmtV(mean)}\x1b[0m`);
      term.writeln(`  stddev \x1b[32m${fmtV(stddev)}\x1b[0m`);
      term.writeln(`  p50    \x1b[32m${fmtV(p50)}\x1b[0m`);
      term.writeln(`  p90    \x1b[32m${fmtV(p90)}\x1b[0m`);
      term.writeln(`  p99    \x1b[32m${fmtV(p99)}\x1b[0m`);
      term.writeln(`  sum    \x1b[33m${fmtV(sum)}\x1b[0m`);
      // Mini distribution histogram (10 buckets)
      if (vals.length >= 2) {
        const lo = vals[0], hi = vals[vals.length - 1];
        if (hi > lo) {
          const NBUCKETS = 10, BAR_MAX = Math.max(16, Math.floor(term.cols / 3));
          const buckets = new Array(NBUCKETS).fill(0);
          const range = hi - lo;
          vals.forEach(v => {
            const b = Math.min(NBUCKETS - 1, Math.floor((v - lo) / range * NBUCKETS));
            buckets[b]++;
          });
          const maxB = Math.max(...buckets, 1);
          term.writeln('  \x1b[2mdist:\x1b[0m');
          buckets.forEach((b, i) => {
            const barLen = Math.round(b / maxB * BAR_MAX);
            const bar = '█'.repeat(barLen);
            const label = fmtV(lo + i * range / NBUCKETS).padStart(10);
            term.writeln(`  \x1b[2m${label}\x1b[0m  \x1b[32m${bar.padEnd(BAR_MAX)}\x1b[0m  \x1b[2m${b}\x1b[0m`);
          });
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/top ') || cmd === '/top' ||
        cmd.startsWith('/head ') || cmd === '/head') {
      const isHead = cmd.startsWith('/head');
      const arg = cmd.slice(isHead ? 5 : 4).trim();
      const n = arg ? parseInt(arg, 10) : 10;
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else if (!n || n < 1) {
        term.writeln('\x1b[2musage: /top [N]  (or /head [N]) — show first N rows of last result (default 10)\x1b[0m');
      } else {
        const total = lastResult.rows.length;
        const sliced = lastResult.rows.slice(0, n);
        const shown = sliced.length;
        const slicedResult = { columns: lastResult.columns, rows: sliced };
        if (shown < total) slicedResult.note = `top ${shown} of ${total}`;
        renderResult({ ...slicedResult, row_count: shown });
        prevResult = lastResult;
        lastResult = slicedResult;
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/tail ') || cmd === '/tail') {
      const arg = cmd.slice(5).trim();
      const n = arg ? parseInt(arg, 10) : 10;
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else if (!n || n < 1) {
        term.writeln('\x1b[2musage: /tail [N]  — show last N rows of last result (default 10)\x1b[0m');
      } else {
        const total = lastResult.rows.length;
        const sliced = lastResult.rows.slice(-n);
        const shown = sliced.length;
        const slicedResult = { columns: lastResult.columns, rows: sliced };
        if (shown < total) slicedResult.note = `last ${shown} of ${total}`;
        renderResult({ ...slicedResult, row_count: shown });
        prevResult = lastResult;
        lastResult = slicedResult;
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/sort ') || cmd === '/sort') {
      const args = cmd.slice(5).trim();
      if (!lastResult || !args) {
        if (!lastResult) term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        else term.writeln(`\x1b[2musage: /sort <col> [desc] [,-col2…]  (-col for desc)  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      // Parse comma-separated sort keys: "col1 desc, col2 asc, col3, -col4"
      const specs = [];
      let ok = true;
      for (const spec of args.split(',')) {
        const trimmed = spec.trim();
        if (!trimmed) continue;
        let colArg, desc;
        if (trimmed.startsWith('-') && trimmed.length > 1) {
          colArg = trimmed.slice(1);
          desc = true;
        } else {
          const parts = trimmed.split(/\s+/);
          colArg = parts[0];
          desc = parts[1]?.toLowerCase() === 'desc';
        }
        const ci = resolveCol(colArg, lastResult.columns);
        if (ci < 0) {
          term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
          ok = false; break;
        }
        specs.push({ ci, desc, name: lastResult.columns[ci] });
      }
      if (!ok || specs.length === 0) { term.write(PROMPT); return; }
      const sorted = [...lastResult.rows].sort((a, b) => {
        for (const { ci, desc } of specs) {
          const av = a[ci], bv = b[ci];
          const an = av?.v ?? av, bn = bv?.v ?? bv;
          if (an === null || an === undefined) return 1;
          if (bn === null || bn === undefined) return -1;
          const cmp = typeof an === 'number' && typeof bn === 'number'
            ? an - bn : String(an).localeCompare(String(bn));
          const ord = desc ? -cmp : cmp;
          if (ord !== 0) return ord;
        }
        return 0;
      });
      const label = specs.map(s => `${s.name} ${s.desc ? 'desc' : 'asc'}`).join(', ');
      const newResult = { columns: lastResult.columns, rows: sorted, row_count: sorted.length, note: `sorted by ${label}` };
      renderResult(newResult);
      prevResult = lastResult;
      lastResult = newResult;
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/filter ') || cmd === '/filter' ||
        cmd.startsWith('/grep ')   || cmd === '/grep') {
      const isGrep = cmd.startsWith('/grep');
      const rawPattern = cmd.slice(isGrep ? 5 : 7).trim();
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else if (!rawPattern) {
        term.writeln('\x1b[2musage: /filter <text>  or  /filter /regex/[flags]  or  /filter @<col> <text>\x1b[0m');
      } else {
        const { columns, rows } = lastResult;
        let colIdx = null, pattern = rawPattern;
        if (rawPattern.startsWith('@')) {
          const sp = rawPattern.slice(1).match(/^(\S+)\s+(.+)$/);
          if (!sp) { term.writeln('\x1b[2musage: /filter @<col> <pattern>\x1b[0m'); term.write(PROMPT); return; }
          colIdx = resolveCol(sp[1], columns);
          if (colIdx < 0) {
            term.writeln(`\x1b[31mcolumn "${sp[1]}" not found\x1b[0m  \x1b[2mavailable: ${columns.join(', ')}\x1b[0m`);
            term.write(PROMPT); return;
          }
          pattern = sp[2];
        }
        let re;
        const reMatch = pattern.match(/^\/(.+)\/([gimsvy]*)$/);
        if (reMatch) {
          try { re = new RegExp(reMatch[1], reMatch[2]); }
          catch (e) { term.writeln(`\x1b[31minvalid regex: ${e.message}\x1b[0m`); term.write(PROMPT); return; }
        }
        const test = re
          ? (s) => re.test(s)
          : (s) => s.toLowerCase().includes(pattern.toLowerCase());
        const filtered = rows.filter(row =>
          colIdx !== null
            ? test(fmtCell(row[colIdx], columns[colIdx]))
            : row.some((cell, i) => test(fmtCell(cell, columns[i])))
        );
        if (filtered.length === 0) {
          term.writeln(`\x1b[33m(no rows match "${pattern}")\x1b[0m`);
        } else {
          const note = `${filtered.length} of ${rows.length} rows match "${pattern}"`;
          const newResult = { columns, rows: filtered, row_count: filtered.length, note };
          renderResult(newResult);
          prevResult = lastResult;
          lastResult = newResult;
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/not ') || cmd === '/not' ||
        cmd.startsWith('/exclude ') || cmd === '/exclude') {
      const isExclude = cmd.startsWith('/exclude');
      const rawPattern = cmd.slice(isExclude ? 8 : 4).trim();
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else if (!rawPattern) {
        term.writeln('\x1b[2musage: /not <text>  or  /not /regex/[flags]  or  /not @<col> <text>  — exclude matching rows\x1b[0m');
      } else {
        const { columns, rows } = lastResult;
        let colIdx = null, pattern = rawPattern;
        if (rawPattern.startsWith('@')) {
          const sp = rawPattern.slice(1).match(/^(\S+)\s+(.+)$/);
          if (!sp) { term.writeln('\x1b[2musage: /not @<col> <pattern>\x1b[0m'); term.write(PROMPT); return; }
          colIdx = resolveCol(sp[1], columns);
          if (colIdx < 0) {
            term.writeln(`\x1b[31mcolumn "${sp[1]}" not found\x1b[0m  \x1b[2mavailable: ${columns.join(', ')}\x1b[0m`);
            term.write(PROMPT); return;
          }
          pattern = sp[2];
        }
        let re;
        const reMatch = pattern.match(/^\/(.+)\/([gimsvy]*)$/);
        if (reMatch) {
          try { re = new RegExp(reMatch[1], reMatch[2]); }
          catch (e) { term.writeln(`\x1b[31minvalid regex: ${e.message}\x1b[0m`); term.write(PROMPT); return; }
        }
        const test = re
          ? (s) => re.test(s)
          : (s) => s.toLowerCase().includes(pattern.toLowerCase());
        const kept = rows.filter(row =>
          colIdx !== null
            ? !test(fmtCell(row[colIdx], columns[colIdx]))
            : !row.some((cell, i) => test(fmtCell(cell, columns[i])))
        );
        const excluded = rows.length - kept.length;
        if (excluded === 0) {
          term.writeln(`\x1b[33m(no rows match "${pattern}" — nothing excluded)\x1b[0m`);
          term.write(PROMPT); return;
        }
        const note = `${excluded} of ${rows.length} rows excluded "${pattern}"`;
        const newResult = { columns, rows: kept, row_count: kept.length, note };
        renderResult(newResult);
        prevResult = lastResult;
        lastResult = newResult;
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/sample ') || cmd === '/sample') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const nStr = cmd.slice(7).trim();
        const n = nStr ? parseInt(nStr, 10) : 10;
        if (isNaN(n) || n <= 0) {
          term.writeln('\x1b[2musage: /sample [N]  — show N random rows (default 10)\x1b[0m');
        } else {
          const rows = lastResult.rows;
          const k = Math.min(n, rows.length);
          // Fisher-Yates partial shuffle to pick k items
          const pool = [...Array(rows.length).keys()];
          for (let i = 0; i < k; i++) {
            const j = i + Math.floor(Math.random() * (pool.length - i));
            [pool[i], pool[j]] = [pool[j], pool[i]];
          }
          const sampled = pool.slice(0, k).sort((a, b) => a - b).map(i => rows[i]);
          const sampledResult = { columns: lastResult.columns, rows: sampled, row_count: sampled.length, note: `random sample of ${k}/${rows.length}` };
          renderResult(sampledResult);
          prevResult = lastResult;
          lastResult = sampledResult;
        }
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/distinct' || cmd === '/dedup') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
      } else {
        const seen = new Set();
        const kept = lastResult.rows.filter(row => {
          const key = row.map((cell, i) => fmtCell(cell, lastResult.columns[i])).join('\x00');
          if (seen.has(key)) return false;
          seen.add(key);
          return true;
        });
        const removed = lastResult.rows.length - kept.length;
        const note = `${kept.length} unique row${kept.length !== 1 ? 's' : ''} (${removed} duplicate${removed !== 1 ? 's' : ''} removed)`;
        const newResult = { columns: lastResult.columns, rows: kept, row_count: kept.length, note };
        renderResult(newResult);
        prevResult = lastResult;
        lastResult = newResult;
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
          ? `\x1b[33m(no class names matching "${pattern}")\x1b[0m`
          : '\x1b[2m(no classes loaded — server may still be loading)\x1b[0m');
      } else {
        const CAP = 200;
        const shown = matches.slice(0, CAP);
        const maxLen = shown.reduce((m, c) => Math.max(m, c.length), 0);
        const colW = maxLen + 2;
        const cols = Math.max(1, Math.floor((term.cols - 4) / colW));
        for (let i = 0; i < shown.length; i += cols) {
          const row = shown.slice(i, i + cols);
          term.writeln('  ' + row.map(c => `\x1b[36m${c}\x1b[0m${' '.repeat(colW - c.length)}`).join('').trimEnd());
        }
        if (matches.length > CAP) {
          term.writeln(`\x1b[2m  ... ${matches.length - CAP} more (showing ${CAP}; use /classes <pattern> to narrow)\x1b[0m`);
        }
        term.writeln(`\x1b[2m(${matches.length} class${matches.length !== 1 ? 'es' : ''})\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/fields' || cmd.startsWith('/fields ')) {
      const pattern = cmd.slice(7).trim().toLowerCase();
      const all = fieldNames.length > 0 ? fieldNames
        : (await fetch(serverUrl + '/help').then(r => r.json()).then(d => {
            if (Array.isArray(d.fields)) fieldNames = d.fields;
            return fieldNames;
          }).catch(() => []));
      const matches = pattern ? all.filter(f => f.toLowerCase().includes(pattern)) : all;
      if (matches.length === 0) {
        term.writeln(pattern
          ? `\x1b[33m(no field names matching "${pattern}")\x1b[0m`
          : '\x1b[2m(no fields loaded — server may still be loading)\x1b[0m');
      } else {
        const CAP = 200;
        const shown = matches.slice(0, CAP);
        const maxLen = shown.reduce((m, f) => Math.max(m, f.length), 0);
        const colW = maxLen + 2;
        const cols = Math.max(1, Math.floor((term.cols - 4) / colW));
        for (let i = 0; i < shown.length; i += cols) {
          const row = shown.slice(i, i + cols);
          term.writeln('  ' + row.map(f => `\x1b[36m${f}\x1b[0m${' '.repeat(colW - f.length)}`).join('').trimEnd());
        }
        if (matches.length > CAP) {
          term.writeln(`\x1b[2m  ... ${matches.length - CAP} more (showing ${CAP}; use /fields <pattern> to narrow)\x1b[0m`);
        }
        term.writeln(`\x1b[2m(${matches.length} field${matches.length !== 1 ? 's' : ''})\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/set' || cmd.startsWith('/set ')) {
      const args = cmd.slice(4).trim().split(/\s+/);
      if (!args[0]) {
        // Print current settings
        term.writeln('\x1b[1mCurrent settings:\x1b[0m');
        term.writeln(`  \x1b[1mlimit\x1b[0m    \x1b[32m${settings.rowLimit === Infinity ? 'unlimited' : settings.rowLimit}\x1b[0m  \x1b[2m(rows displayed; 0 = no cap)\x1b[0m`);
        term.writeln(`  \x1b[1mbytes\x1b[0m    \x1b[32m${settings.bytesRaw ? 'raw' : 'human'}\x1b[0m  \x1b[2m(raw = show numbers, human = 4.3 KiB)\x1b[0m`);
        term.writeln(`  \x1b[1mcolor\x1b[0m    \x1b[32m${settings.color ? 'on' : 'off'}\x1b[0m  \x1b[2m(ANSI colours in table cells)\x1b[0m`);
        term.writeln(`  \x1b[1mnull\x1b[0m     \x1b[32m"${settings.nullStr}"\x1b[0m  \x1b[2m(null display string)\x1b[0m`);
        term.writeln('\x1b[2musage: /set limit 500 | /set bytes raw | /set bytes human | /set null ∅ | /set color off\x1b[0m');
      } else if (args[0] === 'limit') {
        const n = args[1] === '0' || args[1] === 'unlimited' || args[1] === 'none' ? 0 : parseInt(args[1], 10);
        if (isNaN(n) || n < 0 || n > 100000) {
          term.writeln('\x1b[2musage: /set limit <N>  (0 or "unlimited" = no cap)\x1b[0m');
        } else if (n === 0) {
          settings.rowLimit = Infinity;
          localStorage.setItem(SETTINGS_KEY, JSON.stringify({ ...settings, rowLimit: 0 }));
          term.writeln('\x1b[32m✓ row limit: unlimited\x1b[0m');
        } else {
          settings.rowLimit = n;
          localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
          term.writeln(`\x1b[32m✓ row limit: ${n}\x1b[0m`);
        }
      } else if (args[0] === 'bytes') {
        if (args[1] === 'raw') { settings.bytesRaw = true; }
        else if (args[1] === 'human') { settings.bytesRaw = false; }
        else { term.writeln('\x1b[2musage: /set bytes raw|human\x1b[0m'); term.write(PROMPT); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ bytes: ${settings.bytesRaw ? 'raw (numbers)' : 'human (e.g. 4.3 KiB)'}\x1b[0m`);
      } else if (args[0] === 'null') {
        settings.nullStr = args[1] || 'null';
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ null: "${settings.nullStr}"\x1b[0m`);
      } else if (args[0] === 'color') {
        if (args[1] === 'off' || args[1] === 'false') { settings.color = false; }
        else if (args[1] === 'on' || args[1] === 'true' || !args[1]) { settings.color = true; }
        else { term.writeln('\x1b[2musage: /set color on|off\x1b[0m'); term.write(PROMPT); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ color: ${settings.color ? 'on' : 'off'}\x1b[0m`);
      } else {
        term.writeln(`\x1b[31munknown setting: ${args[0]}\x1b[0m  \x1b[2moptions: limit, bytes, color, null\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/export' || cmd.startsWith('/export ')) {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const fmt = cmd.slice(7).trim().toLowerCase() || 'csv';
      let text, mime, ext;
      if (fmt === 'csv') {
        const csvRow = row => row.map(c => {
          const s = c == null ? '' : String(c);
          return /[",\n\r]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
        }).join(',');
        text = [lastResult.columns.map(c => /[",\n]/.test(c) ? '"' + c + '"' : c).join(',')]
          .concat(lastResult.rows.map(row => csvRow(row.map((cell, i) => fmtCell(cell, lastResult.columns[i])))))
          .join('\n');
        mime = 'text/csv'; ext = 'csv';
      } else if (fmt === 'json') {
        const rawCell = (cell, colName) => {
          if (cell === null || cell === undefined) return null;
          if (typeof cell !== 'object') return cell;
          const kind = cell.kind;
          if (kind === 'null') return null;
          if (kind === 'bool') return cell.v;
          if (kind === 'int' || kind === 'float') return cell.v;
          if (kind === 'obj_ref') return fmtCell(cell, colName);
          return fmtCell(cell, colName);
        };
        const objs = lastResult.rows.map(row => {
          const obj = {};
          lastResult.columns.forEach((col, i) => { obj[col] = rawCell(row[i], col); });
          return obj;
        });
        text = JSON.stringify(objs, null, 2);
        mime = 'application/json'; ext = 'json';
      } else {
        text = [lastResult.columns.join('\t')]
          .concat(lastResult.rows.map(row =>
            row.map((cell, i) => fmtCell(cell, lastResult.columns[i])).join('\t')
          ))
          .join('\n');
        mime = 'text/tab-separated-values'; ext = 'tsv';
      }
      try {
        await navigator.clipboard.writeText(text);
        term.writeln(`\x1b[32m✓ copied ${lastResult.rows.length} rows as ${ext.toUpperCase()} to clipboard\x1b[0m`);
      } catch (_) {
        const blob = new Blob([text], { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url; a.download = `query-result.${ext}`; a.click();
        URL.revokeObjectURL(url);
        term.writeln(`\x1b[32m✓ downloaded as query-result.${ext} (${lastResult.rows.length} rows)\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/history' || cmd.startsWith('/history ')) {
      const args = cmd.slice(8).trim();
      if (args === 'clear') {
        history.length = 0;
        localStorage.setItem(HISTORY_KEY, '[]');
        term.writeln('\x1b[32m✓ history cleared\x1b[0m');
        term.write(PROMPT);
        return;
      }
      // history[0] is the /history command itself; skip it for display
      const realHistory = history.slice(1);
      if (realHistory.length === 0) {
        term.writeln('\x1b[2m(no history yet)\x1b[0m');
      } else {
        const limit = args ? Math.min(parseInt(args, 10) || 20, realHistory.length) : Math.min(20, realHistory.length);
        const shown = realHistory.slice(0, limit);
        shown.forEach((h, i) => {
          const num = String(i + 1).padStart(3);
          const truncated = h.length > term.cols - 8 ? h.slice(0, term.cols - 9) + '…' : h;
          term.writeln(`\x1b[2m${num}\x1b[0m  \x1b[36m!${String(i + 1)}\x1b[0m  ${truncated}`);
        });
        if (realHistory.length > limit) {
          term.writeln(`\x1b[2m  … ${realHistory.length - limit} more — /history N to show more\x1b[0m`);
        }
        term.writeln(`\x1b[2m  Use !N to re-run entry N  ·  /history clear to wipe\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    // !N — re-run history entry; !name — run a bookmark
    if (/^!.+$/.test(cmd)) {
      if (/^!\d+$/.test(cmd)) {
        // history[0] is the "!N" command itself; skip it so !1 = most recent real entry
        const realHistory = history.slice(1);
        const n = parseInt(cmd.slice(1), 10) - 1;
        if (n < 0 || n >= realHistory.length) {
          term.writeln(`\x1b[31mno history entry ${cmd.slice(1)}\x1b[0m  \x1b[2m(have ${realHistory.length})\x1b[0m`);
          term.write(PROMPT);
        } else {
          const recalled = realHistory[n];
          const echo = recalled.length > term.cols - PROMPT.length - 1
            ? recalled.slice(0, term.cols - PROMPT.length - 2) + '…' : recalled;
          term.writeln(`\x1b[2m↳ ${echo}\x1b[0m`);
          await runQuery(recalled);
        }
      } else {
        const name = cmd.slice(1);
        const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
        if (bookmarks[name]) {
          const oql = bookmarks[name];
          const echo = oql.length > term.cols - PROMPT.length - 1
            ? oql.slice(0, term.cols - PROMPT.length - 2) + '…' : oql;
          term.writeln(`\x1b[2m↳ [${name}] ${echo}\x1b[0m`);
          await runQuery(oql);
        } else {
          term.writeln(`\x1b[31mno bookmark "!${name}" — use /bookmark to list\x1b[0m`);
          term.write(PROMPT);
        }
      }
      return;
    }
    if (cmd.startsWith('/run ') || cmd === '/run') {
      if (cmd === '/run') {
        if (namedQueries.length === 0) {
          term.writeln('\x1b[2m(no named queries loaded)\x1b[0m');
        } else {
          term.writeln('\x1b[1mNamed queries:\x1b[0m');
          let lastGroup = '';
          namedQueries.forEach(q => {
            if (q.group !== lastGroup) { lastGroup = q.group; term.writeln(`\r  \x1b[2m${q.group}\x1b[0m`); }
            const lock = (q.needs_retained && !hasRetained) ? ' \x1b[33m[needs full analysis]\x1b[0m' : '';
            term.writeln(`    \x1b[36m${q.name.padEnd(36)}\x1b[0m  \x1b[2m${q.display}\x1b[0m${lock}`);
          });
        }
        term.write(PROMPT);
        return;
      }
      const name = cmd.slice(5).trim();
      const q = namedQueries.find(q => q.name === name);
      if (!q) {
        const close = namedQueries
          .filter(q => q.name.toLowerCase().includes(name.toLowerCase()))
          .slice(0, 3)
          .map(q => q.name);
        term.writeln(`\x1b[31merror: unknown query name ${JSON.stringify(name)}\x1b[0m`);
        if (close.length) {
          term.writeln(`\x1b[2m  did you mean: ${close.join(', ')}\x1b[0m`);
        }
        term.write(PROMPT);
        return;
      }
      if (q.needs_retained && !hasRetained) {
        term.writeln('\x1b[33mthis query requires full analysis — click \'Run Analysis\' in the toolbar first\x1b[0m');
        term.write(PROMPT);
        return;
      }
      term.writeln(`\x1b[2m↳ ${q.oql.length > 90 ? q.oql.slice(0, 89) + '…' : q.oql}\x1b[0m`);
      await runQuery(q.oql);
      return;
    }
    // If the input looks like an unrecognized /command (starts with /) warn the user
    // rather than sending it to the server as OQL (which would just fail with a parse error).
    if (full.trim().startsWith('/')) {
      const cmdWord = full.trim().split(/\s+/)[0].toLowerCase();
      term.writeln(`\x1b[33munknown command: ${cmdWord}  (type /help to see all commands)\x1b[0m`);
      const ALL_CMDS = [
        '/classes', '/fields', '/describe', '/obj',
        '/count', '/wc', '/last', '/cols', '/columns', '/history', '/row', '/plan', '/explain',
        '/filter', '/grep', '/not', '/exclude', '/sort', '/select', '/drop', '/rename',
        '/distinct', '/dedup', '/sample', '/top', '/head', '/tail', '/unique', '/pivot',
        '/stats', '/undo', '/run', '/limit', '/set', '/export', '/bookmark', '/save',
        '/forget', '/watch', '/analyze', '/status', '/clear', '/q', '/disconnect', '/help',
      ];
      const typed = cmdWord.slice(1);
      const close = ALL_CMDS.filter(c => {
        const n = c.slice(1);
        return n.startsWith(typed.slice(0, 2)) || typed.startsWith(n.slice(0, 2)) || n.includes(typed) || typed.includes(n);
      }).slice(0, 3);
      if (close.length > 0) {
        term.writeln(`\x1b[2m  did you mean: ${close.join(', ')}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    await runQuery(full.trim());
  }

  let currentAbort = null;  // AbortController for in-flight query
  let lastResult = null;    // { columns, rows } of last successful query for /export
  let prevResult = null;    // single-level undo: saved before result-mutating commands
  let watchTimer = null;    // setInterval handle for /watch
  let currentRowIdx = 0;    // 0-based row cursor for /row next/prev

  function cellColor(cell, colName) {
    if (!settings.color) return '';
    if (cell === null || cell === undefined) return '\x1b[2m';
    if (typeof cell !== 'object') return '';
    const kind = cell.kind;
    if (kind === 'null') return '\x1b[2m';
    if (kind === 'int') {
      if (colName && /address|addr|ptr/i.test(colName)) return '\x1b[35m'; // magenta for addresses
      if (!settings.bytesRaw && colName && /bytes$|_size$|heap_size$/i.test(colName)) return '\x1b[33m'; // yellow for sizes
      return '\x1b[32m'; // green for numbers
    }
    if (kind === 'float') return '\x1b[32m';
    if (kind === 'bool') return cell.v ? '\x1b[32m' : '\x1b[31m';
    if (kind === 'obj_ref') return '\x1b[36m'; // cyan for refs
    return '';
  }

  function renderResult(r) {
    const colNames = r.columns.map(c => c.name || String(c));
    const rows = r.rows || [];
    if (rows.length === 0) {
      term.writeln('\x1b[2m(no rows)\x1b[0m');
      return { colNames, adjW: [], isNumeric: [] };
    }
    const isNumeric = colNames.map((_, i) => {
      const sample = rows.find(row => row[i] !== null && row[i] !== undefined);
      return sample ? isNumericKind(sample[i]) : false;
    });
    const colW = colNames.map((n, i) => {
      const contentMax = rows.slice(0, settings.rowLimit).reduce((m, row) => Math.max(m, fmtCell(row[i], n).length), 0);
      return Math.max(n.length, contentMax, 4);
    });
    const gap = 2;
    const totalW = colW.reduce((s, w) => s + w + gap, 0) - gap;
    const maxW = term.cols - 2;
    const scale = totalW > maxW ? maxW / totalW : 1;
    const adjW = colW.map(w => Math.max(4, Math.floor(w * scale)));
    const displayRows = rows.slice(0, settings.rowLimit);
    // Add a row-number gutter when there are 2+ rows
    const showRowNums = displayRows.length >= 2;
    const rowNumW = showRowNums ? String(displayRows.length).length : 0;
    const gutterPad = showRowNums ? ' '.repeat(rowNumW + 2) : '';
    const header = gutterPad + colNames.map((n, i) => padTo(n, adjW[i], isNumeric[i])).join('  ');
    term.writeln('\x1b[1m' + header + '\x1b[0m');
    term.writeln('\x1b[2m' + '─'.repeat(Math.min(header.length, term.cols - 2)) + '\x1b[0m');
    displayRows.forEach((row, ri) => {
      const cells = row.map((cell, i) => {
        const txt = padTo(fmtCell(cell, colNames[i]), adjW[i], isNumeric[i]);
        const color = cellColor(cell, colNames[i]);
        return color ? color + txt + '\x1b[0m' : txt;
      });
      const gutter = showRowNums
        ? `\x1b[2m${String(ri + 1).padStart(rowNumW)}\x1b[0m  `
        : '';
      term.writeln(gutter + cells.join('  '));
    });
    if (rows.length > settings.rowLimit) {
      term.writeln(`\x1b[33m-- showing ${settings.rowLimit} of ${rows.length} rows (use /set limit 0 or /set limit N to change) --\x1b[0m`);
    }
    if (r.note) {
      term.writeln(`\x1b[33m-- ${r.note} --\x1b[0m`);
    }
    return { colNames, adjW, isNumeric };
  }

  async function runQuery(oql, { showHint = true } = {}) {
    // Soft warning: if query references retained-size attributes but analysis hasn't run
    if (!hasRetained && /@retainedHeapSize|@dominatorClass|dominators\s*\(|retained\s*\(/i.test(oql)) {
      term.writeln('\x1b[33m[hint] Query uses retained-size data — click "Run Analysis" first for accurate results\x1b[0m');
    }
    const t0 = performance.now();
    const abortCtrl = new AbortController();
    currentAbort = abortCtrl;
    // Animated spinner while waiting for response
    const spinFrames = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];
    let spinIdx = 0;
    term.write('\x1b[2m' + spinFrames[0] + ' running\x1b[0m');
    const spinTimer = setInterval(() => {
      spinIdx = (spinIdx + 1) % spinFrames.length;
      const elapsed = (performance.now() - t0) / 1000;
      const elStr = elapsed >= 1 ? ` ${elapsed.toFixed(1)}s` : '';
      term.write('\r\x1b[K\x1b[2m' + spinFrames[spinIdx] + ' running' + elStr + '\x1b[0m');
    }, 100);
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
      const elapsedMs = performance.now() - t0;
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
        // Suggest class names when the error looks like "class not found"
        if (classNames.length > 0 && /class|type|not found|unknown/i.test(msg)) {
          const wordMatch = msg.match(/[A-Z][a-zA-Z0-9$.]+/g) || [];
          wordMatch.forEach(word => {
            const lower = word.toLowerCase();
            const suggestions = classNames
              .filter(c => c.toLowerCase().includes(lower) || lower.includes(c.split('.').pop().toLowerCase()))
              .slice(0, 3);
            if (suggestions.length > 0) {
              term.writeln(`\x1b[2mdid you mean: ${suggestions.map(s => s.split('.').pop()).join(', ')}?\x1b[0m`);
            }
          });
        }
      } else {
        const r = data.result;
        if (r.error) {
          term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
        } else if (r.columns && r.columns.length > 0) {
          const colNames = r.columns.map(c => c.name || String(c));
          const rows = r.rows || [];
          renderResult(r);
          lastResult = { columns: colNames, rows, note: r.note, truncated: r.truncated, row_count: r.row_count };
          currentRowIdx = 0;
          const trunc = r.truncated ? `  \x1b[33m[capped at ${r.row_count} rows — add LIMIT N for more]\x1b[0m` : '';
          const elapsedFmt = elapsedMs < 1000 ? `${elapsedMs.toFixed(0)}ms` : `${(elapsedMs / 1000).toFixed(3)}s`;
          const elapsedColor = elapsedMs > 1000 ? '\x1b[31m' : elapsedMs > 300 ? '\x1b[33m' : '\x1b[2m';
          const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
          term.writeln(`${elapsedColor}${r.row_count} row${r.row_count !== 1 ? 's' : ''}, ${elapsedFmt}\x1b[0m\x1b[2m  [${ts}]\x1b[0m${trunc}`);
          if (rows.length > 20 && showHint) {
            const hasNumeric = colNames.some((_, i) => {
              const sample = rows.find(row => row[i] !== null && row[i] !== undefined);
              return sample ? isNumericKind(sample[i]) : false;
            });
            const statHint = hasNumeric ? '  /stats <col>' : '';
            term.writeln(`\x1b[2m  /filter <text|/re/>  /sort [-]<col>  /select <col>…  /pivot <col>  /row [N]${statHint}  /export [csv|tsv|json]\x1b[0m`);
          }
        } else {
          // No columns — just show the raw result
          term.writeln(JSON.stringify(r, null, 2).split('\n').slice(0, 40).join('\r\n'));
          term.writeln(`\x1b[2m${elapsedMs < 1000 ? elapsedMs.toFixed(0) + 'ms' : elapsed + 's'}\x1b[0m`);
        }
      }
    } catch (e) {
      clearInterval(spinTimer);
      currentAbort = null;
      term.write('\r\x1b[K');
      if (e.name === 'AbortError') {
        term.writeln('\x1b[2m(cancelled)\x1b[0m');
      } else {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
    }
    term.write(PROMPT);
  }

  function printHelp() {
    const h = (t) => term.writeln(`\r\n\x1b[1;33m${t}\x1b[0m`);
    const c = (cmd, desc) => term.writeln(`  \x1b[36m${cmd.padEnd(28)}\x1b[0m ${desc}`);
    term.writeln('');
    h('Heap exploration');
    c('/classes [pat]',          '— list class names (substring filter)');
    c('/fields [pat]',           '— list instance field names (substring filter)');
    c('/describe <cls>',         '— show fields + types of a class, with instance count');
    c('/obj <cls>#<idx>',        '— inspect a specific object by class + dense index');
    h('Running queries');
    c('<oql>',                   '— run an OQL query  (see /help oql for language reference)');
    c('/run [name]',             '— run named query; list all if no name');
    c('/watch <s> <oql>',        '— repeat query every N seconds; /watch stop to cancel');
    c('/count [cls|oql]',        '— count instances (class name) or rows (oql); no arg = shape');
    c('/plan <oql>',             '— show query execution plan without scanning (/explain alias)');
    h('Inspecting results');
    c('/last',                   '— re-display last result');
    c('/wc [col]',               '— shape (rows × cols); col arg = count non-null values');
    c('/row [N|next|prev|last]', '— show row as key=value pairs; next/prev navigate');
    c('/cols',                   '— list columns with type and non-null fill rate');
    c('/stats [col]',            '— min/max/mean/stddev/p50/p90/p99/sum + histogram; no arg = all numeric');
    h('Shaping results');
    c('/filter <text|/re/>',     '— keep matching rows; /filter @<col> <text> for one column');
    c('/not <text|/re/>',        '— exclude matching rows; /not @<col> <text> for one column');
    c('/sort <col> [desc]',      '— sort; - prefix for desc (e.g. /sort -size,name)');
    c('/select <col> …',         '— keep only named columns (names or 1-based numbers)');
    c('/drop <col> …',           '— remove columns (inverse of /select)');
    c('/rename <old> <new>',     '— rename a column');
    c('/distinct',               '— remove duplicate rows (/dedup alias)');
    c('/sample [N]',             '— N randomly sampled rows (default 10)');
    c('/top [N]  /head [N]',     '— first N rows (default 10)');
    c('/tail [N]',               '— last N rows (default 10)');
    c('/unique <col> [N]',       '— distinct value counts, top N by frequency');
    c('/pivot <col> [N]',        '— group by column → (value, count) table, top N optional');
    c('/undo',                   '— restore result before last shaping command');
    h('Exporting');
    c('/export [csv|tsv|json]',  '— copy/download result (default csv)');
    h('History & bookmarks');
    c('/history [N|clear]',      '— show/clear history; !N to re-run entry N');
    c('/bookmark  /save [name]', '— save last query as named bookmark');
    c('/forget <name>',          '— delete a bookmark');
    h('Settings  (/set with no args shows current values)');
    c('/set limit <N>',          '— cap rows displayed (0 = unlimited, default 200)');
    c('/set bytes raw|human',    '— byte columns: numbers or 4.3 KiB (default human)');
    c('/set color on|off',       '— cell colorization (default on)');
    c('/set null <str>',         '— null display string (default "null")');
    c('/limit <N>',              '— alias for /set limit N, re-displays result');
    h('Session');
    c('/status',                 '— analysis status (needed for @retainedHeapSize)');
    c('/analyze',                '— trigger full heap analysis');
    c('/clear',                  '— clear terminal');
    c('/q  /disconnect',         '— back to connect screen (Ctrl+D on empty line)');
    c('/help',                   '— this message');
    c('/help oql',               '— OQL language reference (keywords, functions, syntax)');
    term.writeln('');
    term.writeln('\x1b[1;33mKeyboard shortcuts\x1b[0m');
    term.writeln('  Tab       complete  ·  Ctrl+R  reverse history search  ·  ↑/↓  history');
    term.writeln('  Ctrl+A/E  line start/end  ·  Alt+←/→  word left/right');
    term.writeln('  Ctrl+K/W/U  kill  ·  Ctrl+Y  yank  ·  Ctrl+C  abort query  ·  Ctrl+L  clear');
    term.writeln('  \\  at end of line  →  continue query on next line');
    if (namedQueries.length > 0) {
      term.writeln('');
      term.writeln('\x1b[33mNamed queries\x1b[0m  \x1b[2m(use /run <name> or click sidebar)\x1b[0m');
      let cur = '';
      namedQueries.forEach(q => {
        if (q.group !== cur) {
          cur = q.group;
          term.writeln(`\r  \x1b[2m${cur}\x1b[0m`);
        }
        const lock = q.needs_retained ? '  \x1b[2m[needs analysis]\x1b[0m' : '';
        term.writeln(`    \x1b[36m${q.name.padEnd(36)}\x1b[0m  \x1b[2m${q.display}\x1b[0m${lock}`);
      });
    }
    term.writeln('');
  }

  async function printOqlRef() {
    term.writeln('\r\n\x1b[1mOQL Language Reference\x1b[0m  \x1b[2m(from server /help)\x1b[0m');
    let ref_;
    try {
      ref_ = await fetch(serverUrl + '/help').then(r => r.json());
    } catch (e) {
      term.writeln(`\x1b[31mcould not fetch /help: ${e.message}\x1b[0m`);
      return;
    }
    const section = (title, items) => {
      if (!items || !items.length) return;
      term.writeln(`\r\n  \x1b[33m${title}\x1b[0m`);
      const COLS = 4;
      const colW = Math.max(14, Math.floor((term.cols - 4) / COLS));
      for (let i = 0; i < items.length; i += COLS) {
        term.writeln('    ' + items.slice(i, i + COLS).map(s => padTo(String(s), colW)).join(''));
      }
    };
    section('Keywords', ref_.keywords);
    section('Aggregate functions', ref_.aggregates);
    section('Functions', ref_.functions);
    section('Methods (on objects)', ref_.methods);
    section('Attributes (@ prefix)', ref_.attributes?.map(a => '@' + a));
    term.writeln('');
    term.writeln('  \x1b[33mSyntax examples\x1b[0m');
    term.writeln('    \x1b[2mSELECT * FROM java.lang.String\x1b[0m');
    term.writeln('    \x1b[2mSELECT s.@objectAddress, s.value FROM java.lang.String s WHERE s.count > 100\x1b[0m');
    term.writeln('    \x1b[2mSELECT classof(s).@name, COUNT(*) FROM java.lang.Object s GROUP BY classof(s)\x1b[0m');
    term.writeln('    \x1b[2mSELECT * FROM INSTANCEOF java.util.Collection\x1b[0m');
    term.writeln('    \x1b[2mSELECT s.@retainedHeapSize FROM java.lang.Thread s ORDER BY s.@retainedHeapSize DESC\x1b[0m');
    term.writeln('');
    term.writeln('  \x1b[2mTip: /describe <ClassName> to see available fields\x1b[0m');
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
        void handleEnter(text);
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
      void handleEnter(text);
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
        if (watchTimer) {
          clearInterval(watchTimer);
          watchTimer = null;
          term.writeln('\x1b[32m✓ query aborted, watch stopped\x1b[0m');
          term.write(PROMPT);
        }
      } else if (watchTimer) {
        clearInterval(watchTimer);
        watchTimer = null;
        term.writeln('^C');
        term.writeln('\x1b[32m✓ watch stopped\x1b[0m');
        term.write(PROMPT);
      } else {
        const hadPending = pendingLines.length > 0;
        term.writeln('^C');
        line = '';
        cursorPos = 0;
        histIdx = -1;
        pendingLines = [];
        if (hadPending) {
          term.writeln('\x1b[2m(multi-line input discarded)\x1b[0m');
        }
        term.write(PROMPT);
      }
      return;
    }

    if (ev.ctrlKey && code === 'l') {
      term.clear();
      redrawLine();
      return;
    }

    if (ev.ctrlKey && code === 'd') {
      if (line.length === 0) {
        term.writeln('\x1b[2m(Ctrl+D — disconnecting)\x1b[0m');
        document.getElementById('btn-disconnect').click();
      }
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
