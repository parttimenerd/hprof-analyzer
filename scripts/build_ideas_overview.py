#!/usr/bin/env python3
"""Build docs/ideas-overview.html from the VISUALIZATION_IDEAS*.md files.

Run from the repo root:  python3 scripts/build_ideas_overview.py
"""
import html
import re
from pathlib import Path

from ideas_agent_notes import COMMON, for_id
from ideas_demo_fixtures import PRIORITY

ROOT = Path(__file__).resolve().parent.parent
MAIN = ROOT / "VISUALIZATION_IDEAS.md"
DEFERRED = ROOT / "VISUALIZATION_IDEAS_DEFERRED.md"
OUT = ROOT / "docs" / "ideas-overview.html"

FIELDS = ["Verdict", "Analysis", "Pros", "Cons", "Improve", "Size", "Rust"]

# Mini-demo kind per idea, plus the matching full interactive demo (if any).
DEMO = {
    "A1": ("sunburst", "demo-sunburst"), "A2": ("heat", "demo-heatmap"),
    "A3": ("waterfall", None), "A5": ("tree", None),
    "B1": ("sankey", "demo-sankey"), "B2": ("path", None),
    "B3": ("stack", None), "B4": ("bars", "demo-b4"),
    "K1": ("bars", None), "K2": ("bars", None), "K4": ("path", None),
    "C1": ("stack", "demo-budget"), "C2": ("table", "demo-frameworks"),
    "C3": ("scatter", None), "C4": ("hist", None), "C5": ("milestones", None),
    "I2": ("treemap", None), "K3": ("scatter", "demo-k3"), "K5": ("table", "demo-k5"),
    "D1": ("bars", "demo-j5"), "D2": ("bars2", "demo-waste"),
    "D3": ("stack", None), "D4": ("table", None), "D5": ("stack", None),
    "J1": ("bars", "demo-j1"), "J2": ("bars2", None), "J3": ("table", None),
    "J4": ("table", None), "J5": ("bars", "demo-j5"), "J7": ("table", None),
    "L1": ("stack", None), "L2": ("hist", "demo-l2"), "L3": ("bars", "demo-n1"),
    "L4": ("bars", None), "N1": ("bars", "demo-n1"), "N2": ("table", "demo-n2"),
    "E1": ("stack", "demo-threads"), "E2": ("table", "demo-e2"),
    "E3": ("bars", "demo-e3"), "E4": ("table", None), "E5": ("table", None),
    "I1": ("stack", None), "N4": ("bars", "demo-e3"),
    "F1": ("tree", None), "F2": ("table", None), "F3": ("tree", None),
    "G1": ("spark", "demo-sparklines"), "G3": ("table", None),
    "H1": ("bars", "demo-blame"), "H2": ("signals", "demo-h2h3"),
    "H3": ("table", "demo-h2h3"), "I3": ("stack", None), "M2": ("table", None),
    "N3": ("kpi", None),
    "P1": ("table", "demo-pools"), "P2": ("table", "demo-n2"),
    "P3": ("table", "demo-n2"), "P4": ("bars2", "demo-b4"),
    "X1": ("ux", None), "X2": ("ux", None), "X3": ("ux", None), "X4": ("ux", None),
    "X5": ("path", None), "X6": ("path", None), "X7": ("ux", None),
    "A4": ("chord", None), "A6": ("tree", None), "D6": ("bars", None),
    "G2": ("treemap", None), "I4": ("flame", None), "J6": ("table", None),
    "M1": ("table", None), "M3": ("sunburst", None),
}


def inline(md: str) -> str:
    s = html.escape(md, quote=False)
    s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r"\1", s)
    return s


def parse_ideas(text: str, origin: str):
    ideas, cur, section = [], None, ""
    for line in text.splitlines():
        if line.startswith("## "):
            if line.startswith("## Appendix"):
                break
            section = line[3:].strip()
            cur = None
            continue
        m = re.match(r"^### ([A-Z]\d+)\. (.+)$", line)
        if m:
            cur = {"id": m[1], "title": m[2].strip(), "section": section,
                   "origin": origin, "f": {}}
            ideas.append(cur)
            continue
        if cur:
            m = re.match(r"^- \*\*(\w+):\*\* (.*)$", line)
            if m and m[1] in FIELDS:
                cur["f"][m[1]] = m[2]
                cur["_last"] = m[1]
            elif line.startswith("  ") and cur.get("_last"):
                cur["f"][cur["_last"]] += " " + line.strip()
    return ideas


def parse_verdicts(text: str):
    out, on = {}, False
    for line in text.splitlines():
        if line.startswith("## User verdicts"):
            on = True
            continue
        if on and line.startswith("## "):
            break
        if on:
            m = re.match(r"^\| ([A-Z]\d+) \| ([^|]+) \| ([^|]+) \| ([^|]+) \|", line)
            if m:
                out[m[1]] = {"rating": m[2].strip(), "shown": m[3].strip(),
                             "why": m[4].strip()}
    return out


