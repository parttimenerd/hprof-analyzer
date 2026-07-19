// esbuild build for the hprof-analyzer HTML report.
//
// Bundles src/index.tsx (React + chart.js / react-chartjs-2) into a single minified IIFE with
// CSS inlined, written to dist/bundle.js. The bundle is embedded COMPRESSED in
// the emitted HTML, so this file is the whole client app.
//
// Deterministic: no content hashing, no timestamps in the output; esbuild's
// output for a fixed input + version is byte-stable, which the CI freshness
// check relies on.
import { build } from "esbuild";
import { readFileSync, statSync } from "node:fs";

// ≤ 410 KB minified bundle budget. Enforced here so a source
// change that blows the budget fails the build (and therefore CI).
const BUDGET_BYTES = 410 * 1024;

await build({
  entryPoints: ["src/index.tsx"],
  bundle: true,
  minify: true,
  format: "iife",
  target: ["es2020"],
  legalComments: "none",
  loader: { ".css": "text" },
  define: { "process.env.NODE_ENV": '"production"' },
  outfile: "dist/bundle.js",
});

const size = statSync("dist/bundle.js").size;
const kb = (size / 1024).toFixed(1);
if (size > BUDGET_BYTES) {
  console.error(
    `ERROR: bundle is ${kb} KB, over the ${(BUDGET_BYTES / 1024).toFixed(0)} KB budget`,
  );
  process.exit(1);
}
// Guard against an empty/degenerate bundle sneaking through.
const head = readFileSync("dist/bundle.js", "utf8").slice(0, 64);
if (!head.length) {
  console.error("ERROR: bundle is empty");
  process.exit(1);
}
console.log(`bundle.js: ${kb} KB (budget ${(BUDGET_BYTES / 1024).toFixed(0)} KB)`);
