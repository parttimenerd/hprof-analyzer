# Changelog

All notable changes to hprof-analyzer are documented here.

## Unreleased

### Added

- **`.tar.gz` / `.tgz` input support.** Heap dumps packaged as gzip-compressed
  tar archives (e.g. `heap.hprof.tar.gz`) are now accepted everywhere a plain
  `.hprof` or `.hprof.gz` is accepted — CLI, `diff`, `server`, `query`, and
  `mat` subcommands. The archive is streamed on-the-fly; no decompression to
  disk is needed. Archives with or without a directory prefix are handled
  transparently.

- **Android ART root-tag support.** Five HPROF sub-tags emitted by the Android
  Runtime are now parsed correctly and reported with human-readable names:
  - `0x89` `ROOT_INTERNED_STRING` — Interned String
  - `0x8b` `ROOT_DEBUGGER` — Debugger
  - `0x8d` `ROOT_VM_INTERNAL` — VM Internal
  - `0x8e` `ROOT_JNI_MONITOR` — JNI Monitor
  - `0xc3` `PRIM_ARRAY_NODATA_DUMP` — primitive array declaration without
    element bytes (ART-only)

- **`ROOT_SYSTEM_CLASS` (`0x00`) support.** Sub-tag emitted by IBM J9 and other
  non-HotSpot JVMs. Previously produced `unknown heap sub tag 0x00` during pass
  1 and caused the analysis to abort. It is now handled correctly: treated as a
  GC root and counted in the GC-roots-by-type breakdown. Because it marks system
  classes as unconditionally reachable (same semantics as `ROOT_STICKY_CLASS`),
  it suppresses synthetic system-class root generation in pass 2 — preventing
  phantom object inflation on IBM JVM dumps.

### Fixed

- Eliminated `unknown heap sub tag` errors for Android ART and IBM J9 heap
  dumps. The parser previously aborted on any unrecognised sub-tag; it now
  handles all sub-tags documented in OpenJDK's `heapDumper.cpp` and Android
  ART's `hprof.cc`.

- Bundle-size budget test updated from 1100 KB to 1600 KB to reflect the
  Cytoscape.js addition (interactive Object Graph Explorer).
