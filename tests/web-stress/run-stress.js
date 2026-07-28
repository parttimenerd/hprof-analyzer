#!/usr/bin/env node
// run-stress.js — Playwright stress test for dist/hprof-analyzer-browser.html
// Usage: node tests/web-stress/run-stress.js [--dump <path>]
//        node tests/web-stress/run-stress.js          (runs all dumps in dumps/)

import { chromium } from 'playwright';
import { readdir, writeFile, stat } from 'fs/promises';
import { resolve, join, basename } from 'path';
import { fileURLToPath } from 'url';
import { performance } from 'perf_hooks';

const ROOT = resolve(fileURLToPath(import.meta.url), '../../..');
const HTML  = join(ROOT, 'dist', 'hprof-analyzer-browser.html');
const DUMPS = join(ROOT, 'tests', 'web-stress', 'dumps');
const OUT   = join(ROOT, 'tests', 'web-stress', 'results.md');

const QUERIES = [
  { name: 'top-classes',  oql: 'SELECT classof(x), COUNT(*) AS n FROM java.lang.Object x GROUP BY classof(x) ORDER BY n DESC LIMIT 10' },
  { name: 'string-count', oql: 'SELECT COUNT(*) FROM java.lang.String' },
];

const LOAD_TIMEOUT_MS  = 10 * 60 * 1000;
const QUERY_TIMEOUT_MS = 2 * 60 * 1000;

async function getDumps() {
  const args = process.argv.slice(2);
  const idx = args.indexOf('--dump');
  if (idx !== -1) return [resolve(args[idx + 1])];
  const files = await readdir(DUMPS);
  return files
    .filter(f => f.endsWith('.hprof'))
    .sort()
    .map(f => join(DUMPS, f));
}

async function getFileSizeMB(path) {
  const s = await stat(path);
  return (s.size / 1048576).toFixed(1);
}

async function testDump(browser, dumpPath) {
  const name = basename(dumpPath);
  const sizeMB = await getFileSizeMB(dumpPath);
  console.log(`\n=== ${name} (${sizeMB} MB on disk) ===`);

  const context = await browser.newContext();
  const page    = await context.newPage();

  const pageErrors = [];
  page.on('console', msg => { if (msg.type() === 'error') pageErrors.push(msg.text()); });
  page.on('pageerror', err => pageErrors.push(String(err)));

  const result = {
    dump: name,
    sizeMB,
    loadMs: null,
    jsHeapMB: null,
    queries: [],
    error: null,
    outcome: 'PASS',
  };

  try {
    await page.goto('file://' + HTML);

    // Wait for WASM to initialise — window._hprofWasmReady() is our exposed shim
    // Playwright 1.x: waitForFunction(fn, arg, options) — timeout must be the 3rd arg
    await page.waitForFunction(() => typeof window._hprofWasmReady === 'function' && window._hprofWasmReady() === true, undefined, { timeout: 30_000 });

    const t0 = performance.now();
    await page.setInputFiles('#file-input', dumpPath);

    await page.waitForSelector('#mode-buttons', { state: 'visible', timeout: 10_000 });
    await page.click('#btn-oql-shell');

    // Wait until window._hprofQueryDirect is callable (set when shell initialises with a session)
    // Playwright 1.x: waitForFunction(fn, arg, options) — timeout must be the 3rd arg
    await page.waitForFunction(
      () => typeof window._hprofQueryDirect === 'function' &&
            window._hprofQueryDirect !== null &&
            document.getElementById('shell-screen') &&
            !document.getElementById('shell-screen').classList.contains('hidden'),
      undefined,
      { timeout: LOAD_TIMEOUT_MS }
    );
    result.loadMs = Math.round(performance.now() - t0);
    console.log(`  Load: ${result.loadMs} ms`);

    result.jsHeapMB = await page.evaluate(() => {
      const m = performance.memory;
      return m ? +(m.usedJSHeapSize / 1048576).toFixed(1) : null;
    });
    if (result.jsHeapMB) console.log(`  JS heap: ${result.jsHeapMB} MB`);

    for (const q of QUERIES) {
      const qResult = { name: q.name, oql: q.oql, elapsedMs: null, rows: null, error: null };
      try {
        const t1 = performance.now();
        const raw = await page.evaluate((oql) => {
          const r = window._hprofQueryDirect(oql);
          if (r === null) throw new Error('_hprofQueryDirect returned null — session not loaded');
          return r;
        }, q.oql);
        qResult.elapsedMs = Math.round(performance.now() - t1);
        const data = JSON.parse(raw);
        if (!data.ok) {
          qResult.error = data.error?.message || 'query error';
        } else {
          qResult.rows = data.result?.row_count ?? data.result?.rows?.length ?? null;
        }
        console.log(`  Query [${q.name}]: ${qResult.elapsedMs} ms, rows=${qResult.rows}, err=${qResult.error}`);
      } catch (e) {
        qResult.error = String(e);
        console.log(`  Query [${q.name}] THREW: ${qResult.error}`);
        result.outcome = 'QUERY_FAIL';
      }
      result.queries.push(qResult);
    }

  } catch (e) {
    result.error  = String(e);
    result.outcome = 'FAIL';
    console.log(`  FAILED: ${result.error}`);
    if (pageErrors.length) console.log(`  Page errors: ${pageErrors.slice(0, 3).join('; ')}`);
  } finally {
    await context.close();
  }

  if (pageErrors.length && result.outcome === 'PASS') {
    result.outcome = 'PASS_WITH_ERRORS';
    result.error = pageErrors.slice(0, 3).join('; ');
  }

  return result;
}

