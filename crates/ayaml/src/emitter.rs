//! YAML text emission from the generic value model.
//!
//! The value emitter writes deterministic block output by default, with
//! options for flow layout, document markers, a preferred scalar style, and
//! indentation, and with per-path style overrides. A string is written plain
//! only when the plain text reads back as the same string; otherwise it is
//! quoted, or written as a block scalar when it spans lines. Output is
//! buffered and charged against [`Limits`]. The scalar and tag helpers here
//! are shared with the event emitter.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write;

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;
use crate::tag::{is_uri_byte, verbatim_reads_back};
use crate::value::Value;

/// Emitter presentation options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmitterOptions {
    /// Emit the complete document in flow style.
    pub flow: bool,
    /// Emit an explicit `---` before the document.
    pub explicit_start: bool,
    /// Emit an explicit `...` after the document.
    pub explicit_end: bool,
    /// Preferred scalar style for strings. Plain falls back to quoting when
    /// the text is syntactically ambiguous; block styles fall back to quoted
    /// output in this value-level emitter.
    pub scalar_style: Option<crate::event::ScalarStyle>,
    /// Number of spaces per nested block level, from 2 to 9. Other values
    /// are an emitter error.
    pub indent: usize,
    /// Indent block sequences beneath a mapping key. The default is `false`
    /// (indentless sequences).
    pub indent_sequences: bool,
}

impl EmitterOptions {
    /// Use flow style for the complete document.
    #[must_use]
    pub const fn with_flow(mut self, value: bool) -> Self {
        self.flow = value;
        self
    }

    /// Emit an explicit `---` before the document.
    #[must_use]
    pub const fn with_explicit_start(mut self, value: bool) -> Self {
        self.explicit_start = value;
        self
    }

    /// Emit an explicit `...` after the document.
    #[must_use]
    pub const fn with_explicit_end(mut self, value: bool) -> Self {
        self.explicit_end = value;
        self
    }

    /// Set the preferred scalar style for strings.
    #[must_use]
    pub const fn with_scalar_style(mut self, value: Option<crate::event::ScalarStyle>) -> Self {
        self.scalar_style = value;
        self
    }

    /// Set the number of spaces per nested block level.
    #[must_use]
    pub const fn with_indent(mut self, value: usize) -> Self {
        self.indent = value;
        self
    }

    /// Indent block sequences beneath a mapping key.
    #[must_use]
    pub const fn with_indent_sequences(mut self, value: bool) -> Self {
        self.indent_sequences = value;
        self
    }
}

/// A path segment identifying a value in the emitted tree.
///
/// Mapping keys are matched by value; mapping keys themselves are not style
/// targets in this API. Tagged payloads share their parent's path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PathSegment {
    /// A sequence item index.
    Index(usize),
    /// The value for a mapping key.
    Key(Value),
}

impl PathSegment {
    /// Address a sequence item.
    #[must_use]
    pub const fn index(index: usize) -> Self {
        Self::Index(index)
    }

    /// Address the value of a mapping key.
    #[must_use]
    pub fn key(key: impl Into<Value>) -> Self {
        Self::Key(key.into())
    }
}

/// Explicit block-scalar chomping preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Chomping {
    /// Strip all trailing line breaks.
    Strip,
    /// Keep one trailing line break when present.
    Clip,
    /// Keep all trailing line breaks.
    Keep,
}

/// Per-node emitter presentation overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct NodeStyle {
    /// Preferred scalar style for strings at this node. Falls back to the
    /// global option and then to automatic safe quoting.
    pub scalar_style: Option<crate::event::ScalarStyle>,
    /// Preferred chomping for block scalars. Incompatible requests fall back
    /// to the chomping needed to preserve the value.
    pub chomping: Option<Chomping>,
    /// Force a collection at this path into flow or block layout where the
    /// surrounding context permits it.
    pub flow: Option<bool>,
}

impl NodeStyle {
    /// Set the preferred scalar style for this node.
    #[must_use]
    pub const fn with_scalar_style(mut self, value: Option<crate::event::ScalarStyle>) -> Self {
        self.scalar_style = value;
        self
    }

    /// Set the preferred block-scalar chomping for this node.
    #[must_use]
    pub const fn with_chomping(mut self, value: Option<Chomping>) -> Self {
        self.chomping = value;
        self
    }

    /// Force this collection into flow or block layout.
    #[must_use]
    pub const fn with_flow(mut self, value: Option<bool>) -> Self {
        self.flow = value;
        self
    }
}

/// Per-node style overrides addressed by [`PathSegment`] paths.
#[derive(Clone, Debug, Default)]
pub struct EmitterStyles {
    styles: HashMap<Vec<PathSegment>, NodeStyle>,
}

impl EmitterStyles {
    /// Create an empty style map.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set a style override for a path. The empty path targets the root node.
    pub fn set(&mut self, path: Vec<PathSegment>, style: NodeStyle) {
        self.styles.insert(path, style);
    }

    fn get(&self, path: &[PathSegment]) -> Option<NodeStyle> {
        self.styles.get(path).copied()
    }
}

impl Default for EmitterOptions {
    fn default() -> Self {
        Self {
            flow: false,
            explicit_start: false,
            explicit_end: false,
            scalar_style: None,
            indent: 2,
            indent_sequences: false,
        }
    }
}

/// Serialize a value into YAML text with default limits.
pub fn to_string<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    to_string_with_limits(value, &Limits::default())
}

/// Serialize a value into YAML text with explicit output limits.
pub fn to_string_with_limits<T: serde::Serialize + ?Sized>(
    value: &T,
    limits: &Limits,
) -> Result<String, Error> {
    let value = crate::to_value(value)?;
    emit_value_with_limits(&value, limits, EmitterOptions::default())
}

/// Serialize a value with explicit emitter options.
pub fn to_string_with_options<T: serde::Serialize + ?Sized>(
    value: &T,
    options: &EmitterOptions,
) -> Result<String, Error> {
    let value = crate::to_value(value)?;
    emit_value_with_limits(&value, &Limits::default(), *options)
}

/// Serialize an existing generic value as YAML, preserving its tags.
pub fn to_string_value(value: &Value) -> Result<String, Error> {
    to_string_value_with_limits(value, &Limits::default())
}

/// Serialize an existing generic value as YAML with explicit output limits.
pub fn to_string_value_with_limits(value: &Value, limits: &Limits) -> Result<String, Error> {
    emit_value_with_limits(value, limits, EmitterOptions::default())
}

/// Serialize an existing generic value with explicit emitter options.
pub fn to_string_value_with_options(
    value: &Value,
    options: &EmitterOptions,
) -> Result<String, Error> {
    emit_value_with_limits(value, &Limits::default(), *options)
}

/// Serialize an existing generic value with per-node style overrides.
pub fn to_string_value_with_styles(value: &Value, styles: &EmitterStyles) -> Result<String, Error> {
    emit_value_with_limits_and_styles(value, &Limits::default(), EmitterOptions::default(), styles)
}

/// Serialize an existing generic value with explicit limits and per-node styles.
pub fn to_string_value_with_limits_and_styles(
    value: &Value,
    limits: &Limits,
    styles: &EmitterStyles,
) -> Result<String, Error> {
    emit_value_with_limits_and_styles(value, limits, EmitterOptions::default(), styles)
}

/// Serialize an existing generic value with options and per-node styles.
pub fn to_string_value_with_options_and_styles(
    value: &Value,
    options: &EmitterOptions,
    styles: &EmitterStyles,
) -> Result<String, Error> {
    emit_value_with_limits_and_styles(value, &Limits::default(), *options, styles)
}

/// Serialize an existing tagged value as YAML to a writer with per-node styles.
pub fn to_writer_value_with_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to_with_styles(
        writer,
        value,
        &Limits::default(),
        EmitterOptions::default(),
        styles,
    )
}

/// Serialize an existing tagged value to a writer with options and styles.
pub fn to_writer_value_with_options_and_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    options: &EmitterOptions,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to_with_styles(writer, value, &Limits::default(), *options, styles)
}

/// Serialize an existing tagged value to a writer with limits and styles.
pub fn to_writer_value_with_limits_and_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to_with_styles(writer, value, limits, EmitterOptions::default(), styles)
}

