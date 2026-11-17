# Rust Learning Session — hprof-analyzer as Playground

**For the teaching agent:** Work through this document lesson by lesson with the student. For each lesson:
1. Read the "Concept" section aloud (paraphrase naturally).
2. Show the code excerpt and explain it.
3. Give the student the task. Wait for their attempt.
4. Give brief feedback — what's right, what to fix, why.
5. Move on only when the student is comfortable.

**Student background:** Fluent in C++, Java, Python, JavaScript. Zero Rust experience. Explain ownership from scratch but skip "what is a variable."

**Arc:** By the end the student has added a real method to `types.rs`, fixed code in `reader.rs` style, and can navigate the full codebase independently.

---

## Part 1 — The Language

---

### Lesson 1 — Ownership and Move Semantics

**File:** `src/types.rs` and a short scratch example

#### Concept

Rust has no garbage collector and no `free()`. Instead, every value has exactly one **owner**. When the owner goes out of scope, the value is dropped automatically. This is enforced at compile time — no runtime overhead.

**The key rule:** when you assign a value to another variable (or pass it to a function), ownership *moves*. The original variable becomes invalid.

```rust
let s1 = String::from("hello");
let s2 = s1;           // ownership moves to s2
println!("{}", s1);    // COMPILE ERROR: s1 was moved
```

Compare to Java: both `s1` and `s2` would point to the same object (reference semantics). In Rust, after the move, `s1` is gone — the compiler enforces it.

Compare to C++ move semantics (`std::move`): same idea, but in Rust it's always the default for non-Copy types, and the compiler prevents you from using the moved-from variable.

**Copy types** (small, stack-only: integers, booleans, `char`, `f32`, etc.) are copied automatically instead of moved. You can implement `Copy` for your own types if they're small enough.

```rust
let x: u32 = 5;
let y = x;             // x is copied, not moved
println!("{}", x);     // fine — u32 implements Copy
```

From this project, `src/types.rs:63`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HprofType {
    Object, Boolean, Char, Float, Double, Byte, Short, Int, Long,
}
```

`#[derive(Copy)]` means `HprofType` is Copy — it's just a small tag byte, so copying it is fine. That's why in `Lesson 3` you'll see `byte_size(self)` rather than `byte_size(&self)` — passing `self` by value is free because it's `Copy`.

#### Task

Predict whether each snippet compiles. Explain *why* in one sentence each.

**Snippet A:**
```rust
let name = String::from("Pass1");
let also_name = name;
println!("{}", name);
```

**Snippet B:**
```rust
let count: u64 = 42;
let also_count = count;
println!("{}", count);
```

**Snippet C:**
```rust
fn print_it(s: String) {
    println!("{}", s);
}
let msg = String::from("hello");
print_it(msg);
println!("{}", msg);
```

**Snippet D:**
```rust
#[derive(Copy, Clone)]
struct Tag(u8);

let t = Tag(0x21);
let t2 = t;
println!("{}", t.0);
```

---

### Lesson 2 — Borrowing and References

**File:** `src/pass1.rs`, `src/reader.rs`

#### Concept

Moves are useful but inconvenient when you just want to *look at* a value. **Borrowing** lets you pass a reference without transferring ownership.

```rust
fn print_it(s: &String) {   // & = "borrow" (shared reference)
    println!("{}", s);
}
let msg = String::from("hello");
print_it(&msg);              // lend a reference
println!("{}", msg);         // still valid — we only borrowed it
```

Two kinds of reference:
- `&T` — shared (read-only). Many can exist at once.
- `&mut T` — exclusive (read-write). Only one can exist at a time, and no shared refs can exist simultaneously.

**The borrow checker rules (compile-time):**
1. Any number of `&T` at the same time, OR
2. Exactly one `&mut T` — but then zero `&T`s.

This rule eliminates data races and iterator invalidation at compile time.

From `src/pass1.rs:495`, notice how `scan_heap_segment` takes many `&mut` parameters — it needs to mutate all those arrays, and the borrow checker guarantees no two callers can mutate the same array at the same time:

