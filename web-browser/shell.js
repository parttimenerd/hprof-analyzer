// shell.js — injected into index.html after WASM init block.
// Outer scope provides: namedQueries, wasmReady, wasmComplete, HprofSession,
// activeHprof (the active WASM module's HprofSession — swaps to wasm64 on
// fallback), and window._switchToWasm64 / window._hprofOnWasm64.

// ── Theme management ──────────────────────────────────────────────────────────
// Cycles auto → light → dark → auto, same as the React report viewer.
// Persists in localStorage under the shared key "hprof-theme" so the shell,
// report, and OQL pages all stay in sync.
const THEME_KEY = 'hprof-theme';
const _THEME_CYCLE = { auto: 'light', light: 'dark', dark: 'auto' };
const _THEME_GLYPHS = { auto: '◐', light: '☀', dark: '☾' };

function _currentThemeMode() {
  try { return localStorage.getItem(THEME_KEY) || 'auto'; } catch (_) { return 'auto'; }
}

function _isDark() {
  const mode = _currentThemeMode();
  if (mode === 'dark') return true;
  if (mode === 'light') return false;
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

function termTheme() {
  if (_isDark()) {
    return {
      background: '#0a0a14', foreground: '#c8c8dc', cursor: '#7ab4ff',
      selectionBackground: '#2a3a5a', black: '#1a1a2a', brightBlack: '#3a3a5a',
      cyan: '#60c8e0', brightCyan: '#80e0f8', green: '#70d080', brightGreen: '#90f0a0',
      yellow: '#d0b060', brightYellow: '#f0d080', blue: '#5080d0', brightBlue: '#70a0f8',
      red: '#d06060', brightRed: '#f08080',
    };
  }
  return {
    background: '#f8f9fc', foreground: '#1a1a2e', cursor: '#2563eb',
    selectionBackground: '#bfd7ff', black: '#e8eaf0', brightBlack: '#6b7280',
    cyan: '#0e7490', brightCyan: '#0891b2', green: '#166534', brightGreen: '#15803d',
    yellow: '#854d0e', brightYellow: '#a16207', blue: '#1d4ed8', brightBlue: '#2563eb',
    red: '#9b1c1c', brightRed: '#dc2626',
  };
}

function applyTheme(mode) {
  const m = (mode === 'light' || mode === 'dark') ? mode : 'auto';
  if (m === 'auto') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.setAttribute('data-theme', m);
  const glyph = _THEME_GLYPHS[m];
  const label = m.charAt(0).toUpperCase() + m.slice(1);
  for (const id of ['btn-theme-toggle', 'btn-theme-toggle-shell']) {
    const btn = document.getElementById(id);
    if (btn) btn.textContent = `${glyph} Theme: ${label}`;
  }
  if (window._hprofTerm) window._hprofTerm.options.theme = termTheme();
}

(function initTheme() {
  applyTheme(_currentThemeMode());
})();

window.addEventListener('storage', e => {
  if (e.key === THEME_KEY) applyTheme(e.newValue || 'auto');
});

function _bindThemeToggle(id) {
  const btn = document.getElementById(id);
  if (!btn) return;
  btn.addEventListener('click', () => {
    const cur = _currentThemeMode();
    const next = _THEME_CYCLE[cur] || 'light';
    try { if (next === 'auto') localStorage.removeItem(THEME_KEY); else localStorage.setItem(THEME_KEY, next); } catch (_) {}
    applyTheme(next);
  });
}

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', () => {
    _bindThemeToggle('btn-theme-toggle');
    _bindThemeToggle('btn-theme-toggle-shell');
  });
} else {
  _bindThemeToggle('btn-theme-toggle');
  _bindThemeToggle('btn-theme-toggle-shell');
}

// ── Constants ─────────────────────────────────────────────────────────────────
const PROMPT = 'oql> ';
const HISTORY_KEY = 'hprof-analyzer.oql-history';
const SETTINGS_KEY = 'hprof-analyzer.settings';
const LAST_URL_KEY = 'hprof-analyzer.last-url';
const BOOKMARKS_KEY = 'hprof-analyzer.bookmarks';
const STORED_QUERIES_KEY = 'hprof-analyzer.stored-queries';
const STARRED_KEY = 'hprof-analyzer.starred';

// Restore last-used server URL into the input on page load
(function restoreLastUrl() {
  const saved = localStorage.getItem(LAST_URL_KEY);
  if (saved) {
    const el = document.getElementById('server-url');
    if (el) el.value = saved;
  }
})();

// ── Shell syntax highlighter (for setup-code-block) ─────────────────────────
// Returns an HTML string with span-wrapped tokens. Safe for innerHTML.
// Handles: command names, subcommands, --flags, "strings", # comments, /paths.
const _SHELL_CMDS = new Set(['brew','cargo','hprof-analyzer','claude','rustup','curl','sudo','mv','python3']);
function _shellHighlight(line) {
  const esc = s => s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
  const span = (cls, s) => `<span class="${cls}">${esc(s)}</span>`;
  let result = '';
  let i = 0;
  let wordIdx = 0; // 0=command, 1=first-arg (subcommand), 2+=rest

  while (i < line.length) {
    // # comment — rest of line
    if (line[i] === '#') {
      result += span('sh-cmt', line.slice(i));
      break;
    }
    // quoted string
    if (line[i] === '"' || line[i] === "'") {
      const q = line[i]; let j = i + 1;
      while (j < line.length && line[j] !== q) { if (line[j] === '\\') j++; j++; }
      result += span('sh-str', line.slice(i, j + 1));
      i = j + 1; continue;
    }
    // whitespace — pass through, reset subcommand tracking only between top-level words
    if (line[i] === ' ' || line[i] === '\t') {
      result += esc(line[i++]); continue;
    }
    // --flag or -f
    if (line[i] === '-' && (line[i+1] === '-' || /[a-zA-Z]/.test(line[i+1] || ''))) {
      let j = i;
      while (j < line.length && line[j] !== ' ' && line[j] !== '\t') j++;
      result += span('sh-flag', line.slice(i, j));
      i = j; continue;
    }
    // /path
    if (line[i] === '/') {
      let j = i;
      while (j < line.length && line[j] !== ' ' && line[j] !== '\t') j++;
      result += span('sh-path', line.slice(i, j));
      i = j; continue;
    }
    // word
    if (/\S/.test(line[i])) {
      let j = i;
      while (j < line.length && line[j] !== ' ' && line[j] !== '\t' && line[j] !== '#') j++;
      const word = line.slice(i, j);
      if (wordIdx === 0 && _SHELL_CMDS.has(word)) {
        result += span('sh-cmd', word);
      } else if (wordIdx === 1 && /^[a-z][a-z-]*$/.test(word) && !word.startsWith('-')) {
        result += span('sh-sub', word);
      } else {
        result += esc(word);
      }
      wordIdx++;
      i = j; continue;
    }
    result += esc(line[i++]);
  }
  return result;
}

// Apply shell highlighting to all .setup-code-block code elements.
// Apply JSON highlighting to all .setup-code-json pre elements.
function _jsonHighlight(text) {
  const esc = s => s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
  return text.replace(
    /("(?:[^"\\]|\\.)*")\s*(:)|("(?:[^"\\]|\\.)*")|(\btrue\b|\bfalse\b|\bnull\b)|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g,
    (_, key, colon, str, kw, num) => {
      if (key && colon) return `<span class="json-key">${esc(key)}</span>${esc(colon)}`;
      if (str)          return `<span class="json-str">${esc(str)}</span>`;
      if (kw)           return `<span class="json-kw">${esc(kw)}</span>`;
      if (num)          return `<span class="json-num">${esc(num)}</span>`;
      return esc(_);
    }
  );
}

function _applySetupHighlighting() {
  document.querySelectorAll('.setup-code-block code').forEach(el => {
    if (el.dataset.highlighted) return;
    el.dataset.highlighted = '1';
    el.innerHTML = _shellHighlight(el.textContent);
  });
  document.querySelectorAll('.setup-code-json pre').forEach(el => {
    if (el.dataset.highlighted) return;
    el.dataset.highlighted = '1';
    el.innerHTML = _jsonHighlight(el.textContent);
  });
}

// ── Setup modal wiring ────────────────────────────────────────────────────────
function _openSetupModal() {
  const modal = document.getElementById('setup-modal');
  if (modal) modal.style.display = 'flex';
}
function _closeSetupModal() {
  const modal = document.getElementById('setup-modal');
  if (modal) modal.style.display = 'none';
}

(function initSetupModal() {
  const open = () => _openSetupModal();
  const close = () => _closeSetupModal();

  // Wire buttons once DOM is ready
  function wire() {
    const btnOpen = document.getElementById('btn-setup-guide');
    const btnClose = document.getElementById('btn-close-setup');
    const footerLink = document.getElementById('footer-setup-link');
    const modal = document.getElementById('setup-modal');
    if (btnOpen) btnOpen.addEventListener('click', open);
    if (btnClose) btnClose.addEventListener('click', close);
    if (footerLink) footerLink.addEventListener('click', e => { e.preventDefault(); open(); });
    if (modal) modal.addEventListener('click', e => { if (e.target === modal) close(); });
    document.addEventListener('keydown', e => { if (e.key === 'Escape') close(); });

    // Tab switching inside setup modal
    document.querySelectorAll('.setup-tab').forEach(tab => {
      tab.addEventListener('click', () => {
        const panel = tab.dataset.tab;
        tab.closest('.setup-tabs').querySelectorAll('.setup-tab').forEach(t => t.classList.remove('active'));
        tab.classList.add('active');
        tab.closest('section').querySelectorAll('.setup-tab-panel').forEach(p => {
          p.style.display = p.dataset.panel === panel ? '' : 'none';
        });
      });
    });

    // Apply syntax highlighting to shell code blocks
    _applySetupHighlighting();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', wire);
  } else {
    wire();
  }
})();

// ── Install panel wiring ──────────────────────────────────────────────────────
(function initInstallPanel() {
  function wire() {
    const toggle = document.getElementById('btn-install-toggle');
    if (!toggle) return;
    toggle.addEventListener('click', _openSetupModal);
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', wire);
  } else {
    wire();
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
let wasmSession = null;    // HprofSession when running in WASM (no-server) mode
let term = null;
let classNames = [];  // populated after session loads (server may expose /class-names later)
let fieldNames = [];  // populated after session loads from /help endpoint
let hasRetained = false;
let selectedFile = null;  // File object selected on the upload screen

// ── Background analysis worker state ─────────────────────────────────────────
// Removed: analysis now runs inline before showing the shell.

// ── Screen helpers ────────────────────────────────────────────────────────────
function showScreen(id) {
  for (const sid of ['upload-screen', 'connect-screen', 'report-screen', 'shell-screen']) {
    const el = document.getElementById(sid);
    if (el) el.style.display = sid === id ? 'flex' : 'none';
  }
}

// ── Toast notifications ────────────────────────────────────────────────────────
// showToast(msg, type='info'|'success'|'error', durationMs=3500)
// Appends a self-dismissing bubble to #toast-container (created on first call).
function showToast(msg, type = 'info', durationMs = 3500) {
  let container = document.getElementById('toast-container');
  if (!container) {
    container = document.createElement('div');
    container.id = 'toast-container';
    document.body.appendChild(container);
  }
  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.textContent = msg;
  container.appendChild(toast);
  // Trigger CSS fade-in
  requestAnimationFrame(() => toast.classList.add('toast-visible'));
  const remove = () => {
    toast.classList.remove('toast-visible');
    toast.addEventListener('transitionend', () => toast.remove(), { once: true });
  };
  const timer = setTimeout(remove, durationMs);
  toast.addEventListener('click', () => { clearTimeout(timer); remove(); });
}

// ── ZIP decompression ─────────────────────────────────────────────────────────
// Extract the first .hprof entry from a ZIP archive (bytes) using only the
// browser's native DecompressionStream('deflate-raw'). Returns
// { hprofBytes: Uint8Array, uncmpSize: number } where uncmpSize is the true
// uncompressed size from the central directory (reliable even when flag bit 3
// sets cmpSize=0 in the local header — "data descriptor" ZIPs).
//
// Algorithm: always parse the EOCD + central directory first to get authoritative
// cmpSize/uncmpSize for every entry, then seek to the local-header data offset.

// ── ZIP pre-peek: read just the EOCD+CD tail to get .hprof uncompressed size ──
// Reads at most 65557 + cdSize bytes from the file (no full arrayBuffer needed).
// Returns the uncmpSize of the first .hprof entry, or 0 on any error.
async function _peekZipUncmpSize(file) {
  try {
    // Read the last 65557 bytes (max EOCD offset) to find EOCD
    const tailSize = Math.min(file.size, 65535 + 22);
    const tailBuf  = await file.slice(file.size - tailSize).arrayBuffer();
    const tail     = new Uint8Array(tailBuf);
    const tailView = new DataView(tailBuf);

    // Locate EOCD by scanning backward
    let eocdRelOff = -1;
    for (let i = tailSize - 22; i >= 0; i--) {
      if (tailView.getUint32(i, true) === 0x06054b50) { eocdRelOff = i; break; }
    }
    if (eocdRelOff < 0) return 0;

    const cdOffset  = tailView.getUint32(eocdRelOff + 16, true);
    const cdEntries = tailView.getUint16(eocdRelOff + 10, true);

    // Estimate CD size: from cdOffset to start of EOCD in file
    const eocdAbsOff = file.size - tailSize + eocdRelOff;
    const cdSize     = eocdAbsOff - cdOffset;
    if (cdSize <= 0 || cdSize > 64 * 1024 * 1024) return 0; // sanity

    // Read the central directory
    const cdBuf  = await file.slice(cdOffset, cdOffset + cdSize).arrayBuffer();
    const cdView = new DataView(cdBuf);
    let pos = 0;
    for (let i = 0; i < cdEntries && pos + 46 <= cdSize; i++) {
      if (cdView.getUint32(pos, true) !== 0x02014b50) break;
      const uncmpSize  = cdView.getUint32(pos + 24, true);
      const nameLen    = cdView.getUint16(pos + 28, true);
      const extraLen   = cdView.getUint16(pos + 30, true);
      const commentLen = cdView.getUint16(pos + 32, true);
      const nameBytes  = new Uint8Array(cdBuf, pos + 46, nameLen);
      const entryName  = new TextDecoder().decode(nameBytes);
      pos += 46 + nameLen + extraLen + commentLen;
      if (entryName.endsWith('.hprof') || entryName.endsWith('.HPROF')) {
        return uncmpSize;
      }
    }
    return 0;
  } catch (_) { return 0; }
}
//
// ZIP central-directory entry layout (little-endian):
//   4  sig 0x02014b50, 2 versionMade, 2 versionNeeded, 2 flags, 2 method
//   2  mtime, 2 mdate, 4 crc32
//   4  compressed-size, 4 uncompressed-size
//   2  filename-len, 2 extra-len, 2 comment-len, 2 diskNum
//   2  intAttr, 4 extAttr, 4 local-header-offset
// EOCD record (last 22 bytes when no comment):
//   4  sig 0x06054b50, 2 diskNum, 2 startDisk, 2 entriesHere, 2 totalEntries
//   4  cdSize, 4 cdOffset, 2 comment-len
async function extractHprofFromZip(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const len  = bytes.length;

  // ── 1. Locate End-of-Central-Directory (EOCD) ─────────────────────────────
  // Scan backward for 0x06054b50, allowing up to 65535 bytes of comment.
  const maxEocdScan = Math.min(len, 65535 + 22);
  let eocdOff = -1;
  for (let i = len - 22; i >= len - maxEocdScan; i--) {
    if (view.getUint32(i, true) === 0x06054b50) { eocdOff = i; break; }
  }
  if (eocdOff < 0) throw new Error('ZIP: EOCD record not found');

  const cdOffset = view.getUint32(eocdOff + 16, true);
  const cdEntries = view.getUint16(eocdOff + 10, true);

  // ── 2. Walk central directory to find the first .hprof entry ──────────────
  let cdPos = cdOffset;
  let found = null;
  for (let i = 0; i < cdEntries; i++) {
    if (view.getUint32(cdPos, true) !== 0x02014b50)
      throw new Error('ZIP: invalid central-directory signature');
    const method      = view.getUint16(cdPos + 10, true);
    const cmpSize     = view.getUint32(cdPos + 20, true);
    const uncmpSize   = view.getUint32(cdPos + 24, true);
    const nameLen     = view.getUint16(cdPos + 28, true);
    const extraLen    = view.getUint16(cdPos + 30, true);
    const commentLen  = view.getUint16(cdPos + 32, true);
    const localHdrOff = view.getUint32(cdPos + 42, true);
    const nameBytes   = bytes.subarray(cdPos + 46, cdPos + 46 + nameLen);
    const entryName   = new TextDecoder().decode(nameBytes);
    cdPos += 46 + nameLen + extraLen + commentLen;

    if (!found && (entryName.endsWith('.hprof') || entryName.endsWith('.HPROF'))) {
      found = { method, cmpSize, uncmpSize, localHdrOff, entryName };
    }
  }
  if (!found) throw new Error('No .hprof entry found in ZIP archive');

  // ── 3. Read compressed data via the local-header offset ───────────────────
  // Local header has its own nameLen/extraLen fields (may differ from CD).
  const lhv      = view;
  const lhNameLen  = lhv.getUint16(found.localHdrOff + 26, true);
  const lhExtraLen = lhv.getUint16(found.localHdrOff + 28, true);
  const dataStart  = found.localHdrOff + 30 + lhNameLen + lhExtraLen;
  // Use central-directory cmpSize — it's authoritative even for data-descriptor ZIPs.
  const compressed = bytes.subarray(dataStart, dataStart + found.cmpSize);

  // ── 4. Decompress ──────────────────────────────────────────────────────────
  if (found.method === 0) {
    // STORE — no compression
    return { hprofBytes: compressed.slice(), uncmpSize: found.uncmpSize };
  } else if (found.method === 8) {
    // DEFLATE — use DecompressionStream('deflate-raw')
    const ds = new DecompressionStream('deflate-raw');
    const writer = ds.writable.getWriter();
    const reader = ds.readable.getReader();
    writer.write(compressed);
    writer.close();
    const chunks = [];
    let totalLen = 0;
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      chunks.push(value);
      totalLen += value.length;
    }
    const realSize = found.uncmpSize || totalLen;
    const out = new Uint8Array(realSize);
    let off = 0;
    for (const chunk of chunks) { out.set(chunk, off); off += chunk.length; }
    return { hprofBytes: out, uncmpSize: realSize };
  } else {
    throw new Error(`ZIP entry '${found.entryName}' uses unsupported compression method ${found.method}`);
  }
}

// ── Upload screen ─────────────────────────────────────────────────────────────
// Sample dumps served from docs/samples/ on GitHub Pages (and locally when
// running from the repo root with e.g. python -m http.server).
const SAMPLE_DUMPS = [
  { name: 'mnemonics',     path: 'samples/dump_1_mnemonics.hprof',     sizeMb: 20 },
  { name: 'scala-doku',    path: 'samples/dump_2_scala-doku.hprof',    sizeMb: 51 },
  { name: 'philosophers',  path: 'samples/dump_4_philosophers.hprof',  sizeMb: 23 },
  { name: 'gauss-mix',     path: 'samples/dump_7_gauss-mix.hprof',     sizeMb: 70 },
];

(function initUploadScreen() {
  const dropZone = document.getElementById('drop-zone');
  const fileInput = document.getElementById('file-input');
  const modeButtons = document.getElementById('mode-buttons');
  const sampleSection = document.getElementById('sample-section');

  function onFileSelected(file) {
    if (!file) return;
    selectedFile = file;
    dropZone.classList.add('file-selected');
    document.getElementById('drop-zone-text').innerHTML =
      `<strong>${escHtml(file.name)}</strong> (${(file.size / 1024 / 1024).toFixed(1)} MB) — choose a mode below`;
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
    if (wasmReady && selectedFile) {
      loadWasmSession(selectedFile);
    } else {
      showScreen('connect-screen');
      if (wasmReady) populateOfflineList();
    }
  });

  document.getElementById('btn-analyze-report').addEventListener('click', () => {
    if (wasmReady && selectedFile) {
      loadWasmSessionWithReport(selectedFile, {});
    } else {
      const msg = document.getElementById('report-message');
      msg.textContent =
        'WASM not available. Start the local server and use the OQL Shell mode: ' +
        'hprof-analyzer query heap.hprof --server';
      showScreen('report-screen');
    }
  });

  document.getElementById('btn-full-analysis').addEventListener('click', () => {
    if (wasmReady && selectedFile) {
      loadWasmSessionWithReport(selectedFile, { findDuplicates: true, collections: true });
    } else {
      const msg = document.getElementById('report-message');
      msg.textContent =
        'WASM not available. Start the local server and use the OQL Shell mode: ' +
        'hprof-analyzer query heap.hprof --server';
      showScreen('report-screen');
    }
  });

  // Always show sample section; probe availability to decide if items are clickable
  const sampleList = document.getElementById('sample-list');
  sampleSection.style.display = '';
  SAMPLE_DUMPS.forEach(s => {
    const item = document.createElement('div');
    item.className = 'sample-item sample-item-pending';
    item.innerHTML =
      `<span class="sample-name">${s.name}</span>` +
      `<span class="sample-size">~${s.sizeMb} MB</span>`;
    sampleList.appendChild(item);
    fetch(s.path, { method: 'HEAD' })
      .then(r => {
        if (!r.ok) throw new Error('not found');
        item.classList.remove('sample-item-pending');
        item.addEventListener('click', () => loadSampleDump(s));
      })
      .catch(() => { item.classList.add('sample-item-offline'); item.title = 'Not available in this build'; });
  });

})();

// ── WASM session loading ───────────────────────────────────────────────────────

// Fetch a sample fixture by URL and open a mode-selection popup
async function loadSampleDump(sample) {
  const statusEl = document.getElementById('wasm-load-status');
  const modeButtons = document.getElementById('mode-buttons');
  const dropZone = document.getElementById('drop-zone');

  if (statusEl) { statusEl.textContent = `Fetching ${sample.name}…`; statusEl.style.display = ''; }
  let bytes;
  try {
    const resp = await fetch(sample.path);
    if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
    const buf = await resp.arrayBuffer();
    bytes = new Uint8Array(buf);
  } catch (e) {
    if (statusEl) { statusEl.textContent = `Failed to fetch ${sample.name}: ${e.message}`; }
    return;
  }
  if (statusEl) statusEl.style.display = 'none';

  // Manufacture a File-like object so the existing flow works
  const blob = new Blob([bytes]);
  const file = new File([blob], sample.name + '.hprof', { type: 'application/octet-stream' });
  selectedFile = file;
  dropZone.classList.add('file-selected');
  document.getElementById('drop-zone-text').innerHTML =
    `<strong>${escHtml(file.name)}</strong> (${(file.size / 1024 / 1024).toFixed(1)} MB) — choose a mode below`;
  modeButtons.style.display = 'flex';
}

// ── File format preparation ───────────────────────────────────────────────────
// Detects the format (gzip, zip, plain) and returns { bytes, isGzip, sizeMB }
// where `bytes` is always either already-gzip or will be gzip-compressed by the
// caller.  For ZIP: decompresses the .hprof entry, then gzip-compresses it.
// `onProgress(readBytes, totalBytes)` is called during streaming for plain files.
// `setStage(label, pct)` is called to update UI during each phase.
// Returns { bytes, isGzip, decompressedSize }.
// decompressedSize is the known uncompressed HPROF size in bytes (for the wasm32
// pre-check); 0 means unknown (gzip / plain — use file.size as approximation).
async function _prepareFileBytes(file, { compBarEnd, setStage, onProgress }) {
  const headerBuf = await file.slice(0, 4).arrayBuffer();
  const header = new Uint8Array(headerBuf);
  const isGzip = header[0] === 0x1f && header[1] === 0x8b;
  const isZip  = header[0] === 0x50 && header[1] === 0x4b;

  if (isGzip) {
    setStage(`Reading ${file.name}…`, 0);
    await new Promise(r => setTimeout(r, 20));
    return { bytes: new Uint8Array(await file.arrayBuffer()), isGzip: true, decompressedSize: 0 };
  }

  if (isZip) {
    setStage(`Extracting .hprof from ${file.name}… (step 1/3: reading ZIP)`, 0);
    await new Promise(r => setTimeout(r, 20));
    let zipBytes = new Uint8Array(await file.arrayBuffer());
    setStage(`Extracting .hprof from ${file.name}… (step 2/3: decompressing)`, Math.round(compBarEnd * 0.15));
    await new Promise(r => setTimeout(r, 20));
    let { hprofBytes, uncmpSize } = await extractHprofFromZip(zipBytes);
    zipBytes = null;  // free compressed ZIP bytes before gzip-compressing the extract
    setStage(`Compressing extracted dump… (step 3/3: re-compressing for WASM)`, Math.round(compBarEnd * 0.3));
    await new Promise(r => setTimeout(r, 20));
    const compressed = await gzipCompress(hprofBytes);
    hprofBytes = null;  // free plain hprof bytes before WASM allocates its heap
    return { bytes: compressed, isGzip: false, decompressedSize: uncmpSize };
  }

  // Plain HPROF — stream through CompressionStream
  setStage(`Compressing ${file.name}…`, 0);
  await new Promise(r => setTimeout(r, 20));
  const bytes = await gzipCompressFile(file, onProgress);
  return { bytes, isGzip: false, decompressedSize: 0 };
}

// Stream-compress a File through CompressionStream, collecting into Uint8Array.
// The File's raw bytes are never fully materialised in JS — only the compressed
// output accumulates. For a 2 GB plaintext hprof this typically yields ~500 MB.
// onProgress(bytesRead, total) fires steadily as input bytes are consumed.
async function gzipCompressFile(file, onProgress) {
  const total = file.size;
  let inputRead = 0;

  // Count input bytes as they pass through, before compression.
  const counter = new TransformStream({
    transform(chunk, controller) {
      inputRead += chunk.byteLength;
      if (onProgress) onProgress(inputRead, total);
      controller.enqueue(chunk);
    }
  });

  const stream = file.stream().pipeThrough(counter).pipeThrough(new CompressionStream('gzip'));
  const reader = stream.getReader();
  const chunks = [];
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
  }
  const totalLen = chunks.reduce((s, c) => s + c.byteLength, 0);
  const out = new Uint8Array(totalLen);
  let off = 0;
  for (const c of chunks) { out.set(c, off); off += c.byteLength; }
  return out;
}