/// Serialize a sequence of generic values as a multi-document stream.
pub fn to_string_documents(values: &[Value]) -> Result<String, Error> {
    to_string_documents_with_limits(values, &Limits::default())
}

/// Serialize a sequence of values as a stream with an explicit output limit.
pub fn to_string_documents_with_limits(values: &[Value], limits: &Limits) -> Result<String, Error> {
    let mut output = Vec::new();
    emit_documents_to(
        Output::direct(&mut output, limits),
        values,
        limits,
        EmitterOptions::default(),
        &EmitterStyles::default(),
    )?;
    String::from_utf8(output).map_err(|error| {
        Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
    })
}

fn emit_value_with_limits(
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
) -> Result<String, Error> {
    let styles = EmitterStyles::default();
    emit_value_with_limits_and_styles(value, limits, options, &styles)
}

fn emit_value_with_limits_and_styles(
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: &EmitterStyles,
) -> Result<String, Error> {
    let mut output = Vec::new();
    emit_value_with(
        Output::direct(&mut output, limits),
        value,
        limits,
        options,
        styles,
    )?;
    String::from_utf8(output).map_err(|error| {
        Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
    })
}

fn emit_value_to<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
) -> Result<(), Error> {
    let styles = EmitterStyles::default();
    emit_value_to_with_styles(writer, value, limits, options, &styles)
}

fn emit_value_to_with_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_with(Output::new(writer, limits), value, limits, options, styles)
}

fn emit_value_with<W: Write>(
    mut output: Output<'_, W>,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    limits.validate()?;
    validate_options(&options)?;
    write_document(&mut output, value, limits, options, styles, false)?;
    output.finish()
}

/// Write documents separated by `---`, charging the caller's limits for the
/// whole stream.
fn emit_documents_to<W: Write>(
    mut output: Output<'_, W>,
    values: &[Value],
    limits: &Limits,
    options: EmitterOptions,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    limits.validate()?;
    validate_options(&options)?;
    for (index, value) in values.iter().enumerate() {
        write_document(&mut output, value, limits, options, styles, index > 0)?;
    }
    output.finish()
}

/// Write one document of a stream, `later` when another came before it: the `%TAG` directives
/// that its tags need, one `---` where it needs one, and its root.
#[inline]
fn write_document<W: Write>(
    output: &mut Output<'_, W>,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: &EmitterStyles,
    later: bool,
) -> Result<(), Error> {
    let mut directives = TagDirectives::default();
    directives.scan(value, limits.max_depth)?;
    if !directives.is_empty() {
        // A directive can't follow a document that `...` didn't end.
        if later && !options.explicit_end {
            output.push_str("...\n")?;
        }
        output.push_str(&directives.lines())?;
        output.push_str("---\n")?;
    } else if later || options.explicit_start {
        output.push_str("---\n")?;
    }
    let mut emitter = ValueWriter::new(output, limits, options, styles, directives);
    emitter.emit_root(value);
    emitter.finish()
}

/// Serialize a value as YAML to a writer.
pub fn to_writer<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
) -> Result<(), Error> {
    let value = crate::to_value(value)?;
    emit_value_to(
        writer,
        &value,
        &Limits::default(),
        EmitterOptions::default(),
    )
}

/// Serialize a value as YAML to a writer with explicit emitter options.
pub fn to_writer_with_limits<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
    limits: &Limits,
) -> Result<(), Error> {
    let value = crate::to_value(value)?;
    emit_value_to(writer, &value, limits, EmitterOptions::default())
}

/// Serialize a value as YAML to a writer with explicit emitter options.
pub fn to_writer_with_options<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
    options: &EmitterOptions,
) -> Result<(), Error> {
    let value = crate::to_value(value)?;
    emit_value_to(writer, &value, &Limits::default(), *options)
}

/// Serialize an existing tagged value as YAML to a writer.
pub fn to_writer_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), Error> {
    emit_value_to(writer, value, &Limits::default(), EmitterOptions::default())
}

/// Serialize an existing tagged value to a writer with emitter options.
pub fn to_writer_value_with_limits<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
) -> Result<(), Error> {
    emit_value_to(writer, value, limits, EmitterOptions::default())
}

/// Serialize an existing value as YAML to a writer with emitter options.
pub fn to_writer_value_with_options<W: Write>(
    writer: &mut W,
    value: &Value,
    options: &EmitterOptions,
) -> Result<(), Error> {
    emit_value_to(writer, value, &Limits::default(), *options)
}

/// Write a multi-document value stream one document at a time.
pub fn to_writer_documents<W: Write>(writer: &mut W, values: &[Value]) -> Result<(), Error> {
    to_writer_documents_with_limits(writer, values, &Limits::default())
}

/// Write a multi-document value stream with an explicit output limit.
pub fn to_writer_documents_with_limits<W: Write>(
    writer: &mut W,
    values: &[Value],
    limits: &Limits,
) -> Result<(), Error> {
    emit_documents_to(
        Output::new(writer, limits),
        values,
        limits,
        EmitterOptions::default(),
        &EmitterStyles::default(),
    )
}

/// A reusable emitter configuration.
///
/// The configuration borrows its limits and options so that callers can reuse
/// one setup for many values without copying them. Use [`Emitter::with_styles`]
/// to add per-node presentation overrides.
///
/// ```
/// use ayaml::{Emitter, EmitterOptions, Limits};
///
/// let limits = Limits::default();
/// let options = EmitterOptions::default();
/// let emitter = Emitter::new(&limits, &options);
/// let text = emitter.to_string(&vec!["a", "b"]).unwrap();
/// assert_eq!(text, "- a\n- b\n");
/// ```
#[derive(Clone, Copy, Debug)]
pub struct Emitter<'a> {
    limits: &'a Limits,
    options: &'a EmitterOptions,
    styles: Option<&'a EmitterStyles>,
}

impl<'a> Emitter<'a> {
    /// Create an emitter configuration from limits and presentation options.
    #[must_use]
    pub const fn new(limits: &'a Limits, options: &'a EmitterOptions) -> Self {
        Self {
            limits,
            options,
            styles: None,
        }
    }

    /// Add per-node style overrides.
    #[must_use]
    pub const fn with_styles(mut self, styles: &'a EmitterStyles) -> Self {
        self.styles = Some(styles);
        self
    }

    /// Serialize a value into YAML text.
    pub fn to_string<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<String, Error> {
        let value = crate::to_value(value)?;
        self.value_to_string(&value)
    }

    /// Serialize a value as YAML to a writer.
    pub fn to_writer<T: serde::Serialize + ?Sized, W: Write>(
        &self,
        writer: &mut W,
        value: &T,
    ) -> Result<(), Error> {
        let value = crate::to_value(value)?;
        self.value_to_writer(writer, &value)
    }

    /// Serialize an existing generic value into YAML text.
    pub fn value_to_string(&self, value: &Value) -> Result<String, Error> {
        match self.styles {
            Some(styles) => {
                emit_value_with_limits_and_styles(value, self.limits, *self.options, styles)
            }
            None => emit_value_with_limits(value, self.limits, *self.options),
        }
    }

    /// Serialize an existing generic value as YAML to a writer.
    pub fn value_to_writer<W: Write>(&self, writer: &mut W, value: &Value) -> Result<(), Error> {
        match self.styles {
            Some(styles) => {
                emit_value_to_with_styles(writer, value, self.limits, *self.options, styles)
            }
            None => emit_value_to(writer, value, self.limits, *self.options),
        }
    }

    /// Serialize a sequence of values as a multi-document stream.
    pub fn documents_to_string(&self, values: &[Value]) -> Result<String, Error> {
        let mut output = Vec::new();
        let styles = self.styles.cloned().unwrap_or_default();
        emit_documents_to(
            Output::direct(&mut output, self.limits),
            values,
            self.limits,
            *self.options,
            &styles,
        )?;
        String::from_utf8(output).map_err(|error| {
            Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
        })
    }

    /// Write a multi-document value stream.
    pub fn documents_to_writer<W: Write>(
        &self,
        writer: &mut W,
        values: &[Value],
    ) -> Result<(), Error> {
        let styles = self.styles.cloned().unwrap_or_default();
        emit_documents_to(
            Output::new(writer, self.limits),
            values,
            self.limits,
            *self.options,
            &styles,
        )
    }
}

