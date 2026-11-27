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
use std::io::Write;

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::limits::Limits;
use crate::options::EmitOptions;
use crate::value::Value;

mod scalars;
mod tags;
mod value_writer;

pub(crate) use self::scalars::{
    block_scalar_ok, can_fold, double_quoted_char_len_capped, is_printable,
    looks_like_leading_zero_numeral, needs_explicit_indent, plain_text_ok,
    single_quoted_char_len_capped, single_quoted_ok, write_double_quoted, write_folded_body,
    write_single_quoted,
};
pub(crate) use self::tags::{RedefinedHandles, TagDirectives};
use self::value_writer::ValueWriter;

/// Emitter presentation options.
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags are independent scanner or emitter options, not a state enum"
)]
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
    /// Default block-style emitter options, usable in `const` contexts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flow: false,
            explicit_start: false,
            explicit_end: false,
            scalar_style: None,
            indent: 2,
            indent_sequences: false,
        }
    }

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    /// Empty per-node style override, usable in `const` contexts.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            scalar_style: None,
            chomping: None,
            flow: None,
        }
    }

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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
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
        Self::new()
    }
}

/// Serialize a value into YAML text with default options and limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string<T: serde::Serialize + ?Sized>(value: &T) -> Result<String, Error> {
    to_string_with(value, &EmitOptions::new())
}

/// Serialize a value into YAML text with explicit emitter options and limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_with<T: serde::Serialize + ?Sized>(
    value: &T,
    options: &EmitOptions,
) -> Result<String, Error> {
    options.limits.validate()?;
    validate_options(&options.emitter_options())?;
    let value = crate::to_value(value)?;
    to_string_value_with(&value, options)
}

/// Serialize a value into YAML text with explicit output limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_with_limits<T: serde::Serialize + ?Sized>(
    value: &T,
    limits: &Limits,
) -> Result<String, Error> {
    let value = crate::to_value(value)?;
    emit_value_to_string(&value, limits, EmitterOptions::new(), None)
}

/// Serialize a value with explicit emitter options.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_with_options<T: serde::Serialize + ?Sized>(
    value: &T,
    options: &EmitterOptions,
) -> Result<String, Error> {
    let value = crate::to_value(value)?;
    emit_value_to_string(&value, &Limits::default_profile(), *options, None)
}

/// Serialize an existing generic value as YAML, preserving its tags.
///
/// A tag is written as it is given, with two exceptions that have no text: a tag on a value
/// that is itself tagged, and `!!int`, `!!float`, `!!bool`, or `!!null` on a scalar that the
/// tag's rule doesn't take, such as `!!int` on the string `x`, which no reader loads. A tag of
/// the core schema that is written reads back as what a reader makes of it: `!!str` on the
/// number 1 reads back as the string `1`, and a collection reads back without the tag.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_value(value: &Value) -> Result<String, Error> {
    to_string_value_with(value, &EmitOptions::new())
}

/// Serialize an existing generic value as YAML with explicit emitter options
/// and limits, preserving its tags.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_value_with(value: &Value, options: &EmitOptions) -> Result<String, Error> {
    emit_value_to_string(
        value,
        &options.limits,
        options.emitter_options(),
        options.styles.as_ref(),
    )
}

/// Serialize an existing generic value as YAML with explicit output limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_value_with_limits(value: &Value, limits: &Limits) -> Result<String, Error> {
    emit_value_to_string(value, limits, EmitterOptions::new(), None)
}

/// Serialize an existing generic value with explicit emitter options.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_value_with_options(
    value: &Value,
    options: &EmitterOptions,
) -> Result<String, Error> {
    emit_value_to_string(value, &Limits::default_profile(), *options, None)
}

/// Serialize an existing generic value with per-node style overrides.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_value_with_styles(value: &Value, styles: &EmitterStyles) -> Result<String, Error> {
    emit_value_to_string(
        value,
        &Limits::default_profile(),
        EmitterOptions::new(),
        Some(styles),
    )
}

/// Serialize an existing generic value with explicit limits and per-node styles.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_value_with_limits_and_styles(
    value: &Value,
    limits: &Limits,
    styles: &EmitterStyles,
) -> Result<String, Error> {
    emit_value_to_string(value, limits, EmitterOptions::new(), Some(styles))
}

/// Serialize an existing generic value with options and per-node styles.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_value_with_options_and_styles(
    value: &Value,
    options: &EmitterOptions,
    styles: &EmitterStyles,
) -> Result<String, Error> {
    emit_value_to_string(value, &Limits::default_profile(), *options, Some(styles))
}

