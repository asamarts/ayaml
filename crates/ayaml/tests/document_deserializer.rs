//! Integration tests for `DocumentDeserializer` as a source-backed `serde::Deserializer` bridge
//! for Serde middleware wrappers (AY-143).

use std::fmt;

#[cfg(feature = "libyaml-compat")]
use ayaml::Value;
use ayaml::{
    DocumentDeserializer, Error, ErrorCode, ErrorKind, LimitKind, Limits, MAX_DEPTH_CAP,
    ParseOptions,
};
use serde::Deserialize;
use serde::de::{self, DeserializeSeed, Deserializer, EnumAccess, MapAccess, SeqAccess, Visitor};

/// Minimal path-tracking Serde middleware (modeled after `serde_path_to_error`) that wraps any
/// `serde::Deserializer<'de>`, records the active field/index path, and returns the innermost
/// error alongside the path where deserialization failed.
struct PathTrackingDeserializer<'p, D> {
    inner: D,
    path: &'p mut Vec<String>,
}

fn deserialize_with_path<'de, D, T>(de: D) -> Result<T, (Vec<String>, D::Error)>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let mut path = Vec::new();
    let wrapped = PathTrackingDeserializer {
        inner: de,
        path: &mut path,
    };
    T::deserialize(wrapped).map_err(|err| (path, err))
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for PathTrackingDeserializer<'_, D> {
    type Error = D::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_any(PathVisitor {
            inner: visitor,
            path: self.path,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_struct(
            name,
            fields,
            PathVisitor {
                inner: visitor,
                path: self.path,
            },
        )
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_map(PathVisitor {
            inner: visitor,
            path: self.path,
        })
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_seq(PathVisitor {
            inner: visitor,
            path: self.path,
        })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct tuple
        tuple_struct enum identifier ignored_any
    }
}

struct PathVisitor<'p, V> {
    inner: V,
    path: &'p mut Vec<String>,
}

impl<'de, V: Visitor<'de>> Visitor<'de> for PathVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        self.inner.visit_bool(v)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        self.inner.visit_i64(v)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.inner.visit_u64(v)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
        self.inner.visit_f64(v)
    }

    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
        self.inner.visit_borrowed_str(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        self.inner.visit_str(v)
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        self.inner.visit_string(v)
    }

    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
        self.inner.visit_unit()
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_seq(PathSeqAccess {
            inner: seq,
            path: self.path,
            index: 0,
        })
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_map(PathMapAccess {
            inner: map,
            path: self.path,
            current_key: None,
        })
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_enum(data)
    }
}

struct PathSeqAccess<'p, A> {
    inner: A,
    path: &'p mut Vec<String>,
    index: usize,
}

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for PathSeqAccess<'_, A> {
    type Error = A::Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Self::Error> {
        let idx = self.index;
        self.index += 1;
        self.path.push(idx.to_string());
        let result = self.inner.next_element_seed(PathSeed {
            inner: seed,
            path: self.path,
        });
        if result.is_ok() {
            self.path.pop();
        }
        result
    }
}

struct PathMapAccess<'p, A> {
    inner: A,
    path: &'p mut Vec<String>,
    current_key: Option<String>,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for PathMapAccess<'_, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let mut captured = None;
        let result = self.inner.next_key_seed(CaptureKeySeed {
            inner: seed,
            captured: &mut captured,
        })?;
        self.current_key = captured;
        Ok(result)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let key = self.current_key.take().unwrap_or_else(|| "?".to_owned());
        self.path.push(key);
        let result = self.inner.next_value_seed(PathSeed {
            inner: seed,
            path: self.path,
        });
        if result.is_ok() {
            self.path.pop();
        }
        result
    }
}

struct PathSeed<'p, T> {
    inner: T,
    path: &'p mut Vec<String>,
}