```rust
fn scan_heap_segment(
    r: &mut HprofReader,
    id_size: u8,
    mut remaining: u64,
    class_map: &mut HashMap<u64, ClassInfo>,
    tmp_addrs: &mut Vec<u64>,
    tmp_class_ids: &mut Vec<u32>,
    // ... more &mut params
) -> io::Result<()> {
```

In Java this would just be object references — but Java gives no compile-time guarantee that two threads aren't mutating those lists simultaneously. Rust does.

**`&str` vs `String`:**
- `String` = owned, heap-allocated, growable string (like `std::string`)
- `&str` = borrowed view into a string (like `const char*` or `std::string_view`)
- Functions that only read a string should take `&str`, not `String`

```rust
fn greet(name: &str) {           // accepts both &str and &String
    println!("Hello, {name}");
}
greet("world");                  // &str literal
greet(&String::from("world"));   // borrow a String as &str
```

#### Task

This code has a borrow-checker violation. Find it and fix it:

```rust
fn double_first(v: &mut Vec<i32>) -> i32 {
    let first = &v[0];       // shared borrow of v
    v.push(99);              // mutable borrow of v  <- problem
    *first * 2
}
```

Hint: think about what `v.push()` might do to memory, and why `first` would become dangling.

Fix it so it compiles correctly and still returns `v[0] * 2`.

---

### Lesson 3 — Enums, `Option`, and `match`

**File:** `src/types.rs`

#### Concept

Rust enums are much more powerful than Java/C++ enums. Variants can carry data:

```rust
enum Shape {
    Circle(f64),             // carries a radius
    Rectangle(f64, f64),     // carries width and height
    Point,                   // carries nothing
}
```

This is called an **algebraic data type** (sum type). You'll see it everywhere in Rust.

**`Option<T>`** is the built-in null-safety enum:
```rust
enum Option<T> {
    Some(T),   // has a value
    None,      // no value
}
```

No null pointers. If a function might not return a value, it returns `Option<T>`. The compiler forces you to handle both cases.

**`match`** is exhaustive pattern matching — like Java's `switch` but:
- Must cover all cases (compiler error otherwise)
- Is an *expression* (returns a value)
- Can destructure data out of variants

From `src/types.rs:76`:

```rust
pub fn from_code(code: u8) -> Option<Self> {
    match code {
        2  => Some(Self::Object),
        4  => Some(Self::Boolean),
        5  => Some(Self::Char),
        6  => Some(Self::Float),
        7  => Some(Self::Double),
        8  => Some(Self::Byte),
        9  => Some(Self::Short),
        10 => Some(Self::Int),
        11 => Some(Self::Long),
        _  => None,      // _ is the catch-all
    }
}
```

And the existing `byte_size` method (same file, line ~96) shows multiple patterns in one arm with `|`:

```rust
pub fn byte_size(self) -> usize {
    match self {
        Self::Object                    => 0,
        Self::Boolean | Self::Byte      => 1,
        Self::Char    | Self::Short     => 2,
        Self::Float   | Self::Int       => 4,
        Self::Double  | Self::Long      => 8,
    }
}
```

#### Task

`byte_size` returns `usize` and returns `0` for `Object` (callers handle it specially). Your task: add a new method that makes the "unknown for Object" explicit in the type system. Add this to the `impl HprofType` block in `src/types.rs`:

```rust
pub fn fixed_size(self) -> Option<usize> {
    // return None for Object (size depends on id_size, caller must handle it)
    // return Some(n) for all primitive types
    // hint: reuse the byte_size logic, just change the Object arm
}
```

Write the implementation. You're making a real edit to the project.

**After writing it**, also write one unit test in the `#[cfg(test)] mod tests` block at the bottom of `types.rs`:
```rust
#[test]
fn fixed_size_object_is_none() {
    assert_eq!(HprofType::Object.fixed_size(), None);
}
```

---

### Lesson 4 — `Result` and Error Handling

**File:** `src/reader.rs`

#### Concept

Where `Option<T>` models "might be absent", `Result<T, E>` models "might fail":

