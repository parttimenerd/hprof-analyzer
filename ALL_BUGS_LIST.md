# Complete OQL Engine Bug List - All 25 Bugs

## Critical Bugs (5) - Block Core Heap Analysis

### 1. UNION Deduplication Missing
- **Severity:** CRITICAL
- **Status:** FIXED (code changes applied)
- **Query:** `SELECT 1 UNION SELECT 1`
- **Expected:** 1 row (duplicates removed)
- **Actual:** 2 rows (acts like UNION ALL)
- **Root Cause:** No distinction between UNION and UNION ALL in AST
- **Impact:** Users can't remove duplicate results from set operations

### 2. @root Boolean Attribute Missing
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT COUNT(*) FROM java.lang.Object WHERE @root = true`
- **Error:** "unknown @attribute: @root"
- **Root Cause:** Attr enum has GcRoots/GcRootInfo but no simple Root boolean
- **Impact:** Cannot identify objects held by GC roots - fundamental leak detection technique blocked
- **File to Fix:** `src/query/ast.rs` (add Root variant to Attr enum)

### 3. Cartesian Product FROM Not Supported
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT * FROM java.lang.Object o1, java.lang.Object o2`
- **Error:** "unexpected ','"
- **Root Cause:** Parser only accepts single FROM source, not comma-separated tables
- **Impact:** Cannot trace reference chains between objects - blocks leak chain analysis
- **Files to Fix:** `src/query/parse.rs`, `src/query/plan.rs`, `src/query/stage_runner.rs`

### 4. ENTRIES Expansion Fails on Subqueries
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT * FROM (SELECT * FROM INSTANCEOF java.util.HashMap) ENTRIES`
- **Error:** "unexpected 'ENTRIES'"
- **Root Cause:** ENTRIES only works on direct FROM sources, not subquery results
- **Impact:** Cannot expand collection internals in subqueries - blocks HashMap leak analysis
- **Files to Fix:** `src/query/parse.rs`, `src/query/execute.rs`

### 5. Byte Array Regex Escaping Issues
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT COUNT(*) FROM "byte[]"`
- **Error:** "unclosed character class ([] treated as regex)"
- **Root Cause:** Quoted class names treated as Java regex patterns; [] is character class operator
- **Impact:** Cannot query byte arrays directly - common memory leak source
- **Workaround:** Use `"byte\[\]"` (but inconsistent with user expectations)
- **Files to Fix:** `src/query/parse.rs` (add literal match vs regex distinction)

---

## High Severity Bugs (8) - Common Queries Fail

### 6. Non-Aggregated Columns Without GROUP BY Don't Error
- **Severity:** HIGH
- **Status:** NOT FIXED (test passes but wrong behavior)
- **Query:** `SELECT @displayName, COUNT(*) FROM java.lang.Object`
- **Expected:** Error (SQL semantic violation)
- **Actual:** Returns arbitrary displayName with total count
- **Root Cause:** No validator checking non-aggregated columns appear in GROUP BY
- **Impact:** Memory histograms can show incorrect object attribution
- **Files to Fix:** `src/query/plan.rs` (add validator)

### 7. LIKE Returns Wrong Results
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT s FROM (VALUES ("hello"), (NULL)) AS t(s) WHERE s LIKE "h%"`
- **Expected:** 1 row ("hello")
- **Actual:** 267339 rows (query-specific, wrong filtering)
- **Root Cause:** LIKE NULL handling broken in eval_pred_on_row
- **Impact:** String pattern matching unreliable
- **Files to Fix:** `src/query/run.rs` (eval_pred_on_row LIKE implementation)

### 8. UNION ALL Syntax Not Supported
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT 1 UNION ALL SELECT 2`
- **Error:** "unexpected 'ALL' — did you mean `AND`?"
- **Root Cause:** Parser doesn't recognize ALL keyword after UNION
- **Impact:** Users can't explicitly request duplicate preservation
- **Files to Fix:** `src/query/parse.rs` (add ALL token recognition)

### 9. Expressions in ORDER BY Not Supported
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT value FROM (VALUES (1), (2)) AS t(value) ORDER BY value * 2`
- **Error:** "unexpected '*'"
- **Root Cause:** ORDER BY parser only accepts attr, not expr
- **Impact:** Users must use column aliases for computed sorts
- **Files to Fix:** `src/query/parse.rs` (change ORDER BY from attr to expr)

### 10. Multiple UNION Operations Not Supported
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT 1 UNION SELECT 2 UNION SELECT 3`
- **Error:** "unexpected 'UNION'" at second UNION
- **Root Cause:** Parser collects branches but doesn't handle chaining
- **Impact:** Users must nest UNIONs in subqueries
- **Workaround:** `(SELECT 1 UNION SELECT 2) UNION SELECT 3`
- **Files to Fix:** `src/query/parse.rs` (loop for multiple UNIONs)