// Compress bytes with gzip using the browser's CompressionStream API.
// Returns the compressed Uint8Array. Input is passed by reference; caller
// should null it after this call to free memory before the WASM load.
async function gzipCompress(bytes) {
  const cs = new CompressionStream('gzip');
  const writer = cs.writable.getWriter();
  writer.write(bytes);
  writer.close();
  const chunks = [];
  const reader = cs.readable.getReader();
  while (true) {
    const { done, value } = await reader.read();
    if (done) break;
    chunks.push(value);
  }
  const totalLen = chunks.reduce((s, c) => s + c.byteLength, 0);
  const out = new Uint8Array(totalLen);
  let off = 0;
  for (const c of chunks) { out.set(c, off); off += c.byteLength; }
  return out;
}

// ── ETA estimation ────────────────────────────────────────────────────────────
// Baseline ns/instance constants derived from measurements across 11 dumps
// (fixture dumps + HyperAlloc 128 MB – 2 GB). Correction factors are persisted
// in localStorage and updated after each run so estimates improve over time.
const _ETA_KEY = 'hprof-analyzer.eta-factors';
const _ETA_BASE = {
  // nanoseconds per instance — measured in browser on 712 MB / 9.4M-object dump
  parseNsPerInst: 1957,
  domNsPerInst:   1850,
  // compress: ms per raw MB (for non-gzip files). Gzip files skip JS compress.
  compMsPerMB:    11,
  // estimated instances per raw MB (for compress-phase parse estimate)
  instPerMB:      13200,
};

// Sub-phase fractions of total parse time (pass1_a + pass1_b + pass2 = 1.0).
// Pass2 is the dominant phase (~70%), pass1 scans are quick.
const _LOAD_PHASE_FRACS = { pass1_a: 0.12, pass1_b: 0.18, pass2: 0.70 };
// Cumulative share of dominator-analysis time consumed after each phase fires.
const _ANAL_PHASE_CUM_FRACS = { pass1: 0.03, pass2: 0.15, rpo: 0.25, inbound: 0.45, dominators: 0.80, retained: 0.95 };

function _etaFactors() {
  try { return JSON.parse(localStorage.getItem(_ETA_KEY) || '{}'); } catch { return {}; }
}

function _etaSaveFactors(f) {
  try { localStorage.setItem(_ETA_KEY, JSON.stringify(f)); } catch {}
}

// Record actual vs predicted and update the correction factor via EMA (α=0.3).
function _etaRecord(phase, predictedMs, actualMs) {
  if (predictedMs <= 0 || actualMs <= 0) return;
  const ratio = actualMs / predictedMs;
  const f = _etaFactors();
  const prev = f[phase] ?? 1.0;
  f[phase] = +(prev * 0.7 + ratio * 0.3).toFixed(4);
  _etaSaveFactors(f);
}

function _etaPredict(phase, instances) {
  const f = _etaFactors();
  const correction = f[phase] ?? 1.0;
  const base = _ETA_BASE;
  const nsPerInst = phase === 'parse' ? base.parseNsPerInst : base.domNsPerInst;
  return Math.max(0, Math.round((nsPerInst * instances / 1e6) * correction));
}

function _fmtEta(ms) {
  if (ms < 1000) return '<1s';
  const s = Math.round(ms / 1000);
  if (s < 60) return `~${s}s`;
  return `~${Math.floor(s/60)}m ${s%60}s`;
}

function _fmtCount(n) {
  if (n >= 1e6) return `${(n/1e6).toFixed(1)}M`;
  if (n >= 1e3) return `${(n/1e3).toFixed(0)}K`;
  return String(n);
}

// Build a progress-bar controller.  Returns `{ setStage, enterBlocking, updateBlocking }`.
function _makeProgress(labelId, barId) {
  // For non-blocking phases: quick JS-driven snap to a target pct.
  const setStage = (label, pct) => {
    const lbl = document.getElementById(labelId);
    const bar = document.getElementById(barId);
    if (lbl) lbl.textContent = label;
    if (bar) {
      bar.classList.remove('wasm-blocking');
      bar.style.animation = 'none';
      bar.style.transition = 'width 0.3s ease-out';
      bar.style.width = pct.toFixed(1) + '%';
    }
  };

  // Shared helper: restart the CSS linear transition from currentPct to 99%
  // so the bar reaches targetPct in exactly remainingMs.
  // totalDur = remainingMs × (99 - currentPct) / (targetPct - currentPct)
  const _restartTransition = (bar, currentPct, targetPct, remainingMs) => {
    const range = Math.max(targetPct - currentPct, 0.1);
    const totalDur = Math.round(remainingMs * (99 - currentPct) / range);
    bar.style.transition = 'none';
    bar.style.width = currentPct.toFixed(2) + '%';
    void bar.offsetWidth;
    bar.style.transition = `width ${totalDur}ms linear`;
    bar.style.width = '99%';
  };

  // Call before a synchronous blocking WASM call.
  // CSS transitions are compositor-driven and survive a blocked JS main thread.
  const enterBlocking = (label, fromPct, toPct, etaMs) => new Promise(resolve => {
    const lbl = document.getElementById(labelId);
    const bar = document.getElementById(barId);
    if (lbl) lbl.textContent = label;
    if (bar) {
      bar.classList.remove('wasm-blocking');
      bar.style.animation = 'none';
      _restartTransition(bar, fromPct, toPct, etaMs);
    }
    requestAnimationFrame(() => requestAnimationFrame(resolve));
  });

  // Call from inside a WASM progress callback to re-anchor the transition
  // based on the updated remaining ETA.  Reads the bar's current rendered
  // width so the bar never jumps backward.
  const updateBlocking = (label, targetPct, remainingMs) => {
    const lbl = document.getElementById(labelId);
    const bar = document.getElementById(barId);
    if (lbl && label) lbl.textContent = label;
    if (!bar) return;
    const wrap = bar.parentElement;
    const wrapW = wrap ? wrap.getBoundingClientRect().width : 0;
    const barW  = bar.getBoundingClientRect().width;
    const currentPct = wrapW > 0 ? Math.min((barW / wrapW) * 100, 98.9) : parseFloat(bar.style.width) || 0;
    if (remainingMs > 0) {
      _restartTransition(bar, currentPct, targetPct, remainingMs);
    }
  };

  // No-op kept for legacy call sites.
  const crawlTo = () => {};

  return { setStage, crawlTo, enterBlocking, updateBlocking };
}

async function loadWasmSession(file) {
  const statusEl = document.getElementById('wasm-load-status');
  const modeButtons = document.getElementById('mode-buttons');
  if (modeButtons) modeButtons.style.display = 'none';
  if (statusEl) {
    statusEl.innerHTML = `
      <div class="wasm-progress">
        <div class="wasm-progress-label" id="wasm-load-label">Reading ${escHtml(file.name)}…</div>
        <div class="wasm-progress-bar-wrap">
          <div class="wasm-progress-bar" id="wasm-load-bar"></div>
        </div>
      </div>`;
    statusEl.style.display = '';
  }

  const { setStage, crawlTo, enterBlocking, updateBlocking } = _makeProgress('wasm-load-label', 'wasm-load-bar');

  const fileMB = file.size / (1024 * 1024);

  // Pre-compute ETAs — treat zip as not-yet-gzip (needs compress step)
  const headerBuf = await file.slice(0, 4).arrayBuffer();
  const header = new Uint8Array(headerBuf);
  const isGzipUpfront = header[0] === 0x1f && header[1] === 0x8b;
  const isZipUpfront  = header[0] === 0x50 && header[1] === 0x4b;

  // For ZIP files, peek at the central directory to get the true uncompressed size.
  // This makes the compress-segment ETA proportional to actual work (uncmp MB),
  // not the tiny compressed ZIP size.
  let effectiveMB = fileMB;
  if (isZipUpfront) {
    const uncmp = await _peekZipUncmpSize(file);
    if (uncmp > 0) effectiveMB = uncmp / (1024 * 1024);
  }

  // Pre-compute ETAs so we can show proportional bar
  const compEtaMs  = isGzipUpfront ? 0 : Math.max(0, Math.round(_ETA_BASE.compMsPerMB * effectiveMB * (_etaFactors().compress ?? 1.0)));
  const estInst    = Math.round(effectiveMB * _ETA_BASE.instPerMB);
  const parseEtaMs = _etaPredict('parse', estInst);
  const totalLoadMs = compEtaMs + parseEtaMs;

  // Bar layout: 0→loadBarEnd is proportionally split between compress and parse;
  // loadBarEnd→100 is the dominator/analysis phase.
  const loadBarEnd = 60;
  // Compress gets a slice of 0..loadBarEnd proportional to its share of total time.
  const compBarEnd = totalLoadMs > 0
    ? Math.round((compEtaMs / totalLoadMs) * loadBarEnd)
    : 0;

  // Phase-aware label + bar during load_with_progress callbacks.
  // Re-anchors the CSS transition each time a phase completes (ETA update).
  let loadElapsedMs = 0;
  const onLoadPhase = (phase, _frac) => {
    const phaseMs = phase === 'compress'
      ? compEtaMs
      : Math.round(parseEtaMs * (_LOAD_PHASE_FRACS[phase] ?? 0));
    loadElapsedMs += phaseMs;
    const remainMs = Math.max(0, totalLoadMs - loadElapsedMs);
    const label = _loadPhaseLabel(phase, remainMs, estInst);
    updateBlocking(label, loadBarEnd, remainMs);
  };

  // Detect format and produce gzip bytes for the WASM parser.
  let bytes;
  let isGzip;
  let decompressedSize = 0;
  try {
    const compT0 = performance.now();
    ({ bytes, isGzip, decompressedSize } = await _prepareFileBytes(file, {
      compBarEnd,
      setStage,
      onProgress: (inputRead, total) => {
        const frac = total > 0 ? inputRead / total : 0;
        const pct = Math.round(frac * compBarEnd);
        const readMB = (inputRead / 1048576).toFixed(0);
        const totMB  = (total    / 1048576).toFixed(0);
        setStage(`Compressing ${file.name}… (${readMB} / ${totMB} MB)`, pct);
      },
    }));
    if (!isGzip) _etaRecord('compress', compEtaMs, performance.now() - compT0);
  } catch (e) {
    if (statusEl) statusEl.innerHTML = _errorHtml('Reading file', file.name, e);
    if (modeButtons) modeButtons.style.display = 'flex';
    return;
  }

  const tLoad0 = performance.now();
  try {
    await enterBlocking(`Parsing ${escHtml(file.name)}…`, compBarEnd, loadBarEnd, parseEtaMs);
    wasmSession = await _loadWithFallback(bytes, file, onLoadPhase, decompressedSize, 'wasm-load-label');
    wasmSession._fileName = file.name;
  } catch (e) {
    if (statusEl) { statusEl.innerHTML = _errorHtml('Loading', file.name, e); }
    if (modeButtons) modeButtons.style.display = 'flex';
    return;
  }
  const loadActual = performance.now() - tLoad0;
  _etaRecord('parse', parseEtaMs, loadActual);
  bytes = null;

  classNames = JSON.parse(wasmSession.class_names());

  // ── Analysis phase: dominators ────────────────────────────────────────────
  let instanceCount = estInst;
  try {
    const s = JSON.parse(wasmSession.stats());
    instanceCount = s.instance_count || estInst;
  } catch {}
  const domEtaMs = _etaPredict('dominator', instanceCount);

  // _ANAL_PHASE_CUM_FRACS: cumulative share of domEtaMs consumed after each phase fires.
  let analElapsedFrac = 0;
  const onAnalPhase = (phase, _frac) => {
    analElapsedFrac = _ANAL_PHASE_CUM_FRACS[phase] ?? analElapsedFrac;
    const remainMs = Math.max(0, Math.round(domEtaMs * (1 - analElapsedFrac)));
    updateBlocking(_analPhaseLabel(phase, remainMs, instanceCount), 95, remainMs);
  };

  await enterBlocking(
    `Computing dominators for ${_fmtCount(instanceCount)} objects…`,
    loadBarEnd, 95, domEtaMs);

  const tDom0 = performance.now();
  try {
    wasmSession.run_full_analysis_with_progress(onAnalPhase);
    hasRetained = true;
  } catch (e) {
    hasRetained = false;
    showToast(`Analysis failed — @retainedHeapSize unavailable: ${e}`, 'error', 6000);
  }
  _etaRecord('dominator', domEtaMs, performance.now() - tDom0);

  if (statusEl) statusEl.style.display = 'none';
  showWasmShell(file.name);
}


async function loadWasmSessionWithReport(file, opts = {}) {
  const msg = document.getElementById('report-message');
  const modeButtons = document.getElementById('mode-buttons');
  if (modeButtons) modeButtons.style.display = 'none';
  showScreen('report-screen');

  msg.innerHTML = `
    <div class="wasm-progress">
      <div class="wasm-progress-label" id="wasm-progress-label">Reading ${escHtml(file.name)}…</div>
      <div class="wasm-progress-bar-wrap">
        <div class="wasm-progress-bar" id="wasm-progress-bar"></div>
      </div>
    </div>`;

  const { setStage, crawlTo, enterBlocking, updateBlocking } = _makeProgress('wasm-progress-label', 'wasm-progress-bar');

  const fileMB = file.size / (1024 * 1024);

  // Peek at first 4 bytes to detect format
  const headerBuf2 = await file.slice(0, 4).arrayBuffer();
  const header2 = new Uint8Array(headerBuf2);
  const isGzipUpfront2 = header2[0] === 0x1f && header2[1] === 0x8b;
  const isZipUpfront2  = header2[0] === 0x50 && header2[1] === 0x4b;

  // For ZIP: peek CD for true uncompressed size so ETA is based on actual work.
  let effectiveMB2 = fileMB;
  if (isZipUpfront2) {
    const uncmp2 = await _peekZipUncmpSize(file);
    if (uncmp2 > 0) effectiveMB2 = uncmp2 / (1024 * 1024);
  }

  const compEtaMs  = isGzipUpfront2 ? 0 : Math.max(0, Math.round(_ETA_BASE.compMsPerMB * effectiveMB2 * (_etaFactors().compress ?? 1.0)));
  const estInst    = Math.round(effectiveMB2 * _ETA_BASE.instPerMB);
  const parseEtaMs = _etaPredict('parse', estInst);
  const totalLoadMs = compEtaMs + parseEtaMs;
  const loadBarEnd = 55;
  const compBarEnd2 = totalLoadMs > 0
    ? Math.round((compEtaMs / totalLoadMs) * loadBarEnd)
    : 0;

  let loadElapsedMs = 0;
  const onLoadPhase = (phase, _frac) => {
    const phaseMs = phase === 'compress'
      ? compEtaMs
      : Math.round(parseEtaMs * (_LOAD_PHASE_FRACS[phase] ?? 0));
    loadElapsedMs += phaseMs;
    const remainMs = Math.max(0, totalLoadMs - loadElapsedMs);
    updateBlocking(_loadPhaseLabel(phase, remainMs, estInst), loadBarEnd, remainMs);
  };

  // Detect format and produce gzip bytes for the WASM parser.
  let bytes;
  let decompressedSize = 0;
  try {
    const compT0 = performance.now();
    ({ bytes, decompressedSize } = await _prepareFileBytes(file, {
      compBarEnd: compBarEnd2,
      setStage,
      onProgress: (inputRead, total) => {
        const frac = total > 0 ? inputRead / total : 0;
        const pct = Math.round(frac * compBarEnd2);
        const readMB = (inputRead / 1048576).toFixed(0);
        const totMB  = (total    / 1048576).toFixed(0);
        setStage(`Compressing ${file.name}… (${readMB} / ${totMB} MB)`, pct);
      },
    }));
    if (!isGzipUpfront2) _etaRecord('compress', compEtaMs, performance.now() - compT0);
  } catch (e) {
    msg.innerHTML = _errorHtml('Reading file', file.name, e);
    return;
  }

  const tParse0 = performance.now();
  try {
    await enterBlocking(`Parsing ${escHtml(file.name)}…`, compBarEnd2, loadBarEnd, parseEtaMs);
    wasmSession = await _loadWithFallback(bytes, file, onLoadPhase, decompressedSize, 'wasm-progress-label');
  } catch (e) {
    msg.innerHTML = _errorHtml('Loading', file.name, e);
    return;
  }
  _etaRecord('parse', parseEtaMs, performance.now() - tParse0);
  bytes = null;

  let instanceCount = estInst;
  try {
    const s = JSON.parse(wasmSession.stats());
    instanceCount = s.instance_count || estInst;
  } catch {}
  const domEtaMs = _etaPredict('dominator', instanceCount);

  let analElapsedFrac2 = 0;
  const onAnalPhase = (phase, _frac) => {
    analElapsedFrac2 = _ANAL_PHASE_CUM_FRACS[phase] ?? analElapsedFrac2;
    const remainMs = Math.max(0, Math.round(domEtaMs * (1 - analElapsedFrac2)));
    updateBlocking(_analPhaseLabel(phase, remainMs, instanceCount), 90, remainMs);
  };

  await enterBlocking(`Computing dominators for ${_fmtCount(instanceCount)} objects…`, loadBarEnd, 90, domEtaMs);

  const tDom0 = performance.now();
  try {
    wasmSession.run_full_analysis_with_options_and_progress(
      !!(opts.findDuplicates), !!(opts.collections), onAnalPhase);
    _etaRecord('dominator', domEtaMs, performance.now() - tDom0);
    const reportHtml = wasmSession.get_report_html();
    setStage('Opening report…', 92);
    await new Promise(r => setTimeout(r, 16));
    hasRetained = true;
    classNames = JSON.parse(wasmSession.class_names());
    openReportTab(reportHtml);
    showWasmShell(file.name);
  } catch (e) {
    msg.innerHTML = _errorHtml('Analysis', file.name, e);
  }
}

// wasm32 linear memory is capped at 4 GiB, so a plain .hprof larger than this
// (buffer + parse indices) will OOM. gzip dumps decompress much larger, so the
// trigger threshold is lower. These are heuristics for the *pre-load* prompt;
// the actual OOM catch below is the real safety net.
const _WASM32_PLAIN_LIMIT = 3.2 * 1024 * 1024 * 1024;   // ~3.2 GiB plain .hprof
const _WASM32_GZIP_LIMIT  = 1.0 * 1024 * 1024 * 1024;   // ~1 GiB gzip (expands)

// Silently switch to the experimental wasm64 module, updating `statusLabel`
// (an element id or null) with a brief notice. Returns true on success.
async function _switchToWasm64Silent(statusLabel) {
  if (window._hprofOnWasm64 && window._hprofOnWasm64()) return true;
  if (statusLabel) {
    const el = document.getElementById(statusLabel);
    if (el) el.textContent = 'Dump is large — switching to experimental wasm64 build…';
  }
  try {
    await window._switchToWasm64();
    return true;
  } catch (e) {
    console.warn('[hprof-analyzer] wasm64 switch failed:', e);
    showToast(`Experimental wasm64 build unavailable: ${e.message || e}`, 'error', 6000);
    return false;
  }
}

// Load a heap dump, transparently switching to the experimental wasm64 module
// when the standard wasm32 build can't handle the file. Two triggers:
//   1. Pre-check: file clearly exceeds the wasm32 ceiling → prompt up front.
//   2. OOM catch: wasm32 load throws an OOM error → prompt, switch, retry on
//      the SAME in-scope bytes (the File is never re-picked).
// The wasm32 session is freed before switching; the wasm64 module has its own
// independent linear memory. `bytes` is the (gzip-compressed) buffer already
// prepared by the caller. `decompressedSize` is the known uncompressed size in
// bytes (from ZIP central directory), or 0 if unknown. `statusLabel` is an
// element id for wasm64 switch notices (may be null). Returns the loaded
// HprofSession, or throws.
async function _loadWithFallback(bytes, file, onLoadPhase, decompressedSize = 0, statusLabel = null) {
  const onWasm64 = () => window._hprofOnWasm64 && window._hprofOnWasm64();

  // Pre-check: obviously-too-big files silently switch to wasm64 before loading.
  // For ZIP files we have the authoritative uncompressed size (decompressedSize);
  // for plain/gzip we fall back to file.size as a heuristic.
  if (!onWasm64()) {
    const isGzip = bytes.length >= 2 && bytes[0] === 0x1f && bytes[1] === 0x8b;
    let overLimit;
    if (decompressedSize > 0) {
      // Known uncompressed size (from ZIP central directory): treat same as a
      // plain .hprof of that size — native peak RSS is ~1× the dump size so
      // the 3.2 GiB threshold is appropriate. The OOM catch is the safety net.
      overLimit = decompressedSize > _WASM32_PLAIN_LIMIT;
    } else {
      overLimit = isGzip
        ? file.size > _WASM32_GZIP_LIMIT
        : file.size > _WASM32_PLAIN_LIMIT;
    }
    if (overLimit) await _switchToWasm64Silent(statusLabel);
  }

  // Free any prior wasm32 session before loading (frees its linear memory).
  if (wasmSession) { wasmSession.free(); wasmSession = null; }

  try {
    return activeHprof.load_with_progress(bytes, file.name, onLoadPhase);
  } catch (e) {
    // OOM fallback: silently retry on wasm64 with the same bytes.
    if (_isOomError(e) && !onWasm64()) {
      const switched = await _switchToWasm64Silent(statusLabel);
      if (switched) {
        return activeHprof.load_with_progress(bytes, file.name, onLoadPhase);
      }
    }
    throw e;
  }
}

