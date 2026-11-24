//! Source-backed single-document [`serde::Deserializer`] bridge (AY-143).

use std::fmt;

use serde::Deserializer;
use serde::de::Visitor;

use super::node_de::NodeDeserializer;
use super::{Node, NodeOptions, parse_document};
use crate::error::Error;
use crate::limits::Limits;
use crate::options::ParseOptions;

/// A source-backed single-document [`serde::Deserializer`] that preserves YAML source spans
/// and borrows simple strings directly from the input.
///
/// Unlike [`Deserializer`](crate::Deserializer) (a multi-document pull cursor),
/// `DocumentDeserializer` implements [`serde::Deserializer`] directly—both by value and by
/// `&mut`—so Serde middleware wrappers (such as path trackers or unknown-field collectors) can
/// wrap `ayaml`'s borrowed, span-preserving source deserializer without composing an
/// intermediate [`Value`](crate::Value).
///
/// ```
/// use serde::Deserialize;
///
/// #[derive(Debug, PartialEq, Deserialize)]
/// struct Config<'a> {
///     name: &'a str,
///     retries: u32,
/// }
///
/// let input = "name: ayaml\nretries: 3\n";
/// let de = ayaml::DocumentDeserializer::from_str(input)?;
/// let config = Config::deserialize(de)?;
/// assert_eq!(config, Config { name: "ayaml", retries: 3 });
/// # Ok::<(), ayaml::Error>(())
/// ```
pub struct DocumentDeserializer<'de> {
    input: &'de str,
    node: Node<'de>,
    compat_flow: bool,
    #[cfg(feature = "libyaml-compat")]
    compat_errors: bool,
}

impl fmt::Debug for DocumentDeserializer<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentDeserializer")
            .finish_non_exhaustive()
    }
}

impl<'de> DocumentDeserializer<'de> {
    /// Parse one YAML document under the default limits and return a [`serde::Deserializer`]
    /// backed by `input`.
    ///
    /// Trailing documents and duplicate mapping keys are rejected.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` is invalid YAML, contains trailing documents, or exceeds
    /// the default resource limits.
    #[inline]
    #[allow(
        clippy::should_implement_trait,
        reason = "DocumentDeserializer borrows from input with lifetime 'de, which std::str::FromStr cannot express"
    )]
    pub fn from_str(input: &'de str) -> Result<Self, Error> {
        Self::from_str_with_limits(input, &Limits::default_profile())
    }

    /// Parse one YAML document with explicit parse options and return a [`serde::Deserializer`]
    /// backed by `input`.
    ///
    /// # Errors
    ///
    /// Returns an error when `options.limits` is invalid, when `input` is invalid YAML or
    /// contains trailing documents, or when a configured resource limit is exceeded.
    #[inline]
    pub fn from_str_with(input: &'de str, options: &ParseOptions) -> Result<Self, Error> {
        if options.merge_keys {
            options.limits.validate()?;
            let (events, mut budget) =
                crate::merge::events::expand_single_document(input, &options.limits)?;
            let node = super::build(
                events,
                input.len(),
                &options.limits,
                NodeOptions::default(),
                &mut budget,
            )
            .map_err(|error| error.with_input_location(input, false))?;
            return Ok(Self {
                input,
                node,
                compat_flow: false,
                #[cfg(feature = "libyaml-compat")]
                compat_errors: false,
            });
        }
        Self::from_str_with_limits(input, &options.limits)
    }

    /// Parse one YAML document with explicit limits and return a [`serde::Deserializer`] backed
    /// by `input`.
    ///
    /// # Errors
    ///
    /// Returns an error when `limits` is invalid, when `input` is invalid YAML or contains
    /// trailing documents, or when a configured resource limit is exceeded.
    #[inline]
    pub fn from_str_with_limits(input: &'de str, limits: &Limits) -> Result<Self, Error> {
        limits.validate()?;
        let options = NodeOptions::default();
        let node = parse_document(input, limits, options)
            .map_err(|error| error.with_input_location(input, options.compat_flow))?;
        Ok(Self {
            input,
            node,
            compat_flow: false,
            #[cfg(feature = "libyaml-compat")]
            compat_errors: false,
        })
    }

    /// Parse one YAML document under the `libyaml-compat` rules and default limits, returning a
    /// [`serde::Deserializer`] backed by `input`.
    ///
    /// # Errors
    ///
    /// Returns an error when `input` is invalid under compatibility rules, contains trailing
    /// documents, or exceeds the default limits.
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    #[inline]
    pub fn from_str_compat(input: &'de str) -> Result<Self, Error> {
        Self::from_str_compat_with(input, &crate::compat::ParseOptions::new())
    }

    /// Parse one YAML document under the `libyaml-compat` rules with explicit compatibility
    /// options, returning a [`serde::Deserializer`] backed by `input`.
    ///
    /// # Errors
    ///
    /// Returns an error when `options.limits` is invalid, when `input` is invalid under
    /// compatibility rules or contains trailing documents, or when a configured limit is
    /// exceeded.
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    #[inline]
    pub fn from_str_compat_with(
        input: &'de str,
        options: &crate::compat::ParseOptions,
    ) -> Result<Self, Error> {
        options.limits.validate()?;
        let node_options = crate::compat::node_options(options.global_tags);
        let node = parse_document(input, &options.limits, node_options).map_err(|error| {
            crate::compat::map_error(error.with_input_location(input, node_options.compat_flow))
        })?;
        Ok(Self {
            input,
            node,
            compat_flow: node_options.compat_flow,
            compat_errors: true,
        })
    }

    /// Parse one YAML document under the `libyaml-compat` rules with explicit limits, returning
    /// a [`serde::Deserializer`] backed by `input`.
    ///
    /// # Errors
    ///
    /// Returns an error when `limits` is invalid, when `input` is invalid under compatibility
    /// rules or contains trailing documents, or when a configured limit is exceeded.
    #[cfg(feature = "libyaml-compat")]
    #[cfg_attr(docsrs, doc(cfg(feature = "libyaml-compat")))]
    #[inline]
    pub fn from_str_compat_with_limits(input: &'de str, limits: &Limits) -> Result<Self, Error> {
        let options = crate::compat::ParseOptions::new().with_limits(limits.clone());
        Self::from_str_compat_with(input, &options)
    }

    #[inline]
    fn finish_error(&self, error: Error) -> Error {
        let located = error.with_input_location(self.input, self.compat_flow);
        #[cfg(feature = "libyaml-compat")]
        if self.compat_errors {
            return crate::compat::map_error(located);
        }
        located
    }
}

