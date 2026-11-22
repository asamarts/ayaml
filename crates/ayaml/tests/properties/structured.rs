//! Structured YAML 1.2.2 grammar generator for property tests (AY-036).

use proptest::prelude::*;

#[derive(Clone, Copy, Debug)]
enum BlockChomp {
    Clip,
    Strip,
    Keep,
}

#[derive(Clone, Copy, Debug)]
enum StructuredTag {
    Local(&'static str),
    CoreStr,
    Verbatim,
    Handle,
}

#[derive(Clone, Debug)]
enum StructuredScalar {
    Plain(&'static str),
    SingleQuoted(String),
    DoubleQuoted(String),
    Null(&'static str),
    EmptyBlockValue,
    Bool(&'static str),
    Int(&'static str),
    Float(&'static str),
}

#[derive(Clone, Debug)]
enum StructuredKey {
    Plain(&'static str),
    Quoted(String),
    Explicit(String),
    FlowSeq(Vec<i32>),
}

#[derive(Clone, Debug)]
enum StructuredNode {
    Scalar(StructuredScalar),
    BlockScalar {
        folded: bool,
        chomp: BlockChomp,
        explicit_indent: bool,
        lines: Vec<String>,
    },
    Sequence {
        flow: bool,
        items: Vec<StructuredItem>,
    },
    Mapping {
        flow: bool,
        entries: Vec<(StructuredKey, StructuredItem)>,
    },
    Alias(usize),
}

#[derive(Clone, Debug)]
struct StructuredItem {
    anchor: bool,
    tag: Option<StructuredTag>,
    node: StructuredNode,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "generator options independently toggle orthogonal YAML document syntax features"
)]
#[derive(Clone, Debug)]
struct StructuredDoc {
    with_bom: bool,
    crlf: bool,
    explicit_start: bool,
    explicit_end: bool,
    yaml_directive: bool,
    tag_directive: bool,
    leading_comment: bool,
    root: StructuredItem,
}

fn safe_single_quoted_text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("it's quoted".to_owned()),
        Just("true".to_owned()),
        Just("0123".to_owned()),
        Just("a: b # not comment".to_owned()),
        Just("  spaced  ".to_owned()),
        Just("café 日本語".to_owned()),
        "[A-Za-z0-9 '_:,.-]{0,16}",
    ]
}

fn safe_double_quoted_text() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("line1\nline2".to_owned()),
        Just("tab\there".to_owned()),
        Just("quote \" and slash \\".to_owned()),
        Just("null".to_owned()),
        Just("emoji 🚀".to_owned()),
        "[A-Za-z0-9 _:,./\"\\\\\t\n-]{0,16}",
    ]
}

fn block_scalar_line() -> impl Strategy<Value = String> {
    prop_oneof![
        Just(String::new()),
        Just("plain line".to_owned()),
        Just("  indented line".to_owned()),
        Just("# not a comment".to_owned()),
        Just("key: value".to_owned()),
        Just("- list looking".to_owned()),
        Just("--- marker text".to_owned()),
        Just("unicode naïve".to_owned()),
        "[A-Za-z0-9 _:,.-]{1,14}",
    ]
}

fn structured_scalar() -> impl Strategy<Value = StructuredScalar> {
    prop_oneof![
        3 => prop::sample::select(vec![
            "alpha",
            "beta_1",
            "yes",
            "no",
            "on",
            "off",
            "0123",
            "v1.2.3",
            "http://example.com/a",
            "café",
        ])
        .prop_map(StructuredScalar::Plain),
        2 => safe_single_quoted_text().prop_map(StructuredScalar::SingleQuoted),
        2 => safe_double_quoted_text().prop_map(StructuredScalar::DoubleQuoted),
        1 => prop::sample::select(vec!["null", "~", "Null"]).prop_map(StructuredScalar::Null),
        1 => Just(StructuredScalar::EmptyBlockValue),
        1 => prop::sample::select(vec!["true", "false"]).prop_map(StructuredScalar::Bool),
        1 => prop::sample::select(vec![
            "0",
            "42",
            "-7",
            "0x1f",
            "0o17",
            "9223372036854775807",
            "-9223372036854775808",
            "18446744073709551615",
        ])
        .prop_map(StructuredScalar::Int),
        1 => prop::sample::select(vec!["1.5", "-0.25", "1e3", ".inf", "-.inf", ".nan"])
            .prop_map(StructuredScalar::Float),
    ]
}

fn structured_tag() -> impl Strategy<Value = Option<StructuredTag>> {
    prop_oneof![
        5 => Just(None),
        2 => prop::sample::select(vec!["custom", "Ref", "item"])
            .prop_map(|name| Some(StructuredTag::Local(name))),
        1 => Just(Some(StructuredTag::CoreStr)),
        1 => Just(Some(StructuredTag::Verbatim)),
        1 => Just(Some(StructuredTag::Handle)),
    ]
}

fn structured_key() -> impl Strategy<Value = StructuredKey> {
    prop_oneof![
        5 => prop::sample::select(vec![
            "a", "b", "c", "d", "name", "on", "yes", "count", "nested", "items", "<<",
        ])
        .prop_map(StructuredKey::Plain),
        2 => safe_single_quoted_text().prop_map(StructuredKey::Quoted),
        2 => prop::sample::select(vec!["explicit_a", "explicit_b", "k:1", "space key"])
            .prop_map(|text| StructuredKey::Explicit(text.to_owned())),
        1 => prop::collection::vec(-5_i32..5, 1..3).prop_map(StructuredKey::FlowSeq),
    ]
}

fn structured_leaf() -> impl Strategy<Value = StructuredItem> {
    let scalar_node = prop_oneof![
        5 => structured_scalar().prop_map(StructuredNode::Scalar),
        3 => (
            any::<bool>(),
            prop::sample::select(vec![BlockChomp::Clip, BlockChomp::Strip, BlockChomp::Keep]),
            any::<bool>(),
            prop::collection::vec(block_scalar_line(), 1..4),
        )
            .prop_map(|(folded, chomp, explicit_indent, lines)| {
                StructuredNode::BlockScalar {
                    folded,
                    chomp,
                    explicit_indent,
                    lines,
                }
            }),
        3 => (0_usize..8).prop_map(StructuredNode::Alias),
    ];
    (prop::bool::weighted(0.35), structured_tag(), scalar_node)
        .prop_map(|(anchor, tag, node)| StructuredItem { anchor, tag, node })
}

fn structured_item() -> impl Strategy<Value = StructuredItem> {
    structured_leaf().prop_recursive(3, 28, 4, |inner| {
        let collection_node = prop_oneof![
            (any::<bool>(), prop::collection::vec(inner.clone(), 0..4))
                .prop_map(|(flow, items)| StructuredNode::Sequence { flow, items }),
            (
                any::<bool>(),
                prop::collection::vec((structured_key(), inner), 0..4),
            )
                .prop_map(|(flow, entries)| StructuredNode::Mapping { flow, entries }),
        ];
        (
            prop::bool::weighted(0.30),
            structured_tag(),
            collection_node,
        )
            .prop_map(|(anchor, tag, node)| StructuredItem { anchor, tag, node })
    })
}

fn structured_doc() -> impl Strategy<Value = StructuredDoc> {
    // Bias root toward collections so generated documents regularly contain anchors, aliases,
    // nested mappings, sequences, tags, and block scalars together.
    let root = prop_oneof![
        3 => (
            prop::bool::weighted(0.2),
            structured_tag(),
            any::<bool>(),
            prop::collection::vec((structured_key(), structured_item()), 1..5),
        )
            .prop_map(|(anchor, tag, flow, entries)| StructuredItem {
                anchor,
                tag,
                node: StructuredNode::Mapping { flow, entries },
            }),
        2 => (
            prop::bool::weighted(0.2),
            structured_tag(),
            any::<bool>(),
            prop::collection::vec(structured_item(), 1..5),
        )
            .prop_map(|(anchor, tag, flow, items)| StructuredItem {
                anchor,
                tag,
                node: StructuredNode::Sequence { flow, items },
            }),
        1 => structured_item(),
    ];
    (
        prop::bool::weighted(0.15),
        prop::bool::weighted(0.20),
        prop::bool::weighted(0.45),
        prop::bool::weighted(0.35),
        prop::bool::weighted(0.20),
        prop::bool::weighted(0.35),
        prop::bool::weighted(0.40),
        root,
    )
        .prop_map(
            |(
                with_bom,
                crlf,
                explicit_start,
                explicit_end,
                yaml_directive,
                tag_directive,
                leading_comment,
                root,
            )| StructuredDoc {
                with_bom,
                crlf,
                explicit_start,
                explicit_end,
                yaml_directive,
                tag_directive,
                leading_comment,
                root,
            },
        )
}

struct YamlRenderer {
    out: String,
    next_anchor: usize,
    defined_anchors: Vec<String>,
    has_tag_directive: bool,
    comment_counter: usize,
}

impl YamlRenderer {
    fn new(has_tag_directive: bool) -> Self {
        Self {
            out: String::new(),
            next_anchor: 0,
            defined_anchors: Vec::new(),
            has_tag_directive,
            comment_counter: 0,
        }
    }

