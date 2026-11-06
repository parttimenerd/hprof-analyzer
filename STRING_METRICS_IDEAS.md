# String metrics: deferred accuracy work

Status: **deferred** (2026-11-04). Do not implement the deeper analysis as part of the current visualization-catalog review.

## Why the former savings numbers were unsound

The opt-in duplicate-String pass (`src/pass2/strings.rs`) decodes Java `char[]`/`byte[]` storage into a Rust UTF-8 `String`. Its former `backing_array_bytes - decoded_utf8_bytes` calculation reported false slack: a fully used UTF-16 ASCII array has twice as many bytes as its decoded UTF-8 text. Its former `(String_instance_count - 1) × decoded_utf8_bytes` calculation did not account for shared backing-array identities, physical encoding, array overhead, or references from elsewhere. Neither figure is a verified count of reclaimable heap bytes. A duplicate value is not necessarily a duplicate allocation; a duplicate allocation is not necessarily safe to reclaim.

## Deferred options, in order of cost

1. **Keep the inexpensive census (current conservative direction):** report total String instances, distinct/duplicated value counts, masked examples, decoded *UTF-8* length distributions, and the number of distinct String backing arrays examined. Do not display a String storage-savings or slack estimate, or include one in triage/reclaimable totals. Clearly label deprecated JSON fields so `0` does not mean “confirmed no waste.”
2. **Potentially shareable backing storage (preferred later experiment):** in the existing opt-in duplicate pass, aggregate by decoded-value hash **and distinct backing-array ID**, using physical array sizes from HPROF. Count each allocation only once even if multiple Strings point to it; avoid summing UTF-8 lengths as Java bytes. Report a *potential* physical footprint or candidate savings with explicit 64-bit-hash-collision and shared-reference caveats. Distinguish array payload from its shallow heap size (array header and alignment), and choose whether String objects themselves are in scope. This is not a guaranteed reclaimable-byte metric; validate memory/runtime cost before shipping.
3. **Verified reclaimable storage (more expensive):** inspect inbound references/reachability, ownership and any non-String users of the arrays; model what an actual sharing/compaction intervention would free. Avoid double counting arrays also reported in primitive-array duplication or other waste categories. This may require graph analysis and may not be suitable for the lightweight pass.

Do **not** revive the old “String backing-array slack” calculation simply by changing the UTF-8 multiplier: establishing slack requires reliable Java String layout/offset/count semantics and encoding, not just decoded length. For modern Strings, the backing array may already be exactly filled.

## Current worktree handoff

A partial conservative correction was started before this deferral. It changed `src/pass2/strings.rs`, `src/pass2/model.rs`, `src/report/triage.rs`, `src/report/build.rs`, `src/report/render_md.rs`, `src/report/render_graphs.rs`, `src/report/mod.rs`, `src/mcp/mod.rs`, `web/src/App.tsx`, `tests/scenarios.rs`, and bumped `src/cache.rs` from 13 to 14. It keeps duplicate counts and sample text but zeros deprecated byte fields, and retains an examined-backing-array census. The React view filters old reports' misleading String waste claims. **This work has not been validated as complete**: the last `cargo test --lib --quiet && cargo test --test scenarios --quiet` exited 101; the failure was not investigated after the user requested a stop. A pre-change UTF-16 regression did fail as expected. Check current file contents and tests rather than assuming this handoff is up to date.

Before shipping any present or future change: inspect test failures, run focused Rust tests and synthetic scenarios plus web build/typecheck, audit JSON/Markdown/MCP/UI and old-report compatibility, and bump the report-cache version when newly computed report content changes. Follow `CLAUDE.md` for build/benchmark environment. Do not overwrite unrelated worktree changes. Neither a benchmark nor a deeper reference graph analysis was performed here.
