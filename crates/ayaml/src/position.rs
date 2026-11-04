//! Byte-offset to line/column mapping for scanner positions.
//!
//! The mapping is built once per input. It understands LF, CR, CRLF, and a
//! leading UTF-8 BOM. NEL (U+0085), LS (U+2028), and PS (U+2029) are content
//! in YAML 1.2, not line breaks.

use std::cell::Cell;

use crate::span::Marker;

/// A line-start index over a UTF-8 input.
#[derive(Clone, Debug)]
pub(crate) struct LineIndex {
    starts: Vec<usize>,
    content_start: usize,
    len: usize,
    ascii: bool,
    last_line: Cell<usize>,
}

impl LineIndex {
    /// Build an index for `input`.
    #[must_use]
    pub(crate) fn new(input: &str) -> Self {
        if input.is_ascii() {
            let mut starts = vec![0];
            let bytes = input.as_bytes();
            let mut index = 0;
            while index < bytes.len() {
                match bytes[index] {
                    b'\n' => {
                        index += 1;
                        starts.push(index);
                    }
                    b'\r' => {
                        index += 1;
                        if bytes.get(index) == Some(&b'\n') {
                            index += 1;
                        }
                        starts.push(index);
                    }
                    _ => index += 1,
                }
            }
            return Self {
                starts,
                content_start: 0,
                len: input.len(),
                ascii: true,
                last_line: Cell::new(0),
            };
        }
        let mut starts = vec![0];
        let mut chars = input.char_indices().peekable();
        while let Some((index, ch)) = chars.next() {
            match ch {
                '\r' => {
                    if let Some((_, '\n')) = chars.peek() {
                        chars.next();
                    }
                    starts.push(chars.peek().map_or(input.len(), |(next, _)| *next));
                }
                '\n' => {
                    starts.push(index + ch.len_utf8());
                }
                _ => {}
            }
        }

        let content_start = if input.starts_with('\u{feff}') { 3 } else { 0 };
        Self {
            starts,
            content_start,
            len: input.len(),
            ascii: input.is_ascii(),
            last_line: Cell::new(0),
        }
    }

    /// Return the marker at `index`, or `None` if it is out of bounds or not a
    /// UTF-8 boundary.
    #[must_use]
    pub(crate) fn marker(&self, input: &str, index: usize) -> Option<Marker> {
        if index > self.len || (!self.ascii && !input.is_char_boundary(index)) {
            return None;
        }
        let mut line = self.last_line.get();
        if index < self.starts[line] {
            line = self
                .starts
                .partition_point(|&start| start <= index)
                .saturating_sub(1);
        } else {
            while line + 1 < self.starts.len() && index >= self.starts[line + 1] {
                line += 1;
            }
        }
        self.last_line.set(line);
        let line_start = self.starts[line];
        let content_start = if line == 0 {
            line_start.max(self.content_start)
        } else {
            line_start
        };
        let column = if index < content_start {
            1
        } else if self.ascii {
            index - content_start + 1
        } else {
            input[content_start..index].chars().count() + 1
        };
        Some(Marker::new(index, line + 1, column))
    }

    /// Return a span for `start..end`, reusing the start line for the common
    /// same-line case.
    #[must_use]
    pub(crate) fn span(&self, input: &str, start: usize, end: usize) -> Option<crate::span::Span> {
        let start_marker = self.marker(input, start)?;
        if end < start {
            return None;
        }
        if end == start {
            return Some(crate::span::Span::empty(start_marker));
        }
        if end > self.len || (!self.ascii && !input.is_char_boundary(end)) {
            return None;
        }
        let line = self.last_line.get();
        let next_start = self.starts.get(line + 1).copied().unwrap_or(self.len);
        if end < next_start || (end == next_start && line + 1 == self.starts.len()) {
            let line_start = self.starts[line];
            let content_start = if line == 0 {
                line_start.max(self.content_start)
            } else {
                line_start
            };
            let column = if end < content_start {
                1
            } else if self.ascii {
                end - content_start + 1
            } else {
                input[content_start..end].chars().count() + 1
            };
            let end_marker = crate::span::Marker::new(end, line + 1, column);
            return Some(crate::span::Span::new(start_marker, end_marker));
        }
        self.marker(input, end)
            .map(|end_marker| crate::span::Span::new(start_marker, end_marker))
    }

    /// Return the number of lines (including a final empty line after a break).
    #[must_use]
    /// Byte offset of the line containing `index`.
    pub(crate) fn line_start(&self, index: usize) -> usize {
        match self.starts.binary_search(&index) {
            Ok(line) => self.starts[line],
            Err(next) => self.starts[next.saturating_sub(1)],
        }
    }

    pub(crate) fn line_count(&self) -> usize {
        self.starts.len()
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;
    use crate::span::Marker;

    #[test]
    fn ascii_lines_and_columns() {
        let input = "a: 1\nb: 2\n";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 0), Some(Marker::new(0, 1, 1)));
        assert_eq!(index.marker(input, 5), Some(Marker::new(5, 2, 1)));
        assert_eq!(index.marker(input, 8), Some(Marker::new(8, 2, 4)));
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn unicode_columns_count_scalars_not_bytes() {
        let input = "é: 1\nkéy: 2\n";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 0), Some(Marker::new(0, 1, 1)));
        assert_eq!(index.marker(input, 2), Some(Marker::new(2, 1, 2)));
        // "kéy" starts at byte 6, after the newline.
        assert_eq!(index.marker(input, 6), Some(Marker::new(6, 2, 1)));
        assert_eq!(index.marker(input, 9), Some(Marker::new(9, 2, 3)));
    }

    #[test]
    fn crlf_is_one_line_break() {
        let input = "a\r\nb\r\n";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 3), Some(Marker::new(3, 2, 1)));
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn nel_ls_ps_are_content() {
        for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
            let input = format!("a{separator}b");
            let index = LineIndex::new(&input);
            let after = 1 + separator.len_utf8();
            assert_eq!(index.marker(&input, after), Some(Marker::new(after, 1, 3)));
        }
    }

    #[test]
    fn leading_bom_is_not_a_column() {
        let input = "\u{feff}k: v\n";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 3), Some(Marker::new(3, 1, 1)));
        assert_eq!(index.marker(input, 4), Some(Marker::new(4, 1, 2)));
    }

    #[test]
    fn non_boundary_is_none() {
        let input = "é";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 1), None);
    }
}
