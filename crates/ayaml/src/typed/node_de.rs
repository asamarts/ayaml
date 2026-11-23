//! Serde [`Deserializer`] over a built typed [`Node`] tree.
//!
//! Parameterized by [`StrMode`] so the same implementation drives both borrowed
//! decoding (`'src == 'de`, calling `visit_borrowed_str` / `visit_borrowed_bytes`)
//! and owned-lifetime decoding (`Deserializer::next_seed<'de>` where `'de` is
//! independent of the input lifetime `'src`, calling `visit_str` / `visit_bytes`).

use std::borrow::Cow;
use std::marker::PhantomData;

use serde::Deserializer;
use serde::de::{DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor};

use super::{Node, NodeKind, Text, resolve, unexpected_node, untagged};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::resolver::{float_from_text, parse_bool, parse_int};
use crate::span::Span;

/// Controls whether borrowed source slices are handed to visitors via
/// `visit_borrowed_*` (when `'src == 'de`) or `visit_*` (when `'de` is independent).
pub(super) trait StrMode<'src, 'de>: Copy {
    fn visit_src_str<V: Visitor<'de>>(visitor: V, value: &'src str) -> Result<V::Value, Error>;
    fn visit_src_bytes<V: Visitor<'de>>(visitor: V, value: &'src [u8]) -> Result<V::Value, Error>;
}

/// Borrow strings and byte slices directly from the input (`'src == 'de`).
#[derive(Clone, Copy)]
pub(super) struct BorrowedMode;

impl<'de> StrMode<'de, 'de> for BorrowedMode {
    #[inline]
    fn visit_src_str<V: Visitor<'de>>(visitor: V, value: &'de str) -> Result<V::Value, Error> {
        visitor.visit_borrowed_str(value)
    }

    #[inline]
    fn visit_src_bytes<V: Visitor<'de>>(visitor: V, value: &'de [u8]) -> Result<V::Value, Error> {
        visitor.visit_borrowed_bytes(value)
    }
}

/// Hand strings and byte slices via non-borrowed `visit_str` / `visit_bytes` so any
/// caller-chosen `'de` lifetime can decode from a document's spanned [`Node`] tree.
#[derive(Clone, Copy)]
pub(super) struct OwnedMode;

impl<'src, 'de> StrMode<'src, 'de> for OwnedMode {
    #[inline]
    fn visit_src_str<V: Visitor<'de>>(visitor: V, value: &'src str) -> Result<V::Value, Error> {
        visitor.visit_str(value)
    }

    #[inline]
    fn visit_src_bytes<V: Visitor<'de>>(visitor: V, value: &'src [u8]) -> Result<V::Value, Error> {
        visitor.visit_bytes(value)
    }
}

pub(super) fn enum_error(message: &'static str, span: Span) -> Error {
    Error::coded(
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        message,
    )
    .with_span(span)
}

pub(super) fn with_span(error: Error, span: Span) -> Error {
    if error.span().is_none() {
        error.with_span(span)
    } else {
        error
    }
}

pub(super) struct NodeDeserializer<'src, 'a, M = BorrowedMode> {
    node: &'a Node<'src>,
    mode: PhantomData<M>,
}

/// Deserializer methods for integer targets, parsing a plain scalar whose global tag was dropped.
macro_rules! int_targets {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                let node = untagged(self.node);
                if matches!(node.text, Text::DroppedTag) {
                    return deserialize_dropped_tag_int::<M, V>(node, visitor);
                }
                NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
            }
        )*
    };
}

#[cold]
fn deserialize_dropped_tag_bool<'src, 'de, M: StrMode<'src, 'de>, V: Visitor<'de>>(
    node: &Node<'src>,
    visitor: V,
) -> Result<V::Value, Error> {
    if let NodeKind::String(text) = &node.kind
        && let Some(value) = parse_bool(text)
    {
        return visitor
            .visit_bool(value)
            .map_err(|error| with_span(error, node.span));
    }
    NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
}

#[cold]
fn deserialize_dropped_tag_int<'src, 'de, M: StrMode<'src, 'de>, V: Visitor<'de>>(
    node: &Node<'src>,
    visitor: V,
) -> Result<V::Value, Error> {
    if let NodeKind::String(text) = &node.kind
        && let Some(value) = parse_int(text)
    {
        return value
            .visit(visitor)
            .map_err(|error| with_span(error, node.span));
    }
    NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
}

/// Deserializer methods for targets that read a node as `deserialize_any` does, past an
/// application tag (see `untagged`).
macro_rules! plain_targets {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                NodeDeserializer::<M>::with_mode(untagged(self.node)).deserialize_any(visitor)
            }
        )*
    };
}

