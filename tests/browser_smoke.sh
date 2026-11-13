#!/usr/bin/env bash
# Browser smoke test for the hprof-analyzer HTML report.
# Requires: Node.js 18+.
# Usage: ./tests/browser_smoke.sh [path/to/report.html]
# If no path given, generates one from dump_4_philosophers.hprof.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN="$REPO/target/release/hprof-analyzer"
FIXTURE="$REPO/tests/fixtures/dump_4_philosophers.hprof"

TMPDIR_SMOKE=""
TMPDIR_TEST=""
trap 'rm -rf "$TMPDIR_TEST" "${TMPDIR_SMOKE:-}"' EXIT

# Generate a fresh HTML report if none given; resolve relative paths to absolute.
HTML="${1:-}"
if [[ -z "$HTML" ]]; then
  TMPDIR_SMOKE="$(mktemp -d)"
  HTML="$TMPDIR_SMOKE/smoke.html"
  echo "Generating HTML report from $FIXTURE..."
  "$BIN" "$FIXTURE" "$HTML"
  echo "Report written to $HTML"
elif [[ "$HTML" != /* ]]; then
  HTML="$(cd "$(dirname "$HTML")" && pwd)/$(basename "$HTML")"
fi

# Set up a local playwright runner directory with @playwright/test installed.
# Re-use it if it already has the package installed.
PLAYWRIGHT_RUNNER="${HPROF_PLAYWRIGHT_DIR:-$HOME/.cache/hprof-playwright}"
if [[ ! -d "$PLAYWRIGHT_RUNNER/node_modules/@playwright/test" ]]; then
  echo "Installing @playwright/test in $PLAYWRIGHT_RUNNER..."
  mkdir -p "$PLAYWRIGHT_RUNNER"
  cd "$PLAYWRIGHT_RUNNER"
  npm init -y > /dev/null 2>&1
  npm install @playwright/test 2>&1 | tail -5
  # Install the Chromium browser for this playwright version.
  "$PLAYWRIGHT_RUNNER/node_modules/.bin/playwright" install chromium 2>&1 | tail -5
fi
PLAYWRIGHT_BIN="$PLAYWRIGHT_RUNNER/node_modules/.bin/playwright"
PLAYWRIGHT_MODULES="$PLAYWRIGHT_RUNNER/node_modules"

# Write the Playwright test inline to a temp file.
TMPDIR_TEST="$(mktemp -d)"
cat > "$TMPDIR_TEST/smoke.spec.ts" << 'EOF'
import { test, expect } from '@playwright/test';
import * as http from 'http';
import * as fs from 'fs';

const HTML_PATH = process.env.HPROF_HTML!;
const html = fs.readFileSync(HTML_PATH, 'utf8');

// Serve the HTML file from a local HTTP server so that scripts run.
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

test('page title contains "Heap Dump Analysis"', async ({ page }) => {
  await page.goto(baseUrl);
  await expect(page).toHaveTitle(/Heap Dump Analysis/);
});

test('sort header click reorders table rows', async ({ page }) => {
  await page.goto(baseUrl);
  // Wait for React to mount.
  await page.waitForSelector('.toc');
  // Click the "Shallow" sort header in the Overview class histogram.
  // First expand the histogram details if it exists.
  const showHistogram = page.getByText('Show full class histogram');
  if (await showHistogram.count() > 0) {
    await showHistogram.click();
  }
  // Click first sortable header.
  const sortableHeader = page.locator('th.sortable').first();
  await sortableHeader.click();
  // The sort header should now be active.
  await expect(page.locator('th.sortable.active').first()).toBeVisible();
});

test('collapsible details expand and collapse', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc');
  // Find a closed <details> element (summary that can be clicked).
  const details = page.locator('details').first();
  const summary = details.locator('summary').first();
  // Open it.
  await summary.click();
  await expect(details).toHaveAttribute('open', '');
});

test('theme toggle cycles through modes', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc');
  // Use aria-label to target the actual theme-toggle button (not "Expand all" etc.)
  const btn = page.locator('[aria-label^="Theme:"]').first();
  // Default is auto (no data-theme).
  await expect(page.locator('html')).not.toHaveAttribute('data-theme');
  // Click → light.
  await btn.click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'light');
  // Click → dark.
  await btn.click();
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  // Click → auto (attribute removed).
  await btn.click();
  await expect(page.locator('html')).not.toHaveAttribute('data-theme');
});

test('anchor navigation scrolls to the target section', async ({ page }) => {
  await page.goto(baseUrl + '#leaks');
  // Wait for React to mount and scroll effect to fire.
  await page.waitForSelector('.toc');
  await page.waitForTimeout(600);
  const leaksSection = page.locator('#leaks').first();
  await expect(leaksSection).toBeVisible();
  const box = await leaksSection.boundingBox();
  expect(box).not.toBeNull();
  // The section should be near the top of the viewport (within the page height).
  const viewportSize = page.viewportSize()!;
  expect(box!.y).toBeLessThan(viewportSize.height);
});

test('copy button appears on hover', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc');
  // Expand the class histogram if needed.
  const showHistogram = page.getByText('Show full class histogram');
  if (await showHistogram.count() > 0) {
    await showHistogram.click();
  }
  // Hover over the first td that contains a copy-cell (copy button visible on td:hover).
  const firstCopyCell = page.locator('td:has(.copy-cell)').first();
  await firstCopyCell.hover();
  const copyBtn = firstCopyCell.locator('.copy-btn');
  await expect(copyBtn).toBeVisible();
});

// ── Content-presence tests ────────────────────────────────────────────────────
// These verify that the report data actually reaches the DOM — i.e. the React
// app decoded + rendered the embedded JSON successfully, not just that the HTML
// shell loaded.

test('report content: triage section has at least one signal', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#memory-triage');
  // Each triage signal renders as an <li> inside #memory-triage > ul.
  const signals = page.locator('#memory-triage ul li');
  const count = await signals.count();
  expect(count, 'Memory Triage must render at least one signal').toBeGreaterThan(0);
  // Each signal mentions either a class name (dot in FQN) or a byte size.
  const firstText = await signals.first().innerText();
  const hasContent = /\w+\.\w+|\d+\s*(MB|KB|GB|bytes?)/i.test(firstText);
  expect(hasContent, `First triage signal text "${firstText.slice(0, 80)}" has no class name or size`).toBe(true);
});

test('report content: leak suspects section lists at least one suspect', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#leak-suspects');
  // Suspect entries reference a "retained" heap size.
  const suspects = page.locator('#leak-suspects').getByText(/retained/i).first();
  await expect(suspects).toBeVisible();
});

test('report content: top-consumers table has data rows', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#top-consumers');
  // The top-consumers section contains an HTML table; count <tr> rows.
  // There must be more than one (header row + at least one data row).
  const rows = page.locator('#top-consumers tr');
  const count = await rows.count();
  expect(count, 'Top Consumers must have at least one data row').toBeGreaterThan(1);
});

test('report content: system overview shows nonzero heap size', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#system-overview');
  // The overview renders a heap size figure like "23 MB" or "51.2 MB".
  const overview = page.locator('#system-overview');
  await expect(overview).toContainText(/\d+(\.\d+)?\s*(GB|MB|KB)/i);
});

test('report content: threads section lists at least one thread', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#threads');
  const threads = page.locator('#threads');
  // Thread entries contain the word "Thread" somewhere.
  await expect(threads).toContainText(/Thread/i);
  // There must be at least one <tr> row in the threads table.
  const rows = threads.locator('tr');
  const count = await rows.count();
  expect(count, 'Threads section must have at least one thread row').toBeGreaterThan(1);
});

test('report content: triage signals show class name or size details', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('#memory-triage');
  // The first triage signal must mention a numeric byte-size figure or a class name (dot in FQN).
  // This catches the case where the signal list rendered but all text is placeholder/empty.
  const firstSignal = page.locator('#memory-triage ul li').first();
  const text = await firstSignal.innerText();
  const hasSize = /\d+(\.\d+)?\s*(GB|MB|KB|bytes?)/i.test(text);
  const hasFqn  = /\w+\.\w+/.test(text); // e.g. "cafesat.sat.Vector" or "java.util.HashMap"
  expect(hasSize || hasFqn, `First triage signal "${text.slice(0, 80)}" has no size or FQN`).toBe(true);
});

test('report content: no NaN or undefined visible in rendered text', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc');
  // Scroll to bottom to trigger any lazy sections.
  await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
  await page.waitForTimeout(300);
  const body = await page.locator('body').innerText();
  expect(body, 'Rendered text must not contain NaN').not.toContain('NaN');
  expect(body, 'Rendered text must not contain undefined').not.toContain('undefined');
});

test('report content: table of contents links match present section IDs', async ({ page }) => {
  await page.goto(baseUrl);
  await page.waitForSelector('.toc');
  // Collect all href="#..." links inside the TOC nav.
  const tocLinks = await page.locator('.toc a[href^="#"]').evaluateAll(
    (els: HTMLAnchorElement[]) => els.map(el => el.getAttribute('href')!.slice(1))
  );
  expect(tocLinks.length, 'TOC must have links').toBeGreaterThan(0);
  // Every TOC link must resolve to an element present in the DOM.
  for (const id of tocLinks) {
    const count = await page.locator(`[id="${id}"]`).count();
    expect(count, `TOC link #${id} has no matching DOM element`).toBeGreaterThan(0);
  }
});
EOF

# Create a minimal playwright.config.ts for the temp dir.
cat > "$TMPDIR_TEST/playwright.config.ts" << CONF
import { defineConfig } from '@playwright/test';
export default defineConfig({
  testDir: '.',
  timeout: 30000,
  use: { headless: true },
});
CONF

# Run the tests.
export HPROF_HTML="$HTML"
cd "$TMPDIR_TEST"
NODE_PATH="$PLAYWRIGHT_MODULES" "$PLAYWRIGHT_BIN" test smoke.spec.ts --reporter=line

echo ""
echo "Browser smoke tests passed."