// Detect out-of-memory conditions from WASM/browser errors.
// Note: Rust panics produce "unreachable" RuntimeErrors — those are NOT OOM,
// they are logic errors. Only treat allocation failures as OOM.
function _isOomError(e) {
  const s = String(e).toLowerCase();
  return s.includes('out of memory') || s.includes('allocation failed') ||
         s.includes('memory access out of bounds') ||
         s.includes('rangeerror') || (e instanceof RangeError);
}

// Return an HTML string for load/analysis errors, with OOM-specific guidance.
function _errorHtml(action, fileName, e) {
  const raw = String(e);
  const readmeUrl = 'https://github.com/parttimenerd/hprof-analyzer#quick-start';
  const onWasm64 = window._hprofOnWasm64 && window._hprofOnWasm64();
  if (onWasm64 && (raw.toLowerCase().includes('unreachable') ||
                   raw.toLowerCase().includes('runtimeerror'))) {
    // Rust panic inside wasm64 — not an OOM, a hard abort.
    return `<strong>Experimental wasm64 build failed</strong> — the memory64 build ` +
           `aborted while processing <em>${escHtml(fileName)}</em>.<br>` +
           `Use the <strong>CLI</strong> instead — it handles dumps of any size:<br>` +
           `<code>hprof-analyzer ${escHtml(fileName)}</code><br>` +
           `<a href="${readmeUrl}" target="_blank" rel="noopener">Install &amp; quick-start guide →</a>`;
  }
  if (_isOomError(e)) {
    return `<strong>Out of memory</strong> — <em>${escHtml(fileName)}</em> is too large for ` +
           `the browser's WASM heap.<br>` +
           `Use the <strong>CLI</strong> — it has no memory cap and handles dumps of any size:<br>` +
           `<code>hprof-analyzer ${escHtml(fileName)}</code><br>` +
           `<a href="${readmeUrl}" target="_blank" rel="noopener">Install &amp; quick-start guide →</a>`;
  }
  return `${escHtml(action)} failed: ${escHtml(raw)}`;
}

// Human-readable label for a load sub-phase.
function _loadPhaseLabel(phase, remainMs, estInst) {
  const eta = remainMs > 0 ? ` (${_fmtEta(remainMs)})` : '';
  switch (phase) {
    case 'start':   return `Parsing heap dump${eta}…`;
    case 'pass1_a': return `Pass 1 — reading records & building class index${eta}…`;
    case 'pass1_b': return `Pass 1 (second scan)${eta}…`;
    case 'pass2':   return `Pass 2 — scanning heap & building reference graph${eta}…`;
    case 'compress': return `Compressing heap data for storage${eta}…`;
    default:        return `Loading${eta}…`;
  }
}

// Human-readable label for an analysis sub-phase.
function _analPhaseLabel(phase, remainMs, instanceCount) {
  const eta = remainMs > 0 ? ` (${_fmtEta(remainMs)})` : '';
  const n = _fmtCount(instanceCount);
  switch (phase) {
    case 'pass1':      return `Pass 1 — re-reading class index${eta}…`;
    case 'pass2':      return `Pass 2 — building reference graph${eta}…`;
    case 'rpo':        return `Computing reachability for ${n} objects${eta}…`;
    case 'inbound':    return `Building inbound reference index${eta}…`;
    case 'dominators': return `Computing dominator tree for ${n} objects${eta}…`;
    case 'retained':   return `Computing retained sizes${eta}…`;
    default:           return `Analyzing${eta}…`;
  }
}

function escHtml(s) {
  return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

// Open the full React-based HTML report in a new browser tab via a Blob URL.
function openReportTab(html) {
  const blob = new Blob([html], { type: 'text/html' });
  const url = URL.createObjectURL(blob);
  window.open(url, '_blank');
  // Revoke after a short delay so the new tab has time to read the blob.
  setTimeout(() => URL.revokeObjectURL(url), 60000);
}

function renderWasmReport(report, fileName) {
  const container = document.getElementById('report-container');
  // Remove all children except report-message, then clear its text
  while (container.lastChild && container.lastChild.id !== 'report-message') {
    container.removeChild(container.lastChild);
  }
  while (container.firstChild && container.firstChild.id !== 'report-message') {
    container.removeChild(container.firstChild);
  }
  const msg = document.getElementById('report-message');
  if (msg) msg.textContent = '';

  const ov      = report.overview     || {};
  const leaks   = report.leaks        || {};
  const suspects= leaks.suspects      || [];
  const top     = report.top          || {};
  const hist    = ov.histogram        || [];
  const triage  = report.triage       || [];
  const threads = (report.threads && report.threads.threads) || [];
  const ws      = report.waste_summary;
  const bigObjs = top.biggest_objects || [];
  const bigCls  = top.biggest_classes || [];

  // fmt helpers
  const n = v => (v || 0).toLocaleString('en-US');
  const pct = (a, b) => b > 0 ? (a / b * 100).toFixed(1) + '%' : '—';
  const fmtTs = ms => ms ? new Date(ms).toISOString().replace('T', ' ').replace(/\.\d+Z$/, ' UTC') : '—';

  // severity badge colors
  const sevColor = { critical: '#ff6b6b', warning: '#ffcc44', info: '#7ab4ff' };

  // section helper — tracks registered sections for nav
  const navLinks = [];
  const sec = (id, title, subtitle, body) => {
    navLinks.push({ id, title });
    return `<div class="rpt-section" id="${id}">` +
      `<h2 class="rpt-title">${escHtml(title)}</h2>` +
      (subtitle ? `<p class="rpt-subtitle-text">${subtitle}</p>` : '') +
      body + `</div>`;
  };

  // inline code with backtick stripping (matches CLI detail strings)
  const inlineCode = s => escHtml(s)
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/_([^_]+)_/g, '<em>$1</em>');

  // bar cell helper
  const bar = (v, max) => {
    const w = max > 0 ? Math.round(v / max * 100) : 0;
    return `<td class="num bar-cell"><span class="bar-bg"><span class="bar-fill" style="width:${w}%"></span></span></td>`;
  };

  // sortable table header helper — wraps th content with data-col index
  const sortTh = (label, col, cls) =>
    `<th class="rpt-sort-hdr${cls ? ' ' + cls : ''}" data-col="${col}">${escHtml(label)}</th>`;

  const parts = [];

  // ── Page header ─────────────────────────────────────────────────────────────
  parts.push(`<h1 class="rpt-page-title">Heap Dump Analysis: <code>${escHtml(fileName)}</code></h1>`);
  const genTime = fmtTs(ov.dump_creation);
  parts.push(`<p class="rpt-page-sub">Generated by hprof-analyzer — ${genTime}</p>`);
  parts.push(`<p class="rpt-page-sub">All sizes are binary (1 KB = 1024 bytes, 1 MB = 1024 KB, and so on).</p>`);

  // ── "Start here" intro card ─────────────────────────────────────────────────
  parts.push(`<div class="rpt-intro-card">` +
    `<strong>New to heap analysis?</strong> ` +
    `Start with <a href="#memory-triage">Memory Triage</a> for an instant diagnosis, then check ` +
    `<a href="#leak-suspects">Leak Suspects</a> to find the biggest memory holders. ` +
    `<a href="#histogram">Histogram</a> shows every class ranked by retained heap. ` +
    `<em>Retained heap</em> = memory freed when that object is collected (its own size plus everything it exclusively holds). ` +
    `<a href="#glossary" class="rpt-intro-glossary">Glossary →</a>` +
    `</div>`);

  // ── KPI strip ──────────────────────────────────────────────────────────────
  const topSuspect = suspects[0];
  const topShare = topSuspect ? pct(topSuspect.retained, leaks.total_shallow || ov.total_shallow) : '—';
  parts.push(`<div class="rpt-kpi-grid">`);
  const kpis = [
    [fmtBytes(ov.total_shallow || 0), 'Total reachable heap',
      'Sum of all reachable object shallow sizes — the live heap the GC sees.'],
    [n(ov.total_objects), 'Objects',
      'Number of reachable heap objects (instances + arrays + class objects).'],
    [n(suspects.length), 'Leak suspects',
      'Objects that dominate unusually large retained heaps relative to their class — high-confidence leak candidates.'],
    [topShare, 'Top suspect share',
      'What fraction of the total heap the single biggest leak suspect retains.'],
    [topSuspect ? `<code title="${escHtml(topSuspect.pretty_class||'')}">${escHtml(topSuspect.pretty_class||'—')}</code>` : '—', 'Dominant retainer',
      'The class holding the largest retained heap — the most likely root cause of high memory usage.'],
    [n(ov.gc_roots), 'GC roots',
      'Objects the JVM keeps alive unconditionally (thread stacks, static fields, JNI handles). Every live object traces back to one.'],
  ];
  kpis.forEach(([v, l, tip]) => parts.push(
    `<div class="rpt-kpi" title="${escHtml(tip)}"><div class="rpt-kpi-value">${v}</div><div class="rpt-kpi-label">${escHtml(l)}</div></div>`
  ));
  parts.push(`</div>`);

  // ── Memory Triage ───────────────────────────────────────────────────────────
  if (triage.length > 0) {
    let triageHtml = `<ul class="rpt-triage-list">`;
    triage.forEach(t => {
      const color = sevColor[t.severity] || '#7ab4ff';
      const anchor = t.anchor ? ` See <a href="#${escHtml(t.anchor)}">${escHtml(t.anchor_label||t.anchor)}</a>.` : '';
      triageHtml += `<li><span class="rpt-sev-dot" style="background:${color}"></span>` +
        `<strong>${escHtml(t.title)}:</strong> ${inlineCode(t.detail)}${anchor}</li>`;
    });
    triageHtml += `</ul>`;
    parts.push(sec('memory-triage', 'Memory Triage',
      'Automatic diagnosis: the most actionable signals about where memory is going. Start here.', triageHtml));
  }

  // ── Waste Summary ──────────────────────────────────────────────────────────
  if (ws && ws.total_bytes > 0) {
    const maxWaste = Math.max(...ws.sources.map(s => s.bytes));
    let wHtml = `<p class="rpt-subtitle-text">Approximately <strong>${fmtBytes(ws.total_bytes)}</strong> looks reclaimable across the sources below.</p>`;
    wHtml += `<table class="rpt-table rpt-wide rpt-sortable"><thead><tr>` +
      `${sortTh('Source', 0)}${sortTh('Reclaimable', 1, 'num')}<th></th>` +
      `</tr></thead><tbody>`;
    ws.sources.forEach(s => {
      const link = s.anchor ? `<a href="#${escHtml(s.anchor)}">${escHtml(s.label)}</a>` : escHtml(s.label);
      wHtml += `<tr><td>${link}</td><td class="num" data-val="${s.bytes}">${fmtBytes(s.bytes)}</td>${bar(s.bytes, maxWaste)}</tr>`;
    });
    wHtml += `</tbody></table>`;
    parts.push(sec('waste-summary', 'Waste Summary',
      'Estimated reclaimable memory — empty collection backing arrays, duplicate strings, and similar overhead.', wHtml));
  }

  // ── System Overview ──────────────────────────────────────────────────────────
  {
    let oHtml = `<table class="rpt-table"><tbody>`;
    const rows2 = [
      ['File', escHtml(ov.file_path || fileName)],
      ['File size', fmtBytes(ov.file_size || 0)],
      ['Dump created', fmtTs(ov.dump_creation)],
      ['Total objects', n(ov.total_objects)],
      ['Total shallow heap', fmtBytes(ov.total_shallow || 0)],
      ['Classes', n(ov.classes_loaded)],
      ['Class loaders', n(ov.classloaders_loaded)],
      ['GC roots', n(ov.gc_roots)],
      ['Identifier size', `${ov.identifier_size_bits || '?'}-bit`],
      ['Compressed OOPs', ov.compressed_oops === true ? 'yes' : ov.compressed_oops === false ? 'no' : '—'],
    ];
    if (ov.jvm_version) rows2.push(['JVM', escHtml(ov.jvm_version)]);
    if (ov.unreachable_count) rows2.push(['Unreachable objects', `${n(ov.unreachable_count)} (${fmtBytes(ov.unreachable_shallow || 0)} shallow)`]);
    rows2.forEach(([k, v]) => oHtml += `<tr><th>${escHtml(k)}</th><td>${v}</td></tr>`);
    oHtml += `</tbody></table>`;

    // System properties
    const sp = ov.system_properties;
    if (sp && typeof sp === 'object' && Object.keys(sp).length > 0) {
      oHtml += `<h3 class="rpt-subtitle" style="margin-top:16px">System Properties</h3>`;
      oHtml += `<table class="rpt-table rpt-wide"><tbody>`;
      Object.entries(sp).forEach(([k, v]) =>
        oHtml += `<tr><td class="cls">${escHtml(k)}</td><td>${escHtml(String(v))}</td></tr>`
      );
      oHtml += `</tbody></table>`;
    }
    parts.push(sec('system-overview', 'System Overview',
      'Dump metadata, heap totals, GC root breakdown, and JVM system properties.', oHtml));
  }

  // ── Histogram (by retained) ──────────────────────────────────────────────────
  if (hist.length > 0) {
    const histRows = hist.filter(r => (r.retained || 0) > 0 || (r.shallow || 0) > 0);
    const maxR = Math.max(...histRows.map(r => r.retained || 0));
    let hHtml = `<table class="rpt-table rpt-wide rpt-sortable">`;
    hHtml += `<thead><tr>` +
      `${sortTh('Class', 0)}${sortTh('Instances', 1, 'num')}${sortTh('Shallow', 2, 'num')}${sortTh('Retained', 3, 'num')}<th></th>` +
      `</tr></thead><tbody>`;
    histRows.forEach(row => {
      hHtml += `<tr><td class="cls">${escHtml(row.pretty_class||'')}</td>` +
        `<td class="num" data-val="${row.instances||0}">${n(row.instances)}</td>` +
        `<td class="num" data-val="${row.shallow||0}">${fmtBytes(row.shallow||0)}</td>` +
        `<td class="num" data-val="${row.retained||0}">${fmtBytes(row.retained||0)}</td>` +
        `${bar(row.retained||0, maxR)}</tr>`;
    });
    hHtml += `</tbody></table>`;
    parts.push(sec('histogram', 'Histogram',
      `Every class ranked by retained heap — how much memory is freed when all instances of that class become collectible. ` +
      `<strong>Retained</strong> includes everything a class exclusively holds; <strong>Shallow</strong> is the object headers only. ` +
      `Top ${histRows.length} classes shown. Click a column header to re-sort.`, hHtml));
  }

  // ── Leak Suspects ───────────────────────────────────────────────────────────
  if (suspects.length > 0) {
    let lHtml = '';
    suspects.forEach((s, idx) => {
      lHtml += `<div class="rpt-suspect-card">`;
      lHtml += `<h3 class="rpt-suspect-title">${idx + 1}. ${escHtml(s.pretty_class||'?')}</h3>`;
      lHtml += `<table class="rpt-table"><tbody>`;
      lHtml += `<tr><th>Instances</th><td>${n(s.instance_count)}</td></tr>`;
      lHtml += `<tr><th>Shallow</th><td>${fmtBytes(s.shallow||0)}</td></tr>`;
      lHtml += `<tr><th>Retained</th><td>${fmtBytes(s.retained||0)}</td></tr>`;
      if (s.root_type_label) lHtml += `<tr><th>Root type</th><td>${escHtml(s.root_type_label)}</td></tr>`;
      if (s.accumulation_class) lHtml += `<tr><th>Accumulation point</th><td class="cls">${escHtml(s.accumulation_class)}</td></tr>`;
      lHtml += `</tbody></table>`;

      // Dominator path
      if (s.path && s.path.length > 0) {
        lHtml += `<h4 class="rpt-suspect-sub">Dominator Path</h4><ol class="rpt-path-list">`;
        s.path.forEach(step => {
          lHtml += `<li><code>${escHtml(step.display_class||'?')}</code> — retained ${fmtBytes(step.retained||0)}</li>`;
        });
        lHtml += `</ol>`;
      }

      // Dominated by class (top dominated classes)
      if (s.dominated_by_class && s.dominated_by_class.length > 0) {
        lHtml += `<h4 class="rpt-suspect-sub">Dominated Objects by Class</h4>`;
        lHtml += `<table class="rpt-table rpt-sortable"><thead><tr>` +
          `${sortTh('Class', 0)}${sortTh('Instances', 1, 'num')}${sortTh('Retained', 2, 'num')}` +
          `</tr></thead><tbody>`;
        const maxDom = Math.max(...s.dominated_by_class.map(r => r.retained||0));
        s.dominated_by_class.filter(r => (r.retained||0) > 0 || (r.instances||0) > 0).slice(0, 15).forEach(row => {
          lHtml += `<tr><td class="cls">${escHtml(row.pretty_class||'')}</td>` +
            `<td class="num" data-val="${row.instances||0}">${n(row.instances)}</td>` +
            `<td class="num" data-val="${row.retained||0}">${fmtBytes(row.retained||0)}</td></tr>`;
        });
        lHtml += `</tbody></table>`;
      }
      lHtml += `</div>`;
    });
    parts.push(sec('leak-suspects', 'Leak Suspects',
      `Objects that dominate an unusually large share of the heap — the most likely root causes of an OutOfMemoryError. ` +
      `Each card shows the <strong>dominator path</strong> (the chain of objects keeping this in memory) and ` +
      `the <strong>dominated objects</strong> (what it holds). Fix the top suspect first. ${suspects.length} suspect(s) identified.`,
      lHtml));
  }

  // ── Top Consumers ───────────────────────────────────────────────────────────
  {
    let tcHtml = '';

    // Biggest objects
    if (bigObjs.length > 0) {
      const totalShallow = ov.total_shallow || 1;
      const maxO = Math.max(...bigObjs.map(o => o.retained||0));
      const bigObjsFiltered = bigObjs.filter(o => (o.retained||0) > 0);
      if (bigObjsFiltered.length > 0) {
        tcHtml += `<h3 class="rpt-subtitle">Biggest Objects</h3>`;
        tcHtml += `<table class="rpt-table rpt-wide rpt-sortable"><thead><tr>` +
          `${sortTh('Class', 0)}${sortTh('Retained', 1, 'num')}${sortTh('%', 2, 'num')}<th></th>` +
          `</tr></thead><tbody>`;
        bigObjsFiltered.forEach(o => {
          tcHtml += `<tr><td class="cls">${escHtml(o.display_class||'?')}</td>` +
            `<td class="num" data-val="${o.retained||0}">${fmtBytes(o.retained||0)}</td>` +
            `<td class="num" data-val="${o.retained||0}">${pct(o.retained||0, totalShallow)}</td>` +
            `${bar(o.retained||0, maxO)}</tr>`;
        });
        tcHtml += `</tbody></table>`;
      }
    }

    // Biggest classes
    if (bigCls.length > 0) {
      const maxC = Math.max(...bigCls.map(c => c.retained||0));
      const bigClsFiltered = bigCls.filter(c => (c.retained||0) > 0);
      if (bigClsFiltered.length > 0) {
        tcHtml += `<h3 class="rpt-subtitle" style="margin-top:16px">Biggest Classes by Retained</h3>`;
        tcHtml += `<table class="rpt-table rpt-wide rpt-sortable"><thead><tr>` +
          `${sortTh('Class', 0)}${sortTh('Instances', 1, 'num')}${sortTh('Retained', 2, 'num')}<th></th>` +
          `</tr></thead><tbody>`;
        bigClsFiltered.forEach(c => {
          tcHtml += `<tr><td class="cls">${escHtml(c.pretty_class||'')}</td>` +
            `<td class="num" data-val="${c.instances||0}">${n(c.instances)}</td>` +
            `<td class="num" data-val="${c.retained||0}">${fmtBytes(c.retained||0)}</td>` +
            `${bar(c.retained||0, maxC)}</tr>`;
        });
        tcHtml += `</tbody></table>`;
      }
    }

    // Package tree (top-level)
    const bp = top.biggest_packages;
    if (bp && bp.children && bp.children.length > 0) {
      tcHtml += `<h3 class="rpt-subtitle" style="margin-top:16px">Biggest Packages by Retained</h3>`;
      tcHtml += `<table class="rpt-table rpt-wide rpt-sortable"><thead><tr>` +
        `${sortTh('Package', 0)}${sortTh('Top dominators', 1, 'num')}${sortTh('Retained', 2, 'num')}` +
        `</tr></thead><tbody>`;
      const maxP = Math.max(...bp.children.map(c => c.retained_heap||0));
      bp.children.filter(c => (c.retained_heap||0) > 0).forEach(c => {
        tcHtml += `<tr><td class="cls">${escHtml(c.name||'(root)')}</td>` +
          `<td class="num" data-val="${c.top_dominator_count||0}">${n(c.top_dominator_count)}</td>` +
          `<td class="num" data-val="${c.retained_heap||0}">${fmtBytes(c.retained_heap||0)}</td></tr>`;
      });
      tcHtml += `</tbody></table>`;
    }

    if (tcHtml) parts.push(sec('top-consumers', 'Top Consumers',
      'The individual objects and classes that retain the most heap. ' +
      '"Biggest Objects" are single instances; "Biggest Classes" aggregate all instances of a type; ' +
      '"Biggest Packages" roll up retained heap by Java package.',
      tcHtml));
  }

  // ── Threads ──────────────────────────────────────────────────────────────────
  if (threads.length > 0) {
    const threadsFiltered = threads.filter(t => (t.shallow||0) > 0 || (t.retained||0) > 0 || t.name);
    let tHtml = `<table class="rpt-table rpt-wide rpt-sortable"><thead><tr>` +
      `${sortTh('Name', 0)}${sortTh('State', 1)}${sortTh('Shallow', 2, 'num')}${sortTh('Retained', 3, 'num')}${sortTh('Daemon', 4)}` +
      `</tr></thead><tbody>`;
    threadsFiltered.forEach(t => {
      const state = Array.isArray(t.thread_state) ? t.thread_state.join(', ') : (t.thread_state || '—');
      tHtml += `<tr>` +
        `<td>${escHtml(t.name || '?')}</td>` +
        `<td>${escHtml(state)}</td>` +
        `<td class="num" data-val="${t.shallow||0}">${fmtBytes(t.shallow || 0)}</td>` +
        `<td class="num" data-val="${t.retained||0}">${fmtBytes(t.retained || 0)}</td>` +
        `<td>${t.is_daemon ? 'daemon' : ''}</td></tr>`;

      // significant frames
      if (t.significant_frames && t.significant_frames.length > 0) {
        tHtml += `<tr><td colspan="5" class="rpt-thread-frames">`;
        tHtml += t.significant_frames.slice(0, 5).map(f => {
          const frameStr = typeof f === 'string' ? f : (f && f.frame) ? f.frame : String(f);
          return `<code>${escHtml(frameStr)}</code>`;
        }).join('<br>');
        tHtml += `</td></tr>`;
      }
    });
    tHtml += `</tbody></table>`;
    parts.push(sec('threads', 'Threads',
      `All JVM threads at dump time, ranked by retained heap. A thread with high retained size holds live objects in its local variables or call stack. ` +
      `${threadsFiltered.length} thread(s).`, tHtml));
  }

  // Build nav bar from registered sections
  const navHtml = navLinks.length > 0
    ? `<nav class="rpt-nav" id="rpt-nav-bar">${navLinks.map(({ id, title }) =>
        `<a class="rpt-nav-link" href="#${id}" data-section="${id}">${escHtml(title)}</a>`).join('')}</nav>`
    : '';

  container.insertAdjacentHTML('beforeend', navHtml + parts.join(''));

  // Wire sortable tables: click a .rpt-sort-hdr to sort by that column
  container.querySelectorAll('table.rpt-sortable').forEach(tbl => {
    tbl.querySelectorAll('th.rpt-sort-hdr').forEach(th => {
      th.addEventListener('click', () => {
        const col = parseInt(th.dataset.col, 10);
        const tbody = tbl.querySelector('tbody');
        if (!tbody) return;
        // Determine current sort direction (toggle, default desc first)
        const wasDesc = th.classList.contains('rpt-sort-desc');
        // Clear all sort markers in this table
        tbl.querySelectorAll('th.rpt-sort-hdr').forEach(h => {
          h.classList.remove('rpt-sort-asc', 'rpt-sort-desc');
        });
        const desc = !wasDesc;
        th.classList.add(desc ? 'rpt-sort-desc' : 'rpt-sort-asc');
        // Collect sortable rows (skip colspan rows like thread frames)
        const rows = Array.from(tbody.querySelectorAll('tr')).filter(r =>
          !r.querySelector('td[colspan]')
        );
        // Keep "companion" rows (colspan) paired with their preceding row
        const allRows = Array.from(tbody.querySelectorAll('tr'));
        // Build paired groups: [mainRow, ...companionRows]
        const groups = [];
        for (let i = 0; i < allRows.length; i++) {
          if (!allRows[i].querySelector('td[colspan]')) {
            groups.push([allRows[i]]);
          } else if (groups.length > 0) {
            groups[groups.length - 1].push(allRows[i]);
          }
        }
        groups.sort((a, b) => {
          const aCell = a[0].cells[col];
          const bCell = b[0].cells[col];
          const aRaw = aCell ? (aCell.dataset.val !== undefined ? parseFloat(aCell.dataset.val) : aCell.textContent.trim()) : '';
          const bRaw = bCell ? (bCell.dataset.val !== undefined ? parseFloat(bCell.dataset.val) : bCell.textContent.trim()) : '';
          const aNum = typeof aRaw === 'number' ? aRaw : NaN;
          const bNum = typeof bRaw === 'number' ? bRaw : NaN;
          let cmp;
          if (!isNaN(aNum) && !isNaN(bNum)) {
            cmp = aNum - bNum;
          } else {
            cmp = String(aRaw).localeCompare(String(bRaw));
          }
          return desc ? -cmp : cmp;
        });
        groups.forEach(g => g.forEach(r => tbody.appendChild(r)));
      });
    });
  });

  // Highlight active section in the sticky nav as the user scrolls
  const navBar = container.querySelector('#rpt-nav-bar');
  if (navBar && 'IntersectionObserver' in window) {
    const links = Array.from(navBar.querySelectorAll('.rpt-nav-link[data-section]'));
    const sectionEls = links.map(a => document.getElementById(a.dataset.section)).filter(Boolean);
    let activeId = null;
    const obs = new IntersectionObserver(entries => {
      entries.forEach(e => {
        const id = e.target.id;
        if (e.isIntersecting) {
          activeId = id;
          links.forEach(a => a.classList.toggle('rpt-nav-active', a.dataset.section === id));
        }
      });
    }, { rootMargin: '-10% 0px -70% 0px', threshold: 0 });
    sectionEls.forEach(el => obs.observe(el));
  }
}
function showWasmShell(name) {
  serverUrl = null;
  showScreen('shell-screen');
  const badge = document.getElementById('server-badge');
  if (badge) { badge.textContent = '◉ WASM'; badge.style.color = '#7cb8ff'; }
  document.getElementById('server-url-display').textContent = name;
  document.getElementById('btn-disconnect').textContent = '↩ New file';
  buildSidebar(hasRetained);
  startTerminal();
}

