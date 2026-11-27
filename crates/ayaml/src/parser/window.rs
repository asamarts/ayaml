//! Bounded on-demand logical-line and token window for single-pass streaming parsing.
//!
//! Instead of buffering every `Token` and `Line` for an entire YAML document upfront,
//! [`LineWindow`] scans small bounded batches of logical lines (`<= 128` lines and `<= 384`
//! tokens, ~12 KiB) on demand from [`Scanner`]. Batching keeps the scanner and parser hot in
//! instruction cache with plain contiguous `Vec<Line>` indexing, while flushing and reusing
//! consumed batches in O(1) keeps transient parser memory bounded by O(batch + active line)
//! rather than O(document size).

use std::panic::AssertUnwindSafe;

use crate::error::Error;
use crate::event::ScalarStyle;
use crate::limits::Limits;
use crate::position::column_on_line;
#[cfg(test)]
use crate::position::lines_at_most;
use crate::scanner::{Scanner, Token, TokenKind, UNMATCHED_FLOW};
use crate::span::Span;

use super::{Run, fit_u32, matched_flow_end, to_u32};

/// A logical line's token range in the parser's token buffer.
#[derive(Clone, Copy, Debug)]
pub(super) struct Line {
    pub(super) start: u32,
    pub(super) end: u32,
    pub(super) indent: u32,
}

