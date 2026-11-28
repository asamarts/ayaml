//! Structure-aware `arbitrary::Arbitrary` generators for nested YAML documents,
//! values, event streams, and canary error inputs (AY-132).

use std::borrow::Cow;
use std::fmt::Write as _;

use arbitrary::{Arbitrary, Result as ArbResult, Unstructured};
use ayaml::{
    Anchor, CollectionStyle, Event, Mapping, Number, ScalarStyle, Span, Tag, TaggedValue, Value,
};

const MAX_AST_DEPTH: usize = 4;
const MAX_BRANCH_LEN: usize = 5;

const KEYS: &[&str] = &[
    "a",
    "b",
    "name",
    "count",
    "nested",
    "enabled",
    "items",
    "<<",
    "\"quoted\"",
    "'single'",
    "null",
    "true",
    "0123",
    "k-1",
    "é",
];

const TAGS: &[&str] = &[
    "!local",
    "!cfg/item",
    "!!str",
    "!<tag:example.com,2026:node>",
    "!e!custom",
];

/// A structured YAML scalar AST node.
#[derive(Clone, Debug)]
pub enum FuzzScalar {
    /// Null spelling (`null`, `~`, or empty).
    Null(u8),
    /// Boolean scalar.
    Bool(bool),
    /// Signed integer scalar.
    Int(i64),
    /// Finite floating-point scalar.
    Float(f64),
    /// Plain safe identifier scalar.
    Plain(String),
    /// Single-quoted string scalar.
    SingleQuoted(String),
    /// Double-quoted string scalar (may include escapes and Unicode).
    DoubleQuoted(String),
    /// Literal (`|`) or folded (`>`) block scalar.
    Block {
        /// True for folded `>`, false for literal `|`.
        folded: bool,
        /// Chomping indicator: 0 = clip, 1 = strip `-`, 2 = keep `+`.
        chomp: u8,
        /// Lines of the block scalar.
        lines: Vec<String>,
    },
}

/// A structured YAML node that renders to valid (or near-valid) YAML.
#[derive(Clone, Debug)]
pub enum FuzzNode {
    /// Scalar leaf.
    Scalar(FuzzScalar),
    /// Sequence rendered in block or flow style.
    Sequence {
        /// True for flow `[...]`, false for block `- ...`.
        flow: bool,
        /// Optional anchor index.
        anchor: Option<u8>,
        /// Optional tag index.
        tag: Option<u8>,
        /// Sequence items.
        items: Vec<Self>,
    },
    /// Mapping rendered in block or flow style.
    Mapping {
        /// True for flow `{...}`, false for block `k: v`.
        flow: bool,
        /// Optional anchor index.
        anchor: Option<u8>,
        /// Optional tag index.
        tag: Option<u8>,
        /// Key-value entries (keys are deduplicated on render unless `allow_dup` is set).
        entries: Vec<(String, Self)>,
        /// Whether to include a `<<: *a0` merge key when an anchor exists.
        include_merge: bool,
    },
    /// Acyclic reference to an earlier anchor (falls back to scalar if none defined yet).
    Alias(u8),
}

/// Bitflags controlling document framing in [`FuzzDoc`].
#[derive(Clone, Copy, Debug)]
pub struct DocFlags(u8);

impl DocFlags {
    const BOM: u8 = 1 << 0;
    const CRLF: u8 = 1 << 1;
    const DIRECTIVES: u8 = 1 << 2;
    const EXPLICIT_MARKERS: u8 = 1 << 3;
    const COMMENT: u8 = 1 << 4;

    const fn has(self, bit: u8) -> bool {
        (self.0 & bit) != 0
    }

    const fn set(&mut self, bit: u8, enabled: bool) {
        if enabled {
            self.0 |= bit;
        } else {
            self.0 &= !bit;
        }
    }
}

