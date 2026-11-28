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
//!
//! The writer refuses three things that no reader of this crate would load. An alias needs an
//! anchor of its name earlier in its document. A `%YAML` or `%TAG` directive has to be one
//! that the scanner reads, in its default mode or in the one that reads as libyaml does, and
//! neither stands twice in a document. What the events mean is not checked.
//! The names of a document's anchors are held while it is written, at most `max_anchors` of
//! them.

use std::borrow::Borrow;
use std::collections::HashSet;
use std::io::Write;
use std::marker::PhantomData;

use crate::EventStream;
use crate::emitter::{
    MAX_IMPLICIT_KEY_CHARS, Output, RedefinedHandles, TagDirectives, block_scalar_ok, can_fold,
    double_quoted_char_len_capped, is_printable, looks_like_leading_zero_numeral,
    needs_explicit_indent, plain_text_ok, single_quoted_char_len_capped, single_quoted_ok,
    validate_options, write_double_quoted, write_folded_body, write_single_quoted,
};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::event::{Anchor, CollectionStyle, Event, ScalarStyle};
use crate::limits::Limits;
use crate::mapping::GlobalHasher;
use crate::options::EmitOptions;
use crate::scanner::Scanner;
use crate::tag::Tag;

/// Indentation step of the event writer.
const STEP: usize = 2;

/// Emit a collected event stream as YAML text.
///
/// Three things that no reader of this crate loads are refused: an alias with no node of its
/// anchor earlier in its document, a `%YAML` or `%TAG` directive that the scanner reads in
/// neither of its modes, and a second `%YAML`, or a second `%TAG` for one handle, in a
/// document. An event reader reports an alias as it is written, so events that were read from
/// a document with a dangling alias are refused here. Events are a document's syntax, and
/// what they mean is not checked: an alias inside the node it names, a tag that doesn't suit
/// its node, and a key that a mapping has twice are written as they are given.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_events(events: &[Event<'_>]) -> Result<String, Error> {
    to_string_events_with(events, &EmitOptions::new())
}

/// Emit a collected event stream as YAML text with explicit emitter options
/// and limits.
///
/// Applies `options.limits` (`max_output_bytes`, `max_depth`, and
/// `max_anchors` for the anchors of one document) and validates
/// `options.indent`, while preserving each event's own layout, scalar styles,
/// and explicit document markers.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_events_with(events: &[Event<'_>], options: &EmitOptions) -> Result<String, Error> {
    options.limits.validate()?;
    validate_options(&options.emitter_options())?;
    emit_events_to_string(events, &options.limits)
}

fn emit_events_to_string(events: &[Event<'_>], limits: &Limits) -> Result<String, Error> {
    let mut output = Vec::new();
    write_events(
        Output::direct(&mut output, limits),
        events.iter().map(Ok::<_, Error>),
        limits,
    )?;
    String::from_utf8(output).map_err(|error| {
        Error::coded(
            ErrorKind::Emitter,
            ErrorCode::InvalidEmitEvent,
            "emitter produced invalid UTF-8",
        )
        .with_source(error)
    })
}

/// Write a collected event stream to a writer.
///
/// The writer is handed what is written in pieces, so when an error is returned it may hold
/// the documents before the one that was refused, and the start of that one.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_events<W: Write>(writer: &mut W, events: &[Event<'_>]) -> Result<(), Error> {
    to_writer_events_with(writer, events, &EmitOptions::new())
}

/// Write a collected event stream to a writer with explicit emitter options
/// and limits.
///
/// Applies `options.limits` (`max_output_bytes` for all that is written,
/// `max_depth`, and `max_anchors` for the anchors of one document) and
/// validates `options.indent`, while preserving each event's own layout,
/// scalar styles, and explicit document markers.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_events_with<W: Write>(
    writer: &mut W,
    events: &[Event<'_>],
    options: &EmitOptions,
) -> Result<(), Error> {
    options.limits.validate()?;
    validate_options(&options.emitter_options())?;
    write_events(
        Output::new(writer, &options.limits),
        events.iter().map(Ok::<_, Error>),
        &options.limits,
    )
}

