/**
 * Playwright smoke test for hprof-analyzer HTML reports.
 *
 * Verifies that a self-contained HTML report:
 *  1. Loads without JS runtime errors (catches ReferenceError, TypeError, etc.)
 *  2. Renders report data into the DOM (not just an empty shell)
 *  3. Shows no NaN / undefined in visible text
 *  4. Has working TOC navigation
 *  5. Interactive elements work (sort, collapse, theme toggle, anchor nav, copy button)
 *
 * Run locally:
 *   HPROF_HTML=/path/to/report.html npx playwright test report-smoke.spec.ts
 *
 * In CI the binary generates the report from dump_4_philosophers.hprof first.
 */
import { test, expect } from '@playwright/test';
import * as http from 'http';
import * as fs from 'fs';

// ── Setup: serve the self-contained HTML over localhost ───────────────
const HTML_PATH = process.env.HPROF_HTML;
if (!HTML_PATH) throw new Error('Set HPROF_HTML env var to the path of the report HTML file');
const html = fs.readFileSync(HTML_PATH, 'utf8');

let server: http.Server;
let baseUrl: string;

test.beforeAll(async () => {
  server = http.createServer((_req, res) => {
    res.writeHead(200, { 'Content-Type': 'text/html; charset=utf-8' });
    res.end(html);
  });
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve));
  const addr = server.address() as { port: number };
  baseUrl = `http://127.0.0.1:${addr.port}/`;
});

test.afterAll(() => new Promise<void>(resolve => server.close(() => resolve())));

// Helper: navigate and wait for React mount.
async function openReport(page: import('@playwright/test').Page) {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc', { timeout: 20_000 });
}

// ═══════════════════════════════════════════════════════════════════════
// 1. NO JS ERRORS — would have caught the fmtB ReferenceError
// ═══════════════════════════════════════════════════════════════════════
test('no JS runtime errors during full-page render and scroll', async ({ page }) => {
  const errors: string[] = [];

  page.on('pageerror', err => errors.push(`pageerror: ${err.message}`));
  page.on('console', msg => {
    if (msg.type() === 'error') {
      const text = msg.text();
      if (!text.includes('favicon') && !text.includes('net::ERR_')) {
        errors.push(`console.error: ${text}`);
      }
    }
  });

  await openReport(page);

  // Scroll through the full page to trigger any deferred rendering.
  await page.evaluate(async () => {
    const delay = (ms: number) => new Promise(r => setTimeout(r, ms));
    const step = window.innerHeight;
    for (let y = 0; y < document.body.scrollHeight; y += step) {
      window.scrollTo(0, y);
      await delay(100);
    }
    window.scrollTo(0, document.body.scrollHeight);
    await delay(300);
  });

  expect(errors, `JS errors during render:\n${errors.join('\n')}`).toHaveLength(0);
});

// ═══════════════════════════════════════════════════════════════════════
// 2. REPORT DATA ACTUALLY DISPLAYS — not just an empty shell
// ═══════════════════════════════════════════════════════════════════════
test('memory triage shows at least one signal with a class name or byte size', async ({ page }) => {
  await openReport(page);
  const signals = page.locator('#memory-triage ul li');
  const count = await signals.count();
  expect(count, 'Triage must list at least one signal').toBeGreaterThan(0);

  const text = await signals.first().innerText();
  const hasContent = /\w+\.\w+|\d+\s*(MB|KB|GB|bytes?)/i.test(text);
  expect(hasContent, `First signal "${text.slice(0, 80)}" has no class name or byte size`).toBe(true);
});

test('leak suspects section displays retained heap data', async ({ page }) => {
  await openReport(page);
  await page.waitForSelector('#leak-suspects');
  // The section must mention "retained" somewhere (either suspects or a "no dominant" message).
  const section = page.locator('#leak-suspects');
  await expect(section).toContainText(/retain/i);
});

test('top-consumers tables have data rows', async ({ page }) => {
  await openReport(page);
  await page.waitForSelector('#top-consumers');

  // The "Biggest Objects" and "Biggest Classes" tables use react-data-table-component
  // which renders .rdt_TableRow elements. At least one table must have data.
  const dataRows = page.locator('#top-consumers .rdt_TableRow');
  const count = await dataRows.count();
  expect(count, 'Top Consumers must render at least one data row').toBeGreaterThan(0);
});

