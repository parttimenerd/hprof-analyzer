# OQL evaluator audit findings

Date: 2026-11-05  
Status: **Rechecking against a changing local debug binary/worktree. Earlier and current observations differ; verify each finding against a clean build.**  
Fixture: `tests/fixtures/dump_4_philosophers.hprof`  
Binary: `target/debug/hprof-analyzer` (the assistant did not build it)

This document consolidates issues observed while exercising the README and
[`docs/OQL.md`](OQL.md) examples. Source files changed during the audit, and the
local binary's behavior changed as well. Findings marked below as no longer
reproducing need confirmation with a clean build. No source edits or Cargo tests
were performed by the assistant.

## Evaluator correctness

### OQL-1. Reachable-only filtering differs by result shape

The query CLI help and OQL guide say reachable-only is the default, but scalar
aggregates and some grouped aggregates include unreachable objects. On the
fixture, direct `COUNT(*) FROM java.lang.String` returned **24,760**, while
self-membership against the same `@objectAddress` subquery returned **23,331**.
The direct count remains 24,760 with `--reachable-only`; `--all` makes the
self-membership count 24,760 as well. This indicates the membership path applies
the reachable filter but aggregate accumulation does not.

`GROUP BY @displayName` likewise showed all-object counts, while grouping by
`@category` produced totals of 201,257 + 730 = **201,987**, the reachable
`INSTANCEOF java.lang.Object` count. Reachability behavior therefore also
appears to vary by grouping key/execution path.

Reference counts observed:

| Query population | Reachable | All |
| --- | ---: | ---: |
| `java.lang.String` | 23,331 | 24,760 |
| `java.lang.Thread` | 27 | 29 |
| `INSTANCEOF java.lang.Object` | 201,987 | 203,782 |
| Quoted Java-package regex | 167,932 | 169,980 |

### OQL-2. Row `LIMIT` is applied before reachability pruning

For `SELECT @objectId FROM java.lang.Thread LIMIT 3`, reachable-only mode
returned two rows, while `--all` returned three. The ordered variant
(`ORDER BY @objectId LIMIT 3`) also returned two in reachable-only mode. A
result limit should cap the final eligible result, rather than raw matches that
are later discarded as unreachable.

**Recheck (2026-11-05):** This no longer reproduces in the current local binary:
both unordered and ordered `LIMIT 3` return three reachable Thread rows. This
appears fixed by the local worktree changes; verify with a clean-build regression
test before closing it.

### OQL-3. HTTP request-level `limit` is ignored

A JSON request with `{"query":"SELECT @displayName FROM java.lang.Thread",
"limit":1}` returned 27 rows. With SQL `LIMIT 5` and request-level `limit: 1`,
the response contained four rows, consistent with SQL limit being applied
before reachable-only filtering and the request override not being applied.

### OQL-4. `LIMIT 0` is inconsistent with the REPL guidance

A row query with `LIMIT 0` returned zero rows; `COUNT(*) ... LIMIT 0` still
returned its aggregate row; a UNION with branch `LIMIT 0` returned zero rows.
The REPL message in `src/query/repl.rs` says to use `LIMIT 0` “for all,” which
does not match the observed row-query behavior. Decide and test the intended
semantics, then make the message consistent.

**Recheck (2026-11-05):** `src/query/repl.rs` now removes the inaccurate “LIMIT 0
for all” advice. The runtime distinction remains: row queries return zero rows
for `LIMIT 0`, while an aggregate still emits its scalar row.

## `FROM ENTRIES`

### OQL-5. Logical output columns fail field validation

The guide's `SELECT index, element FROM ENTRIES java.util.ArrayList` query was
rejected with `unknown field 'index' on java.util.ArrayList`. The planner appears
to validate these logical entry-output names against fields of the collection
class before entry expansion. `SELECT * FROM ENTRIES ...` did produce an
actionable full-pipeline error in query-only mode.

### OQL-6. Full-report queries still lack the required forward graph

The same `FROM ENTRIES` query returned “requires the full analysis pipeline”
when run in a report with `--full-analysis --ref-paths`. In that report,
retained-size and dominator OQL queries succeeded. The query execution path does
not appear to provide the forward-reference data required for entry expansion,
even when those report options are requested.

### OQL-7. Address-valued subquery sources silently return no rows

