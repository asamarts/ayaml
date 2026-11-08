//! Resource limits and options for hostile YAML.
//!
//! The defaults use 32 MiB of input, depth 128, a byte-weighted alias budget,
//! and a 256-byte node cost estimate. A stream may hold up to 10,000 documents;
//! the single-document compatibility profile allows one. Limits are inclusive
//! at the accepted boundary, and `max_depth` may not exceed
//! [`MAX_DEPTH_CAP`].

use std::fmt;

use crate::error::{Error, ErrorKind};

/// Maximum structured-document input bytes.
pub const MAX_INPUT_BYTES: usize = 32 * 1024 * 1024;
/// Maximum collection nesting depth.
pub const MAX_DEPTH: usize = 128;
/// The largest `max_depth` an entry point accepts. A larger configured value
/// is an error.
///
/// Parsing, composing, and the event writer keep their own stacks, but
/// Serde deserialization, [`from_value`](crate::from_value),
/// [`to_value`](crate::to_value), [`to_string`](crate::to_string),
/// [`Value::apply_merge_keys`](crate::Value::apply_merge_keys), and dropping
/// a [`Value`](crate::Value) recurse once per nesting level. At this depth
/// they fit a 2 MiB thread, the default for spawned and test threads, in
/// debug and release builds. The tightest paths measured, `from_value`,
/// `to_value`, and `to_string` over nested flow mappings with tags in a debug
/// build, need about 1.6 MiB at this depth, so 1.5 MiB overflows.
pub const MAX_DEPTH_CAP: usize = MAX_DEPTH;
/// Fixed alias-expansion allowance.
pub const MAX_ALIAS_EXPANSION_BYTES: usize = 256 * 1024 * 1024;
/// Additional alias allowance per input byte.
pub const ALIAS_BYTES_PER_INPUT_BYTE: usize = 16;
/// Estimated cost of one composed node.
pub const NODE_COST_BYTES: usize = 256;
/// Maximum bytes that tag handles expand to in one stream.
pub const MAX_TAG_EXPANSION_BYTES: usize = 64 * 1024 * 1024;
/// Default maximum documents in one stream. Single-document entry
/// points reject a second document regardless.
pub const DEFAULT_MAX_DOCUMENTS: usize = 10_000;
/// Maximum documents in the single-document compatibility profile.
pub const COMPAT_MAX_DOCUMENTS: usize = 1;

/// The resource that was exhausted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitKind {
    /// Structured-document input bytes.
    InputBytes,
    /// Collection nesting depth.
    Depth,
    /// Alias-expansion bytes.
    AliasExpansion,
    /// Documents in a stream.
    Documents,
    /// Bytes in one scalar.
    ScalarBytes,
    /// Entries in one collection.
    CollectionEntries,
    /// Events in one stream, or the tokens a reader keeps for one stream.
    Events,
    /// Anchors in one stream.
    Anchors,
    /// Alias dereferences in one stream.
    AliasDereferences,
    /// Bytes written by an emitter call.
    OutputBytes,
    /// Bytes that tag handles expand to in a stream.
    TagExpansion,
}

impl fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::InputBytes => "input bytes",
            Self::Depth => "collection depth",
            Self::AliasExpansion => "alias expansion bytes",
            Self::Documents => "documents",
            Self::ScalarBytes => "scalar bytes",
            Self::CollectionEntries => "collection entries",
            Self::Events => "events",
            Self::Anchors => "anchors",
            Self::AliasDereferences => "alias dereferences",
            Self::OutputBytes => "output bytes",
            Self::TagExpansion => "tag expansion bytes",
        };
        formatter.write_str(name)
    }
}

/// A configured limit was exceeded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LimitExceeded {
    kind: LimitKind,
    limit: u64,
    observed: u64,
}

impl LimitExceeded {
    /// Create a limit violation.
    #[must_use]
    pub const fn new(kind: LimitKind, limit: u64, observed: u64) -> Self {
        Self {
            kind,
            limit,
            observed,
        }
    }

    /// The exhausted resource.
    #[must_use]
    pub const fn kind(self) -> LimitKind {
        self.kind
    }

    /// The configured ceiling.
    #[must_use]
    pub const fn limit(self) -> u64 {
        self.limit
    }

    /// The observed or attempted value.
    #[must_use]
    pub const fn observed(self) -> u64 {
        self.observed
    }
}

impl fmt::Display for LimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} limit exceeded: limit {}, observed {}",
            self.kind, self.limit, self.observed
        )
    }
}

impl std::error::Error for LimitExceeded {}

