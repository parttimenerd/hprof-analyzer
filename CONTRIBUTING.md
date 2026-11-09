# Contributing to hprof-analyzer

Contributions are welcome — bug fixes, new features, documentation improvements, and test coverage.

## Getting started

### Prerequisites

- **Rust 1.85+** — install via [rustup](https://rustup.rs/) if you don't have it
- **Node.js 18+** — only needed if you modify the web frontend (`web/src/`)
- **Git** — for cloning and version control

### Clone and build

```sh
git clone https://github.com/parttimenerd/hprof-analyzer
cd hprof-analyzer
cargo build --release
# binary at target/release/hprof-analyzer
```

### Run the test suite

```sh
cargo test --release          # unit tests + parity fixtures + scenario tests
cargo fmt --all -- --check    # formatting (must pass before CI will accept)
cargo clippy --release --all-targets -- -D warnings   # lint gate
```

CI runs the same `fmt`, `clippy`, and `test` steps on stable Rust.

## Project structure

```
hprof-analyzer/
├── src/                    # Main Rust crate
│   ├── main.rs             # CLI entry point, clap subcommands
│   ├── lib.rs              # Core library (parser, graph, report builder)
│   ├── pass1.rs            # Pass 1: metadata scan, id_map construction
│   ├── pass2/              # Pass 2: object graph, CSR construction
│   ├── dominator.rs        # SEMI-NCA dominator tree
│   ├── retained.rs         # Retained-size fold
│   ├── rpo_dfs.rs          # Reverse post-order DFS
│   ├── query/              # OQL parser + executor
│   ├── report/             # Report model and section builders
│   ├── mcp/                # MCP server implementation
│   ├── serve.rs            # HTTP server (server subcommand)
│   ├── redact.rs           # Heap redaction
│   ├── secrets.rs          # Secret detection (betterleaks patterns)
│   ├── cache.rs            # On-disk analysis cache
│   ├── mat/                # Eclipse MAT cache file generation
│   ├── diff/               # Report comparison / diffing
│   ├── html.rs             # HTML report template
│   ├── md.rs               # Markdown report renderer
│   └── ...
├── crates/
│   ├── hprof-wasm/         # WebAssembly build of the core library
│   └── hprof-redact/       # Standalone redaction binary (minimal deps)
├── web/src/                # React frontend (HTML report UI)
│   ├── App.tsx             # Main report application
│   ├── charts.tsx          # Chart rendering (d3-based)
│   └── ...
├── web-browser/            # Browser-only WASM UI (drag-and-drop)
├── tests/                  # Integration and regression tests
│   ├── scenarios.rs        # Synthetic scenario tests (fast, no JVM)
│   ├── cli_query.rs        # OQL query integration tests
│   ├── integration.rs      # Full pipeline integration tests
│   ├── parity.rs           # MAT parity validation
│   ├── mat_pipeline.rs     # MAT cache file generation tests
│   ├── fuzzing.rs          # Property-based / fuzz tests
│   ├── fixtures/           # Test heap dumps
│   └── common/             # Shared test helpers (heap_builder.rs)
├── docs/                   # User-facing documentation
├── scripts/                # Build, test, and maintenance scripts
├── config/                 # Default configuration (betterleaks.toml)
├── DESIGN.md               # Algorithm design decisions
├── CHANGELOG.md            # Release history
└── README.md               # User documentation
```

### Key modules

| Module | Purpose |
|--------|---------|
| `pass1.rs` + `pass2/` | Two-pass HPROF parser. Pass 1 builds the address→index map; Pass 2 constructs the object graph CSR. |
| `dominator.rs` | Cooper-Harvey-Kennedy iterative dominator tree (see [DESIGN.md](DESIGN.md) §2). |
| `retained.rs` | Bottom-up retained-size fold over the dominator tree. |
| `query/` | OQL lexer (logos), parser (chumsky), planner, and executor. |
| `report/` | Report model structs and section builders (leak suspects, top consumers, triage, etc.). |
| `cache.rs` | Disk-cached analysis results for the `heap` subcommand group and MCP server. |
| `cvec.rs` | Compressed vector (zstd) for cold arrays during the peak-memory pipeline window. |

## Development workflow

### Making changes

1. **Read the relevant code first.** Understand the existing patterns before editing.
2. **One logical change per commit.** Don't batch unrelated changes.
3. **Run the full gate before pushing:**
   ```sh
   cargo fmt --all -- --check
   cargo clippy --release --all-targets -- -D warnings
   cargo test --release
   ```

### Cache invalidation (important)

The `heap` subcommand group, the MCP server's `load_dump`, and `heap report` all read from a per-dump on-disk cache (`<dump>.hprof-cache/`). The triage report and other analysis results are computed once and cached.

**If you change:**
- Triage rule logic (`report/triage.rs`)
- Report model fields or their population
- Anything that alters `build_model` / `evaluate_triage` output

**You must** bump `VERSION` in `src/cache.rs`. This is the only mechanism that invalidates a stale cached report — the cache checks file existence, not content hashes.

When testing changes locally, clear the cache first:

```sh
hprof-analyzer heap cache-clear <dump>
# or just delete <dump>.hprof-cache/
```

Forgetting this will make a correct code change appear to do nothing when tested against an already-cached dump.

### Web frontend changes

The HTML report embeds a pre-built React bundle (`web/dist/bundle.js`). Node.js is only needed if you modify files under `web/src/`.

```sh
# 1. Install dependencies (first time only)
cd web && npm install && cd ..

# 2. Build the bundle
node web/esbuild.config.mjs

# 3. Rebuild the Rust binary (embeds the new bundle)
cargo build --release

# 4. Generate a dev report (readable source, not minified)
./target/release/hprof-analyzer tests/fixtures/dump_1_mnemonics.hprof \
  dist/report_test.html --dev --obj-graph

# 5. Open in browser
open dist/report_test.html
```

For faster iteration, use `hprof-analyzer server` which serves the bundle live from disk, skipping steps 3–4. See [docs/dev-mode.md](docs/dev-mode.md).

### WebAssembly (browser build)

```sh
# Build the WASM module
wasm-pack build crates/hprof-wasm --target web --release

# Assemble the standalone browser HTML
python3 web-browser/assemble.py
```

## Testing

### Test categories

| Test file | What it covers | Speed |
|-----------|---------------|-------|
| `scenarios.rs` | Synthetic heap scenarios — "does the report name the right culprit?" | Fast (no JVM, no dump files) |
| `cli_query.rs` | OQL query correctness against fixture dumps | Fast |
| `integration.rs` | Full analysis pipeline end-to-end | Medium |
| `parity.rs` | MAT vs. hprof-analyzer output comparison | Medium |
| `mat_pipeline.rs` | MAT cache file generation correctness | Medium |
| `fuzzing.rs` | Property-based fuzz tests (proptest) | Fast |
| `leak_demo_regression.rs` | JVM-captured leak demo dumps (opt-in) | Slow |

### Adding a new scenario test

The fastest way to add a regression test is via `tests/scenarios.rs`. Each scenario is a table row that builds a synthetic heap and checks the report output. See the doc comment at the top of that file for exact steps.

### Adding OQL test cases

Add test cases to `tests/cli_query.rs`. Each test runs one or more OQL queries against a fixture dump and asserts on the output.

## Submitting changes

1. Fork the repository and create a feature branch.
2. Make your changes with clear, focused commits.
3. Ensure `cargo fmt`, `cargo clippy`, and `cargo test` all pass.
4. Open a pull request with a description of what changed and why.

Bug reports and feature requests are tracked via [GitHub issues](https://github.com/parttimenerd/hprof-analyzer/issues). Issue templates are provided for bug reports, feature requests, and questions.

## Architecture reference

For detailed algorithm documentation (shallow-size formulas, dominator tree, GC root normalization, exclude-pair edges, leak suspect heuristics), see [DESIGN.md](DESIGN.md).

For the OQL language specification, see [docs/OQL.md](docs/OQL.md).

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
