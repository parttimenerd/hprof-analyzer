# Report Format Critique

Exhaustive critique of every section across all four output formats: plain Markdown (`--full`),
Markdown+graphs (`--full --md-graphs`), HTML, and JSON. The goal of a heap analysis report is
to help a developer answer three questions quickly:

1. **Where is my heap going?** (which class/component owns the most memory)
2. **Is any of it wasted?** (empty collections, duplicated strings, over-allocated arrays)
3. **Is there a leak?** (growing structures, classloader duplication, pinned threads)

Findings below are ranked by impact (most actionable first).

---

## 1. Summary / Memory Triage

### What it does well
- The `Memory Triage` bullet list is the single most useful section: it surfaces the headline
  problem in plain English before the reader has scrolled at all.
- "Likely problem: X retains N% of the heap — investigate this first." is exactly the right
  tone and density.
- Cross-references (`See [Leak Suspects]`) are good, but only work in HTML/rendered Markdown.

### Problems

**1.1 Summary table duplicates Memory Triage.**
The Summary section has a "Top suspects by retained heap" table followed by a plain-text
"Likely problem:" line. This information (headline retainer, %) is then repeated verbatim
as the first bullet of Memory Triage. Pick one location; remove the duplication.
*Suggestion:* Keep Memory Triage bullets only; replace the Summary table with a single-row
`| Heap | Objects | Classes | Threads |` metadata row. Let Triage be the narrative.

**1.2 Memory Triage bullets are not sorted by actionability.**
"Fixed per-object header overhead" and "Off-heap DirectByteBuffer" appear mid-list, but
they are often less urgent than "Headline retainer" or "Classloader leak". Bugs at top,
structural observations later, then FYI stats.

**1.3 "Empty-collection cemetery" bullet is useful but under-specific.**
"5,806 of 6,307 tracked collections (92.1%) are empty" — but which classes? If it's all
`java.util.ArrayList` inside one framework class, that's a pattern worth naming right in
the triage bullet. Currently the reader must scroll to the Collections section to find out.
*Suggestion:* Include the top-1 class + owner field in the triage bullet,
e.g. "dominated by `java.util.HashMap` (3,200) held in `SomeCache#pending`".

**1.4 Triage misses wasted-slot information.**
The new fill-ratio / wasted-slots data is nowhere in triage. A 95%-empty collection pool
wasting 200 MB should bubble up to triage like a leak does.
*Suggestion:* Add a "Wasted slots" triage bullet when total_wasted_slots × element_size
exceeds a threshold (e.g. 1% of heap). Example:
"**Collection waste:** ~48 MB of backing-array capacity is unused (fill ratio avg 12%);
top waster `SomeCache#items`. See [Collections](#collections)."

**1.5 Triage has no "what to do next" ordering.**
Currently a user reading triage must figure out which bullets are "fix now" vs "FYI".
Add a severity tag or ordering: 🔴 Critical / 🟡 Warning / ⚪ Info, or just reorder by
estimated impact in bytes.

---

## 2. System Overview

### What it does well
- Heap composition (instances / object arrays / primitive arrays / class objects) is valuable
  and not shown in most tools.
- GC roots by type is a fast signal for "why is this alive?".
- HPROF Record Census is uniquely useful for diagnosing truncated/strange dumps — keep it.
- The bar columns in the graphs variant beautifully show relative proportions without visual
  overhead.

### Problems

**2.1 GC Roots by Type has no bar column in plain Markdown.**
Plain Markdown has a bare count table; the graphs variant adds bar columns. The bar column
is pure ASCII and should be present in both variants — it costs nothing.

**2.2 Class Histogram shows only shallow heap, not retained.**
The top-50 class histogram (by shallow heap) gives a false picture for classes that are
proxies (e.g. `$colon$colon` has tiny shallow but huge retained). Either add a "Retained"
column or replace shallow ranking with retained ranking and call it out.
*Note:* The Full variant does have a retained-heap histogram section later, but the at-a-glance
class histogram in System Overview misleads by showing only shallow.
*Suggestion:* Add a "Retained" column (or at minimum a "Retained %" column) to the class
histogram in System Overview.

**2.3 Duplicate Strings: "Wasted bytes" column is misleading.**
`Wasted` shows bytes saved if all duplicates were interned, but the empty string `""` shows
`0 B` wasted even though there are 104 copies. The dedup saving is `(count-1) × char_count × 2`
(or 1 for compact strings), but here it appears as 0. Fix the wasted bytes calculation to
include the backing `byte[]` overhead or note that zero-length strings have no backing
allocation. This trips readers.

**2.4 Duplicate Strings: "Longest Values" exposes binary/garbage data.**
Several entries in the Longest Values table are raw binary content (junk Unicode). These
clutter the table and are not useful; the table should either truncate with `[binary N bytes]`
or skip non-printable strings entirely.

**2.5 Class Histogram truncated at 50 rows with no summary.**
"Top 50" is noted but there's no summary like "2,801 more classes, N KB total". The reader
can't tell if the truncated classes are noise or meaningful. Add a "remaining N classes: M KB"
summary row.

**2.6 Header Overhead section is buried.**
"952,666 objects × 12 B = 10.9 MB (36.6% of heap) in object headers" is a powerful
observation, but it sits at the bottom of System Overview behind Duplicate Classes, after
500+ lines of histogram. Elevate it to triage or move it to a top-level structural analysis
section.

**2.7 Duplicate Classes section is very verbose for large codebases.**
The full-variant shows every class name for every classloader in what can be hundreds of
rows. The reader needs the summary (N duplicated classes, M MB extra retained) at the top,
with details in a `<details>` collapse. Currently the summary is missing entirely.

---

## 3. Leak Suspects

### What it does well
- Each suspect gets a dedicated heading, a retained-size headline, and path-to-GC-root.
- The "Merged Paths to GC Roots" section is the most actionable part of the entire report
  for leak diagnosis; it mirrors MAT's "Path to GC Roots" feature.

### Problems

**3.1 Leak Suspects dominator subtrees are catastrophically verbose.**
For a Scala `HashSet` with 100,000-element linked-list chain, the dominator subtree renders
as 5,000+ lines of nested bullet points, filling most of the report. The `<details>` fold
hides it in HTML but not in plain Markdown — the reader must scroll past it.
For a 100k-element list: the subtree is essentially `A → A → A → …` and adds zero information
beyond the depth count. The tool already truncates at a depth cap but the breadth explosion
is the real problem.
*Suggestion:* Collapse repeated chains: if the same class appears N times at consecutive
depths with the same retained size range, emit "↓ N more `ClassName` nodes (same pattern)"
instead of listing every one. Eclipse MAT does this with a "[…]" row.

**3.2 "Merged Paths to GC Roots" is hard to read without graphical expansion.**
In plain Markdown the merged path is a flat table with indent-via-prefix notation. The
graphs variant doesn't add a tree diagram for this section. An ASCII tree would be much more
readable for even 5-hop paths.

**3.3 Leak Suspects only covers "top N by retained heap".**
There are other important leak patterns not currently surfaced:
- A class whose **instance count** has grown (requires two dumps — not applicable here)
- A class held by a **SoftReference** that survived GC (the only-weakly-retained section
  does list these, but it's not cross-linked from Leak Suspects)
- Objects reachable only via `ThreadLocal` (currently mentioned in Threads but not in Suspects)

**3.4 "Top Components" section needs more context.**
The Components section groups by package prefix, which is useful. But it doesn't explain
*why* a component retains what it does. Add a "top field" or "dominant class" column to the
components table so the reader can see "scala.collection retains 8 MB via `HashSet`".

---

## 4. Top Consumers

### What it does well
- "Biggest Objects (Top-Level Dominators)" is exactly what you need for "what is the single
  biggest thing I can free".
- "Biggest Classes by Retained Heap" correctly ranks by retained, not shallow.
- "Top-Dominator Size Distribution" histogram (graphs variant) is one of the most underrated
  features: it shows whether the heap is held by one giant object or many small ones.

### Problems

**4.1 "Top-Dominator Size Distribution" is only in graphs variant.**
The histogram is text-only (ASCII bar chart + table) — there is no reason it can't be in
plain Markdown too. It is more useful than most things that are in plain Markdown.

**4.2 The Biggest Objects table shows heap address (`#`) which is meaningless to users.**
The `#` column is the HPROF object ID. Users can't do anything with a heap address in a
static report. Replace or drop this column; use the "Owner (Class#field)" column if available,
or just show rank.

**4.3 "Biggest Packages by Retained Heap" stops at package root.**
Showing `java` retaining 26 MB and `java.lang` retaining 24 MB is redundant — the user
already knows `java.lang` is inside `java`. The useful level is 2-3 package segments
(e.g., `scala.collection.immutable`). The tree is good, but the threshold for collapsing
should be higher so intermediate nodes are skipped.

**4.4 Immediate Dominators table has no clear explanation of what "dominates" means here.**
The description says "objects immediately dominated, rolled up by dominator class" but a
user unfamiliar with dominator trees won't know what to do with this. Add a one-line
interpretation: "A large `#Dominated` under one class means that class is a retention hub —
free it and you free all those children."

---

## 5. Dominator Analysis

### What it does well
- "Big Drops" table is excellent: it directly answers "where does memory split from a single
  parent into many children?" This is the most reliable signal for finding cache/collection
  leaks.
- The `Drop` column with a bar chart makes the relative sizes scannable.

### Problems

**5.1 Big Drops table shows duplicate object IDs for the same class.**
Multiple rows of `scala.collection.immutable.$colon$colon` with nearly identical retained
sizes clutter the table without adding information. These are siblings in the same list chain.
*Suggestion:* Group duplicate `(class, retained_range)` rows into a single row with a count:
"10× `$colon$colon` (1.8 MB each)". Show distinct objects only when they have meaningfully
different retained sizes.

**5.2 No "why does this hold memory" annotation.**
The Big Drops table shows `java.lang.Object[]` retaining 8 MB with drop 7.2 MB but doesn't
say this array is a collection backing array, or which collection holds it. The Container
Attribution section answers this but is far away. Add an "Owner hint" column populated from
container attribution when available.

---

## 6. Threads

### What it does well
- Thread overview table with retained heap, priority, daemon flag, and state is comprehensive
  and matches MAT's Thread Overview.
- Per-thread "local root objects" table with retained sizes is actionable.

### Problems

**6.1 Thread sections can dominate the report for programs with deep stacks.**
Thread 1 "main" shows 124 local roots. In a program with many threads or deep frames, each
thread section can be hundreds of lines. The local root objects table should be capped at
top-10 by retained size, with a "… N more roots" note.

**6.2 ThreadLocal values are not surfaced.**
Thread pinning is mentioned in triage ("pins 124 thread-local roots") but ThreadLocal values
are not enumerated. If a thread is pinning a large object through a ThreadLocal reference,
the user needs to see which class is in the ThreadLocal, not just the count.

**6.3 Thread state description is technical (`[alive, waiting, waiting indefinitely, in Object.wait]`).**
Simplify to a readable state: "blocked on Object.wait", "running", "parked". The technical
flags are still useful in JSON but in human-readable output, clarity beats precision.

---

## 7. Arrays by Size

### What it does well
- Top N largest object/primitive arrays by shallow size is a fast path to finding the "big
  arrays" problem.
- "Owner (Class#field)" column in the graphs variant is very useful.
- Per-class tallies below the "biggest" table show aggregate footprint.

### Problems

**7.1 Object arrays and primitive arrays sections are duplicated.**
Both have "Top Arrays" and "Top Array Classes" subsections. The structure is fine, but the
plain Markdown renders them as flat tables with no visual break. In the graphs variant, the
bar column helps. In plain Markdown, consider merging into one section with a Kind column.

**7.2 Top Arrays table shows object IDs not owner hints.**
Same issue as with Biggest Objects: the `#` column (heap address) is useless in static output.
The "Owner (Class#field)" column is what matters and it's often `—`. When owner is unknown,
show the retained size of the array's dominator as a hint.

**7.3 Top Array Classes doesn't show per-class wasted capacity.**
For object arrays, the wasted capacity (null slots) would be valuable here too, not just
in the Collection Fill Ratio section. A class like `cafesat.sat.Vector[]` with high null
density should be flagged in the Arrays section as well.

**7.4 "Largest instance" column in per-class array tables is barely useful.**
`java.lang.Object[] largest instance: 32768 slots` — what object is it? Who holds it?
Replace "largest instance" with "top-1 owner Class#field" for the biggest array.

---

## 8. Collections

### What it does well
- Collections by Kind table gives a bird's eye view: how many lists, maps, deques, etc.
  with size distribution.
- Collection Fill Ratio with per-bucket breakdown is excellent for spotting systematic
  over-allocation.
- The new "Likely wasters by field" table (top contributors by wasted slots) is the
  most actionable new feature: it names the code location responsible.
- The new "Worst individual containers" table names the single most wasteful instance.
- Map Collision Ratio section similarly useful for finding over-sized hash tables.

### Problems

**8.1 "Collections by Size" table column "Shallow" should be "Total Shallow".**
*Already fixed in recent work.* Verify the fixture has `Total Shallow` not `Shallow`.

**8.2 "Collections by Kind" table has no retained column.**
Fill count and shallow are shown, but not retained. A single huge collection that retains
500 MB is indistinguishable from 500 small collections each retaining 1 MB. Add a "Retained"
column (or at minimum "Retained (top-1 instance)").

**8.3 Fill Ratio buckets: "0 (empty)" is too broad.**
Grouping "0 elements, any capacity" into one bucket hides the difference between:
- Collections that were created with capacity 0 (zero-arg constructor, never used)
- Collections that were created with capacity N>0 but never filled (pre-allocated waste)
Break the "0 elements" bucket into "0 elements, capacity 0" and "0 elements, capacity > 0".
The second group is pure waste; the first is defensible.

**8.4 Fill Ratio sections don't show the total wasted bytes, only slots.**
"Wasted Slots" tells you how many backing slots are unused but not how many bytes that
represents. For `Object[]` each slot is 4–8 bytes; for `int[]` it's 4 bytes. Show both
wasted slots and wasted bytes (or at least an estimated wasted bytes column).

**8.5 "Top contributors by wasted slots" sorts by slot count, not byte size.**
A collection of 1,000 int[] with 100 wasted slots each = 100,000 wasted int slots = 400 KB.
A collection of 10 Object[] with 50 wasted slots each = 500 wasted Object slots = 2–4 KB
of array overhead but 500 pointers to potentially large objects. Sort by estimated wasted
*bytes* (wasted_slots × element_size) instead of raw slot count.

**8.6 Constant Primitive Arrays table is extremely noisy.**
The "Constant Primitive Arrays" section lists arrays where all elements are the same value.
Most entries are `byte[] value = 49` (the ASCII '1' character, common in `String` backing
arrays). These are uninteresting. The useful case is a large array that is wastefully
repeated or over-allocated.
*Suggestion:* Filter to entries where:
  (a) the array has > 64 elements AND the value is constant (truly large constant), OR
  (b) there are > N instances of this exact content (true duplication candidate).
Drop single-element or very-short constant arrays; they clutter without insight.

**8.7 "Container Attribution — Most Overall" table doesn't show wasted info.**
The Most Overall attribution table shows retained and element totals but not wasted_slots.
This is the best place to answer "which field/class is responsible for the most waste?" —
add a `Wasted Slots` column and sort by it as an alternate sort option.

**8.8 "Container Attribution — Biggest Single" table doesn't show "kind".**
*Partially fixed (kind added to model).* Verify the rendered Markdown table shows the kind
column in the existing `render_collection_attribution` table render, not just in the new
`render_worst_single_containers` inline helper.

**8.9 No aggregate summary across all collection types.**
After all the fill-ratio and map sections there's no summary: "in total, your collections
have N wasted slots representing approximately M MB". This would be a powerful single number
to show in Triage.

**8.10 Collections section is very long; subsections need navigation.**
The full collections section runs hundreds of lines. Add intra-section links or a mini
table of contents inside the Collections section header.

---

## 9. Fields by Retained Size

### What it does well
- Shows which `Class#field` retains the most memory, aggregated across all instances of
  the holder class. This is exactly the "who is keeping this alive" answer.
- "Runtime Pointee Type" and "Category" columns add meaningful type context.
- The truncation note "(group or pointee cap hit)" is honest about limitations.

### Problems

**9.1 "Elements" column is always 0.**
In the sample report, every row shows `Elements: 0`. This suggests the elements count is
not being populated for fields pointing at non-collection objects. If elements is N/A for
non-collections, remove the column; if it should show something, fix the population.

**9.2 No shallow column for the pointee.**
The table shows Retained but not Shallow for the pointed-to object. For objects with high
retained/shallow ratio (e.g., a field pointing at an array that points at many objects), the
ratio tells you how deep the retention chain is. Add a shallow column.

**9.3 The "Holder Instances" column meaning is confusing.**
`scala.collection.immutable.$colon$colon#next` has 146,181 holders but only 100,000 pointees.
This means some cons cells share a tail (aliasing). That's actually interesting — but the
table doesn't explain it. A "Sharing ratio" column (pointees/holders) would flag aliasing.

**9.4 truncation note should appear before the table, not after.**
"Field grouping was truncated (group or pointee cap hit); ranking is a bounded sample."
This disclaimer should appear above the table header, not after all the data, so the reader
doesn't interpret the table as complete.

---

## 10. Biggest Collections

### What it does well
- Shows the individual largest collections with owner, element count, and value type.
- The "By Kind" subsections allow drilling into lists vs maps vs deques.
- "Value Types (top)" column showing the distribution of element types is unique and useful.

### Problems

**10.1 Combined section and "By Kind" sections duplicate every row.**
The Combined section shows all collections ranked by retained size. The "By Kind — map"
section shows the same map rows. Every map in the top-25 appears twice. Either:
  (a) Remove the Combined section and only have By-Kind tables, or
  (b) Remove the By-Kind sections and add a "Kind" column to the Combined table.
The current duplication wastes space and confuses readers who see the same row twice.

**10.2 "Value Type" column shows the internal node type, not the actual value type.**
For `java.util.HashMap`, the value type is `java.util.HashMap$Node`, not the actual
key/value type. The "Value Types (top)" column has the same issue. This is understandable
(the tool can't traverse inside nodes cheaply) but should be noted: "Value type is the
direct array element, not the logical map value." Users will be confused seeing
`HashMap$Node` instead of `String` or `Integer`.

**10.3 Two HashMaps with 2,048 entries in `Manifest#entries` are suspiciously identical.**
The Combined table shows the same `java.util.jar.Manifest#entries` twice with exactly the
same retained size. This is likely two references to the same object being counted twice,
or two equal objects. Flag potential duplicate/shared objects in the biggest collections
table.

**10.4 Biggest Collections doesn't link back to the Collection Fill Ratio.**
The user sees `java.util.HashMap` with 2,048 entries in the Biggest Collections table.
Was this heavily overloaded? Was it allocated at 4x capacity? The fill-ratio sections have
this data but there's no cross-link. Add a note: "See [Map Collision Ratio](#map-collision-ratio)
for fill efficiency."

**10.5 Deque subsection shows 11 identical ArrayDeque rows.**
All 11 deques are `java.util.zip.ZipFile$CleanableResource#inflaterCache` with 1 element.
This is clearly a single pattern repeated. Show it as one row with count = 11 instead of
11 identical rows.

---

## 11. Collection Contents by Type

### What it does well
- Aggregates element types per collection class — useful for understanding what is stored
  in each collection family.
- Concise: one table, easy to scan.

### Problems

**11.1 Only shows top-level element types, not the types of values in maps.**
For `java.util.HashMap`, the element type is `HashMap$Node`. The user needs to know what
the *values* are, not the internal node wrappers. The "Biggest Collections" `value_types`
field has this info for the top instances; this aggregate view should attempt the same.
*Note: this may not be cheaply computable in aggregate; if so, document the limitation.*

**11.2 Section name "Collection Contents by Type" is confusing.**
It reads as "types of collections" rather than "types of things in collections". Rename to
"Collection Element Types" or "What Collections Hold".

---

## 12. References (Soft/Weak/Phantom)

### What it does well
- Listing referent classes for each reference type is useful for understanding what the JVM
  might reclaim under pressure.
- "Only-weakly retained" subsection is a clever approximation of objects that would be freed
  if all weak references were cleared.

### Problems

**12.1 Referent classes table shows shallow only, not retained.**
`java.lang.Class$ReflectionData` × 21 with 1.3 KB shallow is only weakly retained — but
what is its retained heap? If each `ReflectionData` retains 500 KB via caches, the total
is significant. Add a retained column.

**12.2 "Only-weakly retained" approximation is not explained.**
The `_(approximate)_` tag is honest but unexplained. Add a sentence: "Objects flagged here
have no incoming strong reference other than a weak/soft reference chain — GC pressure would
free them." This helps users understand the significance.

**12.3 No cross-link to the specific referents in the heap histogram.**
`java.lang.invoke.LambdaForm` × 178 held softly — are these also in the Top Consumers
class histogram? There's no link. For any referent type that appears in the Top 50 histogram,
add a note.

**12.4 Phantom reference section is minimal.**
Phantom references indicate objects in the process of finalization / cleanup. The tool shows
what they point at (good) but doesn't say how much native memory or off-heap resource is
at stake. For `java.util.zip.Inflater` × 11 phantom refs, each holds a native zlib stream.
Add a note when the referent type is a known resource holder.

---

## 13. Unreachable Objects

### What it does well
- Summary counts by kind (instances, object arrays, primitive arrays, class objects) give
  a quick structural picture.
- Garbage-Root Dominator Trees show which unreachable objects form cohesive groups.

### Problems

**13.1 Two numbers for unreachable size: shallow and retained — but they show the same value.**
"4,266 unreachable objects retaining 673.0 KB shallow (673.0 KB retained within the
unreachable forest)" — the numbers are identical, which makes "retained" redundant here.
Explain: within the unreachable forest, retained = shallow because all paths start and end
within the forest. Or just drop "retained" and say "673.0 KB shallow (unreachable)".

**13.2 "Class objects" row shows 0 B shallow.**
116 unreachable class objects but 0 B shallow? Class objects have non-zero size in the JVM.
This appears to be a bug in the shallow-size calculation for class dump records, or the
shallow size is being excluded. Investigate and fix.

**13.3 Garbage-Root Dominator Trees render the same verbosity problem as Leak Suspects.**
Trees for soft references → ReflectionData → Field[] → Field → String → byte[] are shown
in full recursive depth. For 10 trees this is already 100 lines. Apply the same chain
compression as recommended for Leak Suspects.

**13.4 No actionable recommendation.**
The section shows what is unreachable but doesn't say whether this is normal or abnormal.
Add context: "Unreachable objects are already eligible for collection but have not yet been
reclaimed. A large unreachable heap (>5% of total) may indicate the JVM was not given
time to GC before the dump was taken, or that finalization is backed up."

---

## 14. Allocation Sites

### What it does well
- Present when stack frames were captured in the dump.

### Problems

**14.1 In the sample, the entire section is one row: "serial 1 | 953,964 | 30.4 MB | 84.82 GB".**
Without meaningful stack frame data, the section is useless. When there are no useful
allocation sites (only "serial" stacks), either:
  (a) Hide the section entirely with a note explaining how to capture stack traces
     (`-XX:+HeapDumpAllObjectsOrientedByAllocationSite` or similar), or
  (b) Show a prominent note: "No per-frame allocation data available in this dump. Run with
     `-XX:StartFlightRecording` or similar to capture allocation stacks."

---

## 15. Retention Concentration

### What it does well
- The "concentration curve" framing (Top 1 / Top 10 / Top 100) is the right mental model
  for distinguishing "one big leak" from "many small leaks".
- The interpretation text is clear and actionable.

### Problems

**15.1 The table has only 4 rows; a chart would be more impactful.**
The retention curve is fundamentally a Pareto chart. Even a simple sparkline showing
the cumulative % at each threshold would be more intuitive than a 4-row table.
*Suggestion:* Add a sparkline: `▁▃▆███` above the table, where each mark is a
threshold from Top-1 to Top-1000.

**15.2 No absolute numbers.**
"Top 10 objects = 92.6%" tells you the share but not the size. Add the retained bytes:
"Top 10 objects = 92.6% = 27.6 MB". Both are needed.

**15.3 "Objects each >=1%" = 4 is unexplained.**
This line says 4 objects each hold at least 1% of the heap. List them by name! This is a
short enough list to enumerate inline.

---

## 16. Dominator-Depth Distribution

### What it does well
- The table is comprehensive and the interpretation text is correct.
- "Half of all live objects sit within N hops" is a good summary stat.

### Problems

**16.1 Table has 50 depth rows shown, then "+41305 deeper buckets" truncated.**
The table is already very long (50 rows for a depth range of 1–50). Showing rows 31–50 all
with "0.0%" is noise. Truncate the table at the last depth with ≥0.1% of objects, then
show a "+ N deeper buckets: M total objects (P% of total)" summary row.

**16.2 A bar chart would be far more readable.**
The graphs variant doesn't add a bar column to this section (unlike other histogram sections).
It should — the distribution is the important thing, not the raw numbers.

**16.3 The "Cumulative %" column plateaus at 70.1% after depth 34.**
This is because the 100k-element Scala linked list ($colon$colon chain, depth 41,355)
holds most objects. 30% of objects are in depths 34–41355, but the table only shows 50
depth rows so the reader never sees where the rest of the objects are. The summary stat
("90% within depth 11273") is the right answer — make that the headline, not the table.

---

## 17. Leak Indicators

### What it does well
- Named indicators (anonymous/generated classes, DirectByteBuffer) are good signal sources.
- Raw numbers behind triage bullets are presented cleanly.

### Problems

**17.1 Section is nearly empty (2 rows) in this dump.**
The section could carry more indicators to make it always-useful:
- Total classloader count (already in summary; duplicate here with a threshold note)
- Objects with pending finalization (finalizer queue depth)
- Number of thread locals per thread
- Total phantom reference count (relates to native resource leaks)
- Large static fields (classes holding huge retained sets)

**17.2 No threshold context.**
"178 anonymous/generated classes" — is that normal? Add a note: "Generated classes above
~500 often indicate lambda capture leaks or framework bytecode generation that is not
being reclaimed."

---

## 18. Glossary

### What it does well
- Clear, correct definitions with Wikipedia links for deeper reading.
- Coverage of all major terms.

### Problems

**18.1 Glossary is at the very end but terms appear throughout the report.**
Many terms (shallow, retained, dominator) appear in the first 10 lines of the report.
Users will encounter them before reaching the glossary. Add tooltips (HTML), footnotes,
or move a condensed reference to the top.

**18.2 Missing terms.**
  - "Collection fill ratio" — used in Collections section
  - "Map collision ratio" — used in Collections section
  - "Only-weakly retained" — used in References section
  - "Compressed OOPs" — mentioned in Heap Summary
  - "Top-level dominator" — used in Retention Concentration

---

## 19. Format-Specific Issues

### Plain Markdown

**19.1 No inline graphs.**
Unlike the graphs variant, plain Markdown has no bar charts. For sections like GC Roots by
Type and Heap Composition, adding simple ASCII `█` bars is trivial and makes the report
much more scannable. This should be the default, not opt-in.

**19.2 Dominator subtrees overwhelm the document.**
In plain Markdown, `<details>` is not always rendered. For CLI/editor use, the large
dominator subtrees (hundreds to thousands of lines) render inline. Add a `--compact` flag
that hard-truncates subtrees to depth 3 with a "… N more nodes" note.

**19.3 TOC links work only in rendered Markdown.**
The Contents section has anchor links that won't resolve in many CLI pagers or text editors.
This is unavoidable but worth noting: the document structure should be readable without
working links (good section ordering + visible headings).

### Markdown + Graphs

**19.4 The graphs variant is the better default.**
ASCII bar charts add zero complexity for readers and significantly improve scannability.
The current design has graphs as opt-in (`--md-graphs`). Make bar charts the default in
all Markdown output; reserve `--no-graphs` for truly minimal output.

**19.5 Some sections get bar columns, others don't.**
GC Roots by Type: yes. Heap Composition: yes (graphs). Class Histogram: no bar column
anywhere. Retention Concentration: no sparkline. Dominator-Depth Distribution: no bar.
Be consistent: every count/size table should get a bar column in the graphs variant.

**19.6 The graphs variant is 6,979 lines for this dump.**
Even with the same content, 7k lines is hard to navigate. Consider whether the "full"
variant truly needs by-kind breakdowns of the top 25 collections when they are also in the
combined table. Reduce duplication (see §10.1 above).

### HTML

**Correction to earlier assumptions.** The HTML report is NOT server-rendered Markdown.
`src/html.rs` emits a single self-contained file that embeds the report JSON (raw-deflate +
base64) and a React bundle (`web/src/`, ~5,600 lines), which renders client-side. So the
HTML critique is a critique of the React UI, and several things §15, §16, §19, §20 flagged
as "missing" already exist here. This is the most important finding of the review: **the
formats are not at feature parity, and the gaps run in BOTH directions** — Markdown lacks
charts the HTML has; the HTML lacks some of the plain tables Markdown has.

*What the HTML already does well (do NOT re-suggest these for HTML):*
- **Sticky sidebar TOC** with IntersectionObserver active-section highlighting
  (`App.tsx` `Nav`), grouped Overview/Analysis/Data/Distribution. Answers §18.1 and the
  "persistent sidebar TOC" question for HTML.
- **Retained treemap** (`charts.tsx` `RetainedTreemap` + `TreemapBar`, package→class drill
  via `PackageNode.children`). This is exactly §20.1 — already built for HTML. §20.1 should
  be re-scoped to "port an ASCII 2-level version to md-graphs", not "add a treemap".
- **Chart.js visualizations:** `TopClassesChart`, `HeapCompositionChart` +
  `CompositionStackedBar`, `GcRootsChart`, `LeakShareChart`, `ConcentrationChart` +
  `ConcentrationStackedBar` (§15.1 sparkline — HTML has a real chart), `DepthHistogramChart`
  (§16.2 — HTML has the bar chart AND the smart tail-fold into a `≥N` bucket that §16.1
  recommends for Markdown), `LoaderRollupChart`.
- **Interactive capped tables:** `useCapped`/`ShowMoreRow` cap every long table at 20 rows
  with a "Show N more"/"Collapse" toggle, plus a global expand-all context. This is a
  better answer to the verbosity problems (§3.1, §6.1, §10, §16.1) than Markdown truncation.
- **Dominator subtree as SVG** (`domTree.tsx` `DomSubtreeSvg`, `UnreachableDomTreeSection`)
  rather than thousands of bullet lines — directly solves §3.1/§13.3 for HTML.
- **Theme toggle** (auto/light/dark, persisted) and **back-to-top**.

*Remaining HTML problems (grounded in `web/src/`):*

**19.7 No sortable/filterable class histogram.**
`TopClassesChart` is a fixed bar chart and the histogram table is capped-but-not-sortable.
MAT's single most-used feature is a histogram you can re-sort by shallow/retained/count and
filter by regex. The data (`HistRow` with `instances`/`shallow`/`retained`) is all present;
this is a pure UI addition. This is the #1 HTML gap. *Data: Have.*

**19.7a The `DepthHistogramChart` recomputes the median in JS.**
`charts.tsx` derives the "half of objects within N hops" stat client-side from `DepthBucket`
counts. Markdown computes the same stat in Rust. Two implementations of one statistic will
drift. Move it to the model (a `median_depth`/`p50_depth` field) so both formats read one
number. *Data: Add (small scalar).*

**19.7b Treemap is HTML-only; the same PackageNode tree is under-used in Markdown.**
`PackageNode` (with `retained_heap` + `children`) already backs the HTML treemap. Markdown
renders it as the "Biggest Packages" indented list but stops at the package root (§4.3). The
model has everything for a 2-level ASCII treemap in md-graphs. *Data: Have.*

**19.7c No cross-links from charts to the tables they summarize.**
The charts and the detail tables are separate; clicking a treemap cell or a histogram bar
should scroll to / filter the corresponding table. Purely a UI wiring change. *Data: Have.*

### JSON

**19.8 JSON is the correct format for tooling but needs a stable schema.**
Verified against `schema/report.schema.json` (3,421 lines) and `model.rs`:
- **Version field IS present:** `Report.schema_version` + `SCHEMA_VERSION: u32 = 6`
  (`model.rs:1262`). Consumers can detect breaking changes. ✓ (earlier suggestion satisfied)
- Optional/additive fields carry `#[serde(default)]` widely (round-trips with older JSON).
- Still needed: a schema changelog documenting what each version bump changed.

**19.9 JSON does NOT lose triage — it is already structured.** *(Correction.)*
`Report.triage: Vec<TriageSignal>` (`model.rs:1244`) is a structured array with
`id`/`severity`/`title`/`detail`/`anchor`/`anchor_label`, and both the Markdown and HTML
renderers are dumb formatters over it (`triage.rs` owns the rules). So the JSON already
carries `{ id, severity, title, detail, anchor }` per signal. The only gap vs. the earlier
suggestion: no machine-readable `value_bytes` on each signal — the byte magnitude behind a
signal lives only inside the prose `detail`. Add an optional `value_bytes: Option<u64>` to
`TriageSignal` so tooling can rank signals without parsing English. *Data: Add (small).*

**19.10 Dominator-depth histogram: verify JSON is complete, not top-50-truncated.**
Markdown truncates the depth table for readability; JSON should serialize the FULL
`DepthBucket` vec. Confirm the serialized `dominator_depth_histogram` is uncapped in JSON
even though Markdown shows "+N deeper buckets". If the model vec itself is capped, that cap
leaks into JSON and tooling gets an incomplete distribution. *Action: verify emit path.*

---

## 20. Cross-Cutting: Information Completeness

### Missing analyses (not currently in the report)

**20.1 Retained-heap treemap — exists in HTML, missing in Markdown.** *(Corrected.)*
The HTML report ALREADY has a retained-heap treemap (`charts.tsx` `RetainedTreemap` +
`TreemapBar`, driven by `PackageNode.retained_heap`/`children`). The remaining gap is that
neither Markdown variant surfaces it: plain MD and md-graphs render `PackageNode` only as
the "Biggest Packages" indented list, which stops at the package root (§4.3). The data is
already in the model (Have). Port a 2-level ASCII treemap (package → class, proportional
bar widths) into md-graphs — it would be far more useful than the current class histogram
and requires no new heap pass.

**20.2 String intern candidates.**
The Duplicate Strings section shows what is duplicated. It should also compute: "if you
interned these M strings, you would save N KB". Then rank the candidate classes by how many
of their fields hold string values that are duplicated. This turns a "FYI" section into an
"action list".

**20.3 Boxed primitives waste.**
`java.lang.Integer`, `Long`, `Double` etc. are commonly over-used in maps and collections
where primitive arrays would serve. The report doesn't quantify this.
*Suggestion:* Count total `Integer`/`Long`/`Double`/`Boolean` instances × 16 bytes overhead
vs. equivalent primitive array cost, and surface as a triage bullet when the savings exceed
1% of heap.

**20.4 No "growth path" analysis.**
The deepest dominator chains (41,355 hops for the Scala list) are pathological but not
described as such. When max depth > 1,000, add a triage bullet: "**Pathological chain
depth:** the longest dominator chain is N hops — this typically indicates a linked list,
tree, or chain of single-element collections. Consider switching to an ArrayList or array."

**20.5 Prim-array type breakdown.**
The report shows `int[]`, `byte[]`, `long[]` etc. in the class histogram but doesn't break
down their uses: codec buffers, cryptography, CLDR locale data, JIT data, etc. At minimum,
flag `int[]` arrays with constant content (already partly done in Constant Primitive Arrays)
and `byte[]` arrays that appear to be string backing (where the string is already tracked).

**20.6 Collection fill ratio does not show "waste by size tier".**
It would be useful to see: "large collections (capacity > 100) have average fill ratio 12%;
medium collections (10–100) have 45%; small collections (<10) have 78%." This tells you
whether the waste comes from a few huge under-filled structures or from many tiny ones.

---

## 21. Priority Summary

Ordered by impact. **Effort** now uses the §22 classes: **H** = render-only (data exists),
**C** = compute-cheap (arithmetic/grouping at render), **A** = needs a new field + heap pass,
**F** = format-plumbing/prose. Prefer H/C/F first — they are cheap and touch no heap pass.

| Priority | Finding | Effort |
|---|---|---|
| **P0** | **BUG: graphs format drops 3 sections** (Boxed Numbers, Dup Prim Arrays, Header Overhead) + ships a dead triage link — make render_graphs delegate to shared render_md section fns (§29.2, §29.3) | F |
| **P0** | **BUG: triage "See X" links dead-end per-format** — anchors authored in mixed namespaces: `#overview` breaks in md (heading slugs to `system-overview`, sample line 60), `#leak-suspects`/`#top-consumers` break in HTML (ids are `leaks`/`top`). Introduce one canonical `SectionId`→per-format-slug map feeding ToC + HTML `id=` + triage `anchor` (§42.0, §42.1) | C |
| **P0** | **Cap the dominator subtree & collapse repeated chains** — it is 43% of plain md / 72% of graphs md; depth≤4, breadth≤5, `×N` collapse, box-drawing fence (§28.1, §29.1, §3.1, §13.3) | C |
| **P0** | **Relabel retained-share "% Heap"** — currently retained÷shallow-total (category error); **define "reachable heap" ONCE (canonical) and have §31.6 sub-MB handling + §25 bp displays reference that same base** (§27.1, §27.2, §27.5, §35.5) | F |
| P0 | **Waste Summary** — one headline "reclaimable N MB" number + 9-row table; **also folds in §34.1 String coder-waste as a 10th row and §34.2 by-package waste as a drill-down sub-view** (§24, §34.1, §34.2, §35.1, §35.2) | C (byte terms H; slot→byte is 8.4/A) |
| P0 | **Heap Origin** spine — one lead-in linking the 5 attribution axes (§25) | H (ordering + links) |
| P0 | Biggest Collections: remove Combined+ByKind duplication (§10.1) | C |
| P0 | Constant Primitive Arrays: filter noise (§8.6) | C |
| **P0** | **BUG: Allocation Sites reports 84.82 GB retained on a 29.8 MB heap** — `e.2 += g.retained[i]` (build.rs:1098) sums nested/overlapping per-object retained sizes ~2,800× (sample md:3411 vs total shallow md:96); drop the multi-counted Retained column (shallow is additive) and reconcile the impossible `953,964 > 952,666` object count (§53.1) | H |
| **P0** | **BUG: Dominator-Depth footer prints `10000.0% cumulative`** — `last_cum * 100.0` (render_md.rs:626) rescales an already-0–100 percent (format.rs:244/268; table body renders it right at md:3457); drop the `* 100.0` so the footer reads `100.0%` (§53.2) | H |
| **P1** | **Triage: rank fired signals by reclaimable bytes**, split 5 orientation signals from real problems (§26.2) | C/F |
| **P1** | **Triage: calibrate false-positive rules** (classloader-leak, threadlocal-leak, name-pattern session/connection/listener, interned-string-bloat) (§26.3) | C |
| **P1** | **Triage: "not analyzed" note for gated rules** so absence ≠ clean (§26.4b) | F |
| **P1** | **Surface off-heap:on-heap ratio** — 134 MB off-heap vs 30 MB heap is buried (§27.3) | C |
| **P1** | **Field-labeled reference path-to-GC-roots** — the one true MAT gap; dominator/merged-path views exist but lack field names to null out (§30.2a) | A |
| **P1** | **Collection Contents: unwrap map-entry wrappers** — 6/7 rows are tautological (`HashMap→HashMap$Node`) (§28.5) | A |
| **P1** | **Ship a healthy sample dump** — both current samples are leak-heavy; empty-state prose is untested by example (§31.1) | F (fixture+regen) |
| **P1** | **BUG: dom-tree SVG uses phantom `--surface`/`--text-muted` vars** → stuck in light mode, breaks in dark theme; swap to real `--card`/`--fg`/`--muted` (§32.1) | F (3 edits) |
| **P1** | **Add a "What to do" clause to each section caption** — sections diagnose but don't prescribe; Container Attribution names the fixable `Class#field` yet never says "bound/evict/null it" (§33.2, §33.3, §33.4) | F |
| P1 | Triage: add wasted-slots / reclaimable-waste bullet (§1.4, §26.5) | C |
| P1 | Summary/Triage: remove duplication (§1.1) | F |
| P1 | Class Histogram: add Retained column (§2.2) | H |
| P1 | Retention Concentration: absolute bytes + enumerate named objects (§15.2, §15.3) | H/C |
| P1 | Bring Markdown to HTML chart parity: bars/sparklines from existing data (§19.1, §19.5) | H |
| P1 | Allocation Sites: hide or explain when no stack data (§14.1) | F |
| P1 | Fields by Retained: drop Elements-always-0 column when non-collection (§9.1) | H |
| **P1** | **JSON version gate contradicts its own evolution machinery** — read path is exact-match `!=` (main.rs:709) yet the model has 50 `#[serde(default)]` for cross-version tolerance; either relax to `<=` or document why exact-match (§36.1) | F |
| **P1** | **Diff ranking is endpoints-only** — every Δ is `retained[last]−retained[0]` (diff_reports.rs:174); a class that spikes at r2 and is reclaimed by rN shows Δ=0 and never appears in Growth Leaders. Rank by peak-vs-baseline / monotonic run so N≥3 series surface transient leaks (§37.1) | C |
| **P1** | **Diff "Net Δ Retained" hides offsetting churn** — sums signed per-class deltas (diff_reports.rs:181), so +500MB `Foo` vs −480MB `Bar` reads as a reassuring +20MB. Show gross-growth / gross-shrinkage instead (§37.2) | C |
| **P1** | **Triage prose hardcodes the §27.1 "% Heap" category error** — `pct_of` divides *retained* by `total_shallow` (triage.rs:190) yet 7 rules label the result "% of reachable heap"/"% of heap" (241/498/1047/1110…); it's the first, highest-trust output and **can print >100%**. Point the prose + numerator at the one canonical base (§45.1) | F |
| **P1** | **Collection waste: rank offenders by BYTES, not slots** (user-requested) — "Worst individual containers" (sample 2805) + "Likely wasters by field" sort by *wasted slots*, and Array Fill Ratio's byte column is hardwired `0 B` (sample 2782–2793). Add `Wasted Bytes = (capacity−used)×slot_width` (arithmetic on data already in the row) and sort by it, so "collections that wasted the most memory by not being filled, + location" is answered directly (§46.1) | C |
| **P1** | **Collection waste: cost single/tiny collections as overhead** (user-requested) — 204 size-≤1 + 5,806 empty are *counted* (sample 2758–2762) but never costed or ranked; a 1-entry HashMap is ≥90% wrapper overhead. Add a size-{0,1} overhead ranking by owning `Class#field` with a "replace with `Map.of`/`List.of`/lazy-init" headline (§46.2) | C |
| **P1** | **Location `Class#method` for stack-held big values** (user-requested) — the largest object in the sample (`Thread` 22.9 MB / 76.7%, sample 2323) is stack-held yet shows no origin, while a 9 KB heap array prints its `Class#field`. Add a **Held via** column that reads `Class#field` (heap) or `Class#method` (stack, from `SignificantFrame.frame` model.rs:698) on *every single-big-value view* — Top Consumers, Leak Suspects, biggest collections/arrays, dup-strings/boxed (§47.1, §47.2) | H + gated A |
| **P1** | **Retained-holder table: rank `Class#field` AND `Class#method` together** (user-requested) — "Fields by Retained Size" (sample 3047) already ranks `Class#field`; add the stack half (sum `SignificantLocal.retained` per frame, model.rs:698–712 — the frame-granularity of the existing `Max. Locals' Retained`) and **merge** into one retained-sorted "who holds the most, field or frame" view; keep the additive/`>100%` caveat (§47.4 ↔ §45) | C + gated A |
| **P1** | **References: rank/size referents by RETAINED, not shallow** — referent tables show `Objects \| Shallow` only (render_md.rs:2630; `RefStatClassRow` model.rs:1206), but a soft-referenced 64 B `HashMap` header can retain a 40 MB cache; the "Only-weakly retained" table's retained size *is* the reclaim-on-clear estimate. Add a `retained` field/column (§50.1) | A (row field) + C (only-weakly sum) |
| **P1** | **`weak-ref-escape` triage gates on object COUNT (≥1000), not bytes** — `WeakRefEscape` sums `row.objects` vs `WEAKREF_FLOOR=1000` (triage.rs:570–576, 42), so 40 escapees retaining 200 MB stay silent while 1,200 tiny ones fire; re-gate on retained bytes + attach `bytes` so it ranks by reclaim potential like the other byte rules (§50.5) | H/C |
| **P1** | **Biggest Collections "Value Type" for maps is always the internal `Node`/`Entry` wrapper** — every map row shows `HashMap$Node`/`ConcurrentHashMap$Node`/`LinkedHashMap$Entry` (sample md:3097/3100/3115), a tautology that never names the key/value class; walk one hop past the node to tally `Node.key`/`Node.value` classes so the column says what maps actually hold (§52.1, §28.5) | A |
| **P1** | **`ClassloaderLeak` triage rule has NO threshold — Warns on any duplicate class** — `max_by_key(total_retained)?` always fires (triage.rs:488), flagging `$colon$colon` in 2 loaders (sample md:65) as a "classic reload leak" (false positive on normal Scala); gate on `loader_count >= ~5` (or diff-growth) + retained floor, downgrade to Info for low counts (§54.1) | C |
| **P1** | **Class Histogram "% Heap" column exists in HTML only, at a precision used nowhere else** — HTML adds a per-class `% Heap` col (App.tsx:581/603, `(retained/totalShallow*100).toFixed(2)`, no `<0.1%` floor) that md (render_md.rs:882–919) and graphs (render_graphs.rs:304–334) lack; the same label means 1-decimal-floored `fmt_pct` in Leak Suspects. Add the column to md+graphs via `fmt_pct`/`fmtPct` or drop it; unify the basis (§56.1, §45.2, §48) | C |
| **P2** | **HTML distribution tables add hand-rolled percent columns md/graphs don't render** — Top-Dominator "% of Dom." (App.tsx:757 `.toFixed(1)`, no floor) is HTML-only; `_bp` percents use three rounding rules (App.tsx:1098 `.toFixed(2)`, 1174/757 `.toFixed(1)`), none via `fmtPct`; route all through the shared formatter and match column sets across formats (§56.2, §41, §45) | C |
| **P2** | **Top Arrays shows Length + Shallow but never fill/null-density** — the 131,072-slot `Object[]`/512 KB (sample md:2941) can't be told from padding, though Fill Ratio knows 37,926 object arrays are 0–10% full wasting 5.9 MB (md:2800); `TopArrayRow` (model.rs:920) has no fill field. Add a Used/Length column to Top Arrays (all formats+JSON) reusing the per-array non-null count the fill pass derives (§57.1, §7.3, §8.4) | C→A |
| **P2** | **The single largest array is unattributed** — biggest `Object[]` + 5 of top-10 show Owner `—` (sample md:2941+); `TopArrayRow.owner` (model.rs:925) is filled only from the `--collections` collection-backing scan, so raw application arrays go unnamed and are absent from "Likely wasters by field" (md:2817). Widen array attribution to any inbound `Class#field` edge (§57.2, §47, §30.2a) | C |
| **P2** | **`_bp` percentages render at four different precisions, none floored** — the same "% of reachable heap" prints `{:.0}%` (fill labels render_md.rs:1587), `{:.1}%` (concentration 547, header 3357), `{:.2}%` (boxed 3284), plus HTML `.toFixed` (§56.2); none uses `fmt_pct`, so sub-0.05% shares read `0.0%` not `<0.1%`. Route every `bp/100.0` through `fmt_pct`; mirror in format.ts (§58.1, §45.1, §41.3) | C |
| **P2** | **Retention Concentration back-computes bytes from a rounded bp** — `bp_to_bytes = (bp*total)/10_000` (render_md.rs:540, render_graphs.rs:473) reconstructs the "Retained" column from the stored basis point, quantizing an exact value the pass had (~3 KB steps at 30 MB) and inheriting the §53.1/§53.2 corruption; store `top{1,10,100}_retained: u64` on `RetentionSummary` (model.rs:148) and derive % from bytes, not bytes from % (§58.2, §53) | C (SCHEMA bump) |
| **P2** | **ToC links to Option-backed sections that render only "None"** — ToC gates `fields_by_size`/`biggest_collections`/`collection_contents`/`alloc_sites` on `Option::is_some()` (render_md.rs:306–317) but bodies guard on the inner vec (`f.rows.is_empty()`, 2311), so a Some-but-empty analysis yields a ToC bullet jumping to a heading whose only content is "None"; match the ToC guard to the body check, mirror in `render_toc_graphs` (§59.2, §31.4, §42) | C |
| **P2** | **Commit a `compare` sample + golden test** — the entire diff output path (~200 lines, real sign/format edges) has no sample in docs/samples and no regression guard (§37.4) | F (capture+regen) |
| **P2** | **`md-graphs` silently downgrades to plain md when inferred** — `graphs.md` output path can't express it (main.rs:313); warn on stderr or honor the `.graphs.md` convention the samples already use (§38.1) | F |
| **P2** | **`--detail max` feeds the dominator blow-up** — sets `dominator_tree_max_nodes=100_000` (main.rs:282), 20× the default §28.1 already flags; apply the rendered-tree collapse regardless of `--detail` (§38.3) | C |
| **P2** | **Collection-config `class` field: no dotted-vs-slash guard** — user writes `com.example.Foo`, builtins use `com/example/Foo` (fielddecode.rs:211), so it matches nothing silently, zero rows, no error (§39.1) | C |
| **P2** | **Document the custom collection-handler TOML** — README covers `--collections` but never `--collection-config`, discovery order, `[[collection]]` shape, or the slash-form requirement (§39.4, §39.5) | F (docs) |
| **P2** | **HTML: no client-side `schema_version` check** — Rust hard-gates version (main.rs:709) but `boot()` renders whatever it parses (index.tsx:33); `schema_version` is only a type field (types.ts:675). Mirror the gate client-side (§40.1) | F |
| **P2** | **HTML: no error boundary + `#root` cleared before render** → any render throw = blank white page; `report.overview.source_name` is unguarded (App.tsx:3583), so a missing top-level key blanks everything (§40.2) | C |
| **P2** | **`format_bytes` labels are binary (1024-base) but named `KB`/`MB`/`GB`** (format.rs:179) — ~7% overstatement vs decimal-GB at the GB tier, never disclosed; add one "sizes are binary" caption line (§41.1) | F |
| **P2** | **`fmt_pct` renders sub-0.05% as "0.0%"** (format.rs:268) — indistinguishable from true zero next to a non-zero byte figure; render `(0,0.05)` as "<0.1%". One 2-line edit fixes §27.7 and every small-share site at once (§41.3) | C |
| **P2** | **Document omittable JSON keys + absent-vs-empty semantics** — esp. `alloc_sites` absent = no allocation tracking, vs present-with-`traces_present:false`; the likeliest integration bug (§36.2) | F (docs) |
| **P2** | **Surface the JSON Schema** — `dev emit-schema` is hidden under `Cmd::Dev`; promote it or commit `docs/schema.json` so the contract is discoverable (§36.5) | F |
| **P2** | **Cross-format anchor-resolution test** — assert every `TriageSignal.anchor` resolves to a real target in *both* md (slugged heading) and HTML (`id=`); this is the durable guard that keeps the §42.1 P0 fix from silently regressing (§26.6, §29.3, §42.3) | C (test) |
| **P2** | **Special-case linked-list depth artifact** — "p90 depth 11273" is one cons chain, not shape (§27.4) | C |
| **P2** | **Document triage threshold provenance** + surface crossed threshold in detail (§26.7) | F |
| **P2** | **Reorder sections into drill-down**, hoist waste analyses out of "System Overview" (§28.6) | F |
| **P2** | **Triage: low-water gate on headline retainer** — downgrade to Info + "diffuse" wording below ~5% so healthy dumps don't cry wolf (§31.2) | C |
| **P2** | **Triage: explicit all-clear line** when every signal is Info — say "heap appears healthy" affirmatively (§31.5) | C |
| **P2** | **Empty sections: hoist emptiness check above header, reconcile ToC vs body** so degenerate dumps don't drift (§31.4) | F |
| **P2** | **HTML: keyboard-operable sortable headers** — add `tabIndex`/`role=button`/`onKeyDown`/`aria-sort` to `SortableTh` + siblings (WCAG) (§32.2) | C |
| **P2** | **HTML: copy buttons are mouse-only** — `visibility:hidden` blocks focus; reveal on `:focus-within` so keyboard users can copy class names (§32.5) | C |
| **P2** | **State the action *and its current limit*** for the three non-prescriptive exceptions (leak-suspect path, alloc sites, header overhead) instead of a hollow "consider" (§33.5) | C/A |
| **P2** | **`analyze_error_hint` drops every `InvalidData`** — it humanizes EOF (truncation) and wrong-file, but the bad-`id_size`, 16-EiB-guard, and unknown-sub-tag errors fall through to the bare `io::Error` string (main.rs:556). Add an `InvalidData` arm: *"parsed as HPROF but a record was malformed/unsupported"* (§43.2, §43.1) | F |
| **P2** | **Add corrupt/truncated-input test fixtures** — nothing pins clean-EOF-at-boundary vs mid-record-error, the id_size gate, or the unknown-sub-tag abort; all `unwrap`s are `#[cfg(test)]` against the *valid* DUMP, so a refactor could silently emit a partial report labeled complete (§43.4). Pairs with the anchor-resolution test (§42.3) | C (test) |
| **P2** | **Graphs: bar columns have no shared denominator** — nine per-table local maxes (render_graphs.rs:204…775) so a full `████████████████` means 22.9 MB in one table and 19.7 MB in another (sample 275 vs 105); eye can't compare across sections. Use one retained-heap denominator for byte-bars + caption the unit where it differs (§44.1) | C |
| **P2** | **Graphs: linear bars erase the power-law tail** — below one cell (~max/16) every value renders blank, so the 55 long strings up to 17,989 B show as empty (sample 221–227); guarantee a min-visible `▏` for any nonzero, or offer a log-scaled variant, so diffuse waste in the tail is visible (§44.2) | C |
| **P2** | **Graphs: sparkline is less honest than the bar table beside it** — `(v*7)/max` floors small-nonzero to the zero glyph (`▁`), so 78-value and 1-value buckets look identical while the sibling bar column distinguishes them (sample 209 vs 213–227); lift nonzero to ≥level 1, add axis, or drop it (§44.3) | C |
| **P2** | **Triage: "% of heap" vs "% of reachable heap" wording drifts across 7 rules** for the identical computation (triage.rs:237/498/1082…); freeze one phrase in a single `const HEAP_BASIS_LABEL` so bullets can't disagree (§45.2) | F |
| **P2** | **Unified "Top collection waste" table** (user-requested) — union under-fill + collision-slack + single/empty offenders into one byte-sorted list with `Cause` + `Class#field` columns; the direct answer to "collections that wasted the most memory by not being filled or by being a single value," and the §24 collection drill-down (§46.3) | C |
| **P2** | **State the location caveat ONCE** (user-requested) — the *"`Class#field` — a hint, not a guarantee"* note is repeated on three captions (sample 2749/2795/2830); consolidate to a single preamble sentence covering **both** `Class#field` (heap) and `Class#method` (stack) as "a notion, not a guaranteed allocation site," and drop the per-table repetition (§47.3) | H |
| P2 | True retained per collection kind (§8.2) | A |
| P2 | Wasted **bytes** (element-size-weighted) on fill-ratio + attribution (§8.4, §8.7) — *now specified concretely as the byte-ranked offender columns in §46.1* | A |
| P2 | Collections: split "cap 0" vs "0 elements, cap>0" bucket (§8.3) — *feeds the §46.2 single/tiny-collection overhead ranking* | A |
| P2 | Depth Distribution: truncate past last meaningful depth + bar (§16.1, §16.2) | H/C |
| P2 | Biggest Collections: de-duplicate identical adjacent rows (§10.5, §10.3) | C |
| P2 | Duplicate Strings: skip binary/garbage values in Longest (§2.4) | A |
| P2 | Immediate Dominators: add `dominated_retained` so hub axis is comparable (§25.4); **this single Add also enables any Reference-Hubs view — extend Immediate Dominators rather than adding a separate section (§34.5, §35.3)** | A |
| P3 | HTML: interactive sortable/filterable class histogram — the MAT feature (§19.7) | H (UI) |
| P3 | **Empty sections use three inconsistent syntaxes and mostly bare "None"** — 12× `_None._` (render_md.rs:1536…2577), 6 descriptive `_No X_`, 4 asterisk `*No X.*` (1525…); normalize to one underscore syntax and give each empty section a "what's absent and why" message (the six descriptive ones are the template) (§59.1, §31.4, §33) | F |
| P3 | Port retained treemap to md-graphs (already in HTML) (§20.1) | H |
| P3 | Back-port graphs box-drawing tree + in-table bar columns to plain md (§29.1, §29.4) | C |
| P3 | Cap-honest "Total (top N shown)" labels + sub-0.1% rounding (§27.6, §27.7) — *root-cause fix is the shared `fmt_pct` "<0.1%" guard, §41.3* | F |
| P3 | ThreadLocal value-class enumeration (§6.2) | A |
| P3 | Referent-class retained column (§12.1) | A |
| P3 | Growth-path triage bullet for pathological chain depth (§20.4) | C |
| P3 | Suppress/omit percentages for sub-megabyte heaps (they degrade to a confusing 0.0%) (§31.6) | C |
| P3 | `format_bytes`: add TB/PB tier — the final GB branch is unguarded (format.rs:187), so a petabyte prints "1048576.00 GB" (§41.2) | C |
| P3 | Concentration table: derive displayed bytes + percent from one rounding so `bp_to_bytes` truncation (render_md.rs:505) can't visibly disagree with the % column (§41.4) | C |
| P3 | Unify `fmt_count` (u64, format.rs:191) and `fmt_delta_count` (signed, diff_reports.rs:323) on one shared digit-grouper — two impls of the same grouping drift (§41.5) | F |
| P3 | HTML: keyboard path + `role=button` for clickable chart slices / treemap tiles (§32.3) | C |
| P3 | HTML: descriptive chart `aria-label`s (name the headline value, not just "Pie chart") + `<figure>`/table association (§32.4) | C |
| P3 | HTML: add a legend to RetainedTreemap + colorblind-safe palette so hue isn't the sole channel (§32.6) | C |
| P3 | HTML: theme-toggle `aria-label` should state the action, not current mode; promote exact-byte `title` tooltips to visible text (§32.7) | C |
| P3 | New triage rules: fill-ratio field attribution, extreme top-100 concentration, diffuse classloader leak (§26.5, §26.8) | C |
| P3 | Advertise "Beyond MAT" strengths (waste/unreachable/N-way-diff) up front (§30.3) | F |
| P3 | Golden-file tests for non-MAT sections (parity harness can't cover them) (§30.4) | C |
| P3 | Suggested-OQL links on suspects once the OQL engine lands (§30.2b) | A |
| P3 | NEW: Pending-Finalization subsection under References — enumerate *what* is stuck, not just that something is (§34.4) | C |
| P3 | NEW: Structural object-graph dedup — exceeds MAT; large Add, research direction (§34.3) | A (large) |
| P3 | Diff verdict: note shrink-% is relative to the larger earlier base + guard zero-base ("n/a" not "0.0%"), referencing the §27.1 canonical base (§37.3) | F |
| P3 | Diff: demote & relabel Removed Classes / Gone Suspects ("absent in final dump") — least-actionable, computed on fragile endpoints-only basis (§37.5) | F |
| P3 | Give `compare reports` an output-path + `.gz` arg via `write_output` — it prints via `print!` (main.rs:397), so the diff is stdout-only unlike single-dump reports (§38.5) | F |
| P3 | Make `--detail` an `Option` so an explicit `--detail default` on re-render gets the same honest "no effect" hint as `--detail minimal` (§38.2) | F |
| P3 | Collection-config: fail loudly (exit 1) when an *explicit* `--collection-config` can't be read/parsed — today it warns and exits 0 (collection_config.rs:99), unlike auto-discovery (§39.3) | F |
| P3 | Collection-config: print "loaded N handlers from PATH; M matched" under `--verbose` — no feedback today, so a typo is indistinguishable from "class absent from heap" (§39.2) | C |
| P3 | HTML: lazy-render the two heavy sections (dominator tree, 500-row histogram) for `--detail max` payloads — whole report is parsed + mounted eagerly on the main thread (index.tsx:35, App.tsx:3592+) (§40.3) | C |
| P3 | HTML: upgrade `fail()` (index.tsx:14) from a bare `textContent` exception string to a styled panel with a "regenerate with hprof-analyzer …" hint, mirroring the CLI's `render_error_hint` (§40.4) | F |
| P3 | Parse: drop/clamp `Vec::with_capacity(num_frames)` and sibling count-loops (pass1.rs:252) — a corrupt `0xFFFFFFFF` count over-reserves gigabytes before any read; the `STRING_IN_UTF8` path is already guarded, this one isn't (§43.3) | C |
| P3 | Graphs: the standalone sparkline duplicates the labeled bar-column table that always follows it (sample 209 vs 211–227; render_graphs.rs:745 vs 758) — drop it here and relocate sparklines to the `compare` output as a cross-dump trend line, filling §37's missing-visual gap (§44.4) | F |
| P3 | Triage: compute the reachable-shallow total **once** and have `overview`/`leaks` share it (or `debug_assert_eq!`) — today equality rests on a comment (build.rs:2053) + two summation loops, a latent split-denominator drift (§45.3) | C |
| P3 | Triage: enforce unit-suffix naming on the ~55 threshold consts + add a provenance comment per const — `OVERCAP_WASTE_PCT` (%) sits beside `OVERCAP_FILL_BP` (bp) in one rule, so a bp/pct transposition fails silently (100× gap); this is the §26.7 provenance ask done once (§45.4) | F |
| **P1** | **One canonical name for the `total_shallow` scalar** — the same 29.8 MB is labeled "Total heap (reachable)" (render_md.rs:359), "Total shallow heap" (692/graphs 123/App.tsx 1161) and "Total heap" (App.tsx:300 KPI); reuse the `HEAP_BASIS_LABEL` vocabulary so scalar and "% Heap" denominator share one name across all four formats (§48.1) | H |
| **P2** | **"% of total heap" on the Unreachable row uses a THIRD, unprinted denominator** — `unreachable / (reachable+unreachable)` (render_md.rs:697–699), not the "Total shallow heap 29.8 MB" printed directly above it; both round to 2.2% only because this sample is barely fragmented. Name the base explicitly or switch to the reachable base that matches the adjacent scalar (§48.2) | H |
| P3 | **Drop the duplicated fragmentation percent** — "; 2.2% of total heap" (render_md.rs:699) and the standalone "Heap fragmentation" row (715) are the *same* `unreachable/(reachable+unreachable)` ratio, printed twice under two labels on adjacent rows; keep the fragmentation row, strip the suffix (§48.3) | H |
| **P2** | **md/graphs/HTML disagree on the unreachable-block labels** — only plain-md shows the "% of total heap" fragment (699) and the "(unreachable / total)" clarifier (715); graphs (131/139) and HTML (1181/1187) omit both. Apply the chosen §48.2/§48.3 wording identically in all three renderers (§48.4, parity) | F |
| **P1** | **Significant-frame percentage silently uses a per-thread base** — `` retains 4.6 MB (20.2%) `` (render_md.rs:1361–1366, HTML App.tsx:1931) divides by the *thread's* retained heap (`SignificantLocal.pct`, model.rs:713–714), not the reachable heap every other "%" uses; on tiny threads this prints alarming "47.6%/76.2%" for byte-sized locals (sample 2635/2647). Label the per-thread base once per thread (or add a heap-relative percent) — render-only, both totals already in the model (§49.1) | H |
| **P2** | **Local-root rows read as accidental duplicates** — identical `(class, shallow, retained)` rows repeat 2–3× (sample thread 1: `Solver` ×2 lines 2518–2519, `HashSet` ×3 2528–2530) because `render_thread_locals` (render_md.rs:1391) drops the distinguishing `obj_index_1based` (model.rs:721); collapse to one row with a `×N` count (mirroring the §28.1 dominator `×N` collapse) or surface the index (§49.2) | H |
| **P2** | **Local-root table is a bounded, non-summing sample but says neither** — header `_Local roots: 124._` but only 19 rows shown (sample 2512 vs 2517–2537), capped by `--detail`; add a "showing top N of M; sizes overlap and do not sum to the thread total" line (both counts already in the model, model.rs:654/659; reuses the §47.3 overlap caveat) (§49.3) | F |
| P3 | **"Local roots: N" line is emitted for some threads, omitted for others** — gated on `local_root_count > 0` (render_md.rs:1344), so thread 2 "Reference Handler" (sample 2571) shows no count line and the absence is ambiguous (zero vs omitted); always emit it, printing `0` when empty (§31.4 emptiness principle, per-thread) (§49.4) | F |
| **P2** | **md shows local roots inline; HTML double-nests them behind a collapsed `<details>`** — `ThreadLocalsTable` (App.tsx:1849–1850) wraps the table in a collapsed disclosure *inside* the already-collapsed per-thread card (App.tsx:1896), so expanding a thread in HTML still hides its locals; md renders them inline unconditionally (render_md.rs:1353). Unify default visibility across formats (§49.5, parity) | F |
| **P2** | **References histogram is uncapped and renders a 1-object / 0-byte long tail** — Weak is 22 rows / 13 singletons incl. a literal `` `java.security.SecureClassLoader` \| 1 \| 0 B `` (sample 3220); `render_class_table` iterates the full vec with no cap (render_md.rs:2628–2649) while the section one below caps at `UNREACHABLE_HISTOGRAM_CAP`. Cap + fold the tail into "… N more classes" (§50.2) | C |
| **P2** | **References: add a per-kind verdict/action caption** — the section states "what they point at" (render_md.rs:2621) but never whether it's a problem; the *correct* action differs by kind (soft=cache-tuning, weak=usually benign, phantom=cleanup-lag), so `975 weak reference instances` reads as alarming when routine. One static caption per kind, mirrored md/graphs/HTML (§50.6) | F |
| **P2** | **Unreachable garbage-root tree object count is a cumulative subtree total shown like a per-node count** — `node.objects` is "objects in subtree" (model.rs:63) but renders as bare `({} objects)` (render_md.rs:2765/2773/2798/2808); the sample nests 70⊇69⊇68 (md:3300–3302) so children never sum to the parent. Label "in subtree" on the root, drop on interior nodes (§51.1) | H |
| **P2** | **Unreachable histogram `Retained` column overlaps and isn't additive, with no caveat** — retained sets nest (String 25.4 KB shallow / 86.5 KB retained, md:3262), so summing the column blows past the header's `673.0 KB`; add the §27/§48-style "only Shallow sums; Retained overlaps" caption, mirrored md/graphs/HTML (§51.3) | F |
| **P2** | **Unreachable histogram graphs bar is on Objects (count), not Shallow (bytes)** — `bar(r.objects, obj_max, …)` (render_md.rs:2738) flattens the fact that `int[]` (1,642 obj / 569.6 KB) is ~9:1 the collectable heap vs `byte[]` (1,084 obj / 61.1 KB) down to ~1.5:1; bar `r.shallow` (the additive axis) instead — same as §50.3/§44 (§51.4) | H |
| **P2** | **Biggest Collections "Value Type" and "Value Types (top)" are the same string on every row** — `dominant_value_type` (render_md.rs:2497) duplicates the breakdown's lead entry (`java.lang.Class` / `java.lang.Class ×485`, md:3079; all 24 map rows), wasting ~40 cols; drop the standalone column, keep the breakdown (§52.2) | F |
| **P2** | **Biggest Collections lists byte-identical instance rows with no coalescing** — 6 consecutive identical `ArrayList\|3\|String\|…\|144 B` rows (md:3084–3089) and an entire 11-row all-identical `ArrayDeque\|Inflater\|112 B` deque section (md:3131–3141) drown the ranking; coalesce on (kind,class,elements,value,owner) with a `×N instances` multiplier + summed retained (§52.3, §28.1 collapse) | C |
| **P2** | **Dominator-Depth degenerate 41,355-hop tail floods the table** — ~28 identical `31 \| <0.1% \| 70.1%` rows (md:3438–3457) from a single linked-list/cycle chain slip past the per-row `>=0.1%` cutoff; detect constant-count runs and collapse with a growth-path note (§53.3, §27.4, §20.4) | C |
| **P2** | **`Shape` triage p90 depth = 11273 is corrupted by the degenerate chain** — cum-90% depth (triage.rs:423–431) is dragged to 11273 by the single 41,355-hop chain (sample md:60) though the heap is mostly shallow (median ~10, md:3419); de-artifact the tail (shares §53.3 fix) or report median vs p90 and reword the verdict (§54.2, §27.4) | C |
| **P2** | **Two "tiny-object swarm" triage rules use opposite count-vs-% logic** — `ObjectSwarm` needs count AND % (10M floor, triage.rs:769/773), `BoxedPrimitiveBloat` fires on count OR % (5M floor, triage.rs:825); normalize to a byte-share gate with count as a noise floor, align the floors (§54.3, §26.2, §50.5) | C |
| P2 | **String Length Distribution draws the same histogram twice in graphs** — `if graphs` emits both `sparkline(&counts)` (render_md.rs:135) and a bar-column table over the same `counts` (render_md.rs:136–149); the sparkline (sample graphs 224) is a lower-res duplicate of the table's bars (226–241). Drop the sparkline line (§55.1, §44.4) | F |
| P2 | **Top-Dominator Size Distribution repeats sparkline + min/max in graphs** — sparkline (render_graphs.rs:791–794; sample 5909) duplicates the bucket-table bars (5911–5932) and its `(0 B – 22.9 MB)` label re-prints the "Smallest / largest retained" bullet three lines up (5905); delete the sparkline block (§55.2, §44.4) | F |
| P3 | **References graphs bar annotates Objects (count), not Shallow/Retained (bytes)** — `bar(r.objects, obj_max, …)` (render_md.rs:2644) steers the eye by population, and the §44.2 min-visible floor makes 1- and 2-object rows tie at `▏` (sample graphs 6701–6702); bar the byte axis instead (§50.3) | H |
| P3 | **References: "Only-weakly retained" sub-table silently vanishes under Weak/Phantom** — gated on non-empty (render_md.rs:2660; HTML App.tsx:2926), it shows under Soft but is absent under Weak/Phantom in the sample, so "no escape" is an ambiguous gap not a stated fact; always emit the heading with an explicit "None" note (§31.4 principle) (§50.4) | H |
| P3 | **Garbage-root trees print `(1 objects)`** — the `" objects"` literal (render_md.rs:2765/2773/2798/2808) never pluralizes; 5 of the sample's garbage roots are single-object `int[] … (1 objects)` (md:3296–3312), and every 1-object Leak Suspect (md:2271+) too. Add a shared `plural()` helper, mirror in web/src/format.ts (§51.2) | F |
| P3 | **Retention Concentration puts a count in the "Retained Share" percent column** — `Objects each >=1% \| 4 \| (empty)` (render_md.rs:561; sample md:3421) reads "4" as a share and leaves the byte cell blank; move it to a caption or its own labeled row (§53.4, §45) | F |

*Already satisfied (do not re-do):* structured JSON triage (`Report.triage`, §19.9);
schema versioning (`schema_version`=6, §19.8) — *present and enforced on read-back (main.rs:709),
but the exact-match gate should be reconciled with the `#[serde(default)]` machinery, see new P1 §36.1*;
a machine-readable JSON Schema exists via `dev emit-schema` (§36.0, needs surfacing not building);
the JSON is data-complete — all 18 top-level keys back every human section incl. triage verdicts (§36.3);
string-intern savings + holder ranking
(`DupStrings`, §20.2); boxed-primitive waste (`boxed_numbers`, §20.3) — all confirmed present
in pass 4. These need surfacing/prose, not building. *Also confirmed already-present in pass 15
(§34.0), do not re-add as "new":* retained-size deltas (`compare reports` / `SeriesClassRow`),
classloader-leak detection (`ClassloaderLeak`+`DuplicateClass`), finalizer-queue *signal*
(`FinalizerQueueBacklog`), duplicate-string *values* (`DupStrings`), per-package *retention*
rollup (`PackageNode`). The genuinely-new items are §34.1–34.5.

---

## 22. Data-Availability Matrix

The single most important collection: for **every** suggestion above, does the data
already exist in the model (`src/report/model.rs`), or must it be computed/added?
This is what determines whether a change is a pure rendering change (cheap) or needs a
new analysis pass (expensive). Three columns:

- **Have** — the field(s) already exist; the change is purely in the renderer.
- **Compute-cheap** — derivable from fields already in the model at render time
  (arithmetic on existing numbers, filtering, grouping, sorting).
- **Add** — requires a new field on a struct AND a new pass over the heap graph to
  populate it. These are the only expensive changes.

### 22.1 Ready to render — data already in the model (Have)

These are the highest-ROI changes: the data is already serialized; only the renderer
needs to change. No new heap pass, no schema-breaking field.

| § | Suggestion | Model field(s) that back it |
|---|---|---|
| 1.4 | Wasted-slots triage bullet | `FieldAttributionRow.total_wasted_slots`, `FillRatioBucket.wasted` — both exist; sum for the headline number |
| 1.5 | Severity tag / ordering on triage | `TriageSignal.severity: TriageSeverity` — already carried, currently unused in ordering (see model comment "carried for future HTML styling") |
| 2.1 | Bar column for GC Roots by Type in plain MD | `GcRootTypeRow.count` — render `bar(count, max, 16)` |
| 2.2 | Retained column on class histogram | `HistRow.retained` — already populated; System-Overview render only shows `shallow` |
| 2.5 | "remaining N classes: M bytes" summary row | `SystemOverview` holds the full histogram vec; sum the tail beyond top-50 |
| 4.1 | Top-Dominator Size Distribution in plain MD | `TopSizeDistribution.buckets` — graphs-only today, data is format-agnostic |
| 4.2 | Drop heap-address `#` column from Biggest Objects | `ObjRow` — just stop rendering the id column |
| 7.2 | Owner hint instead of `#` in Top Arrays | `TopArrayRow.owner: Option<String>` — already resolved under `--collections` |
| 8.2 | Retained column on Collections by Kind | `CollectionKindStat` has `total_shallow`; retained not present → see Add. BUT max_elements/count/total_elements are Have for a partial fix |
| 8.8 | Kind column in Biggest-Single attribution | `FieldAttributionBiggestRow.container_kind` — field exists; verify it's rendered |
| 9.1 | "Elements" always 0 — remove or fix | `FieldBySizeRow.elements` exists; it is 0 for non-container fields by design → drop column when category≠collection |
| 9.3 | Sharing-ratio (pointees/holders) column | `FieldBySizeRow.pointees` + `holder_instances` — both exist; ratio is arithmetic |
| 10.1 | Remove Combined+ByKind duplication | `BiggestCollections.combined` + `.by_kind` — pick one; add `kind` column to combined (`BiggestCollectionRow.kind` exists) |
| 10.2 | Note "value type is direct element, not logical value" | `BiggestCollectionRow.dominant_value_type` — annotate, no data change |
| 13.1 | Explain shallow==retained in unreachable forest | `UnreachableClassRow.shallow`/`retained` — prose only |
| 15.2 | Absolute bytes in Retention Concentration | `RetentionSummary.total_retained` + `topN_bp` → bytes = `bp * total / 10_000` |
| 15.3 | Enumerate the "objects each ≥1%" | `RetentionSummary.num_objects_ge_1pct` gives the count; the objects themselves are the top-level dominators already in `TopConsumers` — cross-reference |
| 16.1 | Truncate depth table past last ≥0.1% row | `DominatorAnalysis` / `DepthBucket` vec — filter at render |
| 16.2 | Bar column on depth distribution | `DepthBucket.objects` — render bar |
| 19.5 | Consistent bar columns everywhere | all count/size tables have the raw numbers already |

### 22.2 Derivable at render time — no new heap pass (Compute-cheap)

Requires arithmetic/grouping over fields already present, but no new field and no new
scan of the object graph.

| § | Suggestion | How it's derived from existing fields |
|---|---|---|
| 1.2 | Sort triage by actionability | order `Report.triage` by `severity` then a per-rule byte estimate the rules already know |
| 3.1 / 13.3 | Collapse repeated dominator chains | walk the existing `DomTreeNode.children` / `UnreachableGarbageRoot.children`; when consecutive nodes share `pretty_class` + retained band, fold into "↓ N more" — pure render-time tree rewrite |
| 5.1 | Group duplicate Big-Drops rows | group `BigDropRow` by `(pretty_class, retained band)`, emit count |
| 8.5 | Sort wasted contributors by bytes not slots | `total_wasted_slots × element_size`; element size is inferable from `container_kind`/array class |
| 8.9 | Aggregate "total wasted slots ≈ M bytes" | sum `FillRatioBucket.wasted` and `FieldAttributionRow.total_wasted_slots` |
| 10.5 / 10.3 | De-dup identical adjacent collection rows | group `BiggestCollectionRow` by `(container_class, owner, elements, retained)` |
| 6.1 | Cap per-thread local roots at top-10 | `ThreadInfo` local-root vec — slice + "… N more" |
| 8.6 | Filter noisy Constant Primitive Arrays | filter `ConstantArrayRow` by `length` / `objects` thresholds (already prototyped) |
| 20.4 | Pathological chain-depth triage bullet | max depth is the largest `DepthBucket.depth`; emit a triage signal when it exceeds a threshold |

### 22.3 Needs a new field + heap pass (Add — expensive)

These cannot be rendered today because the underlying number is not computed. Each needs
a model field AND a populating pass. Listed with the reason it's worth the cost.

| § | Suggestion | New data needed | Reason it's worth it |
|---|---|---|---|
| 8.2 | True retained per collection kind | `CollectionKindStat.total_retained` (retained sum, not shallow) | distinguishes one 500 MB collection from 500 small ones — core to "where does heap go" |
| 8.3 | Split "0 elements, cap 0" vs "0 elements, cap>0" | capacity-aware empty sub-bucketing in `CollectionFillRatio` | separates defensible zero-arg collections from pure pre-allocation waste |
| 8.4 / 8.7 | Wasted **bytes** (not slots) | element-size-weighted wasted total on fill-ratio + attribution | slots aren't comparable across `int[]` vs `Object[]`; bytes are the actionable unit |
| 2.4 | Skip binary/garbage in "Longest Values" | printability check when collecting longest strings | garbage rows are noise, actively harmful |
| 6.2 | ThreadLocal value classes | `ThreadLocalObj` exists (struct present) but per-thread enumeration of the referent class must be populated | a thread pinning a large object via ThreadLocal is a top leak pattern |
| 12.1 | Retained column on referent classes | retained on `RefStatClassRow` (only `objects`/`shallow` today) | a softly-held object retaining 500 KB via caches is invisible today |
| 20.6 | Fill ratio by size tier | tiered fill-ratio buckets (small/medium/large capacity) | tells you whether waste is a few huge under-filled structures or many tiny ones |
| 13.2 | Class-object shallow size = 0 bug | fix shallow-size calc for class-dump records | current 0 B is simply wrong |

*Corrections applied this pass (moved OUT of Add — the data already exists):*
- **20.2 (string intern savings) → Have.** `DupStrings.approx_wasted_bytes` already computes
  `Σ (count-1)×first_seen_len`, and `DupStrings.top_string_holders: Vec<StringHolder>` already
  ranks owning classes by String-ref count. Also `DupStrings.char_array_waste:
  Option<CharArrayWaste>` tracks backing `char[]/byte[]` waste under `--find-duplicates`. The
  suggestion is a render/prose change, not a new pass.
- **20.3 (boxed-primitive waste) → Have.** `SystemOverview.boxed_numbers: Vec<BoxedNumberRow>`
  (with `instances`, `total_shallow`, `pct_of_heap_bp`, `avg_shallow`) and
  `boxed_number_holders: Vec<BoxedNumberHolder>` already exist and are wired into the HTML Nav.
  The waste narrative just needs surfacing (and a triage bullet), not computing.
- **2.3 (empty-string wasted bytes) → mostly Have.** `DupStrings.char_array_waste` already
  accounts for backing-array overhead separately from `approx_wasted_bytes`; the "0 for empty
  string" confusion is a rendering choice, not missing data.

### 22.4 Format-plumbing only (no model change)

| § | Suggestion | Nature |
|---|---|---|
| 14.1 | Hide/explain empty Allocation Sites | `AllocSites.traces_present: bool` already distinguishes the empty case — render a note (prototyped) |
| 18.x | Glossary terms + placement | static text |
| 19.1 / 19.4 | Bar charts default in Markdown | renderer default flip |
| 19.9 | Structured `triage` array in JSON | `Report.triage: Vec<TriageSignal>` is already structured — JSON emit just needs to include it |
| 19.10 | Verify depth histogram not top-50-truncated in JSON | JSON should serialize the full `DepthBucket` vec regardless of the MD truncation |

### 22.6 Classification of the remaining §1–21 suggestions

Pass-4 completeness sweep: every suggestion in §1–21 now has an effort class. These are the
ones not already tabled in §22.1–22.4. `H`=Have (render-only), `C`=Compute-cheap, `A`=Add,
`F`=format-plumbing/prose.

| § | Class | Basis |
|---|---|---|
| 1.1 | F | Remove Summary/Triage duplication — reorder/drop prose; `RetentionSummary` already backs both |
| 1.3 | C | Name top-1 class in the empty-collection triage bullet — from `CollectionKindSummary` + attribution, at render |
| 2.6 | F | Elevate header-overhead — `SystemOverview.header_overhead` exists; it's a placement/triage-prose change |
| 2.7 | H/C | Duplicate-classes summary row — `duplicate_classes: Vec<DuplicateClass>` present; sum a header line at render |
| 3.2 | C | ASCII tree for merged paths — `MergedPathNode.children` exists; render-time tree draw (HTML already SVGs it) |
| 3.3 | H | Cross-link SoftRef/ThreadLocal into Suspects — data in `references`/`threads`; prose links only |
| 3.4 | H | "dominant class" column on Components — `ComponentClass` already carries per-class retained |
| 4.3 | C | Collapse package tree to 2–3 segments — walk `PackageNode.children`, skip single-child chains at render |
| 4.4 | F | Interpretation sentence for Immediate Dominators — prose |
| 5.2 | C | "Owner hint" column on Big Drops — join `BigDropRow` to container attribution at render (owner already resolved) |
| 6.3 | F | Human-readable thread state — relabel `ThreadInfo` state flags in the renderer |
| 7.1 | F | Merge obj/prim array sections w/ Kind column — pure layout over `TopArrays` |
| 7.3 | A | Per-class wasted capacity for object arrays — needs null-slot count per array class (new aggregate) |
| 7.4 | H | Replace "largest instance" w/ top-1 owner — `TopArrayRow.owner` already resolved under `--collections` |
| 8.1 | H | "Total Shallow" header rename — verify fixture; render-only |
| 8.7 | A | Wasted-slots column on "Most Overall" attribution — `FieldAttributionRow.total_wasted_slots` exists (Have!) → actually H; sort-by-bytes needs element size (C) |
| 8.10 | F | Intra-section mini-TOC for Collections — layout |
| 9.2 | A | Shallow column for pointee in Fields-by-Retained — pointee shallow not carried on `FieldBySizeRow` |
| 9.4 | F | Move truncation note above table — render order |
| 10.3 | C | Flag duplicate/shared collections — detect equal `(container_class, retained, elements)` rows at render |
| 10.4 | F | Cross-link Biggest Collections → fill ratio — prose anchor |
| 11.1 | A | Map *value* types (not node type) in aggregate — needs value-type tally per class (partially in `value_type_breakdown` for top instances only) |
| 11.2 | F | Rename section "Collection Element Types" — prose |
| 12.2 | F | Explain "only-weakly retained" — prose (glossary) |
| 12.3 | H | Cross-link referents to histogram — both `referent_histogram` and `histogram` present; render links |
| 12.4 | H/A | Native-resource note for phantom referents — referent class in `ReferenceStats`; the "known resource holder" flag is a small static lookup (C) |
| 13.3 | C | Chain-collapse garbage-root trees — same render-time fold as 3.1/13.3 (`UnreachableGarbageRoot.children`) |
| 13.4 | F | "unreachable = eligible for GC" context — prose; `unreachable_count`/`_shallow` present |
| 15.1 | H | Sparkline for retention curve in Markdown — `RetentionSummary` present; HTML already charts it |
| 16.3 | F | Make the p50/p90-depth summary the headline — prose reordering over `DepthBucket` |
| 17.1 | H/A | Enrich Leak Indicators — classloader count/finalizer/threadlocal counts: some in `LeakIndicators`/`SystemOverview` (H); finalizer-queue depth is new (A) |
| 17.2 | F | Threshold context for anon-class count — prose over `LeakIndicators.anonymous_class_count` |
| 18.1 | F | Glossary placement/tooltips — prose/HTML |
| 18.2 | F | Missing glossary terms — prose (partly added already) |
| 19.2 | F | `--compact` flag to truncate subtrees in plain MD — renderer option |
| 19.3 | F | TOC-links-don't-resolve caveat — inherent; ordering guidance |
| 19.4 | F | Graphs-as-default — renderer default flip (dup of 19.1) |
| 19.6 | C | Reduce md-graphs size via de-dup — same as 10.1 (render restructure) |
| 19.7 | H | Sortable/filterable HTML histogram — `HistRow` has all columns; UI-only |
| 19.7a | A | Move depth-median stat to model — small scalar to stop MD/JS drift |
| 19.7b | H | Treemap under-used in MD — `PackageNode` present (= 20.1) |
| 19.7c | F | Chart→table cross-links in HTML — UI wiring |
| 19.8 | F | Schema changelog — docs; `schema_version` already present |
| 20.5 | A | Prim-array use breakdown (codec/crypto/string-backing) — needs classification of `byte[]/int[]` usage beyond constant-array detection |

Net effect of pass 4: the previously "44 uncovered" suggestions resolve to mostly **F**
(prose/layout, ~20) and **H/C** (render-only, ~15); only ~7 are genuine **A** (7.3, 9.2, 11.1,
17.1-finalizer, 19.7a, 20.5, and the 12.4 resource-flag lookup). Combined with the 20.2/20.3
corrections above, the "needs a new heap pass" set shrinks further — reinforcing the takeaway.

### 22.5 Summary of effort distribution

Across the full §1–21 suggestion set (classified in §22.1–22.4 and §22.6):

- **~35 suggestions** are render-only or prose/layout (Have + format-plumbing) — the data is
  already in the model; only the renderer or wording changes. These are cheap and should go
  first.
- **~10 suggestions** are compute-cheap (grouping/arithmetic/tree-folding at render time) —
  no schema change, no new heap pass.
- **~9 suggestions** genuinely need a new field + heap pass (Add): 8.2 (retained per kind),
  8.3 (capacity-aware empty buckets), 8.4/8.7-sort (wasted **bytes**), 6.2 (ThreadLocal value
  classes), 12.1 (referent retained), 20.6 (fill-ratio tiers), 13.2 (class-object shallow
  bug), 7.3 (object-array null capacity), 9.2 (pointee shallow), 11.1 (map value types),
  19.7a (depth-median scalar), 20.5 (prim-array use breakdown).
- Note **20.1 (treemap)**, **20.2 (string-intern savings)**, and **20.3 (boxed-primitive
  waste)** are NOT Add — this pass confirmed the data already exists (`PackageNode`,
  `DupStrings.approx_wasted_bytes`/`top_string_holders`, `boxed_numbers`/`boxed_number_holders`).
  They are surfacing/render work.


The one-line takeaway: **most of the report's problems are rendering problems, not
missing-data problems.** The model already computes retained heap, wasted slots, fill
ratios, owner attribution, severity, size distributions, a package→class tree, boxed-number
waste, header overhead, and duplicate-string waste — the reports just don't surface them
consistently ACROSS formats, nor sum them into one headline waste number. The only genuinely
new analyses worth building are: retained per collection kind, and wasted **bytes**
(element-size-weighted).

---

## 23. Cross-Format Capability Matrix

The review's biggest structural finding: the four formats are NOT at feature parity, and
the gaps run in both directions. HTML (a React app over the same JSON) has rich charts the
Markdown formats lack; the Markdown formats have plain tables and prose the HTML renders
differently or not at all. Since all four are dumb formatters over one `Report`, every gap
here is a rendering gap, not a data gap. Legend: ✓ present · ✗ absent · ~ partial.

| Capability | plain md | md-graphs | HTML | JSON | Backing data |
|---|---|---|---|---|---|
| OOM triage prose | ✓ | ✓ | ✓ | ✓ (structured) | `Report.triage: Vec<TriageSignal>` |
| Triage severity styling | ✗ | ✗ | ~ (carried, styling TBD) | ✓ | `TriageSeverity` |
| Class histogram (table) | ✓ | ✓ | ✓ | ✓ | `HistRow` |
| Class histogram — retained column | ✗ | ✗ | ✓ (chart) | ✓ | `HistRow.retained` (Have) |
| Class histogram — sortable/filter | n/a | n/a | ✗ (#1 HTML gap) | n/a | `HistRow` (Have) |
| Heap composition | ✓ | ✓ (bars) | ✓ (stacked bar) | ✓ | `HeapComposition` |
| GC roots by type | ✓ | ✓ (bars) | ✓ (chart) | ✓ | `GcRootTypeRow` |
| GC roots bar in plain md | ✗ | ✓ | n/a | n/a | `GcRootTypeRow.count` (Have) |
| Retention concentration | ✓ (table) | ✓ (table) | ✓ (chart + stacked) | ✓ | `RetentionSummary` |
| Concentration absolute bytes | ~ (added) | ~ (added) | ✗ | ✓ | `total_retained`+`bp` (Have) |
| Dominator-depth distribution | ✓ (trimmed) | ✓ | ✓ (chart, tail-fold) | ✓ (should be full) | `DepthBucket` |
| Depth bar chart | ✗ | ~ (added) | ✓ | n/a | `DepthBucket.objects` (Have) |
| Dominator subtree | ✓ (verbose) | ✓ (verbose) | ✓ (SVG) | ✓ | `DomTreeNode.children` |
| Subtree chain-collapse | ✗ | ✗ | ~ (SVG compact) | n/a | render-time (Compute-cheap) |
| Retained treemap | ✗ | ✗ | ✓ | ✓ | `PackageNode` (Have) |
| Top-dominator size dist. | ✗ | ✓ | ✓ | ✓ | `TopSizeDistribution` |
| Leak-share chart | ✗ | ✗ | ✓ | n/a | `Suspect.retained` (Have) |
| Loader rollup chart | ✗ | ✗ | ✓ | ✓ | `LoaderRollup` |
| Capped tables + expand | n/a (static trunc) | n/a | ✓ (Show N more) | n/a | all vecs (Have) |
| Sticky sidebar TOC | ✗ (anchor list) | ✗ | ✓ | n/a | — |
| Theme toggle | n/a | n/a | ✓ | n/a | — |
| Boxed-number waste | ✓ | ✓ | ✓ | ✓ | `boxed_numbers` + `boxed_number_holders` (Have) |
| Duplicate-string wasted bytes | ✓ | ✓ | ✓ | ✓ | `DupStrings.approx_wasted_bytes`/`char_array_waste` |
| String intern-candidate holders | ✓ | ✓ | ✓ | ✓ | `DupStrings.top_string_holders` (Have) |
| Header-overhead accounting | ✓ | ✓ | ✓ | ✓ | `SystemOverview.header_overhead` |
| Wasted **bytes** (element-size-weighted) | ✗ | ✗ | ✗ | ✗ | needs Add |
| Retained per collection kind | ✗ | ✗ | ✗ | ✗ | needs Add |

### 23.1 What the matrix tells us

1. **Markdown is the weakest format for visualization** yet is the default. Every "add a
   chart/bar" suggestion (§2.1, §4.1, §15.1, §16.2, §19.1, §19.5) is really "port the HTML
   chart's data shape to an ASCII equivalent" — the data and even the algorithm (e.g.
   depth-histogram tail-folding) already exist in `charts.tsx`.
2. **HTML's one real gap is interactivity on the histogram** (sort/filter) — the MAT-killer
   feature. Everything else HTML does well.
3. **The report already has FOUR waste accountings in all formats** — empty/fill-ratio
   collections, wasted *slots*, duplicate-string bytes, boxed-number shallow, and
   header-overhead. The gap is not "no waste analysis"; it is that (a) they are scattered
   across sections with no single headline number (§5 of the plan / pass-5 work), and (b)
   the one dimension truly missing is **wasted bytes** (element-size-weighted, so `int[]` vs
   `Object[]` waste is comparable) and **retained-per-collection-kind**. Those two are the
   only waste items justifying a new heap pass.
4. **JSON is in good shape**: versioned (`schema_version`=6), structured triage, additive
   fields. The one fix is guaranteeing the depth histogram is emitted uncapped.

The action ordering that falls out: (a) bring Markdown up to HTML's chart parity using
existing data — cheap, big readability win; (b) add histogram sort/filter to HTML; (c)
consolidate the five existing waste signals into one headline number (pass 5); (d) add the
two genuinely-missing waste dimensions (wasted bytes, retained-per-kind) once, lighting up
all four formats.


---

## 24. Unified Waste Accounting (pass 5)

The user's central question — *"where is heap memory wasted?"* — is currently answered in
fragments scattered across §2, §8, §13, and the boxed/dup sections. Each fragment uses a
different unit (slots vs bytes vs instances vs %), so a reader cannot add them up or rank
them. This section consolidates them into ONE model of waste, with ONE headline number, and
specifies exactly which byte fields already exist to compute it.

### 24.1 The waste signals that already exist (all byte-denominated unless noted)

| Waste source | Model field (exact) | Unit | Section today |
|---|---|---|---|
| Duplicate String values | `DupStrings.approx_wasted_bytes` | bytes | §2 (Duplicate Strings) |
| String backing-array slack | `DupStrings.char_array_waste.total_wasted_bytes` (`CharArrayWaste`) | bytes | §2 |
| Duplicate primitive arrays | `DupPrimArrays.total_wasted_bytes` | bytes | Duplicate Prim Arrays |
| Object-header overhead | `HeaderOverheadRow.total_header_bytes` (Σ over rows) | bytes | §2 (Header Overhead) |
| Boxed-number shallow | `BoxedNumberRow.total_shallow` (Σ) | bytes | Boxed Numbers |
| Collection backing-array slack | `FillRatioBucket.wasted` (Σ) | **slots** | §8 (Fill Ratio) |
| Per-field collection waste | `FieldAttributionRow.total_wasted_slots` (Σ) | **slots** | §8 (Container Attribution) |
| Object-array null slots | `ArrayFillRatio` buckets `.wasted` (Σ) | **slots** | §8 (Array Fill Ratio) |
| Unreachable-but-not-collected | `SystemOverview.unreachable_shallow` | bytes | §13 (Unreachable) |

Seven of these nine are already in bytes and can be summed TODAY with zero new computation.
The two exceptions are the collection/array slots (they need element-size weighting — the
one genuine Add from §22.3, item 8.4). Note the potential double-count: header overhead is a
component of every object's shallow size, so boxed-number `total_shallow` already includes
its own header bytes — the headline must pick ONE decomposition, not naively add both.

### 24.2 Proposed single headline number

Add one triage bullet (rule in `triage.rs`, so all four formats inherit it) computed as:

```
reclaimable_bytes ≈ dup_string_bytes
                  + char_array_slack_bytes
                  + dup_prim_array_bytes
                  + (collection_wasted_slots × avg_slot_size)   [needs 8.4]
                  + (object_array_null_slots × ref_size)        [needs 8.4]
```

Rendered as: *"**Reclaimable waste:** ~N MB (P% of heap) is recoverable without changing
behaviour — M MB duplicate strings, K MB collection over-allocation, J MB duplicate arrays.
See [Waste Summary]."* Header overhead and boxed-number cost are reported SEPARATELY as
*structural* cost (they require code/design changes, not just tuning), so the headline stays
honestly "recoverable without behaviour change".

*Data availability:* the three byte-based terms are **Have**; the two slot→byte terms are the
**8.4 Add** (element-size weighting). So a first version of the headline can ship using only
the byte terms and note "collection slack shown separately in slots until byte-weighting
lands" — no new heap pass required for v1.

### 24.3 Why one number matters

- **Triage prioritisation:** a reader currently cannot tell whether the 92%-empty-collection
  finding (§1.3) represents 2 MB or 200 MB. The headline byte number makes waste rankable
  against leaks (which are already in bytes).
- **Removes cross-section duplication:** §1.4, §8.4, §8.9, §20.2, §20.3 all gesture at "waste"
  in different units. A single Waste Summary section that each of those links to (rather than
  re-deriving) is the de-duplication the user asked for.
- **Distinguishes recoverable vs structural:** duplicate strings/arrays and collection slack
  are *tuning* wins (interning, right-sizing, `trimToSize`); header overhead and boxing are
  *design* costs. Splitting them stops a reader from chasing a 10 MB header figure they can't
  actually reclaim.

### 24.4 Placement (avoids duplication)

Introduce a top-level **Waste Summary** section directly after Triage. Every existing
waste-bearing section (§2 dup strings, §8 collections, Boxed Numbers, Header Overhead) keeps
its detail table but drops its own ad-hoc "wasted" prose and instead links up to the Waste
Summary. The summary holds the headline + the 9-row table above; the detail sections hold the
per-class/per-field drill-down. One number at the top, one place per detail — no repetition.

---

## 25. Unified Heap-Origin Attribution (pass 6)

The user's other central question — *"where does heap usage come from?"* — is, like waste,
answered by several sections that never reference each other. There are FIVE distinct
"retained heap grouped by X" views in the model, each keyed differently and capped
independently. A reader who wants to trace "this memory belongs to component C, package P,
class K, held via field F, dominated by hub H" must manually stitch five tables together.

### 25.1 The five attribution axes (all already in the model, all retained-byte-denominated)

| Axis | Model field | Key | Section |
|---|---|---|---|
| By class loader / component | `TopComponents.components` → `Component.retained` + `top_classes` | loader | Top Components |
| By package | `TopConsumers` `PackageNode.retained_heap` (+ `children`) | package path | Top Consumers (Biggest Packages) |
| By class | `ClassRow.retained` and `HistRow.retained` | class | Top Consumers / System Overview histogram |
| By field (`Class#field`) | `FieldBySizeRow.total_retained`, `FieldAttributionRow.total_retained` | holder field | Fields by Retained / Container Attribution |
| By dominator hub | `ImmediateDominatorRow.dominated_shallow`, `BigDropRow` | dominator class | Dominator Analysis |

All five are already retained-byte-denominated (except `ImmediateDominatorRow`, which is
shallow — see 25.4). No new heap pass is needed to make them cohere; the gap is purely that
they are presented as five unrelated tables.

### 25.2 The coherence problems

**25.1a The five axes don't share a common total or reconcile.**
Package retained, component retained, and class retained are three projections of the SAME
dominator tree, so they should each sum to (approximately) total retained heap. Nothing in
the report states this, and nothing lets the reader check that `Σ components ≈ Σ packages ≈
total`. Add a one-line reconciliation note under each: "these N components account for P% of
retained heap; the rest is spread across M smaller loaders." *Data: Have — every row has
retained + there's `overview.total_shallow`/`RetentionSummary.total_retained`.*

**25.1b Class appears in three sections with three different retained numbers.**
`HistRow.retained` (System Overview histogram), `ClassRow.retained` (Biggest Classes), and
`ComponentClass.retained` (within a component) can all show `scala.collection...HashSet` with
DIFFERENT retained values (histogram = all instances; component = only instances under that
loader; Biggest Classes = top-N). A reader sees the same class three times and cannot tell
why the numbers differ. Add a footnote clarifying each table's scope. *Data: Have — it's a
prose/scoping clarification, not a data change.*

**25.1c The field axis is the most actionable but is buried and disconnected.**
`Class#field` attribution (`FieldBySizeRow`) is the closest thing to "which line of code owns
this memory" — it's what a developer actually fixes. Yet it sits far below the package/class
views and never links up to them. When `scala...HashSet` is the top class (by-class axis),
the report should link straight to the `HashSet#table` field row (by-field axis) that
explains it. *Data: Have — both rows exist; add cross-links.*

**25.1d No single drill path.**
The ideal narrative is one drill-down: component → package → class → field → dominator hub,
each level a projection of the next. The data supports it (all five axes exist); the report
just needs to ORDER these sections as a coherent descent and cross-link them, rather than
scattering them across §2, §4, §5, §9. *Data: Have — ordering + links only.*

### 25.3 Proposed "Heap Origin" spine

Mirror the §24 Waste Summary: a short **Heap Origin** lead-in after Triage that names, in one
sentence each, the top contributor on each axis and links to its detail table:

> *Retained heap is concentrated in the `<app>` class loader (P%), the `scala.collection`
> package (Q%), the `HashSet` class (R%), held via `HashSet#table` (S%), dominated by one
> `$colon$colon` chain (T%). See [Top Components], [Biggest Packages], [Fields by Retained],
> [Dominator Analysis].*

Every number here already exists (top row of each axis + its bp/pct). This turns five
disconnected tables into one coherent answer to "where does my heap come from", and each
detail section keeps its drill-down without repeating the headline. *Data: Have.*

### 25.4 One real inconsistency to fix

`ImmediateDominatorRow` reports `dominator_shallow`/`dominated_shallow` (SHALLOW), while every
other origin axis is retained. This makes the dominator-hub view non-comparable with the other
four and undersells retention hubs (a hub's whole point is large *retained*, not shallow). Add
a `dominated_retained` field so the hub axis speaks the same unit as the rest. *Data: Add —
small, but needed for the spine in 25.3 to be apples-to-apples.*

## 26. Triage Rule Audit (pass 7)

The "OOM Triage" section is the single most important part of the whole report — it is the
first thing a reader sees and the only part that says *"here is what is wrong."* It is driven
by `src/report/triage.rs`: **39 rules**, each a `Rule::eval(&Report) -> Option<TriageSignal>`,
run once in registry order (`rules()`), and both the markdown and HTML renderers are dumb
formatters over the resulting `Vec<TriageSignal>`. This is a strong design (rule logic in one
place, thresholds all co-located at the top of the file). But the *policy* — the thresholds,
severities, and coverage — has never been audited against the reports it produces. This pass
does that, rule by rule.

### 26.1 The complete rule table

Registry order = render order. Severity: **C**ritical / **W**arning / **I**nfo. "Always" =
fires on every non-empty report (no floor). "Gated" = requires `--collections` or
`--find-duplicates`.

| # | Rule id | Sev | Fires when | Key threshold(s) | Gated? |
|---|---|---|---|---|---|
| 1 | `headline-retainer` | C/W/I | always | none (fallback names no offender) | no |
| 2 | `concentration` | C/I | always | top suspect ≥ `CONCENTRATION_PCT` 50% → Critical, else Info | no |
| 3 | `gc-root-type` | W | top GC-root type ≥ 50% retained | `GC_ROOT_DOMINANT_PCT` 50% | no |
| 4 | `shape` | I | always (needs depth hist) | p90 depth ≤ 3 → shallow, else deep | no |
| 5 | `one-leak-or-many` | I | always (needs concentration) | none | no |
| 6 | `object-swarm` | W | tiny class, huge count | ≥ 10M inst, ≤ 64 B/inst, ≥ 10% heap | no |
| 7 | `boxed-primitive-bloat` | I | many wrappers | ≥ 5M inst **OR** ≥ 5% heap | no |
| 8 | `classloader-leak` | W | class loaded by >1 loader | max by retained (**no floor**) | no |
| 9 | `classloader-explosion` | W | many live loaders | ≥ 1000 loaders | no |
| 10 | `metaspace-pressure` | W | many classes | ≥ 50 000 classes | no |
| 11 | `threadlocal-leak` | W | cleared TL keys | ≥ 1 (**no floor**) | no |
| 12 | `thread-pinning` | W | one thread hogs heap | ≥ 20% heap **OR** (≥100 locals AND ≥10%) | no |
| 13 | `thread-swarm` | W | many threads | ≥ 1000 threads | no |
| 14 | `weak-ref-escape` | I | only-weakly-reachable | ≥ 1000 objects | no |
| 15 | `proxy-lambda-bloat` | I | generated classes | ≥ 50% of classes, ≥ 200 classes loaded | no |
| 16 | `off-heap` | W | DirectByteBuffer cap | ≥ 64 MiB | no |
| 17 | `gc-waste` | W | unreachable garbage | ≥ 10% of heap | no |
| 18 | `static-field-anchor` | W | top suspect is Sticky Class | ≥ 20% heap | no |
| 19 | `jni-global-ref-leak` | W | JNI Global roots | ≥ 5000 count AND ≥ 5% retained | no |
| 20 | `heap-composition-skew` | I | one kind dominates | ≥ 70% of heap | no |
| 21 | `finalizer-queue-backlog` | W | Finalizer instances | ≥ 10 000 | no |
| 22 | `cached-reflection-metadata` | I | reflect.{Method,…} | ≥ 500 000 combined | no |
| 23 | `session-scope-leak` | W | class name ~ "Session" | ≥ 100 000 inst | no |
| 24 | `connection-leak` | W | class name ~ "Connection/Socket" | ≥ 1000 inst | no |
| 25 | `event-listener-accumulation` | W | name ~ "Listener/Observer/Subscriber" | ≥ 100 000 inst | no |
| 26 | `parser-output-accumulation` | I | XML/JSON parser pkgs | ≥ 100 000 inst | no |
| 27 | `interned-string-bloat` | W | many Strings + JNI Globals | ≥ 2M Strings AND ≥ 1000 JNI | no |
| 28 | `duplicate-strings` | I | dup String waste | ≥ 16 MiB **OR** ≥ 5% heap | **--find-duplicates** |
| 29 | `char-array-slack` | I | over-allocated backing | ≥ 16 MiB AND ≥ 1000 arrays | **--find-duplicates** |
| 30 | `over-capacity-collections` | I | under-filled collections | ≥ 5% heap wasted, fill ≤ 50% | **--collections** |
| 31 | `large-unbounded-collection` | W | one huge collection | ≥ 1M elements **OR** ≥ 20% retained | **--collections** |
| 32 | `sparse-object-arrays` | I | sparse obj arrays | ≥ 10k arrays, fill ≤ 20%, ≥ 5% heap wasted | **--collections** |
| 33 | `constant-value-arrays` | I | single-value prim arrays | ≥ 8 MiB | **--collections** |
| 34 | `big-drop-concentration` | **C** | dominator big drop | ≥ 5% heap AND ≥ 64 MiB | no |
| 35 | `fixed-per-object-overhead` | W | header cost | ≥ 20% of heap in headers | no |
| 36 | `hash-collision-hotspot` | W | over-full maps | ≥ 100 tracked, load > 90% | no |
| 37 | `empty-collection-cemetery` | I | empty collections | ≥ 60% **OR** ≥ 500 000 empties | no |
| 38 | `oversized-prim-array` | W | one huge prim array | ≥ 5% heap AND ≥ 64 MiB | no |
| 39 | `duplicate-prim-arrays` | W | dup prim-array waste | ≥ 16 MiB **OR** ≥ 5% heap | **--find-duplicates** |

### 26.2 Severity distribution is skewed toward Warning

Of the 39 rules: **2 always-Critical** (`concentration` conditionally, `big-drop-concentration`),
1 conditionally-Critical (`headline-retainer`), **~19 Warning**, **~14 Info**. That is a lot of
Warnings competing for attention. On a real leaking dump, it is entirely plausible for 8–12
Warnings to fire at once (object-swarm + thread-pinning + static-field-anchor + gc-waste +
fixed-per-object-overhead + hash-collision + oversized-prim-array + duplicate-prim-arrays …).
The render is "show all that fire" with no ranking beyond registry order, so the reader gets a
wall of equal-weight Warnings and no guidance on *which one to fix first*.

**26.2a Rank fired signals by estimated reclaimable bytes, not registry order.**
Most Warning/Info rules already carry a byte quantity in their detail (wasted bytes, retained
share, shallow). Sort the fired list by that magnitude (Critical first, then by bytes desc) so
the biggest lever floats to the top. Registry order is an implementation artifact, not a
priority. *Data: Have — every byte-denominated rule already computes the number; this is a
render-side sort. A few name-pattern rules (session/connection/listener) carry only a count,
not bytes — fall back to instance count × avg shallow, or sort those last.* **Effort: Compute-cheap.**

**26.2b Collapse the 5 always-on "orientation" signals into one header block.**
`headline-retainer`, `concentration`, `gc-root-type`, `shape`, `one-leak-or-many` are not
"problems" — they are *orientation* (how big, how concentrated, how deep, one-vs-many). They
fire on every report including healthy ones. Mixing them into the same list as real Warnings
dilutes the signal. Group them under a "Heap at a glance" sub-header, separate from
"Problems detected." *Data: Have — pure grouping by rule id.* **Effort: Format-plumbing.**

### 26.3 False-positive risks (rules that will cry wolf)

**26.3a `classloader-leak` (#8) has no floor and no share gate.**
It fires on the single most-retained duplicate-loaded class *whenever any class is loaded by
>1 loader.* But multi-loader loading is completely normal: app servers, OSGi, Spring Boot
nested jars, and JDK bootstrap all legitimately load the same class name under different
loaders. This will fire "Classloader leak — Warning" on healthy app-server dumps. Add a floor:
require `loader_count ≥ 3` (two is routine) **and** `total_retained ≥ ~4 MiB` or a heap-share
gate before emitting a Warning; otherwise downgrade to Info or suppress. *Data: Have —
`DuplicateClass` already has `loader_count` and `total_retained`.* **Effort: Compute-cheap.**

**26.3b `threadlocal-leak` (#11) fires on a single cleared key.**
`n == 0` is the only suppression. One stale `ThreadLocalMap` entry with a cleared key is
normal churn — the ThreadLocal mechanism *expects* stale entries and cleans them lazily. A
Warning on `n == 1` is noise. Add a floor (e.g. ≥ 1000 cleared-key entries, or a share of
total ThreadLocalMap entries) before Warning. *Data: Have — it already has the count; may want
a denominator (total TL entries) which would be an Add.* **Effort: Compute-cheap (floor) /
Add (ratio).**

**26.3c Name-pattern rules (#23 session, #24 connection, #25 listener) match on substrings.**
`contains("Session")` will match `SessionFactory`, `HttpSessionListener`, `SessionConfig`,
`SqlSessionTemplate` (MyBatis) — none of which are leaked sessions. `contains("Connection")`
matches `ConnectionPool`, `ConnectionFactory`, `HttpURLConnection`. These rules assert a leak
("sessions that were never invalidated") purely from a class-name substring and an instance
count, with no retained-size or growth evidence. High false-positive risk, and the detail text
is over-confident. Two fixes: (1) tighten patterns (word-boundary / suffix match, exclude
`*Factory|*Config|*Pool|*Template|*Builder`); (2) soften the wording from assertion to
hypothesis ("N instances of a session-named class — *if* these are per-user sessions, a
registry may be retaining invalidated ones"). *Data: Have — pattern + wording change only.*
**Effort: Compute-cheap + Format-plumbing.**

**26.3d `connection-leak` (#24) floor of 1000 is low and unit-blind.**
1000 `*Connection*` instances is nothing for a busy server with a large pool plus in-flight
`HttpURLConnection`s. Combined with the substring match (26.3c) this is the highest-FP rule in
the set. Raise the floor and/or require the instances to have no owning pool. *Data: Have.*
**Effort: Compute-cheap.**

**26.3e `interned-string-bloat` (#27) infers intern() abuse from an unrelated proxy.**
It fires on (≥2M Strings) AND (≥1000 JNI Global roots). But JNI Global root count is *not* a
measure of intern-table size — interned strings are held by the StringTable (a native
hashtable), not JNI globals. This correlation is a guess, and 2M Strings is common in any
large app regardless of interning. This rule will fire on large healthy heaps that use JNI at
all (e.g. anything with native libs) and mislabel them "intern() abuse." Either find a real
signal (StringTable size is not in a standard hprof, so this may not be detectable) or drop the
rule / downgrade to a soft Info. *Data: the honest signal is likely not present — flag as a
known-weak heuristic.* **Effort: needs rethink (possibly delete).**

**26.3f `boxed-primitive-bloat` (#7) OR-gate makes the 5M floor moot on big heaps.**
The condition is `instances < 5M AND pct < 5%` → suppress; i.e. it fires if *either* ≥5M
instances *or* ≥5% heap. On a 20 GB heap, 5% is 1 GB of boxed primitives — reasonable. But on
a 200 MB heap, 5% is 10 MB, and boxed primitives at 16 B each = ~650k instances, which is
common and benign. The share gate should have an absolute floor too (e.g. also require ≥ ~1M
instances) so small heaps don't trip it. *Data: Have.* **Effort: Compute-cheap.**

### 26.4 False-negative risks (rules that will stay silent when they shouldn't)

**26.4a Absolute-count floors don't scale with heap size.**
Many rules use fixed instance-count floors: object-swarm 10M, session 100k, listener 100k,
parser 100k, reflection 500k, finalizer 10k, boxed 5M. These are calibrated for *large* heaps.
On a 500 MB heap that is genuinely leaking, 2M accumulating event objects (a real swarm) never
reaches the 10M floor and the rule stays silent. Consider making the primary gate a
*heap-share* (already present for some) and treating the absolute count as a secondary
confirmation, so the rules degrade gracefully on small dumps. This ties directly to backlog
item F (tiny-dump behavior): on small dumps almost every count-gated rule is silent, so the
triage section can be nearly empty even for a clearly-sick small heap. *Data: Have (shares
exist for several); some would need an Add (per-class retained for swarm).* **Effort:
Compute-cheap → Add.**

**26.4b Gated rules silently vanish without telling the reader.**
Six rules (#28–33 minus 34; the `--collections`/`--find-duplicates`-gated ones) return `None`
when their flag was not passed — indistinguishable from "checked and found nothing." A reader
running the default profile has no idea that duplicate-string, char-array-slack,
over-capacity, sparse-array, constant-array, and duplicate-prim-array analysis were *never
run.* The triage section should print a single line: "*Not analyzed (run with `--collections
--find-duplicates` for waste detection): duplicate strings, over-capacity collections, …*"
This is the same "silent-absence" problem flagged in §19 for the body sections, but it is worse
in triage because triage is where users look for completeness. *Data: Have — the renderer knows
which flags were set; emit a "not analyzed" note.* **Effort: Format-plumbing.**

**26.4c No rule covers monotonic growth / two-dump diff.**
Every rule is single-snapshot. The strongest leak signal — "this set grew between two dumps" —
is impossible here. Out of scope for one dump, but worth a note in the triage preamble that
these are *snapshot heuristics*, not growth evidence, so readers don't over-trust a single
capture. *Data: N/A (design note).* **Effort: Format-plumbing.**

### 26.5 Coverage gaps — analyses the model already supports but no rule fires on

These are rules that *could* be added cheaply because the backing data is already in the
`Report` (cross-referenced with §22 Have/Compute-cheap):

- **Fill-ratio field attribution.** `FieldAttributionRow.total_wasted_slots` pinpoints the
  exact `Class#field` responsible for collection slack, but no triage rule surfaces it. A
  "wasted memory concentrated in `HashMap#table` via `FooCache#entries`" Warning would be the
  most *actionable* signal in the whole set. *Data: Have.* **Effort: Compute-cheap.**
- **Retention-concentration Gini / long-tail.** `RetentionSummary.top1/top10/top100_bp` is
  rendered as a chart but no rule fires on "top 100 objects hold >90%" (extreme concentration,
  a strong single-leak signal distinct from the top-1 `concentration` rule). *Data: Have.*
  **Effort: Compute-cheap.**
- **Header overhead: rule #35 and the `header_overhead` table can disagree.**
  `FixedPerObjectOverhead` computes a heap-wide `total_objects × 12/16`, while
  `overview.header_overhead` is a *per-class* `Vec<HeaderOverheadRow>` (`total_header_bytes`
  each). These answer different questions (heap total vs top offenders) but the reader sees two
  header-overhead numbers with no stated relationship, and `Σ rows` will be ≤ the rule's total
  if the vec is capped. Reconcile in prose: the rule states the heap total; the table names the
  worst classes; add "(top N classes shown; total across all classes is X)." *Data: Have.*
  **Effort: Compute-cheap / Format-plumbing.**
- **Duplicate-class *count* explosion vs retained.** `classloader-leak` looks only at the max
  single class; a loader leak more often shows as *thousands of classes each duplicated a few
  times.* A rule on `Σ duplicate_classes.total_retained` or `count of classes with loader_count
  ≥ 2` would catch the diffuse case #8 misses. *Data: Have.* **Effort: Compute-cheap.**

### 26.6 Anchor / navigation integrity

Every signal carries an optional `(anchor, label)` used to deep-link into the body section.
Several point at anchors that must exist in *both* renderers:

- #6 object-swarm, #9–10, #20–27 all anchor to `("overview", "System Overview")` — a heavily
  overloaded anchor. Nine distinct problems all deep-link to the same generic section, so the
  link doesn't take the reader to the *evidence* (the specific histogram row / GC-root row).
  Where the evidence lives in a specific table row, anchor to that row's id, not the section
  top. *Data: Have if row ids exist; several rows lack stable ids → small Add.* **Effort:
  Compute-cheap → Add.**
- #34 big-drop and #38 oversized-prim anchor to `("dominator-tree", …)` / `("arrays", …)`;
  #39 dup-prim anchors to `("dup-strings", "Duplicate Strings")` — verify these anchor ids are
  emitted by the markdown renderer (headings are slugified) *and* registered in the HTML
  `Nav`/`IntersectionObserver` TOC. A dangling anchor is a silent dead link. **Add a test** that
  every `TriageSignal.anchor` produced on the sample dumps resolves to a real heading in each
  format. *Data: Have — test-only.* **Effort: Compute-cheap (test).**

### 26.7 Threshold provenance is undocumented

The threshold block (lines 14–127 of triage.rs) is the entire triage policy, but the constants
are magic numbers with no stated derivation: why 64 MiB for off-heap and big-drop and
oversized-prim, but 16 MiB for the three dup/slack rules, and 8 MiB for constant-arrays? Why
50 000 classes for Metaspace? Some are clearly "one order of magnitude above normal," others
look arbitrary. This matters because a reader who disagrees with a fired/not-fired decision has
no way to judge whether the threshold is principled. Two asks: (1) a one-line rationale comment
per constant (many already have good doc-comments — extend to all); (2) surface the *threshold
that was crossed* in the signal detail ("≥ 64 MiB floor") so the reader can calibrate trust.
*Data: Have — constants are in scope at render.* **Effort: Format-plumbing.**

### 26.8 Missing rules worth adding (net-new analyses)

Beyond the cheap coverage gaps in 26.5, these are genuinely new detectors that either need a
small Add or a new pass, ordered by value:

1. **Empty-array / zero-length-collection cluster** — distinct from #37 empty-collection: many
   `new Object[0]`/`Collections.emptyList()`-style zero allocations pinned by long-lived
   holders. *(Compute-cheap from histogram + collection sizes.)*
2. **Nested-collection depth** ("collection of collections of collections") — a `Map<K,
   List<Map<…>>>` retention explosion. Needs the dominator-depth-by-collection data (partial
   Add).
3. **Enum/singleton bloat** — thousands of instances of a class that *should* be a singleton
   (private constructor, `INSTANCE` field pattern). Heuristic; needs field metadata (Add).
4. **Growing-generation proxy** — Strings/char[] whose *backing arrays* skew toward power-of-two
   capacities well above content length across the whole heap (StringBuilder over-grow at
   scale). Overlaps char-array-slack #29 but heap-wide, not per-array. *(Compute-cheap under
   --find-duplicates.)*
5. **Suspiciously uniform object size** — a huge count of one class at an oddly specific shallow
   size (e.g. millions of 48-byte objects) is the signature of a single hot allocation site;
   pair with alloc-sites when present. *(Compute-cheap.)*

### 26.9 Summary of triage findings

The rule *framework* is excellent (single source of truth, co-located thresholds, dumb
renderers). The *policy* has three fixable weaknesses, none requiring a heap pass:

1. **Ranking** — fired signals should sort by reclaimable bytes, and the 5 orientation signals
   should be visually separated from real problems (26.2). Compute-cheap + format.
2. **Calibration** — several rules will false-positive on healthy app-server heaps
   (classloader-leak, threadlocal-leak, the three name-pattern rules, interned-string-bloat)
   and several will false-negative on small heaps (absolute count floors). Mostly Compute-cheap
   threshold fixes; interned-string-bloat needs a rethink (26.3e). 
3. **Honesty** — gated rules vanish silently (26.4b) and thresholds are undocumented magic
   numbers (26.7). Pure format-plumbing.

Every one of these is a *rendering/policy* fix, not a missing-data fix — consistent with the
document's central thesis. The one genuinely new-data item is per-signal row anchors (26.6)
and a couple of the missing rules in 26.8.

## 27. Numerical Rigor Audit (pass 8)

Grounded in the actual `docs/samples/scala-doku-full.md` output. Percentages and totals are
the report's credibility: a reader who catches one incoherent number distrusts all of them.
This pass finds where the arithmetic is misleading, mislabeled, or self-inconsistent. All line
references are to that sample.

### 27.1 The headline defect: "% Heap" is *retained ÷ shallow-total* — a category error

The single most important number in the report is the retained-share percentage, and it is
computed against the wrong denominator throughout. `pct_of(retained, total)` in `triage.rs`
divides by `leaks.total_shallow` (= "Total shallow heap", **29.8 MB** in the sample). But the
numerator is a *retained* size. Retained and shallow are different quantities:

- **Top Components** (sample line 2655–2661): retained column sums to `62.3 + 61.7 + 2.6 +
  0.24 + 0.007 ≈ **126.9 MB**`, yet "Total shallow heap" is **29.8 MB**. The `% Heap` column
  (49.1% + 48.7% + 2.1% + 0.2% + 0.0% ≈ 100%) is `retained / shallow_total`. The retained
  numbers *legitimately* overlap and overshoot 29.8 MB (a dominator's retained set includes the
  shallow of everything under it), but dividing them by the shallow total produces a column
  that only *coincidentally* sums near 100% for this dump. On a heap with deeper sharing it
  would sum to 200%+. **The percentage is not a share of anything real.**
- The caption even says so: *"`% Heap` is the share of total reachable heap"* (line 2653) —
  but "total reachable heap" is ambiguous between shallow-total (29.8 MB) and the sum of all
  retained sets (meaningless). Neither reading makes `retained/shallow` a valid share.

**Fix:** decide what the percentage *means* and label it precisely. Two coherent options:
(a) **retained-of-total-retained-heap** — but total retained heap of the whole graph ≈ shallow
total only if there's a single root; generally the correct denominator for "what fraction of
live memory does this subtree keep alive" is the **shallow total** and the numerator should be
**the subtree's shallow contribution**, not its retained size; or (b) keep retained in the
numerator but denominate by the **root's retained set** (i.e. total live retained = shallow
total), and *state that shares can overlap and need not sum to 100%.* Option (b) with an
explicit "shares overlap; they do not sum to 100%" caption is the smallest honest fix. *Data:
Have — it is a labeling + caption change; the numbers are already computed.* **Effort:
Format-plumbing** (relabel) — but it is the highest-value correctness fix in the whole document.

### 27.2 "% of reachable heap" prose attached to shallow-total denominator

The triage bullets (sample lines 52, 58–64) all say *"76.7% of the reachable heap"* /
*"of heap."* `22.9 MB / 29.8 MB = 76.8%` confirms the base is shallow-total. For a *single*
dominant retainer this reads fine, but it inherits the 27.1 ambiguity: "reachable heap" is
never defined as "total shallow" anywhere the reader can see. Add a one-time definition in the
triage preamble: *"Percentages are share of total reachable **shallow** heap (29.8 MB);
retained-set shares can overlap."* *Data: Have.* **Effort: Format-plumbing.**

### 27.3 Off-heap 134.3 MB vs 29.8 MB on-heap — the biggest number is buried and uncontextualized

Sample line 65: DirectByteBuffers hold **134.3 MB** of native memory — **4.5× the entire
29.8 MB on-heap**. This is arguably the most important fact in the report (the process RSS is
dominated by off-heap, not heap), yet it is one Info-adjacent bullet with no ratio and no
headline. The "Likely problem" line (52) names the 22.9 MB Thread instead. **Surface the
off-heap:on-heap ratio in the summary** ("native/off-heap memory (134.3 MB) is 4.5× the
on-heap total (29.8 MB) — heap analysis alone will not explain this process's footprint").
*Data: Have — both numbers exist (`direct_byte_buffer_capacity_sum`, `total_shallow`); compute
the ratio.* **Effort: Compute-cheap.**

### 27.4 Shape "90% of objects within depth 11273, max depth 41355" is a degenerate artifact

Sample line 61. A p90 dominator depth of **11,273** and max of **41,355** is not "deep" in any
useful sense — it is the signature of a **linked-list** (`scala...$colon$colon` cons chains)
where each cons cell dominates the next, producing a depth equal to list length. Reporting
"90% of objects within depth 11273" as a heap *shape* descriptor is misleading: it sounds like
pervasive deep nesting when it's one pathological chain. Two fixes: (1) detect the linked-list
case (a single class dominating a chain of its own type) and describe it as such rather than as
generic depth; (2) cap the reported depth narrative ("retention flows through a chain
≥N deep, dominated by `$colon$colon`"). This also feeds the `DepthHistogramChart` MAX_BARS
fold (§19.7a) — 41k buckets fold into one `≥N` bar, so the chart already hides the tail, but
the prose doesn't. *Data: Have — depth histogram + histogram class are present; the linked-list
detection is Compute-cheap.* **Effort: Compute-cheap.**

### 27.5 Header overhead 36.6% overlaps with retained shares — double-counting risk

Sample line 66: headers = 10.9 MB = 36.6% of 29.8 MB. This is a *shallow* share (headers are
part of shallow), so 36.6% is coherent against the shallow total — good. But it sits in the
same bulleted list as the 76.7% *retained* share (line 58), and the reader cannot tell the two
percentages have different denominators/meanings (one is shallow-of-shallow, the other is
retained-of-shallow). A reader who adds them ("76.7% + 36.6% = 113%??") is right to be
confused. This is the concrete harm of 27.1/27.2: **mixed-denominator percentages in one
list.** Fix by tagging each: "(share of shallow heap)" vs "(retained, shares overlap)." *Data:
Have.* **Effort: Format-plumbing.**

### 27.6 Cap honesty — "Total" rows that are really "Total of shown rows"

Arrays-by-size (sample line 2689) shows a `**Total** 64,800 / 5.4 MB` — this one is a true
total (buckets partition all arrays). But elsewhere ("Top classes", "Biggest objects",
"Biggest collections") the tables are Top-N capped and any "Total"/summary line risks being
read as a heap-wide total when it is only the sum of shown rows. **Audit every bolded Total/Σ
line**: if it sums a capped table, label it "Total (top N shown)" and, where the true total is
known, add "of X total." The §22 matrix already flags most totals as *Have*; this is the
labeling follow-through. *Data: Have — totals exist; the true denominators exist in overview.*
**Effort: Format-plumbing.**

### 27.7 Rounding: sub-0.05% rows render as "0.0%"

Sample line 2661: PlatformClassLoader shows `7.4 KB / 0.0%`. Rendering a nonzero contribution
as "0.0%" is defensible but can mislead ("this component uses no memory"). Use "<0.1%" for
nonzero-but-rounds-to-zero, reserving "0.0%" (or "—") for genuine zero. *Data: Have.* **Effort:
Compute-cheap.**

### 27.8 Basis-point conversions are consistent but undocumented

`RetentionSummary.*_bp` (top1/top10/top100) are stored as basis points (0–10000) and divided
by 100 for display (charts.tsx ConcentrationChart, sample line 3458 narrative). The conversion
is correct and consistent across md/graphs/HTML. One gap: the JSON emits raw `_bp` integers
with no unit hint, so a JSON consumer must know the ×100 convention. Document the bp unit in
the JSON schema notes / field doc-comment. *Data: Have.* **Effort: Format-plumbing (docs).**

### 27.9 Unreachable "2.2% of total heap" uses a *different* total than everything else

Sample line 87: unreachable = 673 KB = "2.2% of total heap"; and line 88 "Heap fragmentation
(unreachable / total) = 2.2%." Here "total heap" = reachable + unreachable shallow (673 KB /
~30.5 MB ≈ 2.2%), i.e. a **different denominator** than the "% Heap" columns (which use
reachable-only 29.8 MB). Two different "total heap" bases in the same report, one section
apart, both unlabeled. Pick one canonical total, or label each occurrence with its exact base
("% of total-including-unreachable" vs "% of reachable"). *Data: Have — both totals exist.*
**Effort: Format-plumbing.**

### 27.10 Summary of numerical findings

The arithmetic is *mostly* internally consistent, but the **labeling of denominators is the
pervasive defect**: at least three distinct "totals" (reachable-shallow 29.8 MB, total-with-
unreachable ~30.5 MB, and the meaningless sum-of-retained 126.9 MB) all appear as "% of heap"
without disambiguation, and retained numerators are divided by shallow denominators to produce
shares that only coincidentally behave. None of this needs a new heap pass — every correct
denominator is already in the model. The fixes are: (1) **relabel the retained-share columns**
and state that retained shares overlap (27.1, 27.5); (2) **define "reachable heap" once** and
tag each percentage with its base (27.2, 27.9); (3) **surface off-heap:on-heap ratio** (27.3);
(4) **special-case the linked-list depth artifact** (27.4); (5) **cap-honest Totals** and
sub-0.1% rounding (27.6, 27.7). All Format-plumbing or Compute-cheap. This pass reinforces the
thesis: the report's problems are presentation, not computation.

## 28. Plain-Markdown Sample Walkthrough (pass 9)

A concrete top-to-bottom read of `docs/samples/scala-doku-full.md` (3,580 lines). Where §1–18
critique sections in the abstract, this pass reports what the *actual rendered file* does
wrong, with line numbers.

### 28.1 The dominator subtree is 43% of the entire report

Suspect #1 (`java.lang.Thread`) spans **lines 621–2245 (1,625 lines)**, of which the
`<details>Dominator subtree</details>` block alone is **lines 690–2244 = 1,554 lines = ~43% of
the whole 3,580-line report.** It is a `HashSet → BitmapIndexedSetNode → Object[] →
BitmapIndexedSetNode → …` recursion nested to **20+ indentation levels** (400+ lines sit at
16–17 levels deep). This is catastrophic for plain markdown:

- **Markdown list nesting visually collapses past ~6 levels.** GitHub, most static-site
  renderers, and every terminal pager stop adding indentation, so levels 7–20 render as a flat
  wall of bullets — the tree structure the block exists to show is *destroyed by the format.*
- **It buries everything after it.** A reader scrolling for Threads, Collections, or Leak
  Indicators must scroll past 1,550 lines of near-identical `BitmapIndexedSetNode` bullets.
- **It's inside `<details>`**, which helps on GitHub (collapsed by default) but does nothing in
  a terminal, in `less`, in most Markdown-to-PDF pipelines, or when grepping.

**Fix:** cap the plain-md dominator subtree hard — depth ≤ 4 and breadth ≤ 5 children per node,
with a "… +N more descendants (M retained)" roll-up, exactly like the HTML `DomSubtreeSvg`
does visually. The full tree already lives in the JSON for anyone who needs it. The HTML format
solved this with SVG (§domTree.tsx); plain-md needs the *pruning*, not the SVG. This is the
single highest-impact change to the plain-markdown format. *Data: Have — the tree is in the
model; this is a render-side depth/breadth cap.* **Effort: Compute-cheap.** (Cross-ref §5,
§19; this quantifies it.)

### 28.2 The two suspects are wildly imbalanced and #2 inherits the same bloat

Suspect #1 = 1,625 lines; suspect #2 (`java.lang.Class`, line 2246) = ~69 lines. The report
spends 24× more space on suspect #1 purely because its subtree is deeper, not because it is 24×
more important (it's 76.7% vs 11.7% retained — ~7× by the metric that matters). Space allocated
to a suspect should track its *retained share*, not its *subtree depth.* Once 28.1's cap is in
place this self-corrects. *Data: Have.* **Effort: Compute-cheap (falls out of 28.1).**

### 28.3 "Accumulated objects by class" (line 631) is the useful view and should lead

The 50-row class table (lines 633–684) is genuinely the actionable content of the suspect: it
says *what* is piling up (`HashSet` 8.0 MB, `Solver` 4.6 MB, `$colon$colon` 4.2 MB, 8,909
`ConnectiveApplication` = 3.9 MB). This is what a developer acts on. Yet it is followed by the
1,554-line subtree that adds almost nothing (it's the same classes, re-listed 400× in tree
form). **Promote the class table, demote/cap the tree.** Also: the table shows Objects/Shallow/
Retained but not a **% of this suspect's retained** column — add it so the reader sees "HashSet
is 35% of this Thread's 22.9 MB" at a glance. *Data: Have — retained + suspect total both
present.* **Effort: Compute-cheap.**

### 28.4 "Path to GC root" (line 688) is trivial here and mislabeled elsewhere

For suspect #1 the path is one line ("directly held by a GC root; no intermediate chain") —
correct and good. But the section header "Path to GC root (dominator chain)" conflates two
different MAT concepts: the **path to GC root** (the reference chain that keeps the object
alive) and the **dominator chain** (who exclusively retains whom). They are not the same walk.
For a Thread held directly they coincide; for a deeply-held object they diverge and the label
becomes wrong. Rename to just "Dominator chain" (which is what the subtree shows) or compute a
real ref-path. *Data: Have for dominator chain; a true GC-root ref-path is an Add (ref walk).*
**Effort: Format-plumbing (rename) or Add (real path).**

### 28.5 Collection Contents by Type (line 3198) is mostly tautological

The "Top Value Types" column reads `HashMap → HashMap$Node ×7,601`, `ConcurrentHashMap →
ConcurrentHashMap$Node ×5,576`, `LinkedHashMap → LinkedHashMap$Entry ×938`, `Hashtable →
Hashtable$Entry ×9`. These rows convey **zero information** — of course a HashMap contains
HashMap$Nodes; that's its internal structure, not its *contents.* The one useful row is
`ArrayList → Class ×982, String ×163, …` (real element types). The rule should **unwrap the
entry/node wrapper** and report the type of the *value* the node holds (the map's V), not the
node type itself. As rendered, 6 of 7 rows are noise. *Data: needs the entry's value type,
which requires following `Node.value` — likely an Add (one more field decode) unless already
captured.* **Effort: Add (unwrap map entries) — high value for a small pass.**

### 28.6 Section ordering doesn't follow the drill-down narrative (ties to §25.3)

The current order is: Summary → Triage → System Overview (with Duplicate Strings, Boxed
Numbers, Header Overhead, Histogram, Class Loaders, Duplicate Classes all *nested under
Overview*, lines 136–616) → Leak Suspects → Top Consumers → Dominator Analysis → Threads → Top
Components → Arrays → Collections → … This has two problems:

- **System Overview is overloaded** (lines 69–616 = 547 lines): it contains 8 sub-sections
  including three full waste analyses (Duplicate Strings, Dup Prim Arrays, Boxed Numbers) and
  Header Overhead. These are *waste* content (§24's domain), not "overview." Hoist them into a
  dedicated "Wasted Memory" section per §24's unified accounting.
- **Attribution axes are scattered** (Top Consumers §2315, Top Components §2651, Container
  Attribution §2983, Fields by Retained §3047) exactly as §25 describes — 700 lines apart.
  Order them as a contiguous descent.

*Data: Have — pure reordering + a new "Wasted Memory" umbrella heading.* **Effort:
Format-plumbing** (but touches the whole render_md structure).

### 28.7 Small concrete nits found in the walkthrough

- **Duplicate Strings / Boxed Numbers / Header Overhead nested under "System Overview"**
  (lines 136, 311, 344) — misfiled (see 28.6); they are waste analyses.
- **HPROF Record Census (line 110)** is implementation trivia (counts of hprof record types) —
  interesting to *this tool's* developer, useless to someone hunting a leak. Move to an
  appendix or gate behind a `--verbose`/`--debug` flag. *Effort: Format-plumbing.*
- **Threads 4 and 5 are skipped** (line 2569 jumps Thread 3 → Thread 6 at 2609) — the numbering
  implies threads exist that aren't shown, with no "(threads 4–5 omitted: trivial retention)"
  note. Either renumber the shown threads or state why gaps exist. *Effort: Format-plumbing.*
- **Glossary is last (line 3537)** — a reader hits undefined terms ("dominator," "retained,"
  "Sticky Class") 3,000 lines before the definitions. Link first-use to the glossary anchor, or
  move a short glossary up front. *Effort: Format-plumbing.*

### 28.8 Summary of the walkthrough

The plain-markdown format is dominated by one pathology: **the uncapped dominator subtree eats
43% of the file and renders as an unreadable flat bullet-wall.** Fix that one thing (28.1) and
the format roughly halves in size and becomes navigable. The secondary theme is **misfiling**:
waste analyses live under "System Overview," attribution axes are scattered, and the genuinely
actionable content (the accumulated-by-class table, 28.3) is out-ranked by low-value content
(the subtree, the record census, tautological collection contents). Every fix is
Format-plumbing or Compute-cheap except 28.5 (unwrap map entries, a small Add) and 28.4's
optional real ref-path. Consistent with the thesis.

## 29. Markdown-with-Graphs Sample Walkthrough (pass 10)

Comparing `scala-doku-full.graphs.md` (**6,979 lines**) against the plain `scala-doku-full.md`
(3,580 lines). The graphs variant is meant to be "plain md + inline ASCII charts," but the
diff reveals it does more (and less) than that, with two serious regressions.

### 29.1 The graphs variant is 2× larger and its subtree is 72% of the file

Suspect #1's dominator subtree runs **lines 640–~5640 ≈ 5,000 lines = ~72% of the 6,979-line
file** (vs 43% in plain md). Two compounding causes:

- **It renders the subtree as a fenced code block with box-drawing connectors** (`└─ ├─ │`,
  lines 640+). This is *the right call* for structure — a `` ``` `` fence preserves indentation
  that markdown bullet nesting destroys (§28.1), and the box-drawing actually shows the tree.
  **This rendering should be back-ported to plain md**, or plain-md's bullet tree replaced with
  it. *Data: Have.* **Effort: Compute-cheap.**
- **But it does NOT collapse identical siblings with `×N`.** Plain md uses `×N` collapsing 585
  times; the graphs subtree uses it only 93 times (sample lines 665–667 list three identical
  `cafesat.sat.Literal` rows separately where plain md wrote `×3`). That un-collapsing roughly
  doubles the tree. **Apply the same `×N` sibling-collapse in the graphs renderer**, and — with
  §28.1's depth/breadth cap — this drops from 5,000 lines to a few dozen. *Data: Have.*
  **Effort: Compute-cheap.**

Net: the graphs format needs the *same* subtree cap as plain md (§28.1) **plus** `×N` collapse
parity. Its tree *rendering* is superior; its tree *pruning* is worse.

### 29.2 REGRESSION: the graphs variant silently drops three whole sections

`comm` of the two files' headings shows the graphs format is **missing** three sections that
plain md has:

- `### Boxed Numbers`
- `### Duplicate Primitive Arrays (approximate)`
- `### Object Header Overhead`

These are three of the most important *waste* analyses (§24). A user who picks the "graphs"
output — reasonably expecting *more* visualization, not *less* content — loses boxed-number
bloat, duplicate-primitive-array waste, and header-overhead attribution entirely. This is a
straight content regression, almost certainly an un-ported branch in `render_graphs.rs`. **The
graphs renderer must render every section the plain renderer does** (charts are additive, never
subtractive). *Data: Have — the data is identical; the sections just aren't emitted.* **Effort:
Format-plumbing (port the three sections).** This is the highest-severity finding in the pass.

> **Root cause confirmed in code:** `render_graphs.rs` is a **full parallel renderer** (930
> lines), not a `render_md` + charts wrapper — it re-implements each section by hand. Its
> `render_system_overview_graphs` (lines ~102–418) emits record-census, duplicate-strings,
> histogram, class-loaders, and duplicate-classes, but never calls the boxed-numbers,
> header-overhead, or duplicate-primitive-array renderers that `render_md.rs` has. The two
> renderers can (and here do) drift. The durable fix is architectural: have the graphs renderer
> *delegate* to the shared `render_md` section functions and only override the ones that add a
> chart (it already does this for `render_dominator_depth_graphs`, line 469–471) — so a new
> section added to plain md can never silently vanish from graphs again.

### 29.3 REGRESSION: a triage bullet links to a section that doesn't exist in this format

Sample line 66 (graphs): the "Fixed per-object header overhead" triage bullet ends *"See
[Header Overhead](#header-overhead)."* — but per 29.2, **the Header Overhead section does not
exist in the graphs file.** The anchor is dead. This is the exact failure mode §26.6 predicted:
a `TriageSignal.anchor` that resolves in one format but dangles in another. It is caused by
29.2 (triage is format-agnostic; the body section was dropped only in graphs). Fixing 29.2
fixes this, but it also proves the need for §26.6's **cross-format anchor-resolution test.**
*Data: Have — test + the 29.2 fix.* **Effort: Compute-cheap (test) + Format-plumbing (fix).**

### 29.4 The inline charts that DO exist are good and low-cost

Where the graphs format adds visualization, it does so tastefully:

- **Bar column appended to existing tables** (GC Roots line 97–98, Heap Composition 105–107,
  Arrays-by-size 213–220, Biggest Classes 275–290): a `████████▌` column sized to the row's
  value relative to the column max. Readable, aligns in monospace, degrades gracefully. Good.
- **Inline sparkline** for the array-length distribution (line 209: `` `▁▁▂▄▅▇█▂▁▁▁▁▁▁▁` ``) —
  a compact shape-at-a-glance. Good.

Two refinements: (a) the bar column has **no axis/scale label** — a reader can't tell if a full
bar is 22.9 MB or 22.9 GB without reading the adjacent number column (which is fine here since
the number is right there, but a one-time "bars scaled to column max" caption would help); (b)
**Unicode block-drawing assumes a monospace + full-Unicode terminal** — in a proportional font
(some Markdown viewers) or a Unicode-poor terminal the bars misalign or render as tofu. Offer an
ASCII-only fallback (`####----`) behind a flag, or document the monospace requirement. *Data:
Have.* **Effort: Format-plumbing.**

### 29.5 Charts duplicate the table they annotate — acceptable here, but watch the trend

The bar columns live *inside* the data tables (same row, extra column), so there's no
table-vs-chart duplication — the chart *is* a table column. This is the anti-duplication the
user wants and is better than the HTML approach of a separate `<Chart>` + fallback `<table>`.
Where the graphs format adds a *standalone* chart block (not seen much in this sample), ensure
it doesn't restate a table verbatim. Current state: fine. *Data: N/A (observation).*

### 29.6 Summary of the graphs walkthrough

The graphs format has the best *tree rendering* (fenced box-drawing, 29.1) and the best *chart
integration* (in-table bar columns, 29.4–29.5) of any format — those ideas should propagate to
plain md. But it has two **regressions that make it strictly worse than plain md for finding
waste**: it drops Boxed Numbers, Duplicate Primitive Arrays, and Header Overhead (29.2), and
consequently ships a dead triage link (29.3). Priority order: **(1) restore the three dropped
sections (29.2)** — severity-critical content loss; (2) add the cross-format anchor test
(29.3/§26.6); (3) apply subtree cap + `×N` parity (29.1); (4) back-port the box-drawing tree
and bar columns to plain md. All Format-plumbing/Compute-cheap.

## 30. Eclipse MAT Feature Parity (pass 11)

Eclipse Memory Analyzer (MAT) is the reference tool this project measures itself against — and
it does so *literally*: `src/diff/` implements a `compare mat` subcommand
(`main.rs:373 CompareCmd::Mat`) that parses a MAT HTML export and classifies every field
against our report into MATCH / EXPLAINABLE / FAIL tiers (`diff/model.rs Tier`,
`diff/parse.rs`). So parity is *tested* for the pages MAT exports: System_Overview,
Class_Histogram, Top_Consumers, Top_Components, Leak_Suspects (`parse_system_overview`,
`parse_class_histogram`, `parse_top_consumers`, `parse_top_components`, `parse_leak_suspects`).
This pass audits parity against MAT's full feature set, marking each **Present / Partial /
Absent** and grounding in the model + sample.

### 30.1 Parity table

| MAT feature | Status | Evidence / gap |
|---|---|---|
| **Leak Suspects report** | **Present** | `LeakSuspects.suspects`, sample §Leak Suspects (line 617); parity-tested via `parse_leak_suspects` / `MatSuspect`. MAT's *prose narrative* ("System.Object[] … accumulated by …") is richer than our bullet list — see 30.2. |
| **Dominator tree** | **Present** | `DominatorAnalysis`, `DomTreeNode`; sample dominator subtree (line 690). But rendered as an uncapped text tree (§28.1) vs MAT's lazy-expand UI. |
| **Class histogram** | **Present** | `overview.histogram` (`HistRow`), sample line 381; parity-tested (`MatHistRow`). We add a Retained column MAT's overview histogram lacks (`MatHistRow.retained = None` on that page — `diff/model.rs:247`). |
| **Retained set / retained size** | **Present** | Every row carries `retained`; this is the tool's core competency. |
| **Top Consumers (objects/classes/packages)** | **Present** | `TopConsumers`, sample line 2315; parity-tested. |
| **Top Components (by class loader)** | **Present** | `TopComponents`, sample line 2651; parity-tested. |
| **Duplicate Classes** | **Present** | `overview.duplicate_classes`, sample line 450; MAT's "Duplicate Classes" query equivalent. |
| **Thread Overview / thread details** | **Present** | `ThreadOverview`, sample line 2493; includes per-thread locals + stack, matching MAT's Thread_Overview. |
| **Unreachable objects** | **Present (exceeds MAT)** | `unreachable_*`, sample line 3290 incl. Garbage-Root Dominator Trees — MAT typically discards unreachable; we retain and attribute them. |
| **GC roots by type** | **Present** | `gc_roots_by_type` + `gc_roots_retained_by_type`, sample line 93. |
| **Path to GC roots** | **PARTIAL** | Single-object suspect prints "Path to GC root (dominator chain)" (sample line 686) — the *dominator* chain, not the *reference* path (§28.4). Multi-instance suspect prints "**Merged Paths to GC Roots**" (sample line 2252) which *does* name root types ("GC root: Sticky Class"). Both are dominator-based and **omit field names** (`Foo.bar`), which MAT shows. |
| **Merge shortest paths to GC roots** | **PARTIAL** | A merged view exists for multi-instance suspects (sample line 2252, "Merged Paths to GC Roots"), grouping dominated children by class + root type. But it is a *dominator* merge, not MAT's *reference-path* merge, and carries no field-level edges. |
| **OQL (Object Query Language)** | **ABSENT** | No `src/oql`, no `oql`/`query` subcommand in `main.rs`. (Memory notes an OQL parser is *planned*; not yet wired.) MAT's OQL is a major differentiator for ad-hoc investigation. |
| **Immediate dominators query** | **Present** | `ImmediateDominators`, sample line 2455 — but shallow-denominated (§25.4). |
| **Group by class loader / package / class** | **Present** | Top Components (loader), Biggest Packages (package), histogram (class). |
| **Two-snapshot comparison** | **Present (exceeds MAT)** | `compare reports r1.json r2.json …` (`main.rs:388`, `diff_reports.rs`) does N-way growth diff — MAT compares two, we chain N. |
| **Collection fill-ratio / query** | **Present (exceeds MAT)** | `collections.*` (fill ratio, map collision, constant arrays) is deeper than MAT's built-in collection queries. |
| **Duplicate strings / arrays** | **Present (exceeds MAT)** | `duplicate_strings`, `duplicate_prim_arrays` — MAT needs the "Strings" query + manual work; we compute waste directly. |
| **Object inspector / field values** | **ABSENT** | MAT lets you click any object and read its field values. Static report cannot; the HTML app *could* embed per-object field snapshots for suspects (Add). |
| **List objects (incoming/outgoing refs)** | **ABSENT** | MAT's "list_objects [incoming|outgoing]" walks the ref graph interactively. We only expose dominator edges, not raw references. |
| **Histogram of retained set of a selection** | **PARTIAL** | Per-suspect "Accumulated objects by class" (sample line 631) is exactly this for suspects — but only for suspects, not arbitrary selections (no selection mechanism). |

### 30.2 The three gaps worth closing (ranked)

**30.2a Field-level reference path, not just the dominator chain — HIGHEST VALUE.**
The sample's "Path to GC root (dominator chain)" (line 686) and "Merged Paths to GC Roots"
(line 2252) are both dominator-based: they name the retained *objects* and their *root types*
but **not the field names** connecting them. A developer fixing a leak needs the **reference
path** — the actual chain of fields (`ScalaDoku.solver → Solver.clauses →
ArrayList.elementData[3] → …`) that a human edits to break retention. The dominator chain tells
you *what is retained*; the field path tells you *which reference to null out.* MAT's most-used
leak-hunting feature is "Path to GC Roots → exclude weak/soft," and it shows field names.
*Data: Add — requires storing incoming-reference edges (field names) or doing a reverse BFS with
field labels during the pass; the memory notes RefWalk tail-scalar capture already exists, so
the graph edges may be partially available.* **Effort: Add (reference-edge walk with field
labels).** This is the one place the tool is materially behind MAT for the user's stated goal.

**30.2b OQL / ad-hoc query — the flexibility gap.**
Every canned section answers a fixed question; MAT's OQL lets a user ask *their* question
("SELECT * FROM java.lang.String s WHERE s.count > 1000"). Memory records an OQL parser effort
(chumsky+ariadne+logos+reedline) is planned. When landed, the report should link the top
suspects to a pre-filled OQL query ("investigate further: `SELECT …`"). *Data: Add (the OQL
engine itself); Compute-cheap to emit suggested queries once it exists.* **Effort: Add (large,
already scoped separately).**

**30.2c Add field labels to the existing merged-paths view.**
The multi-instance merged view already exists (sample line 2252) and groups by class + root
type — good. The gap is that it merges *dominators*, not *reference paths*, and shows no field
edges: for the diffuse-leak case (§26.3, the `one-leak-or-many` "many" branch) the actionable
question "what field do all N instances share on the way to a root?" is still unanswered.
Upgrading this to a field-labeled reference merge (built on 30.2a's edges) turns an existing
view into MAT's merge-shortest-paths. *Data: Add — depends on 30.2a's reference edges.*
**Effort: Add.**

### 30.3 Where we already EXCEED MAT (surface this as a strength)

The report is not merely chasing MAT — it is ahead on **waste detection** (duplicate strings/
arrays, boxed-number bloat, header overhead, fill-ratio slack, constant/empty collections —
none are one-click in MAT), on **unreachable-object attribution** (MAT discards; we build
garbage-root dominator trees, sample line 3334), and on **N-way snapshot diffing**
(`compare reports`). MAT users do these by hand with OQL and external scripts. The report should
say so once, up front: *"Beyond MAT: this report pre-computes wasted-memory analyses (duplicate/
boxed/slack) and unreachable attribution that MAT requires manual queries for."* This directly
serves the user's "where is heap wasted" goal and differentiates the tool. *Data: Have — it's a
one-line positioning statement.* **Effort: Format-plumbing.**

### 30.4 Parity-testing gaps (the harness itself)

`compare mat` only classifies the five parsed pages (30.1). MAT features we *added* (collections,
duplicates, unreachable trees, dominator-depth) have **no MAT counterpart to diff against**, so
they are untested for correctness by that harness — they rely on unit tests only. That's
expected (MAT can't export what it doesn't compute), but worth noting: the MATCH/FAIL tiers
cover reachability/histogram/suspects/consumers/components, *not* the waste analyses. When those
analyses have bugs (e.g. the §29.2 dropped sections, or slot-vs-byte confusion in §24), the MAT
diff will not catch them. **Recommend an internal golden-file test** over the sample dumps for
the non-MAT sections to complement the parity harness. *Data: Have — test infrastructure exists
(`md_test.rs`).* **Effort: Compute-cheap (tests).**

### 30.5 Summary of parity findings

The tool is at or beyond MAT on every *static* analysis MAT exports, and the parity is
machine-tested for the five core pages. The genuine gaps are all **field-level graph-walk**
features: **field-labeled reference paths** (30.2a — the important one; the dominator-based path
and merged-path views already exist but lack field names), OQL (30.2b, planned), and upgrading
the existing merged view to a reference merge (30.2c). All three require reference-edge data
(with field labels) the current dominator-only model lacks — so unlike every other pass in this
document, **these are true "Add" gaps, not rendering gaps.** Conversely, the tool's waste +
unreachable + N-way-diff advantages over MAT are under-advertised and should be stated (30.3).
Net: parity is strong; close 30.2a first (highest user value), advertise 30.3, and backstop the
non-MAT sections with golden tests (30.4).

## 31. Empty / Degenerate-Heap and Tiny-Dump Behavior (pass 12)

Every prior pass audited the tool against a *pathological* dump (scala-doku-full: 90 MB heap,
76.7% concentration, deep chains). This pass asks the opposite question: **what does the report
render when there is little or nothing to report?** A tool that only reads well on a leak is a
tool that has quietly optimised for the demo. The answer matters because the common real-world
case is a developer pointing this at a *healthy* dump to confirm "nothing is wrong here" — and if
the report can't say that cleanly, it manufactures false alarms or emits confusing empty scaffolding.

The grounding is threefold: the `return None` / `is_empty()` guard branches in `triage.rs`, the
unconditional section-renderer calls in `render_markdown` (render_md.rs:256–277), and the two
shipped sample dumps — **both of which turn out to be leak-heavy**, which is finding 31.1.

### 31.1 There is no healthy-heap sample dump — the empty path is untested by example

`docs/samples/` ships two dumps, `scala-doku` and `scala-doku-full`. I extracted the triage
bullets from the *smaller* one (scala-doku.md:50+) expecting a calmer profile. It is not calmer:

> - **Headline retainer:** `java.lang.Thread` … retains 22.9 MB (76.7% of reachable heap).
> - **Concentration:** highly concentrated … holds 76.7% of the heap …
> - **Shape:** deep … 90% of objects within depth 11273, max depth 41355.
> - **Off-heap (DirectByteBuffer):** 134.3 MB of native memory …
> - **Fixed per-object header overhead:** … 10.9 MB (36.6% of heap) …
> - **Empty-collection cemetery:** 5,806 of 6,307 tracked collections (92.1%) are empty …

Same pathologies, same 76.7% concentration, same p90-depth artifact (§27.4) — it is essentially
the same application, just a subset. **Neither sample exercises the healthy/tiny/empty code
paths.** Every empty-state branch below (`return None`, "No … found", "diffuse", "No dominant
retainer found") is therefore validated *only* by the unit fixture `base_report()`
(triage.rs:1561), never by an end-to-end rendered sample a human has eyeballed. That is a real
gap: the demo output looks great precisely because the demo input is pathological.
**Recommend adding a third sample dump from a small, healthy program** (a hello-world with a few
MB of heap and no leak) and regenerating `docs/samples/` from it, so the empty-state prose is
exercised and reviewable. *Data: Have — the CLI already renders any dump; this is a fixture/CI
task. Effort: Format-plumbing (add fixture + regen).*

### 31.2 The two "always-fire" rules narrate a leak even when there is none

`HeadlineRetainer` (triage.rs:222) and `Concentration` (triage.rs:272) are the only two rules with
no `return None` — they always emit. On a genuinely healthy heap (no suspect over threshold, but a
non-empty `biggest_objects`) the reader gets:

> - **Headline retainer:** `SomeClass` retains 40 KB (0.3% of reachable heap).
> - **Concentration:** diffuse — retention is spread across multiple roots, so there is no single object to free.

The Concentration fallback is fine — "diffuse … no single object to free" is a correct, calming
statement. But **HeadlineRetainer at 0.3% still leads with the word "retainer" and a Critical/
Warning framing**, presenting the largest-of-nothing as if it were a finding. The truly-empty
fallback (`biggest_objects` also empty → "No dominant retainer found.", Info, triage.rs:259) is
better, but it fires only when there are *zero* objects — an almost-impossible real dump. The
common healthy case (small objects, none dominant) gets the alarming middle branch.
**Recommend a low-water gate:** when the headline retainer holds less than, say, 5% of the heap,
downgrade to Info and reword ("Heap is diffuse — the largest single retainer holds only X%; no
dominant consumer"). This mirrors the Concentration rule's own diffuse branch and stops the report
crying wolf on healthy dumps. *Data: Have — `pct_of(s.retained, total)` is already computed at
triage.rs:241. Effort: Compute-cheap (one threshold + branch in the existing rule).* Cross-ref
§26.3 (false-positive risks) — this is the headline-level instance of that family.

### 31.3 Gated rules vanish silently, and the report never says why

Rules behind `--collections` / `--find-duplicates` short-circuit on `tracked == 0` /
`cfr.tracked == 0` (e.g. `OverCapacityCollections`, `EmptyCollectionCemetery` at triage.rs:1461,
`CharArraySlack`, `DuplicateStrings`). When the flag is off, these rules return `None` and produce
**no output and no explanation** — the reader cannot distinguish "we looked and found no waste"
from "we didn't look." The rendered sections have the same problem: `render_collections` with
`tracked == 0` emits a header and an empty/degenerate table rather than "collection analysis was
not run (pass `--collections`)." This was flagged narrowly in §26.4b; Pass F confirms it is a
*systemic* empty-state defect, not a one-rule quirk. **Recommend a "not analysed" sentinel:** when
a gated analysis is off, emit one Info triage line ("Collection waste analysis skipped — rerun
with `--collections` to include it") and have the corresponding section print the same, instead of
rendering an empty table. *Data: Have — the `tracked == 0` / `cfr.tracked == 0` discriminator
already distinguishes off-vs-empty at every call site. Effort: Compute-cheap (sentinel signal +
section guard).*

### 31.4 Empty sections still emit headers, captions, and ToC drift

`render_markdown` (render_md.rs:256–277) calls almost every section renderer *unconditionally*.
Each renderer then guards internally: some print a graceful line ("`_No arrays found._`"
render_md.rs:1478; "`No single object or class group exceeds the threshold.`"
render_md.rs:1011; "`_No package retains more than 1% …_`" render_md.rs:1229), but they still
emit the `## Header` and the italic caption first. On a near-empty dump the report becomes a
sequence of ~15 headers each followed by one apologetic sentence — technically correct, but it
buries the *one* thing that matters (the heap is small/healthy) under scaffolding. Worse, the
**ToC is conditional where the body is not**: `render_toc` gates Top Components, Container
Attribution, Fields-by-size, Biggest Collections on `is_some()`/`!is_empty()` (render_md.rs:294–
309), while the body renders headers regardless. On a degenerate dump the ToC and the body
**disagree about which sections exist** — a ToC link may be missing for a section that still
printed an empty header, or (for the always-on sections) the ToC lists a section whose body is a
single "none" line. **Recommend one of:** (a) make body-emission match the ToC's conditional
logic (skip the header entirely when empty), or (b) make the ToC unconditional and accept the
empty bodies — but pick one so they can't drift. Option (a) is better for the healthy-dump reader.
*Data: Have — every renderer already computes its own emptiness. Effort: Format-plumbing (hoist
the empty check above the header push, or share a `section_present()` predicate the way
`retention_concentration_present()` already does at render_md.rs:481).*

### 31.5 `render_oom_triage` has no all-clear state

`render_oom_triage` (render_md.rs:460) blindly iterates `r.triage`; if that vector were ever empty
it would print just "`## Memory Triage`" + caption + blank line — a section that promises a summary
and delivers nothing. In practice HeadlineRetainer+Concentration guarantee ≥2 bullets (§31.2), so
this is latent rather than live — but it means the *design* has no explicit "heap looks healthy,
no signals" terminal state; health is only ever implied by the *absence* of alarming bullets
inside an always-alarming-sounding list. **Recommend an explicit all-clear:** if every fired
signal is `Info` severity (no Warning/Critical), prepend a single green-flag line ("No leak
indicators crossed a threshold — the heap appears healthy.") so the reader gets an affirmative
answer, not an inference. *Data: Have — `TriageSeverity` is on every signal; the max-severity fold
is trivial. Effort: Compute-cheap.* This pairs with §31.2 (both are about the report being able to
say "you're fine") and with §26.9 (the triage-section summary critique).

### 31.6 Divide-by-zero is handled, but zero-total degrades to a silent 0.0%

`pct_of` guards `total == 0 → 0.0` (triage.rs:188), and `render_leak_suspects` repeats the same
guard inline (render_md.rs:1021). So there is no panic risk on an empty heap — good. But the
*consequence* is that on a zero-or-tiny `leaks.total_shallow`, **every percentage in the report
silently reads `0.0%`**, including the headline retainer's "(0.0% of reachable heap)". A reader
seeing "retains 40 KB (0.0% of reachable heap)" may think the percentage is broken rather than
understanding the heap is trivially small. **Recommend:** when `total_shallow` is below a floor
(say 1 MB) or zero, suppress the percentage entirely and print the absolute only ("retains 40 KB;
heap total is 900 KB"), or add a one-line note that percentages are omitted for sub-megabyte heaps.
*Data: Have — `total_shallow` is already the denominator everywhere. Effort: Compute-cheap.*
Cross-ref §27 (numerical rigor): the whole percentage apparatus assumes a non-trivial denominator;
Pass F is the degenerate-denominator corner of that same audit.

### 31.7 Summary of empty-state findings

The tool does not *crash* on empty/tiny heaps — the `return None` and `total == 0` guards are
thorough (31.6). The defect is **communicative, not numerical**: on a healthy or degenerate dump
the report (a) cannot say "you're fine" affirmatively (31.5), (b) leads with an alarming "headline
retainer" even at 0.3% (31.2), (c) can't distinguish "found no waste" from "didn't look" (31.3),
(d) emits empty scaffolding whose ToC and body disagree (31.4), and (e) degrades all percentages to
a confusing 0.0% (31.6) — all while (f) never being tested against a non-pathological sample (31.1).
None of these are missing-data problems; every fix is Compute-cheap or Format-plumbing over data
already on the `Report`. The single highest-leverage action is **31.1: ship a healthy sample dump**,
because it turns every other item here from an invisible latent bug into something a reviewer sees.
Priority: 31.2 (false-alarm headline) and 31.5 (all-clear state) are P2 UX fixes; 31.3 (gated-skip
sentinel) and 31.4 (ToC/body drift) are P2 correctness-of-presentation; 31.1 is a P1 test-coverage
gap enabling all of the above; 31.6 is P3 polish.

## 32. HTML Accessibility & UX Audit (pass 13)

The HTML report (`--format html`) is a self-contained React app (`web/src/App.tsx`, 3,628 lines;
`charts.tsx`; `domTree.tsx`; `styles.css`, 243 lines). It is the flagship output — the one a
developer actually clicks through — so its accessibility and interaction quality matter as much as
the numbers. This pass audits it against the four axes named in the backlog: color-only encodings,
ARIA/keyboard, print/dark-mode, and large-table performance. The good news up front: the
foundations are unusually strong for a generated report. The defects are specific and fixable.

### 32.0 What is already right (so we don't regress it)

Credit where due, because the Priority Summary should not accidentally "fix" these:
- **Skip link** (`styles.css:110` `.skip-link`, revealed on `:focus`) — real keyboard-nav support.
- **Theme toggle with persistence** (App.tsx:41–59) — light/dark/auto, `localStorage`, wrapped in
  `try/catch` for `file://` (App.tsx:48). `aria-label` present (App.tsx:55).
- **Charts have paired data tables** (charts.tsx header comment lines 18–21: "the paired table in
  App.tsx is the accessibility fallback") and `role="img"` on every chart wrapper.
- **Print stylesheet** (`styles.css:177–184`) hides chrome, force-expands `<details>`, avoids
  mid-card breaks — genuinely thought-through.
- **Responsive tables** (`styles.css:186–201`) make wide tables scroll rather than overflow.
- **Sticky ToC with active-section highlight**, back-to-top button with `aria-label` (App.tsx:214).
These are worth protecting; the findings below are additive, not a teardown.

### 32.1 The SVG dominator tree is hardcoded to light mode — unreadable in dark mode (BUG)

`domTree.tsx` fills its node boxes and text with CSS variables that **do not exist**:

> `domTree.tsx:183` `<rect … fill="var(--surface, #f8fafc)" stroke={col} …>`
> `domTree.tsx:188` `<text … fill="var(--text-muted, #64748b)" …>`
> `domTree.tsx:191` `<text … fill="var(--text-muted, #94a3b8)" …>`

`styles.css` defines `--bg`, `--fg`, `--muted`, `--card`, `--border`, `--accent` — but **never
`--surface` or `--text-muted`** (confirmed: those tokens appear only in domTree.tsx across the
whole `web/src/` tree). So the `var(--surface, …)` fallback *always* wins, and every dominator-tree
node renders as a near-white box (`#f8fafc`) with light-grey text (`#64748b`/`#94a3b8`) regardless
of theme. In **dark mode** (`--bg: #16181c`) that is a near-white box on a near-black page — jarring
but legible — while the **grey-on-white text inside** sits at roughly 4.5:1 on white but is being
drawn on the *light* box, so it's fine on the box yet the box itself clashes with the dark page and
breaks the visual system. The real defect is that the one SVG visualization ignores the theme
entirely. **Recommend: replace the phantom tokens with the real ones** — `var(--card)` for the box
fill, `var(--fg)`/`var(--muted)` for text, `var(--border)` for strokes where appropriate. *Data:
Have — the correct variables already exist and are theme-aware. Effort: Format-plumbing (three
string edits in domTree.tsx).* This is the single highest-value HTML fix: a headline visualization
is broken in one of two shipped themes, and no test caught it (cross-ref §31.1 — no rendered-output
review; a dark-mode screenshot diff would have flagged it).

### 32.2 Sortable table headers are mouse-only and announce no sort state

`SortableTh` (App.tsx:432–441) is a `<th class="num sortable" onClick=…>` with a `title` tooltip
and a `▾` glyph on the active column — but **no `tabIndex`, no `role="button"`, no `onKeyDown`, and
no `aria-sort`.** Consequences: (a) a keyboard-only user cannot sort any table — the class
histogram, top-consumers, and diff tables are all mouse-only for their primary interaction; (b) a
screen-reader user gets no announcement of which column is sorted or in which direction (the `▾` is
a bare glyph in the cell text, and there is no `aria-sort="descending"` on the `<th>`). The same
pattern recurs in the diff-table sorter (App.tsx:3395) and the waste/kinds sorter (App.tsx:3027).
**Recommend:** add `tabIndex={0}`, `role="button"`, an `onKeyDown` that fires the sort on Enter/
Space, and `aria-sort={active ? "descending" : "none"}` to `SortableTh` (and the two sibling
sorters). *Data: Have — `active`/`sortKey` already computed. Effort: Compute-cheap (a11y attrs +
one keydown handler on the shared component).* This is a WCAG 2.1 keyboard-operability failure on
the report's single most interactive feature.

### 32.3 Clickable chart slices/bars and treemap tiles have no keyboard path

Several charts are interactive by mouse only:
- `LeakShareChart` pie slices call `onSlice` → scroll to the suspect (charts.tsx:246); the click
  handler is registered on the Chart.js `onClick` (charts.tsx:66) with no keyboard equivalent.
- `TreemapBar` segments (charts.tsx:419) and the legend `<span>`s (charts.tsx:431) use raw `onClick`
  on `<div>`/`<span>` — not focusable, no key handler, no `role`.
- `RetainedTreemap` leaf `<div>`s (charts.tsx:500) expose data only via `title` + `onMouseEnter`
  tooltip — hover-only, invisible to keyboard and touch.
Because the paired *tables* exist (32.0), the underlying data is reachable without these controls,
so this is a **degraded-affordance** issue, not a total block. But the click-to-navigate on the
leak-share pie is a genuine convenience that keyboard users can't reach. **Recommend:** for the
`TreemapBar` segments/legend, wrap the clickable ones as `<button>` (inheriting focus + Enter/Space)
or add `role="button" tabIndex={0} onKeyDown`. For chart canvases, the paired table is the
accepted a11y fallback — leave as-is but ensure the table is always rendered (see 32.5). *Data:
Have. Effort: Compute-cheap for the div/span controls.*

### 32.4 Charts carry only generic `aria-label`s; hover tooltips are the real content

Every chart wrapper is `role="img" aria-label="Pie chart"` / `"Horizontal bar chart"` /
`"Bar chart"` (charts.tsx:89, 142, 199, 365) — a **type name, not a description**. A screen reader
announces "Pie chart, image" and nothing about what it shows or its top value. The actual data
lives in Chart.js `tooltip.callbacks.label` (e.g. charts.tsx:78–84) which only fire on hover — so
the richest labeling is mouse-only. Since the paired table is the fallback, the chart's own label
is redundant *if* the table is adjacent and associated — but nothing ties them together (no
`aria-describedby` from the chart to the table, no `<figure>`/`<figcaption>`). **Recommend:** make
each `aria-label` say what the chart depicts and its headline (e.g. `"Heap composition by kind;
largest: byte[] at 41%"`, derivable from the same `data` array), and/or wrap chart+table in a
`<figure>` with a `<figcaption>`. *Data: Compute-cheap — the top slice/label is already in the
`data`/`titles` arrays passed to each chart.*

### 32.5 Per-cell copy buttons on a 500-row histogram: focus-order and DOM-weight cost

The class histogram caps at `CAP = 500` visible rows (App.tsx:473) and each class cell carries a
`CopyButton` (App.tsx:566–570) that is `visibility: hidden` until row hover/focus
(`styles.css:172–174`). Two issues: (a) **performance/DOM weight** — 500 rows × (copy button +
resize-aware cells) is a heavy subtree; combined with `useColumnResize` (App.tsx:477) and per-row
`useMemo`-free mapping, expanding to 500 rows can jank on lower-end machines. (b) **focus order** —
the copy buttons are in the tab order (they're `<button>`s) even while `visibility:hidden`? No —
`visibility:hidden` does remove them from tab order, which is correct; but it also means a
keyboard user can *never* reach the copy button (it only becomes visible on `:hover`, and
`td:hover` is mouse-only; the `.copy-btn:focus` rule at styles.css:173 can never trigger because
focus can't land on a `visibility:hidden` element). So **the copy affordance is entirely
mouse-only.** **Recommend:** keep the button in the tab order (use opacity/off-screen rather than
`visibility:hidden`, or reveal on `:focus-within` of the cell) so keyboard users can copy class
names; and consider virtualizing or lowering the 500-row cap (cross-ref §26/§28 on cap honesty).
*Data: Have. Effort: Compute-cheap (CSS reveal strategy) + optional Add (virtualization).*

### 32.6 Color-only encoding in charts; palette not colorblind-checked

The chart palette (`charts.tsx:23–36`, 12 hues) and the dom-tree palette (`domTree.tsx:19–22`, 8
hues) distinguish categories **by color alone** — pie slices, stacked-bar segments, treemap
top-level packages, and dom-tree nodes all rely on hue with no secondary encoding (pattern, label,
or direct on-slice text). The pie/bar legends provide text, so the pie is recoverable; but the
`RetainedTreemap` colors leaves by top-level package (charts.tsx:472–480) with **no legend** — the
only per-tile identification is the hover `title`, so a colorblind or keyboard user cannot map
color→package at all. Additionally neither palette has been checked for colorblind-safe contrast
(the red `#dc2626` / green `#16a34a` pairing at index 1/3 is a classic deuteranopia collision).
**Recommend:** add a small legend to `RetainedTreemap` (top-level package → color, the map already
exists at charts.tsx:473 `topLevelNames`), and either adopt a colorblind-safe palette or add
direct labels so hue is never the sole channel. *Data: Have — `topLevelNames` map already built.
Effort: Compute-cheap (legend) / Format-plumbing (palette swap).*

### 32.7 Minor UX nits (grouped)

- **Theme toggle `aria-label` states the current mode, not the action** — `aria-label={"Theme: " +
  mode}` (App.tsx:55) announces "Theme: dark" on a button that *switches to* the next mode; it
  should announce the action ("Switch to light theme"). *Compute-cheap.*
- **`title` tooltips as primary info** — exact byte counts hang off `title=` in many cells
  (App.tsx:1569, 1753, 1790, 3130); `title` is mouse-hover-only and unreliable on touch. Where the
  exact value matters (MAT-style `509,972,304 (41.08%)`), render it as visible `.mat-exact` text
  (the class exists, styles.css:148) rather than a tooltip. *Format-plumbing.*
- **No `lang` / document-level landmarks audit performed here** — worth a follow-up (is the root
  `<main>`/`<nav>` structured with landmarks so the skip-link target and ToC are real regions?).
  The `#triage` block uses `tabIndex={-1}` (App.tsx:241) as a focus target, which is good; verify
  the skip-link points at a labeled `<main>`. *Compute-cheap to verify.*

### 32.8 Summary of HTML a11y/UX findings

The HTML report's a11y *foundation* is strong (skip link, print CSS, dark mode, paired data tables,
responsive tables — 32.0), which makes the gaps stand out as fixable oversights rather than
architectural debt. The one true **bug** is 32.1 — the dominator-tree SVG references phantom CSS
variables and is stuck in light-mode colors, breaking the flagship visualization in dark mode; it
is a three-line fix and the top HTML priority. The **keyboard-operability** cluster (32.2 sortable
headers, 32.3 clickable charts/treemap, 32.5 copy buttons) is a coherent WCAG gap: the report is
richly interactive by mouse and substantially inert by keyboard — all fixable with `tabIndex`/
`role`/`onKeyDown`/`aria-sort` on shared components. The **screen-reader** gap (32.4 generic chart
labels) and **colorblind** gap (32.6 legend-less treemap, unchecked palette) are lower-severity
because the paired tables provide a data fallback. Every fix here is Have or Compute-cheap — no new
heap data required; this is entirely a front-end polish pass. Priority: 32.1 is **P1** (broken
feature in a shipped theme); 32.2 and 32.5-keyboard are **P2** (WCAG keyboard failures on the main
interaction); 32.3/32.4/32.6 are **P3** (degraded but table-backed); 32.7 nits are **P3**.

## 33. Actionability Audit — "What Do I Do About It?" (pass 14)

Every prior pass judged the report on *correctness* (are the numbers right?) and *legibility* (can
I read them?). This pass asks the question a developer actually has after reading it: **"OK — so
what do I change in my code?"** A heap analyzer that says "byte[] retains 41% of the heap" has
told you *where* the memory is; it has not told you what to *do*. The tool's whole value
proposition (backlog: "help developers discover problems, find where heap comes from, find where
heap is wasted") is only half-delivered if it stops at diagnosis and never reaches prescription.

The finding of this pass is a **structural split**: the *triage bullets* frequently carry a
remediation verb, but the *section bodies* almost never do. That split is arbitrary — the reader
who scrolls to a section gets description; the reader who reads the triage summary gets advice —
and it means the tool's actionability depends on *which entry point* you happen to use.

### 33.1 Where actionability already exists (the good half)

A meaningful minority of triage rules end in a concrete "do this" clause, grounded in triage.rs:
- **BoxedPrimitiveBloat** (triage.rs:762): "…consider primitive-specialized collections (e.g.
  Eclipse Collections, Koloboke)." — names *actual libraries*. Excellent.
- **FixedPerObjectOverhead** (triage.rs:1403): "…consider value types, primitive arrays, or fewer
  wrapper objects."
- **EmptyCollectionCemetery** (triage.rs:1475): "…consider lazy initialisation or null."
- **OversizedPrimArray** (triage.rs:1511): "…consider chunking, memory-mapping, or off-heap storage."
- **DuplicatePrimArrays** (triage.rs:1545) / **DuplicateStrings**: "…could be deduplicated or
  replaced with a shared constant" / "interned."
- **Concentration** (triage.rs:303): "…so freeing it would reclaim most memory." — states the payoff.

And **Retention Concentration**'s section prose (render_md.rs:496–503) is a genuine *decision tree*:
"if **Top 1** is already high, one object is the leak and freeing it reclaims most of the heap; if
the share only climbs as you widen to **Top 10** / **Top 100**, the leak is spread across many
peers … and no single free helps much." That is the gold standard — it tells the reader how to
*interpret and act on* the number. **The rest of the report should aspire to this paragraph.**

### 33.2 The section bodies are diagnostic-only — they describe, they don't prescribe

Contrast the above with the actual captions of the "where the heap is" sections (all quoted from
render_md.rs):
- **System Overview** (render_md.rs:678): "_Reachable-heap totals and the largest classes by
  retained heap._" — pure description.
- **Top Consumers** (render_md.rs:1184): "_Classes whose instances together retain the most heap._"
- **Collections** (render_md.rs:1736): "_Collection and array occupancy: how full collections are,
  how big they get, and constant primitive arrays._"
- **Container Attribution** (render_md.rs:2132): "_Which holder `Class#field` points at the most
  container memory…_"
- **Big Drops** (render_md.rs:2790): "_Dominators where retained heap does not flow into a single
  child … A large drop means this object directly owns a lot of memory spread across many children
  (e.g. an array or collection)._"
- **References** (render_md.rs:2574): "_Soft/weak/phantom reference referents (what they point at)._"

Each caption competently explains *what the table is*. **None tells the reader what a bad value
looks like or what to change.** Container Attribution is the sharpest example: it is arguably the
single most actionable section in the tool — it names the exact `Class#field` holding the memory,
which is *precisely* the code location a developer would edit — yet the caption never says "this is
the field to null out / bound / evict." The data does the reader's most valuable work and the prose
leaves it on the table.

### 33.3 A "Recommended action" angle, per section (proposed prose)

Below is a concrete, per-section action clause, each derivable from data already on the `Report` (no
new heap pass). The recommendation is to append a short **"What to do:"** sentence to each section
caption (or, for the HTML, a small callout), mirroring the Retention Concentration decision-tree
tone. All are **Compute-cheap or Format-plumbing** — prose over existing fields.

| Section | Proposed "What to do" clause | Data |
|---|---|---|
| **Top Consumers** | "Start with the top row: if one class dominates, look at *who* retains its instances (Container Attribution) rather than the class itself." | Have (cross-link) |
| **Container Attribution** | "The `Class#field` here is the code location holding the memory — the field to bound, evict, or null. Fix the top row first." | Have |
| **Fields by Retained Size** | "A field retaining far more than its siblings is an unbounded accumulator; add a size cap or eviction policy on that field." | Have |
| **Collections (fill ratio)** | "Collections far below 100% full are over-allocated — right-size the initial capacity or use a growable default." | Have (`fill_ratio`) |
| **Biggest Collections** | "A single collection holding a large share is an unbounded cache or queue; cap it or make it weak/evicting." | Have |
| **Big Drops** | "A big drop = one object directly owns memory across many small children (an array/map). To shrink it, reduce the *number* of children, not their size." | Have |
| **Arrays by Size / oversized prim arrays** | "Huge single arrays resist GC and fragment the heap; chunk them, memory-map, or move off-heap." | Have (mirror the triage verb) |
| **Duplicate Strings / Prim Arrays** | "Identical payloads can be interned or replaced with a shared constant — reclaim is the reported wasted bytes." | Have |
| **Unreachable Objects** | "Large unreachable heap that hasn't been collected suggests GC pressure or a dump taken mid-collection; not a leak, but a tuning signal." | Have |
| **References (soft/weak/phantom)** | "Objects reachable only softly/weakly are reclaimable under pressure — if these are large, the heap is more elastic than the totals suggest." | Have |
| **Threads** | "A thread retaining a large share pins that memory for its lifetime; check its thread-locals and stack roots for accidental retention." | Have |

None of these invents data — each restates a field the section already prints, in imperative mood.

### 33.4 The Summary/Triage split should be reconciled around a single "action" column

Because actionability lives in the triage bullets (33.1) but not the sections (33.2), the reader's
takeaway depends on entry point. Two ways to close the gap, either acceptable:
- **(a) Push actions down into sections** — add the 33.3 clauses so a reader who lands on a section
  from the ToC gets advice without bouncing back to Triage.
- **(b) Make Triage the single action hub** — ensure every section with a "what to do" also fires a
  triage bullet carrying that verb, and have the section caption link *up* to it ("See Memory
  Triage for the recommended action"). This centralizes advice but forces a round-trip.
Option (a) is better for the scroller; (b) is better for de-duplication (cross-ref §26.2 on ranking
triage signals, and §1.1 on Summary/Triage duplication). **Recommend (a)** — the sections are where
the reader is looking at the offending number, so that is where the fix belongs. *Data: Have.
Effort: Format-plumbing.*

### 33.5 Actionability gaps that need more than prose (the honest exceptions)

Three sections *cannot* be made fully actionable with current data, and saying so is more honest
than a vague clause:
- **Leak Suspects / dominator path** — the action is "null the reference that keeps this alive," but
  the tool cannot name that reference without **field-labeled reference paths** (the true Add gap
  from §30.2a). Until then the best action clause is "follow the dominator chain to the nearest
  object you own, and drop the reference into it." *Data: Add (field-labeled paths).*
- **Allocation Sites** — actionability here means "go to this stack frame and allocate less," but
  the sample shows allocation-site data is frequently absent (§14.1); the action clause must be
  gated on presence. *Data: Have (gate) / Add (richer stacks).*
- **Header Overhead** — the triage verb exists ("value types, primitive arrays"), but value types
  (Project Valhalla) aren't shippable today; the realistic action is "reduce object *count*," which
  the clause should say plainly rather than pointing at unavailable language features. *Compute-cheap
  (reword).*

### 33.6 Summary of actionability findings

The tool is **half-prescriptive**: ~10 of 39 triage rules and the Retention Concentration paragraph
tell the reader what to change; the section bodies — including Container Attribution, the single
most code-actionable view in the whole report — stop at description. This is not a data problem
(33.3 shows every action clause is derivable from fields already printed) but a **prose-and-policy
gap**: the report diagnoses well and prescribes inconsistently, and *which* half the reader gets
depends on whether they read Triage or scroll to a section. The fix is to push the action clauses
down into the section captions (33.4 option a), copying the imperative, payoff-stating tone that
Retention Concentration already models. The honest exceptions (33.5) — leak-suspect paths, alloc
sites, header overhead — should state their action *and its current limit* rather than a hollow
"consider." Priority: 33.3+33.4 (per-section action clauses) is a **P1** actionability uplift that
touches the tool's core value prop and is pure Format-plumbing; 33.5 (honest-limit wording on the
three exceptions) is **P2**; it ties directly to §30.2a (field-labeled paths) as the one Add that
would unlock Leak-Suspect actionability.

## 34. New Analyses Worth Adding (pass 15)

Every prior pass critiqued what the report *does*. This pass asks what it *doesn't* do yet —
analyses that would help a developer discover a problem the current report can't surface. The
discipline here is to (a) verify the analysis genuinely does not already exist (the report has 40+
model structs and 39 triage rules, so "add classloader-leak detection" would be a duplicate — it's
already `ClassloaderLeak` at triage.rs, backed by `DuplicateClass` at model.rs:235), and (b) state
honestly whether the data is already scanned (Compute-cheap) or needs a new heap pass (Add).

### 34.0 Backlog items that ALREADY EXIST — do not re-add (duplication guard)

Grounding this first so the Priority Summary doesn't propose existing features:
- **Retained-size deltas across dumps** — EXISTS as `compare reports` (diff_reports.rs:59
  `SeriesClassRow.delta_retained`, `delta_instances`; SeriesSuspectRow; SeriesDiffResult). The
  N-way snapshot diff is a shipped strength (§30.3). *Not new.*
- **Classloader-leak detection** — EXISTS: `ClassloaderLeak` + `ClassloaderExplosion` triage rules
  over `DuplicateClass` (model.rs:235, `loader_count ≥ 2`). *Not new.*
- **Finalizer queue** — EXISTS: `FinalizerQueueBacklog` (triage.rs:934) reads the histogram for
  `java.lang.ref.Finalizer`. *Not new as a signal* — but see 34.4 (it has no dedicated section).
- **Duplicate strings** — EXISTS: `DupStrings` (model.rs:408), rendered as Duplicate Strings with
  top-by-count and top-by-length. *Not new* — but see 34.1 (value-dup ≠ table pressure).
- **Per-package rollup** — PARTIALLY exists: `PackageNode` (model.rs:602) rolls up *retained heap*
  by package. *But it rolls up retention, not waste* — see 34.2.

So of the backlog's suggestions, the genuinely-absent ones are: **string-table pressure**,
**per-package waste**, **megamorphic/duplicated-shape detection**, and a few new ones below.

### 34.1 String-table pressure (distinct from duplicate-string values) — Add

Duplicate Strings answers "which string *values* repeat." It does **not** answer "how much heap is
the `char[]`/`byte[]` backing store of `java.lang.String` costing, and how much is *slack*." Since
JDK 9, `String` holds a `byte[] value` + `byte coder`; a Latin-1 string stored as UTF-16, or a
substring sharing an oversized backing array, wastes bytes that duplicate-*value* analysis misses
entirely (two strings can be non-duplicate yet both individually wasteful). The report already
walks String instances (DupStrings computes value hashes), so the *iteration* exists — what's
missing is summing `coder`-mispredicted bytes and backing-array slack. **Recommend a "String
Storage" mini-section:** total String count, total backing-array bytes, bytes attributable to
UTF-16 storage of pure-Latin-1 content (compressible), and mean chars-per-String. *Data: Add — the
`coder`/`value.length` fields are read during the dup-string pass but not aggregated for slack;
needs one more accumulator in that existing pass. Effort: Add (small — piggybacks on the pass that
already visits every String).* This directly serves "find where heap is wasted" for the single most
common heap-dominating type (`byte[]`/`char[]` are almost always #1, per every sample).

### 34.2 Per-package WASTE rollup (not per-package retention) — Compute-cheap

`PackageNode` (model.rs:602–613) sums `retained_heap` per package — that answers "where is the heap"
by ownership. It does **not** answer "where is the heap *wasted* by package." The report computes,
per class, several waste quantities: empty collection slots (`FieldAttributionRow.total_wasted_slots`
at model.rs:1017), header overhead (`HeaderOverheadRow.total_header_bytes` at model.rs:277), and
constant/duplicate arrays. **None of these is rolled up by package.** A developer who owns
`com.acme.orders.*` cannot currently ask "how much of the waste is *mine* vs a library's." **Recommend
a "Waste by Package" rollup:** the same tree shape as PackageNode but summing the waste metrics
instead of retained heap, so a team can see their package's reclaimable bytes. *Data: Compute-cheap
— every waste quantity is already computed per class; this is a group-by over `package_path(class)`
(the helper already exists, mod.rs:104) at render time.* This is the "per-package waste" the backlog
asked for, and unlike 34.1 it needs no new scan — pure aggregation of data on the `Report`.

### 34.3 Duplicated object *shape* / structural dedup — Add

Duplicate Strings and DuplicatePrimArrays find byte-identical *leaf* payloads. There is no analysis
of duplicated *object graphs* — e.g. 10,000 structurally-identical small config objects, or repeated
identical boxed-key→boxed-value map entries — which is a common source of bloat that neither the
histogram (counts instances, not equivalence classes) nor dup-strings (leaf only) catches. MAT has
no equivalent either (§30 — this would *exceed* MAT). **Recommend (longer-term):** a value-equality
pass that hashes small immutable object graphs and reports the top duplicated shapes with reclaim
estimate. *Data: Add (significant — needs recursive structural hashing during scan). Effort: Add
(large).* Flagged as a *direction*, not a near-term item — honest about cost.

### 34.4 Finalizer / reference-queue as a first-class section — Compute-cheap

`FinalizerQueueBacklog` fires a triage bullet (34.0) but there is **no section** enumerating what is
stuck: which classes are pending finalization, their retained bytes, and whether the finalizer
thread is alive. The `references` analysis (model.rs:1199 `ReferencesAnalysis`) already tallies
soft/weak/phantom referents by class — a Finalizer/ReferenceQueue view is the same shape over
`java.lang.ref.Finalizer` referents. **Recommend folding a "Pending Finalization" subsection into
References**, listing the referent classes and retained bytes so the reader sees *what* is stuck,
not just *that* something is. *Data: Compute-cheap — the histogram + reference-referent machinery
already exists; this is a filtered view.* Serves "discover problems" for the classic
`Deflater`/JDBC-connection finalizer leak the triage bullet already names (triage.rs:950).

### 34.5 Megamorphic / large-fan-in hub objects — Compute-cheap (data already in dominators)

The backlog's "megamorphic call sites" isn't recoverable from a heap dump (that's a JIT/profiling
concept, no bytecode in an hprof) — but its *heap analog* is: **hub objects with very high
in-degree** (one object referenced by thousands of others, or one object dominating thousands of
distinct child classes). `ImmediateDominators` (model.rs:800, `ImmediateDominatorRow`) already
computes dominator fan-out; `BigDrops` (model.rs:768) already finds objects whose retained heap
spreads across many children. What's missing is the *inverse* framing: "these N objects are each
pointed at by a huge number of references" — a shared singleton, an interner, a giant static map —
which is where retention *concentrates structurally*. **Recommend a "Reference Hubs" view** derived
from the in-degree the graph already has. *Data: Compute-cheap if in-degree is retained from the
scan; Add if it must be recomputed.* Note honestly: this overlaps `BigDrops` (§33.3 already gives
Big Drops an action clause) — before adding, confirm it's not just Big Drops re-skinned (avoid the
duplication this whole document fights).

### 34.6 Summary of proposed new analyses

Prioritized by value-per-effort, grounded in what the scan already collects:
- **34.2 Per-package waste rollup** — highest value-per-effort: it's **Compute-cheap** (group-by over
  waste metrics already on the `Report`), directly answers "is the waste mine or a library's," and
  reuses the existing `package_path` helper. **P2.**
- **34.1 String-storage / table pressure** — high value (targets the perennial #1 type) at small
  **Add** cost (piggybacks the dup-string pass). **P2.**
- **34.4 Pending-finalization section** — **Compute-cheap** enrichment of an existing triage signal
  into a "what's stuck" view. **P3.**
- **34.5 Reference hubs** — **Compute-cheap** *if* in-degree survives the scan, but must be checked
  against Big-Drops overlap first. **P3, gated on de-dup check.**
- **34.3 Structural object-graph dedup** — genuinely new capability that would exceed MAT, but a
  **large Add**; flagged as direction, not near-term. **P3 (research).**
The unifying observation: three of the five proposals are Compute-cheap because the scan already
visits every object and every field for the *existing* analyses — the missing piece is almost always
a *different aggregation* of data already in hand, not a new heap pass. That mirrors the central
thesis of this whole document (§21 preamble): the tool's data model is rich; its gaps are
overwhelmingly in *how* the data is grouped and presented.

## 35. Cross-Section Consistency Pass (pass 16)

This document grew from an initial 25 sections to 34 across fifteen deep-dive passes. Fifteen
independent passes, written on different nights against different parts of the code, inevitably
produce **overlaps, near-duplicates, and a couple of outright tensions**. This capstone pass does
what no single earlier pass could: read the whole document as one artifact, reconcile §24 (Waste)
and §25 (Origin) against everything written after them (§26–34), catch contradictions, and confirm
the Priority Summary is internally coherent. The goal is that a reader implementing from this
document never finds two sections telling them to do incompatible things.

### 35.1 Reconciling §34.1 (String Storage) with §24.1 (waste table) — partial overlap, now bounded

§24.1 already lists **String backing-array slack** (`DupStrings.char_array_waste.total_wasted_bytes`,
verified at pass2/strings.rs:311) as one of the nine waste sources. §34.1 then proposed a new
"String Storage" section. **These overlap and must not become two separate features.** The honest
reconciliation: `char_array_waste` covers *backing-array slack* (a substring holding an oversized
array) — that is already computed and belongs in the §24 Waste Summary. What §34.1 adds *beyond*
that is **coder mis-storage** (Latin-1 content stored as UTF-16), which `char_array_waste` does
*not* measure. So the correct scope is: **§34.1 is not a new section — it is one additional
accumulator (UTF-16-vs-Latin1 bytes) folded into the existing char-array-waste computation, and its
output is a tenth row in the §24 waste table, not a standalone section.** This avoids the exact
duplication the whole document fights. *Net: demote §34.1 from "new section" to "new waste-table
row + one accumulator"; it inherits §24's placement.*

### 35.2 Reconciling §34.2 (Waste-by-Package) with §24 (global Waste Summary) — complementary, not duplicate

§24 proposes ONE global headline ("reclaimable ~N MB"). §34.2 proposes the SAME waste metrics rolled
up **by package**. A careless reader could see these as competing. They are not: §24 answers "how
much total waste," §34.2 answers "whose waste." The consistent framing is a **hierarchy**: the §24
Waste Summary is the headline and the flat 9-(now-10-)row table; §34.2 Waste-by-Package is a
*drill-down within* the Waste Summary section (the same relationship §25.3's Heap-Origin spine has to
the per-axis detail tables). **Recommend §34.2 live as a sub-view under the §24 Waste Summary, not as
an independent top-level section.** Both use the identical per-class waste fields; §34.2 just groups
by `package_path` where §24 sums globally. *Net: §34.2 is the by-package projection of the §24
table; place it inside Waste Summary.*

### 35.3 Reconciling §34.5 (Reference Hubs) with §25.4 (`dominated_retained`) and Big Drops — one Add unlocks both

§34.5 (Reference Hubs) and §25.4 (add `dominated_retained` to `ImmediateDominatorRow`) and the
existing Big Drops section are **three views of the same underlying structure**: objects that
concentrate retention across many children/referrers. §34.5 flagged the Big-Drops overlap itself;
this pass adds the missing link: **§25.4's `dominated_retained` Add is the prerequisite that makes
both the Origin-spine hub axis (25.3) and any Reference-Hubs view (34.5) apples-to-apples with the
rest of the report.** Currently `ImmediateDominatorRow` is shallow-denominated (model.rs:807,809 —
`dominator_shallow`/`dominated_shallow`, verified), so a "hub" view undersells retention. **Recommend
treating §25.4 as the single enabling Add, and building at most ONE hub view on top of it** (extend
Immediate Dominators with the retained column rather than adding a separate Reference Hubs section) —
otherwise Big Drops, Immediate Dominators, and Reference Hubs become three tables answering the same
question in three units. *Net: §34.5 should not ship as a new section; fold its intent into
Immediate Dominators once §25.4's `dominated_retained` exists.*

### 35.4 Reconciling §33 (per-section action clauses) with §24.4 / §25.3 (the two spines)

§33.4 recommended pushing "what to do" clauses *down into* section captions (option a). §24.4 and
§25.3 recommend two *lead-in spines* (Waste Summary, Heap Origin) that sit near the top and link
down. Do these conflict? **No — they are the two ends of the same drill-path and should cross-link:**
the spine says "waste is ~N MB, concentrated in package P (§34.2)"; the reader clicks down to the
section; the section's §33 action clause says "right-size this field / intern these strings." The
consistency requirement is that the **spine states the number and the section states the action** —
they must not both try to do both (that would re-introduce the Summary/Triage duplication flagged in
§1.1). *Net: no contradiction; make it explicit that spines carry numbers+links, sections carry
actions. This is a de-dup contract, not a new item.*

### 35.5 Contradiction check — the "% Heap" relabel (§27.1) vs everything that quotes a %

§27.1 identified that retained-share "% Heap" columns divide retained-numerator by shallow-total
(category error). This is a **P0 relabel**. The consistency risk: *many other sections quote
percentages* — §31.6 (sub-MB heaps degrade to 0.0%), §15/§25 (retention concentration bp), §33.3
(action clauses that mention "top row"). **Verified no contradiction:** §27.1 is about the *label
and denominator*, §31.6 is about the *degenerate-denominator corner*, and the bp-based concentration
percentages (§25, RetentionSummary) use a *different, internally-consistent* denominator
(`total_shallow` for bp→bytes at render_md.rs:505). The one thing Pass J must assert: **when §27.1's
relabel lands ("reachable heap" defined once), §31.6's sub-MB percentage-suppression and §25's bp
displays must adopt the same defined term** — otherwise the report will define "reachable heap" in
one place and silently use a different base in another. *Net: §27.1 is the canonical definition; §31.6
and §25 must reference it, not re-derive. Add this as an implementation note on the §27.1 P0 item.*

### 35.6 Duplication audit across the fifteen passes (the "said it twice" list)

Items that multiple passes independently raised — consolidate to ONE Priority-Summary row each:
- **Gated-rule "not analyzed" note** — raised in §26.4b (P1) *and* §31.3 (as a systemic empty-state
  defect). Same fix. **Keep the §26.4b P1 row; §31.3 is corroboration, not a second item.** (Already
  handled — §31.3 explicitly says it "reinforces rather than duplicates.")
- **Cap honesty** — §27.6 (cap-honest labels), §28/§26 (500-row cap), §32.5 (500-row DOM weight) all
  touch table capping. Distinct angles (label vs perf vs a11y) — keep separate but note they share a
  root (the CAP=500 constant, App.tsx:473).
- **Dominator subtree bloat** — §28.1 (43% plain), §29.1 (72% graphs), §3.1/§13.3 (collapse repeated
  chains). The P0 "Cap the dominator subtree" row already consolidates §28.1+§29.1; the older §3.1/
  §13.3 P0 row is the *same fix* — **merge those two P0 rows into one.**
- **Chart parity md↔HTML** — §19.1/§19.5 (P1) and §29.4/§20.1 (P3 back-port). Related but different
  directions (add to md vs port specific charts); keep separate.

### 35.7 Field-name accuracy audit (stale-citation check)

Because the document cites exact model fields, a Pass J duty is to confirm none have rotted. Verified
this pass against source: `DupStrings.approx_wasted_bytes` (pass2/strings.rs:336,456),
`char_array_waste`/`CharArrayWaste` (strings.rs:284,311), `DupPrimArrays.total_wasted_bytes`
(dup_prim_arrays.rs:60–62), `HeaderOverheadRow.total_header_bytes` (model.rs:277),
`FieldAttributionRow.total_wasted_slots` (model.rs:1017), `ImmediateDominatorRow.dominated_shallow`
(model.rs:809), `SystemOverview.unreachable_shallow` (model.rs:344), `PackageNode.retained_heap`
(model.rs:610) — **all present and correctly named.** The §24.1 and §25.1 tables are accurate as
written. One note: §24.1 lists `FillRatioBucket.wasted` — the struct is `FillRatioBucket` at
model.rs:837 but the `.wasted` field name should be re-verified against that struct's body before
implementation (not confirmed field-by-field this pass). *Net: tables are trustworthy; flag the one
unverified field name.*

### 35.8 Priority Summary coherence — the recommended consolidations

After this reconciliation, the Priority Summary should absorb these edits (all bookkeeping, no new
findings):
1. **Merge** the two P0 dominator-subtree rows (§28.1/§29.1 and §3.1/§13.3) into one — same fix.
2. **Demote §34.1** from "new String Storage section" to "one accumulator + a row in the §24 Waste
   table" (35.1) — it was listed P2 as a section; it's actually part of the P0 Waste Summary.
3. **Relabel §34.2** as "by-package drill-down *within* Waste Summary" not a standalone section (35.2).
4. **Fold §34.5** into the §25.4 `dominated_retained` Add + Immediate Dominators, drop it as an
   independent P3 (35.3).
5. **Annotate the §27.1 P0** with the "define reachable-heap once; §31.6 and §25 must reference it"
   note (35.5).
None of these change *what* to build — they remove the double-counting of *items* so the roadmap
reflects the real, de-duplicated work. This is the same discipline the document asks of the tool
(§1.1, §24.4, §25.3): say each thing once, in one place, and link rather than repeat.

### 35.9 Summary — the document is internally consistent after four reconciliations

Reading all fifteen passes as one artifact surfaced **no hard contradictions** — the tensions are
all *overlaps* where a later "new analysis" (Pass I) restated, in different words, a projection of an
earlier consolidation (§24/§25). Four reconciliations resolve them: §34.1→§24 waste row (35.1),
§34.2→§24 by-package sub-view (35.2), §34.5→§25.4 hub view (35.3), and the spine/section de-dup
contract (35.4). The one genuine implementation-coupling to record is §35.5: §27.1's "reachable heap"
definition is canonical and §31.6/§25 must reference it. Field citations are accurate (35.7, one field
to re-verify). Net: the roadmap is coherent; the §35.8 consolidations are pure bookkeeping that make
the Priority Summary count *real* distinct work rather than fifteen passes' worth of restatements.
The through-line across all sixteen passes holds unbroken: **this tool's data model is rich and its
gaps are overwhelmingly presentation, grouping, and prose — not missing measurement.**

## 36. JSON Output Format & Schema Audit (pass 17)

Every prior pass (A–J) audited the *human* outputs — Markdown, graphs, HTML — or the analyses behind
them. None examined the fourth format, `--format json`, as a **consumer contract**: is it stable, is it
documented, does it expose everything the human formats do, and can a downstream tool depend on it? The
answer is more mature than expected, with a few real sharp edges. This pass grounds every claim in
`src/report/model.rs`, `src/main.rs`, and the JSON sample.

### 36.0 What's already right — this is a real, versioned contract, not a debug dump

Three facts, verified in source, put the JSON well above the usual "we serialized our internal struct"
bar:

1. **A machine-readable JSON Schema is published.** `hprof-analyzer dev emit-schema` (src/main.rs:407)
   runs `schemars::schema_for!(report::Report)` and prints a full JSON Schema. Every model type derives
   `schemars::JsonSchema` (e.g. `Report` at model.rs:1286, `AllocSite`, `AllocSites`, `TriageSignal`).
   A consumer can codegen types or validate payloads without reverse-engineering the sample.
2. **The version is explicit and gated on read-back.** `SCHEMA_VERSION: u32 = 6` (model.rs:1262) is
   emitted as `schema_version` (the first key in the sample) and *enforced* on the read path: `render`
   from a JSON file refuses to proceed unless `report.schema_version == SCHEMA_VERSION` (src/main.rs:709–718),
   with the actionable error "report schema_version N does not match supported version M; refusing to
   render." No silent misparse of a stale payload.
3. **The JSON round-trips.** The tool reads its own JSON back (`serde_json::from_str::<Report>`,
   src/main.rs:703) and can re-render it to md/graphs/html/json (main.rs:719–724). `analyze → json` then
   `render` is a supported, tested pipeline — the JSON is a *lossless* intermediate, not a lossy view.

That combination — schema + version gate + round-trip — is the definition of a stable contract. Say so
in the docs; right now it is invisible to a reader who only sees `--format json`.

### 36.1 The version gate is *exact-match*, which is stricter than the schema-evolution machinery implies

The model is built for **forward/backward tolerance**: 50 `#[serde(default)]` and 24
`skip_serializing_if` attributes (counted in model.rs), plus comments like `top_components` "Additive;
defaults to empty for round-trip with older JSON" (model.rs:1294). That machinery only pays off if a
*newer* binary can read an *older* payload (missing additive fields default in). But the gate at
main.rs:709 is `!=`, not `<`: a v6 binary refuses a v5 JSON outright, even though every v5→v6 delta was
additive and `#[serde(default)]` would fill it. **The `default` attributes are load-bearing for nothing
on the read path** as long as the gate is exact-match — they only help *within* a single version.
Reason to reconcile: either (a) relax the gate to `report.schema_version <= SCHEMA_VERSION` and let
`default` do its job (the design intent), or (b) keep exact-match and document that `default` exists
purely for intra-version robustness, not cross-version migration. Right now the code sends two
contradictory signals. *Effort: Format-plumbing (one comparison) + a doc line. Have.*

### 36.2 `skip_serializing_if` creates absent-vs-empty ambiguity a consumer must be told about

24 fields drop out of the JSON when empty/None. Two distinct shapes result and they mean different
things:
- `alloc_sites: Option<AllocSites>` with `skip_serializing_if = "Option::is_none"` (model.rs:1297):
  **absent key** = the dump had no allocation stack traces at all. But note the deliberate contrast —
  when traces *are* present but empty, the type carries an explicit `traces_present: bool` +
  empty `sites` (model.rs:1276–1280, "reported honestly rather than faked"). So a consumer sees
  *absent `alloc_sites`* and *present `alloc_sites` with `traces_present:false`* as two different
  states, and must know that the first also means "no tracking."
- `TriageSignal.anchor` / `anchor_label` skip-if-None (model.rs:1253/1256): absent = the signal links
  nowhere. Harmless, but a strict schema validator that expects the keys will trip.

The `schemars` schema marks these correctly as optional, so a schema-driven consumer is fine. A
consumer reading the *sample* by eye is not — they will hardcode keys that legitimately vanish on other
dumps. Reason: this is the single most likely integration bug. *Fix: one paragraph in the JSON docs
enumerating which keys are omittable and what absence means (esp. `alloc_sites` = no tracking). Have.*

### 36.3 Does the JSON expose everything the human formats do? — yes for data, no for derivation

The sample carries **18 top-level keys**: `schema_version, generated, overview, leaks, top, threads,
top_components, alloc_sites, arrays_by_size, dominator_analysis, collections, references,
collection_attribution, fields_by_size, biggest_collections, collection_contents, leak_indicators,
triage`. Cross-checked against the Markdown section list (§4-style renderers in render_md.rs): every
rendered section is backed by one of these keys — there is **no human-only analysis** hiding in the
renderer. Even `triage` (the 39-rule OOM signals) is fully in the JSON, so a consumer gets the *same
conclusions*, not just raw numbers. That is the right design and worth stating as a guarantee.

The one asymmetry: the Markdown/HTML renderers compute *derived* framings at render time that never land
in the JSON — the Retention Concentration decision-tree caption (render_md.rs:496–503), the per-section
"what this means" prose, the ASCII/Chart.js visualizations. A JSON consumer gets all the *data* and all
the *triage verdicts* but must re-derive presentation. That is correct (JSON should be data, not prose),
but it means "the JSON is complete" is true for *facts* and false for *narrative* — worth saying
precisely so nobody expects the concentration decision-tree text in the payload.

### 36.4 Field-order stability is relied upon but only informally guaranteed

Two comments (html.rs:50, main.rs:986–990) note that "serde_json preserves field declaration order and
the model carries only sorted [aggregates]," and the HTML embed depends on it for deterministic output.
For a JSON *consumer*, key order must be irrelevant (JSON objects are unordered), so this is safe for
them — but the internal reliance means a field reorder in the struct silently changes byte output and
could break a golden-file test or a naive diff-based consumer. Reason to note: the stability guarantee a
consumer actually gets is "same keys, same schema_version," not "same bytes" — and the sample files in
docs/samples are byte-sensitive. *No code change; a one-line note that byte-order is an internal detail,
not part of the contract. Format-plumbing.*

### 36.5 `dev emit-schema` is hidden — the contract exists but is undiscoverable

The schema command lives under `Cmd::Dev` (main.rs:191/227) — a developer subcommand, not surfaced in
normal `--help` flow the way `analyze`/`render` are. So the strongest evidence that this is a real
contract (a publishable JSON Schema) is reachable only by someone reading the source. Reason: if the
JSON is meant for consumers, the schema should be discoverable — either promote `emit-schema` out of
`dev`, or check a generated `schema.json` into `docs/` and reference it from the README. Cheap, high
signal. *Format-plumbing + a committed artifact.*

### 36.6 Summary — the JSON is the most contract-like output and the least documented

Unlike the human formats (which are self-explanatory), the JSON's strengths are *invisible*: a schemars
schema, an enforced version, and lossless round-trip all exist in code but appear nowhere a consumer
looks. The gaps are correspondingly all documentation/plumbing, consistent with the through-line of
every prior pass — the *measurement* is complete, the *communication* lags:
- **P1 (correctness of contract signal):** reconcile the exact-match version gate (§36.1) with the
  `#[serde(default)]` evolution machinery — pick tolerant-read *or* document why not.
- **P2 (prevent the likely integration bug):** document omittable keys and absent-vs-empty semantics,
  especially `alloc_sites` absent = no allocation tracking (§36.2).
- **P2 (discoverability):** surface the JSON Schema — promote `emit-schema` or commit `docs/schema.json`
  (§36.5).
- **P3 (precision):** state that the JSON is complete for *data + triage verdicts* but not *narrative*,
  and that key/byte order is an internal detail, not the contract (§36.3, §36.4).

No new heap pass, no new field — the JSON already carries everything the human formats show. This pass
adds a *seventeenth* confirmation of the document's central finding: the tool computes richly and
communicates thinly, and here the thin communication is the missing docs around an already-solid
machine contract.

## 37. Cross-Dump Diff (`compare`) Output Audit (pass 18)

Every prior pass audited the *single-dump* report. The tool has a second, entirely separate output path:
`compare` (a.k.a. the N-way time-series diff), implemented in `src/diff_reports.rs` with its own model
(`SeriesDiffResult`), its own Markdown renderer (`render_md`, line 419), and its own HTML path
(`render_diff_html`, html.rs:107, via a `{"kind":"series-diff",...}` envelope). No lettered pass touched
it. This matters disproportionately because **diffing two dumps is the single most reliable way to find
a real leak** — a growing retained set across time beats any single-snapshot heuristic. This pass grounds
every claim in diff_reports.rs.

### 37.0 What's already right

The diff is thoughtfully built, not a bolt-on:
- **True N-way, not just pairwise.** `diff_series` joins the class histogram across all N reports into a
  `len-N` vector per class (lines 152–160), and every table renders one `r1…rN` column plus a Δ column
  (`series_table`, 398–406). A consumer sees the whole trajectory, not just endpoints.
- **Absent-class handling is explicit.** A class missing from report *i* is `None`, rendered as 0, and
  drives the new/removed classification (`first_present`/`last_present`, 169–170, 190–194). No NaN, no
  crash on disjoint class sets.
- **Deterministic and pure.** Every sort has a name tie-break (e.g. 206, 214, 222); the verdict carries
  "the only f64 in the whole renderer" (comment at 347) — a deliberate numerical-stability choice.
- **Suspect dedup matches single-dump semantics.** Where a report lists a suspect class twice, the join
  keeps the MAX retained "as pairwise does" (234) — consistent with the rest of the tool.
- **Labelled columns.** `r1…rN` headers are backed by a Reports legend mapping each to its
  `source_name` with a positional fallback (`labels`, 124–135; legend at 429–432) — the classic
  "which column is which dump?" problem is already solved.

### 37.1 The headline metric ignores every intermediate dump — first→last only (the core flaw)

Every Δ in the entire diff is computed as `retained[last] − retained[0]` (class Δ at 174; suspect Δ at
248; totals at 142/147). For N=2 that is exact. For **N≥3 it silently discards the middle**: a class
that balloons at r2 and is reclaimed by r3 shows `delta_retained = 0` and **never appears in Growth
Leaders** (which filters `delta_retained > 0`, line 201). Yet a transient spike that GC recovers is
often the *most* interesting signal — it is exactly the shape of a burst-allocation or a leak that was
hot-fixed between dumps. The tool collects the full `r1…rN` trajectory in the table but throws away its
own richest signal in the *ranking*. Reason to fix: the whole point of a *time series* (vs a pair) is
the intermediate shape; ranking purely on endpoints makes N≥3 no better than comparing the first and
last dump directly. *Fix: rank Growth Leaders by `max(retained) − retained[0]` (peak growth) or by
monotonic-increase run-length, not just first→last; keep the first→last Δ column as-is. Compute-cheap
(the `retained` vector is already in hand at 184).*

### 37.2 `net_delta_retained` is a misleading "net" that hides offsetting churn

`net_delta_retained` sums *per-class* first→last deltas across every class (line 181, rendered as
"**Net Δ Retained (all classes, r1→rN)**" at 446–449). Because it sums signed deltas, a dump where
`Foo` grew +500 MB while `Bar` shrank −480 MB reports a reassuring **Net Δ +20 MB** — burying a
half-gigabyte leak behind coincidental shrinkage elsewhere. The verdict line separately uses
`delta_total_shallow` (349–351), so the report shows *two different "how much did it grow" numbers* that
can disagree in sign. Reason: a "net" figure invites the reader to treat +20 MB as "basically flat,"
which is the opposite of the truth. *Fix: either drop the net figure, or pair it with **gross growth**
(Σ positive deltas) and **gross shrinkage** (Σ negative deltas) so the churn is visible; the per-class
deltas are already computed. Compute-cheap.*

### 37.3 Growth "%" uses the FIRST dump's shallow as base — correct, but undocumented and asymmetric

`verdict` computes `pct = delta_total_shallow / first_shallow * 100` (350–351) and the comment (344–347)
is explicit that this is deliberate. Good. But two rough edges: (a) for a *shrink* it prints
`pct.abs()` "Heap shrank X%" (376–377) — a 50% shrink and the growth that would undo it are **not the
same percentage** (shrinking to half is −50%, re-growing is +100%), so successive diffs won't read
symmetrically; worth a one-line note that shrink-% is relative to the *earlier, larger* base. (b) When
`first_shallow == 0` the pct silently becomes 0.0 (352–353) and the verdict says "grew 0.0%" even as
bytes appear — the same sub-MB/zero-base confusion flagged for the single-dump report in §31.6/§27.1.
Reason: consistency — the diff should reference the *same* canonical reachable-heap-base decision §27.1
calls for. *Fix: note the base in the verdict prose; guard the zero-base case with "n/a" not "0.0%".
Have/Format-plumbing.*

### 37.4 No sample, no golden file — the second-biggest output path is untested by example

`ls docs/samples/` confirms there is **no diff/compare/series sample** — every committed sample is a
single-dump report (scala-doku[-full].{md,graphs.md,html,json}). So: (a) a user has no example of what
`compare` produces before running it; (b) the README/docs can't show the format; (c) most importantly,
there is no golden-file regression guard on `render_md`/`render_diff_html` the way single-dump output is
pinned. This is the same gap §31.1 raised for the healthy single-dump sample, one level worse — here the
*entire format* is exampleless. Reason: the diff renderer is ~200 lines of table/verdict logic with real
sign/format edge cases (§37.1–37.3) and no committed output to catch regressions. *Fix: capture two (or
three) dumps of a toy growing app, commit the `compare` output in each format as
`docs/samples/*-diff.*`, and add a golden test. F (capture + regen), and it doubles as documentation.*

### 37.5 "Removed Classes" / "Gone Suspects" are computed and rendered but semantically thin

`removed_classes` (present in r1, absent in rN, 192–194) and `gone_suspects` (259–260) each get a full
section. For leak-hunting these are the *least* actionable slices — a class that vanished is, by
definition, not leaking now — yet they occupy equal visual weight to Growth Leaders. Worse, "removed"
is again first-vs-last only (§37.1): a class present at r1, gone at r2, back at r3 is **not** removed,
while a class that merely dropped below the histogram cap looks removed. Reason: prime real estate is
spent on the anti-signal, and the anti-signal is computed on the fragile endpoints-only basis. *Fix:
demote Removed/Gone below the growth sections (they're reassurance, not findings), and label them
"absent in final dump" to set expectations about the endpoints-only definition. Format-plumbing.*

### 37.6 Summary — the diff is the tool's best leak-finder, hobbled by endpoints-only math

The `compare` path is well-engineered structurally (true N-way join, deterministic, labelled, honest
about absent classes) but its *analysis* collapses the time series to its two endpoints everywhere it
matters — ranking, net figure, new/removed classification. For N=2 that's fine; for N≥3 it wastes the
very trajectory it collected. The fixes are almost all Compute-cheap (the `r1…rN` vectors are already in
hand) plus one documentation/sample gap:
- **P1:** rank Growth Leaders by peak-vs-baseline (or monotonic run), not first→last, so transient
  spikes in N≥3 series surface (§37.1).
- **P1:** replace or augment the misleading single "Net Δ Retained" with gross-growth / gross-shrinkage
  so offsetting churn is visible (§37.2).
- **P2:** commit a `compare` sample in every format + a golden test — the whole second output path is
  currently exampleless and unpinned (§37.4).
- **P2/P3:** document the shrink-% base and guard the zero-base verdict (§37.3); demote and relabel the
  endpoints-only Removed/Gone sections (§37.5).

This eighteenth pass extends the through-line to the second output path: the trajectory data is fully
*collected* (rich model) but under-*analyzed* and under-*shown* (endpoints-only ranking, no sample) —
measurement rich, communication thin, once more.

## 38. CLI & Invocation-UX Audit (pass 19)

Every prior pass audited what the tool *emits*. This one audits how a developer *drives* it — argument
design, format inference, error messages, exit codes, streaming, and the first-run experience. This is
where the tool is either trustworthy-as-a-Unix-filter or a foot-gun, and no lettered pass looked at it.
Grounded entirely in `src/main.rs`.

### 38.0 What's already right — this is a properly-behaved Unix filter

The CLI surface is unusually disciplined; several things most tools get wrong are handled:
- **SIGPIPE is restored to default** (main.rs:364–367) so `hprof-analyzer big.hprof | head` terminates
  cleanly instead of panicking on EPIPE. Rare and correct.
- **Ineffective flags are refused with a hint, not silently ignored.** On the re-render path,
  `--collections`, `--collection-config`, `--find-duplicates`, and `--detail` each `fail()` with "has no
  effect when re-rendering a saved report; re-run on the .hprof dump…" (main.rs:472–499). This is the
  gold standard — the tool tells you *why* and *what to do instead*.
- **Diagnostic flags are accepted as harmless no-ops** on re-render (`--verbose`/`--trace-rss`/
  `--progress`, 500–502) — the distinction between data-affecting flags (refused) and diagnostics
  (tolerated) is deliberate and documented in a comment.
- **Missing input files are named up front** (Compare arms pre-check `Path::exists` at 377/392 to avoid
  a bare OS error) and the re-render path has a *content-sniffing* hint: feed it an `.html` or `.md`
  report by mistake and `render_error_hint` (558+) says "looks like a rendered HTML report; re-render
  from the saved report JSON, not the .html."
- **Exit codes are honest:** `fail` → 1 (main.rs:353–355), usage error → 2 (437), gate/parity failure →
  2 (384/416). A CI script can distinguish "broke" from "found a regression."
- **Progress auto-detects a TTY** and disables itself under `--verbose`/`--trace-rss` (447) so piped
  output is never polluted with a progress line.

Call these out in the CRIT record because they set the bar the remaining nits are measured against —
this is a mature CLI, and the findings below are refinements, not rescues.

### 38.1 `md-graphs` can never be inferred from an output path — a silent downgrade to plain md

`resolve_format` deliberately never infers `md-graphs` because it shares the `.md` extension with plain
Markdown (main.rs:313–314 comment; only `-f md-graphs` selects it). Consequence: `hprof-analyzer
heap.hprof report.md -f md-graphs` works, but the far more natural `hprof-analyzer heap.hprof
graphs.md` produces **plain** Markdown with no warning — the user who named the file `graphs.md`
expecting charts gets the chartless format and no signal that `-f md-graphs` existed. Reason: this is
the one format that the extension system structurally cannot express, so a user can silently get the
wrong one. *Fix: when writing `md-graphs` to a file, or when the output path stem contains "graph",
emit a one-line stderr note ("writing plain Markdown; pass -f md-graphs for ASCII charts"). Cheap,
Format-plumbing. Alternatively accept a `.graphs.md` convention as an inference trigger — the samples
already use exactly that name (`scala-doku-full.graphs.md`).*

### 38.2 `--detail` is refused on *any* non-Default value at re-render, including a redundant `--detail default`

The re-render guard fires when `cli.detail != DetailLevel::Default` (main.rs:493). So `render
report.json --detail minimal` correctly errors — but a user scripting a uniform invocation across both
analyze and re-render inputs (`hprof-analyzer "$f" out.html --detail default`) is *fine* only because
Default is the sentinel. The subtlety: `--detail` has no "unset" state (it's `default_value_t =
DetailLevel::Default`, 141), so the tool cannot distinguish "user explicitly asked for default" from
"user said nothing." Passing `--detail default` explicitly is silently accepted on re-render while
`--detail minimal` is refused — inconsistent from the user's view (both are "I passed --detail to a
re-render"). Reason: minor, but the refusal message claims "--detail has no effect when re-rendering"
which is *also* true of `--detail default`, yet that one is allowed. *Fix: either document that Default
is the no-op sentinel, or make `detail` an `Option` so an explicit `--detail default` on re-render gets
the same honest hint. Have/Format-plumbing.*

### 38.3 `--detail max` sets `dominator_tree_max_nodes = 100_000` — directly feeding the §28.1 blow-up

`DetailLevel::Max` raises the dominator cap to **100,000 nodes** and depth 50 (main.rs:282). §28.1
already flagged that the *default* 5000-node subtree is 43–72% of the report; `--detail max` multiplies
that 20× with no warning that the dominator section alone can then dominate a multi-MB Markdown file.
The presets are otherwise sensible, but Max is a cliff for exactly the section the document spends the
most ink taming. Reason: a user reaching for "more detail" gets a pathologically large tree, not
uniformly richer output. *Fix: when the rendered dominator subtree exceeds, say, 2000 nodes, print a
stderr note; and/or cap the *rendered* tree independently of the *computed* one (the §28.1 depth≤4/
breadth≤5 collapse should apply regardless of `--detail`). Compute-cheap once §28.1 lands.*

### 38.4 stdout output is buffered whole then written once — no streaming for large reports

`run`/`render_report` build the entire report `String` and hand it to `write_output` in one shot
(main.rs:719–724, 985–994). For a `--detail max` HTML report (embedding the full JSON, §36.0) this is a
multi-MB allocation held entirely in memory before the first byte reaches stdout — the opposite of the
"streaming passes / low-memory" promise in the `long_about` (main.rs:96–97). The *parse* is streaming;
the *emit* is not. Reason: honesty — the tool advertises low memory, and for the output stage that only
holds for small reports. *Note, don't necessarily fix: the report is bounded aggregates (§36 model
comment "never a per-object Vec"), so the string is O(caps) not O(heap); the buffering is a real but
bounded cost. Worth one sentence in the perf docs rather than a rearchitecture. Have.*

### 38.5 No `--quiet`, and `compare reports` prints via `print!` bypassing the gz/stdout writer

Two smaller asymmetries: (a) there is no `--quiet` to suppress the auto progress line without also
choosing `--progress never` — minor, `never` covers it, but discoverability is low. (b) `compare
reports` writes its result with `print!("{text}")` directly (main.rs:397), unlike the default path which
routes through `write_output` (supporting `.gz` and a named output file). So the diff output **cannot be
written to a file or gz-compressed** the way a single-dump report can — you must shell-redirect. Reason:
capability asymmetry between the two output paths, reinforcing §37's "the diff path is a second-class
citizen." *Fix: give `compare reports` the same optional output-path arg + `write_output` plumbing the
default command has. Format-plumbing.*

### 38.6 Summary — a mature CLI with a few inference and parity edges

The invocation surface is the most polished part of the tool audited so far: SIGPIPE, refuse-with-hint,
content-sniffing errors, honest exit codes, TTY-aware progress. The findings are refinements:
- **P2:** `md-graphs` silently downgrades when inferred from a `.md`/`graphs.md` path — warn, or honor a
  `.graphs.md` convention the samples already use (§38.1).
- **P2:** `--detail max` feeds the §28.1 dominator blow-up (100k nodes) — the rendered-tree collapse must
  apply regardless of `--detail` (§38.3).
- **P3:** give `compare reports` an output-path/`--gz` arg via `write_output` so the diff isn't
  stdout-only (§38.5); make `--detail` an `Option` so explicit `default` on re-render is handled
  consistently (§38.2).
- **Doc-only:** note that report *emission* buffers the whole string (bounded by caps, not heap) despite
  the streaming-parse promise (§38.4).

Nineteenth pass, same through-line from a new direction: the tool's *mechanics* are rich and careful;
the gaps are communication (a missing warning, a doc sentence) and one output-path parity item — not
missing capability.

## 39. Custom Collection-Handler Config (TOML) Audit (pass 20)

The tool ships a user-extensible input surface no prior pass examined: `--collection-config` and its
auto-discovered TOML (`.hprof-analyzer.toml` in CWD, or `$HOME/.config/hprof-analyzer/collections.toml`),
which lets a user teach the container-attribution pass about *their own* collection classes — the exact
data behind §8 Collections and §25 Container Attribution. It has its own parser, discovery, and merge
logic in `src/collection_config.rs`. Because it feeds §8/§25 directly, a silently-wrong config produces
silently-wrong waste/attribution numbers. Grounded in collection_config.rs and pass2/fielddecode.rs.

### 39.0 What's already right

- **User entries shadow built-ins, prepended.** `merge_descs` puts user descs first so they override the
  built-in for the same class (collection_config.rs:84–88, comment "User entries come first so they
  shadow built-ins") — the correct precedence for customization.
- **`Class#field` shorthand is ergonomic.** `parse_class_field` (8–16) lets `size_field = "size"` default
  its owner to the entry's `class`, while `size_field = "Other#count"` names a different declaring class
  — matching the real JVM case where a field is declared on a superclass (the built-ins use exactly this,
  e.g. LinkedHashMap's size lives on HashMap, fielddecode.rs:216–219).
- **`kind` defaults sensibly** to `List` (`default_kind`, 42–44) and is validated against a closed set
  with a listing error: "unknown collection kind: X; expected Map|Set|List|Deque|Queue|Tree" (26–28).
- **Parse/read failures never abort the run.** `load_collection_descs` (95–113) maps both read errors and
  parse errors to `eprintln!("warning: …")` + `.ok()`, then `unwrap_or_default()` — a broken config
  degrades to built-ins-only instead of killing the analysis.

### 39.1 The `class` field is copied verbatim with no JVM-internal-name validation — the silent-mismatch trap

`parse_toml_str` copies `e.class` straight into `CollDesc.class_name` (collection_config.rs:72) with no
normalization or validation. But every built-in uses the **JVM-internal slash form** —
`"java/util/HashMap"`, `"java/util/ArrayList"` (fielddecode.rs:211/231) — because that is how class
names arrive from the HPROF parser. A user who naturally writes the **source dotted form**
`class = "com.example.MyCache"` gets a `CollDesc` that **matches nothing**, produces **zero attribution
rows**, and receives **no error** — the config parsed fine, it just silently never fires. This is the
single most likely user mistake and the tool is completely silent about it. Reason: the one input a user
is *guaranteed* to get wrong (dots vs slashes) has no guard and no feedback. *Fix: normalize `.`→`/` in
`class` (and the `#`-owner) on load — or, if verbatim is intentional, validate the form and warn "class
'com.example.Foo' uses dotted form; HPROF class names are slash-delimited (com/example/Foo)". Cheap,
Compute-cheap, one `replace`. Have.*

### 39.2 No feedback on how many user descs loaded or whether any matched

`load_collection_descs` returns a merged Vec silently — there is no "loaded N custom collection handlers
from PATH" line, and (confirmed by grep) nothing anywhere in the report or stderr reports how many user
descs actually *matched instances* in the dump. So the user cannot distinguish (a) config not found, (b)
config found and loaded but classes absent from this dump, (c) config loaded and matched. Combined with
§39.1, a dotted-name typo is indistinguishable from "my class genuinely isn't in this heap." Reason:
customization is a fire-and-forget with no confirmation loop, so misconfiguration is invisible. *Fix:
under `--verbose`, print "collection config: loaded N handlers from PATH; M matched instances"; the match
count is available where attribution is computed. Compute-cheap.*

### 39.3 An explicit `--collection-config PATH` that fails to read exits 0 with only a warning

`find_config` returns the explicit path unconditionally (collection_config.rs:117–118); if it then can't
be read (typo, permissions), `load_collection_descs` warns and falls back to built-ins (99–104), and the
run **completes normally with exit 0**. That is right for *auto-discovered* config (absence is normal)
but wrong for an *explicit* `--collection-config` — the user named a file on purpose; silently ignoring
it and reporting success violates least-surprise, and a CI job that relies on custom handlers will
green-light with the handlers absent. Contrast §38.0, where the CLI is otherwise scrupulous about
refuse-with-hint. Reason: explicit intent should be honored or fail loudly, unlike optional
auto-discovery. *Fix: when the path is explicit (not auto-discovered), a read/parse failure should
`fail()` (exit 1) with the existing message, not warn-and-continue. Cheap, Format-plumbing — thread a
`was_explicit` bool into the error arm.*

### 39.4 The feature is entirely undocumented — README covers `--collections` but never the TOML

`grep` of README.md shows `--collections` (line 34, 188) but **no mention** of `--collection-config`, the
two auto-discovery paths, the `[[collection]]` TOML shape, the `class`/`kind`/`size_field`/`array_field`/
`nested_map_field` keys, or the required slash class-name form. DESIGN.md and docs/ likewise. So the only
way to discover this input surface — and the only place the slash-form requirement (§39.1) could be
learned — is reading `collection_config.rs`. Reason: a user-extension mechanism that users can't discover
or spec has near-zero realized value, and its one sharp edge (§39.1) is exactly what docs would defuse.
*Fix: a README subsection with a worked `[[collection]]` example in slash form + the discovery order.
F (docs).*

### 39.5 `nested_map_field` and the `kind` taxonomy are unexplained even in code

`RawEntry` accepts `nested_map_field` (collection_config.rs:39) but neither the struct nor any doc
comment says what it *does* (it exists for wrapper collections whose real storage is a delegate map — but
a user can't know that). Likewise the six `CollKind` values drive different sizing paths (e.g. `Map`
reads a `table` array, `Tree` approximates from `size` with no array — fielddecode.rs:225–230) yet the
user picks one blind. Reason: the config's expressiveness is real but unusable without knowing which
`kind` triggers which sizing strategy. *Fix: doc-comment each `kind`'s sizing behavior on the enum and
each `RawEntry` field; fold the summary into the §39.4 README example. F (docs/comments).*

### 39.6 Summary — a genuinely useful extension point, silent and undocumented at every failure mode

The custom-handler system is well-designed internally (shadow-prepend precedence, `Class#field`
shorthand, closed-set `kind` validation, non-fatal degradation) but it fails the user quietly at every
turn: a dotted class name matches nothing silently (§39.1), no load/match feedback confirms it worked
(§39.2), an explicit bad path exits 0 (§39.3), and nothing is documented (§39.4/§39.5). Because this
config feeds §8/§25 directly, a silent misconfig yields silently-wrong waste numbers — the worst kind of
error in an analysis tool.
- **P2:** normalize/validate the `class` field's dotted-vs-slash form — the guaranteed user mistake, with
  no guard today (§39.1).
- **P2:** document the feature (README `[[collection]]` example, discovery order, slash-form requirement,
  `kind` semantics) — currently undiscoverable (§39.4, §39.5).
- **P3:** fail loudly (exit 1) when an *explicit* `--collection-config` can't be read/parsed, unlike
  auto-discovery (§39.3); print loaded/matched handler counts under `--verbose` (§39.2).

Twentieth pass, and the through-line holds on the *input* side too: the extension mechanism is capable
and correctly built, but its communication — validation feedback, match confirmation, documentation — is
thin, so a user's customization can silently do nothing.

## 40. HTML Runtime Robustness & Data-Flow Audit (pass 21)

Prior HTML passes audited *accessibility* (§32) and the *JSON schema contract* (§36). Neither looked at
the React app's **runtime resilience**: how the embedded JSON enters the app, and what the user sees when
it's malformed, schema-drifted, missing keys, or huge. This matters because the HTML report is the
format non-Rust users open, and it is a *single self-contained file* that may be re-opened months later
against a binary that has since bumped the schema. Grounded in `web/src/index.tsx`, `web/src/App.tsx`,
and `src/html.rs`.

### 40.0 What's already right

The boot path is carefully built, not naive:
- **Self-contained, zero-network** (html.rs:3–13): the report JSON and the JS bundle are both embedded as
  base64 `<script type="application/octet-stream">` blobs (html.rs:84–85), raw-DEFLATE-compressed and
  inflated client-side. No CDN, no fetch — it works from `file://`.
- **The decode/parse step is wrapped** (index.tsx:33–39): `hprofDecodeText` + `JSON.parse` sit in a
  `try/catch` that calls `fail("Failed to parse report data: …")`, rendering the message into `#root`
  instead of a silent blank.
- **Missing-bootstrap guards** (index.tsx:28–31, 42–45): absent `hprofDecodeText` or `#root` each produce
  an explicit `fail(...)` message.
- **`localStorage` is defensively wrapped** (App.tsx:31/34/42/48) because "`file://` storage may throw" —
  a real Chrome/Firefox behavior for `file://` origins that would otherwise crash theme init.
- **A no-JS/loading fallback exists**: `#root` ships with "Loading heap dump report…" text (html.rs:83)
  so a user without the bundle inflated sees *something*.
- **Diff vs single-dump dispatch is shape-based** (index.tsx:49–52): checks `kind === "series-diff"`
  before choosing `DiffApp` vs `App`, so the same shell serves both payloads.

### 40.1 No client-side `schema_version` check — the HTML silently renders drifted data (the core gap)

The Rust read path *hard-gates* the schema: `render` from JSON refuses unless `schema_version ==
SCHEMA_VERSION` with an actionable error (§36.0, main.rs:709). The HTML app has **no equivalent**: grep
of `web/src/` shows `schema_version` appears only as a *type field* (types.ts:675), never compared.
`boot()` (index.tsx:33–61) parses and renders whatever it gets. So an HTML file's embedded JSON is
authoritative at *generation* time, but if the app bundle and the JSON blob ever diverge — or a consumer
hand-crafts the `#report-data` blob — the client renders drifted data with no warning, the exact failure
the Rust side is careful to prevent. In practice generation is atomic (html.rs writes both blobs
together), so this can't happen from normal use — but it removes the one guard that would catch a
tampered or hand-edited report, and it is an asymmetry with the tool's own stated contract discipline.
Reason: the schema gate should be enforced everywhere the JSON is consumed, not just the Rust path.
*Fix: in `boot()`, after parse, compare `parsed.schema_version` (single-dump) against a
build-time-injected constant and `fail("report schema vN, viewer expects vM; re-render")`. Cheap; the
constant can be templated into the bootstrap the same way the blobs are. Have.*

### 40.2 No React error boundary + `#root` cleared before render = blank page on any render throw

`boot()` clears `#root` (`el.textContent = ""`, index.tsx:46) and *then* calls `createRoot(el).render(...)`
(53–61). There is **no error boundary** anywhere (`grep`: no `componentDidCatch`/`getDerivedStateFromError`).
So if any component throws during render, React unmounts the tree, `#root` is already empty, and the user
sees a **blank white page** — the "Loading…" fallback is gone and the parse-error `fail()` path doesn't
cover *render*-time throws. This is reachable: `App` accesses `report.overview.source_name` **unguarded**
at App.tsx:3583, and `report.generated` at 3585. Feed it a JSON where `overview` is absent (older schema,
partial hand-edit, a `skip_serializing_if` field a consumer mis-modeled per §36.2) and the very first
line of the app throws `Cannot read properties of undefined`, blanking everything. The scattered `?.`/
`??` guards elsewhere (e.g. `report.triage ?? []` at 239, `report.threads?.threads?.length ?? 0` at 1116)
show the codebase *knows* fields can be absent — but the top-level required keys have no such guard and
no boundary to catch the throw. Reason: a single missing top-level key should degrade to a legible error,
not a blank page. *Fix: wrap the rendered tree in an ErrorBoundary whose fallback shows the error + "this
report may be from an incompatible version"; keep a copy of the "Loading…" node or render the boundary
fallback into `#root`. Compute-cheap (≈30 lines, one class component).*

### 40.3 Whole report is parsed and mounted eagerly — no windowing for max-detail payloads

`boot` does one `JSON.parse` of the entire report and `App` renders every section synchronously
(App.tsx:3592–3603+). §38.3 noted `--detail max` can produce a 100k-node dominator tree and §36 that the
HTML embeds the full JSON; here that lands as (a) a multi-MB `JSON.parse` on the main thread, (b) the
class histogram capped at 500 rows rendered as 500 live DOM rows (noted in §32.5), and (c) the whole
section tree mounted at once with no `React.lazy`/virtualization. For a default report this is fine; for
`--detail max` on a large heap the first paint can stall visibly. Reason: the one format aimed at
non-technical viewers is the one with no payload-size backpressure. *Note over fix: the caps already
bound this (§36 "bounded aggregates"), so it's a max-detail-only concern; a `<details>`-collapsed heavy
section (dominator, histogram) rendered lazily on expand would remove the worst of it. Compute-cheap if
scoped to the two heavy sections.*

### 40.4 `fail()` writes `textContent` = the raw exception string, with no recovery affordance

`fail(msg)` (index.tsx:14–17) sets `root.textContent = msg` — so a parse failure shows a bare
`"Failed to parse report data: SyntaxError: …"` with no styling, no "the report file may be truncated —
re-generate with `hprof-analyzer dump.hprof report.html`" guidance, and no link back to the CLI. Contrast
the Rust `render_error_hint` (§38.0, main.rs:558+) which *sniffs* the bad input and suggests the fix.
The HTML fallback is honest but terminal — a non-technical user hitting it has no next step. Reason:
parity with the CLI's helpful-error discipline; this is the one error surface a GUI user will actually
see. *Fix: make `fail` render a small styled panel with the message + a one-line "how to regenerate"
hint. Format-plumbing.*

### 40.5 Summary — the viewer is robust at the boundary it expected, blank at the one it didn't

The boot path defends the failure it anticipated (decode/parse in try/catch, missing-bootstrap guards,
`file://`-safe storage) but not the two it didn't: **schema drift** (no client version check, §40.1) and
**render-time throws on missing keys** (no error boundary + pre-cleared `#root` = blank page, §40.2).
Both are low-probability from normal atomic generation but are exactly the long-tail cases — a re-opened
old report, a hand-edited or consumer-generated blob — where a GUI user is least equipped to diagnose a
blank screen.
- **P2:** add a client-side `schema_version` check in `boot()` mirroring the Rust gate (§40.1) — enforce
  the contract everywhere the JSON is consumed.
- **P2:** add a React error boundary (and stop leaving `#root` blank) so a missing top-level key yields a
  legible error, not a white page (§40.2).
- **P3:** lazy-render the two heavy sections (dominator, 500-row histogram) for `--detail max` payloads
  (§40.3); upgrade `fail()` to a styled panel with a regenerate hint mirroring `render_error_hint`
  (§40.4).

Twenty-first pass: the through-line extends to the client runtime — the app is *built* richly and guards
the expected boundary, but its *communication on the unexpected path* is a blank page, when the same
schema-gate discipline the Rust side already has would turn it into a legible, actionable message.

## 41. Numeric Formatting Primitives Audit (pass 22)

§27/§7/§16 audited *which base* each percentage divides by and *whether* caps are honest. No pass audited
the formatting functions themselves — `format_bytes`, `fmt_pct`, `fmt_count`, and the bp→percent/bytes
conversions. These are the lowest-level primitives in the report: every byte figure in every section, in
every format, flows through `format_bytes` (used 242× in render_md.rs alone by grep). A rounding or
unit artifact here multiplies across the whole document, so it is worth one focused pass. Grounded in
`src/report/format.rs` and `src/report/render_md.rs`.

### 41.0 What's already right

- **The byte-unit boundary is guarded against the classic "1024.0 KB" bug.** `format_bytes`
  (format.rs:172–188) picks the unit by magnitude, then re-checks the *rounded mantissa*: `(kb *
  10.0).round() < 1024.0 * 10.0` promotes 1 MiB−1 to "1.0 MB" instead of printing "1024.0 KB"
  (comment at 173–175). This is the correct, non-obvious fix most formatters miss.
- **`fmt_count` is a clean, allocation-frugal thousands grouper** (format.rs:191–201) — reverse, insert
  comma every 3, reverse back; `1234567 → 1,234,567`.
- **Derived percent stats are kept out of the JSON on purpose** (`DepthStats`, format.rs:213–216
  "fully derivable, so emitting it would bloat the report") — the same say-it-once discipline §1/§24
  praise, applied at the primitive level.
- **`bp` (basis points) as the stored unit is a deliberate precision choice**: integers 0–10000 avoid
  float drift in the JSON, converted to percent only at render (`bp/100.0`, render_md.rs:512+). Good.
- **Timestamps are deterministic and parity-excluded** (`format_epoch_ms`, 135; `now_iso8601` flagged
  "non-deterministic — parity comparison ignores this line", 124–125) — a thoughtful reproducibility
  split.

### 41.1 `KB`/`MB`/`GB` labels are binary (1024-base) but use decimal-SI names — an unstated ~7% overstatement at GB

`format_bytes` divides by 1024 / 1024² / 1024³ (format.rs:179/183/187) but labels the results `KB`/`MB`/
`GB`. Those SI names mean 1000-base; the binary units are `KiB`/`MiB`/`GiB`. The numeric divergence
compounds per tier: a "1.0 GB" figure is really 1.07 decimal-GB — a ~7% gap a capacity-planning reader
who compares against a decimal-GB cloud quota will misjudge. MAT itself uses the same 1024-base "MB"
labels, so matching it is defensible — but the report never *says* the units are binary, so a reader
can't know which convention applies. Reason: silent unit ambiguity in the tool's most-used primitive;
the fix is a one-time disclosure, not a relabel that would break MAT parity. *Fix: either switch to
`KiB/MiB/GiB` (precise, breaks visual parity with MAT), or — lower-risk — add "sizes are binary (1
MB = 1024 KB)" once to the report preamble/System Overview caption. Format-plumbing. Have.*

### 41.2 The `GB` branch has no upper guard — a huge heap prints an absurd mantissa

The unit ladder in `format_bytes` guards KB→MB and MB→GB (the `n < 1024*1024*…` conditions at 180/184)
but the final branch (187) is unconditional: `format!("{:.2} GB", n / 1024³)`. A 1 PiB value therefore
prints as **"1048576.00 GB"** rather than "1.00 PB". Real heap dumps won't hit petabytes, so this is not
a live bug — but it is an asymmetry in an otherwise carefully-laddered function, and the tool *does*
accept arbitrary `u64` byte counts (a corrupt/crafted dump, or a `compare` summing many dumps, could
produce a silly figure). Reason: completeness and defensiveness of the primitive that everything else
trusts. *Fix: add a `TB`/`PB` tier (or clamp+annotate) so the mantissa never exceeds ~1024. Cheap,
Compute-cheap.*

### 41.3 `fmt_pct` / bp→percent render at one decimal — anything below 0.05% collapses to "0.0%"

`fmt_pct` is `format!("{p:.1}%")` (format.rs:268–269) and every bp display is `bp as f64 / 100.0` at
`{:.1}` (render_md.rs:512/517/522). So a class holding 0.04% of the heap renders **"0.0%"** —
indistinguishable from a true zero. §27.7 raised this for the concentration table specifically; this
pass locates the *root*: it is the shared primitive, so the artifact appears anywhere a small share is
shown (Top Consumers tail, per-package rollup, concentration). For a leak-hunt this matters least at the
tail — but a row that reads "0.0%" next to a non-zero byte figure looks like a bug to the reader and
undermines trust. Reason: a single primitive controls sub-0.1% legibility document-wide. *Fix: in
`fmt_pct`, render values in `(0, 0.05)` as "<0.1%" rather than "0.0%" (a 2-line guard); everything above
is unchanged. This one edit fixes §27.7 and every other sub-0.1% site at once. Compute-cheap. Have.*

### 41.4 `bp_to_bytes` truncates via integer math — the displayed bytes can under-sum the total

The concentration table derives displayed bytes from stored bp: `bp_to_bytes = |bp| (bp as u64 * total)
/ 10_000` (render_md.rs:505). Integer division *truncates*, so each of top1/top10/top100 loses up to
~`total/10000` bytes (for a 30 MB heap, up to ~3 KB per row — negligible), but more importantly the
*byte* column and the *percent* column are derived by two different roundings (bp→bytes truncates;
bp→percent rounds at 1 decimal), so a reader multiplying "X% of 30 MB" by hand won't reproduce the shown
byte figure exactly. Reason: two independent roundings of the same underlying quantity can visibly
disagree, which reads as an arithmetic error. *Note over fix: the magnitudes are tiny; the honest fix is
to derive the displayed bytes and percent from the *same* source (compute bytes first, then percent from
bytes) so they're internally consistent. Compute-cheap; low priority given the sub-KB magnitude.*

### 41.5 `fmt_count` is `u64`-only — signed deltas re-implement grouping separately (drift risk)

`fmt_count` takes `u64` (format.rs:191), so the diff renderer had to write its *own* signed grouper,
`fmt_delta_count` (diff_reports.rs:323, audited in §37), duplicating the reverse-insert-comma logic with
a sign prefix. Two implementations of the same thousands-grouping means a future change to grouping style
(e.g. thin-space instead of comma, or Indian lakh grouping) must be made in two places or they drift.
Reason: a formatting primitive should have one implementation; the signed case is the same algorithm.
*Fix: make `fmt_count` delegate from a shared `group_digits(&str) -> String` that both the unsigned and
signed formatters call. Format-plumbing, pure refactor, no output change.*

### 41.6 Summary — the primitives are careful; the gaps are unit disclosure and sub-0.1% legibility

`format_bytes`'s boundary guard and the bp-as-storage choice show real numerical care. The findings are
small and mostly one-liners, but they sit at the base of the whole report so each fix propagates:
- **P2:** disclose that byte units are binary (1024-base) — the ~7%-at-GB ambiguity in the most-used
  primitive, one caption line (§41.1).
- **P2:** render sub-0.05% as "<0.1%" not "0.0%" in `fmt_pct` — one edit fixes §27.7 and every other
  small-share site at once (§41.3).
- **P3:** add a `TB`/`PB` tier so `format_bytes` never prints a 6-digit mantissa (§41.2); derive
  concentration bytes+percent from one rounding so they can't visibly disagree (§41.4); unify
  `fmt_count`/`fmt_delta_count` on one digit-grouper (§41.5).

Twenty-second pass: even at the primitive layer the pattern holds — the *computation* is precise (bp
storage, boundary-guarded units) and the gaps are *communication*: unstated unit convention and a
small-share display that reads as zero. One `fmt_pct` line and one caption clear most of it.

## 42. Anchor / ToC / Cross-Reference Integrity Audit (pass 23)

§31.4 touched empty-section ToC drift and §26.6 flagged cross-format anchor resolution as a *test to
add*. Neither audited the anchor-generation *mechanism*. This pass does — and it uncovers a **live,
reproducible broken-link bug in the shipped sample**, the first hard functional defect (not a
presentation gap) found since the §32.1 dark-mode SVG and §29.2 dropped-sections bugs. Grounded in
`render_md.rs`, `triage.rs`, `web/src/App.tsx`, and quoted from `docs/samples/scala-doku-full.md`.

### 42.0 The architecture: three independent anchor namespaces for one link space

There are **three separate sources of truth** for section anchors, and nothing reconciles them:
1. **Markdown headings → GitHub auto-slugs.** `## System Overview` becomes `#system-overview`
   (lowercase, spaces→hyphens, punctuation stripped). The ToC hardcodes links matching this convention
   (`render_toc`, render_md.rs:285–327, e.g. `#container-attribution-classfield` at 301).
2. **HTML literal `id=` strings**, hand-written on each `<section>` (App.tsx): `id="overview"`,
   `id="leaks"`, `id="top"`, `id="container-attribution"` — a *different* vocabulary from the md slugs.
3. **Triage `anchor` string literals** in triage.rs (e.g. `Some(("overview", …))` at 349,
   `Some(("leak-suspects", …))` at 243), rendered into md as `[label](#anchor)` (format_signal_md,
   render_md.rs:473) and into HTML as `<a href={`#${anchor}`}>` (App.tsx:251).

For a triage link to work, its `anchor` must match the target format's namespace. But the md-slug and
HTML-id namespaces **disagree** for every section whose heading text isn't already its HTML id:
`system-overview`≠`overview`, `leak-suspects`≠`leaks`, `top-consumers`≠`top`,
`container-attribution-classfield`≠`container-attribution`. So a single `anchor` string **cannot be
correct in both formats** for those sections.

### 42.1 THE BUG: triage anchors are authored inconsistently, so each format has different broken links

The `anchor` literals in triage.rs are not from one namespace — some are md-slugs, some are HTML-ids:
- Rules using **`"overview"`** (triage.rs:349, 719, 790, 953+ — the single most-used anchor): correct in
  HTML (`id="overview"`), **broken in Markdown**. Proof, quoted from the sample (line 60):
  `**Dominant GC-root type:** … See [System Overview](#overview).` — but the only matching heading is
  `## System Overview` (sample line 69) which slugs to `#system-overview` (and the ToC at line 11 links
  exactly that). `#overview` resolves to **nothing** in the Markdown render.
- Rules using **`"leak-suspects"`/`"top-consumers"`** (triage.rs:243/256/309/317/424): correct in
  Markdown (sample lines 58/59/62 link `#leak-suspects`/`#top-consumers`, matching the `##` headings),
  but **broken in HTML** — the sections are `id="leaks"` and `id="top"` (App.tsx:1644/1724), so those
  same links dead-end in the HTML report.
- Rules using **`"collections"`, `"references"`, `"leak-indicators"`, `"duplicate-strings"`,
  `"boxed-numbers"`, `"header-overhead"`**: happen to be identical in both namespaces, so they work
  everywhere — masking the bug in casual testing.

So the defect is *format-complementary*: the md report has broken "System Overview" triage links while
the HTML report has broken "Leak Suspects"/"Top Consumers" triage links, from the **same** `Report`.
This is exactly the failure §26.6 predicted, now confirmed live with line numbers. Reason it matters:
triage is the *first* section a user reads (§33.1 calls it the most action-bearing view), and its "See X"
links are the primary navigation into the detail — a dead anchor silently drops the reader at the top of
the page with no feedback. *Fix (the right one): store a single canonical section key per section and
map it to each format's slug at render time — the ToC, the HTML `id=`, and the triage link must all
derive from one table. Concretely: a `fn section_anchor(key, Format) -> String` (or a `SectionId` enum)
so md emits `system-overview`, HTML emits `overview`, both from `SectionId::Overview`. Compute-cheap to
plumb; eliminates the entire class. Have (all the strings already exist, just unreconciled).*

### 42.2 The ToC comment claims slug-generation but the code hardcodes — a latent drift trap

`render_toc`'s doc comment says it follows "GitHub's slug convention … matching the `##` headings emitted
by the section renderers. Kept in lock-step with `render_toc_graphs`" (render_md.rs:282–284). But the
function does **not** derive slugs from the headings — it hardcodes both the link text and the `#slug`
as string literals (287–326). So if someone renames a heading (e.g. `## Top Consumers` → `## Top Heap
Consumers`), the heading's real slug becomes `#top-heap-consumers` while the hardcoded ToC link stays
`#top-consumers` — a silently broken ToC entry, with the comment actively reassuring the maintainer that
they're "in lock-step." §29.2 already found render_graphs is a *parallel* renderer that dropped sections;
this is the same class of hazard in the ToC. Reason: the comment describes an invariant the code does
not enforce, which is worse than no comment. *Fix: either genuinely derive the slug from the heading
string via a shared `slugify(&str)` (then the ToC can't drift), or fix the comment to say the links are
hand-maintained and MUST be updated with any heading rename. The former also feeds §42.1's canonical-key
table. Compute-cheap.*

### 42.3 Nothing verifies anchor targets exist — a broken link is invisible to CI

There is no test that every emitted `#anchor` (ToC link, triage link, glossary cross-ref) resolves to a
real heading/`id` in the *same* format. §26.6 proposed exactly this and it remains unbuilt (the §42.1 bug
is the proof it's needed — it shipped in the committed sample undetected). The check is mechanical:
render each format, extract all `#…` link targets and all heading-slugs/`id=`s, assert every target is
present. Reason: this bug class is undetectable by eye across 3300-line reports but trivial for a test;
without it, §42.1 will regress the moment a section is renamed or added. *Fix: a golden-adjacent test
per format (md: slugify headings; HTML: parse `id=`) asserting link-target closure. Compute-cheap
(test), and it's the durable guard behind the §42.1 fix.*

### 42.4 Summary — a real broken-link bug, one root cause, one durable fix

Unlike most findings in this document (presentation/prose gaps), §42.1 is a **functional defect present
in the shipped sample**: triage "See X" links dead-end — `#overview` in Markdown, `#leaks`/`#top` in
HTML — because triage anchors are authored in a mix of two namespaces that disagree, and no format-aware
mapping reconciles them. The fixes collapse to one architectural change plus its guard:
- **P1:** introduce a single canonical `SectionId` (or key→per-format-slug table) that the ToC, HTML
  `id=`, and triage `anchor` all derive from — this fixes the live broken links in *both* formats at once
  (§42.0, §42.1). This is the same "one source of truth" discipline §29.2 asked of the graphs renderer.
- **P2:** add the cross-format anchor-resolution test (§26.6, §42.3) — the durable guard so it can't
  regress; and either truly slugify the ToC or correct its misleading "lock-step" comment (§42.2).

Twenty-third pass, and for once the through-line bends: this is not a "rich data, thin prose" gap but a
genuine bug — yet its *cause* is still a communication failure between three components that each hold
their own copy of the anchor vocabulary instead of sharing one. The fix is to make them speak the same
names.

---

## 43. Parse-Pipeline Error & Robustness Audit (pass 24)

Every prior pass audited what the tool *says*; this one audits what it does when the input is **hostile,
truncated, or from-the-future** — the failure modes a user actually hits when a copy goes wrong or a new
JVM emits a record we don't know. Grounded in `src/reader.rs`, `src/pass1.rs`, and the error-surfacing
funnel in `src/main.rs`.

### 43.0 What's genuinely hardened — credit where due

The byte layer is defensively written, not naively:

- **No double-open / no TOCTOU.** `HprofReader::open` sniffs the gzip magic, then *stitches the two peeked
  bytes back onto the front of the same stream* via `Cursor::new(magic).chain(peek)` (reader.rs:37–45)
  rather than re-opening the path. The comment (37–39) names the reason: avoids a double-open and a
  TOCTOU window, and works on inputs that can only be opened once (pipes). This is careful engineering.
- **Header validation is explicit, not `unwrap`.** A bogus `id_size` (anything but 4 or 8) returns a
  named `InvalidData` — *"unsupported id_size in HPROF header: {n} (expected 4 or 8)"* (reader.rs:70–75) —
  instead of silently reading garbage-width ids for the rest of the file.
- **Truncation is a first-class, distinguished condition.** `ensure()` maps a short read to
  `UnexpectedEof "unexpected eof"` (reader.rs:112–117), and the top-level scan loop treats EOF *at a
  record boundary* as a clean stop (`Err(e) if e.kind()==UnexpectedEof => break`, pass1.rs:191–194) while
  EOF *mid-record* propagates as an error. That's the correct distinction: a dump that ends cleanly on a
  record boundary is complete; one that stops mid-record is truncated.
- **The 16-EiB allocation trap is explicitly defused.** `STRING_IN_UTF8` computes its payload length as
  `length.checked_sub(id_size)` (pass1.rs:206–211) with a comment spelling out the attack: a corrupt
  record with `length < id_size` would underflow to ~`u64::MAX`, triggering a ~16 EiB `read_bytes` (abort
  or OOM). It's rejected as `InvalidData` instead. Someone thought about malformed input here.
- **Unknown *top-level* record tags are skipped, not fatal.** pass1.rs:289–291 `skip(length)` on any
  unrecognized record tag — the right forward-compat choice, since HPROF reserves room for new record
  types and the outer framing is self-describing (`length` tells us how far to skip).

That is a materially better robustness posture than most heap tools, which `mmap` the whole file and index
blindly. Worth stating plainly so the criticisms below read as polish on a solid base, not alarm.

### 43.1 THE ASYMMETRY: unknown *heap sub-tags* are fatal while unknown *records* are skipped (new P2)

The forward-compat generosity of the outer loop (skip unknown record, §43.0) **reverses** one level down.
Inside a heap-dump segment, an unrecognized sub-tag is a hard stop:

```
// src/pass1.rs:595–599
other => {
    return Err(io::Error::new(
        ErrorKind::InvalidData,
        format!("unknown heap sub-tag: 0x{other:02x}, remaining={remaining}"),
    ));
}
```

This is defensible *in isolation* — heap sub-records have no self-describing length prefix the way outer
records do, so on hitting an unknown sub-tag the parser genuinely cannot know how many bytes to skip;
continuing would desync the stream and produce garbage. Bailing is the safe choice. **But the consequence
is under-communicated and the trade-off is invisible to the user:** a single unknown sub-tag from a newer
JVM (or a vendor extension) aborts the *entire analysis* of an otherwise-complete multi-gigabyte dump, and
the message — *"unknown heap sub-tag: 0x2c, remaining=848321"* — reads like an internal assertion, not a
"this dump uses a heap-record type this build doesn't understand; it may be from a newer JVM" explanation.
Contrast the truncation path (§43.2), which *does* translate its terse condition into a user-facing hint.
- **Have (message):** the tag byte and remaining count are already in hand at the error site.
- **Fix (F, prose):** widen `analyze_error_hint` to recognize the `"unknown heap sub-tag"` prefix and add a
  hint — *"this dump contains a heap-record type this build does not support (possibly a newer JVM or a
  vendor extension); analysis cannot continue safely because sub-records are not length-delimited."* Same
  one-branch pattern already used for EOF and the not-HPROF-magic case.

### 43.2 The error-hint funnel covers EOF and wrong-file, but NOT `InvalidData` (new P2)

`analyze_error_hint` (main.rs:540–557, the analyze-path counterpart to `render_error_hint`) is the single
place a raw `io::Error` becomes an actionable sentence. It special-cases exactly two conditions:

1. input exists but lacks HPROF magic → *"…does not start with the HPROF magic; if it is a saved report
   JSON, rename it without the .hprof extension…"* (541–546);
2. `UnexpectedEof` on a real dump → *"…appears truncated or corrupt — the parser hit end of file
   mid-record; re-copy the .hprof dump and retry"* (550–555).

Everything else falls through to the bare `msg` (556). That "everything else" includes **all the
`InvalidData` cases the pipeline deliberately raises**: the bad-`id_size` header error (reader.rs:70), the
16-EiB-guard string error (pass1.rs:209), and the unknown-sub-tag error (pass1.rs:597). So the pipeline
carefully constructs precise machine-readable failure conditions, and then the funnel that exists *for the
express purpose of humanizing them* passes them through untouched. The one error class most likely to mean
"your JVM/dump is doing something this build doesn't handle" gets the least help.
- **Compute-cheap (F):** add an `InvalidData` arm to `analyze_error_hint` that, at minimum, prefixes
  *"'{input}' parsed as HPROF but a record was malformed or unsupported:"* before `msg`, so the user knows
  the file *was* a real dump (magic matched, header read) and the failure is structural, not a wrong-file
  mistake. The two existing branches already prove the pattern and cost.

### 43.3 `num_frames`/array-count loops trust attacker-controlled counts before reading (new P3)

Several record parsers read a count, then loop that many times issuing reads — e.g. `STACK_TRACE` reads
`num_frames = r.u4()` and immediately `Vec::with_capacity(num_frames as usize)` then pushes `num_frames`
ids (pass1.rs:251–255). A corrupt `num_frames` of `0xFFFFFFFF` requests a 4-billion-element `Vec`
reservation *before* a single frame id is read. Unlike the `STRING_IN_UTF8` case (§43.0, §43.1), there is
**no `checked_*` or sanity bound** here — the `with_capacity` can attempt a multi-gigabyte allocation on a
count that the subsequent reads would immediately fail to satisfy (the stream isn't that long), so the
failure mode is a large-allocation-then-EOF rather than a clean "malformed count" rejection.
- **Have:** the count and the reader's remaining length are both known at the call site.
- **Fix (C):** either drop `with_capacity(count)` in favor of a plain `Vec::new()` + `push` (lets the
  allocator grow naturally and caps peak at what's actually read), or clamp the reservation
  (`with_capacity(num_frames.min(SOME_SANE_CAP))`). Low severity — the STRING path was the dangerous one
  and is already guarded — but it's the same class of "trust the length prefix" bug, and consistency
  argues for closing it. Note as P3, not P2, because the trailing reads *do* eventually fail on a
  truncated stream; the only exposure is a transient over-reservation, not an unbounded read.

### 43.4 No corrupt/truncated-input test fixtures (new P2)

The robustness properties in §43.0 — clean-EOF-at-boundary vs error-mid-record, the id_size gate, the
16-EiB `checked_sub` guard, unknown-tag skip vs unknown-sub-tag fatal — are exactly the behaviors that
rot silently because *no happy-path test exercises them*. Grepping the pipeline (`src/reader.rs`,
`src/pass1.rs`, `src/pass2/*`) the `unwrap()`/`expect()` sites are **all inside `#[cfg(test)]` blocks**
against the *valid* embedded `DUMP` fixture (pass1.rs:972+, pass2/mod.rs:1493+); there is no fixture that
is truncated at a record boundary, truncated mid-record, carries a bad `id_size`, or contains an unknown
sub-tag. So a refactor that (say) accidentally turned the mid-record EOF into a `break` would pass every
test while silently producing a *partial* report labeled as complete — the worst possible failure for an
analysis tool.
- **Add (test):** three tiny fixtures derived by byte-truncating/mutating the existing `DUMP` const —
  (a) truncate on a record boundary → expect a *complete* report; (b) truncate one byte into a record →
  expect the truncation hint (§43.2); (c) flip one heap sub-tag byte to an unused value → expect the
  unknown-sub-tag error (§43.1). These lock in the very distinctions §43.0 credits, so they can't silently
  invert. Pairs with the anchor-resolution test (§42.3, §26.6) as the two highest-value missing guards.

### 43.5 Summary — a robust core whose failures are muffled at the last inch

Pass 24 inverts the usual finding: the parse pipeline's *internals* are the most defensively-engineered
code in the repo (TOCTOU-safe open, explicit header gate, the 16-EiB `checked_sub` with an attack comment,
the boundary-precise clean-EOF-vs-truncation distinction). The gaps are all at the **edges**:

- the humanizing funnel (`analyze_error_hint`) covers EOF and wrong-file but drops every `InvalidData`
  the pipeline so carefully raised (§43.2) — so the most "your JVM is newer than this build" of all
  errors gets the least explanation;
- the unknown-*sub*-tag abort is correct but its *rationale* (sub-records aren't length-delimited, so we
  can't skip) never reaches the user (§43.1);
- one count-loop still trusts an unbounded length prefix the sibling string path already learned not to
  (§43.3);
- and none of these behaviors is pinned by a corrupt-input fixture, so they can silently invert on the
  next refactor (§43.4).

So the through-line holds after all, one level deeper than usual: not "rich data, thin prose" but **robust
mechanism, thin communication of failure**. The engine handles the hostile input correctly and then
describes what happened in the terse language of `io::Error` instead of the user's language of "your dump
is truncated / from a newer JVM / not actually a dump." Four small edits — one hint arm, one message
widening, one `with_capacity` clamp, three fixtures — bring the *reporting* of failure up to the standard
the *handling* of failure already set.

---

## 44. ASCII Chart Honesty & Legibility Audit (pass 25)

`--format md-graphs` exists to make the numbers *visual* — its whole reason to be over plain `md` is the
bars and sparklines. §29 audited how sections are wired; this pass audits the **rendering primitives
themselves** (`bar`, `sparkline`, `tree_prefix` in `src/md.rs`) and asks the only question that matters for
a chart: *does the picture tell the truth about magnitude, and can the reader decode it?* Grounded in
`src/md.rs:160–216`, the 9 call sites in `src/report/render_graphs.rs`, and the shipped sample
`docs/samples/scala-doku-full.graphs.md`.

### 44.0 What's genuinely right about the primitives

- **`bar` is deterministic sub-cell integer math.** It works in eighths (`total_eighths = v*width*8/max`,
  md.rs:180) so a 16-cell bar has 128 distinguishable levels, computed in `u128` to avoid overflow on
  multi-GB byte values, with `value` clamped to `max` (md.rs:177) — no panics, no float non-determinism.
  The endpoints are correct and tested (`bar(10,10,4)="████"`, `bar(20,10,4)` clamps, md.rs:299–303).
- **Fixed column width.** `GRAPH_BAR_WIDTH = 16` (render_graphs.rs:98) with the comment noting columns stay
  aligned regardless of value — so bars form a clean vertical edge in a monospaced table, which is exactly
  what makes them scannable. `bar_is_fixed_display_width` (md.rs:290) pins this.
- **Sparkline keeps a visible baseline.** An all-zero series renders `▁▁▁` rather than blanks
  (md.rs:206–207), and the top is guarded so `v==max` lands on `█` (md.rs:211–213). Sensible choices.

So the drawing layer is well-built. The problems are not bugs in the glyph math — they are **what the math
is applied to**, and **what the reader is (not) told about the scale**.

### 44.1 CORE: every bar column normalizes to its own table's local max — no cross-table comparability (new P2)

There are **nine independent `max` computations**, one per bar column: the two anonymous `let max` for GC
Roots and Heap Composition (render_graphs.rs:204, 228), `hist_max` (258), `lmax` (306), `rmax` (374),
suspects `max` (491), `obj_max` (657), `cls_max` (694), `bmax` (749), `pkg_max` (775). Each bar is filled
to `value / (that table's leader)`. The consequence: **a full bar `████████████████` means a different
absolute quantity in every table**, and nothing on the page says so.

Concrete, from the sample: in Biggest Classes (line 275) `java.lang.Thread` at **22.9 MB** retained draws a
full bar. In Heap Composition (line 105) `Instances` at **19.7 MB** *also* draws a full bar. A reader
scanning the two tables sees two identical full bars representing 22.9 MB and 19.7 MB — and worse, the
Thread's 22.9 MB is a **dominator-tree artifact** (it's a GC root that retains much of the graph), not 22.9
MB of `Thread` objects, while Instances' 19.7 MB is a real shallow-size total. The chart visually equates
two numbers that aren't even the same *kind* of quantity.
- **Why it matters for the tool's mission:** the entire point of md-graphs is "see where the heap is at a
  glance." Per-table normalization means the eye *cannot* compare across sections — the one thing a visual
  format should enable. A user hunting "where is my heap" gets a page of full bars, each locally true, that
  don't compose into a global picture.
- **Have (the totals):** `SystemOverview` already carries `total_shallow` (passed into
  render_top_consumers_graphs at render_graphs.rs:650) and the retained total is derivable from the
  dominator roots. So an *absolute* reference exists.
- **Fix (C, compute-cheap):** for the retained-heap bar columns (Biggest Objects/Classes, Leak Suspects,
  Class Loaders, package tree), normalize to a **single shared retained-heap denominator** (e.g. total
  live retained) rather than the per-table leader, so a full bar always means "≈100% of the heap." Keep
  per-table max only where the axis is genuinely a different unit (counts vs bytes vs bp). At minimum,
  **print the max under each bar column** — e.g. `bar column scaled to 22.9 MB (this table's largest)` —
  so the reader knows the yardstick changed. One caption line per table; the value is already in `*_max`.

### 44.2 Linear bars on power-law data flatten the tail into invisibility (new P2)

Heap retained-size distributions are heavy-tailed: the sample's Biggest Classes (lines 275–286) run 22.9 MB
→ 3.6 MB across the top 12, and the true tail (ranks 13+) is far smaller. With a **linear** scale to the
leader, rank 5 (`HashSet`, 8.0 MB) already draws only `█████▌` and by rank 12 (`Solver$Clause`, 3.6 MB)
it's `██▌` — and anything below ~1.4 MB (one cell = 22.9/16 ≈ 1.43 MB) rounds to a **single sliver or
blank**. The String Length table (lines 213–227) shows the degenerate end explicitly: buckets `≤256`
through `≤32768` (values 35, 9, 4, 1, 1, 4, 1) all render **blank** — the bar says "nothing here" when
there are in fact 55 strings up to 17,989 bytes long, i.e. exactly the *long strings a waste-hunter cares
about*.
- **Why it matters:** a linear bar answers "who is biggest" (which the sorted order already told you) but
  destroys "how is the mass distributed" — the question that reveals *diffuse* waste (many mid-size
  offenders) vs *concentrated* waste (one whale). For a heap tool, the tail is where the recoverable waste
  often hides, and the linear bar erases it.
- **Fix (C):** offer a log-scaled bar variant for the size-distribution and histogram tables (a `bar_log`
  that maps `log(value)/log(max)`), OR — cheaper and honest — keep linear but **guarantee a minimum
  visible glyph for any nonzero value** (render `▏` whenever `value>0 && bar()` would round to blank), so
  "nonzero but small" is never visually identical to "zero." The String table already half-does this at the
  head (`≤1` bucket of 78 → `▏`, line 213) but not the tail; make it uniform.

### 44.3 Sparkline collapses "small nonzero" into "zero" and the zero-vs-tiny distinction the *sibling bar column* preserves (new P2)

The sparkline maps `idx = (v*7)/max` (md.rs:212), integer floor into 8 levels. In the sample's String
Length Distribution the sparkline is `▁▁▂▄▅▇█▂▁▁▁▁▁▁▁` (line 209) over the same 15 buckets the table lists
(213–227). Decoding against the table exposes two defects:

1. **Zero and smallest-nonzero are the same glyph.** `max=5295` (bucket ≤64). Bucket `≤1` has 78 values →
   `(78*7)/5295 = 0` → `▁`. Buckets `≤2048`/`≤4096`/`≤32768` have **1** value → also `▁`. So the seven
   trailing `▁▁▁▁▁▁▁` mix *78-value* and *1-value* and (if any) *0-value* buckets indistinguishably —
   while the bar column in the very same section correctly renders bucket-78 as `▏` and the 1-value buckets
   as blank. The two visualizations of identical data **disagree**, and the sparkline is the less honest of
   the two.
2. **No axis, no labels, no anchor.** The bare backtick line `▁▁▂▄▅▇█▂▁▁▁▁▁▁▁` has nothing telling the
   reader that glyph 1 = "length ≤1" and glyph 7 = "length ≤64". A sparkline is legitimate as a *shape
   preview* only if the reader can map position → bucket; here they'd have to count glyphs against the
   table below and hope the ordering matches (it does, but nothing says so).
- **Fix (C, two small edits):** (a) in `sparkline`, lift any nonzero value to at least level 1 the way
  `bar` should (§44.2) — e.g. `idx = if v>0 { ((v*7/max).max(1)) } else { 0 }` after reserving `▁` for
  true zero and starting nonzero at `▂` — so tiny ≠ empty; and (b) since the sparkline always precedes a
  labeled bucket table, either drop the standalone sparkline (the bar-column table already shows the shape
  *with* axis labels, making the sparkline pure redundancy — see §44.4) or add a one-line caption
  `low→high by bucket; see table below`.

### 44.4 The sparkline is redundant with the bar-column table it always precedes (new P3)

In every place a `sparkline` is emitted it is immediately followed by a `bar()`-column table over the same
counts: String Length Distribution (sample 209 sparkline, 211–227 bar table), and Top-Dominator Size
Distribution (render_graphs.rs:745 sparkline then 758 bar table — the comment at 722–723 even says the bar
table "mirrors" it). So the reader is shown the same distribution twice, once without labels (sparkline)
and once with (bar table). Given §44.3's finding that the sparkline is the *less* accurate of the two, the
duplication isn't just redundant — it shows a **worse** rendering next to a better one.
- **Why it matters for the "no duplication" mandate:** this is a literal instance of the anti-duplication
  goal. Two glyph-charts of one dataset, adjacent, differing only in fidelity.
- **Fix (F):** drop the standalone sparkline where a labeled bar-column table follows; reserve sparklines
  for the one place they earn their keep — a *trend across dumps* in the `compare` output, where there is
  no room for a full table and the shape-over-time is the whole point (and where §37 noted the diff output
  currently has *no* visual at all). Moving the sparkline there kills a duplication and fills a real gap.

### 44.5 `tree_prefix` is correct but the package tree still leans on per-table `pkg_max` (Have/no-fix-needed + one note)

`tree_prefix` (md.rs:227) is the one primitive with no magnitude concern — it draws structure, not
quantity, and its `├─/└─/│  ` logic with `ancestors_continue` flags is exactly right for a monospaced tree.
The only note: the retained-heap bar *inside* each tree row (render_graphs.rs:820) uses `pkg_max` (the
largest single package node), so it inherits §44.1 — a full bar on a leaf package means "biggest package,"
not "biggest thing in the report." Folding it into the shared-denominator fix (§44.1) makes the tree's bars
comparable to the flat tables' bars, which is valuable precisely because users pivot between "by package"
and "by class" views hunting the same heap.

### 44.6 Summary — the drawing is exact, the *scale* is unstated and the *tail* is erased

Pass 25 keeps the through-line but sharpens it to the visual format's own terms. The glyph math is
deterministic, overflow-safe, and tested (§44.0). What's missing is everything *around* the glyphs:

- **no shared denominator**, so a full bar means a different quantity in each of nine tables and the eye
  can't compare across sections — the one thing a visual format must enable (§44.1);
- **linear scale on power-law data**, so the long tail where diffuse waste hides renders blank (§44.2);
- **the sparkline is strictly less honest than the bar table beside it** (zero ≡ tiny; no axis) *and*
  duplicates it (§44.3, §44.4);
- and the tree's bars inherit the same local-max ambiguity (§44.5).

The fixes are cheap and mostly compute-side: one shared retained denominator feeding the byte-bar columns
(with a per-table caption where the unit genuinely differs), a minimum-visible-glyph rule so nonzero is
never blank, and relocating the sparkline from "redundant preview" to "the diff output's missing trend
line." None require new heap passes — the totals are already in `SystemOverview`. The result would make
md-graphs do what it promises: let a user *see*, across the whole report and down into the tail, where the
heap actually is and where it's being wasted — not just where each table's local winner sits.

---

## 45. Triage Threshold & Percentage-Basis Consistency Audit (pass 26)

§26 enumerated the triage *rules* and flagged that thresholds are undocumented (§26.7). This pass audits the
one thing §26 didn't: the **arithmetic the rules share** — the single `pct_of` helper, the ~55 magic
constants at the top of `src/report/triage.rs`, and the *numerator/denominator/label* triple that every
"% of heap" phrase in the triage output is built from. The finding is that the triage layer **hardcodes
the §27.1 "% Heap" category error into the first prose a user reads**, and does so *inconsistently* across
rules, so two signals that quote "% of heap" can be measuring different things.

### 45.0 What's right — the constants are centralized and the helper is honest about its base

- **Every threshold is a named `const` at the top of the file** (triage.rs:20–127) rather than scattered
  inline. That's the right structure — one place to tune, and §26.7's "document provenance" fix has a
  single obvious home.
- **`pct_of` is documented truthfully.** Its doc comment (triage.rs:187) says *"Percentage of total
  reachable **shallow** heap. Basis matches the report tables."* — so the helper itself does not lie; it
  correctly states it divides by shallow-heap total. The lie is downstream, in the *rule prose* that calls
  it (§45.1).
- **The two `total_shallow` sources are provably equal.** Rules variously read `r.leaks.total_shallow`
  (triage.rs:225, 275, 335, 480, 1100) and `r.overview.total_shallow` (694, 740, 830, 1031, 1063). I
  checked the builder: `build.rs:2053` computes the leaks denominator with the comment *"consistency with
  build_system_overview's total_shallow"* and both are the same sum of reachable shallow bytes. So this is
  **not** a split-denominator bug — worth stating so a future reader doesn't "fix" a non-problem. (But see
  §45.3: relying on a comment to keep two fields equal is itself fragile.)

### 45.1 CORE: `pct_of` divides *retained* by *shallow*, and the prose calls the result "% of reachable heap" (new P1)

`pct_of(retained, total)` computes `retained / total * 100` (triage.rs:190). Its callers pass a **retained**
numerator against the **shallow** `total_shallow` denominator in the majority of rules:

- Headline retainer: `pct_of(s.retained, total)` → *"retains {bytes} ({:.1}% of reachable heap)"*
  (triage.rs:241, prose 237); and the fallback branch `pct_of(o.retained, total)` (254, prose 251).
- Concentration: `pct_of(s.retained, total)` (307).
- GC-root dominance: `pct_of(top.retained, total)` (337).
- Thread pinning: `pct_of(t.retained, total)` → *"({:.1}% of heap)"* (482, prose 498).
- Unbounded collection: `pct_of(ret, r.leaks.total_shallow) >= UNBOUNDED_COLL_PCT` (898).
- Static anchor: `pct_of(s.retained, total)` → *"({:.1}% of heap)"* (1101, prose 1110).
- JNI global: `pct_of(retained, total)` → *"({:.1}% of heap)"* (1039, prose 1047).

This is exactly the §27.1 P0 category error, but *worse-placed*: it is not in a deep table, it is in the
**triage signals** — the summary a user reads first and trusts most. A retained-over-shallow ratio has no
clean interpretation and **can exceed 100%** (retained sets nest and overlap; the shallow total does not
bound any single object's retained size). In the sample, the Headline retainer prints
*"`java.lang.Thread` … retains 22.9 MB (76.3% of reachable heap)"* — but 22.9 MB is a retained figure and
the 30.0 MB denominator is a shallow total, so "76.3%" is a retained/shallow quotient dressed up as a share
of the heap. If a single dominator retained more than the shallow total (entirely possible when off-heap or
double-counted graph mass is involved), the same line would print ">100% of reachable heap" and destroy
user trust in the whole report.
- **Have:** the fix is definitional, not computational — it's the same canonical-base decision §27.1
  already calls a P0. Once "reachable heap" is defined once (retained-of-root or shallow-total, pick one),
  the triage prose must reference *that* base and `pct_of` must take a matching numerator.
- **Fix (F, prose + one helper signature):** either (a) rename the helper to `pct_of_shallow` and change
  every "% of reachable heap"/"% of heap" phrase to *"% of shallow heap"* (honest, cheap, no recompute),
  or (b) if the canonical base becomes retained-of-root, divide retained numerators by *that*. Do **not**
  leave retained ÷ shallow labeled as a heap share — that is the single most-read wrong number in the tool.

### 45.2 The label wording is inconsistent across rules for the *same* computation (new P2)

Even setting aside the numerator problem, the *phrase* attached to `pct_of` output varies rule-to-rule:

- *"% of reachable heap"* — Headline retainer (237, 251), Heap-skew bulk-data (1082).
- *"% of heap"* — Thread pinning (498), Object swarm (713), JNI global (1047), Static anchor (1110).

Same helper, same base, three surface forms ("reachable heap", "heap", and the §41.3 "0.0%" degeneracy on
top). A reader comparing two triage bullets cannot tell whether "% of heap" and "% of reachable heap" are
the same denominator (they are) or two different ones (they look like they might be). This is the triage-
layer instance of the §35 consistency problem and the §27.1 labeling problem combined.
- **Fix (F):** pick ONE phrase — whatever §27.1 settles on — and use it verbatim in all seven rules.
  A single `const HEAP_BASIS_LABEL: &str` referenced in every `format!` guarantees they can't drift.

### 45.3 Two fields kept equal only by a comment — no assertion, no shared source (new P3)

§45.0 confirmed `leaks.total_shallow == overview.total_shallow`, but the guarantee rests on a *comment*
(build.rs:2053) and two independent summation loops (build.rs:1346 region and 2055). Nothing enforces it:
a future change to how one of the two is accumulated (e.g. one starts excluding a class, the other doesn't)
would silently make the two denominators disagree, and then the triage rules reading
`overview.total_shallow` would quote a different "% of heap" than those reading `leaks.total_shallow` — the
exact split-denominator bug §45.0 says doesn't *currently* exist. This is a latent version of §35's drift
trap.
- **Fix (C):** compute the reachable-shallow total **once** and have both structs borrow/copy from that
  single value (or add a `debug_assert_eq!` in the builder). Cheap insurance for a number that backs every
  triage percentage and every table share.

### 45.4 Threshold *units* are mixed (f64 %, u32 bp, raw bytes, counts) with no naming convention (new P3)

The constant block mixes four incompatible units with no suffix discipline:

- **f64 percent:** `CONCENTRATION_PCT=50.0`, `THREAD_PIN_PCT=20.0`, `BOXED_PCT=5.0`, … (compared against
  `pct_of` output).
- **u32 basis points:** `OVERCAP_FILL_BP=5000`, `SPARSE_ARRAY_FILL_BP=2_000`, `COLLISION_HIGH_BP=9_000`
  (compared against model `*_bp` fields).
- **raw byte floors:** `DBB_FLOOR_BYTES`, `DUP_STRINGS_FLOOR_BYTES`, `CONSTARR_FLOOR=8*1024*1024`, …
- **raw counts:** `BOXED_FLOOR_INSTANCES=5_000_000`, `THREAD_SWARM_FLOOR=1000`, `SESSION_FLOOR=100_000`, …

The `_BP`/`_PCT`/`_BYTES`/`_FLOOR` suffixes *mostly* encode the unit, but not reliably: `OVERCAP_WASTE_PCT`
(a percent) sits beside `OVERCAP_FILL_BP` (basis points) for the *same* rule (triage.rs:43,47), so the
overcap rule compares one field in bp and another in percent — easy to cross up in a future edit. And
`SPARSE_ARRAY_WASTED_PCT=5.0` (percent) vs `SPARSE_ARRAY_FILL_BP=2_000` (bp) is the same hazard in the
sparse-array rule.
- **Why it matters:** §26.7 asked for *documented* thresholds; but documentation won't stop a bp-vs-pct
  mix-up. The 100× unit gap between bp and pct means a transposition (comparing a bp field against a pct
  const) fails *silently* — 2000 bp read as 2000% or 20% depending which side is wrong, and both "pass."
- **Fix (F + C):** (a) enforce the suffix convention so every `const` name states its unit, and (b) add a
  one-line comment per const giving *why that number* (the §26.7 provenance ask) — e.g.
  `// >50% retained by one object ⇒ concentration; MAT uses a similar dominator-share heuristic`. The
  provenance table §26.7 wants and this unit-discipline fix are the same edit done once per constant.

### 45.5 Summary — the category error lives where it does the most damage

Pass 26 finds that the §27.1 "% Heap" mislabel is not merely a table-header wart — it is **compiled into
the triage prose**, the highest-trust, first-read output, via a `pct_of` helper that is honest about its
base while every calling rule relabels the result as a heap share (§45.1). The wording of that relabel is
itself inconsistent (§45.2), the shared denominator is kept equal only by a comment (§45.3), and the
threshold constants that gate every rule mix percent, basis points, bytes, and counts with a suffix
convention that breaks exactly where a rule needs two units at once (§45.4).

None of this needs a heap pass. The fixes are: define the heap base once (already the §27.1 P0), point the
triage prose and `pct_of` numerator at it, freeze the label in one `const` string, collapse the two
denominators to one value, and finish the §26.7 provenance comments with unit-suffixed names. The through-
line holds and sharpens: the engine computes a defensible number (shallow share) and then *tells the user
it computed a different one* (heap share) — a communication failure, but this time one that can print
">100%" and read as a bug in the user's application when it is a labeling bug in the report.

---

## 46. Collection Waste: Top Byte-Offenders & Single-Value Overhead (pass 27)

Direct response to a user request: *"the collections that wasted the most memory by not being filled or by
being a single value"* and *"the top offenders with the approximate locations."* This pass audits, against
`docs/samples/scala-doku-full.md` and `src/report/model.rs`, exactly how close the report is to answering
that — and specifies the one missing view that would answer it outright. The short version: the tool
**already has the location machinery and most of the raw data, but never ranks collection waste in bytes,
and never treats single-element collections as an overhead category** — so the user's two questions are
each ~80% answered by disjoint tables and 0% answered by any single ranked list.

### 46.0 What already exists — more than the model comment implies

The Collections section already renders, in the sample:

- **Collection Fill Ratio** (sample 2730–2748) — a used/capacity histogram *with a real byte `Wasted`
  column* (2736: `0–10%` fill → 28.8 KB wasted). So under-fill waste **is** quantified in bytes, in
  aggregate.
- **"Worst individual containers (most empty slots)"** (sample 2803–2809, 2837+) — this **is** a
  top-offenders-with-locations table: `java.util.HashMap#table` → `HashMap$Node[]`, Used 2,048 /
  Capacity 4,096 / **2,048 wasted slots** / 577.2 KB retained. The `Class#field` column is the
  "approximate location" the user asked for (backed by the owner-edge scan, model.rs:899–902, 925–929).
- **"Likely wasters by field"** (sample 2795–2801) — `FieldAttributionRow` rollup:
  `HashMap#table` across 349 containers → 13,239 wasted slots (model.rs:1001–1017).
- **Collections by Size** (sample 2756–2774) — element-count histogram; **204 collections of size ≤1**
  and 5,806 empty (`empty_count`, model.rs:864).

So the answer to *"do we have top offenders with locations?"* is: **yes for arrays/maps, via the "Worst
individual containers" tables** — the location is the `Class#field`, and the ranking exists. The gaps below
are about *what those tables rank by* and *what they omit*.

### 46.1 CORE: nothing ranks collection waste by *bytes* — the offender tables rank by *slots* (new P1)

Every "worst offender" and "likely waster" table ranks by **wasted slots**, not wasted bytes:

- "Worst individual containers" sorts by `Wasted Slots` (sample 2805 header; 2807 = 2,048 slots).
- "Likely wasters by field" sorts by `Wasted Slots` (sample 2797; `FieldAttributionRow.total_wasted_slots`,
  model.rs:1013–1017, whose own doc says *"Counts empty slots, not bytes"*).
- **Array Fill Ratio's byte `Wasted` column is literally always `0 B`** (sample 2782–2793, every row and the
  total) — object-array under-fill is never costed in bytes at all.

Why this fails the user's exact question ("wasted the most **memory**"): a slot is not a byte. An
`Object[]` slot is 4–8 B; a `HashMap$Node[]` slot is a pointer to a *node object* whose real cost is the
node, not the slot. Ranking by slots over-weights wide-but-cheap primitive gaps and under-weights
node-based maps. The user asked for *memory* wasted; the tool answers in *slots*. In the sample, the
#1 slot-offender (`HashMap#table`, 2,048 slots, 2807) may or may not be the #1 *byte* waster — the report
cannot say, because the byte figure is only computed in aggregate (Collection Fill Ratio, §46.0) and never
attached to the individual offenders.
- **Data availability — Compute-cheap for classified-array-backed collections, Add for the rest:** the
  "Worst individual containers" rows already carry `Capacity`, `Used`, and `Retained` (sample 2805). Wasted
  bytes ≈ `(capacity − used) × slot_width`, and `slot_width` is `id_size` for object/node arrays (known
  from the header) — so for these array-backed rows it is **arithmetic on data already in the row (C)**.
  For classified collections whose capacity isn't cheaply available (the model.rs:1014–1016 caveat), it
  remains an **Add**.
- **Fix (C, mostly):** add a `Wasted Bytes` column to "Worst individual containers" and "Likely wasters by
  field," computed as `(capacity − used) × slot_width`, and **sort by it**. Keep `Wasted Slots` as a
  secondary column. This directly produces "the collections that wasted the most memory by not being
  filled, with locations" — the user's first request, from data the offender table already holds.

### 46.2 Single-value / tiny collections are counted but never costed as overhead waste (new P1)

`CollectionsBySize` reports **204 collections of size ≤1** and 5,806 empty (sample 2758, 2762), but only as
histogram buckets — there is **no offender list and no overhead cost** for them. Yet a single-element
collection is a specific, common waste pattern: a `HashMap` holding one entry carries ~48 B of map object +
a 16-or-more-slot `Node[]` backing array + a `Node` — on the order of **≥90% pure overhead** for one
payload reference. The report's own Empty-collection triage bullet (sample line 67: *"5,806 … are empty …
waste object-header overhead; consider lazy initialisation or null"*) makes exactly this argument for the
size-0 case, but stops there — it never extends to size-1/tiny collections, and never *ranks which classes*
own the most of them.
- **Why it matters for the mission:** "where is heap wasted" for many real apps is *death by a thousand
  tiny maps* — per-request `HashMap`s with one entry, singleton `ArrayList`s, etc. This is diffuse waste
  that no "biggest objects" view surfaces (each is tiny) but that aggregates to real memory, and the fix
  (use `Map.of(k,v)` / `List.of(x)` / `Collections.singletonMap`) is concrete.
- **Data availability — Have/Compute-cheap:** `CollectionsBySize` already has the ≤1 and empty counts;
  the per-instance shallow sizes feeding the histogram are the same data. Grouping the size-0 and size-1
  buckets *by owning `Class#field`* reuses the identical owner-edge scan the array tables already use.
- **Fix (C):** add a "Tiny-collection overhead" sub-view: for size ∈ {0,1}, rank by
  `overhead_bytes = shallow_of_collection + shallow_of_backing_array` grouped by owning `Class#field`, with
  a headline *"N single/empty collections cost ≈ M MB in wrapper overhead — replace with `Map.of`/`List.of`
  or lazy-init."* This answers the user's second waste category ("by being a single value") with locations.

### 46.3 The three waste views are disjoint — no unified "top memory-wasting collections" (new P2)

Under-fill (Collection/Array Fill Ratio), collision slack (Map Collision Ratio), and tiny/empty
(Collections by Size) are three separate subsections, each with its own offender table sorted its own way,
none in bytes (§46.1). A user hunting "where is my collection memory wasted" must read three tables, mentally
convert slots→bytes, and merge them. The user's phrasing — *"the collections that wasted the most memory by
not being filled **or** by being a single value"* — is literally a request to **union these into one ranked
list.**
- **Fix (C, render-only):** a single "Top collection waste" table unioning the offenders from all three
  causes, one `Wasted Bytes` column (from §46.1/§46.2), a `Cause` column (`under-filled` / `single/empty` /
  `collision-slack`), and the `Class#field` location — sorted by wasted bytes descending. This is the one
  view that answers both halves of the request at once, and it composes cleanly with the §24 Waste Summary
  as its collection-specific drill-down (no new pass; reuses §46.1/§46.2 numbers).

### 46.4 Location precision: `Class#field` is a hint, and the report says so — keep that honesty (Have, note only)

The offender tables label the location caveat correctly: *"dominant incoming `Class#field` — a hint, not a
guarantee"* (sample 2795, 2830). That honesty is right and must survive the §46.1–46.3 changes: the
owner-edge is the *dominant* referrer, not necessarily the allocation site. Where a stronger location
exists — the suspect `root_path` / dominator chain (model.rs:537–549) — a future enhancement could link the
top byte-waster to its retaining path (the §30.2a field-labeled-path item), turning "held via
`HashMap#table`" into "held via `SessionCache.sessions → HashMap#table`." Not required for the ranked list;
noted so the "approximate" in the user's request stays honestly approximate rather than silently overclaimed.

### 46.5 Summary — the data and the locations exist; the ranking axis is wrong and one category is missing

The user asked two precise questions and the answers are: **(1) top offenders with locations — yes**, the
"Worst individual containers" tables already provide `Class#field` + container class + used/capacity/
retained (sample 2805–2809); but **(2) ranked by wasted memory — no**, they rank by *slots*, the object-
array byte-waste column is hardwired to `0 B`, and single/tiny collections are counted but never costed.

Three fixes, all compute-cheap and reusing existing owner-edge + capacity data (no new heap pass except the
classified-collection capacity edge case):
- add a `Wasted Bytes = (capacity − used) × slot_width` column and sort the offender tables by it (§46.1);
- add a tiny-collection (size 0/1) overhead ranking by owning `Class#field` (§46.2);
- union all three waste causes into one "Top collection waste" table, byte-sorted, as the §24 drill-down
  (§46.3) — while preserving the honest "`Class#field` is a hint" caveat (§46.4).

Same through-line, applied to a concrete user need: the engine already *found* the wasteful collections and
*where* they are held — it just ranks them by the wrong unit and omits the single-value case. Turning slots
into bytes and adding the tiny-collection view converts three diagnostic histograms into the direct answer:
*"these named collections, held here, waste this many megabytes — fix them."*

## 47. Location Attribution: `Class#field` for heap, `Class#method` for stack (pass 28)

The user asked for three things, in three messages: locations should read `Class#field` **or** `Class#method`
depending on whether the holder lives on the heap or the stack; both should be flagged as *a notion, not a
guaranteed allocation site* — but that qualifier should be **stated once**, not repeated on every table; and
there should be a **single table ranking the `Class#field` / `Class#method` holders that retain (or dominate)
the most memory**. This pass grounds all three in what the model already captures.

### 47.0 What already exists — the heap half is done, the stack half is measured but unnamed

Heap-resident ownership is already surfaced as `Class#field` everywhere it matters: the collection-waste
offender tables (sample 2749/2795/2830), the array owners (sample 2851/2921/2953), the collection owners
(sample 3088), the Container Attribution table (sample 2983–3019), and — most directly for the user's third
request — **"Fields by Retained Size (`Class#field`)"** (sample 3047–3051), which already answers *"which
holder `Class#field` retains the most memory"* (top row `scala…$colon$colon#next` → 84.65 GB additive across
146,181 holders). The backing struct is `FieldBySizeRow`/`FieldsBySize` (model.rs:1051–1091), gated on
`--collections`.

The stack half is *measured* but never *named as a location*:

- Every thread carries `frames: Vec<String>`, each rendered `class.method (source:line)` (model.rs:648).
- Under `--thread-locals`, `SignificantFrame { frame: "class.method (source:line)", locals: Vec<SignificantLocal> }`
  (model.rs:697–702) pins each significant local to the exact frame that holds it, and
  `SignificantLocal { display_class, retained, pct }` (model.rs:704–714) already carries the retained bytes.
- The Thread Overview even has a **`Max. Locals' Retained`** column (sample 2499–2504: `main` → 4.6 MB),
  proving stack-held retention is already computed per thread.

So the raw material for a `Class#method` location — *"this object is retained by the local `x` in
`Solver.solve (Solver.scala:214)`"* — is present in the model. It is simply never rendered as a location the
way `Class#field` is.

### 47.1 CORE: the biggest object in the sample is stack-held and shows no `Class#method` origin (new P1)

The single largest consumer is `java.lang.Thread` at 22.9 MB / 76.7% (sample 2323, Top Consumers). A thread's
retained heap *is* its stack — the objects reachable only through its frames' locals. The report says so
indirectly (Thread Overview `Max. Locals' Retained` 4.6 MB for `main`, sample 2501) but the Top Consumers row,
the Leak Suspects entry, and every "biggest single value" view show **no `Class#method`**: the reader is told
*what* dominates and *how much*, never *which method's local variable is pinning it*. For heap objects the
same views print the `Class#field` owner; for the stack-dominated object — the one that matters most here —
the location column is silent.

**Fix (mostly Have + Compute-cheap):** wherever a "single big value" is shown, resolve its holder to one of two
forms and print it in a single **Held via** column:

- **heap-resident** → `Class#field` (the existing dominant owner-edge, model.rs:583/899/925);
- **stack-resident** → `Class#method` (from the `SignificantFrame.frame` that retains it, model.rs:698 —
  strip `(source:line)` for the compact form, keep it on hover / in the detail view).

The classification is cheap: an object is stack-resident for this purpose when its dominator chain terminates
at a thread-local root (the GC-root kind is already known — model.rs GC-root `root_type`). Rendering the frame
as `Class#method` is a pure string transform of data the model already holds; the only **Add** is joining the
significant-frame table to the dominator, and only when `--thread-locals` is on (otherwise fall back to the
thread name, e.g. `Thread "main"`, so the column is never blank).

### 47.2 Apply to everything that deals with single big values — not just collections (new P1)

Per the user's second message, this is **not** a collections-only feature. The `Class#field`-or-`Class#method`
**Held via** column belongs on every view whose subject is one large object or value:

- **Top Consumers → Biggest Objects** (sample 2317–2342) — currently no owner column at all;
- **Leak Suspects** (the P0 headline retainers) — the 76.7% `Thread` suspect should read *held via
  `Solver.solve` (stack)* not just *`java.lang.Thread`*;
- **Biggest single collection instances** (sample 3084–3088) — already has `Owner (Class#field)`; extend it to
  print `Class#method` when the container is a stack local;
- **Largest arrays / constant arrays** (sample 2851/2921/2953) — same;
- **Duplicate strings / boxed-number offenders** (§8, §11) — a duplicated `String[]` or boxed `Integer` cache
  held by a frame local should attribute to the `Class#method`, not appear origin-less.

Everywhere the answer to *"where does this big value live?"* is either a heap field or a stack frame, and the
column should say which. This is the through-line of the whole document — *find where the heap comes from* —
applied uniformly instead of only to `--collections` output.

### 47.3 State the "notion, not guarantee" caveat once (Have — trim, do not proliferate)

The current honest caveat *"dominant incoming `Class#field` — a hint, not a guarantee"* is repeated on three
separate table captions (sample 2749, 2795, 2830). The user explicitly asked for it **once**, not on every
table. Consolidate: state it a single time — in the section preamble of whatever section first introduces the
**Held via** column (Top Consumers, or a short "Reading locations" note near §24) — as one sentence covering
**both** forms:

> _`Class#field` (heap) and `Class#method` (stack) are a **notion** of where a value lives — the dominant
> referrer or retaining frame, a hint for navigation, not a guaranteed allocation site._

Then drop the per-table repetition. This is a **Have** (delete two caption fragments, add one preamble line);
it makes the caveat *more* prominent by stating it as a definition rather than burying it three times in table
subtitles, and it now correctly covers the stack case the current wording never mentioned.

### 47.4 The retained-holder table: rank `Class#field` **and** `Class#method` together (new P1)

The user's third request — *a table of the `Class#method` and `Class#field` that hold / dominate the most
retained memory*. Half of it already exists ("Fields by Retained Size", sample 3047, ranks `Class#field` by
retained). The missing half is the stack side and the **union**:

| Held via                                   | Kind  | Retained | Instances / Frames |
| ------------------------------------------ | ----- | -------: | -----------------: |
| `Solver.solve (Solver.scala:214)`          | stack |  22.9 MB |                  1 |
| `…$colon$colon#next`                        | heap  |  84.6 GB* |            146,181 |
| `…BitmapIndexedSetNode#content`             | heap  |  32.8 MB |             22,791 |

(*the `$colon$colon#next` figure is *additive over all holders*, §47.0 / the FieldsBySize semantics — the
table must keep that "summed, may exceed live heap" caveat, cf. §45's percentage-basis discipline, else it
reads as >100%.)

**Availability:** the `Class#field` rows are **Have** (`FieldsBySize`, model.rs:1083, already rendered). The
`Class#method` rows are **Compute-cheap-to-Add**: sum `SignificantLocal.retained` per `SignificantFrame.frame`
across threads (model.rs:698–712), which is the per-frame retained the `Max. Locals' Retained` column already
aggregates at thread granularity — this pushes it down to frame granularity. Merging the two into one
retained-sorted table is a render-time concat. Gate the stack rows on `--thread-locals` (fall back to
thread-name granularity otherwise, matching §47.1). This table becomes the single answer to *"what holds the
most memory, and is it a field or a frame?"* — the top row in this sample is a **stack frame**, which is
exactly the fact the current field-only table cannot express.

### 47.5 Summary — name the stack the way the heap is already named

The heap side is done: `Class#field` locations are everywhere, and "Fields by Retained Size" already ranks
them. The stack side is fully *measured* (per-frame `class.method (source:line)`, per-local retained, thread
`Max. Locals' Retained`) but never *rendered as a location* — so the single biggest object in the sample, a
22.9 MB stack-held `Thread`, appears origin-less while a 9 KB heap array proudly prints its `Class#field`.

Four moves, all reusing captured data:
- print a **Held via** column that reads `Class#field` (heap) or `Class#method` (stack) on every single-big-value
  view — Top Consumers, Leak Suspects, biggest collections/arrays, dup-strings/boxed (§47.1–47.2, Have + one Add);
- state the "a notion, not a guarantee" caveat **once**, covering both forms (§47.3, Have — trim);
- add the stack half of the retained-holder ranking and **merge** it with the existing `Class#field` table into
  one retained-sorted "who holds the most" view (§47.4, Compute-cheap + gated Add);
- keep the additive/`>100%` caveat on the field rows so the merged table stays honest (§47.4 ↔ §45).

The engine already knows the biggest thing on the heap lives in a stack frame. §47 just makes it *say so*, in
the same `Class#…` vocabulary it already uses for fields.

## 48. "Total heap" Denominator & Label Consistency Audit (pass 29)

This pass is a *cross-section arithmetic* audit, distinct from §27 (which argued the "% Heap" **basis** is a
category error) and §45 (threshold/percentage-basis discipline). Here the question is narrower and entirely
verifiable against the committed sample: **does the phrase "total heap" mean the same number everywhere it
appears, and is the same scalar labeled the same way across the four formats?** It does not, and it is not.
Every claim below is grounded in `docs/samples/scala-doku-full.md`, its `.graphs.md` sibling, and the render
sources.

### 48.1 The same 29.8 MB scalar wears four different labels (new P1)

`SystemOverview.total_shallow` (model.rs:319) is a single value — 31,252,288 bytes → "29.8 MB". It is rendered
under **four distinct labels** across the report surfaces:

| Surface                                   | Label                       | Source                          |
| ----------------------------------------- | --------------------------- | ------------------------------- |
| md / graphs **Summary digest**            | `Total heap (reachable)`    | render_md.rs:359                |
| md **System Overview** detail table       | `Total shallow heap`        | render_md.rs:692                |
| graphs **System Overview** detail table   | `Total shallow heap`        | render_graphs.rs:123            |
| HTML **System Overview** `<dl>`           | `Total shallow heap`        | App.tsx:1161                    |
| HTML **KPI strip**                        | `Total heap`                | App.tsx:300                     |

So a reader who scrolls from the Summary ("Total heap (reachable) 29.8 MB", sample 40) to the System Overview
("Total shallow heap 29.8 MB", sample 85) sees the *same number* introduced twice under two names, with no
statement that they are the same quantity. The HTML KPI strip adds a third bare "Total heap". The three names
—"total heap (reachable)", "total shallow heap", "total heap"— are not obviously synonyms to a non-expert:
"shallow" vs "reachable" vs bare "heap" each imply a different scope.

**Reason it matters:** the whole document's thesis is *find where the heap comes from*. If the headline size of
"the heap" is named three ways, a reader cannot be sure the 76.7% share, the 29.8 MB total, and the composition
rows are all denominated in the same thing. **Availability: Have** — this is a pure label-consolidation. Pick one
canonical rendering (the §0b `HEAP_BASIS_LABEL` = "reachable heap" work already established the *denominator*
name; the *scalar* should match it) and use it in all four places: e.g. "Total heap (reachable, shallow) —
29.8 MB" once, then "reachable heap" everywhere the same number recurs. This dovetails with the existing
`HEAP_BASIS_LABEL` constant (format.rs) — extend it to the scalar's row label, not just the percentage suffix.

### 48.2 "% of total heap" uses a THIRD denominator — reachable + unreachable (new P2)

The Unreachable row prints "4,266 (673.0 KB; 2.2% of total heap)" (sample 89). The percentage is computed
(render_md.rs:697–699) as:

```
let total = o.total_shallow + o.unreachable_shallow;      // reachable + unreachable
format!(" {:.1}% of total heap", o.unreachable_shallow as f64 / total as f64 * 100.0)
```

So *this* "total heap" = **reachable + unreachable** (31,252,288 + 673.0 KB ≈ 31.9 MB), a different quantity
from the "Total shallow heap 29.8 MB" row six lines above it (reachable only). The label "% of total heap"
therefore refers to a denominator the report never prints as a line item. 673 KB ÷ 29.8 MB would be 2.20%;
673 KB ÷ (29.8 MB + 673 KB) is 2.15% → both round to "2.2%" in *this* sample, which is precisely why the bug is
invisible here and will surface on a dump with more garbage. On a heap that is 40% unreachable the two
denominators diverge by 40% and the "% of total heap" figure will look wrong to anyone who divides by the
printed 29.8 MB.

**Reason:** a percentage whose denominator is never shown and differs from the adjacent scalar is unverifiable
and, on fragmented heaps, misleading. **Availability: Have.** Either (a) relabel to "% of all heap (reachable +
unreachable)" so the base is explicit, or (b) print the reachable-only ratio to match the scalar directly above
it. Option (a) is preferable because it keeps fragmentation honest; whichever is chosen, the label must name the
base. This is the §27.5/§45.1 discipline applied to the one denominator those passes did not cover: the
unreachable base.

### 48.3 "Heap fragmentation (unreachable / total)" restates the exact same ratio under a new name (new P3)

Immediately below the Unreachable row, md prints "Heap fragmentation (unreachable / total) 2.2%" (sample 90),
from `heap_fragmentation_ratio` (model.rs:365, documented "unreachable shallow / total heap (reachable +
unreachable)"). That is the **same formula** as the "% of total heap" fragment on the line above
(render_md.rs:713–716 vs 697–699): both are `unreachable_shallow / (reachable + unreachable)`. The report thus
prints 2.2% twice, once appended to the Unreachable count and once as its own "Heap fragmentation" row — two
labels, one number, adjacent rows.

**Reason:** duplication the user explicitly asked to avoid ("without duplication"). **Availability: Have.** Drop
one of the two. Recommended: keep the standalone "Heap fragmentation" row (its label already parenthesizes the
formula) and strip the redundant "; 2.2% of total heap" suffix from the Unreachable row, leaving that row to
report the *count and bytes* only (which is unique information). Net effect: the Unreachable row says *how many
/ how big*, the fragmentation row says *what fraction* — no overlap.

### 48.4 The md ↔ graphs ↔ HTML label set diverges on this very block (new P2, parity)

The three renderers do not even agree on how much denominator context to show for these identical scalars:

| Row                    | md (render_md.rs)                          | graphs (render_graphs.rs)     | HTML (App.tsx)              |
| ---------------------- | ------------------------------------------ | ----------------------------- | --------------------------- |
| Unreachable            | `… (673.0 KB; 2.2% of total heap)` (699)   | `… (673.0 KB)` only (131)     | `… (673.0 KB)` only (1181)  |
| Heap fragmentation     | `Heap fragmentation (unreachable / total)` (715) | `Heap fragmentation` (139) | `Heap fragmentation` (1187) |

So the plain-md reader is told the fragmentation denominator ("unreachable / total") and given the inline
percent; the graphs and HTML readers get neither clarifier. This violates the standing comparability rule (the
same block must present the same data and the same labels in all four formats). It is not a data difference —
all three have `unreachable_shallow` and `heap_fragmentation_ratio` — it is a *rendering* divergence.

**Reason / fix:** once §48.2 and §48.3 pick the canonical wording, apply it identically in all three renderers
(and confirm JSON carries the raw `unreachable_shallow` + `heap_fragmentation_ratio` so the value is
reconstructable — it does, model.rs:344/368). **Availability: Have** (format-plumbing only, no model change, no
SCHEMA bump). The graphs/HTML rows simply gain the same clarifier text the md row already has.

### 48.5 One thing that IS consistent — note it so §35/§48 don't contradict

To keep this pass reconciled with the §35 consistency table: the Heap Composition rows **do** sum to the
headline total. 770,497 + 65,061 + 114,257 + 2,851 = **952,666** objects = "Total objects" (sample 84 vs
107–110), and 19.7 + 5.4 + 4.7 + 0.034 MB ≈ **29.8 MB** = "Total shallow heap" (sample 85). This confirms
`total_objects`/`total_shallow` are the **reachable** aggregates (composition is documented reachable-only,
model.rs:325) and that the composition is a faithful partition of them. The defect is purely in the *labels*
(§48.1) and the *unreachable denominator* (§48.2–48.4), not in the reachable arithmetic. This is worth stating
explicitly so the fix does not "correct" a total that is already right.

### 48.6 Summary and Priority-Summary deltas

- **§48.1 (P1, Have):** one canonical name for the `total_shallow` scalar across Summary / Overview / KPI in all
  four formats; reuse the `HEAP_BASIS_LABEL` vocabulary so the scalar and the "% Heap" denominator share a name.
- **§48.2 (P2, Have):** name the "% of total heap" base (reachable + unreachable) explicitly, or switch it to the
  reachable-only base that matches the adjacent scalar; today it silently uses a third, unprinted denominator.
- **§48.3 (P3, Have):** drop the duplicated fragmentation percent — it is printed twice under two labels.
- **§48.4 (P2, Have, parity):** apply the chosen wording identically in md / graphs / HTML; today only plain-md
  shows the denominator clarifier and the inline percent.
- **§48.5:** the reachable composition arithmetic is correct and must be preserved; the fix is label-only.

All five are **Have** (label/format work, no new field, no heap pass, no SCHEMA bump), and none change a
rendered *number* except §48.2, which only changes it on fragmented heaps where the current figure is already
wrong. This pass closes the last "what does 'total heap' mean here?" ambiguity that §27/§45 left on the
unreachable side of the ledger.

## 49. Threads Section: Local-Root & Significant-Frame Rendering Audit (pass 30)

The Threads section (`render_threads`, render_md.rs:1257–1376; shared by md + graphs; HTML `ThreadCard`
App.tsx:1890–1945) is one of the most information-dense parts of the report — thread 1 "main" in the sample
spans lines 2508–2567 (≈60 lines for one thread). It has real actionability (it is the only place that ties a
retained size to a *stack frame*, i.e. to code), but a walkthrough of the sample surfaces five defects: an
uncomparable-percentage basis, apparent-but-unexplained duplicate rows, a local-root sample that neither sums
nor says it is a sample, an inconsistently-present "Local roots" count line, and an md/graphs↔HTML parity gap
on how the whole block is disclosed. None of these are covered by earlier passes (§45/§48 audited *heap*
percentages and the *total-heap* label; the thread section's percentages use a different, per-thread base that
no pass has examined).

### 49.1 The significant-frame percentage uses a per-thread base but is printed like every other "% Heap" (new, P1, Have)

Each significant-frame local prints `` `Class` retains <bytes> (<pct>%) `` (render_md.rs:1361–1366; HTML
App.tsx:1931). The `pct` is documented as "Retained heap as a percentage of the **owning thread's** retained
heap" (model.rs:713–714), and the sample confirms it: thread 1 line 2557 shows
`` `cafesat.sat.Solver` retains 4.6 MB (20.2%) `` where 4.6 MB ÷ 22.9 MB (thread retained) = 20.1% — **not** 4.6
MB ÷ 29.8 MB (heap) = 15.4%. Thread 3 "Finalizer" line 2600 shows `` NativeReferenceQueue retains 40 B
(19.2%) `` where 40 B ÷ 208 B (thread retained) = 19.2% — a bare "19.2%" that looks alarming until you realise
the whole thread retains 208 B.

**Reason / fix:** every *other* percent in the report is "% of reachable heap" (now canonically
`HEAP_BASIS_LABEL`, §45.2/§48.1). This one silently switches base with no label, so a reader cannot compare
"20.2%" here against "20.2%" anywhere else, and the 47.6%/28.6%/76.2% figures on tiny threads (sample 2635/
2638/2647) read as major retainers when they are bytes. Fix (label-only, no value change): print the base once
per thread — e.g. `_Frame percentages are of this thread's 22.9 MB retained heap._` under the Thread heading —
**or** add a second heap-relative percent so the two bases are both visible and cross-comparable. The thread's
retained total is already in `ThreadInfo.retained` (model.rs:665) and the heap total is the §48.1 scalar, so
either fix is **Have** (render-only, no model change). Applies identically to md/graphs (render_md.rs:1362) and
HTML (App.tsx:1931).

### 49.2 Local-root table rows look like accidental duplicates because instances have no identity (new, P2, Have)

The Local root objects table (render_thread_locals, render_md.rs:1381–1400) lists one row per sampled local
with only `Object | Shallow | Retained`. In the sample, thread 1 shows `` `cafesat/sat/Solver` | 168 B | 4.6
MB `` **twice** (lines 2518–2519), `` `scala/runtime/ObjectRef` | 16 B | 962.1 KB `` twice (2522–2523), and
`` scala/collection/immutable/HashSet | 16 B | 16 B `` **three** times (2528–2530); thread 3 shows three
identical `NativeReferenceQueue` 40 B rows (2587–2589). These are distinct object *instances* (each has its own
`obj_index_1based`, model.rs:721), but the table drops that field, so a reader sees what looks like a rendering
bug — the same row repeated.

**Reason / fix:** either (a) collapse identical (class, shallow, retained) rows into one with a `×N` count
column — mirroring the `×N` collapse the plan already mandates for the dominator subtree (§28.1/§13.3) — so
"3× HashSet 16 B" is one honest row; or (b) surface the distinguishing `obj_index_1based` (e.g. `@ #12345`) so
the rows are visibly different instances. Option (a) is the better default (it shortens a 19-row table to ~12
and makes the multiplicity a *fact* rather than an eyesore). `obj_index_1based` is already in the model
(model.rs:721) so both options are **Have** (render-only). Must land in md/graphs (render_md.rs:1391) and HTML
(`ThreadLocalsTable`, App.tsx:1846–1852) together.

### 49.3 The local-root table is a bounded sample but neither sums nor says so (new, P2, Have)

Thread 1's header says `_Local roots: 124._` (sample 2512) but the table lists only 19 rows (2517–2537) — the
list is capped by `--detail` (documented model.rs:656–657) yet nothing in the *rendered* output says "showing
19 of 124" or "sampled". Worse, the 19 retained values do not and cannot sum to the thread's 22.9 MB (they
overlap — `Solver` 4.6 MB is likely an ancestor of the `$colon$colon` 3.2 MB), but a reader naturally tries to
add them. There is no total row and no "sample" caveat.

**Reason / fix:** add a truncation line when `local_objects.len() < local_root_count` — `_Showing top 19 of 124
local roots by retained heap; sizes overlap and do not sum to the thread total._` — reusing the retention-
overlap caveat the plan wants stated once (§47.3). Both counts are already in the model
(`local_objects.len()` and `local_root_count`, model.rs:654/659), so this is **Have** (render-only). This is
distinct from §22 row 6.1 ("cap per-thread local roots at top-10"), which is about *capping*; this is about
*disclosing* that a cap was applied and that the numbers overlap. md/graphs (render_md.rs:1352) + HTML
(App.tsx:1920) together.

### 49.4 The "Local roots: N" count line is emitted for some threads but not others (new, P3, Have)

The count line is gated on `local_root_count > 0` (render_md.rs:1344). Thread 2 "Reference Handler" (sample
2571–2575) has no count line and jumps straight from heading to a bare frame list, while threads 1/3/6 all show
`_Local roots: N._`. A reader scanning the section cannot tell whether thread 2 has *zero* resolved locals or
whether the line was simply omitted — the absence is ambiguous.

**Reason / fix:** always emit the line, printing `_Local roots: 0._` (or `_No resolved local roots._`) when the
count is zero, so the section is structurally uniform per thread and the zero is an explicit statement rather
than a silent gap. This mirrors the §31.4 "empty-section emptiness-check" principle applied per-thread.
**Availability: Have** (drop the `> 0` guard, render-only). md/graphs (render_md.rs:1344) + HTML (App.tsx: the
meta-row already always shows retained, so add an explicit locals count) together.

### 49.5 md/graphs disclose the whole thread inline; HTML hides locals behind a nested `<details>` — a parity gap (new, P2, Have, parity)

In md/graphs the local-root table renders inline and unconditionally (render_md.rs:1353). In HTML the same data
is wrapped in a collapsed `<details><summary>Local root objects (N)</summary>` (App.tsx:1849–1850), *nested*
inside the already-collapsed per-thread `<details>` (App.tsx:1896). So a reader who expands thread 1 in the
HTML report still does not see the local roots — they must find and expand a second disclosure. The three
formats therefore present the *same data* at *different* default visibility, violating the comparability rule
(md shows it, HTML hides it two levels deep).

**Reason / fix:** pick one disclosure policy and apply it across formats. Given md shows locals inline, the HTML
`ThreadLocalsTable` should render the table open-by-default (or inline, no nested `<details>`) once its parent
thread card is expanded — the nesting is what breaks parity, not the outer per-thread collapse (which md lacks
but which is a legitimate HTML affordance for hundreds of threads). Alternatively, if collapsing is kept for
very large local sets, apply the *same* cap-then-collapse threshold in md (§49.3's truncation line). Either way
the *default-visible content* must match. **Availability: Have** (HTML render change in `ThreadLocalsTable`,
App.tsx:1849; no model change, no SCHEMA bump).

### 49.6 Summary and Priority-Summary deltas

- **§49.1 (P1, Have):** label the significant-frame percentage's per-thread base (or add a heap-relative
  percent) — today it silently uses a different denominator than every other "%" in the report.
- **§49.2 (P2, Have):** collapse identical local-root rows with a `×N` count (or surface `obj_index_1based`) so
  distinct instances stop reading as duplicate rows.
- **§49.3 (P2, Have):** disclose that the local-root table is a bounded, non-summing sample ("top N of M;
  sizes overlap") — both counts are already in the model.
- **§49.4 (P3, Have):** always emit the "Local roots: N" line (including `0`) so per-thread structure is
  uniform and an absent line is never ambiguous.
- **§49.5 (P2, Have, parity):** unify the default visibility of the local-root table across md/graphs (inline)
  and HTML (currently double-nested `<details>`).

All five are **Have** — render-only, no new model field, no heap pass, no SCHEMA bump — and none change a
rendered byte value; §49.1 changes only a *label* (or adds a second percent). This pass is the Threads-section
analogue of the §45/§48 percentage-basis work: it finds the one remaining place a percentage silently switches
denominator, plus four presentation defects that make an otherwise highly-actionable section (frame → retained
size → code location) read as buggy or incomplete.

## 50. References (Soft/Weak/Phantom) Section Audit (pass 31)

The References section (`render_references`, render_md.rs:2618–2665; shared by md + graphs; HTML
`ReferencesSection`/`RefClassTable`, App.tsx:2908–2937) reports, per reference kind, a referent-class
histogram and — where populated — an "only-weakly-retained" breakdown. It is the report's only view of the
soft/weak/phantom subsystem, and it is the one section a developer consults to answer "is a soft-reference
cache holding real heap, or are these references clean?" Auditing the sample (scala-doku-full.md:3169–3245,
scala-doku-full.graphs.md same range) against the model (`ReferenceStats`/`RefStatClassRow`, model.rs:1202–
1238) and the one triage rule that reads this data (`WeakRefEscape`, triage.rs:565–590) surfaces six defects
that make this section long on rows and short on decisions: it denominates referents in *shallow* bytes (the
one number that cannot indicate a reclaim opportunity), renders an uncapped long tail of 1-object / 0-byte
noise, attaches the graphs bar to the wrong axis, presents an inconsistent per-kind structure, gates its triage
rule on an object *count* rather than bytes, and never states a verdict or action. No earlier pass has examined
this section beyond §12's brief early critique (which predates the current renderer) and §22's completeness
matrix.

### 50.1 Referent classes are ranked and sized by *shallow* bytes — the one measure that can't tell you if reclaiming helps (new, P1, Compute-cheap→Add)

Every referent row prints `Class | Objects | Shallow` (render_md.rs:2630/2641; `RefStatClassRow` carries only
`{pretty_class, objects, shallow}`, model.rs:1206–1210). In the sample, Soft's top row is
`` `java.lang.invoke.LambdaForm` | 178 | 8.3 KB `` (scala-doku-full.md:3181) and the entire Soft table sums to
well under 15 KB shallow. But *shallow* size of a referent is nearly meaningless for the question this section
exists to answer: a soft-referenced 64-byte `HashMap` header can retain a 40 MB cache, and this table would
show it as "64 B". The developer wants to know **how much heap becomes reclaimable if these referents are
cleared** — i.e. the *retained* size of the only-weakly-retained referents — and the section shows the opposite
(shallow of *all* referents, strong-held or not).

**Reason / fix:** rank and size the "Only-weakly retained" table by **retained** bytes, not shallow — that
table already isolates the referents with `idom == u32::MAX` (reachable only via the weak edge, model.rs:1214–
1215), so their retained size *is* the reclaim-on-clear estimate. The plain "Referent classes" histogram can
keep `Objects` but should add a `Retained` (or "Reclaimable if cleared") column so a heavy soft cache is
visible. **Availability:** the referent's retained size is not in `RefStatClassRow` today, so this is **Add**
(one `retained: u64` field on the row, summed during the existing reference scan that already groups referents
by class); the "only-weakly" subset already knows its members, so summing their retained is **Compute-cheap**
once the retained-per-object array is in scope. Must land in md/graphs (render_md.rs:2630) + HTML
(`RefClassTable`, App.tsx) + JSON (`RefStatClassRow`, SCHEMA bump) together.

### 50.2 The referent histogram is uncapped and renders a 1-object / 0-byte long tail as noise (new, P2, Compute-cheap)

`render_class_table` iterates `stats.referent_histogram` in full with no cap and no tiny-row folding
(render_md.rs:2628–2649). In the sample the Weak table is **22 rows** (scala-doku-full.md:3206–3229), of which
**13 are single-object rows** and one — `` `java.security.SecureClassLoader` | 1 | 0 B `` (line 3220) — is a
literal **0-byte row**. The signal (`MethodType`, 894 objects / 34.9 KB) is one line; the other 21 rows are a
scrolling tail of loader/logging singletons that carry no decision.

**Reason / fix:** cap the histogram (the report caps every other class table — e.g. `UNREACHABLE_HISTOGRAM_CAP`,
used one section below at render_md.rs:2684) and fold the tail into a `… and N more classes (M objects, X B)`
row, mirroring the folding the plan already mandates elsewhere. At minimum, drop 0-byte rows or rows below a
1-object-and-≤a-few-bytes floor — a `0 B` referent tells the reader nothing. This also fixes the graphs variant,
where each of those 1-object rows still gets a min-visible `▏` bar (see §50.3). **Availability: Compute-cheap**
(truncate + sum the remainder at render; the rows are already sorted). md/graphs (render_md.rs:2637 loop) + HTML
(`RefClassTable`) together; JSON already carries the full vec, so add a cap constant consumed by all renderers.

### 50.3 The graphs bar annotates *Objects* (count), not *Shallow* (bytes), and the min-visible floor makes 1 and 2 objects identical (new, P2, Have)

In the graphs variant the appended bar is proportional to `r.objects` against `obj_max`
(render_md.rs:2629/2644). The sample shows `` `[Ljava.lang.Object;` | 2 | 64 B | ▏ `` and
`` `java.util.ArrayList` | 1 | 24 B | ▏ `` (scala-doku-full.graphs.md ~6701–6702; References section at 6682) rendering the **same** `▏`
bar for 1 and 2 objects — the §44.2 min-visible floor collapses the low tail into visual ties. Worse, the bar is
on the *count* axis while the meaningful magnitude is *bytes*: a class with 894 tiny `MethodType`s gets a full
bar, while a class with 2 objects retaining a large cache would get `▏`. The chart therefore steers the eye by
*population*, not *memory* — the opposite of the report's stated goal.

**Reason / fix:** put the bar on the byte column that matters. Once §50.1 adds `Retained`, bar on retained; until
then, bar on `Shallow` (already present) rather than `Objects`. This makes the ASCII chart honest about *memory*
and stops 1-vs-2-object rows from tying. **Availability: Have** (change the `bar(r.objects, obj_max, …)` argument
to the byte column and recompute `obj_max` as the byte max; render-only, no model change). This is the §44 "bar
the meaningful axis" principle applied to the one section §44 did not walk. graphs-only edit (render_md.rs:2644);
HTML has no bar here so no parity change, but if HTML later adds one it must use the same axis.

### 50.4 "Only-weakly retained" appears under Soft but silently vanishes under Weak/Phantom — structure looks inconsistent (new, P3, Have)

The `#### Only-weakly retained _(approximate)_` sub-table is gated on `!stats.only_weakly_retained.is_empty()`
(render_md.rs:2660; HTML App.tsx:2926 mirrors the gate). In the sample it renders under **Soft** (one row,
`Class$ReflectionData`, scala-doku-full.md:3196–3198) but is **absent** under Weak and Phantom. A reader cannot
tell whether Weak/Phantom genuinely have zero only-weakly-retained referents or whether the sub-section was
dropped — the same silent-gap ambiguity §49.4 flagged for the per-thread "Local roots" line and §31.4 flagged
for empty sections generally.

**Reason / fix:** when the vec is empty, still emit the sub-heading with an explicit `_None — all referents are
also strongly reachable._` so every reference kind has the same two-part structure. This makes "no escape" a
*stated fact* (the reassuring answer to "is anything leaking through weak refs?") rather than a missing block.
**Availability: Have** (emit the heading + a one-line note in the else branch; render-only). md/graphs
(render_md.rs:2660) + HTML (App.tsx:2926) together.

### 50.5 The `weak-ref-escape` triage rule gates on object *count* (≥1000), not retained bytes — it can't fire on a few huge escapees and cries wolf on many tiny ones (new, P1, Have)

`WeakRefEscape` sums `row.objects` across all three kinds' `only_weakly_retained` and fires only when the total
≥ `WEAKREF_FLOOR = 1000` (triage.rs:570–576, 42). This is a *count* threshold on data whose whole point is
*reclaimable bytes*: 1,200 only-weakly-retained 16-byte objects (19 KB) trips the rule, while 40 only-weakly-
retained objects each retaining 5 MB (200 MB reclaimable) stays silent because 40 < 1000. In the sample the
count is 21 (Soft) so the rule correctly stays quiet — but for the wrong reason (count, not size). The signal
also carries no `bytes`, so under the §26.2/Phase-5 "rank problems by reclaimable bytes" ordering it sorts to
the bottom regardless of how much heap would actually free up.

**Reason / fix:** gate on **retained bytes** of the only-weakly-retained set (with a small floor to suppress
trivia) and attach that byte figure via `with_bytes(…)` so the signal ranks by reclaim potential like the other
byte-denominated rules (off-heap, gc-waste, over-capacity-collections, triage.rs). This is the same
count-vs-bytes correction §46.1 made for collection waste and §50.1 makes for the section itself. **Availability:
Have** once §50.1's retained-per-referent lands (the rule can then sum retained instead of `objects`); until
then it is **Compute-cheap** if the reference scan can expose an only-weakly retained-byte total. The message
should also switch from "N objects … likely reclaimable" to "N MB retained only via soft/weak/phantom refs —
reclaimable under memory pressure."

### 50.6 The section states *what refs point at* but never whether it's a problem or what to do (new, P2, Format/prose)

The only prose is the caption `_Soft/weak/phantom reference referents (what they point at)._`
(render_md.rs:2621) and the per-kind `_N reference instances._` line (render_md.rs:2655). There is no verdict
("these referents are clean" / "this soft cache holds X MB you could evict") and no recommended action — the
same actionability gap §33 flagged report-wide, here made sharper because the *correct* action differs by kind:
soft-ref escapees are a *cache-tuning* signal (they'll clear under pressure — usually fine), weak-ref escapees
are usually *benign* (canonicalizing maps), and a large phantom-referent set can indicate *cleanup lag* (native
resources awaiting `Cleaner`). Presenting all three identically with no framing invites the reader to treat
`975 weak reference instances` as alarming when it is routine.

**Reason / fix:** add a one-line "what this means / what to do" caption per kind — e.g. under Soft:
_"Soft referents clear under memory pressure; a large only-weakly-retained total here is reclaimable cache, not
a leak — tune cache size if it dominates."_ Tie the verdict to the §50.1 retained total so the prose can say
"clean" when the reclaimable bytes are negligible. **Availability: Format/prose** (static per-kind captions;
no model change). Mirror the wording across md/graphs (render_md.rs:2653) + HTML (App.tsx:2922) so all three
formats give the same guidance (comparability rule).

### 50.7 Summary and Priority-Summary deltas

- **§50.1 (P1, Add):** rank/size referents (esp. only-weakly-retained) by **retained** bytes — shallow of a
  referent can't indicate a reclaim opportunity; add a `retained` column/field and a "reclaimable if cleared"
  read on the only-weakly table.
- **§50.2 (P2, Compute-cheap):** cap the referent histogram and fold the 1-object / 0-byte long tail (Weak is
  22 rows, 13 singletons, one literal `0 B` row in the sample).
- **§50.3 (P2, Have):** move the graphs bar off *Objects* (count) onto the byte axis so the chart steers by
  memory, and 1-vs-2-object rows stop tying at the min-visible floor.
- **§50.4 (P3, Have):** always emit "Only-weakly retained" per kind (with an explicit "None" note) so the
  per-kind structure is uniform and "no escape" is a stated fact, not a silent gap.
- **§50.5 (P1, Have/Compute-cheap):** re-gate `WeakRefEscape` on **retained bytes** (not ≥1000 objects) and
  attach `bytes` so it ranks by reclaim potential — today it can miss a few huge escapees and fire on many tiny
  ones.
- **§50.6 (P2, Format/prose):** add per-kind verdict/action captions (soft = cache-tuning, weak = usually
  benign, phantom = cleanup-lag) so the reader knows whether the numbers are a problem.

§50.1 and §50.5 are the load-bearing pair: both correct the same **count/shallow-vs-retained** category error
that recurs across the tool (§46.1 collection waste, §27.1 "% Heap"), applied to the one subsystem whose entire
value is "how much becomes collectable." §50.2/50.3/50.4/50.6 are the presentation half — cap the noise, bar the
right axis, keep the structure uniform, and say what to do — none of which change a rendered byte value except
by adding the retained column §50.1 introduces.

## 51. Unreachable Objects Section & Garbage-Root Trees Audit (pass 32)

The always-on **Unreachable Objects** section (render_md.rs:2671, `render_unreachable_histogram`; sample
scala-doku-full.md:3247) is a genuine differentiator — it inventories heap that is *already* collectable, which
MAT discards outright. Precisely because it is unique the presentation has to be crisp, and this pass finds four
defects that either mislead the reader on scale or corrupt the prose.

### 51.1 Garbage-root tree `(N objects)` is a cumulative subtree count rendered as if it were the node's own population (new, P2, Have)

The tree lines print `{} objects` from `node.objects` (render_md.rs:2765/2773/2798/2808), and per the model that
field is "Number of real objects in the subtree rooted at this node" (model.rs:63) — i.e. **cumulative**,
including all descendants. The sample makes the ambiguity visible: garbage root #3 reads
`**java.lang.ref.SoftReference** — 3.2 KB (70 objects)`, its child `java.lang.Class$ReflectionData — 3.2 KB
(69 objects)`, then `java.lang.reflect.Field[] — 3.1 KB (68 objects)` (scala-doku-full.md:3300-3302). A reader
naturally sums the children expecting them to add up to the parent, but the numbers *nest* (70 ⊇ 69 ⊇ 68), so
any attempt to reconcile the tree fails silently. The retained bytes nest correctly (that is what a dominator
tree is), but the object count is presented with identical syntax and no cue that one is a running total and the
other is a per-node figure.

**Reason / fix:** label the count as cumulative — e.g. `(70 objects in subtree)` on the root line and drop it on
interior nodes (the retained byte already conveys subtree magnitude), or render the *node-local* object count
instead so children can be summed. Either removes the false invitation to add. **Availability: Have** — the model
already distinguishes; this is a render-string change in the four `({} objects)` sites (render_md.rs:2765/2773/
2798/2808) plus the graphs mirror and App.tsx garbage-root renderer (comparability rule). Simplest: change the
literal to `"({} objects in subtree)"` on the root and `""` on children.

### 51.2 `(1 objects)` — ungrammatical singular across every objects-count site (new, P3, Format/prose)

`fmt_count(objects)` is always suffixed with the bare literal `" objects"` (render_md.rs:2765/2773/2798/2808 in
this section; also Leak Suspects at md:2271-2281, e.g. `` `java.time.zone.ZoneRulesProvider` (1 objects, retained
198.4 KB) ``). Single-object roots are extremely common in this section — garbage roots #1, #2, #4, #5, #6 are all
`int[] — … (1 objects)` (scala-doku-full.md:3296-3312). The tool otherwise takes pains with formatting (§41);
`(1 objects)` reads as a bug to a careful reviewer and undermines trust in the numbers next to it.

**Reason / fix:** a `plural(n, "object")` helper (or inline `if n == 1 { "object" } else { "objects" }`) shared by
the garbage-root renderer, Leak Suspects, and any other `N objects` callsite. **Availability: Format/prose** — no
model change; a one-line helper in format.rs consumed everywhere the `" objects"` literal appears, mirrored in
web/src/format.ts so HTML pluralizes identically (comparability).

### 51.3 Histogram `Retained` column overlaps and can exceed the section's own shallow total, with no caveat (new, P2, Format/prose)

The per-class histogram carries a `Retained` column (render_md.rs:2735, model.rs:47) whose values *overlap*: a
class's retained heap includes objects counted under other classes' rows. In the sample `java.lang.String` shows
`25.4 KB shallow / 86.5 KB retained` and `java.lang.ref.SoftReference` shows `840 B / 14.0 KB`
(scala-doku-full.md:3262/3271) — sum the `Retained` column and it wildly exceeds the section header's `673.0 KB
retained within the unreachable forest`. This is correct dominator behavior (retained sets nest and overlap), but
the report presents shallow and retained as peer columns with no note that only *shallow* is additive. The same
overlap caveat §27/§48 demanded for the reachable Top Consumers table is absent here.

**Reason / fix:** add a one-line caption under the histogram: _"Retained sizes overlap and are not additive; only
the Shallow column sums to the section total."_ **Availability: Format/prose** — static caption, no data change.
Mirror in graphs + App.tsx histogram (comparability). This is the unreachable-section instance of the report-wide
"retained columns don't sum" hazard.

### 51.4 Graphs bar is on the Objects (count) axis, not bytes — same defect as §50.3, here on a bigger table (new, P2, Have)

In the `graphs` variant the histogram's appended bar is `bar(r.objects, obj_max, …)` (render_md.rs:2738) — the
identical count-axis choice §50.3 flagged for References. It bites harder here because the class populations and
byte sizes diverge sharply: `int[]` has 1,642 objects / 569.6 KB while `byte[]` has 1,084 objects / 61.1 KB
(scala-doku-full.md:3260-3261), so a byte-proportional bar would show `int[]` dominating ~9:1, but the count bar
shows only ~1.5:1 — visually flattening the fact that primitive `int[]` *is* essentially the entire 673 KB of
collectable heap. A developer scanning the bars to find "where is the reclaimable memory" is steered to
population, not memory.

**Reason / fix:** bar `r.shallow` (the additive, section-summing axis) against a shallow-max, matching the fix
proposed in §50.3 for References and §44 report-wide. **Availability: Have** — `shallow` is already in the row;
swap the two args at render_md.rs:2738 and the graphs/HTML mirrors. Prefer shallow over retained for the bar
denominator here specifically because retained is non-additive (§51.3), so a retained bar would not sum to the
whole and would double-count nested classes.

### 51.5 Summary and Priority-Summary deltas

- **§51.1 (P2, Have):** garbage-root tree object counts are cumulative-subtree totals rendered like per-node
  counts (parent 70 ⊇ child 69 ⊇ 68 in the sample) — label as "in subtree" or drop on interior nodes so readers
  stop trying to sum them.
- **§51.2 (P3, Format/prose):** fix `(1 objects)` via a shared `plural()` helper — the singular is pervasive
  (5 of the sample's garbage roots, plus every 1-object Leak Suspect).
- **§51.3 (P2, Format/prose):** caption the histogram that `Retained` overlaps and is not additive; only
  `Shallow` sums to the section total (retained-column values in the sample exceed the 673 KB header if summed).
- **§51.4 (P2, Have):** move the graphs histogram bar off the *Objects* count axis onto *Shallow* bytes, so the
  chart shows that `int[]` is ~9:1 the collectable heap rather than a flattened ~1.5:1.

§51.1/§51.3 are the correctness pair (counts that don't sum, bytes that don't sum, both unlabeled); §51.2/§51.4
mirror recurring report-wide fixes (pluralization hygiene, bar-the-byte-axis) into this section. None change a
computed value — §51.1 and §51.4 are pure render-arg/label swaps, §51.2/§51.3 are prose — so all four are
safely shippable in one render-only commit with no SCHEMA bump.

## 52. Biggest Collections & Collection Contents: Value-Type Usefulness and Row Noise (pass 33)

The `--collections` pair — **Biggest Collections** (render_md.rs:2393, sample scala-doku-full.md:3068) and
**Collection Contents by Type** (render_md.rs:2563, sample md:3155) — is where the tool answers "which containers
hold the heap and what's inside them." This pass audits whether the *value-type* columns actually tell the reader
what is inside, and whether the row set is legible. Both fail in ways the sample makes stark.

### 52.1 For maps, the "Value Type" is always the internal entry-node wrapper — a tautology, never the key/value class (new, P1, Add)

Every map row in the sample reports its value type as the map's own internal `Node`/`Entry` struct:
`java.util.HashMap → java.util.HashMap$Node ×2,048` (md:3097), `ConcurrentHashMap → ConcurrentHashMap$Node`
(md:3100), `LinkedHashMap → LinkedHashMap$Entry ×132` (md:3115). This is what the map's backing `table[]` array
literally points at, so it is *always* true and *never* informative — it tells the reader "this HashMap contains
HashMap nodes," which conveys nothing about whether the map holds `String→URL`, `Class→Loader`, or a 40 MB cache.
Collection Contents by Type repeats the tautology at the class level: `java.util.HashMap | 349 | 7,601 |
HashMap$Node ×7,601` (md:3160). The one collection kind where value type is *useful* — the map, whose whole point
is key/value payload — is exactly the kind where the column is dead. (Lists do better: `ArrayList → java.lang.Class
×485` at md:3079 is genuinely the element type.) §28.5 flagged "unwrap map-entry wrappers" abstractly; this is the
concrete symptom in the shipped sample.

**Reason / fix:** for map-kind collections, walk one level past the `Node`/`Entry` and tally the runtime class of
`Node.key` (and/or `Node.value`) instead of the node struct itself, so the column reads e.g. `String→ZoneRules
×455` rather than `ConcurrentHashMap$Node ×455`. **Availability: Add** — the collection-values pass
(src/pass2) already dereferences the backing array to reach the nodes; it must follow the node's `key`/`value`
fields one more hop and record *those* target classes. Land the shape (`dominant_value_type`/breakdown carrying
key/value classes for maps) in the model + all three renderers + JSON together (comparability). This is the single
most valuable fix in the collections area: it turns a dead column into "what your maps are actually keyed/valued
by," directly serving "where does the heap come from."

### 52.2 "Value Type" and "Value Types (top)" are the same string on nearly every row — redundant columns (new, P2, Format)

The table emits both `dominant_value_type` (render_md.rs:2497) and `value_type_breakdown` (render_md.rs:2503).
When one type dominates — which is almost always, since a typed collection holds one element class — the two
columns are byte-identical: `java.lang.Class` / `java.lang.Class ×485` (md:3079), `HashMap$Node` /
`HashMap$Node ×2,048` (md:3097). Across the entire sample map table (24 rows) *every* row has this duplication;
the list table is the same but for the `varies` case (which never occurs in the sample). Two columns, ~40
characters of horizontal budget, carrying one fact.

**Reason / fix:** drop the standalone `Value Type` column and keep only `Value Types (top)` (the breakdown already
leads with the dominant type and its count). If a single-type shorthand is wanted, render the breakdown's first
entry without the `×N` when the list has length 1. **Availability: Format** — pure render change
(render_md.rs:2461–2467/2496–2512 + graphs + App.tsx); `dominant_value_type` stays in JSON for machine readers,
so no SCHEMA change. Narrower tables also mitigate the §32 HTML horizontal-scroll and §44 md-width problems.

### 52.3 Byte-identical rows repeat with no coalescing — the ranking is mostly duplicate noise (new, P2, Compute-cheap)

The "largest individual collection instances" tables are flooded with identical rows. The map table opens with two
byte-for-byte duplicates — `HashMap | 2,048 | HashMap$Node | Manifest#entries | 577.2 KB` twice (md:3097–3098) —
and the list table has **six** consecutive identical `ArrayList | 3 | String | ConcurrentHashMap$Node#val | 144 B`
rows (md:3084–3089) plus more at 80 B, while the deque table is **eleven** identical `ArrayDeque | 1 | Inflater |
inflaterCache | 112 B` rows (md:3131–3141) — the entire deque section is one row repeated 11×. These are distinct
object instances, so listing each is *correct*, but as a ranked "biggest" view it is noise: the reader learns
"there are 11 one-element inflater caches" far better from one coalesced row than from eleven identical lines that
push the genuinely-large entries off a scanned screen.

**Reason / fix:** coalesce rows that are identical on (kind, container_class, elements, dominant_value_type, owner)
into one row with a `×N instances` multiplier and summed retained — the same `×N` collapse §28.1/§3.1 applied to
dominator subtrees. Keep a per-instance mode behind `--detail max` for the rare case someone wants every instance.
**Availability: Compute-cheap** — group the existing `Vec<BiggestCollectionRow>` before rendering; add an
`instances: u64` (default 1) to the row so JSON carries the count. Coalescing also fixes the deque "Total: 11"
(md:3143) reading as eleven findings when it is one pattern. Mirror across md/graphs/HTML + JSON.

### 52.4 Summary and Priority-Summary deltas

- **§52.1 (P1, Add):** map "Value Type" is always the internal `Node`/`Entry` wrapper (tautology, md:3097/3100/
  3115) — walk one hop to the node's key/value class so the column says what maps actually hold; the highest-value
  collections fix for "where does the heap come from."
- **§52.2 (P2, Format):** `Value Type` and `Value Types (top)` are identical on every sample row (md:3079/3097) —
  drop the standalone column, keep the breakdown; narrower table helps §32/§44 width.
- **§52.3 (P2, Compute-cheap):** coalesce byte-identical instance rows with a `×N` multiplier — the sample has 6
  identical list rows (md:3084–3089) and an 11-row all-identical deque section (md:3131–3141) drowning the ranking.

§52.1 is the load-bearing item: it converts the map value-type column from a tautology into payload-class
attribution, the collections-side counterpart to §47's `Class#field` heap attribution. §52.2/§52.3 are legibility
(kill the redundant column, collapse duplicate rows) and change no computed byte value.

## 53. Allocation Sites, Retention Concentration & Dominator-Depth: Impossible Numbers (pass 34)

This pass audits the three tail sections that quantify *where* and *how deep* the heap sits (render_md.rs:3136,
524, 579; sample scala-doku-full.md:3407/3413/3424). Two of them print physically-impossible figures in the
shipped sample — a retained size 2,800× the entire heap, and a cumulative percentage of 10000% — both traceable
to a specific line. These are the most concrete correctness bugs found in this whole review series.

### 53.1 Allocation Sites reports 84.82 GB retained on a 30 MB heap — naive retained-sum multi-counts nested dominators (new, P0, Have)

The sample's entire Allocation Sites section is one row: `serial 1 | 953,964 | 30.4 MB | 84.82 GB`
(scala-doku-full.md:3411). The heap's **total reachable shallow is 29.8 MB** (System Overview, md:96) and the
headline retainer keeps 22.9 MB — so **84.82 GB retained is ~2,800× the whole heap**, an impossible value. Two
things are wrong: (a) the object count `953,964` exceeds the dump's `Total objects 952,666` (md:93) by ~1,300;
(b) the retained total is nonsense. The cause is at build.rs:1098: `e.2 += self.g.retained[i]` sums *every*
object's individual retained size into the bucket. Retained sets nest — a parent's retained size already includes
all its descendants — so summing per-object retained across ~950k objects counts the same bytes thousands of
times. This is the exact non-additive-retained hazard §51.3/§27 warn about, here escaping into a headline number.
Worse, when there is no real stack data every object falls into a single synthetic `serial 1` bucket (md renders
`serial {stack_serial}` when `frames` is empty, render_md.rs:3169), so the "allocation site" is meaningless *and*
carries an impossible retained figure.

**Reason / fix (two parts):** (1) **Never sum retained across objects** — either drop the Retained column from
alloc-sites entirely (shallow *is* additive and honest), or compute a true bucket-retained via a dominator pass
over the bucket's object set (expensive; the shallow column already answers "how much did this site allocate").
(2) The `953,964 > 952,666` count discrepancy signals the serial stream and object count are misaligned
(build.rs:1092's overrun guard silently `return`s, so extra serials are counted as… nothing — yet the count is
still too high, meaning `idx` advances on skipped/serial-0 objects); reconcile `object_count` to real objects.
**Availability: Have** — removing the multi-counted Retained column is a one-line render + model change; the
honest shallow total is already correct. This is P0: a 84 GB figure on a 30 MB heap destroys trust in every other
number in the report.

### 53.2 Dominator-Depth footer prints "10000.0% cumulative" — a percent scaled by 100 twice (new, P0, Have)

The hidden-tail footer reads `_… (+41305 deeper buckets, 284,826 objects, 10000.0% cumulative — full data in
JSON)_` (scala-doku-full.md:3486). Cumulative percent cannot exceed 100%. The bug is at render_md.rs:626:
`last_cum * 100.0`. But `depth_stats` already returns `cum` on the **0.0–100.0 scale** — format.rs:268 computes
`cum = running / total_f * 100.0` and the docstring (format.rs:244) states "percents are 0.0–100.0". The table
body renders it correctly as `fmt_pct(cum)` (render_md.rs:610, e.g. `70.1%` at md:3457), because `fmt_pct` just
appends `%` without rescaling. The footer alone multiplies the already-percent value by 100 → `100.0 → 10000.0`.

**Reason / fix:** drop the `* 100.0` at render_md.rs:626 — pass `last_cum` straight to `fmt_pct` like the table
body does, so the footer reads `100.0% cumulative`. **Availability: Have** — one-line fix. Verify the graphs +
HTML depth renderers don't repeat the double-scale (render_dominator_depth_graphs delegates to the same
`_inner`, so fixing once fixes both; check App.tsx depth footer for the same `*100`). Add a unit test asserting
the footer cumulative equals the last table row's cumulative.

### 53.3 The degenerate 41,355-hop depth tail floods the table and hints at an unhandled artifact (new, P2, Compute-cheap)

The depth summary says `the deepest chain is 41355 hops` (md:3421) and the table's tail from depth 23 onward is a
flat run of `31 objects | <0.1% | 70.1%` repeating for ~28 identical rows (md:3438–3457) before the footer folds
`+41305 deeper buckets`. A 41,355-deep dominator chain is not a real object nesting — it is the signature of a
linked-list / self-referential structure (or a pathological cycle broken arbitrarily by the dominator pass),
exactly the artifact §27.4 flagged for the depth-p90 statistic. Presented raw, it (a) makes the "deepest chain"
headline alarming without explanation, and (b) pads the table with ~28 visually-identical `31 | <0.1%` rows that
carry no signal.

**Reason / fix:** detect a long flat tail (constant object count across many consecutive depths = a single chain
descending one hop at a time) and collapse it with a note: _"depths 23–41355 are a single ~31-object chain
descending one level per hop (likely a linked list or cycle) — see §27.4."_ The existing `>= 0.1%` cutoff already
trims most, but the run of exactly-31 rows slips through because the cutoff is per-row, not run-aware. **Availability:
Compute-cheap** — detect `objects[d] == objects[d+1]` runs in `render_dominator_depth_inner` before the DEPTH_CAP
slice; add a "growth-path" bullet (§20.4) naming the class if the chain's dominant class is known.

### 53.4 Retention Concentration puts a count in the "Retained Share" column with an empty Retained cell (new, P3, Format)

The concentration table's last row is `Objects each >=1% | 4 | (empty)` (scala-doku-full.md:3421, render_md.rs:561).
The value `4` is an object *count*, but it sits under the `Retained Share` header (a percentage column, e.g.
`76.7%` above it) and leaves the `Retained` byte column blank. A reader scanning the column reads "4" as a share.
It is a different kind of fact (how many objects individually exceed 1% of heap) shoehorned into a percentage row.

**Reason / fix:** move it out of the table into a caption line — _"4 individual objects each retain ≥1% of the
reachable heap"_ — or give it its own two-cell row (`Objects ≥1% of heap | 4`) with a header that isn't
"Retained Share". **Availability: Format** — render-only (render_md.rs:557–563 + graphs + App.tsx concentration
panel); no model change. Ties into §45's percentage-basis-consistency theme: don't put counts in percent columns.

### 53.5 Summary and Priority-Summary deltas

- **§53.1 (P0, Have):** Allocation Sites reports **84.82 GB retained on a 29.8 MB heap** (md:3411 vs md:96) —
  `e.2 += g.retained[i]` (build.rs:1098) sums nested/overlapping retained sizes ~2,800×; drop the multi-counted
  Retained column (shallow is additive and correct). Also reconcile the `953,964 > 952,666` object count.
- **§53.2 (P0, Have):** Dominator-Depth footer prints **`10000.0% cumulative`** (md:3486) — `last_cum * 100.0`
  (render_md.rs:626) rescales an already-0–100 percent (format.rs:244/268); drop the `* 100.0`.
- **§53.3 (P2, Compute-cheap):** collapse the degenerate 41,355-hop flat tail (~28 identical `31 | <0.1%` rows,
  md:3438–3457) with a linked-list/cycle note (§27.4/§20.4) instead of padding the table.
- **§53.4 (P3, Format):** move `Objects each >=1% | 4` out of the `Retained Share` percentage column (md:3421)
  into a caption or its own labeled row — a count doesn't belong in a percent column (§45).

§53.1 and §53.2 are P0 correctness bugs: both print numbers that are physically impossible (retained > total
heap; cumulative > 100%), both from a single identifiable line, both one-line fixes, and both catastrophic for
reader trust. They are the counterpart in the *rendered* output to the shallow-vs-retained additivity category
error §51.3/§52 trace through the model. §53.3/§53.4 are the surrounding legibility cleanup.

## 54. Triage Rule-by-Rule: False Positives & Inconsistent Gating (pass 35)

`triage.rs` ships **38 rules** (registry `rules()`, triage.rs:139–181). §26/§45 audited thresholds abstractly and
§50.5 fixed one rule's count-vs-byte gate; this pass reads the actual rule bodies and the sample's fired signals
(Memory Triage, scala-doku-full.md:57–72) to find rules that fire wrongly or gate inconsistently with their peers.
Three concrete defects, each visible in the shipped sample.

### 54.1 `ClassloaderLeak` has NO threshold — it fires as a Warning on any duplicate class, misfiring on normal Scala/library code (new, P1, Compute-cheap)

The rule (triage.rs:488) does `duplicate_classes.iter().max_by_key(|d| d.total_retained)?` and then *always*
emits a `Warning` if any duplicate exists — there is no `loader_count` floor and no retained floor. In the sample
it fires: `Classloader leak: scala.collection.immutable.$colon$colon is loaded by 2 class loaders (8.6 MB
retained) — classic reload leak.` (md:65). A Scala cons cell (`::`) present in two class loaders is completely
routine in any Scala app (the bootstrap and app loaders both see core collection classes); calling it a "classic
reload leak" Warning is a false positive that will fire on essentially every non-trivial JVM app. A real
classloader/reload leak shows *many* loaders (tens to hundreds) of the *same* application class accumulating over
redeploys, not 2 loaders of a stdlib class.

**Reason / fix:** gate on (a) `loader_count >= N` (start at ~5, well above the 2–3 seen in healthy bootstrap/app/
platform splits) OR (b) a monotonic-growth signal in diff mode, and (c) a retained floor so trivial dups stay
quiet. Downgrade to Info unless the loader count is high. The message should distinguish "loaded by 2 loaders
(normal bootstrap/app split)" from "loaded by 40 loaders (reload leak)." **Availability: Compute-cheap** —
`loader_count` and `total_retained` are already on the row; add the two comparisons. This is the highest-FP-risk
rule in the set because it has literally zero gating.

### 54.2 `Shape` reports a p90 depth of 11273 — the degenerate 41,355-hop chain (§53.3) corrupts the statistic (new, P2, Compute-cheap)

The Shape rule computes p90 as the depth at which cumulative objects reach 90% (triage.rs:423–431) and prints
`deep … — 90% of objects within depth 11273, max depth 41355.` in the sample (md:60). But §53's depth table shows
cumulative reaches only ~70% by depth 20 and the entire tail from depth 23 onward is a single ~31-object chain
descending one hop per level (md:3438–3457). So the *real* mass sits shallow (>67% within 12 hops) and the p90 of
11273 is an artifact: the 90th-percentile object is dragged deep purely by one pathological linked-list/cycle
chain that §27.4 and §53.3 already identify. "90% of objects within depth 11273" is actively misleading — it
tells the developer the heap is deeply chained when it is mostly shallow with one long tail.

**Reason / fix:** compute p90 (and the "deep/shallow" verdict) over the *de-artifacted* distribution — exclude the
constant-count single-chain tail §53.3 proposes detecting, or report p90 alongside the median so the spread is
visible (median is ~10 hops per the summary line, md:3419, vs p90 11273 — the 1000× gap is itself the tell).
Better: when p90/median exceeds a large ratio, say "mostly shallow (median 10 hops) with a long ~31-object chain
to depth 41355 — likely a linked list or cycle" instead of a flat "deep." **Availability: Compute-cheap** — the
histogram is already summed here; add the median and the run-detection §53.3 shares. Fixing §53.3's render and
this rule together keeps the depth story consistent across the table and the triage bullet (comparability).

### 54.3 Structurally identical "swarm of tiny objects" rules use opposite count-vs-percent boolean logic (new, P2, Compute-cheap)

Two rules detect "very many small instances of one class": `ObjectSwarm` (triage.rs:760) and
`BoxedPrimitiveBloat` (triage.rs:799). They gate oppositely. `ObjectSwarm` requires **both** a count floor and a
percent floor — `instances >= SWARM_FLOOR_INSTANCES` (10,000,000) in the filter (triage.rs:769) *and*
`pct_of(shallow) >= SWARM_PCT` (line 773), a conjunction. `BoxedPrimitiveBloat` returns None only when **both**
fail — `if instances < BOXED_FLOOR_INSTANCES && pct_of(shallow) < BOXED_PCT` (triage.rs:825), i.e. it fires if
**either** the 5,000,000-instance floor *or* the 5%-of-heap share is crossed, a disjunction. So one rule needs
count AND bytes, the sibling needs count OR bytes. On a heap with 6M boxed Integers at 3% of heap, BoxedBloat
fires (count alone) but an equivalent 6M-instance swarm of another tiny class would *not* fire ObjectSwarm (below
the 10M count floor even at high %). The 10M vs 5M floors and AND vs OR logic are unexplained and produce
inconsistent sensitivity for the same underlying phenomenon.

**Reason / fix:** pick one policy for "swarm" rules and document the provenance (§26.7). The byte-share axis is the
one that matters for "where is heap wasted" (§26.2/§50.5), so prefer: fire on `pct_of(shallow) >= PCT` with the
instance floor only as a *noise gate* (skip tiny heaps), identically across both rules. Align SWARM_FLOOR_INSTANCES
and BOXED_FLOOR_INSTANCES or justify the difference in a comment. **Availability: Compute-cheap** — both rules
already have `instances`, `shallow`, and `total`; normalize the two conditionals. Ties into §50.5 (weak-ref
count-gate) and §26.2 (rank by reclaimable bytes): the whole rule set should gate on bytes and use counts only to
suppress noise.

### 54.4 Summary and Priority-Summary deltas

- **§54.1 (P1, Compute-cheap):** `ClassloaderLeak` (triage.rs:488) has zero gating — fires a Warning on any
  duplicate class; the sample flags `$colon$colon` in 2 loaders (md:65) as a "classic reload leak," a false
  positive on normal Scala. Add a `loader_count >= ~5` (or diff-growth) + retained floor; downgrade to Info for
  low loader counts.
- **§54.2 (P2, Compute-cheap):** `Shape` p90 = 11273 (md:60) is corrupted by the degenerate 41,355-hop chain
  (§53.3/§27.4) — the heap is mostly shallow (median ~10). De-artifact the tail or report median vs p90 and switch
  the verdict wording.
- **§54.3 (P2, Compute-cheap):** `ObjectSwarm` (count AND %) and `BoxedPrimitiveBloat` (count OR %) gate the same
  "tiny-object swarm" phenomenon with opposite boolean logic and mismatched 10M/5M floors (triage.rs:769/773 vs
  825); normalize to a byte-share gate with count as a noise floor (§26.2/§50.5).

§54.1 is the load-bearing item — an ungated Warning that misfires on nearly every real Scala/Java app is worse
than a missing rule, because false alarms train the reader to ignore triage. §54.2 shares the fix with §53.3 (one
tail-detection serves both the depth table and this bullet); §54.3 continues the §50.5/§26.2 program of gating the
whole rule set on reclaimable bytes rather than raw counts.

## 55. Graphs-Sample ASCII-Chart Audit: Redundant Sparklines (pass 36)

Line-by-line read of `docs/samples/scala-doku-full.graphs.md` (backlog item B) for chart legibility. The
`md-graphs` renderer's job is to add proportional bars/sparklines on top of the plain `md` tables (src/md.rs:150-155
docstring). The audit finds the additions are *sound* mathematically — `bar()` (md.rs:169) and `sparkline()`
(md.rs:207) both normalize to the series max, so a table's bar column and a standalone sparkline over the same
counts always agree in shape — but in two sections the renderer emits *both* a standalone sparkline **and** a
bar-column table over the identical histogram, doubling the visual and the vertical space for zero added
information.

### 55.1 String Length Distribution renders the same histogram twice (P2, Format)

`docs/samples/scala-doku-full.graphs.md:224` prints `` `▂▂▂▄▅▇█▂▂▂▂▂▂▂▂` `` — a 15-glyph sparkline over the
string-length histogram — immediately followed (lines 226-241) by a three-column table whose bar column
(`████████████████` peaking at bucket "64") plots the *same 15 counts*. The source is render_md.rs:133-149: line
133 collects `counts`, then under `if graphs` line 135 emits `sparkline(&counts)` **and** lines 136-149 emit a
`Table` whose third column is `bar(b.count, bmax, GRAPH_BAR_WIDTH)` over the same `counts`. Because both primitives
scale to the same max (`bmax` == the sparkline's internal `max`), the sparkline is a lower-resolution duplicate of
the table's bar column — the table already shows the shape *plus* the exact per-bucket values and upper bounds.

**Reason / fix:** the standalone sparkline is pure redundancy here; the bar-column table is strictly more
informative (same shape, plus labels and counts). Drop the `sparkline(&counts)` line (render_md.rs:135) from the
histogram section, keeping the bar-column table. This is the concrete instance §44.4 flagged abstractly ("relocate
redundant sparkline"); a sparkline earns its place only where there is *no* accompanying bar table (a compact
inline trend), which is not the case in either histogram section. **Availability: Format** — delete one line; the
plain-`md` branch (lines 150-155) is untouched, and no model/JSON change.

### 55.2 Top-Dominator Size Distribution: duplicate sparkline + duplicated min/max label (P2, Format)

`docs/samples/scala-doku-full.graphs.md:5909` prints `` `▂▂▂▂█▄▂▂▂▂▂▂▂▂▂▂▂▂▂▂▂`  (0 B – 22.9 MB) `` followed
(lines 5911-5932) by a 21-row `Size ≤ / Count / bar` table over the same bucket counts. Two separate redundancies:
(1) the sparkline duplicates the table's bar column exactly as in §55.1 (source render_graphs.rs:789-808: line 790
collects `counts`, 791-793 emits the sparkline, 795-807 emits the bar table over the same `counts`); (2) the
sparkline's trailing label `(0 B – 22.9 MB)` re-prints the "Smallest / largest retained: 0 B / 22.9 MB" bullet that
appears three lines above it (sample 5905; source render_graphs.rs:780-784). So the reader sees the min/max range
stated twice and the distribution shape drawn twice within one short section.

**Reason / fix:** delete the sparkline block (render_graphs.rs:791-794) and keep the labeled bucket table — it
carries the shape, the per-bucket counts, and the bucket bounds, and the min/max/median/total already appear as
bullets above it. If a one-glance trend line is still wanted, it belongs in the `compare` output where successive
dumps' distributions are shown side by side (§44.4), not stacked on top of a table that already draws the same
bars. **Availability: Format** — remove one `push_str`; no model/JSON change, plain `md` unaffected.

### 55.3 Summary and Priority-Summary deltas

- **§55.1 (P2, Format):** String Length Distribution (graphs.md:224 vs 226-241) draws the length histogram as both a
  standalone sparkline and a bar-column table over the same `counts` (render_md.rs:135 + 136-149); the sparkline is a
  lower-res duplicate. Drop render_md.rs:135.
- **§55.2 (P2, Format):** Top-Dominator Size Distribution (graphs.md:5909 vs 5911-5932) repeats the same
  double-render (render_graphs.rs:791-794 sparkline + 795-807 bar table) *and* re-prints the min/max range already in
  the bullets above (5905). Drop the sparkline block.

Both are the concrete symptoms §44.4 predicted: a sparkline adds value only where it stands alone as a compact trend;
stacked above a bar-column table over identical data it is noise. The fix is two deletions and shortens two sections
without losing any datum, keeping `md-graphs` legible and reinforcing the rule that each visual should show data no
other visual in the same section already shows.

## 56. HTML-Only Columns & Hand-Rolled Percentages: Cross-Format Comparability Breaks (pass 37)

A report is trustworthy only if a developer can open the HTML, the plain `md`, and the JSON side by side and see the
*same* numbers. This pass reads `web/src/App.tsx` against `render_md.rs`/`render_graphs.rs` for the two ways that
guarantee fails: (a) the HTML renders **columns the Markdown views do not**, so a value exists in one format and not
the others; and (b) the HTML computes several of those percentages with inline `x / y * 100).toFixed(n)` arithmetic
that **bypasses the shared `fmtPct`/`pctOf` helpers** it already imports (App.tsx:3), so even where a percent is
shown in more than one place it is rounded differently and lacks the `<0.1%` floor. `format.ts` exists precisely to
keep HTML "match[ing] the Markdown/JSON views byte-for-byte" (format.ts:1-2) — these sites defeat that intent.

### 56.1 Class Histogram: HTML has a "% Heap" column md/graphs lack, at a precision used nowhere else (P1, Compute-cheap)

The Class Histogram renders **different column sets per format**:
- **md** (render_md.rs:882-919): `# / Class / Instances / Shallow Heap / Largest / Retained Heap` — six columns, no
  percentage.
- **graphs** (render_graphs.rs:304-334): the same six plus a retained-proportional `bar()` column.
- **HTML** (App.tsx:578-603): `# / Class / [Loader] / Instances / Shallow Heap / Largest / Retained Heap / % Heap` —
  it adds a **Loader** column (when `showLoader`) *and* a **"% Heap"** column (header App.tsx:581) that neither
  Markdown view has.

Two problems. (1) **Comparability:** a reader cross-referencing the histogram sees a per-class "% Heap" figure only
in HTML; it is absent from md, graphs, and — since it is computed at render time from `retained`/`totalShallow` — it
is not a stored field either. (2) **Precision inconsistency:** the value is `(h.retained / totalShallow *
100).toFixed(2)` (App.tsx:603) — **two** decimals with **no `<0.1%` floor**, while every *other* "% Heap" column in
the tool goes through `fmt_pct` at **one** decimal with the floor (render_md.rs:609-610 for Leak Suspects/Top
Consumers; the HTML mirror is `fmtPct`, format.ts:31). So the label "% Heap" means one-decimal-floored in Leak
Suspects and two-decimal-unfloored in the histogram, *within the same HTML page*.

**Reason / fix:** decide whether "% of reachable heap" belongs in the histogram at all; if yes (it is useful — it
tells you what fraction of the heap each class dominates), add the column to **md and graphs too** and render it
through `fmt_pct` (Rust) / `fmtPct` (TS) so all three agree at one decimal with the `<0.1%` floor. If no, drop the
HTML column. Either way the three formats must carry the same columns (plan comparability rule). **Availability:
Compute-cheap** — `retained` and the reachable-heap denominator (`overview.total_shallow`, the §48/§45 basis) are
already present in every format; this is a column-parity + one-helper-call change, no new data. Ties to §45.2
(single `HEAP_BASIS_LABEL`) and §48 (one denominator): the histogram % must use the *same* basis and formatter as
every other "% Heap".

### 56.2 Two HTML distribution tables add hand-rolled percent columns md/graphs don't render (P2, Compute-cheap)

The same pattern recurs in two size-distribution tables that §55 showed render only `bound / count / bar` in md and
graphs:
- **Top-Dominator Size Distribution** (App.tsx:748-757): HTML adds a **"% of Dom."** column computed
  `(b.count / d.count * 100).toFixed(1)` (App.tsx:757) — one decimal, **no `<0.1%` floor**, so a real 0.02%-of-
  dominators bucket prints `0.0%` (indistinguishable from empty). md/graphs (render_graphs.rs:795-807) show only
  `Size ≤ / Count / bar`.
- **Class Histogram → retention** aside: the Retention Concentration and header-overhead tables use the `_bp`
  (basis-point) path — `(row.pct_of_heap_bp / 100).toFixed(2)` (App.tsx:1098) and `(…header_pct_of_shallow_bp /
  100).toFixed(1)` (App.tsx:1174) — i.e. **three different rounding rules** (`.toFixed(2)` here, `.toFixed(1)`
  there) for percentages on one page, none through `fmtPct`.

**Reason / fix:** for the "% of Dom." column, either add the same column to md/graphs (routed through the shared
formatter) or drop it; for the `_bp` sites, route the basis-point value through a single `fmtPct(bp / 100)` so the
floor and decimal count match the rest of the report. A bucket that is a real but sub-0.1% share should read
`<0.1%`, never `0.0%` — the whole reason `fmtPct` exists (format.ts:29-33). **Availability: Compute-cheap** — every
value is already in the row; this is routing existing numbers through the existing helper. Reinforces §41
(formatting primitives) and §45 (percentage-basis consistency): the HTML must use `fmtPct`/`pctOf` everywhere it
prints a percent, exactly as `format.ts` promises.

### 56.3 Summary and Priority-Summary deltas

- **§56.1 (P1, Compute-cheap):** Class Histogram carries a per-class "% Heap" column in **HTML only** (App.tsx:581/
  603), computed `.toFixed(2)` with no `<0.1%` floor — md (render_md.rs:882-919) and graphs
  (render_graphs.rs:304-334) have no such column, and the "% Heap" label elsewhere means one-decimal-floored
  (`fmt_pct`). Add the column to md+graphs through `fmt_pct`/`fmtPct`, or drop it; unify the basis (§45.2/§48).
- **§56.2 (P2, Compute-cheap):** HTML adds a "% of Dom." column to Top-Dominator Size Distribution (App.tsx:757,
  `.toFixed(1)`, no floor) that md/graphs don't render, and formats `_bp` percents with three different rounding
  rules (App.tsx:1098 `.toFixed(2)`, 1174 `.toFixed(1)`, 757 `.toFixed(1)` no floor); route all through `fmtPct`
  and match column sets across formats.

Both findings are one theme: the HTML has quietly grown percent columns and rounding rules the Markdown/JSON views
don't share, defeating the byte-for-byte-parity contract that `format.ts` was written to uphold (format.ts:1-2). The
fix is mechanical — either add the column to all three formats or remove it, and in every case call the shared
formatter — but the payoff is real: a developer who computes "class X is 3.5% of the heap" from the HTML should get
the identical figure from the md report and the same value, unrounded, from the JSON. §56.1 is the load-bearing one
because the histogram is a primary "where is the heap" table and the divergent column is the most likely to be
quoted.

## 57. Arrays Section: Size and Waste Are Computed but Never Joined (pass 38)

§7 gave the Arrays section an early, bullet-style pass; this revisits it with field/line citations now that the
later passes have set the bar. The finding: the report *separately* knows (a) which individual arrays are biggest
(Top Arrays) and (b) that tens of thousands of object arrays are nearly empty (Array Fill Ratio) — but the two are
rendered as disconnected tables, so a developer can never see that a specific big array is mostly null padding, and
the single largest array in the dump is left completely unattributed. Both the "where is the heap" and "where is it
wasted" questions are answerable from data already on the `Report`; they just aren't joined.

### 57.1 Top Arrays shows Length and Shallow but never fill/null-density (P2, Compute-cheap→Add)

`TopArrayRow` (model.rs:920-930) carries `array_class / length / shallow / obj_index_1based / owner` — **no
occupied-slot or null-count field.** So `render_top_arrays` (render_md.rs:2056-2119) renders columns
`Array class / Length / Shallow / [Owner] / [bar]` and nothing about how full the array is. In the sample the top
object array is `` `java.lang.Object[]` | 131,072 | 512.0 KB | — `` (scala-doku-full.md:2941): a half-megabyte array
with **no indication whether those 131,072 slots are populated or 95% null.** Meanwhile the Array Fill Ratio section
three tables up reports **37,926 object arrays in the 0–10%-full bucket wasting 5.9 MB** (md:2800) — the largest
single array-waste line in the whole dump — but bucketed and nameless. The reader cannot cross the two: is that
131K-slot `Object[]` one of the 37,926 near-empty ones, or a genuinely-full 512 KB payload? The report has the
answer and won't say.

**Reason / fix:** add a **"Used / Length"** (or "Fill %") column to the Top Arrays *individual* table so the biggest
arrays are immediately triaged as "full and legitimately large" vs "huge and mostly empty — reclaimable." For object
arrays the non-null count is exactly what the fill-ratio pass computes per array (it must count non-null slots to
bucket them, `ArrayFillRatio`, model.rs:868-875); the question is whether that per-array count is *retained* or only
folded into buckets. **Availability: Compute-cheap if the fill pass keeps the per-array non-null count for the
top-N arrays it already visits; Add (small) if the count is currently discarded after bucketing** — either way it
piggybacks a pass that already touches every object array. Route the new column through all three renderers +
`types.ts` (the §56 comparability rule). This directly upgrades the Arrays section from "here are big arrays" to
"here are big arrays *and which ones are padding*," serving "where is heap wasted" at the per-instance level §8's
fill-ratio buckets only serve in aggregate. (Supersedes §7.3 with a concrete field/line grounding.)

### 57.2 The single largest array is unattributed; fill-ratio attribution stops at collection backing stores (P2, Compute-cheap)

The biggest object array — `java.lang.Object[]` 131,072 slots (md:2941) — has Owner `—`, as do 5 of the top-10
object arrays (md:2941/2944/2945/2948/2950) and every primitive array below the top few. `TopArrayRow.owner`
(model.rs:925-929) is populated only from "the `--collections` holder-edge scan," which resolves arrays that back a
*tracked collection* (HashMap#table etc.) but leaves raw application `Object[]`/`Formula[]` unattributed. The
Fill-Ratio "Likely wasters by field" table (md:2817-2819) has the same blind spot: it names only
`HashMap#table` / `ConcurrentHashMap#table` / `SetN#elements` — the collection internals — never the raw
`cafesat.*[]` arrays that dominate Top Arrays with real owners (`cafesat.sat.Solver#watched`, md:2942). So the two
attribution surfaces cover *collection-backing* arrays well and *raw* arrays not at all, and the largest single
array in the dump falls in the gap.

**Reason / fix:** widen array owner-resolution beyond the collection holder-edge scan to any inbound
`Class#field` edge (the same field-attribution machinery §47 proposes for the single-big-value views), so a raw
`Object[]` held by an application field gets a `Class#field` label like the collection-backed ones already do. A
131 KB-slot array with Owner `—` is the report failing at its core job — telling you *where the heap comes from* —
for the biggest array it found. **Availability: Compute-cheap** where an inbound-edge index exists (the dominator /
reference machinery already walks incoming edges); **Add** only if the array's referrers aren't retained. Ties to
§47 (field-labeled attribution) and §30.2a (path-to-GC-roots): the same inbound-edge plumbing serves all three.

### 57.3 Summary and Priority-Summary deltas

- **§57.1 (P2, Compute-cheap→Add):** Top Arrays (render_md.rs:2077-2102) shows `Length` + `Shallow` but no fill/
  null-density, so the 131,072-slot `Object[]` (md:2941) can't be told apart from padding; the Fill Ratio section
  knows 37,926 arrays are 0–10% full wasting 5.9 MB (md:2800) but bucketed and nameless. Add a Used/Length column to
  the individual Top Arrays table (all three formats + JSON), reusing the per-array non-null count the fill pass
  already needs. `TopArrayRow` (model.rs:920) has no fill field today.
- **§57.2 (P2, Compute-cheap):** the biggest object array and 5 of the top 10 have Owner `—` (md:2941+) because
  `TopArrayRow.owner` (model.rs:925) is filled only from the `--collections` holder-edge scan (collection backing
  stores); raw application arrays go unattributed and are absent from "Likely wasters by field" (md:2817). Widen
  array attribution to any inbound `Class#field` edge (shared with §47/§30.2a plumbing).

Both findings share one root: the Arrays section computes size (Top Arrays) and waste (Fill Ratio) in separate
passes and never joins them, and attribution is scoped to collection internals rather than arbitrary fields. §57.1
is the higher-leverage fix — a single Used/Length column turns the largest "where is the heap" table into a "where
is it wasted" table at the instance level — and it reuses a count the fill-ratio pass already derives, so the cost
is a column, not a scan.

## 58. Basis-Point (`_bp`) Storage & Rounding Pipeline: Four Precisions, One Lossy Round-Trip (pass 39)

Deep-dive E, focused on the ten `_bp` (integer basis-point, 100 bp = 1%) fields the model uses to store
percentages. §27/§45/§53 audited individual percentages; this pass follows the whole `_bp` pipeline
model→renderer and finds three systemic problems: the same "share of reachable heap" concept is printed at **four
different decimal precisions**, **none** of them routed through the `<0.1%`-floored `fmt_pct`, and one table
**reconstructs bytes from a rounded basis-point value** the analysis pass had exactly — a lossy round-trip that
discards precision the tool already computed.

The ten fields (model.rs): `top1_bp`/`top10_bp`/`top100_bp` (152-154), `pct_of_heap_bp` (260),
`header_pct_of_shallow_bp` (280), `top_class_concentration_bp` (372), `pct_bp` (576), `threshold_bp` (623),
`lower_ratio_bp`/`upper_ratio_bp` (838-839). All document "100 bp = 1%"; the doc discipline is good. The rendering is
where it breaks.

### 58.1 Four different decimal precisions for the same "% of reachable heap" concept (P2, Compute-cheap)

Every `_bp` render is `bp as f64 / 100.0` formatted with a hardcoded precision, and they disagree:
- **`{:.0}%`** — fill-ratio bucket labels (`fill_ratio_label`, render_md.rs:1587-1589: `"{lo:.0}–{hi:.0}%"`).
- **`{:.1}%`** — Retention Concentration Top-1/10/100 (render_md.rs:547/552/557), header-overhead
  `header_pct_of_shallow_bp` (3357), top-class concentration (765).
- **`{:.2}%`** — Boxed-Numbers `pct_of_heap_bp` (render_md.rs:3284).
- Plus the **HTML** adds `.toFixed(1)`/`.toFixed(2)` on the *same* `_bp` values (§56.2, App.tsx:1098/1174).

So "share of reachable heap" prints as `41%`, `41.1%`, and `41.08%` in three different sections of one report, and
**none** of the four goes through `fmt_pct` (format.rs:299), so a real sub-0.05% share renders as `0.0%`/`0.00%`
instead of `<0.1%` — the exact defect `fmt_pct` was written to prevent (§41.3/§45.1). A reader comparing the
Boxed-Numbers "0.02% of heap" against a Concentration row rounded to "0.0%" cannot tell they mean the same magnitude.

**Reason / fix:** route every `_bp`→percent render through one helper — `fmt_pct(bp as f64 / 100.0)` — so all
`_bp` percentages share one decimal count *and* the `<0.1%` floor. If a section genuinely needs 2 decimals (the
MAT-style exact figure, §32.7 `.mat-exact`), make that an explicit second formatter used consistently, not an
ad-hoc `{:.2}` at one callsite. **Availability: Compute-cheap** — replace ~7 `format!("{:.N}%", bp as f64 / 100.0)`
sites with one shared call; mirror in `web/src/format.ts` so HTML matches (§56). This is the Rust-side companion to
§56.2 (which found the same disorder in App.tsx) and closes §45.1 for the `_bp` path specifically.

### 58.2 Retention Concentration reconstructs bytes from a rounded bp — a lossy round-trip (P2, Compute-cheap)

`render_retention_concentration` builds the "Retained" byte column by *back-computing bytes from the stored basis
point*: `let bp_to_bytes = |bp: u32| -> u64 { (bp as u64 * total) / 10_000 };` (render_md.rs:540, identical in
render_graphs.rs:473), then `format_bytes(bp_to_bytes(rc.top1_bp))` (548/553/558). But `top1_bp` is itself a rounded
integer basis point — the concentration pass computed the true retained bytes of the top-1/10/100 dominators, then
stored only `round(retained / total * 10_000)` and threw the bytes away, so the table reconstructs an
*approximation* of a value the tool had exactly. At 30 MB total, one basis point ≈ 3 KB, so the reconstructed
"Retained" is quantized to ~3 KB steps and can disagree with the true retained by up to half a bucket. Worse, this
is the very table §53.1/§53.2 flagged for physically-impossible values (84 GB retained, 10000% cumulative): the
byte column is derived from the same corrupted `_bp`, so fixing the bp fixes both the percent *and* the bytes, but
storing bytes directly would have made the byte column immune to the percentage bug entirely.

**Reason / fix:** store the true retained bytes for top-1/10/100 alongside (or instead of) the basis point —
`RetentionSummary` (model.rs:148-157) already carries `total_retained: u64`, so adding `top1_retained`/
`top10_retained`/`top100_retained: u64` is natural and lets the "Retained" column print the exact figure while the
percent is derived *from the bytes* at render time (the correct direction: bytes are the ground truth, percent is
the derived view — never the reverse). **Availability: Compute-cheap** — the concentration pass already sums these
retained bytes to compute the bp; keep them instead of discarding. SCHEMA bump (Option fields, per the plan's
`#[serde(default)]` rule); mirror in `types.ts`. Removes the `bp_to_bytes` round-trip in both md and graphs.

### 58.3 Summary and Priority-Summary deltas

- **§58.1 (P2, Compute-cheap):** the same "% of reachable heap" concept renders at `{:.0}%` (fill labels
  render_md.rs:1587), `{:.1}%` (concentration 547, header 3357, top-class 765), and `{:.2}%` (boxed 3284) — plus the
  HTML's own `.toFixed` (§56.2) — and **none** uses `fmt_pct`, so sub-0.05% shares print `0.0%` not `<0.1%`. Route
  every `_bp`→percent through `fmt_pct(bp/100.0)`; mirror in format.ts.
- **§58.2 (P2, Compute-cheap):** Retention Concentration's "Retained" byte column is back-computed from the rounded
  bp (`bp_to_bytes`, render_md.rs:540 / render_graphs.rs:473), quantizing an exact value the pass had (~3 KB steps
  at 30 MB) and inheriting the §53.1/§53.2 corruption. Store `top{1,10,100}_retained: u64` on `RetentionSummary`
  (model.rs:148) and derive the percent *from* the bytes; SCHEMA bump + `types.ts`.

The unifying defect: percentages are treated as the stored ground truth and bytes as the derived quantity, when it
should be the reverse — bytes are exact and additive, percentages are the lossy view. §58.2 is the higher-leverage
fix because it both removes a lossy round-trip *and* makes the "Retained" column immune to the concentration-percent
bug (§53); §58.1 is the mechanical cleanup that finally brings the `_bp` path under the `fmt_pct` contract the rest
of the report already follows (§41.3/§45.1/§56.2).

## 59. Empty-Section Rendering: Three Syntaxes, Generic Messages, and ToC Links to "None" (pass 40)

Deep-dive F revisited at source depth. §31 discussed degenerate-heap behavior abstractly; this pass audits the
*actual empty-branch code* in `render_md.rs` and finds the report handles "this section has no data" three
incompatible ways, uses a bare uninformative placeholder for most sections, and — because the ToC and section bodies
guard on *different* predicates — links the reader to sections whose entire content is the word "None." All
fixtures on disk are 20–73 MB (no tiny/empty dump exists, itself §31.1's open item), so this is grounded in the
render code and its guards rather than a captured tiny-dump sample.

### 59.1 Three different empty-message syntaxes coexist in one renderer (P3, Format)

An exhaustive grep of `render_md.rs` empty-branches yields **three mutually inconsistent placeholder styles**:
- **Bare generic underscore** — `"_None._"` appears **12 times** (render_md.rs:1536, 1613, 1680, 1738, 1791, 2001,
  2066, 2125, 2188, 2242, 2312, 2451, 2577 …), the default for most tables.
- **Descriptive underscore** — six section-specific messages: `"_No dominant retainer found._"` (427),
  `"_No package retains more than 1% of the total retained heap._"` (1277), `"_No thread call stacks were recorded
  in this dump._"` (1324), `"_No class-loader components were resolved in this dump._"` (1474), `"_No soft, weak, or
  phantom references found._"`, `"_No per-frame allocation data is available…_"`.
- **Asterisk style** — four `"*No X.*"` messages: `"*No arrays found.*"` (1525), `"*No immediate dominators.*"`,
  `"*No significant drops.*"`, `"*No unreachable objects.*"`.

So within one document the reader sees `_None._`, `_No thread call stacks were recorded in this dump._`, and
`*No arrays found.*` — italic-via-underscore *and* italic-via-asterisk, generic *and* specific — depending on which
section happens to be empty. The asterisk vs underscore choice is cosmetically identical in rendered Markdown but
signals two different authors/eras, and the graphs renderer (render_graphs.rs) shares almost none of these strings
(it delegates to the shared fns), so an empty section can even read differently between md and md-graphs.

**Reason / fix:** pick one syntax (underscore, matching the dominant 12 callsites) and one policy: every empty
section states *what* is absent and *why it might be*, not a bare "None." A reader who sees `_None._` under "Fields
by Retained Size" cannot tell whether the analysis ran and found nothing or was skipped; `_No field retains a
significant share (requires `--collections`)._` answers both. **Availability: Format** — replace the 12 bare
`_None._` with section-specific messages (the six descriptive ones are the model to follow) and normalize the four
asterisk strings to underscore. No model/JSON change. This is the §31.4/§33 actionability principle applied to the
empty state: an empty section is still a finding ("no leak-shaped concentration here") and should say so.

### 59.2 ToC lists Option-backed sections whose body renders only "None" (P2, Compute-cheap)

The ToC (`render_toc`, render_md.rs:286-327) gates four sections on `Option::is_some()`:
`fields_by_size.is_some()` (306), `biggest_collections.is_some()` (309), `collection_contents.is_some()` (312),
`alloc_sites.is_some()` (317). But the *bodies* guard on the **inner vec** being empty: `render_fields_by_size`
emits the full `## Fields by Retained Size (Class#field)` heading + two-sentence intro prose, then
`if f.rows.is_empty() { out.push_str("_None._"); return; }` (render_md.rs:2304-2313). So when the analysis produced
a `Some(FieldsBySize { rows: [] })` — ran but found nothing — the ToC shows a clickable "Fields by Retained Size"
bullet that jumps to a heading whose entire content is intro prose and the word "None." The same
`is_some()`-in-ToC / `rows.is_empty()`-in-body split applies to all four Option-backed sections.

This is the §31.4 mismatch made concrete: the ToC promises a section, the anchor resolves (§42 anchor integrity is
fine), but the destination is empty. Worse than a bare empty section, because the ToC *advertised* it. **Reason /
fix:** make the ToC guard match the body's real emptiness check — e.g. `r.fields_by_size.as_ref().is_some_and(|f|
!f.rows.is_empty())` — so an Option-Some-but-empty section is neither listed nor headed; or, conversely, always
emit the heading and give it a useful empty message (59.1) and keep the ToC link honest by pointing at a section
that at least explains its emptiness. **Availability: Compute-cheap** — the emptiness predicate is one field access
already performed in the body; hoist the same check into the ToC guard. Do it identically in `render_toc` and
`render_toc_graphs` (the §55/§56 comparability rule — ToC parity across formats).

### 59.3 Summary and Priority-Summary deltas

- **§59.1 (P3, Format):** empty sections use three syntaxes — 12× bare `_None._` (render_md.rs:1536…2577), 6
  descriptive `_No X_`, 4 asterisk `*No X.*` (1525…) — mixing generic and specific, underscore and asterisk, in one
  renderer; graphs shares almost none of the strings. Normalize to one underscore syntax and give every empty
  section a "what's absent and why" message (the six descriptive ones are the template).
- **§59.2 (P2, Compute-cheap):** the ToC gates `fields_by_size`/`biggest_collections`/`collection_contents`/
  `alloc_sites` on `Option::is_some()` (render_md.rs:306-317) but the bodies guard on the inner vec (e.g.
  `f.rows.is_empty()`, 2311), so an Option-Some-but-empty analysis yields a ToC link to a heading whose only content
  is "None." Match the ToC guard to the body's emptiness check (or emit a useful empty message); mirror in
  `render_toc_graphs`.

Both are the empty-state corollary of the document's recurring theme: the report is rich when data exists and
under-considered when it doesn't. §59.2 is the higher priority because a ToC link that leads to nothing erodes trust
in the whole navigation (§42), and the fix is a one-field predicate already computed in the body; §59.1 is a
Format-only normalization that turns twelve uninformative "None"s into statements a developer can act on (§33).