// ── Report screen ─────────────────────────────────────────────────────────────
let prevScreen = null;  // tracks screen shown before report-screen

// Show/hide the report-topbar action buttons based on whether a WASM session
// with a cached report is active.
function updateReportTopbar() {
  const hasReport = !!(wasmSession && wasmSession.get_report_html && (() => {
    try { return !!wasmSession.get_report_html(); } catch { return false; }
  })());
  const dlBtn = document.getElementById('btn-download-report');
  const openBtn = document.getElementById('btn-open-report');
  if (dlBtn) dlBtn.style.display = hasReport ? '' : 'none';
  if (openBtn) openBtn.style.display = hasReport ? '' : 'none';
}

function showReport(report, fileName) {
  prevScreen = wasmSession ? 'shell-screen' : (serverUrl ? 'shell-screen' : null);
  renderWasmReport(report, fileName);
  showScreen('report-screen');
  updateReportTopbar();
}

document.getElementById('btn-new-file').addEventListener('click', () => {
  if (wasmSession) { wasmSession.free(); wasmSession = null; }
  serverUrl = null; hasRetained = false; classNames = [];
  prevScreen = null;
  showScreen('upload-screen');
});

document.getElementById('btn-to-shell').addEventListener('click', () => {
  if (prevScreen === 'shell-screen' && wasmSession) {
    showWasmShell(document.getElementById('server-url-display').textContent || 'heap.hprof');
  } else if (prevScreen === 'shell-screen' && serverUrl) {
    showScreen('shell-screen');
  } else if (wasmSession) {
    showWasmShell(document.getElementById('server-url-display').textContent || 'heap.hprof');
  } else {
    showScreen('connect-screen');
    if (wasmReady) populateOfflineList();
  }
});

// ── Open full React report in new tab (report-screen topbar) ─────────────────
document.getElementById('btn-open-report').addEventListener('click', () => {
  if (!wasmSession) { showToast('No active session', 'info'); return; }
  try {
    const html = wasmSession.get_report_html();
    if (html) {
      openReportTab(html);
    } else {
      showToast('Report not yet generated — wait for analysis to complete', 'info');
    }
  } catch (e) {
    showToast(`Report failed: ${e}`, 'error');
  }
});

// ── Download self-contained HTML report file (report-screen topbar) ──────────
document.getElementById('btn-download-report').addEventListener('click', () => {
  if (!wasmSession) { showToast('No active session', 'info'); return; }
  try {
    const html = wasmSession.get_report_html();
    if (!html) { showToast('Report not yet generated — wait for analysis to complete', 'info'); return; }
    const fileName = (wasmSession._fileName || 'heap').replace(/\.hprof(\.gz)?$/i, '') + '-report.html';
    const blob = new Blob([html], { type: 'text/html' });
    const a = document.createElement('a');
    a.href = URL.createObjectURL(blob);
    a.download = fileName;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    setTimeout(() => URL.revokeObjectURL(a.href), 60000);
  } catch (e) {
    showToast(`Download failed: ${e}`, 'error');
  }
});
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
      return '0x' + v.toString(16).toUpperCase().padStart(16, '0');
    }
    // Byte-size columns shown as human-readable (unless bytesRaw)
    if (!settings.bytesRaw && v >= 0 && colName && /bytes$|_size$|heap_size$/i.test(colName)) {
      return fmtBytes(v);
    }
    return v.toLocaleString('en-US');
  }
  if (kind === 'float') {
    if (typeof v !== 'number') return String(v);
    // Match CLI: 6 decimal places, trailing zeros stripped
    let s = v.toFixed(6);
    s = s.replace(/\.?0+$/, '');
    return s || '0';
  }
  if (kind === 'str') return String(v);
  if (kind === 'obj_ref') {
    const cls = v && v.class ? v.class : '?';
    const idx = v && v.index !== undefined ? v.index : '?';
    return `${cls}@${idx}`;
  }
  // fallback
  return JSON.stringify(cell);
}

// Pad/truncate a string to a fixed column width (right-align if numeric)
function padTo(s, w, rightAlign) {
  if (s.length > w) return s.slice(0, w - 1) + '…';
  return rightAlign ? s.padStart(w) : s.padEnd(w);
}

// ── Global keyboard shortcuts ─────────────────────────────────────────────────
// Ctrl+Shift+R (or Cmd+Shift+R on Mac) — toggle between shell and report screens.
document.addEventListener('keydown', e => {
  if ((e.ctrlKey || e.metaKey) && e.shiftKey && e.key === 'R') {
    e.preventDefault();
    const shellVisible  = document.getElementById('shell-screen').style.display  !== 'none';
    const reportVisible = document.getElementById('report-screen').style.display !== 'none';
    if (shellVisible) {
      showScreen('report-screen');
      updateReportTopbar();
    } else if (reportVisible) {
      document.getElementById('btn-to-shell').click();
    }
  }
});

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
  const badge = document.getElementById('server-badge');
  if (badge) { badge.textContent = '● Connected'; badge.style.color = ''; }
  document.getElementById('btn-disconnect').textContent = '✕ Disconnect';
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
  if (wasmSession) { wasmSession.free(); wasmSession = null; }
  serverUrl = null;
  hasRetained = false;
  classNames = [];
  if (pollTimer) { clearTimeout(pollTimer); pollTimer = null; }
  if (keepaliveTimer) { clearInterval(keepaliveTimer); keepaliveTimer = null; }
  if (term) { term.dispose(); term = null; }
  document.getElementById('named-query-list').innerHTML = '';
  document.getElementById('connect-status').textContent = '';
  document.getElementById('connect-status').className = '';
  document.getElementById('btn-disconnect').textContent = '✕ Disconnect';
  showScreen('upload-screen');
});

// ── Dashboard button ──────────────────────────────────────────────────────────
document.getElementById('btn-dashboard').addEventListener('click', () => {
  if (typeof openDashboard === 'function') openDashboard();
});

