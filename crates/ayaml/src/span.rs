//! Byte offsets, zero-based positions, and one-based locations.
//!
//! Spans are half-open byte ranges over the original UTF-8 input. A line and
//! column come from a [`LineIndex`](crate::LineIndex), which can count UTF-8
//! bytes, Unicode scalar values, or UTF-16 code units. A [`Position`] counts
//! from zero, for a machine; a [`Location`] counts from one, for a person.

use std::fmt;
use std::marker::PhantomData;
use std::ops::{Deref, Range};

use serde::de::{self, Deserialize, Deserializer, MapAccess, Visitor};
use serde::ser::{Serialize, Serializer};

pub(crate) const SPANNED_TOKEN: &str = "$__ayaml_private_Spanned";
pub(crate) const SPANNED_START_FIELD: &str = "$__ayaml_private_start";
pub(crate) const SPANNED_END_FIELD: &str = "$__ayaml_private_end";
pub(crate) const SPANNED_VALUE_FIELD: &str = "$__ayaml_private_value";
pub(crate) const SPANNED_FIELDS: &[&str] =
    &[SPANNED_START_FIELD, SPANNED_END_FIELD, SPANNED_VALUE_FIELD];

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

/// A deserialized value paired with the exact byte [`Span`] where it appeared in the YAML source.
///
/// When deserialized through `ayaml`'s source-backed typed readers ([`from_str`](crate::from_str),
/// [`from_str_with`](crate::from_str_with), [`from_str_borrowed`](crate::from_str_borrowed),
/// [`from_slice`](crate::from_slice), [`from_reader`](crate::from_reader),
/// [`Deserializer`](crate::Deserializer), [`DocumentDeserializer`](crate::DocumentDeserializer),
/// [`parse_frontmatter`](crate::parse_frontmatter), and
/// `ayaml::compat`), `Spanned<T>` captures the node's byte [`Span`] alongside the decoded value
/// `T` on both the small-tree and event-stream paths.
///
/// # Behavior details
///
/// - **Aliases:** `Spanned<T>` on an alias node records the alias use-site span (e.g. `*anchor`),
///   matching [`SpannedTree`](crate::SpannedTree), while inner `Spanned<U>` fields inside an
///   aliased collection record the spans of the anchored target's entries.
/// - **Tags:** `Spanned<T>` on a tagged node records the node's span, matching
///   [`SpannedTree`](crate::SpannedTree).
/// - **Collections:** `Spanned<Vec<T>>`, structs, and maps record the full collection span from
///   opening token through closing token, matching [`SpannedTree::span_at`](crate::SpannedTree::span_at).
/// - **Borrowed strings:** `Spanned<&'a str>` borrows directly from the YAML source when the
///   underlying scalar is borrowable.
/// - **Non-`ayaml` deserializers and [`from_value`](crate::from_value):** Deserializers that do
///   not carry YAML source spans (such as `serde_json` or [`from_value`](crate::from_value))
///   return a Serde error rather than fabricating a placeholder span.
/// - **Serialization:** [`Serialize`] writes the inner value transparently so structs containing
///   `Spanned<T>` fields round-trip cleanly through [`to_string`](crate::to_string).
///
/// ```
/// use ayaml::Spanned;
/// use serde::Deserialize;
///
/// #[derive(Deserialize)]
/// struct Config<'a> {
///     #[serde(borrow)]
///     name: Spanned<&'a str>,
///     retries: Spanned<u32>,
/// }
///
/// let input = "name: ayaml\nretries: 3\n";
/// let config: Config<'_> = ayaml::from_str_borrowed(input)?;
/// assert_eq!(*config.name, "ayaml");
/// assert_eq!(config.name.span().slice(input), Some("ayaml"));
/// assert_eq!(*config.retries, 3);
/// assert_eq!(config.retries.span().slice(input), Some("3"));
/// # Ok::<(), ayaml::Error>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Spanned<T> {
    span: Span,
    value: T,
}

impl<T> Spanned<T> {
    /// Pair a value with its byte [`Span`].
    #[must_use]
    pub const fn new(span: Span, value: T) -> Self {
        Self { span, value }
    }

    /// The byte [`Span`] of this value in the original YAML input.
    #[must_use]
    pub const fn span(&self) -> Span {
        self.span
    }

    /// Borrow the inner value.
    #[must_use]
    pub const fn get_ref(&self) -> &T {
        &self.value
    }

    /// Mutably borrow the inner value.
    #[must_use]
    pub const fn get_mut(&mut self) -> &mut T {
        &mut self.value
    }

    /// Consume the wrapper and return the inner value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.value
    }

    /// Consume the wrapper and return `(value, span)`.
    #[must_use]
    pub fn into_parts(self) -> (T, Span) {
        (self.value, self.span)
    }
}

impl<T> Deref for Spanned<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.value
    }
}

impl<T> std::ops::DerefMut for Spanned<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.value
    }
}

impl<T> AsRef<T> for Spanned<T> {
    fn as_ref(&self) -> &T {
        &self.value
    }
}