```rust
enum Result<T, E> {
    Ok(T),    // success, carries the value
    Err(E),   // failure, carries the error
}
```

No exceptions. Functions that can fail return `Result`. Callers must handle both cases.

**The `?` operator** is the killer feature. Instead of:
```java
try {
    int x = readInt();
    int y = readInt();
    return x + y;
} catch (IOException e) { throw e; }
```

In Rust:
```rust
fn read_sum(r: &mut Reader) -> io::Result<i32> {
    let x = r.read_i32()?;   // if Err, return early with that error
    let y = r.read_i32()?;   // same
    Ok(x + y)
}
```

`?` means: "if this is `Err`, return it from the current function; if `Ok`, unwrap the value and continue."

From `src/reader.rs:282`, the HPROF header parser uses `?` throughout:

```rust
fn read_header(&mut self) -> io::Result<()> {
    let mut s = Vec::new();
    loop {
        let b = self.u1()?;        // read one byte, propagate error
        if b == 0 { break; }
        s.push(b);
        if s.len() >= 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HPROF format string exceeds 256 bytes — not a valid HPROF file",
            ));
        }
    }
    self.format = String::from_utf8_lossy(&s).into_owned();
    let id_size = self.u4()?;      // read u32
    if id_size != 4 && id_size != 8 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported id_size in HPROF header: {id_size} (expected 4 or 8)"),
        ));
    }
    self.id_size = id_size as u8;
    self.timestamp_ms = self.u8()?;
    Ok(())                         // success — return unit wrapped in Ok
}
```

**When to use `unwrap()`:** `unwrap()` panics if the value is `Err` or `None`. Use it only in tests and in situations where the error is genuinely impossible. Never in production parsing code.

#### Task