// ── Show Report button ────────────────────────────────────────────────────────
document.getElementById('btn-show-report').addEventListener('click', async () => {
  if (wasmSession) {
    try {
      const html = wasmSession.get_report_html();
      if (html) {
        openReportTab(html);
      } else {
        showToast('Report not yet generated — wait for analysis to complete', 'info');
      }
    } catch (e) {
      showToast(`Report failed: ${e}`, 'error');
    }
    return;
  }
  // Server mode: fetch /report
  try {
    const rr = await fetch(serverUrl + '/report');
    if (rr.ok) {
      const reportJson = await rr.text();
      showReport(JSON.parse(reportJson), serverUrl || '');
    } else {
      showToast('Report not ready', 'info');
    }
  } catch (e) {
    showToast(`Error: ${e.message}`, 'error');
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
    if (data.status === 'ready') {
      hasRetained = true;
      buildSidebar(true);
      if (term) term.writeln('\r\n\x1b[32m[Analysis complete — @retainedHeapSize queries now available]\x1b[0m');
      try {
        const rr = await fetch(serverUrl + '/report');
        if (rr.ok) {
          const reportJson = await rr.text();
          showReport(JSON.parse(reportJson), serverUrl || '');
        }
      } catch (_) {}
    } else if (data.status === 'analyzing') {
      pollTimer = setTimeout(pollAnalysisStatus, 2000);
    } else if (data.status === 'failed') {
      showToast(`Server analysis failed: ${data.error || ''}`, 'error');
    } else {
      // not_started
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
// OQL syntax highlighter — returns an HTML string safe for innerHTML.
function _oqlHighlight(oql) {
  const _KW_SET = new Set([
    'SELECT','FROM','WHERE','AS','INSTANCEOF','AND','OR','NOT','NULL','TRUE','FALSE',
    'UNION','LIMIT','OFFSET','ORDER','GROUP','ASC','DESC','DISTINCT','ALL',
    'HAVING','BETWEEN','IN','IS','EXISTS',
    'CASE','WHEN','THEN','ELSE','END',
    'EXCEPT','INTERSECT','BY',
    'COUNT','SUM','MIN','MAX','AVG','PERCENTILE','MEDIAN','STDDEV',
  ]);

  // Tokenise by scanning left-to-right
  const tokens = [];
  let i = 0;
  while (i < oql.length) {
    // line comment: -- to end of line
    if (oql[i] === '-' && oql[i+1] === '-') {
      let j = i + 2;
      while (j < oql.length && oql[j] !== '\n') j++;
      tokens.push({ cls: 'oql-comment', text: oql.slice(i, j) });
      i = j; continue;
    }
    // warn block [...]
    if (oql[i] === '[') {
      const end = oql.indexOf(']', i);
      if (end !== -1) {
        tokens.push({ cls: 'oql-warn', text: oql.slice(i, end + 1) });
        i = end + 1; continue;
      }
    }
    // string
    if (oql[i] === '"' || oql[i] === "'") {
      const q = oql[i]; let j = i + 1;
      while (j < oql.length && oql[j] !== q) { if (oql[j] === '\\') j++; j++; }
      tokens.push({ cls: 'oql-str', text: oql.slice(i, j + 1) });
      i = j + 1; continue;
    }
    // @attr
    if (oql[i] === '@') {
      let j = i + 1;
      while (j < oql.length && /[a-zA-Z0-9_]/.test(oql[j])) j++;
      tokens.push({ cls: 'oql-at', text: oql.slice(i, j) });
      i = j; continue;
    }
    // word: keyword or function call or identifier
    if (/[a-zA-Z_]/.test(oql[i])) {
      let j = i + 1;
      while (j < oql.length && /[a-zA-Z0-9_]/.test(oql[j])) j++;
      const word = oql.slice(i, j);
      // skip whitespace to check for (
      let k = j;
      while (k < oql.length && oql[k] === ' ') k++;
      const isKw = _KW_SET.has(word.toUpperCase());
      const isFn = oql[k] === '(' && !isKw;
      tokens.push({ cls: isKw ? 'oql-kw' : isFn ? 'oql-fn' : 'oql-id', text: word });
      i = j; continue;
    }
    // number
    if (/[0-9]/.test(oql[i]) || (oql[i] === '-' && /[0-9]/.test(oql[i+1] || ''))) {
      let j = i + 1;
      while (j < oql.length && /[0-9._eE+\-]/.test(oql[j])) j++;
      tokens.push({ cls: 'oql-num', text: oql.slice(i, j) });
      i = j; continue;
    }
    // operator / punctuation
    if (/[().,\[\]{}]/.test(oql[i])) {
      tokens.push({ cls: 'oql-op', text: oql[i] });
      i++; continue;
    }
    // everything else (whitespace, newlines, operators like =<>!*/)
    tokens.push({ cls: null, text: oql[i] });
    i++;
  }

  return tokens.map(t => {
    const esc = t.text.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
    return t.cls ? `<span class="${t.cls}">${esc}</span>` : esc;
  }).join('');
}

// Singleton OQL preview popup, created once and reused.
const _nqPreview = (() => {
  const el = document.createElement('div');
  el.className = 'nq-oql-preview';
  el.innerHTML = `<div class="nq-preview-header"><div class="nq-preview-name"></div><div class="nq-preview-desc"></div></div><div class="nq-preview-code"></div>`;
  document.body.appendChild(el);
  return el;
})();

let _nqPreviewTimer = null;

function _showNqPreview(card, oql, name, desc) {
  clearTimeout(_nqPreviewTimer);
  _nqPreviewTimer = setTimeout(() => {
    const nameEl = _nqPreview.querySelector('.nq-preview-name');
    const descEl = _nqPreview.querySelector('.nq-preview-desc');
    const codeEl = _nqPreview.querySelector('.nq-preview-code');
    if (nameEl) nameEl.textContent = name || '';
    if (descEl) descEl.textContent = desc || '';
    if (nameEl) nameEl.style.display = name ? '' : 'none';
    if (descEl) descEl.style.display = desc ? '' : 'none';
    const hdr = _nqPreview.querySelector('.nq-preview-header');
    if (hdr) hdr.style.display = (name || desc) ? '' : 'none';
    if (codeEl) codeEl.innerHTML = _oqlHighlight(oql);
    _nqPreview.classList.remove('visible');
    _nqPreview.style.display = 'flex';

    const rect = card.getBoundingClientRect();
    const previewW = 420;
    const spaceRight = window.innerWidth - rect.right - 16;
    const spaceLeft  = rect.left - 16;
    if (spaceRight >= 200) {
      _nqPreview.style.left = (rect.right + 8) + 'px';
    } else {
      _nqPreview.style.left = Math.max(4, rect.left - previewW - 8) + 'px';
    }
    const top = Math.min(rect.top, window.innerHeight - 300);
    _nqPreview.style.top = Math.max(8, top) + 'px';
    // trigger transition
    requestAnimationFrame(() => _nqPreview.classList.add('visible'));
  }, 120);
}

function _hideNqPreview() {
  clearTimeout(_nqPreviewTimer);
  _nqPreview.classList.remove('visible');
  // hide after fade
  setTimeout(() => { if (!_nqPreview.classList.contains('visible')) _nqPreview.style.display = 'none'; }, 130);
}

function buildSidebar(analysisReady) {
  const list = document.getElementById('named-query-list');
  list.innerHTML = '';

  // ── My Queries (stored / starred) ──────────────────────────────────────────
  const stored = JSON.parse(localStorage.getItem(STORED_QUERIES_KEY) || '{}');
  const starred = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');

  function _makeMyQueryCard(name, oql, onDelete) {
    const card = document.createElement('div');
    card.className = 'nq-card nq-my-query';
    card.dataset.oql = oql;
    const row = document.createElement('div');
    row.className = 'nq-my-row';
    const nameEl = document.createElement('div');
    nameEl.className = 'nq-name';
    nameEl.textContent = name;
    const delBtn = document.createElement('button');
    delBtn.className = 'nq-del-btn';
    delBtn.title = 'Remove';
    delBtn.textContent = '×';
    delBtn.addEventListener('click', (e) => { e.stopPropagation(); onDelete(); });
    row.appendChild(nameEl);
    row.appendChild(delBtn);
    card.appendChild(row);
    card.addEventListener('mouseenter', () => _showNqPreview(card, oql, name, ''));
    card.addEventListener('mouseleave', _hideNqPreview);
    card.addEventListener('click', () => {
      _hideNqPreview();
      if (term && window._hprofRunQuery) window._hprofRunQuery(oql);
      else if (term && window._hprofSetLine) window._hprofSetLine(oql);
    });
    return card;
  }

  const storedEntries = Object.entries(stored);
  if (storedEntries.length > 0) {
    const hdr = document.createElement('div');
    hdr.className = 'nq-group-hdr nq-group-my';
    hdr.textContent = 'My Queries';
    list.appendChild(hdr);
    storedEntries.forEach(([name, oql]) => {
      const card = _makeMyQueryCard(name, oql, () => {
        const s = JSON.parse(localStorage.getItem(STORED_QUERIES_KEY) || '{}');
        delete s[name];
        localStorage.setItem(STORED_QUERIES_KEY, JSON.stringify(s));
        buildSidebar(analysisReady);
        showToast(`Removed "${name}"`, 'info');
      });
      list.appendChild(card);
    });
  }

  // ── Named queries (WASM built-ins) ─────────────────────────────────────────
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
    card.dataset.oql = q.oql;
    const nameEl = document.createElement('div');
    nameEl.className = 'nq-name';
    nameEl.textContent = q.name;
    const descEl = document.createElement('div');
    descEl.className = 'nq-display';
    descEl.textContent = q.display;
    card.appendChild(nameEl);
    card.appendChild(descEl);
    const previewOql = disabled
      ? q.oql + '\n\n[Requires full analysis — click "Run Analysis" first]'
      : q.oql;
    card.addEventListener('mouseenter', () => _showNqPreview(card, previewOql, q.name, q.display));
    card.addEventListener('mouseleave', _hideNqPreview);
    if (!disabled) {
      card.addEventListener('click', () => {
        _hideNqPreview();
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
    theme: termTheme(),
    cursorBlink: true,
    fontSize: 13,
    fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', Menlo, Consolas, monospace",
    scrollback: 8000,
    allowProposedApi: true,
  });
  const fitAddon = new FitAddon.FitAddon();
  term.loadAddon(fitAddon);
  term.open(document.getElementById('terminal-container'));
  window._hprofTerm = term;
  fitAddon.fit();

  // ── Floating action strip (☆ Star / 📊 Viz) ──────────────────────────────
  // Sits in the terminal-container corner; appears on hover when a result exists.
  const termActions = document.createElement('div');
  termActions.id = 'term-actions';
  termActions.innerHTML = `
    <button id="term-act-star" title="Star last result (save to Dashboard)">☆ Star</button>
    <button id="term-act-viz"  title="Visualise last result as treemap / histogram">📊 Viz</button>`;
  document.getElementById('terminal-container').appendChild(termActions);

  function _updateTermActions() {
    const has = !!lastResult;
    termActions.classList.toggle('has-result', has);
  }

  document.getElementById('term-act-star').addEventListener('click', () => {
    if (term && window._hprofRunQuery) window._hprofRunQuery('/star');
    else if (term) {
      term.paste('/star');
    }
  });
  document.getElementById('term-act-viz').addEventListener('click', () => {
    if (term && window._hprofRunQuery) window._hprofRunQuery('/viz');
    else if (term) {
      term.paste('/viz');
    }
  });

  window._updateTermActions = _updateTermActions;

  const ro = new ResizeObserver(() => fitAddon.fit());
  ro.observe(document.getElementById('terminal-container'));

  const verStr = serverVersion ? ` \x1b[2mv${serverVersion.version || ''}\x1b[0m` : '';
  const histCount = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]').length;
  term.writeln('\x1b[1;36m hprof-analyzer\x1b[0m\x1b[36m OQL Shell\x1b[0m' + verStr);
  const connLabel = wasmSession
    ? document.getElementById('server-url-display').textContent || 'WASM'
    : serverUrl || '';
  term.writeln(`\x1b[2m └─ ${connLabel}`
    + (namedQueries.length ? `  ·  ${namedQueries.length.toLocaleString('en-US')} named queries` : '')
    + (histCount ? `  ·  ${histCount.toLocaleString('en-US')} history entries` : '')
    + '\x1b[0m');
  term.writeln('\x1b[2m    Tab = complete  ·  Ctrl+R = history  ·  /help = commands  ·  /examples = OQL tour\x1b[0m');
  term.writeln('');

  // Wrapper: write prompt and refresh the floating action strip state.
  const writePrompt = () => {
    term.write(PROMPT);
    if (window._updateTermActions) window._updateTermActions();
  };
  term.write(PROMPT); // initial prompt — lastResult not yet declared, skip _updateTermActions

  let line = '';
  let cursorPos = 0;  // index within line where the cursor sits
  let ghostText = '';   // current inline suggestion suffix (display only, not in line)
  let histIdx = -1;
  let histSavedLine = '';  // current draft line saved when entering history navigation

  // ── Completion popover ────────────────────────────────────────────────────
  const popover = document.createElement('div');
  popover.id = 'completion-popover';
  document.body.appendChild(popover);

  let popItems = [];   // [{value, group}]
  let popSel  = -1;    // selected index (-1 = none)

  function popHide() {
    popover.classList.remove('visible');
    popItems = [];
    popSel = -1;
  }

  function popShow(items, typedToken) {
    popItems = items;
    popSel = -1;
    popover.innerHTML = '';

    const MAX = 200;
    const visible = items.slice(0, MAX);
    visible.forEach((c, i) => {
      const div = document.createElement('div');
      div.className = 'cp-item';
      div.dataset.idx = i;

      // highlight the typed prefix inside value
      const val = c.value;
      const lo = typedToken.toLowerCase();
      const matchStart = val.toLowerCase().indexOf(lo);
      let valHtml;
      if (lo && matchStart >= 0) {
        valHtml = escHtml(val.slice(0, matchStart))
          + '<span class="cp-match">' + escHtml(val.slice(matchStart, matchStart + lo.length)) + '</span>'
          + escHtml(val.slice(matchStart + lo.length));
      } else {
        valHtml = escHtml(val);
      }

      const grpHtml = c.group ? `<span class="cp-group">${escHtml(c.group)}</span>` : '';
      const descHtml = c.description ? `<span class="cp-desc">${escHtml(c.description)}</span>` : '';
      // Per-item syntax highlighting on the value span — use CSS variables for theme-awareness
      const cs = getComputedStyle(document.documentElement);
      let valColor;
      if (c.group === 'field' || val.startsWith('@')) {
        valColor = cs.getPropertyValue('--cp-col-field').trim() || '#c0a0ff';
      } else if (c.group === 'keyword') {
        valColor = cs.getPropertyValue('--cp-col-kw').trim()    || '#f0d080';
      } else if (c.group === 'class' || val.includes('.') || /^[A-Z]/.test(val)) {
        valColor = cs.getPropertyValue('--cp-col-class').trim() || '#90c0f8';
      } else {
        valColor = cs.getPropertyValue('--cp-col-fn').trim()    || '#60c8e0';
      }
      div.innerHTML = `<span class="cp-value" style="color:${valColor}">${valHtml}</span>${grpHtml}${descHtml}`;
      div.addEventListener('mousedown', e => {
        e.preventDefault();
        popAccept(i);
        term.focus();
      });
      popover.appendChild(div);
    });

    if (items.length > MAX) {
      const more = document.createElement('div');
      more.className = 'cp-more';
      more.textContent = `… ${items.length - MAX} more`;
      popover.appendChild(more);
    }

    popPosition();
    popover.classList.add('visible');
  }

  function escHtml(s) {
    return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;');
  }

  function popPosition() {
    // Use xterm cell metrics to position below the current cursor column
    const containerRect = document.getElementById('terminal-container').getBoundingClientRect();
    let cellW = 8, cellH = 17;
    try {
      const core = term._core;
      cellW = core._renderService.dimensions.css.cell.width;
      cellH = core._renderService.dimensions.css.cell.height;
    } catch (_) {}

    // Use the actual xterm buffer cursor position instead of computing from
    // promptLen + cursorPos, which is wrong when the line wraps.
    // xterm has a small left/top padding (matches .xterm { padding: 4px 8px })
    const xtermPadLeft = 8;
    const xtermPadTop  = 4;
    const cursorCol = term.buffer.active.cursorX;
    const cursorRow = term.buffer.active.cursorY;
    const x = containerRect.left + xtermPadLeft + cursorCol * cellW;
    const y = containerRect.top  + xtermPadTop  + (cursorRow + 1) * cellH;

    // flip above if not enough space below
    const below = window.innerHeight - y;
    const above = y - cellH;
    const popH = Math.min(280, popItems.length * 25 + 8);

    popover.style.left = Math.min(x, window.innerWidth - 490) + 'px';
    if (below >= popH || below >= above) {
      popover.style.top  = y + 'px';
      popover.style.bottom = '';
    } else {
      popover.style.bottom = (window.innerHeight - y + cellH) + 'px';
      popover.style.top = '';
    }
  }

  function popSelect(idx) {
    const items = popover.querySelectorAll('.cp-item');
    items.forEach((el, i) => el.classList.toggle('selected', i === idx));
    if (idx >= 0 && items[idx]) {
      items[idx].scrollIntoView({ block: 'nearest' });
    }
    popSel = idx;
  }

  function popAccept(idx) {
    if (idx < 0 || idx >= popItems.length) { popHide(); return; }
    const c = popItems[idx];
    // Replace the typed token at cursor with the chosen value
    const prefix = line.slice(0, cursorPos);
    const lastDelim = prefix.search(/[\s,(](?=[^\s,(]*$)/);
    const tokenStart = lastDelim >= 0 ? lastDelim + 1 : 0;
    const suffix = line.slice(cursorPos);
    line = line.slice(0, tokenStart) + c.value + (c.trailing_space ? ' ' : '') + suffix;
    cursorPos = tokenStart + c.value.length + (c.trailing_space ? 1 : 0);
    ghostText = '';
    popHide();
    redrawLine();
  }
  const history = JSON.parse(localStorage.getItem(HISTORY_KEY) || '[]');
  let killRing = '';  // text killed by Ctrl+K/W/U

  // Ctrl+R incremental search state
  let isearching = false;
  let isearchQuery = '';
  let isearchMatch = -1;  // index into history of current match

  let inputRowCount = 1;  // terminal rows occupied by current input (for internal tracking)
  let pendingLines = [];  // lines accumulated for multi-line query (via \ continuation)
  const CONT_PROMPT = '...> ';  // shown on continuation lines

  // OQL keyword syntax highlighting for the input line.
  // Returns the line with ANSI color codes applied; raw line.length is unchanged
  // so cursor positioning arithmetic remains correct.
  const _OQL_KW = new Set([
    'SELECT','FROM','WHERE','ORDER','BY','GROUP','LIMIT','OFFSET','AS','INSTANCEOF',
    'UNION','DISTINCT','ALL','AND','OR','NOT','IN','IS','NULL','TRUE','FALSE',
    'HAVING','BETWEEN','EXISTS','CASE','WHEN','THEN','ELSE','END','EXCEPT','INTERSECT',
    'COUNT','SUM','MIN','MAX','AVG','PERCENTILE','MEDIAN','STDDEV',
  ]);
  function highlightOql(s) {
    if (!s || s.startsWith('/') || s.startsWith('!')) return s;
    // Keywords: bright cyan; @attr: yellow; strings: green; numbers: magenta; comments: dim
    let result = '';
    let i = 0;
    while (i < s.length) {
      // Line comment: -- to end of string
      if (s[i] === '-' && s[i+1] === '-') {
        result += '\x1b[2m' + s.slice(i) + '\x1b[0m';
        i = s.length;
        continue;
      }
      // String literal
      if (s[i] === '"' || s[i] === "'") {
        const q = s[i];
        let j = i + 1;
        while (j < s.length && s[j] !== q) {
          if (s[j] === '\\') j++;
          j++;
        }
        result += '\x1b[32m' + s.slice(i, j + 1) + '\x1b[0m';
        i = j + 1;
        continue;
      }
      // Number
      if (/[0-9]/.test(s[i]) && (i === 0 || /\W/.test(s[i-1]))) {
        let j = i;
        while (j < s.length && /[0-9._]/.test(s[j])) j++;
        result += '\x1b[35m' + s.slice(i, j) + '\x1b[0m';
        i = j;
        continue;
      }
      // @ attribute
      if (s[i] === '@') {
        let j = i + 1;
        while (j < s.length && /\w/.test(s[j])) j++;
        result += '\x1b[33m' + s.slice(i, j) + '\x1b[0m';
        i = j;
        continue;
      }
      // Word — check if keyword
      if (/[a-zA-Z_]/.test(s[i])) {
        let j = i;
        while (j < s.length && /[\w$.]/.test(s[j])) j++;
        const word = s.slice(i, j);
        if (_OQL_KW.has(word.toUpperCase())) {
          result += '\x1b[36;1m' + word + '\x1b[0m';
        } else {
          result += word;
        }
        i = j;
        continue;
      }
      result += s[i++];
    }
    return result;
  }

  // Redraw line and reposition cursor; does NOT change histIdx.
  // Uses CONT_PROMPT on continuation lines so editing mid-line doesn't
  // flip the prompt back to PROMPT.
  function redrawLine() {
    const prompt = pendingLines.length > 0 ? CONT_PROMPT : PROMPT;
    const highlighted = highlightOql(line);
    // Draw: prompt + highlighted line + dim ghost suffix (only when cursor is at end)
    const ghost = (cursorPos === line.length && ghostText)
      ? '\x1b[2m' + ghostText + '\x1b[0m'
      : '';
    term.write('\r\x1b[K' + prompt + highlighted + '\x1b[0m' + ghost);
    // Reposition cursor: move back over ghost text (raw char count)
    const moveBack = (line.length - cursorPos) + (ghost ? ghostText.length : 0);
    if (moveBack > 0) {
      term.write(`\x1b[${moveBack}D`);
    }
  }

  function updateGhost() {
    if (!wasmReady || !classNames.length) { ghostText = ''; popHide(); return; }
    // Only suggest inside OQL, not for / commands or ! bookmarks
    if (line.startsWith('/') || line.startsWith('!')) { ghostText = ''; popHide(); return; }
    try {
      const cs = JSON.parse(wasmSession && wasmSession.complete_query ? wasmSession.complete_query(line, cursorPos) : wasmComplete(line, cursorPos, classNames));
      if (cs.length === 1) {
        // Single match: show ghost text inline, no popover
        popHide();
        const prefix = line.slice(0, cursorPos);
        const lastDelim = prefix.search(/[\s,(](?=[^\s,(]*$)/);
        const typedToken = lastDelim >= 0 ? prefix.slice(lastDelim + 1) : prefix;
        const valSuffix = cs[0].value.startsWith(typedToken)
          ? cs[0].value.slice(typedToken.length)
          : '';
        ghostText = valSuffix + (cs[0].trailing_space && valSuffix !== '' ? ' ' : '');
      } else if (cs.length > 1) {
        // Multiple matches: show popover, no ghost
        ghostText = '';
        const prefix = line.slice(0, cursorPos);
        const lastDelim = prefix.search(/[\s,(](?=[^\s,(]*$)/);
        const typedToken = lastDelim >= 0 ? prefix.slice(lastDelim + 1) : prefix;
        popShow(cs, typedToken);
      } else {
        ghostText = '';
        popHide();
      }
    } catch (_) {
      ghostText = '';
      popHide();
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
    ghostText = '';
    popHide();
    redrawLine();
    term.focus();
  }
  window._hprofSetLine = setLine;

  async function runQueryFromSidebar(oql) {
    term.focus();
    // Show a truncated, flattened echo so multi-line/long queries don't break the line
    const maxEcho = term.cols - PROMPT.length - 1;
    const flat = oql.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
    const echo = flat.length > maxEcho ? flat.slice(0, maxEcho - 1) + '…' : flat;
    term.write('\r\x1b[K' + PROMPT + highlightOql(echo) + '\x1b[0m');
    line = '';
    cursorPos = 0;
    histIdx = -1;
    ghostText = '';
    term.writeln('');
    await handleEnter(oql);
  }
  window._hprofRunQuery = runQueryFromSidebar;
  window._hprofQueryDirect = (oql) => wasmSession ? wasmSession.query(oql) : null;

  function handleTab() {
    ghostText = '';
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
                    'sort','unique','pivot','stats','top','head','tail','row','undo','sample','cols','columns','select','drop','rename','wc','limit','not','exclude','distinct','dedup','obj','run','bookmark','save','forget','last','describe','count','watch','q','quit','disconnect',
                    'store','remove','star','unstar','viz','dashboard'];
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
        matches.forEach(q => term.writeln(`  \x1b[36m${q.name.padEnd(40)}\x1b[0m  \x1b[2m${q.display}\x1b[0m`));
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
      const fmts = ['csv', 'tsv', 'json'].filter(f => f.startsWith(partial));
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
        const keys = ['limit', 'bytes', 'color', 'null'].filter(k => k.startsWith(partial));
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
    // Complete /remove <stored-name> and /unstar <starred-label>
    if (line.startsWith('/remove ') || line.startsWith('/unstar ')) {
      const isRemove = line.startsWith('/remove ');
      const pfxLen = isRemove ? 8 : 8;
      const partial = line.slice(pfxLen).toLowerCase();
      let names;
      if (isRemove) {
        names = Object.keys(JSON.parse(localStorage.getItem(STORED_QUERIES_KEY) || '{}')).filter(n => n.toLowerCase().startsWith(partial));
      } else {
        names = (JSON.parse(localStorage.getItem(STARRED_KEY) || '[]')).map(e => e.label).filter(n => n.toLowerCase().startsWith(partial));
      }
      if (names.length === 1) { setLine(line.slice(0, pfxLen) + names[0]); }
      else if (names.length > 1) { term.writeln(''); term.writeln('  ' + names.map(n => `\x1b[35m${n}\x1b[0m`).join('  ')); redrawLine(); }
      return;
    }
    // Complete /viz [kind] [labelcol] [valuecol]
    if (line.startsWith('/viz ')) {
      const after = line.slice(5);
      const parts = after.split(' ');
      const kinds = ['treemap', 'histogram', 'table'];
      if (parts.length === 1) {
        // completing kind or @N
        const partial = parts[0].toLowerCase();
        const matches = kinds.filter(k => k.startsWith(partial));
        if (matches.length === 1) { setLine('/viz ' + matches[0]); }
        else if (matches.length > 1) { term.writeln(''); term.writeln('  ' + matches.join('  ')); redrawLine(); }
      } else if (parts.length >= 2 && lastResult) {
        // completing label or value column
        const partial = parts[parts.length - 1].toLowerCase();
        const cols = lastResult.columns.filter(c => c.toLowerCase().startsWith(partial));
        if (cols.length === 1) { setLine(line.slice(0, line.lastIndexOf(' ') + 1) + cols[0]); }
        else if (cols.length > 1) { term.writeln(''); term.writeln('  ' + cols.map(c => `\x1b[36m${c}\x1b[0m`).join('  ')); redrawLine(); }
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
      // Multi-column commands: /select and /drop complete the last space-separated token
      if (line.startsWith('/select ') || line.startsWith('/drop ')) {
        const pfxEnd = line.indexOf(' ') + 1;
        const prefix = line.slice(0, pfxEnd);
        const afterCmd = line.slice(pfxEnd);
        const lastSpace = afterCmd.lastIndexOf(' ');
        const keepPrefix = lastSpace >= 0 ? prefix + afterCmd.slice(0, lastSpace + 1) : prefix;
        const partial = (lastSpace >= 0 ? afterCmd.slice(lastSpace + 1) : afterCmd).toLowerCase();
        const cols = lastResult.columns.filter(c => c.toLowerCase().startsWith(partial));
        if (cols.length === 1) { setLine(keepPrefix + cols[0]); }
        else if (cols.length > 1 && cols.length <= 20) {
          term.writeln('');
          term.writeln('  ' + cols.map(c => `\x1b[36m${c}\x1b[0m`).join('  '));
          redrawLine();
        }
        return;
      }
      // Single-column commands: /filter /grep /unique /stats /pivot /not /exclude /rename /sample /wc
      const singleColCmds = ['/filter ', '/grep ', '/unique ', '/stats ', '/pivot ',
                              '/not ', '/exclude ', '/rename ', '/sample ', '/wc '];
      const matched = singleColCmds.find(p => line.startsWith(p));
      if (matched) {
        const rawArg = line.slice(matched.length);
        // /rename completes only the first argument; once there's a space, the
        // second arg is a free-form new name — don't try to complete it.
        if (matched === '/rename ' && rawArg.includes(' ')) { return; }
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
      const cs = JSON.parse(wasmSession && wasmSession.complete_query ? wasmSession.complete_query(line, cursorPos) : wasmComplete(line, cursorPos, classNames));
      if (cs.length === 0) { popHide(); return; }
      if (cs.length === 1) {
        // Complete to the single suggestion at cursor
        popHide();
        ghostText = '';
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
        // If popover is visible and has a selection, accept it
        if (popover.classList.contains('visible') && popSel >= 0) {
          popAccept(popSel);
          return;
        }
        // Show/refresh popover
        const prefix = line.slice(0, cursorPos);
        const lastDelim = prefix.search(/[\s,(](?=[^\s,(]*$)/);
        const typedToken = lastDelim >= 0 ? prefix.slice(lastDelim + 1) : prefix;
        ghostText = '';
        popShow(cs, typedToken);
        popSelect(0);
      }
    } catch (_) { popHide(); }
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
      writePrompt();
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
      writePrompt();
      return;
    }
    if (cmd === '/examples' || cmd.startsWith('/examples ')) {
      const cat = cmd.replace('/examples', '').trim() || null;
      printExamples(cat);
      writePrompt();
      return;
    }
    if (cmd === '/clear') {
      term.clear();
      writePrompt();
      return;
    }
    if (cmd === '/q' || cmd === '/quit' || cmd === '/disconnect') {
      document.getElementById('btn-disconnect').click();
      return;
    }
    if (cmd === '/analyze') {
      document.getElementById('btn-show-report').click();
      term.writeln('\x1b[33mopening report…\x1b[0m');
      writePrompt();
      return;
    }
    if (cmd === '/status') {
      if (wasmSession) {
        const st = hasRetained ? 'ready' : 'not_started';
        if (st === 'ready') term.writeln('\x1b[32m● Analysis ready — @retainedHeapSize queries available\x1b[0m');
        else term.writeln('\x1b[33m● Analysis not run yet — click "Run Analysis" in the toolbar\x1b[0m');
        writePrompt(); return;
      }
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
      writePrompt();
      return;
    }
    if (cmd.startsWith('/describe ') || cmd === '/describe') {
      const cls = cmd.slice(9).trim();
      if (!cls) {
        term.writeln('\x1b[2musage: /describe <ClassName>  — show fields and instance count\x1b[0m');
        writePrompt();
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
            let sugg = classNames.filter(c => c.toLowerCase().includes(lower)).slice(0, 5);
            if (sugg.length === 0) {
              // fallback: simple-name substring match
              sugg = classNames.filter(c => {
                const sn = c.split('.').pop() || c;
                return sn.toLowerCase().includes(lower);
              }).slice(0, 5);
            }
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
            ? `  \x1b[2m(${instanceCount.toLocaleString('en-US')} instance${instanceCount === 1 ? '' : 's'})\x1b[0m`
            : '';
          term.writeln(`Fields of \x1b[1m${cls}\x1b[0m${countStr}`);
          const idxW = String(colNames.length).length;
          const nameW = Math.max(...colNames.map(c => c.length), 8);
          colNames.forEach((n, i) => {
            let typeTag = 'null';
            if (rows.length > 0) {
              const cell = rows[0][i];
              if (cell !== null && cell !== undefined) {
                if (typeof cell !== 'object') typeTag = typeof cell;
                else if (cell.kind && cell.kind !== 'null') typeTag = cell.kind === 'obj_ref' ? 'ref' : cell.kind;
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
      writePrompt();
      return;
    }
    if (cmd.startsWith('/obj ') || cmd === '/obj') {
      // /obj <ClassName>#<idx>  or  /obj <ClassName> <idx>
      const arg = cmd.slice(4).trim();
      if (!arg) {
        term.writeln('\x1b[2musage: /obj <ClassName>#<idx>  — inspect a specific object by class + dense index\x1b[0m');
        writePrompt();
        return;
      }
      // Parse "<Class>#<n>" or "<Class> <n>" formats
      const m = arg.match(/^(.+?)#(\d+)$/) || arg.match(/^(.+?)\s+(\d+)$/);
      if (!m) {
        term.writeln('\x1b[2musage: /obj <ClassName>#<idx>  e.g. /obj java.lang.String#42\x1b[0m');
        writePrompt();
        return;
      }
      const [, cls, idx] = m;
      const clsTrimmed = cls.trim();
      // Run the query; if exactly 1 row, show as key=value (nicer than a 1-row table)
      term.write('\x1b[2m⠋ fetching…\x1b[0m');
      try {
        const res = await fetch(serverUrl + '/', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ query: `SELECT * FROM ${clsTrimmed} s WHERE s.@objectId = ${idx}` }),
          signal: AbortSignal.timeout(10000),
        }).then(r => r.json());
        term.write('\r\x1b[K');
        if (!res.ok) {
          const msg = res.error?.message || JSON.stringify(res.error) || 'unknown error';
          term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
          writePrompt(); return;
        }
        const r = res.result;
        if (r?.error) {
          term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
        } else if (!r?.columns || r.columns.length === 0) {
          term.writeln(`\x1b[33m(no object ${clsTrimmed}#${idx} found)\x1b[0m`);
        } else {
          const colNames = r.columns.map(c => c.name || String(c));
          const rows = r.rows || [];
          if (rows.length === 1) {
            const keyW = Math.max(...colNames.map(n => n.length));
            const idxW = String(colNames.length).length;
            term.writeln(`\x1b[1m── ${clsTrimmed}#${idx} ──\x1b[0m`);
            colNames.forEach((col, i) => {
              const cell = rows[0][i];
              const val = fmtCell(cell, col);
              const cc = cellColor(cell, col);
              const valStr = cc ? `${cc}${val}\x1b[0m` : val;
              term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  \x1b[36m${col.padEnd(keyW)}\x1b[0m  ${valStr}`);
            });
            prevResult = lastResult;
            lastResult = { columns: colNames, rows, note: r.note, truncated: r.truncated, row_count: r.row_count };
            currentRowIdx = 0;
          } else if (rows.length === 0) {
            term.writeln(`\x1b[33m(no object ${clsTrimmed}#${idx} found)\x1b[0m`);
          } else {
            renderResult(r);
            prevResult = lastResult;
            lastResult = { columns: colNames, rows, note: r.note, truncated: r.truncated, row_count: r.row_count };
            currentRowIdx = 0;
          }
        }
      } catch (e) {
        term.write('\r\x1b[K');
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd.startsWith('/plan ') || cmd === '/plan' ||
        cmd.startsWith('/explain ') || cmd === '/explain') {
      const isPlan = cmd.startsWith('/plan') || cmd === '/plan';
      const oql = cmd.slice(isPlan ? 5 : 8).trim();
      if (!oql) {
        term.writeln(`\x1b[2musage: /plan <oql>  — show query execution plan (no scan)\x1b[0m`);
        writePrompt();
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
      writePrompt();
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
        writePrompt();
        return;
      }
      const m = args.match(/^(\d+(?:\.\d+)?)\s+(.+)$/s);
      if (!m) {
        term.writeln('\x1b[2musage: /watch <seconds> <oql>\x1b[0m');
        writePrompt();
        return;
      }
      const secs = parseFloat(m[1]);
      const watchOql = m[2].trim();
      if (secs < 1) {
        term.writeln('\x1b[31mminimum interval is 1 second\x1b[0m');
        writePrompt();
        return;
      }
      if (watchTimer) { clearInterval(watchTimer); watchTimer = null; }
      term.writeln(`\x1b[2mwatching every ${secs}s — Ctrl+C or /watch stop to cancel\x1b[0m`);
      const tick = async () => {
        const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
        term.writeln(`\x1b[2m── ${ts} ──────────────────────────────────────────\x1b[0m`);
        await runQuery(watchOql, { showHint: false });
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
          term.writeln(`\x1b[32m${n.toLocaleString('en-US')}\x1b[0m row${n !== 1 ? 's' : ''} × \x1b[32m${m}\x1b[0m col${m !== 1 ? 's' : ''}`);
          writePrompt(); return;
        }
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
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
          const nFmt = n != null ? n.toLocaleString('en-US') : '?';
          const dynLabel = isOql ? label
            : `instance${n === 1 ? '' : 's'} of \x1b[36m${arg}\x1b[0m`;
          term.writeln(`\x1b[32m${nFmt}\x1b[0m ${dynLabel}`);
          // Store count result so /filter, /sort etc. can chain on it (mirrors CLI !count behaviour)
          const r = data.result;
          if (r?.columns) {
            const colNames = r.columns.map(c => c.name || String(c));
            prevResult = null;
            lastResult = { columns: colNames, rows: r.rows || [], note: r.note, truncated: r.truncated, row_count: r.row_count };
            currentRowIdx = 0;
          }
        } else {
          const msg = data.error?.message || data.error || 'unknown error';
          term.writeln(`\x1b[31merror: ${msg}\x1b[0m`);
        }
      } catch (e) {
        term.write('\r\x1b[K');
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd === '/last') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no previous query to re-display)\x1b[0m');
      } else {
        renderResult(lastResult);
        term.writeln(`\x1b[2m${lastResult.rows.length.toLocaleString('en-US')} rows (re-displayed)\x1b[0m`);
      }
      writePrompt();
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
          term.writeln(`\x1b[32m${n.toLocaleString('en-US')}\x1b[0m row${n !== 1 ? 's' : ''} × \x1b[32m${m}\x1b[0m col${m !== 1 ? 's' : ''}`);
        } else {
          const ci = resolveCol(colArg, lastResult.columns);
          if (ci < 0) {
            term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
          } else {
            const total = lastResult.rows.length;
            const nonNull = lastResult.rows.filter(row => row[ci] !== null && row[ci] !== undefined && !(typeof row[ci] === 'object' && row[ci]?.kind === 'null')).length;
            term.writeln(`\x1b[32m${nonNull.toLocaleString('en-US')}\x1b[0m non-null / \x1b[32m${total.toLocaleString('en-US')}\x1b[0m total in "${lastResult.columns[ci]}"`);
          }
        }
      }
      writePrompt();
      return;
    }
    if (cmd === '/undo') {
      if (!prevResult) {
        term.writeln('\x1b[33m(nothing to undo)\x1b[0m');
      } else {
        lastResult = prevResult;
        prevResult = null;
        term.writeln(`\x1b[32m✓ undone\x1b[0m  \x1b[2m(restored ${lastResult.rows.length.toLocaleString('en-US')} row${lastResult.rows.length !== 1 ? 's' : ''})\x1b[0m`);
        renderResult(lastResult);
      }
      writePrompt();
      return;
    }
    if (cmd.startsWith('/row ') || cmd === '/row') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      if (lastResult.rows.length === 0) {
        term.writeln('\x1b[33m(result has no rows)\x1b[0m');
        writePrompt();
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
          term.writeln(`\x1b[31mrow ${n} out of range\x1b[0m  \x1b[2mresult has ${lastResult.rows.length.toLocaleString('en-US')} rows\x1b[0m`);
          writePrompt();
          return;
        } else if (isNaN(n)) {
          term.writeln(`\x1b[2musage: /row [N|first|last|next|prev]  — show row as key=value pairs\x1b[0m`);
          writePrompt();
          return;
        }
        currentRowIdx = n - 1;
      }
      const row = lastResult.rows[n - 1];
      const keyW = Math.max(...lastResult.columns.map(c => c.length));
      const idxW = String(lastResult.columns.length).length;
      const total = lastResult.rows.length;
      const navHint = total > 1 ? `\x1b[2m  (use /row next / /row prev to navigate)\x1b[0m` : '';
      term.writeln(`\x1b[2m── row ${n} of ${total.toLocaleString('en-US')} ──\x1b[0m${navHint}`);
      lastResult.columns.forEach((col, i) => {
        const key = col.padEnd(keyW);
        const cell = row[i];
        const val = fmtCell(cell, col);
        const cc = cellColor(cell, col);
        const valStr = cc ? `${cc}${val}\x1b[0m` : val;
        term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  \x1b[36m${key}\x1b[0m  ${valStr}`);
      });
      writePrompt();
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
          writePrompt();
          return;
        }
        settings.rowLimit = n;
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ row limit: ${n}\x1b[0m`);
      }
      if (lastResult) {
        renderResult(lastResult);
        term.writeln(`\x1b[2m${lastResult.rows.length.toLocaleString('en-US')} rows\x1b[0m`);
      }
      writePrompt();
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
            if (typeTag === 'null') typeTag = (cell.kind === 'obj_ref' ? 'ref' : cell.kind) || typeof cell;
          }
          const fill = total > 0 ? `  ${nonNull}/${total} (${Math.round(nonNull / total * 100)}%)` : '';
          const allNull = total > 0 && nonNull === 0;
          const nameColor = allNull ? '\x1b[2;33m' : '\x1b[36m';
          const dimSuffix = allNull ? ' \x1b[33m(all null)\x1b[0m' : '';
          term.writeln(`  \x1b[2m${String(i + 1).padStart(idxW)}\x1b[0m  ${nameColor}${f.padEnd(colW)}\x1b[0m  \x1b[2m${typeTag.padEnd(8)}${fill}\x1b[0m${dimSuffix}`);
        });
        term.writeln(`\x1b[2m(${fields.length} column${fields.length !== 1 ? 's' : ''})\x1b[0m`);
      }
      writePrompt();
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
            const colsChanged = newCols.length !== fields.length || newCols.some((c, i) => c !== fields[i]);
            if (colsChanged) prevResult = lastResult;
            lastResult = { columns: newCols, rows: newRows };
            renderResult(lastResult);
          }
        }
      }
      writePrompt();
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
              renderResult(lastResult);
            }
          }
        }
      }
      writePrompt();
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
            prevResult = { ...lastResult, columns: [...lastResult.columns] };
            lastResult.columns[i] = newName;
            term.writeln(`\x1b[32m✓\x1b[0m \x1b[2m${JSON.stringify(oldName)}\x1b[0m → \x1b[32m${JSON.stringify(newName)}\x1b[0m`);
          }
        }
      }
      writePrompt();
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
      writePrompt();
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
      writePrompt();
      return;
    }
    // /store [name] — save last OQL query permanently in sidebar "My Queries"
    if (cmd.startsWith('/store') && (cmd === '/store' || cmd[6] === ' ')) {
      const stored = JSON.parse(localStorage.getItem(STORED_QUERIES_KEY) || '{}');
      const rest = cmd.slice(6).trim();
      if (!rest) {
        const entries = Object.entries(stored);
        if (entries.length === 0) {
          term.writeln('\x1b[2m(no stored queries — use /store <name> to save the last query)\x1b[0m');
        } else {
          term.writeln('\x1b[1mMy Queries\x1b[0m  \x1b[2m(persisted in sidebar)\x1b[0m');
          entries.forEach(([n, oql]) => {
            const flat = oql.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
            const trunc = flat.length > term.cols - n.length - 6 ? flat.slice(0, term.cols - n.length - 7) + '…' : flat;
            term.writeln(`  \x1b[36m${n.padEnd(20)}\x1b[0m  \x1b[2m${trunc}\x1b[0m`);
          });
          term.writeln('\x1b[2m  Use /store <name> to add, /remove <name> to delete\x1b[0m');
        }
      } else {
        const toSave = history.find(h => !h.startsWith('/store') && !h.startsWith('/star') && !h.startsWith('/viz'));
        if (!toSave) {
          term.writeln('\x1b[33m(no query to store — run a query first)\x1b[0m');
        } else {
          stored[rest] = toSave;
          localStorage.setItem(STORED_QUERIES_KEY, JSON.stringify(stored));
          buildSidebar(hasRetained);
          term.writeln(`\x1b[32m✓ stored as "${rest}"\x1b[0m  \x1b[2m(visible in sidebar · /remove ${rest} to delete)\x1b[0m`);
        }
      }
      writePrompt();
      return;
    }
    // /remove <name> — delete a stored query from My Queries
    if (cmd.startsWith('/remove ')) {
      const stored = JSON.parse(localStorage.getItem(STORED_QUERIES_KEY) || '{}');
      const name = cmd.slice(8).trim();
      if (stored[name]) {
        delete stored[name];
        localStorage.setItem(STORED_QUERIES_KEY, JSON.stringify(stored));
        buildSidebar(hasRetained);
        term.writeln(`\x1b[32m✓ removed "${name}" from My Queries\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mno stored query named "${name}"\x1b[0m`);
      }
      writePrompt();
      return;
    }
    // /star [name] — star the last query+result for the dashboard
    if (cmd.startsWith('/star') && (cmd === '/star' || cmd[5] === ' ')) {
      const name = cmd.slice(5).trim() || null;
      const toSave = history.find(h => !h.startsWith('/star') && !h.startsWith('/viz'));
      if (!toSave) {
        term.writeln('\x1b[33m(no query to star — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      if (!lastResult) {
        term.writeln('\x1b[33m(no result to star — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      const starred = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');
      const label = name || toSave.replace(/\n/g, ' ').replace(/\s+/g, ' ').slice(0, 50);
      // Replace existing entry with same label if present
      const idx = starred.findIndex(e => e.label === label);
      const entry = { label, oql: toSave, columns: lastResult.columns, rows: lastResult.rows.slice(0, 200), ts: Date.now() };
      if (idx >= 0) { starred[idx] = entry; } else { starred.unshift(entry); }
      if (starred.length > 20) starred.length = 20;
      localStorage.setItem(STARRED_KEY, JSON.stringify(starred));
      term.writeln(`\x1b[33m★\x1b[0m starred as \x1b[1m"${label}"\x1b[0m  \x1b[2m(/dashboard to view · /unstar "${label}" to remove)\x1b[0m`);
      writePrompt();
      return;
    }
    // /unstar <name> — remove a starred result
    if (cmd.startsWith('/unstar ')) {
      const name = cmd.slice(8).trim();
      const starred = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');
      const idx = starred.findIndex(e => e.label === name);
      if (idx >= 0) {
        starred.splice(idx, 1);
        localStorage.setItem(STARRED_KEY, JSON.stringify(starred));
        term.writeln(`\x1b[32m✓ unstarred "${name}"\x1b[0m`);
      } else {
        term.writeln(`\x1b[31mno starred result named "${name}"\x1b[0m`);
      }
      writePrompt();
      return;
    }
    // /dashboard — open the starred results panel
    if (cmd === '/dashboard') {
      openDashboard();
      writePrompt();
      return;
    }
    // /viz [@N] [kind] [labelcol] [valuecol] — visualise last (or Nth previous) result
    if (cmd.startsWith('/viz') && (cmd === '/viz' || cmd[4] === ' ')) {
      const rawArgs = cmd.slice(4).trim();
      const parts = rawArgs ? rawArgs.split(/\s+/) : [];

      // Resolve source result: @N references resultLog (1=last, 2=second-to-last…)
      let srcResult = lastResult;
      let srcQuery = null;
      let argOffset = 0;
      const refMatch = parts[0] && /^@(\d+)$/.exec(parts[0]);
      if (refMatch) {
        const idx = parseInt(refMatch[1], 10) - 1;
        if (idx < 0 || idx >= resultLog.length) {
          term.writeln(`\x1b[31m@${idx + 1}: no result at that position — only ${resultLog.length} result${resultLog.length !== 1 ? 's' : ''} in history\x1b[0m`);
          if (resultLog.length > 0) {
            term.writeln('\x1b[2mAvailable:\x1b[0m');
            resultLog.slice(0, 10).forEach((e, i) => {
              term.writeln(`  \x1b[33m@${i + 1}\x1b[0m  \x1b[2m${e.query.slice(0, 60)}${e.query.length > 60 ? '…' : ''}  (${e.result.rows.length} rows)\x1b[0m`);
            });
          }
          writePrompt();
          return;
        }
        srcResult = resultLog[idx].result;
        srcQuery = resultLog[idx].query;
        argOffset = 1;
      }

      // If no args and no result — show history list
      if (!srcResult || !srcResult.rows.length) {
        if (resultLog.length > 0) {
          term.writeln('\x1b[33m(no current result)\x1b[0m  \x1b[2mUse /viz @N to reference a previous result:\x1b[0m');
          resultLog.slice(0, 10).forEach((e, i) => {
            term.writeln(`  \x1b[33m@${i + 1}\x1b[0m  \x1b[2m${e.query.slice(0, 60)}${e.query.length > 60 ? '…' : ''}  (${e.result.rows.length} rows)\x1b[0m`);
          });
        } else {
          term.writeln('\x1b[33m(no result to visualise — run a query first)\x1b[0m');
        }
        writePrompt();
        return;
      }

      const kindArg = parts[argOffset] || '';
      const kinds = ['treemap', 'histogram', 'table'];
      const kind = kinds.includes(kindArg) ? kindArg : 'treemap';
      const colArgOffset = argOffset + (kinds.includes(kindArg) ? 1 : 0);

      const cols = srcResult.columns;

      // /viz with no args: show auto-detected columns and usage
      if (!rawArgs || (argOffset === 0 && !kindArg)) {
        const autoLabelIdx = cols.findIndex((c, i) => {
          const sample = srcResult.rows.find(r => r[i] !== null && r[i] !== undefined);
          return sample ? !isNumericKind(sample[i]) : false;
        });
        const autoValueIdx = cols.findIndex((c, i) => {
          const sample = srcResult.rows.find(r => r[i] !== null && r[i] !== undefined);
          return sample ? isNumericKind(sample[i]) : false;
        });
        const lCol = autoLabelIdx >= 0 ? cols[autoLabelIdx] : cols[0];
        const vCol = autoValueIdx >= 0 ? cols[autoValueIdx] : cols[cols.length - 1];
        term.writeln(`\x1b[33m/viz\x1b[0m  \x1b[2m[treemap|histogram|table]\x1b[0m  \x1b[36m[labelcol]\x1b[0m  \x1b[36m[valuecol]\x1b[0m`);
        term.writeln(`  Auto-detected: label=\x1b[36m${lCol}\x1b[0m  value=\x1b[36m${vCol}\x1b[0m`);
        term.writeln(`  Columns: \x1b[2m${cols.join(', ')}\x1b[0m`);
        if (resultLog.length > 1) {
          term.writeln(`  Previous results: \x1b[2m${resultLog.slice(0, 5).map((e, i) => `@${i + 1} (${e.result.rows.length}r)`).join('  ')}\x1b[0m`);
        }
        term.writeln(`\x1b[2m  Press Enter to run /viz with auto-detected columns, or specify them:\x1b[0m`);
        // Auto-run with detected columns
        const slices = srcResult.rows
          .map(r => {
            const lc = r[autoLabelIdx >= 0 ? autoLabelIdx : 0];
            const vc = r[autoValueIdx >= 0 ? autoValueIdx : cols.length - 1];
            const name = lc == null ? '' : (typeof lc === 'object' ? String(lc.v ?? lc) : String(lc));
            const value = vc == null ? 0 : (typeof vc === 'object' ? Number(vc.v) || 0 : Number(vc) || 0);
            return { name, value };
          })
          .filter(s => s.value > 0)
          .slice(0, 50);
        if (slices.length) {
          openVizOverlay(slices, 'treemap', vCol, srcQuery);
        } else {
          term.writeln('\x1b[33m(no positive values found in auto-detected value column)\x1b[0m');
        }
        writePrompt();
        return;
      }

      if (kind === 'table') {
        renderResult(srcResult);
        writePrompt();
        return;
      }

      // auto-detect label column (first non-numeric) and value column (first numeric)
      const autoLabel = parts[colArgOffset] || cols.find((c, i) => {
        const sample = srcResult.rows.find(r => r[i] !== null && r[i] !== undefined);
        return sample ? !isNumericKind(sample[i]) : false;
      }) || cols[0];
      const autoValue = parts[colArgOffset + 1] || cols.find((c, i) => {
        const sample = srcResult.rows.find(r => r[i] !== null && r[i] !== undefined);
        return sample ? isNumericKind(sample[i]) : false;
      }) || cols[cols.length - 1];
      const labelIdx = resolveCol(autoLabel, cols);
      const valueIdx = resolveCol(autoValue, cols);
      if (labelIdx < 0 || valueIdx < 0) {
        term.writeln(`\x1b[31mcould not resolve columns — available: ${cols.join(', ')}\x1b[0m`);
        writePrompt();
        return;
      }
      const slices = srcResult.rows
        .map(r => {
          const lc = r[labelIdx]; const vc = r[valueIdx];
          const name = lc == null ? '' : (typeof lc === 'object' ? String(lc.v ?? lc) : String(lc));
          const value = vc == null ? 0 : (typeof vc === 'object' ? Number(vc.v) || 0 : Number(vc) || 0);
          return { name, value };
        })
        .filter(s => s.value > 0)
        .slice(0, 50);
      if (!slices.length) {
        term.writeln('\x1b[33m(no positive values to chart)\x1b[0m');
        writePrompt();
        return;
      }
      openVizOverlay(slices, kind, cols[valueIdx], srcQuery);
      writePrompt();
      return;
    }
    if (cmd.startsWith('/unique ') || cmd === '/unique') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      const rawArg = cmd.slice(7).trim();
      if (!rawArg) {
        term.writeln(`\x1b[2musage: /unique <col> [N]  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
        return;
      }
      // Parse optional top-N: "classname 10" or "classname top 10"
      let colArg = rawArg, topN = null;
      const topMatch = rawArg.match(/^(\S+)\s+(?:top\s+)?(\d+)$/i);
      if (topMatch) { colArg = topMatch[1]; topN = parseInt(topMatch[2], 10); }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
        return;
      }
      const colName = lastResult.columns[ci];
      const seen = new Map();
      lastResult.rows.forEach(row => {
        const key = fmtCell(row[ci], colName);
        seen.set(key, (seen.get(key) || 0) + 1);
      });
      const totalDistinct = seen.size;
      let entries = [...seen.entries()].sort((a, b) => (b[1] - a[1]) || a[0].localeCompare(b[0]));
      const showN = topN !== null ? topN : entries.length;
      const shown = Math.min(entries.length, showN);
      entries = entries.slice(0, shown);
      const total = lastResult.rows.length;
      const maxCnt = entries.length > 0 ? entries[0][1] : 0;
      const cntFmt = n => n.toLocaleString('en-US');
      const cntW = Math.max(5, cntFmt(maxCnt).length);
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
        term.writeln(`${val.padEnd(colW)}  \x1b[32m${cntFmt(cnt).padStart(cntW)}\x1b[0m  \x1b[2m${pct.padStart(pctW)}\x1b[0m${bar}`);
      });
      if (shown < totalDistinct) {
        term.writeln(`\x1b[2m(${shown.toLocaleString('en-US')} of ${totalDistinct.toLocaleString('en-US')} distinct values, top ${showN.toLocaleString('en-US')} shown  ·  ${total.toLocaleString('en-US')} total rows)\x1b[0m`);
      } else {
        term.writeln(`\x1b[2m(${totalDistinct.toLocaleString('en-US')} distinct value${totalDistinct !== 1 ? 's' : ''} in ${lastResult.rows.length.toLocaleString('en-US')} rows)\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd.startsWith('/pivot ') || cmd === '/pivot') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      const rawArg = cmd.slice(6).trim();
      if (!rawArg) {
        term.writeln(`\x1b[2musage: /pivot <col> [N]  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
        return;
      }
      let colArg = rawArg, topN = null;
      const topMatch = rawArg.match(/^(\S+)\s+(?:top\s+)?(\d+)$/i);
      if (topMatch) { colArg = topMatch[1]; topN = parseInt(topMatch[2], 10); }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
        return;
      }
      const colName = lastResult.columns[ci];
      const counts = new Map();
      lastResult.rows.forEach(row => {
        const key = fmtCell(row[ci], colName);
        counts.set(key, (counts.get(key) || 0) + 1);
      });
      let entries = [...counts.entries()].sort((a, b) => (b[1] - a[1]) || a[0].localeCompare(b[0]));
      const totalGroups = entries.length;
      if (topN !== null) entries = entries.slice(0, topN);
      const pivotRows = entries.map(([v, c]) => [v, { kind: 'int', v: c }]);
      const note = (topN !== null && pivotRows.length < totalGroups)
        ? `top ${pivotRows.length.toLocaleString('en-US')} of ${totalGroups.toLocaleString('en-US')} groups` : null;
      const pivotResult = { columns: [colName, 'count'], rows: pivotRows, row_count: pivotRows.length, note };
      renderResult(pivotResult);
      prevResult = lastResult;
      lastResult = pivotResult;
      writePrompt();
      return;
    }
    if (cmd.startsWith('/stats ') || cmd === '/stats') {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
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
              if (!settings.bytesRaw && v >= 0 && cName && /bytes$|_size$|heap_size$/i.test(cName)) return fmtBytes(v);
              return v % 1 === 0 ? v.toLocaleString('en-US') : v.toFixed(3);
            };
            const nullNote2 = nullCount2 > 0 ? `  \x1b[2m(${nullCount2.toLocaleString('en-US')} null)\x1b[0m` : '';
            term.writeln(`\x1b[1m${cName}\x1b[0m  \x1b[2m(${vals2.length.toLocaleString('en-US')} non-null values)\x1b[0m${nullNote2}`);
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
          writePrompt();
          return;
        } else {
          term.writeln(`\x1b[2musage: /stats <col>  — no numeric columns found  available: ${lastResult.columns.join(', ')}\x1b[0m`);
          writePrompt();
          return;
        }
      }
      const ci = resolveCol(colArg, lastResult.columns);
      if (ci < 0) {
        term.writeln(`\x1b[31mcolumn "${colArg}" not found\x1b[0m  \x1b[2mavailable: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
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
        writePrompt();
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
        if (!settings.bytesRaw && v >= 0 && colName && /bytes$|_size$|heap_size$/i.test(colName)) return fmtBytes(v);
        return v % 1 === 0 ? v.toLocaleString('en-US') : v.toFixed(3);
      };
      const nullInfo = nullCount > 0 ? `  \x1b[2m(${nullCount.toLocaleString('en-US')} null)\x1b[0m` : '';
      term.writeln(`\x1b[1m${colName}\x1b[0m  \x1b[2m(${vals.length.toLocaleString('en-US')} non-null values)\x1b[0m${nullInfo}`);
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
      writePrompt();
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
        const slicedResult = { columns: [...lastResult.columns], rows: sliced };
        if (shown < total) slicedResult.note = `top ${shown.toLocaleString('en-US')} of ${total.toLocaleString('en-US')}`;
        renderResult({ ...slicedResult, row_count: shown });
        if (shown < total) prevResult = lastResult;
        lastResult = slicedResult;
      }
      writePrompt();
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
        const slicedResult = { columns: [...lastResult.columns], rows: sliced };
        if (shown < total) slicedResult.note = `last ${shown.toLocaleString('en-US')} of ${total.toLocaleString('en-US')}`;
        renderResult({ ...slicedResult, row_count: shown });
        if (shown < total) prevResult = lastResult;
        lastResult = slicedResult;
      }
      writePrompt();
      return;
    }
    if (cmd.startsWith('/sort ') || cmd === '/sort') {
      const args = cmd.slice(5).trim();
      if (!lastResult || !args) {
        if (!lastResult) term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        else term.writeln(`\x1b[2musage: /sort <col> [desc] [,-col2…]  (-col for desc)  — available: ${lastResult.columns.join(', ')}\x1b[0m`);
        writePrompt();
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
      if (!ok || specs.length === 0) { writePrompt(); return; }
      const sorted = [...lastResult.rows].sort((a, b) => {
        for (const { ci, desc, name } of specs) {
          const av = a[ci], bv = b[ci];
          // Null/undefined sorts last
          const aNull = av === null || av === undefined || (typeof av === 'object' && av?.kind === 'null');
          const bNull = bv === null || bv === undefined || (typeof bv === 'object' && bv?.kind === 'null');
          if (aNull && bNull) continue;
          if (aNull) return 1;
          if (bNull) return -1;
          // Extract numeric value for int/float, formatted string otherwise
          const toSortKey = cell => {
            if (typeof cell !== 'object') return cell;
            const k = cell.kind;
            if (k === 'int' || k === 'float') return cell.v;
            return fmtCell(cell, name);
          };
          const an = toSortKey(av), bn = toSortKey(bv);
          const cmp = typeof an === 'number' && typeof bn === 'number'
            ? an - bn : String(an).localeCompare(String(bn));
          const ord = desc ? -cmp : cmp;
          if (ord !== 0) return ord;
        }
        return 0;
      });
      const label = specs.map(s => `${s.name} ${s.desc ? 'desc' : 'asc'}`).join(', ');
      const newResult = { ...lastResult, columns: [...lastResult.columns], rows: sorted, row_count: sorted.length, note: `sorted by ${label}` };
      const savedPrev = prevResult;
      prevResult = lastResult;
      renderResult(newResult);
      if (newResult.note === lastResult.note) { prevResult = savedPrev; }
      lastResult = newResult;
      writePrompt();
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
          if (!sp) { term.writeln('\x1b[2musage: /filter @<col> <pattern>\x1b[0m'); writePrompt(); return; }
          colIdx = resolveCol(sp[1], columns);
          if (colIdx < 0) {
            term.writeln(`\x1b[31mcolumn "${sp[1]}" not found\x1b[0m  \x1b[2mavailable: ${columns.join(', ')}\x1b[0m`);
            writePrompt(); return;
          }
          pattern = sp[2];
        }
        let re;
        const reMatch = pattern.match(/^\/(.+)\/([gimsvy]*)$/);
        if (reMatch) {
          try { re = new RegExp(reMatch[1], reMatch[2]); }
          catch (e) { term.writeln(`\x1b[31minvalid regex: ${e.message}\x1b[0m`); writePrompt(); return; }
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
          const note = `${filtered.length.toLocaleString('en-US')} of ${rows.length.toLocaleString('en-US')} rows match "${pattern}"`;
          const newResult = { columns: [...columns], rows: filtered, row_count: filtered.length, note };
          renderResult(newResult);
          if (filtered.length !== rows.length) prevResult = lastResult;
          lastResult = newResult;
        }
      }
      writePrompt();
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
          if (!sp) { term.writeln('\x1b[2musage: /not @<col> <pattern>\x1b[0m'); writePrompt(); return; }
          colIdx = resolveCol(sp[1], columns);
          if (colIdx < 0) {
            term.writeln(`\x1b[31mcolumn "${sp[1]}" not found\x1b[0m  \x1b[2mavailable: ${columns.join(', ')}\x1b[0m`);
            writePrompt(); return;
          }
          pattern = sp[2];
        }
        let re;
        const reMatch = pattern.match(/^\/(.+)\/([gimsvy]*)$/);
        if (reMatch) {
          try { re = new RegExp(reMatch[1], reMatch[2]); }
          catch (e) { term.writeln(`\x1b[31minvalid regex: ${e.message}\x1b[0m`); writePrompt(); return; }
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
          writePrompt(); return;
        }
        const note = `${excluded.toLocaleString('en-US')} of ${rows.length.toLocaleString('en-US')} rows excluded "${pattern}"`;
        const newResult = { columns: [...columns], rows: kept, row_count: kept.length, note };
        renderResult(newResult);
        prevResult = lastResult;
        lastResult = newResult;
      }
      writePrompt();
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
          const sampledResult = { columns: [...lastResult.columns], rows: sampled, row_count: sampled.length, note: `random sample of ${k.toLocaleString('en-US')}/${rows.length.toLocaleString('en-US')}` };
          renderResult(sampledResult);
          prevResult = lastResult;
          lastResult = sampledResult;
        }
      }
      writePrompt();
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
        const note = `${kept.length.toLocaleString('en-US')} unique row${kept.length !== 1 ? 's' : ''} (${removed.toLocaleString('en-US')} duplicate${removed !== 1 ? 's' : ''} removed)`;
        const newResult = { columns: [...lastResult.columns], rows: kept, row_count: kept.length, note };
        renderResult(newResult);
        if (removed > 0) { prevResult = lastResult; }
        lastResult = newResult;
      }
      writePrompt();
      return;
    }
    if (cmd === '/classes' || cmd.startsWith('/classes ')) {
      const pattern = cmd.slice(8).trim().toLowerCase();
      const all = classNames.length > 0 ? classNames
        : (await fetch(serverUrl + '/help').then(r => r.json()).then(d => {
            if (Array.isArray(d.classes)) classNames = d.classes;
            return classNames;
          }).catch(() => []));
      const filtered = all.filter(c => !c.startsWith('['));
      const matches = pattern ? filtered.filter(c => c.toLowerCase().includes(pattern)) : filtered;
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
          term.writeln(`\x1b[2m  ... ${(matches.length - CAP).toLocaleString('en-US')} more (showing ${CAP}; use /classes <pattern> to narrow)\x1b[0m`);
        }
        term.writeln(`\x1b[2m(${matches.length.toLocaleString('en-US')} class${matches.length !== 1 ? 'es' : ''})\x1b[0m`);
      }
      writePrompt();
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
          term.writeln(`\x1b[2m  ... ${(matches.length - CAP).toLocaleString('en-US')} more (showing ${CAP}; use /fields <pattern> to narrow)\x1b[0m`);
        }
        term.writeln(`\x1b[2m(${matches.length.toLocaleString('en-US')} field${matches.length !== 1 ? 's' : ''})\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd === '/set' || cmd.startsWith('/set ')) {
      const args = cmd.slice(4).trim().split(/\s+/);
      if (!args[0]) {
        // Print current settings
        term.writeln('\x1b[1mCurrent settings:\x1b[0m');
        const limitVal = settings.rowLimit === Infinity ? 'unlimited' : String(settings.rowLimit);
        term.writeln(`  \x1b[1mlimit\x1b[0m  \x1b[32m${limitVal.padEnd(12)}\x1b[0m  \x1b[2m(rows displayed; 0 = no cap)\x1b[0m`);
        term.writeln(`  \x1b[1mbytes\x1b[0m  \x1b[32m${(settings.bytesRaw ? 'raw' : 'human').padEnd(12)}\x1b[0m  \x1b[2m(raw = show numbers, human = 4.3 KiB)\x1b[0m`);
        term.writeln(`  \x1b[1mcolor\x1b[0m  \x1b[32m${(settings.color ? 'on' : 'off').padEnd(12)}\x1b[0m  \x1b[2m(ANSI colours in table cells)\x1b[0m`);
        term.writeln(`  \x1b[1mnull\x1b[0m   \x1b[32m${('"' + settings.nullStr + '"').padEnd(12)}\x1b[0m  \x1b[2m(null display string)\x1b[0m`);
        term.writeln('\x1b[2musage: /set limit 500 | /set bytes raw | /set bytes human | /set null ∅ | /set color off\x1b[0m');
      } else if (args[0] === 'limit') {
        const n = args[1] === '0' || args[1] === 'unlimited' || args[1] === 'none' ? 0 : parseInt(args[1], 10);
        if (isNaN(n) || n < 0) {
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
        if (lastResult && !isNaN(n) && n >= 0) {
          renderResult(lastResult);
          term.writeln(`\x1b[2m${lastResult.rows.length.toLocaleString('en-US')} rows\x1b[0m`);
        }
      } else if (args[0] === 'bytes') {
        if (args[1] === 'raw') { settings.bytesRaw = true; }
        else if (args[1] === 'human') { settings.bytesRaw = false; }
        else { term.writeln('\x1b[2musage: /set bytes raw|human\x1b[0m'); writePrompt(); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ bytes: ${settings.bytesRaw ? 'raw (numbers)' : 'human (e.g. 4.3 KiB)'}\x1b[0m`);
        if (lastResult) { renderResult(lastResult); }
      } else if (args[0] === 'null') {
        settings.nullStr = args[1] || 'null';
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ null: "${settings.nullStr}"\x1b[0m`);
        if (lastResult) { renderResult(lastResult); }
      } else if (args[0] === 'color') {
        if (args[1] === 'off' || args[1] === 'false') { settings.color = false; }
        else if (args[1] === 'on' || args[1] === 'true' || !args[1]) { settings.color = true; }
        else { term.writeln('\x1b[2musage: /set color on|off\x1b[0m'); writePrompt(); return; }
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        term.writeln(`\x1b[32m✓ color: ${settings.color ? 'on' : 'off'}\x1b[0m`);
        if (lastResult) { renderResult(lastResult); }
      } else {
        term.writeln(`\x1b[31munknown setting: ${args[0]}\x1b[0m  \x1b[2moptions: limit, bytes, color, null\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd === '/export' || cmd.startsWith('/export ')) {
      if (!lastResult) {
        term.writeln('\x1b[33m(no result — run a query first)\x1b[0m');
        writePrompt();
        return;
      }
      const fmt = cmd.slice(7).trim().toLowerCase() || 'csv';
      let text, mime, ext;
      if (fmt === 'csv') {
        const csvRow = row => row.map(c => {
          const s = c == null ? '' : String(c);
          return /[",\n\r]/.test(s) ? '"' + s.replace(/"/g, '""') + '"' : s;
        }).join(',');
        text = [lastResult.columns.map(c => /[",\n\r]/.test(c) ? '"' + c + '"' : c).join(',')]
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
      } else if (fmt === 'tsv') {
        text = [lastResult.columns.join('\t')]
          .concat(lastResult.rows.map(row =>
            row.map((cell, i) => fmtCell(cell, lastResult.columns[i])).join('\t')
          ))
          .join('\n');
        mime = 'text/tab-separated-values'; ext = 'tsv';
      } else {
        term.writeln(`\x1b[31merror: unknown format "${fmt}" — use csv, tsv, or json\x1b[0m`);
        writePrompt();
        return;
      }
      try {
        await navigator.clipboard.writeText(text);
        term.writeln(`\x1b[32m✓ copied ${lastResult.rows.length.toLocaleString('en-US')} rows as ${ext.toUpperCase()} to clipboard\x1b[0m`);
      } catch (_) {
        const blob = new Blob([text], { type: mime });
        const url = URL.createObjectURL(blob);
        const a = document.createElement('a');
        a.href = url; a.download = `query-result.${ext}`; a.click();
        URL.revokeObjectURL(url);
        term.writeln(`\x1b[32m✓ downloaded as query-result.${ext} (${lastResult.rows.length.toLocaleString('en-US')} rows)\x1b[0m`);
      }
      writePrompt();
      return;
    }
    if (cmd === '/history' || cmd.startsWith('/history ')) {
      const args = cmd.slice(8).trim();
      if (args === 'clear') {
        history.length = 0;
        localStorage.setItem(HISTORY_KEY, '[]');
        term.writeln('\x1b[32m✓ history cleared\x1b[0m');
        writePrompt();
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
          // Flatten multi-line entries to one display line
          const flat = h.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
          const truncated = flat.length > term.cols - 8 ? flat.slice(0, term.cols - 9) + '…' : flat;
          term.writeln(`\x1b[2m${num}\x1b[0m  \x1b[36m!${String(i + 1)}\x1b[0m  ${truncated}`);
        });
        if (realHistory.length > limit) {
          term.writeln(`\x1b[2m  … ${realHistory.length - limit} more — /history N to show more\x1b[0m`);
        }
        term.writeln(`\x1b[2m  Use !N to re-run entry N  (1 = most recent)  ·  /history clear to wipe\x1b[0m`);
      }
      writePrompt();
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
          writePrompt();
        } else {
          const recalled = realHistory[n];
          const flat = recalled.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
          const echo = flat.length > term.cols - PROMPT.length - 1
            ? flat.slice(0, term.cols - PROMPT.length - 2) + '…' : flat;
          term.writeln(`\x1b[2m↳ ${echo}\x1b[0m`);
          if (recalled.trimStart().startsWith('/')) {
            // History entry is a command — re-dispatch it fully
            await handleEnter(recalled);
          } else {
            await runQuery(recalled);
          }
        }
      } else {
        const name = cmd.slice(1);
        const bookmarks = JSON.parse(localStorage.getItem(BOOKMARKS_KEY) || '{}');
        if (bookmarks[name]) {
          const oql = bookmarks[name];
          const oqlFlat = oql.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
          const echo = oqlFlat.length > term.cols - PROMPT.length - 1
            ? oqlFlat.slice(0, term.cols - PROMPT.length - 2) + '…' : oqlFlat;
          term.writeln(`\x1b[2m↳ [${name}] ${echo}\x1b[0m`);
          await runQuery(oql);
        } else {
          term.writeln(`\x1b[31mno bookmark "!${name}" — use /bookmark to list\x1b[0m`);
          writePrompt();
        }
      }
      return;
    }
    if (cmd.startsWith('/run ') || cmd === '/run') {
      if (cmd === '/run') {
        if (namedQueries.length === 0) {
          term.writeln('\x1b[2m(no named queries loaded)\x1b[0m');
        } else {
          term.writeln(`\x1b[1mNamed queries\x1b[0m  \x1b[2m(/run <name>)\x1b[0m:`);
          let lastGroup = '';
          namedQueries.forEach(q => {
            if (q.group !== lastGroup) { lastGroup = q.group; term.writeln(`\r  \x1b[2m${q.group}\x1b[0m`); }
            const lock = (q.needs_retained && !hasRetained) ? '  \x1b[33m[needs full analysis]\x1b[0m' : '';
            term.writeln(`    \x1b[36m${q.name.padEnd(40)}\x1b[0m  \x1b[2m${q.display}\x1b[0m${lock}`);
          });
        }
        writePrompt();
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
        writePrompt();
        return;
      }
      if (q.needs_retained && !hasRetained) {
        term.writeln('\x1b[33mthis query requires full analysis — click \'Run Analysis\' in the toolbar first\x1b[0m');
        writePrompt();
        return;
      }
      const maxEcho = term.cols - 6;
      const oqlFlat = q.oql.replace(/\n/g, ' ↵ ').replace(/\s+/g, ' ');
      term.writeln(`\x1b[2m↳ ${oqlFlat.length > maxEcho ? oqlFlat.slice(0, maxEcho - 1) + '…' : oqlFlat}\x1b[0m`);
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
        '/forget', '/watch', '/analyze', '/status', '/clear', '/q', '/disconnect', '/help', '/examples',
        '/store', '/remove', '/star', '/unstar', '/viz', '/dashboard',
      ];
      const typed = cmdWord.slice(1);
      const close = ALL_CMDS.filter(c => {
        const n = c.slice(1);
        return n.startsWith(typed.slice(0, 2)) || typed.startsWith(n.slice(0, 2)) || n.includes(typed) || typed.includes(n);
      }).slice(0, 3);
      if (close.length > 0) {
        term.writeln(`\x1b[2m  did you mean: ${close.join(', ')}\x1b[0m`);
      }
      writePrompt();
      return;
    }
    await runQuery(full.trim());
  }

  // ── Viz overlay (floating treemap / histogram panel) ─────────────────────────
  let _vizOverlay = null;

  function openVizOverlay(slices, kind, valueLabel, srcQuery) {
    if (_vizOverlay) { _vizOverlay.remove(); _vizOverlay = null; }
    const overlay = document.createElement('div');
    overlay.className = 'viz-overlay';
    const header = document.createElement('div');
    header.className = 'viz-overlay-header';
    const title = document.createElement('span');
    title.className = 'viz-overlay-title';
    const titleBase = srcQuery ? srcQuery.replace(/\n/g, ' ').slice(0, 60) + (srcQuery.length > 60 ? '…' : '') : `${kind} — ${valueLabel || ''}`;
    title.textContent = titleBase;
    title.title = srcQuery || '';
    const actions = document.createElement('div');
    actions.className = 'viz-overlay-actions';
    const starBtn = document.createElement('button');
    starBtn.className = 'viz-action-btn';
    starBtn.title = 'Star this result';
    starBtn.textContent = '☆';
    starBtn.addEventListener('click', () => {
      if (!lastResult) return;
      const starred = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');
      const oql = srcQuery || history.find(h => !h.startsWith('/star') && !h.startsWith('/viz'));
      const label = (oql || 'unnamed').replace(/\n/g, ' ').slice(0, 50);
      const entry = { label, oql: oql || '', columns: lastResult.columns, rows: lastResult.rows.slice(0, 200), ts: Date.now() };
      const idx = starred.findIndex(e => e.label === label);
      if (idx >= 0) { starred[idx] = entry; } else { starred.unshift(entry); }
      if (starred.length > 20) starred.length = 20;
      localStorage.setItem(STARRED_KEY, JSON.stringify(starred));
      starBtn.textContent = '★';
      starBtn.style.color = '#f0d060';
      showToast(`Starred "${label}"`, 'success');
    });
    const closeBtn = document.createElement('button');
    closeBtn.className = 'viz-action-btn viz-close-btn';
    closeBtn.title = 'Close';
    closeBtn.textContent = '✕';
    closeBtn.addEventListener('click', () => { overlay.remove(); _vizOverlay = null; });
    actions.appendChild(starBtn);
    actions.appendChild(closeBtn);
    header.appendChild(title);
    header.appendChild(actions);
    const body = document.createElement('div');
    body.className = 'viz-overlay-body';
    overlay.appendChild(header);
    overlay.appendChild(body);
    document.getElementById('shell-screen').appendChild(overlay);
    _vizOverlay = overlay;
    // Use React bundle if available, otherwise ASCII fallback
    if (window.hprofRenderViz) {
      const fmtKind = (valueLabel || '').toLowerCase().includes('size') || (valueLabel || '').toLowerCase().includes('byte') ? 'bytes' : 'count';
      window.hprofRenderViz(body, slices, kind, fmtKind, 320);
    } else {
      _asciiViz(slices, kind);
    }
    // Drag to move
    let dragX = 0, dragY = 0;
    header.addEventListener('mousedown', e => {
      dragX = e.clientX - overlay.offsetLeft;
      dragY = e.clientY - overlay.offsetTop;
      const onMove = e2 => { overlay.style.left = (e2.clientX - dragX) + 'px'; overlay.style.top = (e2.clientY - dragY) + 'px'; };
      const onUp = () => { document.removeEventListener('mousemove', onMove); document.removeEventListener('mouseup', onUp); };
      document.addEventListener('mousemove', onMove);
      document.addEventListener('mouseup', onUp);
    });
  }

  function _asciiViz(slices, kind) {
    // Fallback ASCII bar chart when React bundle not loaded yet
    const max = slices.reduce((m, s) => Math.max(m, s.value), 0) || 1;
    const barW = 30;
    slices.slice(0, 20).forEach(s => {
      const filled = Math.round((s.value / max) * barW);
      const bar = '█'.repeat(filled) + '░'.repeat(barW - filled);
      term.writeln(`  \x1b[36m${bar}\x1b[0m  ${s.name}`);
    });
  }

  // ── Dashboard ─────────────────────────────────────────────────────────────────
  let _dashOverlay = null;

  function openDashboard() {
    if (_dashOverlay) { _dashOverlay.remove(); _dashOverlay = null; }
    const starred = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');
    const overlay = document.createElement('div');
    overlay.className = 'dashboard-overlay';
    const header = document.createElement('div');
    header.className = 'dashboard-header';
    const htitle = document.createElement('span');
    htitle.className = 'dashboard-title';
    htitle.textContent = '★ Dashboard';
    const closeBtn = document.createElement('button');
    closeBtn.className = 'viz-action-btn viz-close-btn';
    closeBtn.textContent = '✕';
    closeBtn.title = 'Close';
    closeBtn.addEventListener('click', () => { overlay.remove(); _dashOverlay = null; });
    header.appendChild(htitle);
    header.appendChild(closeBtn);
    overlay.appendChild(header);
    if (starred.length === 0) {
      const empty = document.createElement('p');
      empty.className = 'dashboard-empty';
      empty.textContent = 'No starred results yet. Run a query and use /star or click ☆ on a viz overlay.';
      overlay.appendChild(empty);
    } else {
      const grid = document.createElement('div');
      grid.className = 'dashboard-grid';
      starred.forEach((entry, ei) => {
        const card = document.createElement('div');
        card.className = 'dashboard-card';
        const cardHdr = document.createElement('div');
        cardHdr.className = 'dashboard-card-header';
        const cardTitle = document.createElement('span');
        cardTitle.className = 'dashboard-card-title';
        cardTitle.textContent = entry.label;
        cardTitle.title = entry.oql;
        const cardActions = document.createElement('div');
        cardActions.className = 'dashboard-card-actions';
        const rerunBtn = document.createElement('button');
        rerunBtn.className = 'dashboard-card-btn';
        rerunBtn.title = 'Re-run query';
        rerunBtn.textContent = '↺';
        rerunBtn.addEventListener('click', () => {
          overlay.remove(); _dashOverlay = null;
          if (window._hprofRunQuery && entry.oql) window._hprofRunQuery(entry.oql);
        });
        const delBtn = document.createElement('button');
        delBtn.className = 'dashboard-card-btn';
        delBtn.title = 'Remove from dashboard';
        delBtn.textContent = '×';
        delBtn.addEventListener('click', () => {
          const s = JSON.parse(localStorage.getItem(STARRED_KEY) || '[]');
          s.splice(ei, 1);
          localStorage.setItem(STARRED_KEY, JSON.stringify(s));
          card.remove();
          if (grid.children.length === 0) openDashboard();
        });
        cardActions.appendChild(rerunBtn);
        cardActions.appendChild(delBtn);
        cardHdr.appendChild(cardTitle);
        cardHdr.appendChild(cardActions);
        const vizContainer = document.createElement('div');
        vizContainer.className = 'dashboard-viz';
        card.appendChild(cardHdr);
        card.appendChild(vizContainer);
        grid.appendChild(card);
        // Render treemap for this card using saved rows
        if (window.hprofRenderViz && entry.columns && entry.rows) {
          const cols = Array.isArray(entry.columns) ? entry.columns.map(c => (typeof c === 'string' ? c : c.name)) : [];
          // Find first string-ish and first numeric col
          const numIdx = entry.rows[0] ? cols.findIndex((_, i) => {
            const v = entry.rows[0][i];
            return typeof v === 'number' || (typeof v === 'object' && v && (v.kind === 'int' || v.kind === 'float' || v.kind === 'i64'));
          }) : -1;
          const labelIdx = numIdx > 0 ? 0 : (numIdx === 0 ? 1 : 0);
          const valueIdx = numIdx >= 0 ? numIdx : cols.length - 1;
          const slices = entry.rows
            .map(r => {
              const lc = r[labelIdx]; const vc = r[valueIdx];
              const name = lc == null ? '' : (typeof lc === 'object' ? String(lc.v ?? lc) : String(lc));
              const value = vc == null ? 0 : (typeof vc === 'object' ? Number(vc.v) || 0 : Number(vc) || 0);
              return { name, value };
            })
            .filter(s => s.value > 0)
            .slice(0, 40);
          if (slices.length > 0) {
            const fmtKind = (cols[valueIdx] || '').toLowerCase().includes('byte') || (cols[valueIdx] || '').toLowerCase().includes('size') ? 'bytes' : 'count';
            window.hprofRenderViz(vizContainer, slices, 'treemap', fmtKind, 180);
          } else {
            vizContainer.textContent = '(no numeric data)';
          }
        }
      });
      overlay.appendChild(grid);
    }
    document.getElementById('shell-screen').appendChild(overlay);
    _dashOverlay = overlay;
  }

  // ── Star-on-hover: show ☆ button after each result output ────────────────────
  // We wrap renderResult to emit a clickable star button into the DOM after
  // the terminal output. The star attaches to the query that produced the result.
  const _origRenderResult = null; // set below after renderResult is defined


  let currentAbort = null;  // AbortController for in-flight query
  let lastResult = null;    // { columns, rows } of last successful query for /export
  let prevResult = null;    // single-level undo: saved before result-mutating commands
  let watchTimer = null;    // setInterval handle for /watch
  let currentRowIdx = 0;    // 0-based row cursor for /row next/prev
  const resultLog = [];     // ring buffer of { query, result } for /viz @N references
  const RESULT_LOG_MAX = 20;

  function pushResultLog(query, result) {
    resultLog.unshift({ query, result });
    if (resultLog.length > RESULT_LOG_MAX) resultLog.length = RESULT_LOG_MAX;
  }

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
      term.writeln(`\x1b[33m-- showing ${settings.rowLimit.toLocaleString('en-US')} of ${rows.length.toLocaleString('en-US')} rows (use \`/set limit 0\` or \`/set limit N\` to change) --\x1b[0m`);
    }
    if (r.note) {
      term.writeln(`\x1b[33m-- ${r.note} --\x1b[0m`);
    }
    return { colNames, adjW, isNumeric };
  }

  function handleQueryResponse(data, elapsedMs, showHint, oql) {
    const fmtElapsed = ms => ms < 1 ? `${(ms * 1000).toFixed(0)}µs` : ms < 1000 ? `${ms.toFixed(1)}ms` : `${(ms / 1000).toFixed(2)}s`;
    if (!data.ok) {
      const msg = data.error?.message || JSON.stringify(data.error) || 'unknown error';
      const report = data.error?.report;
      if (report) {
        report.split('\n').forEach(l => term.writeln(l));
      } else {
        // Try to extract position from "... at 1:COL" pattern and show a pointer
        const posMatch = msg.match(/\bat\s+\d+:(\d+)/);
        const col = posMatch ? parseInt(posMatch[1], 10) - 1 : -1;
        // Split off any "— hint" part after an em-dash
        const dashIdx = msg.indexOf(' — ');
        const mainMsg = dashIdx >= 0 ? msg.slice(0, dashIdx) : msg;
        const hint = dashIdx >= 0 ? msg.slice(dashIdx + 3) : null;
        term.writeln(`\x1b[31merror:\x1b[0m ${mainMsg}`);
        if (hint) term.writeln(`\x1b[2m  hint: ${hint}\x1b[0m`);
        // Show a pointer under the problematic token in the current line
        if (col >= 0 && line) {
          const promptLen = PROMPT.replace(/\x1b\[[^m]*m/g, '').length;
          const pointer = ' '.repeat(promptLen + col) + '\x1b[31m^\x1b[0m';
          term.writeln(pointer);
        }
      }
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
      const queryMs = (r.elapsed_ms != null) ? Number(r.elapsed_ms) : elapsedMs;
      if (r.error) {
        term.writeln(`\x1b[31merror: ${r.error}\x1b[0m`);
      } else if (r.columns && r.columns.length > 0) {
        const colNames = r.columns.map(c => c.name || String(c));
        const rows = r.rows || [];
        renderResult(r);
        prevResult = null;
        lastResult = { columns: colNames, rows, note: r.note, truncated: r.truncated, row_count: r.row_count };
        pushResultLog(oql, lastResult);
        currentRowIdx = 0;
        const trunc = r.truncated ? `  \x1b[33m[capped at ${Number(r.row_count).toLocaleString('en-US')} rows — add LIMIT N or increase with LIMIT 0 for all]\x1b[0m` : '';
        const elapsedFmt = fmtElapsed(queryMs);
        const elapsedColor = queryMs > 1000 ? '\x1b[31m' : queryMs > 300 ? '\x1b[33m' : '\x1b[2m';
        const ts = new Date().toLocaleTimeString('en-GB', { hour12: false });
        term.writeln(`${elapsedColor}${Number(r.row_count).toLocaleString('en-US')} row${r.row_count !== 1 ? 's' : ''}, ${elapsedFmt}\x1b[0m\x1b[2m  [${ts}]\x1b[0m${trunc}`);
        if (rows.length > 20 && showHint) {
          const hasNumeric = colNames.some((_, i) => {
            const sample = rows.find(row => row[i] !== null && row[i] !== undefined);
            return sample ? isNumericKind(sample[i]) : false;
          });
          const statHint = hasNumeric ? '  /stats <col>' : '';
          term.writeln(`\x1b[2m  /filter <text|/re/>  /sort [-]<col>  /select <col>…  /pivot <col>  /row [N]${statHint}  /export [csv|tsv|json]  /viz  /star  /store\x1b[0m`);
        }
      } else {
        prevResult = null;
        term.writeln(JSON.stringify(r, null, 2).split('\n').slice(0, 40).join('\r\n'));
        term.writeln(`\x1b[2m${fmtElapsed(queryMs)}\x1b[0m`);
      }
    }
    writePrompt();
  }

  async function runQuery(oql, { showHint = true } = {}) {
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
      // WASM mode: dispatch directly to HprofSession (no network)
      if (wasmSession) {
        await new Promise(resolve => setTimeout(resolve, 0));  // yield to let spinner render
        const rawJson = wasmSession.query(oql);
        clearInterval(spinTimer);
        currentAbort = null;
        term.write('\r\x1b[K');
        const elapsedMs = performance.now() - t0;
        const data = JSON.parse(rawJson);
        // Normalize to the same shape the server returns
        if (!data.ok) {
          data.error = data.error || { message: 'unknown error' };
        } else {
          const r = data.result;
          r.elapsed_ms = elapsedMs;
          // WASM returns column names as plain strings; wrap for renderResult compat
          if (r.columns && r.columns.length > 0 && typeof r.columns[0] === 'string') {
            r.columns = r.columns.map(n => ({ name: n }));
          }
        }
        return handleQueryResponse(data, elapsedMs, showHint, oql);
      }

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
      const elapsedMs = performance.now() - t0;
      let data;
      try {
        data = await res.json();
      } catch (_) {
        term.writeln(`\x1b[31merror: server returned non-JSON (HTTP ${res.status})\x1b[0m`);
        writePrompt();
        return;
      }

      handleQueryResponse(data, elapsedMs, showHint, oql);
    } catch (e) {
      clearInterval(spinTimer);
      currentAbort = null;
      term.write('\r\x1b[K');
      if (e.name === 'AbortError') {
        term.writeln('\x1b[2m(cancelled)\x1b[0m');
      } else {
        term.writeln(`\x1b[31merror: ${e.message}\x1b[0m`);
      }
      writePrompt();
    }
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
    h('My Queries & Dashboard');
    c('/store [name]',           '— save last query permanently in sidebar (persists across reloads)');
    c('/remove <name>',          '— delete a stored query from sidebar');
    c('/star [label]',           '— star last query+result for the dashboard');
    c('/unstar <label>',         '— remove a starred result');
    c('/dashboard',              '— open starred-results dashboard panel');
    c('/viz [@N] [kind] [l] [v]','— visualise last (or @N previous) result as treemap|histogram; auto-detects columns');
    c('',                        '  @1=last  @2=second-to-last … (up to 20)  Tab completes kind + column names');
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
    c('/examples [category]',    '— runnable OQL examples: basic, groupby, subquery, viz, misc');
    term.writeln('');
    term.writeln('\x1b[1;33mKeyboard shortcuts\x1b[0m');
    term.writeln('  Tab       complete  ·  Ctrl+R  reverse history search  ·  ↑/↓  history');
    term.writeln('  Ctrl+A/E  line start/end  ·  Alt+←/→  word left/right');
    term.writeln('  Ctrl+K/W/U  kill  ·  Ctrl+Y  yank  ·  Ctrl+C  abort query  ·  Ctrl+L  clear');
    term.writeln('  \\  at end of line  →  continue query on next line');
    term.writeln('  Ctrl+Shift+R  toggle between shell and report screen');
    if (namedQueries.length > 0) {
      term.writeln('');
      term.writeln('\x1b[33mNamed queries\x1b[0m  \x1b[2m(use /run <name> or click sidebar)\x1b[0m');
      let cur = '';
      namedQueries.forEach(q => {
        if (q.group !== cur) {
          cur = q.group;
          term.writeln(`\r  \x1b[2m${cur}\x1b[0m`);
        }
        const lock = (q.needs_retained && !hasRetained) ? '  \x1b[33m[needs full analysis]\x1b[0m' : '';
        term.writeln(`    \x1b[36m${q.name.padEnd(40)}\x1b[0m  \x1b[2m${q.display}\x1b[0m${lock}`);
      });
    }
    term.writeln('');
  }

  async function printOqlRef() {
    // In WASM mode HprofSession.oql_help() provides the reference offline.
    // In server mode we fetch from /help (which also includes dump class names).
    let ref_;
    try {
      if (serverUrl) {
        ref_ = await fetch(serverUrl + '/help').then(r => r.json());
        term.writeln('\r\n\x1b[1mOQL Language Reference\x1b[0m  \x1b[2m(from server /help)\x1b[0m');
      } else {
        ref_ = JSON.parse(activeHprof.oql_help());
        term.writeln('\r\n\x1b[1mOQL Language Reference\x1b[0m  \x1b[2m(built-in)\x1b[0m');
      }
    } catch (e) {
      term.writeln(`\x1b[31mcould not load OQL reference: ${e.message}\x1b[0m`);
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
    section('Clauses / modifiers', ref_.reserved);
    section('Aggregate functions', ref_.aggregates);
    section('Scalar functions', ref_.functions);
    section('Methods (on objects)', ref_.methods);
    section('Attributes (@ prefix)', ref_.attributes);

    term.writeln('');
    term.writeln('  \x1b[33mGrammar summary\x1b[0m');
    const g = (s) => term.writeln('  \x1b[2m' + s + '\x1b[0m');
    g('SELECT [DISTINCT] [OBJECTS] <expr, …> [AS RETAINED SET]');
    g('  FROM [OBJECTS] <class | "regex" | INSTANCEOF class | (subquery)> [alias]');
    g('  [WHERE <pred>]  [GROUP BY <expr>]  [HAVING <pred>]');
    g('  [ORDER BY <expr> [ASC|DESC]]  [LIMIT n]');
    g('[UNION | INTERSECT | EXCEPT  <select> …]');

    term.writeln('');
    term.writeln('  \x1b[33mQuick examples\x1b[0m  \x1b[2m(type /examples for more)\x1b[0m');
    const ex = (q) => term.writeln('  \x1b[36m' + q + '\x1b[0m');
    ex('SELECT * FROM java.lang.String LIMIT 10');
    ex('SELECT classof(s) AS cls, COUNT(*) AS n FROM INSTANCEOF java.lang.Object s GROUP BY classof(s) ORDER BY n DESC LIMIT 15');
    ex('SELECT s.@retainedHeapSize FROM java.lang.Thread s ORDER BY s.@retainedHeapSize DESC LIMIT 5');
    ex('SELECT toString(s) AS val, COUNT(*) AS n FROM java.lang.String s GROUP BY toString(s) HAVING n > 5 ORDER BY n DESC LIMIT 20');
    term.writeln('');
    term.writeln('  \x1b[2mTip: /describe <ClassName>  ·  /examples [basic|groupby|subquery|viz|misc]  ·  Tab to complete\x1b[0m');
    term.writeln('');
  }

  // OQL example categories shown by /examples
  const OQL_EXAMPLES = [
    { cat: 'basic', title: 'Basic SELECT / WHERE / ORDER BY / LIMIT', examples: [
      { desc: 'All String objects (first 10)',
        oql: 'SELECT * FROM java.lang.String LIMIT 10' },
      { desc: 'Class and shallow size, sorted biggest first',
        oql: 'SELECT @displayName AS class, @usedHeapSize AS bytes FROM INSTANCEOF java.lang.Object ORDER BY bytes DESC LIMIT 20' },
      { desc: 'All subclasses of Collection',
        oql: 'SELECT * FROM INSTANCEOF java.util.Collection LIMIT 20' },
      { desc: 'Objects matching a class-name regex',
        oql: 'SELECT @displayName, @usedHeapSize FROM "java\\.util\\..*" LIMIT 20' },
      { desc: 'Strings longer than 100 chars',
        oql: 'SELECT toString(s) AS value, @usedHeapSize AS bytes FROM java.lang.String s ORDER BY bytes DESC LIMIT 20' },
      { desc: 'Distinct class names (DISTINCT)',
        oql: 'SELECT DISTINCT @displayName FROM java.lang.Thread' },
      { desc: 'Retained heap for threads (needs full analysis)',
        oql: 'SELECT @displayName AS thread, @retainedHeapSize AS ret_bytes FROM java.lang.Thread ORDER BY ret_bytes DESC' },
      { desc: 'BETWEEN predicate (moderate-sized objects)',
        oql: 'SELECT @displayName, @usedHeapSize AS bytes FROM INSTANCEOF java.lang.Object WHERE @usedHeapSize BETWEEN 500 AND 5000 LIMIT 20' },
    ]},
    { cat: 'groupby', title: 'GROUP BY / HAVING / aggregates', examples: [
      { desc: 'Instance count per class (top 15)',
        oql: 'SELECT classof(s) AS cls, COUNT(*) AS n FROM INSTANCEOF java.lang.Object s GROUP BY classof(s) ORDER BY n DESC LIMIT 15' },
      { desc: 'Classes with >100 instances',
        oql: 'SELECT @displayName AS cls, COUNT(*) AS n FROM INSTANCEOF java.lang.Object GROUP BY @displayName HAVING n > 100 ORDER BY n DESC LIMIT 20' },
      { desc: 'Duplicate String values (top by count)',
        oql: 'SELECT toString(s) AS value, COUNT(*) AS n FROM java.lang.String s GROUP BY toString(s) HAVING n > 1 ORDER BY n DESC LIMIT 30' },
      { desc: 'Total shallow heap per class',
        oql: 'SELECT @displayName AS cls, SUM(@usedHeapSize) AS total FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY total DESC LIMIT 15' },
      { desc: 'Size bucket distribution (CASE WHEN)',
        oql: 'SELECT CASE WHEN @usedHeapSize>10000 THEN "large" WHEN @usedHeapSize>1000 THEN "medium" ELSE "small" END AS bucket, COUNT(*) AS n FROM INSTANCEOF java.lang.Object GROUP BY CASE WHEN @usedHeapSize>10000 THEN "large" WHEN @usedHeapSize>1000 THEN "medium" ELSE "small" END ORDER BY n DESC' },
      { desc: 'PERCENTILE — 95th percentile shallow size per class',
        oql: 'SELECT @displayName AS cls, PERCENTILE(@usedHeapSize,95) AS p95 FROM INSTANCEOF java.lang.Object GROUP BY @displayName HAVING COUNT(*) > 50 ORDER BY p95 DESC LIMIT 15' },
      { desc: 'COALESCE — replace null with placeholder',
        oql: 'SELECT COALESCE(toString(s), "<null>") AS val FROM java.lang.String s LIMIT 20' },
    ]},
    { cat: 'subquery', title: 'Subqueries / UNION / INTERSECT / EXCEPT / EXISTS', examples: [
      { desc: 'Subquery in FROM (objects held by HashMap)',
        oql: 'SELECT @displayName, @usedHeapSize FROM (SELECT * FROM java.util.HashMap) LIMIT 10' },
      { desc: 'IN predicate — addresses from subquery',
        oql: 'SELECT * FROM java.lang.Object WHERE @objectAddress IN (SELECT @objectAddress FROM java.lang.Thread) LIMIT 10' },
      { desc: 'EXISTS — run only when leaked connections exist',
        oql: 'SELECT COUNT(*) FROM java.lang.Object WHERE EXISTS (SELECT * FROM java.net.Socket s WHERE s.closed = false)' },
      { desc: 'UNION — combine two class sets',
        oql: 'SELECT @displayName, @usedHeapSize FROM java.util.HashMap UNION SELECT @displayName, @usedHeapSize FROM java.util.ArrayList ORDER BY @usedHeapSize DESC LIMIT 20' },
      { desc: 'INTERSECT — class names in two packages',
        oql: 'SELECT @displayName FROM "com\\.example\\.cache\\..*" INTERSECT SELECT @displayName FROM "com\\.example\\..*"' },
      { desc: 'EXCEPT — exclude subclass from parent scan',
        oql: 'SELECT @displayName, COUNT(*) AS n FROM INSTANCEOF java.util.AbstractList GROUP BY @displayName EXCEPT SELECT @displayName, COUNT(*) AS n FROM java.util.ArrayList GROUP BY @displayName ORDER BY n DESC' },
    ]},
    { cat: 'viz', title: 'Visualization directives (-- @viz)', examples: [
      { desc: 'Histogram of class instance counts',
        oql: '-- @viz histogram\nSELECT classof(s) AS cls, COUNT(*) AS n FROM INSTANCEOF java.lang.Object s GROUP BY classof(s) ORDER BY n DESC LIMIT 15' },
      { desc: 'Treemap of heap by class (shallow size)',
        oql: '-- @viz treemap cap=20\nSELECT @displayName AS cls, SUM(@usedHeapSize) AS bytes FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY bytes DESC LIMIT 20' },
      { desc: 'Treemap of retained heap by class (needs full analysis)',
        oql: '-- @viz treemap\nSELECT @displayName AS cls, @retainedHeapSize AS ret_bytes FROM INSTANCEOF java.lang.Object ORDER BY ret_bytes DESC LIMIT 30' },
      { desc: 'Named chart block',
        oql: '-- @viz histogram name="String duplicates" title="Top duplicate String values"\nSELECT toString(s) AS value, COUNT(*) AS n FROM java.lang.String s GROUP BY toString(s) HAVING n > 1 ORDER BY n DESC LIMIT 20' },
    ]},
    { cat: 'misc', title: 'Attributes, functions, array access, field paths', examples: [
      { desc: 'Object address and hex address',
        oql: 'SELECT @objectId AS id, toHex(@objectAddress) AS addr, @displayName FROM java.lang.Thread' },
      { desc: 'Field path traversal',
        oql: 'SELECT s.value AS chars, @usedHeapSize AS bytes FROM java.lang.String s ORDER BY bytes DESC LIMIT 10' },
      { desc: 'classof() — class object attributes',
        oql: 'SELECT classof(s) AS cls, @usedHeapSize AS obj_bytes FROM java.lang.Thread s' },
      { desc: 'Array element access (first element)',
        oql: 'SELECT @objectId, value[0] AS first FROM byte[] LIMIT 10' },
      { desc: 'Array slice',
        oql: 'SELECT @objectId, value[0:4] AS slice FROM char[] LIMIT 10' },
      { desc: 'Dominator chain for largest object (needs full analysis)',
        oql: 'SELECT dominators(o) FROM INSTANCEOF java.lang.Object o ORDER BY @retainedHeapSize DESC LIMIT 1' },
      { desc: 'GC roots (@GCRoots attribute)',
        oql: 'SELECT @GCRoots FROM java.lang.Thread LIMIT 5' },
      { desc: 'Inbound / outbound reference counts (needs full analysis)',
        oql: 'SELECT @displayName, @inbounds AS refs_in, @outbounds AS refs_out FROM java.lang.Thread' },
    ]},
  ];

  function printExamples(filter) {
    const cats = filter
      ? OQL_EXAMPLES.filter(c => c.cat === filter || c.title.toLowerCase().includes(filter.toLowerCase()))
      : OQL_EXAMPLES;
    if (!cats.length) {
      term.writeln(`\x1b[33mNo examples matched "${filter}". Categories: ${OQL_EXAMPLES.map(c => c.cat).join(', ')}\x1b[0m`);
      return;
    }
    term.writeln('');
    term.writeln('\x1b[1mOQL Examples\x1b[0m  \x1b[2m(click or paste a query, then press Enter)\x1b[0m');
    for (const { title, examples } of cats) {
      term.writeln(`\r\n  \x1b[1;33m${title}\x1b[0m`);
      for (const { desc, oql } of examples) {
        term.writeln(`  \x1b[2m${desc}\x1b[0m`);
        // Syntax-highlight each line of the query, same as the input line
        const lines = oql.split('\n');
        for (const ln of lines) {
          term.writeln('    ' + highlightOql(ln) + '\x1b[0m');
        }
      }
    }
    term.writeln('');
    term.writeln(`  \x1b[2m/examples basic  |  /examples groupby  |  /examples subquery  |  /examples viz  |  /examples misc\x1b[0m`);
    term.writeln('');
  }

  // ── Input handler — handles both paste and normal character input ────────────
  // onData fires for ALL input (keystrokes and paste). onKey fires only for
  // keyboard events, NOT for paste. We insert all printable chars here so that
  // single-char pastes are not silently dropped.
  term.onData(data => {
    // During isearch, onKey handles char input for the search query — skip here.
    if (isearching) return;
    // Strip control bytes (0x00-0x1F, 0x7F-0x9F) and ANSI escape sequences.
    const printable = data
        .replace(/[\x00-\x1F\x7F-\x9F]|\x1B\[[\x20-\x3F]*[\x40-\x7E]/g, '')
        .replace(/^\[(?:[ABCDHF]|[0-9]+~)/, '');
    if (!printable) return;
    line = line.slice(0, cursorPos) + printable + line.slice(cursorPos);
    cursorPos += printable.length;
    ghostText = '';       // clear stale ghost before recomputing
    updateGhost();
    redrawLine();
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
      // Accept popover selection if open
      if (popover.classList.contains('visible') && popSel >= 0) {
        popAccept(popSel);
        return;
      }
      popHide();
      const text = line;
      line = '';
      cursorPos = 0;
      ghostText = '';
      histIdx = -1;
      histSavedLine = '';
      term.writeln('');
      void handleEnter(text);
      return;
    }

    if (code === 'Backspace') {
      if (cursorPos > 0) {
        line = line.slice(0, cursorPos - 1) + line.slice(cursorPos);
        cursorPos--;
        ghostText = '';
        updateGhost();
        redrawLine();
      }
      return;
    }

    if (code === 'Delete') {
      if (cursorPos < line.length) {
        line = line.slice(0, cursorPos) + line.slice(cursorPos + 1);
        redrawLine();
        ghostText = '';
        updateGhost();
        redrawLine();
      }
      return;
    }

    if (code === 'Escape') {
      if (popover.classList.contains('visible')) { popHide(); return; }
      return;
    }

    if (code === 'Tab') {
      ev.preventDefault();
      handleTab();
      return;
    }

    if (code === 'ArrowUp') {
      // Navigate popover if open
      if (popover.classList.contains('visible')) {
        const newSel = popSel <= 0 ? popItems.length - 1 : popSel - 1;
        popSelect(newSel);
        return;
      }
      if (histIdx + 1 < history.length) {
        if (histIdx === -1) histSavedLine = line;  // save draft before entering history
        histIdx++;
        line = history[histIdx];
        cursorPos = line.length;
        ghostText = '';
        popHide();
        redrawLine();
      }
      return;
    }

    if (code === 'ArrowDown') {
      // Navigate popover if open
      if (popover.classList.contains('visible')) {
        const newSel = popSel >= popItems.length - 1 ? 0 : popSel + 1;
        popSelect(newSel);
        return;
      }
      if (histIdx > 0) {
        histIdx--;
        line = history[histIdx];
        cursorPos = line.length;
        ghostText = '';
        popHide();
        redrawLine();
      } else if (histIdx === 0) {
        histIdx = -1;
        line = histSavedLine;
        cursorPos = line.length;
        ghostText = '';
        popHide();
        redrawLine();
      }
      return;
    }

    if (code === 'ArrowLeft') {
      if (ev.ctrlKey || ev.altKey) {
        // Jump to previous word boundary
        let p = cursorPos;
        while (p > 0 && line[p - 1] === ' ') p--;
        while (p > 0 && line[p - 1] !== ' ') p--;
        if (p !== cursorPos) { cursorPos = p; ghostText = ''; redrawLine(); }
      } else if (cursorPos > 0) {
        cursorPos--;
        ghostText = '';
        term.write('\x1b[D');
      }
      return;
    }

    if (code === 'ArrowRight') {
      // Accept ghost text when at end of line
      if (!ev.ctrlKey && !ev.altKey && cursorPos === line.length && ghostText) {
        line += ghostText;
        cursorPos = line.length;
        ghostText = '';
        redrawLine();
        return;
      }
      if (ev.ctrlKey || ev.altKey) {
        // Jump to next word boundary
        let p = cursorPos;
        while (p < line.length && line[p] !== ' ') p++;
        while (p < line.length && line[p] === ' ') p++;
        if (p !== cursorPos) { cursorPos = p; ghostText = ''; updateGhost(); redrawLine(); }
      } else if (cursorPos < line.length) {
        cursorPos++;
        term.write('\x1b[C');
      }
      return;
    }

    if (code === 'Home' || (ev.ctrlKey && code === 'a')) {
      if (cursorPos > 0) { cursorPos = 0; ghostText = ''; redrawLine(); }
      return;
    }

    if (code === 'End' || (ev.ctrlKey && code === 'e')) {
      // Accept ghost text when at end of line
      if (cursorPos === line.length && ghostText) {
        line += ghostText;
        cursorPos = line.length;
        ghostText = '';
        redrawLine();
        return;
      }
      if (cursorPos < line.length) { cursorPos = line.length; ghostText = ''; updateGhost(); redrawLine(); }
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
          writePrompt();
        }
      } else if (watchTimer) {
        clearInterval(watchTimer);
        watchTimer = null;
        term.writeln('^C');
        term.writeln('\x1b[32m✓ watch stopped\x1b[0m');
        writePrompt();
      } else {
        const hadPending = pendingLines.length > 0;
        term.writeln('^C');
        line = '';
        cursorPos = 0;
        histIdx = -1;
        histSavedLine = '';
        pendingLines = [];
        if (hadPending) {
          term.writeln('\x1b[2m(multi-line input discarded)\x1b[0m');
        }
        ghostText = '';
        popHide();
        writePrompt();
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
        ghostText = '';
        popHide();
        redrawLine();
      }
      return;
    }

    if (ev.ctrlKey && code === 'k') {
      if (cursorPos < line.length) {
        killRing = line.slice(cursorPos);
        line = line.slice(0, cursorPos);
        ghostText = '';
        updateGhost();
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
        ghostText = '';
        updateGhost();
        redrawLine();
      }
      return;
    }

    if (ev.ctrlKey && code === 'y') {
      // Yank (paste) kill ring
      if (killRing) {
        line = line.slice(0, cursorPos) + killRing + line.slice(cursorPos);
        cursorPos += killRing.length;
        ghostText = '';
        updateGhost();
        redrawLine();
      }
      return;
    }
    // Printable char insertion is now handled in onData (above), which fires
    // for both keystrokes and paste. Nothing to do here.
  });
}

// Expose URL-based loader for automated testing / Playwright.
// Usage: await window._hprofLoadUrl('/path/to/dump.hprof', 'oql-shell')
// mode: 'oql-shell' (default) | 'report'
window._hprofLoadUrl = async (url, mode = 'oql-shell') => {
  await loadSampleDump({ path: url, name: url.split('/').pop().replace(/\.hprof$/, '') });
  const modeButtons = document.getElementById('mode-buttons');
  if (modeButtons && modeButtons.style.display !== 'none') {
    const btnId = mode === 'report' ? 'btn-report' : 'btn-oql-shell';
    document.getElementById(btnId)?.click();
  }
};