function renderMarkdown(results) {
  const lines = [
    '# Web Stress Test Results',
    '',
    `Generated: ${new Date().toISOString()}`,
    '',
    '## Summary',
    '',
    '| Dump | Size (MB) | Load (ms) | JS Heap (MB) | Query: top-classes (ms/rows) | Query: string-count (ms/rows) | Outcome |',
    '|---|---|---|---|---|---|---|',
  ];

  for (const r of results) {
    const q0 = r.queries[0];
    const q1 = r.queries[1];
    const fmt = (q) => q ? (q.error ? `ERR: ${q.error.slice(0,40)}` : `${q.elapsedMs}ms / ${q.rows} rows`) : 'n/a';
    lines.push(
      `| ${r.dump} | ${r.sizeMB} | ${r.loadMs ?? 'n/a'} | ${r.jsHeapMB ?? 'n/a'} | ${fmt(q0)} | ${fmt(q1)} | ${r.outcome} |`
    );
  }

  lines.push('', '## Full Results', '');
  for (const r of results) {
    lines.push(`### ${r.dump}`);
    lines.push(`- **Outcome:** ${r.outcome}`);
    lines.push(`- **Load time:** ${r.loadMs ?? 'n/a'} ms`);
    lines.push(`- **JS heap:** ${r.jsHeapMB ?? 'n/a'} MB`);
    if (r.error) lines.push(`- **Error:** ${r.error}`);
    for (const q of r.queries) {
      lines.push(`- **Query [${q.name}]:** ${q.error ? 'ERROR: ' + q.error : `${q.elapsedMs} ms, ${q.rows} rows`}`);
    }
    lines.push('');
  }

  return lines.join('\n');
}

async function main() {
  const dumps = await getDumps();
  if (!dumps.length) {
    console.error('No .hprof files found in ' + DUMPS);
    process.exit(1);
  }
  console.log(`Testing ${dumps.length} dumps against ${HTML}`);

  // Use Chrome with default settings (no extra memory or security flags)
  // to reflect real-world browser constraints.
  const browser = await chromium.launch({
    channel: 'chrome',
    args: [
      '--enable-precise-memory-info',
    ],
  });

  const results = [];
  for (const dump of dumps) {
    results.push(await testDump(browser, dump));
  }

  await browser.close();

  const md = renderMarkdown(results);
  await writeFile(OUT, md, 'utf8');
  console.log(`\nResults written to ${OUT}`);

  console.log('\n--- Summary ---');
  for (const r of results) {
    console.log(`${r.dump.padEnd(22)} load=${String(r.loadMs??'n/a').padStart(7)}ms  heap=${String(r.jsHeapMB??'n/a').padStart(6)}MB  ${r.outcome}`);
  }

  const failed = results.filter(r => r.outcome !== 'PASS' && r.outcome !== 'PASS_WITH_ERRORS');
  process.exit(failed.length > 0 ? 1 : 0);
}

main().catch(e => { console.error(e); process.exit(1); });