impl<'src, 'a> NodeDeserializer<'src, 'a, BorrowedMode> {
    pub(super) fn new(node: &'a Node<'src>) -> Self {
        Self {
            node,
            mode: PhantomData,
        }
    }
}

impl<'src, 'a> NodeDeserializer<'src, 'a, OwnedMode> {
    pub(super) fn owned(node: &'a Node<'src>) -> Self {
        Self {
            node,
            mode: PhantomData,
        }
    }
}

impl<'src, 'a, M> NodeDeserializer<'src, 'a, M> {
    fn with_mode(node: &'a Node<'src>) -> Self {
        Self {
            node,
            mode: PhantomData,
        }
    }

    /// The node to read. Aliases form no cycles: an anchor is defined only
    /// once its node is complete, so an alias cannot reach itself.
    fn target(&self) -> &'a Node<'src> {
        resolve(self.node)
    }
}

impl<'src, 'de, M: StrMode<'src, 'de>> Deserializer<'de> for NodeDeserializer<'src, '_, M> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::Null => visitor.visit_unit(),
            NodeKind::Bool(value) => visitor.visit_bool(*value),
            NodeKind::Number(value) => value.visit(visitor),
            NodeKind::String(Cow::Borrowed(value)) => M::visit_src_str(visitor, value),
            NodeKind::String(Cow::Owned(value)) => visitor.visit_str(value),
            NodeKind::Sequence(sequence) => {
                let mut access = NodeSeqAccess::<M> {
                    iter: sequence.iter(),
                    len: sequence.len(),
                    mode: PhantomData,
                };
                visitor.visit_seq(&mut access).and_then(|value| {
                    // A visitor that stops early, such as a tuple's, leaves
                    // elements behind.
                    check_consumed(access.iter.len(), sequence.len(), "sequence").map(|()| value)
                })
            }
            NodeKind::Mapping(entries) => {
                let mut access = NodeMapAccess::<M> {
                    iter: entries.iter(),
                    entry: None,
                    mode: PhantomData,
                };
                visitor.visit_map(&mut access).and_then(|value| {
                    check_consumed(access.iter.len(), entries.len(), "map").map(|()| value)
                })
            }
            // A tagged node is an enum whose variant names the tag, so a
            // `Value` keeps the tag.
            NodeKind::Tagged(tag, value) => {
                let mut entered = false;
                return visitor
                    .visit_enum(NodeEnumAccess::<M> {
                        variant: tag.serde_name(),
                        variant_span: span,
                        value: NodeValue::Ref(value),
                        in_path: false,
                        entered: Some(&mut entered),
                        mode: PhantomData,
                    })
                    .map_err(|error| {
                        with_span(crate::serde_impl::tag_refused(error, tag, entered), span)
                    });
            }
            NodeKind::Alias(_) => unreachable!("aliases are resolved by target"),
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        if matches!(&node.kind, NodeKind::Null) {
            visitor.visit_none()
        } else {
            visitor
                .visit_some(NodeDeserializer::<M>::with_mode(node))
                .map_err(|error| with_span(error, span))
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let span = self.target().span;
        visitor
            .visit_newtype_struct(self)
            .map_err(|error| with_span(error, span))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::String(Cow::Borrowed(value)) => M::visit_src_str(visitor, value),
            NodeKind::String(Cow::Owned(value)) => visitor.visit_str(value),
            NodeKind::Tagged(_, value) => {
                return NodeDeserializer::<M>::with_mode(value)
                    .deserialize_string(visitor)
                    .map_err(|error| with_span(error, span));
            }
            _ => match &node.text {
                Text::Plain(value) | Text::Spelled(value) => M::visit_src_str(visitor, value),
                Text::Decoded(value) => visitor.visit_str(value),
                Text::None | Text::Nothing | Text::DroppedTag => {
                    return NodeDeserializer::<M>::with_mode(node)
                        .deserialize_any(visitor)
                        .map_err(|error| with_span(error, span));
                }
            },
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        let result = match &node.kind {
            NodeKind::String(Cow::Borrowed(value)) => M::visit_src_bytes(visitor, value.as_bytes()),
            NodeKind::String(Cow::Owned(value)) => visitor.visit_bytes(value.as_bytes()),
            NodeKind::Tagged(_, value) => {
                return NodeDeserializer::<M>::with_mode(value)
                    .deserialize_bytes(visitor)
                    .map_err(|error| with_span(error, span));
            }
            _ => {
                return NodeDeserializer::<M>::with_mode(node)
                    .deserialize_any(visitor)
                    .map_err(|error| with_span(error, span));
            }
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_seq(EmptySeqAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        NodeDeserializer::<M>::with_mode(node)
            .deserialize_any(visitor)
            .map_err(|error| with_span(error, span))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_map(EmptyMapAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        if matches!(node.kind, NodeKind::Mapping(_) | NodeKind::Tagged(_, _)) {
            return NodeDeserializer::<M>::with_mode(node)
                .deserialize_any(visitor)
                .map_err(|error| with_span(error, span));
        }
        Err(with_span(
            serde::de::Error::invalid_type(unexpected_node(node), &visitor),
            span,
        ))
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        let span = node.span;
        if is_empty_scalar(node) {
            return visitor
                .visit_map(EmptyMapAccess)
                .map_err(|error| with_span(error, span));
        }
        null_is_not_a_collection(node, &visitor)?;
        NodeDeserializer::<M>::with_mode(node)
            .deserialize_any(visitor)
            .map_err(|error| with_span(error, span))
    }

    /// A field name is the key's text: `1: x` names the field `"1"`, not the
    /// second field.
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    /// A character is read from the scalar's text, so `5` is `'5'`.
    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let node = self.target();
        let span = node.span;
        if !matches!(
            &node.kind,
            NodeKind::Tagged(_, _) | NodeKind::String(_) | NodeKind::Mapping(_)
        ) {
            return Err(with_span(
                serde::de::Error::invalid_type(unexpected_node(node), &visitor),
                span,
            ));
        }
        let (variant, variant_span, value) = match &node.kind {
            // A tag names the variant by its whole name (`Tag::variant_name`), so `!<tag:x:B>`
            // and `!e!B` under `%TAG !e! tag:x:` select the same variant.
            NodeKind::Tagged(tag, value) => (tag.variant_name(), span, NodeValue::Ref(value)),
            // A bare name holds nothing. That is a unit variant, and it is not an empty
            // collection for a variant that holds one: `List` is not `List: []`.
            NodeKind::String(variant) => (
                variant.as_ref().to_owned(),
                span,
                NodeValue::Null(Node::scalar(NodeKind::Null, Text::Nothing, span)),
            ),
            NodeKind::Mapping(entries) => {
                let mut iter = entries.iter();
                let Some((key, value)) = iter.next() else {
                    return Err(enum_error("enum mapping must contain one variant", span));
                };
                if iter.next().is_some() {
                    return Err(enum_error("enum mapping must contain one variant", span));
                }
                let variant_span = key.span;
                let key = match &resolve(key).kind {
                    // The non-specific tag makes a scalar a string, so `! B: 5` names `B`.
                    NodeKind::Tagged(tag, inner) if tag.is_non_specific() => resolve(inner),
                    _ => resolve(key),
                };
                let NodeKind::String(variant) = &key.kind else {
                    return Err(enum_error(
                        "enum variant key must be a string",
                        variant_span,
                    ));
                };
                (
                    variant.as_ref().to_owned(),
                    variant_span,
                    NodeValue::Ref(value),
                )
            }
            _ => {
                return Err(enum_error(
                    "enum value must be tagged or a single-entry mapping",
                    span,
                ));
            }
        };
        visitor
            .visit_enum(NodeEnumAccess::<M> {
                variant,
                variant_span,
                value,
                in_path: true,
                entered: None,
                mode: PhantomData,
            })
            .map_err(|error| with_span(error, span))
    }

    /// A float target parses a plain scalar from its source text, so `01` is
    /// 1.0 and a radix integer such as `0x1F` is rejected. Quoted scalars are
    /// strings and are not parsed this way.
    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        if let NodeKind::Number(number) = &node.kind
            && number.is_f64()
        {
            return visitor
                .visit_f64(number.as_f64().unwrap_or(f64::NAN))
                .map_err(|error| with_span(error, node.span));
        }
        // A plain scalar reads from its text, so an integer wider than 64 bits reads, and so does
        // a number spelling with a leading zero, as in `01`, which resolves to a string. Other text,
        // such as `inf` or `1e999`, doesn't, and nor does a string that a tag makes one, which
        // keeps no text.
        if let Text::Plain(text) = node.text {
            match &node.kind {
                NodeKind::Number(_) => {
                    if let Ok(value) = text.parse::<f64>() {
                        return visitor
                            .visit_f64(value)
                            .map_err(|error| with_span(error, node.span));
                    }
                    // A radix integer is a number to the resolver, but not text that parses
                    // into a float.
                    return Err(Error::coded(
                        ErrorKind::Serde,
                        ErrorCode::InvalidType,
                        format!("invalid type: integer `{text}`, expected f64"),
                    )
                    .with_span(node.span));
                }
                NodeKind::String(_) => {
                    if let Some(value) = float_from_text(text) {
                        return visitor
                            .visit_f64(value)
                            .map_err(|error| with_span(error, node.span));
                    }
                }
                _ => {}
            }
        } else if matches!(node.text, Text::DroppedTag)
            && let NodeKind::String(text) = &node.kind
            && let Some(value) = float_from_text(text)
        {
            return visitor
                .visit_f64(value)
                .map_err(|error| with_span(error, node.span));
        }
        NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_unit(visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::<M>::with_mode(untagged(self.node)).deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.deserialize_tuple(0, visitor)
    }

    fn deserialize_bool<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let node = untagged(self.node);
        if matches!(node.text, Text::DroppedTag) {
            return deserialize_dropped_tag_bool::<M, V>(node, visitor);
        }
        NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
    }

    int_targets! {
        deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_u128
    }

    plain_targets! {
        deserialize_unit
    }

    serde::forward_to_deserialize_any! {
        ignored_any
    }
}

