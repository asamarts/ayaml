use super::{
    Anchor, BlockState, Call, CollectionStyle, Cont, Cow, DUPLICATE_PROPERTIES, Error, ErrorCode,
    Event, FlowLack, NO_VALUE_INDEX, NodeBounds, Parser, Run, ScalarStyle, Span, Step, Tag,
    TokenKind, fit_u32, flow_brackets_match, has_line_break, has_value_indicator, is_mapping_line,
    to_u32, value_indicator_index,
};

impl<'a> Parser<'a> {
    #[allow(
        clippy::too_many_arguments,
        reason = "the grammar routine takes its full parsed state explicitly so recursion stays heap-based"
    )]
    pub(super) fn node(
        &mut self,
        mut index: usize,
        mut indent: usize,
        depth: usize,
        mut tag: Option<Tag>,
        mut anchor: Option<Anchor<'a>>,
        bounds: NodeBounds,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let span = self.empty_span(index);
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(span, error))?;
        loop {
            let line = self.line_run(index);
            let (line_tag, line_anchor, rest_start) = self.properties(line)?;
            let inherited_tag = tag.clone();
            let inherited_anchor = anchor.clone();
            tag = line_tag.clone().or(tag);
            anchor = line_anchor.clone().or(anchor);
            let rest = line.from(rest_start);
            let duplicate = (inherited_tag.is_some() && line_tag.is_some())
                || (inherited_anchor.is_some() && line_anchor.is_some());
            if rest.is_empty() {
                if duplicate {
                    return Err(self.error(
                        self.token(line, 0).span,
                        ErrorCode::DuplicateNodeProperties,
                        DUPLICATE_PROPERTIES,
                    ));
                }
                let next_index = index + 1;
                let has_properties = tag.is_some() || anchor.is_some();
                let next_indent = self.lines.get(next_index).map_or(0, |line| line.indent());
                // After a property-only line, a `-` line may start the node's
                // block sequence down to the parent's bound: a mapping value's
                // sequence may sit at the key's column, a sequence entry's
                // must be indented past its `-`.
                let next_is_indentless_sequence = next_indent >= bounds.sequence_indent
                    && self
                        .line_starts_with(next_index, |kind| matches!(kind, TokenKind::BlockEntry));
                // A block scalar header may sit at any column here, as in
                // libyaml; YAML 1.2.2 wants `min_indent`.
                let next_is_block_scalar = self.line_starts_with(next_index, |kind| {
                    kind.scalar_style().is_some_and(|style| {
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    })
                });
                // Other content may sit left of the property line, down to
                // the parent's bound (`-\n    !t\n  x`).
                let next_is_node = next_index < self.lines.len()
                    && (next_indent >= indent.min(bounds.min_indent)
                        || next_is_indentless_sequence
                        || next_is_block_scalar)
                    && !self.line_starts_with(next_index, |kind| {
                        matches!(
                            kind,
                            TokenKind::DocumentStart { .. } | TokenKind::DocumentEnd
                        )
                    });
                if has_properties && next_is_node {
                    index = next_index;
                    indent = self.lines[index].indent();
                    continue;
                }
                self.check_truncated_eof(next_index)?;
                let span = self.empty_span(index);
                self.emit_null_scalar_with(span, tag, anchor)?;
                return Ok(Step::Return(index + 1));
            }

            if matches!(self.token(rest, 0).kind, TokenKind::BlockEntry) {
                if line_tag.is_some() || line_anchor.is_some() {
                    return Err(self.error(
                        self.token(rest, 0).span,
                        ErrorCode::MisplacedNodeProperties,
                        "properties cannot precede a compact block sequence",
                    ));
                }
                return Ok(Step::Call(Call::SequenceFirst {
                    index,
                    first: rest,
                    indent,
                    depth,
                    tag,
                    anchor,
                }));
            }
            if is_mapping_line(self.tokens(rest)) {
                // Properties on the same line as a block mapping key belong to
                // the key. A mapping's own properties must be on a preceding
                // property-only line or an enclosing flow collection.
                if line_tag.is_some() || line_anchor.is_some() {
                    return Ok(Step::Call(Call::MappingFirst {
                        first: line,
                        next_index: index + 1,
                        indent,
                        depth,
                        tag: inherited_tag,
                        anchor: inherited_anchor,
                    }));
                }
                return Ok(Step::Call(Call::MappingFirst {
                    first: rest,
                    next_index: index + 1,
                    indent,
                    depth,
                    tag,
                    anchor,
                }));
            }
            if duplicate {
                return Err(self.error(
                    self.token(rest, 0).span,
                    ErrorCode::DuplicateNodeProperties,
                    DUPLICATE_PROPERTIES,
                ));
            }
            stack.push(Cont::NodeInline {
                rest,
                index: fit_u32(index),
            });
            return Ok(Step::Call(Call::Inline {
                tokens: rest,
                depth,
                tag,
                anchor,
            }));
        }
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the grammar routine takes its full parsed state explicitly so recursion stays heap-based"
    )]
    pub(super) fn sequence_first(
        &mut self,
        index: usize,
        first: Run,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let start_span = self.token(first, 0).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::SequenceStart {
            tag,
            anchor,
            style: CollectionStyle::Block,
            span: start_span,
        })?;
        let state = BlockState {
            indent: to_u32(indent, start_span, self.input, self.compat)?,
            depth: fit_u32(depth),
            entries: 0,
            current_index: fit_u32(index + 1),
            first: Some(first.from(1)),
        };
        self.sequence_loop(state, stack)
    }

    pub(super) fn sequence_loop(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        loop {
            let (rest, next_index) = if let Some(rest) = state.first.take() {
                (rest, state.current_index as usize)
            } else {
                let current_index = state.current_index as usize;
                if current_index >= self.lines.len()
                    || self.lines[current_index].indent() != state.indent as usize
                    || !self.line_starts_with(current_index, |kind| {
                        matches!(kind, TokenKind::BlockEntry)
                    })
                {
                    self.check_truncated_eof(current_index)?;
                    self.emit(Event::SequenceEnd {
                        span: self.last_span(current_index),
                    })?;
                    return Ok(Step::Return(current_index));
                }
                (self.line_run(current_index).from(1), current_index + 1)
            };
            state.entries += 1;
            let span = self.last_span(state.current_index as usize);
            self.limits
                .check_collection_entries(state.entries as usize)
                .map_err(|error| self.limit_at(span, error))?;
            // The overwhelmingly common block-sequence entry is one scalar.
            // Emit it inline so a wide sequence does not pay a continuation
            // stack push/pop and nested parser call for every item.
            if self.is_single_scalar(rest) {
                self.emit_scalar_token(rest, 0, None, None)?;
                state.current_index = fit_u32(next_index);
                // A reader that takes the events one at a time gets its turn here, so that
                // they don't all wait for it at once.
                if self.sink.is_full() {
                    stack.push(Cont::Sequence(state));
                    return Ok(Step::Return(next_index));
                }
                continue;
            }
            let (indent, depth) = (state.indent as usize, state.depth as usize);
            stack.push(Cont::Sequence(state));
            return Ok(Step::Call(Call::SequenceItem {
                rest,
                next_index,
                indent,
                depth: depth + 1,
            }));
        }
    }

    /// Whether `run` is a single scalar token that needs no property or nested-node work.
    #[inline]
    pub(super) fn is_single_scalar(&self, run: Run) -> bool {
        run.len() == 1
            && matches!(
                self.token(run, 0).kind,
                TokenKind::PlainScalar
                    | TokenKind::SingleQuotedScalar
                    | TokenKind::DoubleQuotedScalar
                    | TokenKind::Scalar(_)
            )
    }

    pub(super) fn sequence_item(
        &mut self,
        rest: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        if rest.is_empty() {
            if next_index < self.lines.len() && self.lines[next_index].indent() > indent {
                return Ok(Step::Call(Call::Node {
                    index: next_index,
                    indent: self.lines[next_index].indent(),
                    depth,
                    tag: None,
                    anchor: None,
                    bounds: NodeBounds::block_in(indent),
                }));
            }
            self.check_truncated_eof(next_index)?;
            let span = self.empty_span(next_index);
            self.emit_null_scalar(span)?;
            return Ok(Step::Return(next_index));
        }
        let (tag, anchor, rest_start) = self.properties(rest)?;
        let content = rest.from(rest_start);
        if content.is_empty() {
            if tag.is_some() || anchor.is_some() {
                if next_index < self.lines.len() && self.lines[next_index].indent() > indent {
                    return Ok(Step::Call(Call::Node {
                        index: next_index,
                        indent: self.lines[next_index].indent(),
                        depth,
                        tag,
                        anchor,
                        bounds: NodeBounds::block_in(indent),
                    }));
                }
                self.check_truncated_eof(next_index)?;
                let span = self.empty_span(next_index);
                self.emit_null_scalar_with(span, tag, anchor)?;
                return Ok(Step::Return(next_index));
            }
            let span = self.empty_span(next_index);
            self.emit_null_scalar(span)?;
            return Ok(Step::Return(next_index));
        }
        let content_first = self.token(content, 0);
        if matches!(content_first.kind, TokenKind::BlockEntry) {
            if tag.is_some() || anchor.is_some() {
                return Err(self.error(
                    content_first.span,
                    ErrorCode::MisplacedNodeProperties,
                    "properties cannot precede a compact block sequence",
                ));
            }
            let nested_indent = self
                .column_at(content.line(), content_first.span.start())
                .saturating_sub(1);
            return Ok(Step::Call(Call::SequenceFirst {
                index: next_index.saturating_sub(1),
                first: content,
                indent: nested_indent,
                depth,
                tag,
                anchor,
            }));
        }
        let explicit_key = matches!(content_first.kind, TokenKind::Key);
        if explicit_key || has_value_indicator(self.tokens(content)) {
            if explicit_key && (tag.is_some() || anchor.is_some()) {
                return Err(self.error(
                    self.token(content, 0).span,
                    ErrorCode::MisplacedNodeProperties,
                    "properties cannot precede an explicit mapping key",
                ));
            }
            // On a sequence item, properties before the first block key belong
            // to that key, not to the mapping node, so the mapping starts at
            // the first property (`- &x a: b` has its keys at column 2).
            let rest_start = self.token(rest, 0).span.start();
            let mapping_indent = self.column_at(rest.line(), rest_start).saturating_sub(1);
            return Ok(Step::Call(Call::MappingFirst {
                first: rest,
                next_index,
                indent: mapping_indent,
                depth,
                tag: None,
                anchor: None,
            }));
        }
        stack.push(Cont::ItemInline {
            content,
            next_index: fit_u32(next_index),
        });
        Ok(Step::Call(Call::Inline {
            tokens: content,
            depth,
            tag,
            anchor,
        }))
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the grammar routine takes its full parsed state explicitly so recursion stays heap-based"
    )]
    pub(super) fn mapping_first(
        &mut self,
        first: Run,
        next_index: usize,
        indent: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let start_span = self.token(first, 0).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag,
            anchor,
            style: CollectionStyle::Block,
            span: start_span,
        })?;
        let state = BlockState {
            indent: to_u32(indent, start_span, self.input, self.compat)?,
            depth: fit_u32(depth),
            entries: 0,
            current_index: fit_u32(next_index),
            first: Some(first),
        };
        self.mapping_loop(state, stack)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the block mapping loop keeps its inline fast paths contiguous"
    )]
    pub(super) fn mapping_loop(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        loop {
            let tokens = if let Some(first) = state.first.take() {
                first
            } else {
                let current_index = state.current_index as usize;
                if current_index >= self.lines.len()
                    || self.lines[current_index].indent() != state.indent as usize
                    || !is_mapping_line(self.line_tokens(current_index))
                {
                    if self.is_truncated_mapping_key_line(current_index, state.indent as usize) {
                        state.current_index += 1;
                        self.line_run(current_index)
                    } else {
                        self.check_truncated_eof(current_index)?;
                        self.emit(Event::MappingEnd {
                            span: self.last_span(current_index),
                        })?;
                        return Ok(Step::Return(current_index));
                    }
                } else {
                    state.current_index += 1;
                    self.line_run(current_index)
                }
            };
            let (indent, depth) = (state.indent as usize, state.depth as usize);

            // A very common explicit entry is `? key` followed by `: value`,
            // both single plain scalars. Emit the pair inline instead of paying
            // continuation frames for the key and value nodes.
            if matches!(self.token(tokens, 0).kind, TokenKind::Key) {
                let key_rest = tokens.from(1);
                let current_index = state.current_index as usize;
                if self.is_single_scalar(key_rest)
                    && current_index < self.lines.len()
                    && self.lines[current_index].indent() == indent
                    && self.line_starts_with(current_index, |kind| matches!(kind, TokenKind::Value))
                {
                    let value_run = self.line_run(current_index).from(1);
                    if self.is_single_scalar(value_run) {
                        let key_span = self.token(key_rest, 0).span;
                        state.entries += 1;
                        self.limits
                            .check_collection_entries(state.entries as usize)
                            .map_err(|error| self.limit_at(key_span, error))?;
                        self.emit_scalar_token(key_rest, 0, None, None)?;
                        self.emit_scalar_token(value_run, 0, None, None)?;
                        state.current_index = fit_u32(current_index + 1);
                        if self.sink.is_full() {
                            stack.push(Cont::MappingEntry(state));
                            return Ok(Step::Return(current_index + 1));
                        }
                        continue;
                    }
                }
            }

            if matches!(self.token(tokens, 0).kind, TokenKind::Key) {
                // `? <node>`: the rest of the line is a block-indented node, so a
                // `:` on this line belongs to a compact mapping inside the key.
                // The entry's value is a separate `:` line at this indentation.
                let key_rest = tokens.from(1);
                let current_index = state.current_index as usize;
                if key_rest.is_empty() {
                    if current_index < self.lines.len() {
                        let next_indent = self.lines[current_index].indent();
                        let indentless_sequence = next_indent == indent
                            && self.line_starts_with(current_index, |kind| {
                                matches!(kind, TokenKind::BlockEntry)
                            });
                        if next_indent > indent || indentless_sequence {
                            stack.push(Cont::MappingExplicitKey(state));
                            return Ok(Step::Call(Call::Node {
                                index: current_index,
                                indent: next_indent,
                                depth: depth + 1,
                                tag: None,
                                anchor: None,
                                bounds: NodeBounds::block_out(indent),
                            }));
                        }
                    }
                    self.check_truncated_eof(current_index)?;
                    let span = Span::empty(self.token(tokens, 0).span.end());
                    self.emit_null_scalar(span)?;
                    return self.mapping_after_explicit_key(state, stack);
                }
                // Properties alone on the `?` line may be followed by an indentless
                // sequence, as on a mapping value's line.
                let (tag, anchor, rest_start) = self.properties(key_rest)?;
                if rest_start == key_rest.len()
                    && current_index < self.lines.len()
                    && self.lines[current_index].indent() == indent
                    && self.line_starts_with(current_index, |kind| {
                        matches!(kind, TokenKind::BlockEntry)
                    })
                {
                    stack.push(Cont::MappingExplicitKey(state));
                    return Ok(Step::Call(Call::Node {
                        index: current_index,
                        indent,
                        depth: depth + 1,
                        tag,
                        anchor,
                        bounds: NodeBounds::block_out(indent),
                    }));
                }
                stack.push(Cont::MappingExplicitKey(state));
                return Ok(Step::Call(Call::SequenceItem {
                    rest: key_rest,
                    next_index: current_index,
                    indent,
                    depth: depth + 1,
                }));
            }

            // Fast path for the overwhelmingly common implicit single-line scalar key entry:
            // `PlainScalar`, `SingleQuotedScalar`, and `DoubleQuotedScalar` are guaranteed
            // single-line, and a byte distance <= 1024 to `:` guarantees <= 1024 characters
            // without counting scalar columns.
            if tokens.len() >= 2 && matches!(self.token(tokens, 1).kind, TokenKind::Value) {
                let first = self.token(tokens, 0);
                let key_span = first.span;
                let single_line_scalar = matches!(
                    first.kind,
                    TokenKind::PlainScalar
                        | TokenKind::SingleQuotedScalar
                        | TokenKind::DoubleQuotedScalar
                );
                if single_line_scalar {
                    let colon_start = self.token(tokens, 1).span.start();
                    if colon_start.saturating_sub(key_span.start()) <= 1024 {
                        self.emit_scalar_token(tokens, 0, None, None)?;
                        let value_run = tokens.from(2);
                        if self.is_single_scalar(value_run) {
                            state.entries += 1;
                            self.limits
                                .check_collection_entries(state.entries as usize)
                                .map_err(|error| self.limit_at(key_span, error))?;
                            self.emit_scalar_token(value_run, 0, None, None)?;
                            if self.sink.is_full() {
                                let next_index = state.current_index as usize;
                                stack.push(Cont::MappingEntry(state));
                                return Ok(Step::Return(next_index));
                            }
                            continue;
                        }
                        return self.mapping_after_implicit_key(state, tokens, Some(1), stack);
                    }
                }
            }

            let value_index = value_indicator_index(self.tokens(tokens));
            let key = value_index.map_or(tokens, |index| tokens.until(index));
            if !key.is_empty() {
                let key_tokens = self.tokens(key);
                let first_start = key_tokens[0].span.start();
                let last_end = key_tokens[key_tokens.len() - 1].span.end();
                if has_line_break(&self.input.as_bytes()[first_start..last_end]) {
                    if flow_brackets_match(key_tokens) {
                        return Err(self.error(
                            key_tokens[0].span,
                            ErrorCode::InvalidImplicitKey,
                            "implicit mapping keys must be on one line",
                        ));
                    }
                } else if let Some(value_index) = value_index
                    && !self.implicit_key_fits_range(
                        first_start,
                        self.token(tokens, value_index).span.start(),
                    )
                {
                    return Err(self.error(
                        key_tokens[0].span,
                        ErrorCode::InvalidImplicitKey,
                        "an implicit mapping key may be at most 1024 characters long",
                    ));
                }
            }
            if key.is_empty() {
                let span = Span::empty(self.token(tokens, 0).span.start());
                self.emit_null_scalar(span)?;
                return self.mapping_after_implicit_key(state, tokens, value_index, stack);
            }
            stack.push(Cont::MappingImplicitKey {
                state,
                tokens,
                value_index: value_index.map_or(NO_VALUE_INDEX, fit_u32),
            });
            // The key is inside the mapping, as an explicit key and a value are.
            return Ok(Step::Call(Call::Inline {
                tokens: key,
                depth: depth + 1,
                tag: None,
                anchor: None,
            }));
        }
    }

    pub(super) fn mapping_after_explicit_key(
        &mut self,
        mut state: BlockState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        state.entries += 1;
        let current_index = state.current_index as usize;
        let span = self.empty_span(current_index);
        self.limits
            .check_collection_entries(state.entries as usize)
            .map_err(|error| self.limit_at(span, error))?;
        let has_value_line = current_index < self.lines.len()
            && self.lines[current_index].indent() == state.indent as usize
            && self.line_starts_with(current_index, |kind| matches!(kind, TokenKind::Value));
        if has_value_line {
            let value = self.line_run(current_index).from(1);
            state.current_index += 1;
            let (current_index, indent, depth) = (
                state.current_index as usize,
                state.indent as usize,
                state.depth as usize,
            );
            stack.push(Cont::MappingEntry(state));
            return Ok(Step::Call(Call::MappingValue {
                value,
                current_index,
                indent,
                depth,
                allow_compact: true,
            }));
        }
        self.check_truncated_eof(current_index)?;
        let span = self.empty_span(current_index);
        self.emit_null_scalar(span)?;
        // Continue through the driver rather than calling `mapping_loop`:
        // a run of `?` entries must not nest a native frame per entry.
        stack.push(Cont::MappingEntry(state));
        Ok(Step::Return(current_index))
    }

    pub(super) fn mapping_after_implicit_key(
        &mut self,
        mut state: BlockState,
        tokens: Run,
        value_index: Option<usize>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let Some(value_index) = value_index else {
            self.check_truncated_run_end(tokens, tokens.len())?;
            return Err(self.error(
                self.token(tokens, 0).span,
                ErrorCode::MissingMappingColon,
                "explicit mapping key must be followed by a ':' line",
            ));
        };
        state.entries += 1;
        let span = self.token(tokens, 0).span;
        self.limits
            .check_collection_entries(state.entries as usize)
            .map_err(|error| self.limit_at(span, error))?;
        let (current_index, indent, depth) = (
            state.current_index as usize,
            state.indent as usize,
            state.depth as usize,
        );
        stack.push(Cont::MappingEntry(state));
        Ok(Step::Call(Call::MappingValue {
            value: tokens.from(value_index + 1),
            current_index,
            indent,
            depth,
            allow_compact: false,
        }))
    }

    pub(super) fn mapping_value(
        &mut self,
        value: Run,
        current_index: usize,
        indent: usize,
        depth: usize,
        allow_compact: bool,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let (tag, anchor, rest_start) = self.properties(value)?;
        let content = value.from(rest_start);
        if content.is_empty() {
            let next_indent = self
                .lines
                .get(current_index)
                .map_or(0, |line| line.indent());
            let indentless_sequence = next_indent == indent
                && self
                    .line_starts_with(current_index, |kind| matches!(kind, TokenKind::BlockEntry));
            // libyaml allows a block scalar at the parent's column as a mapping value, as in
            // `&a::\n|`. The scanner has already established that this line is the scalar.
            let block_scalar = self.compat
                && self.line_starts_with(current_index, |kind| {
                    kind.scalar_style().is_some_and(|style| {
                        matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    })
                })
                && self
                    .line_first(current_index)
                    .map(|token| token.span.start())
                    .is_some_and(|offset| self.column_at(current_index, offset) == 1)
                && current_index > 0
                && self
                    .line_tokens(current_index - 1)
                    .iter()
                    .any(|token| matches!(token.kind, TokenKind::Anchor));
            if current_index < self.lines.len()
                && (next_indent > indent || indentless_sequence || block_scalar)
            {
                return Ok(Step::Call(Call::Node {
                    index: current_index,
                    indent: self.lines[current_index].indent(),
                    depth: depth + 1,
                    tag,
                    anchor,
                    bounds: NodeBounds::block_out(indent),
                }));
            }
            self.check_truncated_eof(current_index)?;
            let span = self.empty_span(current_index);
            self.emit_null_scalar_with(span, tag, anchor)?;
            return Ok(Step::Return(current_index));
        }
        let content_first = self.token(content, 0);
        if allow_compact && matches!(content_first.kind, TokenKind::BlockEntry) {
            if tag.is_some() || anchor.is_some() {
                return Err(self.error(
                    content_first.span,
                    ErrorCode::MisplacedNodeProperties,
                    "properties cannot precede a compact block sequence",
                ));
            }
            let content_start = content_first.span.start();
            let sequence_indent = self
                .column_at(content.line(), content_start)
                .saturating_sub(1);
            return Ok(Step::Call(Call::SequenceFirst {
                index: current_index.saturating_sub(1),
                first: content,
                indent: sequence_indent,
                depth: depth + 1,
                tag,
                anchor,
            }));
        }
        let explicit_key = matches!(content_first.kind, TokenKind::Key);
        if allow_compact && (explicit_key || has_value_indicator(self.tokens(content))) {
            if explicit_key && (tag.is_some() || anchor.is_some()) {
                return Err(self.error(
                    self.token(content, 0).span,
                    ErrorCode::MisplacedNodeProperties,
                    "properties cannot precede an explicit mapping key",
                ));
            }
            // As on a sequence item: properties belong to the first key, and
            // the compact mapping starts at the first property.
            let value_start = self.token(value, 0).span.start();
            let mapping_indent = self.column_at(value.line(), value_start).saturating_sub(1);
            return Ok(Step::Call(Call::MappingFirst {
                first: value,
                next_index: current_index,
                indent: mapping_indent,
                depth: depth + 1,
                tag: None,
                anchor: None,
            }));
        }
        stack.push(Cont::ValueInline {
            value,
            current_index: fit_u32(current_index),
        });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth: depth + 1,
            tag: None,
            anchor: None,
        }))
    }

    /// Parse one node from a run of a line's tokens. Returns how many
    /// tokens it consumed.
    pub(super) fn inline(
        &mut self,
        tokens: Run,
        depth: usize,
        mut tag: Option<Tag>,
        mut anchor: Option<Anchor<'a>>,
    ) -> Result<Step<'a>, Box<Error>> {
        let mut index = 0;
        if self.starts_with_property(tokens) {
            let (own_tag, own_anchor, count) = self.read_properties(tokens)?;
            tag = own_tag.or(tag);
            anchor = own_anchor.or(anchor);
            index = count;
        }
        if index >= tokens.len() {
            self.check_truncated_run_end(tokens, index)?;
            if index > 0 {
                let span = Span::empty(self.token(tokens, index - 1).span.end());
                self.emit_null_scalar_with(span, tag, anchor)?;
                return Ok(Step::Return(index));
            }
            return Err(self.flow_error(
                tokens,
                0,
                FlowLack::Node(ErrorCode::UnexpectedToken, "expected a node".to_owned()),
            ));
        }
        let abs = self.lines[tokens.line()].start() + tokens.start() + index;
        let token = &mut self.token_arena[abs];
        let span = token.span;
        match &mut token.kind {
            TokenKind::PlainScalar => {
                let text = &self.input[span.start()..span.end()];
                if tag.is_none() && anchor.is_none() {
                    self.emit_plain_scalar(text, span)?;
                } else {
                    self.emit_scalar(
                        Cow::Borrowed(text),
                        text,
                        ScalarStyle::Plain,
                        tag,
                        anchor,
                        span,
                    )?;
                }
                Ok(Step::Return(index + 1))
            }
            TokenKind::SingleQuotedScalar => {
                self.emit_scalar(
                    Cow::Borrowed(&self.input[span.start() + 1..span.end() - 1]),
                    &self.input[span.start()..span.end()],
                    ScalarStyle::SingleQuoted,
                    tag,
                    anchor,
                    span,
                )?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::DoubleQuotedScalar => {
                self.emit_scalar(
                    Cow::Borrowed(&self.input[span.start() + 1..span.end() - 1]),
                    &self.input[span.start()..span.end()],
                    ScalarStyle::DoubleQuoted,
                    tag,
                    anchor,
                    span,
                )?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::Scalar(scalar) => {
                let value = std::mem::replace(&mut scalar.value, Cow::Borrowed(""));
                let style = scalar.style;
                self.emit_scalar(
                    value,
                    &self.input[span.start()..span.end()],
                    style,
                    tag,
                    anchor,
                    span,
                )?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::Alias => {
                if tag.is_some() || anchor.is_some() {
                    return Err(self.error(
                        span,
                        ErrorCode::AliasWithProperties,
                        "aliases cannot carry tags or anchors",
                    ));
                }
                let event = Event::Alias {
                    anchor: Anchor::new(&self.input[span.start() + 1..span.end()]),
                    span,
                };
                self.emit(event)?;
                Ok(Step::Return(index + 1))
            }
            TokenKind::FlowSequenceStart { .. } => Ok(Step::Call(Call::FlowSequence {
                tokens,
                start: index,
                depth,
                tag,
                anchor,
            })),
            TokenKind::FlowMappingStart { .. } => Ok(Step::Call(Call::FlowMapping {
                tokens,
                start: index,
                depth,
                tag,
                anchor,
            })),
            // Properties before `,`, `]`, or `}` belong to an empty node
            // (`[!t ]`, `[&a , b]`; YAML 1.2.2 production 161).
            TokenKind::FlowEntry | TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd
                if index > 0 =>
            {
                self.emit_null_scalar_with(Span::empty(span.start()), tag, anchor)?;
                Ok(Step::Return(index))
            }
            kind => {
                let problem = format!("unexpected {kind} in node position");
                Err(self.flow_error(
                    tokens,
                    index,
                    FlowLack::Node(ErrorCode::UnexpectedToken, problem),
                ))
            }
        }
    }
}