`FROM (SELECT @objectAddress FROM java.lang.Thread) s` produced zero rows with
no error, both for `SELECT *` and `SELECT @objectId` in the outer query. The
corresponding `FROM (SELECT @objectId FROM java.lang.Thread) s` did produce
Thread rows. Source inspection confirms the mismatch: `enforce_from_subquery_projection`
explicitly accepts `@objectAddress`, but the FROM semi-join calls
`row_dense_index`, which interprets every integer result as a dense index. A
heap address is not a dense index, so the valid address projection cannot join
to the outer rows. The guide advertises subqueries generally as a row source.
Convert accepted addresses to dense indices before joining, or reject this
projection explicitly rather than returning a successful empty result.

### OQL-8. Nested uncorrelated subqueries are not executed correctly

The documented non-correlated EXISTS behavior works at one level: on this
fixture, `EXISTS (SELECT * FROM java.lang.Thread)` returns 134,277 objects from
`java.lang.Object`. Adding a second, also non-correlated `EXISTS` level for
`java.lang.String` changes the result to zero, although the inner class has
instances. A nested `IN` query instead panicked with the internal
`IN(<subquery>) predicate must be resolved at plan time` unreachable-code
message. Source inspection confirms the runner gathers/materializes only one
subquery level, while nested subplans are still planned and no explicit
unsupported-nesting error is produced. Either execute subqueries recursively
or reject nested forms during planning; they must not silently return wrong
results or panic.

## Documentation / API contract

### OQL-9. Reachability default disagrees in README

