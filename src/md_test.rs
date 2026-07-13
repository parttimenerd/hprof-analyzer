//! Structural Markdown assertions for tests.
//!
//! A tiny, dependency-free parser for the subset of GitHub-flavored Markdown
//! that our report generator emits: ATX headings (`#`..`######`), bullet list
//! items (`- ...`), and pipe tables (header / `| --- | ---: |` delimiter / data
//! rows). It exists so tests can assert on *structure* ("under the `## OOM
//! Triage` heading there is a bullet starting with `**Headline retainer:**`",
//! "the Heap Composition table has a column named Kind") instead of blind
//! substring matches that would falsely pass on text inside a code block or the
//! wrong section.
//!
//! Design notes:
//! - `Md::parse` builds a flat list of headings (with byte offsets into the
//!   source) so a *section* can be reconstructed as the slice from a heading up
//!   to the next heading of same-or-higher level.
//! - Tables and bullets are parsed lazily from a section's body text, which
//!   keeps the model small and avoids tracking nesting we do not need.

#![allow(dead_code)] // Helpers are used across report.rs and integration.rs; not every one in both.

/// An ATX heading with its level (1..=6), trimmed text, and the byte range of
/// its section body (everything after this heading's line up to the next
/// heading of same-or-higher level).
#[derive(Debug, Clone)]
pub struct Heading {
    level: u8,
    text: String,
    /// Byte offset of the first character *after* this heading's line.
    body_start: usize,
    /// Byte offset where this heading's section body ends (exclusive).
    body_end: usize,
}

impl Heading {
    pub fn level(&self) -> u8 {
        self.level
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Parsed Markdown document: the original source plus the heading index.
#[derive(Debug, Clone)]
pub struct Md {
    src: String,
    headings: Vec<Heading>,
}

/// A section is a heading together with its body text. It is a lightweight view
/// borrowing from the owning [`Md`].
#[derive(Debug, Clone, Copy)]
pub struct Section<'a> {
    heading: &'a Heading,
    body: &'a str,
}

/// A parsed pipe table: column names (in order) and data rows (cells in order).
#[derive(Debug, Clone)]
pub struct MdTable {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Md {
    /// Parse a Markdown string into the structural model.
    pub fn parse(src: &str) -> Md {
        // First pass: locate every ATX heading and record its level, text and
        // the byte offset at which its line begins / its body begins.
        struct Raw {
            level: u8,
            text: String,
            line_start: usize,
            body_start: usize,
        }
        let mut raw: Vec<Raw> = Vec::new();
        let mut offset = 0usize;
        for line in src.split_inclusive('\n') {
            let line_start = offset;
            offset += line.len();
            let trimmed = line.trim_end_matches(['\n', '\r']);
            if let Some((level, text)) = parse_atx_heading(trimmed) {
                raw.push(Raw {
                    level,
                    text,
                    line_start,
                    body_start: offset, // first byte after this heading line
                });
            }
        }

        // Second pass: each heading's body ends where the next heading of
        // same-or-higher level (numerically <= level) begins; otherwise EOF.
        let mut headings = Vec::with_capacity(raw.len());
        for i in 0..raw.len() {
            let mut body_end = src.len();
            for r in raw.iter().skip(i + 1) {
                if r.level <= raw[i].level {
                    body_end = r.line_start;
                    break;
                }
            }
            headings.push(Heading {
                level: raw[i].level,
                text: raw[i].text.clone(),
                body_start: raw[i].body_start,
                body_end,
            });
        }

        Md {
            src: src.to_string(),
            headings,
        }
    }

    /// Look up a heading whose text equals or contains `needle`. Returns the
    /// first match in document order.
    pub fn heading(&self, needle: &str) -> Option<&Heading> {
        self.headings
            .iter()
            .find(|h| h.text == needle || h.text.contains(needle))
    }

    /// The section (heading + body) for the first heading matching `needle`.
    pub fn section(&self, needle: &str) -> Option<Section<'_>> {
        let idx = self
            .headings
            .iter()
            .position(|h| h.text == needle || h.text.contains(needle))?;
        let h = &self.headings[idx];
        Some(Section {
            heading: h,
            body: &self.src[h.body_start..h.body_end],
        })
    }

    /// Byte offset of the first heading matching `needle`, if any. Useful for
    /// ordering assertions ("A comes before B").
    pub fn heading_offset(&self, needle: &str) -> Option<usize> {
        self.headings
            .iter()
            .find(|h| h.text == needle || h.text.contains(needle))
            .map(|h| h.body_start)
    }
}

impl<'a> Section<'a> {
    pub fn heading(&self) -> &Heading {
        self.heading
    }
    pub fn level(&self) -> u8 {
        self.heading.level
    }
    pub fn body(&self) -> &str {
        self.body
    }

    /// Does the section body contain a bullet list item (`- ...`) whose text
    /// (after the marker) starts with `prefix`?
    pub fn has_bullet_starting_with(&self, prefix: &str) -> bool {
        bullets(self.body).any(|b| b.starts_with(prefix))
    }