/// An empty scalar, such as a missing value, reads as an empty sequence,
/// map, or struct; an explicit `~` or `null` does not.
fn is_empty_scalar(node: &Node<'_>) -> bool {
    // A value that the source doesn't write is a null with no text. A written null keeps its
    // text in every style, and is explicit unless that text is empty, as in `!!null ""`. What
    // a bare variant name holds is no value at all.
    matches!(node.kind, NodeKind::Null)
        && !matches!(node.text, Text::Nothing)
        && node.text.as_str().is_none_or(str::is_empty)
}

/// An explicit `~` or `null` is not an empty collection. It's reported before
/// the visitor sees it, so even a visitor that accepts a unit fails.
fn null_is_not_a_collection(
    node: &Node<'_>,
    expected: &dyn serde::de::Expected,
) -> Result<(), Error> {
    if matches!(node.kind, NodeKind::Null) {
        Err(with_span(
            serde::de::Error::invalid_type(serde::de::Unexpected::Unit, expected),
            node.span,
        ))
    } else {
        Ok(())
    }
}

/// Fail when a visitor left `remaining` of `total` elements unread.
pub(super) fn check_consumed(remaining: usize, total: usize, kind: &str) -> Result<(), Error> {
    if remaining == 0 {
        Ok(())
    } else {
        Err(serde::de::Error::invalid_length(
            total,
            &format!("fewer elements in the {kind}").as_str(),
        ))
    }
}

