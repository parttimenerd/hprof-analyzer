#!/usr/bin/env python3
"""Compare hprof-analyzer markdown output against hprof-redact baseline."""
import re, sys
from pathlib import Path

REL_TOL = 0.05  # 5% tolerance

def parse_num(s: str) -> float:
    """Parse a number that may be bytes with unit or plain integer."""
    if s is None: return -1.0
    s = s.strip().replace(',', '').replace('`', '').strip()
    for unit, factor in [('GB', 1<<30), ('MB', 1<<20), ('KB', 1<<10), ('B', 1)]:
        if s.endswith(unit):
            try: return float(s[:-len(unit)].strip()) * factor
            except ValueError: pass
    try: return float(s)
    except ValueError: return -1.0

def extract_table_by_heading(md: str, heading: str) -> list:
    """Extract table rows after a heading line containing the given text."""
    lines = md.splitlines()
    in_sec = False
    headers = None
    rows = []
    for line in lines:
        if heading.lower() in line.lower():
            in_sec = True
            headers = None
            rows = []
            continue
        if not in_sec: continue
        if not line.startswith('|'): 
            if in_sec and headers is not None and rows:
                break  # end of table
            continue
        if re.match(r'\|[\s\-:|]+\|', line): continue  # separator row
        cells = [c.strip() for c in line.split('|')[1:-1]]
        if not cells: continue
        if headers is None:
            headers = cells
        else:
            if len(cells) >= len(headers):
                rows.append(dict(zip(headers, cells)))
    return rows

def check(label, bval, oval, failures, tol=REL_TOL):
    if bval < 0: return
    if bval == 0:
        ok = oval == 0
    else:
        diff = abs(oval - bval) / bval
        ok = diff <= tol
    status = 'PASS' if ok else 'FAIL'
    diff_pct = abs(oval - bval) / max(bval, 1) * 100
    print(f'  {status}  {label:60s}  baseline={bval:>16.0f}  ours={oval:>16.0f}  diff={diff_pct:5.1f}%')
    if not ok:
        failures.append(label)

def compare(baseline: Path, ours: Path) -> int:
    bmd = baseline.read_text()
    omd = ours.read_text()
    failures = []

    # ── Heap Summary ─────────────────────────────────────────────────────────
    bsum = {r.get('Property', r.get('Metric', '')): r.get('Value', '') 
            for r in extract_table_by_heading(bmd, 'Heap Summary')}
    osum = {r.get('Property', r.get('Metric', '')): r.get('Value', '')
            for r in extract_table_by_heading(omd, 'Heap Summary')}
    
    print('\n=== Heap Summary ===')
    for key in ['Total objects', 'GC roots', 'Classes loaded', 'Total shallow heap']:
        bv = parse_num(bsum.get(key, ''))
        ov = parse_num(osum.get(key, ''))
        if bv >= 0:
            check(f'summary/{key}', bv, ov, failures)
        else:
            print(f'  SKIP  summary/{key} (not found in baseline: {list(bsum.keys())[:5]})')

    # ── Class Histogram top-10 ────────────────────────────────────────────────
    bhist = extract_table_by_heading(bmd, 'Class Histogram')[:10]
    ohist = extract_table_by_heading(omd, 'Class Histogram')[:10]
    print(f'\n=== Class Histogram (top {len(bhist)} entries) ===')
    for i, br in enumerate(bhist):
        bclass = br.get('Class', br.get('Class Name', '?'))
        bret = parse_num(br.get('Retained Heap', br.get('Retained', '')))
        # Find matching row by class name in ours
        or_ = next((r for r in ohist if r.get('Class', r.get('Class Name', '')) == bclass), None)
        if or_ is None:
            print(f'  MISS  hist[{i}] {bclass} (not in our top 10)')
            failures.append(f'hist[{i}] {bclass} missing')
        else:
            oret = parse_num(or_.get('Retained Heap', or_.get('Retained', '')))
            check(f'hist[{i}] {bclass[:45]} retained', bret, oret, failures)

    # ── Top Consumers ─────────────────────────────────────────────────────────
    bbig = extract_table_by_heading(bmd, 'Biggest Objects')[:5]
    obig = extract_table_by_heading(omd, 'Biggest Objects')[:5]
    print(f'\n=== Biggest Objects (top {len(bbig)}) ===')
    for i, (br, or_) in enumerate(zip(bbig, obig)):
        bret = parse_num(br.get('Retained Heap', br.get('Retained', '')))
        oret = parse_num(or_.get('Retained Heap', or_.get('Retained', '')))
        bclass = br.get('Class', '?')
        check(f'biggest[{i}] {bclass[:45]}', bret, oret, failures)

    # ── Summary ───────────────────────────────────────────────────────────────
    print(f'\n  Total failures: {len(failures)}')
    if failures:
        print(f'  FAILED: {failures[:5]}')
        return 1
    return 0

if __name__ == '__main__':
    if len(sys.argv) < 3:
        print('usage: compare_parity.py <baseline.md> <ours.md>')
        sys.exit(1)
    rc = compare(Path(sys.argv[1]), Path(sys.argv[2]))
    sys.exit(rc)