def category(rating: str) -> str:
    for c in ("Must-have", "Helpful", "Nice-to-have", "Skip", "Merged", "Shared UX", "Separate"):
        if rating.startswith(c):
            return c
    return rating


def priority_demo(idea_id: str) -> str:
        fixture = PRIORITY[idea_id]
        heading = "".join(f'<th scope="col">{html.escape(c)}</th>' for c in fixture["columns"])
        rows = "".join("<tr>" + "".join(f"<td>{html.escape(cell)}</td>" for cell in row)
                                     + "</tr>" for row in fixture["rows"])
        bars = ""
        if "visual" in fixture:
                visual = fixture["visual"]
                maximum = max(visual["values"]) or 1
                items = "".join(
                        f'<li><span class="metric-name">{html.escape(row[0])}</span>'
                        f'<span class="metric-rail" aria-hidden="true"><span style="width:{value / maximum * 100:.2f}%"></span></span>'
                        f'<span class="metric-value">{value:,} {html.escape(visual["unit"])}</span></li>'
                        for row, value in zip(fixture["rows"], visual["values"]))
                bars = (f'<div class="metric-bars"><strong>{html.escape(visual["metric"])}</strong>'
                                f'<small>Bar widths relative to largest example row—not a share of the heap.</small>'
                                f'<ul>{items}</ul></div>')
        zero_button = ('<button type="button" class="state-tab" data-state="zero" '
                                     'aria-pressed="false">Zero observed</button>') if "zero" in fixture else ""
        zero_panel = (f'<div data-demo-state="zero" class="demo-state" hidden>'
                                    f'<p><strong>In the report:</strong> {html.escape(fixture["zero"])}</p></div>') if "zero" in fixture else ""
        return f'''<div class="priority-demo">
            <div class="example-label">Illustrative example · not a measured heap dump</div>
            <p class="demo-question">{html.escape(fixture["question"])}</p>
            <div class="state-tabs" role="group" aria-label="Example state for {idea_id}">
                <button type="button" class="state-tab" data-state="sample" aria-pressed="true">Example</button>
                {zero_button}
                <button type="button" class="state-tab" data-state="unavailable" aria-pressed="false">No data</button>
            </div>
            <div data-demo-state="sample" class="demo-state">
                {bars}
                <div class="sample-table"><table><thead><tr>{heading}</tr></thead><tbody>{rows}</tbody></table></div>
                <p class="demo-takeaway">{html.escape(fixture["takeaway"])}</p>
            </div>
            {zero_panel}
            <div data-demo-state="unavailable" class="demo-state" hidden>
                <p><strong>In the report:</strong> {html.escape(fixture["unavailable"])}</p>
            </div>
            <p class="demo-source"><b>Input:</b> {html.escape(fixture["source"])}</p>
        </div>'''


def card(idea, verdicts):
    i = idea["id"]
    v = verdicts.get(i, {})
    rating = v.get("rating") or ("Separate" if idea["origin"] == "deferred"
                                 else "Shared UX" if i.startswith("X") else "—")
    cat = category(rating)
    kind, full = DEMO.get(i, ("table", None))
    rows = "".join(
        f'<div class="fld fld-{k.lower()}"><h4>{"Technical notes" if k == "Analysis" else k}'
        f'</h4><p>{inline(idea["f"][k])}</p></div>'
        for k in FIELDS if k in idea["f"] and k != "Verdict")
    link = (f'<a class="full" href="visualization-demos.html#{full}" target="_blank">'
            f'Open full interactive demo ↗</a>') if full else ""
    shown = f'<span class="shown">Shown when: {inline(v["shown"])}</span>' if v else ""
    a = for_id(i)
    effort = ("Review only" if cat in ("Skip", "Merged") else
              "Frontend only" if a["rust"].startswith("none") else
              "Deferred" if a["rust"].startswith("Deferred") else "Rust + frontend")
    agent = f'''<details class="agent" open><summary>Agent notes — <b>{effort}</b></summary>
      <dl><dt>Inputs</dt><dd>{html.escape(a["data"])}</dd><dt>Rust</dt><dd>{html.escape(a["rust"])}</dd>
      <dt>UI</dt><dd>{html.escape(a["ui"])}</dd><dt>Acceptance</dt><dd>{html.escape(a["test"])}</dd></dl></details>'''
    schema_status = ("Existing input / derivation" if a["schema"].upper().startswith("EXISTING")
                     or (a["rust"].startswith("none") and not a["schema"].upper().startswith("PROPOSED"))
                     else "Planning sketch — verify, not implemented")
    contract = (f'<div class="contract"><h4>Data contract · {schema_status}</h4>'
                f'<pre>{html.escape(a["schema"])}</pre></div>') if a["schema"] != "—" else ""
    why = f'<p class="why">{inline(v["why"])}</p>' if v else ""
    slug = re.sub(r"[^a-z0-9]+", "-", cat.lower()).strip("-")
    drawing = (priority_demo(i) if i in PRIORITY else
               f'<div class="merge-banner">Merged into another view: {html.escape(a["ui"])}</div>'
               if cat == "Merged" else
               f'<div class="mini" data-kind="{kind}" data-seed="{i}"></div>'
               '<div class="example-label">Concept sketch · not an implementation spec</div>')
    searchable = (i + " " + idea["title"] + " " + " ".join(idea["f"].values())
                  + " " + " ".join(a.values())).lower()
    expanded = i in PRIORITY
    card_class = "card" if expanded else "card collapsed"
    return f'''
<article class="{card_class}" id="{i}" data-rating="{html.escape(cat)}" data-section="{html.escape(idea["section"])}" data-text="{html.escape(searchable)}">
  <header>
        <span class="id">{i}</span><h3><button type="button" class="card-toggle" aria-expanded="{str(expanded).lower()}" aria-controls="body-{i}">{inline(idea["title"])}</button></h3>
    <span class="badge b-{slug}">{html.escape(rating)}</span>
  </header>
  <div class="meta">{shown}</div>
    <div class="body" id="body-{i}">
    <div class="demo">{drawing}{link}{contract}</div>
    <div class="info">
      <div class="verdict"><h4>Verdict</h4><p>{inline(idea["f"].get("Verdict", "—"))}</p>{why}</div>
      {agent}
      {rows}
    </div>
  </div>
</article>'''


