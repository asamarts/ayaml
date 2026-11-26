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
use crate::span::{
    SPANNED_END_FIELD, SPANNED_FIELDS, SPANNED_START_FIELD, SPANNED_TOKEN, SPANNED_VALUE_FIELD,
    Span,
};

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

enum NodeRef<'src, 'a> {
    Borrowed(&'a Node<'src>),
    Owned(Node<'src>),
}

impl<'src> NodeRef<'src, '_> {
    #[inline]
    fn as_ref(&self) -> &Node<'src> {
        match self {
            Self::Borrowed(node) => node,
            Self::Owned(node) => node,
        }
    }
}

pub(super) struct NodeDeserializer<'src, 'a, M = BorrowedMode> {
    node: NodeRef<'src, 'a>,
    mode: PhantomData<M>,
}

/// Deserializer methods for integer targets, parsing a plain scalar whose global tag was dropped.
macro_rules! int_targets {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                let node = untagged(self.node.as_ref());
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
                NodeDeserializer::<M>::with_mode(untagged(self.node.as_ref())).deserialize_any(visitor)
            }
        )*
    };
}

impl<'src, 'a> NodeDeserializer<'src, 'a, BorrowedMode> {
    pub(super) fn new(node: &'a Node<'src>) -> Self {
        Self {
            node: NodeRef::Borrowed(node),
            mode: PhantomData,
        }
    }

    pub(super) fn from_owned(node: Node<'src>) -> Self {
        Self {
            node: NodeRef::Owned(node),
            mode: PhantomData,
        }
    }
}

impl<'src> NodeDeserializer<'src, '_, OwnedMode> {
    pub(super) fn owned_from_owned(node: Node<'src>) -> Self {
        Self {
            node: NodeRef::Owned(node),
            mode: PhantomData,
        }
    }
}

impl<'src, 'a, M> NodeDeserializer<'src, 'a, M> {
    fn with_mode(node: &'a Node<'src>) -> Self {
        Self {
            node: NodeRef::Borrowed(node),
            mode: PhantomData,
        }
    }

    fn from_ref(node: NodeRef<'src, 'a>) -> Self {
        Self {
            node,
            mode: PhantomData,
        }
    }

    /// The node to read. Aliases form no cycles: an anchor is defined only
    /// once its node is complete, so an alias cannot reach itself.
    fn target(&self) -> &Node<'src> {
        resolve(self.node.as_ref())
    }
}

impl<'src, 'de, M: StrMode<'src, 'de>> Deserializer<'de> for NodeDeserializer<'src, '_, M> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if let NodeRef::Owned(Node {
            kind: NodeKind::String(text),
            span,
            ..
        }) = self.node
        {
            let result = match text {
                Cow::Borrowed(value) => M::visit_src_str(visitor, value),
                Cow::Owned(value) => visitor.visit_string(value),
            };
            return result.map_err(|error| with_span(error, span));
        }
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
                .visit_some(self)
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
        if let NodeRef::Owned(Node {
            kind: NodeKind::String(text),
            span,
            ..
        }) = self.node
        {
            let result = match text {
                Cow::Borrowed(value) => M::visit_src_str(visitor, value),
                Cow::Owned(value) => visitor.visit_string(value),
            };
            return result.map_err(|error| with_span(error, span));
        }
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
                Text::None | Text::CollectionEnd(_) | Text::Nothing | Text::DroppedTag => {
                    return NodeDeserializer::<M>::with_mode(node)
                        .deserialize_any(visitor)
                        .map_err(|error| with_span(error, span));
                }
            },
        };
        result.map_err(|error| with_span(error, span))
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if let NodeRef::Owned(Node {
            kind: NodeKind::String(text),
            span,
            ..
        }) = self.node
        {
            let result = match text {
                Cow::Borrowed(value) => M::visit_src_bytes(visitor, value.as_bytes()),
                Cow::Owned(value) => visitor.visit_byte_buf(value.into_bytes()),
            };
            return result.map_err(|error| with_span(error, span));
        }
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
        let node = untagged(self.node.as_ref());
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
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if name == SPANNED_TOKEN && fields == SPANNED_FIELDS {
            // Preserve the outer node's full span (e.g. alias use-site span or full collection
            // span) before resolving aliases.
            let span = self.node.as_ref().full_span();
            let _guard = crate::span::TrustedSpanGuard::enter(span);
            return visitor
                .visit_map(SpannedMapAccess::<M> {
                    node: Some(self.node),
                    span,
                    state: 0,
                    mode: PhantomData,
                })
                .map_err(|error| with_span(error, span));
        }
        let node = untagged(self.node.as_ref());
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
        let node = untagged(self.node.as_ref());
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
        let node = untagged(self.node.as_ref());
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
        NodeDeserializer::<M>::with_mode(untagged(self.node.as_ref())).deserialize_any(visitor)
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
        let node = untagged(self.node.as_ref());
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

