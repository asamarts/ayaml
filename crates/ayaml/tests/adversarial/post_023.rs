//! Process-isolated adversarial tests for post-0.2.3 public APIs, large `Error::masked()`
//! failures, and emitter escape / `%TAG` expansion `DoS` defenses (AY-171).
//!
//! Executed inside the bounded `in_child!` harness of `crates/ayaml/tests/adversarial.rs`.

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::fmt::Write as _;
use std::io::{self, Read};

use ayaml::{
    DocumentDeserializer, EmitOptions, ErrorCode, ErrorKind, LimitExceeded, LimitKind, Limits,
    MAX_DEPTH, ParseOptions, Spanned, Tag, TaggedValue, Value,
};

use super::in_child_named;

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n;
        Ok(n)
    }
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::BrokenPipe, "reader broken"))
    }
}

fn chained_merge_bomb() -> String {
    let mut bomb = String::from("m0: &m0\n  a: x\n  b: y\n");
    for level in 1..20 {
        let prev = level - 1;
        let _ = writeln!(
            bomb,
            "m{level}: &m{level}\n  x:\n    <<: *m{prev}\n  y:\n    <<: *m{prev}"
        );
    }
    bomb
}

/// `from_reader_with` enforces input-byte, alias, depth, merge-key, and I/O bounds cleanly.
#[test]
fn from_reader_with_enforces_limits_and_merge_bombs_boundedly() {
    in_child!(|| {
        let opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(512));

        // Infinite stream stops after max_input_bytes + 1 bytes.
        let mut infinite = CountingReader {
            inner: io::repeat(b'a'),
            bytes_read: 0,
        };
        let err =
            ayaml::from_reader_with::<_, Value>(&mut infinite, &opts).expect_err("infinite stream");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(
            err.limit().map(LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );
        assert_eq!(infinite.bytes_read, 513);

        // Oversized finite stream stops after max_input_bytes + 1 bytes.
        let oversized = vec![b'z'; 8192];
        let mut finite = CountingReader {
            inner: oversized.as_slice(),
            bytes_read: 0,
        };
        let err =
            ayaml::from_reader_with::<_, Value>(&mut finite, &opts).expect_err("oversized finite");
        assert_eq!(err.kind(), ErrorKind::Limit);
        assert_eq!(finite.bytes_read, 513);

        // I/O errors preserve ErrorKind::Io and underlying source.
        let io_err = ayaml::from_reader_with::<_, Value>(FailingReader, &opts).expect_err("io err");
        assert_eq!(io_err.kind(), ErrorKind::Io);
        assert!(io_err.source().is_some());

        // Alias bomb and deep nesting are stopped cleanly.
        let bomb = super::alias_bomb();
        let default_opts = ParseOptions::new();
        let alias_err = ayaml::from_reader_with::<_, Value>(bomb.as_bytes(), &default_opts)
            .expect_err("alias bomb");
        assert_eq!(alias_err.kind(), ErrorKind::Limit);

        let deep = format!(
            "{}{}\n",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        let depth_err = ayaml::from_reader_with::<_, Value>(deep.as_bytes(), &default_opts)
            .expect_err("depth bomb");
        assert_eq!(depth_err.code(), ErrorCode::DepthLimit);

        // Opt-in merge-key bomb through `from_reader_with` stops at resource limits.
        let merge_opts = ParseOptions::new().with_merge_keys(true);
        let merge_bomb = chained_merge_bomb();
        let merge_err = ayaml::from_reader_with::<_, Value>(merge_bomb.as_bytes(), &merge_opts)
            .expect_err("merge bomb via from_reader_with");
        assert_eq!(merge_err.kind(), ErrorKind::Limit);
    });
}

/// `DocumentDeserializer` enforces alias, depth, scalar, event, collection-entry, trailing-doc,
/// and opt-in merge-key limits boundedly.
#[test]
fn document_deserializer_enforces_limits_and_hostile_inputs_boundedly() {
    in_child!(|| {
        // Alias bomb.
        let bomb = super::alias_bomb();
        let alias_err = DocumentDeserializer::from_str(&bomb)
            .and_then(<Value as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer alias bomb");
        assert_eq!(alias_err.kind(), ErrorKind::Limit);

        // Depth bomb at MAX_DEPTH + 1 and extreme depth 5,000.
        for depth in [MAX_DEPTH + 1, 5_000] {
            let deep = format!("{}{}\n", "[".repeat(depth), "]".repeat(depth));
            let err = DocumentDeserializer::from_str(&deep)
                .and_then(<Value as serde::Deserialize>::deserialize)
                .expect_err("DocumentDeserializer depth bomb");
            assert_eq!(err.code(), ErrorCode::DepthLimit);
        }

        // Giant scalar, max_events, and max_collection_entries.
        let scalar_opts = ParseOptions::new().with_limits(Limits::new().with_max_scalar_bytes(64));
        let big_scalar = format!("k: \"{}\"\n", "x".repeat(4096));
        let scalar_err = DocumentDeserializer::from_str_with(&big_scalar, &scalar_opts)
            .and_then(<Value as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer scalar limit");
        assert_eq!(scalar_err.code(), ErrorCode::ScalarSizeLimit);

        let entry_opts =
            ParseOptions::new().with_limits(Limits::new().with_max_collection_entries(32));
        let mut wide = String::new();
        for i in 0..100 {
            let _ = writeln!(wide, "k{i}: {i}");
        }
        let entry_err = DocumentDeserializer::from_str_with(&wide, &entry_opts)
            .and_then(<Value as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer entry limit");
        assert_eq!(entry_err.code(), ErrorCode::CollectionEntriesLimit);

        // Trailing documents rejected cleanly.
        let multi_err = DocumentDeserializer::from_str("---\na: 1\n---\nb: 2\n")
            .and_then(<Value as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer trailing docs");
        assert_eq!(multi_err.code(), ErrorCode::TrailingDocuments);

        // Opt-in merge-key bomb rejected cleanly.
        let merge_opts = ParseOptions::new().with_merge_keys(true);
        let merge_bomb = chained_merge_bomb();
        let merge_err = DocumentDeserializer::from_str_with(&merge_bomb, &merge_opts)
            .and_then(<Value as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer merge bomb");
        assert_eq!(merge_err.kind(), ErrorKind::Limit);
    });
}

/// Frontmatter helpers enforce opt-in merge-key bombs and skip multi-megabyte non-YAML bodies
/// without charging body bytes against `max_input_bytes`.
#[test]
fn frontmatter_with_merge_and_huge_body_stays_bounded() {
    in_child!(|| {
        // 4 MiB hostile non-YAML body after a tiny valid frontmatter succeeds under tight 256 B cap.
        let mut doc = String::with_capacity(4 * 1024 * 1024 + 64);
        doc.push_str("---\ntitle: ok\n---\n");
        for _ in 0..(4 * 1024) {
            doc.push_str(&"[".repeat(1024));
        }
        let tight = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(256));
        let split = ayaml::split_frontmatter_with(&doc, &tight)
            .expect("split_frontmatter_with ignores body size")
            .expect("frontmatter present");
        assert_eq!(split.body().len(), 4 * 1024 * 1024);

        let parsed_val = ayaml::parse_frontmatter_value_with(&doc, &tight)
            .expect("parse_frontmatter_value_with ignores body size")
            .expect("frontmatter value present");
        assert_eq!(parsed_val.frontmatter["title"], Value::from("ok"));
        assert_eq!(parsed_val.body.len(), 4 * 1024 * 1024);

        let parsed_typed = ayaml::parse_frontmatter_with::<Value>(&doc, &tight)
            .expect("parse_frontmatter_with ignores body size")
            .expect("frontmatter typed present");
        assert_eq!(parsed_typed.frontmatter["title"], Value::from("ok"));

        // Opt-in merge-key bomb inside frontmatter is stopped cleanly across both parsers.
        let merge_opts = ParseOptions::new().with_merge_keys(true);
        let merge_front = format!("---\n{}---\n# body\n", chained_merge_bomb());
        for err in [
            ayaml::parse_frontmatter_with::<Value>(&merge_front, &merge_opts)
                .expect_err("typed frontmatter merge bomb"),
            ayaml::parse_frontmatter_value_with(&merge_front, &merge_opts)
                .expect_err("value frontmatter merge bomb"),
        ] {
            assert_eq!(err.kind(), ErrorKind::Limit);
        }
    });
}

/// `Spanned<T>` enforces alias, depth, scalar, and collection-entry limits on hostile inputs and
/// preserves exact UTF-8 spans across wide documents.
#[test]
fn spanned_typed_enforces_limits_on_hostile_inputs_boundedly() {
    in_child!(|| {
        // Alias bomb.
        let bomb = super::alias_bomb();
        let err = ayaml::from_str::<Spanned<Value>>(&bomb).expect_err("Spanned alias bomb");
        assert_eq!(err.kind(), ErrorKind::Limit);

        // Depth bomb at MAX_DEPTH + 1 and 5,000.
        for depth in [MAX_DEPTH + 1, 5_000] {
            let deep = format!("{}{}\n", "[".repeat(depth), "]".repeat(depth));
            let err = ayaml::from_str::<Spanned<Value>>(&deep).expect_err("Spanned depth bomb");
            assert_eq!(err.code(), ErrorCode::DepthLimit);
        }

        // Giant scalar and tight collection entries.
        let scalar_opts = ParseOptions::new().with_limits(Limits::new().with_max_scalar_bytes(64));
        let giant = format!("k: \"{}\"\n", "a".repeat(4096));
        let err = ayaml::from_str_with::<Spanned<BTreeMap<String, Spanned<String>>>>(
            &giant,
            &scalar_opts,
        )
        .expect_err("Spanned scalar limit");
        assert_eq!(err.code(), ErrorCode::ScalarSizeLimit);

        let entry_opts =
            ParseOptions::new().with_limits(Limits::new().with_max_collection_entries(64));
        let mut wide = String::with_capacity(32 * 1024);
        for i in 0..2_000 {
            let _ = writeln!(wide, "k{i:04}: \"val_{i:04}_é\"");
        }
        let err =
            ayaml::from_str_with::<Spanned<BTreeMap<String, Spanned<String>>>>(&wide, &entry_opts)
                .expect_err("Spanned entry limit");
        assert_eq!(err.code(), ErrorCode::CollectionEntriesLimit);

        // Valid wide mapping decodes cleanly with every inner span slicing valid UTF-8.
        let decoded: Spanned<BTreeMap<String, Spanned<String>>> =
            ayaml::from_str(&wide).expect("wide Spanned map");
        assert_eq!(decoded.as_ref().len(), 2_000);
        assert!(decoded.span().slice(&wide).is_some());
        for val in decoded.as_ref().values() {
            assert!(val.span().slice(&wide).is_some());
        }

        // DocumentDeserializer -> Spanned<Value> also enforces merge-key bombs.
        let merge_opts = ParseOptions::new().with_merge_keys(true);
        let merge_bomb = chained_merge_bomb();
        let err = DocumentDeserializer::from_str_with(&merge_bomb, &merge_opts)
            .and_then(<Spanned<Value> as serde::Deserialize>::deserialize)
            .expect_err("DocumentDeserializer Spanned merge bomb");
        assert_eq!(err.kind(), ErrorKind::Limit);
    });
}

/// `Error::masked()` on large hostile errors (giant scalars, deep paths, huge unknown fields,
/// huge duplicate keys) stays small, bounded in memory, and leaks zero secret content.
#[test]
fn error_masked_handles_large_hostile_errors_boundedly() {
    in_child!(|| {
        #[derive(Debug, serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        #[allow(
            dead_code,
            reason = "deserialization target for hostile unknown-field errors"
        )]
        struct TinyCfg {
            ok: u32,
        }

        #[derive(Debug, serde::Deserialize)]
        #[allow(
            dead_code,
            reason = "deserialization target for hostile unknown-variant errors"
        )]
        enum TinyChoice {
            One,
        }

        let canary = "SECRET_CANARY_AY171_987654321";

        // 1. 512 KiB scalar with invalid escape containing canary at the end.
        let mut big_escape = String::with_capacity(512 * 1024 + 64);
        big_escape.push_str("k: \"");
        big_escape.push_str(&"a".repeat(512 * 1024));
        big_escape.push_str(canary);
        big_escape.push_str("\\q\"\n");
        let err_escape = ayaml::parse_value(&big_escape).expect_err("invalid escape");

        // 2. 100-deep nested mapping with canary in every key and type mismatch at the leaf.
        //    Use a recursive Deserialize impl that actually descends through all 100 levels to the
        //    leaf so `Error`'s path accumulator builds and truncates the full deep path.
        #[derive(Debug)]
        struct DeepMap;

        impl<'de> serde::Deserialize<'de> for DeepMap {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct DeepVisitor;
                impl<'de> serde::de::Visitor<'de> for DeepVisitor {
                    type Value = DeepMap;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("nested mapping ending in u32 leaf")
                    }
                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: serde::de::MapAccess<'de>,
                    {
                        while let Some(key) = map.next_key::<String>()? {
                            if key.ends_with("_leaf") {
                                let _: u32 = map.next_value()?;
                            } else {
                                let _: DeepMap = map.next_value()?;
                            }
                        }
                        Ok(DeepMap)
                    }
                }
                deserializer.deserialize_map(DeepVisitor)
            }
        }

        let mut deep_path = String::new();
        for depth in 0..100 {
            for _ in 0..depth {
                deep_path.push_str("  ");
            }
            let _ = writeln!(deep_path, "{canary}_{depth}:");
        }
        for _ in 0..100 {
            deep_path.push_str("  ");
        }
        let _ = writeln!(deep_path, "{canary}_leaf: not_a_number_{canary}");
        let err_deep = ayaml::from_str::<DeepMap>(&deep_path).expect_err("deep path type mismatch");
        let deep_unmasked = err_deep.to_string();
        assert!(
            deep_unmasked.contains(&format!("{canary}_leaf"))
                && deep_unmasked.starts_with('\u{2026}'),
            "expected deep path to reach leaf and truncate long path, got: {deep_unmasked:?}"
        );

        // 3. 64 KiB unknown field and 64 KiB unknown enum variant carrying canary.
        let big_key = format!("{canary}_{}", "k".repeat(64 * 1024));
        let err_unknown_field =
            ayaml::from_str::<TinyCfg>(&format!("{big_key}: 1\n")).expect_err("unknown field");
        let err_unknown_variant =
            ayaml::from_str::<TinyChoice>(&format!("{big_key}\n")).expect_err("unknown variant");

        // 4. 64 KiB duplicate mapping key carrying canary.
        let err_dup = ayaml::parse_value(&format!("{big_key}: 1\n{big_key}: 2\n"))
            .expect_err("huge duplicate key");

        for (label, err) in [
            ("big_escape", err_escape),
            ("deep_path", err_deep),
            ("unknown_field", err_unknown_field),
            ("unknown_variant", err_unknown_variant),
            ("huge_dup", err_dup),
        ] {
            let masked = err.masked();
            assert_eq!(masked.kind(), err.kind(), "{label}: kind changed");
            assert_eq!(masked.code(), err.code(), "{label}: code changed");
            assert_eq!(masked.span(), err.span(), "{label}: span changed");
            assert_eq!(
                masked.location(),
                err.location(),
                "{label}: location changed"
            );
            assert_eq!(masked.limit(), err.limit(), "{label}: limit changed");
            assert!(masked.source().is_none(), "{label}: masked exposed source");
            let display = masked.to_string();
            let debug = format!("{masked:?}");
            assert!(!display.contains(canary), "{label}: Display leaked canary");
            assert!(!debug.contains(canary), "{label}: Debug leaked canary");
            assert!(
                display.len() <= 512,
                "{label}: masked Display unexpectedly large ({} bytes)",
                display.len()
            );
            assert!(
                debug.len() <= 2048,
                "{label}: masked Debug unexpectedly large ({} bytes)",
                debug.len()
            );
        }
    });
}