def main():
    main_text = MAIN.read_text()
    ideas = parse_ideas(main_text, "main") + parse_ideas(DEFERRED.read_text(), "deferred")
    for d in ideas:
        if d["origin"] == "deferred":
            d["section"] = "Separate (not in default flow)"
    verdicts = parse_verdicts(main_text)
    sections = list(dict.fromkeys(d["section"] for d in ideas))
    ratings = ["Must-have", "Helpful", "Nice-to-have", "Skip", "Merged", "Shared UX"]
    counts = {r: sum(1 for d in ideas if category(verdicts.get(d["id"], {}).get("rating")
              or ("Shared UX" if d["id"].startswith("X") else "")) == r) for r in ratings}

    body = []
    for s in sections:
        cards = "".join(card(d, verdicts) for d in ideas if d["section"] == s)
        sid = re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")
        body.append(f'<section class="grp" id="s-{sid}"><h2>{html.escape(s)}</h2>{cards}</section>')
    toc = "".join(
        f'<a href="#s-{re.sub(r"[^a-z0-9]+", "-", s.lower()).strip("-")}">{html.escape(s)}</a>'
        for s in sections)
    chips = "".join(
        f'<button class="chip" data-r="{r}">{r} <b>{counts[r]}</b></button>' for r in ratings)

    common = "".join(f"<li>{c}</li>" for c in COMMON)
    OUT.write_text(TEMPLATE.replace("%%TOC%%", toc).replace("%%CHIPS%%", chips)
                   .replace("%%COMMON%%", common)
                   .replace("%%BODY%%", "".join(body)).replace("%%N%%", str(len(ideas))))
    print(f"wrote {OUT.relative_to(ROOT)} with {len(ideas)} ideas")


