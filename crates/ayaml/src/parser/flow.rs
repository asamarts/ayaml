use super::{
    Anchor, Call, CollectionStyle, Cont, Cow, DUPLICATE_PROPERTIES, Error, ErrorCode, ErrorKind,
    Event, FlowLack, FlowState, Parser, Properties, Run, ScalarStyle, Span, Step, Tag, Token,
    TokenKind, fit_u32, matched_flow_end,
};

#[inline]
fn is_scalar_kind(kind: &TokenKind<'_>) -> bool {
    matches!(
        kind,
        TokenKind::PlainScalar
            | TokenKind::SingleQuotedScalar
            | TokenKind::DoubleQuotedScalar
            | TokenKind::Scalar(_)
    )
}

impl<'a> Parser<'a> {
    pub(super) fn flow_sequence(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        if depth == 0
            && self.sink.wants_collection_hint()
            && let Some(entries) = self.flow_entries(tokens, start)
        {
            self.sink
                .reserve_collection(entries.min(self.limits.max_collection_entries));
        }
        self.emit(Event::SequenceStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: fit_u32(start + 1),
            entries: 0,
            depth: fit_u32(depth),
        };
        self.flow_sequence_loop(state, stack)
    }

    pub(super) fn flow_sequence_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let tokens = state.tokens;
        loop {
            let index = state.index as usize;
            if index >= tokens.len() {
                return Err(self.flow_error(tokens, index, FlowLack::Separator));
            }
            let token = self.token(tokens, index);
            if matches!(token.kind, TokenKind::FlowSequenceEnd) {
                let end_span = token.span;
                self.emit(Event::SequenceEnd { span: end_span })?;
                return Ok(Step::Return(index + 1));
            }
            if matches!(token.kind, TokenKind::FlowEntry) {
                return Err(self.flow_error(
                    tokens,
                    index,
                    FlowLack::Node(
                        ErrorCode::InvalidFlowCollection,
                        "unexpected empty flow sequence entry".to_owned(),
                    ),
                ));
            }
            // Fast path: single scalar item directly followed by `,` or `]` avoids
            // `implicit_flow_colon`, `Cont::FlowSequenceItem`, and `Call::Inline`.
            if is_scalar_kind(&token.kind) {
                let span = token.span;
                if let Some(next_kind) = self.token_at(tokens, index + 1).map(|t| &t.kind)
                    && matches!(next_kind, TokenKind::FlowEntry | TokenKind::FlowSequenceEnd)
                {
                    let consume_comma = matches!(next_kind, TokenKind::FlowEntry);
                    state.entries += 1;
                    self.limits
                        .check_collection_entries(state.entries as usize)
                        .map_err(|error| self.limit_at(span, error))?;
                    self.emit_scalar_token(tokens, index, None, None)?;
                    state.index = fit_u32(index + 1 + usize::from(consume_comma));
                    if self.sink.is_full() {
                        stack.push(Cont::FlowSequenceNext(state));
                        return Ok(Step::Return(0));
                    }
                    continue;
                }
            }
            let span = token.span;
            state.entries += 1;
            self.limits
                .check_collection_entries(state.entries as usize)
                .map_err(|error| self.limit_at(span, error))?;
            let depth = state.depth as usize + 1;
            if matches!(self.token(tokens, index).kind, TokenKind::Key) {
                stack.push(Cont::FlowSequenceItem {
                    state,
                    inline: false,
                });
                return Ok(Step::Call(Call::FlowExplicitMapping {
                    tokens,
                    start: index,
                    depth,
                }));
            }
            if let Some(colon) = self.implicit_flow_colon(tokens, index) {
                stack.push(Cont::FlowSequenceItem {
                    state,
                    inline: false,
                });
                return Ok(Step::Call(Call::ImplicitFlowMapping {
                    tokens,
                    start: index,
                    colon,
                    depth,
                }));
            }
            stack.push(Cont::FlowSequenceItem {
                state,
                inline: true,
            });
            return Ok(Step::Call(Call::Inline {
                tokens: tokens.from(index),
                depth,
                tag: None,
                anchor: None,
            }));
        }
    }

    pub(super) fn flow_explicit_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        // The pair is a mapping inside the sequence, so its key and value
        // nest one level deeper than the sequence's items.
        let depth = depth + 1;
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag: None,
            anchor: None,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let key_start = start + 1;
        let key_end = self.find_flow_value(tokens, key_start);
        let key = tokens.slice(key_start, key_end);
        if key.is_empty() {
            self.check_truncated_run_end(tokens, key_end)?;
            self.emit_null_scalar(Span::empty(start_span.end()))?;
            return self.flow_explicit_value(tokens, start_span, key_end, depth, stack);
        }
        stack.push(Cont::FlowExplicitKey {
            tokens,
            start_span,
            key_start: fit_u32(key_start),
            key_end: fit_u32(key_end),
            depth: fit_u32(depth),
        });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn flow_explicit_value(
        &mut self,
        tokens: Run,
        start_span: Span,
        key_end: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let mut index = key_end;
        if index < tokens.len() && matches!(self.token(tokens, index).kind, TokenKind::Value) {
            index += 1;
            let value_end = self.find_flow_item_end(tokens, index);
            if index == value_end {
                self.check_truncated_run_end(tokens, value_end)?;
                let span = Span::empty(self.token(tokens, index - 1).span.end());
                self.emit_null_scalar(span)?;
                return self.flow_mapping_end(tokens, start_span, value_end);
            }
            let value = tokens.slice(index, value_end);
            stack.push(Cont::FlowExplicitValue {
                tokens,
                start_span,
                value_start: fit_u32(index),
                value_end: fit_u32(value_end),
            });
            return Ok(Step::Call(Call::Inline {
                tokens: value,
                depth,
                tag: None,
                anchor: None,
            }));
        }
        self.check_truncated_run_end(tokens, index)?;
        let span = self.empty_before(tokens, index, start_span);
        self.emit_null_scalar(span)?;
        self.flow_mapping_end(tokens, start_span, index)
    }

    /// End a single-pair flow mapping before the token at `index`.
    pub(super) fn flow_mapping_end(
        &mut self,
        tokens: Run,
        start_span: Span,
        index: usize,
    ) -> Result<Step<'a>, Box<Error>> {
        let span = self.empty_before(tokens, index, start_span);
        self.emit(Event::MappingEnd { span })?;
        Ok(Step::Return(index))
    }

    pub(super) fn implicit_flow_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        colon: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        // As in `flow_explicit_mapping`, the pair is one level deeper.
        let depth = depth + 1;
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag: None,
            anchor: None,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let key_start = start
            + usize::from(matches!(
                self.token_at(tokens, start).map(|token| &token.kind),
                Some(TokenKind::Key)
            ));
        let key = tokens.slice(key_start, colon);
        if key.is_empty() {
            let span = Span::empty(self.token(tokens, colon).span.start());
            self.emit_null_scalar(span)?;
            return self.implicit_flow_value(tokens, start_span, colon, depth, stack);
        }
        stack.push(Cont::ImplicitFlowKey {
            tokens,
            start_span,
            key_start: fit_u32(key_start),
            colon: fit_u32(colon),
            depth: fit_u32(depth),
        });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn implicit_flow_value(
        &mut self,
        tokens: Run,
        start_span: Span,
        colon: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let value_start = colon + 1;
        let value_end = self.find_flow_item_end(tokens, value_start);
        if value_start >= tokens.len() || value_start == value_end {
            self.check_truncated_run_end(tokens, value_end)?;
            let span = Span::empty(self.token(tokens, colon).span.end());
            self.emit_null_scalar(span)?;
            return self.flow_mapping_end(tokens, start_span, value_end);
        }
        let value = tokens.slice(value_start, value_end);
        stack.push(Cont::ImplicitFlowValue {
            tokens,
            start_span,
            value_start: fit_u32(value_start),
            value_end: fit_u32(value_end),
        });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn flow_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        if depth == 0
            && self.sink.wants_collection_hint()
            && let Some(entries) = self.flow_entries(tokens, start)
        {
            self.sink
                .reserve_collection(entries.min(self.limits.max_collection_entries));
        }
        self.emit(Event::MappingStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: fit_u32(start + 1),
            entries: 0,
            depth: fit_u32(depth),
        };
        self.flow_mapping_loop(state, stack)
    }

    pub(super) fn flow_mapping_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let tokens = state.tokens;
        loop {
            let mut index = state.index as usize;
            if index >= tokens.len() {
                return Err(self.flow_error(tokens, index, FlowLack::Separator));
            }
            let token = self.token(tokens, index);
            if matches!(token.kind, TokenKind::FlowMappingEnd) {
                let end_span = token.span;
                self.emit(Event::MappingEnd { span: end_span })?;
                return Ok(Step::Return(index + 1));
            }
            if matches!(token.kind, TokenKind::FlowEntry) {
                return Err(self.flow_error(
                    tokens,
                    index,
                    FlowLack::Node(
                        ErrorCode::InvalidFlowCollection,
                        "unexpected empty flow mapping entry".to_owned(),
                    ),
                ));
            }
            // Fast paths for the overwhelmingly common `scalar: scalar` and `scalar: [..]|{..}`
            // flow-mapping entries (such as JSON-in-YAML and nested flow documents).
            if is_scalar_kind(&token.kind)
                && index + 2 < tokens.len()
                && matches!(self.token(tokens, index + 1).kind, TokenKind::Value)
            {
                let colon_span = self.token(tokens, index + 1).span;
                match self.token(tokens, index + 2).kind {
                    TokenKind::PlainScalar
                    | TokenKind::SingleQuotedScalar
                    | TokenKind::DoubleQuotedScalar
                    | TokenKind::Scalar(_)
                        if self.token_at(tokens, index + 3).is_some_and(|after| {
                            matches!(after.kind, TokenKind::FlowEntry | TokenKind::FlowMappingEnd)
                        }) =>
                    {
                        let consume_comma =
                            matches!(self.token(tokens, index + 3).kind, TokenKind::FlowEntry);
                        self.emit_scalar_token(tokens, index, None, None)?;
                        state.entries += 1;
                        self.limits
                            .check_collection_entries(state.entries as usize)
                            .map_err(|error| self.limit_at(colon_span, error))?;
                        self.emit_scalar_token(tokens, index + 2, None, None)?;
                        if self.sink.is_full() {
                            // Leave `state.index` on `,` or `}` so `FlowMappingNext` consumes at
                            // most one comma before resuming the loop.
                            state.index = fit_u32(index + 3);
                            stack.push(Cont::FlowMappingNext(state));
                            return Ok(Step::Return(0));
                        }
                        state.index = fit_u32(index + 3 + usize::from(consume_comma));
                        continue;
                    }
                    TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                        let is_seq = matches!(
                            self.token(tokens, index + 2).kind,
                            TokenKind::FlowSequenceStart { .. }
                        );
                        if let Some(delta) = matched_flow_end(end)
                            && let Some(close) = (index + 2).checked_add(delta)
                            && close < tokens.len()
                        {
                            let value_end = close + 1;
                            if self.token_at(tokens, value_end).is_some_and(|after| {
                                matches!(
                                    after.kind,
                                    TokenKind::FlowEntry | TokenKind::FlowMappingEnd
                                )
                            }) {
                                self.emit_scalar_token(tokens, index, None, None)?;
                                state.entries += 1;
                                self.limits
                                    .check_collection_entries(state.entries as usize)
                                    .map_err(|error| self.limit_at(colon_span, error))?;
                                let value = tokens.slice(index + 2, value_end);
                                let depth = state.depth as usize + 1;
                                state.index = fit_u32(value_end);
                                stack.push(Cont::FlowMappingNext(state));
                                return Ok(Step::Call(if is_seq {
                                    Call::FlowSequence {
                                        tokens: value,
                                        start: 0,
                                        depth,
                                        tag: None,
                                        anchor: None,
                                    }
                                } else {
                                    Call::FlowMapping {
                                        tokens: value,
                                        start: 0,
                                        depth,
                                        tag: None,
                                        anchor: None,
                                    }
                                }));
                            }
                        }
                    }
                    _ => {}
                }
            }
            if matches!(self.token(tokens, index).kind, TokenKind::Key) {
                index += 1;
            }
            let key_end = self.find_flow_value(tokens, index);
            let key = tokens.slice(index, key_end);
            state.index = fit_u32(key_end);
            if key.is_empty() {
                // Where a key starts, after `{`, `,` or `?`, nothing stands for it, so a `]` is
                // where a node belongs, as in a sequence. After an entry with no `,`, a separator
                // is missing.
                if index > 0
                    && matches!(
                        self.token(tokens, index - 1).kind,
                        TokenKind::FlowMappingStart { .. } | TokenKind::FlowEntry | TokenKind::Key
                    )
                    && self
                        .token_at(tokens, key_end)
                        .is_some_and(|token| matches!(token.kind, TokenKind::FlowSequenceEnd))
                {
                    return Err(self.flow_error(
                        tokens,
                        key_end,
                        FlowLack::Node(
                            ErrorCode::InvalidFlowCollection,
                            "unexpected ] in node position".to_owned(),
                        ),
                    ));
                }
                self.check_truncated_run_end(tokens, key_end)?;
                let span = Span::empty(self.token(tokens, index.saturating_sub(1)).span.end());
                self.emit_null_scalar(span)?;
                return self.flow_mapping_after_key(state, stack);
            }
            let depth = state.depth as usize + 1;
            stack.push(Cont::FlowMappingKey {
                state,
                key_start: fit_u32(index),
            });
            return Ok(Step::Call(Call::Inline {
                tokens: key,
                depth,
                tag: None,
                anchor: None,
            }));
        }
    }

    pub(super) fn flow_mapping_after_key(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        let tokens = state.tokens;
        let mut index = state.index as usize;
        // A key ends at the end of its tokens, or at a `]` that closes nothing here.
        let Some(token) = self
            .token_at(tokens, index)
            .filter(|token| !matches!(token.kind, TokenKind::FlowSequenceEnd))
        else {
            return Err(self.flow_error(tokens, index, FlowLack::Separator));
        };
        state.entries += 1;
        let span = token.span;
        self.limits
            .check_collection_entries(state.entries as usize)
            .map_err(|error| self.limit_at(span, error))?;
        if matches!(self.token(tokens, index).kind, TokenKind::Value) {
            index += 1;
        } else {
            // Flow mappings permit omitted values: `{key, other: value}`.
            let span = self.empty_before(tokens, index, state.start_span);
            self.emit_null_scalar(span)?;
        }
        if index >= tokens.len()
            || matches!(
                self.token(tokens, index).kind,
                TokenKind::FlowEntry | TokenKind::FlowMappingEnd
            )
        {
            self.check_truncated_run_end(tokens, index)?;
            if matches!(
                self.token(tokens, index.saturating_sub(1)).kind,
                TokenKind::Value
            ) {
                let span = self.empty_before(tokens, index, state.start_span);
                self.emit_null_scalar(span)?;
            }
            state.index = fit_u32(index);
            // Through the driver, as for block `?` entries: `{:, :, …}`
            // must not nest a native frame per entry.
            stack.push(Cont::FlowMappingNext(state));
            return Ok(Step::Return(0));
        }
        let value_end = self.find_flow_item_end(tokens, index);
        // Only a `]` ends a value before it starts, since the other closers returned above.
        if value_end == index {
            return Err(self.flow_error(
                tokens,
                index,
                FlowLack::Node(
                    ErrorCode::InvalidFlowCollection,
                    "unexpected ] in node position".to_owned(),
                ),
            ));
        }
        let value = tokens.slice(index, value_end);
        state.index = fit_u32(value_end);
        let depth = state.depth as usize + 1;
        stack.push(Cont::FlowMappingValue {
            state,
            value_start: fit_u32(index),
        });
        Ok(Step::Call(Call::Inline {
            tokens: value,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn flow_mapping_next(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Box<Error>> {
        if (state.index as usize) < state.tokens.len()
            && matches!(
                self.token(state.tokens, state.index as usize).kind,
                TokenKind::FlowEntry
            )
        {
            state.index += 1;
        }
        self.flow_mapping_loop(state, stack)
    }

    #[allow(
        clippy::unused_self,
        reason = "the method is part of a uniform grammar or diagnostic surface"
    )]
    pub(super) fn implicit_flow_colon(&self, run: Run, start: usize) -> Option<usize> {
        let tokens = self.tokens(run);
        let mut depth = 0usize;
        let mut index = start;
        while index < tokens.len() {
            let token = &tokens[index];
            match token.kind {
                TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                    // A nested collection is one key candidate. Jump over it in one step rather
                    // than rescanning its content at every enclosing level.
                    if let Some(delta) = matched_flow_end(end) {
                        index = index.saturating_add(delta).saturating_add(1);
                        continue;
                    }
                    depth += 1;
                }
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                TokenKind::Value if depth == 0 => {
                    let key_start = tokens[start].span.start();
                    let implicit_key = self.implicit_key_fits_range(key_start, token.span.start());
                    if implicit_key || matches!(tokens[start].kind, TokenKind::Key) {
                        return Some(index);
                    }
                    return None;
                }
                TokenKind::FlowEntry if depth == 0 => return None,
                _ => {}
            }
            index += 1;
        }
        None
    }

    /// Where the flow key from `start` ends. With no end in sight the parse goes on to the end of
    /// `tokens`, and finds what is wrong in order.
    #[allow(
        clippy::match_same_arms,
        reason = "the arms stay separate to document each representation or state"
    )]
    pub(super) fn find_flow_value(&self, run: Run, start: usize) -> usize {
        let tokens = self.tokens(run);
        let mut depth = 0usize;
        let mut index = start;
        while index < tokens.len() {
            match tokens[index].kind {
                TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                    if let Some(delta) = matched_flow_end(end)
                        && let Some(close) = index.checked_add(delta)
                        && close < tokens.len()
                    {
                        index = close + 1;
                        continue;
                    }
                    depth += 1;
                }
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return index;
                    }
                    depth -= 1;
                }
                TokenKind::Value if depth == 0 => return index,
                TokenKind::FlowEntry if depth == 0 => return index,
                _ => {}
            }
            index += 1;
        }
        tokens.len()
    }

    /// Where the flow item from `start` ends, on the same terms as [`Self::find_flow_value`].
    pub(super) fn find_flow_item_end(&self, run: Run, start: usize) -> usize {
        let tokens = self.tokens(run);
        let mut depth = 0usize;
        let mut index = start;
        while index < tokens.len() {
            match tokens[index].kind {
                TokenKind::FlowSequenceStart { end } | TokenKind::FlowMappingStart { end } => {
                    if let Some(delta) = matched_flow_end(end)
                        && let Some(close) = index.checked_add(delta)
                        && close < tokens.len()
                    {
                        index = close + 1;
                        continue;
                    }
                    depth += 1;
                }
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return index;
                    }
                    depth -= 1;
                }
                TokenKind::FlowEntry if depth == 0 => return index,
                _ => {}
            }
            index += 1;
        }
        tokens.len()
    }

    /// The error for a flow collection that fails at `index` of `tokens`, or at the end of the
    /// input past the last of them. Inside a collection it names the failure and the opening.
    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    pub(super) fn flow_error(&mut self, tokens: Run, index: usize, lack: FlowLack) -> Box<Error> {
        let failure = self.token_at(tokens, index).map(|token| token.span);
        if failure.is_none()
            && let Err(error) = self.window.take_scanner_error()
        {
            return error;
        }
        let span = failure.unwrap_or_else(|| self.end_of_input());
        let code = match &lack {
            FlowLack::Separator if failure.is_none() => ErrorCode::UnterminatedFlowCollection,
            FlowLack::Separator => ErrorCode::InvalidFlowCollection,
            FlowLack::Node(code, _) => *code,
        };
        let Some((opening, sequence)) = self.open_flow(tokens, index) else {
            return self.error(
                span,
                code,
                match lack {
                    FlowLack::Separator => "expected ',' or a closing bracket".to_owned(),
                    FlowLack::Node(_, problem) => problem,
                },
            );
        };
        let (name, closing) = if sequence {
            ("sequence", ']')
        } else {
            ("mapping", '}')
        };
        let from = self.at(opening.start());
        let there = format!("at line {} column {}", from.line(), from.column());
        let lack = if self.compat {
            self.libyaml_flow_lack(tokens, index, sequence, lack)
        } else {
            lack
        };
        // The failure's location goes between the two when the error is displayed.
        let (problem, context) = match lack {
            FlowLack::Separator if self.compat => (
                format!("did not find expected ',' or '{closing}'"),
                format!(", while parsing a flow {name} {there}"),
            ),
            FlowLack::Node(_, _) if self.compat => (
                "did not find expected node content".to_owned(),
                format!(", while parsing a flow {name} {there}"),
            ),
            FlowLack::Separator if failure.is_none() => (
                format!("unterminated flow {name}"),
                format!(", opened {there}"),
            ),
            FlowLack::Separator => (
                format!("expected ',' or '{closing}'"),
                format!(", in the flow {name} opened {there}"),
            ),
            FlowLack::Node(_, problem) => (problem, format!(", in the flow {name} opened {there}")),
        };
        Box::new((*self.error(span, code, problem)).with_context(&context))
    }

    /// What libyaml's parser lacks where compat's flow collection fails at `index` of `tokens`, or
    /// at the end of the input past them, for its wording. `sequence` is whether the innermost
    /// open collection is a sequence.
    pub(super) fn libyaml_flow_lack(
        &self,
        tokens: Run,
        index: usize,
        sequence: bool,
        lack: FlowLack,
    ) -> FlowLack {
        // The tokens before the failure on its logical line, which a run that starts at an item
        // leaves out.
        let line_tokens = self.line_tokens(tokens.line());
        let at = tokens.start() + index.min(tokens.len());
        let before = |back: usize| {
            at.checked_sub(back)
                .and_then(|token| line_tokens.get(token))
                .map(|token| &token.kind)
        };
        let failing = self.token_at(tokens, index).map(|token| &token.kind);
        // In a flow sequence, libyaml takes the `:`, `,` or `]` after an empty `?` key as part of
        // the pair, then ends the pair and asks for a `,` or `]` (`[? :`, `[? ,}`).
        if sequence
            && matches!(before(2), Some(TokenKind::Key))
            && matches!(before(1), Some(TokenKind::Value | TokenKind::FlowEntry))
            && !matches!(failing, Some(TokenKind::Value | TokenKind::FlowEntry))
        {
            return FlowLack::Separator;
        }
        // Where an entry, a key or a value starts, it asks for a node: at the end of the input
        // (`[a,`, `{a:`), and at a `}` in a sequence (`[a: }`, `[? }`).
        match lack {
            FlowLack::Separator
                if (failing.is_none()
                    || sequence && matches!(failing, Some(TokenKind::FlowMappingEnd)))
                    && matches!(
                        before(1),
                        Some(
                            TokenKind::FlowSequenceStart { .. }
                                | TokenKind::FlowMappingStart { .. }
                                | TokenKind::FlowEntry
                                | TokenKind::Key
                                | TokenKind::Value
                        )
                    ) =>
            {
                FlowLack::Node(ErrorCode::UnterminatedFlowCollection, String::new())
            }
            lack => lack,
        }
    }

    /// The innermost flow collection still open before `index` of `tokens`: its opening token's
    /// span, and whether it is a sequence. A closing bracket here closes what it follows, since
    /// the parse stops at the first one that doesn't match.
    pub(super) fn open_flow(&self, tokens: Run, index: usize) -> Option<(Span, bool)> {
        let line_tokens = self.line_tokens(tokens.line());
        let before = (tokens.start() + index).min(line_tokens.len());
        let mut closed = 0_usize;
        for token in line_tokens[..before].iter().rev() {
            match token.kind {
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => closed += 1,
                TokenKind::FlowSequenceStart { .. } | TokenKind::FlowMappingStart { .. }
                    if closed > 0 =>
                {
                    closed -= 1;
                }
                TokenKind::FlowSequenceStart { .. } => return Some((token.span, true)),
                TokenKind::FlowMappingStart { .. } => return Some((token.span, false)),
                _ => {}
            }
        }
        None
    }

    /// An empty span at the end of the input.
    pub(super) fn end_of_input(&self) -> Span {
        Span::empty(self.input.len())
    }

    /// A node's properties at the start of `run`, and how many tokens they are. A node
    /// has at most one tag and one anchor, in either order.
    #[inline]
    pub(super) fn properties(&self, run: Run) -> Result<Properties<'a>, Box<Error>> {
        // Most nodes have none.
        if !self.starts_with_property(run) {
            return Ok((None, None, 0));
        }
        self.read_properties(run)
    }

    #[inline]
    pub(super) fn starts_with_property(&self, run: Run) -> bool {
        matches!(
            self.tokens(run).first().map(|token| &token.kind),
            Some(TokenKind::Tag(_) | TokenKind::Anchor)
        )
    }

    pub(super) fn read_properties(&self, run: Run) -> Result<Properties<'a>, Box<Error>> {
        let mut tag = None;
        let mut anchor = None;
        let mut index = 0;
        for token in self.tokens(run) {
            let duplicate = match &token.kind {
                TokenKind::Tag(value) => tag.replace(value.clone()).is_some(),
                TokenKind::Anchor => anchor
                    .replace(Anchor::new(
                        &self.input[token.span.start() + 1..token.span.end()],
                    ))
                    .is_some(),
                _ => break,
            };
            if duplicate {
                return Err(self.error(
                    token.span,
                    ErrorCode::DuplicateNodeProperties,
                    DUPLICATE_PROPERTIES,
                ));
            }
            index += 1;
        }
        Ok((tag, anchor, index))
    }

    #[inline]
    pub(super) fn ensure_line(&mut self, index: usize) -> Result<bool, Box<Error>> {
        self.window.ensure_line(index, &self.limits)
    }

    /// The first surviving token of line `index`, if present in the active window.
    #[inline]
    pub(super) fn line_first(&self, index: usize) -> Option<&Token<'a>> {
        self.window.line_first(index)
    }

    #[inline]
    pub(super) fn line_tokens(&self, index: usize) -> &[Token<'a>] {
        self.window.line_tokens(index)
    }

    pub(super) fn take_line_first_span(&mut self, index: usize) -> Option<Span> {
        self.window.take_line_first_span(index)
    }

    #[inline]
    pub(super) fn line_run(&self, index: usize) -> Run {
        Run {
            line: fit_u32(index),
            start: 0,
            end: fit_u32(self.window.line(index).len()),
        }
    }

    #[inline]
    pub(super) fn tokens(&self, run: Run) -> &[Token<'a>] {
        self.window.run_tokens(run)
    }

    /// The token at `index` within `run`, which must exist.
    #[inline]
    pub(super) fn token(&self, run: Run, index: usize) -> &Token<'a> {
        &self.tokens(run)[index]
    }

    #[inline]
    pub(super) fn token_at(&self, run: Run, index: usize) -> Option<&Token<'a>> {
        self.tokens(run).get(index)
    }

    /// An empty span at the start of `run`'s token at `index`, or, past the
    /// end of the run, at the end of `start` (a flow collection's opening
    /// token).
    pub(super) fn empty_before(&self, run: Run, index: usize, start: Span) -> Span {
        self.token_at(run, index).map_or_else(
            || Span::empty(start.end()),
            |token| Span::empty(token.span.start()),
        )
    }

    #[inline]
    pub(super) fn emit_null_scalar(&mut self, span: Span) -> Result<(), Box<Error>> {
        self.emit_plain_scalar("", span)
    }

    #[inline]
    pub(super) fn emit_null_scalar_with(
        &mut self,
        span: Span,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    ) -> Result<(), Box<Error>> {
        if tag.is_none() && anchor.is_none() {
            self.emit_plain_scalar("", span)
        } else {
            self.emit_scalar(Cow::Borrowed(""), "", ScalarStyle::Plain, tag, anchor, span)
        }
    }

    /// Emit a scalar token directly from the active line window via the sink's scalar fast path,
    /// moving owned escaped/block scalar text without cloning.
    #[inline]
    pub(super) fn emit_scalar_token(
        &mut self,
        run: Run,
        offset: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    ) -> Result<(), Box<Error>> {
        let token = self.window.token_mut(run, offset);
        let span = token.span;
        match &mut token.kind {
            TokenKind::PlainScalar => {
                let text = &self.input[span.start()..span.end()];
                if tag.is_none() && anchor.is_none() {
                    self.emit_plain_scalar(text, span)
                } else {
                    self.emit_scalar(
                        Cow::Borrowed(text),
                        text,
                        ScalarStyle::Plain,
                        tag,
                        anchor,
                        span,
                    )
                }
            }
            TokenKind::SingleQuotedScalar => self.emit_scalar(
                Cow::Borrowed(&self.input[span.start() + 1..span.end() - 1]),
                &self.input[span.start()..span.end()],
                ScalarStyle::SingleQuoted,
                tag,
                anchor,
                span,
            ),
            TokenKind::DoubleQuotedScalar => self.emit_scalar(
                Cow::Borrowed(&self.input[span.start() + 1..span.end() - 1]),
                &self.input[span.start()..span.end()],
                ScalarStyle::DoubleQuoted,
                tag,
                anchor,
                span,
            ),
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
                )
            }
            _ => unreachable!("emit_scalar_token called on non-scalar token"),
        }
    }

    #[inline]
    pub(super) fn emit_plain_scalar(
        &mut self,
        value: &'a str,
        span: Span,
    ) -> Result<(), Box<Error>> {
        self.emitted = self.emitted.saturating_add(1);
        self.limits
            .check_events(self.emitted)
            .map_err(|error| self.limit_at(span, error))?;
        self.sink.plain_scalar(value, span)
    }

    #[inline]
    pub(super) fn emit_scalar(
        &mut self,
        value: Cow<'a, str>,
        raw: &'a str,
        style: ScalarStyle,
        mut tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        span: Span,
    ) -> Result<(), Box<Error>> {
        if tag.as_ref().is_some_and(Tag::is_core_collection) {
            tag = None;
        }
        self.emitted = self.emitted.saturating_add(1);
        self.limits
            .check_events(self.emitted)
            .map_err(|error| self.limit_at(span, error))?;
        self.sink.scalar(value, raw, style, tag, anchor, span)
    }

    /// Count one event against `max_events` without pushing it into the sink (used when
    /// `EventCursor` yields `StreamStart` directly before allocating its queue).
    #[inline]
    pub(super) fn count_event(&mut self, span: Span) -> Result<(), Box<Error>> {
        self.emitted = self.emitted.saturating_add(1);
        self.limits
            .check_events(self.emitted)
            .map_err(|error| self.limit_at(span, error))
    }

    /// Count one event against `max_events`, and pass it on.
    pub(super) fn emit(&mut self, mut event: Event<'a>) -> Result<(), Box<Error>> {
        match &mut event {
            Event::Scalar { tag, .. } => {
                if tag.as_ref().is_some_and(Tag::is_core_collection) {
                    *tag = None;
                }
            }
            Event::SequenceStart { tag, .. } => {
                if tag.as_ref().is_some_and(|tag| {
                    tag.is_core_scalar()
                        || (tag.is_core_collection()
                            && !matches!(tag.suffix(), "seq" | "omap" | "pairs"))
                }) {
                    *tag = None;
                }
            }
            Event::MappingStart { tag, .. } => {
                let drop = tag.as_ref().is_some_and(|tag| {
                    tag.is_core_scalar()
                        || (tag.is_core_collection() && !matches!(tag.suffix(), "map" | "set"))
                });
                if drop {
                    *tag = None;
                }
            }
            _ => {}
        }
        self.emitted = self.emitted.saturating_add(1);
        let span = event.span();
        self.limits
            .check_events(self.emitted)
            .map_err(|error| self.limit_at(span, error))?;
        self.sink.event(event)
    }

    pub(super) fn line_starts_with(
        &self,
        index: usize,
        predicate: impl Fn(&TokenKind<'a>) -> bool,
    ) -> bool {
        self.line_first(index)
            .is_some_and(|token| predicate(&token.kind))
    }

    pub(super) fn empty_span(&self, index: usize) -> Span {
        if let Some(token) = self.line_first(index) {
            return Span::empty(token.span.start());
        }
        Span::empty(self.window.end_offset())
    }

    pub(super) fn last_span(&self, index: usize) -> Span {
        if index > 0
            && let Some(end) = self.window.line_last_end(index - 1)
        {
            return Span::empty(end);
        }
        self.empty_span(index)
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unused_self,
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    pub(super) fn error(
        &self,
        span: Span,
        code: ErrorCode,
        message: impl Into<String>,
    ) -> Box<Error> {
        Box::new(
            Error::coded(ErrorKind::Parser, code, message)
                .with_span(span)
                .with_input_location(self.input, self.compat),
        )
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unused_self,
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    pub(super) fn limit_at(&self, span: Span, limit: crate::limits::LimitExceeded) -> Box<Error> {
        Box::new(
            Error::from(limit)
                .with_span(span)
                .with_input_location(self.input, self.compat),
        )
    }
}
