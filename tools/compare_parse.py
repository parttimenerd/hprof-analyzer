#!/usr/bin/env python3
"""
Compare pass1 parse output (our Rust tool) against hprof-redact diagnose output.
Usage: python3 compare_parse.py <dump.hprof> <hprof-analyzer-binary>
"""
import json, re, subprocess, sys
from pathlib import Path

def run_diagnose(dump_path):
    result = subprocess.run(
        ["java", "-jar", str(Path.home() / "hprof-redact.jar"),
         "diagnose", "--histogram", dump_path],
        capture_output=True, text=True
    )
    text = result.stdout
    data = {}

    for tag, key in [
        ("HPROF_GC_INSTANCE_DUMP", "instances"),
        ("HPROF_GC_OBJ_ARRAY_DUMP", "obj_arrays"),
        ("HPROF_GC_PRIM_ARRAY_DUMP", "prim_arrays"),
        ("HPROF_GC_CLASS_DUMP", "classes"),
        ("HPROF_GC_ROOT_JAVA_FRAME", "root_java_frame"),
        ("HPROF_GC_ROOT_STICKY_CLASS", "root_sticky_class"),
        ("HPROF_GC_ROOT_THREAD_OBJ", "root_thread_obj"),
        ("HPROF_GC_ROOT_JNI_GLOBAL", "root_jni_global"),
        ("HPROF_GC_ROOT_JNI_LOCAL", "root_jni_local"),
        ("HPROF_GC_ROOT_NATIVE_STACK", "root_native_stack"),
        ("HPROF_GC_ROOT_THREAD_BLOCK", "root_thread_block"),
        ("HPROF_GC_ROOT_MONITOR_USED", "root_monitor_used"),
        ("HPROF_GC_ROOT_UNKNOWN", "root_unknown"),
    ]:
        m = re.search(rf"{re.escape(tag)}\s+([\d,]+)", text)
        data[key] = int(m.group(1).replace(",", "")) if m else 0

    data["gc_roots_total"] = sum(
        data.get(k, 0) for k in data if k.startswith("root_")
    )

    m = re.search(r"ID size:\s+(\d+) bytes", text)
    data["id_size"] = int(m.group(1)) if m else 0

    m = re.search(r"Header:\s+(.+)", text)
    data["format"] = m.group(1).strip() if m else ""

    m = re.search(r"HPROF_UTF8\s+([\d,]+)", text)
    data["strings"] = int(m.group(1).replace(",", "")) if m else 0

    hist = {}
    in_hist = False
    for line in text.splitlines():
        if "Class Histogram" in line:
            in_hist = True
            continue
        if in_hist and line.startswith("---"):
            break
        if in_hist:
            m = re.match(r"\s+\d+\s+(\S+)\s+([\d,]+)", line)
            if m:
                hist[m.group(1)] = int(m.group(2).replace(",", ""))
    data["class_histogram"] = hist
    return data

def run_our_tool(dump_path, binary):
    result = subprocess.run(
        [binary, "--dump-json", dump_path],
        capture_output=True, text=True
    )
    if result.returncode != 0:
        print("ERROR from our tool:", result.stderr[:500])
        sys.exit(1)
    return json.loads(result.stdout)

def compare(ref, ours):
    failures = []

    def check(label, r, o, exact=True):
        ok = (r == o) if exact else abs(r - o) / max(abs(r), 1) <= 0.01
        status = "PASS" if ok else "FAIL"
        print(f"  {status}  {label:45s}  ref={r!r:>15}  ours={o!r:>15}")
        if not ok:
            failures.append(label)

    check("id_size",        ref["id_size"],        ours.get("id_size", -1))
    check("format",         ref["format"],          ours.get("format", ""))
    check("instances",      ref["instances"],       ours.get("instances", -1))
    check("obj_arrays",     ref["obj_arrays"],      ours.get("obj_arrays", -1))
    check("prim_arrays",    ref["prim_arrays"],     ours.get("prim_arrays", -1))
    check("classes",        ref["classes"],         ours.get("classes", -1))
    check("gc_roots_total", ref["gc_roots_total"],  ours.get("gc_roots_total", -1))
    check("strings",        ref["strings"],         ours.get("strings", -1))

    ref_top = sorted(ref["class_histogram"].items(), key=lambda x: -x[1])[:20]
    our_hist = ours.get("class_histogram", {})
    for cls, ref_cnt in ref_top:
        our_cnt = our_hist.get(cls, -1)
        check(f"hist/{cls}", ref_cnt, our_cnt)

    print()
    if failures:
        print(f"FAILED ({len(failures)} checks): {failures}")
    else:
        print("All checks PASSED")
    return len(failures)

if __name__ == "__main__":
    if len(sys.argv) < 3:
        print("usage: compare_parse.py <dump.hprof> <hprof-analyzer-binary>")
        sys.exit(1)
    dump   = sys.argv[1]
    binary = sys.argv[2]
    print(f"=== Parsing {dump} ===")
    print("Running hprof-redact diagnose...")
    ref  = run_diagnose(dump)
    print("Running our tool (--dump-json)...")
    ours = run_our_tool(dump, binary)
    print()
    sys.exit(compare(ref, ours))
