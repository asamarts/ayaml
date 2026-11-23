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
//! Configure options and limits with [`ParseOptions`] and [`EmitOptions`]:
//!
//! ```
//! use ayaml::{EmitOptions, Limits, ParseOptions, parse_value_with, to_string_value_with};
//!
//! const PARSE: ParseOptions =
//!     ParseOptions::new().with_limits(Limits::new().with_max_depth(64));
//! let value = parse_value_with("a: [1, 2, 3]\n", &PARSE)?;
//! assert_eq!(value["a"][1].as_u64(), Some(2));
//!
//! let yaml = to_string_value_with(&value, &EmitOptions::flow())?;
//! assert_eq!(yaml, "{a: [1, 2, 3]}\n");
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
//! Every parse, decode, and emit operation has a `_with` form taking one
//! options value ([`ParseOptions`], `compat::ParseOptions`, or
//! [`EmitOptions`]), which carries its [`Limits`]; [`Value::apply_merge_keys`]
//! and [`Value::apply_merge_keys_with_limits`] bound merge depth and merged
//! mapping sizes; `from_value`,
//! `from_value_seed`, and `to_value` operate on an in-memory [`Value`] and do
//! not take limits. Exceeding a limit
//! returns [`ErrorKind::Limit`]; [`Error::limit`] exposes the structured
//! [`LimitExceeded`] value. See the `docs/limits.md` file for the defaults and
//! the alias-budget formula.
//!
//! # Errors and spans
//!
//! [`Error`] implements [`Clone`], [`PartialEq`], and [`Eq`], and carries a
//! coarse category ([`Error::kind`] -> [`ErrorKind`]), a stable
//! machine-readable code ([`Error::code`] -> [`ErrorCode`], with
//! [`ErrorCode::as_str`] returning `yaml.<kebab-name>`), an optional byte
//! span, an optional source error, and a human-readable message. Input-related
//! errors carry a span and [`Error::location`] returns the one-based line and
//! column. Normal [`Display`](std::fmt::Display) and [`Debug`](std::fmt::Debug)
//! output on [`Error`] may include text from the document (such as scalar
//! values, mapping keys, paths, tags, or directives); use [`Error::masked`]
//! ([`MaskedError`]) when logging or returning errors from sensitive or
//! untrusted documents. See the documentation book
//! (<https://asamarts.github.io/ayaml/>) — including `docs/error-codes.md`,
//! `docs/security.md`, and `docs/stability.md` — for the full code reference,
//! security and untrusted-input guidance, stability guarantees, and masked
//! error formatting. Events carry their own spans for source-aware tooling.
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
mod options;
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
mod spanned;
mod typed;

pub mod error;
pub mod event;
pub mod limits;
pub mod mapping;
pub mod number;
pub mod span;
pub mod tag;
pub mod value;

pub use error::{Error, ErrorCode, ErrorKind, MaskedError};
pub use event::{Anchor, CollectionStyle, Event, ScalarStyle};
pub use limits::{
    ALIAS_BYTES_PER_INPUT_BYTE, COMPAT_MAX_DOCUMENTS, DEFAULT_MAX_DOCUMENTS, LimitExceeded,
    LimitKind, Limits, MAX_ALIAS_EXPANSION_BYTES, MAX_DEPTH, MAX_DEPTH_CAP, MAX_INPUT_BYTES,
    MAX_TAG_EXPANSION_BYTES, NODE_COST_BYTES,
};
pub use mapping::{Entry, Mapping};
pub use number::Number;
pub use options::{EmitOptions, ParseOptions};
pub use position::{ColumnEncoding, LineIndex, PositionError};
#[allow(deprecated, reason = "re-export the compatibility marker")]
pub use span::{Location, Marker, Position, Span};
pub use spanned::{SpannedNode, SpannedTree};
pub use tag::{Tag, TaggedValue};
pub use value::Value;

