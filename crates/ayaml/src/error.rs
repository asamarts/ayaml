//! Structured, span-carrying errors.

use std::error::Error as StdError;
use std::fmt;

use crate::span::{Location, Span};

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
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)?;
        if let Some(span) = self.span
            && !self.message.contains("at line ")
        {
            let location = span.start().location();
            write!(
                formatter,
                " at line {} column {}",
                location.line(),
                location.column()
            )?;
        }
        Ok(())
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
    fn categories_display_stably() {
        assert_eq!(ErrorKind::DuplicateKey.to_string(), "duplicate key");
        assert_eq!(ErrorKind::Limit.to_string(), "resource limit");
    }
}