TEMPLATE = r"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>hprof-analyzer — visualization ideas review</title>
<style>
:root{--bg:#f6f7f9;--card:#fff;--ink:#1d2433;--mute:#5d6778;--line:#e2e6ec;--acc:#2f6fdf;
--must:#0f8a5f;--help:#2f6fdf;--nice:#8a63d2;--skip:#9aa3b2;--merged:#c78a12;--sep:#b0583c}
*{box-sizing:border-box}body{margin:0;font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif;background:var(--bg);color:var(--ink)}
.top{position:sticky;top:0;z-index:5;background:#fff;border-bottom:1px solid var(--line);padding:12px 24px}
.top h1{font-size:18px;margin:0 0 8px}.row{display:flex;gap:8px;flex-wrap:wrap;align-items:center}
input[type=search]{padding:6px 10px;border:1px solid var(--line);border-radius:6px;min-width:260px}
.chip{border:1px solid var(--line);background:#fff;border-radius:999px;padding:4px 10px;cursor:pointer;font-size:12px}
.chip.on{background:var(--ink);color:#fff;border-color:var(--ink)}
.toc{display:flex;gap:12px;flex-wrap:wrap;margin-top:8px;font-size:12px}.toc a{color:var(--acc);text-decoration:none}
main{max-width:1280px;margin:0 auto;padding:16px 24px 80px}
.intro{background:#fff;border:1px solid var(--line);border-radius:10px;padding:14px 18px;margin:12px 0;color:var(--mute)}
.grp h2{font-size:16px;margin:28px 0 10px;border-bottom:2px solid var(--line);padding-bottom:4px}
.card{background:var(--card);border:1px solid var(--line);border-radius:10px;margin:12px 0;overflow:hidden}
.card header{display:flex;align-items:center;gap:10px;padding:10px 14px;border-bottom:1px solid var(--line)}
.card-toggle{background:none;border:0;color:inherit;font:inherit;text-align:left;cursor:pointer;padding:0}
.card-toggle:focus-visible,.state-tab:focus-visible{outline:2px solid var(--acc);outline-offset:3px}
.card h3{font-size:15px;margin:0;flex:1}.id{font:600 12px ui-monospace,monospace;background:#eef2f8;border-radius:4px;padding:2px 6px}
.badge{font-size:11px;font-weight:600;color:#fff;border-radius:4px;padding:2px 8px}
.b-must-have{background:var(--must)}.b-helpful{background:var(--help)}.b-nice-to-have{background:var(--nice)}
.b-skip{background:var(--skip)}.b-merged{background:var(--merged)}.b-separate{background:var(--sep)}.b-shared-ux{background:#3aa7b9}
.meta{padding:0 14px;font-size:12px;color:var(--mute)}.shown{display:inline-block;margin-top:6px}
.body{display:grid;grid-template-columns:340px 1fr;gap:16px;padding:12px 14px}
.card.collapsed .body,.card.collapsed .meta{display:none}
.demo{border:1px solid var(--line);border-radius:8px;padding:8px;background:#fbfcfe;align-self:start}
.mini svg{width:100%;height:auto;display:block}.full{display:block;margin-top:6px;font-size:12px;color:var(--acc)}
.info{display:grid;grid-template-columns:1fr 1fr;gap:10px 16px}
.info h4{margin:0 0 2px;font-size:11px;text-transform:uppercase;letter-spacing:.04em;color:var(--mute)}
.info p{margin:0}.verdict,.fld-analysis,.fld-improve{grid-column:1/-1}
.verdict{background:#f3f6fb;border-radius:6px;padding:8px 10px}.why{color:var(--mute);margin-top:4px!important}
.fld-pros h4{color:var(--must)}.fld-cons h4{color:var(--sep)}
.fld-rust,.fld-size{background:#faf8f2;border-radius:6px;padding:6px 8px}
code{font:12px ui-monospace,monospace;background:#eef1f5;padding:0 3px;border-radius:3px}
.hidden{display:none}
.agent{grid-column:1/-1;border:1px dashed #3aa7b9;border-radius:6px;padding:6px 10px;background:#f2fbfc}
.agent summary{cursor:pointer;font-size:12px;color:#1d6f7c}.agent dl{display:grid;grid-template-columns:90px 1fr;gap:4px 10px;margin:6px 0 0}
.agent dt{font-size:11px;text-transform:uppercase;color:var(--mute)}.agent dd{margin:0}
.contract{margin-top:8px}.contract h4{margin:0 0 2px;font-size:11px;text-transform:uppercase;color:var(--mute)}
.contract pre{margin:0;white-space:pre-wrap;font:11px/1.4 ui-monospace,monospace;background:#1d2433;color:#e8edf5;padding:6px 8px;border-radius:6px}
.agentcommon ol{margin:6px 0;padding-left:20px}.agentcommon li{margin:2px 0}
.cap{font-size:11px;color:var(--mute);margin-top:4px}
.example-label{font-size:10px;font-weight:700;letter-spacing:.055em;text-transform:uppercase;color:#7a5264;margin:0 0 7px}
.priority-demo{min-height:180px}.demo-question{font-weight:700;font-size:13px;margin:0 0 8px}
.state-tabs{display:flex;gap:4px;margin:6px 0 8px}.state-tab{border:1px solid var(--line);background:#fff;border-radius:4px;padding:3px 9px;cursor:pointer;color:var(--ink)}
.state-tab[aria-pressed="true"]{background:var(--acc);border-color:var(--acc);color:white}
.sample-table{overflow-x:auto}.priority-demo table{width:100%;border-collapse:collapse;font-size:11px;white-space:nowrap}
.priority-demo th,.priority-demo td{text-align:left;padding:4px 6px;border-bottom:1px solid var(--line)}
.priority-demo th{color:var(--mute);font-weight:600}.priority-demo tbody tr:first-child{background:#edf4ff}
.demo-takeaway,.demo-source,.demo-state>p{font-size:11px;margin:8px 0 0;line-height:1.45}
.demo-takeaway{border-left:3px solid var(--acc);padding-left:7px}.demo-source{color:var(--mute)}
.demo-state[hidden]{display:none}
.merge-banner{padding:14px;border:1px solid var(--line);border-radius:6px;color:var(--mute);font-size:12px}
.metric-bars{margin:8px 0}.metric-bars strong{display:block;font-size:11px}
.metric-bars small{display:block;color:var(--mute);font-size:10px}
.metric-bars ul{list-style:none;padding:0;margin:6px 0}.metric-bars li{display:flex;align-items:center;gap:6px;margin:4px 0;font-size:10px}
.metric-name{width:34%;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.metric-rail{background:#e2e8f4;height:12px;border-radius:3px;flex:1;overflow:hidden}
.metric-rail span{height:100%;display:block;background:var(--acc)}.metric-value{text-align:right;min-width:78px}
@media(max-width:900px){.body{grid-template-columns:1fr}.info{grid-template-columns:1fr}}
@media print{.top{position:static}.card{break-inside:avoid}}
</style></head><body>
<div class="top">
  <h1>Visualization ideas — review overview (%%N%% ideas)</h1>
  <div class="row">
    <input type="search" id="q" placeholder="Filter by ID, title, or text…">
    %%CHIPS%%
    <button class="chip" id="toggle">Collapse all</button>
  </div>
  <nav class="toc">%%TOC%%</nav>
</div>
<main>
<div class="intro">One report; a view is shown only when the heap dump contains its data. Only
heap-dump data is used (no RSS, GC logs, <code>jcmd</code>, JFR, or sidecars). Retained bytes overlap,
so only shallow bytes are summed. Each card shows a schematic mini demo with illustrative sample
data (not a real measurement), the verdict, technical notes, Rust cost, size budget, pros and cons.
Generated from <code>VISUALIZATION_IDEAS.md</code> and <code>VISUALIZATION_IDEAS_DEFERRED.md</code>
by <code>scripts/build_ideas_overview.py</code>.</div>
<details class="intro agentcommon" open><summary><b>Rules for implementing agents (apply to every card)</b></summary><ol>%%COMMON%%</ol>
<p>Workflow per idea: 1) confirm inputs in <code>src/report/model.rs</code>; 2) Rust change + cache VERSION bump if needed;
3) types.ts + schema.json; 4) UI component that hides when data is absent; 5) scenario test; 6) regenerate a report and compare with the mini demo here.</p></details>
%%BODY%%
</main>
<script>
(function(){
const NS='http://www.w3.org/2000/svg',W=320,H=170,P=['#2f6fdf','#0f8a5f','#c78a12','#8a63d2','#b0583c','#3aa7b9','#9aa3b2'];
function rnd(seed){let h=0;for(const c of seed)h=(h*31+c.charCodeAt(0))>>>0;return()=>((h=(h*1664525+1013904223)>>>0)/4294967296);}
function el(t,a,txt){const e=document.createElementNS(NS,t);for(const k in a)e.setAttribute(k,a[k]);if(txt!=null)e.textContent=txt;return e;}
const CL=['HashMap$Node','byte[]','String','ArrayList','Session','char[]','Object[]','CacheEntry'];
function svg(){return el('svg',{viewBox:`0 0 ${W} ${H}`,role:'img'});}
function t(s,x,y,o={}){s.appendChild(el('text',Object.assign({x,y,'font-size':9,fill:'#5d6778'},o),o.txt));}
const K={
bars(s,r){const n=6;for(let i=0;i<n;i++){const v=0.25+0.75*r()*(1-i/9);const y=8+i*26;
 s.appendChild(el('rect',{x:90,y,width:v*220,height:18,rx:3,fill:P[i%P.length]}));
 t(s,4,y+12,{txt:CL[i]});t(s,94+v*220-2,y+12,{txt:Math.round(v*900)+' MB','text-anchor':'end',fill:'#fff'});}},
bars2(s,r){for(let i=0;i<5;i++){const a=0.4+0.6*r(),b=a*(0.2+0.6*r()),y=8+i*32;
 s.appendChild(el('rect',{x:90,y,width:a*220,height:12,fill:'#d8e2f4'}));
 s.appendChild(el('rect',{x:90,y,width:b*220,height:12,fill:P[0]}));
 t(s,4,y+10,{txt:CL[i]});t(s,90,y+24,{txt:'used '+Math.round(b/a*100)+'% of capacity'});}},
stack(s,r){let x=10;const v=[...Array(5)].map(()=>r()),tot=v.reduce((a,b)=>a+b);
 v.forEach((q,i)=>{const w=q/tot*300;s.appendChild(el('rect',{x,y:40,width:w,height:40,fill:P[i]}));
 t(s,x+3,95+(i%2)*12,{txt:CL[i]});x+=w;});t(s,10,25,{txt:'Share of shallow bytes',fill:'#1d2433'});},
table(s,r){t(s,6,14,{txt:'Name','font-weight':600,fill:'#1d2433'});t(s,170,14,{txt:'Count','font-weight':600,fill:'#1d2433'});t(s,240,14,{txt:'Shallow','font-weight':600,fill:'#1d2433'});
 for(let i=0;i<6;i++){const y=34+i*22;if(i%2==0)s.appendChild(el('rect',{x:0,y:y-14,width:W,height:22,fill:'#f1f4f9'}));
 t(s,6,y,{txt:CL[i]});t(s,170,y,{txt:Math.round(r()*90000).toLocaleString()});t(s,240,y,{txt:(r()*400).toFixed(1)+' MB'});}},
scatter(s,r){s.appendChild(el('line',{x1:20,y1:150,x2:310,y2:150,stroke:'#ccd'}));s.appendChild(el('line',{x1:20,y1:10,x2:20,y2:150,stroke:'#ccd'}));
 for(let i=0;i<40;i++){const x=20+r()*290,y=150-Math.pow(r(),2)*140;s.appendChild(el('circle',{cx:x,cy:y,r:2+r()*5,fill:P[i%4],'fill-opacity':.6}));}
 t(s,240,165,{txt:'instance count →'});t(s,24,14,{txt:'↑ bytes/instance'});},
hist(s,r){for(let i=0;i<14;i++){const h=Math.max(4,140*Math.exp(-Math.pow((i-4)/3,2))*(0.6+0.4*r()));
 s.appendChild(el('rect',{x:12+i*21,y:150-h,width:18,height:h,fill:P[0]}));}t(s,12,165,{txt:'buckets (scope in idea notes) →'});},
waterfall(s,r){let y0=150,x=12;const steps=[1,-.25,-.18,-.12,-.08];let cur=130;
 steps.forEach((d,i)=>{const h=Math.abs(d)*cur;const top=d>0?150-h:150-cur;s.appendChild(el('rect',{x,y:top,width:44,height:d>0?h:Math.abs(d)*130,fill:d>0?P[0]:P[4]}));
 if(d<0){cur+=d*130;}x+=60;});s.appendChild(el('rect',{x,y:150-cur,width:44,height:cur,fill:P[1]}));t(s,12,165,{txt:'total → removable parts → remainder'});},
treemap(s,r){let x=0,y=0,w=W,h=H;for(let i=0;i<7;i++){const f=0.35+0.3*r();if(i%2==0){const ww=w*f;s.appendChild(el('rect',{x,y,width:ww,height:h,fill:P[i%P.length],stroke:'#fff'}));t(s,x+4,y+12,{txt:CL[i],fill:'#fff'});x+=ww;w-=ww;}else{const hh=h*f;s.appendChild(el('rect',{x,y,width:w,height:hh,fill:P[i%P.length],stroke:'#fff'}));t(s,x+4,y+12,{txt:CL[i],fill:'#fff'});y+=hh;h-=hh;}}},
sunburst(s,r){const cx=160,cy=85;function arc(r0,r1,a0,a1,c){const p=(R,a)=>[cx+R*Math.cos(a),cy+R*Math.sin(a)];const[x0,y0]=p(r1,a0),[x1,y1]=p(r1,a1),[x2,y2]=p(r0,a1),[x3,y3]=p(r0,a0);const L=a1-a0>Math.PI?1:0;
 s.appendChild(el('path',{d:`M${x0} ${y0}A${r1} ${r1} 0 ${L} 1 ${x1} ${y1}L${x2} ${y2}A${r0} ${r0} 0 ${L} 0 ${x3} ${y3}Z`,fill:c,stroke:'#fff'}));}
 s.appendChild(el('circle',{cx,cy,r:22,fill:'#1d2433'}));let a=-Math.PI/2;for(let i=0;i<5;i++){const d=(0.5+r())*1.2;const e=Math.min(a+d,3*Math.PI/2);arc(24,50,a,e,P[i]);let b=a;for(let j=0;j<3;j++){const f=b+(e-a)*(0.2+0.3*r());const g=Math.min(f,e);arc(52,78,b,g,P[i]+'aa');b=g;}a=e;if(a>=3*Math.PI/2)break;}},
sankey(s,r){const L=['GC roots','Threads','Statics'],M=['Cache','Session','Pool'],R=['byte[]','String','Node'];
 [L,M,R].forEach((col,c)=>col.forEach((n,i)=>{s.appendChild(el('rect',{x:10+c*140,y:15+i*50,width:12,height:36,fill:P[c]}));t(s,26+c*140,36+i*50,{txt:n});}));
 for(let c=0;c<2;c++)for(let i=0;i<3;i++){const j=Math.floor(r()*3);const x1=22+c*140,x2=10+(c+1)*140,y1=33+i*50,y2=33+j*50;
 s.appendChild(el('path',{d:`M${x1} ${y1}C${x1+60} ${y1},${x2-60} ${y2},${x2} ${y2}`,stroke:P[i+3],'stroke-width':4+r()*10,fill:'none','stroke-opacity':.35}));}},
heat(s,r){for(let i=0;i<6;i++)for(let j=0;j<6;j++){const v=Math.pow(r(),3);s.appendChild(el('rect',{x:70+j*40,y:8+i*25,width:38,height:23,fill:`rgba(47,111,223,${.08+v*.9})`}));}
 for(let i=0;i<6;i++)t(s,4,24+i*25,{txt:CL[i]});},
tree(s,r){const rows=['ClassLoader app','  ├ WebappLoader #1','  │  ├ com.acme.* (412 classes)','  │  └ zero instances: 37','  └ WebappLoader #2 (dup)','     └ com.acme.* (412 classes)'];
 rows.forEach((x,i)=>t(s,8,20+i*24,{txt:x,'font-family':'ui-monospace,monospace','font-size':10,fill:i==4?'#b0583c':'#1d2433'}));},
path(s,r){const n=['GC root: Thread "main"','static Registry.INSTANCE','HashMap.table','HashMap$Node','Session #4711  (retains 812 MB)'];
 n.forEach((x,i)=>{const y=12+i*31;s.appendChild(el('rect',{x:10+i*12,y,width:250,height:22,rx:4,fill:i==4?'#fdecea':'#eef2f8',stroke:i==4?'#b0583c':'#ccd'}));t(s,16+i*12,y+15,{txt:x,fill:'#1d2433'});
 if(i<4)t(s,20+i*12,y+30,{txt:'↓'});});},
spark(s,r){for(let k=0;k<4;k++){let v=40,pts=[];for(let i=0;i<8;i++){v+=(r()-.3)*12*(k==0?2:1);pts.push(`${100+i*28},${30+k*38-v/4}`);}
 t(s,4,32+k*38,{txt:CL[k]});s.appendChild(el('polyline',{points:pts.join(' '),fill:'none',stroke:P[k],'stroke-width':2}));}},
signals(s,r){const L=[['HIGH','Unbounded HashMap in CacheService'],['MED','ThreadLocal values on pooled threads'],['LOW','Finalizer population 14k'],['INFO','Coverage: 98% records decoded']],C=['#b0583c','#c78a12','#2f6fdf','#9aa3b2'];
 L.forEach(([a,b],i)=>{s.appendChild(el('rect',{x:6,y:8+i*38,width:44,height:20,rx:3,fill:C[i]}));t(s,28,22+i*38,{txt:a,fill:'#fff','text-anchor':'middle','font-weight':600});t(s,58,22+i*38,{txt:b,fill:'#1d2433'});});},
milestones(s,r){[['top 1',0.34],['top 10',0.71],['top 100',0.93]].forEach(([l,v],i)=>{const y=20+i*48;t(s,6,y+12,{txt:l});
 s.appendChild(el('rect',{x:60,y,width:240,height:18,fill:'#e2e6ec'}));s.appendChild(el('rect',{x:60,y,width:240*v,height:18,fill:P[i]}));t(s,64,y+13,{txt:Math.round(v*100)+'% of retained',fill:'#fff'});});},
kpi(s,r){[['Live heap','3.8 GB'],['Top retainer','41%'],['Waste','612 MB'],['Signals','3']].forEach(([a,b],i)=>{const x=8+(i%2)*156,y=8+Math.floor(i/2)*80;
 s.appendChild(el('rect',{x,y,width:148,height:72,rx:6,fill:'#eef2f8'}));t(s,x+10,y+22,{txt:a});t(s,x+10,y+54,{txt:b,'font-size':22,'font-weight':700,fill:'#1d2433'});});},
flame(s,r){function f(x,w,d){if(d>5||w<8)return;s.appendChild(el('rect',{x,y:150-d*24,width:w-1,height:22,fill:P[d%P.length],'fill-opacity':.85}));let cx=x;while(cx<x+w-4){const ww=Math.min(x+w-cx,w*(0.25+r()*.6));f(cx,ww,d+1);cx+=ww+(r()*6);}}f(4,312,0);},
chord(s,r){const cx=160,cy=85,R=70;for(let i=0;i<6;i++){const a=i/6*2*Math.PI;s.appendChild(el('circle',{cx:cx+R*Math.cos(a),cy:cy+R*Math.sin(a),r:6,fill:P[i]}));}
 for(let k=0;k<8;k++){const a=Math.floor(r()*6)/6*2*Math.PI,b=Math.floor(r()*6)/6*2*Math.PI;s.appendChild(el('path',{d:`M${cx+R*Math.cos(a)} ${cy+R*Math.sin(a)}Q${cx} ${cy} ${cx+R*Math.cos(b)} ${cy+R*Math.sin(b)}`,stroke:P[k%6],fill:'none','stroke-opacity':.5,'stroke-width':3}));}},
ux(s,r){s.appendChild(el('rect',{x:10,y:20,width:180,height:120,rx:6,fill:'#eef2f8'}));for(let i=0;i<5;i++)s.appendChild(el('rect',{x:20,y:32+i*20,width:60+r()*100,height:12,fill:P[0],'fill-opacity':.5}));
 s.appendChild(el('rect',{x:150,y:50,width:160,height:84,rx:6,fill:'#fff',stroke:'#1d2433'}));['HashMap$Node','retained 812 MB','→ Why retained?','→ Show owners'].forEach((x,i)=>t(s,160,68+i*17,{txt:x,fill:i>1?'#2f6fdf':'#1d2433'}));}
};
const CAP={bars:'Concept bar: width = named metric in the card, not necessarily bytes. Reuse an existing chart wrapper.',
bars2:'Concept paired bars: denominator and units depend on the idea; do not infer capacity from this sketch.',
stack:'Only stack disjoint additive measures (e.g. exclusive shallow), never overlapping retained values.',
table:'Concept table: columns here are placeholders; use the card inputs and verdict, not these mock headings.',
scatter:'Scatter (chartSetup.ts): log axes, radius = retained, hover = X1 tooltip.',
hist:'VBar (charts.tsx): fixed buckets, counts; never interpolate.',
waterfall:'Waterfall: total, then subtractive parts, then remainder; parts never exceed total.',
treemap:'ZoomableTreemap / RetainedTreemap: area = shallow or retained (label which).',
sunburst:'Optional mode over dominator tree; centre = selected owner; include self+unshown slice.',
sankey:'Concept only: current type-reference weights are not conserved flow; prefer A2 pair table.',
heat:'Heatmap: rows owner class, cols payload class, opacity = bytes; table fallback.',
tree:'Indented tree (domTree.tsx style), lazy expand, counts per node.',
path:'Root-to-object path, one node per hop: root type, field name, class, retained on target.',
spark:'Sparkline per class across comparable dumps: show scale per row; 2 dumps are a delta, not a trend.',
signals:'Triage digest: rule order by default, severity pill + measured fact + link to evidence.',
milestones:'Exact top-1/10/100 shares only (ConcentrationChart); no curve.',
kpi:'KpiStrip tiles: one number + label, each tile links to its section.',
flame:'Icicle/flame from allocation traces; only when traces are present.',
chord:'Chord (deferred): prefer the A2 table.',ux:'Interaction pattern: tooltip/inspector/context menu applied to all views.'};
document.querySelectorAll('.mini').forEach(m=>{const s=svg();(K[m.dataset.kind]||K.table)(s,rnd(m.dataset.seed));m.appendChild(s);
 const c=document.createElement('div');c.className='cap';c.textContent=CAP[m.dataset.kind]||'';m.appendChild(c);});
const q=document.getElementById('q');let active=null;
function apply(){const v=q.value.trim().toLowerCase();document.querySelectorAll('.card').forEach(c=>{
 const ok=(!v||c.dataset.text.includes(v))&&(!active||c.dataset.rating===active);c.classList.toggle('hidden',!ok);
 if(ok&&v){c.classList.remove('collapsed');c.querySelector('.card-toggle').setAttribute('aria-expanded','true');}});
 document.querySelectorAll('.grp').forEach(g=>g.classList.toggle('hidden',!g.querySelector('.card:not(.hidden)')));}
q.addEventListener('input',apply);
document.querySelectorAll('.chip[data-r]').forEach(b=>b.addEventListener('click',()=>{active=active===b.dataset.r?null:b.dataset.r;
 document.querySelectorAll('.chip[data-r]').forEach(x=>x.classList.toggle('on',x.dataset.r===active));apply();}));
document.querySelectorAll('.state-tabs').forEach(tabs=>tabs.addEventListener('click',e=>{
 const button=e.target.closest('button[data-state]');if(!button||!tabs.contains(button))return;
 const demo=tabs.closest('.priority-demo');
 tabs.querySelectorAll('button').forEach(b=>b.setAttribute('aria-pressed',String(b===button)));
 demo.querySelectorAll('.demo-state').forEach(panel=>{panel.hidden=panel.dataset.demoState!==button.dataset.state;});
}));
document.querySelectorAll('.card-toggle').forEach(b=>b.addEventListener('click',()=>{
 const collapsed=b.closest('.card').classList.toggle('collapsed');b.setAttribute('aria-expanded',String(!collapsed));
}));
const tg=document.getElementById('toggle');tg.addEventListener('click',()=>{const c=tg.textContent.startsWith('Collapse');
 document.querySelectorAll('.card').forEach(x=>{x.classList.toggle('collapsed',c);
    x.querySelector('.card-toggle').setAttribute('aria-expanded',String(!c));});tg.textContent=c?'Expand all':'Collapse all';});
})();
</script></body></html>
"""

if __name__ == "__main__":
    main()