/// Synthetic [`MapAccess`] supplying `(start, end, value)` to [`crate::Spanned`].
struct SpannedMapAccess<'src, 'a, M> {
    node: Option<NodeRef<'src, 'a>>,
    span: Span,
    state: u8,
    mode: PhantomData<M>,
}

impl<'src, 'de, M: StrMode<'src, 'de>> MapAccess<'de> for SpannedMapAccess<'src, '_, M> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let key = match self.state {
            0 => SPANNED_START_FIELD,
            1 => SPANNED_END_FIELD,
            2 => SPANNED_VALUE_FIELD,
            _ => return Ok(None),
        };
        seed.deserialize(serde::de::value::BorrowedStrDeserializer::<Error>::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let step = self.state;
        self.state += 1;
        match step {
            0 => seed.deserialize(serde::de::value::U64Deserializer::<Error>::new(
                self.span.start() as u64,
            )),
            1 => seed.deserialize(serde::de::value::U64Deserializer::<Error>::new(
                self.span.end() as u64,
            )),
            2 => {
                let Some(node) = self.node.take() else {
                    return Err(Error::new(
                        ErrorKind::Serde,
                        "Spanned<T> deserializer state exhausted",
                    ));
                };
                seed.deserialize(NodeDeserializer::<M>::from_ref(node))
            }
            _ => Err(Error::new(
                ErrorKind::Serde,
                "Spanned<T> deserializer state exhausted",
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::marker::PhantomData;

    use serde::de::{Deserialize, Deserializer, Visitor};

    use crate::{Deserializer as YamlDeserializer, Spanned, from_str};

    struct OwnedBytes(Vec<u8>);

    impl<'de> Deserialize<'de> for OwnedBytes {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct BytesVisitor;
            impl Visitor<'_> for BytesVisitor {
                type Value = OwnedBytes;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("owned byte buffer")
                }
                fn visit_byte_buf<E: serde::de::Error>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                    Ok(OwnedBytes(v))
                }
            }
            deserializer.deserialize_byte_buf(BytesVisitor)
        }
    }

    #[test]
    fn owned_node_moves_escaped_strings_through_spanned_option_and_owned_mode() {
        // Exceed the 4 KiB threshold so the streaming reader builds and moves owned Nodes via
        // `NodeDeserializer::from_owned` (`BorrowedMode`), and exercise `owned_from_owned`
        // (`OwnedMode`) via `Deserializer::next_seed`.
        let mut input = String::from("spanned: \"hello\\nworld\"\noptional: \"opt\\tval\"\n");
        for idx in 0..220 {
            use std::fmt::Write as _;
            let _ = writeln!(input, "pad{idx}: \"line-{idx}\\nend\"");
        }
        assert!(input.len() > 4096);

        let map: BTreeMap<String, Option<Spanned<String>>> =
            from_str(&input).expect("streamed owned decode");
        let spanned = map
            .get("spanned")
            .and_then(Option::as_ref)
            .expect("spanned entry");
        assert_eq!(spanned.get_ref(), "hello\nworld");
        let optional = map
            .get("optional")
            .and_then(Option::as_ref)
            .expect("optional entry");
        assert_eq!(optional.get_ref(), "opt\tval");
        let last = map
            .get("pad219")
            .and_then(Option::as_ref)
            .expect("pad219 entry");
        assert_eq!(last.get_ref(), "line-219\nend");

        // Exercise single-root owned string move through `Option<Spanned<String>>` and `visit_byte_buf`.
        let long_root = format!("\"{}\\n\"", "x".repeat(5_000));
        let root: Option<Spanned<String>> = from_str(&long_root).expect("root spanned string");
        assert_eq!(root.expect("some").into_inner().len(), 5_001);
        let bytes: OwnedBytes = from_str(&long_root).expect("root owned byte_buf");
        assert_eq!(bytes.0.len(), 5_001);

        // Exercise `NodeDeserializer::owned_from_owned` (`OwnedMode`) through `Deserializer::next_seed`.
        let mut de = YamlDeserializer::from_str(&long_root).expect("deserializer");
        let owned_spanned = de
            .next_seed(PhantomData::<Option<Spanned<String>>>)
            .transpose()
            .expect("owned spanned")
            .expect("one doc");
        assert_eq!(owned_spanned.expect("some").into_inner().len(), 5_001);

        let mut de = YamlDeserializer::from_str(&long_root).expect("deserializer");
        let owned_bytes = de
            .next_seed(PhantomData::<OwnedBytes>)
            .transpose()
            .expect("owned bytes")
            .expect("one doc");
        assert_eq!(owned_bytes.0.len(), 5_001);
    }
}
