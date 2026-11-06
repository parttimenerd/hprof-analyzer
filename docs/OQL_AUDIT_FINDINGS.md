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

### OQL-18. `CAST(NULL AS STRING)` turns NULL into the text `"null"`

Source inspection of `cast_query_value` shows that `CAST(NULL AS INT)`, `FLOAT`,
and `BOOL` preserve NULL, but the `STRING` arm converts it to
`QueryValue::Str("null")`. This breaks normal NULL propagation and makes, for
example, `COALESCE(CAST(NULL AS STRING), "fallback")` return `"null"` instead
of the fallback. Preserve `QueryValue::Null` for string casts. This is a
source-level finding; runtime confirmation is pending because the local debug
binary does not recognize the CAST syntax present in the current source, and
the repository requires builds/tests to run on the ThinkStation.

### OQL-19. CAST column names omit the cast expression

`expr_name` formats `Expr::Cast` as `CAST(AS {target:?})` and ignores the inner
expression. Thus a projection like `CAST(@usedHeapSize AS FLOAT)` gets an
invalid/incomplete default column name such as `CAST(AS Float)`, affecting JSON
metadata and any later lookup by column name. Include the operand in the
rendered name; runtime confirmation is likewise pending a current build.

### OQL-20. Invalid string-to-boolean casts silently become `false`

The `BOOL` cast arm maps only case-insensitive `"true"` and `"1"` to true;
every other string becomes false. Thus an invalid value such as
`CAST("not-a-bool" AS BOOL)` is indistinguishable from an intentional false
value, unlike invalid string-to-INT/FLOAT casts, which return NULL. Validate the
accepted false spellings (for example `"false"` and `"0"`) and return NULL or
an actionable cast error for other strings. This is source-level only pending a
current build that can parse CAST.

### OQL-21. `INSTANCEOF java.lang.Object` excludes array objects

With `--all`, `FROM ".*"` matched 239,355 objects and a regex matching array
class names matched 35,573. `FROM INSTANCEOF java.lang.Object`
matched only 203,782 — exactly the non-array remainder. `FROM INSTANCEOF
java.lang.Cloneable` and `FROM INSTANCEOF java.io.Serializable` each returned
zero as well. Java arrays are instances of `Object` and implement both marker
interfaces, so they should be included. The array visitor currently compares
the array class name directly to the requested class instead of applying Java
array assignability / the `INSTANCEOF` rule. Handle arrays as `Object` and as
instances of `Cloneable` and `Serializable` when matching `INSTANCEOF`.
Array covariance is also missing: exact `java.lang.Object[]` matched 2,418
objects and exact `java.lang.String[]` matched 450, while
`INSTANCEOF java.lang.Object[]` still returned only 2,418. Since `String[]` is
assignable to `Object[]`, the latter query should include at least those 450
String arrays as well. In the current source, `visit_array` uses the ordinary
class-name matcher rather than the `INSTANCEOF` hierarchy/assignability path.

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
  aggregates/grouping, limit-after-reachability, HTTP row-limit override,
  `LIMIT 0` contract, multiple and non-address-LHS `IN` predicates,
  short-circuit `IN`/`EXISTS` alignment, address-valued and nested subqueries,
  unprojected `ORDER BY`, CAST NULL propagation/default column naming/invalid
  boolean strings, array `INSTANCEOF`, and `FROM ENTRIES` planning/execution
  paths.
- Test report-mode `FROM ENTRIES` with the actual graph/capability contract and
  make unsupported states fail with the right actionable message.
- Update README/OQL examples, restrictions, and HTTP response schema only after
  behavior is confirmed against a rebuilt binary.
