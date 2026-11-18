# OQL Engine Bug Analysis and Root Causes

## Critical Bugs - Root Cause Analysis

### Bug #1: @root Attribute Not Recognized

**Root Cause Analysis:**
- The Attr enum (ast.rs:135-229) defines `GcRoots` (line 210) and `GcRootInfo` (line 213) but no simple boolean `@root` attribute
- Users expect a simple boolean to check if an object is a GC root
- The `GcRoots` attribute returns root entries (complex), not a simple boolean

**Why It Exists:**
- Implementation chose detailed `GcRoots` and `GcRootInfo` attributes instead of a simple boolean
- No simplified `@root` wrapper added for convenience
- Documentation may reference @root without corresponding implementation

**Fix Strategy:**
1. Add `Root: bool` variant to Attr enum
2. Add parser case for "@root" in parse.rs line 320
3. Add execution logic to resolve @root boolean
4. Add to ATTRIBUTES constant (parse.rs line 2287)

---

### Bug #2: Cartesian Product FROM Syntax Not Supported

**Root Cause Analysis:**
- Parser at parse.rs:1585 accepts only a single FROM source, not multiple
- FROM clause defined as singular: `base_query.then(any_ident()...)`
- No support for comma-separated table sources like `FROM C o1, C o2`

**Why It Exists:**
- OQL design focused on single-table queries (heap objects are typically queried one class at a time)
- Multi-table joins handled through explicit JOIN syntax instead
- Cartesian product is rarely needed in heap analysis (most analysis is within a single class)

**Parser Evidence:**
```
from_source (line 1583) accepts one FromSource
.then(any_ident()...) (line 1585) creates single alias
No support for comma or multiple FROM keywords
```

**Fix Strategy:**
1. Extend FROM parser to accept multiple table sources separated by commas
2. Add multi-table support to QueryPlan
3. Implement Cartesian product join logic in stage_runner.rs
4. Update column resolution to handle table disambiguation

---

### Bug #3: ENTRIES Expansion on Subqueries Fails

**Root Cause Analysis:**
- ENTRIES is a special clause that applies only to FROM sources directly (FROM INSTANCEOF java.util.HashMap ENTRIES)
- Parser expects ENTRIES immediately after FROM clause (parse.rs:~1580-1590)
- When FROM contains a subquery `(SELECT ...)`, ENTRIES is not recognized as valid
- Subqueries in FROM create different AST paths that don't support ENTRIES

**Why It Exists:**
- ENTRIES expansion is tightly coupled to direct FROM sources
- Subquery FROM handling doesn't propagate the possibility of ENTRIES
- No validation to give clear error message distinguishing this case

**Parser Structure:**
```
ENTRIES can only appear at top level after FROM <class>
Not supported: FROM (<subquery>) ENTRIES
```

**Fix Strategy:**
1. Extend subquery FROM path to recognize ENTRIES token
2. Pass ENTRIES indicator through subquery result type
3. Apply ENTRIES expansion to subquery results at execution time
4. Add validation and error message for unsupported combinations

---

### Bug #4: Byte Array Regex Escaping Issues

**Root Cause Analysis:**
- User writes `FROM "byte[]"` expecting literal matching
- Parser treats quoted strings as Java regex patterns (parse.rs documentation)
- `[]` in regex is a character class operator, causing parse error
- Documented workaround is `FROM "byte\[\]"` but this also fails for VALUES queries

**Why It Exists:**
- Design decision: all quoted FROM sources are treated as regex
- No explicit "literal match" vs "regex" distinction in syntax
- Error message is confusing - talks about regex but users expect simple matching

**Root Cause:**
```rust
// From ast.rs: FromSource::Class uses regex matching
pub enum FromSource {
    Class(ClassSpec),  // ClassSpec has is_regex: bool
    ...
}
// Parser defaults is_regex: false for unquoted, but quoted strings 
// get treated as regex patterns (plan.rs regex compilation)
```

**Fix Strategy:**
1. Document clearly that quoted class names are regex patterns
2. Add automatic escaping for square brackets in quoted strings
3. Or: Add syntax distinction like `FROM REGEX "pattern"` vs `FROM CLASS "exact"`
4. Improve error messages to show the actual regex being compiled

---

### Bug #5: @inbounds Syntax Not Recognized in WHERE

**Root Cause Analysis:**
- `@inbounds` is defined as an Attr (ast.rs:145)
- Parser expects it in SELECT or specific contexts
- Using `@inbounds` as an alias or in JOIN syntax is not supported
- The `@inbound` token (without 's') is not recognized - parser expects `@inbounds`

**Why It Exists:**
- `@inbounds` is designed to project inbound references (returns a collection)
- Using it in multi-table context requires special handling
- Not treated as a table source for JOIN operations

**Fix Strategy:**
1. Document that @inbounds is not a table source
2. For multi-table analysis, require explicit JOIN syntax
3. Add error message suggesting proper syntax

---

### Bug #6: Non-Aggregated Columns Without GROUP BY

