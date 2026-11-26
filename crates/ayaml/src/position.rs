//! Byte-offset to line/column mapping: zero-allocation error-path coordinates and the public
//! [`LineIndex`].
//!
//! Coordinates understand LF, CR, CRLF, and a UTF-8 BOM at the start of a line; in default mode
//! the BOM takes up no column, while compatibility mode counts one on a later line. NEL
//! (U+0085), LS (U+2028), and PS (U+2029) are content in YAML 1.2, not line breaks.
//!
//! [`LineIndex`] is the same mapping for a caller, in the names and rules that the format
//! contract gives the positions layer: `to_position`, `to_offset`, and `line_count`, with
//! zero-based positions.

#[cfg(test)]
use std::cell::Cell;
use std::fmt;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use crate::span::Span;
use crate::span::{Location, Position};

/// The unit that a [`LineIndex`] column counts. A later release may add a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColumnEncoding {
    /// UTF-8 bytes.
    Utf8,
    /// Unicode scalar values, one per `char`.
    Scalar,
    /// UTF-16 code units, as an editor's LSP positions use.
    Utf16,
}

/// Why a byte offset or a position cannot be converted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PositionError {
    /// The offset is past the end of the input, the line does not exist, or the column is past
    /// the end of its line.
    OutOfBounds,
    /// The offset is inside a UTF-8 character. A column in Unicode scalar values or in UTF-16
    /// code units can only describe an offset at a character boundary.
    InvalidUtf8Boundary,
    /// The UTF-16 column falls between the two code units of a surrogate pair, inside a
    /// character such as an emoji.
    SplitSurrogate,
    /// The offset is between the CR and the LF of one CRLF line ending.
    InsideCrLf,
}

impl fmt::Display for PositionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OutOfBounds => "source position is out of bounds",
            Self::InvalidUtf8Boundary => "byte offset is not on a UTF-8 character boundary",
            Self::SplitSurrogate => "UTF-16 column splits a surrogate pair",
            Self::InsideCrLf => "byte offset lies inside a CRLF line ending",
        })
    }
}

impl std::error::Error for PositionError {}

/// The readers' line-start index over a UTF-8 input, kept in tests as a reference oracle against
/// the zero-allocation coordinate helpers.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct LineStarts {
    starts: Arc<Vec<usize>>,
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

/// Whether an ASCII byte is unconditionally forbidden anywhere in YAML (C0 control characters
/// other than `\t`, `\n`, and `\r`).
#[inline]
pub(crate) const fn is_forbidden_control_byte(byte: u8) -> bool {
    byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r')
}

/// Whether `bytes[pos..]` starts with a non-printable character that YAML allows only inside
/// quoted scalars: DEL (`0x7F`), C1 controls (`U+0080..=U+0084`, `U+0086..=U+009F`, excluding
/// printable NEL `U+0085`), and noncharacters `U+FFFE` / `U+FFFF`.
#[inline]
pub(crate) fn is_quoted_only_non_printable(bytes: &[u8], pos: usize) -> bool {
    match bytes[pos] {
        0x7f => true,
        0xC2 => matches!(bytes.get(pos + 1), Some(0x80..=0x84 | 0x86..=0x9f)),
        0xEF => {
            bytes.get(pos + 1) == Some(&0xBF) && matches!(bytes.get(pos + 2), Some(0xBE | 0xBF))
        }
        _ => false,
    }
}

/// Whether `bytes` contains any YAML 1.2 line break (`\n` or `\r`).
#[inline]
pub(crate) fn has_line_break(bytes: &[u8]) -> bool {
    bytes.iter().any(|&byte| byte == b'\n' || byte == b'\r')
}

/// Byte offset where the line containing `offset` begins, handling LF, CR, and CRLF.
#[inline]
pub(crate) fn line_start_of(bytes: &[u8], offset: usize) -> usize {
    let mut i = offset.min(bytes.len());
    // An offset between `\r` and `\n` of one CRLF belongs to the line before the CRLF.
    if i > 0 && bytes[i - 1] == b'\r' && bytes.get(i) == Some(&b'\n') {
        i -= 1;
    }
    while i > 0 {
        let byte = bytes[i - 1];
        if byte == b'\n' || byte == b'\r' {
            return i;
        }
        i -= 1;
    }
    0
}

/// Byte offset where columns begin on the line starting at `line_start`.
#[inline]
fn line_content_start_at(input: &str, line_start: usize, count_line_boms: bool) -> usize {
    if line_start == 0 {
        if count_line_boms {
            compat_content_start(input)
        } else {
            input.len() - input.trim_start_matches('\u{feff}').len()
        }
    } else if !count_line_boms {
        let mut start = line_start;
        while input[start..].starts_with('\u{feff}') {
            start += '\u{feff}'.len_utf8();
        }
        start
    } else {
        line_start
    }
}

/// One-based Unicode scalar column of `offset` on its line, without allocating a line index.
#[inline]
pub(crate) fn column_on_line(input: &str, offset: usize, count_line_boms: bool) -> usize {
    let line_start = line_start_of(input.as_bytes(), offset);
    let content_start = line_content_start_at(input, line_start, count_line_boms);
    if offset <= content_start {
        return 1;
    }
    let slice = &input[content_start..offset];
    if slice.is_ascii() {
        slice.len() + 1
    } else {
        slice.chars().count() + 1
    }
}

