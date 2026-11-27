//! Serde [`Deserializer`] implementations for [`Tape`] and [`TapeRef`].

use serde::Deserializer;
use serde::de::{
    DeserializeSeed, EnumAccess, MapAccess, SeqAccess, Unexpected, VariantAccess, Visitor,
};

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::resolver::float_from_text;
use crate::serde_impl::tag_refused;
use crate::span::{
    SPANNED_END_FIELD, SPANNED_FIELDS, SPANNED_START_FIELD, SPANNED_TOKEN, SPANNED_VALUE_FIELD,
    Span, TrustedSpanGuard,
};

use super::{
    FLAG_PLAIN_SCALAR, FLAG_RADIX_INT, NodeTag, Tape, TapeMapIter, TapeRef, TapeSeqIter,
    TapeStorage,
};

fn with_loc(error: Error, span: Span, input: &str) -> Error {
    let error = if error.span().is_none() {
        error.with_span(span)
    } else {
        error
    };
    error.with_input_location(input, false)
}

fn enum_error(message: &'static str, span: Span, input: &str) -> Error {
    Error::coded(
        ErrorKind::Serde,
        ErrorCode::InvalidEnumRepresentation,
        message,
    )
    .with_span(span)
    .with_input_location(input, false)
}

fn check_consumed(remaining: usize, len: usize, label: &str) -> Result<(), Error> {
    if remaining == 0 {
        return Ok(());
    }
    Err(serde::de::Error::invalid_length(
        len,
        &format!("fewer elements in the {label}").as_str(),
    ))
}

fn unexpected_tape<'a>(node: TapeRef<'_, 'a>) -> Unexpected<'a> {
    let inner = node.untag_non_core();
    let raw = inner.node();
    match raw.tag() {
        NodeTag::EmptyNull | NodeTag::ExplicitNull => Unexpected::Unit,
        NodeTag::Bool => Unexpected::Bool(raw.payload() != 0),
        NodeTag::PosInt => Unexpected::Unsigned(raw.payload()),
        NodeTag::NegInt => Unexpected::Signed(raw.payload().cast_signed()),
        NodeTag::Float => Unexpected::Float(f64::from_bits(raw.payload())),
        NodeTag::WideInt => Unexpected::Other("128-bit integer"),
        NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
            if let Some(s) = inner.as_borrowed_str() {
                Unexpected::Str(s)
            } else {
                Unexpected::Other("string")
            }
        }
        NodeTag::Sequence => Unexpected::Seq,
        NodeTag::Mapping => Unexpected::Map,
        NodeTag::Tagged => Unexpected::Enum,
    }
}

fn inside_key(error: Error, key: TapeRef<'_, '_>) -> Error {
    let k = key.untag();
    match k.node().tag() {
        NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
            error.inside_key(k.as_str().unwrap_or(""))
        }
        NodeTag::EmptyNull | NodeTag::ExplicitNull => error.inside_key("null"),
        NodeTag::Bool => error.inside_key(if k.node().payload() != 0 {
            "true"
        } else {
            "false"
        }),
        NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
            if let Some(num) = k.as_number() {
                error.inside_key(&num.to_string())
            } else {
                error
            }
        }
        NodeTag::Sequence | NodeTag::Mapping | NodeTag::Tagged => error.inside_collection_key(),
    }
}

macro_rules! forward_to_root {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                self.root().$method(visitor)
            }
        )*
    };
}

impl<'de> Deserializer<'de> for Tape<'de> {
    type Error = Error;

    forward_to_root! {
        deserialize_any deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32
        deserialize_i64 deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32
        deserialize_u64 deserialize_u128 deserialize_f32 deserialize_f64 deserialize_char
        deserialize_str deserialize_string deserialize_bytes deserialize_byte_buf
        deserialize_option deserialize_unit deserialize_seq deserialize_map
        deserialize_identifier deserialize_ignored_any
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_enum(name, variants, visitor)
    }
}

impl<'de> Deserializer<'de> for &Tape<'de> {
    type Error = Error;

    forward_to_root! {
        deserialize_any deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32
        deserialize_i64 deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32
        deserialize_u64 deserialize_u128 deserialize_f32 deserialize_f64 deserialize_char
        deserialize_str deserialize_string deserialize_bytes deserialize_byte_buf
        deserialize_option deserialize_unit deserialize_seq deserialize_map
        deserialize_identifier deserialize_ignored_any
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.root().deserialize_enum(name, variants, visitor)
    }
}

