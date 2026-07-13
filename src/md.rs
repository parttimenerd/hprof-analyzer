//! Tiny Markdown formatting helpers.
//!
//! The Markdown report is meant to be read directly in a text editor, so
//! tables are rendered with every cell in a column padded to the same width
//! and the delimiter row widened to match. This is still valid
//! GitHub-flavored Markdown (renderers ignore the extra spaces) but is far
//! easier to scan as plain text.
//!
//! Usage:
//! ```ignore
//! let mut t = Table::new(&["#", "Class", "Retained"], &[Align::Right, Align::Left, Align::Right]);
//! t.row(["1".into(), "java.lang.String".into(), "1.2 MB".into()]);
//! t.render(out);
//! ```

/// Column alignment. Left renders a plain `---` delimiter; Right renders
/// `---:` and pads cell contents on the left.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// An accumulating Markdown table. Collects a header and rows, then renders
/// them with per-column padding so the source lines up.
pub struct Table {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Create a table with the given column headers and alignments. The two
    /// slices must be the same length (one alignment per column).
    pub fn new(headers: &[&str], aligns: &[Align]) -> Self {
        debug_assert_eq!(
            headers.len(),
            aligns.len(),
            "headers and aligns must have the same length"
        );
        Table {
            headers: headers.iter().map(|s| s.to_string()).collect(),
            aligns: aligns.to_vec(),
            rows: Vec::new(),
        }
    }

    /// Append one row. The cell count must match the header count.
    pub fn row<I>(&mut self, cells: I)
    where
        I: IntoIterator<Item = String>,
    {
        let row: Vec<String> = cells.into_iter().collect();
        debug_assert_eq!(
            row.len(),
            self.headers.len(),
            "row cell count must match header count"
        );
        self.rows.push(row);
    }

    /// Render the table into `out`, padding every cell in a column to the
    /// column's max content width. Emits a trailing newline after the last
    /// row (but not a blank line — callers add section spacing).
    pub fn render(&self, out: &mut String) {
        // Column width = max over header + all cells (display width).
        let mut widths: Vec<usize> = self.headers.iter().map(|h| display_width(h)).collect();
        for row in &self.rows {
            for (c, cell) in row.iter().enumerate() {
                let w = display_width(cell);
                if w > widths[c] {
                    widths[c] = w;
                }
            }
        }
        // A GFM delimiter needs at least one dash; a right-align delimiter is
        // `<dashes>:`, so a right column needs width >= 2 (one dash + colon)
        // and a left column width >= 1. Header/content widths are almost
        // always larger, so this floor only kicks in for tiny numeric columns.
        for (c, w) in widths.iter_mut().enumerate() {
            let min = if self.aligns[c] == Align::Right { 2 } else { 1 };
            if *w < min {
                *w = min;
            }
        }

        self.render_line(&self.headers, &widths, out);
        self.render_delim(&widths, out);
        for row in &self.rows {
            self.render_line(row, &widths, out);
        }
    }

    fn render_line(&self, cells: &[String], widths: &[usize], out: &mut String) {
        out.push('|');
        for (c, cell) in cells.iter().enumerate() {
            let pad = widths[c].saturating_sub(display_width(cell));
            out.push(' ');
            match self.aligns[c] {
                Align::Left => {
                    out.push_str(cell);
                    for _ in 0..pad {
                        out.push(' ');
                    }
                }
                Align::Right => {
                    for _ in 0..pad {
                        out.push(' ');
                    }
                    out.push_str(cell);
                }
            }
            out.push(' ');
            out.push('|');
        }
        out.push('\n');
    }

    fn render_delim(&self, widths: &[usize], out: &mut String) {
        out.push('|');
        for (c, w) in widths.iter().enumerate() {
            out.push(' ');
            match self.aligns[c] {
                Align::Left => {
                    for _ in 0..*w {
                        out.push('-');
                    }
                }
                Align::Right => {
                    for _ in 0..w.saturating_sub(1) {
                        out.push('-');
                    }
                    out.push(':');
                }
            }
            out.push(' ');
            out.push('|');
        }
        out.push('\n');
    }
}

/// Display width of a string for padding purposes. We measure Unicode scalar
/// values (chars), which is correct for the ASCII-dominant identifiers and
/// numbers in these reports. Backtick-quoted class names are counted with
/// their backticks (they are part of the visible source text).
fn display_width(s: &str) -> usize {
    s.chars().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pads_columns_to_equal_width() {
        let mut t = Table::new(
            &["#", "Class", "Retained"],
            &[Align::Right, Align::Left, Align::Right],
        );
        t.row(["1".into(), "java.lang.String".into(), "1.2 MB".into()]);
        t.row(["10".into(), "int[]".into(), "999 B".into()]);
        let mut out = String::new();
        t.render(&mut out);
        let lines: Vec<&str> = out.lines().collect();
        // Every line must have the same length as the header line.
        let hlen = lines[0].len();
        for l in &lines {
            assert_eq!(l.len(), hlen, "line width mismatch: {l:?}");
        }
        assert_eq!(lines[0], "|  # | Class            | Retained |");
        assert_eq!(lines[1], "| -: | ---------------- | -------: |");
    }

    #[test]
    fn right_align_delimiter_has_colon() {
        let mut t = Table::new(&["N"], &[Align::Right]);
        t.row(["1".into()]);
        let mut out = String::new();
        t.render(&mut out);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].ends_with(": |"), "delim: {:?}", lines[1]);
    }

    #[test]
    fn left_align_delimiter_no_colon() {
        let mut t = Table::new(&["Name"], &[Align::Left]);
        t.row(["ab".into()]);
        let mut out = String::new();
        t.render(&mut out);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[1].contains("----"), "delim: {:?}", lines[1]);
        assert!(
            !lines[1].contains(':'),
            "delim should have no colon: {:?}",
            lines[1]
        );
    }
}