/// A single structured YAML document with optional directives, BOM, CRLF, and mutation.
#[derive(Clone, Debug)]
pub struct FuzzDoc {
    /// Framing flags (BOM, CRLF, directives, explicit markers, comment).
    pub flags: DocFlags,
    /// Root YAML node.
    pub root: FuzzNode,
    /// Optional mutation kind to create near-valid hostile inputs.
    pub mutation: Option<u8>,
}

/// A multi-document structured YAML stream (1..=3 documents).
#[derive(Clone, Debug)]
pub struct FuzzStream {
    /// Documents in the stream.
    pub docs: Vec<FuzzDoc>,
}

/// An arbitrary `ayaml::Value` tree for direct emitter and `apply_merge_keys` fuzzing.
#[derive(Clone, Debug)]
pub struct FuzzValue {
    /// The generated `ayaml::Value`.
    pub value: Value,
}

/// An arbitrary event stream (either balanced or deliberately scrambled) for `to_string_events`
/// and `to_writer_event_stream` fuzzing.
#[derive(Clone, Debug)]
pub struct FuzzEventStream {
    /// Generated events.
    pub events: Vec<Event<'static>>,
}

/// Total number of failure scenarios dispatched by `run_masked_errors`.
pub const MASKED_ERROR_SCENARIOS: u8 = 27;

/// A secret canary token paired with a failure-scenario selector for `Error::masked()` fuzzing.
#[derive(Clone, Debug)]
pub struct FuzzCanary {
    /// Secret token that must never appear in masked output.
    pub secret: String,
    /// Scenario selector in `0..MASKED_ERROR_SCENARIOS`.
    pub scenario: u8,
}

fn arb_small_text(u: &mut Unstructured<'_>) -> ArbResult<String> {
    let kind: u8 = u.int_in_range(0..=5)?;
    Ok(match kind {
        0 => "alpha".to_owned(),
        1 => "hello world".to_owned(),
        2 => "café 🚀".to_owned(),
        3 => "0123".to_owned(),
        4 => "line1\nline2".to_owned(),
        _ => {
            let len: usize = u.int_in_range(0..=12)?;
            let mut out = String::with_capacity(len);
            for _ in 0..len {
                let ch = match u.int_in_range(0_u8..=10)? {
                    0..=5 => char::from(b'a' + u.int_in_range(0_u8..=25)?),
                    6 => ' ',
                    7 => ':',
                    8 => '"',
                    9 => '\\',
                    _ => 'é',
                };
                out.push(ch);
            }
            out
        }
    })
}

fn arb_key(u: &mut Unstructured<'_>) -> ArbResult<String> {
    Ok((*u.choose(KEYS)?).to_owned())
}