struct EmptySeqAccess;

impl<'de> SeqAccess<'de> for EmptySeqAccess {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        _seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        Ok(None)
    }

    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}

struct EmptyMapAccess;

impl<'de> MapAccess<'de> for EmptyMapAccess {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        _seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        Ok(None)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        _seed: V,
    ) -> Result<V::Value, Self::Error> {
        Err(Error::coded(
            ErrorKind::Serde,
            ErrorCode::MapValueBeforeKey,
            "map value requested before key",
        ))
    }
}

enum NodeValue<'src, 'a> {
    Ref(&'a Node<'src>),
    Null(Node<'src>),
}

impl<'src> NodeValue<'src, '_> {
    fn node(&self) -> &Node<'src> {
        match self {
            Self::Ref(node) => node,
            Self::Null(node) => node,
        }
    }
}

struct NodeSeqAccess<'src, 'a, M> {
    iter: std::slice::Iter<'a, Node<'src>>,
    len: usize,
    mode: PhantomData<M>,
}

impl<'src, 'de, M: StrMode<'src, 'de>> SeqAccess<'de> for NodeSeqAccess<'src, '_, M> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(node) = self.iter.next() else {
            return Ok(None);
        };
        let span = node.span;
        seed.deserialize(NodeDeserializer::<M>::with_mode(node))
            .map(Some)
            .map_err(|error| with_span(error, span).inside_index(self.len - self.iter.len() - 1))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct NodeMapAccess<'src, 'a, M> {
    iter: std::slice::Iter<'a, (Node<'src>, Node<'src>)>,
    /// The entry whose key was read and whose value wasn't.
    entry: Option<&'a (Node<'src>, Node<'src>)>,
    mode: PhantomData<M>,
}

