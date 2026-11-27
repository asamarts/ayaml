//! Compact parser state machines and explicit-stack frames (`NodeBounds`, `Run`, `BlockState`,
//! `FlowState`, `Opening`, `Call`, `Cont`, `Step`).

use crate::error::ErrorCode;
use crate::event::Anchor;
use crate::span::Span;
use crate::tag::Tag;

use super::fit_u32;

/// Where a block node that starts on a later line may sit, relative to its
/// parent at column `n` (YAML 1.2.2 section 8.2): content lines at
/// `min_indent` (n + 1) or deeper, and the `-` lines of its block sequence
/// at `sequence_indent` (`seq-spaces` + 1).
#[derive(Clone, Copy, Debug)]
pub(super) struct NodeBounds {
    pub(super) min_indent: usize,
    pub(super) sequence_indent: usize,
}

impl NodeBounds {
    /// A sequence entry's node, for an entry at column `n`.
    pub(super) fn block_in(n: usize) -> Self {
        Self {
            min_indent: n + 1,
            sequence_indent: n + 1,
        }
    }

    /// A mapping value or explicit key's node, for a key at column `n`. Its
    /// block sequence may sit at the key's column.
    pub(super) fn block_out(n: usize) -> Self {
        Self {
            min_indent: n + 1,
            sequence_indent: n,
        }
    }
}

pub(super) const DUPLICATE_PROPERTIES: &str =
    "a node cannot have duplicate tag or anchor properties";

/// Sentinel `value_index` in [`Cont::MappingImplicitKey`] when a mapping key line has no `:`.
pub(super) const NO_VALUE_INDEX: u32 = u32::MAX;

/// A node's tag and anchor, and how many tokens they take up.
pub(super) type Properties<'a> = (Option<Tag>, Option<Anchor<'a>>, usize);

/// What a malformed flow collection lacks where it fails.
pub(super) enum FlowLack {
    /// `,` or its closing bracket.
    Separator,
    /// A node, with its error code and what stands there instead.
    Node(ErrorCode, String),
}

/// A run of one logical line's tokens in the active line window.
/// Every token slice the grammar works on is one; a flow collection is
/// always on one logical line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Run {
    pub(super) line: u32,
    pub(super) start: u32,
    pub(super) end: u32,
}

impl Run {
    #[inline]
    pub(super) fn line(self) -> usize {
        self.line as usize
    }

    #[inline]
    pub(super) fn start(self) -> usize {
        self.start as usize
    }

    #[inline]
    pub(super) fn end(self) -> usize {
        self.end as usize
    }

    #[inline]
    pub(super) fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub(super) fn is_empty(self) -> bool {
        self.start == self.end
    }

    /// The tokens from `offset` on.
    #[inline]
    pub(super) fn from(self, offset: usize) -> Self {
        Self {
            start: self.start + fit_u32(offset),
            ..self
        }
    }

    /// The tokens before `offset`.
    #[inline]
    pub(super) fn until(self, offset: usize) -> Self {
        Self {
            end: self.start + fit_u32(offset),
            ..self
        }
    }

    /// The tokens in `start..end`, relative to this run.
    #[inline]
    pub(super) fn slice(self, start: usize, end: usize) -> Self {
        Self {
            line: self.line,
            start: self.start + fit_u32(start),
            end: self.start + fit_u32(end),
        }
    }
}

/// A block collection between entries.
#[derive(Debug)]
pub(crate) struct BlockState {
    pub(super) indent: u32,
    pub(super) depth: u32,
    pub(super) entries: u32,
    /// The line after the last entry parsed so far.
    pub(super) current_index: u32,
    /// The first entry's tokens, before it is parsed: a collection starts
    /// on the line, or part of the line, that opened it.
    pub(super) first: Option<Run>,
}

