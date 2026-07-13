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

// ── In-text graphics for the `md-graphs` report ─────────────────────────────
// These render proportional bars, sparklines, and tree branches using fixed
// block glyphs so the output is deterministic and lines up in a monospace
// editor. They only feed the `md-graphs` renderer — plain `md` never calls
// them, so its byte-exact output is untouched.

/// Full and partial eighth-width block glyphs, indexed 0..=8, used by both the
/// proportional bar and the sparkline. Index 0 is a space (empty); 1..=8 fill
/// from a thin left/bottom sliver up to a full block.
const EIGHTHS: [char; 9] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉', '█'];
/// Sparkline row glyphs, low → high (8 visible levels; a zero value renders as
/// the lowest glyph so the baseline stays visible).
const SPARK: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A proportional horizontal bar `width` cells wide, filled to `value/max`
/// using Unicode block glyphs with eighth-cell sub-resolution. `max == 0` (or
/// `value <= 0`) renders an empty bar. `value` is clamped to `max`.
///
/// Example: `bar(3, 4, 10)` → `"█▎        "` (3/10 of ten cells).
pub fn bar(value: u64, max: u64, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if max == 0 || value == 0 {
        return " ".repeat(width);
    }
    let v = value.min(max);
    // Work in eighths of a cell to get sub-cell resolution deterministically
    // with integer math: total filled eighths across the whole bar.
    let total_eighths = (v as u128 * (width as u128) * 8) / max as u128;
    let full = (total_eighths / 8) as usize;
    let rem = (total_eighths % 8) as usize;
    let mut s = String::with_capacity(width * 3);
    for _ in 0..full.min(width) {
        s.push(EIGHTHS[8]);
    }
    let mut cells = full.min(width);
    if cells < width && rem > 0 {
        s.push(EIGHTHS[rem]);
        cells += 1;
    }
    for _ in cells..width {
        s.push(' ');
    }
    s
}

/// A one-line sparkline of `values`, one glyph per value, scaled to the series
/// max. An empty series yields an empty string; an all-zero series yields the
/// lowest glyph repeated (a flat baseline).
pub fn sparkline(values: &[u64]) -> String {
    if values.is_empty() {
        return String::new();
    }
    let max = *values.iter().max().unwrap();
    if max == 0 {
        return SPARK[0].to_string().repeat(values.len());
    }
    let mut s = String::with_capacity(values.len() * 3);
    for &v in values {
        // Map v into 0..=7. Guard the top so v==max lands on the highest glyph.
        let idx = ((v as u128 * 7) / max as u128) as usize;
        s.push(SPARK[idx.min(7)]);
    }
    s
}

/// The branch prefix for a node at `depth` in a hierarchy listing. `depth == 0`
/// renders no prefix (top level). Deeper nodes get `is_last`-aware box-drawing
/// connectors: `└─ ` for the last child at its level, `├─ ` otherwise, with the
/// `ancestors_continue` flags supplying `│  ` / `   ` spacers for outer levels.
///
/// `ancestors_continue[i]` == true means the ancestor at depth `i` has a later
/// sibling (so its vertical bar continues past this row). Its length must be
/// `depth` (one flag per level above this node); the flag for this node's own
/// level is given by `is_last`.
pub fn tree_prefix(depth: usize, is_last: bool, ancestors_continue: &[bool]) -> String {
    if depth == 0 {
        return String::new();
    }
    let mut s = String::with_capacity(depth * 3);
    // Spacers for every ancestor level except the immediate parent.
    for &cont in ancestors_continue.iter().take(depth.saturating_sub(1)) {
        s.push_str(if cont { "│  " } else { "   " });
    }
    s.push_str(if is_last { "└─ " } else { "├─ " });
    s
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

    #[test]
    fn bar_is_fixed_display_width() {
        // Every bar of a given width renders exactly `width` glyphs, regardless
        // of value, so table columns stay aligned.
        for &(v, m) in &[(0u64, 10u64), (1, 10), (5, 10), (10, 10), (10, 0), (7, 3)] {
            assert_eq!(display_width(&bar(v, m, 10)), 10, "bar({v},{m},10)");
        }
    }

    #[test]
    fn bar_full_and_empty_endpoints() {
        assert_eq!(bar(0, 10, 4), "    "); // empty
        assert_eq!(bar(10, 10, 4), "████"); // full
        assert_eq!(bar(5, 0, 4), "    "); // max==0 → empty
        assert_eq!(bar(20, 10, 4), "████"); // value clamped to max
    }

    #[test]
    fn bar_partial_uses_eighths() {
        // Half of a single cell → the ▌ (4/8) glyph.
        assert_eq!(bar(1, 2, 1), "▌");
        // 3/10 of ten cells = 24 eighths = exactly 3 full blocks.
        assert_eq!(bar(3, 10, 10), "███       ");
        // 1/4 of one cell = 2 eighths → the ▎ glyph.
        assert_eq!(bar(1, 4, 1), "▎");
    }

    #[test]
    fn sparkline_scales_and_bounds() {
        assert_eq!(sparkline(&[]), "");
        assert_eq!(sparkline(&[0, 0, 0]), "▁▁▁"); // all-zero → flat baseline
        let s = sparkline(&[1, 2, 4, 8]);
        assert_eq!(s.chars().count(), 4);
        // Max value maps to the tallest glyph; min (nonzero) to a low glyph.
        assert!(s.ends_with('█'), "spark: {s:?}");
    }

    #[test]
    fn tree_prefix_shapes() {
        assert_eq!(tree_prefix(0, false, &[]), "");
        assert_eq!(tree_prefix(1, false, &[true]), "├─ ");
        assert_eq!(tree_prefix(1, true, &[true]), "└─ ");
        // Depth 2, parent continues → "│  " spacer then a branch.
        assert_eq!(tree_prefix(2, false, &[true, false]), "│  ├─ ");
        // Depth 2, parent is last → "   " spacer then a branch.
        assert_eq!(tree_prefix(2, true, &[false, true]), "   └─ ");
    }
}
