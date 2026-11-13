#![cfg_attr(docsrs, feature(doc_cfg))]

//! ayaml is a safe, pure-Rust YAML 1.2.2 parser, event stream, value model,
//! Serde implementation, and emitter.
//!
//! One scanner and grammar back every surface, so the spanned event stream,
//! generic values, typed Serde decoding, and emitted output use the same
//! syntax, resolution, and resource limits.
//!
//! # Quick start
//!
//! Parse a document into the generic value model:
//!
//! ```
//! use ayaml::{parse_value, to_string};
//!
//! let value = parse_value("name: example\nitems:\n  - one\n  - two\n")?;
//! assert_eq!(value["name"].as_str(), Some("example"));
//!
//! let output = to_string(&value)?;
//! assert!(output.contains("name: example"));
//! # Ok::<(), ayaml::Error>(())
//! ```
//!
//! Decode into a typed value:
//!
//! ```
//! use serde::Deserialize;
//!
//! #[derive(Debug, Deserialize)]
//! struct Config {
//!     name: String,
//!     retries: u32,
//! }
//!
//! let config: Config = ayaml::from_str("name: example\nretries: 3\n")?;
//! assert_eq!(config.retries, 3);
//! # Ok::<(), ayaml::Error>(())
//! ```
//!
//! Walk the event stream when source spans matter:
//!
//! ```
//! use ayaml::Event;
//!
//! let events: Vec<Event<'_>> = ayaml::parse_events("a: 1\n")?
//!     .collect::<Result<_, _>>()?;
//! assert!(events.iter().any(|event| matches!(event, Event::MappingStart { .. })));
//! # Ok::<(), ayaml::Error>(())
//! ```
//!
//! Configure limits with the `with_*` builders:
//!
//! ```
//! use ayaml::{parse_value_with_limits, Limits};
//!
//! let limits = Limits::default().with_max_depth(64);
//! let value = parse_value_with_limits("a: [1, 2, 3]\n", &limits)?;
//! assert_eq!(value["a"][1].as_u64(), Some(2));
//! # Ok::<(), ayaml::Error>(())
//! ```
//!
//! # Compatibility
//!
//! The default parser follows YAML 1.2.2. The additive `libyaml-compat`
//! feature exposes `ayaml::compat` for callers that need libyaml-compatible
//! behavior; it never changes the root functions. The deliberate deviations
//! are listed in the workspace's
//! `tests/data/yaml-test-suite-deviations.txt` file.
//!
//! # Features
//!
//! - `libyaml-compat`: adds `ayaml::compat` and its decoding entry points.
//! - `bench-internals`: unstable hooks for ayaml's benchmark and conformance
//!   harnesses. This module is hidden and outside `SemVer`.
//!
//! # Resource limits
//!
//! Every entry point has a `_with_limits` form, and [`Limits`] is accepted by
//! the document cursor, emitters, and typed decoders. Exceeding a limit
//! returns [`ErrorKind::Limit`]; [`Error::limit`] exposes the structured
//! [`LimitExceeded`] value. See the `docs/limits.md` file for the defaults and
//! the alias-budget formula.
//!
//! # Errors and spans
//!
//! [`Error`] has a stable category, an optional byte span, an optional source
//! error, and a human-readable message. Input-related errors carry a span and
//! [`Error::location`] returns the one-based line and column. Events carry
//! their own spans for source-aware tooling.
//!
//! # Minimum supported Rust version
//!
//! Rust 1.88. The crate is dual-licensed under MIT or Apache-2.0.
#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![warn(rustdoc::all)]

#[allow(
    dead_code,
    reason = "the conformance and benchmark layers use these hooks only under their features"
)]
mod composer;
mod emitter;
mod event_emitter;
mod merge;
#[allow(
    dead_code,
    reason = "the conformance and benchmark layers use these hooks only under their features"
)]
mod parser;
#[allow(
    dead_code,
    reason = "the conformance and benchmark layers use these hooks only under their features"
)]
mod position;
#[allow(
    dead_code,
    reason = "the conformance and benchmark layers use these hooks only under their features"
)]
mod resolver;
#[allow(
    dead_code,
    reason = "the conformance and benchmark layers use these hooks only under their features"
)]
mod scanner;
mod serde_impl;
mod typed;

pub mod error;
pub mod event;
pub mod limits;
pub mod mapping;
pub mod number;
pub mod span;
pub mod tag;
pub mod value;