fn arb_node(u: &mut Unstructured<'_>, depth: usize) -> ArbResult<FuzzNode> {
    if depth >= MAX_AST_DEPTH || u.is_empty() {
        return Ok(FuzzNode::Scalar(FuzzScalar::arbitrary(u)?));
    }
    match u.int_in_range(0_u8..=5)? {
        0 | 1 => Ok(FuzzNode::Scalar(FuzzScalar::arbitrary(u)?)),
        2 => Ok(FuzzNode::Alias(u.int_in_range(0_u8..=3)?)),
        3 => {
            let flow = bool::arbitrary(u)?;
            let anchor = bool::arbitrary(u)?
                .then(|| u.int_in_range(0_u8..=3))
                .transpose()?;
            let tag = bool::arbitrary(u)?
                .then(|| u.int_in_range(0_u8..=u8::try_from(TAGS.len() - 1).unwrap_or(0)))
                .transpose()?;
            let len: usize = u.int_in_range(0..=MAX_BRANCH_LEN)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(arb_node(u, depth + 1)?);
            }
            Ok(FuzzNode::Sequence {
                flow,
                anchor,
                tag,
                items,
            })
        }
        _ => {
            let flow = bool::arbitrary(u)?;
            let anchor = bool::arbitrary(u)?
                .then(|| u.int_in_range(0_u8..=3))
                .transpose()?;
            let tag = bool::arbitrary(u)?
                .then(|| u.int_in_range(0_u8..=u8::try_from(TAGS.len() - 1).unwrap_or(0)))
                .transpose()?;
            let include_merge = bool::arbitrary(u)?;
            let len: usize = u.int_in_range(0..=MAX_BRANCH_LEN)?;
            let mut entries = Vec::with_capacity(len);
            for _ in 0..len {
                entries.push((arb_key(u)?, arb_node(u, depth + 1)?));
            }
            Ok(FuzzNode::Mapping {
                flow,
                anchor,
                tag,
                entries,
                include_merge,
            })
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzScalar {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        match u.int_in_range(0_u8..=7)? {
            0 => Ok(Self::Null(u.int_in_range(0_u8..=2)?)),
            1 => Ok(Self::Bool(bool::arbitrary(u)?)),
            2 => Ok(Self::Int(i64::arbitrary(u)?)),
            3 => {
                let raw = i32::arbitrary(u)?;
                Ok(Self::Float(f64::from(raw) / 100.0))
            }
            4 => Ok(Self::Plain(format!("val_{}", u.int_in_range(0_u16..=999)?))),
            5 => Ok(Self::SingleQuoted(arb_small_text(u)?)),
            6 => Ok(Self::DoubleQuoted(arb_small_text(u)?)),
            _ => {
                let folded = bool::arbitrary(u)?;
                let chomp = u.int_in_range(0_u8..=2)?;
                let count: usize = u.int_in_range(1..=3)?;
                let mut lines = Vec::with_capacity(count);
                for _ in 0..count {
                    lines.push(format!("line {}", u.int_in_range(0_u8..=99)?));
                }
                Ok(Self::Block {
                    folded,
                    chomp,
                    lines,
                })
            }
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzDoc {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let mut flags = DocFlags(0);
        flags.set(DocFlags::BOM, u.ratio(1, 6)?);
        flags.set(DocFlags::CRLF, u.ratio(1, 4)?);
        let with_directives = u.ratio(1, 3)?;
        flags.set(DocFlags::DIRECTIVES, with_directives);
        flags.set(
            DocFlags::EXPLICIT_MARKERS,
            with_directives || bool::arbitrary(u)?,
        );
        flags.set(DocFlags::COMMENT, bool::arbitrary(u)?);
        let root = arb_node(u, 0)?;
        let mutation = if u.ratio(1, 4)? {
            Some(u.int_in_range(0_u8..=9)?)
        } else {
            None
        };
        Ok(Self {
            flags,
            root,
            mutation,
        })
    }
}

impl<'a> Arbitrary<'a> for FuzzStream {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let count: usize = u.int_in_range(1..=3)?;
        let mut docs = Vec::with_capacity(count);
        for _ in 0..count {
            docs.push(FuzzDoc::arbitrary(u)?);
        }
        Ok(Self { docs })
    }
}

fn arb_value(u: &mut Unstructured<'_>, depth: usize) -> ArbResult<Value> {
    if depth >= MAX_AST_DEPTH || u.is_empty() {
        return Ok(match u.int_in_range(0_u8..=5)? {
            0 => Value::Null,
            1 => Value::Bool(bool::arbitrary(u)?),
            2 => Value::Number(Number::from_i64(i64::arbitrary(u)?)),
            3 => Value::Number(Number::from_u64(u64::arbitrary(u)?)),
            4 => Value::Number(Number::from_f64(f64::from(i16::arbitrary(u)?) / 10.0)),
            _ => Value::String(arb_small_text(u)?),
        });
    }
    match u.int_in_range(0_u8..=7)? {
        0 => Ok(Value::Null),
        1 => Ok(Value::Bool(bool::arbitrary(u)?)),
        2 => Ok(Value::Number(Number::from_i64(i64::arbitrary(u)?))),
        3 => Ok(Value::String(arb_small_text(u)?)),
        4 => {
            let len: usize = u.int_in_range(0..=MAX_BRANCH_LEN)?;
            let mut items = Vec::with_capacity(len);
            for _ in 0..len {
                items.push(arb_value(u, depth + 1)?);
            }
            Ok(Value::Sequence(items))
        }
        5 | 6 => {
            let len: usize = u.int_in_range(0..=MAX_BRANCH_LEN)?;
            let mut mapping = Mapping::new();
            for i in 0..len {
                let key = if i == 0 && u.ratio(1, 2)? {
                    Value::from("<<")
                } else {
                    Value::from(arb_key(u)?)
                };
                mapping.insert(key, arb_value(u, depth + 1)?);
            }
            Ok(Value::Mapping(mapping))
        }
        _ => {
            let tag = match u.int_in_range(0_u8..=2)? {
                0 => Tag::local("custom"),
                1 => Tag::local("merge-node"),
                _ => Tag::new("tag:example.com,2026:", "tagged"),
            };
            Ok(Value::from(TaggedValue::new(tag, arb_value(u, depth + 1)?)))
        }
    }
}

impl<'a> Arbitrary<'a> for FuzzValue {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        Ok(Self {
            value: arb_value(u, 0)?,
        })
    }
}

fn arb_scalar_style(u: &mut Unstructured<'_>) -> ArbResult<ScalarStyle> {
    Ok(match u.int_in_range(0_u8..=4)? {
        0 => ScalarStyle::Plain,
        1 => ScalarStyle::SingleQuoted,
        2 => ScalarStyle::DoubleQuoted,
        3 => ScalarStyle::Literal,
        _ => ScalarStyle::Folded,
    })
}

const ANCHOR_NAMES: [&str; 5] = ["a0", "a1", "a2", "a3", "a4"];

fn arb_anchor(u: &mut Unstructured<'_>) -> ArbResult<Option<Anchor<'static>>> {
    if bool::arbitrary(u)? {
        let id: usize = u.int_in_range(0..=3)?;
        Ok(Some(Anchor::new(ANCHOR_NAMES[id])))
    } else {
        Ok(None)
    }
}

fn arb_tag(u: &mut Unstructured<'_>) -> ArbResult<Option<Tag>> {
    Ok(match u.int_in_range(0_u8..=5)? {
        0 => Some(Tag::local("t")),
        1 => Some(Tag::core("str")),
        2 => Some(Tag::new("tag:example.com,2026:", "x")),
        _ => None,
    })
}

fn push_balanced_node_events(
    u: &mut Unstructured<'_>,
    out: &mut Vec<Event<'static>>,
    anchors: &mut Vec<&'static str>,
    depth: usize,
) -> ArbResult<()> {
    if depth >= MAX_AST_DEPTH || u.is_empty() {
        let text = arb_small_text(u)?;
        let anchor = arb_anchor(u)?;
        if let Some(ref a) = anchor
            && let Some(&name) = ANCHOR_NAMES.iter().find(|&&n| n == a.as_str())
        {
            anchors.push(name);
        }
        out.push(Event::Scalar {
            value: Cow::Owned(text.clone()),
            raw: Cow::Owned(text),
            style: arb_scalar_style(u)?,
            tag: arb_tag(u)?,
            anchor,
            span: Span::default(),
        });
        return Ok(());
    }
    match u.int_in_range(0_u8..=4)? {
        0 => {
            let text = arb_small_text(u)?;
            let anchor = arb_anchor(u)?;
            if let Some(ref a) = anchor
                && let Some(&name) = ANCHOR_NAMES.iter().find(|&&n| n == a.as_str())
            {
                anchors.push(name);
            }
            out.push(Event::Scalar {
                value: Cow::Owned(text.clone()),
                raw: Cow::Owned(text),
                style: arb_scalar_style(u)?,
                tag: arb_tag(u)?,
                anchor,
                span: Span::default(),
            });
        }
        1 if !anchors.is_empty() => {
            let chosen = *u.choose(anchors)?;
            out.push(Event::Alias {
                anchor: Anchor::new(chosen),
                span: Span::default(),
            });
        }
        2 => {
            let style = if bool::arbitrary(u)? {
                CollectionStyle::Flow
            } else {
                CollectionStyle::Block
            };
            let anchor = arb_anchor(u)?;
            if let Some(ref a) = anchor
                && let Some(&name) = ANCHOR_NAMES.iter().find(|&&n| n == a.as_str())
            {
                anchors.push(name);
            }
            out.push(Event::SequenceStart {
                tag: arb_tag(u)?,
                anchor,
                style,
                span: Span::default(),
            });
            let len: usize = u.int_in_range(0..=4)?;
            for _ in 0..len {
                push_balanced_node_events(u, out, anchors, depth + 1)?;
            }
            out.push(Event::SequenceEnd {
                span: Span::default(),
            });
        }
        _ => {
            let style = if bool::arbitrary(u)? {
                CollectionStyle::Flow
            } else {
                CollectionStyle::Block
            };
            let anchor = arb_anchor(u)?;
            if let Some(ref a) = anchor
                && let Some(&name) = ANCHOR_NAMES.iter().find(|&&n| n == a.as_str())
            {
                anchors.push(name);
            }
            out.push(Event::MappingStart {
                tag: arb_tag(u)?,
                anchor,
                style,
                span: Span::default(),
            });
            let len: usize = u.int_in_range(0..=4)?;
            for i in 0..len {
                let key = format!("k{i}");
                out.push(Event::Scalar {
                    value: Cow::Owned(key.clone()),
                    raw: Cow::Owned(key),
                    style: ScalarStyle::Plain,
                    tag: None,
                    anchor: None,
                    span: Span::default(),
                });
                push_balanced_node_events(u, out, anchors, depth + 1)?;
            }
            out.push(Event::MappingEnd {
                span: Span::default(),
            });
        }
    }
    Ok(())
}

impl<'a> Arbitrary<'a> for FuzzEventStream {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let mut events = Vec::new();
        let scrambled = u.ratio(1, 3)?;
        if scrambled {
            let count: usize = u.int_in_range(0..=16)?;
            for _ in 0..count {
                let ev = match u.int_in_range(0_u8..=9)? {
                    0 => Event::StreamStart {
                        span: Span::default(),
                    },
                    1 => Event::StreamEnd {
                        span: Span::default(),
                    },
                    2 => Event::DocumentStart {
                        explicit: bool::arbitrary(u)?,
                        span: Span::default(),
                    },
                    3 => Event::DocumentEnd {
                        explicit: bool::arbitrary(u)?,
                        span: Span::default(),
                    },
                    4 => Event::SequenceStart {
                        tag: arb_tag(u)?,
                        anchor: arb_anchor(u)?,
                        style: if bool::arbitrary(u)? {
                            CollectionStyle::Flow
                        } else {
                            CollectionStyle::Block
                        },
                        span: Span::default(),
                    },
                    5 => Event::SequenceEnd {
                        span: Span::default(),
                    },
                    6 => Event::MappingStart {
                        tag: arb_tag(u)?,
                        anchor: arb_anchor(u)?,
                        style: if bool::arbitrary(u)? {
                            CollectionStyle::Flow
                        } else {
                            CollectionStyle::Block
                        },
                        span: Span::default(),
                    },
                    7 => Event::MappingEnd {
                        span: Span::default(),
                    },
                    8 => {
                        let text = arb_small_text(u)?;
                        Event::Scalar {
                            value: Cow::Owned(text.clone()),
                            raw: Cow::Owned(text),
                            style: arb_scalar_style(u)?,
                            tag: arb_tag(u)?,
                            anchor: arb_anchor(u)?,
                            span: Span::default(),
                        }
                    }
                    _ => Event::Alias {
                        anchor: Anchor::new(ANCHOR_NAMES[u.int_in_range(0..=4)?]),
                        span: Span::default(),
                    },
                };
                events.push(ev);
            }
            return Ok(Self { events });
        }

        events.push(Event::StreamStart {
            span: Span::default(),
        });
        let doc_count: usize = u.int_in_range(1..=2)?;
        for _ in 0..doc_count {
            let mut anchors = Vec::new();
            if u.ratio(1, 4)? {
                events.push(Event::Directive {
                    name: Cow::Borrowed("YAML"),
                    value: Cow::Borrowed("1.2"),
                    span: Span::default(),
                });
            }
            events.push(Event::DocumentStart {
                explicit: true,
                span: Span::default(),
            });
            push_balanced_node_events(u, &mut events, &mut anchors, 0)?;
            events.push(Event::DocumentEnd {
                explicit: bool::arbitrary(u)?,
                span: Span::default(),
            });
        }
        events.push(Event::StreamEnd {
            span: Span::default(),
        });
        Ok(Self { events })
    }
}

impl<'a> Arbitrary<'a> for FuzzCanary {
    fn arbitrary(u: &mut Unstructured<'a>) -> ArbResult<Self> {
        let len: usize = u.int_in_range(8..=14)?;
        let mut secret = String::from("CANARY_");
        for _ in 0..len {
            let b = u.int_in_range(0_u8..=35)?;
            let ch = if b < 10 {
                char::from(b'0' + b)
            } else {
                char::from(b'A' + (b - 10))
            };
            secret.push(ch);
        }
        match u.int_in_range(0_u8..=2)? {
            1 => secret.push_str("_é"),
            2 => secret.push_str("_🚀"),
            _ => {}
        }
        let max_scenario = MASKED_ERROR_SCENARIOS - 1;
        let scenario = u.int_in_range(0_u8..=max_scenario)?;
        Ok(Self { secret, scenario })
    }
}

// ---------------------------------------------------------------------------
// YAML renderer for FuzzDoc / FuzzStream
// ---------------------------------------------------------------------------

struct RenderCtx {
    anchors: Vec<String>,
    allow_e_handle: bool,
}

impl RenderCtx {
    fn format_props(&self, anchor: Option<u8>, tag: Option<u8>) -> (String, Option<String>) {
        let mut out = String::new();
        let anchor_name = anchor.map(|idx| {
            let name = format!("a{idx}");
            let _ = write!(out, "&{name}");
            name
        });
        if let Some(tag_idx) = tag {
            let raw = TAGS[usize::from(tag_idx) % TAGS.len()];
            let chosen = if raw.starts_with("!e!") && !self.allow_e_handle {
                "!local"
            } else {
                raw
            };
            if !out.is_empty() {
                out.push(' ');
            }
            out.push_str(chosen);
        }
        (out, anchor_name)
    }

    fn register_anchor(&mut self, name: Option<String>) {
        if let Some(name) = name
            && !self.anchors.contains(&name)
        {
            self.anchors.push(name);
        }
    }

    fn render_scalar_inline(scalar: &FuzzScalar) -> String {
        match scalar {
            FuzzScalar::Null(0) => "null".to_owned(),
            FuzzScalar::Null(1) => "~".to_owned(),
            FuzzScalar::Null(_) => "NULL".to_owned(),
            FuzzScalar::Bool(b) => b.to_string(),
            FuzzScalar::Int(i) => i.to_string(),
            FuzzScalar::Float(f) => format!("{f:.2}"),
            FuzzScalar::Plain(s) => s.clone(),
            FuzzScalar::SingleQuoted(s) => {
                let clean = s.replace(['\n', '\r'], " ").replace('\'', "''");
                format!("'{clean}'")
            }
            FuzzScalar::DoubleQuoted(s) => {
                let mut out = String::from("\"");
                for ch in s.chars() {
                    match ch {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\r' => out.push_str("\\r"),
                        '\t' => out.push_str("\\t"),
                        other => out.push(other),
                    }
                }
                out.push('"');
                out
            }
            FuzzScalar::Block { lines, .. } => {
                let joined = lines.join("\\n");
                format!("\"{joined}\"")
            }
        }
    }

    fn render_flow(&mut self, node: &FuzzNode) -> String {
        match node {
            FuzzNode::Scalar(s) => Self::render_scalar_inline(s),
            FuzzNode::Alias(idx) => {
                if self.anchors.is_empty() {
                    "null".to_owned()
                } else {
                    let chosen = &self.anchors[usize::from(*idx) % self.anchors.len()];
                    format!("*{chosen}")
                }
            }
            FuzzNode::Sequence {
                anchor, tag, items, ..
            } => {
                let (props, anchor_name) = self.format_props(*anchor, *tag);
                let inner: Vec<String> = items.iter().map(|it| self.render_flow(it)).collect();
                self.register_anchor(anchor_name);
                if props.is_empty() {
                    format!("[{}]", inner.join(", "))
                } else {
                    format!("{props} [{}]", inner.join(", "))
                }
            }
            FuzzNode::Mapping {
                anchor,
                tag,
                entries,
                include_merge,
                ..
            } => {
                let (props, anchor_name) = self.format_props(*anchor, *tag);
                let mut seen = Vec::new();
                let mut pairs = Vec::new();
                if *include_merge && let Some(first) = self.anchors.first().cloned() {
                    seen.push("<<".to_owned());
                    pairs.push(format!("<<: *{first}"));
                }
                for (k, v) in entries {
                    if seen.contains(k) {
                        continue;
                    }
                    seen.push(k.clone());
                    let rendered_val = self.render_flow(v);
                    pairs.push(format!("{k}: {rendered_val}"));
                }
                self.register_anchor(anchor_name);
                if props.is_empty() {
                    format!("{{{}}}", pairs.join(", "))
                } else {
                    format!("{props} {{{}}}", pairs.join(", "))
                }
            }
        }
    }

    fn render_block(&mut self, node: &FuzzNode, indent: usize, out: &mut String) {
        let pad = " ".repeat(indent);
        match node {
            FuzzNode::Scalar(FuzzScalar::Block {
                folded,
                chomp,
                lines,
            }) => {
                let indicator = if *folded { '>' } else { '|' };
                let chomp_str = match chomp {
                    1 => "-",
                    2 => "+",
                    _ => "",
                };
                let _ = writeln!(out, "{indicator}{chomp_str}");
                let body_pad = " ".repeat(indent + 2);
                for line in lines {
                    let _ = writeln!(out, "{body_pad}{line}");
                }
            }
            FuzzNode::Scalar(s) => {
                let _ = writeln!(out, "{}", Self::render_scalar_inline(s));
            }
            FuzzNode::Alias(idx) => {
                if self.anchors.is_empty() {
                    let _ = writeln!(out, "null");
                } else {
                    let chosen = &self.anchors[usize::from(*idx) % self.anchors.len()];
                    let _ = writeln!(out, "*{chosen}");
                }
            }
            FuzzNode::Sequence { flow: true, .. } | FuzzNode::Mapping { flow: true, .. } => {
                let rendered = self.render_flow(node);
                let _ = writeln!(out, "{rendered}");
            }
            FuzzNode::Sequence {
                flow: false,
                anchor,
                tag,
                items,
            } => {
                let (props, anchor_name) = self.format_props(*anchor, *tag);
                if items.is_empty() {
                    if props.is_empty() {
                        let _ = writeln!(out, "[]");
                    } else {
                        let _ = writeln!(out, "{props} []");
                    }
                    self.register_anchor(anchor_name);
                    return;
                }
                if !props.is_empty() {
                    let _ = writeln!(out, "{props}");
                }
                for item in items {
                    match item {
                        FuzzNode::Sequence { flow: false, .. }
                        | FuzzNode::Mapping { flow: false, .. } => {
                            let _ = writeln!(out, "{pad}-");
                            self.render_block(item, indent + 2, out);
                        }
                        _ => {
                            let _ = write!(out, "{pad}- ");
                            self.render_block(item, indent + 2, out);
                        }
                    }
                }
                self.register_anchor(anchor_name);
            }
            FuzzNode::Mapping {
                flow: false,
                anchor,
                tag,
                entries,
                include_merge,
            } => {
                let (props, anchor_name) = self.format_props(*anchor, *tag);
                let has_merge = *include_merge && !self.anchors.is_empty();
                if entries.is_empty() && !has_merge {
                    if props.is_empty() {
                        let _ = writeln!(out, "{{}}");
                    } else {
                        let _ = writeln!(out, "{props} {{}}");
                    }
                    self.register_anchor(anchor_name);
                    return;
                }
                if !props.is_empty() {
                    let _ = writeln!(out, "{props}");
                }
                let mut seen = Vec::new();
                if *include_merge && let Some(first) = self.anchors.first().cloned() {
                    seen.push("<<".to_owned());
                    let _ = writeln!(out, "{pad}<<: *{first}");
                }
                for (k, v) in entries {
                    if seen.contains(k) {
                        continue;
                    }
                    seen.push(k.clone());
                    match v {
                        FuzzNode::Sequence { flow: false, .. }
                        | FuzzNode::Mapping { flow: false, .. } => {
                            let _ = writeln!(out, "{pad}{k}:");
                            self.render_block(v, indent + 2, out);
                        }
                        _ => {
                            let _ = write!(out, "{pad}{k}: ");
                            self.render_block(v, indent + 2, out);
                        }
                    }
                }
                self.register_anchor(anchor_name);
            }
        }
    }
}

impl FuzzDoc {
    /// Render this structured YAML document into a string (with optional hostile mutation).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        if self.flags.has(DocFlags::BOM) {
            out.push('\u{feff}');
        }
        let with_directives = self.flags.has(DocFlags::DIRECTIVES);
        let explicit_markers = self.flags.has(DocFlags::EXPLICIT_MARKERS);
        if with_directives {
            out.push_str("%YAML 1.2\n%TAG !e! tag:example.com,2026:\n");
        }
        if explicit_markers || with_directives {
            out.push_str("---\n");
        }
        let mut ctx = RenderCtx {
            anchors: Vec::new(),
            allow_e_handle: with_directives,
        };
        ctx.render_block(&self.root, 0, &mut out);
        if self.flags.has(DocFlags::COMMENT) {
            out.push_str("# trailing comment\n");
        }
        if explicit_markers {
            out.push_str("...\n");
        }
        if let Some(kind) = self.mutation {
            out = apply_mutation(&out, kind);
        }
        if self.flags.has(DocFlags::CRLF) {
            out = out.replace('\n', "\r\n");
        }
        out
    }
}