macro_rules! untagged_plain_targets {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                self.untag_non_core().deserialize_any(visitor)
            }
        )*
    };
}

impl TapeRef<'_, '_> {
    /// Strip only non-core tags (`!custom`, `!`), leaving unresolved YAML core tags
    /// (such as `!!binary` or `!!unknown`) intact so they surface as `tagged value` errors,
    /// matching `typed::NodeDeserializer` and `serde_impl::from_value`.
    #[inline]
    fn untag_non_core(self) -> Self {
        let mut idx = self.index;
        while self.storage.node(idx).tag() == NodeTag::Tagged {
            if self.storage.tag_at(idx).is_some_and(crate::Tag::is_core) {
                break;
            }
            idx += 1;
        }
        Self {
            storage: self.storage,
            index: idx,
        }
    }
}

impl<'de> Deserializer<'de> for TapeRef<'_, 'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let span = self.span();
        let input = self.storage.input;
        let raw = self.node();
        let result = match raw.tag() {
            NodeTag::EmptyNull | NodeTag::ExplicitNull => visitor.visit_unit(),
            NodeTag::Bool => visitor.visit_bool(raw.payload() != 0),
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
                if let Some(num) = self.storage.number_at(self.index) {
                    num.visit(visitor)
                } else {
                    visitor.visit_unit()
                }
            }
            NodeTag::BorrowedStr => {
                if let Some(s) = self.storage.borrowed_str_at(self.index) {
                    visitor.visit_borrowed_str(s)
                } else {
                    visitor.visit_str("")
                }
            }
            NodeTag::DecodedStr => visitor.visit_str(self.storage.str_at(self.index).unwrap_or("")),
            NodeTag::LargeStr => {
                if let Some(s) = self.storage.borrowed_str_at(self.index) {
                    visitor.visit_borrowed_str(s)
                } else {
                    visitor.visit_str(self.storage.str_at(self.index).unwrap_or(""))
                }
            }
            NodeTag::Sequence => {
                let iter = self.as_sequence().unwrap_or(TapeSeqIter {
                    storage: self.storage,
                    cursor: 0,
                    end: 0,
                    remaining: 0,
                });
                let len = iter.len();
                let mut access = TapeSeqAccess { iter, index: 0 };
                visitor
                    .visit_seq(&mut access)
                    .and_then(|v| check_consumed(access.iter.len(), len, "sequence").map(|()| v))
            }
            NodeTag::Mapping => {
                let iter = self.as_mapping().unwrap_or(TapeMapIter {
                    storage: self.storage,
                    cursor: 0,
                    end: 0,
                    remaining: 0,
                });
                let len = iter.len();
                let mut access = TapeMapAccess {
                    iter,
                    pending_value: None,
                };
                visitor
                    .visit_map(&mut access)
                    .and_then(|v| check_consumed(access.iter.len(), len, "map").map(|()| v))
            }
            NodeTag::Tagged => {
                let Some((tag, inner)) = self.as_tagged() else {
                    return visitor.visit_unit().map_err(|e| with_loc(e, span, input));
                };
                let mut entered = false;
                return visitor
                    .visit_enum(TapeEnumAccess {
                        variant: tag.serde_name(),
                        variant_span: span,
                        value: EnumPayload::Node(inner),
                        in_path: false,
                        entered: Some(&mut entered),
                        input,
                    })
                    .map_err(|error| with_loc(tag_refused(error, tag, entered), span, input));
            }
        };
        result.map_err(|error| with_loc(error, span, input))
    }

    untagged_plain_targets! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_u128 deserialize_unit
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.untag_non_core().deserialize_any(visitor)
    }

    fn deserialize_f64<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let inner = self.untag_non_core();
        let span = inner.span();
        let input = inner.storage.input;
        let raw = inner.node();
        match raw.tag() {
            NodeTag::Float => visitor
                .visit_f64(f64::from_bits(raw.payload()))
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::WideInt => {
                if raw.has_flag(FLAG_RADIX_INT) {
                    let spelling = inner
                        .as_number()
                        .map_or_else(|| "integer".to_owned(), |n| n.to_string());
                    return Err(with_loc(
                        Error::coded(
                            ErrorKind::Serde,
                            ErrorCode::InvalidType,
                            format!("invalid type: integer `{spelling}`, expected f64"),
                        ),
                        span,
                        input,
                    ));
                }
                if let Some(num) = inner.as_number()
                    && let Some(val) = num.as_f64()
                {
                    return visitor.visit_f64(val).map_err(|e| with_loc(e, span, input));
                }
                inner.deserialize_any(visitor)
            }
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr
                if raw.has_flag(FLAG_PLAIN_SCALAR) =>
            {
                if let Some(text) = inner.as_str()
                    && let Some(val) = float_from_text(text)
                {
                    return visitor.visit_f64(val).map_err(|e| with_loc(e, span, input));
                }
                inner.deserialize_any(visitor)
            }
            _ => inner.deserialize_any(visitor),
        }
    }

    fn deserialize_f32<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_f64(visitor)
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let span = self.span();
        let input = self.storage.input;
        if matches!(
            self.node().tag(),
            NodeTag::EmptyNull | NodeTag::ExplicitNull
        ) {
            visitor.visit_none().map_err(|e| with_loc(e, span, input))
        } else {
            visitor
                .visit_some(self)
                .map_err(|e| with_loc(e, span, input))
        }
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let span = self.span();
        let input = self.storage.input;
        visitor
            .visit_newtype_struct(self)
            .map_err(|e| with_loc(e, span, input))
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_string(visitor)
    }

    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let inner = self.untag();
        let span = inner.span();
        let input = inner.storage.input;
        match inner.node().tag() {
            NodeTag::BorrowedStr => {
                if let Some(s) = inner.as_borrowed_str() {
                    visitor
                        .visit_borrowed_str(s)
                        .map_err(|e| with_loc(e, span, input))
                } else {
                    visitor.visit_str("").map_err(|e| with_loc(e, span, input))
                }
            }
            NodeTag::DecodedStr => visitor
                .visit_str(inner.as_str().unwrap_or(""))
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::LargeStr => {
                if let Some(s) = inner.as_borrowed_str() {
                    visitor
                        .visit_borrowed_str(s)
                        .map_err(|e| with_loc(e, span, input))
                } else {
                    visitor
                        .visit_str(inner.as_str().unwrap_or(""))
                        .map_err(|e| with_loc(e, span, input))
                }
            }
            NodeTag::EmptyNull if inner.node().has_flag(FLAG_PLAIN_SCALAR) => visitor
                .visit_borrowed_str("")
                .map_err(|e| with_loc(e, span, input)),
            _ => inner.deserialize_any(visitor),
        }
    }

    fn deserialize_bytes<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let inner = self.untag();
        let span = inner.span();
        let input = inner.storage.input;
        match inner.node().tag() {
            NodeTag::BorrowedStr => {
                if let Some(s) = inner.as_borrowed_str() {
                    visitor
                        .visit_borrowed_bytes(s.as_bytes())
                        .map_err(|e| with_loc(e, span, input))
                } else {
                    visitor
                        .visit_bytes(&[])
                        .map_err(|e| with_loc(e, span, input))
                }
            }
            NodeTag::DecodedStr => visitor
                .visit_bytes(inner.as_str().unwrap_or("").as_bytes())
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::LargeStr => {
                if let Some(s) = inner.as_borrowed_str() {
                    visitor
                        .visit_borrowed_bytes(s.as_bytes())
                        .map_err(|e| with_loc(e, span, input))
                } else {
                    visitor
                        .visit_bytes(inner.as_str().unwrap_or("").as_bytes())
                        .map_err(|e| with_loc(e, span, input))
                }
            }
            _ => inner.deserialize_any(visitor),
        }
    }

    fn deserialize_byte_buf<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_bytes(visitor)
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_scalar_as_text(visitor)
    }

    fn deserialize_char<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.deserialize_scalar_as_text(visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let inner = self.untag_non_core();
        let span = inner.span();
        let input = inner.storage.input;
        match inner.node().tag() {
            NodeTag::EmptyNull => visitor
                .visit_seq(EmptySeqAccess)
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::ExplicitNull => Err(with_loc(
                serde::de::Error::invalid_type(Unexpected::Unit, &visitor),
                span,
                input,
            )),
            _ => inner.deserialize_any(visitor),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.untag_non_core().deserialize_any(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.untag_non_core().deserialize_any(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        let inner = self.untag_non_core();
        let span = inner.span();
        let input = inner.storage.input;
        match inner.node().tag() {
            NodeTag::EmptyNull => visitor
                .visit_map(EmptyMapAccess)
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::ExplicitNull => Err(with_loc(
                serde::de::Error::invalid_type(Unexpected::Unit, &visitor),
                span,
                input,
            )),
            _ => inner.deserialize_any(visitor),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let input = self.storage.input;
        if name == SPANNED_TOKEN && fields == SPANNED_FIELDS {
            let span = self.span();
            let _guard = TrustedSpanGuard::enter(span);
            return visitor
                .visit_map(SpannedTapeAccess {
                    node: Some(self),
                    span,
                    state: 0,
                })
                .map_err(|e| with_loc(e, span, input));
        }
        let inner = self.untag_non_core();
        let span = inner.span();
        match inner.node().tag() {
            NodeTag::EmptyNull => visitor
                .visit_map(EmptyMapAccess)
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::ExplicitNull => Err(with_loc(
                serde::de::Error::invalid_type(Unexpected::Unit, &visitor),
                span,
                input,
            )),
            NodeTag::Mapping | NodeTag::Tagged => inner.deserialize_any(visitor),
            _ => Err(with_loc(
                serde::de::Error::invalid_type(unexpected_tape(inner), &visitor),
                span,
                input,
            )),
        }
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let span = self.span();
        let input = self.storage.input;
        let (variant, variant_span, value) = match self.node().tag() {
            NodeTag::Tagged => {
                let Some((tag, inner)) = self.as_tagged() else {
                    return Err(enum_error(
                        "enum value must be tagged or a single-entry mapping",
                        span,
                        input,
                    ));
                };
                (tag.variant_name(), span, EnumPayload::Node(inner))
            }
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => (
                self.as_str().unwrap_or("").to_owned(),
                span,
                EnumPayload::Nothing(span),
            ),
            NodeTag::Mapping => {
                let mut iter = self.as_mapping().unwrap_or(TapeMapIter {
                    storage: self.storage,
                    cursor: 0,
                    end: 0,
                    remaining: 0,
                });
                let Some((key, val)) = iter.next() else {
                    return Err(enum_error(
                        "enum mapping must contain one variant",
                        span,
                        input,
                    ));
                };
                if iter.next().is_some() {
                    return Err(enum_error(
                        "enum mapping must contain one variant",
                        span,
                        input,
                    ));
                }
                let variant_span = key.span();
                let key_node = match key.as_tagged() {
                    Some((tag, inner)) if tag.is_non_specific() => inner,
                    _ => key,
                };
                let variant_str = if matches!(
                    key_node.node().tag(),
                    NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr
                ) {
                    key_node.storage.str_at(key_node.index)
                } else {
                    None
                };
                let Some(variant_str) = variant_str else {
                    return Err(enum_error(
                        "enum variant key must be a string",
                        variant_span,
                        input,
                    ));
                };
                (variant_str.to_owned(), variant_span, EnumPayload::Node(val))
            }
            _ => {
                return Err(with_loc(
                    serde::de::Error::invalid_type(unexpected_tape(self), &visitor),
                    span,
                    input,
                ));
            }
        };
        visitor
            .visit_enum(TapeEnumAccess {
                variant,
                variant_span,
                value,
                in_path: true,
                entered: None,
                input,
            })
            .map_err(|e| with_loc(e, span, input))
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor
            .visit_unit()
            .map_err(|e| with_loc(e, self.span(), self.storage.input))
    }
}

impl<'de> TapeRef<'_, 'de> {
    fn deserialize_scalar_as_text<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        let inner = self.untag();
        let span = inner.span();
        let input = inner.storage.input;
        match inner.node().tag() {
            NodeTag::BorrowedStr | NodeTag::DecodedStr | NodeTag::LargeStr => {
                inner.deserialize_string(visitor)
            }
            NodeTag::EmptyNull | NodeTag::ExplicitNull => visitor
                .visit_str("null")
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::Bool => visitor
                .visit_str(if inner.node().payload() != 0 {
                    "true"
                } else {
                    "false"
                })
                .map_err(|e| with_loc(e, span, input)),
            NodeTag::PosInt | NodeTag::NegInt | NodeTag::Float | NodeTag::WideInt => {
                if let Some(num) = inner.as_number() {
                    visitor
                        .visit_string(num.to_string())
                        .map_err(|e| with_loc(e, span, input))
                } else {
                    inner.deserialize_any(visitor)
                }
            }
            _ => inner.deserialize_any(visitor),
        }
    }
}

struct TapeSeqAccess<'t, 'a> {
    iter: TapeSeqIter<'t, 'a>,
    index: usize,
}

impl<'de> SeqAccess<'de> for TapeSeqAccess<'_, 'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let Some(item) = self.iter.next() else {
            return Ok(None);
        };
        let idx = self.index;
        self.index += 1;
        seed.deserialize(item)
            .map(Some)
            .map_err(|error| error.inside_index(idx))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

struct TapeMapAccess<'t, 'a> {
    iter: TapeMapIter<'t, 'a>,
    pending_value: Option<(TapeRef<'t, 'a>, TapeRef<'t, 'a>)>,
}

impl<'de> MapAccess<'de> for TapeMapAccess<'_, 'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let Some((key, value)) = self.iter.next() else {
            self.pending_value = None;
            return Ok(None);
        };
        self.pending_value = Some((key, value));
        seed.deserialize(key).map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let Some((key, value)) = self.pending_value.take() else {
            return Err(Error::coded(
                ErrorKind::Serde,
                ErrorCode::MapValueBeforeKey,
                "map value requested before key",
            ));
        };
        seed.deserialize(value).map_err(|err| inside_key(err, key))
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
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

    fn size_hint(&self) -> Option<usize> {
        Some(0)
    }
}