/// Emitter escape expansion (multi-byte control/BiDi escapes) and repeated custom `%TAG` URI
/// emission respect `max_output_bytes` without amplifying memory across `Value`, Serde, and
/// `Event` writers in both block and flow styles.
#[test]
fn emitter_escape_and_tag_expansion_respect_output_limits_boundedly() {
    in_child!(|| {
        // 1. 160 KiB scalar of BiDi / NUL / quote bytes expands 6x-9x when double-quoted; a tight
        //    512-byte output cap must stop emission immediately across Value, Serde, and Event writers.
        let hostile_scalar: String = "\0\u{202e}\"".repeat(32 * 1024);
        let escape_val = Value::String(hostile_scalar.clone());
        let escape_events = vec![
            ayaml::Event::StreamStart {
                span: ayaml::Span::default(),
            },
            ayaml::Event::DocumentStart {
                explicit: false,
                span: ayaml::Span::default(),
            },
            ayaml::Event::Scalar {
                value: std::borrow::Cow::Borrowed(hostile_scalar.as_str()),
                raw: std::borrow::Cow::Borrowed(hostile_scalar.as_str()),
                style: ayaml::ScalarStyle::DoubleQuoted,
                tag: None,
                anchor: None,
                span: ayaml::Span::default(),
            },
            ayaml::Event::DocumentEnd {
                explicit: false,
                span: ayaml::Span::default(),
            },
            ayaml::Event::StreamEnd {
                span: ayaml::Span::default(),
            },
        ];

        // 2. 1,000 tagged nodes with a 4 KiB custom tag URI prefix (including `>` to force `%TAG`
        //    directive generation) must stop immediately at 512 bytes without allocating megabytes.
        let long_prefix = format!("tag:example.com,2026:>{}/", "p".repeat(4_000));
        let long_tag = Tag::new(long_prefix.clone(), "node");
        let tagged_items: Vec<Value> = (0..1_000_i64)
            .map(|i| Value::from(TaggedValue::new(long_tag.clone(), Value::from(i))))
            .collect();
        let tagged_val = Value::Sequence(tagged_items);
        let mut tagged_events = Vec::with_capacity(1_006);
        tagged_events.push(ayaml::Event::StreamStart {
            span: ayaml::Span::default(),
        });
        tagged_events.push(ayaml::Event::Directive {
            name: std::borrow::Cow::Borrowed("TAG"),
            value: std::borrow::Cow::Owned(format!("!p! {long_prefix}")),
            span: ayaml::Span::default(),
        });
        tagged_events.push(ayaml::Event::DocumentStart {
            explicit: true,
            span: ayaml::Span::default(),
        });
        tagged_events.push(ayaml::Event::SequenceStart {
            tag: None,
            anchor: None,
            style: ayaml::CollectionStyle::Block,
            span: ayaml::Span::default(),
        });
        for _ in 0..1_000 {
            tagged_events.push(ayaml::Event::Scalar {
                value: std::borrow::Cow::Borrowed("1"),
                raw: std::borrow::Cow::Borrowed("1"),
                style: ayaml::ScalarStyle::Plain,
                tag: Some(long_tag.clone()),
                anchor: None,
                span: ayaml::Span::default(),
            });
        }
        tagged_events.push(ayaml::Event::SequenceEnd {
            span: ayaml::Span::default(),
        });
        tagged_events.push(ayaml::Event::DocumentEnd {
            explicit: false,
            span: ayaml::Span::default(),
        });
        tagged_events.push(ayaml::Event::StreamEnd {
            span: ayaml::Span::default(),
        });

        let tight_limits = Limits::default().with_max_output_bytes(512);
        let block_opts = EmitOptions::new().with_limits(tight_limits);
        let flow_opts = block_opts.clone().with_flow(true);

        let check = || {
            let mut buf = Vec::new();
            let cases: [(&str, Result<(), ayaml::Error>); 13] = [
                (
                    "value_block_escape",
                    ayaml::to_string_value_with(&escape_val, &block_opts).map(drop),
                ),
                (
                    "value_flow_escape",
                    ayaml::to_string_value_with(&escape_val, &flow_opts).map(drop),
                ),
                (
                    "writer_value_escape",
                    ayaml::to_writer_value_with(&mut buf, &escape_val, &block_opts),
                ),
                (
                    "serde_block_escape",
                    ayaml::to_string_with(&hostile_scalar, &block_opts).map(drop),
                ),
                (
                    "serde_flow_escape",
                    ayaml::to_string_with(&hostile_scalar, &flow_opts).map(drop),
                ),
                (
                    "serde_writer_escape",
                    ayaml::to_writer_with(&mut buf, &hostile_scalar, &block_opts),
                ),
                (
                    "events_escape",
                    ayaml::to_string_events_with(&escape_events, &block_opts).map(drop),
                ),
                (
                    "writer_events_escape",
                    ayaml::to_writer_events_with(&mut buf, &escape_events, &block_opts),
                ),
                (
                    "value_block_tag_expansion",
                    ayaml::to_string_value_with(&tagged_val, &block_opts).map(drop),
                ),
                (
                    "value_flow_tag_expansion",
                    ayaml::to_string_value_with(&tagged_val, &flow_opts).map(drop),
                ),
                (
                    "writer_value_tag_expansion",
                    ayaml::to_writer_value_with(&mut buf, &tagged_val, &block_opts),
                ),
                (
                    "events_tag_expansion",
                    ayaml::to_string_events_with(&tagged_events, &block_opts).map(drop),
                ),
                (
                    "writer_events_tag_expansion",
                    ayaml::to_writer_events_with(&mut buf, &tagged_events, &block_opts),
                ),
            ];
            for (label, result) in cases {
                let err = result.expect_err(label);
                assert_eq!(err.kind(), ErrorKind::Limit, "{label}: {err:?}");
                assert_eq!(err.code(), ErrorCode::OutputSizeLimit, "{label}: {err:?}");
                assert_eq!(
                    err.limit().map(LimitExceeded::kind),
                    Some(LimitKind::OutputBytes),
                    "{label}: {err:?}"
                );
                buf.clear();
            }
        };

        if super::MEASURES_MEMORY {
            let growth = super::peak_growth(check);
            assert!(
                growth <= 2 << 20,
                "escape/tag expansion rejection grew memory by {growth} bytes (expected <= 2 MiB)"
            );
        } else {
            check();
        }

        // Serde `to_string_with` also rejects oversized tagged structures cleanly.
        let small_tagged = Value::Sequence(vec![Value::from(TaggedValue::new(
            long_tag,
            Value::from(1_i64),
        ))]);
        let serde_tag_err =
            ayaml::to_string_with(&small_tagged, &block_opts).expect_err("serde tag limit");
        assert_eq!(serde_tag_err.code(), ErrorCode::OutputSizeLimit);
    });
}