impl Line {
    /// The line of `tokens[start..end]`, from `input`. Its indent is its first token's column,
    /// less any tab before that token and what follows the tab: YAML indents with spaces alone
    /// (production 63), so a tab there separates and `k:\n\tv` holds `v` at indent 0.
    pub(super) fn new(
        tokens: &[Token<'_>],
        start: usize,
        end: usize,
        input: &str,
        compat: bool,
    ) -> Result<Self, Box<Error>> {
        let first_span = tokens.get(start).map_or_else(|| Span::empty(0), |t| t.span);
        let indent = tokens.get(start).map_or(0, |token| {
            let offset = token.span.start();
            let bytes = input.as_bytes();
            let mut i = offset;
            let mut first_tab: Option<usize> = None;
            while i > 0 {
                match bytes[i - 1] {
                    b' ' => i -= 1,
                    b'\t' => {
                        i -= 1;
                        first_tab = Some(i);
                    }
                    _ => break,
                }
            }
            if i == 0 || matches!(bytes[i - 1], b'\n' | b'\r') {
                first_tab.map_or(offset - i, |tab| tab - i)
            } else {
                let column = column_on_line(input, offset, compat).saturating_sub(1);
                match first_tab {
                    Some(tab) => column.saturating_sub(offset - tab),
                    None => column,
                }
            }
        });
        Ok(Self {
            start: to_u32(start, first_span, input, compat)?,
            end: to_u32(end, first_span, input, compat)?,
            indent: to_u32(indent, first_span, input, compat)?,
        })
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
    pub(super) fn indent(self) -> usize {
        self.indent as usize
    }

    #[inline]
    pub(super) fn len(self) -> usize {
        (self.end - self.start) as usize
    }

    #[inline]
    pub(super) fn is_empty(self) -> bool {
        self.start == self.end
    }

    #[inline]
    pub(super) fn advance(&mut self, count: usize) {
        self.start = self.start.saturating_add(fit_u32(count)).min(self.end);
    }
}

/// Compact summary of a retired line for compatibility-mode indentation diagnostics (`compat` only).
#[derive(Clone, Copy, Debug)]
struct CompatParent {
    indent: u32,
    /// Byte offset of the line's first surviving token when retired, or `None` if empty.
    first_offset: Option<usize>,
    is_block_entry: bool,
}

/// Token-arena growth chunks used when a single logical line (e.g., a large flow collection)
/// grows past the bounded streaming batch window.
pub(super) const TOKEN_ARENA_CHUNK: usize = 8 * 1024;
#[cfg(test)]
pub(super) const TOKEN_ARENA_WIDE_CHUNK: usize = 16 * 1024;
pub(super) const TOKEN_ARENA_SMALL_CHUNK: usize = 64;
#[cfg(any(test, kani))]
pub(super) const TOKEN_ARENA_MID_CHUNK: usize = 256;

/// Maximum logical lines scanned per bounded refill batch (128 * 12 B = 1.5 KiB).
const WINDOW_BATCH_LINES: usize = 128;
/// Soft token target per bounded refill batch (384 * 32 B = 12 KiB).
const WINDOW_BATCH_TOKENS: usize = 384;
/// Initial token buffer capacity for tiny/single-scalar inputs (512 B).
const WINDOW_INIT_CHUNK: usize = 16;
/// Capacity above which an emptied token buffer (e.g. after a large single-line flow collection
/// retires) is released immediately rather than kept warm across subsequent block lines.
const WINDOW_SHRINK_CAP: usize = 1_024;

pub(super) fn reserve_tokens(
    tokens: &mut Vec<Token<'_>>,
    additional: usize,
    most: usize,
    span: Span,
    input: &str,
    compat: bool,
) -> Result<(), Box<Error>> {
    tokens.try_reserve_exact(additional).map_err(|_| {
        let observed = u64::try_from(tokens.len().saturating_add(additional)).unwrap_or(u64::MAX);
        let limit = u64::try_from(most).unwrap_or(u64::MAX);
        Box::new(
            Error::from(crate::limits::LimitExceeded::new(
                crate::limits::LimitKind::Events,
                limit,
                observed,
            ))
            .with_span(span)
            .with_input_location(input, compat),
        )
    })
}

#[cfg(test)]
pub(super) fn push_token<'a>(
    tokens: &mut Vec<Token<'a>>,
    token: Token<'a>,
    chunk: usize,
    most: usize,
    input: &str,
) -> Result<(), Box<Error>> {
    if tokens.len() == tokens.capacity() {
        let room = most.saturating_sub(tokens.len()).max(1);
        let additional = arena_growth(tokens.len(), chunk).min(room);
        reserve_tokens(tokens, additional, most, token.span, input, false)?;
    }
    tokens.push(token);
    Ok(())
}

/// How many tokens a full arena of `len` grows by before any estimate: its first chunk, and
/// then as many tokens as it holds.
pub(super) const fn arena_growth(len: usize, chunk: usize) -> usize {
    if len <= chunk { chunk } else { len }
}

/// The token arena capacity to reserve before scanning a mid-sized document, or `None` to grow
/// as tokens arrive.
#[cfg(any(test, kani))]
pub(super) fn mid_input_token_capacity(
    input_len: usize,
    line_count: usize,
    most: usize,
) -> Option<usize> {
    if (4097..=32768).contains(&input_len) && line_count > 0 && input_len / line_count <= 80 {
        Some((input_len / 10).min(most))
    } else {
        None
    }
}

/// How far one reservation may run ahead of the tokens already scanned.
pub(super) const TOKEN_ARENA_AHEAD: usize = 8;

/// How many tokens a full arena of `len` grows by once estimates have begun.
pub(super) const fn estimated_growth(
    len: usize,
    estimate: usize,
    chunk: usize,
    most: usize,
) -> usize {
    let sixteenth = len / 16;
    let least = if sixteenth > chunk { sixteenth } else { chunk };
    let wanted = estimate.saturating_sub(len);
    let growth = if wanted > least { wanted } else { least };
    let room = most.saturating_sub(len);
    let room = if room == 0 { 1 } else { room };
    if growth < room { growth } else { room }
}

/// Tokens to hold after scanning a prefix of a large logical line, or `None` before enough tokens
/// show the density.
pub(super) fn prefix_token_capacity(
    tokens: usize,
    consumed: usize,
    last_tokens: usize,
    last_consumed: usize,
    input_len: usize,
    most: usize,
) -> Option<usize> {
    const THRESHOLD: usize = 1_024;
    if tokens < THRESHOLD || consumed == 0 {
        return None;
    }
    let overall = tokens as u128 * input_len as u128 / consumed as u128;
    let remaining = input_len.saturating_sub(consumed) as u128;
    let recent_tokens = tokens.saturating_sub(last_tokens) as u128;
    let recent_bytes = consumed.saturating_sub(last_consumed) as u128;
    let recent = if recent_tokens > 0 && recent_bytes > 0 {
        (tokens as u128).saturating_add(recent_tokens * remaining / recent_bytes)
    } else {
        0
    };
    let estimate = overall.max(recent);
    let estimate = usize::try_from(estimate.saturating_add(estimate / 64)).unwrap_or(usize::MAX);
    let cap = input_len
        .min(most)
        .min(tokens.saturating_mul(TOKEN_ARENA_AHEAD));
    Some(estimate.min(cap).max(tokens))
}

/// Pair every flow collection's opening token with the relative token distance to its matching
/// closing token (`close - open`), making links invariant under both window compaction and
/// leading token advancement (`line.advance(1)`), reusing `stack` to avoid per-line allocations.
pub(super) fn link_flow_ends_with_stack(tokens: &mut [Token<'_>], stack: &mut Vec<usize>) {
    stack.clear();
    for index in 0..tokens.len() {
        match tokens[index].kind {
            TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. } => {
                stack.push(index);
            }
            TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                if let Some(open) = stack.pop()
                    && let Ok(delta) = u32::try_from(index - open)
                    && delta != UNMATCHED_FLOW
                {
                    match &mut tokens[open].kind {
                        TokenKind::FlowSequenceStart { end }
                        | TokenKind::FlowMappingStart { end } => *end = delta,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

/// Compute the compaction cut for shifting live lines when a batch retires partially.
/// Pure helper verified symbolically by Kani.
#[inline]
pub(super) fn compact_shift(
    dead_prefix: usize,
    tokens_len: usize,
    first_live_start: Option<usize>,
) -> usize {
    let bound = first_live_start.map_or(tokens_len, |start| start.min(tokens_len));
    dead_prefix.min(bound)
}

/// Bounded on-demand logical-line and token window fed in small cache-friendly batches from
/// [`Scanner`].
#[derive(Debug)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags represent independent stream boundary, truncation, and compat-lookback states"
)]
pub(super) struct LineWindow<'a> {
    scanner: Scanner<'a>,
    input: &'a str,
    compat: bool,
    /// Global logical line index of `lines[0]` (total lines already flushed/retired).
    base_line: usize,
    /// Cached `base_line + lines.len()` so `ensure_line`'s hot path is a single comparison.
    loaded_end: usize,
    /// Highest line index retired by the parser (flushed lazily at batch refill boundaries).
    retire_mark: usize,
    /// Active logical lines in the current batch (`base_line .. base_line + lines.len()`).
    lines: Vec<Line>,
    /// Token storage for the current batch (cleared/reused in O(1) at batch boundaries).
    tokens: Vec<Token<'a>>,
    /// Reusable stack for per-line flow partner linking (zero allocs on block-only docs).
    flow_link_stack: Vec<usize>,
    /// Recent density state for large single-line flow collections.
    last_tokens: usize,
    last_consumed: usize,
    chunk: usize,
    stream_start: Option<Span>,
    stream_end: Option<Span>,
    started: bool,
    eof: bool,
    /// Deferred scanner failure recorded after a flow opener (surfaced at truncated EOF / run end).
    ///
    /// Scanner errors hold only owned metadata (no dynamic `source`), so wrapping in
    /// `AssertUnwindSafe` preserves `EventStream` and `Deserializer`'s `UnwindSafe` auto-trait.
    scanner_error: Option<AssertUnwindSafe<Box<Error>>>,
    /// Non-flow scanner failure encountered speculatively during batch lookahead past the
    /// demanded line; surfaced only if the parser actually advances to `loaded_end()`.
    prefetch_error: Option<AssertUnwindSafe<Box<Error>>>,
    /// Whether the last scanned line was truncated mid-line by `scanner_error`.
    truncated_last_line: bool,
    /// End byte offset of the last surviving token on line `base_line - 1`, if any.
    last_retired_end: Option<usize>,
    /// Whether line `base_line - 1` contained a `TokenKind::Anchor` (`compat` mode only).
    last_retired_had_anchor: bool,
    /// Monotonic indentation stack of retired lines for `compat_indentation_message` (`compat` only).
    compat_parents: Vec<CompatParent>,
    #[cfg(test)]
    peak_tokens_len: usize,
    #[cfg(test)]
    peak_tokens_cap: usize,
}

impl<'a> LineWindow<'a> {
    pub(super) fn new(scanner: Scanner<'a>, input: &'a str, compat: bool) -> Self {
        let chunk = if input.len() <= 4096 {
            TOKEN_ARENA_SMALL_CHUNK
        } else {
            TOKEN_ARENA_CHUNK
        };
        Self {
            scanner,
            input,
            compat,
            base_line: 0,
            loaded_end: 0,
            retire_mark: 0,
            lines: Vec::new(),
            tokens: Vec::new(),
            flow_link_stack: Vec::new(),
            last_tokens: 0,
            last_consumed: 0,
            chunk,
            stream_start: None,
            stream_end: None,
            started: false,
            eof: false,
            scanner_error: None,
            prefetch_error: None,
            truncated_last_line: false,
            last_retired_end: None,
            last_retired_had_anchor: false,
            compat_parents: Vec::new(),
            #[cfg(test)]
            peak_tokens_len: 0,
            #[cfg(test)]
            peak_tokens_cap: 0,
        }
    }

    #[inline]
    pub(super) fn loaded_end(&self) -> usize {
        self.loaded_end
    }

    #[inline]
    pub(super) fn stream_end(&self) -> Option<Span> {
        self.stream_end
    }

    #[inline]
    pub(super) fn end_offset(&self) -> usize {
        self.stream_end.map_or(self.input.len(), Span::start)
    }

    #[inline]
    pub(super) fn has_scanner_error(&self) -> bool {
        self.scanner_error.is_some()
    }

    #[inline]
    pub(super) fn truncated_last_line(&self) -> bool {
        self.truncated_last_line
    }

    #[cold]
    #[inline(never)]
    pub(super) fn take_scanner_error(&mut self) -> Result<(), Box<Error>> {
        if let Some(error) = self.scanner_error.take() {
            return Err(error.0);
        }
        Ok(())
    }

    /// Consume the scanner's `StreamStart` token once and return its span (preserving BOM offset).
    pub(super) fn ensure_started(&mut self, limits: &Limits) -> Result<Span, Box<Error>> {
        if self.started {
            return Ok(self.stream_start.unwrap_or_else(|| Span::empty(0)));
        }
        self.started = true;
        match self.scanner.next_token(limits)? {
            Some(token) if matches!(token.kind, TokenKind::StreamStart) => {
                self.stream_start = Some(token.span);
                Ok(token.span)
            }
            Some(token) if matches!(token.kind, TokenKind::StreamEnd) => {
                self.stream_end = Some(token.span);
                self.eof = true;
                Ok(Span::empty(0))
            }
            None => {
                self.eof = true;
                Ok(Span::empty(0))
            }
            Some(_) => Ok(Span::empty(0)),
        }
    }

    /// Ensure logical line `index` is scanned into the active batch, returning `true` if it exists
    /// or `false` if EOF/deferred flow error occurred before `index`.
    #[inline]
    pub(super) fn ensure_line(
        &mut self,
        index: usize,
        limits: &Limits,
    ) -> Result<bool, Box<Error>> {
        if index < self.loaded_end() {
            return Ok(true);
        }
        if self.eof && self.prefetch_error.is_none() {
            return Ok(false);
        }
        self.fill_to(index, limits)
    }

    /// Mark every logical line `< target` as retired. Actual reclamation happens once per batch
    /// inside [`Self::fill_to`], keeping the per-line hot path to a single integer max.
    #[inline]
    pub(super) fn retire_before(&mut self, target: usize) {
        if target > self.retire_mark {
            self.retire_mark = target;
        }
    }

    #[cold]
    #[inline(never)]
    fn fill_to(&mut self, target: usize, limits: &Limits) -> Result<bool, Box<Error>> {
        if let Some(error) = self.prefetch_error.take() {
            return Err(error.0);
        }
        if self.eof {
            return Ok(false);
        }
        if !self.started {
            self.ensure_started(limits)?;
        }
        self.flush_retired();

        let most = limits.max_events;
        let input = self.input;
        let input_len = self.scanner.input_len();
        let compat = self.compat;

        // Scan at least through `target`, then fill up to one bounded cache-friendly batch
        // (`WINDOW_BATCH_LINES` / `WINDOW_BATCH_TOKENS`) in a tight scanner loop.
        let mut batch_lines = 0_usize;
        let batch_token_start = self.tokens.len();
        while !self.eof {
            let demanded = self.loaded_end <= target;
            if !demanded
                && (batch_lines >= WINDOW_BATCH_LINES
                    || self.tokens.len().saturating_sub(batch_token_start) >= WINDOW_BATCH_TOKENS)
            {
                break;
            }
            let line_start = self.tokens.len();
            let mut saw_flow_on_line = false;
            let mut stop_batch = false;

            loop {
                let token = match self.scanner.next_token(limits) {
                    Ok(Some(token)) => token,
                    Ok(None) => {
                        self.eof = true;
                        break;
                    }
                    Err(error) => {
                        if self.scanner.saw_flow_start() {
                            self.eof = true;
                            self.truncated_last_line = self.tokens.len() > line_start;
                            self.scanner_error = Some(AssertUnwindSafe(error));
                            break;
                        }
                        if demanded {
                            return Err(error);
                        }
                        // Speculative prefetch hit a non-flow scanner error on a future line:
                        // discard partial tokens for this line and park the error until the parser
                        // actually demands `loaded_end()`.
                        self.tokens.truncate(line_start);
                        self.eof = true;
                        self.prefetch_error = Some(AssertUnwindSafe(error));
                        return Ok(target < self.loaded_end());
                    }
                };
                match token.kind {
                    TokenKind::StreamStart => {
                        self.stream_start = Some(token.span);
                    }
                    TokenKind::StreamEnd => {
                        self.stream_end = Some(token.span);
                        self.eof = true;
                        break;
                    }
                    TokenKind::LineBreak => {
                        if self.tokens.len() > line_start {
                            break;
                        }
                    }
                    TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. } => {
                        saw_flow_on_line = true;
                        self.push_token_into_window(token, most, input_len)?;
                    }
                    TokenKind::Scalar(ref scalar)
                        if matches!(scalar.style, ScalarStyle::Literal | ScalarStyle::Folded) =>
                    {
                        self.push_token_into_window(token, most, input_len)?;
                        break;
                    }
                    _ => {
                        self.push_token_into_window(token, most, input_len)?;
                    }
                }
            }

            if self.tokens.len() > line_start {
                if saw_flow_on_line {
                    link_flow_ends_with_stack(
                        &mut self.tokens[line_start..],
                        &mut self.flow_link_stack,
                    );
                }
                let first_kind = &self.tokens[line_start].kind;
                if matches!(first_kind, TokenKind::DocumentEnd)
                    || (self.loaded_end > 0
                        && matches!(first_kind, TokenKind::DocumentStart { .. }))
                {
                    stop_batch = true;
                }
                let line = Line::new(&self.tokens, line_start, self.tokens.len(), input, compat)?;
                if self.lines.capacity() == 0 {
                    let line_cap = if input_len <= 64 {
                        4
                    } else if input_len <= 1024 {
                        32
                    } else {
                        WINDOW_BATCH_LINES
                    };
                    self.lines.reserve_exact(line_cap);
                }
                self.lines.push(line);
                self.loaded_end += 1;
                batch_lines += 1;
                if stop_batch && self.loaded_end > target {
                    break;
                }
            }
        }
        Ok(target < self.loaded_end)
    }

    fn flush_retired(&mut self) {
        let retire_to = self.retire_mark.min(self.loaded_end());
        if retire_to <= self.base_line {
            return;
        }
        let count = retire_to - self.base_line;
        if self.compat {
            for &line in &self.lines[..count] {
                if line.is_empty() {
                    push_compat_parent(
                        &mut self.compat_parents,
                        CompatParent {
                            indent: line.indent,
                            first_offset: None,
                            is_block_entry: false,
                        },
                    );
                } else {
                    let first = &self.tokens[line.start()];
                    push_compat_parent(
                        &mut self.compat_parents,
                        CompatParent {
                            indent: line.indent,
                            first_offset: Some(first.span.start()),
                            is_block_entry: matches!(first.kind, TokenKind::BlockEntry),
                        },
                    );
                }
            }
        }
        let last_retired = self.lines[count - 1];
        if last_retired.is_empty() {
            self.last_retired_end = None;
            self.last_retired_had_anchor = false;
        } else {
            self.last_retired_end = Some(self.tokens[last_retired.end() - 1].span.end());
            if self.compat {
                self.last_retired_had_anchor = self.tokens
                    [last_retired.start()..last_retired.end()]
                    .iter()
                    .any(|token| matches!(token.kind, TokenKind::Anchor));
            }
        }
        if count == self.lines.len() {
            self.lines.clear();
            self.last_tokens = 0;
            self.last_consumed = 0;
            if self.tokens.capacity() > WINDOW_SHRINK_CAP {
                self.tokens = Vec::new();
            } else {
                self.tokens.clear();
            }
        } else {
            let cut = compact_shift(
                last_retired.end(),
                self.tokens.len(),
                Some(self.lines[count].start()),
            );
            self.lines.drain(..count);
            if cut > 0 {
                self.tokens.drain(..cut);
                self.last_tokens = 0;
                self.last_consumed = 0;
                if self.tokens.capacity() > WINDOW_SHRINK_CAP
                    && self.tokens.len() <= WINDOW_BATCH_TOKENS
                {
                    self.tokens.shrink_to(WINDOW_BATCH_TOKENS);
                }
                let cut_u32 = fit_u32(cut);
                for line in &mut self.lines {
                    line.start -= cut_u32;
                    line.end -= cut_u32;
                }
            }
        }
        self.base_line = retire_to;
        self.loaded_end = self.base_line + self.lines.len();
    }

    #[inline]
    fn push_token_into_window(
        &mut self,
        token: Token<'a>,
        most: usize,
        input_len: usize,
    ) -> Result<(), Box<Error>> {
        if self.tokens.len() == self.tokens.capacity() {
            self.grow_tokens(token.span, most, input_len)?;
        }
        self.tokens.push(token);
        #[cfg(test)]
        {
            self.peak_tokens_len = self.peak_tokens_len.max(self.tokens.len());
            self.peak_tokens_cap = self.peak_tokens_cap.max(self.tokens.capacity());
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    fn grow_tokens(&mut self, span: Span, most: usize, input_len: usize) -> Result<(), Box<Error>> {
        let len = self.tokens.len();
        let room = most.saturating_sub(len).max(1);
        let additional = if len == 0 {
            let init = if (65..=1024).contains(&input_len) {
                64
            } else {
                WINDOW_INIT_CHUNK
            };
            init.min(room)
        } else if len < WINDOW_BATCH_TOKENS {
            let target = if input_len <= 1024 && len < 64 {
                64
            } else {
                WINDOW_BATCH_TOKENS + 64
            };
            target.saturating_sub(len).max(1).min(room)
        } else if len < WINDOW_SHRINK_CAP {
            // Stay at or below `WINDOW_SHRINK_CAP` for medium-width lines (`448..1024` tokens) so
            // the token buffer is reused across batches instead of jumping to `TOKEN_ARENA_CHUNK`
            // and immediately freeing on retirement.
            WINDOW_SHRINK_CAP.saturating_sub(len).max(1).min(room)
        } else if let Some(capacity) = prefix_token_capacity(
            len + 1,
            span.end(),
            self.last_tokens,
            if self.last_tokens == 0 {
                self.tokens.first().map_or(0, |t| t.span.start())
            } else {
                self.last_consumed
            },
            input_len,
            most,
        ) {
            self.last_tokens = len + 1;
            self.last_consumed = span.end();
            estimated_growth(len, capacity, self.chunk, most)
        } else {
            arena_growth(len, self.chunk).min(room)
        };
        reserve_tokens(
            &mut self.tokens,
            additional,
            most,
            span,
            self.input,
            self.compat,
        )
    }

    /// Nearest preceding line `< index` whose indent is strictly less than `indent`, returning its
    /// `(first_token_offset, is_block_entry)` if `indent == parent.indent + 1` and non-empty.
    #[cold]
    pub(super) fn compat_parent(&self, index: usize, indent: usize) -> Option<(usize, bool)> {
        let active_end = index.saturating_sub(self.base_line).min(self.lines.len());
        for &line in self.lines[..active_end].iter().rev() {
            if line.indent() < indent {
                if indent != line.indent() + 1 || line.is_empty() {
                    return None;
                }
                let token = &self.tokens[line.start()];
                return Some((
                    token.span.start(),
                    matches!(token.kind, TokenKind::BlockEntry),
                ));
            }
        }
        let parent = self
            .compat_parents
            .iter()
            .rev()
            .find(|parent| (parent.indent as usize) < indent)?;
        if indent != (parent.indent as usize) + 1 {
            return None;
        }
        parent
            .first_offset
            .map(|offset| (offset, parent.is_block_entry))
    }

    #[inline]
    pub(super) fn get_line(&self, index: usize) -> Option<Line> {
        let offset = index.checked_sub(self.base_line)?;
        self.lines.get(offset).copied()
    }

    #[inline]
    pub(super) fn line(&self, index: usize) -> Line {
        debug_assert!(
            index >= self.base_line && index < self.loaded_end(),
            "line {index} outside active window {}..{}",
            self.base_line,
            self.loaded_end()
        );
        self.lines[index - self.base_line]
    }

    #[inline]
    pub(super) fn line_slice(&self, line: Line) -> &[Token<'a>] {
        &self.tokens[line.start()..line.end()]
    }

    #[inline]
    pub(super) fn line_tokens(&self, index: usize) -> &[Token<'a>] {
        self.line_slice(self.line(index))
    }

    #[inline]
    pub(super) fn line_first(&self, index: usize) -> Option<&Token<'a>> {
        let line = self.get_line(index)?;
        if line.is_empty() {
            return None;
        }
        self.tokens.get(line.start())
    }

    pub(super) fn take_line_first_span(&mut self, index: usize) -> Option<Span> {
        let offset = index.checked_sub(self.base_line)?;
        let line = self.lines.get_mut(offset)?;
        if line.is_empty() {
            return None;
        }
        let span = self.tokens[line.start()].span;
        line.advance(1);
        Some(span)
    }

    #[inline]
    pub(super) fn run_tokens(&self, run: Run) -> &[Token<'a>] {
        let line = self.line(run.line());
        &self.tokens[line.start() + run.start()..line.start() + run.end()]
    }

    #[inline]
    pub(super) fn token_mut(&mut self, run: Run, offset: usize) -> &mut Token<'a> {
        let abs = self.line(run.line()).start() + run.start() + offset;
        &mut self.tokens[abs]
    }

    /// End byte offset of line `index`'s last surviving token, supporting both active lines and the
    /// immediately retired line `base_line - 1`.
    #[inline]
    pub(super) fn line_last_end(&self, index: usize) -> Option<usize> {
        if let Some(offset) = index.checked_sub(self.base_line) {
            let line = self.lines.get(offset)?;
            if line.is_empty() {
                return None;
            }
            return line
                .end()
                .checked_sub(1)
                .and_then(|end| self.tokens.get(end))
                .map(|token| token.span.end());
        }
        if index + 1 == self.base_line {
            return self.last_retired_end;
        }
        debug_assert!(
            false,
            "line_last_end requested for retired line {index} (base_line = {})",
            self.base_line
        );
        None
    }

    /// Whether line `index` contained a `TokenKind::Anchor` token (`compat` `&a::\n|` check).
    #[cold]
    pub(super) fn line_had_anchor(&self, index: usize) -> bool {
        if index >= self.base_line {
            return self
                .line_tokens(index)
                .iter()
                .any(|token| matches!(token.kind, TokenKind::Anchor));
        }
        if index + 1 == self.base_line {
            return self.last_retired_had_anchor;
        }
        false
    }

    /// Release all buffered lines, tokens, and auxiliary tables immediately.
    pub(super) fn release(&mut self) {
        self.lines = Vec::new();
        self.loaded_end = self.base_line;
        self.tokens = Vec::new();
        self.flow_link_stack = Vec::new();
        self.compat_parents = Vec::new();
    }

    /// Current tokens held in the active batch window.
    #[inline]
    pub(super) fn tokens_len(&self) -> usize {
        self.tokens.len()
    }

    #[cfg(test)]
    pub(super) fn tokens_cap(&self) -> usize {
        self.tokens.capacity()
    }

    #[cfg(test)]
    pub(super) fn peak_tokens_len(&self) -> usize {
        self.peak_tokens_len
    }

    #[cfg(test)]
    pub(super) fn peak_tokens_cap(&self) -> usize {
        self.peak_tokens_cap
    }

    /// Whether the currently buffered logical line(s) hold a large single-line flow shape that
    /// benefits from building the root tree whole and releasing the token buffer before the Serde
    /// target reads.
    ///
    /// Multi-line block documents only keep one bounded batch (`<= ~448` tokens, ~14 KiB) in the
    /// window at any time, so this returns `false` immediately without scanning.
    pub(super) fn should_build_whole(&self, drop_global_tags: bool) -> bool {
        if self.tokens.len() < 1_024 {
            return false;
        }
        let large_span = self.tokens.len() / 4;
        let mut anchors = 0_usize;
        for &line in &self.lines {
            let line_tokens = &self.tokens[line.start()..line.end()];
            for (index, token) in line_tokens.iter().enumerate() {
                match &token.kind {
                    TokenKind::Anchor => {
                        anchors += 1;
                        if next_node_is_large(line_tokens, index + 1, large_span) {
                            return true;
                        }
                    }
                    TokenKind::Tag(tag)
                        if !(tag.is_core_collection()
                            || tag.is_core_scalar()
                            || (drop_global_tags && tag.is_non_core_global())) =>
                    {
                        if next_node_is_large(line_tokens, index + 1, large_span) {
                            return true;
                        }
                    }
                    TokenKind::Key => {
                        if next_node_is_large(line_tokens, index + 1, large_span) {
                            return true;
                        }
                    }
                    TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                        if let Some(delta) = matched_flow_end(*end)
                            && delta >= large_span
                            && index
                                .checked_add(delta)
                                .and_then(|close| close.checked_add(1))
                                .and_then(|next| line_tokens.get(next))
                                .is_some_and(|t| matches!(t.kind, TokenKind::Value))
                        {
                            return true;
                        }
                    }
                    _ => {}
                }
            }
        }
        anchors >= 16 && anchors.saturating_mul(32) >= self.tokens.len()
    }
}

#[cold]
fn push_compat_parent(parents: &mut Vec<CompatParent>, parent: CompatParent) {
    while parents
        .last()
        .is_some_and(|top| top.indent >= parent.indent)
    {
        parents.pop();
    }
    parents.push(parent);
}

fn next_node_is_large(line_tokens: &[Token<'_>], mut index: usize, large_span: usize) -> bool {
    while matches!(
        line_tokens.get(index).map(|t| &t.kind),
        Some(TokenKind::Anchor | TokenKind::Tag(_))
    ) {
        index += 1;
    }
    if let Some(TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end }) =
        line_tokens.get(index).map(|t| &t.kind)
    {
        return matched_flow_end(*end).is_some_and(|delta| delta >= large_span);
    }
    false
}

/// Whole-document logical-line reader retained for arena-growth unit tests.
#[cfg(test)]
pub(super) struct Lines<'a> {
    pub(super) tokens: Vec<Token<'a>>,
    pub(super) growths: usize,
}