/// A flow collection between entries.
#[derive(Debug)]
pub(crate) struct FlowState {
    pub(super) tokens: Run,
    pub(super) start_span: Span,
    /// The next token to parse, relative to `tokens`.
    pub(super) index: u32,
    pub(super) entries: u32,
    pub(super) depth: u32,
}

/// What the lines at a document's start lead to.
pub(super) enum Opening<'a> {
    /// The stream has no more to read, or the reader stops at a second document.
    End,
    /// A line that opens no document, such as a lone `...`. The stream goes on at this line.
    NoDocument(usize),
    /// Directives of a document, with more of them or its start still to read from this line.
    Directives(usize),
    /// A document. Its root is read by `call`, or is empty; either way it stops before line
    /// `index` or the line that the call returns. `block` says whether it is a block collection.
    Root {
        call: Option<Call<'a>>,
        index: usize,
        block: bool,
    },
}

/// A grammar function to run, with its arguments. Each returns a line
/// index (the block forms) or a token index relative to its run (the
/// flow and inline forms).
pub(super) enum Call<'a> {
    Node {
        index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        bounds: NodeBounds,
    },
    SequenceFirst {
        index: usize,
        first: Run,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    SequenceItem {
        rest: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
    },
    MappingFirst {
        first: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    /// Returns the line after the value.
    MappingValue {
        value: Run,
        current_index: usize,
        indent: usize,
        depth: usize,
        allow_compact: bool,
    },
    /// Returns how many of `tokens` the node consumed.
    Inline {
        tokens: Run,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    FlowSequence {
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
    FlowExplicitMapping {
        tokens: Run,
        start: usize,
        depth: usize,
    },
    ImplicitFlowMapping {
        tokens: Run,
        start: usize,
        colon: usize,
        depth: usize,
    },
    FlowMapping {
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    },
}

/// The rest of a grammar function, waiting for a nested call's result.
#[derive(Debug)]
pub(crate) enum Cont {
    /// A node's inline content must consume the rest of line `index`.
    NodeInline {
        rest: Run,
        index: u32,
    },
    /// A sequence entry's inline content must consume `content`.
    ItemInline {
        content: Run,
        next_index: u32,
    },
    /// A block sequence entry ended at the returned line.
    Sequence(BlockState),
    /// An explicit block key ended at the returned line.
    MappingExplicitKey(BlockState),
    /// An implicit block key consumed the returned number of key tokens.
    MappingImplicitKey {
        state: BlockState,
        tokens: Run,
        value_index: u32,
    },
    /// A block mapping value ended at the returned line.
    MappingEntry(BlockState),
    /// An inline block mapping value consumed the returned number of tokens.
    ValueInline {
        value: Run,
        current_index: u32,
    },
    /// A flow sequence entry ended at the returned index; with `inline`,
    /// the index is relative to the entry.
    FlowSequenceItem {
        state: FlowState,
        inline: bool,
    },
    /// Resume a flow sequence loop after yielding to a full pull queue.
    FlowSequenceNext(FlowState),
    FlowExplicitKey {
        tokens: Run,
        start_span: Span,
        key_start: u32,
        key_end: u32,
        depth: u32,
    },
    FlowExplicitValue {
        tokens: Run,
        start_span: Span,
        value_start: u32,
        value_end: u32,
    },
    ImplicitFlowKey {
        tokens: Run,
        start_span: Span,
        key_start: u32,
        colon: u32,
        depth: u32,
    },
    ImplicitFlowValue {
        tokens: Run,
        start_span: Span,
        value_start: u32,
        value_end: u32,
    },
    FlowMappingKey {
        state: FlowState,
        key_start: u32,
    },
    FlowMappingValue {
        state: FlowState,
        value_start: u32,
    },
    /// A flow mapping entry is complete; skip a trailing `,` if present and continue.
    FlowMappingNext(FlowState),
}

/// What a grammar function does next: run a call in its place, or return.
pub(super) enum Step<'a> {
    Call(Call<'a>),
    Return(usize),
}