pub use error::{Error, ErrorKind};
pub use event::{Anchor, CollectionStyle, Event, ScalarStyle};
pub use limits::{
    ALIAS_BYTES_PER_INPUT_BYTE, COMPAT_MAX_DOCUMENTS, DEFAULT_MAX_DOCUMENTS, LimitExceeded,
    LimitKind, Limits, MAX_ALIAS_EXPANSION_BYTES, MAX_DEPTH, MAX_DEPTH_CAP, MAX_INPUT_BYTES,
    MAX_TAG_EXPANSION_BYTES, NODE_COST_BYTES,
};
pub use mapping::{Entry, Mapping};
pub use number::Number;
pub use span::{Location, Marker, Span};
pub use tag::{Tag, TaggedValue};
pub use value::Value;

pub use emitter::{
    Chomping, Emitter, EmitterOptions, EmitterStyles, NodeStyle, PathSegment, to_string,
    to_string_documents, to_string_documents_with_limits, to_string_value,
    to_string_value_with_limits, to_string_value_with_limits_and_styles,
    to_string_value_with_options, to_string_value_with_options_and_styles,
    to_string_value_with_styles, to_string_with_limits, to_string_with_options, to_writer,
    to_writer_documents, to_writer_documents_with_limits, to_writer_value,
    to_writer_value_with_limits, to_writer_value_with_limits_and_styles,
    to_writer_value_with_options, to_writer_value_with_options_and_styles,
    to_writer_value_with_styles, to_writer_with_limits, to_writer_with_options,
};
pub use event_emitter::{
    to_string_events, to_string_events_with_limits, to_writer_event_stream,
    to_writer_event_stream_with_limits, to_writer_events,
};
pub use serde_impl::{from_value, from_value_seed, to_value};

/// Libyaml-compatible parsing and decoding.
///
/// This module is additive: enabling [`libyaml-compat`](crate#features) never
/// changes the root parsing functions. It reproduces libyaml-compatible
/// behavior for callers that need it, including scanner quirks that the
/// default YAML 1.2.2 parser deliberately does not adopt.
#[cfg(feature = "libyaml-compat")]
#[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
pub mod compat {
    use crate::error::{Code, Error, ErrorKind};
    use crate::limits::LimitKind;
    use serde::de::Visitor;

    /// Translate ayaml diagnostics into the strings expected by
    /// libyaml-compatible consumers. Each branch is a documented compatibility
    /// deviation keyed by the parser's structured failure, never by the text of
    /// the input or of the message.
    fn map_error(error: Error) -> Error {
        if error.code() == Code::ReservedIndicatorAfterBom {
            return Error::new(
                ErrorKind::Scanner,
                "found character that cannot start any token",
            );
        }
        if error
            .limit()
            .is_some_and(|limit| limit.kind() == LimitKind::Depth)
        {
            let mapped = Error::new(ErrorKind::Serde, "recursion limit exceeded");
            return if let Some(span) = error.span() {
                mapped.with_span(span)
            } else {
                mapped
            };
        }
        if error.code() == Code::UnexpectedMappingValue {
            let mapped = Error::new(
                ErrorKind::Parser,
                "mapping values are not allowed in this context",
            );
            return if let Some(span) = error.span() {
                mapped.with_span(span)
            } else {
                mapped
            };
        }
        if error.code() == Code::TrailingDocuments
            || error
                .limit()
                .is_some_and(|limit| limit.kind() == LimitKind::Documents)
        {
            let mapped = Error::new(ErrorKind::Parser, "more than one document in stream");
            return if let Some(span) = error.span() {
                mapped.with_span(span)
            } else {
                mapped
            };
        }
        error
    }