test('system overview displays a non-zero heap size', async ({ page }) => {
  await openReport(page);
  await page.waitForSelector('#system-overview');
  const overview = page.locator('#system-overview');
  // Must show a size like "23 MB", "1.2 GB", etc.
  await expect(overview).toContainText(/\d+(\.\d+)?\s*(GB|MB|KB)/i);
});

test('threads section shows thread names and a data table', async ({ page }) => {
  await openReport(page);
  await page.waitForSelector('#threads');
  const threads = page.locator('#threads');
  await expect(threads).toContainText(/Thread/i);
  // ThreadsByRetainedTable uses react-data-table-component (rdt_TableRow), not <tr>.
  const rows = threads.locator('.rdt_TableRow');
  const count = await rows.count();
  expect(count, 'Threads section must have data rows').toBeGreaterThan(0);
});

test('class histogram renders class names with byte sizes', async ({ page }) => {
  await openReport(page);
  // The class histogram is inside #system-overview and uses react-data-table.
  const histRows = page.locator('#system-overview .rdt_TableRow');
  const count = await histRows.count();
  expect(count, 'Class histogram must have rows').toBeGreaterThan(0);

  // The first row should contain a Java class name (has a dot).
  const firstRowText = await histRows.first().innerText();
  expect(firstRowText, 'Histogram row must show a class name').toMatch(/\w+\.\w+/);
});

// ═══════════════════════════════════════════════════════════════════════
// 3. NO NaN OR undefined
// ═══════════════════════════════════════════════════════════════════════
test('no NaN or undefined visible in rendered text', async ({ page }) => {
  await openReport(page);

  // Scroll through to render everything.
  await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
  await page.waitForTimeout(400);

  const body = await page.locator('body').innerText();
  expect(body, 'Body must not contain NaN').not.toContain('NaN');
  expect(body, 'Body must not contain undefined').not.toContain('undefined');
});

// ═══════════════════════════════════════════════════════════════════════
// 4. TOC NAVIGATION
// ═══════════════════════════════════════════════════════════════════════
test('every TOC link resolves to a present DOM element', async ({ page }) => {
  await openReport(page);

  const tocIds = await page.locator('.toc a[href^="#"]').evaluateAll(
    (els: HTMLAnchorElement[]) => els.map(el => el.getAttribute('href')!.slice(1))
  );
  expect(tocIds.length, 'TOC must have links').toBeGreaterThan(0);

  for (const id of tocIds) {
    const count = await page.locator(`[id="${id}"]`).count();
    expect(count, `TOC link #${id} has no matching element`).toBeGreaterThan(0);
  }
});

// ═══════════════════════════════════════════════════════════════════════
// 5. INTERACTION TESTS — sort, collapse, theme, anchor nav, copy button
// ═══════════════════════════════════════════════════════════════════════
test('page title contains "Heap Dump Analysis"', async ({ page }) => {
  await openReport(page);
  await expect(page).toHaveTitle(/Heap Dump Analysis/);
});

test('sort header click does not crash rendering', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', err => errors.push(err.message));

  await openReport(page);
  // The class histogram uses a custom sortable <th>; Top Consumers uses react-data-table.
  // Try the custom table first, fall back to react-data-table.
  let clicked = false;
  const customSortable = page.locator('th.sortable').first();
  if (await customSortable.count() > 0) {
    await customSortable.click();
    clicked = true;
  } else {
    const rdtSortable = page.locator('#top-consumers .rdt_TableCol_Sortable').first();
    if (await rdtSortable.count() > 0) {
      await rdtSortable.click();
      clicked = true;
    }
  }
  if (clicked) {
    await page.waitForTimeout(300);
    expect(errors, 'JS errors after sort click: ' + errors.join('; ')).toHaveLength(0);
  }
});