/// Serialize an existing tagged value as YAML to a writer with per-node styles.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_value_with_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to(
        writer,
        value,
        &Limits::default_profile(),
        EmitterOptions::new(),
        Some(styles),
    )
}

/// Serialize an existing tagged value to a writer with options and styles.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_value_with_options_and_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    options: &EmitterOptions,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to(
        writer,
        value,
        &Limits::default_profile(),
        *options,
        Some(styles),
    )
}

/// Serialize an existing tagged value to a writer with limits and styles.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_value_with_limits_and_styles<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
    styles: &EmitterStyles,
) -> Result<(), Error> {
    emit_value_to(writer, value, limits, EmitterOptions::new(), Some(styles))
}

/// Serialize a sequence of generic values as a multi-document stream.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_documents(values: &[Value]) -> Result<String, Error> {
    to_string_documents_with(values, &EmitOptions::new())
}

/// Serialize a sequence of generic values as a multi-document stream with
/// explicit emitter options and limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_string_documents_with(values: &[Value], options: &EmitOptions) -> Result<String, Error> {
    emit_documents_to_string(
        values,
        &options.limits,
        options.emitter_options(),
        options.styles.as_ref(),
    )
}

/// Serialize a sequence of values as a stream with an explicit output limit.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_string_documents_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_string_documents_with_limits(values: &[Value], limits: &Limits) -> Result<String, Error> {
    emit_documents_to_string(values, limits, EmitterOptions::new(), None)
}

fn emit_documents_to_string(
    values: &[Value],
    limits: &Limits,
    options: EmitterOptions,
    styles: Option<&EmitterStyles>,
) -> Result<String, Error> {
    let mut output = Vec::new();
    emit_documents_to(
        Output::direct(&mut output, limits),
        values,
        limits,
        options,
        styles,
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

fn emit_value_to_string(
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: Option<&EmitterStyles>,
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
        Error::coded(
            ErrorKind::Emitter,
            ErrorCode::InvalidEmitEvent,
            "emitter produced invalid UTF-8",
        )
        .with_source(error)
    })
}

fn emit_value_to<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: Option<&EmitterStyles>,
) -> Result<(), Error> {
    emit_value_with(Output::new(writer, limits), value, limits, options, styles)
}

fn emit_value_with<W: Write>(
    mut output: Output<'_, W>,
    value: &Value,
    limits: &Limits,
    options: EmitterOptions,
    styles: Option<&EmitterStyles>,
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
    styles: Option<&EmitterStyles>,
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
    styles: Option<&EmitterStyles>,
    later: bool,
) -> Result<(), Error> {
    let mut directives = TagDirectives::default();
    directives.scan(value, limits.max_depth, output)?;
    if !directives.is_empty() {
        // A directive can't follow a document that `...` didn't end.
        if later && !options.explicit_end {
            output.push_str("...\n")?;
        }
        directives.write_lines(output)?;
        output.push_str("---\n")?;
    } else if later || options.explicit_start {
        output.push_str("---\n")?;
    }
    let mut emitter = ValueWriter::new(output, limits, options, styles, directives);
    emitter.emit_root(value);
    emitter.finish()
}

/// Serialize a value as YAML to a writer.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
) -> Result<(), Error> {
    to_writer_with(writer, value, &EmitOptions::new())
}

/// Serialize a value as YAML to a writer with explicit emitter options and
/// limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_with<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
    options: &EmitOptions,
) -> Result<(), Error> {
    options.limits.validate()?;
    validate_options(&options.emitter_options())?;
    let value = crate::to_value(value)?;
    to_writer_value_with(writer, &value, options)
}

/// Serialize a value as YAML to a writer with explicit limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_with_limits<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
    limits: &Limits,
) -> Result<(), Error> {
    let value = crate::to_value(value)?;
    emit_value_to(writer, &value, limits, EmitterOptions::new(), None)
}

/// Serialize a value as YAML to a writer with explicit emitter options.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_with_options<T: serde::Serialize + ?Sized, W: Write>(
    writer: &mut W,
    value: &T,
    options: &EmitterOptions,
) -> Result<(), Error> {
    let value = crate::to_value(value)?;
    emit_value_to(writer, &value, &Limits::default_profile(), *options, None)
}

/// Serialize an existing tagged value as YAML to a writer.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_value<W: Write>(writer: &mut W, value: &Value) -> Result<(), Error> {
    to_writer_value_with(writer, value, &EmitOptions::new())
}

