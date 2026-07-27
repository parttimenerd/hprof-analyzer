#!/usr/bin/env bash
# scripts/build-oql-guide.sh — render docs/OQL.md into docs/oql/index.html
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
mkdir -p docs/oql

python3 - << 'PYEOF'
import re, sys, html as html_mod

def read(p):
    with open(p, encoding='utf-8') as f:
        return f.read()

md = read('docs/OQL.md')

def esc(s):
    return html_mod.escape(s)

def inline(s):
    # Escape HTML-special chars in the base string first
    s = esc(s)
    # Now apply markup patterns (groups are already-escaped or safe literals)
    s = re.sub(r'\*\*(.+?)\*\*', lambda m: f'<strong>{m.group(1)}</strong>', s)
    s = re.sub(r'(?<!\w)\*(.+?)\*(?!\w)', lambda m: f'<em>{m.group(1)}</em>', s)
    s = re.sub(r'`([^`]+)`', lambda m: f'<code>{m.group(1)}</code>', s)
    s = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', lambda m: f'<a href="{m.group(2)}">{m.group(1)}</a>', s)
    return s

def convert(md):
    lines = md.splitlines()
    out = []
    i = 0
    in_fence = False
    fence_lang = ''
    fence_buf = []
    in_table = False
    in_list = False
    in_olist = False
    table_rows = []
    para_buf = []

    def flush_table(rows):
        buf = ['<table>']
        for ri, row in enumerate(rows):
            tag = 'th' if ri == 0 else 'td'
            buf.append('<tr>' + ''.join(f'<{tag}>{inline(c.strip())}</{tag}>' for c in row) + '</tr>')
        buf.append('</table>')
        return '\n'.join(buf)

    while i < len(lines):
        line = lines[i]

        if re.match(r'^```', line):
            if not in_fence:
                if para_buf:
                    out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                    para_buf = []
                in_fence = True
                fence_lang = line[3:].strip()
                fence_buf = []
                if in_table:
                    out.append(flush_table(table_rows))
                    table_rows = []
                    in_table = False
                if in_list:
                    out.append('</ul>')
                    in_list = False
                if in_olist:
                    out.append('</ol>')
                    in_olist = False
            else:
                in_fence = False
                lang_class = f' class="language-{esc(fence_lang)}"' if fence_lang else ''
                code = esc('\n'.join(fence_buf))
                out.append(f'<pre><code{lang_class}>{code}</code></pre>')
                fence_buf = []
            i += 1
            continue

        if in_fence:
            fence_buf.append(line)
            i += 1
            continue

        if line.startswith('|'):
            cells = [c for c in line.split('|') if c.strip()]
            if all(re.match(r'^[-:]+$', c.strip()) for c in cells):
                i += 1
                continue
            if not in_table:
                if para_buf:
                    out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                    para_buf = []
                if in_list:
                    out.append('</ul>')
                    in_list = False
                if in_olist:
                    out.append('</ol>')
                    in_olist = False
                in_table = True
                table_rows = []
            table_rows.append(cells)
            i += 1
            continue
        else:
            if in_table:
                out.append(flush_table(table_rows))
                table_rows = []
                in_table = False

        m = re.match(r'^(#{1,6})\s+(.*)', line)
        if m:
            if para_buf:
                out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                para_buf = []
            if in_list:
                out.append('</ul>')
                in_list = False
            if in_olist:
                out.append('</ol>')
                in_olist = False
            level = len(m.group(1))
            text = m.group(2)
            slug = re.sub(r'[^a-z0-9]+', '-', text.lower()).strip('-')
            out.append(f'<h{level} id="{slug}">{inline(text)}</h{level}>')
            i += 1
            continue

        # Blockquote
        m = re.match(r'^> ?(.*)', line)
        if m:
            if para_buf:
                out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                para_buf = []
            if in_list:
                out.append('</ul>')
                in_list = False
            if in_olist:
                out.append('</ol>')
                in_olist = False
            if in_table:
                out.append(flush_table(table_rows))
                table_rows = []
                in_table = False
            out.append(f'<blockquote><p>{inline(m.group(1))}</p></blockquote>')
            i += 1
            continue

        if re.match(r'^---+$', line.strip()):
            if para_buf:
                out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                para_buf = []
            if in_list:
                out.append('</ul>')
                in_list = False
            if in_olist:
                out.append('</ol>')
                in_olist = False
            out.append('<hr>')
            i += 1
            continue

        # Ordered list item
        m = re.match(r'^\d+\. (.*)', line)
        if m:
            if para_buf:
                out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                para_buf = []
            if in_list:
                out.append('</ul>')
                in_list = False
            if not in_olist:
                out.append('<ol>')
                in_olist = True
            out.append(f'<li>{inline(m.group(1))}</li>')
            i += 1
            continue

        m = re.match(r'^[-*] (.*)', line)
        if m:
            if in_olist:
                out.append('</ol>')
                in_olist = False
            if not in_list:
                if para_buf:
                    out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                    para_buf = []
                out.append('<ul>')
                in_list = True
            out.append(f'<li>{inline(m.group(1))}</li>')
            i += 1
            continue

        # List item continuation (indented lines while in_list)
        if in_list and line.startswith('  ') and line.strip():
            if out and out[-1].endswith('</li>'):
                out[-1] = out[-1][:-5] + ' ' + inline(line.strip()) + '</li>'
            else:
                out.append(f'<li>{inline(line.strip())}</li>')
            i += 1
            continue

        # Ordered list item continuation
        if in_olist and line.startswith('  ') and line.strip():
            if out and out[-1].endswith('</li>'):
                out[-1] = out[-1][:-5] + ' ' + inline(line.strip()) + '</li>'
            else:
                out.append(f'<li>{inline(line.strip())}</li>')
            i += 1
            continue

        # Close ordered list if we hit a non-matching line
        if in_olist:
            out.append('</ol>')
            in_olist = False

        if line.strip() == '':
            if para_buf:
                out.append(f'<p>{inline(" ".join(para_buf))}</p>')
                para_buf = []
            if in_list:
                out.append('</ul>')
                in_list = False
            if in_olist:
                out.append('</ol>')
                in_olist = False
            out.append('')
            i += 1
            continue

        if in_list:
            out.append('</ul>')
            in_list = False
        if in_olist:
            out.append('</ol>')
            in_olist = False
        para_buf.append(line)
        i += 1

    if in_list:
        out.append('</ul>')
    if in_olist:
        out.append('</ol>')
    if in_table:
        out.append(flush_table(table_rows))
    if para_buf:
        out.append(f'<p>{inline(" ".join(para_buf))}</p>')

    return '\n'.join(out)