impl<T> AsMut<T> for Spanned<T> {
    fn as_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T: Serialize> Serialize for Spanned<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.value.serialize(serializer)
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Spanned<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct SpannedVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for SpannedVisitor<T> {
            type Value = Spanned<T>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a spanned YAML value from an ayaml source-backed deserializer")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                enum Field {
                    Start,
                    End,
                    Value,
                }

                impl<'de> Deserialize<'de> for Field {
                    fn deserialize<D: Deserializer<'de>>(
                        deserializer: D,
                    ) -> Result<Self, D::Error> {
                        struct FieldVisitor;

                        impl Visitor<'_> for FieldVisitor {
                            type Value = Field;

                            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                                formatter.write_str(
                                    "a spanned YAML value from an ayaml source-backed deserializer",
                                )
                            }

                            fn visit_str<E: de::Error>(
                                self,
                                value: &str,
                            ) -> Result<Self::Value, E> {
                                match value {
                                    SPANNED_START_FIELD => Ok(Field::Start),
                                    SPANNED_END_FIELD => Ok(Field::End),
                                    SPANNED_VALUE_FIELD => Ok(Field::Value),
                                    _ => Err(de::Error::custom(
                                        "Spanned<T> requires an ayaml source-backed deserializer",
                                    )),
                                }
                            }
                        }

                        deserializer.deserialize_identifier(FieldVisitor)
                    }
                }

                let mut start: Option<usize> = None;
                let mut end: Option<usize> = None;
                let mut value: Option<T> = None;
                while let Some(key) = map.next_key::<Field>()? {
                    match key {
                        Field::Start => {
                            if start.is_some() {
                                return Err(de::Error::duplicate_field(SPANNED_START_FIELD));
                            }
                            start = Some(map.next_value()?);
                        }
                        Field::End => {
                            if end.is_some() {
                                return Err(de::Error::duplicate_field(SPANNED_END_FIELD));
                            }
                            end = Some(map.next_value()?);
                        }
                        Field::Value => {
                            if value.is_some() {
                                return Err(de::Error::duplicate_field(SPANNED_VALUE_FIELD));
                            }
                            value = Some(map.next_value()?);
                        }
                    }
                }
                let (Some(start), Some(end), Some(value)) = (start, end, value) else {
                    return Err(de::Error::custom(
                        "Spanned<T> requires an ayaml source-backed deserializer",
                    ));
                };
                Ok(Spanned {
                    span: Span::new(start, end),
                    value,
                })
            }
        }

        deserializer.deserialize_struct(SPANNED_TOKEN, SPANNED_FIELDS, SpannedVisitor(PhantomData))
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

    #[test]
    fn inverted_and_max_spans_are_overflow_free() {
        let inverted = Span::new(10, 4);
        assert_eq!(inverted.len(), 0);
        assert!(inverted.is_empty());
        assert!(!inverted.contains(4));
        assert!(!inverted.contains(7));
        assert!(!inverted.contains(10));
        assert_eq!(inverted.slice("0123456789abcdef"), None);

        let max_span = Span::new(usize::MAX, usize::MAX);
        assert_eq!(max_span.len(), 0);
        assert!(max_span.is_empty());
        assert!(!max_span.contains(usize::MAX));
        assert_eq!(max_span.slice("abc"), None);

        let text = "aé🚀";
        assert_eq!(Span::new(0, 1).slice(text), Some("a"));
        assert_eq!(Span::new(1, 3).slice(text), Some("é"));
        assert_eq!(Span::new(3, 7).slice(text), Some("🚀"));
        assert_eq!(Span::new(3, 5).slice(text), None);
        assert_eq!(Span::new(0, 8).slice(text), None);
    }
}

#[cfg(kani)]
mod kani_proofs {
    use super::{Location, Position, Span};

    #[kani::proof]
    fn verify_span_invariants_and_slice_safety() {
        let start: usize = kani::any();
        let end: usize = kani::any();
        let index: usize = kani::any();
        let span = Span::new(start, end);

        assert_eq!(span.start(), start);
        assert_eq!(span.end(), end);
        assert_eq!(Span::from(span.range()), span);
        assert_eq!(span.is_empty(), start >= end);
        assert_eq!(span.len(), end.saturating_sub(start));
        assert_eq!(span.contains(index), index >= start && index < end);
        if span.is_empty() {
            assert!(!span.contains(index));
        }

        // Fixed UTF-8 sample exercising 1-, 2-, 3-, and 4-byte UTF-8 sequences.
        let sample = "aé€🚀";
        match span.slice(sample) {
            Some(sub) => {
                assert!(start <= end && end <= sample.len());
                assert!(sample.is_char_boundary(start));
                assert!(sample.is_char_boundary(end));
                assert_eq!(sub.len(), span.len());
            }
            None => {
                assert!(
                    start > end
                        || end > sample.len()
                        || !sample.is_char_boundary(start)
                        || !sample.is_char_boundary(end)
                );
            }
        }
    }

    #[kani::proof]
    fn verify_location_from_position_saturates() {
        let line: usize = kani::any();
        let column: usize = kani::any();
        let loc = Location::from(Position::new(line, column));
        assert_eq!(loc.line(), line.saturating_add(1));
        assert_eq!(loc.column(), column.saturating_add(1));
        assert!(loc.line() >= 1 && loc.column() >= 1);
    }
}