/// Serialize an existing tagged value as YAML to a writer with explicit emitter
/// options and limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_value_with<W: Write>(
    writer: &mut W,
    value: &Value,
    options: &EmitOptions,
) -> Result<(), Error> {
    emit_value_to(
        writer,
        value,
        &options.limits,
        options.emitter_options(),
        options.styles.as_ref(),
    )
}

/// Serialize an existing tagged value to a writer with explicit limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_value_with_limits<W: Write>(
    writer: &mut W,
    value: &Value,
    limits: &Limits,
) -> Result<(), Error> {
    emit_value_to(writer, value, limits, EmitterOptions::new(), None)
}

/// Serialize an existing value as YAML to a writer with emitter options.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_value_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_value_with_options<W: Write>(
    writer: &mut W,
    value: &Value,
    options: &EmitterOptions,
) -> Result<(), Error> {
    emit_value_to(writer, value, &Limits::default_profile(), *options, None)
}

/// Write a multi-document value stream one document at a time.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_documents<W: Write>(writer: &mut W, values: &[Value]) -> Result<(), Error> {
    to_writer_documents_with(writer, values, &EmitOptions::new())
}

/// Write a multi-document value stream one document at a time with explicit
/// emitter options and limits.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
pub fn to_writer_documents_with<W: Write>(
    writer: &mut W,
    values: &[Value],
    options: &EmitOptions,
) -> Result<(), Error> {
    emit_documents_to(
        Output::new(writer, &options.limits),
        values,
        &options.limits,
        options.emitter_options(),
        options.styles.as_ref(),
    )
}