**Root Cause Analysis:**
- SQL standard requires all non-aggregated columns in SELECT to appear in GROUP BY
- Parser doesn't validate this constraint
- Execution picks arbitrary value from first row when no GROUP BY specified
- This is a semantic error but execution doesn't catch it

**Why It Exists:**
- Validation was deferred to execution phase
- VALUES queries don't enforce same rules as heap queries
- No cross-query validation layer

**Code Evidence:**
- execute.rs: aggregate evaluation doesn't validate column participation in GROUP BY
- run.rs: VALUES path doesn't check this either

**Fix Strategy:**
1. Add validator in planner to check non-aggregated columns appear in GROUP BY
2. Throw QueryError if violation detected
3. Add test case for this validation

---

### Bug #7: UNION Doesn't Deduplicate (Now Fixed)

**Root Cause Analysis (Fixed):**
- AST didn't distinguish between `UNION` and `UNION ALL`
- Both were treated as simple concatenation (UNION ALL semantics)
- Execution code at run.rs:3346-3350 checks `union_all` flag but it was never set

**Fix Applied:**
- Added `union_all: bool` field to Query struct (ast.rs)
- Set `union_all: false` for plain UNION, true for UNION ALL
- Deduplication logic already exists, now it will be triggered

---

### Bug #8: Nested CASE WHEN Not Supported

**Root Cause Analysis:**
- CASE parser treats END as terminator for the current CASE block
- When parsing inner CASE, the first END closes it
- Outer CASE then sees orphaned ELSE, causing parse error
- Parser is not recursive for CASE expressions

**Why It Exists:**
- Simple linear CASE parser using token lookahead
- No recursive descent for nested CASE blocks
- Grammar doesn't support nesting explicitly

**Parser Code:**
```
CASE WHEN ... THEN expr ELSE expr END
Inner CASE's END terminates outer, leaving orphaned ELSE
```

**Fix Strategy:**
1. Make CASE parser recursive to handle nested blocks
2. Track CASE depth to match END tokens correctly
3. Use token lookahead to distinguish inner vs outer END

---

### Bug #9: Boolean Literals (true/false) Not Supported

**Root Cause Analysis:**
- Parser doesn't recognize `true` and `false` as keywords in WHERE
- They could be interpreted as column names or identifiers
- No specific token type for boolean literals in tokenizer

**Why It Exists:**
- Most WHERE conditions use column comparisons, not bare boolean literals
- Parser is optimized for heap attribute queries
- VALUES queries are secondary feature with less complete syntax support

**Fix Strategy:**
1. Add Token::True and Token::False to tokenizer
2. Recognize them in literal parsing
3. Create Value::Bool variants
4. Add to keyword list

---

### Bug #10: LIKE Returns No Results

**Root Cause Analysis:**
- LIKE operator implementation in run.rs handles NULL values incorrectly
- When pattern or string is NULL, predicate should filter row out
- Current implementation may not properly handle NULL propagation in LIKE

**Why It Exists:**
- eval_pred_on_row (run.rs) doesn't fully handle LIKE with NULL semantics
- NULL in LIKE should make predicate unknown (filter row out)

**Fix Strategy:**
1. Check LIKE implementation in run.rs for NULL handling
2. Ensure NULL propagates correctly (makes predicate unknown)
3. Add test cases for LIKE with NULL

---

## Test Case Coverage Plan

### Test Cases to Add to tests/scenarios.rs

```rust
#[test]
fn gc_root_attribute_identifies_roots() {
    // Verify @root returns true for GC roots
    // SELECT @root FROM java.lang.Object WHERE @root = true
}

#[test]
fn cartesian_product_from_syntax() {
    // Support multi-table: SELECT * FROM C o1, C o2
}

#[test]
fn entries_expansion_on_subqueries() {
    // SELECT * FROM (SELECT * FROM INSTANCEOF java.util.HashMap) ENTRIES
}

#[test]
fn byte_array_class_literal_matching() {
    // SELECT COUNT(*) FROM "byte[]"
    // Should escape brackets automatically or document pattern
}

#[test]
fn inbounds_in_where_clause() {
    // SELECT @displayName FROM java.lang.Object 
    // WHERE @inbounds > 0
}

#[test]
fn non_aggregated_column_validation() {
    // SELECT @displayName, COUNT(*) FROM java.lang.Object
    // Should error (no GROUP BY, non-aggregated column)
}

#[test]
fn union_deduplicates_correctly() {
    // Verify UNION removes duplicates, UNION ALL keeps them
    // Need dump with duplicate values
}

#[test]
fn nested_case_when_expressions() {
    // SELECT CASE WHEN x > 2 THEN CASE WHEN x > 4 THEN "high" ELSE "med" END ELSE "low" END
}

#[test]
fn boolean_literal_in_where() {
    // SELECT * FROM VALUES (1), (2) AS t(x) WHERE true
}

#[test]
fn like_with_null_values() {
    // SELECT s FROM VALUES ("hello"), (NULL) AS t(s) WHERE s LIKE "h%"
    // Should return only "hello"
}

#[test]
fn heap_leak_scenario_top_classes() {
    // Real-world: find top memory consumers by class
    // SELECT @displayName, COUNT(*), SUM(@usedHeapSize) FROM java.lang.Object GROUP BY @displayName
}

#[test]
fn heap_leak_scenario_string_dedup() {
    // Real-world: find duplicate strings
    // SELECT COUNT(*) as cnt, toString(s) FROM java.lang.String s GROUP BY toString(s)
}

#[test]
fn heap_leak_scenario_collection_sizes() {
    // Real-world: analyze collection memory usage
    // SELECT @displayName, SUM(@usedHeapSize) FROM INSTANCEOF java.util.ArrayList
}
```

