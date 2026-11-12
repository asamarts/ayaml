//! Byte-offset to line/column mapping for scanner positions.
//!
//! The mapping is built once per input. It understands LF, CR, CRLF, and a
//! UTF-8 BOM at the start of a line, which takes up no column. NEL (U+0085),
//! LS (U+2028), and PS (U+2029) are content in YAML 1.2, not line breaks.

use std::cell::Cell;

use crate::span::Marker;

/// A line-start index over a UTF-8 input.
#[derive(Clone, Debug)]
pub(crate) struct LineIndex {
    starts: Vec<usize>,
    content_start: usize,
    /// Whether a BOM at the start of a later line takes up no column, as one on the first line
    /// does. Compat counts one, as libyaml does.
    skip_line_boms: bool,
    len: usize,
    ascii: bool,
    last_line: Cell<usize>,
    /// The last column counted outside ASCII: its line, byte index, and column. Tokens come in
    /// order, so counting on from it keeps a long line from being read again for each token.
    counted_column: Cell<(usize, usize, usize)>,
}

/// Whether `text` starts with a document marker: `---` or `...` and then a blank, a line break, or
/// its end.
pub(crate) fn starts_with_document_marker(text: &str) -> bool {
    (text.starts_with("---") || text.starts_with("..."))
        && text[3..]
            .chars()
            .next()
            .is_none_or(|ch| matches!(ch, ' ' | '\t' | '\n' | '\r'))
}

/// At most how many lines `bytes` holds: one more than its CR and LF bytes. That is its line count
/// when no CR is followed by an LF. Otherwise it counts each CRLF twice, which stays under twice the
/// line count, since every CRLF ends a line, and under one more than the bytes. NEL, LS, and PS are
/// content, and no byte of their UTF-8 is a CR or LF.
fn lines_at_most(bytes: &[u8]) -> usize {
    // A byte-wide sum over chunks of 240 bytes, which can't overflow, is vectorized. Counting into a
    // `usize` for each byte, or counting a CRLF once, cost as much again as building the index.
    1 + bytes
        .chunks(240)
        .map(|chunk| {
            usize::from(chunk.iter().fold(0_u8, |count, &byte| {
                count + (u8::from(byte == b'\n') | u8::from(byte == b'\r'))
            }))
        })
        .sum::<usize>()
}

/// Whether a non-ASCII code point may appear outside a quoted scalar.
fn is_printable_non_ascii(code: u32) -> bool {
    code == 0x85
        || (0xa0..=0xd7ff).contains(&code)
        || (0xe000..=0xfffd).contains(&code)
        || code >= 0x1_0000
}

/// Build the line starts and, when `CHECK`, the offsets of non-printable characters that only a
/// quoted scalar may hold. The validation shares the line-start scan so a valid input is read once.
fn scan_line_starts<const CHECK: bool>(
    input: &str,
    quoted_only: &mut Vec<usize>,
) -> Result<(Vec<usize>, bool), usize> {
    let bytes = input.as_bytes();
    // Allocated once, at 8 bytes for each CR or LF: growing by doubling would hold up to twice the
    // index, and three times it while a copying `realloc` moves it.
    let mut starts = Vec::with_capacity(lines_at_most(bytes));
    starts.push(0);
    let mut ascii = true;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            if CHECK {
                if byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r') {
                    return Err(index);
                }
                if byte == 0x7f {
                    quoted_only.push(index);
                }
            }
            match byte {
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
            continue;
        }
        ascii = false;
        let Some(ch) = input[index..].chars().next() else {
            break;
        };
        if CHECK && !is_printable_non_ascii(u32::from(ch)) {
            quoted_only.push(index);
        }
        index += ch.len_utf8();
    }
    Ok((starts, ascii))
}

impl LineIndex {
    /// Build an index for `input`.
    #[must_use]
    pub(crate) fn new(input: &str) -> Self {
        let mut quoted_only = Vec::new();
        let Ok((starts, ascii)) = scan_line_starts::<false>(input, &mut quoted_only) else {
            unreachable!("an unchecked scan cannot fail");
        };
        let content_start = if input.starts_with('\u{feff}') { 3 } else { 0 };
        Self {
            starts,
            content_start,
            skip_line_boms: !ascii,
            len: input.len(),
            ascii,
            last_line: Cell::new(0),
            counted_column: Cell::new((usize::MAX, 0, 0)),
        }
    }

    /// Build an index and check the characters that YAML forbids outside a quoted scalar.
    ///
    /// The scan that builds the line starts also collects the quoted-only offsets. `count_line_boms`
    /// applies the compat rule of [`Self::counting_line_boms`].
    pub(crate) fn new_checked(
        input: &str,
        count_line_boms: bool,
    ) -> Result<(Self, Vec<usize>), usize> {
        let mut quoted_only = Vec::new();
        let (starts, ascii) = scan_line_starts::<true>(input, &mut quoted_only)?;
        let content_start = if count_line_boms
            && input
                .strip_prefix('\u{feff}')
                .is_some_and(starts_with_document_marker)
        {
            0
        } else if input.starts_with('\u{feff}') {
            3
        } else {
            0
        };
        Ok((
            Self {
                starts,
                content_start,
                skip_line_boms: !count_line_boms && !ascii,
                len: input.len(),
                ascii,
                last_line: Cell::new(0),
                counted_column: Cell::new((usize::MAX, 0, 0)),
            },
            quoted_only,
        ))
    }