pub use emitter::{
    Chomping, Emitter, EmitterOptions, EmitterStyles, NodeStyle, PathSegment, to_string,
    to_string_documents, to_string_documents_with, to_string_value, to_string_value_with,
    to_string_with, to_writer, to_writer_documents, to_writer_documents_with, to_writer_value,
    to_writer_value_with, to_writer_with,
};
#[allow(
    deprecated,
    reason = "re-export deprecated emitter combination functions through 0.x until removal in 1.0"
)]
pub use emitter::{
    to_string_documents_with_limits, to_string_value_with_limits,
    to_string_value_with_limits_and_styles, to_string_value_with_options,
    to_string_value_with_options_and_styles, to_string_value_with_styles, to_string_with_limits,
    to_string_with_options, to_writer_documents_with_limits, to_writer_value_with_limits,
    to_writer_value_with_limits_and_styles, to_writer_value_with_options,
    to_writer_value_with_options_and_styles, to_writer_value_with_styles, to_writer_with_limits,
    to_writer_with_options,
};
pub use event_emitter::{
    to_string_events, to_string_events_with, to_writer_event_stream, to_writer_event_stream_with,
    to_writer_events, to_writer_events_with,
};
#[allow(
    deprecated,
    reason = "re-export deprecated event-emitter functions through 0.x until removal in 1.0"
)]
pub use event_emitter::{
    to_string_events_with_limits, to_writer_event_stream_with_limits, to_writer_events_with_limits,
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
    use crate::error::{Error, ErrorCode, ErrorKind};
    use crate::limits::LimitKind;
    use serde::de::Visitor;

    /// How compatibility typed readers treat global tags outside the YAML 1.2 core schema, such as
    /// `!!timestamp`, `!!binary`, `!<tag:example.com,2000:x>`, or a `%TAG` handle mapped to a URI.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
    #[non_exhaustive]
    pub enum GlobalTags {
        /// Keep global tags on their nodes, as the default readers do.
        #[default]
        Keep,
        /// Drop global tags outside the core schema, as a libyaml-based reader does.
        ///
        /// A scalar beneath a dropped global tag reads as its text for a target that reads any
        /// value and parses as the requested type for a typed target. A collection beneath one
        /// reads as if it had no tag. Core-schema tags, the non-specific `!` tag, and local tags
        /// keep their usual reading.
        Drop,
    }

    /// Options for [`compat`](crate::compat) readers.
    #[derive(Clone, Debug, PartialEq, Eq)]
    #[non_exhaustive]
    pub struct ParseOptions {
        /// Resource limits applied to the parse.
        pub limits: crate::Limits,
        /// How global tags outside the YAML 1.2 core schema are handled by
        /// typed and visitor readers. Event readers and
        /// [`SpannedTree::parse_compat_with`](crate::SpannedTree::parse_compat_with)
        /// preserve syntax tags as written.
        pub global_tags: GlobalTags,
    }

    impl Default for ParseOptions {
        fn default() -> Self {
            Self::new()
        }
    }

    impl From<crate::Limits> for ParseOptions {
        fn from(limits: crate::Limits) -> Self {
            Self::new().with_limits(limits)
        }
    }

    impl ParseOptions {
        /// Compatibility parse options with the default limits and [`GlobalTags::Keep`].
        #[must_use]
        pub const fn new() -> Self {
            Self {
                limits: crate::Limits::default_profile(),
                global_tags: GlobalTags::Keep,
            }
        }

        /// Compatibility parse options using [`Limits::for_max_input`](crate::Limits::for_max_input)
        /// and [`GlobalTags::Keep`].
        #[must_use]
        pub const fn for_max_input() -> Self {
            Self {
                limits: crate::Limits::for_max_input(),
                global_tags: GlobalTags::Keep,
            }
        }

        /// Set the resource limits applied to the parse.
        #[must_use]
        pub const fn with_limits(mut self, limits: crate::Limits) -> Self {
            self.limits = limits;
            self
        }

        /// Set how global tags outside the YAML 1.2 core schema are handled.
        #[must_use]
        pub const fn with_global_tags(mut self, global_tags: GlobalTags) -> Self {
            self.global_tags = global_tags;
            self
        }
    }

    const fn node_options(global_tags: GlobalTags) -> crate::typed::NodeOptions {
        crate::typed::NodeOptions {
            allow_duplicates: true,
            compat_flow: true,
            drop_global_tags: matches!(global_tags, GlobalTags::Drop),
        }
    }

    /// Translate ayaml diagnostics into the strings expected by
    /// libyaml-compatible consumers. Each branch is a documented compatibility
    /// deviation keyed by the parser's structured failure, never by the text of
    /// the input or of the message.
    fn map_error(error: Error) -> Error {
        if error.is_reserved_after_bom() {
            let mapped = Error::coded(
                ErrorKind::Scanner,
                ErrorCode::ReservedIndicator,
                "found character that cannot start any token",
            );
            return if let Some(span) = error.span() {
                let mapped = mapped.with_span(span);
                if let Some(location) = error.location() {
                    mapped.with_location(location)
                } else {
                    mapped
                }
            } else {
                mapped
            };
        }
        if error
            .limit()
            .is_some_and(|limit| limit.kind() == LimitKind::Depth)
        {
            let mapped = Error::coded(
                ErrorKind::Serde,
                ErrorCode::DepthLimit,
                "recursion limit exceeded",
            );
            return if let Some(span) = error.span() {
                let mapped = mapped.with_span(span);
                if let Some(location) = error.location() {
                    mapped.with_location(location)
                } else {
                    mapped
                }
            } else {
                mapped
            };
        }
        if error.code() == ErrorCode::UnexpectedMappingValue {
            let mapped = Error::coded(
                ErrorKind::Parser,
                ErrorCode::UnexpectedMappingValue,
                "mapping values are not allowed in this context",
            );
            return if let Some(span) = error.span() {
                let mapped = mapped.with_span(span);
                if let Some(location) = error.location() {
                    mapped.with_location(location)
                } else {
                    mapped
                }
            } else {
                mapped
            };
        }
        if error.code() == ErrorCode::TrailingDocuments
            || error
                .limit()
                .is_some_and(|limit| limit.kind() == LimitKind::Documents)
        {
            let mapped = Error::coded(
                ErrorKind::Parser,
                error.code(),
                "more than one document in stream",
            );
            return if let Some(span) = error.span() {
                let mapped = mapped.with_span(span);
                if let Some(location) = error.location() {
                    mapped.with_location(location)
                } else {
                    mapped
                }
            } else {
                mapped
            };
        }
        error
    }

    /// Deserialize one document with a caller-supplied seed under the
    /// compatibility policies.
    ///
    /// An input of more than 4 KiB is read into the seed as it is parsed, as
    /// [`from_str`](crate::from_str) says. A seed that writes to its caller's data as it reads
    /// may have written the start of a document that is then refused, so on an error what it
    /// wrote is to be dropped.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_seed<'de, T: serde::de::DeserializeSeed<'de>>(
        input: &'de str,
        seed: T,
    ) -> Result<T::Value, Error> {
        from_str_seed_with(input, &ParseOptions::new(), seed)
    }

    /// Deserialize one document with a caller-supplied seed and explicit limits
    /// under the compatibility policies. What [`from_str_seed`] says of a document that is
    /// refused holds here.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `compat::from_str_seed_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn from_str_seed_with_limits<'de, T: serde::de::DeserializeSeed<'de>>(
        input: &'de str,
        limits: &crate::Limits,
        seed: T,
    ) -> Result<T::Value, Error> {
        crate::typed::from_str_seed_with_options(
            input,
            limits,
            node_options(GlobalTags::Keep),
            seed,
        )
        .map_err(map_error)
    }

    /// Deserialize one document with a caller-supplied seed and compatibility options.
    ///
    /// What [`from_str_seed`] says of a document that is refused holds here.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_seed_with<'de, T: serde::de::DeserializeSeed<'de>>(
        input: &'de str,
        options: &ParseOptions,
        seed: T,
    ) -> Result<T::Value, Error> {
        crate::typed::from_str_seed_with_options(
            input,
            &options.limits,
            node_options(options.global_tags),
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
        from_str_with(input, &ParseOptions::new())
    }

    /// Deserialize a typed value with compatibility policies and explicit limits,
    /// using borrowed scalar text where possible.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `compat::from_str_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn from_str_with_limits<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &crate::Limits,
    ) -> Result<T, Error> {
        crate::typed::from_str_with_options(input, limits, node_options(GlobalTags::Keep))
            .map_err(map_error)
    }

    /// Deserialize a typed value with compatibility options, using borrowed scalar text where
    /// possible.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_with<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        options: &ParseOptions,
    ) -> Result<T, Error> {
        crate::typed::from_str_with_options(
            input,
            &options.limits,
            node_options(options.global_tags),
        )
        .map_err(map_error)
    }

    /// Parse a stream into a lazy event traversal with the libyaml-compatible scanner.
    ///
    /// As in default mode, an alias is reported as written and is not checked against earlier
    /// anchors; the compatibility value readers do that check.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn parse_events(input: &str) -> Result<crate::EventStream<'_>, Error> {
        parse_events_with(input, &ParseOptions::new())
    }

    /// Parse a stream into a lazy event traversal with explicit compatibility
    /// options and the libyaml-compatible scanner.
    ///
    /// Applies `options.limits`. Aliases and tags are reported as written;
    /// `options.global_tags` applies only to typed and visitor readers.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn parse_events_with<'a>(
        input: &'a str,
        options: &ParseOptions,
    ) -> Result<crate::EventStream<'a>, Error> {
        crate::parser::EventCursor::with_compat(input, &options.limits, true)
            .map(crate::EventStream::from_cursor)
    }

    /// Parse a stream into a lazy event traversal with explicit limits and the
    /// libyaml-compatible scanner.
    ///
    /// Aliases are reported as written and are not checked against earlier anchors, as in
    /// [`parse_events`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `compat::parse_events_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn parse_events_with_limits<'a>(
        input: &'a str,
        limits: &crate::Limits,
    ) -> Result<crate::EventStream<'a>, Error> {
        crate::parser::EventCursor::with_compat(input, limits, true)
            .map(crate::EventStream::from_cursor)
    }

    /// Parse a whole stream into an eager event vector with explicit
    /// compatibility options and the libyaml-compatible scanner. The
    /// conformance runner uses this to check compatibility deviations.
    ///
    /// Applies `options.limits`. Aliases and tags are reported as written;
    /// `options.global_tags` applies only to typed and visitor readers.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn parse_events_eager_with<'a>(
        input: &'a str,
        options: &ParseOptions,
    ) -> Result<Vec<crate::Event<'a>>, Error> {
        crate::parser::parse_events_compat(input, &options.limits)
    }

    /// Parse a whole stream into an eager event vector with the
    /// libyaml-compatible scanner. The conformance runner uses this to check
    /// compatibility deviations.
    ///
    /// Aliases are reported as written and are not checked against earlier anchors, as in
    /// [`parse_events`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `compat::parse_events_eager_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn parse_events_eager_with_limits<'a>(
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
        deserialize_any_with(input, &ParseOptions::new(), visitor)
    }

    /// Deserialize one document with a visitor, explicit limits, and compatibility
    /// policies.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `compat::deserialize_any_with` with `compat::ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn deserialize_any_with_limits<'de, V: Visitor<'de>>(
        input: &'de str,
        limits: &crate::Limits,
        visitor: V,
    ) -> Result<V::Value, Error> {
        crate::typed::from_str_visitor(input, limits, node_options(GlobalTags::Keep), visitor)
            .map_err(map_error)
    }

    /// Deserialize one document with a visitor and compatibility options.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn deserialize_any_with<'de, V: Visitor<'de>>(
        input: &'de str,
        options: &ParseOptions,
        visitor: V,
    ) -> Result<V::Value, Error> {
        crate::typed::from_str_visitor(
            input,
            &options.limits,
            node_options(options.global_tags),
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

    /// Decode one document into a typed value by building its whole tree first, as a typed read
    /// of a small input does. `compat` reads with the compatibility scanner and keeps duplicate
    /// keys, as `compat::from_str` does, but reports errors as default mode words them. The
    /// tests compare this reader with [`from_str_by_events`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_by_tree<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &Limits,
        compat: bool,
    ) -> Result<T, Error> {
        from_str_by_tree_with(input, limits, compat, false)
    }

    /// [`from_str_by_tree`], with an opt-in to drop non-core global tags.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_by_tree_with<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &Limits,
        compat: bool,
        drop_global_tags: bool,
    ) -> Result<T, Error> {
        crate::typed::from_str_by_tree(
            input,
            limits,
            crate::typed::NodeOptions {
                allow_duplicates: compat,
                compat_flow: compat,
                drop_global_tags,
            },
            std::marker::PhantomData::<T>,
        )
    }

    /// Decode one document into a typed value from the parser's events, as a typed read of a
    /// larger input does, whatever this input's size. `compat` is as for [`from_str_by_tree`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_by_events<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &Limits,
        compat: bool,
    ) -> Result<T, Error> {
        from_str_by_events_with(input, limits, compat, false)
    }

    /// [`from_str_by_events`], with an opt-in to drop non-core global tags.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_by_events_with<'de, T: serde::Deserialize<'de>>(
        input: &'de str,
        limits: &Limits,
        compat: bool,
        drop_global_tags: bool,
    ) -> Result<T, Error> {
        crate::typed::from_str_by_events(
            input,
            limits,
            crate::typed::NodeOptions {
                allow_duplicates: compat,
                compat_flow: compat,
                drop_global_tags,
            },
            std::marker::PhantomData::<T>,
        )
    }
}

