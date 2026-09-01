# homebrew-hprof-analyzer

Homebrew tap for [hprof-analyzer](https://github.com/parttimenerd/hprof-analyzer) — a fast, low-memory Java HPROF heap-dump analyzer with a built-in MCP server for Claude/Cline and an interactive `heap` CLI.

## Install

```sh
brew tap parttimenerd/hprof-analyzer
brew trust parttimenerd/hprof-analyzer   # required by Homebrew for third-party taps
brew install hprof-analyzer
```

After install, Homebrew prints setup instructions for the MCP server.

### Nightly build (tracks `main`)

```sh
brew install parttimenerd/hprof-analyzer/hprof-analyzer-nightly
```

## Quick usage

Analyze a heap dump:

```sh
hprof-analyzer heap.hprof report.html
```

Interactive cached queries (fast after first run):

```sh
hprof-analyzer heap query heap.hprof --oql "SELECT COUNT(*) FROM java.lang.String"
hprof-analyzer heap summary heap.hprof
hprof-analyzer heap browse heap.hprof
```

OQL reference (no dump needed):

```sh
hprof-analyzer heap docs --topic examples
```

## MCP server — Claude & Cline integration

```sh
# Claude Code
claude mcp add hprof -- hprof-analyzer mcp

# With a specific dump pre-loaded (recommended)
claude mcp add hprof -- hprof-analyzer mcp --dump /path/to/heap.hprof
```

Claude Desktop (`~/.claude/mcp.json`):

```json
{
  "mcpServers": {
    "hprof": {
      "command": "hprof-analyzer",
      "args": ["mcp"]
    }
  }
}
```

See the [main README](https://github.com/parttimenerd/hprof-analyzer#mcp-server--ai-integration) for full documentation.
