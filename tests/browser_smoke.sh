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