fn io_error(error: std::io::Error) -> Error {
    Error::new(ErrorKind::Io, "failed to write YAML output").with_source(error)
}

/// Implicit keys longer than this many characters are written as explicit
/// `? ` keys. YAML allows 1024; libyaml's emitter switches at 128, and so do
/// we, for output that libyaml-based readers accept.
pub(crate) const MAX_IMPLICIT_KEY_CHARS: usize = 128;

/// Bytes collected before a write to the underlying writer.
const OUTPUT_CHUNK: usize = 8 * 1024;

/// Output with a cumulative byte limit, shared by the emitters. Writes to an
/// arbitrary writer are collected into chunks of [`OUTPUT_CHUNK`] bytes;
/// writes into an in-memory buffer go straight through.
pub(crate) struct Output<'a, W: Write> {
    writer: &'a mut W,
    /// Collect writes in `buffer`: for a writer that isn't in memory, and while holding.
    buffered: bool,
    /// The writer is an in-memory buffer, which needs no chunking.
    direct: bool,
    /// Write `buffer` out once it holds this many bytes: [`OUTPUT_CHUNK`], or never while holding.
    flush_at: usize,
    buffer: Vec<u8>,
    written: usize,
    limits: Limits,
}

impl<'a, W: Write> Output<'a, W> {
    /// Output to a writer, in chunks.
    pub(crate) fn new(writer: &'a mut W, limits: &Limits) -> Self {
        Self {
            writer,
            buffered: true,
            direct: false,
            flush_at: OUTPUT_CHUNK,
            buffer: Vec::new(),
            written: 0,
            limits: limits.clone(),
        }
    }

    /// Output to an in-memory buffer, which needs no chunking.
    pub(crate) fn direct(writer: &'a mut W, limits: &Limits) -> Self {
        Self {
            buffered: false,
            direct: true,
            ..Self::new(writer, limits)
        }
    }

    pub(crate) fn push_str(&mut self, value: &str) -> Result<(), Error> {
        let total = self.written.saturating_add(value.len());
        self.limits.check_output_bytes(total).map_err(Error::from)?;
        self.written = total;
        if !self.buffered {
            return self.writer.write_all(value.as_bytes()).map_err(io_error);
        }
        self.buffer.extend_from_slice(value.as_bytes());
        if self.buffer.len() >= self.flush_at {
            self.flush()?;
        }
        Ok(())
    }

    /// Hold what is written from here on, until [`Output::release`], so that
    /// [`Output::insert_held`] can put text before it. Returns the place to insert at.
    pub(crate) fn hold(&mut self) -> usize {
        self.buffered = true;
        self.flush_at = usize::MAX;
        self.buffer.len()
    }

    /// Put `text` at `place`, which [`Output::hold`] returned, before what was written since.
    pub(crate) fn insert_held(&mut self, place: usize, text: &str) -> Result<(), Error> {
        let total = self.written.saturating_add(text.len());
        self.limits.check_output_bytes(total).map_err(Error::from)?;
        self.written = total;
        self.buffer.splice(place..place, text.bytes());
        Ok(())
    }

    /// Stop holding, and write what was held as any other output.
    pub(crate) fn release(&mut self) -> Result<(), Error> {
        self.buffered = !self.direct;
        self.flush_at = OUTPUT_CHUNK;
        if self.direct || self.buffer.len() >= OUTPUT_CHUNK {
            self.flush()?;
        }
        Ok(())
    }

    pub(crate) fn spaces(&mut self, count: usize) -> Result<(), Error> {
        const SPACES: &str = "                                ";
        let mut left = count;
        while left > 0 {
            let chunk = left.min(SPACES.len());
            self.push_str(&SPACES[..chunk])?;
            left -= chunk;
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<(), Error> {
        if !self.buffer.is_empty() {
            self.writer.write_all(&self.buffer).map_err(io_error)?;
            self.buffer.clear();
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<(), Error> {
        self.flush()
    }
}

pub(crate) fn validate_options(options: &EmitterOptions) -> Result<(), Error> {
    if (2..=9).contains(&options.indent) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorKind::Emitter,
            format!(
                "EmitterOptions::indent must be between 2 and 9, got {}",
                options.indent
            ),
        ))
    }
}

fn nested_tag_error() -> Error {
    Error::new(
        ErrorKind::Emitter,
        "YAML cannot write a tagged value whose payload is also tagged (for example a nested Serde enum)",
    )
}

struct ValueWriter<'a, 'w, 's, W: Write> {
    output: &'a mut Output<'w, W>,
    options: EmitterOptions,
    styles: &'s EmitterStyles,
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
}

