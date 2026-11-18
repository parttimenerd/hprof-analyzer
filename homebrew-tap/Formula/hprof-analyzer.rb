class HprofAnalyzer < Formula
  desc "Fast, low-memory Java HPROF heap-dump analyzer with MCP server and OQL query engine"
  homepage "https://github.com/parttimenerd/hprof-analyzer"
  license "MIT"
  version "0.3.4"

  on_macos do
    on_arm do
      url "https://github.com/parttimenerd/hprof-analyzer/releases/download/v0.3.4/hprof-analyzer-aarch64-apple-darwin.tar.gz"
      sha256 "c46bccec79dbe2cc0c5b09b42edb748c3dabec2f18c6f2fef7579a0aaebb4393"
    end
  end

  on_linux do
    on_intel do
      url "https://github.com/parttimenerd/hprof-analyzer/releases/download/v0.3.4/hprof-analyzer-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "9f6ed2fb977b15d5c3ae7cd1fbf107c7fde37bf79ca56cd659f1185f1f09ec5d"
    end
    on_arm do
      url "https://github.com/parttimenerd/hprof-analyzer/releases/download/v0.3.4/hprof-analyzer-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "f387da54966008bd2f36c88c869a2676d8b3a4d406c9e11e6f7c85ec9a5adad3"
    end
  end

  def install
    bin.install "hprof-analyzer"
  end

  def caveats
    <<~EOS
      Add the MCP server to Claude Code:
        claude mcp add hprof -- hprof-analyzer mcp

      Or add to Claude Desktop (~/.claude/mcp.json):
        { "mcpServers": { "hprof": { "command": "hprof-analyzer", "args": ["mcp"] } } }

      Update in place with:
        hprof-analyzer update latest   # or: update nightly

      See https://github.com/parttimenerd/hprof-analyzer#mcp-server--ai-integration
    EOS
  end

  test do
    system "#{bin}/hprof-analyzer", "--version"
  end
end