    /// Does the section body contain a bullet whose text contains `needle`?
    pub fn has_bullet_containing(&self, needle: &str) -> bool {
        bullets(self.body).any(|b| b.contains(needle))
    }

    /// All bullet item texts (marker stripped) in this section body.
    pub fn bullets(&self) -> Vec<String> {
        bullets(self.body).map(str::to_string).collect()
    }

    /// Every pipe table found in this section body, in document order.
    pub fn tables(&self) -> Vec<MdTable> {
        parse_tables(self.body)
    }

    /// The nth (0-based) pipe table in this section body.
    pub fn table(&self, n: usize) -> Option<MdTable> {
        parse_tables(self.body).into_iter().nth(n)
    }

    /// Does the section body literally contain `needle`? Kept for the rare case
    /// where prose (not a heading/bullet/table) is what must be asserted; it is
    /// still scoped to *this* section rather than the whole document.
    pub fn body_contains(&self, needle: &str) -> bool {
        self.body.contains(needle)
    }
}

impl MdTable {
    pub fn columns(&self) -> &[String] {
        &self.columns
    }
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Is there a column whose name equals or contains `name`?
    pub fn has_column(&self, name: &str) -> bool {
        self.columns.iter().any(|c| c == name || c.contains(name))
    }

    /// Index of the first column matching `name` (exact then contains).
    fn column_index(&self, name: &str) -> Option<usize> {
        self.columns
            .iter()
            .position(|c| c == name)
            .or_else(|| self.columns.iter().position(|c| c.contains(name)))
    }

    /// Cell at `row` (0-based data row) in the column named `col`.
    pub fn cell(&self, row: usize, col: &str) -> Option<&str> {
        let ci = self.column_index(col)?;
        self.rows
            .get(row)
            .and_then(|r| r.get(ci))
            .map(|s| s.as_str())
    }

