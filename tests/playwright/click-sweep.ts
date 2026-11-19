#!/usr/bin/env node
/**
 * click-sweep — randomised exhaustive crash detector for hprof HTML reports.
 *
 * Visits every distinct interactive element in a self-contained HTML report in
 * random order, so each run exercises a different interaction sequence.
 * State-dependent crashes (only triggered by clicking A then B, never A alone)
 * are found much faster than a fixed DOM-order sweep.
 *
 * After the main sweep the script does a keyboard pass (Tab/Enter/Escape/arrows)
 * and a hover pass (revealing copy buttons, tooltips, row actions).  Any
 * JavaScript error, React render-error boundary, or unhandled rejection is
 * collected and reported; the process exits non-zero if any were found.
 *
 * USAGE
 * ─────
 *   # Minimal — 60-second randomised sweep
 *   HPROF_HTML=/path/to/report.html npx ts-node click-sweep.ts
 *
 *   # 5-minute CI run (the standard CI invocation)
 *   HPROF_HTML=/tmp/smoke-report.html npx ts-node click-sweep.ts --budget 300
 *
 *   # Reproduce a previous run exactly (use the seed printed in the summary)
 *   HPROF_HTML=/path/to/report.html npx ts-node click-sweep.ts --seed 1234567890
 *
 *   # Long local soak with visible browser and 400 ms between interactions
 *   HPROF_HTML=/path/to/report.html \
 *     npx ts-node click-sweep.ts --budget 600 --delay 400 --no-headless
 *
 *   # Verbose mode — print every click/hover action and JS warning
 *   HPROF_HTML=/path/to/report.html npx ts-node click-sweep.ts --verbose
 *
 * OPTIONS
 * ───────
 *   --budget <s>       Time budget in seconds (default: 60).
 *                      The sweep stops when the budget expires, even if unseen
 *                      elements remain.  Skipped elements are reported.
 *   --delay <ms>       Pause after each interaction in milliseconds (default: 200).
 *                      Increase for slower machines or animated UIs.
 *   --seed <n>         Integer seed for the random shuffle (default: random).
 *                      Use the seed printed at the end of a run to reproduce it.
 *   --revisit <n>      After seeing all elements once, randomly revisit this many
 *                      previously-clicked ones to trigger state-dependent crashes
 *                      (default: 20, 0 = disabled).
 *   --no-headless      Show the browser window (useful for local debugging).
 *   --warnings         Also treat console.warn messages as failures.
 *   --no-hover         Skip the hover pass (faster, misses hover-only UI).
 *   --no-keyboard      Skip the keyboard pass (Tab/Enter/Escape/arrows).
 *   --verbose          Print every action and all console messages (warnings,
 *                      console.log).  Does NOT make warnings fail the run unless
 *                      --warnings is also passed.
 *   --help             Print this message and exit.
 *
 * EXIT CODES
 * ──────────
 *   0  No errors (and no warnings if --warnings was passed)
 *   1  At least one JS error, render-error boundary, or warning (if --warnings)
 *   2  Usage error (bad arguments, missing HPROF_HTML)
 *
 * CI INTEGRATION
 * ──────────────
 *   - name: Run click sweep (5-minute budget)
 *     working-directory: tests/playwright
 *     env:
 *       HPROF_HTML: /tmp/smoke-report.html
 *     run: npx ts-node click-sweep.ts --budget 300
 */

import { chromium } from '@playwright/test';
import type { Browser, Page, ConsoleMessage } from '@playwright/test';
import * as http from 'http';
import * as fs from 'fs';
import * as path from 'path';

// ── Argument parsing ──────────────────────────────────────────────────────────

const argv = process.argv.slice(2);