/// A pull traversal over spanned parser events.
///
/// Events are produced lazily in document-sized chunks. Pulling stops further
/// scanning when the consumer drops the stream, and syntax errors are returned
/// as iterator items rather than being discovered only after the whole input is
/// parsed. An alias is reported as written and is not checked against earlier
/// anchors; composing values does that check. The current implementation
/// buffers one document at a time; finer per-event streaming and bounded line
/// buffering remain hardening work.
#[derive(Debug)]
pub struct EventStream<'a> {
    inner: parser::EventCursor<'a>,
}

impl<'a> EventStream<'a> {
    pub(crate) fn next_document(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        self.inner.next_document()
    }
}

#[cfg(feature = "libyaml-compat")]
impl<'a> EventStream<'a> {
    pub(crate) fn from_cursor(inner: parser::EventCursor<'a>) -> Self {
        Self { inner }
    }
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
/// Both [`next`](Self::next) and [`next_borrowed`](Self::next_borrowed) decode
/// directly from spanned events, preserving per-node source spans on Serde
/// errors without composing an intermediate owned [`Value`];
/// [`next_borrowed`](Self::next_borrowed) additionally allows targets to borrow
/// simple scalar strings from the input. The aliases of every document draw on
/// one alias budget for the whole stream.
#[derive(Debug)]
pub struct Deserializer<'a> {
    events: EventStream<'a>,
    limits: Limits,
    input: &'a str,
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
        Self::from_str_with(input, &ParseOptions::new())
    }

    /// Create a typed document cursor with explicit parse options.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn from_str_with(input: &'a str, options: &ParseOptions) -> Result<Self, Error> {
        let events = parse_events_with(input, options)?;
        Ok(Self {
            events,
            limits: options.limits.clone(),
            input,
            input_len: input.len(),
            finished: false,
            budget: limits::AliasBudget::default(),
        })
    }

    /// Create a typed document cursor with explicit limits.
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[deprecated(
        note = "use `Deserializer::from_str_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
    )]
    pub fn from_str_with_limits(input: &'a str, limits: &Limits) -> Result<Self, Error> {
        Self::from_str_with(input, &ParseOptions::new().with_limits(limits.clone()))
    }

    /// Deserialize the next document, or return `None` at end of stream.
    ///
    /// Decodes directly from spanned events so Serde errors preserve per-node
    /// source spans without materializing an intermediate owned [`Value`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    #[allow(
        clippy::should_implement_trait,
        reason = "the cursor deliberately mirrors Iterator::next with a different return type"
    )]
    pub fn next<T: serde::de::DeserializeOwned>(&mut self) -> Result<Option<T>, Error> {
        self.next_borrowed()
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
        typed::from_events(events, self.input_len, &self.limits, &mut self.budget)
            .map(Some)
            .map_err(|error| error.with_input_location(self.input, false))
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
            .map_err(|error| error.with_input_location(self.input, false))
    }

    /// Deserialize the next document using a caller-supplied seed.
    ///
    /// Decodes directly from spanned events so Serde errors preserve per-node
    /// source spans without materializing an intermediate owned [`Value`].
    ///
    /// # Errors
    ///
    /// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
    pub fn next_seed<'de, T: serde::de::DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        let Some(events) = self.next_events()? else {
            return Ok(None);
        };
        typed::from_events_seed_owned(events, self.input_len, &self.limits, &mut self.budget, seed)
            .map(Some)
            .map_err(|error| error.with_input_location(self.input, false))
    }

    fn next_events(&mut self) -> Result<Option<Vec<Event<'a>>>, Error> {
        if self.finished {
            return Ok(None);
        }
        match self.events.next_document() {
            Ok(Some(events)) => Ok(Some(events)),
            Ok(None) => {
                self.finished = true;
                Ok(None)
            }
            Err(error) => {
                self.finished = true;
                Err(error)
            }
        }
    }
}