impl FuzzStream {
    /// Render this multi-document YAML stream into a string.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, doc) in self.docs.iter().enumerate() {
            let mut copy = doc.clone();
            let keep_bom = i == 0 && doc.flags.has(DocFlags::BOM);
            copy.flags.set(DocFlags::BOM, keep_bom);
            copy.flags.set(DocFlags::EXPLICIT_MARKERS, true);
            copy.flags.set(DocFlags::CRLF, false);
            out.push_str(&copy.render());
        }
        out
    }
}

fn apply_mutation(input: &str, kind: u8) -> String {
    match kind % 10 {
        0 => format!("{input}\n[unterminated"),
        1 => format!("a: *unknown_anchor\n{input}"),
        2 => format!("dup: 1\ndup: 2\n{input}"),
        3 => format!("!undef!tag 1\n{input}"),
        4 => format!("{input}\n\"unterminated"),
        5 => format!("{input}\n\tbad_tab: 1\n"),
        6 => format!("{input}\n{{a: [1, 2}}"),
        7 => format!("a: &cyc\n  self: *cyc\n{input}"),
        8 => format!("{input}\n---\nextra: 1\n"),
        _ => {
            // Truncate cleanly at a UTF-8 character boundary.
            let target = input.len() / 2;
            let mut cut = target;
            while cut > 0 && !input.is_char_boundary(cut) {
                cut -= 1;
            }
            input[..cut].to_owned()
        }
    }
}