    fn push_indent(&mut self, indent: usize) {
        for _ in 0..indent {
            self.out.push(' ');
        }
    }

    fn maybe_comment(&mut self, indent: usize) {
        self.comment_counter += 1;
        if self.comment_counter.is_multiple_of(5) {
            self.push_indent(indent);
            self.out.push_str("# generated comment\n");
        }
    }

    fn render_single_quoted(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('\'');
        for ch in text.chars() {
            if ch == '\'' {
                out.push_str("''");
            } else {
                out.push(ch);
            }
        }
        out.push('\'');
        out
    }

    fn render_double_quoted(text: &str) -> String {
        let mut out = String::with_capacity(text.len() + 2);
        out.push('"');
        for ch in text.chars() {
            match ch {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(ch),
            }
        }
        out.push('"');
        out
    }

    fn render_scalar(scalar: &StructuredScalar) -> String {
        match scalar {
            StructuredScalar::Plain(text)
            | StructuredScalar::Null(text)
            | StructuredScalar::Bool(text)
            | StructuredScalar::Int(text)
            | StructuredScalar::Float(text) => (*text).to_owned(),
            StructuredScalar::EmptyBlockValue => "null".to_owned(),
            StructuredScalar::SingleQuoted(text) => Self::render_single_quoted(text),
            StructuredScalar::DoubleQuoted(text) => Self::render_double_quoted(text),
        }
    }

