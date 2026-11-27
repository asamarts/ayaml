//! Iterative YAML value emitter backed by an explicit heap stack.
//!
//! Nested collections and tagged values are pushed onto a bounded O(depth) heap
//! stack rather than recursing on the native call stack, while scalar and empty
//! collection leaves emit inline inside each collection's loop without stack
//! traffic.

use std::io::Write;

use super::scalars::{
    block_scalar_ok, can_fold, double_quoted_char_len_capped, needs_explicit_indent,
    plain_is_block_safe, plain_is_safe, single_quoted_char_len_capped, single_quoted_ok,
    write_double_quoted, write_folded_body, write_single_quoted,
};
use super::tags::{RedefinedHandles, TagDirectives};
use super::{
    Chomping, EmitterOptions, EmitterStyles, MAX_IMPLICIT_KEY_CHARS, NodeStyle, Output,
    PathSegment, STACK_MEDIUM, STACK_SMALL, TieredStack,
};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::limits::Limits;
use crate::value::Value;

fn nested_tag_error() -> Error {
    Error::coded(
        ErrorKind::Emitter,
        ErrorCode::UnsupportedTag,
        "YAML cannot write a tagged value whose payload is also tagged (for example a nested Serde enum)",
    )
}

/// How the value writer writes a string scalar.
#[derive(Clone, Copy)]
enum StringForm {
    Plain,
    SingleQuoted,
    DoubleQuoted,
}

enum BlockMapStage<'v> {
    NextEntry,
    AfterExplicitKey { key: &'v Value, value: &'v Value },
    AfterValue { newline: bool },
}

enum BlockSeqStage {
    NextEntry,
    AfterItem { newline: bool },
}

enum FlowMapStage<'v> {
    NextEntry,
    AfterKey {
        key: &'v Value,
        value: &'v Value,
        explicit: bool,
        prev_in_key: bool,
    },
    AfterValue,
}