/// One-based line and Unicode scalar column of `offset` in `input`, computed in a single
/// zero-allocation scan (intended for cold diagnostic and error paths).
#[cold]
pub(crate) fn location_at(input: &str, offset: usize, count_line_boms: bool) -> Option<Location> {
    if offset > input.len() || !input.is_char_boundary(offset) {
        return None;
    }
    let bytes = input.as_bytes();
    let mut line = 1_usize;
    let mut line_start = 0_usize;
    let mut i = 0_usize;
    while i < offset {
        match bytes[i] {
            b'\n' => {
                i += 1;
                line += 1;
                line_start = i;
            }
            b'\r' => {
                if bytes.get(i + 1) == Some(&b'\n') {
                    if i + 2 <= offset {
                        i += 2;
                        line += 1;
                        line_start = i;
                    } else {
                        // `offset` sits between `\r` and `\n`: still on the line before CRLF.
                        break;
                    }
                } else {
                    i += 1;
                    line += 1;
                    line_start = i;
                }
            }
            _ => i += 1,
        }
    }
    let content_start = line_content_start_at(input, line_start, count_line_boms);
    let column = if offset <= content_start {
        1
    } else {
        let slice = &input[content_start..offset];
        if slice.is_ascii() {
            slice.len() + 1
        } else {
            slice.chars().count() + 1
        }
    };
    Some(Location::new(line, column))
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

/// Where the first line's columns start in compat. A stream-start run of marks takes up no column,
/// as when libyaml detects the encoding itself, unless a document marker follows it. The scanner
/// reads that marker as text, as when the encoding was set beforehand, so the rest of the line
/// reads that way too: the run counts as the one column a single mark would.
fn compat_content_start(input: &str) -> usize {
    let marks = input.len() - input.trim_start_matches('\u{feff}').len();
    if marks > 0 && starts_with_document_marker(&input[marks..]) {
        marks - '\u{feff}'.len_utf8()
    } else {
        marks
    }
}

/// At most how many lines `bytes` holds: one more than its CR and LF bytes. That is its line count
/// when no CR is followed by an LF. Otherwise it counts each CRLF twice, which stays under twice the
/// line count, since every CRLF ends a line, and under one more than the bytes. NEL, LS, and PS are
/// content, and no byte of their UTF-8 is a CR or LF.
pub(crate) fn lines_at_most(bytes: &[u8]) -> usize {
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
#[cfg(any(test, kani))]
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
    const LINE_SPECIAL: [bool; 256] = {
        let mut table = [false; 256];
        let mut i = 0;
        while i < 0x20 {
            table[i] = true;
            i += 1;
        }
        table[b'\t' as usize] = false;
        table[0x7f] = true;
        let mut j = 0x80;
        while j < 256 {
            table[j] = true;
            j += 1;
        }
        table
    };
    let bytes = input.as_bytes();
    // Allocated once, at 8 bytes for each CR or LF: growing by doubling would hold up to twice the
    // index, and three times it while a copying `realloc` moves it.
    let mut starts = Vec::with_capacity(lines_at_most(bytes));
    starts.push(0);
    let mut ascii = true;
    let mut index = 0;
    while index < bytes.len() {
        while index < bytes.len() && !LINE_SPECIAL[bytes[index] as usize] {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        let byte = bytes[index];
        if byte < 0x80 {
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
                0x7f => {
                    if CHECK {
                        quoted_only.push(index);
                    }
                    index += 1;
                }
                _ => {
                    if CHECK {
                        return Err(index);
                    }
                    index += 1;
                }
            }
            continue;
        }
        ascii = false;
        let Some(ch) = input[index..].chars().next() else {
            break;
        };
        if CHECK && is_quoted_only_non_printable(bytes, index) {
            quoted_only.push(index);
        }
        index += ch.len_utf8();
    }
    Ok((starts, ascii))
}

#[cfg(test)]
impl LineStarts {
    /// Build an index for `input`.
    #[must_use]
    pub(crate) fn new(input: &str) -> Self {
        let mut quoted_only = Vec::new();
        let Ok((starts, ascii)) = scan_line_starts::<false>(input, &mut quoted_only) else {
            unreachable!("an unchecked scan cannot fail");
        };
        let content_start = input.len() - input.trim_start_matches('\u{feff}').len();
        Self {
            starts: Arc::new(starts),
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
        let content_start = if count_line_boms {
            compat_content_start(input)
        } else {
            input.len() - input.trim_start_matches('\u{feff}').len()
        };
        Ok((
            Self {
                starts: Arc::new(starts),
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
    /// The first line's marks take up none, unless a document marker follows them
    /// ([`compat_content_start`]).
    #[must_use]
    pub(crate) fn counting_line_boms(input: &str) -> Self {
        Self {
            skip_line_boms: false,
            content_start: compat_content_start(input),
            ..Self::new(input)
        }
    }

    /// The marker at the end of `input`, as the index that [`Self::new`] builds gives it, or the one
    /// [`Self::counting_line_boms`] builds when `count_line_boms`. It holds no line starts, so an
    /// error at the end of the input doesn't hold a second index beside the scanner's.
    #[must_use]
    pub(crate) fn end_marker(input: &str, _count_line_boms: bool) -> usize {
        input.len()
    }

    /// The column of `index` on `line`, in the requested encoding.
    fn column(&self, input: &str, line: usize, index: usize, encoding: ColumnEncoding) -> usize {
        let start = self.line_content_start(input, line);
        if index <= start {
            return 1;
        }
        if self.ascii {
            return index - start + 1;
        }
        match encoding {
            ColumnEncoding::Utf8 => index - start + 1,
            ColumnEncoding::Scalar => self.counted_column_of(input, line, index),
            ColumnEncoding::Utf16 => input[start..index].encode_utf16().count() + 1,
        }
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

    /// Return the one-based line and column at `index`, as a reader places an error.
    ///
    /// `encoding` selects whether a column counts UTF-8 bytes, Unicode scalar values, or UTF-16
    /// code units. A leading byte order mark is excluded from the column and included in the byte
    /// offset. An offset outside the input or inside a UTF-8 character is an error.
    pub(crate) fn location(
        &self,
        input: &str,
        index: usize,
        encoding: ColumnEncoding,
    ) -> Result<Location, PositionError> {
        if index > self.len {
            return Err(PositionError::OutOfBounds);
        }
        if !self.ascii && !input.is_char_boundary(index) {
            return Err(PositionError::InvalidUtf8Boundary);
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
        Ok(Location::new(
            line + 1,
            self.column(input, line, index, encoding),
        ))
    }

    /// Return the scalar position at `index`.
    #[must_use]
    pub(crate) fn marker(&self, input: &str, index: usize) -> Option<Location> {
        self.location(input, index, ColumnEncoding::Scalar).ok()
    }

    /// Return a byte span for `start..end`.
    #[inline]
    #[must_use]
    pub(crate) fn span(&self, input: &str, start: usize, end: usize) -> Option<Span> {
        if start > end || end > self.len {
            return None;
        }
        if !(self.ascii || (input.is_char_boundary(start) && input.is_char_boundary(end))) {
            return None;
        }
        Some(Span::new(start, end))
    }

    /// Byte offset of the line containing `index`. An index past the end of the input is on the
    /// last line.
    #[inline]
    #[must_use]
    pub(crate) fn line_start(&self, index: usize) -> usize {
        let line = lookup_line_index(&self.starts, self.last_line.get(), index);
        self.last_line.set(line);
        self.starts[line]
    }

    /// The number of lines, including a final empty line after a break.
    #[must_use]
    pub(crate) fn line_count(&self) -> usize {
        self.starts.len()
    }
}

/// Pure cached-cursor line lookup over strictly increasing `starts` (beginning with `0`).
///
/// Checks `last_line` and `last_line + 1` in O(1) for sequential token scans before falling back
/// to binary search (`partition_point`).
#[cfg(any(test, kani))]
#[inline]
fn lookup_line_index(starts: &[usize], last_line: usize, index: usize) -> usize {
    if index >= starts[last_line] {
        if last_line + 1 >= starts.len() || index < starts[last_line + 1] {
            return last_line;
        }
        if last_line + 2 >= starts.len() || index < starts[last_line + 2] {
            return last_line + 1;
        }
    }
    starts
        .partition_point(|&start| start <= index)
        .saturating_sub(1)
}

/// A line index over one input, for converting between byte offsets and positions.
///
/// Lines end at LF, at CRLF, and at a CR alone. NEL (U+0085), LS (U+2028), and PS (U+2029) are
/// content, as YAML 1.2 has them. A [`Position`] counts lines and columns from zero, and a
/// column counts what the [`ColumnEncoding`] says: UTF-8 bytes, Unicode scalar values, or UTF-16
/// code units. Byte offsets count every byte of the input, a leading byte-order mark included.
///
/// Columns leave out one byte-order mark that opens the input, as an editor does, and an offset
/// inside that mark is at column 0 of the first line. Every other mark is a character of its
/// line, and counts as one column: a second mark at the start, and one that begins a later
/// line.
///
/// A reader places an error where the index puts the start of its span, counted from one:
/// `e.location()` is
/// `index.to_position(e.span()?.start(), ColumnEncoding::Scalar).map(Location::from)`. On a
/// line that begins with a byte-order mark the columns can differ, since the readers count
/// such marks in their own ways and the index as an editor does:
///
/// - a default reader leaves every mark that begins a line out of that line's columns, where
///   the index leaves out only the one that opens the input;
/// - a compatibility reader leaves out the whole run of marks that opens the input, and counts
///   one of them where a document marker follows the run, as libyaml does. A mark that begins
///   a later line is a column to it, as to the index.
///
/// Building the index reads the input once and keeps eight bytes a line. A conversion finds its
/// line by binary search. A column in UTF-8 bytes is then a subtraction, and one in scalar
/// values or in UTF-16 code units reads the line up to the offset, so its cost grows with the
/// length of the line.
///
/// ```
/// use ayaml::{ColumnEncoding, LineIndex, Location, Position};
///
/// let input = "a: 1\nname: \u{1f600}!\n";
/// let index = LineIndex::new(input);
/// let offset = input.find('!').unwrap();
///
/// assert_eq!(index.line_count(), 3);
/// assert_eq!(index.to_position(offset, ColumnEncoding::Utf8)?, Position::new(1, 10));
/// assert_eq!(index.to_position(offset, ColumnEncoding::Scalar)?, Position::new(1, 7));
/// assert_eq!(index.to_position(offset, ColumnEncoding::Utf16)?, Position::new(1, 8));
/// assert_eq!(index.to_offset(Position::new(1, 8), ColumnEncoding::Utf16)?, offset);
/// // What a person reads counts from one.
/// let shown = Location::from(index.to_position(offset, ColumnEncoding::Scalar)?);
/// assert_eq!((shown.line(), shown.column()), (2, 8));
/// # Ok::<(), ayaml::PositionError>(())
/// ```
#[derive(Clone, Debug)]
pub struct LineIndex<'a> {
    input: &'a str,
    /// The offset at which each line starts. The first is 0.
    starts: Vec<usize>,
    /// Where the first line's columns start: after one mark that opens the input.
    first_column: usize,
}

impl<'a> LineIndex<'a> {
    /// Build an index over `input`, borrowing it.
    #[must_use]
    pub fn new(input: &'a str) -> Self {
        let mut quoted_only = Vec::new();
        let Ok((mut starts, _)) = scan_line_starts::<false>(input, &mut quoted_only) else {
            unreachable!("an unchecked scan cannot fail");
        };
        // The scan reserves for the breaks it may meet, and a CRLF is two of them for one line.
        starts.shrink_to_fit();
        Self {
            input,
            starts,
            first_column: if input.starts_with('\u{feff}') {
                '\u{feff}'.len_utf8()
            } else {
                0
            },
        }
    }

    /// The number of lines, counting the empty line after a final line ending: `"a\n"` has
    /// two, `"a"` has one, and `""` has one.
    #[must_use]
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Where the columns of `line` start: at its first byte, or after the mark that opens the
    /// input.
    fn column_start(&self, line: usize) -> usize {
        if line == 0 {
            self.first_column
        } else {
            self.starts[line]
        }
    }

    /// Where the content of `line` ends: at its line ending's first byte, or at the input's end.
    fn line_end(&self, line: usize) -> usize {
        let Some(&next) = self.starts.get(line + 1) else {
            return self.input.len();
        };
        let bytes = self.input.as_bytes();
        if next >= 2 && bytes[next - 1] == b'\n' && bytes[next - 2] == b'\r' {
            next - 2
        } else {
            next - 1
        }
    }

    /// Convert a byte offset to a zero-based line and column.
    ///
    /// With [`ColumnEncoding::Utf8`] every offset on a line has a column, one inside a
    /// multi-byte character too. [`ColumnEncoding::Scalar`] and [`ColumnEncoding::Utf16`] need
    /// the offset at a character boundary. The offset of a line ending's first byte is the
    /// column after the line's last character, and the offset at the end of the input is a
    /// position too.
    ///
    /// # Errors
    ///
    /// - [`PositionError::OutOfBounds`] past the end of the input.
    /// - [`PositionError::InsideCrLf`] between the CR and the LF of one line ending.
    /// - [`PositionError::InvalidUtf8Boundary`] inside a character, for a column in scalar
    ///   values or in UTF-16 code units.
    pub fn to_position(
        &self,
        offset: usize,
        encoding: ColumnEncoding,
    ) -> Result<Position, PositionError> {
        if offset > self.input.len() {
            return Err(PositionError::OutOfBounds);
        }
        // A line starts at 0, so there is always one at or before the offset.
        let line = self.starts.partition_point(|&start| start <= offset) - 1;
        if offset > self.line_end(line) {
            return Err(PositionError::InsideCrLf);
        }
        let start = self.column_start(line);
        if offset <= start {
            return Ok(Position::new(line, 0));
        }
        let column = match encoding {
            ColumnEncoding::Utf8 => offset - start,
            ColumnEncoding::Scalar | ColumnEncoding::Utf16 => {
                let Some(before) = self.input.get(start..offset) else {
                    return Err(PositionError::InvalidUtf8Boundary);
                };
                // The standard count of characters reads whole words of bytes at a time.
                // Under Kani, use `characters_begun` (separately proven equivalent to
                // `chars().count()` in `verify_utf8_block_counters`) to avoid unwinding stdlib's
                // pointer-aligned SWAR loop.
                #[cfg(not(kani))]
                let scalars = before.chars().count();
                #[cfg(kani)]
                let scalars = characters_begun(before.as_bytes());
                if encoding == ColumnEncoding::Utf16 {
                    scalars + four_byte_characters(before.as_bytes())
                } else {
                    scalars
                }
            }
        };
        Ok(Position::new(line, column))
    }

    /// Convert a zero-based line and column to a byte offset.
    ///
    /// The column can be at most the end of the line's content: a line ending is not more
    /// columns. With [`ColumnEncoding::Utf8`] a column can point inside a multi-byte character,
    /// and that byte's offset is returned. Column 0 of the first line is the byte after the mark
    /// that opens the input, if one does.
    ///
    /// # Errors
    ///
    /// - [`PositionError::OutOfBounds`] when the line does not exist or the column is past the
    ///   end of the line's content.
    /// - [`PositionError::SplitSurrogate`] when a UTF-16 column falls between the two code
    ///   units of a surrogate pair.
    pub fn to_offset(
        &self,
        position: Position,
        encoding: ColumnEncoding,
    ) -> Result<usize, PositionError> {
        if position.line >= self.starts.len() {
            return Err(PositionError::OutOfBounds);
        }
        let start = self.column_start(position.line);
        let end = self.line_end(position.line);
        let Some(content) = self.input.get(start..end) else {
            return Err(PositionError::OutOfBounds);
        };
        if position.column > content.len() {
            return Err(PositionError::OutOfBounds);
        }
        if encoding == ColumnEncoding::Utf8 {
            return Ok(start + position.column);
        }
        let utf16 = encoding == ColumnEncoding::Utf16;
        let bytes = content.as_bytes();
        // Whole blocks that end before the column are passed by counting them, without
        // decoding a character.
        let (mut at, mut column) = (0, 0);
        while let Some(block) = bytes.get(at..at + BLOCK) {
            let counted = characters_begun(block)
                + if utf16 {
                    four_byte_characters(block)
                } else {
                    0
                };
            if column + counted > position.column {
                break;
            }
            column += counted;
            at += BLOCK;
        }
        // A block can end inside a character, which it counted by that character's first byte.
        while bytes.get(at).is_some_and(|&byte| is_continuation(byte)) {
            at += 1;
        }
        let Some(tail) = content.get(at..) else {
            return Err(PositionError::OutOfBounds);
        };
        for (offset, character) in tail.char_indices() {
            if column == position.column {
                return Ok(start + at + offset);
            }
            column += if utf16 { character.len_utf16() } else { 1 };
            if column > position.column {
                return Err(PositionError::SplitSurrogate);
            }
        }
        if column == position.column {
            Ok(end)
        } else {
            Err(PositionError::OutOfBounds)
        }
    }
}

/// How many bytes `to_offset` counts at a time before it decodes characters.
const BLOCK: usize = 256;

/// Whether a byte continues a character that an earlier byte began.
const fn is_continuation(byte: u8) -> bool {
    byte & 0xC0 == 0x80
}

/// How many characters begin in `bytes`, which are those of a `str` and may begin or end inside
/// a character: each byte that continues none begins one. The sum can't overflow, and is
/// written so that no build checks each step of it.
fn characters_begun(bytes: &[u8]) -> usize {
    bytes.iter().fold(0_usize, |count, &byte| {
        count.wrapping_add(usize::from(!is_continuation(byte)))
    })
}

/// How many characters of four bytes begin in `bytes`: each takes two UTF-16 code units, and
/// begins with a byte of `0xF0` or more.
fn four_byte_characters(bytes: &[u8]) -> usize {
    bytes.iter().fold(0_usize, |count, &byte| {
        count.wrapping_add(usize::from(byte >= 0xF0))
    })
}

#[cfg(test)]
mod tests {
    use super::{LineStarts, lines_at_most};
    use crate::span::Location;

    /// The index is allocated once, at its length when no CR is followed by an LF and under twice
    /// it otherwise, on every text of up to five characters from line breaks, ASCII, and the
    /// characters that are content, and on longer ones that cross the count's chunks.
    #[test]
    fn the_index_is_allocated_once_within_a_bound() {
        let alphabet = ["a", "\n", "\r", "é", "\u{85}", "\u{2028}", "\u{feff}"];
        let mut texts = vec![String::new()];
        let mut checked = 0;
        let mut check = |text: &str| {
            for index in [LineStarts::new(text), LineStarts::counting_line_boms(text)] {
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

    /// The end is the input's byte length in either mode, so an error at the end needs no second
    /// index.
    #[test]
    fn the_end_is_the_input_length() {
        let check = |text: &str| {
            for count_line_boms in [false, true] {
                assert_eq!(
                    LineStarts::end_marker(text, count_line_boms),
                    text.len(),
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
        let index = LineStarts::new(input);
        assert_eq!(index.marker(input, 0), Some(Location::new(1, 1)));
        assert_eq!(index.marker(input, 5), Some(Location::new(2, 1)));
        assert_eq!(index.marker(input, 8), Some(Location::new(2, 4)));
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn unicode_columns_count_scalars_not_bytes() {
        let input = "é: 1\nkéy: 2\n";
        let index = LineStarts::new(input);
        assert_eq!(index.marker(input, 0), Some(Location::new(1, 1)));
        assert_eq!(index.marker(input, 2), Some(Location::new(1, 2)));
        // "kéy" starts at byte 6, after the newline.
        assert_eq!(index.marker(input, 6), Some(Location::new(2, 1)));
        assert_eq!(index.marker(input, 9), Some(Location::new(2, 3)));
    }

    #[test]
    fn crlf_is_one_line_break() {
        let input = "a\r\nb\r\n";
        let index = LineStarts::new(input);
        assert_eq!(index.marker(input, 3), Some(Location::new(2, 1)));
        assert_eq!(index.line_count(), 3);
    }

    #[test]
    fn nel_ls_ps_are_content() {
        for separator in ['\u{85}', '\u{2028}', '\u{2029}'] {
            let input = format!("a{separator}b");
            let index = LineStarts::new(&input);
            let after = 1 + separator.len_utf8();
            assert_eq!(index.marker(&input, after), Some(Location::new(1, 3)));
        }
    }

    #[test]
    fn leading_bom_is_not_a_column() {
        let input = "\u{feff}k: v\n";
        let index = LineStarts::new(input);
        assert_eq!(index.marker(input, 3), Some(Location::new(1, 1)));
        assert_eq!(index.marker(input, 4), Some(Location::new(1, 2)));
    }

    #[test]
    fn a_later_lines_bom_is_not_a_column() {
        let input = "a\n\u{feff}\u{feff}k: v\n";
        let index = LineStarts::new(input);
        let marker = index.marker(input, input.find('k').unwrap()).unwrap();
        assert_eq!((marker.line(), marker.column()), (2, 1));
        let index = LineStarts::counting_line_boms(input);
        let marker = index.marker(input, input.find('k').unwrap()).unwrap();
        assert_eq!((marker.line(), marker.column()), (2, 3));
    }

    #[test]
    fn columns_are_right_in_any_order() {
        let input = "é\u{feff}x é y\n\u{feff}\u{feff}é z\n";
        let index = LineStarts::new(input);
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
        let index = LineStarts::new(input);
        assert_eq!(index.marker(input, 1), None);
    }

    #[test]
    fn checked_scan_agrees_with_the_index_and_collects_quoted_only_characters() {
        for input in ["", "a\nb\r\nc", "é\n\u{feff}x\n", "a\u{85}b\u{2028}c"] {
            let index = LineStarts::new(input);
            let (checked, quoted) = LineStarts::new_checked(input, false).unwrap();
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
            let result = LineStarts::new_checked(input, false);
            if expected.is_empty() {
                assert_eq!(result.unwrap_err(), 1, "{input:?}");
            } else {
                assert_eq!(result.unwrap().1, expected, "{input:?}");
            }
        }
        let input = "\u{feff}---\nx\n";
        let normal = LineStarts::new(input);
        let compat = LineStarts::counting_line_boms(input);
        let checked_compat = LineStarts::new_checked(input, true).unwrap().0;
        assert_eq!(normal.marker(input, 3).unwrap().column(), 1);
        assert_eq!(compat.marker(input, 3).unwrap().column(), 2);
        // A run of marks before a marker is the one column that a single mark is, and no column
        // where no marker follows it.
        for marks in [2, 3] {
            let start = marks * '\u{feff}'.len_utf8();
            let run = "\u{feff}".repeat(marks);
            let input = format!("{run}---\nx\n");
            for index in [
                LineStarts::counting_line_boms(&input),
                LineStarts::new_checked(&input, true).unwrap().0,
            ] {
                assert_eq!(
                    index.marker(&input, start).unwrap().column(),
                    2,
                    "{input:?}"
                );
                assert_eq!(
                    index.marker(&input, start + 4).unwrap().column(),
                    1,
                    "{input:?}"
                );
            }
            assert_eq!(
                LineStarts::new(&input)
                    .marker(&input, start)
                    .unwrap()
                    .column(),
                1
            );
            let input = format!("{run}a\n");
            let index = LineStarts::counting_line_boms(&input);
            assert_eq!(
                index.marker(&input, start).unwrap().column(),
                1,
                "{input:?}"
            );
        }
        for at in (0..=input.len()).filter(|&at| input.is_char_boundary(at)) {
            assert_eq!(
                compat.marker(input, at),
                checked_compat.marker(input, at),
                "{at}"
            );
        }
    }

    #[test]
    fn line_index_and_block_counters_cover_unicode_and_error_boundaries() {
        use super::{
            ColumnEncoding, LineIndex, Position, PositionError, characters_begun,
            four_byte_characters,
        };

        let sample = "\u{feff}aé€🚀\r\nb\nc";
        assert_eq!(characters_begun(sample.as_bytes()), sample.chars().count());
        assert_eq!(
            characters_begun(sample.as_bytes()) + four_byte_characters(sample.as_bytes()),
            sample.encode_utf16().count()
        );

        let idx = LineIndex::new(sample);
        assert_eq!(idx.line_count(), 3);
        let cr = sample.find('\r').unwrap();
        let rocket = sample.find('🚀').unwrap();
        for encoding in [
            ColumnEncoding::Utf8,
            ColumnEncoding::Scalar,
            ColumnEncoding::Utf16,
        ] {
            assert_eq!(idx.to_position(0, encoding), Ok(Position::new(0, 0)));
            assert_eq!(idx.to_offset(Position::new(0, 0), encoding), Ok(3));
            assert_eq!(
                idx.to_position(cr + 1, encoding),
                Err(PositionError::InsideCrLf)
            );
            assert_eq!(
                idx.to_position(sample.len() + 1, encoding),
                Err(PositionError::OutOfBounds)
            );
            assert_eq!(
                idx.to_offset(Position::new(3, 0), encoding),
                Err(PositionError::OutOfBounds)
            );
        }
        assert_eq!(
            idx.to_position(rocket + 1, ColumnEncoding::Scalar),
            Err(PositionError::InvalidUtf8Boundary)
        );
        assert_eq!(
            idx.to_position(rocket + 1, ColumnEncoding::Utf16),
            Err(PositionError::InvalidUtf8Boundary)
        );
        let rocket_utf16 = idx.to_position(rocket, ColumnEncoding::Utf16).unwrap();
        assert_eq!(
            idx.to_offset(
                Position::new(rocket_utf16.line, rocket_utf16.column + 1),
                ColumnEncoding::Utf16
            ),
            Err(PositionError::SplitSurrogate)
        );
    }

    #[test]
    fn zero_alloc_position_helpers_match_line_starts_oracle() {
        use super::{
            column_on_line, is_forbidden_control_byte, is_printable_non_ascii,
            is_quoted_only_non_printable, line_start_of, location_at,
        };

        // Exhaustive check over every Unicode scalar value for non-printable classification.
        for cp in 0..=0x10_FFFF_u32 {
            let Some(ch) = char::from_u32(cp) else {
                continue;
            };
            let mut buf = [0_u8; 4];
            let encoded = ch.encode_utf8(&mut buf).as_bytes();
            if cp < 0x80 {
                let byte = encoded[0];
                let expected_forbidden = byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r');
                assert_eq!(
                    is_forbidden_control_byte(byte),
                    expected_forbidden,
                    "U+{cp:04X}"
                );
                assert_eq!(
                    is_quoted_only_non_printable(encoded, 0),
                    byte == 0x7f,
                    "U+{cp:04X}"
                );
            } else {
                assert!(!is_forbidden_control_byte(encoded[0]), "U+{cp:04X}");
                assert_eq!(
                    is_quoted_only_non_printable(encoded, 0),
                    !is_printable_non_ascii(cp),
                    "U+{cp:04X}"
                );
            }
        }

        // Exact agreement with `LineStarts` across ASCII, UTF-8, CR, LF, CRLF, and BOM lines.
        let samples = [
            "",
            "a",
            "a\nb\rc\r\nd\n",
            "\r\n\r\n",
            "é: 1\nkéy: 🚀\r\nend",
            "\u{feff}k: v\n\u{feff}\u{feff}x: 1\r\n\u{feff}y",
            "\u{feff}---\nx\n",
            "\u{feff}\u{feff}--- a\n\u{feff}b",
            "\u{feff}\u{feff}a\n",
            "a\u{85}b\u{2028}c\u{2029}d",
        ];
        for input in samples {
            for count_line_boms in [false, true] {
                let oracle = if count_line_boms {
                    LineStarts::counting_line_boms(input)
                } else {
                    LineStarts::new(input)
                };
                for offset in 0..=input.len() + 1 {
                    let expected = oracle.marker(input, offset);
                    let actual = location_at(input, offset, count_line_boms);
                    assert_eq!(
                        actual, expected,
                        "location_at mismatch: input={input:?} offset={offset} compat={count_line_boms}"
                    );
                    if offset <= input.len() && input.is_char_boundary(offset) {
                        assert_eq!(
                            line_start_of(input.as_bytes(), offset),
                            oracle.line_start(offset),
                            "line_start_of mismatch: input={input:?} offset={offset}"
                        );
                        assert_eq!(
                            column_on_line(input, offset, count_line_boms),
                            expected.expect("valid boundary").column(),
                            "column_on_line mismatch: input={input:?} offset={offset} compat={count_line_boms}"
                        );
                    }
                }
            }
        }
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::{
        ColumnEncoding, LineIndex, Position, PositionError, characters_begun, four_byte_characters,
        lines_at_most,
    };

    #[kani::proof]
    #[kani::unwind(10)]
    fn verify_lines_at_most_bounds() {
        let buf: [u8; 8] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= buf.len());
        let slice = &buf[..len];

        let mut breaks = 0_usize;
        let mut i = 0;
        while i < len {
            if slice[i] == b'\n' || slice[i] == b'\r' {
                breaks += 1;
            }
            i += 1;
        }
        let est = lines_at_most(slice);
        assert_eq!(est, 1 + breaks);
        assert!(est >= 1 && est <= len + 1);
    }

    #[kani::proof]
    #[kani::unwind(18)]
    fn verify_utf8_block_counters() {
        // Symbolically check ALL valid Unicode scalar values across 1..=4 UTF-8 byte widths.
        let mut buf = [0_u8; 16];
        let mut byte_len = 0_usize;
        let mut expected_scalars = 0_usize;
        let mut expected_utf16 = 0_usize;

        let count: usize = kani::any();
        kani::assume(count <= 4);
        let mut i = 0;
        while i < count {
            let ch: char = kani::any();
            let encoded = ch.encode_utf8(&mut buf[byte_len..]);
            byte_len += encoded.len();
            expected_scalars += 1;
            expected_utf16 += ch.len_utf16();
            i += 1;
        }

        let slice = &buf[..byte_len];
        let scalars = characters_begun(slice);
        let utf16 = scalars + four_byte_characters(slice);
        assert_eq!(scalars, expected_scalars);
        assert_eq!(utf16, expected_utf16);
    }

    #[kani::proof]
    #[kani::unwind(10)]
    fn verify_line_index_to_position_and_to_offset_roundtrip() {
        // Exact byte layout of "\u{feff}aé🚀\r\nb" (len 13):
        //   0..3 = BOM, 3 = 'a', 4..6 = 'é', 6..10 = '🚀', 10 = '\r', 11 = '\n', 12 = 'b', 13 = EOF
        let sample = "\u{feff}aé🚀\r\nb";
        let index = LineIndex {
            input: sample,
            starts: vec![0, 12],
            first_column: 3,
        };
        let encoding = match kani::any::<u8>() % 3 {
            0 => ColumnEncoding::Utf8,
            1 => ColumnEncoding::Scalar,
            _ => ColumnEncoding::Utf16,
        };
        let offset: usize = kani::any();
        kani::assume(offset <= 15);

        let actual = index.to_position(offset, encoding);
        let expected = match offset {
            0..=3 => Ok(Position::new(0, 0)),
            4 => Ok(Position::new(0, 1)),
            5 => match encoding {
                ColumnEncoding::Utf8 => Ok(Position::new(0, 2)),
                ColumnEncoding::Scalar | ColumnEncoding::Utf16 => {
                    Err(PositionError::InvalidUtf8Boundary)
                }
            },
            6 => Ok(Position::new(
                0,
                match encoding {
                    ColumnEncoding::Utf8 => 3,
                    ColumnEncoding::Scalar | ColumnEncoding::Utf16 => 2,
                },
            )),
            7..=9 => match encoding {
                ColumnEncoding::Utf8 => Ok(Position::new(0, offset - 3)),
                ColumnEncoding::Scalar | ColumnEncoding::Utf16 => {
                    Err(PositionError::InvalidUtf8Boundary)
                }
            },
            10 => Ok(Position::new(
                0,
                match encoding {
                    ColumnEncoding::Utf8 => 7,
                    ColumnEncoding::Scalar => 3,
                    ColumnEncoding::Utf16 => 4,
                },
            )),
            11 => Err(PositionError::InsideCrLf),
            12 => Ok(Position::new(1, 0)),
            13 => Ok(Position::new(1, 1)),
            _ => Err(PositionError::OutOfBounds),
        };
        assert_eq!(actual, expected);
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn verify_line_index_to_offset_bounds_and_surrogates() {
        let sample = "\u{feff}aé🚀\r\nb";
        let index = LineIndex {
            input: sample,
            starts: vec![0, 12],
            first_column: 3,
        };
        let encoding = match kani::any::<u8>() % 3 {
            0 => ColumnEncoding::Utf8,
            1 => ColumnEncoding::Scalar,
            _ => ColumnEncoding::Utf16,
        };
        let col: usize = kani::any();
        kani::assume(col <= 8);

        // Concrete outer loop over lines 0..=2 keeps slice start/end concrete per iteration so
        // CBMC constant-folds `bytes.len() < BLOCK`, cutting peak solver memory dramatically
        // while still covering every `(line, col, encoding)` triple exhaustively.
        for line in 0..=2_usize {
            let actual = index.to_offset(Position::new(line, col), encoding);
            let expected = match (line, encoding, col) {
                (0, ColumnEncoding::Utf8, 0..=7) => Ok(3 + col),
                (0, ColumnEncoding::Scalar, 0) => Ok(3),
                (0, ColumnEncoding::Scalar, 1) => Ok(4),
                (0, ColumnEncoding::Scalar, 2) => Ok(6),
                (0, ColumnEncoding::Scalar, 3) => Ok(10),
                (0, ColumnEncoding::Utf16, 0) => Ok(3),
                (0, ColumnEncoding::Utf16, 1) => Ok(4),
                (0, ColumnEncoding::Utf16, 2) => Ok(6),
                (0, ColumnEncoding::Utf16, 3) => Err(PositionError::SplitSurrogate),
                (0, ColumnEncoding::Utf16, 4) => Ok(10),
                (1, _, 0) => Ok(12),
                (1, _, 1) => Ok(13),
                _ => Err(PositionError::OutOfBounds),
            };
            assert_eq!(actual, expected);
        }
    }

    #[kani::proof]
    #[kani::unwind(6)]
    fn verify_line_starts_cursor_cache_matches_binary_search() {
        let s1: usize = kani::any();
        let s2: usize = kani::any();
        let s3: usize = kani::any();
        kani::assume(0 < s1 && s1 < s2 && s2 < s3);
        let starts = [0_usize, s1, s2, s3];

        let init_cursor: usize = kani::any();
        kani::assume(init_cursor < starts.len());

        let idx1: usize = kani::any();
        let line1 = super::lookup_line_index(&starts, init_cursor, idx1);
        let expected1 = starts
            .partition_point(|&start| start <= idx1)
            .saturating_sub(1);
        assert_eq!(line1, expected1);
        assert!(line1 < starts.len());
        assert!(starts[line1] <= idx1);
        assert!(line1 + 1 == starts.len() || idx1 < starts[line1 + 1]);

        // Sequential lookup reusing the updated cursor matches binary search for any second offset.
        let idx2: usize = kani::any();
        let line2 = super::lookup_line_index(&starts, line1, idx2);
        let expected2 = starts
            .partition_point(|&start| start <= idx2)
            .saturating_sub(1);
        assert_eq!(line2, expected2);
        assert!(line2 < starts.len());
        assert!(starts[line2] <= idx2);
        assert!(line2 + 1 == starts.len() || idx2 < starts[line2 + 1]);
    }

    #[kani::proof]
    fn verify_non_printable_utf8_classification_matches_scalar_oracle() {
        let ch: char = kani::any();
        let mut buf = [0_u8; 4];
        let encoded = ch.encode_utf8(&mut buf).as_bytes();
        let cp = ch as u32;
        if cp < 0x80 {
            assert_eq!(
                super::is_forbidden_control_byte(encoded[0]),
                cp < 0x20 && cp != 0x09 && cp != 0x0a && cp != 0x0d
            );
            assert_eq!(super::is_quoted_only_non_printable(encoded, 0), cp == 0x7f);
        } else {
            assert!(!super::is_forbidden_control_byte(encoded[0]));
            assert_eq!(
                super::is_quoted_only_non_printable(encoded, 0),
                !super::is_printable_non_ascii(cp)
            );
        }
    }
}