impl<'a, 'w, 's, W: Write> ValueWriter<'a, 'w, 's, W> {
    fn new(
        output: &'a mut Output<'w, W>,
        limits: &Limits,
        options: EmitterOptions,
        styles: &'s EmitterStyles,
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
        }
    }

    fn style(&self) -> NodeStyle {
        if self.in_key {
            NodeStyle::default()
        } else {
            self.styles.get(&self.path).unwrap_or_default()
        }
    }

    fn scalar_style(&self) -> Option<crate::event::ScalarStyle> {
        self.style().scalar_style.or(self.options.scalar_style)
    }

    fn styles_active(&self) -> bool {
        !self.styles.styles.is_empty()
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

    fn fail(&mut self, error: Error) {
        if self.error.is_none() {
            self.error = Some(error);
        }
    }

    fn push_str(&mut self, value: &str) {
        if self.error.is_some() {
            return;
        }
        if let Err(error) = self.output.push_str(value) {
            self.error = Some(error);
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
            self.error = Some(error);
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

    /// Write a tag token. Fails for a tag whose payload is also tagged, which
    /// YAML cannot express.
    fn push_tag(&mut self, tagged: &crate::tag::TaggedValue) -> bool {
        if matches!(tagged.value, Value::Tagged(_)) {
            self.fail(nested_tag_error());
            return false;
        }
        match self
            .directives
            .token(&tagged.tag, RedefinedHandles::default())
        {
            Ok(token) => {
                self.push_str(&token);
                true
            }
            Err(error) => {
                self.fail(error);
                false
            }
        }
    }

    /// The first error, if writing failed. It borrows the writer rather than take it, which would
    /// copy it.
    fn finish(&mut self) -> Result<(), Error> {
        self.error.take().map_or(Ok(()), Err)
    }

    /// Write the root node, after any `---` and directives.
    fn emit_root(&mut self, value: &Value) {
        if self.style().flow.unwrap_or(self.options.flow) {
            self.emit_flow(value);
            self.push('\n');
        } else if let Value::String(text) = value
            && self.use_block_scalar(text)
        {
            self.emit_block_scalar(text, self.options.indent);
        } else if let Value::Tagged(tagged) = value
            && let Value::String(text) = &tagged.value
            && self.use_block_scalar(text)
        {
            if self.push_tag(tagged) {
                self.push(' ');
                self.emit_block_scalar(text, self.options.indent);
            }
        } else {
            match value {
                Value::Mapping(mapping) if !mapping.is_empty() => self.emit_mapping(mapping, 0),
                Value::Sequence(sequence) if !sequence.is_empty() => {
                    self.emit_sequence(sequence, 0)
                }
                _ => {
                    self.emit_flow(value);
                    self.push('\n');
                }
            }
        }
        if self.options.explicit_end {
            self.push_str("...\n");
        }
    }

    fn emit_mapping(&mut self, mapping: &crate::mapping::Mapping, indent: usize) {
        self.emit_mapping_items(mapping, indent, false);
    }

    /// Whether a block-mapping key must use the explicit `? ` form.
    fn needs_explicit_key(&self, key: &Value) -> bool {
        match key {
            Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => true,
            Value::String(text) => self.key_width(text) > MAX_IMPLICIT_KEY_CHARS,
            _ => false,
        }
    }

    /// How many characters a string key takes as [`ValueWriter::emit_key`] writes it, in the
    /// current flow context. Per-path styles don't apply to a key, but the scalar style option does.
    fn key_width(&self, text: &str) -> usize {
        match self.string_form(text, self.options.scalar_style) {
            StringForm::Plain => text.chars().count(),
            StringForm::SingleQuoted => text.chars().count() + text.matches('\'').count() + 2,
            StringForm::DoubleQuoted => double_quoted(text).chars().count(),
        }
    }

    /// Whether a flow-mapping key is a scalar whose text, with any tag, runs past
    /// [`MAX_IMPLICIT_KEY_CHARS`]. YAML puts no limit on an implicit key in a flow mapping, but
    /// readers built on libyaml refuse one past 1024 characters there too.
    fn flow_key_is_long(&mut self, key: &Value) -> bool {
        match key {
            Value::String(_) => self.needs_explicit_key(key),
            Value::Tagged(tagged) => {
                // A tag with no form fails when it is written.
                let Ok(token) = self
                    .directives
                    .token(&tagged.tag, RedefinedHandles::default())
                else {
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
                token.chars().count() + 1 + text > MAX_IMPLICIT_KEY_CHARS
            }
            _ => false,
        }
    }

    fn emit_key(&mut self, key: &Value) {
        let in_key = std::mem::replace(&mut self.in_key, true);
        self.emit_flow(key);
        self.in_key = in_key;
    }

    fn emit_mapping_items(
        &mut self,
        mapping: &crate::mapping::Mapping,
        indent: usize,
        inline_first: bool,
    ) {
        if !self.enter() {
            return;
        }
        for (index, (key, value)) in mapping.iter().enumerate() {
            if self.error.is_some() {
                break;
            }
            if index > 0 || !inline_first {
                self.indent(indent);
            }
            if self.needs_explicit_key(key) {
                self.push_str("? ");
                self.emit_key(key);
                self.push('\n');
                self.indent(indent);
                self.push(':');
            } else {
                self.emit_key(key);
                self.push(':');
            }
            self.push_key(key);
            if self.force_flow()
                && matches!(
                    value,
                    Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
                )
            {
                self.push(' ');
                self.emit_flow(value);
                self.push('\n');
            } else if Self::needs_block_value(value) {
                self.push('\n');
                self.emit_node(value, self.block_value_indent(value, indent));
            } else {
                self.push(' ');
                self.emit_inline_value(value, indent + self.options.indent);
            }
            self.pop_path();
        }
        self.leave();
    }

    fn block_value_indent(&self, value: &Value, indent: usize) -> usize {
        let sequence_like = matches!(value, Value::Sequence(sequence) if !sequence.is_empty());
        if !self.options.indent_sequences && sequence_like {
            indent
        } else {
            indent + self.options.indent
        }
    }

    fn emit_sequence(&mut self, sequence: &[Value], indent: usize) {
        self.emit_sequence_items(sequence, indent, false);
    }

    fn emit_sequence_items(&mut self, sequence: &[Value], indent: usize, inline_first: bool) {
        if !self.enter() {
            return;
        }
        let step = self.options.indent;
        for (index, item) in sequence.iter().enumerate() {
            if self.error.is_some() {
                break;
            }
            if index > 0 || !inline_first {
                self.indent(indent);
            }
            self.push('-');
            self.push_index(index);
            if self.force_flow()
                && matches!(
                    item,
                    Value::Mapping(_) | Value::Sequence(_) | Value::Tagged(_)
                )
            {
                self.push(' ');
                self.emit_flow(item);
                self.push('\n');
            } else if let Value::Sequence(inner) = item
                && !inner.is_empty()
            {
                // A compact entry starts at the child indentation, so the
                // `-` is padded to `indent` columns.
                self.indent(step - 1);
                self.emit_sequence_items(inner, indent + step, true);
            } else if let Value::Mapping(mapping) = item
                && !mapping.is_empty()
            {
                self.indent(step - 1);
                self.emit_mapping_items(mapping, indent + step, true);
            } else if Self::needs_block_value(item) {
                self.push('\n');
                self.emit_node(item, indent + step);
            } else {
                self.push(' ');
                self.emit_inline_value(item, indent + step);
            }
            self.pop_path();
        }
        self.leave();
    }

    fn emit_node(&mut self, value: &Value, indent: usize) {
        match value {
            Value::Mapping(mapping) if !mapping.is_empty() => self.emit_mapping(mapping, indent),
            Value::Sequence(sequence) if !sequence.is_empty() => {
                self.emit_sequence(sequence, indent)
            }
            Value::Tagged(tagged) => {
                self.indent(indent);
                if !self.push_tag(tagged) {
                    return;
                }
                self.push('\n');
                if !tagged.value.is_null() {
                    self.emit_node(&tagged.value, indent);
                }
            }
            _ => {
                self.indent(indent);
                self.emit_flow(value);
                self.push('\n');
            }
        }
    }

    fn needs_block_value(value: &Value) -> bool {
        match value {
            Value::Mapping(mapping) => !mapping.is_empty(),
            Value::Sequence(sequence) => !sequence.is_empty(),
            Value::Tagged(tagged) => Self::needs_block_value(&tagged.value),
            _ => false,
        }
    }

    /// Emit a node that follows `key: ` or `- `, ending with a line break.
    fn emit_inline_value(&mut self, value: &Value, indent: usize) {
        match value {
            Value::Mapping(mapping) if !mapping.is_empty() => {
                self.push('\n');
                self.emit_mapping(mapping, indent);
            }
            Value::Sequence(sequence) if !sequence.is_empty() => {
                self.push('\n');
                self.emit_sequence(sequence, indent);
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
                    self.emit_block_scalar(text, indent);
                } else if Self::needs_block_value(&tagged.value) {
                    self.push('\n');
                    self.emit_node(&tagged.value, indent);
                } else {
                    self.push(' ');
                    self.emit_flow(&tagged.value);
                    self.push('\n');
                }
            }
            Value::String(text) if self.use_block_scalar(text) => {
                self.emit_block_scalar(text, indent);
            }
            _ => {
                self.emit_flow(value);
                self.push('\n');
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

    fn emit_block_scalar(&mut self, text: &str, content_indent: usize) {
        let trailing = text.chars().rev().take_while(|ch| *ch == '\n').count();
        let body = &text[..text.len().saturating_sub(trailing)];
        let folded = matches!(self.scalar_style(), Some(crate::event::ScalarStyle::Folded))
            && can_fold(body);
        let folded_body;
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
            // The indicator counts from the parent node: `content_indent` is
            // always one indentation step below it.
            self.push(char::from_digit(self.options.indent as u32, 10).expect("validated"));
        }
        match chomping {
            Chomping::Strip => self.push('-'),
            Chomping::Clip => {}
            Chomping::Keep => self.push('+'),
        }
        self.push('\n');
        let body = if folded {
            folded_body = expand_folded(body);
            folded_body.as_str()
        } else {
            body
        };
        if body.is_empty() {
            for _ in 0..trailing {
                self.push('\n');
            }
        } else {
            for line in body.split('\n') {
                if !line.is_empty() {
                    self.indent(content_indent);
                    self.push_str(line);
                }
                self.push('\n');
            }
            for _ in 1..trailing {
                self.push('\n');
            }
        }
    }

    /// Write a node on the current line: a scalar, or a collection in flow
    /// style.
    fn emit_flow(&mut self, value: &Value) {
        match value {
            Value::Null => self.push_str("null"),
            Value::Bool(value) => self.push_str(if *value { "true" } else { "false" }),
            Value::Number(value) => self.push_str(&value.to_string()),
            Value::String(value) => self.emit_string(value),
            Value::Sequence(sequence) if sequence.is_empty() => self.push_str("[]"),
            Value::Mapping(mapping) if mapping.is_empty() => self.push_str("{}"),
            Value::Sequence(sequence) => {
                if !self.enter() {
                    return;
                }
                let in_flow = std::mem::replace(&mut self.in_flow, true);
                self.push('[');
                for (index, item) in sequence.iter().enumerate() {
                    if index > 0 {
                        self.push_str(", ");
                    }
                    self.push_index(index);
                    self.emit_flow(item);
                    self.pop_path();
                }
                self.push(']');
                self.in_flow = in_flow;
                self.leave();
            }
            Value::Mapping(mapping) => {
                if !self.enter() {
                    return;
                }
                let in_flow = std::mem::replace(&mut self.in_flow, true);
                self.push('{');
                for (index, (key, value)) in mapping.iter().enumerate() {
                    if index > 0 {
                        self.push_str(", ");
                    }
                    // libyaml writes non-empty collection keys and long
                    // keys in the explicit form; so do we.
                    let explicit = Self::needs_block_value(key) || self.flow_key_is_long(key);
                    if explicit {
                        self.push_str("? ");
                    }
                    self.emit_key(key);
                    // A tagged null already ends with a space.
                    if explicit && !matches!(key, Value::Tagged(tagged) if tagged.value.is_null()) {
                        self.push(' ');
                    }
                    self.push_str(": ");
                    self.push_key(key);
                    self.emit_flow(value);
                    self.pop_path();
                }
                self.push('}');
                self.in_flow = in_flow;
                self.leave();
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
                } else {
                    self.push(' ');
                    self.emit_flow(&tagged.value);
                }
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
        match self.string_form(value, self.scalar_style()) {
            StringForm::SingleQuoted => {
                self.push('\'');
                for (index, part) in value.split('\'').enumerate() {
                    if index > 0 {
                        self.push_str("''");
                    }
                    self.push_str(part);
                }
                self.push('\'');
            }
            StringForm::DoubleQuoted => self.push_str(&double_quoted(value)),
            StringForm::Plain => self.push_str(value),
        }
    }
}

/// How the value writer writes a string scalar.
#[derive(Clone, Copy)]
enum StringForm {
    Plain,
    SingleQuoted,
    DoubleQuoted,
}

/// Which of the default tag handles a document redefines with `%TAG`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RedefinedHandles {
    /// `%TAG ! ...` is in effect.
    pub(crate) primary: bool,
    /// `%TAG !! ...` is in effect.
    pub(crate) secondary: bool,
}

/// How a tag is written (D82).
#[derive(Debug)]
pub(crate) enum TagForm {
    /// As it stands: `!`, a `!` or `!!` shorthand, or a verbatim tag.
    Token(String),
    /// As a shorthand under a `%TAG` directive that the writer adds to the document: the prefix
    /// that the directive's handle stands for, and the suffix after the handle.
    Directive { prefix: String, suffix: String },
}

impl TagForm {
    /// The form of `tag` in a document that redefines the handles in `redefined`.
    ///
    /// A local tag uses `!` and a core tag `!!`, unless the document redefines that handle. Any
    /// other tag is written verbatim (`!<...>`) when that reads back as the same name: a verbatim
    /// tag is read as written, so it can hold only URI characters, and only a `%` that starts an
    /// escape, and only a local name or one with a URI scheme is a tag name there. Every other tag
    /// gets a `%TAG` directive, whose prefix and shorthand suffix are read with their escapes
    /// decoded, so they can hold any text. A tag of fewer than two characters that is neither local
    /// nor written verbatim has no form: both parts of a directive's shorthand hold a character.
    pub(crate) fn of(tag: &crate::tag::Tag, redefined: RedefinedHandles) -> Result<Self, Error> {
        let (handle, suffix) = (tag.handle(), tag.suffix());
        // The non-specific tag `!` is not a shorthand.
        if tag.is_non_specific() {
            return Ok(Self::Token("!".to_owned()));
        }
        if let Some(rest) = handle.strip_prefix('!')
            && !redefined.primary
        {
            let mut output = String::with_capacity(handle.len() + suffix.len());
            output.push('!');
            escape_tag(rest, &mut output);
            escape_tag(suffix, &mut output);
            return Ok(Self::Token(output));
        }
        if tag.is_core() && !suffix.is_empty() && !redefined.secondary {
            let mut output = String::with_capacity(suffix.len() + 2);
            output.push_str("!!");
            escape_tag(suffix, &mut output);
            return Ok(Self::Token(output));
        }
        if verbatim_reads_back(handle, suffix) {
            return Ok(Self::Token(format!("!<{handle}{suffix}>")));
        }
        if !handle.is_empty() && !suffix.is_empty() {
            return Ok(Self::Directive {
                prefix: handle.to_owned(),
                suffix: suffix.to_owned(),
            });
        }
        // A name kept whole: split it after its last `:`, `/` or `#`, or else before its last
        // character, so that both parts hold one.
        let name = if handle.is_empty() { suffix } else { handle };
        match name.char_indices().next_back() {
            Some((last, _)) if last > 0 => {
                let cut = name[..last]
                    .rfind([':', '/', '#'])
                    .map_or(last, |index| index + 1);
                Ok(Self::Directive {
                    prefix: name[..cut].to_owned(),
                    suffix: name[cut..].to_owned(),
                })
            }
            _ => Err(Error::new(
                ErrorKind::Emitter,
                format!("YAML has no way to write the tag {name:?}"),
            )),
        }
    }
}

/// Whether `tag` takes a `%TAG` directive, or has no form, in a document that redefines no handle:
/// [`TagForm::of`] without building the token.
fn needs_directive(tag: &crate::tag::Tag) -> bool {
    let (handle, suffix) = (tag.handle(), tag.suffix());
    !(handle.starts_with('!')
        || (tag.is_core() && !suffix.is_empty())
        || verbatim_reads_back(handle, suffix))
}

/// %-escape `text` for a tag shorthand's suffix or a `%TAG` prefix, whose escapes the parser
/// decodes: every byte but [`is_uri_byte`]'s, so a literal `%` is written `%25`.
fn escape_tag(text: &str, output: &mut String) {
    for byte in text.bytes() {
        if is_uri_byte(byte) {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
}

/// The `%TAG` directives that the writer adds to one document, for the tags whose [`TagForm`] is a
/// directive. Each handle is `!tN!`, numbered in order of first use, skipping any handle that the
/// document declares itself.
#[derive(Debug, Default)]
pub(crate) struct TagDirectives {
    /// The handles added so far, kept apart so that a document that adds none allocates nothing.
    added: Option<Box<AddedHandles>>,
    /// Named handles that the document's own directives declare.
    declared: Vec<String>,
}

#[derive(Debug, Default)]
struct AddedHandles {
    /// Each added handle and the prefix it stands for, in order.
    handles: Vec<(String, String)>,
    /// The index in `handles` of each prefix.
    by_prefix: std::collections::BTreeMap<String, usize>,
    /// The number of the next handle to try: one past the last one added, which may have skipped
    /// declared handles.
    next: usize,
}

impl TagDirectives {
    /// No directives yet, in a document whose own directives declare `declared`.
    pub(crate) fn declaring(declared: Vec<String>) -> Self {
        Self {
            declared,
            ..Self::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_none()
    }

    /// The handle that stands for `prefix`, added if there is none yet.
    pub(crate) fn handle_for(&mut self, prefix: &str) -> &str {
        let added = self.added.get_or_insert_with(Box::default);
        let index = match added.by_prefix.get(prefix) {
            Some(&index) => index,
            None => {
                let handle = loop {
                    let handle = format!("!t{}!", added.next);
                    added.next += 1;
                    if !self.declared.contains(&handle) {
                        break handle;
                    }
                };
                added.handles.push((handle, prefix.to_owned()));
                added
                    .by_prefix
                    .insert(prefix.to_owned(), added.handles.len() - 1);
                added.handles.len() - 1
            }
        };
        &added.handles[index].0
    }

    /// Write `tag` in its form, adding a directive for it if it needs one.
    pub(crate) fn token(
        &mut self,
        tag: &crate::tag::Tag,
        redefined: RedefinedHandles,
    ) -> Result<String, Error> {
        Ok(match TagForm::of(tag, redefined)? {
            TagForm::Token(token) => token,
            TagForm::Directive { prefix, suffix } => {
                let mut token = self.handle_for(&prefix).to_owned();
                escape_tag(&suffix, &mut token);
                token
            }
        })
    }

    /// The directives' lines.
    pub(crate) fn lines(&self) -> String {
        let mut output = String::new();
        for (handle, prefix) in self.added.iter().flat_map(|added| &added.handles) {
            output.push_str("%TAG ");
            output.push_str(handle);
            output.push(' ');
            // A local prefix keeps its `!`; a global one may start with any other character,
            // escaped.
            match prefix.strip_prefix('!') {
                Some(rest) => {
                    output.push('!');
                    escape_tag(rest, &mut output);
                }
                None => escape_tag(prefix, &mut output),
            }
            output.push('\n');
        }
        output
    }

    /// Add a directive for each tag in `value` that needs one, in the order the writer meets them,
    /// or fail at the first tag that has no form, as the writer would. The walk stops where the
    /// writer does, at a collection past `max_depth` and at a tag on a tagged value, so its
    /// recursion is no deeper than the writer's.
    fn scan(&mut self, value: &Value, max_depth: usize) -> Result<(), Error> {
        if may_hold_a_tag(value)
            && let Err(tag) = self.scan_at(value, max_depth)
        {
            return TagForm::of(tag, RedefinedHandles::default()).map(|_| ());
        }
        Ok(())
    }

    /// Add the directive that `tag` needs, apart from the walk so that the walk's frame stays small.
    #[cold]
    #[inline(never)]
    fn add<'v>(&mut self, tag: &'v crate::tag::Tag) -> Result<(), &'v crate::tag::Tag> {
        match self.token(tag, RedefinedHandles::default()) {
            Ok(_) => Ok(()),
            Err(_) => Err(tag),
        }
    }

    /// [`TagDirectives::scan`] with `depth_left` more collections to enter. Returns the tag with
    /// no form that stopped it.
    fn scan_at<'v>(
        &mut self,
        value: &'v Value,
        depth_left: usize,
    ) -> Result<(), &'v crate::tag::Tag> {
        match value {
            Value::Tagged(tagged) => {
                if matches!(tagged.value, Value::Tagged(_)) {
                    return Ok(());
                }
                if needs_directive(&tagged.tag) {
                    self.add(&tagged.tag)?;
                }
                self.scan_at(&tagged.value, depth_left)
            }
            Value::Sequence(items) if depth_left > 0 => {
                for item in items.iter().filter(|item| may_hold_a_tag(item)) {
                    self.scan_at(item, depth_left - 1)?;
                }
                Ok(())
            }
            Value::Mapping(mapping) if depth_left > 0 => {
                for (key, value) in mapping.iter() {
                    for node in [key, value] {
                        if may_hold_a_tag(node) {
                            self.scan_at(node, depth_left - 1)?;
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Whether `value` is a tag or a collection, which the scan for directives looks into: every other
/// value is a scalar, which it can pass without a call.
fn may_hold_a_tag(value: &Value) -> bool {
    matches!(
        value,
        Value::Tagged(_) | Value::Sequence(_) | Value::Mapping(_)
    )
}

/// YAML's printable character set (production 1, `c-printable`).
pub(crate) fn is_printable(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n'
            | '\r'
            | ' '..='~'
            | '\u{85}'
            | '\u{a0}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}'
    )
}

/// Whether a character can be written raw inside one line of a plain,
/// single-quoted, or block scalar. Line breaks, non-printable characters,
/// and the byte order mark need a double-quoted escape. NEL, LS, and PS are
/// escaped as well, because YAML 1.1 readers treat them as line breaks.
pub(crate) fn is_raw_char(ch: char) -> bool {
    is_printable(ch)
        && !matches!(
            ch,
            '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}'
        )
}

fn is_flow_indicator(ch: char) -> bool {
    matches!(ch, ',' | '[' | ']' | '{' | '}')
}

/// Whether `value` reads back as the same plain scalar text, in block
/// context or, with `flow`, inside a flow collection. This checks syntax
/// only; callers that write strings must also check how the text resolves.
///
/// Where libyaml's emitter is stricter than the spec, these rules follow it,
/// so the output also reads back under libyaml-based readers: a leading `?` or
/// `:`, and any `:` inside a flow collection, are quoted.
pub(crate) fn plain_text_ok(value: &str, flow: bool) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let second = chars.next();
    if first.is_whitespace()
        || value.ends_with(char::is_whitespace)
        || value.starts_with("---")
        || value.starts_with("...")
    {
        return false;
    }
    let leading_ok = match first {
        '-' => second.is_some_and(|next| next != ' ' && !(flow && is_flow_indicator(next))),
        '?' | ':' => !flow && second.is_some_and(|next| next != ' '),
        ',' | '[' | ']' | '{' | '}' | '#' | '&' | '*' | '!' | '|' | '>' | '\'' | '"' | '%'
        | '@' | '`' => false,
        _ => true,
    };
    if !leading_ok {
        return false;
    }
    let mut previous = None;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if !is_raw_char(ch) || ch == '\t' {
            return false;
        }
        if flow && (is_flow_indicator(ch) || ch == ':') {
            return false;
        }
        if ch == ':' && chars.peek().is_none_or(|next| *next == ' ') {
            return false;
        }
        if ch == '#' && previous == Some(' ') {
            return false;
        }
        previous = Some(ch);
    }
    true
}

/// Whether a plain scalar with this text resolves to a string. A resolution
/// error (an integer outside the value model) counts as "not a string", so
/// such text is quoted instead of being written in a form that cannot be
/// read back.
fn resolves_to_string(value: &str) -> bool {
    crate::resolver::plain_resolves_to_string(value)
}

/// Whether a string can be written as a plain scalar inside a flow collection.
pub(crate) fn plain_is_safe(value: &str) -> bool {
    plain_text_ok(value, true) && resolves_to_string(value)
}

fn plain_is_block_safe(value: &str) -> bool {
    plain_text_ok(value, false) && resolves_to_string(value)
}

/// Whether a string can be written single-quoted on one line.
pub(crate) fn single_quoted_ok(value: &str) -> bool {
    value.chars().all(is_raw_char)
}

/// Whether a string can be written as a literal or folded block scalar. It
/// must hold no characters that need escapes and no whitespace-only lines,
/// because a block scalar can lose their spaces.
pub(crate) fn block_scalar_ok(value: &str) -> bool {
    value.chars().all(|ch| ch == '\n' || is_raw_char(ch)) && !has_whitespace_only_line(value)
}

pub(crate) fn can_fold(body: &str) -> bool {
    if body.starts_with('\n') || body.contains(['\t', '\r']) {
        return false;
    }
    body.split('\n')
        .all(|line| !line.starts_with(' ') && !line.ends_with(' '))
}

pub(crate) fn expand_folded(body: &str) -> String {
    let mut output = String::with_capacity(body.len().saturating_add(8));
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        output.push(ch);
        if ch == '\n' {
            output.push('\n');
            while chars.peek() == Some(&'\n') {
                output.push('\n');
                chars.next();
            }
        }
    }
    output
}

/// Whether a block scalar needs an explicit indentation indicator. It does
/// when its first non-empty line starts with whitespace, because
/// auto-detection would read that whitespace as indentation.
pub(crate) fn needs_explicit_indent(text: &str) -> bool {
    let body = text.trim_end_matches('\n');
    body.split('\n')
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with([' ', '\t']))
}

fn has_whitespace_only_line(text: &str) -> bool {
    text.trim_end_matches('\n')
        .split('\n')
        .any(|line| !line.is_empty() && line.chars().all(|ch| matches!(ch, ' ' | '\t')))
}

pub(crate) fn double_quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    emit_double_quoted(value, &mut output);
    output
}

/// Write a double-quoted scalar. Every character outside the printable set,
/// every line break, and the byte order mark are escaped, using libyaml's
/// escape choices, so the output always fits on one line.
pub(crate) fn emit_double_quoted(value: &str, output: &mut String) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\0' => output.push_str("\\0"),
            '\u{0007}' => output.push_str("\\a"),
            '\u{0008}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{000b}' => output.push_str("\\v"),
            '\u{000c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\u{001b}' => output.push_str("\\e"),
            '\u{0085}' => output.push_str("\\N"),
            '\u{2028}' => output.push_str("\\L"),
            '\u{2029}' => output.push_str("\\P"),
            ch if !is_printable(ch) || ch == '\u{feff}' => {
                let code = ch as u32;
                if code <= 0xFF {
                    let _ = write!(output, "\\x{code:02X}");
                } else if code <= 0xFFFF {
                    let _ = write!(output, "\\u{code:04X}");
                } else {
                    let _ = write!(output, "\\U{code:08X}");
                }
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use crate::value::Value;

    #[test]
    fn emits_and_reparses_scalars_and_collections() {
        let mut mapping = crate::Mapping::new();
        mapping.insert(Value::from("plain"), Value::from("text"));
        mapping.insert(Value::from("ambiguous"), Value::from("true"));
        mapping.insert(
            Value::from("items"),
            Value::Sequence(vec![Value::from(1_u64), Value::from("x")]),
        );
        let value = Value::Mapping(mapping);
        let text = crate::to_string(&value).expect("emit");
        assert!(text.contains("ambiguous: \"true\""));
        let reparsed = crate::parse_value(&text).expect("reparse");
        assert_eq!(reparsed, value);
    }

    #[test]
    fn emits_tagged_values_and_round_trips_enums() {
        #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        enum E {
            Unit,
            Tuple(u64, u64),
        }
        let text = crate::to_string(&E::Unit).expect("emit");
        let value: E = crate::from_str(&text).expect("decode");
        assert_eq!(value, E::Unit);
        let text = crate::to_string(&E::Tuple(1, 2)).expect("emit");
        let value: E = crate::from_str(&text).expect("decode");
        assert_eq!(value, E::Tuple(1, 2));
    }

    #[test]
    fn generic_emitters_accept_unsized_serializable_values() {
        let scalar: &str = "sample";
        assert_eq!(crate::to_string(scalar).expect("emit str"), "sample\n");

        let sequence: &[u8] = &[1, 2, 3];
        let mut output = Vec::new();
        crate::to_writer(&mut output, sequence).expect("emit slice");
        assert_eq!(
            crate::parse_value(std::str::from_utf8(&output).expect("UTF-8 output"))
                .expect("parse sequence"),
            crate::Value::Sequence(vec![1_u64.into(), 2_u64.into(), 3_u64.into()])
        );
    }
}

#[cfg(test)]
mod tag_form_tests {
    use super::{RedefinedHandles, TagForm, needs_directive};
    use crate::tag::Tag;

    /// `needs_directive` is `TagForm::of` without the token, so the two agree on every tag.
    #[test]
    fn needs_directive_agrees_with_the_tag_form() {
        let tags = [
            Tag::local("x"),
            Tag::local("a b"),
            Tag::local(""),
            Tag::new("!", "x"),
            Tag::new("!e-", "x"),
            Tag::new("tag:yaml.org,2002:", "str"),
            Tag::new("tag:yaml.org,2002:", ""),
            Tag::new("tag:yaml.org,2002:", "a b"),
            Tag::new("tag:x:", "y"),
            Tag::new("tag:x:", "50%25"),
            Tag::new("tag:x:", "50%"),
            Tag::new("tag:x:", "50%2"),
            Tag::new("tag:x:", "50%zz"),
            Tag::new("tag:x:", "a b"),
            Tag::new("tag:x:", "é"),
            Tag::new("tag:x:", "[a],b"),
            Tag::new("", "foobar"),
            Tag::new("", "tag:x:y"),
            Tag::new("", "1abc:x"),
            Tag::new("", "a+b-c.d:x"),
            Tag::new("", "a_b:x"),
            Tag::new("foo", "bar"),
            Tag::new("x", "y"),
            Tag::new("x", ""),
            Tag::new("", "x"),
            Tag::new("", ""),
        ];
        for tag in tags {
            let form = TagForm::of(&tag, RedefinedHandles::default());
            let directive = !matches!(form, Ok(TagForm::Token(_)));
            assert_eq!(needs_directive(&tag), directive, "{tag:?}: {form:?}");
        }
    }
}

#[cfg(test)]
mod value_tests {
    use crate::value::Value;

    #[test]
    fn existing_tagged_values_round_trip_through_the_value_emitter() {
        for input in ["!Custom text\n", "!Unit\n", "!Tag [a, b]\n"] {
            let value = crate::parse_value(input).expect("parse");
            let text = crate::to_string_value(&value).expect("emit");
            let decoded = crate::parse_value(&text).expect("reparse");
            assert_eq!(decoded, value, "input={input:?} output={text:?}");
            assert!(matches!(decoded, Value::Tagged(_)));
        }
    }
}

#[cfg(test)]
mod collection_tests {
    #[test]
    fn mapping_sequences_are_indentless_and_sequence_mappings_are_compact() {
        let value = crate::parse_value("traces:\n  - a\n  - b\nitems:\n  - k: v\n").unwrap();
        let text = crate::to_string_value(&value).expect("emit");
        assert_eq!(text, "traces:\n- a\n- b\nitems:\n- k: v\n");
        assert_eq!(crate::parse_value(&text).unwrap(), value);
    }

    #[test]
    fn block_scalars_need_no_flow_quoting_and_control_escapes_are_named() {
        assert_eq!(crate::to_string("a, b").unwrap(), "a, b\n");
        assert_eq!(crate::to_string("a[b").unwrap(), "a[b\n");
        assert_eq!(crate::to_string("\u{1b}[41m").unwrap(), "\"\\e[41m\"\n");
    }

    #[test]
    fn nested_and_tagged_collections_round_trip() {
        for input in [
            "key:\n  - a\n  - b\n",
            "key: !Tag\n  - a\n  - b\n",
            "key: !Tag\n  a: b\n",
            "1: one\n2:\n  nested: true\n",
            "empty: {}\nitems: []\n",
            "? [a, b]\n: value\n",
        ] {
            let value = crate::parse_value(input).expect("parse");
            let text = crate::to_string_value(&value).expect("emit");
            let decoded = crate::parse_value(&text).expect("reparse");
            assert_eq!(decoded, value, "input={input:?} output={text:?}");
        }
    }
}

#[cfg(test)]
mod options_tests {
    use super::EmitterOptions;
    use crate::value::Value;

    #[test]
    fn flow_and_document_options_round_trip() {
        let value = crate::parse_value("a:\n  - 1\n  - 2\n").expect("parse");
        let flow = crate::to_string_value_with_options(
            &value,
            &EmitterOptions {
                flow: true,
                explicit_start: true,
                explicit_end: true,
                ..EmitterOptions::default()
            },
        )
        .expect("flow emit");
        assert!(flow.starts_with("---\n{"));
        assert!(flow.ends_with("...\n"));
        let decoded = crate::parse_value(&flow).expect("flow reparse");
        assert_eq!(decoded, value);

        let first = crate::parse_value("a: 1\n").expect("first");
        let second = crate::parse_value("b: 2\n").expect("second");
        let documents = crate::to_string_documents(&[first, second]).expect("documents");
        let limits = crate::Limits {
            max_documents: 2,
            ..crate::Limits::default()
        };
        let decoded =
            crate::parse_documents_with_limits(&documents, &limits).expect("reparse documents");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1]["b"], Value::from(2_u64));
    }
}

#[cfg(test)]
mod ambiguity_tests {
    use crate::value::Value;

    #[test]
    fn ambiguous_strings_round_trip() {
        let strings = [
            "",
            "null",
            "true",
            "0123",
            "0b11",
            "1e3",
            "a: b",
            "a #b",
            " leading",
            "trailing ",
            "-",
            "?",
            ":",
            "---",
            "...",
            "yes",
            "line\nbreak",
            "quote\"slash\\",
            "é",
        ];
        let value = Value::Sequence(strings.iter().map(|text| Value::from(*text)).collect());
        let text = crate::to_string_value(&value).expect("emit");
        let decoded = crate::parse_value(&text).expect("reparse");
        assert_eq!(decoded, value, "output={text:?}");
    }
}

#[cfg(test)]
mod scalar_style_tests {
    use crate::event::ScalarStyle;
    use crate::value::Value;

    #[test]
    fn scalar_style_options_are_applied() {
        let value = Value::from("plain text");
        let text = crate::to_string_value_with_options(
            &value,
            &super::EmitterOptions {
                scalar_style: Some(ScalarStyle::SingleQuoted),
                ..super::EmitterOptions::default()
            },
        )
        .expect("single quote");
        assert_eq!(text, "'plain text'\n");

        let text = crate::to_string_value_with_options(
            &value,
            &super::EmitterOptions {
                scalar_style: Some(ScalarStyle::DoubleQuoted),
                ..super::EmitterOptions::default()
            },
        )
        .expect("double quote");
        assert_eq!(text, "\"plain text\"\n");
        assert_eq!(crate::parse_value(&text).unwrap(), value);
    }
}

#[cfg(test)]
mod output_limit_tests {
    use crate::value::Value;

    #[test]
    fn output_limits_are_enforced_for_values_events_and_documents() {
        let value = Value::from("long enough to exceed the limit");
        let limits = crate::Limits {
            max_output_bytes: 3,
            ..crate::Limits::default()
        };
        let error = crate::to_string_value_with_limits(&value, &limits).expect_err("value limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);

        let documents = crate::parse_documents_with_limits(
            "a: 1\n---\nb: 2\n",
            &crate::Limits {
                max_documents: 2,
                ..crate::Limits::default()
            },
        )
        .expect("documents");
        let error = crate::to_string_documents_with_limits(&documents, &limits)
            .expect_err("document limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);

        let events: Vec<crate::Event<'_>> = crate::parse_events("a: 1\n")
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let error = crate::to_string_events_with_limits(&events, &limits).expect_err("event limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
    }
}

#[cfg(test)]
mod tag_token_tests {
    use crate::tag::{Tag, TaggedValue};
    use crate::value::Value;

    #[test]
    fn expanded_tags_use_verbatim_syntax() {
        let value = Value::from(TaggedValue::new(
            Tag::new("tag:example.com,2026:", "foo"),
            Value::from("x"),
        ));
        let text = crate::to_string_value(&value).expect("emit");
        assert!(text.contains("!<tag:example.com,2026:foo>"), "{text:?}");
        let decoded = crate::parse_value(&text).expect("reparse");
        assert!(decoded.is_tagged());
    }
}

#[cfg(test)]
mod writer_tests {
    use crate::value::Value;

    #[test]
    fn document_writer_streams_parseable_output() {
        let values = vec![
            crate::parse_value("a: 1\n").expect("first"),
            crate::parse_value("b: 2\n").expect("second"),
        ];
        let mut output = Vec::new();
        crate::to_writer_documents(&mut output, &values).expect("write");
        let limits = crate::Limits {
            max_documents: 2,
            ..crate::Limits::default()
        };
        let decoded = crate::parse_documents_with_limits(
            std::str::from_utf8(&output).expect("utf8"),
            &limits,
        )
        .expect("reparse");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1]["b"], Value::from(2_u64));
    }
}

#[cfg(test)]
mod writer_option_tests {
    #[test]
    fn writer_options_apply_and_round_trip() {
        let value = crate::parse_value("a: 1\n").expect("parse");
        let mut output = Vec::new();
        crate::to_writer_value_with_options(
            &mut output,
            &value,
            &super::EmitterOptions {
                flow: true,
                explicit_start: true,
                ..super::EmitterOptions::default()
            },
        )
        .expect("write");
        let text = std::str::from_utf8(&output).expect("utf8");
        assert!(text.starts_with("---\n{"));
        assert_eq!(crate::parse_value(text).expect("reparse"), value);
    }
}

#[cfg(test)]
mod streaming_writer_tests {
    #[test]
    fn value_writer_is_incremental_and_limits_output() {
        let value = crate::parse_value("a: 1\n").expect("parse");
        let mut output = Vec::new();
        crate::to_writer_value(&mut output, &value).expect("write");
        assert_eq!(
            std::str::from_utf8(&output).expect("utf8"),
            crate::to_string_value(&value).expect("string")
        );

        let limits = crate::Limits {
            max_output_bytes: 2,
            ..crate::Limits::default()
        };
        let mut limited = Vec::new();
        let error =
            crate::to_writer_value_with_limits(&mut limited, &value, &limits).expect_err("limit");
        assert_eq!(error.kind(), crate::ErrorKind::Limit);
        assert!(limited.len() <= 2);
    }
}

#[cfg(test)]
mod value_block_scalar_tests {
    use crate::event::ScalarStyle;
    use crate::value::Value;

    #[test]
    fn multiline_value_strings_emit_as_block_scalars() {
        for text in ["a\nb\n", "a\nb", "a\n\n", "line\n\nnext\n"] {
            let value = Value::from(text);
            let output = crate::to_string_value(&value).expect("emit");
            assert!(output.starts_with('|'), "output={output:?}");
            assert_eq!(
                crate::parse_value(&output).expect("reparse"),
                value,
                "output={output:?}"
            );
        }
    }

    #[test]
    fn forced_block_scalar_style_round_trips() {
        let value = Value::from("plain text");
        let output = crate::to_string_value_with_options(
            &value,
            &super::EmitterOptions {
                scalar_style: Some(ScalarStyle::Literal),
                ..super::EmitterOptions::default()
            },
        )
        .expect("literal");
        assert!(output.starts_with("|-"));
        assert_eq!(crate::parse_value(&output).unwrap(), value);

        let output = crate::to_string_value_with_options(
            &value,
            &super::EmitterOptions {
                scalar_style: Some(ScalarStyle::Folded),
                ..super::EmitterOptions::default()
            },
        )
        .expect("folded");
        assert!(output.starts_with(">-"));
        assert_eq!(crate::parse_value(&output).unwrap(), value);
    }
}

#[cfg(test)]
mod tagged_block_scalar_tests {
    #[test]
    fn tagged_multiline_strings_emit_as_tagged_block_scalars() {
        let value = crate::parse_value("!Custom |\n  a\n  b\n").expect("parse");
        let output = crate::to_string_value(&value).expect("emit");
        assert!(output.starts_with("!Custom |"), "{output:?}");
        let decoded = crate::parse_value(&output).expect("reparse");
        assert_eq!(decoded, value);

        let value = crate::parse_value("key: !Custom |\n  a\n  b\n").expect("parse");
        let output = crate::to_string_value(&value).expect("emit");
        assert!(output.contains("!Custom |"), "{output:?}");
        assert_eq!(crate::parse_value(&output).expect("reparse"), value);
    }
}

#[cfg(test)]
mod indent_tests {
    #[test]
    fn custom_indent_is_used_for_nested_blocks() {
        let value = crate::parse_value("a:\n  b: 1\n").expect("parse");
        let output = crate::to_string_value_with_options(
            &value,
            &super::EmitterOptions {
                indent: 4,
                ..super::EmitterOptions::default()
            },
        )
        .expect("emit");
        assert!(output.contains("a:\n    b: 1"), "{output:?}");
        assert_eq!(crate::parse_value(&output).expect("reparse"), value);
    }
}

#[cfg(test)]
mod builder_tests {
    use super::{Emitter, EmitterOptions, EmitterStyles, NodeStyle};
    use crate::value::Value;

    #[test]
    fn builder_emits_values_writers_and_documents() {
        let limits = crate::Limits::default();
        let options = EmitterOptions::default().with_flow(true);
        let emitter = Emitter::new(&limits, &options);
        let value = crate::parse_value("a: 1\n").expect("parse");

        let text = emitter.value_to_string(&value).expect("string");
        assert_eq!(text, "{a: 1}\n");
        assert_eq!(crate::parse_value(&text).expect("reparse"), value);

        let mut output = Vec::new();
        emitter.value_to_writer(&mut output, &value).expect("write");
        assert_eq!(String::from_utf8(output).expect("utf8"), text);

        let documents = [value, Value::from("next")];
        let stream = emitter.documents_to_string(&documents).expect("documents");
        assert_eq!(stream, "{a: 1}\n---\nnext\n");
    }

    #[test]
    fn builder_applies_per_node_styles() {
        let limits = crate::Limits::default();
        let options = EmitterOptions::default();
        let mut styles = EmitterStyles::new();
        styles.set(
            vec![crate::PathSegment::key("a")],
            NodeStyle::default().with_scalar_style(Some(crate::ScalarStyle::SingleQuoted)),
        );
        let emitter = Emitter::new(&limits, &options).with_styles(&styles);
        let value = crate::parse_value("a: plain\n").expect("parse");
        assert_eq!(
            emitter.value_to_string(&value).expect("emit"),
            "a: 'plain'\n"
        );
    }
}