impl<'de, T: DeserializeSeed<'de>> DeserializeSeed<'de> for PathSeed<'_, T> {
    type Value = T::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.inner.deserialize(PathTrackingDeserializer {
            inner: deserializer,
            path: self.path,
        })
    }
}

/// Captures string map keys while forwarding unmodified to the inner key seed.
struct CaptureKeySeed<'k, K> {
    inner: K,
    captured: &'k mut Option<String>,
}

impl<'de, K: DeserializeSeed<'de>> DeserializeSeed<'de> for CaptureKeySeed<'_, K> {
    type Value = K::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.inner.deserialize(CaptureKeyDeserializer {
            inner: deserializer,
            captured: self.captured,
        })
    }
}

struct CaptureKeyDeserializer<'k, D> {
    inner: D,
    captured: &'k mut Option<String>,
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for CaptureKeyDeserializer<'_, D> {
    type Error = D::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_any(CaptureKeyVisitor {
            inner: visitor,
            captured: self.captured,
        })
    }

    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_identifier(CaptureKeyVisitor {
            inner: visitor,
            captured: self.captured,
        })
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct enum ignored_any
    }
}

struct CaptureKeyVisitor<'k, V> {
    inner: V,
    captured: &'k mut Option<String>,
}

impl<'de, V: Visitor<'de>> Visitor<'de> for CaptureKeyVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
        *self.captured = Some(v.to_owned());
        self.inner.visit_borrowed_str(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        *self.captured = Some(v.to_owned());
        self.inner.visit_str(v)
    }

    fn visit_string<E: de::Error>(self, v: String) -> Result<Self::Value, E> {
        *self.captured = Some(v.clone());
        self.inner.visit_string(v)
    }
}

/// Minimal unknown-field-collecting Serde middleware (modeled after `serde_ignored`) that records
/// every map entry whose value is deserialized via `deserialize_ignored_any`.
struct IgnoredCollector<'a, D> {
    inner: D,
    prefix: Vec<String>,
    ignored: &'a mut Vec<String>,
}

fn deserialize_collecting_ignored<'de, D, T>(de: D) -> Result<(T, Vec<String>), D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let mut ignored = Vec::new();
    let value = T::deserialize(IgnoredCollector {
        inner: de,
        prefix: Vec::new(),
        ignored: &mut ignored,
    })?;
    Ok((value, ignored))
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for IgnoredCollector<'_, D> {
    type Error = D::Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_any(IgnoredVisitor {
            inner: visitor,
            prefix: self.prefix,
            ignored: self.ignored,
        })
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_struct(
            name,
            fields,
            IgnoredVisitor {
                inner: visitor,
                prefix: self.prefix,
                ignored: self.ignored,
            },
        )
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        self.inner.deserialize_map(IgnoredVisitor {
            inner: visitor,
            prefix: self.prefix,
            ignored: self.ignored,
        })
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Self::Error> {
        if !self.prefix.is_empty() {
            self.ignored.push(self.prefix.join("."));
        }
        self.inner.deserialize_ignored_any(visitor)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct enum identifier
    }
}

struct IgnoredVisitor<'a, V> {
    inner: V,
    prefix: Vec<String>,
    ignored: &'a mut Vec<String>,
}

impl<'de, V: Visitor<'de>> Visitor<'de> for IgnoredVisitor<'_, V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.expecting(formatter)
    }

    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
        self.inner.visit_bool(v)
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
        self.inner.visit_i64(v)
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
        self.inner.visit_u64(v)
    }

    fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
        self.inner.visit_borrowed_str(v)
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
        self.inner.visit_str(v)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_seq(seq)
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
        self.inner.visit_map(IgnoredMapAccess {
            inner: map,
            prefix: self.prefix,
            ignored: self.ignored,
            current_key: None,
        })
    }
}

struct IgnoredMapAccess<'a, A> {
    inner: A,
    prefix: Vec<String>,
    ignored: &'a mut Vec<String>,
    current_key: Option<String>,
}