#[cfg(test)]
pub(super) fn read_lines<'a>(
    scanner: &mut Scanner<'a>,
    limits: &Limits,
) -> Result<Lines<'a>, Box<Error>> {
    let input = scanner.input();
    let input_len = scanner.input_len();
    let compat = scanner.is_compat();
    let most = limits.max_events;
    let mut mid_capacity = None;
    let (initial_chunk, chunk) = match input_len {
        0..=4096 => (TOKEN_ARENA_SMALL_CHUNK, TOKEN_ARENA_SMALL_CHUNK),
        4097..=32768 => {
            let line_count = lines_at_most(input.as_bytes());
            mid_capacity = mid_input_token_capacity(input_len, line_count, most);
            (TOKEN_ARENA_MID_CHUNK, TOKEN_ARENA_MID_CHUNK)
        }
        _ => {
            let sample = &input.as_bytes()[..4096];
            let sample_lines = lines_at_most(sample);
            if sample_lines == 0 || sample.len() / sample_lines > 80 {
                (TOKEN_ARENA_SMALL_CHUNK, TOKEN_ARENA_CHUNK)
            } else if sample.len() / sample_lines <= 10 {
                (TOKEN_ARENA_WIDE_CHUNK, TOKEN_ARENA_WIDE_CHUNK)
            } else {
                (TOKEN_ARENA_CHUNK, TOKEN_ARENA_CHUNK)
            }
        }
    };
    let mut tokens = Vec::new();
    let mut flow_stack = Vec::new();
    if let Some(capacity) = mid_capacity {
        reserve_tokens(&mut tokens, capacity, most, Span::empty(0), input, compat)?;
    }
    let mut last_tokens = 0_usize;
    let mut last_consumed = 0_usize;
    let mut line_start = 0;
    let mut saw_flow_on_line = false;
    let mut growths = 0;
    loop {
        let token = match scanner.next_token(limits) {
            Ok(Some(token)) => token,
            Ok(None) => break,
            Err(error) => {
                if !scanner.saw_flow_start() {
                    return Err(error);
                }
                break;
            }
        };
        match token.kind {
            TokenKind::StreamStart => {}
            TokenKind::StreamEnd => break,
            TokenKind::LineBreak => {
                if tokens.len() > line_start {
                    if saw_flow_on_line {
                        link_flow_ends_with_stack(&mut tokens[line_start..], &mut flow_stack);
                        saw_flow_on_line = false;
                    }
                    let _ = Line::new(&tokens, line_start, tokens.len(), input, compat)?;
                    line_start = tokens.len();
                }
            }
            _ => {
                let block_scalar = token.kind.scalar_style().is_some_and(|style| {
                    matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                });
                saw_flow_on_line |= matches!(
                    token.kind,
                    TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. }
                );
                let consumed = token.span.end();
                if tokens.is_empty() {
                    last_consumed = token.span.start();
                }
                let capacity_before = tokens.capacity();
                if tokens.len() == tokens.capacity() {
                    let room = most.saturating_sub(tokens.len()).max(1);
                    let step_chunk = if tokens.is_empty() {
                        initial_chunk
                    } else {
                        chunk
                    };
                    let additional = if let Some(capacity) = prefix_token_capacity(
                        tokens.len() + 1,
                        consumed,
                        last_tokens,
                        last_consumed,
                        input_len,
                        most,
                    ) {
                        last_tokens = tokens.len() + 1;
                        last_consumed = consumed;
                        estimated_growth(tokens.len(), capacity, step_chunk, most)
                    } else {
                        arena_growth(tokens.len(), step_chunk).min(room)
                    };
                    reserve_tokens(&mut tokens, additional, most, token.span, input, compat)?;
                }
                tokens.push(token);
                growths += usize::from(tokens.capacity() != capacity_before);
                if block_scalar {
                    if saw_flow_on_line {
                        link_flow_ends_with_stack(&mut tokens[line_start..], &mut flow_stack);
                        saw_flow_on_line = false;
                    }
                    let _ = Line::new(&tokens, line_start, tokens.len(), input, compat)?;
                    line_start = tokens.len();
                }
            }
        }
    }
    if tokens.len() > line_start {
        if saw_flow_on_line {
            link_flow_ends_with_stack(&mut tokens[line_start..], &mut flow_stack);
        }
        let _ = Line::new(&tokens, line_start, tokens.len(), input, compat)?;
    }
    if tokens.len() <= 8192 {
        tokens.shrink_to_fit();
    }
    Ok(Lines { tokens, growths })
}