/// Sentinel deserializer for a bare string enum variant (fills unit / `Option`, rejects collections).
struct NothingDeserializer<'de> {
    span: Span,
    input: &'de str,
}

impl<'de> Deserializer<'de> for NothingDeserializer<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor
            .visit_unit()
            .map_err(|e| with_loc(e, self.span, self.input))
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        visitor
            .visit_none()
            .map_err(|e| with_loc(e, self.span, self.input))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let span = self.span;
        let input = self.input;
        visitor
            .visit_newtype_struct(self)
            .map_err(|e| with_loc(e, span, input))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        if name == SPANNED_TOKEN && fields == SPANNED_FIELDS {
            let _guard = TrustedSpanGuard::enter(self.span);
            return visitor
                .visit_map(SpannedNothingAccess {
                    span: self.span,
                    input: self.input,
                    state: 0,
                })
                .map_err(|e| with_loc(e, self.span, self.input));
        }
        Err(with_loc(
            serde::de::Error::invalid_type(Unexpected::Unit, &visitor),
            self.span,
            self.input,
        ))
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct
        map enum identifier ignored_any
    }
}

struct SpannedNothingAccess<'de> {
    span: Span,
    input: &'de str,
    state: u8,
}

impl<'de> MapAccess<'de> for SpannedNothingAccess<'de> {
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
        self.state += 1;
        seed.deserialize(serde::de::value::StrDeserializer::<Error>::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        match self.state {
            1 => seed.deserialize(serde::de::value::UsizeDeserializer::<Error>::new(
                self.span.start(),
            )),
            2 => seed.deserialize(serde::de::value::UsizeDeserializer::<Error>::new(
                self.span.end(),
            )),
            3 => seed.deserialize(NothingDeserializer {
                span: self.span,
                input: self.input,
            }),
            _ => Err(Error::new(
                ErrorKind::Serde,
                "spanned map value requested past end",
            )),
        }
    }
}