Write this function from scratch (don't need to run it, just write correct Rust):

```rust
/// Parse two u32s from a byte slice and return their sum.
/// Returns Err if the slice is shorter than 8 bytes.
fn sum_of_two_u32s(data: &[u8]) -> Result<u64, String> {
    // hint: use std::convert::TryInto or just index the slice
    // hint: u32::from_be_bytes([a, b, c, d]) parses big-endian bytes
    // hint: return Err("message".to_string()) for the error case
    // hint: return Ok(result) for success
}
```

Write the body. Use early return with `?` or with an explicit `return Err(...)`.

---

### Lesson 5 — Structs, Traits, and `impl`

**File:** `src/heap_ops.rs`, `src/pass1.rs`

#### Concept

**Structs** are Rust's record types — like a class with only data fields, no inheritance:

```rust
struct Point {
    x: f64,
    y: f64,
}
```

**`impl` blocks** add methods:

```rust
impl Point {
    fn distance_from_origin(&self) -> f64 {   // &self = shared borrow of self
        (self.x * self.x + self.y * self.y).sqrt()
    }
}
```

- `&self` → read-only method (like `const` in C++)
- `&mut self` → mutating method
- `self` → consuming method (takes ownership, `self` is gone after)
- No `self` → associated function (like a static method — used for constructors)

**Traits** are interfaces. They define a set of methods a type must implement:

```rust
trait Greet {
    fn greet(&self) -> String;
}
impl Greet for Point {
    fn greet(&self) -> String {
        format!("I am ({}, {})", self.x, self.y)
    }
}
```

**`Display`** is the standard trait for human-readable printing (used by `println!("{}", x)`). From `src/heap_ops.rs:31`:

```rust
#[derive(Debug)]
pub enum HeapOpsError {
    BadParam(String),
    CacheMissing(String),
    Internal(String),
}

impl std::fmt::Display for HeapOpsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadParam(s)      => write!(f, "bad parameter: {s}"),
            Self::CacheMissing(s)  => write!(f, "cache missing: {s}"),
            Self::Internal(s)      => write!(f, "internal error: {s}"),
        }
    }
}
```

Note: `#[derive(Debug)]` auto-generates a debug representation (like `{:?}`). `Display` (used by `{}`) must be written by hand — it's the human-readable form.

#### Task

Add a `Display` implementation to `HprofType` in `src/types.rs` so that printing a type gives its Java name:

```rust
impl std::fmt::Display for HprofType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Object  -> "Object"
        // Boolean -> "boolean"
        // Char    -> "char"
        // Int     -> "int"
        // Long    -> "long"
        // (etc — use the lowercase Java primitive names for primitives)
    }
}
```

Write the implementation. After writing it, add a test:
```rust
#[test]
fn display_names() {
    assert_eq!(HprofType::Int.to_string(), "int");
    assert_eq!(HprofType::Object.to_string(), "Object");
}
```

---

### Lesson 6 — Iterators and Closures

**File:** `src/mcp/mod.rs`

#### Concept

Rust iterators are **lazy** — they do no work until consumed. Chaining `.map()`, `.filter()`, etc. builds a pipeline; `.collect()` or a `for` loop runs it. They compile to the same machine code as a manual loop — zero overhead.

```rust
let numbers = vec![1, 2, 3, 4, 5];
let doubled_evens: Vec<i32> = numbers
    .iter()
    .filter(|&&x| x % 2 == 0)   // keep evens
    .map(|&x| x * 2)             // double them
    .collect();                  // gather into Vec
// doubled_evens = [4, 8]
```

**Closures** are inline anonymous functions:
```rust
|x| x * 2           // takes x, returns x*2
|x, y| x + y        // two params
|x| { x * 2 + 1 }   // block body
```

Like Java lambdas, but they capture variables by borrow by default (or by move with `move |x| ...`).

From `src/mcp/mod.rs`, the `load_dump` handler builds two text summaries using iterator chains:

```rust
let suspects_text: String = result
    .report
    .leaks
    .suspects
    .iter()
    .take(5)
    .enumerate()
    .map(|(i, s)| {
        format!(
            "  {}. {} — retained {} MB\n",
            i + 1,
            s.pretty_class,
            s.retained / 1_000_000
        )
    })
    .collect();
```

Breaking it down:
- `.iter()` — borrows each element
- `.take(5)` — stop after 5
- `.enumerate()` — pairs each element with its index `(0, elem), (1, elem), ...`
- `.map(|(i, s)| ...)` — destructure the tuple in the closure signature
- `.collect::<String>()` — concatenate all the format strings into one String

**`format!`** is like `String.format()` in Java or f-strings in Python.

#### Task

Write a function that takes a slice of `HprofType` values and returns a `Vec<String>` of their Java names — but only for primitive types (not `Object`). Use your `Display` impl from Lesson 5 and `fixed_size` from Lesson 3.

```rust
fn primitive_names(types: &[HprofType]) -> Vec<String> {
    types
        .iter()
        // filter out Object (hint: use fixed_size() — it returns None for Object)
        // map each remaining type to its string name (hint: .to_string())
        // collect
}
```

Fill in the iterator chain. Three lines inside the function body.

---

## Part 2 — The Libraries

---

### Lesson 7 — The Lexer: `logos`

**File:** `src/query/parse.rs`

#### Concept

This project implements an OQL query language (like SQL for heap dumps). Parsing has two stages:
1. **Lexing** (tokenizing): raw text → stream of tokens
2. **Parsing**: stream of tokens → structured AST

**`logos`** handles the lexer. You annotate an enum with regex patterns, and it generates a state machine that recognizes each variant:

```rust
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]   // skip whitespace between tokens
enum Token {
    #[token("(")]
    LParen,

    #[token("SELECT")]
    Select,

    #[regex(r"[0-9]+", |lex| lex.slice().parse::<i64>().ok())]
    Int(i64),   // the closure parses the matched text into an i64
}
```

From `src/query/parse.rs:35`:

```rust
#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\r\n]+")]
pub enum Token {
    #[token("(")]   LParen,
    #[token(")")]   RParen,
    #[token(",")]   Comma,
    #[token("=")]   Eq,
    #[token("!=")]  Ne,
    #[token("*")]   Star,
    #[token("+")]   Plus,
    // ...

    // @attribute — capture the name after '@'
    #[regex(r"@[A-Za-z_][A-Za-z0-9_.$]*", |lex| lex.slice()[1..].to_string())]
    At(String),

    // double-quoted string with escape handling
    #[regex(r#""([^"\\]|\\.)*""#, |lex| { /* parse body */ })]
    Str(String),
}
```

Key idea: **enum variants carry data** (from Lesson 3). `At(String)` carries the attribute name, `Str(String)` carries the string contents. The `logos` closure runs on the matched text and produces the payload.

#### Task

Read the full `Token` enum in `src/query/parse.rs` (lines ~35–160). Then trace this query string through the lexer by hand:

```
SELECT @objectId FROM INSTANCEOF java.lang.String WHERE @retainedHeapSize > 1000
```

Write out what tokens the lexer would produce, in order. Format:
```
Ident("SELECT") | At("objectId") | Ident("FROM") | ...
```

(You don't need to run code — just read the token definitions and match them up.)

---

### Lesson 8 — The Parser: `chumsky`

**File:** `src/query/parse.rs`

#### Concept

**`chumsky`** is a parser combinator library. Instead of writing a loop that reads tokens one at a time, you compose small parsers into bigger ones using combinators:

| Combinator | Meaning |
|---|---|
| `just(Token::LParen)` | match exactly this token |
| `a.then(b)` | match a, then b; return `(a_result, b_result)` |
| `a.or(b)` | try a; if it fails, try b |
| `choice([a, b, c])` | try each in order, return first success |
| `a.map(\|x\| ...)` | transform the result |
| `a.ignore_then(b)` | match a then b, discard a's result |
| `a.repeated()` | match a zero or more times |

From `src/query/parse.rs:280`, the `dom_fn` parser parses `dominators(alias)` syntax:

```rust
fn dom_fn<'a, I>(name: &'static str)
    -> impl Parser<'a, I, String, extra::Err<Rich<'a, Token>>> + Clone
{
    ident_ci(name)                         // match the keyword (case-insensitive)
        .ignore_then(just(Token::LParen))  // then "(", discard it
        .ignore_then(any_ident().or_not()) // then optional identifier
        .then_ignore(just(Token::RParen))  // then ")", discard it
        .validate(move |arg, e, emitter| match arg {
            Some(a) => a,
            None => {
                emitter.emit(Rich::custom(e.span(),
                    format!("{name}(x) requires a single alias argument")));
                String::new()
            }
        })
}
```

Reading this: "match the keyword, skip `(`, optionally match an identifier, skip `)`, validate the identifier is present."

The return type `impl Parser<...>` means "some type that implements the Parser trait" — the exact type is complex and generated by the combinator chain.

#### Task

Read the above `dom_fn` function and answer these questions:

1. What does `.ignore_then()` do differently from `.then()`?
2. What does `.or_not()` return if the identifier is missing?
3. What happens if someone writes `dominators()` (no argument)?
4. What type does `dom_fn` return when parsing succeeds?

Then find where `dom_fn` is called in the file (search for `dom_fn(` in `src/query/parse.rs`). What keywords does it handle?

---

## Part 3 — Codebase Orientation

---

### Lesson 9 — Full Pipeline Walkthrough

**Files:** `src/main.rs`, `src/pass1.rs`, `src/pass2/mod.rs`, `src/dominator.rs`, `src/report/`

#### Concept

No new Rust concepts here — this is a guided read-along. The goal: you can open any file in the repo and know where you are in the pipeline.

#### The pipeline (from `src/main.rs:1-13`)

```
pass1 → pass2 → compress cold arrays → RPO DFS → inbound CSR → dominators → retained → build_model → render
```

**Step by step:**

| Stage | File | What it does |
|---|---|---|
| **pass1** | `src/pass1.rs` | First scan of the `.hprof` file. Reads metadata (strings, class names, stack frames) and builds a compact index: one entry per object in parallel arrays (`class_ids`, `kind`, etc.). Does NOT compute sizes. |
| **pass2** | `src/pass2/mod.rs` | Second scan. Builds the reference graph: forward CSR (what does each object point to?) and inbound CSR (what points to each object?). Also computes shallow sizes. |
| **RPO DFS** | `src/rpo_dfs.rs` | Reverse post-order depth-first search over the reference graph. Needed by the dominator algorithm. |
| **dominators** | `src/dominator.rs` | Lengauer-Tarjan dominator tree. For each object, finds its "immediate dominator" — the object you'd have to free to also free this one. The basis for "retained heap size." |
| **retained** | `src/retained.rs` | Computes retained size: the sum of shallow sizes of all objects in each node's dominator subtree. |
| **build_model** | `src/report/build.rs` | Turns raw arrays into the structured `Report`: leak suspects, biggest classes, waste analysis, etc. |
| **render** | `src/report/`, `src/html.rs`, `src/md.rs` | Serializes the report as JSON, HTML, or Markdown. |

**The data structures that connect them:**

- `Pass1` struct → output of pass1, input to pass2
- `Pass2` struct → output of pass2, holds the CSR graph
- `idom: Vec<u32>` → output of dominators: for each object index, its immediate dominator's index
- `retained: Vec<u64>` → output of retained: retained bytes per object
- `Report` struct → final output, serialized to disk and returned by MCP tools

**How MCP tools plug in** (`src/mcp/mod.rs`):

The MCP server loads a dump once and caches the full analysis result. When a tool like `load_dump` or `get_report` is called, it reads from the cache — it doesn't re-run the pipeline. The cache is on disk at `<dump>.hprof-cache/`.

#### Task

This is a navigation exercise. Without running anything, answer these questions by reading the source:

1. Open `src/pass1.rs`. Find the `Pass1` struct definition. List 5 of its fields and say in one sentence what each stores.

2. Open `src/dominator.rs`. Find the function that computes the dominator tree. What is its return type? What does the `idom` array represent?

3. Open `src/mcp/mod.rs`. Find the `get_report` tool handler (line ~620). What does it call to get the report data — does it re-run analysis or read from cache?

4. Open `src/query/parse.rs`. The OQL query `SELECT @objectId FROM java.lang.String` needs to parse. What token sequence would the `FROM` clause produce? (Look at how `FROM` and class names are tokenized.)

These four questions cover all the major layers of the system. When you can answer them by reading the code, you're oriented.

---

## Appendix: Quick Reference

### Types you'll see everywhere

| Rust | Analogous to | When used |
|---|---|---|
| `String` | `std::string` / `String` | owned, growable string |
| `&str` | `const char*` / `String` (read) | borrowed string slice |
| `Vec<T>` | `std::vector<T>` / `ArrayList<T>` | owned growable array |
| `&[T]` | `span<T>` / array param | borrowed slice of array |
| `HashMap<K,V>` | `unordered_map` / `HashMap` | hash map |
| `Option<T>` | `Optional<T>` / nullable | "might be absent" |
| `Result<T,E>` | checked exception | "might fail" |
| `Box<T>` | `unique_ptr<T>` / heap ref | heap-allocated single value |
| `Arc<T>` | `shared_ptr<T>` / shared ref | shared ownership, thread-safe |

### Syntax quick map

| Java/C++ | Rust |
|---|---|
| `void foo()` | `fn foo()` |
| `int foo()` | `fn foo() -> i32` |
| `this.x` | `self.x` |
| `null` | `None` |
| `instanceof` | `matches!(x, SomeVariant(_))` or `if let` |
| `try/catch` | `match result { Ok(v) => ..., Err(e) => ... }` |
| `// comment` | `// comment` |
| `/* block */` | `/* block */` |
| `System.out.println` | `println!("{}", x)` |

### Common iterator methods

```rust
.iter()          // borrow each element
.iter_mut()      // mutable borrow each element
.into_iter()     // move each element (consumes the collection)
.map(|x| ...)    // transform
.filter(|x| ...) // keep matching
.enumerate()     // add index: (0, elem), (1, elem), ...
.take(n)         // first n elements
.skip(n)         // skip first n
.flat_map(|x| ...) // map + flatten
.any(|x| ...)    // true if any matches
.all(|x| ...)    // true if all match
.count()         // count elements
.sum()           // sum (requires Summable type)
.collect()       // gather into Vec, HashMap, String, etc.
```
