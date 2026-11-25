//! Options types for parsing, decoding, and emitting YAML.

use crate::emitter::{EmitterOptions, EmitterStyles};
use crate::event::ScalarStyle;
use crate::limits::Limits;

/// Options for default-profile YAML parsers and decoders.
///
/// Start from [`ParseOptions::new`] (the default resource limits) or
/// [`ParseOptions::for_max_input`] (the dense-document count profile), then
/// customize with [`with_limits`](Self::with_limits) or the public fields.
///
/// ```
/// use ayaml::{Limits, ParseOptions, parse_value_with};
///
/// const OPTIONS: ParseOptions =
///     ParseOptions::new().with_limits(Limits::new().with_max_depth(32));
///
/// let value = parse_value_with("a: [1, 2]\n", &OPTIONS)?;
/// assert_eq!(value["a"][1].as_u64(), Some(2));
/// # Ok::<(), ayaml::Error>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParseOptions {
    /// Resource limits applied to the parse.
    pub limits: Limits,
    /// Expand YAML 1.1 plain `<<` merge keys during event, value, and typed reading.
    ///
    /// Defaults to `false` (YAML 1.2.2 keeps `<<` as a literal mapping key). When `true`,
    /// plain `<<` (and `!!merge <<`) keys expand their mapping or sequence-of-mappings operand
    /// while quoted `"<<"` / `'<<'` and `!!str <<` keys remain literal.
    pub merge_keys: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Limits> for ParseOptions {
    fn from(limits: Limits) -> Self {
        Self::new().with_limits(limits)
    }
}

impl ParseOptions {
    /// Parse options with the default resource limits.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: Limits::default_profile(),
            merge_keys: false,
        }
    }

    /// Parse options using [`Limits::for_max_input`], which lifts the four
    /// count limits while keeping input, depth, scalar, tag-expansion,
    /// document, and alias budgets active.
    #[must_use]
    pub const fn for_max_input() -> Self {
        Self {
            limits: Limits::for_max_input(),
            merge_keys: false,
        }
    }

    /// Set the resource limits applied to the parse.
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    /// Enable or disable opt-in YAML 1.1 `<<` merge-key expansion.
    #[must_use]
    pub const fn with_merge_keys(mut self, merge_keys: bool) -> Self {
        self.merge_keys = merge_keys;
        self
    }
}

/// Options for YAML emitters.
///
/// One value carries resource limits, presentation flags, indentation, and
/// optional per-node style overrides. Value and Serde emitters apply every
/// field; event emitters apply [`limits`](Self::limits) (and validate
/// [`indent`](Self::indent)) while preserving each event's own layout, scalar
/// styles, and explicit document markers.
///
/// ```
/// use ayaml::{EmitOptions, to_string_with};
///
/// const FLOW: EmitOptions = EmitOptions::flow().with_explicit_start(true);
///
/// let yaml = to_string_with(&vec!["a", "b"], &FLOW)?;
/// assert_eq!(yaml, "---\n[a, b]\n");
/// # Ok::<(), ayaml::Error>(())
/// ```
#[allow(
    clippy::struct_excessive_bools,
    reason = "the flags are independent emitter options, not a state enum"
)]
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EmitOptions {
    /// Resource limits applied to the emitter call (`max_output_bytes`,
    /// `max_depth`, and `max_anchors` for event writers).
    pub limits: Limits,
    /// Emit the complete document in flow style.
    ///
    /// Applies to value and Serde emitters; event emitters preserve each
    /// event's own collection style.
    pub flow: bool,
    /// Emit an explicit `---` before the document.
    ///
    /// Applies to value and Serde emitters; event emitters preserve explicit
    /// document markers from the event stream.
    pub explicit_start: bool,
    /// Emit an explicit `...` after the document.
    ///
    /// Applies to value and Serde emitters; event emitters preserve explicit
    /// document markers from the event stream.
    pub explicit_end: bool,
    /// Preferred scalar style for strings. Plain falls back to quoting when
    /// the text is syntactically ambiguous; block styles fall back to quoted
    /// output in the value-level emitter.
    ///
    /// Applies to value and Serde emitters; event emitters preserve each
    /// scalar event's style where valid.
    pub scalar_style: Option<ScalarStyle>,
    /// Number of spaces per nested block level, from 2 to 9. Other values are
    /// an emitter error.
    ///
    /// Applies to value and Serde emitters.
    pub indent: usize,
    /// Indent block sequences beneath a mapping key. The default is `false`
    /// (indentless sequences).
    ///
    /// Applies to value and Serde emitters.
    pub indent_sequences: bool,
    /// Optional per-node style overrides addressed by
    /// [`PathSegment`](crate::PathSegment) paths.
    ///
    /// Applies to value and Serde emitters.
    pub styles: Option<EmitterStyles>,
}

impl Default for EmitOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl From<Limits> for EmitOptions {
    fn from(limits: Limits) -> Self {
        Self::new().with_limits(limits)
    }
}