struct SpannedTapeAccess<'t, 'a> {
    node: Option<TapeRef<'t, 'a>>,
    span: Span,
    state: u8,
}

impl<'de> MapAccess<'de> for SpannedTapeAccess<'_, 'de> {
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
        self.state += 1;
        seed.deserialize(serde::de::value::StrDeserializer::<Error>::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        match self.state {
            1 => seed.deserialize(serde::de::value::UsizeDeserializer::<Error>::new(
                self.span.start(),
            )),
            2 => seed.deserialize(serde::de::value::UsizeDeserializer::<Error>::new(
                self.span.end(),
            )),
            3 => {
                let Some(node) = self.node.take() else {
                    return Err(Error::new(
                        ErrorKind::Serde,
                        "spanned value already consumed",
                    ));
                };
                seed.deserialize(node)
            }
            _ => Err(Error::new(
                ErrorKind::Serde,
                "spanned map value requested past end",
            )),
        }
    }
}

enum EnumPayload<'t, 'a> {
    Node(TapeRef<'t, 'a>),
    Nothing(Span),
}

struct TapeEnumAccess<'t, 'a, 'e> {
    variant: String,
    variant_span: Span,
    value: EnumPayload<'t, 'a>,
    in_path: bool,
    entered: Option<&'e mut bool>,
    input: &'a str,
}

