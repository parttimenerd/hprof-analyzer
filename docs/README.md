# Documentation

User guides and reference documentation for hprof-analyzer.

## Guides

| Document | Description |
|----------|-------------|
| [Getting Started](getting-started.md) | Installation, taking heap dumps, your first report, common workflows |
| [OQL Reference](OQL.md) | Full OQL language reference — grammar, attributes, aggregates, joins, visualization directives, worked examples |
| [Object Graph Explorer](obj-graph.md) | Interactive MAT-style object navigation in HTML reports |
| [Troubleshooting](troubleshooting.md) | Common issues and solutions |

## Reference

| Document | Description |
|----------|-------------|
| [MAT Cache Generation](mat-cache.md) | Using hprof-analyzer to speed up Eclipse MAT's first open |
| [Report Size Benchmarks](report-size-benchmarks.md) | Object graph capture tier benchmarks and size trade-offs |
| [Developer Mode](dev-mode.md) | `--dev` flag for debugging the HTML report's React UI |
| [JSON Schema](schema.json) | JSON Schema (draft-2020-12) for the report format |

## Architecture and internals

| Document | Description |
|----------|-------------|
| [DESIGN.md](../DESIGN.md) | Algorithm design: shallow-size formulas, dominator tree, GC roots, leak suspects, top consumers |
| [CONTRIBUTING.md](../CONTRIBUTING.md) | Project structure, build instructions, testing, and contribution guidelines |
| [CHANGELOG.md](../CHANGELOG.md) | Release history and feature changelog |

## AI and LLM integration

| Document | Description |
|----------|-------------|
| [llms.txt](../llms.txt) | Machine-readable project summary for AI assistants |
| [README — MCP server](../README.md#mcp-server) | MCP server setup for Claude, Cline, and other agents |
| [README — AI assistant integration](../README.md#ai-assistant-integration) | Overview of all AI integration options |

## Sample reports

A live viewer showing all four output formats (plain Markdown, Markdown with ASCII graphs, HTML, JSON) is available at:

**[Open the sample report viewer](https://parttimenerd.github.io/hprof-analyzer/reports/)**

Individual samples from the Renaissance `scala-doku` benchmark:

| Format | Default | Full analysis |
|--------|---------|---------------|
| Markdown | [scala-doku.md](samples/scala-doku.md) | [scala-doku-full.md](samples/scala-doku-full.md) |
| Markdown + ASCII graphs | [scala-doku.graphs.md](samples/scala-doku.graphs.md) | [scala-doku-full.graphs.md](samples/scala-doku-full.graphs.md) |
| HTML | [scala-doku.html](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku.html) | [scala-doku-full.html](https://parttimenerd.github.io/hprof-analyzer/samples/scala-doku-full.html) |
| JSON | [scala-doku.json](samples/scala-doku.json) | [scala-doku-full.json](samples/scala-doku-full.json) |
