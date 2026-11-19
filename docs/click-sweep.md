# click-sweep — Exhaustive UI Crash Detector

`click-sweep.ts` is a standalone Playwright script that exhaustively visits
every interactive element in a self-contained hprof HTML report and reports any
JavaScript errors, React render-error boundaries, or crashed components it
finds. Its goal is to find **crashing parts of the UI** — stack overflows,
render bugs, unhandled rejections — by actually exercising the whole report
rather than testing a predetermined set of paths.

## Quick Start

```bash
# Build the binary, generate a report, and run a 60-second sweep:
./scripts/run-click-sweep.sh /path/to/app.hprof

# 5-minute CI-style soak:
./scripts/run-click-sweep.sh /path/to/app.hprof --budget 300

# Skip report generation (use a pre-made one):
HPROF_HTML=/tmp/my-report.html ./scripts/run-click-sweep.sh

# Or invoke click-sweep.ts directly if you already have a report:
cd tests/playwright
HPROF_HTML=/tmp/my-report.html npx ts-node click-sweep.ts --budget 300
```

## How It Works

### Phase 1 — Scroll pass

Scrolls the entire page from top to bottom, triggering any lazy-rendered React
sections before the click pass begins.

### Phase 2 — Click sweep

Repeatedly queries the DOM for all `button`, `[onclick]`, `[role="button"]`,
`[role="tab"]`, and `[role="menuitem"]` elements. Clicks each one it has not
seen before, then re-queries the DOM (new elements may have appeared as a result
of the click — sidebars opening, flamegraph tabs appearing, accordion panels
expanding). Repeats until no new elements remain or the budget expires.

Each element is identified by a **fingerprint**: nearest-ancestor `id` +
tag name + sorted CSS classes + trimmed text. This means a "Close" button inside
a sidebar and a "Close" button inside a modal are treated as two distinct
targets.

### Phase 3 — Hover pass

Hovers over every table row, SVG rectangle, `[data-tip]`, `[title]`, and
similar elements that show copy buttons or tooltips on hover. After each hover
it clicks any newly revealed clickable elements.

### Error collection

Throughout all phases, the script listens for:

- `page.on('pageerror')` — uncaught JS exceptions (e.g. stack overflows)
- `page.on('console', type='error')` — `console.error` calls
- `page.on('console', type='warning')` — `console.warn` calls (only counted as
  failures with `--warnings`)
- Presence of `.render-error` in the DOM — React's ErrorBoundary class

Each finding records which element interaction triggered it.

## Options

| Option | Default | Description |
|---|---|---|
| `--budget <s>` | 60 | Total time budget in seconds. The sweep stops when the budget expires; skipped elements are reported. |
| `--delay <ms>` | 200 | Pause after each click/hover. Increase on slow machines or animated UIs. |
| `--no-headless` | (headless) | Show the browser window — useful for debugging what the script is clicking. |
| `--warnings` | off | Also fail on `console.warn` messages. |
| `--no-hover` | (hover on) | Skip the hover pass. Faster, but misses hover-only UI like copy buttons and tooltips. |
| `--verbose` | off | Print every click/hover action with its fingerprint, and log all console messages including warnings and `console.log`. Does not by itself cause warnings to fail the run. |
| `--help` | | Print usage and exit. |

## Environment

| Variable | Required | Description |
|---|---|---|
| `HPROF_HTML` | yes | Path to the self-contained HTML report file to test. |

## Exit Codes

| Code | Meaning |
|---|---|
| `0` | No JS errors, no render-error boundaries, no warnings (if `--warnings`) |
| `1` | At least one error/boundary/warning found |
| `2` | Usage error (bad arguments, missing/nonexistent `HPROF_HTML`) |

## CI Integration

The standard CI step uses a 5-minute budget:

```yaml
- name: Generate smoke report
  run: |
    ./target/release/hprof-analyzer heap report /tmp/smoke-dump.hprof \
      --format html --output /tmp/smoke-report.html

- name: Run click sweep (5-minute budget)
  working-directory: tests/playwright
  env:
    HPROF_HTML: /tmp/smoke-report.html
  run: npx ts-node click-sweep.ts --budget 300
```

The step exits 1 if any JavaScript error or React render-error boundary
occurred during the sweep, failing the CI run. Warnings are not counted as
failures in CI unless `--warnings` is passed.

## Wrapper Script

`scripts/run-click-sweep.sh` handles the full pipeline:

1. Builds `target/release/hprof-analyzer` if it is missing or stale.
2. Generates `/tmp/click-sweep-report.html` from the provided dump.
3. Installs `node_modules` if `tests/playwright/node_modules` is absent.
4. Runs `click-sweep.ts`, forwarding all extra arguments.

```bash
./scripts/run-click-sweep.sh /path/to/dump.hprof [click-sweep options...]
```

If `HPROF_HTML` is already set, the binary and report steps are skipped.

## Interpreting the Output

```
hprof click-sweep
  Report : philosophers.html
  Budget : 60s
  Delay  : 200ms
  Hover  : yes
  Verbose: no
  Mode   : headless

  Scrolling... done
  Clicking:.....................
  Hovering:...........

── Results ──────────────────────────────────────────────────────
  Clicked  : 22 unique elements in 8.1s
  Hovered  : 11 elements
  Skipped  : 0
  Rounds   : 3
  Errors   : 0
  Render ✗ : 0

✓ No issues found.
```

| Field | Meaning |
|---|---|
| `Clicked` | Unique elements actually clicked |
| `Hovered` | Elements hovered in the hover pass |
| `Skipped` | Elements whose fingerprint was seen but were never actually reached (budget ran out, or they disappeared) |
| `Rounds` | How many times the click loop re-queried the DOM looking for new elements |
| `Errors` | JS errors + console.error messages |
| `Render ✗` | Times a React ErrorBoundary appeared |

When issues are found they appear after the results table:

```
── Issues ───────────────────────────────────────────────────────
  ✗ [error] after "/dominator-analysis/button[]:Flame"
      Maximum call stack size exceeded
          at buildNode (bundle.js:4321)
```

The trigger string is the fingerprint of the element that was being clicked or
hovered when the error appeared. Use `--verbose` and `--no-headless` together to
watch the sweep in real time and see which UI state preceded the error.

## Example: Debugging a Crash

```bash
# Run with verbose output and a visible browser:
./scripts/run-click-sweep.sh /path/to/dump.hprof \
  --no-headless --verbose --budget 120

# Output will show each action:
#   → click [1]  /button[]:Dominator Analysis
#   → click [2]  /dominator-analysis/button[]:Flame
#   ✗ pageerror [/dominator-analysis/button[]:Flame]: Maximum call stack size exceeded
```

The trigger fingerprint (`/dominator-analysis/button[]:Flame`) identifies the
exact button whose click caused the crash. You can then reproduce it manually:
navigate to `#dominator-analysis` and click the Flame button.