### 11. Nested CASE WHEN Not Supported
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT CASE WHEN x > 2 THEN CASE WHEN x > 4 THEN "high" ELSE "mid" END ELSE "low" END`
- **Error:** "unexpected 'ELSE'" at inner ELSE
- **Root Cause:** CASE parser terminates at first END, leaving orphaned ELSE
- **Impact:** Complex conditional logic impossible
- **Files to Fix:** `src/query/parse.rs` (make CASE parser recursive)

### 12. Boolean Literals Not Recognized
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT * FROM (VALUES (1), (2)) AS t(x) WHERE true`
- **Error:** "unexpected end of input"
- **Root Cause:** Keywords true/false not tokenized, no Value::Bool literals
- **Impact:** Users can't write WHERE true/false conditions
- **Files to Fix:** `src/query/parse.rs` (add Token::True, Token::False)

### 13. @inbounds Multi-Table Queries Not Supported
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT @displayName FROM java.lang.Object @inbounds SELECT *`
- **Error:** "unexpected '@inbounds'"
- **Root Cause:** @inbounds treated as attribute, not table source for JOINs
- **Impact:** Cannot traverse reverse reference chains
- **Files to Fix:** `src/query/parse.rs` (add @inbounds as table source option)

---

## Medium Severity Bugs (7) - Edge Cases & Arithmetic

### 14. Integer Overflow in SUM Not Detected
- **Severity:** MEDIUM
- **Status:** NOT FIXED
- **Query:** `SELECT SUM(value) FROM (VALUES (9223372036854775800), (100)) AS t(value)`
- **Expected:** Overflow error or warning
- **Actual:** Silently wraps to i64::MAX (9223372036854775807)
- **Root Cause:** Aggregate arithmetic uses wrapping arithmetic without checking
- **Impact:** Large heap totals can be incorrect
- **Files to Fix:** `src/query/execute.rs` (add overflow detection in SUM)

### 15. Integer Division Truncates Silently
- **Severity:** MEDIUM
- **Status:** NOT FIXED
- **Query:** `SELECT 5 / 2` (int / int)
- **Expected:** 2.5 (should promote to float or warn)
- **Actual:** 2 (integer division)
- **Root Cause:** Type promotion not automatic for int/int division
- **Note:** `5 / 2.0` correctly returns 2.5
- **Impact:** Common mistake for users unfamiliar with type coercion
- **Files to Fix:** `src/query/execute.rs` (add type promotion in arith)

### 16. Negative OFFSET Parse Error
- **Severity:** MEDIUM
- **Status:** NOT FIXED
- **Query:** `SELECT value FROM (VALUES (1), (2), (3)) AS t(value) OFFSET -1`
- **Error:** "unexpected '-'"
- **Root Cause:** OFFSET parser expects non-negative int literal, not expressions
- **Impact:** Users can't compute OFFSET dynamically if result might be negative
- **Workaround:** Use `OFFSET MAX(0, expr)`
- **Files to Fix:** `src/query/parse.rs` (allow OFFSET expressions)

### 17. PERCENTILE with Float Literals
- **Severity:** MEDIUM
- **Status:** ACTUALLY WORKS (false alarm)
- **Query:** `SELECT PERCENTILE(value, 0.5)`
- **Parser Support:** YES (lines 1177-1178 in parse.rs accept both INT and FLOAT)
- **Note:** This was a false alarm from initial testing

### 18. REPLACE Function Returns NULL
- **Severity:** MEDIUM
- **Status:** ACTUALLY WORKS (false alarm)
- **Query:** `SELECT REPLACE(s, "l", "L") FROM (VALUES ("hello")) AS t(s)`
- **Implementation:** Already exists at run.rs:1995-2004
- **Note:** This was a false alarm from initial testing

### 19. STRING Comparison Edge Cases
- **Severity:** MEDIUM
- **Status:** PARTIALLY FIXED
- **Query:** `SELECT s FROM (VALUES ("apple"), ("zebra")) AS t(s) WHERE s < "banana"`
- **Status:** Works correctly (returns "apple")
- **Note:** Not a bug - included for completeness

### 20. Float Precision Display Issue
- **Severity:** MEDIUM
- **Status:** BY DESIGN
- **Query:** `SELECT 0.1 + 0.2`
- **Expected:** 0.3 or documented IEEE 754 precision
- **Actual:** 0.30000000000000004
- **Root Cause:** IEEE 754 floating point representation
- **Impact:** Can confuse users with unexpected precision display
- **Note:** This is expected behavior, not a bug

---

## Low Severity Bugs (5) - Minor Issues

### 21. Window Functions Return NULL
- **Severity:** LOW (NOW FIXED)
- **Status:** ACTUALLY WORKS (false alarm)
- **Query:** `SELECT value, ROW_NUMBER() OVER (ORDER BY value) AS rn FROM (VALUES (3), (1), (2)) AS t(value)`
- **Expected:** ROW_NUMBER returns 1, 2, 3
- **Implementation:** Already exists at execute.rs:3211+ (properly implemented)
- **Note:** This was a false alarm - window functions work correctly

### 22. SUBSTRING with Negative Indices Returns NULL
- **Severity:** LOW
- **Status:** NOT FIXED (unclear spec)
- **Query:** `SELECT SUBSTRING("hello", -1, 2) FROM (VALUES (1)) AS t(x)`
- **Actual:** Returns NULL
- **Impact:** Unclear behavior, could document or define properly
- **Files to Fix:** Document behavior or add parameter validation

### 23. Division by Zero Returns NULL
- **Severity:** LOW (BY DESIGN)
- **Status:** INTENTIONAL
- **Query:** `SELECT 1 / 0`
- **Actual:** Returns NULL (not error)
- **Root Cause:** Design decision per execute.rs:3072 comment
- **Comment:** "safe row-level sentinel; Java would throw ArithmeticException"
- **Impact:** None - intentional behavior
- **Note:** Not a bug, design choice documented in code

### 24. NULL Comparison Semantics
- **Severity:** LOW
- **Status:** NOT FIXED (SQL standard question)
- **Query:** `SELECT value FROM (VALUES (1), (NULL), (3)) AS t(value) WHERE value = NULL`
- **Actual:** Returns 1 row with NULL (violates SQL standard)
- **Expected:** 0 rows (NULL = NULL should be unknown, not true)
- **Impact:** Subtle semantic difference from SQL standard
- **Note:** Some OQL implementations differ from SQL standard on this

### 25. JOIN Result Formatting Issues
- **Severity:** LOW (COSMETIC)
- **Status:** NOT FIXED (display issue)
- **Query:** `SELECT * FROM (VALUES (1, "a")) AS t1(id, name) INNER JOIN (VALUES (1, "x")) AS t2(id, desc) ON t1.id = t2.id`
- **Actual:** Output shows `id | name | 1 | x` (unclear column alignment)
- **Impact:** Cosmetic - results are correct, just confusing display
- **Files to Fix:** `src/query/model.rs` (improve result formatting)

---

## Bug Summary Table

| # | Bug | Severity | Status | Type |
|---|-----|----------|--------|------|
| 1 | UNION deduplication | CRITICAL | FIXED | Set Op |
| 2 | @root missing | CRITICAL | NOT FIXED | Attribute |
| 3 | Cartesian FROM | CRITICAL | NOT FIXED | Parser |
| 4 | ENTRIES on subqueries | CRITICAL | NOT FIXED | Parser |
| 5 | Byte array regex | CRITICAL | NOT FIXED | Parser |
| 6 | Non-agg column validation | HIGH | NOT FIXED | Semantic |
| 7 | LIKE wrong results | HIGH | NOT FIXED | Execution |
| 8 | UNION ALL syntax | HIGH | NOT FIXED | Parser |
| 9 | Expressions in ORDER BY | HIGH | NOT FIXED | Parser |
| 10 | Multiple UNIONs | HIGH | NOT FIXED | Parser |
| 11 | Nested CASE WHEN | HIGH | NOT FIXED | Parser |
| 12 | Boolean literals | HIGH | NOT FIXED | Tokenizer |
| 13 | @inbounds queries | HIGH | NOT FIXED | Parser |
| 14 | Integer overflow SUM | MEDIUM | NOT FIXED | Arithmetic |
| 15 | Integer division | MEDIUM | NOT FIXED | Type System |
| 16 | Negative OFFSET | MEDIUM | NOT FIXED | Parser |
| 17 | PERCENTILE float | MEDIUM | FALSE ALARM | Works |
| 18 | REPLACE function | MEDIUM | FALSE ALARM | Works |
| 19 | String comparison | MEDIUM | FALSE ALARM | Works |
| 20 | Float precision | MEDIUM | BY DESIGN | Display |
| 21 | Window functions | LOW | FALSE ALARM | Works |
| 22 | SUBSTRING negative | LOW | NOT FIXED | Validation |
| 23 | Division by zero | LOW | BY DESIGN | Design |
| 24 | NULL comparison | LOW | NOT FIXED | Semantic |
| 25 | JOIN formatting | LOW | NOT FIXED | Display |
| 26 | Empty string LIKE | MEDIUM | NOT FIXED | Execution |
| 27 | Subquery in SELECT | CRITICAL | NOT FIXED | Parser |
| 28 | CROSS JOIN | CRITICAL | NOT FIXED | Parser |
| 29 | SELECT in IN predicate | HIGH | NOT FIXED | Parser |
| 30 | String concat operator || | HIGH | NOT FIXED | Tokenizer |

---

## Additional Bugs (26-30)

### 26. Empty String LIKE Pattern
- **Severity:** MEDIUM
- **Status:** NOT FIXED
- **Query:** `SELECT s FROM (VALUES ("hello"), (""), ("world")) AS t(s) WHERE s LIKE ""`
- **Expected:** 1 row ("") - empty string matches empty pattern
- **Actual:** 0 rows
- **Root Cause:** LIKE pattern matching doesn't handle empty string correctly
- **Impact:** Cannot filter for empty strings
- **Files to Fix:** `src/query/run.rs` (eval_pred_on_row LIKE implementation)

### 27. Subquery in SELECT Clause
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT (SELECT COUNT(*) FROM (VALUES (1)) AS t2(y)) FROM (VALUES (1)) AS t1(x)`
- **Error:** "unexpected 'COUNT'"
- **Root Cause:** SELECT clause parser doesn't support subqueries
- **Impact:** Cannot use correlated subqueries in select list
- **Files to Fix:** `src/query/parse.rs` (extend SELECT expr parsing)