    /// Deserialize one document with a caller-supplied seed under the
    /// compatibility policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_seed<'de, T: serde::de::DeserializeSeed<'de>>(
        input: &'de str,
        seed: T,
    ) -> Result<T::Value, Error> {
        from_str_seed_with_limits(input, &crate::Limits::default(), seed)
    }

    /// Deserialize one document with a caller-supplied seed and explicit limits
    /// under the compatibility policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_seed_with_limits<'de, T: serde::de::DeserializeSeed<'de>>(
        input: &'de str,
        limits: &crate::Limits,
        seed: T,
    ) -> Result<T::Value, Error> {
        crate::typed::from_str_seed_with_options(
            input,
            limits,
            crate::typed::NodeOptions {
                allow_duplicates: true,
                compat_flow: true,
            },
            seed,
        )
        .map_err(map_error)
    }

    /// Deserialize a typed value with compatibility policies, using borrowed
    /// scalar text where possible.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str<'de, T: serde::Deserialize<'de>>(input: &'de str) -> Result<T, Error> {
        from_str_with_limits(input, &crate::Limits::default())
    }

    /// Deserialize a typed value with compatibility policies and explicit limits,
    /// using borrowed scalar text where possible.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_with_limits<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &crate::Limits,
    ) -> Result<T, Error> {
        crate::typed::from_str_with_options(
            input,
            limits,
            crate::typed::NodeOptions {
                allow_duplicates: true,
                compat_flow: true,
            },
        )
        .map_err(map_error)
    }

    /// Parse a whole stream into events with the libyaml-compatible scanner.
    ///
    /// This is the event stream behind the compatibility deserializers; the
    /// conformance runner uses it to check compat-mode deviations.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn parse_events_with_limits<'a>(
        input: &'a str,
        limits: &crate::Limits,
    ) -> Result<Vec<crate::Event<'a>>, Error> {
        crate::parser::parse_events_compat(input, limits)
    }

    /// Deserialize one document with a visitor and compatibility policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn deserialize_any<'de, V: Visitor<'de>>(
        input: &'de str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        deserialize_any_with_limits(input, &crate::Limits::default(), visitor)
    }

    /// Deserialize one document with a visitor, explicit limits, and compatibility
    /// policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn deserialize_any_with_limits<'de, V: Visitor<'de>>(
        input: &'de str,
        limits: &crate::Limits,
        visitor: V,
    ) -> Result<V::Value, Error> {
        crate::typed::from_str_visitor(
            input,
            limits,
            crate::typed::NodeOptions {
                allow_duplicates: true,
                compat_flow: true,
            },
            visitor,
        )
        .map_err(map_error)
    }
}

/// Unstable layer hooks for the benchmark harness.
///
/// **Unstable and outside SemVer.** This module is hidden from the public
/// documentation and exists only for ayaml's benchmark and
/// conformance harnesses. It may change or disappear in any release.
#[cfg(feature = "bench-internals")]
#[cfg_attr(docsrs, doc(cfg(feature = "bench-internals")))]
#[doc(hidden)]
pub mod internals {
    use crate::error::Error;
    use crate::event::Event;
    use crate::limits::Limits;
    use crate::value::Value;

    /// Scan all tokens and return their count.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn scan_tokens(input: &str, limits: &Limits) -> Result<usize, Error> {
        let mut scanner = crate::scanner::Scanner::new(input, limits.clone())?;
        let mut count = 0;
        while scanner.next_token()?.is_some() {
            count += 1;
        }
        Ok(count)
    }

    /// Parse all events into an owned vector.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn parse_events<'a>(input: &'a str, limits: &Limits) -> Result<Vec<Event<'a>>, Error> {
        crate::parser::parse_events(input, limits)
    }

    /// Compose an already-collected event vector. Like every entry point that takes limits, it
    /// refuses limits that would turn a budget off.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn compose_events(
        events: &[Event<'_>],
        input_len: usize,
        limits: &Limits,
    ) -> Result<Vec<Value>, Error> {
        limits.validate()?;
        crate::composer::compose_documents(events, input_len, limits)
    }
}

/// A pull traversal over spanned parser events.
///
/// Events are produced lazily in document-sized chunks. Pulling stops further
/// scanning when the consumer drops the stream, and syntax errors are returned
/// as iterator items rather than being discovered only after the whole input is
/// parsed. The current implementation buffers one document at a time; finer
/// per-event streaming and bounded line buffering remain hardening work.
#[derive(Debug)]
pub struct EventStream<'a> {
    inner: parser::EventCursor<'a>,
}

impl<'a> Iterator for EventStream<'a> {
    type Item = Result<Event<'a>, Error>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next_event().transpose()
    }
}

