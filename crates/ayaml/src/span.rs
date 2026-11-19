//! Byte offsets, zero-based positions, and one-based locations.
//!
//! Spans are half-open byte ranges over the original UTF-8 input. A line and
//! column come from a [`LineIndex`](crate::LineIndex), which can count UTF-8
//! bytes, Unicode scalar values, or UTF-16 code units. A [`Position`] counts
//! from zero, for a machine; a [`Location`] counts from one, for a person.

use std::ops::Range;

/// A byte offset with one-based line and column values.
///
/// This is the pre-byte-span position type. Spans no longer store it; use
/// [`LineIndex`](crate::LineIndex) for positions derived from a span.
#[deprecated(note = "use Span byte offsets with LineIndex, Position, and Location")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Marker {
    index: usize,
    line: usize,
    column: usize,
}

#[allow(deprecated, reason = "this is the compatibility type itself")]
impl Marker {
    /// Create a marker at a byte offset and one-based line/column.
    #[must_use]
    pub const fn new(index: usize, line: usize, column: usize) -> Self {
        Self {
            index,
            line,
            column,
        }
    }

    /// The zero-based byte offset from the start of the input.
    #[must_use]
    pub const fn index(self) -> usize {
        self.index
    }

    /// The one-based line number.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// The one-based column number.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }

    /// The line/column location, without the byte offset.
    #[must_use]
    pub const fn location(self) -> Location {
        Location::new(self.line, self.column)
    }
}

/// A zero-based line and column, as a [`LineIndex`](crate::LineIndex) gives and takes them.
///
/// This is the position for a machine: an editor protocol, a source map, another index. What
/// the column counts is the [`ColumnEncoding`](crate::ColumnEncoding) that was asked for. For a
/// person, convert it to a [`Location`], which counts from one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Position {
    /// The line, counted from zero.
    pub line: usize,
    /// The column on that line, counted from zero.
    pub column: usize,
}

impl Position {
    /// Create a zero-based line/column position.
    #[must_use]
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }
}

/// A one-based line and column, for display: what [`Error::location`](crate::Error::location)
/// returns and what an error prints.
///
/// Its `Default` is line 0, column 0, as in 0.1.x: no place in a text, for a location that
/// isn't known. The first character of a text is at `Location::new(1, 1)`, which is what
/// `Location::from(Position::default())` gives.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Location {
    line: usize,
    column: usize,
}

impl Location {
    /// Create a one-based line/column location.
    #[must_use]
    pub const fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }

    /// The one-based line number.
    #[must_use]
    pub const fn line(self) -> usize {
        self.line
    }

    /// The one-based column number.
    #[must_use]
    pub const fn column(self) -> usize {
        self.column
    }
}

/// The location that a person reads for a position: one more in the line and in the column.
impl From<Position> for Location {
    fn from(position: Position) -> Self {
        Self::new(
            position.line.saturating_add(1),
            position.column.saturating_add(1),
        )
    }
}

#[allow(deprecated, reason = "converts the compatibility marker")]
impl From<Marker> for Location {
    fn from(marker: Marker) -> Self {
        marker.location()
    }
}

/// A half-open byte range in the original input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    start: usize,
    end: usize,
}

impl Span {
    /// Create a half-open span from byte offsets.
    #[must_use]
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    /// A zero-width span at `offset`.
    #[must_use]
    pub const fn empty(offset: usize) -> Self {
        Self {
            start: offset,
            end: offset,
        }
    }

    /// The start (inclusive) byte offset.
    #[must_use]
    pub const fn start(self) -> usize {
        self.start
    }

    /// The end (exclusive) byte offset.
    #[must_use]
    pub const fn end(self) -> usize {
        self.end
    }

    /// The span as a half-open byte range.
    #[must_use]
    pub const fn range(self) -> Range<usize> {
        self.start..self.end
    }

    /// The number of bytes in the span. A span whose end is before its start holds none, as a
    /// `Range` built that way does.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Whether the span is zero-width.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Whether `index` is inside the half-open range.
    #[must_use]
    pub const fn contains(self, index: usize) -> bool {
        index >= self.start && index < self.end
    }

    /// Slice `source` with this span.
    ///
    /// Returns `None` if the span is not on a UTF-8 boundary or is out of
    /// bounds.
    #[must_use]
    pub fn slice(self, source: &str) -> Option<&str> {
        source.get(self.start..self.end)
    }
}

impl From<Span> for Range<usize> {
    fn from(span: Span) -> Self {
        span.range()
    }
}

impl From<Range<usize>> for Span {
    fn from(range: Range<usize>) -> Self {
        Self::new(range.start, range.end)
    }
}

#[cfg(test)]
mod tests {
    use super::{Location, Position, Span};

    #[test]
    fn a_location_is_one_more_than_its_position() {
        let location = Location::new(2, 3);
        assert_eq!((location.line(), location.column()), (2, 3));
        let position = Position::new(1, 2);
        assert_eq!((position.line, position.column), (1, 2));
        assert_eq!(Location::from(position), location);
        assert_eq!(Location::from(Position::default()), Location::new(1, 1));
        // The last position there is has no location past it, and doesn't overflow.
        let last = Location::from(Position::new(usize::MAX, usize::MAX));
        assert_eq!((last.line(), last.column()), (usize::MAX, usize::MAX));
    }

    #[test]
    fn spans_are_half_open_and_slice_utf8() {
        let text = "é: 1\n";
        let span = Span::new(0, 2);
        assert_eq!(span.len(), 2);
        assert_eq!(span.slice(text), Some("é"));
        assert!(!span.contains(2));
        assert!(span.contains(1));
        assert_eq!(span.range(), 0..2);
        assert_eq!(Span::from(0..2), span);
    }

    #[test]
    fn invalid_utf8_boundary_is_none() {
        let text = "é";
        assert_eq!(Span::new(0, 1).slice(text), None);
    }

    #[test]
    fn empty_span_has_zero_length() {
        let span = Span::empty(3);
        assert!(span.is_empty());
        assert_eq!(span.len(), 0);
    }
}