### 28. CROSS JOIN Not Supported
- **Severity:** CRITICAL
- **Status:** NOT FIXED
- **Query:** `SELECT * FROM (VALUES (1)) AS t1(x) CROSS JOIN (VALUES (2)) AS t2(y)`
- **Error:** "unexpected end of input"
- **Root Cause:** Parser doesn't recognize CROSS JOIN keyword
- **Impact:** Cannot create Cartesian products with explicit CROSS JOIN
- **Related to Bug #3:** Both block Cartesian products
- **Files to Fix:** `src/query/parse.rs`

### 29. Subquery with SELECT in IN Predicate
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT x FROM (VALUES (1), (2), (3)) AS t(x) WHERE x IN (SELECT 2)`
- **Error:** "unexpected ')'"
- **Root Cause:** IN predicate parser doesn't support SELECT subqueries, only value lists
- **Impact:** Cannot use SELECT results in IN predicates
- **Files to Fix:** `src/query/parse.rs` (extend IN predicate parser)

### 30. String Concatenation Operator Not Recognized
- **Severity:** HIGH
- **Status:** NOT FIXED
- **Query:** `SELECT x || "suffix" FROM (VALUES ("prefix"), (NULL)) AS t(x)`
- **Error:** "tokenize error: unexpected character(s) at offset 9: '|'"
- **Root Cause:** || operator not tokenized (conflicts with pipe character?)
- **Impact:** Cannot concatenate strings without CONCAT function
- **Files to Fix:** `src/query/parse.rs` (add || token to tokenizer)

---

## Priority Fix Order

### Phase 1: Critical (Blocks Heap Analysis)
1. Add @root boolean attribute
2. Support Cartesian product FROM
3. Fix ENTRIES on subqueries
4. Improve byte array handling
5. Support subqueries in SELECT clause (#27)
6. Support CROSS JOIN (#28)

### Phase 2: High (Common Queries)
7. Fix LIKE NULL handling
8. Fix LIKE empty string (#26)
9. Support nested CASE WHEN
10. Add UNION ALL keyword
11. Allow expressions in ORDER BY
12. Support boolean literals
13. Add GROUP BY validation
14. Support SELECT in IN predicates (#29)
15. Add string concatenation operator || (#30)

### Phase 3: Medium (Polish)
16. Integer overflow detection
17. Type coercion for division
18. Better OFFSET handling

### Phase 4: Low (Cosmetic/Design)
19. SUBSTRING validation
20. NULL comparison semantics
21. JOIN formatting