enum BlockMapAction<'v> {
    Flow(&'v Value),
    BlockNode(&'v Value, usize),
    Inline(&'v Value, usize),
    Done,
}

enum BlockSeqAction<'v> {
    Flow(&'v Value),
    CompactSeq(&'v [Value], usize),
    CompactMap(&'v crate::mapping::Mapping, usize),
    BlockNode(&'v Value, usize),
    Inline(&'v Value, usize),
    Done,
}

enum Frame<'v> {
    BlockMapping {
        iter: crate::mapping::Iter<'v>,
        indent: usize,
        first: bool,
        inline_first: bool,
        stage: BlockMapStage<'v>,
    },
    BlockSequence {
        iter: std::slice::Iter<'v, Value>,
        index: usize,
        indent: usize,
        inline_first: bool,
        stage: BlockSeqStage,
    },
    FlowSequence {
        iter: std::slice::Iter<'v, Value>,
        index: usize,
        prev_in_flow: bool,
        pop_path: bool,
    },
    FlowMapping {
        iter: crate::mapping::Iter<'v>,
        first: bool,
        prev_in_flow: bool,
        stage: FlowMapStage<'v>,
    },
}

/// Whether `value` emits in flow context as a single leaf without opening a nested stack frame.
#[inline]
fn is_flow_leaf(value: &Value) -> bool {
    match value {
        Value::Sequence(sequence) => sequence.is_empty(),
        Value::Mapping(mapping) => mapping.is_empty(),
        Value::Tagged(_) => false,
        _ => true,
    }
}

pub(super) struct ValueWriter<'a, 'w, 's, W: Write> {
    output: &'a mut Output<'w, W>,
    options: EmitterOptions,
    styles: Option<&'s EmitterStyles>,
    path: Vec<PathSegment>,
    limits: Limits,
    depth: usize,
    error: Option<Error>,
    /// Inside a flow collection.
    in_flow: bool,
    /// Writing a mapping key: per-path styles do not apply.
    in_key: bool,
    /// The document's `%TAG` directives, which [`TagDirectives::scan`] found.
    directives: TagDirectives,
    #[cfg(test)]
    pub(super) flow_loop_steps: usize,
    #[cfg(test)]
    pub(super) block_scalar_lines: usize,
    #[cfg(test)]
    pub(super) quoted_chunks: usize,
}

impl<'a, 'w, 's, W: Write> ValueWriter<'a, 'w, 's, W> {
    pub(super) fn new(
        output: &'a mut Output<'w, W>,
        limits: &Limits,
        options: EmitterOptions,
        styles: Option<&'s EmitterStyles>,
        directives: TagDirectives,
    ) -> Self {
        Self {
            output,
            options,
            styles,
            path: Vec::new(),
            limits: limits.clone(),
            depth: 0,
            error: None,
            in_flow: false,
            in_key: false,
            directives,
            #[cfg(test)]
            flow_loop_steps: 0,
            #[cfg(test)]
            block_scalar_lines: 0,
            #[cfg(test)]
            quoted_chunks: 0,
        }
    }

    fn style(&self) -> NodeStyle {
        if self.in_key {
            NodeStyle::default()
        } else {
            self.styles
                .and_then(|styles| styles.get(&self.path))
                .unwrap_or_default()
        }
    }

    fn scalar_style(&self) -> Option<crate::event::ScalarStyle> {
        self.style().scalar_style.or(self.options.scalar_style)
    }

    fn styles_active(&self) -> bool {
        self.styles.is_some_and(|styles| !styles.styles.is_empty())
    }

    fn force_flow(&self) -> bool {
        self.style().flow == Some(true)
    }

    fn push_key(&mut self, key: &Value) {
        if self.styles_active() {
            self.path.push(PathSegment::Key(key.clone()));
        }
    }

    fn push_index(&mut self, index: usize) {
        if self.styles_active() {
            self.path.push(PathSegment::Index(index));
        }
    }

    fn pop_path(&mut self) {
        if self.styles_active() {
            self.path.pop();
        }
    }

    #[cold]
    #[inline(never)]
    fn fail(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    #[inline]
    fn push_str(&mut self, value: &str) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.output.push_str(value) {
            self.fail(error);
        }
    }

    fn push(&mut self, value: char) {
        let mut buffer = [0_u8; 4];
        self.push_str(value.encode_utf8(&mut buffer));
    }

    fn indent(&mut self, indent: usize) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.output.spaces(indent) {
            self.fail(error);
        }
    }

    /// Enter a collection. Returns false, after recording the error, when
    /// the depth limit is exceeded or an earlier write failed.
    fn enter(&mut self) -> bool {
        if self.error.is_some() {
            return false;
        }
        self.depth += 1;
        if let Err(error) = self.limits.check_depth(self.depth) {
            self.depth -= 1;
            self.fail(Error::from(error));
            return false;
        }
        true
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    /// Stream a tag token directly to `self.output`. Fails for a tag whose payload is also
    /// tagged, which YAML cannot express.
    fn push_tag(&mut self, tagged: &crate::tag::TaggedValue) -> bool {
        if self.error.is_some() {
            return false;
        }
        if matches!(tagged.value, Value::Tagged(_)) {
            self.fail(nested_tag_error());
            return false;
        }
        if let Err(error) =
            self.directives
                .write_tag(&tagged.tag, RedefinedHandles::default(), self.output)
        {
            self.fail(error);
            return false;
        }
        self.error.is_none()
    }

    /// The first error, if writing failed. It borrows the writer rather than take it, which would
    /// copy it.
    pub(super) fn finish(&mut self) -> Result<(), Error> {
        self.error.take().map_or(Ok(()), Err)
    }

    /// Write the root node, after any `---` and directives.
    pub(super) fn emit_root(&mut self, value: &Value) {
        if self.style().flow.unwrap_or(self.options.flow) {
            if is_flow_leaf(value) {
                self.emit_flow_leaf(value);
            } else {
                let mut small = [const { None }; STACK_SMALL];
                let mut stack = TieredStack::new(&mut small, &mut []);
                self.start_flow(value, &mut stack);
                self.run_stack(&mut stack);
            }
            self.push('\n');
        } else if let Value::String(text) = value
            && self.use_block_scalar(text)
        {
            self.emit_block_scalar(text, self.options.indent, true);
        } else if let Value::Tagged(tagged) = value
            && let Value::String(text) = &tagged.value
            && self.use_block_scalar(text)
        {
            if self.push_tag(tagged) {
                self.push(' ');
                self.emit_block_scalar(text, self.options.indent, true);
            }
        } else {
            match value {
                Value::Mapping(mapping) if !mapping.is_empty() => {
                    let mut small = [const { None }; STACK_SMALL];
                    let mut stack = TieredStack::new(&mut small, &mut []);
                    self.start_block_mapping(mapping, 0, false, &mut stack);
                    self.run_stack(&mut stack);
                }
                Value::Sequence(sequence) if !sequence.is_empty() => {
                    let mut small = [const { None }; STACK_SMALL];
                    let mut stack = TieredStack::new(&mut small, &mut []);
                    self.start_block_sequence(sequence, 0, false, &mut stack);
                    self.run_stack(&mut stack);
                }
                _ => {
                    if is_flow_leaf(value) {
                        self.emit_flow_leaf(value);
                    } else {
                        let mut small = [const { None }; STACK_SMALL];
                        let mut stack = TieredStack::new(&mut small, &mut []);
                        self.start_flow(value, &mut stack);
                        self.run_stack(&mut stack);
                    }
                    self.push('\n');
                }
            }
        }
        if self.options.explicit_end {
            self.push_str("...\n");
        }
    }

    /// Drive open collection frames iteratively until `stack` is empty, promoting once to a
    /// medium inline buffer if nesting exceeds `STACK_SMALL` and spilling to the heap past
    /// `STACK_SMALL + STACK_MEDIUM`.
    fn run_stack(&mut self, stack: &mut TieredStack<'_, '_, Frame<'_>>) {
        loop {
            if let Some(pending) = stack.take_pending() {
                let (small, small_len) = stack.small_parts();
                self.run_stack_deep(small, small_len, pending);
                return;
            }
            let Some(frame) = stack.last_mut() else {
                return;
            };
            match frame {
                Frame::BlockMapping {
                    iter,
                    indent,
                    first,
                    inline_first,
                    stage,
                } => {
                    let action =
                        self.advance_block_mapping(iter, *indent, first, *inline_first, stage);
                    match action {
                        BlockMapAction::Flow(node) => self.start_flow(node, stack),
                        BlockMapAction::BlockNode(node, child_indent) => {
                            self.start_block_node(node, child_indent, stack);
                        }
                        BlockMapAction::Inline(node, child_indent) => {
                            self.start_inline_value(node, child_indent, stack);
                        }
                        BlockMapAction::Done => {
                            stack.drop_top();
                            self.leave();
                        }
                    }
                }
                Frame::BlockSequence {
                    iter,
                    index,
                    indent,
                    inline_first,
                    stage,
                } => {
                    let action =
                        self.advance_block_sequence(iter, index, *indent, *inline_first, stage);
                    match action {
                        BlockSeqAction::Flow(node) => self.start_flow(node, stack),
                        BlockSeqAction::CompactSeq(inner, child_indent) => {
                            self.start_block_sequence(inner, child_indent, true, stack);
                        }
                        BlockSeqAction::CompactMap(mapping, child_indent) => {
                            self.start_block_mapping(mapping, child_indent, true, stack);
                        }
                        BlockSeqAction::BlockNode(node, child_indent) => {
                            self.start_block_node(node, child_indent, stack);
                        }
                        BlockSeqAction::Inline(node, child_indent) => {
                            self.start_inline_value(node, child_indent, stack);
                        }
                        BlockSeqAction::Done => {
                            stack.drop_top();
                            self.leave();
                        }
                    }
                }
                Frame::FlowSequence {
                    iter,
                    index,
                    prev_in_flow,
                    pop_path,
                } => {
                    let prev_in_flow = *prev_in_flow;
                    if let Some(item) = self.advance_flow_sequence(iter, index, pop_path) {
                        self.start_flow(item, stack);
                    } else {
                        stack.drop_top();
                        self.push(']');
                        self.in_flow = prev_in_flow;
                        self.leave();
                    }
                }
                Frame::FlowMapping {
                    iter,
                    first,
                    prev_in_flow,
                    stage,
                } => {
                    let prev_in_flow = *prev_in_flow;
                    if let Some(node) = self.advance_flow_mapping(iter, first, stage) {
                        self.start_flow(node, stack);
                    } else {
                        stack.drop_top();
                        self.push('}');
                        self.in_flow = prev_in_flow;
                        self.leave();
                    }
                }
            }
        }
    }

    #[cold]
    #[inline(never)]
    fn run_stack_deep<'v>(
        &mut self,
        small: &mut [Option<Frame<'v>>],
        small_len: usize,
        pending: Frame<'v>,
    ) {
        let mut medium = [const { None }; STACK_MEDIUM];
        let mut stack = TieredStack::from_promoted(small, &mut medium, small_len, pending);
        self.run_stack(&mut stack);
    }

    fn start_block_mapping<'v>(
        &mut self,
        mapping: &'v crate::mapping::Mapping,
        indent: usize,
        inline_first: bool,
        stack: &mut TieredStack<'_, '_, Frame<'v>>,
    ) {
        if !self.enter() {
            return;
        }
        stack.push(Frame::BlockMapping {
            iter: mapping.iter(),
            indent,
            first: true,
            inline_first,
            stage: BlockMapStage::NextEntry,
        });
    }

    fn start_block_sequence<'v>(
        &mut self,
        sequence: &'v [Value],
        indent: usize,
        inline_first: bool,
        stack: &mut TieredStack<'_, '_, Frame<'v>>,
    ) {
        if !self.enter() {
            return;
        }
        stack.push(Frame::BlockSequence {
            iter: sequence.iter(),
            index: 0,
            indent,
            inline_first,
            stage: BlockSeqStage::NextEntry,
        });
    }

    #[inline]
    fn advance_block_mapping<'v>(
        &mut self,
        iter: &mut crate::mapping::Iter<'v>,
        indent: usize,
        first: &mut bool,
        inline_first: bool,
        stage: &mut BlockMapStage<'v>,
    ) -> BlockMapAction<'v> {
        let step = self.options.indent;
        loop {
            match std::mem::replace(stage, BlockMapStage::NextEntry) {
                BlockMapStage::AfterValue { newline } => {
                    if newline {
                        self.push('\n');
                    }
                    self.pop_path();
                }
                BlockMapStage::AfterExplicitKey { key, value } => {
                    self.in_key = false;
                    if self.error.is_some() {
                        return BlockMapAction::Done;
                    }
                    self.push('\n');
                    self.indent(indent);
                    self.push(':');
                    if self.error.is_some() {
                        return BlockMapAction::Done;
                    }
                    self.push_key(key);
                    if self.force_flow()
                        && matches!(
                            value,
                            Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
                        )
                    {
                        self.push(' ');
                        *stage = BlockMapStage::AfterValue { newline: true };
                        return BlockMapAction::Flow(value);
                    }
                    if Self::needs_block_value(value) {
                        self.push('\n');
                        let child_indent = self.block_value_indent(value, indent);
                        *stage = BlockMapStage::AfterValue { newline: false };
                        return BlockMapAction::BlockNode(value, child_indent);
                    }
                    self.push(' ');
                    if self.emit_inline_leaf_fast(value, indent + step) {
                        self.pop_path();
                    } else {
                        *stage = BlockMapStage::AfterValue { newline: false };
                        return BlockMapAction::Inline(value, indent + step);
                    }
                }
                BlockMapStage::NextEntry => {
                    if self.error.is_some() {
                        return BlockMapAction::Done;
                    }
                    let Some((key, value)) = iter.next() else {
                        return BlockMapAction::Done;
                    };
                    let is_first = *first;
                    *first = false;
                    if !is_first || !inline_first {
                        self.indent(indent);
                    }
                    if let Value::String(text) = key {
                        let form = self.string_form(text, self.options.scalar_style);
                        if Self::string_key_exceeds_limit(text, form) {
                            self.push_str("? ");
                            self.emit_string_with_form(text, form);
                            self.push('\n');
                            self.indent(indent);
                            self.push(':');
                        } else {
                            self.emit_string_with_form(text, form);
                            self.push(':');
                        }
                    } else if self.needs_explicit_key(key) {
                        self.push_str("? ");
                        if is_flow_leaf(key) {
                            self.emit_key_leaf(key);
                            self.push('\n');
                            self.indent(indent);
                            self.push(':');
                        } else {
                            self.in_key = true;
                            *stage = BlockMapStage::AfterExplicitKey { key, value };
                            return BlockMapAction::Flow(key);
                        }
                    } else {
                        self.emit_key_leaf(key);
                        self.push(':');
                    }
                    if self.error.is_some() {
                        return BlockMapAction::Done;
                    }
                    self.push_key(key);
                    if self.force_flow()
                        && matches!(
                            value,
                            Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
                        )
                    {
                        self.push(' ');
                        *stage = BlockMapStage::AfterValue { newline: true };
                        return BlockMapAction::Flow(value);
                    }
                    if Self::needs_block_value(value) {
                        self.push('\n');
                        let child_indent = self.block_value_indent(value, indent);
                        *stage = BlockMapStage::AfterValue { newline: false };
                        return BlockMapAction::BlockNode(value, child_indent);
                    }
                    self.push(' ');
                    if self.emit_inline_leaf_fast(value, indent + step) {
                        self.pop_path();
                    } else {
                        *stage = BlockMapStage::AfterValue { newline: false };
                        return BlockMapAction::Inline(value, indent + step);
                    }
                }
            }
        }
    }

    #[inline]
    fn advance_block_sequence<'v>(
        &mut self,
        iter: &mut std::slice::Iter<'v, Value>,
        index: &mut usize,
        indent: usize,
        inline_first: bool,
        stage: &mut BlockSeqStage,
    ) -> BlockSeqAction<'v> {
        let step = self.options.indent;
        loop {
            match std::mem::replace(stage, BlockSeqStage::NextEntry) {
                BlockSeqStage::AfterItem { newline } => {
                    if newline {
                        self.push('\n');
                    }
                    self.pop_path();
                }
                BlockSeqStage::NextEntry => {
                    if self.error.is_some() {
                        return BlockSeqAction::Done;
                    }
                    let Some(item) = iter.next() else {
                        return BlockSeqAction::Done;
                    };
                    let item_index = *index;
                    *index += 1;
                    if item_index > 0 || !inline_first {
                        self.indent(indent);
                    }
                    self.push('-');
                    if self.error.is_some() {
                        return BlockSeqAction::Done;
                    }
                    self.push_index(item_index);
                    if self.force_flow()
                        && matches!(
                            item,
                            Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
                        )
                    {
                        self.push(' ');
                        *stage = BlockSeqStage::AfterItem { newline: true };
                        return BlockSeqAction::Flow(item);
                    }
                    if let Value::Sequence(inner) = item
                        && !inner.is_empty()
                    {
                        // A compact entry starts at the child indentation, so the
                        // `-` is padded to `indent` columns.
                        self.indent(step - 1);
                        *stage = BlockSeqStage::AfterItem { newline: false };
                        return BlockSeqAction::CompactSeq(inner, indent + step);
                    }
                    if let Value::Mapping(mapping) = item
                        && !mapping.is_empty()
                    {
                        self.indent(step - 1);
                        *stage = BlockSeqStage::AfterItem { newline: false };
                        return BlockSeqAction::CompactMap(mapping, indent + step);
                    }
                    if Self::needs_block_value(item) {
                        self.push('\n');
                        *stage = BlockSeqStage::AfterItem { newline: false };
                        return BlockSeqAction::BlockNode(item, indent + step);
                    }
                    self.push(' ');
                    if self.emit_inline_leaf_fast(item, indent + step) {
                        self.pop_path();
                    } else {
                        *stage = BlockSeqStage::AfterItem { newline: false };
                        return BlockSeqAction::Inline(item, indent + step);
                    }
                }
            }
        }
    }

    #[inline(never)]
    fn advance_flow_sequence<'v>(
        &mut self,
        iter: &mut std::slice::Iter<'v, Value>,
        index: &mut usize,
        pop_path: &mut bool,
    ) -> Option<&'v Value> {
        if *pop_path {
            *pop_path = false;
            self.pop_path();
        }
        loop {
            if self.error.is_some() {
                return None;
            }
            let item = iter.next()?;
            #[cfg(test)]
            {
                self.flow_loop_steps += 1;
            }
            let item_index = *index;
            *index += 1;
            if item_index > 0 {
                self.push_str(", ");
                if self.error.is_some() {
                    return None;
                }
            }
            self.push_index(item_index);
            if is_flow_leaf(item) {
                self.emit_flow_leaf(item);
                self.pop_path();
            } else {
                *pop_path = true;
                return Some(item);
            }
        }
    }

    #[inline(never)]
    fn advance_flow_mapping<'v>(
        &mut self,
        iter: &mut crate::mapping::Iter<'v>,
        first: &mut bool,
        stage: &mut FlowMapStage<'v>,
    ) -> Option<&'v Value> {
        loop {
            match std::mem::replace(stage, FlowMapStage::NextEntry) {
                FlowMapStage::AfterValue => {
                    self.pop_path();
                }
                FlowMapStage::AfterKey {
                    key,
                    value,
                    explicit,
                    prev_in_key,
                } => {
                    self.in_key = prev_in_key;
                    if self.error.is_some() {
                        return None;
                    }
                    // A tagged null already ends with a space.
                    if explicit && !matches!(key, Value::Tagged(tagged) if tagged.value.is_null()) {
                        self.push(' ');
                    }
                    self.push_str(": ");
                    if self.error.is_some() {
                        return None;
                    }
                    self.push_key(key);
                    if is_flow_leaf(value) {
                        self.emit_flow_leaf(value);
                        self.pop_path();
                    } else {
                        *stage = FlowMapStage::AfterValue;
                        return Some(value);
                    }
                }
                FlowMapStage::NextEntry => {
                    if self.error.is_some() {
                        return None;
                    }
                    let (key, value) = iter.next()?;
                    #[cfg(test)]
                    {
                        self.flow_loop_steps += 1;
                    }
                    let is_first = *first;
                    *first = false;
                    if !is_first {
                        self.push_str(", ");
                        if self.error.is_some() {
                            return None;
                        }
                    }
                    // libyaml writes non-empty collection keys and long
                    // keys in the explicit form; so do we.
                    let explicit = Self::needs_block_value(key) || self.flow_key_is_long(key);
                    if explicit {
                        self.push_str("? ");
                    }
                    if is_flow_leaf(key) {
                        self.emit_key_leaf(key);
                        if self.error.is_some() {
                            return None;
                        }
                        if explicit {
                            self.push(' ');
                        }
                        self.push_str(": ");
                        if self.error.is_some() {
                            return None;
                        }
                        self.push_key(key);
                        if is_flow_leaf(value) {
                            self.emit_flow_leaf(value);
                            self.pop_path();
                        } else {
                            *stage = FlowMapStage::AfterValue;
                            return Some(value);
                        }
                    } else {
                        let prev_in_key = std::mem::replace(&mut self.in_key, true);
                        *stage = FlowMapStage::AfterKey {
                            key,
                            value,
                            explicit,
                            prev_in_key,
                        };
                        return Some(key);
                    }
                }
            }
        }
    }

    fn start_block_node<'v>(
        &mut self,
        mut value: &'v Value,
        indent: usize,
        stack: &mut TieredStack<'_, '_, Frame<'v>>,
    ) {
        loop {
            match value {
                Value::Mapping(mapping) if !mapping.is_empty() => {
                    self.start_block_mapping(mapping, indent, false, stack);
                    return;
                }
                Value::Sequence(sequence) if !sequence.is_empty() => {
                    self.start_block_sequence(sequence, indent, false, stack);
                    return;
                }
                Value::Tagged(tagged) => {
                    self.indent(indent);
                    if !self.push_tag(tagged) {
                        return;
                    }
                    self.push('\n');
                    if tagged.value.is_null() {
                        return;
                    }
                    value = &tagged.value;
                }
                _ => {
                    self.indent(indent);
                    self.emit_flow_leaf(value);
                    self.push('\n');
                    return;
                }
            }
        }
    }

    /// Emit a leaf inline value directly without touching the stack; returns `false` only if
    /// `value` is a non-empty collection or tagged value requiring full `start_inline_value`.
    #[inline]
    fn emit_inline_leaf_fast(&mut self, value: &Value, indent: usize) -> bool {
        match value {
            Value::Mapping(mapping) if !mapping.is_empty() => false,
            Value::Sequence(sequence) if !sequence.is_empty() => false,
            Value::Tagged(_) => false,
            Value::String(text) if self.use_block_scalar(text) => {
                self.emit_block_scalar(text, indent, false);
                true
            }
            _ => {
                self.emit_flow_leaf(value);
                self.push('\n');
                true
            }
        }
    }

    /// Emit a node that follows `key: ` or `- `, ending with a line break.
    fn start_inline_value<'v>(
        &mut self,
        value: &'v Value,
        indent: usize,
        stack: &mut TieredStack<'_, '_, Frame<'v>>,
    ) {
        match value {
            Value::Mapping(mapping) if !mapping.is_empty() => {
                self.push('\n');
                self.start_block_mapping(mapping, indent, false, stack);
            }
            Value::Sequence(sequence) if !sequence.is_empty() => {
                self.push('\n');
                self.start_block_sequence(sequence, indent, false, stack);
            }
            Value::Tagged(tagged) => {
                if !self.push_tag(tagged) {
                    return;
                }
                if tagged.value.is_null() {
                    self.push('\n');
                } else if let Value::String(text) = &tagged.value
                    && self.use_block_scalar(text)
                {
                    self.push(' ');
                    self.emit_block_scalar(text, indent, false);
                } else if Self::needs_block_value(&tagged.value) {
                    self.push('\n');
                    self.start_block_node(&tagged.value, indent, stack);
                } else {
                    self.push(' ');
                    self.emit_flow_leaf(&tagged.value);
                    self.push('\n');
                }
            }
            Value::String(text) if self.use_block_scalar(text) => {
                self.emit_block_scalar(text, indent, false);
            }
            _ => {
                self.emit_flow_leaf(value);
                self.push('\n');
            }
        }
    }

    fn start_flow<'v>(&mut self, mut value: &'v Value, stack: &mut TieredStack<'_, '_, Frame<'v>>) {
        loop {
            match value {
                Value::Sequence(sequence) if !sequence.is_empty() => {
                    if !self.enter() {
                        return;
                    }
                    let prev_in_flow = std::mem::replace(&mut self.in_flow, true);
                    self.push('[');
                    stack.push(Frame::FlowSequence {
                        iter: sequence.iter(),
                        index: 0,
                        prev_in_flow,
                        pop_path: false,
                    });
                    return;
                }
                Value::Mapping(mapping) if !mapping.is_empty() => {
                    if !self.enter() {
                        return;
                    }
                    let prev_in_flow = std::mem::replace(&mut self.in_flow, true);
                    self.push('{');
                    stack.push(Frame::FlowMapping {
                        iter: mapping.iter(),
                        first: true,
                        prev_in_flow,
                        stage: FlowMapStage::NextEntry,
                    });
                    return;
                }
                Value::Tagged(tagged) => {
                    if !self.push_tag(tagged) {
                        return;
                    }
                    if tagged.value.is_null() {
                        // `[!t ]`: libyaml needs a space between a tag and a
                        // flow indicator.
                        if self.in_flow {
                            self.push(' ');
                        }
                        return;
                    }
                    self.push(' ');
                    value = &tagged.value;
                }
                _ => {
                    self.emit_flow_leaf(value);
                    return;
                }
            }
        }
    }

    /// Write a scalar or empty collection in flow style without touching the heap stack.
    #[inline]
    fn emit_flow_leaf(&mut self, value: &Value) {
        match value {
            Value::Null => self.push_str("null"),
            Value::Bool(value) => self.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => self.push_str(&value.to_string()),
            Value::String(value) => self.emit_string(value),
            // An empty collection is one level too, as the parser and the event writer count
            // it: what is written within `max_depth` is read back within it.
            Value::Sequence(_) => {
                if self.enter() {
                    self.push_str("[]");
                    self.leave();
                }
            }
            Value::Mapping(_) => {
                if self.enter() {
                    self.push_str("{}");
                    self.leave();
                }
            }
            Value::Tagged(_) => unreachable!("tagged values are handled by start_flow"),
        }
    }

    #[inline]
    fn emit_key_leaf(&mut self, key: &Value) {
        let in_key = std::mem::replace(&mut self.in_key, true);
        self.emit_flow_leaf(key);
        self.in_key = in_key;
    }

    /// Whether a block-mapping key must use the explicit `? ` form.
    fn needs_explicit_key(&self, key: &Value) -> bool {
        match key {
            Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => true,
            Value::String(text) => self.key_exceeds_implicit_limit(text),
            _ => false,
        }
    }

    fn key_exceeds_implicit_limit(&self, text: &str) -> bool {
        let form = self.string_form(text, self.options.scalar_style);
        Self::string_key_exceeds_limit(text, form)
    }

    fn string_key_exceeds_limit(text: &str, form: StringForm) -> bool {
        const CAP: usize = MAX_IMPLICIT_KEY_CHARS + 1;
        match form {
            StringForm::Plain => {
                text.len() > MAX_IMPLICIT_KEY_CHARS
                    && text.chars().take(CAP).count() > MAX_IMPLICIT_KEY_CHARS
            }
            StringForm::SingleQuoted => {
                single_quoted_char_len_capped(text, CAP) > MAX_IMPLICIT_KEY_CHARS
            }
            StringForm::DoubleQuoted => {
                double_quoted_char_len_capped(text, CAP) > MAX_IMPLICIT_KEY_CHARS
            }
        }
    }

    /// How many characters a string key takes as a mapping key in the current flow context,
    /// capped at `MAX_IMPLICIT_KEY_CHARS + 1` so long keys never allocate.
    fn key_width(&self, text: &str) -> usize {
        const CAP: usize = MAX_IMPLICIT_KEY_CHARS + 1;
        match self.string_form(text, self.options.scalar_style) {
            StringForm::Plain => text.chars().take(CAP).count(),
            StringForm::SingleQuoted => single_quoted_char_len_capped(text, CAP),
            StringForm::DoubleQuoted => double_quoted_char_len_capped(text, CAP),
        }
    }

    /// Whether a flow-mapping key is a scalar whose text, with any tag, runs past
    /// [`MAX_IMPLICIT_KEY_CHARS`]. YAML puts no limit on an implicit key in a flow mapping, but
    /// readers built on libyaml refuse one past 1024 characters there too.
    fn flow_key_is_long(&mut self, key: &Value) -> bool {
        const CAP: usize = MAX_IMPLICIT_KEY_CHARS + 1;
        match key {
            Value::String(_) => self.needs_explicit_key(key),
            Value::Tagged(tagged) => {
                // Nested tags fail when written; do not mutate `self.directives` here.
                if matches!(tagged.value, Value::Tagged(_)) {
                    return false;
                }
                // A tag with no form fails when it is written.
                let Ok(token_len) = self.directives.token_char_len_capped(
                    &tagged.tag,
                    RedefinedHandles::default(),
                    CAP,
                    self.output,
                ) else {
                    return false;
                };
                let text = match &tagged.value {
                    Value::String(text) => self.key_width(text),
                    Value::Bool(value) => {
                        if *value {
                            4
                        } else {
                            5
                        }
                    }
                    Value::Number(number) => number.to_string().len(),
                    // A null writes no text, and a collection is explicit already.
                    _ => 0,
                };
                token_len.saturating_add(1).saturating_add(text) > MAX_IMPLICIT_KEY_CHARS
            }
            _ => false,
        }
    }

    fn block_value_indent(&self, value: &Value, indent: usize) -> usize {
        let sequence_like = matches!(value, Value::Sequence(sequence) if !sequence.is_empty());
        if !self.options.indent_sequences && sequence_like {
            indent
        } else {
            indent + self.options.indent
        }
    }

    fn needs_block_value(mut value: &Value) -> bool {
        loop {
            match value {
                Value::Mapping(mapping) => return !mapping.is_empty(),
                Value::Sequence(sequence) => return !sequence.is_empty(),
                Value::Tagged(tagged) => value = &tagged.value,
                _ => return false,
            }
        }
    }

    fn use_block_scalar(&self, text: &str) -> bool {
        if self.in_flow || self.in_key {
            return false;
        }
        let block_style = match self.scalar_style() {
            Some(
                crate::event::ScalarStyle::SingleQuoted | crate::event::ScalarStyle::DoubleQuoted,
            ) => return false,
            Some(crate::event::ScalarStyle::Literal | crate::event::ScalarStyle::Folded) => true,
            _ => text.contains('\n'),
        };
        block_style && block_scalar_ok(text)
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the indentation indicator is a single digit in 1..=9"
    )]
    fn emit_block_scalar(&mut self, text: &str, content_indent: usize, root: bool) {
        if self.error.is_some() {
            return;
        }
        // A root node's indentation is -1 and an indicator is one digit, so keep a root
        // scalar's content at most eight columns in.
        let content_indent = if root {
            content_indent.min(8)
        } else {
            content_indent
        };
        let trailing = text.chars().rev().take_while(|ch| *ch == '\n').count();
        let body = &text[..text.len().saturating_sub(trailing)];
        let folded = matches!(self.scalar_style(), Some(crate::event::ScalarStyle::Folded))
            && can_fold(body);
        let inferred = if trailing == 0 {
            Chomping::Strip
        } else if trailing == 1 && !body.is_empty() {
            Chomping::Clip
        } else {
            Chomping::Keep
        };
        let requested = self.style().chomping;
        let chomping = match requested {
            Some(Chomping::Strip) if trailing == 0 => Chomping::Strip,
            Some(Chomping::Clip)
                if (trailing == 1 && !body.is_empty()) || (body.is_empty() && trailing == 0) =>
            {
                Chomping::Clip
            }
            Some(Chomping::Keep) if trailing > 0 || body.is_empty() => Chomping::Keep,
            _ => inferred,
        };
        let indicator = if folded { '>' } else { '|' };
        self.push(indicator);
        if needs_explicit_indent(text) {
            // The indicator counts from the parent node. A root node's indentation is -1, so a
            // root scalar needs one more than its content indentation; a nested scalar needs one
            // indentation step.
            let indicator = if root {
                content_indent + 1
            } else {
                self.options.indent
            };
            self.push(char::from_digit(indicator as u32, 10).unwrap_or('0'));
        }
        match chomping {
            Chomping::Strip => self.push('-'),
            Chomping::Clip => {}
            Chomping::Keep => self.push('+'),
        }
        self.push('\n');
        if self.error.is_some() {
            return;
        }
        if body.is_empty() {
            for _ in 0..trailing {
                if self.error.is_some() {
                    break;
                }
                #[cfg(test)]
                {
                    self.block_scalar_lines += 1;
                }
                self.push('\n');
            }
        } else if folded {
            // Folding only inserts line breaks, so `body.len()` is a lower bound on the bytes that
            // will be written; reject immediately if it already exceeds `max_output_bytes`.
            if let Err(err) = self.output.check_additional(body.len()) {
                self.fail(err);
                return;
            }
            if let Err(err) = write_folded_body(
                body,
                content_indent,
                self.output,
                #[cfg(test)]
                Some(&mut self.block_scalar_lines),
            ) {
                self.fail(err);
                return;
            }
            for _ in 1..trailing {
                if self.error.is_some() {
                    break;
                }
                #[cfg(test)]
                {
                    self.block_scalar_lines += 1;
                }
                self.push('\n');
            }
        } else {
            for line in body.split('\n') {
                if self.error.is_some() {
                    break;
                }
                #[cfg(test)]
                {
                    self.block_scalar_lines += 1;
                }
                if !line.is_empty() {
                    self.indent(content_indent);
                    self.push_str(line);
                }
                self.push('\n');
            }
            for _ in 1..trailing {
                if self.error.is_some() {
                    break;
                }
                #[cfg(test)]
                {
                    self.block_scalar_lines += 1;
                }
                self.push('\n');
            }
        }
    }

    /// How [`ValueWriter::emit_string`] writes `value` under `style`, in the current flow context.
    fn string_form(&self, value: &str, style: Option<crate::event::ScalarStyle>) -> StringForm {
        match style {
            Some(crate::event::ScalarStyle::SingleQuoted) if single_quoted_ok(value) => {
                StringForm::SingleQuoted
            }
            Some(
                crate::event::ScalarStyle::SingleQuoted
                | crate::event::ScalarStyle::DoubleQuoted
                | crate::event::ScalarStyle::Literal
                | crate::event::ScalarStyle::Folded,
            ) => StringForm::DoubleQuoted,
            _ if self.in_flow && plain_is_safe(value) => StringForm::Plain,
            _ if !self.in_flow && plain_is_block_safe(value) => StringForm::Plain,
            _ => StringForm::DoubleQuoted,
        }
    }

    fn emit_string(&mut self, value: &str) {
        let form = self.string_form(value, self.scalar_style());
        self.emit_string_with_form(value, form);
    }

    #[inline]
    fn emit_string_with_form(&mut self, value: &str, form: StringForm) {
        match form {
            StringForm::Plain => self.push_str(value),
            StringForm::SingleQuoted => self.emit_single_quoted(value),
            StringForm::DoubleQuoted => self.emit_double_quoted(value),
        }
    }

    #[inline(never)]
    fn emit_single_quoted(&mut self, value: &str) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = write_single_quoted(
            value,
            self.output,
            #[cfg(test)]
            Some(&mut self.quoted_chunks),
        ) {
            self.fail(error);
        }
    }

    #[inline(never)]
    fn emit_double_quoted(&mut self, value: &str) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = write_double_quoted(
            value,
            self.output,
            #[cfg(test)]
            Some(&mut self.quoted_chunks),
        ) {
            self.fail(error);
        }
    }
}