macro_rules! forward_document_deserialize {
    ($($method:ident),* $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
                NodeDeserializer::new(&self.node)
                    .$method(visitor)
                    .map_err(|error| self.finish_error(error))
            }
        )*
    };
}

impl<'de> Deserializer<'de> for &mut DocumentDeserializer<'de> {
    type Error = Error;

    forward_document_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_unit_struct(name, visitor)
            .map_err(|error| self.finish_error(error))
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_newtype_struct(name, visitor)
            .map_err(|error| self.finish_error(error))
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_tuple(len, visitor)
            .map_err(|error| self.finish_error(error))
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_tuple_struct(name, len, visitor)
            .map_err(|error| self.finish_error(error))
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_struct(name, fields, visitor)
            .map_err(|error| self.finish_error(error))
    }

    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        NodeDeserializer::new(&self.node)
            .deserialize_enum(name, variants, visitor)
            .map_err(|error| self.finish_error(error))
    }
}

macro_rules! forward_owned_document_deserialize {
    ($($method:ident),* $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(mut self, visitor: V) -> Result<V::Value, Self::Error> {
                (&mut self).$method(visitor)
            }
        )*
    };
}

impl<'de> Deserializer<'de> for DocumentDeserializer<'de> {
    type Error = Error;

    forward_owned_document_deserialize!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_seq,
        deserialize_map,
        deserialize_identifier,
        deserialize_ignored_any,
    );

    fn deserialize_unit_struct<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        mut self,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_tuple(len, visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        len: usize,
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_tuple_struct(name, len, visitor)
    }

    fn deserialize_struct<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_struct(name, fields, visitor)
    }

    fn deserialize_enum<V: Visitor<'de>>(
        mut self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        (&mut self).deserialize_enum(name, variants, visitor)
    }
}