body_md = convert(md)

headings = re.findall(r'<h([23]) id="([^"]+)">(.*?)</h\1>', body_md)
nav_items = []
for level, slug, text in headings:
    indent = 'style="padding-left:1.2rem"' if level == '3' else ''
    nav_items.append(f'<a href="#{slug}" {indent}>{text}</a>')
nav_html = '\n'.join(nav_items)

extra_html = '''
<hr>
<h2 id="repl-usage">REPL usage</h2>

<p>Start an interactive session with tab-completion and history:</p>
<pre><code class="language-sh">hprof-analyzer query heap.hprof --repl</code></pre>

<p>The prompt shows <code>oql&gt;</code>. Type a query and press Enter to run it.
Multi-line queries are supported: press Enter mid-query — the REPL detects an
incomplete statement and prompts for more input.</p>

<h3 id="repl-tab-completion">Tab completion</h3>
<p>Tab-complete class names, OQL keywords, <code>@</code> attributes, field names on the most recent
result, <code>!&lt;cmd&gt;</code> commands, named queries, and the <code>-- @viz</code> directive.</p>

<h3 id="repl-commands">REPL commands</h3>
<p>Commands that start with <code>!</code> operate on the last query result and never re-run the query.</p>
<table>
<tr><th>Command</th><th>What it does</th></tr>
<tr><td><code>!help</code> / <code>!help oql</code></td><td>Show command reference or OQL reference</td></tr>
<tr><td><code>!quit</code></td><td>Exit the REPL</td></tr>
<tr><td><code>!last</code></td><td>Re-run the previous query</td></tr>
<tr><td><code>!count</code></td><td>Row count of last result</td></tr>
<tr><td><code>!plan [--raw] &lt;oql&gt;</code></td><td>Show execution plan without scanning the heap</td></tr>
<tr><td><code>!row [N|first|last|next|prev]</code></td><td>Show one row as key=value pairs; navigate with next/prev</td></tr>
<tr><td><code>!obj &lt;class&gt;#&lt;idx&gt;</code></td><td>Inspect a specific heap object (dense index)</td></tr>
<tr><td><code>!top [N]</code> / <code>!tail [N]</code></td><td>First / last N rows (default 10)</td></tr>
<tr><td><code>!filter &lt;pat&gt;</code></td><td>Keep rows matching a substring or <code>/regex/</code></td></tr>
<tr><td><code>!sort &lt;col&gt; [desc]</code></td><td>Sort result by column</td></tr>
<tr><td><code>!select &lt;col&gt;…</code></td><td>Keep only named columns</td></tr>
<tr><td><code>!stats [col]</code></td><td>Numeric summary: min/max/mean/stddev/p50/p90/p99</td></tr>
<tr><td><code>!unique &lt;col&gt; [N]</code></td><td>Distinct value counts, top N by frequency</td></tr>
<tr><td><code>!undo</code></td><td>Restore result before last shaping command</td></tr>
<tr><td><code>!save &lt;file&gt;</code></td><td>Write result to CSV/TSV/JSON (format by extension)</td></tr>
<tr><td><code>!set limit N</code></td><td>Cap rows displayed (0 = unlimited)</td></tr>
<tr><td><code>!classes [pat]</code></td><td>List class names (substring-filtered)</td></tr>
<tr><td><code>!describe &lt;class&gt;</code></td><td>Show fields and types of a class</td></tr>
<tr><td><code>!reachable</code> / <code>!all</code></td><td>Restrict to GC-reachable objects only / include all</td></tr>
<tr><td><code>!run [&lt;name&gt;]</code></td><td>Run a named query (no arg = list all)</td></tr>
<tr><td><code>!analyze</code></td><td>Run full analysis (enables <code>@retainedHeapSize</code>, dominators)</td></tr>
</table>

<h3 id="repl-web">Browser REPL</h3>
<p>An in-browser version of the REPL is available at
<a href="https://parttimenerd.github.io/hprof-analyzer/">parttimenerd.github.io/hprof-analyzer/</a>.
In server-connected mode, paste the URL printed by <code>hprof-analyzer server heap.hprof</code>
and click Connect. All <code>!</code> commands work the same way.</p>

<hr>
<h2 id="differences-from-eclipse-mat-oql">Differences from Eclipse MAT OQL</h2>

<h3 id="extensions-beyond-mat">Extensions (not in MAT)</h3>
<table>
<tr><th>Feature</th><th>Example</th></tr>
<tr><td><code>MEDIAN(e)</code>, <code>PERCENTILE(e, n)</code> aggregates</td><td><code>MEDIAN(@usedHeapSize)</code></td></tr>
<tr><td><code>GROUP BY</code> / <code>HAVING</code></td><td><code>GROUP BY @displayName HAVING COUNT(*) &gt; 100</code></td></tr>
<tr><td><code>CASE WHEN … THEN … ELSE … END</code></td><td><code>CASE WHEN @usedHeapSize &gt; 1000 THEN &#x27;large&#x27; ELSE &#x27;small&#x27; END</code></td></tr>
<tr><td><code>INTERSECT</code> / <code>EXCEPT</code></td><td>Set operations between result sets</td></tr>
<tr><td><code>EXISTS (subquery)</code></td><td>Guard outer query on inner result</td></tr>
<tr><td>Arithmetic in SELECT and WHERE</td><td><code>@usedHeapSize * 8 AS bits</code></td></tr>
<tr><td><code>-- @viz &lt;kind&gt;</code> visualization directive</td><td><code>-- @viz histogram label=class value=bytes</code></td></tr>
<tr><td>Interactive REPL with tab-completion</td><td><code>hprof-analyzer query heap.hprof --repl</code></td></tr>
<tr><td>Named queries library (<code>!run &lt;name&gt;</code>)</td><td><code>!run top-classes-by-count</code></td></tr>
<tr><td>Report embedding (<code>--query</code> / <code>--query-file</code>)</td><td>Fold OQL results into HTML/MD/JSON report</td></tr>
</table>

<h3 id="behavioural-differences">Behavioural differences</h3>
<table>
<tr><th>Area</th><th>MAT</th><th>hprof-analyzer</th></tr>
<tr><td>Unreachable objects</td><td>Discarded at index time</td><td>Included in raw scan (MAT ⊆ ours)</td></tr>
<tr><td><code>s.count</code> / <code>s.offset</code> on String</td><td>Works (pre-JDK 9 layout)</td><td>Unknown field — use <code>s.value</code>, <code>s.coder</code>, <code>s.hash</code></td></tr>
<tr><td>Integer division by zero</td><td>Throws <code>ArithmeticException</code></td><td>Returns <code>NULL</code> — never crashes</td></tr>
<tr><td><code>toString()</code> on non-String</td><td>Calls JVM reflection</td><td>Returns <code>NULL</code> — static analysis only</td></tr>
<tr><td><code>get(n)</code> array/collection access</td><td>Works</td><td>Rejected — navigate the backing field directly</td></tr>
<tr><td><code>eval(…)</code> / <code>${snapshot}</code></td><td>Works</td><td>Not implemented</td></tr>
</table>

<h3 id="not-yet-supported">Not yet supported</h3>
<table>
<tr><th>Construct</th><th>Notes</th></tr>
<tr><td><code>FROM OBJECTS &lt;decimal-id&gt;</code></td><td>Hex address (<code>FROM OBJECTS 0x7f3a</code>) works; decimal ID does not yet</td></tr>
<tr><td><code>s[0]</code> / <code>s[1:3]</code> array element access</td><td>Parsed; execution returns NULL</td></tr>
<tr><td><code>${snapshot}.getClasses()</code></td><td>Class-object iteration not yet implemented</td></tr>
</table>

<hr>
<h2 id="use-with-ai-agents">Use with AI agents</h2>

<p><code>hprof-analyzer</code> exposes an HTTP API via the <code>server</code> subcommand, making it easy to
use as a tool for LLM agents (Claude, GPT-4, etc.).</p>

<h3 id="starting-the-server">Starting the server</h3>
<pre><code class="language-sh">hprof-analyzer server heap.hprof         # listens on 127.0.0.1:7070 by default
hprof-analyzer server heap.hprof --port 8080</code></pre>

<p>The server prints a startup banner with all available endpoints.</p>

<h3 id="claude-code-skill">Claude Code skill</h3>
<p>A ready-made Claude Code skill is included in the repository at
<a href="https://github.com/parttimenerd/hprof-analyzer/blob/main/skills/hprof-analyzer.md">skills/hprof-analyzer.md</a>.
Load it in a Claude Code session with:</p>
<pre><code>@skills/hprof-analyzer.md</code></pre>

<p>The skill tells Claude how to start the server, write OQL queries, interpret
results, and produce useful heap analysis narratives.</p>

<h3 id="example-agent-workflow">Example agent workflow</h3>
<pre><code class="language-sh"># 1. Start the server
hprof-analyzer server heap.hprof &amp;

# 2. In Claude Code, load the skill and point it at the server
# @skills/hprof-analyzer.md
# "Connect to http://127.0.0.1:7070 and identify the top memory consumers"</code></pre>

<p>The agent will POST OQL queries to find large retained sets, walk reference
chains, and produce a plain-language summary of the heap state.</p>

<h3 id="key-endpoints">Key endpoints</h3>
<table>
<tr><th>Method</th><th>Path</th><th>Description</th></tr>
<tr><td>POST</td><td><code>/</code></td><td>Run OQL query → JSON result</td></tr>
<tr><td>POST</td><td><code>/stream</code></td><td>Run OQL query → NDJSON (streaming)</td></tr>
<tr><td>POST</td><td><code>/analyze</code></td><td>Trigger full analysis (enables retained sizes)</td></tr>
<tr><td>GET</td><td><code>/status</code></td><td><code>{"status":"ready"|"analyzing"|"not_started"}</code></td></tr>
<tr><td>GET</td><td><code>/report/overview</code></td><td>System overview section</td></tr>
<tr><td>GET</td><td><code>/report/leaks</code></td><td>Leak suspects section</td></tr>
<tr><td>GET</td><td><code>/report/top</code></td><td>Top consumers section</td></tr>
<tr><td>GET</td><td><code>/report/threads</code></td><td>Thread overview section</td></tr>
</table>

<hr>
<h2 id="embedding-queries-in-reports">Embedding queries in reports</h2>

<h3 id="embed-via-flag">Via <code>--query</code> / <code>--query-file</code> flags</h3>
<p>Pass <code>--query</code> or <code>--query-file</code> to the main <code>hprof-analyzer</code> command to run
queries as part of the full report pipeline. These run <em>after</em> the full analysis,
so <code>@retainedHeapSize</code> and dominator attributes are available.</p>
<pre><code class="language-sh">hprof-analyzer heap.hprof report.html \
    --query "SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 20"

# Or put queries in a file (one per line, blank lines ignored, # comments OK)
hprof-analyzer heap.hprof report.html --query-file queries.oql</code></pre>

<h3 id="embed-via-config">Via <code>[[query]]</code> in a config file</h3>
<p>Persist a query set alongside a dump in a <code>.toml</code> config file. Every
subsequent report run includes the same queries automatically.</p>
<pre><code class="language-toml">[[query]]
name = "threads-by-retained"
oql = """
-- @viz histogram title="Threads by retained heap" label=@displayName value=@retainedHeapSize
SELECT @displayName, @retainedHeapSize FROM java.lang.Thread ORDER BY @retainedHeapSize DESC LIMIT 20
"""

[[query]]
name = "string-count"
oql = "SELECT COUNT(*) FROM java.lang.String"</code></pre>

<p>Pass the config with <code>--config queries.toml</code>. Named queries also appear in the
REPL's <code>!run</code> tab-completion.</p>

<h3 id="viz-directives">Visualization directives (<code>-- @viz</code>)</h3>
<p>Prefix any query with a <code>-- @viz</code> comment to declare how its result is
rendered in reports:</p>
<pre><code>-- @viz histogram title="Threads by heap" label=@displayName value=@usedHeapSize cap=10
SELECT @displayName, @usedHeapSize FROM java.lang.Thread ORDER BY @usedHeapSize DESC</code></pre>

<table>
<tr><th>Kind</th><th>Description</th></tr>
<tr><td><code>table</code> (default)</td><td>Plain data table</td></tr>
<tr><td><code>histogram</code></td><td>Horizontal bar chart (HTML) / ASCII bars (Markdown)</td></tr>
<tr><td><code>piechart</code></td><td>Pie chart (HTML only; table in Markdown)</td></tr>
<tr><td><code>treemap</code></td><td>Treemap (HTML only; table in Markdown)</td></tr>
</table>
'''

