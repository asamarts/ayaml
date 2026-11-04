//! Byte offsets and one-based source locations.
//!
//! Spans are half-open ranges over the original UTF-8 input. `index` is a byte
//! offset; `line` and `column` are one-based and are supplied by the scanner,
//! not derived by character-counting here.

/// A position in the original input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Marker {
    index: usize,
    line: usize,
    column: usize,
}

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

/// A one-based line and column, suitable for diagnostics and editors.
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

impl From<Marker> for Location {
    fn from(marker: Marker) -> Self {
        marker.location()
    }
}

/// A half-open byte range in the original input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Span {
    start: Marker,
    end: Marker,
}

impl Span {
    /// Create a half-open span.
    #[must_use]
    pub const fn new(start: Marker, end: Marker) -> Self {
        Self { start, end }
    }

    /// A zero-width span at `marker`.
    #[must_use]
    pub const fn empty(marker: Marker) -> Self {
        Self {
            start: marker,
            end: marker,
        }
    }

    /// The start (inclusive) marker.
    #[must_use]
    pub const fn start(self) -> Marker {
        self.start
    }

    /// The end (exclusive) marker.
    #[must_use]
    pub const fn end(self) -> Marker {
        self.end
    }

    /// The number of bytes in the span.
    #[must_use]
    pub const fn len(self) -> usize {
        self.end.index - self.start.index
    }

    /// Whether the span is zero-width.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.len() == 0
    }

    /// Whether `index` is inside the half-open range.
    #[must_use]
    pub const fn contains(self, index: usize) -> bool {
        index >= self.start.index && index < self.end.index
    }

    /// Slice `source` with this span.
    ///
    /// Returns `None` if the span is not on a UTF-8 boundary or is out of
    /// bounds.
    #[must_use]
    pub fn slice(self, source: &str) -> Option<&str> {
        source.get(self.start.index..self.end.index)
    }
}

#[cfg(test)]
mod tests {
    use super::{Location, Marker, Span};

    #[test]
    fn markers_are_one_based_and_byte_indexed() {
        let marker = Marker::new(4, 2, 3);
        assert_eq!(marker.index(), 4);
        assert_eq!(marker.line(), 2);
        assert_eq!(marker.column(), 3);
        assert_eq!(marker.location(), Location::new(2, 3));
    }

    #[test]
    fn spans_are_half_open_and_slice_utf8() {
        let text = "é: 1\n";
        let start = Marker::new(0, 1, 1);
        let end = Marker::new(2, 1, 2);
        let span = Span::new(start, end);
        assert_eq!(span.len(), 2);
        assert_eq!(span.slice(text), Some("é"));
        assert!(!span.contains(2));
        assert!(span.contains(1));
    }

    #[test]
    fn invalid_utf8_boundary_is_none() {
        let text = "é";
        let span = Span::new(Marker::new(0, 1, 1), Marker::new(1, 1, 2));
        assert_eq!(span.slice(text), None);
    }

    #[test]
    fn empty_span_has_zero_length() {
        let span = Span::empty(Marker::new(3, 1, 4));
        assert!(span.is_empty());
        assert_eq!(span.len(), 0);
    }
}