    /// Does any data row have `value` (equals or contains) in column `col`?
    pub fn has_row_where(&self, col: &str, value: &str) -> bool {
        let Some(ci) = self.column_index(col) else {
            return false;
        };
        self.rows
            .iter()
            .filter_map(|r| r.get(ci))
            .any(|c| c == value || c.contains(value))
    }
}

/// Parse a single line as an ATX heading, returning `(level, text)`.
///
/// Rules match GitHub: 1..=6 leading `#`, then at least one space, then the
/// heading text (trailing `#` runs are stripped). Lines with 7+ `#` or no space
/// after the run are not headings.
fn parse_atx_heading(line: &str) -> Option<(u8, String)> {
    let hashes = line.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = &line[hashes..];
    // Must be followed by a space (or be an empty heading like `#`).
    if !rest.is_empty() && !rest.starts_with(' ') {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim().to_string();
    Some((hashes as u8, text))
}

/// Iterate bullet-item texts (marker `- ` or `* ` stripped) from a body.
fn bullets(body: &str) -> impl Iterator<Item = &str> {
    body.lines().filter_map(|l| {
        let t = l.trim_start();
        t.strip_prefix("- ")
            .or_else(|| t.strip_prefix("* "))
            .map(str::trim)
    })
}

/// Split a pipe-table row into trimmed cells, dropping the empty leading/
/// trailing fields produced by outer pipes.
fn split_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

/// Is this a table delimiter row (all cells are runs of `-` with optional
/// leading/trailing `:` for alignment)? e.g. `|---|---:|` or `| :--- | ---: |`.
fn is_delimiter_row(line: &str) -> bool {
    let cells = split_row(line);
    if cells.is_empty() {
        return false;
    }
    cells.iter().all(|c| {
        let c = c.trim();
        let c = c.strip_prefix(':').unwrap_or(c);
        let c = c.strip_suffix(':').unwrap_or(c);
        !c.is_empty() && c.chars().all(|ch| ch == '-')
    })
}

/// Parse every pipe table in `body`. A table is a header row immediately
/// followed by a delimiter row, then zero or more data rows (until a
/// non-pipe/blank line).
fn parse_tables(body: &str) -> Vec<MdTable> {
    let lines: Vec<&str> = body.lines().collect();
    let mut tables = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let looks_like_row = |l: &str| l.trim_start().starts_with('|');
        if looks_like_row(lines[i]) && i + 1 < lines.len() && is_delimiter_row(lines[i + 1]) {
            let columns = split_row(lines[i]);
            let mut rows = Vec::new();
            let mut j = i + 2;
            while j < lines.len() && looks_like_row(lines[j]) && !is_delimiter_row(lines[j]) {
                rows.push(split_row(lines[j]));
                j += 1;
            }
            tables.push(MdTable { columns, rows });
            i = j;
        } else {
            i += 1;
        }
    }
    tables
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "\
# Title

Intro prose.

## Alpha

- **One:** first bullet
- second bullet

### Alpha Sub

Nested body text.

## Beta

| Name | Count | Size |
|:---|---:|---:|
| `com.foo.A` | 10 | 1 KB |
| `com.foo.B` | 20 | 2 KB |

Trailing prose after table.
";

    #[test]
    fn parses_heading_levels() {
        let md = Md::parse(DOC);
        assert_eq!(md.heading("Title").unwrap().level(), 1);
        assert_eq!(md.heading("Alpha").unwrap().level(), 2);
        assert_eq!(md.heading("Alpha Sub").unwrap().level(), 3);
        assert_eq!(md.heading("Beta").unwrap().level(), 2);
        assert!(md.heading("Nonexistent").is_none());
    }

    #[test]
    fn heading_matches_by_contains() {
        let md = Md::parse(DOC);
        // "Alpha" matches "Alpha" (exact) first, in document order.
        assert_eq!(md.heading("Alpha").unwrap().text(), "Alpha");
        // A substring that only appears in the sub-heading resolves to it.
        assert_eq!(md.heading("Alpha Sub").unwrap().text(), "Alpha Sub");
    }

    #[test]
    fn section_body_is_scoped_to_next_same_or_higher_heading() {
        let md = Md::parse(DOC);
        let alpha = md.section("Alpha").unwrap();
        // Alpha's body includes its own bullets AND the nested ### Alpha Sub,
        // but stops before ## Beta (same level).
        assert!(alpha.body().contains("first bullet"));
        assert!(alpha.body().contains("Alpha Sub"));
        assert!(alpha.body().contains("Nested body text"));
        assert!(!alpha.body().contains("Beta"));
        assert!(!alpha.body().contains("Name | Count"));

        // The nested sub-section is scoped tighter and ends at ## Beta.
        let sub = md.section("Alpha Sub").unwrap();
        assert!(sub.body().contains("Nested body text"));
        assert!(!sub.body().contains("first bullet"));
    }

    #[test]
    fn bullets_are_detected_by_prefix_and_content() {
        let md = Md::parse(DOC);
        let alpha = md.section("Alpha").unwrap();
        assert!(alpha.has_bullet_starting_with("**One:**"));
        assert!(alpha.has_bullet_containing("first bullet"));
        assert!(alpha.has_bullet_starting_with("second bullet"));
        // No bullet begins with this prefix.
        assert!(!alpha.has_bullet_starting_with("third"));
        assert_eq!(alpha.bullets().len(), 2);
    }

    #[test]
    fn table_columns_and_alignment() {
        let md = Md::parse(DOC);
        let beta = md.section("Beta").unwrap();
        let t = beta.table(0).expect("beta has a table");
        assert_eq!(t.columns(), &["Name", "Count", "Size"]);
        assert!(t.has_column("Name"));
        assert!(t.has_column("Count"));
        assert!(t.has_column("Size"));
        assert!(!t.has_column("Nope"));
        assert_eq!(t.rows().len(), 2);
    }

    #[test]
    fn table_cells_by_column_name() {
        let md = Md::parse(DOC);
        let beta = md.section("Beta").unwrap();
        let t = beta.table(0).unwrap();
        assert_eq!(t.cell(0, "Name"), Some("`com.foo.A`"));
        assert_eq!(t.cell(0, "Count"), Some("10"));
        assert_eq!(t.cell(1, "Size"), Some("2 KB"));
        assert_eq!(t.cell(2, "Name"), None); // out of range row
        assert_eq!(t.cell(0, "Missing"), None); // unknown column
        assert!(t.has_row_where("Name", "com.foo.B"));
        assert!(!t.has_row_where("Name", "com.foo.Z"));
    }

    #[test]
    fn delimiter_row_recognition() {
        assert!(is_delimiter_row("|---|---:|"));
        assert!(is_delimiter_row("| :--- | ---: | :---: |"));
        assert!(is_delimiter_row("|---|"));
        assert!(!is_delimiter_row("| Name | Count |"));
        assert!(!is_delimiter_row("| a-b | c |"));
    }

    #[test]
    fn atx_heading_edge_cases() {
        assert_eq!(parse_atx_heading("# Hi"), Some((1, "Hi".to_string())));
        assert_eq!(
            parse_atx_heading("###### Deep"),
            Some((6, "Deep".to_string()))
        );
        // 7 hashes is not a heading.
        assert_eq!(parse_atx_heading("####### Too deep"), None);
        // No space after the run: a fragment, not a heading.
        assert_eq!(parse_atx_heading("#nospace"), None);
        // Trailing hashes are stripped.
        assert_eq!(parse_atx_heading("## Mid ##"), Some((2, "Mid".to_string())));
        assert_eq!(parse_atx_heading("not a heading"), None);
    }

    #[test]
    fn no_table_when_delimiter_missing() {
        let md = Md::parse("## X\n\n| a | b |\n| 1 | 2 |\n");
        let x = md.section("X").unwrap();
        assert!(x.tables().is_empty(), "no delimiter row => not a table");
    }
}