/// Write a lazy event stream as it is parsed.
///
/// The output is the same as [`to_string_events`] would produce for the
/// collected events. It holds one document's text at a time, since the
/// document's `%TAG` directives come first, and otherwise needs memory for the
/// nesting depth and for the names of that document's anchors. The writer is handed
/// what is written in pieces, so when an error is returned it may hold the documents
/// before the one that was refused.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_event_stream<W: Write>(
    writer: &mut W,
    stream: EventStream<'_>,
) -> Result<(), Error> {
    to_writer_event_stream_with(writer, stream, &EmitOptions::new())
}

/// Write a lazy event stream with explicit emitter options and limits.
///
/// Applies `options.limits` (`max_output_bytes`, `max_depth`, and
/// `max_anchors`) and validates `options.indent`, while preserving each
/// event's own layout, scalar styles, and explicit document markers.
///
/// # Errors
///
/// Returns an error when the events cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_event_stream_with<W: Write>(
    writer: &mut W,
    stream: EventStream<'_>,
    options: &EmitOptions,
) -> Result<(), Error> {
    options.limits.validate()?;
    validate_options(&options.emitter_options())?;
    write_events(
        Output::new(writer, &options.limits),
        stream,
        &options.limits,
    )
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
        anchors: HashSet::with_hasher(GlobalHasher),
        handles_seen: HashSet::with_hasher(GlobalHasher),
        version_seen: false,
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

/// Stream a scalar on one line directly to `output`: plain or single-quoted when the text allows
/// that style in this context, double-quoted otherwise.
fn write_scalar_inline<W: Write>(
    value: &str,
    style: ScalarStyle,
    flow: bool,
    tag: Option<&Tag>,
    output: &mut Output<'_, W>,
) -> Result<(), Error> {
    let plain =
        plain_text_ok(value, flow) && (tag.is_some() || !looks_like_leading_zero_numeral(value));
    match style {
        ScalarStyle::Plain if plain => output.push_str(value),
        ScalarStyle::SingleQuoted if single_quoted_ok(value) => write_single_quoted(
            value,
            output,
            #[cfg(test)]
            None,
        ),
        _ => write_double_quoted(
            value,
            output,
            #[cfg(test)]
            None,
        ),
    }
}

/// Character count of `write_scalar_inline`, capped at `cap`, without allocating.
fn scalar_inline_char_len_capped(
    value: &str,
    style: ScalarStyle,
    flow: bool,
    tag: Option<&Tag>,
    cap: usize,
) -> usize {
    let plain =
        plain_text_ok(value, flow) && (tag.is_some() || !looks_like_leading_zero_numeral(value));
    match style {
        ScalarStyle::Plain if plain => value.chars().take(cap).count(),
        ScalarStyle::SingleQuoted if single_quoted_ok(value) => {
            single_quoted_char_len_capped(value, cap)
        }
        _ => double_quoted_char_len_capped(value, cap),
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
    /// The anchors that the document being written has defined so far. An alias refers to one
    /// of them, and to none of another document.
    anchors: HashSet<String, GlobalHasher>,
    /// Every handle that the next document's `%TAG` directives have declared, `!` and `!!`
    /// included: a document gives a handle one prefix.
    handles_seen: HashSet<String, GlobalHasher>,
    /// Whether the next document has its `%YAML` directive already.
    version_seen: bool,
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
                    // Reject directives that already exceed `max_output_bytes` before scanning
                    // characters or allocating a validation line.
                    let directive_bytes = usize::from(open_ended)
                        .saturating_mul(4)
                        .saturating_add(1)
                        .saturating_add(name.len())
                        .saturating_add(1)
                        .saturating_add(value.len())
                        .saturating_add(1);
                    self.output.check_additional(directive_bytes)?;

                    // The parser reads a name up to a space or tab, and neither may hold a line
                    // break. A byte order mark is rejected in directive names and parameters,
                    // while a trailing comment in compatibility mode may hold one, matching the
                    // scanner. NEL, LS, and PS are content; a directive has no escapes, so they
                    // are written as they are.
                    let directive_char = |ch: char| is_printable(ch) && !matches!(ch, '\n' | '\r');
                    let comment_start = value
                        .match_indices('#')
                        .find(|&(at, _)| at == 0 || value[..at].ends_with([' ', '\t']))
                        .map_or(value.len(), |(at, _)| at);
                    if name.is_empty()
                        || !name
                            .chars()
                            .all(|ch| directive_char(ch) && !matches!(ch, ' ' | '\t' | '\u{feff}'))
                        || !value.chars().all(directive_char)
                        || value[..comment_start].contains('\u{feff}')
                    {
                        return Err(emitter_error(format!(
                            "cannot write the directive %{name} {value}"
                        )));
                    }
                    self.check_directive(name, value)?;
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
                    // The directives before this point were this document's, and an anchor is
                    // one document's too.
                    self.handles_seen.clear();
                    self.version_seen = false;
                    self.anchors.clear();
                    let held = self.output.hold();
                    // An empty root needs `---`, or the document would vanish.
                    let marker = *explicit || documents > 0 || directives || empty_root;
                    if marker {
                        self.push("---\n")?;
                    }
                    let root = self.node_event()?;
                    self.node(root, Pos::Root)?;
                    if !self.directives.is_empty() {
                        let header_len = usize::from(open_ended)
                            .saturating_mul(4)
                            .saturating_add(self.directives.lines_byte_len())
                            .saturating_add(usize::from(!marker).saturating_mul(4));
                        self.output.check_additional(header_len)?;
                        let mut header = String::with_capacity(header_len);
                        // A directive can't follow a document that `...` didn't end. The
                        // document's own directives wrote one already, and cleared `open_ended`.
                        if open_ended {
                            header.push_str("...\n");
                        }
                        self.directives.push_lines_into(&mut header);
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

    /// Refuse a `%YAML` or `%TAG` directive that the scanner would refuse in both of its modes
    /// when it reads the output back, or that the document already has. The scanner reads the
    /// one line, so what a directive may say is its rule alone; what a document may hold once
    /// is the parser's rule, and is counted here. The compatibility mode takes a little more
    /// than the default one, such as a `%TAG` prefix that starts with a flow indicator, and
    /// events read in that mode are written back. A directive of another name is the
    /// application's, and its characters were checked already.
    fn check_directive(&mut self, name: &str, value: &str) -> Result<(), Error> {
        if name != "YAML" && name != "TAG" {
            return Ok(());
        }
        let refused = |reason: &str| {
            emitter_error(format!(
                "cannot write the directive %{name} {value}: {reason}"
            ))
        };
        let line = format!("%{name} {value}\n");
        // The stream's start, and then the directive or the reason it is none. Only the
        // line's syntax is asked about, bounded by the caller's output limit rather than
        // reader-side limits.
        let limits = Limits::default()
            .with_max_input_bytes(self.limits.max_output_bytes)
            .with_max_tag_expansion_bytes(self.limits.max_output_bytes);
        let scan = |compat: bool| -> Result<(), Box<Error>> {
            let mut scanner = Scanner::with_compat(&line, &limits, compat)?;
            scanner.next_token(&limits)?;
            scanner.next_token(&limits)?;
            Ok(())
        };
        if scan(false).is_err()
            && let Err(error) = scan(true)
        {
            // The reason is the compatibility mode's: the one that stands in both.
            return Err(refused(error.message()));
        }
        if name == "YAML" && std::mem::replace(&mut self.version_seen, true) {
            return Err(refused("a document has one %YAML directive"));
        }
        if name == "TAG"
            && let Some(handle) = value.split([' ', '\t']).find(|part| !part.is_empty())
            && !self.handles_seen.insert(handle.to_owned())
        {
            return Err(refused("a document gives a handle one prefix"));
        }
        Ok(())
    }

    /// Validate an optional anchor (and record it in `self.anchors` within `max_anchors`) and an
    /// optional tag, without allocating any formatted property string.
    fn validate_properties(
        &mut self,
        anchor: Option<&Anchor<'_>>,
        tag: Option<&Tag>,
    ) -> Result<(), Error> {
        if let Some(anchor) = anchor {
            if !anchor_name_ok(anchor.as_str()) {
                return Err(emitter_error(format!(
                    "cannot write the anchor name {:?}",
                    anchor.as_str()
                )));
            }
            // An anchor may be defined again, and an alias then refers to the later one. Its
            // name is new to the document once.
            if !self.anchors.contains(anchor.as_str()) {
                self.limits.check_anchors(self.anchors.len() + 1)?;
                self.output
                    .check_additional(1usize.saturating_add(anchor.as_str().len()))?;
                self.anchors.insert(anchor.as_str().to_owned());
            }
        }
        if let Some(tag) = tag {
            // Validate tag form and ensure any required `%TAG` directive fits within output limits.
            self.directives
                .token_char_len_capped(tag, self.handles, 1, self.output)?;
        }
        Ok(())
    }

    /// Validate optional properties and compute their character length capped at `cap`, without
    /// allocating a formatted property string.
    fn properties_char_len_capped(
        &mut self,
        anchor: Option<&Anchor<'_>>,
        tag: Option<&Tag>,
        cap: usize,
    ) -> Result<usize, Error> {
        let mut total = 0usize;
        if let Some(anchor) = anchor {
            if !anchor_name_ok(anchor.as_str()) {
                return Err(emitter_error(format!(
                    "cannot write the anchor name {:?}",
                    anchor.as_str()
                )));
            }
            if !self.anchors.contains(anchor.as_str()) {
                self.limits.check_anchors(self.anchors.len() + 1)?;
                self.output
                    .check_additional(1usize.saturating_add(anchor.as_str().len()))?;
                self.anchors.insert(anchor.as_str().to_owned());
            }
            total = 1usize.saturating_add(anchor.as_str().chars().take(cap).count());
        }
        if let Some(tag) = tag {
            let sep = usize::from(total > 0);
            let rem = cap.saturating_sub(total.saturating_add(sep));
            let tag_len = self.directives.token_char_len_capped(
                tag,
                self.handles,
                rem.max(1),
                self.output,
            )?;
            total = total.saturating_add(sep).saturating_add(tag_len);
        }
        Ok(total.min(cap))
    }

    /// Stream `&anchor !tag` directly to `self.output` after validating them, returning whether
    /// any property was written.
    fn write_properties(
        &mut self,
        anchor: Option<&Anchor<'_>>,
        tag: Option<&Tag>,
    ) -> Result<bool, Error> {
        let mut wrote = false;
        if let Some(anchor) = anchor {
            self.output.push_str("&")?;
            self.output.push_str(anchor.as_str())?;
            wrote = true;
        }
        if let Some(tag) = tag {
            if wrote {
                self.output.push_str(" ")?;
            }
            self.directives.write_tag(tag, self.handles, self.output)?;
            wrote = true;
        }
        Ok(wrote)
    }

    /// Validate an alias name and check that an anchor of that name appeared earlier in the
    /// document.
    fn validate_alias(&self, anchor: &Anchor<'_>) -> Result<(), Error> {
        if !anchor_name_ok(anchor.as_str()) {
            return Err(emitter_error(format!(
                "cannot write the alias name {:?}",
                anchor.as_str()
            )));
        }
        if !self.anchors.contains(anchor.as_str()) {
            return Err(emitter_error(format!(
                "cannot write the alias {:?}: no node before it in its document has that anchor",
                anchor.as_str()
            )));
        }
        Ok(())
    }

    fn alias(&mut self, anchor: &Anchor<'_>) -> Result<(), Error> {
        self.validate_alias(anchor)?;
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
        self.validate_properties(anchor, tag)?;
        let has_props = anchor.is_some() || tag.is_some();
        if matches!(style, ScalarStyle::Literal | ScalarStyle::Folded) && block_scalar_ok(value) {
            self.separate(pos)?;
            if has_props {
                self.write_properties(anchor, tag)?;
                self.push(" ")?;
            }
            return self.block_scalar(value, style, pos.content_indent(), matches!(pos, Pos::Root));
        }
        if style == ScalarStyle::Plain && value.is_empty() {
            if !has_props {
                // The root of a document that is only `---`.
                return if matches!(pos, Pos::Root) {
                    Ok(())
                } else {
                    self.push("\n")
                };
            }
            self.separate(pos)?;
            self.write_properties(anchor, tag)?;
            return self.push("\n");
        }
        self.separate(pos)?;
        if has_props {
            self.write_properties(anchor, tag)?;
            self.push(" ")?;
        }
        write_scalar_inline(value, style, false, tag, self.output)?;
        self.push("\n")
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the indentation indicator is a single digit in 1..=9"
    )]
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
            header.push(char::from_digit(indicator as u32, 10).unwrap_or('0'));
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
        if folded {
            self.output.check_additional(body.len())?;
            write_folded_body(
                body,
                content_indent,
                self.output,
                #[cfg(test)]
                None,
            )?;
        } else {
            for line in body.split('\n') {
                if !line.is_empty() {
                    self.output.spaces(content_indent)?;
                    self.push(line)?;
                }
                self.push("\n")?;
            }
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
        self.validate_properties(anchor, tag)?;
        let has_props = anchor.is_some() || tag.is_some();
        let (indent, inline) = match pos {
            Pos::Root => {
                if has_props {
                    self.write_properties(anchor, tag)?;
                    self.push("\n")?;
                }
                (0, false)
            }
            Pos::Entry(indent) if !has_props => {
                self.push(" ")?;
                (indent + STEP, true)
            }
            Pos::Entry(indent) => {
                self.push(" ")?;
                self.write_properties(anchor, tag)?;
                self.push("\n")?;
                (indent + STEP, false)
            }
            Pos::Value(indent) => {
                if has_props {
                    self.push(" ")?;
                    self.write_properties(anchor, tag)?;
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
                if self.try_write_implicit_key(key.borrow())? {
                    self.push(":")?;
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
                stack.push(Frame::FlowSequence { first: false });
                if is_empty_plain(event.borrow()) {
                    // An untagged, unanchored empty plain scalar (such as after dropping a
                    // mismatched core collection tag like `[!!map ]`) has no empty form inside a
                    // flow sequence; `~` keeps the plain-scalar event and its null value.
                    return self.push("~");
                }
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

    /// Stream a block-mapping key in implicit form directly to `self.output` if it fits within
    /// [`MAX_IMPLICIT_KEY_CHARS`], returning `false` when the key needs `? `: collections, empty
    /// keys, block scalars, and keys longer than [`MAX_IMPLICIT_KEY_CHARS`].
    fn try_write_implicit_key(&mut self, key: &Event<'_>) -> Result<bool, Error> {
        const CAP: usize = MAX_IMPLICIT_KEY_CHARS + 1;
        match key {
            Event::Alias { anchor, .. } => {
                // `:` can be part of an alias name, so it needs a space.
                self.validate_alias(anchor)?;
                let width = 1usize
                    .saturating_add(anchor.as_str().chars().take(CAP).count())
                    .saturating_add(1);
                if width > MAX_IMPLICIT_KEY_CHARS {
                    return Ok(false);
                }
                self.push("*")?;
                self.push(anchor.as_str())?;
                self.push(" ")?;
                Ok(true)
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
                    return Ok(false);
                }
                let props_len =
                    self.properties_char_len_capped(anchor.as_ref(), tag.as_ref(), CAP)?;
                let sep = usize::from(props_len > 0);
                let rem = CAP.saturating_sub(props_len.saturating_add(sep));
                let scalar_len = if rem == 0 {
                    0
                } else {
                    scalar_inline_char_len_capped(value, *style, false, tag.as_ref(), rem)
                };
                if props_len.saturating_add(sep).saturating_add(scalar_len) > MAX_IMPLICIT_KEY_CHARS
                {
                    return Ok(false);
                }
                if self.write_properties(anchor.as_ref(), tag.as_ref())? {
                    self.push(" ")?;
                }
                write_scalar_inline(value, *style, false, tag.as_ref(), self.output)?;
                Ok(true)
            }
            _ => Ok(false),
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
        self.validate_properties(anchor, tag)?;
        if self.write_properties(anchor, tag)? {
            self.push(" ")?;
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
        const CAP: usize = MAX_IMPLICIT_KEY_CHARS + 1;
        Ok(match key {
            Event::SequenceStart { .. } => !matches!(self.peek()?, Some(Event::SequenceEnd { .. })),
            Event::MappingStart { .. } => !matches!(self.peek()?, Some(Event::MappingEnd { .. })),
            Event::Alias { anchor, .. } => {
                anchor.as_str().chars().take(CAP).count().saturating_add(2) > MAX_IMPLICIT_KEY_CHARS
            }
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
                    let props_len =
                        self.properties_char_len_capped(anchor.as_ref(), tag.as_ref(), CAP)?;
                    let rem = CAP.saturating_sub(props_len);
                    let scalar_len = if rem == 0 {
                        0
                    } else {
                        scalar_inline_char_len_capped(value, *style, true, tag.as_ref(), rem)
                    };
                    props_len.saturating_add(scalar_len) > MAX_IMPLICIT_KEY_CHARS
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
                self.validate_properties(anchor.as_ref(), tag.as_ref())?;
                if self.write_properties(anchor.as_ref(), tag.as_ref())? {
                    // `[!t ]`: libyaml needs a space between properties and
                    // a flow indicator.
                    self.push(" ")?;
                }
                if !(*style == ScalarStyle::Plain && value.is_empty()) {
                    write_scalar_inline(value, *style, true, tag.as_ref(), self.output)?;
                }
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
        let limits = crate::Limits::new()
            .with_max_documents(2)
            .with_max_output_bytes(10);
        let parse_options = crate::ParseOptions::new().with_limits(limits.clone());
        let emit_options = crate::EmitOptions::new().with_limits(limits);
        let stream =
            crate::parse_events_with("---\na: 1\n---\nb: 2\n", &parse_options).expect("stream");
        let mut output = Vec::new();
        let error = crate::to_writer_event_stream_with(&mut output, stream, &emit_options)
            .expect_err("cumulative limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
    }

    #[test]
    fn lazy_event_writer_emits_multiple_documents() {
        let limits = crate::Limits::new().with_max_documents(2);
        let parse_options = crate::ParseOptions::new().with_limits(limits.clone());
        let emit_options = crate::EmitOptions::new().with_limits(limits);
        let stream =
            crate::parse_events_with("---\na: 1\n---\nb: 2\n", &parse_options).expect("stream");
        let mut output = Vec::new();
        crate::to_writer_event_stream_with(&mut output, stream, &emit_options).expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert!(text.contains("a: 1"), "{text:?}");
        let documents = crate::parse_documents_with(text, &parse_options).expect("reparse");
        assert_eq!(documents.len(), 2);
        assert_eq!(documents[1]["b"].as_u64(), Some(2));
    }
}

#[cfg(test)]
mod stream_block_scalar_tests {
    #[test]
    fn per_event_writer_preserves_folded_blank_lines() {
        let input = "folded: >\n  a\n\n  b\n";
        let stream = crate::parse_events_with(input, &crate::ParseOptions::new()).expect("stream");
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
        let stream = crate::parse_events_with(
            "literal: |\n  one\n  two\nfolded: >\n  a b\n",
            &crate::ParseOptions::new(),
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

    fn wrap_test_doc(
        directives: Vec<crate::Event<'static>>,
        root: Vec<crate::Event<'static>>,
    ) -> Vec<crate::Event<'static>> {
        let mut events = vec![crate::Event::StreamStart {
            span: crate::Span::default(),
        }];
        events.extend(directives);
        events.push(crate::Event::DocumentStart {
            explicit: false,
            span: crate::Span::default(),
        });
        events.extend(root);
        events.push(crate::Event::DocumentEnd {
            explicit: false,
            span: crate::Span::default(),
        });
        events.push(crate::Event::StreamEnd {
            span: crate::Span::default(),
        });
        events
    }

    fn test_scalar(
        value: std::borrow::Cow<'static, str>,
        style: crate::ScalarStyle,
        tag: Option<crate::Tag>,
        anchor: Option<crate::Anchor<'static>>,
    ) -> crate::Event<'static> {
        crate::Event::Scalar {
            value,
            raw: std::borrow::Cow::Borrowed(""),
            style,
            tag,
            anchor,
            span: crate::Span::default(),
        }
    }

    fn test_kv_map(
        style: crate::CollectionStyle,
        key: crate::Event<'static>,
    ) -> Vec<crate::Event<'static>> {
        vec![
            crate::Event::MappingStart {
                tag: None,
                anchor: None,
                style,
                span: crate::Span::default(),
            },
            key,
            test_scalar(
                std::borrow::Cow::Borrowed("1"),
                crate::ScalarStyle::Plain,
                None,
                None,
            ),
            crate::Event::MappingEnd {
                span: crate::Span::default(),
            },
        ]
    }

    fn oversized_event_limit_cases() -> Vec<(&'static str, Vec<crate::Event<'static>>)> {
        use std::borrow::Cow;

        use crate::{Anchor, CollectionStyle, Event, ScalarStyle, Span, Tag};

        vec![
            (
                "double_quoted",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Owned("a\"b\\c\n".repeat(10_000)),
                        ScalarStyle::DoubleQuoted,
                        None,
                        None,
                    )],
                ),
            ),
            (
                "single_quoted",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Owned("it's ".repeat(10_000)),
                        ScalarStyle::SingleQuoted,
                        None,
                        None,
                    )],
                ),
            ),
            (
                "folded_block",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Owned("line\n".repeat(10_000)),
                        ScalarStyle::Folded,
                        None,
                        None,
                    )],
                ),
            ),
            (
                "block_long_key",
                wrap_test_doc(
                    vec![],
                    test_kv_map(
                        CollectionStyle::Block,
                        test_scalar(
                            Cow::Owned("k\"\\".repeat(10_000)),
                            ScalarStyle::DoubleQuoted,
                            None,
                            None,
                        ),
                    ),
                ),
            ),
            (
                "flow_long_key",
                wrap_test_doc(
                    vec![],
                    test_kv_map(
                        CollectionStyle::Flow,
                        test_scalar(
                            Cow::Owned("k\"\\".repeat(10_000)),
                            ScalarStyle::DoubleQuoted,
                            None,
                            None,
                        ),
                    ),
                ),
            ),
            (
                "huge_directive_tag",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Borrowed("v"),
                        ScalarStyle::Plain,
                        Some(Tag::new(format!("custom {}:", "p".repeat(10_000)), "s")),
                        None,
                    )],
                ),
            ),
            (
                "huge_local_tag",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Borrowed("v"),
                        ScalarStyle::Plain,
                        Some(Tag::local("x".repeat(10_000))),
                        None,
                    )],
                ),
            ),
            (
                "huge_directive",
                wrap_test_doc(
                    vec![Event::Directive {
                        name: Cow::Borrowed("TAG"),
                        value: Cow::Owned(format!(
                            "!e! tag:example.com,2026:{}:",
                            "a".repeat(10_000)
                        )),
                        span: Span::default(),
                    }],
                    vec![test_scalar(
                        Cow::Borrowed("v"),
                        ScalarStyle::Plain,
                        None,
                        None,
                    )],
                ),
            ),
            (
                "huge_anchor",
                wrap_test_doc(
                    vec![],
                    vec![test_scalar(
                        Cow::Borrowed("v"),
                        ScalarStyle::Plain,
                        None,
                        Some(Anchor::owned("a".repeat(10_000))),
                    )],
                ),
            ),
            (
                "huge_anchor_key",
                wrap_test_doc(
                    vec![],
                    test_kv_map(
                        CollectionStyle::Block,
                        test_scalar(
                            Cow::Borrowed("k"),
                            ScalarStyle::Plain,
                            None,
                            Some(Anchor::owned("a".repeat(10_000))),
                        ),
                    ),
                ),
            ),
        ]
    }

    #[test]
    fn event_writer_enforces_output_limit_incrementally_on_scalars_keys_tags_and_directives() {
        use std::borrow::Cow;

        use crate::{EmitOptions, ErrorKind, Event, LimitKind, Limits, ScalarStyle, Span};

        let tight = EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(64));
        let cases = oversized_event_limit_cases();

        for (label, events) in &cases {
            let err = crate::to_string_events_with(events, &tight)
                .expect_err(&format!("{label}: to_string_events_with"));
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err}");
            assert_eq!(
                err.limit().map(crate::LimitExceeded::kind),
                Some(LimitKind::OutputBytes),
                "{label}: {err}"
            );
            let mut buf = Vec::new();
            let err = crate::to_writer_events_with(&mut buf, events, &tight)
                .expect_err(&format!("{label}: to_writer_events_with"));
            assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err}");
            assert_eq!(
                err.limit().map(crate::LimitExceeded::kind),
                Some(LimitKind::OutputBytes),
                "{label}: {err}"
            );
        }

        // 2. Folded scalar where `body.len() <= max_output_bytes` but folded expansion exceeds it.
        let folded_events = wrap_test_doc(
            vec![],
            vec![test_scalar(
                Cow::Owned("a\n".repeat(40)),
                ScalarStyle::Folded,
                None,
                None,
            )],
        );
        let mid_limit =
            EmitOptions::new().with_limits(Limits::default().with_max_output_bytes(100));
        let err = crate::to_string_events_with(&folded_events, &mid_limit)
            .expect_err("folded expansion limit");
        assert_eq!(
            err.limit().map(crate::LimitExceeded::kind),
            Some(LimitKind::OutputBytes)
        );

        // 3. Directive validation respects `max_output_bytes` boundary exactly while ignoring
        //    reader-side `max_input_bytes`.
        let dir_events = wrap_test_doc(
            vec![Event::Directive {
                name: Cow::Borrowed("TAG"),
                value: Cow::Borrowed("!e! tag:example.com,2026:"),
                span: Span::default(),
            }],
            vec![test_scalar(
                Cow::Borrowed("a"),
                ScalarStyle::Plain,
                None,
                None,
            )],
        );
        let full = crate::to_string_events(&dir_events).expect("emit directive doc");
        let exact = EmitOptions::new().with_limits(
            Limits::default()
                .with_max_input_bytes(4)
                .with_max_output_bytes(full.len()),
        );
        assert_eq!(
            crate::to_string_events_with(&dir_events, &exact).expect("exact output limit"),
            full
        );
        let one_short = EmitOptions::new().with_limits(
            Limits::default()
                .with_max_input_bytes(4)
                .with_max_output_bytes(full.len() - 1),
        );
        let err =
            crate::to_string_events_with(&dir_events, &one_short).expect_err("one byte short");
        assert_eq!(
            err.limit().map(crate::LimitExceeded::kind),
            Some(LimitKind::OutputBytes)
        );
    }
}