    /// Build an index for compat, where a later line's BOM takes up a column, as libyaml reads it.
    /// The first line's takes up none, as when libyaml detects the encoding itself, unless a
    /// document marker follows it. The scanner reads that marker as text, as when the encoding was
    /// set beforehand, so the rest of the line reads that way too.
    #[must_use]
    pub(crate) fn counting_line_boms(input: &str) -> Self {
        let index = Self::new(input);
        let content_start = if input
            .strip_prefix('\u{feff}')
            .is_some_and(starts_with_document_marker)
        {
            0
        } else {
            index.content_start
        };
        Self {
            skip_line_boms: false,
            content_start,
            ..index
        }
    }

    /// The marker at the end of `input`, as the index that [`Self::new`] builds gives it, or the one
    /// [`Self::counting_line_boms`] builds when `count_line_boms`. It holds no line starts, so an
    /// error at the end of the input doesn't hold a second index beside the scanner's.
    #[must_use]
    pub(crate) fn end_marker(input: &str, count_line_boms: bool) -> Marker {
        let bytes = input.as_bytes();
        let (mut line, mut start, mut at) = (0, 0, 0);
        while at < bytes.len() {
            match bytes[at] {
                b'\n' => {
                    at += 1;
                    line += 1;
                    start = at;
                }
                b'\r' => {
                    at += 1;
                    if bytes.get(at) == Some(&b'\n') {
                        at += 1;
                    }
                    line += 1;
                    start = at;
                }
                _ => at += 1,
            }
        }
        let end = input.len();
        if line == 0 {
            // One line, whose index holds one start, and whose columns may start past a mark.
            let index = if count_line_boms {
                Self::counting_line_boms(input)
            } else {
                Self::new(input)
            };
            return index
                .marker(input, end)
                .unwrap_or_else(|| Marker::new(end, 1, 1));
        }
        let mut last = &input[start..];
        if !count_line_boms {
            while let Some(rest) = last.strip_prefix('\u{feff}') {
                last = rest;
            }
        }
        Marker::new(end, line + 1, last.chars().count() + 1)
    }

    /// The column of `index` on `line`.
    #[inline]
    fn column(&self, input: &str, line: usize, index: usize) -> usize {
        if self.ascii {
            return index - self.starts[line] + 1;
        }
        self.counted_column_of(input, line, index)
    }

    fn counted_column_of(&self, input: &str, line: usize, index: usize) -> usize {
        let (counted_line, counted_index, counted) = self.counted_column.get();
        let column = if counted_line == line && counted_index <= index {
            counted + input[counted_index..index].chars().count()
        } else {
            let start = self.line_content_start(input, line);
            if index < start {
                return 1;
            }
            input[start..index].chars().count() + 1
        };
        self.counted_column.set((line, index, column));
        column
    }