    fn format_tag(&self, tag: StructuredTag, node: &StructuredNode) -> Option<String> {
        match tag {
            StructuredTag::Local(name) => Some(format!("!{name}")),
            StructuredTag::CoreStr => match node {
                StructuredNode::Scalar(_) | StructuredNode::BlockScalar { .. } => {
                    Some("!!str".to_owned())
                }
                StructuredNode::Sequence { .. } => Some("!!seq".to_owned()),
                StructuredNode::Mapping { .. } => Some("!!map".to_owned()),
                StructuredNode::Alias(_) => None,
            },
            StructuredTag::Verbatim => Some("!<tag:example.com,2000:node>".to_owned()),
            StructuredTag::Handle => {
                if self.has_tag_directive {
                    Some("!e!item".to_owned())
                } else {
                    Some("!local".to_owned())
                }
            }
        }
    }

    fn properties(&mut self, item: &StructuredItem) -> (Option<String>, String) {
        if matches!(item.node, StructuredNode::Alias(_)) {
            return (None, String::new());
        }
        let anchor_name = if item.anchor {
            let name = format!("a{}", self.next_anchor);
            self.next_anchor += 1;
            Some(name)
        } else {
            None
        };
        let tag_text = item.tag.and_then(|tag| self.format_tag(tag, &item.node));
        let mut prefix = String::new();
        if let Some(name) = &anchor_name {
            prefix.push('&');
            prefix.push_str(name);
        }
        if let Some(tag) = &tag_text {
            if !prefix.is_empty() {
                prefix.push(' ');
            }
            prefix.push_str(tag);
        }
        (anchor_name, prefix)
    }