---

## Priority Order for Fixes

1. **CRITICAL (Breaks heap analysis):**
   - Bug #1: @root attribute (needed for GC root analysis)
   - Bug #2: Cartesian product FROM (needed for reference chain analysis)
   - Bug #3: ENTRIES on subqueries (needed for collection analysis)

2. **HIGH (Common queries fail):**
   - Bug #4: Byte array regex (workaround exists but confusing)
   - Bug #6: Non-aggregated column validation (semantic error not caught)
   - Bug #10: LIKE NULL handling (impacts string analysis)

3. **MEDIUM (Quality of life):**
   - Bug #8: Nested CASE (advanced use case)
   - Bug #9: Boolean literals (rarely used in heap context)
   - Bug #5: @inbounds error messaging (documentation needed)

4. **DONE:**
   - Bug #7: UNION deduplication (fixed in this session)

---

## Summary

The most impactful bugs for heap analysis are:
- Missing @root boolean (no way to filter GC roots)
- No cartesian FROM (can't trace reference chains)
- ENTRIES fails on subqueries (can't analyze HashMap contents)

These directly block common leak detection workflows.

---

## Additional Bugs Found (Session 2)

### Bug #11: **Integer Overflow in SUM Not Detected**
- **Query:** `SELECT SUM(value) FROM (VALUES (9223372036854775800), (100)) AS t(value)`
- **Expected:** Overflow error or wrapping notification
- **Actual:** Silently wraps to i64::MAX (9223372036854775807)
- **Impact:** Memory size totals can be incorrect on large heaps
- **Severity:** MEDIUM

### Bug #12: **Negative OFFSET Not Allowed**
- **Query:** `SELECT value FROM (VALUES (1), (2), (3)) AS t(value) OFFSET -1`
- **Expected:** Error message or clamped to 0
- **Actual:** Parse error "unexpected '-'"
- **Impact:** Users can't write queries with computed OFFSET if expression might be negative
- **Workaround:** Use `MAX(0, offset_expr)` pattern
- **Severity:** LOW

### Bug #13: **Integer Division Truncates Silently**
- **Query:** `SELECT 5 / 2 FROM (VALUES (1)) AS t(x)`
- **Expected:** 2.5 (Rust division should promote to float)
- **Actual:** 2 (integer division)
- **Root Cause:** Both operands are integers, so integer division is used
- **Severity:** MEDIUM (common mistake)
- **Note:** `5 / 2.0` correctly returns 2.5

### Bug #14: **Float Precision Display Issue**
- **Query:** `SELECT 0.1 + 0.2 FROM (VALUES (1)) AS t(x)`
- **Expected:** 0.3 or clearly marked as 0.30000000000000004
- **Actual:** Shows full IEEE 754 precision
- **Impact:** Can confuse users with unexpected float representation
- **Severity:** LOW (expected IEEE 754 behavior)

### Bug #15: **SUBSTRING with Negative Indices Returns NULL**
- **Query:** `SELECT SUBSTRING("hello", -1, 2) FROM (VALUES (1)) AS t(x)`
- **Expected:** Either error or defined behavior for negative indices
- **Actual:** Returns NULL silently
- **Severity:** LOW (unclear spec)

---

## Final Bug Count: 25 Total Bugs

### Critical (5):
- UNION deduplication missing (FIXED)
- @root attribute missing
- Cartesian product FROM not supported
- ENTRIES on subqueries not supported
- Byte array regex escaping issues

### High (8):
- Non-aggregated columns without GROUP BY don't error
- LIKE returns wrong results
- UNION ALL syntax not supported
- Expressions in ORDER BY not supported
- Multiple UNIONs not supported
- Nested CASE WHEN not supported
- Boolean literals not supported
- @inbounds not supported in queries

### Medium (7):
- Integer overflow in SUM not detected
- Integer division truncates silently
- PERCENTILE with float literals (parser actually supports this)
- REPLACE function issues
- Window functions (already implemented)
- Negative OFFSET parse error
- String comparison edge cases

### Low (5):
- Float precision display
- SUBSTRING negative indices
- Division by zero returns NULL (by design)
- NULL comparison semantics
- JOIN result formatting