/// A document-bounded typed deserializer.
///
/// This consumes one YAML document at a time from the public event traversal.
/// [`next`](Self::next) composes owned values, while
/// [`next_borrowed`](Self::next_borrowed) decodes from spanned events and can
/// borrow simple scalar strings from the input. The aliases of every document
/// draw on one alias budget for the whole stream.
#[derive(Debug)]
pub struct Deserializer<'a> {
    events: EventStream<'a>,
    limits: Limits,
    input_len: usize,
    finished: bool,
    /// One alias budget for the whole stream.
    budget: limits::AliasBudget,
}

impl<'a> Deserializer<'a> {
    /// Create a typed document cursor over one UTF-8 YAML input.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[allow(
        clippy::should_implement_trait,
        reason = "the cursor deliberately mirrors Iterator::next with a different return type"
    )]
    pub fn from_str(input: &'a str) -> Result<Self, Error> {
        Self::from_str_with_limits(input, &Limits::default())
    }

    /// Create a typed document cursor with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        let limits = limits.clone();
        let events = parse_events_with_limits(input, &limits)?;
        Ok(Self {
            events,
            limits,
            input_len: input.len(),
            finished: false,
            budget: limits::AliasBudget::default(),
        })
    }

    /// Deserialize the next document, or return `None` at end of stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[allow(
        clippy::should_implement_trait,
        reason = "the cursor deliberately mirrors Iterator::next with a different return type"
    )]
    pub fn next<T: serde::de::DeserializeOwned>(&mut self) -> Result<Option<T>, Error> {
        let Some(value) = self.next_value()? else {
            return Ok(None);
        };
        from_value(value).map(Some)
    }

    /// Deserialize the next document, borrowing simple strings from the input.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn next_borrowed<T: serde::Deserialize<'a>>(&mut self) -> Result<Option<T>, Error> {
        let Some(events) = self.next_events()? else {
            return Ok(None);
        };
        typed::from_events(events, self.input_len, &self.limits, &mut self.budget).map(Some)
    }

    /// Deserialize the next document with a seed, borrowing simple strings.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn next_seed_borrowed<T: serde::de::DeserializeSeed<'a>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(events) = self.next_events()? else {
            return Ok(None);
        };
        typed::from_events_seed(events, self.input_len, &self.limits, &mut self.budget, seed)
            .map(Some)
    }

    /// Deserialize the next document using a caller-supplied seed.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn next_seed<'de, T: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(value) = self.next_value()? else {
            return Ok(None);
        };
        from_value_seed(value, seed).map(Some)
    }

    fn next_value(&mut self) -> Result<Option<Value>, Error> {
        let Some(events) = self.next_events()? else {
            return Ok(None);
        };
        let value = composer::compose_documents_with_budget(
            events,
            self.input_len,
            &self.limits,
            &mut self.budget,
        )?
        .into_iter()
        .next()
        .ok_or_else(|| Error::new(ErrorKind::Composition, "document produced no value"))?;
        Ok(Some(value))
    }

    #[allow(
        clippy::match_same_arms,
        reason = "the arms stay separate to document each representation or state"
    )]
    fn next_events(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        if self.finished {
            return Ok(None);
        }
        let mut events = Vec::new();
        let mut saw_document = false;
        loop {
            match self.events.next() {
                None => {
                    self.finished = true;
                    break;
                }
                Some(Ok(Event::StreamStart { .. })) => {}
                Some(Ok(Event::StreamEnd { .. })) => {
                    self.finished = true;
                    break;
                }
                Some(Ok(event)) => {
                    if matches!(event, Event::DocumentStart { .. }) {
                        saw_document = true;
                    }
                    let end = matches!(event, Event::DocumentEnd { .. });
                    events.push(event);
                    if end {
                        break;
                    }
                }
                Some(Err(error)) => {
                    self.finished = true;
                    return Err(error);
                }
            }
        }
        if !saw_document {
            return Ok(None);
        }
        Ok(Some(events))
    }
}

/// Parse a YAML input into a pull traversal of spanned events.
///
/// This exposes the shared scanner/parser pipeline without composing values.
/// Duplicate mapping entries remain visible to the caller. Syntax and limit
/// errors are returned by the iterator.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_events(input: &str) -> Result<EventStream<'_>, Error> {
    parse_events_with_limits(input, &Limits::default())
}

/// Parse a YAML input into a pull traversal with explicit limits.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_events_with_limits<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<EventStream<'a>, Error> {
    parser::EventCursor::new(input, limits).map(|inner| EventStream { inner })
}