css = '''
*, *::before, *::after { box-sizing: border-box; margin: 0; padding: 0; }
body {
  font-family: system-ui, -apple-system, sans-serif;
  background: #0f0f1a;
  color: #c8c8d8;
  line-height: 1.6;
  display: flex;
  min-height: 100vh;
}
nav {
  width: 240px;
  flex-shrink: 0;
  background: #0a0a14;
  border-right: 1px solid #1e1e38;
  padding: 24px 12px;
  position: sticky;
  top: 0;
  height: 100vh;
  overflow-y: auto;
}
nav a {
  display: block;
  padding: 3px 8px;
  font-size: .78rem;
  color: #6080a0;
  text-decoration: none;
  border-radius: 4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
nav a:hover { background: #161628; color: #90b0d0; }
nav .nav-title {
  font-size: .65rem;
  text-transform: uppercase;
  letter-spacing: .08em;
  color: #3a4a5a;
  padding: 16px 8px 6px;
  font-weight: 700;
}
main {
  flex: 1;
  padding: 40px 48px;
  max-width: 900px;
}
h1 { font-size: 2rem; color: #7ab4ff; margin-bottom: .4rem; }
h2 { font-size: 1.35rem; color: #a0c4ff; border-bottom: 1px solid #1e1e38; padding-bottom: .3rem; margin: 2.4rem 0 .8rem; }
h3 { font-size: 1.05rem; color: #80a0cc; margin: 1.6rem 0 .5rem; }
p { margin: .7rem 0; }
a { color: #5090d0; }
a:hover { color: #80b8f0; }
code {
  background: #1a1a2e;
  padding: 1px 5px;
  border-radius: 4px;
  font-family: \'Cascadia Code\', \'Fira Code\', Menlo, Consolas, monospace;
  font-size: .86em;
  color: #d0d8ff;
}
pre {
  background: #111120;
  border: 1px solid #1e1e38;
  border-radius: 6px;
  padding: 14px 16px;
  overflow-x: auto;
  margin: .9rem 0;
}
pre code { background: none; padding: 0; font-size: .84rem; color: #c0c8e8; }
table {
  border-collapse: collapse;
  width: 100%;
  margin: .9rem 0;
  font-size: .88rem;
}
th, td { border: 1px solid #1e1e38; padding: .35rem .65rem; text-align: left; }
th { background: #14142a; color: #90b0d0; }
tr:nth-child(even) td { background: #0e0e1e; }
ul { padding-left: 1.5rem; margin: .5rem 0; }
li { margin: .25rem 0; }
hr { border: 0; border-top: 1px solid #1e1e38; margin: 2rem 0; }
@media (max-width: 700px) {
  nav { display: none; }
  main { padding: 20px 18px; }
}
'''

page = f'''<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>OQL Reference — hprof-analyzer</title>
<style>{css}</style>
</head>
<body>
<nav>
  <div class="nav-title">hprof-analyzer OQL</div>
  {nav_html}
  <div class="nav-title" style="margin-top:16px">Links</div>
  <a href="../">Browser REPL</a>
  <a href="../reports/">Sample Reports</a>
  <a href="https://github.com/parttimenerd/hprof-analyzer">GitHub</a>
</nav>
<main>
{body_md}
{extra_html}
</main>
</body>
</html>'''

out = 'docs/oql/index.html'
with open(out, 'w', encoding='utf-8') as f:
    f.write(page)
size = len(page.encode()) // 1024
print(f"Built {out} ({size} KB)")
PYEOF