impl From<LimitExceeded> for Error {
    fn from(value: LimitExceeded) -> Self {
        Self::new(ErrorKind::Limit, value.to_string()).with_limit(value)
    }
}

/// Limits applied to one parse.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Maximum structured-document input bytes.
    pub max_input_bytes: usize,
    /// Maximum collection nesting depth, at most [`MAX_DEPTH_CAP`].
    pub max_depth: usize,
    /// Fixed alias-expansion byte allowance.
    pub max_alias_expansion_bytes: usize,
    /// Additional alias allowance per input byte.
    pub alias_bytes_per_input_byte: usize,
    /// Estimated cost of one composed node, at least 1. Each alias is charged this much for every
    /// node it copies, on top of the text it copies, so a small cost lets more nodes through the
    /// alias budget.
    pub node_cost_bytes: usize,
    /// Maximum documents in one stream.
    pub max_documents: usize,
    /// Maximum bytes in one scalar: the larger of its text in the input and the value it reads
    /// as. A block scalar's text runs from its `|` or `>` through its last line, so its header,
    /// indentation, and trailing blank lines count. A double-quoted `\L` or `\P` takes 2 bytes of
    /// text and 3 of value.
    pub max_scalar_bytes: usize,
    /// Maximum entries in one collection.
    pub max_collection_entries: usize,
    /// Maximum events in one stream, and maximum tokens a reader keeps for one stream. The events
    /// are those [`parse_events`](crate::parse_events) yields, the stream's start and end
    /// included, and every reader counts them alike. The tokens are the input's scalars, anchors,
    /// aliases, tags, indicators, document markers, and directives. Neither count includes a
    /// line break or a comment.
    pub max_events: usize,
    /// Maximum anchors in one stream.
    pub max_anchors: usize,
    /// Maximum alias dereferences in one stream.
    pub max_alias_dereferences: usize,
    /// Maximum bytes written by one emitter call.
    pub max_output_bytes: usize,
    /// Maximum bytes that tag handles expand to in one stream. Each use of a handle counts the
    /// whole prefix it stands for, so a long `%TAG` prefix can't multiply the cost of short tags.
    pub max_tag_expansion_bytes: usize,
}

impl Default for Limits {
    /// The compatibility profile with a stream-sized document limit.
    fn default() -> Self {
        Self {
            max_documents: DEFAULT_MAX_DOCUMENTS,
            ..Self::compat()
        }
    }
}

impl Limits {
    /// The single-document compatibility profile.
    #[must_use]
    pub const fn compat() -> Self {
        Self {
            max_input_bytes: MAX_INPUT_BYTES,
            max_depth: MAX_DEPTH,
            max_alias_expansion_bytes: MAX_ALIAS_EXPANSION_BYTES,
            alias_bytes_per_input_byte: ALIAS_BYTES_PER_INPUT_BYTE,
            node_cost_bytes: NODE_COST_BYTES,
            max_documents: COMPAT_MAX_DOCUMENTS,
            max_scalar_bytes: MAX_INPUT_BYTES,
            max_collection_entries: 1_000_000,
            max_events: 1_000_000,
            max_anchors: 100_000,
            max_alias_dereferences: 100_000,
            max_output_bytes: 64 * 1024 * 1024,
            max_tag_expansion_bytes: MAX_TAG_EXPANSION_BYTES,
        }
    }

    /// Set the maximum input size.
    #[must_use]
    pub const fn with_max_input_bytes(mut self, value: usize) -> Self {
        self.max_input_bytes = value;
        self
    }

    /// Set the maximum collection nesting depth.
    #[must_use]
    pub const fn with_max_depth(mut self, value: usize) -> Self {
        self.max_depth = value;
        self
    }

    /// Set the fixed alias-expansion allowance.
    #[must_use]
    pub const fn with_max_alias_expansion_bytes(mut self, value: usize) -> Self {
        self.max_alias_expansion_bytes = value;
        self
    }

    /// Set the per-input-byte alias allowance.
    #[must_use]
    pub const fn with_alias_bytes_per_input_byte(mut self, value: usize) -> Self {
        self.alias_bytes_per_input_byte = value;
        self
    }

    /// Set the estimated cost of one composed node. Every entry point rejects 0, which would make
    /// a copied collection of nulls free and so turn the alias budget off. A cost below the
    /// memory a node takes, about 100 bytes, lets aliases hold more memory than the budget: at 1,
    /// aliases of empty nodes held about 65 times it before the budget stopped them.
    #[must_use]
    pub const fn with_node_cost_bytes(mut self, value: usize) -> Self {
        self.node_cost_bytes = value;
        self
    }