    fn render_flow_item(&mut self, item: &StructuredItem) {
        if let StructuredNode::Alias(pick) = item.node {
            if let Some(name) = self
                .defined_anchors
                .get(pick % self.defined_anchors.len().max(1))
            {
                self.out.push('*');
                self.out.push_str(name);
                return;
            }
            self.out.push_str("fallback");
            return;
        }
        let (anchor_name, props) = self.properties(item);
        if !props.is_empty() {
            self.out.push_str(&props);
            self.out.push(' ');
        }
        match &item.node {
            StructuredNode::Scalar(scalar) => self.out.push_str(&Self::render_scalar(scalar)),
            StructuredNode::BlockScalar { lines, .. } => {
                // Block scalars are invalid in flow context; emit as escaped double-quoted string.
                let joined = lines.join("\n");
                self.out.push_str(&Self::render_double_quoted(&joined));
            }
            StructuredNode::Sequence { items, .. } => {
                self.out.push('[');
                for (idx, child) in items.iter().enumerate() {
                    if idx > 0 {
                        self.out.push_str(", ");
                    }
                    self.render_flow_item(child);
                }
                self.out.push(']');
            }
            StructuredNode::Mapping { entries, .. } => {
                self.out.push('{');
                let mut seen = std::collections::HashSet::new();
                let mut emitted = 0;
                for (key, value) in entries {
                    let (canonical, rendered_key) = Self::render_flow_key(key);
                    if !seen.insert(canonical) {
                        continue;
                    }
                    if emitted > 0 {
                        self.out.push_str(", ");
                    }
                    self.out.push_str(&rendered_key);
                    self.out.push_str(": ");
                    self.render_flow_item(value);
                    emitted += 1;
                }
                self.out.push('}');
            }
            StructuredNode::Alias(_) => unreachable!("handled above"),
        }
        if let Some(name) = anchor_name {
            self.defined_anchors.push(name);
        }
    }

