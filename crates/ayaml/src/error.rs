//! Structured, span-carrying errors.

use std::error::Error as StdError;
use std::fmt;

use crate::span::{Location, Span};

/// The longest path a message holds. A longer one keeps its deepest steps, after [`ELIDED`].
const MAX_PATH_BYTES: usize = 1024;
/// The most characters of one key a path shows.
const MAX_KEY_CHARS: usize = 64;
/// What starts a path that lost its outer steps.
const ELIDED: &str = "…";

/// Stable error categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ErrorKind {
    /// Input is not valid UTF-8 or uses an unsupported encoding.
    InvalidEncoding,
    /// The scanner could not produce a token.
    Scanner,
    /// The parser could not build an event.
    Parser,
    /// An alias is unknown, cyclic, or over budget.
    Alias,
    /// A mapping contains a duplicate semantic key.
    DuplicateKey,
    /// A tag is malformed or unsupported.
    Tag,
    /// A scalar could not be resolved under the active profile.
    Resolution,
    /// A configured resource limit was exceeded.
    Limit,
    /// Composition or value-model construction failed.
    Composition,
    /// Serde deserialization or serialization failed.
    Serde,
    /// The emitter failed.
    Emitter,
    /// I/O failed.
    Io,
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InvalidEncoding => "invalid encoding",
            Self::Scanner => "scanner error",
            Self::Parser => "parser error",
            Self::Alias => "alias error",
            Self::DuplicateKey => "duplicate key",
            Self::Tag => "tag error",
            Self::Resolution => "scalar resolution error",
            Self::Limit => "resource limit",
            Self::Composition => "composition error",
            Self::Serde => "serde error",
            Self::Emitter => "emitter error",
            Self::Io => "I/O error",
        };
        formatter.write_str(name)
    }
}

/// An ayaml error with a stable category, optional source span, and, for
/// resource-limit failures, the structured limit that was exceeded.
#[derive(Debug)]
pub struct Error {
    kind: ErrorKind,
    /// How much of `message` is the path to what a typed reader rejected, with the `: ` after it.
    path_len: u32,
    /// How much of the end of `message` is context that the error's location goes before.
    context_len: u32,
    span: Option<Span>,
    message: String,
    limit: Option<Box<crate::limits::LimitExceeded>>,
    source: Option<Box<dyn StdError + Send + Sync + 'static>>,
}

impl Error {
    /// Create an error with no source location.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            path_len: 0,
            context_len: 0,
            span: None,
            message: message.into(),
            limit: None,
            source: None,
        }
    }

    /// Attach a source span.
    #[must_use]
    pub fn with_span(mut self, span: Span) -> Self {
        self.span = Some(span);
        self
    }

    /// End the message with `context`, which follows the location when the error is displayed.
    #[cold]
    pub(crate) fn with_context(mut self, context: &str) -> Self {
        self.message.push_str(context);
        self.context_len = context.len() as u32;
        self
    }

    /// Attach a source error.
    #[must_use]
    pub fn with_source(mut self, source: impl StdError + Send + Sync + 'static) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Attach the structured limit that was exceeded.
    #[must_use]
    pub fn with_limit(mut self, limit: crate::limits::LimitExceeded) -> Self {
        self.limit = Some(Box::new(limit));
        self
    }

    /// The structured resource-limit failure, when this is a limit error.
    #[must_use]
    pub fn limit(&self) -> Option<crate::limits::LimitExceeded> {
        self.limit.as_deref().copied()
    }

    /// The stable category.
    #[must_use]
    pub const fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The source span, if any.
    #[must_use]
    pub const fn span(&self) -> Option<Span> {
        self.span
    }

    /// The one-based line/column, if the error has a span.
    #[must_use]
    pub fn location(&self) -> Option<Location> {
        self.span.map(|span| span.start().location())
    }

    /// The human-readable message without location decoration.
    ///
    /// A flow collection's error ends with where the collection opened, as in
    /// `unterminated flow sequence, opened at line 1 column 1`. That is part of the message, and
    /// the error's own location goes before it when the error is displayed.
    ///
    /// When a typed reader rejects something below the document's root, the message starts with
    /// the path to it, as in `tasks[0].limits.cpu: invalid type: string "x", expected u32`. A
    /// field is its name, an element is its index in brackets, and a key that isn't a short name
    /// made of ASCII letters, digits, `_` and `-` is quoted in brackets, as in `ports["80"]`. A
    /// key that is a collection is `[?]`. An enum's content adds the variant's name. A long key,
    /// or a long path, is cut short with `…`.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Say that this error came from inside element `index` of a sequence.
    #[cold]
    pub(crate) fn inside_index(self, index: usize) -> Self {
        self.inside(&format!("[{index}]"))
    }

    /// Say that this error came from inside the value of the mapping key with this text.
    #[cold]
    pub(crate) fn inside_key(self, key: &str) -> Self {
        if is_short_name(key) {
            self.inside(key)
        } else {
            self.inside(&quoted_key(key))
        }
    }

    /// Say that this error came from inside the value of a key that is a collection.
    #[cold]
    pub(crate) fn inside_collection_key(self) -> Self {
        self.inside("[?]")
    }

    /// Put one step in front of the path. Errors travel outwards, so the path grows from its
    /// deepest step, and only the step that reads a value adds to it.
    fn inside(mut self, step: &str) -> Self {
        let path_len = self.path_len as usize;
        if path_len == 0 {
            self.message.insert_str(0, ": ");
            self.message.insert_str(0, step);
            self.path_len = (step.len() + 2) as u32;
            return self;
        }
        if self.message.starts_with(ELIDED) {
            return self;
        }
        let separator = if self.message.starts_with('[') {
            ""
        } else {
            "."
        };
        let step = if path_len + step.len() + separator.len() > MAX_PATH_BYTES {
            ELIDED
        } else {
            step
        };
        self.message.insert_str(0, separator);
        self.message.insert_str(0, step);
        self.path_len = (path_len + step.len() + separator.len()) as u32;
        self
    }
}