    /// Set the maximum number of documents in one stream.
    #[must_use]
    pub const fn with_max_documents(mut self, value: usize) -> Self {
        self.max_documents = value;
        self
    }

    /// Set the maximum size of one scalar, its text or its value, whichever is larger; see
    /// [`max_scalar_bytes`](Self::max_scalar_bytes).
    #[must_use]
    pub const fn with_max_scalar_bytes(mut self, value: usize) -> Self {
        self.max_scalar_bytes = value;
        self
    }

    /// Set the maximum number of entries in one collection.
    #[must_use]
    pub const fn with_max_collection_entries(mut self, value: usize) -> Self {
        self.max_collection_entries = value;
        self
    }

    /// Set the maximum number of events, and of kept tokens, in one stream; see
    /// [`max_events`](Self::max_events).
    #[must_use]
    pub const fn with_max_events(mut self, value: usize) -> Self {
        self.max_events = value;
        self
    }

    /// Set the maximum number of anchors in one stream.
    #[must_use]
    pub const fn with_max_anchors(mut self, value: usize) -> Self {
        self.max_anchors = value;
        self
    }

    /// Set the maximum number of alias dereferences in one stream.
    #[must_use]
    pub const fn with_max_alias_dereferences(mut self, value: usize) -> Self {
        self.max_alias_dereferences = value;
        self
    }

    /// Set the maximum output size for one emitter call.
    #[must_use]
    pub const fn with_max_output_bytes(mut self, value: usize) -> Self {
        self.max_output_bytes = value;
        self
    }

    /// Set the maximum bytes that tag handles expand to in one stream.
    #[must_use]
    pub const fn with_max_tag_expansion_bytes(mut self, value: usize) -> Self {
        self.max_tag_expansion_bytes = value;
        self
    }

    /// The alias-expansion budget for an input of `input_len` bytes.
    ///
    /// The input contribution is capped at `max_input_bytes`, so padding a
    /// document cannot raise the budget above roughly 768 MiB with the
    /// compatibility constants.
    #[must_use]
    pub fn expansion_budget(&self, input_len: usize) -> usize {
        let input = input_len.min(self.max_input_bytes);
        self.max_alias_expansion_bytes
            .saturating_add(self.alias_bytes_per_input_byte.saturating_mul(input))
    }

    /// Reject settings no entry point can honour, a `max_depth` above [`MAX_DEPTH_CAP`], and
    /// settings that would turn a budget off: a `node_cost_bytes` of 0 makes every node an alias
    /// copies free, so a nest of aliases to collections of nulls could grow without bound.
    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.max_depth > MAX_DEPTH_CAP {
            return Err(Error::new(
                ErrorKind::Limit,
                format!(
                    "max_depth {} is above the supported maximum of {MAX_DEPTH_CAP}",
                    self.max_depth
                ),
            ));
        }
        if self.node_cost_bytes == 0 {
            return Err(Error::new(
                ErrorKind::Limit,
                "node_cost_bytes 0 would make the nodes an alias copies free, which turns off the \
                 alias budget; the least accepted cost is 1",
            ));
        }
        Ok(())
    }

    /// Check the input size before scanning.
    pub fn check_input(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::InputBytes, self.max_input_bytes, observed)
    }

    /// Check collection depth before descending.
    pub fn check_depth(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::Depth, self.max_depth, observed)
    }

    /// Check the number of documents.
    pub fn check_documents(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::Documents, self.max_documents, observed)
    }

    /// Check the bytes that tag handles have expanded to so far.
    pub fn check_tag_expansion(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(
            LimitKind::TagExpansion,
            self.max_tag_expansion_bytes,
            observed,
        )
    }

    /// Check a scalar's byte length.
    pub fn check_scalar_bytes(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::ScalarBytes, self.max_scalar_bytes, observed)
    }

    /// Check one collection's entry count.
    pub fn check_collection_entries(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(
            LimitKind::CollectionEntries,
            self.max_collection_entries,
            observed,
        )
    }

    /// Check the event count.
    pub fn check_events(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::Events, self.max_events, observed)
    }

    /// Check the anchor count.
    pub fn check_anchors(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::Anchors, self.max_anchors, observed)
    }

    /// Check the alias-dereference count.
    pub fn check_alias_dereferences(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(
            LimitKind::AliasDereferences,
            self.max_alias_dereferences,
            observed,
        )
    }

    /// Check output bytes before a write.
    pub fn check_output_bytes(&self, observed: usize) -> Result<(), LimitExceeded> {
        check(LimitKind::OutputBytes, self.max_output_bytes, observed)
    }

    /// Check a charged node/alias expansion against an explicit budget.
    pub fn check_alias_expansion(
        &self,
        input_len: usize,
        observed: usize,
    ) -> Result<(), LimitExceeded> {
        check(
            LimitKind::AliasExpansion,
            self.expansion_budget(input_len),
            observed,
        )
    }
}