/// Parse one YAML document into the generic value model.
///
/// The compatibility profile is used: `yes` and date-like scalars stay
/// strings, legacy octal-looking scalars such as `0123` stay strings, `0b11`
/// and `0x1f` become integers, and `1e3` becomes a float. Custom tags remain
/// represented as [`Value::Tagged`]. Anchors and aliases are materialized under
/// the default alias-expansion budget; merge keys stay literal.
///
/// This is a single-document convenience API. Trailing documents are rejected;
/// use [`parse_documents`] for a document stream.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_value(input: &str) -> Result<Value, Error> {
    parse_value_with_limits(input, &Limits::default())
}

/// Parse one YAML document with explicit limits.
///
/// Limits are checked before scanning and again during composition. The input
/// is never allowed to exceed `limits.max_input_bytes`, and alias materialization
/// is charged before allocation. Trailing documents are rejected even when the
/// limits would otherwise permit them.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_value_with_limits(input: &str, limits: &Limits) -> Result<Value, Error> {
    limits
        .check_input(input.len())
        .map_err(|error| Error::from(error).with_span(Span::empty(Marker::new(0, 1, 1))))?;
    let mut documents = parser::parse_documents(input, limits, true)?;
    if documents.len() > 1 {
        return Err(Error::new(
            ErrorKind::Parser,
            "expected a single YAML document, but found trailing documents",
        ));
    }
    Ok(documents.pop().unwrap_or(Value::Null))
}

/// Parse every YAML document in the stream into generic values.
///
/// The same compatibility resolver and composition budgets as [`parse_value`]
/// apply. The configured `max_documents` limit is enforced by the event parser.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_documents(input: &str) -> Result<Vec<Value>, Error> {
    parse_documents_with_limits(input, &Limits::default())
}

/// Parse every YAML document in the stream with explicit limits.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_documents_with_limits(input: &str, limits: &Limits) -> Result<Vec<Value>, Error> {
    limits
        .check_input(input.len())
        .map_err(|error| Error::from(error).with_span(Span::empty(Marker::new(0, 1, 1))))?;
    parser::parse_documents(input, limits, false)
}

/// Deserialize one YAML document into a typed Serde value.
///
/// This uses the span-carrying typed node path: empty documents can decode
/// into defaulted structs, duplicate keys are reported at their first
/// occurrence, and alias limits are enforced. Trailing documents are
/// rejected. Use `ayaml::compat` for libyaml-compatible decoding.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, Error> {
    typed::from_str(input, &Limits::default())
}

/// Deserialize one YAML document while borrowing simple strings from `input`.
///
/// Unlike [`from_str`], this path does not compose an owned [`Value`] first.
/// Plain, single-quoted, and unescaped double-quoted scalar strings can be
/// returned as `&str` or `Cow::Borrowed`, and typed errors retain the span of
/// the failing YAML node. Complex/folded strings remain owned.
///
/// ```
/// #[derive(serde::Deserialize)]
/// struct Config<'a> {
///     name: &'a str,
/// }
///
/// let config: Config<'_> = ayaml::from_str_borrowed("name: ayaml\n").unwrap();
/// assert_eq!(config.name, "ayaml");
/// ```
///
/// Trailing documents are rejected.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str_borrowed<'de, T: serde::Deserialize<'de>>(input: &'de str) -> Result<T, Error> {
    from_str_borrowed_with_limits(input, &Limits::default())
}

/// Deserialize one YAML document with explicit limits, borrowing simple strings.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str_borrowed_with_limits<'de, T: serde::Deserialize<'de>>(
    input: &'de str,
    limits: &Limits,
) -> Result<T, Error> {
    typed::from_str(input, limits)
}

/// Deserialize one UTF-8 YAML document from bytes, borrowing simple strings.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_slice_borrowed<'de, T: serde::Deserialize<'de>>(input: &'de [u8]) -> Result<T, Error> {
    let input = std::str::from_utf8(input).map_err(|error| {
        Error::new(ErrorKind::InvalidEncoding, "input is not valid UTF-8").with_source(error)
    })?;
    from_str_borrowed(input)
}

/// Deserialize one UTF-8 YAML document from bytes.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_slice<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, Error> {
    let input = std::str::from_utf8(input).map_err(|error| {
        Error::new(ErrorKind::InvalidEncoding, "input is not valid UTF-8").with_source(error)
    })?;
    from_str(input)
}

/// The crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