impl<'de, A: MapAccess<'de>> MapAccess<'de> for IgnoredMapAccess<'_, A> {
    type Error = A::Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Self::Error> {
        let mut captured = None;
        let key = self.inner.next_key_seed(CaptureKeySeed {
            inner: seed,
            captured: &mut captured,
        })?;
        self.current_key = captured;
        Ok(key)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, Self::Error> {
        let mut next_prefix = self.prefix.clone();
        if let Some(key) = self.current_key.take() {
            next_prefix.push(key);
        }
        self.inner.next_value_seed(IgnoredSeed {
            inner: seed,
            prefix: next_prefix,
            ignored: self.ignored,
        })
    }
}

struct IgnoredSeed<'a, T> {
    inner: T,
    prefix: Vec<String>,
    ignored: &'a mut Vec<String>,
}

impl<'de, T: DeserializeSeed<'de>> DeserializeSeed<'de> for IgnoredSeed<'_, T> {
    type Value = T::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        self.inner.deserialize(IgnoredCollector {
            inner: deserializer,
            prefix: self.prefix,
            ignored: self.ignored,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct ServerConfig<'a> {
    host: &'a str,
    ports: Vec<u16>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
struct AppConfig<'a> {
    name: &'a str,
    #[serde(borrow)]
    server: ServerConfig<'a>,
}

#[test]
fn path_tracking_middleware_preserves_borrowed_strings_paths_and_yaml_spans() {
    let valid = "name: demo\nserver:\n  host: localhost\n  ports:\n    - 80\n    - 443\n";
    let de = DocumentDeserializer::from_str(valid).expect("valid doc");
    let config: AppConfig<'_> = deserialize_with_path(de).expect("borrowed decode");
    assert_eq!(
        config,
        AppConfig {
            name: "demo",
            server: ServerConfig {
                host: "localhost",
                ports: vec![80, 443],
            },
        }
    );

    // Works via `&mut DocumentDeserializer` as well as by value, and implements Debug cleanly.
    let mut de_mut = DocumentDeserializer::from_str(valid).expect("valid doc mut");
    assert!(format!("{de_mut:?}").contains("DocumentDeserializer"));
    let config_mut: AppConfig<'_> = deserialize_with_path(&mut de_mut).expect("mut decode");
    assert_eq!(config_mut, config);

    // Anchors and aliases borrow directly from the source through Serde middleware.
    let aliased = "name: &h localhost\nserver:\n  host: *h\n  ports: [80]\n";
    let de_alias = DocumentDeserializer::from_str(aliased).expect("aliased doc");
    let aliased_cfg: AppConfig<'_> = deserialize_with_path(de_alias).expect("aliased decode");
    assert_eq!(aliased_cfg.name, "localhost");
    assert_eq!(aliased_cfg.server.host, "localhost");

    // On a nested Serde type error, the middleware captures the full path AND ayaml::Error
    // retains the exact YAML source span and line/column location.
    let invalid = "name: demo\nserver:\n  host: localhost\n  ports:\n    - 80\n    - not_a_port\n";
    let de = DocumentDeserializer::from_str(invalid).expect("valid yaml");
    let (path, error): (Vec<String>, Error) =
        deserialize_with_path::<_, AppConfig<'_>>(de).expect_err("type mismatch");
    assert_eq!(path, vec!["server", "ports", "1"]);
    assert_eq!(error.kind(), ErrorKind::Serde);
    let span = error.span().expect("error span");
    assert_eq!(&invalid[span.range()], "not_a_port");
    assert_eq!(
        error.location().map(|loc| (loc.line(), loc.column())),
        Some((6, 7))
    );
}

#[test]
fn unknown_field_collecting_middleware_records_ignored_keys_while_borrowing() {
    let input = "\
name: demo
deprecated_flag: true
server:
  host: localhost
  extra_option: 42
  ports: [8080]
";
    let de = DocumentDeserializer::from_str(input).expect("valid doc");
    let (config, ignored): (AppConfig<'_>, Vec<String>) =
        deserialize_collecting_ignored(de).expect("decode with ignored");
    assert_eq!(config.name, "demo");
    assert_eq!(config.server.host, "localhost");
    assert_eq!(config.server.ports, vec![8080]);
    assert_eq!(ignored, vec!["deprecated_flag", "server.extra_option"]);
}

#[test]
#[allow(deprecated, reason = "tests deprecated 0.x *_with_limits shims")]
fn document_deserializer_enforces_limits_and_compat_options() {
    // Invalid limits fail fast before parsing.
    for bad in [
        Limits::new().with_max_depth(MAX_DEPTH_CAP + 1),
        Limits::new().with_node_cost_bytes(0),
    ] {
        let opts = ParseOptions::new().with_limits(bad.clone());
        for err in [
            DocumentDeserializer::from_str_with("a: 1\n", &opts).unwrap_err(),
            DocumentDeserializer::from_str_with_limits("a: 1\n", &bad).unwrap_err(),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit);
            assert_eq!(err.limit(), None);
        }
    }

    // Input-size, depth, duplicate-key, and trailing-document limits apply.
    let small = Limits::new().with_max_input_bytes(4);
    let err = DocumentDeserializer::from_str_with_limits("name: demo\n", &small).unwrap_err();
    assert_eq!(
        err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    let shallow = ParseOptions::new().with_limits(Limits::new().with_max_depth(1));
    let err = DocumentDeserializer::from_str_with("a: [1, 2]\n", &shallow).unwrap_err();
    assert_eq!(
        err.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );

    let err = DocumentDeserializer::from_str("a: 1\na: 2\n").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::DuplicateKey);

    let err = DocumentDeserializer::from_str("a: 1\n---\nb: 2\n").unwrap_err();
    assert_eq!(err.code(), ErrorCode::TrailingDocuments);

    #[cfg(feature = "libyaml-compat")]
    {
        // Compat allows duplicate keys (last wins) and supports GlobalTags::Drop.
        let de = DocumentDeserializer::from_str_compat("a: 1\na: 2\n").expect("compat dup");
        let map = std::collections::BTreeMap::<String, u32>::deserialize(de).expect("map");
        assert_eq!(map.get("a"), Some(&2));

        let tagged = "ts: !!timestamp 2026-01-01\n";
        let drop_opts =
            ayaml::compat::ParseOptions::new().with_global_tags(ayaml::compat::GlobalTags::Drop);
        let de_drop = DocumentDeserializer::from_str_compat_with(tagged, &drop_opts).expect("drop");
        let dropped = Value::deserialize(de_drop).expect("dropped value");
        assert_eq!(dropped["ts"], Value::from("2026-01-01"));

        // Compat remaps depth limits to ErrorKind::Serde while keeping ErrorCode::DepthLimit and
        // LimitExceeded metadata, and remaps trailing/count document errors to libyaml's message.
        let shallow_limits = Limits::new().with_max_depth(1);
        let err = DocumentDeserializer::from_str_compat_with_limits("a: [1]\n", &shallow_limits)
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Serde);
        assert_eq!(err.code(), ErrorCode::DepthLimit);
        assert_eq!(
            err.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::Depth)
        );

        let err = DocumentDeserializer::from_str_compat("a: 1\n---\nb: 2\n").unwrap_err();
        assert_eq!(err.code(), ErrorCode::TrailingDocuments);
        assert_eq!(err.limit(), None);
        assert!(err.to_string().contains("more than one document in stream"));

        let one_doc =
            ayaml::compat::ParseOptions::new().with_limits(Limits::new().with_max_documents(1));
        let err =
            DocumentDeserializer::from_str_compat_with("a: 1\n---\nb: 2\n", &one_doc).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Parser);
        assert_eq!(err.code(), ErrorCode::DocumentCountLimit);
        assert_eq!(
            err.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::Documents)
        );
    }
}
