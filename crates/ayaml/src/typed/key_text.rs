//! The text of a mapping key: what a scalar key is compared by, and what a diagnostic names.

use std::borrow::Cow;
use std::fmt::Write as _;

use super::{Node, NodeKind, resolve};
use crate::value::{key_text_full, push_quoted_key};

/// The text of a scalar key, whatever its scalar type. Two keys with the same text but different
/// types, as `1` and `"1"`, are one key to a target whose key type is narrower.
pub(super) fn scalar_key_text<'de>(node: &Node<'de>) -> Option<Cow<'de, str>> {
    let node = resolve(node);
    match &node.kind {
        NodeKind::Null => Some(Cow::Borrowed("null")),
        NodeKind::Bool(value) => Some(Cow::Owned(value.to_string())),
        NodeKind::Number(value) => Some(Cow::Owned(value.to_string())),
        NodeKind::String(value) => Some(value.clone()),
        _ => None,
    }
}

/// A human-readable mapping key for typed diagnostics.
pub(super) fn node_key_text(node: &Node<'_>) -> String {
    let mut text = String::new();
    push_node_key_text(resolve(node), &mut text);
    text
}

fn push_node_key_text(node: &Node<'_>, output: &mut String) {
    if key_text_full(output) {
        return;
    }
    match &node.kind {
        NodeKind::Null => output.push_str("null"),
        NodeKind::Bool(value) => {
            let _ = write!(output, "{value}");
        }
        NodeKind::Number(value) => {
            let _ = write!(output, "{value}");
        }
        NodeKind::String(value) => push_quoted_key(value, output),
        NodeKind::Sequence(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                if index > 0 {
                    output.push_str(", ");
                }
                push_node_key_text(item, output);
            }
            if !key_text_full(output) {
                output.push(']');
            }
        }
        NodeKind::Mapping(entries) => {
            output.push('{');
            for (index, (key, value)) in entries.iter().enumerate() {
                if key_text_full(output) {
                    output.push('…');
                    return;
                }
                if index > 0 {
                    output.push_str(", ");
                }
                push_node_key_text(key, output);
                if !key_text_full(output) {
                    output.push_str(": ");
                }
                push_node_key_text(value, output);
            }
            if !key_text_full(output) {
                output.push('}');
            }
        }
        NodeKind::Tagged(tag, inner) => {
            let _ = write!(output, "{tag} ");
            push_node_key_text(inner, output);
        }
        NodeKind::Alias(target) => push_node_key_text(target, output),
    }
}
