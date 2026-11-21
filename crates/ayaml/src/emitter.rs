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

use crate::error::{Error, ErrorKind};
use crate::limits::Limits;
use crate::options::EmitOptions;
use crate::value::Value;

mod scalars;
mod tags;

use self::scalars::plain_is_block_safe;
pub(crate) use self::scalars::{
    block_scalar_ok, can_fold, double_quoted, emit_double_quoted, expand_folded, is_printable,
    looks_like_leading_zero_numeral, needs_explicit_indent, plain_is_safe, plain_text_ok,
    single_quoted_ok,
};
pub(crate) use self::tags::{RedefinedHandles, TagDirectives};

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
        Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
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
        Error::new(ErrorKind::Emitter, "emitter produced invalid UTF-8").with_source(error)
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
            format!("indent must be between 2 and 9, got {}", options.indent),
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
}

impl<'a, 'w, 's, W: Write> ValueWriter<'a, 'w, 's, W> {
    fn new(
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
    /// YAML cannot express. A tag of the core schema on a scalar that it doesn't take was
    /// refused before the document's first line, by the scan for the tags that need a
    /// directive.
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
                Value::Mapping(mapping) if !mapping.is_empty() => self.emit_mapping(mapping, 0),
                Value::Sequence(sequence) if !sequence.is_empty() => {
                    self.emit_sequence(sequence, 0);
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
                self.emit_sequence(sequence, indent);
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
                    self.emit_block_scalar(text, indent, false);
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
                self.emit_block_scalar(text, indent, false);
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

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the indentation indicator is a single digit in 1..=9"
    )]
    fn emit_block_scalar(&mut self, text: &str, content_indent: usize, root: bool) {
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
            // An empty collection is one level too, as the parser and the event writer count
            // it: what is written within `max_depth` is read back within it.
            Value::Sequence(sequence) if sequence.is_empty() => {
                if self.enter() {
                    self.push_str("[]");
                    self.leave();
                }
            }
            Value::Mapping(mapping) if mapping.is_empty() => {
                if self.enter() {
                    self.push_str("{}");
                    self.leave();
                }
            }
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
mod tag_form_tests {
    use super::RedefinedHandles;
    use super::tags::{TagForm, needs_directive};
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
mod writer_tests {
    use crate::value::Value;
    use crate::{Limits, ParseOptions};

    #[test]
    fn document_writer_streams_parseable_output() {
        let values = vec![
            crate::parse_value("a: 1\n").expect("first"),
            crate::parse_value("b: 2\n").expect("second"),
        ];
        let mut output = Vec::new();
        crate::to_writer_documents(&mut output, &values).expect("write");
        let options = ParseOptions::new().with_limits(Limits::new().with_max_documents(2));
        let decoded =
            crate::parse_documents_with(std::str::from_utf8(&output).expect("utf8"), &options)
                .expect("reparse");
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1]["b"], Value::from(2_u64));
    }
}

#[cfg(test)]
mod builder_tests;
#[cfg(test)]
mod indent_tests;
#[cfg(test)]
mod options_tests;
#[cfg(test)]
mod tagged_block_scalar_tests;
#[cfg(test)]
mod value_block_scalar_tests;