/// Whether a key can stand in a path as it is, with no doubt about where it ends.
fn is_short_name(key: &str) -> bool {
    let mut chars = key.chars();
    key.len() <= MAX_KEY_CHARS
        && chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|next| next.is_ascii_alphanumeric() || next == '_' || next == '-')
}

/// A key in brackets and quotes, with anything that could change how a terminal shows it escaped.
fn quoted_key(key: &str) -> String {
    let mut quoted = String::from("[\"");
    let mut chars = key.chars();
    for next in chars.by_ref().take(MAX_KEY_CHARS) {
        match next {
            '"' | '\\' => {
                quoted.push('\\');
                quoted.push(next);
            }
            _ if hides_text(next) => {
                quoted.push_str(&format!("\\u{{{:x}}}", u32::from(next)));
            }
            _ => quoted.push(next),
        }
    }
    if chars.next().is_some() {
        quoted.push_str(ELIDED);
    }
    quoted.push_str("\"]");
    quoted
}

/// Control characters, line separators, the byte order mark, and the marks that reorder text.
fn hides_text(character: char) -> bool {
    character.is_control()
        || matches!(
            character,
            '\u{61c}'
                | '\u{200e}'
                | '\u{200f}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2066}'..='\u{2069}'
                | '\u{feff}'
        )
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(span) = self.span else {
            return formatter.write_str(&self.message);
        };
        let (problem, context) = self
            .message
            .split_at(self.message.len() - self.context_len as usize);
        let location = span.start().location();
        write!(
            formatter,
            "{problem} at line {} column {}{context}",
            location.line(),
            location.column()
        )
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn StdError + 'static))
    }
}