/// Parse a YAML input into a pull traversal of spanned events.
///
/// This exposes the shared scanner/parser pipeline without composing values.
/// Duplicate mapping entries remain visible to the caller. Syntax and limit
/// errors are returned by the iterator.
///
/// An [`Event::Alias`] is reported as written. These readers do not check that an earlier anchor
/// in the document defines it; the value readers compose and check aliases. A consumer that
/// resolves aliases must track anchors itself.
///
/// ```
/// let events: Vec<ayaml::Event<'_>> = ayaml::parse_events("a: *missing\n")?
///     .collect::<Result<_, _>>()?;
/// assert!(events.iter().any(|event| matches!(event, ayaml::Event::Alias { .. })));
/// assert!(ayaml::parse_value("a: *missing\n").is_err());
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_events(input: &str) -> Result<EventStream<'_>, Error> {
    parse_events_with(input, &ParseOptions::new())
}

/// Parse a YAML input into a pull traversal with explicit parse options.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_events_with<'a>(
    input: &'a str,
    options: &ParseOptions,
) -> Result<EventStream<'a>, Error> {
    parser::EventCursor::new(input, &options.limits).map(|inner| EventStream { inner })
}

/// Parse a YAML input into a pull traversal with explicit limits.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
#[deprecated(
    note = "use `parse_events_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn parse_events_with_limits<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<EventStream<'a>, Error> {
    parser::EventCursor::new(input, limits).map(|inner| EventStream { inner })
}

