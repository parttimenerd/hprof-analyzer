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
      const cmds = ['help','clear','status','analyze','history','export','set','classes','filter','grep',
                    'sort','unique','stats','top','head','tail','cols','columns','obj','run','bookmark','save','forget','last','describe','count','watch','q','quit','disconnect'];
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
    // Complete /export csv|tsv
    if (line.startsWith('/export ')) {
      const partial = line.slice(8).toLowerCase();
      const fmts = ['tsv', 'csv'].filter(f => f.startsWith(partial));
      if (fmts.length === 1) { setLine('/export ' + fmts[0]); }
      else if (fmts.length > 1) { term.writeln(''); term.writeln('  tsv  csv'); redrawLine(); }
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
    // Complete /sort <col> from lastResult columns
    if (line.startsWith('/sort ') || line.startsWith('/filter ') || line.startsWith('/grep ') ||
        line.startsWith('/unique ') || line.startsWith('/stats ')) {
      if (lastResult && lastResult.columns.length > 0) {
        const pfxLen = line.startsWith('/sort ') ? 6 : line.startsWith('/stats ') ? 7
                     : line.startsWith('/grep ') ? 6 : 8;
        const partial = line.slice(pfxLen).toLowerCase();
        const cols = lastResult.columns.filter(c => c.toLowerCase().startsWith(partial));
        if (cols.length === 1) { setLine(line.slice(0, pfxLen) + cols[0]); }
        else if (cols.length > 1 && cols.length <= 20) {
          term.writeln('');
          term.writeln('  ' + cols.map(c => `\x1b[36m${c}\x1b[0m`).join('  '));
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
    if (cmd.startsWith('/describe ') || cmd === '/describe') {
      const cls = cmd.slice(9).trim();
      if (!cls) {
        term.writeln('\x1b[33mUsage: /describe <ClassName>  — show fields by running SELECT * LIMIT 1\x1b[0m');
        term.write(PROMPT);
        return;
      }
      term.writeln(`\x1b[2mQuerying fields of ${cls}…\x1b[0m`);
      try {
        const res = await fetch(serverUrl + '/', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: `SELECT * FROM ${cls} LIMIT 1` }),
          signal: AbortSignal.timeout(10000),
        });
        const data = await res.json();
        term.write('\r\x1b[K');
        if (!data.ok || !data.result?.columns) {
          const msg = data.error?.message || 'no result';
          term.writeln(`\x1b[31m${msg}\x1b[0m`);
          if (classNames.length > 0) {
            const lower = cls.toLowerCase();
            const sugg = classNames.filter(c => c.toLowerCase().includes(lower)).slice(0, 5);
            if (sugg.length) term.writeln(`\x1b[2mSimilar: ${sugg.map(c => c.split('.').pop()).join(', ')}\x1b[0m`);
          }
        } else {
          const colNames = data.result.columns.map(c => c.name || String(c));
          term.writeln(`\x1b[1mFields of ${cls}:\x1b[0m`);
          const COLS = 3;
          const colW = Math.max(20, Math.floor((term.cols - 4) / COLS));
          for (let i = 0; i < colNames.length; i += COLS) {
            term.writeln('  ' + colNames.slice(i, i + COLS).map(n => padTo(n, colW)).join('  '));
          }
          term.writeln(`\x1b[2m${colNames.length} field${colNames.length !== 1 ? 's' : ''}\x1b[0m`);
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
        term.writeln('\x1b[33mUsage: /obj <ClassName>#<idx>  — inspect a specific object by class + dense index\x1b[0m');
        term.write(PROMPT);
        return;
      }
      // Parse "<Class>#<n>" or "<Class> <n>" formats
      const m = arg.match(/^(.+?)#(\d+)$/) || arg.match(/^(.+?)\s+(\d+)$/);
      if (!m) {
        term.writeln('\x1b[33mUsage: /obj <ClassName>#<idx>  e.g. /obj java.lang.String#42\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const [, cls, idx] = m;
      await runQuery(`SELECT * FROM ${cls.trim()} s WHERE s.@objectId = ${idx}`);
      return;
    }
    if (cmd === '/watch' || cmd.startsWith('/watch ')) {
      const args = cmd.slice(6).trim();
      // /watch stop
      if (args === 'stop' || args === '') {
        if (watchTimer) {
          clearInterval(watchTimer);
          watchTimer = null;
          term.writeln('\x1b[33mWatch stopped.\x1b[0m');
        } else if (args === '') {
          term.writeln('\x1b[33mUsage: /watch <seconds> <oql>  — refresh query every N seconds; /watch stop\x1b[0m');
        } else {
          term.writeln('\x1b[2mNo active watch.\x1b[0m');
        }
        term.write(PROMPT);
        return;
      }
      const m = args.match(/^(\d+(?:\.\d+)?)\s+(.+)$/s);
      if (!m) {
        term.writeln('\x1b[33mUsage: /watch <seconds> <oql>\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const secs = parseFloat(m[1]);
      const watchOql = m[2].trim();
      if (secs < 1) {
        term.writeln('\x1b[31mMinimum interval is 1 second.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      if (watchTimer) { clearInterval(watchTimer); watchTimer = null; }
      term.writeln(`\x1b[2mWatching every ${secs}s — Ctrl+C or /watch stop to cancel\x1b[0m`);
      const tick = async () => {
        const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
        term.writeln(`\x1b[2m── ${ts} ──────────────────────────────────────────\x1b[0m`);
        await runQuery(watchOql);
        term.write(PROMPT);
      };
      await tick();
      watchTimer = setInterval(tick, secs * 1000);
      return;
    }
    if (cmd.startsWith('/count ') || cmd === '/count') {
      const cls = cmd.slice(6).trim();
      if (!cls) {
        term.writeln('\x1b[33mUsage: /count <ClassName>  — number of live instances\x1b[0m');
        term.write(PROMPT);
        return;
      }
      term.write('\x1b[2m⠋ counting…\x1b[0m');
      try {
        const res = await fetch(serverUrl + '/', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: `SELECT COUNT(*) FROM INSTANCEOF ${cls}` }),
        });
        const data = await res.json();
        term.write('\r\x1b[K');
        if (data.ok) {
          const cell = data.result?.rows?.[0]?.[0];
          const n = cell == null ? null : (typeof cell === 'object' ? cell.v : cell);
          term.writeln(`\x1b[32m${n != null ? n.toLocaleString() : '?'}\x1b[0m instance${n === 1 ? '' : 's'} of \x1b[36m${cls}\x1b[0m`);
        } else {
          const msg = data.error?.message || data.error || 'unknown error';
          term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
        }
      } catch (e) {
        term.write('\r\x1b[K');
        term.writeln(`\x1b[31mrequest failed: ${e.message}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/last') {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result yet — run a query first.\x1b[0m');
      } else {
        renderResult(lastResult);
        term.writeln(`\x1b[2m${lastResult.rows.length} rows (re-displayed)\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/cols' || cmd === '/columns') {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result — run a query first.\x1b[0m');
      } else {
        const fields = lastResult.columns;
        const colW = Math.max(...fields.map(f => f.length)) + 2;
        const cols = Math.max(1, Math.floor((term.cols - 4) / colW));
        for (let i = 0; i < fields.length; i += cols) {
          term.writeln('  ' + fields.slice(i, i + cols).map(f => f.padEnd(colW)).join('').trimEnd());
        }
        term.writeln(`\x1b[2m${fields.length} column${fields.length !== 1 ? 's' : ''}\x1b[0m`);
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
          term.writeln('\x1b[1mBookmarks:\x1b[0m');
          entries.forEach(([name, oql]) => {
            const truncated = oql.length > term.cols - name.length - 6
              ? oql.slice(0, term.cols - name.length - 7) + '…' : oql;
            term.writeln(`  \x1b[36m${name.padEnd(20)}\x1b[0m  \x1b[2m${truncated}\x1b[0m`);
          });
          term.writeln('\x1b[2m  Use /bookmark <name> to save, /forget <name> to delete, !<name> to run\x1b[0m');
        }
      } else {
        // Save last query (or current line) under a name
        const toSave = history[0];
        if (!toSave) {
          term.writeln('\x1b[33mNo query to bookmark — run a query first.\x1b[0m');
        } else {
          bookmarks[rest] = toSave;
          localStorage.setItem(BOOKMARKS_KEY, JSON.stringify(bookmarks));
          term.writeln(`\x1b[32m✓ Saved as "${rest}": \x1b[2m${toSave.length > 60 ? toSave.slice(0, 59) + '…' : toSave}\x1b[0m`);
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
        term.writeln(`\x1b[32m✓ Removed bookmark "${name}"\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mNo bookmark named "${name}"\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/unique ') || cmd === '/unique') {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result — run a query first.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const colArg = cmd.slice(7).trim();
      if (!colArg) {
        term.writeln('\x1b[33mUsage: /unique <col>  — show distinct values in a column\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const ci = lastResult.columns.findIndex(c => c.toLowerCase() === colArg.toLowerCase()
        || c.toLowerCase().includes(colArg.toLowerCase()));
      if (ci < 0) {
        term.writeln(`\x1b[31mColumn "${colArg}" not found. Available: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const colName = lastResult.columns[ci];
      const seen = new Map();
      lastResult.rows.forEach(row => {
        const key = fmtCell(row[ci], colName);
        seen.set(key, (seen.get(key) || 0) + 1);
      });
      const entries = [...seen.entries()].sort((a, b) => b[1] - a[1]);
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
      entries.slice(0, settings.rowLimit).forEach(([val, cnt]) => {
        const pct = total > 0 ? (cnt / total * 100).toFixed(1) + '%' : '—';
        let bar = '';
        if (showBar && maxCnt > 0) {
          const filled = Math.round((cnt / maxCnt) * barCap);
          bar = '  \x1b[2m' + '█'.repeat(filled) + '░'.repeat(barCap - filled) + '\x1b[0m';
        }
        term.writeln(`${val.padEnd(colW)}  \x1b[32m${String(cnt).padStart(cntW)}\x1b[0m  \x1b[2m${pct.padStart(pctW)}\x1b[0m${bar}`);
      });
      term.writeln(`\x1b[2m${seen.size} distinct value${seen.size !== 1 ? 's' : ''} in ${lastResult.rows.length} rows\x1b[0m`);
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/stats ') || cmd === '/stats') {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result — run a query first.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const colArg = cmd.slice(6).trim();
      if (!colArg) {
        term.writeln('\x1b[33mUsage: /stats <col>  — numeric summary of a column\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const ci = lastResult.columns.findIndex(c => c.toLowerCase() === colArg.toLowerCase()
        || c.toLowerCase().includes(colArg.toLowerCase()));
      if (ci < 0) {
        term.writeln(`\x1b[31mColumn "${colArg}" not found. Available: ${lastResult.columns.join(', ')}\x1b[0m`);
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
        term.writeln(`\x1b[33mNo numeric values in column "${colName}"\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const sum = vals.reduce((s, v) => s + v, 0);
      const mean = sum / vals.length;
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
      term.writeln(`  p50    \x1b[32m${fmtV(p50)}\x1b[0m`);
      term.writeln(`  p90    \x1b[32m${fmtV(p90)}\x1b[0m`);
      term.writeln(`  p99    \x1b[32m${fmtV(p99)}\x1b[0m`);
      term.writeln(`  sum    \x1b[33m${fmtV(sum)}\x1b[0m`);
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/top ') || cmd === '/top' ||
        cmd.startsWith('/head ') || cmd === '/head') {
      const isHead = cmd.startsWith('/head');
      const n = parseInt(cmd.slice(isHead ? 5 : 4).trim(), 10);
      if (!lastResult) {
        term.writeln('\x1b[33mNo result to slice — run a query first.\x1b[0m');
      } else if (!n || n < 1) {
        term.writeln('\x1b[33mUsage: /top <N>  (or /head <N>) — show first N rows of last result\x1b[0m');
      } else {
        const sliced = lastResult.rows.slice(0, n);
        renderResult({ columns: lastResult.columns, rows: sliced, row_count: n });
        term.writeln(`\x1b[2mShowing top ${n} of ${lastResult.rows.length} rows\x1b[0m`);
        lastResult = { columns: lastResult.columns, rows: sliced };
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/tail ') || cmd === '/tail') {
      const n = parseInt(cmd.slice(5).trim(), 10);
      if (!lastResult) {
        term.writeln('\x1b[33mNo result to slice — run a query first.\x1b[0m');
      } else if (!n || n < 1) {
        term.writeln('\x1b[33mUsage: /tail <N>  — show last N rows of last result\x1b[0m');
      } else {
        const sliced = lastResult.rows.slice(-n);
        renderResult({ columns: lastResult.columns, rows: sliced, row_count: sliced.length });
        term.writeln(`\x1b[2mShowing last ${sliced.length} of ${lastResult.rows.length} rows\x1b[0m`);
        lastResult = { columns: lastResult.columns, rows: sliced };
      }
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/sort ') || cmd === '/sort') {
      const args = cmd.slice(5).trim();
      if (!lastResult || !args) {
        if (!lastResult) term.writeln('\x1b[33mNo result to sort — run a query first.\x1b[0m');
        else term.writeln('\x1b[33mUsage: /sort <col> [desc]  — sort last result by column\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const parts = args.split(/\s+/);
      const colArg = parts[0].toLowerCase();
      const desc = parts[1]?.toLowerCase() === 'desc';
      const ci = lastResult.columns.findIndex(c => c.toLowerCase() === colArg
        || c.toLowerCase().includes(colArg));
      if (ci < 0) {
        term.writeln(`\x1b[31mColumn "${parts[0]}" not found. Available: ${lastResult.columns.join(', ')}\x1b[0m`);
        term.write(PROMPT);
        return;
      }
      const colName = lastResult.columns[ci];
      const sorted = [...lastResult.rows].sort((a, b) => {
        const av = a[ci], bv = b[ci];
        const an = av?.v ?? av, bn = bv?.v ?? bv;
        if (an === null || an === undefined) return 1;
        if (bn === null || bn === undefined) return -1;
        const cmp = typeof an === 'number' && typeof bn === 'number'
          ? an - bn : String(an).localeCompare(String(bn));
        return desc ? -cmp : cmp;
      });
      renderResult({ columns: lastResult.columns, rows: sorted, row_count: sorted.length });
      lastResult = { columns: lastResult.columns, rows: sorted };
      term.writeln(`\x1b[2mSorted by ${colName} ${desc ? 'desc' : 'asc'}\x1b[0m`);
      term.write(PROMPT);
      return;
    }
    if (cmd.startsWith('/filter ') || cmd === '/filter' ||
        cmd.startsWith('/grep ')   || cmd === '/grep') {
      const isGrep = cmd.startsWith('/grep');
      const pattern = cmd.slice(isGrep ? 5 : 7).trim();
      if (!lastResult) {
        term.writeln('\x1b[33mNo result to filter — run a query first.\x1b[0m');
      } else if (!pattern) {
        term.writeln('\x1b[33mUsage: /filter <text>  or  /filter /regex/[flags]  (/grep is an alias)\x1b[0m');
      } else {
        const { columns, rows } = lastResult;
        let re;
        const reMatch = pattern.match(/^\/(.+)\/([gimsvy]*)$/);
        if (reMatch) {
          try { re = new RegExp(reMatch[1], reMatch[2] || 'i'); }
          catch (e) { term.writeln(`\x1b[31mInvalid regex: ${e.message}\x1b[0m`); term.write(PROMPT); return; }
        }
        const test = re
          ? (s) => re.test(s)
          : (s) => s.toLowerCase().includes(pattern.toLowerCase());
        const filtered = rows.filter(row =>
          row.some((cell, i) => test(fmtCell(cell, columns[i])))
        );
        if (filtered.length === 0) {
          term.writeln(`\x1b[33mNo rows match "${pattern}"\x1b[0m`);
        } else {
          renderResult({ columns, rows: filtered, row_count: filtered.length });
          term.writeln(`\x1b[2m${filtered.length} of ${rows.length} rows match "${pattern}"\x1b[0m`);
          lastResult = { columns, rows: filtered };
        }
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
        const maxLen = matches.reduce((m, c) => Math.max(m, c.length), 0);
        const colW = maxLen + 2;
        const cols = Math.max(1, Math.floor((term.cols - 4) / colW));
        for (let i = 0; i < matches.length; i += cols) {
          const row = matches.slice(i, i + cols);
          term.writeln('  ' + row.map(c => c.padEnd(colW)).join('').trimEnd());
        }
        term.writeln(`\x1b[2m${matches.length} class${matches.length !== 1 ? 'es' : ''}\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/set' || cmd.startsWith('/set ')) {
      const args = cmd.slice(4).trim().split(/\s+/);
      if (!args[0]) {
        // Print current settings
        term.writeln('\x1b[1mCurrent settings:\x1b[0m');
        term.writeln(`  limit    ${settings.rowLimit === Infinity ? 'unlimited' : settings.rowLimit}  \x1b[2m(rows displayed per query; 0 or "unlimited" = no cap)\x1b[0m`);
        term.writeln(`  bytes    ${settings.bytesRaw ? 'raw' : 'human'}  \x1b[2m(raw = show bytes as numbers)\x1b[0m`);
        term.writeln(`  null     "${settings.nullStr}"  \x1b[2m(how null values display)\x1b[0m`);
        term.writeln(`  color    ${settings.color ? 'on' : 'off'}  \x1b[2m(colorize table cells)\x1b[0m`);
        term.writeln('\x1b[2mUsage: /set limit 500 | /set bytes raw | /set bytes human | /set null ∅ | /set color off\x1b[0m');
      } else if (args[0] === 'limit') {
        const n = args[1] === '0' || args[1] === 'unlimited' || args[1] === 'none' ? 0 : parseInt(args[1], 10);
        if (isNaN(n) || n < 0 || n > 100000) {
          term.writeln('\x1b[31mUsage: /set limit <N>  (0 or "unlimited" = no cap)\x1b[0m');
        } else if (n === 0) {
          settings.rowLimit = Infinity;
          localStorage.setItem(SETTINGS_KEY, JSON.stringify({ ...settings, rowLimit: 0 }));
          term.writeln('\x1b[32mrow limit disabled (showing all rows)\x1b[0m');
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
      } else if (args[0] === 'color') {
        if (args[1] === 'off' || args[1] === 'false') { settings.color = false; }
        else if (args[1] === 'on' || args[1] === 'true' || !args[1]) { settings.color = true; }
        else { term.writeln('\x1b[31mUsage: /set color on|off\x1b[0m'); term.write(PROMPT); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32mcell color: ${settings.color ? 'on' : 'off'}\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mUnknown setting: ${args[0]}. Options: limit, bytes, null, color\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/export' || cmd.startsWith('/export ')) {
      if (!lastResult) {
        term.writeln('\x1b[33mNo result to export — run a query first.\x1b[0m');
        term.write(PROMPT);
        return;
      }
      const fmt = cmd.slice(7).trim().toLowerCase() || 'tsv';
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
        term.writeln(`\x1b[32m✓ Copied ${lastResult.rows.length} rows as ${ext.toUpperCase()} to clipboard\x1b[0m`);
      } catch (_) {
        const blob = new Blob([text], { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url; a.download = `query-result.${ext}`; a.click();
        URL.revokeObjectURL(url);
        term.writeln(`\x1b[32m✓ Downloaded result as query-result.${ext} (${lastResult.rows.length} rows)\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    if (cmd === '/history' || cmd.startsWith('/history ')) {
      if (history.length === 0) {
        term.writeln('\x1b[2m(no history yet)\x1b[0m');
      } else {
        const args = cmd.slice(8).trim();
        if (args === 'clear') {
          history.length = 0;
          localStorage.setItem(HISTORY_KEY, '[]');
          term.writeln('\x1b[32m✓ History cleared\x1b[0m');
          term.write(PROMPT);
          return;
        }
        const limit = args ? Math.min(parseInt(args, 10) || 20, history.length) : Math.min(20, history.length);
        const shown = history.slice(0, limit);
        shown.forEach((h, i) => {
          const num = String(i + 1).padStart(3);
          const truncated = h.length > term.cols - 8 ? h.slice(0, term.cols - 9) + '…' : h;
          term.writeln(`\x1b[2m${num}\x1b[0m  \x1b[2m!\x1b[0m\x1b[2m${String(i + 1)}\x1b[0m  ${truncated}`);
        });
        if (history.length > limit) {
          term.writeln(`\x1b[2m  … ${history.length - limit} more — /history N to show more\x1b[0m`);
        }
        term.writeln(`\x1b[2m  Use !N to re-run entry N  ·  /history clear to wipe\x1b[0m`);
      }
      term.write(PROMPT);
      return;
    }
    // !N — re-run history entry; !name — run a bookmark
    if (/^!.+$/.test(cmd)) {
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
          term.writeln(`\x1b[31mNo bookmark "!${name}" — use /bookmark to list\x1b[0m`);
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
            const lock = (q.needs_retained && !hasRetained) ? ' \x1b[2m[needs analysis]\x1b[0m' : '';
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
    await runQuery(full.trim());
  }

  let currentAbort = null;  // AbortController for in-flight query
  let lastResult = null;    // { columns, rows } of last successful query for /export
  let watchTimer = null;    // setInterval handle for /watch

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
    const header = colNames.map((n, i) => padTo(n, adjW[i], isNumeric[i])).join('  ');
    term.writeln('\x1b[1m' + header + '\x1b[0m');
    term.writeln('\x1b[2m' + '─'.repeat(Math.min(header.length, term.cols - 2)) + '\x1b[0m');
    const displayRows = rows.slice(0, settings.rowLimit);
    displayRows.forEach(row => {
      const cells = row.map((cell, i) => {
        const txt = padTo(fmtCell(cell, colNames[i]), adjW[i], isNumeric[i]);
        const color = cellColor(cell, colNames[i]);
        return color ? color + txt + '\x1b[0m' : txt;
      });
      term.writeln(cells.join('  '));
    });
    if (rows.length > settings.rowLimit) {
      term.writeln(`\x1b[2m… ${rows.length - settings.rowLimit} more rows (display limit ${settings.rowLimit} — use /set limit N)\x1b[0m`);
    }
    return { colNames, adjW, isNumeric };
  }

  async function runQuery(oql) {
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
              term.writeln(`\x1b[2mDid you mean: ${suggestions.map(s => s.split('.').pop()).join(', ')}?\x1b[0m`);
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
          lastResult = { columns: colNames, rows };
          const note = r.note ? `  \x1b[33m[${r.note}]\x1b[0m` : '';
          const trunc = r.truncated ? '  \x1b[33m[truncated]\x1b[0m' : '';
          const elapsedFmt = elapsedMs < 1000 ? `${elapsedMs.toFixed(0)}ms` : `${(elapsedMs / 1000).toFixed(3)}s`;
          const elapsedColor = elapsedMs > 1000 ? '\x1b[31m' : elapsedMs > 300 ? '\x1b[33m' : '\x1b[2m';
          const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
          term.writeln(`${elapsedColor}${r.row_count} row${r.row_count !== 1 ? 's' : ''}, ${elapsedFmt}\x1b[0m\x1b[2m  [${ts}]\x1b[0m${trunc}${note}`);
          if (rows.length > 20) {
            term.writeln(`\x1b[2m  /filter <text|/re/>  /sort <col>  /stats <col>  /unique <col>  /export [csv]\x1b[0m`);
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
        term.writeln('\x1b[33mcancelled\x1b[0m');
      } else {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
    }
    term.write(PROMPT);
  }

  function printHelp() {
    const h = (t) => term.writeln(`\r\n\x1b[33m${t}\x1b[0m`);
    const c = (cmd, desc) => term.writeln(`  \x1b[36m${cmd.padEnd(26)}\x1b[0m ${desc}`);
    term.writeln('');
    h('Session');
    c('/status',                 '— analysis status');
    c('/analyze',                '— trigger full heap analysis (enables @retainedHeapSize)');
    c('/q  /disconnect',         '— back to connect screen (Ctrl+D on empty line)');
    c('/clear',                  '— clear terminal');
    h('Query');
    c('/run [name]',             '— run named query; list all if no name');
    c('/watch <s> <oql>',        '— repeat query every N seconds; /watch stop to cancel');
    c('/classes [pat]',          '— list class names filtered by pattern');
    c('/describe <cls>',         '— show fields of a class');
    c('/count <cls>',            '— count live instances of a class');
    c('/obj <cls>#<idx>',        '— inspect a specific object by class + dense index');
    h('Result post-processing');
    c('/last',                   '— re-display last result');
    c('/cols',                   '— list column names of last result');
    c('/filter <text|/re/>',     '— filter rows by substring or regex  (/grep is an alias)');
    c('/sort <col> [desc]',      '— sort rows by column');
    c('/top <N>  /head <N>',      '— first N rows (updates lastResult for chaining)');
    c('/tail <N>',               '— last N rows');
    c('/unique <col>',           '— distinct value counts');
    c('/stats <col>',            '— min/max/mean/percentiles/sum');
    c('/export [csv]',           '— copy to clipboard as TSV or CSV');
    h('History & bookmarks');
    c('/history [N|clear]',      '— show/clear history; !N to re-run');
    c('/bookmark  /save [name]', '— save last query as a named bookmark');
    c('/forget <name>',          '— delete a bookmark');
    h('Settings');
    c('/set',                    '— view current settings');
    c('/set limit <N>',          '— max rows displayed (default 200)');
    c('/set bytes raw|human',    '— byte column formatting');
    c('/set color on|off',       '— cell colorization');
    c('/set null <str>',         '— null display string');
    h('Help');
    c('/help',                   '— this message');
    c('/?',                      '— alias for /help');
    c('/help oql',               '— OQL language reference');
    term.writeln('');
    term.writeln('\x1b[33mKeyboard shortcuts:\x1b[0m');
    term.writeln('  Tab       complete  ·  Ctrl+R  history search  ·  Up/Down  history');
    term.writeln('  Ctrl+A/E  line start/end  ·  Alt+←/→  word left/right');
    term.writeln('  Ctrl+K/W/U  kill  ·  Ctrl+Y  yank  ·  Ctrl+C  abort  ·  Ctrl+L  clear');
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
    term.writeln('    SELECT * FROM java.lang.String');
    term.writeln('    SELECT s.@objectAddress, s.value FROM java.lang.String s WHERE s.count > 100');
    term.writeln('    SELECT classof(s).@name, COUNT(*) FROM java.lang.Object s GROUP BY classof(s)');
    term.writeln('    SELECT * FROM INSTANCEOF java.util.Collection');
    term.writeln('    SELECT s.@retainedHeapSize FROM java.lang.Thread s ORDER BY s.@retainedHeapSize DESC');
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
          term.writeln('\x1b[33m^C — query aborted, watch stopped\x1b[0m');
          term.write(PROMPT);
        }
      } else if (watchTimer) {
        clearInterval(watchTimer);
        watchTimer = null;
        term.writeln('^C');
        term.writeln('\x1b[33mWatch stopped.\x1b[0m');
        term.write(PROMPT);
      } else {
        term.writeln('^C');
        line = '';
        cursorPos = 0;
        histIdx = -1;
        pendingLines = [];
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