impl serde::de::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::new(ErrorKind::Serde, message.to_string())
    }
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::new(ErrorKind::Serde, message.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, ErrorKind};
    use crate::span::{Marker, Span};
    use std::error::Error as _;

    #[test]
    fn kind_and_message_are_exposed() {
        let error = Error::new(ErrorKind::Parser, "unexpected token");
        assert_eq!(error.kind(), ErrorKind::Parser);
        assert_eq!(error.message(), "unexpected token");
        assert_eq!(error.to_string(), "unexpected token");
        assert!(error.span().is_none());
        assert!(error.location().is_none());
    }

    #[test]
    fn span_and_location_are_one_based() {
        let span = Span::new(Marker::new(4, 2, 3), Marker::new(5, 2, 4));
        let error = Error::new(ErrorKind::Scanner, "bad token").with_span(span);
        assert_eq!(error.span(), Some(span));
        let location = error.location().expect("location");
        assert_eq!((location.line(), location.column()), (2, 3));
    }

    #[test]
    fn source_is_chained() {
        let inner = std::io::Error::other("inner");
        let error = Error::new(ErrorKind::Io, "outer").with_source(inner);
        assert!(error.source().is_some());
    }

    #[test]
    fn a_path_joins_its_steps_and_stands_before_the_message() {
        let error = Error::new(ErrorKind::Serde, "bad")
            .inside_key("c")
            .inside_index(2)
            .inside_key("b")
            .inside_key("a b")
            .inside_index(0);
        assert_eq!(error.message(), "[0][\"a b\"].b[2].c: bad");
        assert_eq!(error.kind(), ErrorKind::Serde);
        let error = Error::new(ErrorKind::Serde, "bad").inside_collection_key();
        assert_eq!(error.message(), "[?]: bad");
    }

    #[test]
    fn a_key_is_bare_only_if_it_is_a_short_name() {
        for bare in ["a", "_a", "runs-on", "a_1", &"k".repeat(64)] {
            let error = Error::new(ErrorKind::Serde, "bad").inside_key(bare);
            assert_eq!(error.message(), format!("{bare}: bad"));
        }
        for (key, shown) in [
            ("", "[\"\"]"),
            ("80", "[\"80\"]"),
            ("-a", "[\"-a\"]"),
            ("a.b", "[\"a.b\"]"),
            ("名前", "[\"名前\"]"),
            ("a\"b\\c", "[\"a\\\"b\\\\c\"]"),
            ("a\nb\u{1b}", "[\"a\\u{a}b\\u{1b}\"]"),
            ("\u{85}\u{2028}\u{2029}", "[\"\\u{85}\\u{2028}\\u{2029}\"]"),
            ("\u{200e}\u{200f}", "[\"\\u{200e}\\u{200f}\"]"),
            ("a\u{61c}b", "[\"a\\u{61c}b\"]"),
            (
                "\u{202e}\u{2066}\u{feff}",
                "[\"\\u{202e}\\u{2066}\\u{feff}\"]",
            ),
        ] {
            let error = Error::new(ErrorKind::Serde, "bad").inside_key(key);
            assert_eq!(error.message(), format!("{shown}: bad"));
        }
    }

    #[test]
    fn a_long_key_and_a_long_path_are_cut_short() {
        let error = Error::new(ErrorKind::Serde, "bad").inside_key(&"é".repeat(65));
        assert_eq!(error.message(), format!("[\"{}…\"]: bad", "é".repeat(64)));

        let mut error = Error::new(ErrorKind::Serde, "bad");
        let name = "k".repeat(64);
        for _ in 0..100 {
            error = error.inside_key(&name);
        }
        let (path, rest) = error.message().split_once(": ").expect("a path");
        assert_eq!(rest, "bad");
        assert!(path.starts_with("….k"), "{path}");
        assert!(path.len() <= 1024 + 4, "{}", path.len());
        let before = error.message().to_owned();
        let error = error.inside_key("outer").inside_index(1);
        assert_eq!(error.message(), before);

        let mut error = Error::new(ErrorKind::Serde, "bad").inside_index(7);
        for _ in 0..200 {
            error = error.inside_index(1234567);
        }
        assert!(error.message().starts_with("…["), "{}", error.message());
    }

    /// Text that reads like a location, in a key or in the message, never stands for the error's.
    #[test]
    fn a_location_in_the_text_does_not_stand_for_the_error_s() {
        let span = Span::new(Marker::new(4, 2, 3), Marker::new(5, 2, 4));
        let error = Error::new(ErrorKind::Serde, "bad")
            .with_span(span)
            .inside_key("at line 9");
        assert_eq!(error.to_string(), "[\"at line 9\"]: bad at line 2 column 3");
        let error = Error::new(ErrorKind::Serde, "invalid type: string \"at line 9\"")
            .with_span(span)
            .inside_key("a");
        assert_eq!(
            error.to_string(),
            "a: invalid type: string \"at line 9\" at line 2 column 3"
        );
    }

    #[test]
    fn the_location_goes_before_the_context() {
        let span = Span::new(Marker::new(4, 2, 3), Marker::new(5, 2, 4));
        let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
            .with_context(", opened at line 1 column 1")
            .with_span(span);
        assert_eq!(
            error.message(),
            "unterminated flow sequence, opened at line 1 column 1"
        );
        assert_eq!(
            error.to_string(),
            "unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
        );
        let error = error.inside_index(0).inside_key("k");
        assert_eq!(
            error.to_string(),
            "k[0]: unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
        );
        // With no location, the context follows the problem.
        let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
            .with_context(", opened at line 1 column 1");
        assert_eq!(
            error.to_string(),
            "unterminated flow sequence, opened at line 1 column 1"
        );
    }

    #[test]
    fn categories_display_stably() {
        assert_eq!(ErrorKind::DuplicateKey.to_string(), "duplicate key");
        assert_eq!(ErrorKind::Limit.to_string(), "resource limit");
    }
}
