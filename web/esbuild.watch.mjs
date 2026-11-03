// Dev watch mode for the hprof-analyzer HTML report bundle.
//
// Rebuilds dist/bundle.js on every source change. Use with --bundle-path
// to iterate on JS/CSS without recompiling the Rust binary:
//
//   Terminal 1:  cd web && npm run watch
//   Terminal 2:  hprof-analyzer heap report --dev --bundle-path web/dist/bundle.js my.hprof -o /tmp/out.html
//                (re-run after each TS change; no cargo build needed)
import { context } from "esbuild";

const ctx = await context({
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

await ctx.watch();
console.log("Watching src/ — rebuilds dist/bundle.js on change. Ctrl+C to stop.");
console.log("Dev workflow:");
console.log("  hprof-analyzer heap report --dev --bundle-path web/dist/bundle.js <dump.hprof> -o /tmp/out.html");