[README.md](../README.md#L227) says `--reachable-only` is off by default. CLI
help and [the OQL guide](OQL.md#L801) say reachable-only is the default.
Reconcile these descriptions with actual source behavior.

### OQL-10. Several worked-example counts are stale or mismatched

The observed results differ from the guide's documented fixture numbers:

- String count: 23,331 reachable, 24,760 all objects.
- Thread `LIMIT 3`: two reachable rows, three with `--all`.
- Quoted Java-package regex: 167,932 reachable, 169,980 all objects.
- `INSTANCEOF java.lang.Object`: 201,987 reachable, 203,782 all objects.
  The 134,277 figure is the exact `java.lang.Object` class count, not the
  subtype-inclusive `INSTANCEOF` count.

Rerun all affected examples after the evaluator fix and label any `--all`
example explicitly.

### OQL-11. Edge-attribute projection restriction is omitted

`@inbounds` and `@outbounds` worked when each was the sole selected item. A
query selecting either alongside another column failed with an error explaining
that it must be the only select item. The OQL reference should document this
special row-expanding behavior and restriction.

### OQL-12. HTTP success response envelope is underspecified

The HTTP endpoint documentation describes `POST /` as returning a JSON
`QueryResult`. The observed success body is wrapped as
`{"ok":true,"result":{...QueryResult...}}`, while errors are also wrapped.
Clarify the documented response schema; change behavior only if the wrapper is
not the intended API contract.

## Additional evaluator correctness

### OQL-13. Multiple `IN` predicates on the same attribute reuse the first set

With `--all`, `@objectAddress IN (SELECT @objectAddress FROM java.lang.Thread)`
matched 29 objects, and the equivalent String membership matched 24,760. An
`OR` combining both returned only the first branch's count (29); reversing the
branches returned only the String count (24,760), rather than their disjoint
union of 24,789. An `AND` of the same two disjoint memberships incorrectly
returned 29 instead of zero. Source inspection finds `eval_in_subquery` selects
the first `InSet` whose `lhs` matches, so later subqueries on the same attribute
are ignored. Resolve each predicate to its own subquery set rather than keying
only by the left-hand attribute.

**Recheck (2026-11-05):** The current local binary returns the expected 24,789
for the Thread-or-String membership query. The worktree now uses a positional
cursor for each `IN`; that change introduces the short-circuit defect recorded
as OQL-16 below.

### OQL-14. `ORDER BY` on an unselected expression is not applied

`SELECT @objectId FROM INSTANCEOF java.lang.Object ORDER BY @usedHeapSize DESC
LIMIT 3` returned scan-order IDs and a note saying that sorting was not applied
because the key was not selected. Adding `@usedHeapSize` to the projection
produced the expected 144-byte top rows. The OQL guide says `ORDER BY <expr>`
sorts the full result and does not state this restriction. Implement sorting by
unprojected keys or document/reject the unsupported form rather than returning
unsorted rows under a valid `ORDER BY` clause.

### OQL-15. `IN` ignores its left-hand attribute

With `--all`, `@objectAddress IN (SELECT @objectAddress FROM java.lang.Thread)`
correctly matched 29 objects. Replacing the left side with `@objectId` or
`@usedHeapSize` still matched the same 29. Source inspection confirms
`eval_in_subquery` looks up the `InSet` by LHS but then always tests the current
object's real address; it never evaluates the declared LHS attribute. The
planner should either restrict the LHS to `@objectAddress` with an actionable
error (the executor documents that as its only supported form), or evaluate
and type-check the actual LHS value.

**Recheck (2026-11-05):** The current local binary returns zero for
`@objectId IN (SELECT @objectAddress FROM java.lang.Thread)`, as expected for
these nonmatching numeric domains. The worktree now evaluates the LHS value, so
the earlier “always tests the object's address” behavior no longer reproduces.

### OQL-16. Short-circuiting shifts positional `IN` subquery sets

The current worktree assigns `IN` sets in traversal order and consumes them via
a per-row cursor. When boolean short-circuiting skips an earlier `IN`, a later
predicate consumes the earlier predicate's set. With `--all`, this query should
return zero because a Thread cannot also be a String:

`(@objectId = 65980 OR @objectAddress IN (SELECT @objectAddress FROM java.lang.Thread)) AND @objectAddress IN (SELECT @objectAddress FROM java.lang.String)`

It returned **1**. Reordering the `OR` terms returned **0**, confirming the
result depends on evaluation order rather than predicate meaning. Bind each
subquery set to its predicate identity (not a cursor advanced only when that
predicate executes), or otherwise preserve alignment through short-circuiting.

### OQL-17. Short-circuiting also shifts positional `EXISTS` results

The same cursor hazard affects multiple `EXISTS` predicates. For a Thread with
`@objectId = 65980`, this query should return one object: the first OR term is
true, and the final `EXISTS` over Threads is true:

`(@objectId = 65980 OR EXISTS (SELECT * FROM java.lang.Thread WHERE @objectId < 0)) AND EXISTS (SELECT * FROM java.lang.Thread)`

It returned zero. Reordering the OR terms so the false `EXISTS` is evaluated
before the matching object-ID test returned one. The `exists_cursor` is likewise
advanced only when an `EXISTS` node is reached; bind each result to its
predicate, or prevent cursor drift under short-circuit evaluation.

### OQL-22. A GROUP BY query in a batch can disable a sibling row query's LIMIT

With default reachable-only filtering, running these two queries together
returned **27** rows for the Thread query, ignoring its `LIMIT 3`:

`SELECT @objectId FROM java.lang.Thread LIMIT 3`

`SELECT @category, COUNT(*) FROM INSTANCEOF java.lang.Object GROUP BY @category`

The Thread query alone returned three rows. A batch of two limited row queries
also returned three rows for each, and the same GROUP BY + Thread batch with
`--all` returned three Thread rows. This points to an interaction between
reachable-only processing and batching when a GROUP BY forces the shared scan
to continue; the precise cause needs verification against a clean build. Keep
per-query LIMIT enforcement independent of whether sibling queries require a
full scan.

### OQL-23. Late-phase `COUNT(DISTINCT ...)` counts every non-null value

With `--all`, `COUNT(DISTINCT @retainedHeapSize)` returned **203,782**, the
same as `COUNT(@retainedHeapSize)`. As an independent oracle, projecting all
retained-size values and counting unique values gave **388**. The scan-time
control also distinguished `COUNT(DISTINCT @usedHeapSize)` (**17**) from
`COUNT(@usedHeapSize)` (**203,782**). In `stage_runner.rs`, both retained-size
aggregate paths explicitly implement `CountDistinct` as a non-null count, and
the planner accepts the retained-size form. Use a distinct-value set in the
late phase or reject this aggregate there; do not silently return the ordinary
count.

### OQL-24. `FILTER` is ignored or evaluated too early for retained aggregates

With `--all`,
`COUNT(@retainedHeapSize) FILTER (WHERE @usedHeapSize > 100)` returned
**203,782**, while the equivalent qualifying population
`COUNT(*) ... WHERE @usedHeapSize > 100` is **438**. The late aggregate path is
not applying the aggregate's FILTER predicate. A retained-size FILTER also
misbehaves: `COUNT(*) FILTER (WHERE @retainedHeapSize > 1000)` returned **0**,
whereas the corresponding WHERE query returned **505**. In `plan.rs`, aggregate
planning collects needs from the argument but not the FILTER predicate; in
`stage_runner.rs`, the retained aggregate loops fold values without testing
`filter`. Collect FILTER dependencies and evaluate each predicate at the phase
where its attributes are available.

### OQL-25. Subqueries outside WHERE are not planned

Subqueries inside expressions other than the top-level WHERE predicate are not
collected into execution subplans. On the fixture, an `EXISTS` filter over the
nonempty Thread class returned **0** for `COUNT(*)`, and
`CASE WHEN EXISTS (SELECT * FROM java.lang.Thread) THEN 1 ELSE 0 END` returned
**0** instead of **1**. An `IN (subquery)` inside an aggregate FILTER panicked
with `IN(<subquery>) predicate must be resolved at plan time`. The planner only
collects IN/EXISTS subplans from `q.where_`; discover them in aggregate FILTERs
and SELECT-expression predicates too, or reject those placements before scan.

### OQL-27. Unsupported HAVING predicates silently keep every group

`HAVING EXISTS (SELECT * FROM java.lang.String WHERE @objectId < 0)` returned
both object-category groups, even though the subquery count was zero. The
HAVING evaluator's fallback arm returns `true` for predicate variants it does
not handle, including subqueries. Either evaluate the predicate with planned
subquery results or return an actionable unsupported-expression error; never
silently retain all groups.

### OQL-28. `String.toString()` returns NULL instead of the string contents

On the fixture, `SELECT toString(s) FROM java.lang.String s LIMIT 3` returned
decoded string values, while `SELECT s.toString() FROM java.lang.String s
LIMIT 3` returned three NULLs. A Java String's `toString()` should return its
contents. The method-call projection does not arm the late String-value capture
path that the function form uses; route it through the same resolution or
reject it rather than returning NULL.

### OQL-29. Mixed integer/float comparisons lose precision above 2^53

The predicate `9007199254740993 = 9007199254740992.0` matched an object, while
the corresponding `!=` predicate matched none. These values are not equal: the
integer is one greater than the exactly represented floating-point value. Same-
type integer controls behaved correctly. The comparison converts the integer
to `f64`, rounding it to the float's value; `qv_value_eq` uses the same
conversion for cross-type `.equals()`. Compare the values without lossy
coercion, or document a deliberately approximate numeric comparison contract.

### OQL-31. Scalar method expressions in HAVING evaluate as NULL

`HAVING LOWER(@category) = "jdk"` returned no groups, while the direct
`HAVING @category = "jdk"` control returned the `jdk` group. The HAVING
expression evaluator maps `Expr::Method` to NULL instead of evaluating supported
operations such as `LOWER`; implement the same expression semantics as the row
evaluator or reject unsupported HAVING expressions explicitly.

### OQL-32. `INSTANCEOF` does not match implemented interfaces

With `--all`, `java.lang.Thread` had 29 instances, but
`FROM INSTANCEOF java.lang.Runnable` returned zero. The fixture also had 362
`java.util.HashMap` instances, while `FROM INSTANCEOF java.util.Map` returned
zero. Both classes implement the queried interface. `LiveResolver::is_instance_of`
walks only `super_id`, and `ClassInfo` does not retain implemented-interface
metadata. Include interface transitive closure in assignability checks; the OQL
guide itself uses `INSTANCEOF java.util.Map` as an example.

### OQL-33. `COUNT(DISTINCT ...)` treats positive and negative zero as different

`0.0 = -0.0` matched all 203,782 objects, but
`COUNT(DISTINCT CASE WHEN @objectId < 100000 THEN 0.0 ELSE -0.0 END)` returned
**2**. The aggregate's distinct accumulator keys values by debug-formatted
strings, which distinguishes `Float(0.0)` from `Float(-0.0)` even though numeric
comparison considers them equal. Likewise, `1 = 1.0` matched all rows, while
`COUNT(DISTINCT CASE WHEN @objectId < 100000 THEN 1 ELSE 1.0 END)` returned **2**.
The analogous GROUP BY expression also produced two groups for `0.0` and
`-0.0`. Use canonical numeric equality/hash semantics for distinct and group
keys.

### OQL-34. Array `WHERE ... INSTANCEOF` uses exact class-name matching

`SELECT COUNT(*) FROM java.lang.Object[] a WHERE a INSTANCEOF java.lang.Object`
returned **0**, although the same exact array-class source contains 2,418
objects. The `Cloneable` predicate also returned zero, and
`String[] WHERE ... INSTANCEOF Object[]` failed to match its 450 rows. This is
separate from the `FROM INSTANCEOF` array gap above: `array_eval_pred` uses the
ordinary class-name matcher instead of Java array assignability, so WHERE and
FROM class matching disagree. Apply the same array-aware assignability rules in
both paths.

### OQL-35. `NULLIF` uses variant equality instead of query equality

`NULLIF(1, 1.0)` returned the integer `1`, even though the query comparison
`1 = 1.0` is true. `NULLIF` is documented to return NULL when its operands are
equal, but the expression evaluators compare `QueryValue` variants directly
instead of using the numeric cross-type comparison semantics. Use the same
equality routine as predicates for `NULLIF`.

### OQL-36. Dotted method calls silently ignore unsupported argument counts

`SELECT s.getObjectAddress(1) FROM java.lang.String s LIMIT 1` succeeded and
returned exactly the same address as the zero-argument `s.getObjectAddress()`
call. `getObjectAddress` takes no arguments, but the dotted-call parser accepts
an arbitrary argument list and planning validates only the method name; dispatch
ignores `args` for this method. This can silently accept malformed calls (and
extra arguments to other methods) instead of reporting an arity error. Validate
method argument counts during planning, including allowed overloads and
variadic methods.

### OQL-37. Java `String.length()` returns NULL

`SELECT s.length() FROM java.lang.String s LIMIT 3` returned NULL for all three
rows, while `toString(s)` decoded those same rows as `org.renaissance`,
`org.renaissance.core`, and `org.renaissance`. The parser and method table
recognize `length()` as supported, but dispatch maps it to `Attr::Length`, whose
instance-row projection returns NULL; only the array-row path supplies an
actual length. Implement String length from its decoded value/backing storage,
or reject the String receiver explicitly instead of returning NULL.

### OQL-38. LIKE regex alternation is escaped when the pattern contains `_`

The guide says LIKE accepts SQL globs and regex patterns, but the pattern
`foo_bar|baz` is classified as a glob solely because it contains `_`. With
`--all`, `WHERE "baz" LIKE "foo_bar|baz"` returned **0**, while the equivalent
anchored regex `WHERE "baz" LIKE "^(?:foo_bar|baz)$"` returned **134,277**.
The glob conversion changes `_` to `.` and escapes `|`, destroying regex
alternation. The heuristic should not silently reinterpret valid regex syntax;
provide an unambiguous pattern mode or improve regex/glob detection.

### OQL-39. [Stale] Method-expression column names omitted the receiver

This finding no longer reproduces in the current worktree/binary:
`SELECT LOWER("ABC") ...` now labels its column `"ABC".lower()`, and
`SELECT SUBSTR("abcdef", 2, 3) ...` labels its column
`"abcdef".substr(2, 3)`. Current `expr_name` includes the receiver. Retain this
entry as audit history, not as an active bug.

### OQL-40. Window `PARTITION BY` silently ignores unselected keys

`ROW_NUMBER() OVER (PARTITION BY @objectId)` should restart at 1 for every
Thread, since `@objectId` is unique. Without projecting that key,
`SELECT ROW_NUMBER() OVER (PARTITION BY @objectId) FROM java.lang.Thread LIMIT 3`
returned **1, 2, 3**. Adding `@objectId` to the SELECT list made the same query
return **1, 1, 1**. `apply_window_functions` resolves partition keys only by
searching projected columns and silently drops keys it cannot resolve. Carry
window keys independently of output columns, or reject unselected keys with an
actionable error.

### OQL-41. `LAG`/`LEAD` return NULL for non-attribute arguments

`LAG(@objectId) OVER (ORDER BY @objectId)` returned the preceding Thread ID as
expected, but `LAG(@objectId + 1) OVER (ORDER BY @objectId)` returned NULL for
every row, including rows with a predecessor. The parser accepts an expression
argument, but `apply_window_functions` resolves the argument only when it is a
bare `Expr::Attr`; all other forms produce no source column and silently
default to NULL. Evaluate the argument expression per source row or reject
unsupported argument forms during planning.

### OQL-42. `LAG`/`LEAD` silently discard non-literal defaults

The parser accepts an expression as the third `LAG`/`LEAD` argument, but the
window evaluator uses the default only when it is an `Expr::Lit`. At the first
Thread, `LAG(@objectId, 1, 123)` returned **123**, while
`LAG(@objectId, 1, @objectId)` returned NULL instead of that row's ID (**65,980**).
Likewise, a one-row `LEAD(@objectId, 1, @objectId)` query returned NULL at the
partition boundary. Evaluate default expressions in the current row context or
reject non-literal defaults during planning.

### OQL-43. Oversized `LAG`/`LEAD` offsets wrap to smaller offsets

The parser casts the parsed integer offset to `u32` without range checking.
Consequently, `LAG(@objectId, 4294967296, 123)` returned each current ID, as if
the offset were **0**, instead of the default **123** because no predecessor
exists at that distance. An offset of `4294967297` behaved like **1** and
returned the preceding row. Reject offsets above `u32::MAX` (or use a wider,
checked representation) rather than silently changing the requested offset.

### OQL-44. `LIMIT` truncates input before window functions run

`ROW_NUMBER() OVER (ORDER BY @objectId DESC)` should rank the full Thread
partition before a top-level `LIMIT` trims output. The limited query returned
**3, 2, 1** for the first three scan-order Thread IDs, while the same query
without `LIMIT` gave those rows **29, 28, 27**. The scan-time early-stop checks
for a top-level `ORDER BY` but not for window functions; `apply_window_functions`
then ranks only the collected prefix. Defer LIMIT until after window evaluation.

### OQL-45. Array-row `CONCAT` ignores arguments after the second operand

`CONCAT("a", "b", "c")` returned `"abc"` from `java.lang.Object` rows but
`"ab"` from `java.lang.Object[]` rows. The array expression evaluator's
`concat` branch reads only `args.first()`, while the instance evaluator folds
all remaining arguments. Honor every variadic argument in both row paths.

### OQL-46. `HAVING` mishandles an unselected `GROUP BY` key

`SELECT COUNT(*) FROM INSTANCEOF java.lang.Object GROUP BY @category
HAVING @category = "jdk"` returned no groups, while adding `@category` to the
SELECT list returned the matching `jdk` group with **203,052** objects. The
HAVING evaluator resolves an unselected attribute by treating its GROUP BY
position as a position in the output row; that row contains only selected
columns, so it reads the aggregate value instead of the hidden group key.
Evaluate HAVING against the retained group-key vector, independent of SELECT
projection.

### OQL-47. `REPLACE` is not evaluated in `HAVING`

`HAVING REPLACE(@category, "jdk", "jdk") = "jdk"` returned no groups, while
the equivalent direct predicate `HAVING @category = "jdk"` returned the
`jdk` group. `eval_having_string_method` implements several string operations
but has no `replace` arm, so this supported expression becomes NULL and filters
out the matching group. Implement the same `REPLACE` semantics used by ordinary
row expressions, or reject it explicitly during planning.

### OQL-48. HAVING aggregate lookup ignores the selected aggregate's FILTER

Source inspection shows `eval_having_expr` resolves a HAVING aggregate by
matching only its function and argument against `query.select`; it ignores the
selected aggregate's `FILTER` clause. Thus, if the first matching projection is
`COUNT(*) FILTER (WHERE ...)`, a separate `HAVING COUNT(*) > n` can read that
filtered count instead of the unfiltered group count. A discriminating runtime
check is still pending: the local debug binary appears not to honor ordinary
scan-time aggregate FILTERs, so its result cannot validate this current-source
path. Resolve HAVING aggregates independently or include filter identity and
semantics in the lookup.

### OQL-49. HAVING aggregates must also appear in SELECT

`SELECT @category FROM INSTANCEOF java.lang.Object GROUP BY @category
HAVING COUNT(*) > 1000` returned no groups, while adding `COUNT(*)` to the
projection returned the `jdk` group with **203,052** objects. The HAVING
evaluator looks up aggregate values only among `query.select` items and returns
NULL when no matching aggregate is projected. SQL-style HAVING must be able to
evaluate an aggregate independently of the SELECT list; compute the required
aggregate for HAVING or reject the construct explicitly rather than silently
filtering every group.

### OQL-50. `MEDIAN` returns the lower middle value for even-sized samples

`MEDIAN` is currently accumulated as `PERCENTILE(..., 50)` and finalized with
the nearest-rank rule. For an even-sized sample, that returns the lower middle
value rather than the conventional arithmetic mean of the two middle values.
Reproducer on the fixture (run with `--all`):

```sql
SELECT MEDIAN(CASE WHEN @objectId = 2 THEN 1 ELSE 2 END)
FROM INSTANCEOF java.lang.Object WHERE @objectId < 4
```

The two matched values are 1 and 2. Expected conventional median: **1.5**;
actual: **1.0**. The odd-sized control with values 1, 2, 3 returns **2.0**;
`PERCENTILE(..., 50)` on the two-value sample also returns **1.0**, confirming
that `MEDIAN` shares the nearest-rank percentile implementation. Implement the
even-sample median separately, or explicitly document that `MEDIAN` means
nearest-rank 50th percentile if that is the intended contract.

### OQL-51. Grouped `SELECT *` is accepted but returns NULL

Reproducer on the fixture (run with `--all`):

```sql
SELECT *, COUNT(*) FROM INSTANCEOF java.lang.Object GROUP BY @category
```

Actual: the query succeeds with two groups, but the `*` column is NULL in both
rows; the explicit-key control
`SELECT @category, COUNT(*) FROM INSTANCEOF java.lang.Object GROUP BY @category`
returns `jdk` and `other` as expected. The planner's GROUP BY validation skips
`SelectItem::Star` because it has no expression to compare to a group key, and
the late grouped-row builder maps unmatched non-aggregate items to NULL. Since
`*` cannot represent a grouped row unless all of its expanded columns are valid
group keys, reject it in grouped SELECTs (or define and implement its grouped
semantics) instead of silently returning NULL.

### OQL-53. `MIN`/`MAX` with NaN depend on input order

Floating division by zero produces NaN, and `total_cmp_query_value` defines NaN
as sorting after finite numbers. But the scan-time `MIN`/`MAX` accumulator uses
`<=`/`>=`; comparisons with NaN are false, so it replaces the saved extremum
with whichever value arrives last. Reproducer pair on the fixture (run with
`--all`):

```sql
SELECT MIN(CASE WHEN @objectId = 2 THEN 0.0 / 0.0 ELSE 5.0 END),
       MAX(CASE WHEN @objectId = 2 THEN 0.0 / 0.0 ELSE 5.0 END)
FROM INSTANCEOF java.lang.Object WHERE @objectId < 4
```

Actual: `MIN = 5.0`, `MAX = 5.0`. Reverse which row yields NaN:

```sql
SELECT MIN(CASE WHEN @objectId = 2 THEN 5.0 ELSE 0.0 / 0.0 END),
       MAX(CASE WHEN @objectId = 2 THEN 5.0 ELSE 0.0 / 0.0 END)
FROM INSTANCEOF java.lang.Object WHERE @objectId < 4
```

Actual: both JSON values are `null` (the underlying extrema are NaN). Under the
existing total-order policy, both input orders should give `MIN = 5.0` and
`MAX = NaN`; use the same explicit NaN ordering in the aggregate accumulator.

### OQL-52. Late-phase grouped aggregates ignore their `FILTER`

Reproducer on the fixture (run with `--all`):

```sql
SELECT @category, COUNT(*) FILTER (WHERE @objectId = 2)
FROM INSTANCEOF java.lang.Object GROUP BY @category
```

Actual: returns the unfiltered counts, `jdk = 203,052` and `other = 730`. The
control `SELECT @category, COUNT(*) FROM INSTANCEOF java.lang.Object
WHERE @objectId = 2 GROUP BY @category` returns only `jdk = 1`, so the filtered
aggregate should return `jdk = 1` and `other = 0`. In
`stage_runner.rs::join_retained_group_by`, the aggregate loop matches
`SelectItem::Aggregate { func, arg, .. }` and folds every row without evaluating
the `filter` predicate. This is the late grouped path and differs from OQL-24's
retained-size aggregate path; apply each aggregate's filter before folding.

### OQL-54. Late grouped `MEDIAN`/`PERCENTILE` silently return zero

Reproducer on the fixture (run with `--all`):

```sql
SELECT @category, MEDIAN(@usedHeapSize)
FROM INSTANCEOF java.lang.Object GROUP BY @category
```

Actual: both groups return **0**. The control
`SELECT @category, MIN(@usedHeapSize), MAX(@usedHeapSize) FROM INSTANCEOF
java.lang.Object GROUP BY @category` shows `jdk` sizes range from **16** to
**144**, and `other` from **16** to **128**; therefore a median of zero is
outside either group's data range. `PERCENTILE(@usedHeapSize, 50)` in the same
grouped query also returns zero. In `join_retained_group_by`, the accumulators
start at `Int(0)` and `AggFunc::Median`/`Percentile` are a no-op, but the
planner accepts these aggregates on the late GROUP BY path. Implement them
there or reject the combination during planning.

### OQL-55. Server returns pre-OFFSET rows for inline `VALUES` queries

Reproducer: POST this query to the OQL server's `POST /` endpoint:

```sql
SELECT x FROM (VALUES (3), (1), (2)) AS t(x)
ORDER BY x LIMIT 1 OFFSET 1
```

Expected: one row, `x = 2`. Actual: two rows, `x = 1` and `x = 2`. The same
query through the CLI returns only `x = 2`. `plan.rs::scan_limit` correctly
sets the scan cap to `LIMIT + OFFSET` (2 here), but the VALUES-only server path
returns `execute_values_query` directly, which truncates to that cap without
draining the offset. The CLI's later `collapse_union_results` performs the
OFFSET and final LIMIT; the server's `run_single_dump` VALUES fast path bypasses
that collapse step. Apply OFFSET and the post-offset LIMIT in the VALUES fast
path, or route its results through the common collapse logic.

### OQL-56. Inline `VALUES` projections silently turn expressions into NULL

Reproducer on the fixture:

```sql
SELECT x + 1 FROM (VALUES (3), (1)) AS t(x)
```

Expected rows: **4**, **2**. Actual: two NULLs. The direct-column control
`SELECT x FROM (VALUES (3), (1)) AS t(x)` returns **3**, **1**, and the ordinary
heap-row control `SELECT @objectId + 1 FROM java.lang.Object LIMIT 2` evaluates
arithmetic successfully. In `execute_values_query`, the projection handles
field attributes and literal expressions only; every other expression falls
through to NULL. Evaluate supported expressions with the VALUES row as context,
or reject unsupported projections instead of silently returning NULL.

### OQL-57. Inline `VALUES` queries ignore `WHERE`

Reproducer on the fixture:

```sql
SELECT x FROM (VALUES (3), (1), (2)) AS t(x) WHERE x < 0
```

Expected: zero rows. Actual: all three rows (`3`, `1`, `2`). A selective
control, `SELECT x FROM (VALUES (3), (1), (2)) AS t(x) WHERE x > 1 ORDER BY x`,
also returns all three rows instead of only `2` and `3`. `execute_values_query`
materializes the VALUES rows and proceeds to JOIN, ORDER BY, and LIMIT without
evaluating `q.where`; the accepted predicate is silently ignored. Evaluate the
WHERE expression against each VALUES row or reject WHERE on this source.

### OQL-58. Array rows return NULL for supported object-identity methods

Reproducer on the fixture (run with `--all`):

```sql
SELECT a.getObjectAddress(), @objectAddress, a.getObjectId(), @objectId
FROM INSTANCEOF byte[] a LIMIT 1
```

Expected: each method value matches its corresponding attribute. Actual: the
method columns are NULL, while `@objectAddress` is **2148538552** and
`@objectId` is **178**. The control query on `java.lang.Thread` returns matching
method/attribute pairs (**2153790184** and **65980**). Planning accepts these
zero-argument method names, and ordinary instance evaluation dispatches them to
the corresponding attributes, but `eval_expr_array` handles only array
`length`/`size` and string methods; these identity methods fall through to NULL.
Route them to the array attribute projection, or reject them for array rows.

## Checks that passed in this run

The tested scalar forms (field/arithmetic projections, `@superclass`,
`@category`, `@collectionSize`, `@length`, `LIKE` glob and regex,
`EXISTS`/`NOT EXISTS`, `INTERSECT`/`EXCEPT`, `BETWEEN`, `NOT IN`, and division by
zero to `NULL`) executed without errors. `MEDIAN`, `PERCENTILE`, `DISTINCT`,
`COALESCE`, `NULLIF`, and the documented dominator/path queries also ran.
Array subscripting and `jsonify` without an `offsets.bin` index produced the
documented unavailable/null result and explanatory note. These checks are
smoke tests, not comprehensive correctness proof.

## Validation required before fixes are considered complete

- Build and run OQL regression tests on the ThinkStation, following the repo's
  `CLAUDE.md` constraint; sync with `rsync` first.
- Add failing regression tests before production changes for reachable
  aggregates/grouping, limit-after-reachability, multiple and non-address-LHS
  `IN` predicates, short-circuit `IN`/`EXISTS` alignment, address-valued and
  nested subqueries, unprojected `ORDER BY`, Java String `length()`, LIKE
  regex/glob classification, method-expression column names, window partition
  keys/arguments/defaults and offset bounds/LIMIT ordering, array `INSTANCEOF`,
  and `FROM ENTRIES` planning/execution paths, including variadic string
  functions on array rows and HAVING with unselected group keys, unprojected
  aggregates, string methods, and FILTER-bound aggregates.
  Include even-sized `MEDIAN` samples and compare against an odd-sized control.
  Also verify grouped `SELECT *` is rejected or has explicitly defined output.
  Test `FILTER` on grouped late-phase aggregates against equivalent WHERE
  controls. Test `MIN`/`MAX` with finite values and NaN in both input orders.
  Also test `MEDIAN`/`PERCENTILE` with late GROUP BY keys or require a planning
  error for unsupported combinations.
  Exercise `ORDER BY`/`LIMIT`/`OFFSET` on inline `VALUES` through both CLI and
  HTTP paths; assert that HTTP does not return the pre-offset scan prefix.
  Also exercise computed projections over inline `VALUES` and compare against
  equivalent direct-column and ordinary-row expressions.
  Verify inline `VALUES` WHERE predicates filter rows, including an
  always-false predicate that must return an empty result.
  Compare supported object-identity methods with `@objectAddress` and
  `@objectId` on array rows and ordinary instance rows.
- Test report-mode `FROM ENTRIES` with the actual graph/capability contract and
  make unsupported states fail with the right actionable message.
- Update README/OQL examples, restrictions, and HTTP response schema only after
  behavior is confirmed against a rebuilt binary.