    fn render_flow_key(key: &StructuredKey) -> (String, String) {
        match key {
            StructuredKey::Plain(text) => (format!("s:{text}"), (*text).to_owned()),
            StructuredKey::Quoted(text) => (format!("s:{text}"), Self::render_single_quoted(text)),
            StructuredKey::Explicit(text) => {
                (format!("s:{text}"), Self::render_single_quoted(text))
            }
            StructuredKey::FlowSeq(nums) => {
                let text = format!(
                    "[{}]",
                    nums.iter()
                        .map(i32::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                (format!("seq:{text}"), text)
            }
        }
    }

    fn is_empty_block_value(item: &StructuredItem) -> bool {
        matches!(
            item.node,
            StructuredNode::Scalar(StructuredScalar::EmptyBlockValue)
        )
    }

    /// Render an item in block context where `indent` is the column of this node's block
    /// content. `inline_prefix` indicates we are right after `: ` or `- ` on the current line.
    fn render_block_item(&mut self, item: &StructuredItem, indent: usize, inline_prefix: bool) {
        if let StructuredNode::Alias(pick) = item.node {
            if !inline_prefix {
                self.push_indent(indent);
            }
            if let Some(name) = self
                .defined_anchors
                .get(pick % self.defined_anchors.len().max(1))
            {
                self.out.push('*');
                self.out.push_str(name);
            } else {
                self.out.push_str("fallback");
            }
            self.out.push('\n');
            return;
        }
        let (anchor_name, props) = self.properties(item);
        match &item.node {
            StructuredNode::Scalar(StructuredScalar::EmptyBlockValue) if inline_prefix => {
                if !props.is_empty() {
                    self.out.push(' ');
                    self.out.push_str(&props);
                }
                self.out.push('\n');
            }
            StructuredNode::Scalar(scalar) => {
                if !inline_prefix {
                    self.push_indent(indent);
                }
                if !props.is_empty() {
                    self.out.push_str(&props);
                    self.out.push(' ');
                }
                self.out.push_str(&Self::render_scalar(scalar));
                self.comment_counter += 1;
                if self.comment_counter.is_multiple_of(7) {
                    self.out.push_str(" # inline");
                }
                self.out.push('\n');
            }
            StructuredNode::BlockScalar {
                folded,
                chomp,
                explicit_indent,
                lines,
            } => {
                if !inline_prefix {
                    self.push_indent(indent);
                }
                if !props.is_empty() {
                    self.out.push_str(&props);
                    self.out.push(' ');
                }
                self.out.push(if *folded { '>' } else { '|' });
                let first_non_empty_starts_with_space = lines
                    .iter()
                    .find(|line| !line.is_empty())
                    .is_some_and(|line| line.starts_with(' '));
                if *explicit_indent || first_non_empty_starts_with_space {
                    self.out.push('2');
                }
                match chomp {
                    BlockChomp::Clip => {}
                    BlockChomp::Strip => self.out.push('-'),
                    BlockChomp::Keep => self.out.push('+'),
                }
                self.out.push('\n');
                let body_indent = indent + 2;
                for line in lines {
                    if !line.is_empty() {
                        self.push_indent(body_indent);
                        self.out.push_str(line);
                    }
                    self.out.push('\n');
                }
            }
            StructuredNode::Sequence { flow, items } if *flow || items.is_empty() => {
                if !inline_prefix {
                    self.push_indent(indent);
                }
                if !props.is_empty() {
                    self.out.push_str(&props);
                    self.out.push(' ');
                }
                self.out.push('[');
                for (idx, child) in items.iter().enumerate() {
                    if idx > 0 {
                        self.out.push_str(", ");
                    }
                    self.render_flow_item(child);
                }
                self.out.push_str("]\n");
            }
            StructuredNode::Mapping { flow, entries } if *flow || entries.is_empty() => {
                if !inline_prefix {
                    self.push_indent(indent);
                }
                if !props.is_empty() {
                    self.out.push_str(&props);
                    self.out.push(' ');
                }
                self.out.push('{');
                let mut seen = std::collections::HashSet::new();
                let mut emitted = 0;
                for (key, value) in entries {
                    let (canonical, rendered_key) = Self::render_flow_key(key);
                    if !seen.insert(canonical) {
                        continue;
                    }
                    if emitted > 0 {
                        self.out.push_str(", ");
                    }
                    self.out.push_str(&rendered_key);
                    self.out.push_str(": ");
                    self.render_flow_item(value);
                    emitted += 1;
                }
                self.out.push_str("}\n");
            }
            StructuredNode::Sequence { items, .. } => {
                let seq_indent = if inline_prefix { indent + 2 } else { indent };
                if !props.is_empty() {
                    if inline_prefix {
                        self.out.push(' ');
                    } else {
                        self.push_indent(indent);
                    }
                    self.out.push_str(&props);
                    self.out.push('\n');
                } else if inline_prefix {
                    self.out.push('\n');
                }
                for child in items {
                    self.maybe_comment(seq_indent);
                    self.push_indent(seq_indent);
                    if Self::is_block_collection(child) || Self::is_empty_block_value(child) {
                        self.out.push('-');
                        self.render_block_item(child, seq_indent, true);
                    } else {
                        self.out.push_str("- ");
                        self.render_block_item(child, seq_indent + 2, true);
                    }
                }
            }
            StructuredNode::Mapping { entries, .. } => {
                self.render_block_mapping(entries, &props, indent, inline_prefix);
            }
            StructuredNode::Alias(_) => unreachable!("handled above"),
        }
        if let Some(name) = anchor_name {
            self.defined_anchors.push(name);
        }
    }

    fn render_block_mapping(
        &mut self,
        entries: &[(StructuredKey, StructuredItem)],
        props: &str,
        indent: usize,
        inline_prefix: bool,
    ) {
        let mut unique = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for (key, value) in entries {
            let (canonical, _) = Self::render_flow_key(key);
            if seen.insert(canonical) {
                unique.push((key, value));
            }
        }
        if unique.is_empty() {
            if !inline_prefix {
                self.push_indent(indent);
            }
            if !props.is_empty() {
                self.out.push_str(props);
                self.out.push(' ');
            }
            self.out.push_str("{}\n");
            return;
        }
        let map_indent = if inline_prefix { indent + 2 } else { indent };
        if !props.is_empty() {
            if inline_prefix {
                self.out.push(' ');
            } else {
                self.push_indent(indent);
            }
            self.out.push_str(props);
            self.out.push('\n');
        } else if inline_prefix {
            self.out.push('\n');
        }
        for (key, value) in unique {
            self.maybe_comment(map_indent);
            self.push_indent(map_indent);
            if let StructuredKey::Explicit(text) = key {
                self.out.push_str("? ");
                self.out.push_str(&Self::render_single_quoted(text));
                self.out.push('\n');
                self.push_indent(map_indent);
                if Self::is_block_collection(value) || Self::is_empty_block_value(value) {
                    self.out.push(':');
                    self.render_block_item(value, map_indent, true);
                } else {
                    self.out.push_str(": ");
                    self.render_block_item(value, map_indent + 2, true);
                }
            } else {
                let (_, rendered_key) = Self::render_flow_key(key);
                self.out.push_str(&rendered_key);
                if Self::is_block_collection(value) || Self::is_empty_block_value(value) {
                    self.out.push(':');
                    self.render_block_item(value, map_indent, true);
                } else {
                    self.out.push_str(": ");
                    self.render_block_item(value, map_indent + 2, true);
                }
            }
        }
    }

    fn is_block_collection(item: &StructuredItem) -> bool {
        match &item.node {
            StructuredNode::Sequence { flow, items } => !*flow && !items.is_empty(),
            StructuredNode::Mapping { flow, entries } => !*flow && !entries.is_empty(),
            _ => false,
        }
    }

    fn render_document(doc: &StructuredDoc) -> String {
        let mut renderer = Self::new(doc.tag_directive);
        if doc.with_bom {
            renderer.out.push('\u{feff}');
        }
        if doc.leading_comment {
            renderer.out.push_str("# leading comment\n");
        }
        let needs_start = doc.explicit_start || doc.yaml_directive || doc.tag_directive;
        if doc.yaml_directive {
            renderer.out.push_str("%YAML 1.2\n");
        }
        if doc.tag_directive {
            renderer.out.push_str("%TAG !e! tag:example.com,2000:\n");
        }
        if needs_start {
            renderer.out.push_str("---\n");
        }
        renderer.render_block_item(&doc.root, 0, false);
        if doc.explicit_end {
            renderer.out.push_str("...\n");
        }
        if doc.crlf {
            renderer.out.replace('\n', "\r\n")
        } else {
            renderer.out
        }
    }
}

pub(super) fn structured_yaml_document() -> impl Strategy<Value = String> {
    structured_doc().prop_map(|doc| YamlRenderer::render_document(&doc))
}

pub(super) fn structured_yaml_stream() -> impl Strategy<Value = String> {
    prop::collection::vec(structured_doc(), 2..=4).prop_map(|docs| {
        let mut out = String::new();
        for (idx, doc) in docs.iter().enumerate() {
            let mut adjusted = doc.clone();
            adjusted.with_bom = false;
            adjusted.crlf = false;
            if idx > 0 {
                adjusted.explicit_start = true;
            }
            if docs
                .get(idx + 1)
                .is_some_and(|next| next.yaml_directive || next.tag_directive)
            {
                adjusted.explicit_end = true;
            }
            out.push_str(&YamlRenderer::render_document(&adjusted));
        }
        out
    })
}

/// Apply one small syntax mutation to a valid structured YAML document to exercise near-valid
/// error paths and partial-parse equivalence.
pub(super) fn mutated_structured_yaml() -> impl Strategy<Value = String> {
    (
        structured_yaml_document(),
        0_usize..100,
        prop::sample::select(vec![
            "",
            ":",
            "-",
            "[",
            "]",
            "{",
            "}",
            "&a ",
            "*missing ",
            "!",
            "\t",
            "'",
            "\"",
            "\n...",
        ]),
    )
        .prop_map(|(doc, pct, patch)| {
            if doc.is_empty() {
                return patch.to_owned();
            }
            let target = (doc.len() * pct) / 100;
            let mut split = target.min(doc.len());
            while split > 0 && !doc.is_char_boundary(split) {
                split -= 1;
            }
            let mut mutated = String::with_capacity(doc.len() + patch.len());
            mutated.push_str(&doc[..split]);
            mutated.push_str(patch);
            if !patch.is_empty() {
                mutated.push_str(&doc[split..]);
            }
            mutated
        })
}
