//! Spanned parser events.

use std::borrow::Cow;

use crate::span::Span;
use crate::tag::Tag;

/// A scalar presentation style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ScalarStyle {
    /// Unquoted plain scalar.
    Plain,
    /// Single-quoted scalar.
    SingleQuoted,
    /// Double-quoted scalar with escape processing.
    DoubleQuoted,
    /// Literal block scalar (`|`).
    Literal,
    /// Folded block scalar (`>`).
    Folded,
}

/// A collection presentation style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CollectionStyle {
    /// Block collection.
    Block,
    /// Flow collection (`[...]` or `{...}`).
    Flow,
}

/// An anchor name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Anchor<'a>(Cow<'a, str>);

impl<'a> Anchor<'a> {
    /// Create a borrowed or owned anchor name.
    #[must_use]
    pub fn new(name: impl Into<Cow<'a, str>>) -> Self {
        Self(name.into())
    }

    /// Create an owned anchor name.
    #[must_use]
    pub fn owned(name: String) -> Anchor<'static> {
        Anchor(Cow::Owned(name))
    }

    /// The anchor name without the leading `&`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A parser event with byte spans and presentation metadata.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Event<'a> {
    /// Start of a document stream.
    StreamStart {
        /// Span of the stream start.
        span: Span,
    },
    /// End of a document stream.
    StreamEnd {
        /// Span of the stream end.
        span: Span,
    },
    /// Start of one document.
    DocumentStart {
        /// Whether an explicit `---` marker introduced the document.
        explicit: bool,
        /// Span of the document start.
        span: Span,
    },
    /// End of one document.
    DocumentEnd {
        /// Whether an explicit `...` marker ended the document.
        explicit: bool,
        /// Span of the document end.
        span: Span,
    },
    /// A scalar node.
    Scalar {
        /// Decoded scalar text.
        value: Cow<'a, str>,
        /// Scalar text as written, including quotes or block indicators.
        raw: Cow<'a, str>,
        /// Presentation style.
        style: ScalarStyle,
        /// Explicit tag, if any.
        tag: Option<Tag>,
        /// Anchor, if any.
        anchor: Option<Anchor<'a>>,
        /// Source span.
        span: Span,
    },
    /// Start of a sequence.
    SequenceStart {
        /// Explicit tag, if any.
        tag: Option<Tag>,
        /// Anchor, if any.
        anchor: Option<Anchor<'a>>,
        /// Block or flow style.
        style: CollectionStyle,
        /// Source span.
        span: Span,
    },
    /// End of a sequence.
    SequenceEnd {
        /// Span of the closing token.
        span: Span,
    },
    /// Start of a mapping.
    MappingStart {
        /// Explicit tag, if any.
        tag: Option<Tag>,
        /// Anchor, if any.
        anchor: Option<Anchor<'a>>,
        /// Block or flow style.
        style: CollectionStyle,
        /// Source span.
        span: Span,
    },
    /// End of a mapping.
    MappingEnd {
        /// Span of the closing token.
        span: Span,
    },
    /// A `%YAML` or `%TAG` directive.
    Directive {
        /// Directive name.
        name: Cow<'a, str>,
        /// Directive value.
        value: Cow<'a, str>,
        /// Source span.
        span: Span,
    },
    /// An alias use.
    Alias {
        /// Referenced anchor.
        anchor: Anchor<'a>,
        /// Span of the alias use.
        span: Span,
    },
}

impl Event<'_> {
    /// The event's source span.
    #[must_use]
    pub const fn span(&self) -> Span {
        match self {
            Self::StreamStart { span }
            | Self::StreamEnd { span }
            | Self::DocumentStart { span, .. }
            | Self::DocumentEnd { span, .. }
            | Self::Scalar { span, .. }
            | Self::SequenceStart { span, .. }
            | Self::SequenceEnd { span }
            | Self::MappingStart { span, .. }
            | Self::MappingEnd { span }
            | Self::Directive { span, .. }
            | Self::Alias { span, .. } => *span,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Anchor, CollectionStyle, Event, ScalarStyle};
    use crate::span::{Marker, Span};
    use crate::tag::Tag;

    fn span(start: usize, end: usize) -> Span {
        Span::new(
            Marker::new(start, 1, start + 1),
            Marker::new(end, 1, end + 1),
        )
    }

    #[test]
    fn scalar_event_exposes_raw_presentation_and_decoded_value() {
        let event = Event::Scalar {
            value: "a b".into(),
            raw: "\"a b\"".into(),
            style: ScalarStyle::DoubleQuoted,
            tag: Some(Tag::local("Custom")),
            anchor: Some(Anchor::new("base")),
            span: span(5, 10),
        };
        assert_eq!(event.span(), span(5, 10));
        assert!(matches!(
            event,
            Event::Scalar {
                style: ScalarStyle::DoubleQuoted,
                ..
            }
        ));
    }

    #[test]
    fn collection_events_carry_style_and_anchor() {
        let event = Event::SequenceStart {
            tag: None,
            anchor: Some(Anchor::new("items")),
            style: CollectionStyle::Flow,
            span: span(3, 4),
        };
        assert_eq!(event.span(), span(3, 4));
    }

    #[test]
    fn alias_event_carries_its_use_site() {
        let event = Event::Alias {
            anchor: Anchor::new("base"),
            span: span(8, 13),
        };
        assert_eq!(event.span(), span(8, 13));
        assert!(matches!(event, Event::Alias { .. }));
    }
}