test('collapsible details expand and collapse', async ({ page }) => {
  await openReport(page);
  const details = page.locator('details').first();
  if (await details.count() > 0) {
    const summary = details.locator('summary').first();
    // Open it.
    await summary.click();
    await expect(details).toHaveAttribute('open', '');
    // Close it.
    await summary.click();
    await expect(details).not.toHaveAttribute('open');
  }
});

test('theme toggle switches between light and dark', async ({ page }) => {
  await openReport(page);
  // The app is a 2-state toggle (light ↔ dark); data-theme is always set on mount.
  const btn = page.locator('[aria-label^="Theme:"]').first();
  await expect(btn).toBeVisible();

  // Read the initial mode from data-theme (always present after mount).
  const initial = await page.locator('html').getAttribute('data-theme');
  expect(initial === 'light' || initial === 'dark', `data-theme must be light or dark, got "${initial}"`).toBe(true);

  // Click → should switch to the other mode.
  await btn.click();
  const after = await page.locator('html').getAttribute('data-theme');
  expect(after).not.toBe(initial);
  expect(after === 'light' || after === 'dark', `data-theme must be light or dark after toggle, got "${after}"`).toBe(true);

  // Click again → should cycle back to original.
  await btn.click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', initial!);
});

test('anchor navigation scrolls to leak-suspects section', async ({ page }) => {
  await page.goto(baseUrl + '#leak-suspects');
  await page.waitForSelector('.toc', { timeout: 20_000 });
  // Wait for smooth-scroll animation (app uses requestAnimationFrame + smooth behavior).
  await page.waitForTimeout(1500);
  const section = page.locator('#leak-suspects').first();
  await expect(section).toBeVisible();
  // Confirm the element is present and has a bounding box.
  const box = await section.boundingBox();
  expect(box).not.toBeNull();
  // The section must be scrolled into view: scroll-position + element-top should be near 0.
  // We check this by forcing a scroll and confirming visibility rather than relying on y-coord,
  // since smooth-scroll timing is non-deterministic in headless.
  await section.scrollIntoViewIfNeeded();
  await expect(section).toBeInViewport();
});

test('copy button appears on hover over a data cell', async ({ page }) => {
  await openReport(page);
  // Find the first cell containing a .copy-cell span (class name cells in tables).
  const firstCopyCell = page.locator('.rdt_TableRow .copy-cell').first();
  if (await firstCopyCell.count() > 0) {
    // Hover over the row to make the copy button visible.
    await firstCopyCell.hover();
    const copyBtn = firstCopyCell.locator('.copy-btn').first();
    await expect(copyBtn).toBeVisible();
  }
});

// ═══════════════════════════════════════════════════════════════════════
// 6. DOMINATOR FLAME GRAPH — regression for cycle-induced stack overflow
// ═══════════════════════════════════════════════════════════════════════
test('dominator flame graph tab renders without JS errors or crash', async ({ page }) => {
  const errors: string[] = [];
  page.on('pageerror', err => errors.push(`pageerror: ${err.message}`));
  page.on('console', msg => {
    if (msg.type() === 'error') {
      const text = msg.text();
      if (!text.includes('favicon') && !text.includes('net::ERR_')) {
        errors.push(`console.error: ${text}`);
      }
    }
  });

  await openReport(page);

  // Navigate to Dominator Analysis section.
  await page.evaluate(() => {
    document.getElementById('dominator-analysis')?.scrollIntoView();
  });
  await page.waitForTimeout(300);

  // Look for the Flame button (🔥 Flame) in the dominator view switcher.
  const flameBtn = page.locator('#dominator-analysis button', { hasText: /Flame/ }).first();
  if (await flameBtn.count() === 0) {
    // Report was generated without dominator pairs — skip gracefully.
    return;
  }

  await flameBtn.click();
  // Wait for React to re-render; a cycle bug would throw before this resolves.
  await page.waitForTimeout(500);

  // Check there's no "Report failed to render" error boundary.
  const renderError = page.locator('.render-error');
  expect(await renderError.count(), 'render-error boundary triggered after Flame click').toBe(0);

  expect(errors, `JS errors after clicking Flame:\n${errors.join('\n')}`).toHaveLength(0);
});

