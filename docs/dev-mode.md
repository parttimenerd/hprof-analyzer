# Developer Mode (`--dev`)

The `--dev` flag generates an HTML report where the React bundle is embedded as a readable `<script>` tag instead of the normal deflate+base64 blob. This makes it easy to set breakpoints, inspect component state, and step through React code in browser DevTools.

## Usage

```bash
hprof-analyzer heap.hprof report-dev.html --dev
```

The report data (heap analysis JSON) is still deflate+base64 encoded as usual. Only the application bundle is uncompressed.

## Trade-offs

| | Normal report | `--dev` report |
|--|---------------|----------------|
| Bundle encoding | deflate+base64 (~850 KB compressed) | raw JS (~840 KB uncompressed) |
| Output size delta | — | ~750 KB larger |
| DevTools sources | Minified, hard to read | Readable source with original names |
| React component names | Mangled | Preserved |

## When to use it

- Setting breakpoints in the Object Graph Explorer, OQL chart renderer, or treemap code
- Investigating unexpected rendering behaviour in the report
- Adding `console.log` debugging to the React components during a feature change

## Workflow for UI development

The recommended cycle for changing `web/src/` files and testing the result:

```bash
# 1. Build the bundle
node web/esbuild.config.mjs

# 2. Rebuild the binary (embeds the new bundle)
cargo build --release -p hprof-analyzer

# 3. Generate a dev report
./target/release/hprof-analyzer tests/fixtures/dump_1_mnemonics.hprof \
  dist/report_test.html --dev --obj-graph

# 4. Open dist/report_test.html in the browser
open dist/report_test.html
```

For even faster iteration, the browser tool (`hprof-analyzer server`) serves the bundle live from disk and skips the embed step entirely, removing steps 2 and 3 from the cycle.