/// Parse one YAML document into the generic value model.
///
/// The default YAML 1.2.2 profile is used: `yes` and date-like scalars stay
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
    parse_value_with(input, &ParseOptions::new())
}

/// Parse one YAML document with explicit parse options.
///
/// Limits are checked before scanning and again during composition. The input
/// is never allowed to exceed `options.limits.max_input_bytes`, and alias
/// materialization is charged before allocation. Trailing documents are
/// rejected even when the limits would otherwise permit them.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_value_with(input: &str, options: &ParseOptions) -> Result<Value, Error> {
    parse_single_value(input, &options.limits)
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
#[deprecated(
    note = "use `parse_value_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn parse_value_with_limits(input: &str, limits: &Limits) -> Result<Value, Error> {
    parse_single_value(input, limits)
}

fn parse_single_value(input: &str, limits: &Limits) -> Result<Value, Error> {
    limits.validate()?;
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    let mut documents = parser::parse_documents(input, limits, true)?;
    if documents.len() > 1 {
        return Err(Error::coded(
            ErrorKind::Parser,
            ErrorCode::TrailingDocuments,
            "expected a single YAML document, but found trailing documents",
        ));
    }
    Ok(documents.pop().unwrap_or(Value::Null))
}

/// Parse every YAML document in the stream into generic values.
///
/// The same default resolver and composition budgets as [`parse_value`]
/// apply. The configured `max_documents` limit is enforced by the event parser.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_documents(input: &str) -> Result<Vec<Value>, Error> {
    parse_documents_with(input, &ParseOptions::new())
}