impl<'src, 'de, M: StrMode<'src, 'de>> MapAccess<'de> for NodeMapAccess<'src, '_, M> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some(entry) = self.iter.next() else {
            // The end leaves no value to ask for, as it leaves none to a reader of events: a
            // value that was passed over would otherwise be given out after it.
            self.entry = None;
            return Ok(None);
        };
        let key = &entry.0;
        let span = key.span;
        self.entry = Some(entry);
        seed.deserialize(NodeDeserializer::<M>::with_mode(key))
            .map(Some)
            .map_err(|error| with_span(error, span))
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let (key, value) = self.entry.take().ok_or_else(|| {
            Error::coded(
                ErrorKind::Serde,
                ErrorCode::MapValueBeforeKey,
                "map value requested before key",
            )
        })?;
        let span = value.span;
        seed.deserialize(NodeDeserializer::<M>::with_mode(value))
            .map_err(|error| inside_key(with_span(error, span), key))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct NodeEnumAccess<'src, 'a, 'e, M> {
    variant: String,
    variant_span: Span,
    value: NodeValue<'src, 'a>,
    /// Whether an error inside names the variant. A target that isn't an enum takes the tag as a
    /// tag, and the document has no key by that name.
    in_path: bool,
    /// Set to `true` once the visitor reads the variant name, so a refusal before that is known
    /// to be a refusal of the tag itself.
    entered: Option<&'e mut bool>,
    mode: PhantomData<M>,
}

impl<'src, 'de, 'a, M: StrMode<'src, 'de>> EnumAccess<'de> for NodeEnumAccess<'src, 'a, '_, M> {
    type Error = Error;
    type Variant = NodeVariantAccess<'src, 'a, M>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        mut self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        if let Some(entered) = self.entered.take() {
            *entered = true;
        }
        let node = Node::new(
            NodeKind::String(Cow::Owned(self.variant)),
            self.variant_span,
        );
        let variant = seed
            .deserialize(NodeDeserializer::<M>::with_mode(&node))
            .map_err(|error| with_span(error, self.variant_span))?;
        Ok((
            variant,
            NodeVariantAccess {
                name: self.in_path.then_some(node),
                value: self.value,
                mode: PhantomData,
            },
        ))
    }
}

struct NodeVariantAccess<'src, 'a, M> {
    /// The variant's name, if an error inside gives it.
    name: Option<Node<'src>>,
    value: NodeValue<'src, 'a>,
    mode: PhantomData<M>,
}

impl<M> NodeVariantAccess<'_, '_, M> {
    #[cold]
    fn inside_variant(&self, error: Error) -> Error {
        match &self.name {
            Some(name) => inside_key(error, name),
            None => error,
        }
    }
}

impl<'src, 'de, M: StrMode<'src, 'de>> VariantAccess<'de> for NodeVariantAccess<'src, '_, M> {
    type Error = Error;

    /// A variant's value reads past an application tag, as a target that isn't an enum does, so
    /// `Nothing: !t` is the unit variant `Nothing`.
    fn unit_variant(self) -> Result<(), Self::Error> {
        let node = untagged(self.value.node());
        if matches!(&node.kind, NodeKind::Null) {
            Ok(())
        } else {
            Err(self.inside_variant(enum_error("expected a unit enum variant", node.span)))
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        let node = self.value.node();
        let span = node.span;
        seed.deserialize(NodeDeserializer::<M>::with_mode(node))
            .map_err(|error| self.inside_variant(with_span(error, span)))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        length: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::<M>::with_mode(self.value.node())
            .deserialize_tuple(length, visitor)
            .map_err(|error| self.inside_variant(error))
    }

    /// A tag with no value, as in `!Point`, fills a struct variant's defaults, as an empty value
    /// fills a struct. A bare name, as in `Point`, names a unit variant, so it fills none.
    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let result = match &self.value {
            NodeValue::Ref(node) => {
                NodeDeserializer::<M>::with_mode(node).deserialize_struct("", fields, visitor)
            }
            NodeValue::Null(node) => {
                NodeDeserializer::<M>::with_mode(node).deserialize_any(visitor)
            }
        };
        result.map_err(|error| self.inside_variant(error))
    }
}

/// Say that an error came from inside the value of this key. A scalar key reads as its value,
/// so `0x10:` and `16:` are one path, whichever typed reader reports it.
#[cold]
pub(super) fn inside_key(error: Error, key: &Node<'_>) -> Error {
    let mut node = resolve(key);
    while let NodeKind::Tagged(_, inner) = &node.kind {
        node = resolve(inner);
    }
    match &node.kind {
        NodeKind::String(text) => error.inside_key(text),
        NodeKind::Null => error.inside_key("null"),
        NodeKind::Bool(value) => error.inside_key(&value.to_string()),
        NodeKind::Number(value) => error.inside_key(&value.to_string()),
        _ => error.inside_collection_key(),
    }
}
