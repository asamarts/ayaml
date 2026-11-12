use super::*;

impl<'a> Parser<'a> {
    pub(super) fn flow_sequence(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::SequenceStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: start + 1,
            entries: 0,
            depth,
        };
        self.flow_sequence_loop(state, stack)
    }

    pub(super) fn flow_sequence_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let index = state.index;
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
                FlowLack::Node("unexpected empty flow sequence entry".to_owned()),
            ));
        }
        state.entries += 1;
        self.limits
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(token.span, error))?;
        let depth = state.depth + 1;
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
        if let Some(colon) = self.implicit_flow_colon(self.tokens(tokens), index) {
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
        Ok(Step::Call(Call::Inline {
            tokens: tokens.from(index),
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn flow_explicit_mapping(
        &mut self,
        tokens: Run,
        start: usize,
        depth: usize,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
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
        let key_end = self.find_flow_value(self.tokens(tokens), key_start);
        let key = tokens.slice(key_start, key_end);
        if key.is_empty() {
            self.emit(self.null_scalar(Span::empty(start_span.end())))?;
            return self.flow_explicit_value(tokens, start_span, key_end, depth, stack);
        }
        stack.push(Cont::FlowExplicitKey {
            tokens,
            start_span,
            key,
            key_end,
            depth,
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
    ) -> Result<Step<'a>, Error> {
        let mut index = key_end;
        if index < tokens.len() && matches!(self.token(tokens, index).kind, TokenKind::Value) {
            index += 1;
            let value_end = self.find_flow_item_end(self.tokens(tokens), index);
            if index == value_end {
                let span = Span::empty(self.token(tokens, index - 1).span.end());
                self.emit(self.null_scalar(span))?;
                return self.flow_mapping_end(tokens, start_span, value_end);
            }
            let value = tokens.slice(index, value_end);
            stack.push(Cont::FlowExplicitValue {
                tokens,
                start_span,
                value,
                value_end,
            });
            return Ok(Step::Call(Call::Inline {
                tokens: value,
                depth,
                tag: None,
                anchor: None,
            }));
        }
        let span = self.empty_before(tokens, index, start_span);
        self.emit(self.null_scalar(span))?;
        self.flow_mapping_end(tokens, start_span, index)
    }

    /// End a single-pair flow mapping before the token at `index`.
    pub(super) fn flow_mapping_end(
        &mut self,
        tokens: Run,
        start_span: Span,
        index: usize,
    ) -> Result<Step<'a>, Error> {
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
    ) -> Result<Step<'a>, Error> {
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
            self.emit(self.null_scalar(span))?;
            return self.implicit_flow_value(tokens, start_span, colon, depth, stack);
        }
        stack.push(Cont::ImplicitFlowKey {
            tokens,
            start_span,
            key,
            colon,
            depth,
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
    ) -> Result<Step<'a>, Error> {
        let value_start = colon + 1;
        let value_end = self.find_flow_item_end(self.tokens(tokens), value_start);
        if value_start >= tokens.len() || value_start == value_end {
            let span = Span::empty(self.token(tokens, colon).span.end());
            self.emit(self.null_scalar(span))?;
            return self.flow_mapping_end(tokens, start_span, value_end);
        }
        let value = tokens.slice(value_start, value_end);
        stack.push(Cont::ImplicitFlowValue {
            tokens,
            start_span,
            value,
            value_end,
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
    ) -> Result<Step<'a>, Error> {
        let start_span = self.token(tokens, start).span;
        self.limits
            .check_depth(depth + 1)
            .map_err(|error| self.limit_at(start_span, error))?;
        self.emit(Event::MappingStart {
            tag,
            anchor,
            style: CollectionStyle::Flow,
            span: start_span,
        })?;
        let state = FlowState {
            tokens,
            start_span,
            index: start + 1,
            entries: 0,
            depth,
        };
        self.flow_mapping_loop(state, stack)
    }

    pub(super) fn flow_mapping_loop(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let mut index = state.index;
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
                FlowLack::Node("unexpected empty flow mapping entry".to_owned()),
            ));
        }
        if matches!(token.kind, TokenKind::Key) {
            index += 1;
        }
        let key_end = self.find_flow_value(self.tokens(tokens), index);
        let key = tokens.slice(index, key_end);
        state.index = key_end;
        if key.is_empty() {
            // Where a key starts, after `{`, `,` or `?`, nothing stands for it, so a `]` is where a
            // node belongs, as in a sequence. After an entry with no `,`, a separator is missing.
            if index > 0
                && matches!(
                    self.token(tokens, index - 1).kind,
                    TokenKind::FlowMappingStart | TokenKind::FlowEntry | TokenKind::Key
                )
                && self
                    .token_at(tokens, key_end)
                    .is_some_and(|token| matches!(token.kind, TokenKind::FlowSequenceEnd))
            {
                return Err(self.flow_error(
                    tokens,
                    key_end,
                    FlowLack::Node("unexpected ] in node position".to_owned()),
                ));
            }
            let span = Span::empty(self.token(tokens, index.saturating_sub(1)).span.end());
            self.emit(self.null_scalar(span))?;
            return self.flow_mapping_after_key(state, stack);
        }
        let depth = state.depth + 1;
        stack.push(Cont::FlowMappingKey { state, key });
        Ok(Step::Call(Call::Inline {
            tokens: key,
            depth,
            tag: None,
            anchor: None,
        }))
    }

    pub(super) fn flow_mapping_after_key(
        &mut self,
        mut state: FlowState,
        stack: &mut Vec<Cont>,
    ) -> Result<Step<'a>, Error> {
        let tokens = state.tokens;
        let mut index = state.index;
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
            .check_collection_entries(state.entries)
            .map_err(|error| self.limit_at(span, error))?;
        if !matches!(self.token(tokens, index).kind, TokenKind::Value) {
            // Flow mappings permit omitted values: `{key, other: value}`.
            let span = self.empty_before(tokens, index, state.start_span);
            self.emit(self.null_scalar(span))?;
        } else {
            index += 1;
        }
        if index >= tokens.len()
            || matches!(
                self.token(tokens, index).kind,
                TokenKind::FlowEntry | TokenKind::FlowMappingEnd
            )
        {
            if matches!(
                self.token(tokens, index.saturating_sub(1)).kind,
                TokenKind::Value
            ) {
                let span = self.empty_before(tokens, index, state.start_span);
                self.emit(self.null_scalar(span))?;
            }
            state.index = index;
            // Through the driver, as for block `?` entries: `{:, :, …}`
            // must not nest a native frame per entry.
            stack.push(Cont::FlowMappingNext(state));
            return Ok(Step::Return(0));
        }
        let value_end = self.find_flow_item_end(self.tokens(tokens), index);
        // Only a `]` ends a value before it starts, since the other closers returned above.
        if value_end == index {
            return Err(self.flow_error(
                tokens,
                index,
                FlowLack::Node("unexpected ] in node position".to_owned()),
            ));
        }
        let value = tokens.slice(index, value_end);
        state.index = value_end;
        let depth = state.depth + 1;
        stack.push(Cont::FlowMappingValue { state, value });
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
    ) -> Result<Step<'a>, Error> {
        if state.index < state.tokens.len()
            && matches!(
                self.token(state.tokens, state.index).kind,
                TokenKind::FlowEntry
            )
        {
            state.index += 1;
        }
        self.flow_mapping_loop(state, stack)
    }

    pub(super) fn implicit_flow_colon(&self, tokens: &[Token<'a>], start: usize) -> Option<usize> {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return None;
                    }
                    depth -= 1;
                }
                TokenKind::Value if depth == 0 => {
                    let key_start = tokens[start].span.start();
                    let implicit_key = key_start.line() == token.span.start().line()
                        && implicit_key_fits(key_start, token.span.start());
                    if implicit_key || matches!(tokens[start].kind, TokenKind::Key) {
                        return Some(index);
                    }
                    return None;
                }
                TokenKind::FlowEntry if depth == 0 => return None,
                _ => {}
            }
        }
        None
    }

    /// Where the flow key from `start` ends. With no end in sight the parse goes on to the end of
    /// `tokens`, and finds what is wrong in order.
    pub(super) fn find_flow_value(&self, tokens: &[Token<'a>], start: usize) -> usize {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
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
        }
        tokens.len()
    }

    /// Where the flow item from `start` ends, on the same terms as [`Self::find_flow_value`].
    pub(super) fn find_flow_item_end(&self, tokens: &[Token<'a>], start: usize) -> usize {
        let mut depth = 0usize;
        for (index, token) in tokens.iter().enumerate().skip(start) {
            match token.kind {
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart => depth += 1,
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => {
                    if depth == 0 {
                        return index;
                    }
                    depth -= 1;
                }
                TokenKind::FlowEntry if depth == 0 => return index,
                _ => {}
            }
        }
        tokens.len()
    }

    /// The error for a flow collection that fails at `index` of `tokens`, or at the end of the
    /// input past the last of them. Inside a collection it names the failure and the opening.
    #[cold]
    pub(super) fn flow_error(&self, tokens: Run, index: usize, lack: FlowLack) -> Error {
        let failure = self.token_at(tokens, index).map(|token| token.span);
        let span = failure.unwrap_or_else(|| self.end_of_input());
        let Some((opening, sequence)) = self.open_flow(tokens, index) else {
            return self.error(
                span,
                match lack {
                    FlowLack::Separator => "expected ',' or a closing bracket".to_owned(),
                    FlowLack::Node(problem) => problem,
                },
            );
        };
        let (name, closing) = if sequence {
            ("sequence", ']')
        } else {
            ("mapping", '}')
        };
        let from = opening.start();
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
            FlowLack::Node(_) if self.compat => (
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
            FlowLack::Node(problem) => (problem, format!(", in the flow {name} opened {there}")),
        };
        self.error(span, problem).with_context(&context)
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
        let line = &self.lines[tokens.line];
        let at = line.start + tokens.start + index.min(tokens.len());
        let before = |back: usize| {
            at.checked_sub(back)
                .filter(|&token| token >= line.start)
                .map(|token| &self.token_arena[token].kind)
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
                            TokenKind::FlowSequenceStart
                                | TokenKind::FlowMappingStart
                                | TokenKind::FlowEntry
                                | TokenKind::Key
                                | TokenKind::Value
                        )
                    ) =>
            {
                FlowLack::Node(String::new())
            }
            lack => lack,
        }
    }

    /// The innermost flow collection still open before `index` of `tokens`: its opening token's
    /// span, and whether it is a sequence. A closing bracket here closes what it follows, since
    /// the parse stops at the first one that doesn't match.
    pub(super) fn open_flow(&self, tokens: Run, index: usize) -> Option<(Span, bool)> {
        let line = &self.lines[tokens.line];
        let before = (line.start + tokens.start + index).min(line.end);
        let mut closed = 0_usize;
        for token in self.token_arena[line.start..before].iter().rev() {
            match token.kind {
                TokenKind::FlowSequenceEnd | TokenKind::FlowMappingEnd => closed += 1,
                TokenKind::FlowSequenceStart | TokenKind::FlowMappingStart if closed > 0 => {
                    closed -= 1;
                }
                TokenKind::FlowSequenceStart => return Some((token.span, true)),
                TokenKind::FlowMappingStart => return Some((token.span, false)),
                _ => {}
            }
        }
        None
    }

    /// An empty span at the end of the input, counted as the scanner counts it.
    pub(super) fn end_of_input(&self) -> Span {
        Span::empty(LineIndex::end_marker(self.input, self.compat))
    }

    /// A node's properties at the start of `run`, and how many tokens they are. A node
    /// has at most one tag and one anchor, in either order.
    #[inline]
    pub(super) fn properties(&self, run: Run) -> Result<Properties<'a>, Error> {
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
            Some(TokenKind::Tag(_) | TokenKind::Anchor(_))
        )
    }

    pub(super) fn read_properties(&self, run: Run) -> Result<Properties<'a>, Error> {
        let mut tag = None;
        let mut anchor = None;
        let mut index = 0;
        for token in self.tokens(run) {
            let duplicate = match &token.kind {
                TokenKind::Tag(value) => tag.replace(value.clone()).is_some(),
                TokenKind::Anchor(value) => anchor.replace(Anchor::new(value.clone())).is_some(),
                _ => break,
            };
            if duplicate {
                return Err(self.error(token.span, DUPLICATE_PROPERTIES));
            }
            index += 1;
        }
        Ok((tag, anchor, index))
    }

    /// All of line `index`'s tokens.
    pub(super) fn line_first(&self, index: usize) -> Option<&Token<'a>> {
        let line = self.lines.get(index)?;
        if line.is_empty() {
            return None;
        }
        self.token_arena.get(line.start)
    }

    pub(super) fn line_last(&self, index: usize) -> Option<&Token<'a>> {
        let line = self.lines.get(index)?;
        if line.is_empty() {
            return None;
        }
        line.end
            .checked_sub(1)
            .and_then(|end| self.token_arena.get(end))
    }

    pub(super) fn line_tokens(&self, index: usize) -> &[Token<'a>] {
        let line = &self.lines[index];
        &self.token_arena[line.start..line.end]
    }

    pub(super) fn take_line_first_span(&mut self, index: usize) -> Option<Span> {
        let line = self.lines.get_mut(index)?;
        if line.is_empty() {
            return None;
        }
        let span = self.token_arena[line.start].span;
        line.advance(1);
        Some(span)
    }

    pub(super) fn line_run(&self, index: usize) -> Run {
        Run {
            line: index,
            start: 0,
            end: self.lines[index].len(),
        }
    }

    pub(super) fn tokens(&self, run: Run) -> &[Token<'a>] {
        let line = &self.lines[run.line];
        &self.token_arena[line.start + run.start..line.start + run.end]
    }

    /// The token at `index` within `run`, which must exist.
    pub(super) fn token(&self, run: Run, index: usize) -> &Token<'a> {
        &self.tokens(run)[index]
    }

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

    pub(super) fn null_scalar(&self, span: Span) -> Event<'a> {
        self.null_scalar_with(span, None, None)
    }

    pub(super) fn null_scalar_with(
        &self,
        span: Span,
        tag: Option<Tag>,
        anchor: Option<Anchor<'a>>,
    ) -> Event<'a> {
        Event::Scalar {
            value: Cow::Borrowed(""),
            raw: Cow::Borrowed(""),
            style: ScalarStyle::Plain,
            tag,
            anchor,
            span,
        }
    }

    /// Count one event against `max_events`, and pass it on.
    pub(super) fn emit(&mut self, mut event: Event<'a>) -> Result<(), Error> {
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
        if self.lines.get(index).is_some()
            && let Some(token) = self.line_first(index)
        {
            let marker = token.span.start();
            return Span::empty(marker);
        }
        Span::empty(self.end)
    }

    pub(super) fn last_span(&self, index: usize) -> Span {
        if index > 0
            && self.lines.get(index - 1).is_some()
            && let Some(token) = self.line_last(index - 1)
        {
            return Span::empty(token.span.end());
        }
        self.empty_span(index)
    }

    pub(super) fn error(&self, span: Span, message: impl Into<String>) -> Error {
        Error::new(ErrorKind::Parser, message).with_span(span)
    }

    pub(super) fn limit_at(&self, span: Span, limit: crate::limits::LimitExceeded) -> Error {
        Error::from(limit).with_span(span)
    }
}