/// Parse every YAML document in the stream with explicit parse options.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn parse_documents_with(input: &str, options: &ParseOptions) -> Result<Vec<Value>, Error> {
    parse_stream_documents(input, &options.limits)
}

/// Parse every YAML document in the stream with explicit limits.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
#[deprecated(
    note = "use `parse_documents_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn parse_documents_with_limits(input: &str, limits: &Limits) -> Result<Vec<Value>, Error> {
    parse_stream_documents(input, limits)
}

fn parse_stream_documents(input: &str, limits: &Limits) -> Result<Vec<Value>, Error> {
    limits.validate()?;
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    parser::parse_documents(input, limits, false)
}

/// Deserialize one YAML document into a typed Serde value.
///
/// An empty document can decode into a defaulted struct. A duplicate key is an error, placed
/// at the key that repeats an earlier one, and the alias limits are enforced. Trailing
/// documents are rejected. An error names the span of the YAML node it is about. Use
/// `ayaml::compat` for libyaml-compatible decoding.
///
/// An input of more than 4 KiB is read into the target as it is parsed. The value and the
/// error are what they would be otherwise, but the target's `Deserialize` may have run on the
/// start of a document that is then refused, and a collection other than a root flow
/// collection tells its visitor no size.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str<T: serde::de::DeserializeOwned>(input: &str) -> Result<T, Error> {
    typed::from_str(input, &Limits::default_profile())
}

