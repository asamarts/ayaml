//! Event-stream YAML emission.
//!
//! [`to_string_events`] and [`to_writer_event_stream`] share one writer. It
//! keeps block collections in block layout and flow collections in flow
//! layout. It also keeps tags, anchors, aliases, directives, explicit
//! document markers, and each scalar's style, as long as that style can hold
//! the value where the scalar appears. Otherwise the scalar is written
//! double-quoted, which can hold any value. A plain scalar only falls back to
//! quotes when its text resolves to a string, so quoting never changes a
//! value's type. The writer reads one event ahead, and holds each document's
//! text until the document ends, so that the `%TAG` directives that some tags
//! need (see [`Tag`]) come before it. The lazy entry point therefore needs
//! memory for the nesting depth and for one document's text, which
//! `max_output_bytes` bounds.

use std::borrow::Borrow;
use std::io::Write;
use std::marker::PhantomData;

use crate::EventStream;
use crate::emitter::{
    MAX_IMPLICIT_KEY_CHARS, Output, RedefinedHandles, TagDirectives, block_scalar_ok, can_fold,
    emit_double_quoted, expand_folded, is_printable, looks_like_leading_zero_numeral,
    needs_explicit_indent, plain_text_ok, single_quoted_ok,
};
use crate::error::{Error, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::tag::Tag;

/// Indentation step of the event writer.
const STEP: usize = 2;

/// Emit a collected event stream as YAML text.
pub fn to_string_events(events: &[Event<'_>]) -> Result<String, Error> {
    to_string_events_with_limits(events, &Limits::default())
}

/// Emit a collected event stream with an explicit output-size limit.
pub fn to_string_events_with_limits(
    events: &[Event<'_>],
    limits: &Limits,
) -> Result<String, Error> {
    let mut output = Vec::new();
    write_events(
        Output::direct(&mut output, limits),
        events.iter().map(Ok::<_, Error>),
        limits,
    )?;
    String::from_utf8(output).map_err(|error| {
        Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
    })
}

/// Write a collected event stream to a writer.
pub fn to_writer_events<W: Write>(writer: &mut W, events: &[Event<'_>]) -> Result<(), Error> {
    let limits = Limits::default();
    write_events(
        Output::new(writer, &limits),
        events.iter().map(Ok::<_, Error>),
        &limits,
    )
}

/// Write a lazy event stream as it is parsed.
///
/// The output is the same as [`to_string_events`] would produce for the
/// collected events. It holds one document's text at a time, since the
/// document's `%TAG` directives come first, and otherwise needs memory for the
/// nesting depth.
pub fn to_writer_event_stream<W: Write>(
    writer: &mut W,
    stream: EventStream<'_>,
) -> Result<(), Error> {
    to_writer_event_stream_with_limits(writer, stream, &Limits::default())
}

/// Write a lazy event stream with a cumulative output limit.
pub fn to_writer_event_stream_with_limits<W: Write>(
    writer: &mut W,
    stream: EventStream<'_>,
    limits: &Limits,
) -> Result<(), Error> {
    write_events(Output::new(writer, limits), stream, limits)
}

fn emitter_error(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Emitter, message)
}

fn write_events<'e, W, I, B>(
    mut output: Output<'_, W>,
    events: I,
    limits: &Limits,
) -> Result<(), Error>
where
    W: Write,
    I: Iterator<Item = Result<B, Error>>,
    B: Borrow<Event<'e>>,
{
    limits.validate()?;
    let mut emitter = EventWriter {
        output: &mut output,
        events,
        pending: None,
        limits,
        depth: 0,
        handles: RedefinedHandles::default(),
        directives: TagDirectives::default(),
        declared: Vec::new(),
        marker: PhantomData,
    };
    emitter.run()?;
    output.finish()
}

/// Where a block-context node is written.
#[derive(Clone, Copy)]
enum Pos {
    /// A document's root, at the start of a line.
    Root,
    /// After `-`, `?`, or an explicit-value `:` that belongs to a block
    /// collection at this column. A block collection can start on the same
    /// line (a compact entry).
    Entry(usize),
    /// After `key:` in a block mapping at this column.
    Value(usize),
}

/// An open collection in the writer; the innermost is on top of the stack.
#[derive(Clone, Copy)]
enum Frame {
    /// A block sequence at this column. With `inline`, its next entry
    /// continues the current line (a compact entry).
    BlockSequence {
        indent: usize,
        inline: bool,
    },
    /// A block mapping at this column, before its next key.
    BlockMapping {
        indent: usize,
        inline: bool,
    },
    /// After `? key` in a block mapping at this column: the `:` line and
    /// the value.
    BlockValue {
        indent: usize,
    },
    FlowSequence {
        first: bool,
    },
    FlowMapping {
        first: bool,
    },
    /// After a flow-mapping key that is a collection: `: ` and the value.
    FlowValue,
    /// After a flow collection written in block context: its line break.
    LineEnd,
}

impl Pos {
    fn content_indent(self) -> usize {
        match self {
            Self::Root => STEP,
            Self::Entry(indent) | Self::Value(indent) => indent + STEP,
        }
    }
}

fn is_empty_plain(event: &Event<'_>) -> bool {
    matches!(
        event,
        Event::Scalar {
            value,
            style: ScalarStyle::Plain,
            tag: None,
            anchor: None,
            ..
        } if value.is_empty()
    )
}

/// Whether an anchor name reads back unchanged: non-empty, printable, and
/// free of white space, line breaks, and flow indicators.
fn anchor_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.chars().all(|ch| {
            is_printable(ch)
                && !matches!(
                    ch,
                    ' ' | '\t' | '\n' | '\r' | '\u{feff}' | ',' | '[' | ']' | '{' | '}'
                )
        })
}

/// Append a scalar on one line: plain or single-quoted when the text allows
/// that style in this context, double-quoted otherwise.
fn scalar_inline(
    value: &str,
    style: ScalarStyle,
    flow: bool,
    tag: Option<&Tag>,
    output: &mut String,
) {
    let plain =
        plain_text_ok(value, flow) && (tag.is_some() || !looks_like_leading_zero_numeral(value));
    match style {
        ScalarStyle::Plain if plain => output.push_str(value),
        ScalarStyle::SingleQuoted if single_quoted_ok(value) => {
            output.push('\'');
            for (index, part) in value.split('\'').enumerate() {
                if index > 0 {
                    output.push_str("''");
                }
                output.push_str(part);
            }
            output.push('\'');
        }
        _ => emit_double_quoted(value, output),
    }
}

struct EventWriter<'o, 'w, 'l, 'e, W: Write, I, B> {
    output: &'o mut Output<'w, W>,
    events: I,
    pending: Option<B>,
    limits: &'l Limits,
    depth: usize,
    handles: RedefinedHandles,
    /// The `%TAG` directives added to the document being written (D82).
    directives: TagDirectives,
    /// The named handles that the next document's own `%TAG` directives declare.
    declared: Vec<String>,
    marker: PhantomData<Event<'e>>,
}

impl<'e, W, I, B> EventWriter<'_, '_, '_, 'e, W, I, B>
where
    W: Write,
    I: Iterator<Item = Result<B, Error>>,
    B: Borrow<Event<'e>>,
{
    fn next(&mut self) -> Result<Option<B>, Error> {
        if let Some(event) = self.pending.take() {
            return Ok(Some(event));
        }
        self.events.next().transpose()
    }

    fn peek(&mut self) -> Result<Option<&Event<'e>>, Error> {
        if self.pending.is_none() {
            self.pending = self.events.next().transpose()?;
        }
        Ok(self.pending.as_ref().map(Borrow::borrow))
    }

    fn node_event(&mut self) -> Result<B, Error> {
        self.next()?
            .ok_or_else(|| emitter_error("unexpected end of event stream"))
    }

    fn push(&mut self, value: &str) -> Result<(), Error> {
        self.output.push_str(value)
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        self.limits.check_depth(self.depth).map_err(Error::from)
    }

    fn run(&mut self) -> Result<(), Error> {
        let mut documents = 0_usize;
        let mut open_ended = false;
        let mut directives = false;
        while let Some(event) = self.next()? {
            match event.borrow() {
                Event::StreamStart { .. } | Event::StreamEnd { .. } => {}
                Event::Directive { name, value, .. } => {
                    // The parser reads a name up to a space or tab, and
                    // neither may hold a line break or a byte order mark.
                    // NEL, LS, and PS are content; a directive has no
                    // escapes, so they are written as they are.
                    let directive_char =
                        |ch: char| is_printable(ch) && !matches!(ch, '\n' | '\r' | '\u{feff}');
                    if name.is_empty()
                        || !name
                            .chars()
                            .all(|ch| directive_char(ch) && !matches!(ch, ' ' | '\t'))
                        || !value.chars().all(directive_char)
                    {
                        return Err(emitter_error(format!(
                            "cannot write the directive %{name} {value}"
                        )));
                    }
                    // A directive cannot follow a document left open.
                    if open_ended {
                        self.push("...\n")?;
                        open_ended = false;
                    }
                    self.push("%")?;
                    self.push(name)?;
                    self.push(" ")?;
                    self.push(value)?;
                    self.push("\n")?;
                    if name == "TAG" {
                        match value.split([' ', '\t']).find(|part| !part.is_empty()) {
                            Some("!") => self.handles.primary = true,
                            Some("!!") => self.handles.secondary = true,
                            Some(handle) => self.declared.push(handle.to_owned()),
                            None => {}
                        }
                    }
                    directives = true;
                }
                Event::DocumentStart { explicit, .. } => {
                    let empty_root = match self.peek()? {
                        Some(Event::DocumentEnd { .. }) | None => {
                            return Err(emitter_error("document has no root node"));
                        }
                        Some(root) => is_empty_plain(root),
                    };
                    // The directives a tag needs are known only once the document is written,
                    // so it is held until then, and they go in before it.
                    self.directives = TagDirectives::declaring(std::mem::take(&mut self.declared));
                    let held = self.output.hold();
                    // An empty root needs `---`, or the document would vanish.
                    let marker = *explicit || documents > 0 || directives || empty_root;
                    if marker {
                        self.push("---\n")?;
                    }
                    let root = self.node_event()?;
                    self.node(root, Pos::Root)?;
                    if !self.directives.is_empty() {
                        let mut header = String::new();
                        // A directive can't follow a document that `...` didn't end. The
                        // document's own directives wrote one already, and cleared `open_ended`.
                        if open_ended {
                            header.push_str("...\n");
                        }
                        header.push_str(&self.directives.lines());
                        if !marker {
                            header.push_str("---\n");
                        }
                        self.output.insert_held(held, &header)?;
                    }
                    self.output.release()?;
                    let end = self.next()?;
                    match end.as_ref().map(Borrow::borrow) {
                        Some(Event::DocumentEnd { explicit, .. }) => {
                            open_ended = !*explicit;
                            if *explicit {
                                self.push("...\n")?;
                            }
                        }
                        _ => return Err(emitter_error("document has no end event")),
                    }
                    documents += 1;
                    directives = false;
                    self.handles = RedefinedHandles::default();
                }
                _ => return Err(emitter_error("unexpected event outside a document")),
            }
        }
        if directives {
            return Err(emitter_error("directives are not followed by a document"));
        }
        Ok(())
    }

    /// `&anchor !tag`, or an empty string.
    fn properties(
        &mut self,
        anchor: Option<&Anchor<'_>>,
        tag: Option<&Tag>,
    ) -> Result<String, Error> {
        let mut output = String::new();
        if let Some(anchor) = anchor {
            if !anchor_name_ok(anchor.as_str()) {
                return Err(emitter_error(format!(
                    "cannot write the anchor name {:?}",
                    anchor.as_str()
                )));
            }
            output.push('&');
            output.push_str(anchor.as_str());
        }
        if let Some(tag) = tag {
            if !output.is_empty() {
                output.push(' ');
            }
            output.push_str(&self.directives.token(tag, self.handles)?);
        }
        Ok(output)
    }

    fn alias(&mut self, anchor: &Anchor<'_>) -> Result<(), Error> {
        if !anchor_name_ok(anchor.as_str()) {
            return Err(emitter_error(format!(
                "cannot write the alias name {:?}",
                anchor.as_str()
            )));
        }
        self.push("*")?;
        self.push(anchor.as_str())
    }

    fn separate(&mut self, pos: Pos) -> Result<(), Error> {
        if matches!(pos, Pos::Root) {
            Ok(())
        } else {
            self.push(" ")
        }
    }

    /// Write a node in block context, ending with a line break. Open
    /// collections are kept on an explicit stack, so nesting costs heap, not
    /// native stack.
    fn node(&mut self, event: B, pos: Pos) -> Result<(), Error> {
        let mut stack = Vec::new();
        self.block_node(event, pos, &mut stack)?;
        while let Some(frame) = stack.pop() {
            self.resume(frame, &mut stack)?;
        }
        Ok(())
    }

    /// Start a node in block context: write a scalar or an alias, or open a
    /// collection on `stack`.
    fn block_node(&mut self, event: B, pos: Pos, stack: &mut Vec<Frame>) -> Result<(), Error> {
        match event.borrow() {
            Event::Alias { anchor, .. } => {
                self.separate(pos)?;
                self.alias(anchor)?;
                self.push("\n")
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => self.scalar_node(value, *style, tag.as_ref(), anchor.as_ref(), pos),
            Event::SequenceStart {
                tag, anchor, style, ..
            }
            | Event::MappingStart {
                tag, anchor, style, ..
            } => {
                let sequence = matches!(event.borrow(), Event::SequenceStart { .. });
                let empty = match self.peek()? {
                    Some(Event::SequenceEnd { .. }) => sequence,
                    Some(Event::MappingEnd { .. }) => !sequence,
                    _ => false,
                };
                if *style == CollectionStyle::Flow || empty {
                    self.separate(pos)?;
                    stack.push(Frame::LineEnd);
                    self.flow_collection(sequence, tag.as_ref(), anchor.as_ref(), stack)
                } else {
                    self.block_collection(sequence, tag.as_ref(), anchor.as_ref(), pos, stack)
                }
            }
            _ => Err(emitter_error("unexpected event in node position")),
        }
    }

    fn scalar_node(
        &mut self,
        value: &str,
        style: ScalarStyle,
        tag: Option<&Tag>,
        anchor: Option<&Anchor<'_>>,
        pos: Pos,
    ) -> Result<(), Error> {
        let mut text = self.properties(anchor, tag)?;
        if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) && block_scalar_ok(value) {
            self.separate(pos)?;
            if !text.is_empty() {
                text.push(' ');
                self.push(&text)?;
            }
            return self.block_scalar(value, style, pos.content_indent(), matches!(pos, Pos::Root));
        }
        if style == ScalarStyle::Plain && value.is_empty() {
            if text.is_empty() {
                // The root of a document that is only `---`.
                return if matches!(pos, Pos::Root) {
                    Ok(())
                } else {
                    self.push("\n")
                };
            }
            self.separate(pos)?;
            self.push(&text)?;
            return self.push("\n");
        }
        self.separate(pos)?;
        if !text.is_empty() {
            text.push(' ');
        }
        scalar_inline(value, style, false, tag, &mut text);
        text.push('\n');
        self.push(&text)
    }

    fn block_scalar(
        &mut self,
        value: &str,
        style: ScalarStyle,
        content_indent: usize,
        root: bool,
    ) -> Result<(), Error> {
        let content_indent = if root {
            content_indent.min(8)
        } else {
            content_indent
        };
        let trailing = value.chars().rev().take_while(|ch| *ch == '\n').count();
        let body = &value[..value.len() - trailing];
        let folded = style == ScalarStyle::Folded && can_fold(body);
        let mut header = String::from(if folded { ">" } else { "|" });
        if needs_explicit_indent(value) {
            // Content sits one step below the parent node; a root node's indentation is -1, so
            // a root scalar's indicator is one more than its content indentation.
            let indicator = if root { content_indent + 1 } else { STEP };
            header.push(char::from_digit(indicator as u32, 10).expect("single digit"));
        }
        if trailing == 0 {
            header.push('-');
        } else if trailing > 1 || body.is_empty() {
            header.push('+');
        }
        header.push('\n');
        self.push(&header)?;
        if body.is_empty() {
            for _ in 0..trailing {
                self.push("\n")?;
            }
            return Ok(());
        }
        let folded_body = folded.then(|| expand_folded(body));
        for line in folded_body.as_deref().unwrap_or(body).split('\n') {
            if !line.is_empty() {
                self.output.spaces(content_indent)?;
                self.push(line)?;
            }
            self.push("\n")?;
        }
        for _ in 1..trailing {
            self.push("\n")?;
        }
        Ok(())
    }

    /// Write a block collection's properties and open it on `stack`.
    fn block_collection(
        &mut self,
        sequence: bool,
        tag: Option<&Tag>,
        anchor: Option<&Anchor<'_>>,
        pos: Pos,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let properties = self.properties(anchor, tag)?;
        let (indent, inline) = match pos {
            Pos::Root => {
                if !properties.is_empty() {
                    self.push(&properties)?;
                    self.push("\n")?;
                }
                (0, false)
            }
            Pos::Entry(indent) if properties.is_empty() => {
                self.push(" ")?;
                (indent + STEP, true)
            }
            Pos::Entry(indent) => {
                self.push(" ")?;
                self.push(&properties)?;
                self.push("\n")?;
                (indent + STEP, false)
            }
            Pos::Value(indent) => {
                if !properties.is_empty() {
                    self.push(" ")?;
                    self.push(&properties)?;
                }
                self.push("\n")?;
                // Sequences under a mapping key are indentless, as libyaml
                // writes them.
                (if sequence { indent } else { indent + STEP }, false)
            }
        };
        self.enter()?;
        stack.push(if sequence {
            Frame::BlockSequence { indent, inline }
        } else {
            Frame::BlockMapping { indent, inline }
        });
        Ok(())
    }

    /// Take the next step of the innermost open collection: write its next
    /// entry, opening any collection the entry starts above it, or close it.
    fn resume(&mut self, frame: Frame, stack: &mut Vec<Frame>) -> Result<(), Error> {
        match frame {
            Frame::BlockSequence { indent, inline } => {
                let event = self
                    .next()?
                    .ok_or_else(|| emitter_error("unterminated sequence"))?;
                if matches!(event.borrow(), Event::SequenceEnd { .. }) {
                    self.depth -= 1;
                    return Ok(());
                }
                if !inline {
                    self.output.spaces(indent)?;
                }
                self.push("-")?;
                stack.push(Frame::BlockSequence {
                    indent,
                    inline: false,
                });
                self.block_node(event, Pos::Entry(indent), stack)
            }
            Frame::BlockMapping { indent, inline } => {
                let key = self
                    .next()?
                    .ok_or_else(|| emitter_error("unterminated mapping"))?;
                if matches!(key.borrow(), Event::MappingEnd { .. }) {
                    self.depth -= 1;
                    return Ok(());
                }
                if !inline {
                    self.output.spaces(indent)?;
                }
                stack.push(Frame::BlockMapping {
                    indent,
                    inline: false,
                });
                if let Some(mut text) = self.implicit_key(key.borrow())? {
                    text.push(':');
                    self.push(&text)?;
                    let value = self.node_event()?;
                    self.block_node(value, Pos::Value(indent), stack)
                } else {
                    self.push("?")?;
                    stack.push(Frame::BlockValue { indent });
                    self.block_node(key, Pos::Entry(indent), stack)
                }
            }
            Frame::BlockValue { indent } => {
                self.output.spaces(indent)?;
                self.push(":")?;
                let value = self.node_event()?;
                self.block_node(value, Pos::Entry(indent), stack)
            }
            Frame::FlowSequence { first } => {
                let event = self
                    .next()?
                    .ok_or_else(|| emitter_error("unterminated sequence"))?;
                if matches!(event.borrow(), Event::SequenceEnd { .. }) {
                    return self.close_flow("]");
                }
                if !first {
                    self.push(", ")?;
                }
                if is_empty_plain(event.borrow()) {
                    return Err(emitter_error(
                        "an empty plain scalar without a tag or anchor cannot be a flow sequence entry",
                    ));
                }
                stack.push(Frame::FlowSequence { first: false });
                self.flow_node(event, stack).map(drop)
            }
            Frame::FlowMapping { first } => {
                let key = self
                    .next()?
                    .ok_or_else(|| emitter_error("unterminated mapping"))?;
                if matches!(key.borrow(), Event::MappingEnd { .. }) {
                    return self.close_flow("}");
                }
                if !first {
                    self.push(", ")?;
                }
                if self.flow_key_is_explicit(key.borrow())? {
                    self.push("? ")?;
                }
                stack.push(Frame::FlowMapping { first: false });
                if matches!(
                    key.borrow(),
                    Event::SequenceStart { .. } | Event::MappingStart { .. }
                ) {
                    // The `: ` follows once the key collection closes.
                    stack.push(Frame::FlowValue);
                    return self.flow_node(key, stack).map(drop);
                }
                if self.flow_node(key, stack)? {
                    self.push(" ")?;
                }
                self.flow_value(stack)
            }
            Frame::FlowValue => self.flow_value(stack),
            Frame::LineEnd => self.push("\n"),
        }
    }

    /// Write a flow mapping's `: ` and start its value.
    fn flow_value(&mut self, stack: &mut Vec<Frame>) -> Result<(), Error> {
        self.push(": ")?;
        let value = self.node_event()?;
        self.flow_node(value, stack).map(drop)
    }

    fn close_flow(&mut self, close: &str) -> Result<(), Error> {
        self.push(close)?;
        self.depth -= 1;
        Ok(())
    }

    /// The text of a block-mapping key written in the implicit form, or
    /// `None` when the key needs `? `: collections, empty keys, block
    /// scalars, and keys longer than [`MAX_IMPLICIT_KEY_CHARS`].
    fn implicit_key(&mut self, key: &Event<'_>) -> Result<Option<String>, Error> {
        match key {
            Event::Alias { anchor, .. } => {
                if !anchor_name_ok(anchor.as_str()) {
                    return Err(emitter_error(format!(
                        "cannot write the alias name {:?}",
                        anchor.as_str()
                    )));
                }
                // `:` can be part of an alias name, so it needs a space.
                Ok(Some(format!("*{} ", anchor.as_str())))
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => {
                let block = matches!(style, ScalarStyle::Literal | ScalarStyle::Folded)
                    && block_scalar_ok(value);
                if block || (*style == ScalarStyle::Plain && value.is_empty()) {
                    return Ok(None);
                }
                let mut text = self.properties(anchor.as_ref(), tag.as_ref())?;
                if !text.is_empty() {
                    text.push(' ');
                }
                scalar_inline(value, *style, false, tag.as_ref(), &mut text);
                Ok((text.chars().count() <= MAX_IMPLICIT_KEY_CHARS).then_some(text))
            }
            _ => Ok(None),
        }
    }

    /// Write a flow collection's properties and opening bracket, and open it
    /// on `stack`; its start event is consumed.
    fn flow_collection(
        &mut self,
        sequence: bool,
        tag: Option<&Tag>,
        anchor: Option<&Anchor<'_>>,
        stack: &mut Vec<Frame>,
    ) -> Result<(), Error> {
        let mut properties = self.properties(anchor, tag)?;
        if !properties.is_empty() {
            properties.push(' ');
            self.push(&properties)?;
        }
        self.enter()?;
        self.push(if sequence { "[" } else { "{" })?;
        stack.push(if sequence {
            Frame::FlowSequence { first: true }
        } else {
            Frame::FlowMapping { first: true }
        });
        Ok(())
    }

    /// Whether a flow-mapping key needs `? `, as libyaml writes it: an empty
    /// key, a non-empty collection, or a long scalar.
    fn flow_key_is_explicit(&mut self, key: &Event<'_>) -> Result<bool, Error> {
        Ok(match key {
            Event::SequenceStart { .. } => !matches!(self.peek()?, Some(Event::SequenceEnd { .. })),
            Event::MappingStart { .. } => !matches!(self.peek()?, Some(Event::MappingEnd { .. })),
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => {
                if is_empty_plain(key) {
                    true
                } else {
                    let mut text = self.properties(anchor.as_ref(), tag.as_ref())?;
                    scalar_inline(value, *style, true, tag.as_ref(), &mut text);
                    text.chars().count() > MAX_IMPLICIT_KEY_CHARS
                }
            }
            _ => false,
        })
    }

    /// Start a node inside a flow collection: write a scalar or an alias, or
    /// open a collection on `stack`. Returns true when a following `:` must
    /// be separated by a space.
    fn flow_node(&mut self, event: B, stack: &mut Vec<Frame>) -> Result<bool, Error> {
        match event.borrow() {
            Event::Alias { anchor, .. } => {
                self.alias(anchor)?;
                Ok(true)
            }
            Event::Scalar {
                value,
                style,
                tag,
                anchor,
                ..
            } => {
                let mut text = self.properties(anchor.as_ref(), tag.as_ref())?;
                if !text.is_empty() {
                    // `[!t ]`: libyaml needs a space between properties and
                    // a flow indicator.
                    text.push(' ');
                }
                if !(*style == ScalarStyle::Plain && value.is_empty()) {
                    scalar_inline(value, *style, true, tag.as_ref(), &mut text);
                }
                self.push(&text)?;
                Ok(false)
            }
            Event::SequenceStart { tag, anchor, .. } => {
                self.flow_collection(true, tag.as_ref(), anchor.as_ref(), stack)?;
                Ok(false)
            }
            Event::MappingStart { tag, anchor, .. } => {
                self.flow_collection(false, tag.as_ref(), anchor.as_ref(), stack)?;
                Ok(false)
            }
            _ => Err(emitter_error("unexpected event in node position")),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::Event;

    #[test]
    fn event_emission_preserves_directives() {
        let input = "%YAML 1.2\n---\na: [1, 2]\n";
        let events: Vec<Event<'_>> = crate::parse_events(input)
            .expect("events")
            .collect::<Result<Vec<_>, _>>()
            .expect("items");
        let text = crate::to_string_events(&events).expect("emit");
        assert!(text.contains("%YAML 1.2"));
        assert!(text.contains("---"));
        let value = crate::parse_value(&text).expect("reparse");
        let original = crate::parse_value(input).expect("original");
        assert_eq!(value, original);
    }

    #[test]
    fn event_emission_preserves_simple_folded_style() {
        let input = "folded: >\n  a b\n";
        let events: Vec<Event<'_>> = crate::parse_events(input)
            .expect("events")
            .collect::<Result<Vec<_>, _>>()
            .expect("items");
        let text = crate::to_string_events(&events).expect("emit");
        assert!(text.contains("folded: >"), "{text:?}");
        let value = crate::parse_value(&text).expect("reparse");
        let original = crate::parse_value(input).expect("original");
        assert_eq!(value, original);
    }

    #[test]
    fn event_emission_preserves_block_scalar_values() {
        let input = "literal: |\n  line1\n  line2\nfolded: >\n  a b\n";
        let events: Vec<Event<'_>> = crate::parse_events(input)
            .expect("events")
            .collect::<Result<Vec<_>, _>>()
            .expect("items");
        let text = crate::to_string_events(&events).expect("emit");
        assert!(text.contains("literal: |"));
        let value = crate::parse_value(&text).expect("reparse");
        let original = crate::parse_value(input).expect("original");
        assert_eq!(value, original);
    }

    #[test]
    fn event_emission_preserves_tags_anchors_aliases_and_scalar_styles() {
        let input = "base: &b {x: \"quoted\"}\ncopy: *b\ntagged: !Custom value\n";
        let events: Vec<Event<'_>> = crate::parse_events(input)
            .expect("events")
            .collect::<Result<Vec<_>, _>>()
            .expect("items");
        let text = crate::to_string_events(&events).expect("emit");
        assert!(text.contains("base:"));
        assert!(text.contains("&b"));
        assert!(text.contains("*b"));
        assert!(text.contains("!Custom"));
        let value = crate::parse_value(&text).expect("reparse");
        let original = crate::parse_value(input).expect("original");
        assert_eq!(value, original);
    }
}

#[cfg(test)]
mod writer_tests {
    use crate::Event;

    #[test]
    fn event_writer_round_trips() {
        let events: Vec<Event<'_>> = crate::parse_events("a: [1, 2]\n")
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let mut output = Vec::new();
        crate::to_writer_events(&mut output, &events).expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert_eq!(crate::parse_value(text).unwrap()["a"][1].as_u64(), Some(2));
    }
}

#[cfg(test)]
mod stream_writer_tests {
    #[test]
    fn lazy_event_writer_enforces_cumulative_output_limit() {
        let limits = crate::Limits {
            max_documents: 2,
            max_output_bytes: 10,
            ..crate::Limits::default()
        };
        let stream =
            crate::parse_events_with_limits("---\na: 1\n---\nb: 2\n", &limits).expect("stream");
        let mut output = Vec::new();
        let error = crate::to_writer_event_stream_with_limits(&mut output, stream, &limits)
            .expect_err("cumulative limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
    }

    #[test]
    fn lazy_event_writer_emits_multiple_documents() {
        let limits = crate::Limits {
            max_documents: 2,
            ..crate::Limits::default()
        };
        let stream =
            crate::parse_events_with_limits("---\na: 1\n---\nb: 2\n", &limits).expect("stream");
        let mut output = Vec::new();
        crate::to_writer_event_stream_with_limits(&mut output, stream, &limits).expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert!(text.contains("a: 1"), "{text:?}");
        let documents = crate::parse_documents_with_limits(text, &limits).expect("reparse");
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[1]["b"].as_u64(), Some(2));
    }
}

#[cfg(test)]
mod stream_block_scalar_tests {
    #[test]
    fn per_event_writer_preserves_folded_blank_lines() {
        let input = "folded: >\n  a\n\n  b\n";
        let limits = crate::Limits::default();
        let stream = crate::parse_events_with_limits(input, &limits).expect("stream");
        let mut output = Vec::new();
        crate::to_writer_event_stream(&mut output, stream).expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert!(text.contains("folded: >"), "{text:?}");
        assert_eq!(
            crate::parse_value(text).expect("reparse"),
            crate::parse_value(input).expect("original")
        );
    }

    #[test]
    fn per_event_writer_emits_block_scalars() {
        let limits = crate::Limits::default();
        let stream = crate::parse_events_with_limits(
            "literal: |\n  one\n  two\nfolded: >\n  a b\n",
            &limits,
        )
        .expect("stream");
        let mut output = Vec::new();
        crate::to_writer_event_stream(&mut output, stream).expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert!(text.contains("literal: |"), "{text:?}");
        assert!(text.contains("folded: >"), "{text:?}");
        let decoded = crate::parse_value(text).expect("reparse");
        let original =
            crate::parse_value("literal: |\n  one\n  two\nfolded: >\n  a b\n").expect("original");
        assert_eq!(decoded, original);
    }
}
