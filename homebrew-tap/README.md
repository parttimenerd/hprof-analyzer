# homebrew-hprof-analyzer

Homebrew tap for [hprof-analyzer](https://github.com/parttimenerd/hprof-analyzer) — a fast, low-memory Java HPROF heap-dump analyzer with a built-in MCP server for Claude/Cline and an interactive `heap` CLI.

## Install

```sh
brew tap parttimenerd/hprof-analyzer
brew trust parttimenerd/hprof-analyzer   # required once for third-party taps (Homebrew 6+)
brew install hprof-analyzer
```

After install, Homebrew prints setup instructions for the MCP server.

### Nightly build (tracks `main`)

```sh
brew install parttimenerd/hprof-analyzer/hprof-analyzer-nightly
```

## Quick usage

Generate a self-contained HTML report:

```sh
hprof-analyzer heap.hprof report.html
```

Interactive cached analysis (fast after first run):

```sh
hprof-analyzer heap summary heap.hprof
hprof-analyzer heap query heap.hprof --oql "SELECT @displayName, COUNT(*) FROM INSTANCEOF java.lang.Object GROUP BY @displayName ORDER BY COUNT(*) DESC LIMIT 20"
hprof-analyzer heap browse heap.hprof
```

Update to the latest nightly build:

```sh
hprof-analyzer update nightly
```

For Homebrew-managed installs this command detects the brew Cellar and tells
you to use `brew upgrade` instead. Or upgrade directly — channels are
separate formulas, so each upgrades independently:

```sh
brew upgrade hprof-analyzer-nightly     # nightly channel
brew upgrade hprof-analyzer             # stable channel
```

To switch channels:

```sh
brew uninstall hprof-analyzer && brew install hprof-analyzer-nightly      # stable → nightly
brew uninstall hprof-analyzer-nightly && brew install hprof-analyzer      # nightly → stable
```

> The nightly formula uses `sha256 :no_check` because the tarball is
> re-uploaded on every push to `main`; the stable `hprof-analyzer` formula
> pins a checksum per release.

OQL language reference (no dump needed):

```sh
hprof-analyzer heap docs --topic examples
```

## MCP server — Claude & Cline integration

**Claude Code:**
```sh
claude mcp add hprof -- hprof-analyzer mcp

# With a specific dump pre-loaded (skips load_dump step)
claude mcp add hprof -- hprof-analyzer mcp --dump /path/to/heap.hprof
```

**Cline (VS Code)** — click **MCP Servers → Add Server** in the Cline panel, or add directly to Cline's MCP settings file:
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

**Claude Desktop** (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows):
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