/// Deserialize one YAML document into a typed Serde value with explicit parse
/// options, borrowing simple strings from `input` where possible.
///
/// Accepts both owned (`DeserializeOwned`) and borrowed (`Deserialize<'de>`)
/// targets. See [`from_str`] and [`from_str_borrowed`] for the decoding rules.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str_with<'de, T: serde::Deserialize<'de>>(
    input: &'de str,
    options: &ParseOptions,
) -> Result<T, Error> {
    typed::from_str(input, &options.limits)
}

/// Deserialize one YAML document while borrowing simple strings from `input`.
///
/// It reads as [`from_str`] does, and its target may borrow from `input`: a plain,
/// single-quoted, or unescaped double-quoted scalar string on one line can be returned as
/// `&str` or `Cow::Borrowed`. A string that decoding changes, such as a folded or escaped one,
/// is owned, so only a `Cow` or a `String` can hold it.
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
    typed::from_str(input, &Limits::default_profile())
}

/// Deserialize one YAML document with explicit limits, borrowing simple strings.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
#[deprecated(
    note = "use `from_str_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn from_str_borrowed_with_limits<'de, T: serde::Deserialize<'de>>(
    input: &'de str,
    limits: &Limits,
) -> Result<T, Error> {
    typed::from_str(input, limits)
}