/// Write a multi-document value stream with an explicit output limit.
///
/// # Errors
///
/// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
#[deprecated(
    note = "use `to_writer_documents_with` with `EmitOptions`; deprecated through 0.x and will be removed in 1.0"
)]
pub fn to_writer_documents_with_limits<W: Write>(
    writer: &mut W,
    values: &[Value],
    limits: &Limits,
) -> Result<(), Error> {
    emit_documents_to(
        Output::new(writer, limits),
        values,
        limits,
        EmitterOptions::new(),
        None,
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
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    #[must_use]
    pub const fn with_styles(mut self, styles: &'a EmitterStyles) -> Self {
        self.styles = Some(styles);
        self
    }

    /// Serialize a value into YAML text.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn to_string<T: serde::Serialize + ?Sized>(&self, value: &T) -> Result<String, Error> {
        let value = crate::to_value(value)?;
        self.value_to_string(&value)
    }

    /// Serialize a value as YAML to a writer.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn to_writer<T: serde::Serialize + ?Sized, W: Write>(
        &self,
        writer: &mut W,
        value: &T,
    ) -> Result<(), Error> {
        let value = crate::to_value(value)?;
        self.value_to_writer(writer, &value)
    }

    /// Serialize an existing generic value into YAML text.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn value_to_string(&self, value: &Value) -> Result<String, Error> {
        emit_value_to_string(value, self.limits, *self.options, self.styles)
    }

    /// Serialize an existing generic value as YAML to a writer.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn value_to_writer<W: Write>(&self, writer: &mut W, value: &Value) -> Result<(), Error> {
        emit_value_to(writer, value, self.limits, *self.options, self.styles)
    }

    /// Serialize a sequence of values as a multi-document stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn documents_to_string(&self, values: &[Value]) -> Result<String, Error> {
        emit_documents_to_string(values, self.limits, *self.options, self.styles)
    }

    /// Write a multi-document value stream.
    ///
    /// # Errors
    ///
    /// Returns an error when the value cannot be written, a configured limit is exceeded, or the writer fails.
    pub fn documents_to_writer<W: Write>(
        &self,
        writer: &mut W,
        values: &[Value],
    ) -> Result<(), Error> {
        emit_documents_to(
            Output::new(writer, self.limits),
            values,
            self.limits,
            *self.options,
            self.styles,
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

    #[inline]
    pub(crate) fn check_additional(&self, bytes: usize) -> Result<(), Error> {
        let total = self.written.saturating_add(bytes);
        self.limits.check_output_bytes(total).map_err(Error::from)
    }

    #[inline]
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

/// Number of inline frames kept on the fast-path stack (covers typical YAML depths with zero
/// heap allocation and minimal stack initialization).
pub(super) const STACK_SMALL: usize = 8;

/// Number of additional inline frames allocated lazily in a single non-recursive cold frame
/// when nesting exceeds [`STACK_SMALL`] (`STACK_SMALL + STACK_MEDIUM == MAX_DEPTH_CAP`), keeping
/// heap allocations at zero up to `MAX_DEPTH_CAP` while spilling arbitrarily deeper trees onto
/// the heap.
pub(super) const STACK_MEDIUM: usize = 120;

/// Two-tier inline-plus-heap LIFO stack with O(1) native stack usage at any depth.
pub(super) struct TieredStack<'s, 'm, T> {
    small: &'s mut [Option<T>],
    medium: &'m mut [Option<T>],
    inline_len: usize,
    pending: Option<T>,
    overflow: Vec<T>,
}

impl<'s, 'm, T> TieredStack<'s, 'm, T> {
    #[inline]
    pub(super) fn new(small: &'s mut [Option<T>], medium: &'m mut [Option<T>]) -> Self {
        Self {
            small,
            medium,
            inline_len: 0,
            pending: None,
            overflow: Vec::new(),
        }
    }

    #[inline]
    pub(super) fn from_promoted(
        small: &'s mut [Option<T>],
        medium: &'m mut [Option<T>],
        small_len: usize,
        pending: T,
    ) -> Self {
        medium[0] = Some(pending);
        Self {
            small,
            medium,
            inline_len: small_len + 1,
            pending: None,
            overflow: Vec::new(),
        }
    }

    #[inline]
    pub(super) fn push(&mut self, value: T) {
        let small_cap = self.small.len();
        if self.inline_len < small_cap {
            self.small[self.inline_len] = Some(value);
            self.inline_len += 1;
            return;
        }
        let medium_cap = self.medium.len();
        if medium_cap == 0 {
            debug_assert!(self.pending.is_none());
            self.pending = Some(value);
            return;
        }
        let medium_idx = self.inline_len - small_cap;
        if medium_idx < medium_cap {
            self.medium[medium_idx] = Some(value);
            self.inline_len += 1;
        } else {
            self.overflow.push(value);
        }
    }

    #[inline]
    pub(super) fn take_pending(&mut self) -> Option<T> {
        self.pending.take()
    }

    #[inline]
    pub(super) fn small_parts(&mut self) -> (&mut [Option<T>], usize) {
        (self.small, self.inline_len)
    }

    #[inline]
    pub(super) fn pop(&mut self) -> Option<T> {
        if let Some(value) = self.overflow.pop() {
            return Some(value);
        }
        if self.inline_len == 0 {
            return None;
        }
        self.inline_len -= 1;
        let small_cap = self.small.len();
        if self.inline_len < small_cap {
            self.small[self.inline_len].take()
        } else {
            self.medium[self.inline_len - small_cap].take()
        }
    }

    #[inline]
    pub(super) fn drop_top(&mut self) {
        if self.overflow.pop().is_some() {
            return;
        }
        if self.inline_len == 0 {
            return;
        }
        self.inline_len -= 1;
        let small_cap = self.small.len();
        if self.inline_len < small_cap {
            self.small[self.inline_len] = None;
        } else {
            self.medium[self.inline_len - small_cap] = None;
        }
    }

    #[inline]
    pub(super) fn last_mut(&mut self) -> Option<&mut T> {
        if let Some(last) = self.overflow.last_mut() {
            return Some(last);
        }
        if self.inline_len == 0 {
            return None;
        }
        let idx = self.inline_len - 1;
        let small_cap = self.small.len();
        if idx < small_cap {
            self.small[idx].as_mut()
        } else {
            self.medium[idx - small_cap].as_mut()
        }
    }
}

pub(crate) fn validate_options(options: &EmitterOptions) -> Result<(), Error> {
    if (2..=9).contains(&options.indent) {
        Ok(())
    } else {
        Err(Error::coded(
            ErrorKind::Emitter,
            ErrorCode::InvalidEmitterOptions,
            format!("indent must be between 2 and 9, got {}", options.indent),
        ))
    }
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
            Value::Sequence(vec![1_u64.into(), 2_u64.into(), 3_u64.into()])
        );
    }
}

#[cfg(test)]
mod builder_tests;
#[cfg(test)]
mod collection_tests;
#[cfg(test)]
mod indent_tests;
#[cfg(test)]
mod options_tests;
#[cfg(test)]
mod tag_form_tests;
#[cfg(test)]
mod tagged_block_scalar_tests;
#[cfg(test)]
mod value_block_scalar_tests;
#[cfg(test)]
mod value_tests;
#[cfg(test)]
mod writer_tests;