impl From<EmitterOptions> for EmitOptions {
    fn from(options: EmitterOptions) -> Self {
        Self {
            flow: options.flow,
            explicit_start: options.explicit_start,
            explicit_end: options.explicit_end,
            scalar_style: options.scalar_style,
            indent: options.indent,
            indent_sequences: options.indent_sequences,
            ..Self::new()
        }
    }
}

impl From<EmitterStyles> for EmitOptions {
    fn from(styles: EmitterStyles) -> Self {
        Self::new().with_styles(styles)
    }
}

impl EmitOptions {
    /// Emit options with default block layout, default limits, and no per-node
    /// style overrides.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            limits: Limits::default_profile(),
            flow: false,
            explicit_start: false,
            explicit_end: false,
            scalar_style: None,
            indent: 2,
            indent_sequences: false,
            styles: None,
        }
    }

    /// Emit options with flow layout enabled and default limits.
    #[must_use]
    pub const fn flow() -> Self {
        Self::new().with_flow(true)
    }

    /// Set the resource limits applied to the emitter call.
    #[must_use]
    pub const fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
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
    pub const fn with_scalar_style(mut self, value: Option<ScalarStyle>) -> Self {
        self.scalar_style = value;
        self
    }

    /// Set the number of spaces per nested block level, from 2 to 9.
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

    /// Set per-node style overrides addressed by
    /// [`PathSegment`](crate::PathSegment) paths.
    #[must_use]
    pub fn with_styles(mut self, styles: EmitterStyles) -> Self {
        self.styles = Some(styles);
        self
    }

    /// Extract the presentation fields shared with the value writer.
    pub(crate) const fn emitter_options(&self) -> EmitterOptions {
        EmitterOptions::new()
            .with_flow(self.flow)
            .with_explicit_start(self.explicit_start)
            .with_explicit_end(self.explicit_end)
            .with_scalar_style(self.scalar_style)
            .with_indent(self.indent)
            .with_indent_sequences(self.indent_sequences)
    }
}

#[cfg(test)]
mod tests {
    use super::{EmitOptions, ParseOptions};
    use crate::emitter::{EmitterOptions, EmitterStyles, NodeStyle, PathSegment};
    use crate::event::ScalarStyle;
    use crate::limits::Limits;

    const PARSE_DEFAULT: ParseOptions = ParseOptions::new();
    const PARSE_MAX_INPUT: ParseOptions = ParseOptions::for_max_input();
    const PARSE_CUSTOM: ParseOptions =
        ParseOptions::new().with_limits(Limits::new().with_max_depth(16));

    const EMIT_DEFAULT: EmitOptions = EmitOptions::new();
    const EMIT_FLOW: EmitOptions = EmitOptions::flow();
    const EMIT_CUSTOM: EmitOptions = EmitOptions::new()
        .with_limits(Limits::new().with_max_output_bytes(1024))
        .with_flow(true)
        .with_explicit_start(true)
        .with_explicit_end(true)
        .with_scalar_style(Some(ScalarStyle::DoubleQuoted))
        .with_indent(4)
        .with_indent_sequences(true);

    #[test]
    fn const_presets_and_defaults_match() {
        let flow = EMIT_FLOW;
        let custom = EMIT_CUSTOM;
        assert_eq!(PARSE_DEFAULT, ParseOptions::default());
        assert_eq!(PARSE_MAX_INPUT.limits, Limits::for_max_input());
        assert_eq!(PARSE_CUSTOM.limits.max_depth, 16);
        assert_eq!(
            ParseOptions::from(Limits::new().with_max_depth(8))
                .limits
                .max_depth,
            8
        );

        assert_eq!(EMIT_DEFAULT, EmitOptions::default());
        assert!(flow.flow);
        assert_eq!(flow.indent, 2);
        assert_eq!(custom.limits.max_output_bytes, 1024);
        assert!(custom.flow);
        assert!(custom.explicit_start);
        assert!(custom.explicit_end);
        assert_eq!(custom.scalar_style, Some(ScalarStyle::DoubleQuoted));
        assert_eq!(custom.indent, 4);
        assert!(custom.indent_sequences);
        assert!(custom.styles.is_none());
    }

    #[test]
    fn emit_options_conversions_and_styles() {
        let emitter_opts = EmitterOptions::new()
            .with_flow(true)
            .with_explicit_start(true)
            .with_explicit_end(true)
            .with_scalar_style(Some(ScalarStyle::SingleQuoted))
            .with_indent(3)
            .with_indent_sequences(true);
        let from_emitter = EmitOptions::from(emitter_opts);
        assert_eq!(from_emitter.emitter_options(), emitter_opts);
        assert_eq!(from_emitter.limits, Limits::default());

        let mut styles = EmitterStyles::new();
        styles.set(
            vec![PathSegment::key("a")],
            NodeStyle::new().with_flow(Some(true)),
        );
        let from_styles = EmitOptions::from(styles.clone());
        assert_eq!(from_styles.styles, Some(styles));
        assert_eq!(
            EmitOptions::from(Limits::new().with_max_output_bytes(42))
                .limits
                .max_output_bytes,
            42
        );
    }
}