/// Deserialize one YAML document with a caller-supplied seed, borrowing simple strings from
/// `input`.
///
/// It reads as [`from_str_borrowed`] does, under the default YAML 1.2.2 rules and limits.
/// Duplicate keys and trailing documents are rejected. An input of more than 4 KiB is read into
/// the seed as it is parsed, as [`from_str`] says: a seed that writes to its caller's data as it
/// reads may have written the start of a document that is then refused, so on an error what it
/// wrote is to be dropped.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str_seed<'de, T: serde::de::DeserializeSeed<'de>>(
    input: &'de str,
    seed: T,
) -> Result<T::Value, Error> {
    from_str_seed_with(input, &ParseOptions::new(), seed)
}

/// Deserialize one YAML document with a caller-supplied seed and explicit
/// parse options, borrowing simple strings from `input`.
///
/// What [`from_str_seed`] says of a document that is refused holds here.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_str_seed_with<'de, T: serde::de::DeserializeSeed<'de>>(
    input: &'de str,
    options: &ParseOptions,
    seed: T,
) -> Result<T::Value, Error> {
    typed::from_str_seed_with_options(input, &options.limits, typed::NodeOptions::default(), seed)
}

/// Deserialize one YAML document with a caller-supplied seed and explicit limits, borrowing
/// simple strings from `input`.
///
/// What [`from_str_seed`] says of a document that is refused holds here.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
#[deprecated(
    note = "use `from_str_seed_with` with `ParseOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn from_str_seed_with_limits<'de, T: serde::de::DeserializeSeed<'de>>(
    input: &'de str,
    limits: &Limits,
    seed: T,
) -> Result<T::Value, Error> {
    typed::from_str_seed_with_options(input, limits, typed::NodeOptions::default(), seed)
}

/// Deserialize one UTF-8 YAML document from bytes, borrowing simple strings.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_slice_borrowed<'de, T: serde::Deserialize<'de>>(input: &'de [u8]) -> Result<T, Error> {
    from_slice_with(input, &ParseOptions::new())
}

/// Deserialize one UTF-8 YAML document from bytes.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_slice<T: serde::de::DeserializeOwned>(input: &[u8]) -> Result<T, Error> {
    from_slice_with(input, &ParseOptions::new())
}

/// Deserialize one UTF-8 YAML document from bytes with explicit parse options,
/// borrowing simple strings from `input` where possible.
///
/// Checks `options.limits` before validating UTF-8 so oversized byte slices
/// fail without scanning the whole buffer. Accepts both owned
/// (`DeserializeOwned`) and borrowed (`Deserialize<'de>`) targets.
///
/// # Errors
///
/// Returns an error when the input is invalid, a configured limit is exceeded, or the target cannot be decoded.
pub fn from_slice_with<'de, T: serde::Deserialize<'de>>(
    input: &'de [u8],
    options: &ParseOptions,
) -> Result<T, Error> {
    from_str_with(text_of_with(input, &options.limits)?, options)
}

/// The bytes as text, checking the configured limits before UTF-8 validation
/// so oversized slices fail without scanning all of them.
fn text_of_with<'a>(input: &'a [u8], limits: &Limits) -> Result<&'a str, Error> {
    limits.validate()?;
    limits.check_input(input.len()).map_err(|error| {
        Error::from(error)
            .with_span(Span::empty(0))
            .with_location(Location::new(1, 1))
    })?;
    std::str::from_utf8(input).map_err(|error| {
        Error::new(ErrorKind::InvalidEncoding, "input is not valid UTF-8").with_source(error)
    })
}

/// The crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