impl<'t, 'de> EnumAccess<'de> for TapeEnumAccess<'t, 'de, '_> {
    type Error = Error;
    type Variant = TapeVariantAccess<'t, 'de>;

    fn variant_seed<V: DeserializeSeed<'de>>(
        mut self,
        seed: V,
    ) -> Result<(V::Value, Self::Variant), Self::Error> {
        if let Some(entered) = self.entered.take() {
            *entered = true;
        }
        let variant = seed
            .deserialize(serde::de::value::StrDeserializer::<Error>::new(
                &self.variant,
            ))
            .map_err(|e| with_loc(e, self.variant_span, self.input))?;
        Ok((
            variant,
            TapeVariantAccess {
                name: self.variant,
                value: self.value,
                in_path: self.in_path,
                input: self.input,
            },
        ))
    }
}

struct TapeVariantAccess<'t, 'a> {
    name: String,
    value: EnumPayload<'t, 'a>,
    in_path: bool,
    input: &'a str,
}

impl TapeVariantAccess<'_, '_> {
    fn wrap_error(&self, error: Error) -> Error {
        if self.in_path {
            error.inside_key(&self.name)
        } else {
            error
        }
    }
}

impl<'de> VariantAccess<'de> for TapeVariantAccess<'_, 'de> {
    type Error = Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        match self.value {
            EnumPayload::Nothing(_) => Ok(()),
            EnumPayload::Node(node) => {
                let inner = node.untag_non_core();
                if matches!(
                    inner.node().tag(),
                    NodeTag::EmptyNull | NodeTag::ExplicitNull
                ) {
                    Ok(())
                } else {
                    Err(self.wrap_error(enum_error(
                        "expected a unit enum variant",
                        node.span(),
                        self.input,
                    )))
                }
            }
        }
    }

    fn newtype_variant_seed<T: DeserializeSeed<'de>>(
        self,
        seed: T,
    ) -> Result<T::Value, Self::Error> {
        let res = match self.value {
            EnumPayload::Node(node) => seed.deserialize(node),
            EnumPayload::Nothing(span) => seed.deserialize(NothingDeserializer {
                span,
                input: self.input,
            }),
        };
        res.map_err(|e| self.wrap_error(e))
    }

    fn tuple_variant<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let res = match self.value {
            EnumPayload::Node(node) => node.deserialize_tuple(len, visitor),
            EnumPayload::Nothing(span) => NothingDeserializer {
                span,
                input: self.input,
            }
            .deserialize_tuple(len, visitor),
        };
        res.map_err(|e| self.wrap_error(e))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        let res = match self.value {
            EnumPayload::Node(node) => node.deserialize_struct("", fields, visitor),
            EnumPayload::Nothing(span) => NothingDeserializer {
                span,
                input: self.input,
            }
            .deserialize_struct("", fields, visitor),
        };
        res.map_err(|e| self.wrap_error(e))
    }
}

#[allow(
    dead_code,
    reason = "ensures TapeStorage reference stays covariant in 'a"
)]
const fn _assert_tape_storage_covariant<'a>(s: &'a TapeStorage<'a>) -> &'a TapeStorage<'a> {
    s
}