/// The alias use charged so far: dereferences and the estimated bytes they
/// materialize. One budget covers a whole stream, so a multi-document
/// stream cannot multiply it.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct AliasBudget {
    dereferences: usize,
    bytes: usize,
}

impl AliasBudget {
    /// Charge one alias to a node estimated at `size` bytes, before
    /// anything is materialized for it.
    pub(crate) fn charge(
        &mut self,
        limits: &Limits,
        input_len: usize,
        size: usize,
    ) -> Result<(), Error> {
        self.dereferences = self.dereferences.saturating_add(1);
        limits
            .check_alias_dereferences(self.dereferences)
            .map_err(Error::from)?;
        self.bytes = self.bytes.saturating_add(size);
        limits
            .check_alias_expansion(input_len, self.bytes)
            .map_err(Error::from)
    }
}

fn check(kind: LimitKind, limit: usize, observed: usize) -> Result<(), LimitExceeded> {
    if observed > limit {
        Err(LimitExceeded::new(
            kind,
            u64::try_from(limit).unwrap_or(u64::MAX),
            u64::try_from(observed).unwrap_or(u64::MAX),
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{LimitKind, Limits, MAX_ALIAS_EXPANSION_BYTES, MAX_DEPTH, MAX_INPUT_BYTES};

    #[test]
    fn compatibility_defaults_match_the_profile() {
        let limits = Limits::compat();
        assert_eq!(limits.max_input_bytes, 32 * 1024 * 1024);
        assert_eq!(limits.max_depth, MAX_DEPTH);
        assert_eq!(limits.max_alias_expansion_bytes, 256 * 1024 * 1024);
        assert_eq!(limits.alias_bytes_per_input_byte, 16);
        assert_eq!(limits.node_cost_bytes, 256);
        assert_eq!(limits.max_documents, 1);
    }

    #[test]
    fn defaults_use_a_stream_document_limit() {
        assert_eq!(
            Limits::default(),
            Limits {
                max_documents: 10_000,
                ..Limits::compat()
            }
        );
    }

    #[test]
    fn input_boundary_is_inclusive() {
        let limits = Limits::default();
        assert!(limits.check_input(MAX_INPUT_BYTES - 1).is_ok());
        assert!(limits.check_input(MAX_INPUT_BYTES).is_ok());
        let error = limits.check_input(MAX_INPUT_BYTES + 1).unwrap_err();
        assert_eq!(error.kind(), LimitKind::InputBytes);
        assert_eq!(error.limit(), MAX_INPUT_BYTES as u64);
    }

    #[test]
    fn depth_boundary_is_inclusive() {
        let limits = Limits::default();
        assert!(limits.check_depth(MAX_DEPTH).is_ok());
        assert_eq!(
            limits.check_depth(MAX_DEPTH + 1).unwrap_err().kind(),
            LimitKind::Depth
        );
    }

    #[test]
    fn alias_budget_formula_and_cap() {
        let limits = Limits::default();
        assert_eq!(limits.expansion_budget(0), MAX_ALIAS_EXPANSION_BYTES);
        assert_eq!(
            limits.expansion_budget(1024),
            MAX_ALIAS_EXPANSION_BYTES + 1024 * 16
        );
        assert_eq!(
            limits.expansion_budget(MAX_INPUT_BYTES),
            MAX_ALIAS_EXPANSION_BYTES + MAX_INPUT_BYTES * 16
        );
        assert_eq!(
            limits.expansion_budget(MAX_INPUT_BYTES + 1),
            MAX_ALIAS_EXPANSION_BYTES + MAX_INPUT_BYTES * 16
        );
    }

    #[test]
    fn alias_expansion_boundary_is_inclusive() {
        let limits = Limits::default();
        let budget = limits.expansion_budget(0);
        assert!(limits.check_alias_expansion(0, budget - 1).is_ok());
        assert!(limits.check_alias_expansion(0, budget).is_ok());
        assert_eq!(
            limits
                .check_alias_expansion(0, budget + 1)
                .unwrap_err()
                .kind(),
            LimitKind::AliasExpansion
        );
    }
}
