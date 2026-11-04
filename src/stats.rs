//! Line and character bookkeeping for the status bar and "go to line".
//!
//! Recomputed once per text change; cursor queries are then O(log n).

/// Squares in one row of 200-square manuscript paper (원고지).
pub const MANUSCRIPT_COLUMNS: usize = 20;
/// Rows on one sheet of manuscript paper.
pub const MANUSCRIPT_ROWS: usize = 10;

#[derive(Clone)]
pub struct TextStats {
    /// Character index where each line starts. Always contains 0.
    line_starts: Vec<usize>,
    /// Total characters including line breaks.
    total: usize,
    /// Rows the text fills on manuscript paper; see [`manuscript_rows`].
    manuscript_rows: usize,
}

impl Default for TextStats {
    /// Stats of an empty text (one empty line).
    fn default() -> Self {
        Self::compute("")
    }
}

impl TextStats {
    pub fn compute(text: &str) -> Self {
        let mut line_starts = vec![0];
        let mut total = 0;
        for c in text.chars() {
            total += 1;
            if c == '\n' {
                line_starts.push(total);
            }
        }
        let manuscript_rows = manuscript_rows(&line_starts, total);
        Self { line_starts, total, manuscript_rows }
    }

    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Characters without line breaks: the "글자 수" users see.
    pub fn chars_without_breaks(&self) -> usize {
        self.total - (self.line_count() - 1)
    }

    /// All characters including line breaks: the largest cursor position.
    pub fn total_chars(&self) -> usize {
        self.total
    }

    /// Zero-based line containing the character index.
    pub fn line_of(&self, char_index: usize) -> usize {
        match self.line_starts.binary_search(&char_index) {
            Ok(line) => line,
            Err(next) => next.saturating_sub(1),
        }
    }

    /// One-based (line, column) of a cursor position.
    pub fn line_col(&self, char_index: usize) -> (usize, usize) {
        let char_index = char_index.min(self.total);
        let line = self.line_of(char_index);
        (line + 1, char_index - self.line_starts[line] + 1)
    }

    /// Characters between two cursor positions, not counting line breaks.
    pub fn chars_between(&self, a: usize, b: usize) -> usize {
        let (lo, hi) = (a.min(b).min(self.total), a.max(b).min(self.total));
        (hi - lo) - (self.line_of(hi) - self.line_of(lo))
    }

    /// Character index of the start of a one-based line.
    pub fn line_start(&self, line: usize) -> Option<usize> {
        self.line_starts.get(line.checked_sub(1)?).copied()
    }

    pub fn manuscript_rows(&self) -> usize {
        self.manuscript_rows
    }

    /// Sheets of 200-square manuscript paper the text fills; a partly used sheet counts.
    pub fn manuscript_sheets(&self) -> usize {
        self.manuscript_rows.div_ceil(MANUSCRIPT_ROWS)
    }
}

/// Rows the text fills on manuscript paper: one character a square (spaces too),
/// and every line starts a new row, leaving the rest of the previous row empty.
/// Empty lines take a row each, except at the end of the text.
fn manuscript_rows(line_starts: &[usize], total: usize) -> usize {
    let line_ends = line_starts.iter().skip(1).map(|&next| next - 1).chain([total]);
    let mut rows = 0;
    // Empty lines are only counted once text follows them.
    let mut blank_rows = 0;
    for (start, end) in line_starts.iter().zip(line_ends) {
        let len = end - start;
        if len == 0 {
            blank_rows += 1;
        } else {
            rows += blank_rows + len.div_ceil(MANUSCRIPT_COLUMNS);
            blank_rows = 0;
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts() {
        let s = TextStats::compute("ab\n가나다\n\nx");
        assert_eq!(s.line_count(), 4);
        assert_eq!(s.chars_without_breaks(), 6);
        assert_eq!(s.line_col(0), (1, 1));
        assert_eq!(s.line_col(2), (1, 3));
        assert_eq!(s.line_col(3), (2, 1));
        assert_eq!(s.line_col(7), (3, 1));
        assert_eq!(s.line_col(9), (4, 2));
        assert_eq!(s.chars_between(0, 9), 6);
        assert_eq!(s.chars_between(1, 4), 2);
        assert_eq!(s.line_start(2), Some(3));
        assert_eq!(s.line_start(5), None);
    }

    #[test]
    fn empty() {
        let s = TextStats::compute("");
        assert_eq!(s.line_count(), 1);
        assert_eq!(s.chars_without_breaks(), 0);
        assert_eq!(s.line_col(0), (1, 1));
        assert_eq!(TextStats::default().line_col(0), (1, 1));
        assert_eq!(s.manuscript_rows(), 0);
        assert_eq!(s.manuscript_sheets(), 0);
    }

    #[test]
    fn manuscript_paper() {
        let rows = |text: &str| TextStats::compute(text).manuscript_rows();
        let sheets = |text: &str| TextStats::compute(text).manuscript_sheets();
        assert_eq!(rows("ab\n가나다\n\nx"), 4);
        assert_eq!(rows(&"가".repeat(20)), 1);
        assert_eq!(rows(&"가".repeat(21)), 2);
        assert_eq!(rows("가 나"), 1);
        // Blank lines count only when text follows them.
        assert_eq!(rows("\n\na"), 3);
        assert_eq!(rows("a\n\n\n"), 1);
        assert_eq!(rows("\n\n"), 0);
        assert_eq!(sheets("a"), 1);
        assert_eq!(sheets(&"가".repeat(200)), 1);
        assert_eq!(sheets(&"가".repeat(201)), 2);
        // Ten short lines fill one sheet even though they hold only ten characters.
        assert_eq!(sheets(&["a"; 10].join("\n")), 1);
        assert_eq!(sheets(&["a"; 11].join("\n")), 2);
    }
}