    /// Where a line's columns start.
    fn line_content_start(&self, input: &str, line: usize) -> usize {
        let mut start = self.starts[line];
        if line == 0 {
            start = start.max(self.content_start);
        }
        if self.skip_line_boms {
            while input[start..].starts_with('\u{feff}') {
                start += '\u{feff}'.len_utf8();
            }
        }
        start
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
        Some(Marker::new(
            index,
            line + 1,
            self.column(input, line, index),
        ))
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
            let end_marker = Marker::new(end, line + 1, self.column(input, line, end));
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
    use super::{LineIndex, lines_at_most};
    use crate::span::Marker;

    /// The index is allocated once, at its length when no CR is followed by an LF and under twice
    /// it otherwise, on every text of up to five characters from line breaks, ASCII, and the
    /// characters that are content, and on longer ones that cross the count's chunks.
    #[test]
    fn the_index_is_allocated_once_within_a_bound() {
        let alphabet = ["a", "\n", "\r", "é", "\u{85}", "\u{2028}", "\u{feff}"];
        let mut texts = vec![String::new()];
        let mut checked = 0;
        let mut check = |text: &str| {
            for index in [LineIndex::new(text), LineIndex::counting_line_boms(text)] {
                let (lines, capacity) = (index.line_count(), index.starts.capacity());
                assert_eq!(capacity, lines_at_most(text.as_bytes()), "{text:?}");
                if text.contains("\r\n") {
                    assert!(lines < capacity && capacity < 2 * lines, "{text:?}");
                } else {
                    assert_eq!(capacity, lines, "{text:?}");
                }
                assert!(capacity <= text.len() + 1, "{text:?}");
            }
            checked += 1;
        };
        for _ in 0..5 {
            let mut longer = Vec::new();
            for text in &texts {
                for piece in alphabet {
                    longer.push(format!("{text}{piece}"));
                }
            }
            for text in &longer {
                check(text);
            }
            texts = longer;
        }
        for length in [239, 240, 241, 480, 1000] {
            check(&"\n".repeat(length));
            check(&"\r".repeat(length));
            check(&"\r\n".repeat(length));
            check(&"a\r\n\n\u{2028}".repeat(length));
        }
        assert_eq!(checked, 7 + 49 + 343 + 2401 + 16_807 + 20);
        assert_eq!(lines_at_most(b""), 1);
    }

    /// The end's marker is the one a whole index gives, in either mode, on every text of up to five
    /// characters from line breaks, ASCII, marks, and other characters.
    #[test]
    fn the_end_is_where_an_index_puts_it() {
        let check = |text: &str| {
            for count_line_boms in [false, true] {
                let index = if count_line_boms {
                    LineIndex::counting_line_boms(text)
                } else {
                    LineIndex::new(text)
                };
                assert_eq!(
                    Some(LineIndex::end_marker(text, count_line_boms)),
                    index.marker(text, text.len()),
                    "{text:?}, {count_line_boms}"
                );
            }
        };
        let alphabet = ["a", "\n", "\r", "é", "\u{2028}", "\u{feff}", "-"];
        let mut texts = vec![String::new()];
        check("");
        let mut checked = 1;
        for _ in 0..5 {
            let mut longer = Vec::new();
            for text in &texts {
                for piece in alphabet {
                    longer.push(format!("{text}{piece}"));
                }
            }
            for text in &longer {
                check(text);
            }
            checked += longer.len();
            texts = longer;
        }
        assert_eq!(checked, 19_608);
        for text in [
            "\u{feff}---",
            "\u{feff}--- a",
            "\u{feff}\u{feff}x\n\u{feff}\u{feff}y",
        ] {
            check(text);
        }
    }

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
    fn a_later_lines_bom_is_not_a_column() {
        let input = "a\n\u{feff}\u{feff}k: v\n";
        let index = LineIndex::new(input);
        let marker = index.marker(input, input.find('k').unwrap()).unwrap();
        assert_eq!((marker.line(), marker.column()), (2, 1));
        let index = LineIndex::counting_line_boms(input);
        let marker = index.marker(input, input.find('k').unwrap()).unwrap();
        assert_eq!((marker.line(), marker.column()), (2, 3));
    }

    #[test]
    fn columns_are_right_in_any_order() {
        let input = "é\u{feff}x é y\n\u{feff}\u{feff}é z\n";
        let index = LineIndex::new(input);
        let at = |needle: &str| input.find(needle).unwrap();
        let column = |offset| index.marker(input, offset).unwrap().column();
        // Forward, backward, into the marks at a line's start, and forward again.
        assert_eq!(column(at("x")), 3);
        assert_eq!(column(at("y")), 7);
        assert_eq!(column(at("x")), 3);
        assert_eq!(column(at("z")), 3);
        assert_eq!(column(input.find('\n').unwrap() + 1), 1);
        assert_eq!(column(at("z")), 3);
        assert_eq!(column(at("x")), 3);
    }

    #[test]
    fn non_boundary_is_none() {
        let input = "é";
        let index = LineIndex::new(input);
        assert_eq!(index.marker(input, 1), None);
    }

    #[test]
    fn checked_scan_agrees_with_the_index_and_collects_quoted_only_characters() {
        for input in ["", "a\nb\r\nc", "é\n\u{feff}x\n", "a\u{85}b\u{2028}c"] {
            let index = LineIndex::new(input);
            let (checked, quoted) = LineIndex::new_checked(input, false).unwrap();
            assert_eq!(index.line_count(), checked.line_count(), "{input:?}");
            assert!(quoted.is_empty(), "{input:?}: {quoted:?}");
            for at in (0..=input.len()).filter(|&at| input.is_char_boundary(at)) {
                assert_eq!(
                    index.marker(input, at),
                    checked.marker(input, at),
                    "{input:?}"
                );
            }
        }
        for (input, expected) in [
            ("a\u{7f}b", vec![1]),
            ("a\u{fffe}b", vec![1]),
            ("a\u{1}b", Vec::new()),
        ] {
            let result = LineIndex::new_checked(input, false);
            if expected.is_empty() {
                assert_eq!(result.unwrap_err(), 1, "{input:?}");
            } else {
                assert_eq!(result.unwrap().1, expected, "{input:?}");
            }
        }
        let input = "\u{feff}---\nx\n";
        let normal = LineIndex::new(input);
        let compat = LineIndex::counting_line_boms(input);
        let checked_compat = LineIndex::new_checked(input, true).unwrap().0;
        assert_eq!(normal.marker(input, 3).unwrap().column(), 1);
        assert_eq!(compat.marker(input, 3).unwrap().column(), 2);
        for at in (0..=input.len()).filter(|&at| input.is_char_boundary(at)) {
            assert_eq!(
                compat.marker(input, at),
                checked_compat.marker(input, at),
                "{at}"
            );
        }
    }
}