if (argv.includes('--help')) {
  const src = fs.readFileSync(__filename, 'utf8');
  const docMatch = src.match(/^\/\*\*([\s\S]*?)\*\//);
  if (docMatch) console.log(docMatch[1].replace(/^ \* ?/gm, '').trim());
  process.exit(0);
}

function getArg(flag: string, defaultVal: number): number {
  const idx = argv.indexOf(flag);
  if (idx === -1) return defaultVal;
  const raw = argv[idx + 1];
  const n = Number(raw);
  if (!isFinite(n) || n < 0) {
    console.error(`Error: ${flag} requires a non-negative number, got "${raw}"`);
    process.exit(2);
  }
  return n;
}

const BUDGET_MS   = getArg('--budget', 60) * 1000;
const DELAY_MS    = getArg('--delay', 200);
const REVISIT_N   = getArg('--revisit', 20);
const HEADLESS    = !argv.includes('--no-headless');
const FAIL_WARNS  = argv.includes('--warnings');
const DO_HOVER    = !argv.includes('--no-hover');
const DO_KEYBOARD = !argv.includes('--no-keyboard');
const VERBOSE     = argv.includes('--verbose');

// Seed: use --seed if given, otherwise a random integer.
const SEED_IDX = argv.indexOf('--seed');
const SEED = SEED_IDX !== -1 ? parseInt(argv[SEED_IDX + 1], 10) : Math.floor(Math.random() * 2 ** 32);
if (SEED_IDX !== -1 && !isFinite(SEED)) {
  console.error(`Error: --seed requires an integer, got "${argv[SEED_IDX + 1]}"`);
  process.exit(2);
}

const HTML_PATH = process.env.HPROF_HTML;
if (!HTML_PATH) {
  console.error(
    'Error: set HPROF_HTML to the path of the report HTML file.\n' +
    '  Example: HPROF_HTML=/tmp/report.html npx ts-node click-sweep.ts',
  );
  process.exit(2);
}
if (!fs.existsSync(HTML_PATH)) {
  console.error(`Error: file not found: ${HTML_PATH}`);
  process.exit(2);
}

// ── Seeded PRNG (mulberry32) ──────────────────────────────────────────────────
// Pure JS, no crypto dep, good enough for shuffle randomness.

function makePrng(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s += 0x6d2b79f5;
    let t = Math.imul(s ^ (s >>> 15), 1 | s);
    t ^= t + Math.imul(t ^ (t >>> 7), 61 | t);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function shuffle<T>(arr: T[], rng: () => number): T[] {
  const a = arr.slice();
  for (let i = a.length - 1; i > 0; i--) {
    const j = Math.floor(rng() * (i + 1));
    [a[i], a[j]] = [a[j], a[i]];
  }
  return a;
}

// ── Selectors ─────────────────────────────────────────────────────────────────

const HOVER_SELECTOR =
  '.rdt_TableRow, .rdt_TableCell, svg rect, svg g[class], ' +
  'li, .toc a, [data-tip], [title], canvas';

// All clickable elements.
const CLICK_SELECTOR = 'button, [onclick], [role="button"], [role="tab"], [role="menuitem"]';

// Max samples to click from each high-cardinality button type (per-row actions).
const MAX_TABLE_ROWS = 5;

// Keys sent during the keyboard pass.  Escape first (closes any stray modal),
// then navigation keys to stress any focused component.
const KEYBOARD_SEQUENCE = ['Escape', 'Tab', 'Enter', 'ArrowDown', 'ArrowUp', 'ArrowLeft', 'ArrowRight', 'Escape'];

type ElMeta = { key: string; index: number; scroll: number };

const log = (msg: string) => { if (VERBOSE) console.log('  ' + msg); };

// ── Main ──────────────────────────────────────────────────────────────────────

interface Finding {
  kind: 'error' | 'warning' | 'render-error';
  trigger: string;
  message: string;
}

(async () => {
  const rng = makePrng(SEED);

  // ── Serve the report ────────────────────────────────────────────────────────

  const html = fs.readFileSync(HTML_PATH!, 'utf8');
  const server = http.createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(html);
  });
  await new Promise<void>(r => server.listen(0, '127.0.0.1', r));
  const port = (server.address() as { port: number }).port;
  const baseUrl = `http://127.0.0.1:${port}/`;

  // ── Browser ─────────────────────────────────────────────────────────────────

  const browser: Browser = await chromium.launch({ headless: HEADLESS });
  const page: Page = await browser.newPage();

  const findings: Finding[] = [];
  let lastTrigger = '(page load)';

  page.on('pageerror', (err: Error) => {
    const msg = err.message;
    // Clipboard API is blocked in headless Chromium — not an app crash.
    if (msg.includes('Write permission denied') || msg.includes('clipboard')) return;
    findings.push({ kind: 'error', trigger: lastTrigger, message: msg });
    if (VERBOSE) console.log(`  ✗ pageerror [${lastTrigger}]: ${msg.split('\n')[0]}`);
  });
  page.on('console', (msg: ConsoleMessage) => {
    const type = msg.type();
    const text = msg.text();
    if (text.includes('favicon') || text.includes('net::ERR_')) return;
    if (type === 'error') {
      findings.push({ kind: 'error', trigger: lastTrigger, message: `console.error: ${text}` });
      if (VERBOSE) console.log(`  ✗ console.error [${lastTrigger}]: ${text.split('\n')[0]}`);
    } else if (type === 'warning') {
      if (FAIL_WARNS) findings.push({ kind: 'warning', trigger: lastTrigger, message: `console.warn: ${text}` });
      if (VERBOSE) console.log(`  ⚠ console.warn [${lastTrigger}]: ${text.split('\n')[0]}`);
    } else if (VERBOSE && type === 'log') {
      console.log(`  · console.log: ${text.split('\n')[0].slice(0, 120)}`);
    }
  });
  page.on('dialog', d => d.dismiss().catch(() => {}));

  // ── Load ────────────────────────────────────────────────────────────────────

  console.log(`\nhprof click-sweep`);
  console.log(`  Report : ${path.basename(HTML_PATH!)}`);
  console.log(`  Budget : ${(BUDGET_MS / 1000).toFixed(0)}s`);
  console.log(`  Delay  : ${DELAY_MS}ms`);
  console.log(`  Seed   : ${SEED}`);
  console.log(`  Hover  : ${DO_HOVER ? 'yes' : 'no (--no-hover)'}`);
  console.log(`  Keys   : ${DO_KEYBOARD ? 'yes' : 'no (--no-keyboard)'}`);
  console.log(`  Revisit: ${REVISIT_N}`);
  console.log(`  Verbose: ${VERBOSE ? 'yes' : 'no'}`);
  console.log(`  Mode   : ${HEADLESS ? 'headless' : 'headed'}${FAIL_WARNS ? '  warnings=fail' : ''}`);
  console.log('');

  await page.goto(baseUrl);

  try {
    await page.waitForSelector('.toc', { timeout: 20_000 });
  } catch {
    const bodyText = await page.locator('body').innerText().catch(() => '(no body text)');
    const errors = findings.filter(f => f.kind === 'error');
    console.error('\n✗ Report never rendered (.toc not found within 20s).');
    console.error('  This usually means:');
    console.error('    • Schema mismatch (report generated by a different binary version)');
    console.error('    • A JS crash prevented React from mounting');
    if (errors.length) {
      console.error('\n  JS errors during load:');
      errors.forEach(e => console.error(`    ${e.message.split('\n')[0]}`));
    }
    console.error('\n  First 300 chars of page body:');
    console.error('    ' + bodyText.slice(0, 300).replace(/\n/g, '\n    '));
    await browser.close(); server.close();
    process.exit(1);
  }

  if (findings.length > 0) {
    console.log(`  ⚠  ${findings.length} issue(s) during page load — see report below.`);
  }

  // ── State ────────────────────────────────────────────────────────────────────

  const startMs    = Date.now();
  const deadline   = startMs + BUDGET_MS;
  const clickSeen  = new Set<string>();
  const clickOrder: string[] = [];   // ordered list of seen keys, for revisit sampling
  const hoverSeen  = new Set<string>();
  // skippedFinal is computed once at the end: elements ever seen but never clicked.
  let   clicked    = 0;
  let   hovered    = 0;
  let   revisited  = 0;
  let   round      = 0;
  let   stopped    = false;
  // Track the full set of element keys ever observed (across all rounds).
  const everSeen   = new Set<string>();

  // ── Utility: check render-error boundary ────────────────────────────────────

  async function checkBoundary(trigger: string): Promise<boolean> {
    const n = await page.locator('.render-error').count();
    if (n > 0) {
      findings.push({ kind: 'render-error', trigger, message: 'React render-error boundary appeared' });
      console.log(`\n  ✗ render-error boundary after "${trigger}" — stopping.`);
      stopped = true;
    }
    return stopped;
  }

  // ── Utility: snapshot clickable elements, partitioned by priority ────────────
  //
  // Reports have thousands of per-row action buttons (same tag+class+text in
  // every row).  We partition by frequency:
  //   tier0 — "structural" buttons that appear <= MAX_DUPES times total
  //   tier1 — per-row buttons: cap each type at MAX_TABLE_ROWS random samples
  // Both tiers are shuffled so each run visits them in a different order.

  async function snapshotClickable(): Promise<ElMeta[]> {
    const all = await page.locator(CLICK_SELECTOR).evaluateAll((els: Element[]) => {
      return els.map((el, i) => {
        const tag     = el.tagName.toLowerCase();
        const text    = (el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 40);
        const cls     = [...el.classList].sort().join('.');
        let ctxId     = '';
        let anc: Element | null = el.parentElement;
        while (anc) { if (anc.id) { ctxId = anc.id; break; } anc = anc.parentElement; }
        const key     = `${ctxId}/${tag}[${cls}]:${text || `idx${i}`}`;
        // typeKey ignores context — identifies the button "type" across rows.
        const typeKey = `${tag}[${cls}]:${text}`;
        // Stamp the element so clickEl can find it by stable attribute.
        (el as HTMLElement).dataset.sweepKey = key;
        const rect    = el.getBoundingClientRect();
        return { key, typeKey, index: i, scroll: Math.round(window.scrollY + rect.top) };
      });
    });

    // Count occurrences of each typeKey.
    const typeCount = new Map<string, number>();
    for (const m of all) typeCount.set(m.typeKey, (typeCount.get(m.typeKey) ?? 0) + 1);

    const MAX_DUPES = 8;
    type AllMeta = ElMeta & { typeKey: string };
    const tier0: AllMeta[] = [];
    const tier1Map = new Map<string, AllMeta[]>();

    for (const m of (all as AllMeta[])) {
      if ((typeCount.get(m.typeKey) ?? 0) <= MAX_DUPES) {
        tier0.push(m);
      } else {
        const bucket = tier1Map.get(m.typeKey) ?? [];
        bucket.push(m);
        tier1Map.set(m.typeKey, bucket);
      }
    }

    // Sample MAX_TABLE_ROWS unseen elements from each high-cardinality bucket.
    const tier1: AllMeta[] = [];
    for (const bucket of tier1Map.values()) {
      const unseen = bucket.filter(m => !clickSeen.has(m.key));
      tier1.push(...shuffle(unseen, rng).slice(0, MAX_TABLE_ROWS));
    }

    return [...shuffle(tier0, rng), ...shuffle(tier1, rng)];
  }

  // ── Utility: click one element by its locator index ─────────────────────────

  async function clickEl(meta: ElMeta, label: string): Promise<boolean> {
    lastTrigger = label;
    await page.evaluate((y: number) => window.scrollTo({ top: Math.max(0, y - 100) }), meta.scroll);
    // Use data-sweep-key for stable identification across DOM mutations.
    const escapedKey = meta.key.replace(/\\/g, '\\\\').replace(/"/g, '\\"');
    const target = page.locator(`[data-sweep-key="${escapedKey}"]`).first();
    try {
      if (await target.isVisible({ timeout: 500 })) {
        await target.scrollIntoViewIfNeeded();
        await target.click({ timeout: 3_000 });
        if (DELAY_MS > 0) await page.waitForTimeout(DELAY_MS);
        return true;
      }
    } catch {
      log(`skip (detached/covered): ${label}`);
    }
    return false;
  }

  // ── Phase 1: scroll to trigger lazy-rendered React sections ─────────────────

  process.stdout.write('  Scrolling... ');
  await page.evaluate(async () => {
    const delay = (ms: number) => new Promise<void>(r => setTimeout(r, ms));
    for (let y = 0; y < document.body.scrollHeight; y += window.innerHeight) {
      window.scrollTo(0, y);
      await delay(80);
    }
    window.scrollTo(0, 0);
  });
  process.stdout.write('done\n');

  // ── Phase 2: randomised click sweep ──────────────────────────────────────────
  //
  // Each round: snapshot all clickable elements, shuffle them, click all unseen
  // ones in that snapshot before re-snapshotting.  After each structural click
  // (tabs, toggles) the loop re-snapshots to discover newly revealed elements.
  // Re-snapshotting every click is too slow for large reports (17k+ elements).

  // Structural elements likely to reveal new content when clicked.
  const STRUCTURAL_RE = /role="tab"|tab-|section|toggle|collapse|expand|view-btn|view-switcher/;

  process.stdout.write('  Clicking:');
  if (VERBOSE) process.stdout.write('\n');

  while (!stopped && Date.now() < deadline) {
    round++;

    const allMetas  = await snapshotClickable();
    const shuffled  = shuffle(allMetas, rng);

    // Track everything currently on screen as ever-seen.
    shuffled.forEach(m => everSeen.add(m.key));

    const unseen = shuffled.filter(m => !clickSeen.has(m.key));
    if (unseen.length === 0) break;

    // Click the whole unseen batch; re-snapshot if a structural element might
    // have revealed new content.
    let needsResnapshot = false;
    for (const next of unseen) {
      if (stopped || Date.now() >= deadline) break;

      clickSeen.add(next.key);
      clickOrder.push(next.key);

      const ok = await clickEl(next, next.key);
      if (ok) {
        clicked++;
        if (VERBOSE) {
          console.log(`  → click [${clicked}] ${next.key}`);
        } else {
          process.stdout.write('.');
          if (clicked % 60 === 0) process.stdout.write('\n            ');
        }
        // Structural elements may reveal new content — re-snapshot after them.
        if (STRUCTURAL_RE.test(next.key)) { needsResnapshot = true; break; }
      }

      if (await checkBoundary(next.key)) break;
    }

    // If nothing structural was clicked, the DOM is stable — no re-snapshot needed.
    if (!needsResnapshot) break;
  }

  if (!VERBOSE) process.stdout.write('\n');

  // ── Phase 3: random revisit ───────────────────────────────────────────────────
  //
  // Pick REVISIT_N previously-seen elements at random and click them again.
  // This exercises state-dependent crashes: element X might crash only after
  // element Y was already clicked, which a single-pass sweep never finds.

  if (REVISIT_N > 0 && clickOrder.length > 0 && !stopped && Date.now() < deadline) {
    const pool    = shuffle(clickOrder.slice(), rng).slice(0, REVISIT_N);
    process.stdout.write(`  Revisiting ${pool.length} random elements:`);
    if (VERBOSE) process.stdout.write('\n');

    for (const key of pool) {
      if (stopped || Date.now() >= deadline) break;

      const allMetas = await snapshotClickable();
      const meta = allMetas.find(m => m.key === key);
      if (!meta) continue;

      lastTrigger = `revisit:${key}`;
      const ok = await clickEl(meta, lastTrigger);
      if (ok) {
        revisited++;
        if (VERBOSE) {
          console.log(`  ↺ revisit [${revisited}] ${key}`);
        } else {
          process.stdout.write('↺');
          if (revisited % 60 === 0) process.stdout.write('\n            ');
        }
      }
      if (await checkBoundary(lastTrigger)) break;
    }

    if (!VERBOSE) process.stdout.write('\n');
  }

  // ── Phase 4: keyboard pass ───────────────────────────────────────────────────
  //
  // Send Tab/Enter/Escape/arrow keys while focused on a random subset of
  // clickable elements.  This exercises keyboard event handlers and catches
  // crashes in focused components that click-only sweeps miss.

  if (DO_KEYBOARD && !stopped && Date.now() < deadline) {
    // Pick up to 10 random elements to focus-and-key-stress.
    const keyTargets = shuffle(clickOrder.slice(), rng).slice(0, 10);

    process.stdout.write(`  Keys    :`);
    if (VERBOSE) process.stdout.write('\n');

    let keyActions = 0;
    for (const key of keyTargets) {
      if (stopped || Date.now() >= deadline) break;

      const allMetas = await snapshotClickable();
      const meta = allMetas.find(m => m.key === key);
      if (!meta) continue;

      await page.evaluate((y: number) => window.scrollTo({ top: Math.max(0, y - 100) }), meta.scroll);
      const target = page.locator(CLICK_SELECTOR).nth(meta.index);
      try {
        if (!await target.isVisible({ timeout: 300 })) continue;
        await target.scrollIntoViewIfNeeded();
        await target.focus({ timeout: 1_000 });

        for (const k of KEYBOARD_SEQUENCE) {
          if (stopped || Date.now() >= deadline) break;
          lastTrigger = `key:${k}@${key}`;
          await page.keyboard.press(k);
          await page.waitForTimeout(50);
          if (await checkBoundary(lastTrigger)) break;
        }

        keyActions++;
        if (VERBOSE) {
          console.log(`  ⌨ keys [${keyActions}] ${KEYBOARD_SEQUENCE.join(' ')} @ ${key}`);
        } else {
          process.stdout.write('⌨');
          if (keyActions % 60 === 0) process.stdout.write('\n            ');
        }
        if (DELAY_MS > 0) await page.waitForTimeout(Math.min(DELAY_MS, 100));
      } catch {
        log(`skip keyboard (detached): ${key}`);
      }
    }

    if (!VERBOSE) process.stdout.write('\n');
  }

  // ── Phase 5: hover pass ───────────────────────────────────────────────────────

  if (DO_HOVER && !stopped && Date.now() < deadline) {
    process.stdout.write('  Hovering:');
    if (VERBOSE) process.stdout.write('\n');

    await page.evaluate(() => window.scrollTo(0, 0));

    const rawHoverMetas = await page
      .locator(HOVER_SELECTOR)
      .evaluateAll((els: Element[]) =>
        els.map((el, i) => {
          const tag  = el.tagName.toLowerCase();
          const text = (el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 20);
          const cls  = [...el.classList].sort().join('.');
          let ctxId  = '';
          let anc: Element | null = el.parentElement;
          while (anc) { if (anc.id) { ctxId = anc.id; break; } anc = anc.parentElement; }
          return { key: `hover:${ctxId}/${tag}[${cls}]:${text || `idx${i}`}`, index: i };
        })
      );

    // Shuffle the hover targets too.
    const hoverMetas = shuffle(rawHoverMetas, rng);

    for (const hm of hoverMetas) {
      if (stopped || Date.now() >= deadline) break;
      if (hoverSeen.has(hm.key)) continue;
      hoverSeen.add(hm.key);

      lastTrigger = hm.key;
      const target = page.locator(HOVER_SELECTOR).nth(hm.index);
      try {
        if (!await target.isVisible({ timeout: 300 })) continue;
        await target.scrollIntoViewIfNeeded();
        await target.hover({ timeout: 2_000 });
        hovered++;
        if (VERBOSE) {
          console.log(`  · hover [${hovered}] ${hm.key}`);
        } else {
          process.stdout.write('·');
          if (hovered % 60 === 0) process.stdout.write('\n            ');
        }

        await page.waitForTimeout(Math.min(DELAY_MS, 150));

        // Click any newly revealed clickable elements.
        const revealedMetas: ElMeta[] = await page
          .locator(`${CLICK_SELECTOR}:visible`)
          .evaluateAll((els: Element[]) =>
            els.map((el, i) => {
              const tag  = el.tagName.toLowerCase();
              const text = (el.textContent ?? '').trim().replace(/\s+/g, ' ').slice(0, 40);
              const cls  = [...el.classList].sort().join('.');
              let ctxId  = '';
              let anc: Element | null = el.parentElement;
              while (anc) { if (anc.id) { ctxId = anc.id; break; } anc = anc.parentElement; }
              return { key: `${ctxId}/${tag}[${cls}]:${text || `idx${i}`}`, index: i, scroll: 0 };
            })
          );

        for (const rm of revealedMetas) {
          if (clickSeen.has(rm.key)) continue;
          clickSeen.add(rm.key);
          clickOrder.push(rm.key);
          everSeen.add(rm.key);
          lastTrigger = `hover-reveal:${rm.key}`;
          const revTarget = page.locator(`${CLICK_SELECTOR}:visible`).nth(rm.index);
          try {
            if (await revTarget.isVisible({ timeout: 300 })) {
              await revTarget.click({ timeout: 2_000 });
              clicked++;
              if (VERBOSE) {
                console.log(`  + hover-reveal click [${clicked}] ${rm.key}`);
              } else {
                process.stdout.write('+');
              }
              if (DELAY_MS > 0) await page.waitForTimeout(DELAY_MS);
            }
          } catch { /* element gone */ }
          if (await checkBoundary(lastTrigger)) break;
          if (stopped || Date.now() >= deadline) break;
        }
      } catch {
        log(`skip hover (detached/moved): ${hm.key}`);
      }
      if (await checkBoundary(hm.key)) break;
    }

    if (!VERBOSE) process.stdout.write('\n');
  }

  await browser.close();
  server.close();

  // ── Summary ───────────────────────────────────────────────────────────────────

  // Elements that were ever observed but never actually clicked.
  const skippedCount = [...everSeen].filter(k => !clickSeen.has(k)).length;

  const elapsedS = ((Date.now() - startMs) / 1000).toFixed(1);
  const errors   = findings.filter(f => f.kind === 'error');
  const warns    = findings.filter(f => f.kind === 'warning');
  const renders  = findings.filter(f => f.kind === 'render-error');

  console.log('');
  console.log('── Results ──────────────────────────────────────────────────────');
  console.log(`  Clicked  : ${clicked} unique elements in ${elapsedS}s`);
  console.log(`  Revisited: ${revisited}`);
  if (DO_HOVER)    console.log(`  Hovered  : ${hovered} elements`);
  console.log(`  Skipped  : ${skippedCount}${stopped ? ' (stopped early)' : ' (budget)'}`);
  console.log(`  Rounds   : ${round}`);
  console.log(`  Errors   : ${errors.length}`);
  if (FAIL_WARNS || VERBOSE) console.log(`  Warnings : ${warns.length}`);
  console.log(`  Render ✗ : ${renders.length}`);
  console.log(`  Seed     : ${SEED}  (re-run with --seed ${SEED} to reproduce)`);

  if (findings.length > 0) {
    console.log('');
    console.log('── Issues ───────────────────────────────────────────────────────');
    for (const f of findings) {
      const icon = f.kind === 'warning' ? '⚠' : '✗';
      console.log(`  ${icon} [${f.kind}] after "${f.trigger}"`);
      f.message.split('\n').forEach(line => console.log(`      ${line}`));
    }
  }

  const failed = errors.length > 0 || renders.length > 0 || (FAIL_WARNS && warns.length > 0);
  console.log('');
  console.log(failed ? '✗ Issues found — see above.' : '✓ No issues found.');
  process.exit(failed ? 1 : 0);
})();
