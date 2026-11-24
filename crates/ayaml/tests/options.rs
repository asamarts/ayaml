//! Contracts for the unified options API (`ParseOptions`, `compat::ParseOptions`,
//! `EmitOptions`), every `*_with` entry point, and compatibility of every deprecated
//! combination function through `0.x`.

use std::marker::PhantomData;

use ayaml::{
    Deserializer, EmitOptions, EmitterOptions, EmitterStyles, ErrorKind, Event, LimitKind, Limits,
    MAX_DEPTH_CAP, NodeStyle, ParseOptions, PathSegment, ScalarStyle, SpannedTree, Value,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;

const DEFAULT_PARSE: ParseOptions = ParseOptions::new();
const MAX_INPUT_PARSE: ParseOptions = ParseOptions::for_max_input();
const CUSTOM_PARSE: ParseOptions =
    ParseOptions::new().with_limits(Limits::new().with_max_depth(16));

const DEFAULT_EMIT: EmitOptions = EmitOptions::new();
const FLOW_EMIT: EmitOptions = EmitOptions::flow();
const CUSTOM_EMIT: EmitOptions = EmitOptions::new()
    .with_limits(Limits::new().with_max_output_bytes(4096))
    .with_flow(true)
    .with_explicit_start(true)
    .with_explicit_end(true)
    .with_scalar_style(Some(ScalarStyle::SingleQuoted))
    .with_indent(4)
    .with_indent_sequences(true);

#[cfg(feature = "libyaml-compat")]
const COMPAT_PARSE: ayaml::compat::ParseOptions = ayaml::compat::ParseOptions::new();
#[cfg(feature = "libyaml-compat")]
const COMPAT_MAX_INPUT: ayaml::compat::ParseOptions = ayaml::compat::ParseOptions::for_max_input();
#[cfg(feature = "libyaml-compat")]
const COMPAT_DROP: ayaml::compat::ParseOptions = ayaml::compat::ParseOptions::new()
    .with_limits(Limits::new().with_max_depth(32))
    .with_global_tags(ayaml::compat::GlobalTags::Drop);

#[derive(Debug, PartialEq, Deserialize)]
struct Borrowed<'a> {
    name: &'a str,
    count: u32,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Owned {
    name: String,
    count: u32,
}

fn load_owned_from_local_string<T: DeserializeOwned>(
    options: &ParseOptions,
) -> Result<T, ayaml::Error> {
    let local = String::from("name: ayaml\ncount: 3\n");
    ayaml::from_str_with(&local, options)
}

fn load_owned_from_local_bytes<T: DeserializeOwned>(
    options: &ParseOptions,
) -> Result<T, ayaml::Error> {
    let local = b"name: ayaml\ncount: 3\n".to_vec();
    ayaml::from_slice_with(&local, options)
}

#[test]
fn const_presets_and_conversions_match_defaults() {
    assert_eq!(DEFAULT_PARSE, ParseOptions::default());
    assert_eq!(MAX_INPUT_PARSE.limits, Limits::for_max_input());
    assert_eq!(CUSTOM_PARSE.limits.max_depth, 16);
    assert_eq!(
        ParseOptions::from(Limits::new().with_max_depth(16)),
        CUSTOM_PARSE
    );

    let custom_emit = CUSTOM_EMIT;
    assert_eq!(DEFAULT_EMIT, EmitOptions::default());
    assert_eq!(FLOW_EMIT, EmitOptions::new().with_flow(true));
    assert_eq!(custom_emit.limits.max_output_bytes, 4096);
    assert!(custom_emit.flow);
    assert!(custom_emit.explicit_start);
    assert!(custom_emit.explicit_end);
    assert_eq!(custom_emit.scalar_style, Some(ScalarStyle::SingleQuoted));
    assert_eq!(custom_emit.indent, 4);
    assert!(custom_emit.indent_sequences);
    assert!(custom_emit.styles.is_none());

    assert_eq!(EmitterOptions::new(), EmitterOptions::default());
    assert_eq!(NodeStyle::new(), NodeStyle::default());
    assert_eq!(Limits::new(), Limits::default());

    let from_emitter = EmitOptions::from(
        EmitterOptions::new()
            .with_flow(true)
            .with_explicit_start(true)
            .with_indent(4),
    );
    assert!(from_emitter.flow);
    assert!(from_emitter.explicit_start);
    assert_eq!(from_emitter.indent, 4);
    assert_eq!(from_emitter.limits, Limits::default());

    #[cfg(feature = "libyaml-compat")]
    {
        assert_eq!(COMPAT_PARSE, ayaml::compat::ParseOptions::default());
        assert_eq!(COMPAT_MAX_INPUT.limits, Limits::for_max_input());
        assert_eq!(COMPAT_DROP.limits.max_depth, 32);
        assert_eq!(COMPAT_DROP.global_tags, ayaml::compat::GlobalTags::Drop);
        assert_eq!(
            ayaml::compat::ParseOptions::from(Limits::new().with_max_depth(32))
                .limits
                .max_depth,
            32
        );
    }
}

#[test]
fn from_str_with_and_from_slice_with_support_owned_and_borrowed_targets() {
    let input = "name: ayaml\ncount: 3\n";
    let borrowed: Borrowed<'_> = ayaml::from_str_with(input, &DEFAULT_PARSE).expect("borrowed str");
    assert_eq!(
        borrowed,
        Borrowed {
            name: "ayaml",
            count: 3
        }
    );
    let owned: Owned = load_owned_from_local_string(&DEFAULT_PARSE).expect("owned str");
    assert_eq!(
        owned,
        Owned {
            name: "ayaml".to_owned(),
            count: 3
        }
    );

    let borrowed_slice: Borrowed<'_> =
        ayaml::from_slice_with(input.as_bytes(), &DEFAULT_PARSE).expect("borrowed slice");
    assert_eq!(borrowed_slice, borrowed);
    let owned_slice: Owned = load_owned_from_local_bytes(&DEFAULT_PARSE).expect("owned slice");
    assert_eq!(owned_slice, owned);

    let tight = ParseOptions::new().with_limits(Limits::new().with_max_depth(0));
    let error = ayaml::from_str_with::<Owned>(input, &tight).expect_err("depth limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );

    let error = ayaml::from_slice_with::<Owned>(input.as_bytes(), &tight).expect_err("slice depth");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::Depth)
    );
}

#[test]
fn from_slice_with_checks_limits_before_utf8_validation() {
    let invalid_utf8 = b"\xff\xfe\xfd\xfc\xfb";

    // Invalid configuration fails before inspecting bytes.
    let bad_depth =
        ParseOptions::new().with_limits(Limits::new().with_max_depth(MAX_DEPTH_CAP + 1));
    let error = ayaml::from_slice_with::<Value>(invalid_utf8, &bad_depth).expect_err("bad depth");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit(), None);

    let bad_cost = ParseOptions::new().with_limits(Limits::new().with_node_cost_bytes(0));
    let error = ayaml::from_slice_with::<Value>(invalid_utf8, &bad_cost).expect_err("bad cost");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit(), None);

    // Input-byte ceiling rejects oversized buffers before checking UTF-8.
    let small = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(4));
    let error = ayaml::from_slice_with::<Value>(invalid_utf8, &small).expect_err("too large");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Within the configured ceiling, invalid UTF-8 is reported as InvalidEncoding.
    let enough = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(5));
    let error = ayaml::from_slice_with::<Value>(invalid_utf8, &enough).expect_err("invalid utf-8");
    assert_eq!(error.kind(), ErrorKind::InvalidEncoding);

    // The _with_limits slice variants obey the same ordering.
    let small_limits = Limits::new().with_max_input_bytes(4);
    let enough_limits = Limits::new().with_max_input_bytes(5);
    for (name, error) in [
        (
            "from_slice_with_limits",
            ayaml::from_slice_with_limits::<Value>(invalid_utf8, &small_limits)
                .expect_err("slice_with_limits too large"),
        ),
        (
            "from_slice_borrowed_with_limits",
            ayaml::from_slice_borrowed_with_limits::<Value>(invalid_utf8, &small_limits)
                .expect_err("slice_borrowed_with_limits too large"),
        ),
    ] {
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        assert_eq!(
            error.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::InputBytes),
            "{name}: {error}"
        );
    }
    assert_eq!(
        ayaml::from_slice_with_limits::<Value>(invalid_utf8, &enough_limits)
            .expect_err("slice_with_limits utf-8")
            .kind(),
        ErrorKind::InvalidEncoding
    );
    assert_eq!(
        ayaml::from_slice_borrowed_with_limits::<Value>(invalid_utf8, &enough_limits)
            .expect_err("slice_borrowed_with_limits utf-8")
            .kind(),
        ErrorKind::InvalidEncoding
    );
}

#[test]
fn root_with_limits_and_from_reader_variants_cover_bounds_utf8_and_io_errors() {
    use std::error::Error as _;
    use std::io;

    struct CountingReader<R> {
        inner: R,
        bytes_read: usize,
    }

    impl<R: io::Read> io::Read for CountingReader<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes_read += n;
            Ok(n)
        }
    }

    struct FailingReader;

    impl io::Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "pipe closed"))
        }
    }

    let input = "name: ayaml\ncount: 3\n";
    let default_limits = Limits::default();
    let expected = Owned {
        name: "ayaml".to_owned(),
        count: 3,
    };

    assert_eq!(
        ayaml::from_str_with_limits::<Owned>(input, &default_limits).expect("str_with_limits"),
        expected
    );
    assert_eq!(
        ayaml::from_slice_with_limits::<Owned>(input.as_bytes(), &default_limits)
            .expect("slice_with_limits"),
        expected
    );
    let borrowed: Borrowed<'_> =
        ayaml::from_slice_borrowed_with_limits(input.as_bytes(), &default_limits)
            .expect("slice_borrowed_with_limits");
    assert_eq!(
        borrowed,
        Borrowed {
            name: "ayaml",
            count: 3,
        }
    );
    assert_eq!(
        ayaml::from_reader::<_, Owned>(input.as_bytes()).expect("from_reader"),
        expected
    );
    assert_eq!(
        ayaml::from_reader_with::<_, Owned>(input.as_bytes(), &DEFAULT_PARSE)
            .expect("from_reader_with"),
        expected
    );
    assert_eq!(
        ayaml::from_reader_with_limits::<_, Owned>(input.as_bytes(), &default_limits)
            .expect("from_reader_with_limits"),
        expected
    );

    // Exact boundary is accepted; one byte above stops after reading at most max_input_bytes + 1.
    let exact = Limits::new().with_max_input_bytes(input.len());
    assert_eq!(
        ayaml::from_reader_with_limits::<_, Owned>(input.as_bytes(), &exact).expect("exact"),
        expected
    );

    let tight = Limits::new().with_max_input_bytes(10);
    let mut counter = CountingReader {
        inner: io::repeat(b'a'),
        bytes_read: 0,
    };
    let error =
        ayaml::from_reader_with_limits::<_, Value>(&mut counter, &tight).expect_err("oversized");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );
    assert_eq!(counter.bytes_read, 11);

    // Oversized invalid UTF-8 fails with InputBytes before UTF-8 validation.
    let invalid_utf8 = b"\xff\xfe\xfd\xfc\xfb";
    let error = ayaml::from_reader_with_limits::<_, Value>(
        &invalid_utf8[..],
        &Limits::new().with_max_input_bytes(4),
    )
    .expect_err("oversized invalid utf-8");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );

    // Within limit, invalid UTF-8 fails with InvalidEncoding.
    let error = ayaml::from_reader_with_limits::<_, Value>(
        &invalid_utf8[..],
        &Limits::new().with_max_input_bytes(5),
    )
    .expect_err("invalid utf-8");
    assert_eq!(error.kind(), ErrorKind::InvalidEncoding);

    // I/O errors surface as ErrorKind::Io / ErrorCode::Io, preserve the io::Error source,
    // and redact the underlying message when formatted through Error::masked().
    let error = ayaml::from_reader::<_, Value>(FailingReader).expect_err("io error");
    assert_eq!(error.kind(), ErrorKind::Io);
    assert_eq!(error.code(), ayaml::ErrorCode::Io);
    let io_source = error
        .source()
        .and_then(|source| source.downcast_ref::<io::Error>())
        .expect("io::Error source");
    assert_eq!(io_source.kind(), io::ErrorKind::BrokenPipe);
    assert!(!error.masked().to_string().contains("pipe closed"));

    // Partial read followed by an I/O error still surfaces as ErrorKind::Io.
    let partial_then_fail = io::Read::chain(&b"name: "[..], FailingReader);
    let error = ayaml::from_reader::<_, Value>(partial_then_fail).expect_err("partial io error");
    assert_eq!(error.kind(), ErrorKind::Io);
    assert_eq!(error.code(), ayaml::ErrorCode::Io);

    // Zero byte limit accepts an empty reader and rejects a 1-byte stream after reading 1 byte.
    let zero = Limits::new().with_max_input_bytes(0);
    assert_eq!(
        ayaml::from_reader_with_limits::<_, Value>(&b""[..], &zero).expect("empty reader"),
        Value::Null
    );
    let mut one_byte = CountingReader {
        inner: &b"a"[..],
        bytes_read: 0,
    };
    let error =
        ayaml::from_reader_with_limits::<_, Value>(&mut one_byte, &zero).expect_err("zero limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::InputBytes)
    );
    assert_eq!(one_byte.bytes_read, 1);

    // Trailing documents are rejected by from_reader just like from_str.
    let error =
        ayaml::from_reader::<_, Value>(&b"a: 1\n---\nb: 2\n"[..]).expect_err("trailing document");
    assert_eq!(error.code(), ayaml::ErrorCode::TrailingDocuments);

    // Invalid limits fail before reading from the reader.
    let bad_limits = Limits::new().with_max_depth(MAX_DEPTH_CAP + 1);
    let error = ayaml::from_reader_with_limits::<_, Value>(FailingReader, &bad_limits)
        .expect_err("bad limits before io");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(error.limit(), None);
}

#[test]
fn emit_options_combines_limits_presentation_and_styles_in_one_value() {
    let value = ayaml::parse_value("items:\n  - one\n  - two\nnested:\n  x: 1\n").expect("parse");
    let mut styles = EmitterStyles::new();
    styles.set(
        vec![PathSegment::key("items"), PathSegment::index(1)],
        NodeStyle::new().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    );
    let options = EmitOptions::new()
        .with_explicit_start(true)
        .with_explicit_end(true)
        .with_indent(4)
        .with_indent_sequences(true)
        .with_styles(styles);

    let from_value = ayaml::to_string_value_with(&value, &options).expect("value emit");
    let from_serde = ayaml::to_string_with(&value, &options).expect("serde emit");
    assert_eq!(from_value, from_serde);
    assert!(from_value.starts_with("---\n"), "{from_value:?}");
    assert!(from_value.ends_with("...\n"), "{from_value:?}");
    assert!(
        from_value.contains("    - one\n    - 'two'\n"),
        "{from_value:?}"
    );
    assert!(from_value.contains("nested:\n    x: 1\n"), "{from_value:?}");
    assert_eq!(ayaml::parse_value(&from_value).expect("reparse"), value);

    let mut written_value = Vec::new();
    ayaml::to_writer_value_with(&mut written_value, &value, &options).expect("writer value");
    assert_eq!(String::from_utf8(written_value).expect("utf8"), from_value);

    let mut written_serde = Vec::new();
    ayaml::to_writer_with(&mut written_serde, &value, &options).expect("writer serde");
    assert_eq!(String::from_utf8(written_serde).expect("utf8"), from_value);

    // Multi-document writers apply the same combined EmitOptions (options + styles + limits).
    let docs = [
        value.clone(),
        ayaml::parse_value("items:\n  - a\n  - b\n").expect("second"),
    ];
    let docs_str = ayaml::to_string_documents_with(&docs, &options).expect("docs emit");
    let mut docs_writer = Vec::new();
    ayaml::to_writer_documents_with(&mut docs_writer, &docs, &options).expect("docs writer");
    assert_eq!(String::from_utf8(docs_writer).expect("utf8"), docs_str);
    assert!(docs_str.contains("'two'"), "{docs_str:?}");
    assert!(docs_str.contains("'b'"), "{docs_str:?}");
    assert_eq!(
        ayaml::parse_documents(&docs_str).expect("reparse docs"),
        docs
    );

    // Tight limit inside the same EmitOptions rejects the combined call.
    let tight = options.with_limits(Limits::new().with_max_output_bytes(8));
    let error = ayaml::to_string_value_with(&value, &tight).expect_err("tight limit");
    assert_eq!(error.kind(), ErrorKind::Limit);
    assert_eq!(
        error.limit().map(ayaml::LimitExceeded::kind),
        Some(LimitKind::OutputBytes)
    );
}

#[test]
fn every_emitter_with_rejects_invalid_indent() {
    let value = ayaml::parse_value("a: 1\n").expect("parse");
    let docs = std::slice::from_ref(&value);
    let events: Vec<Event<'_>> = ayaml::parse_events("a: 1\n")
        .expect("events")
        .collect::<Result<_, _>>()
        .expect("events");

    for indent in [0, 1, 10] {
        let bad = EmitOptions::new().with_indent(indent);
        assert_eq!(
            ayaml::to_string_with(&value, &bad).unwrap_err().kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_writer_with(&mut Vec::new(), &value, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_string_value_with(&value, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_writer_value_with(&mut Vec::new(), &value, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_string_documents_with(docs, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_writer_documents_with(&mut Vec::new(), docs, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_string_events_with(&events, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_writer_events_with(&mut Vec::new(), &events, &bad)
                .unwrap_err()
                .kind(),
            ErrorKind::Emitter
        );
        assert_eq!(
            ayaml::to_writer_event_stream_with(
                &mut Vec::new(),
                ayaml::parse_events("a: 1\n").expect("stream"),
                &bad
            )
            .unwrap_err()
            .kind(),
            ErrorKind::Emitter
        );
    }
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_parse_options_cover_every_compat_entry_point() {
    use serde::de::IgnoredAny;

    let tagged = "ts: !!timestamp 2026-01-01\n";
    let kept: Value = ayaml::compat::from_str_with(tagged, &COMPAT_PARSE).expect("keep");
    assert!(kept["ts"].is_tagged());

    let dropped: Value = ayaml::compat::from_str_with(tagged, &COMPAT_DROP).expect("drop");
    assert_eq!(dropped["ts"], Value::from("2026-01-01"));

    let seed_dropped =
        ayaml::compat::from_str_seed_with(tagged, &COMPAT_DROP, PhantomData::<Value>)
            .expect("seed drop");
    assert_eq!(seed_dropped, dropped);

    ayaml::compat::deserialize_any_with(tagged, &COMPAT_DROP, IgnoredAny).expect("any drop");

    // Event readers and SpannedTree preserve syntax tags while enforcing limits.
    let lazy_events: Vec<_> = ayaml::compat::parse_events_with(tagged, &COMPAT_DROP)
        .expect("compat lazy events")
        .collect::<Result<_, _>>()
        .expect("compat lazy items");
    let eager_events =
        ayaml::compat::parse_events_eager_with(tagged, &COMPAT_DROP).expect("compat eager events");
    assert_eq!(lazy_events, eager_events);
    assert!(
        lazy_events
            .iter()
            .any(|event| matches!(event, Event::Scalar { tag: Some(_), .. }))
    );

    let tree = SpannedTree::parse_compat_with(tagged, &COMPAT_DROP).expect("compat spanned tree");
    assert_eq!(
        tree.scalar_text_at(&[PathSegment::key("ts")]),
        Some("2026-01-01")
    );

    let tight = ayaml::compat::ParseOptions::new().with_limits(Limits::new().with_max_events(1));
    assert_eq!(
        ayaml::compat::parse_events_with(tagged, &tight)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .unwrap_err()
            .kind(),
        ErrorKind::Limit
    );
    assert_eq!(
        ayaml::compat::parse_events_eager_with(tagged, &tight)
            .unwrap_err()
            .kind(),
        ErrorKind::Limit
    );
    assert_eq!(
        SpannedTree::parse_compat_with(tagged, &tight)
            .unwrap_err()
            .kind(),
        ErrorKind::Limit
    );
}

/// Every deprecated combination function remains callable through `0.x` and produces the exact
/// same result as its `*_with` replacement.
#[test]
#[allow(
    deprecated,
    reason = "verify every deprecated combination function remains equivalent to its _with replacement until 1.0"
)]
fn every_deprecated_combination_function_matches_its_with_replacement() {
    let input = "a:\n  - one\n  - two\n";
    let multi = "a: 1\n---\nb: 2\n";
    let limits = Limits::new().with_max_documents(4).with_max_depth(16);
    let parse_opts = ParseOptions::new().with_limits(limits.clone());

    // Default parsing / decoding shims.
    assert_eq!(
        ayaml::parse_events_with_limits(input, &limits)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .unwrap(),
        ayaml::parse_events_with(input, &parse_opts)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>)
            .unwrap()
    );
    assert_eq!(
        ayaml::parse_value_with_limits(input, &limits).unwrap(),
        ayaml::parse_value_with(input, &parse_opts).unwrap()
    );
    assert_eq!(
        ayaml::parse_documents_with_limits(multi, &limits).unwrap(),
        ayaml::parse_documents_with(multi, &parse_opts).unwrap()
    );
    assert_eq!(
        ayaml::from_str_borrowed_with_limits::<Value>(input, &limits).unwrap(),
        ayaml::from_str_with::<Value>(input, &parse_opts).unwrap()
    );
    assert_eq!(
        ayaml::from_str_seed_with_limits(input, &limits, PhantomData::<Value>).unwrap(),
        ayaml::from_str_seed_with(input, &parse_opts, PhantomData::<Value>).unwrap()
    );

    let mut old_de = Deserializer::from_str_with_limits(multi, &limits).unwrap();
    let mut new_de = Deserializer::from_str_with(multi, &parse_opts).unwrap();
    assert_eq!(
        old_de.next::<Value>().unwrap(),
        new_de.next::<Value>().unwrap()
    );
    assert_eq!(
        old_de.next::<Value>().unwrap(),
        new_de.next::<Value>().unwrap()
    );
    assert_eq!(
        old_de.next::<Value>().unwrap(),
        new_de.next::<Value>().unwrap()
    );

    let old_tree = SpannedTree::parse_with_limits(input, &limits).unwrap();
    let new_tree = SpannedTree::parse_with(input, &parse_opts).unwrap();
    let path = [PathSegment::key("a"), PathSegment::index(1)];
    assert_eq!(old_tree.span_at(&path), new_tree.span_at(&path));

    // Compat shims.
    #[cfg(feature = "libyaml-compat")]
    {
        use serde::de::IgnoredAny;

        let compat_opts = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
        assert_eq!(
            ayaml::compat::from_str_with_limits::<Value>(input, &limits).unwrap(),
            ayaml::compat::from_str_with::<Value>(input, &compat_opts).unwrap()
        );
        assert_eq!(
            ayaml::compat::from_str_seed_with_limits(input, &limits, PhantomData::<Value>).unwrap(),
            ayaml::compat::from_str_seed_with(input, &compat_opts, PhantomData::<Value>).unwrap()
        );
        assert!(ayaml::compat::deserialize_any_with_limits(input, &limits, IgnoredAny).is_ok());
        assert_eq!(
            ayaml::compat::parse_events_with_limits(input, &limits)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .unwrap(),
            ayaml::compat::parse_events_with(input, &compat_opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .unwrap()
        );
        assert_eq!(
            ayaml::compat::parse_events_eager_with_limits(input, &limits).unwrap(),
            ayaml::compat::parse_events_eager_with(input, &compat_opts).unwrap()
        );
        let old_compat_tree = SpannedTree::parse_compat_with_limits(input, &limits).unwrap();
        let new_compat_tree = SpannedTree::parse_compat_with(input, &compat_opts).unwrap();
        assert_eq!(
            old_compat_tree.span_at(&path),
            new_compat_tree.span_at(&path)
        );
    }

    // Emitter shims (all 16 value/serde/documents functions + 3 event functions).
    let value = ayaml::parse_value(input).unwrap();
    let docs = ayaml::parse_documents(multi).unwrap();
    let events: Vec<Event<'_>> = ayaml::parse_events(input)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let emitter_opts = EmitterOptions::new()
        .with_flow(true)
        .with_explicit_start(true)
        .with_explicit_end(true);
    let mut styles = EmitterStyles::new();
    styles.set(
        path.to_vec(),
        NodeStyle::new().with_scalar_style(Some(ScalarStyle::SingleQuoted)),
    );

    let by_limits = EmitOptions::from(limits.clone());
    let by_options = EmitOptions::from(emitter_opts);
    let by_styles = EmitOptions::from(styles.clone());
    let by_limits_styles = by_limits.clone().with_styles(styles.clone());
    let by_options_styles = by_options.clone().with_styles(styles.clone());

    assert_eq!(
        ayaml::to_string_with_limits(&value, &limits).unwrap(),
        ayaml::to_string_with(&value, &by_limits).unwrap()
    );
    assert_eq!(
        ayaml::to_string_with_options(&value, &emitter_opts).unwrap(),
        ayaml::to_string_with(&value, &by_options).unwrap()
    );
    assert_eq!(
        ayaml::to_string_value_with_limits(&value, &limits).unwrap(),
        ayaml::to_string_value_with(&value, &by_limits).unwrap()
    );
    assert_eq!(
        ayaml::to_string_value_with_options(&value, &emitter_opts).unwrap(),
        ayaml::to_string_value_with(&value, &by_options).unwrap()
    );
    assert_eq!(
        ayaml::to_string_value_with_styles(&value, &styles).unwrap(),
        ayaml::to_string_value_with(&value, &by_styles).unwrap()
    );
    assert_eq!(
        ayaml::to_string_value_with_limits_and_styles(&value, &limits, &styles).unwrap(),
        ayaml::to_string_value_with(&value, &by_limits_styles).unwrap()
    );
    assert_eq!(
        ayaml::to_string_value_with_options_and_styles(&value, &emitter_opts, &styles).unwrap(),
        ayaml::to_string_value_with(&value, &by_options_styles).unwrap()
    );
    assert_eq!(
        ayaml::to_string_documents_with_limits(&docs, &limits).unwrap(),
        ayaml::to_string_documents_with(&docs, &by_limits).unwrap()
    );
    assert_eq!(
        ayaml::to_string_events_with_limits(&events, &limits).unwrap(),
        ayaml::to_string_events_with(&events, &by_limits).unwrap()
    );

    let capture = |write: &dyn Fn(&mut Vec<u8>) -> Result<(), ayaml::Error>| -> Vec<u8> {
        let mut out = Vec::new();
        write(&mut out).unwrap();
        out
    };

    assert_eq!(
        capture(&|w| ayaml::to_writer_with_limits(w, &value, &limits)),
        capture(&|w| ayaml::to_writer_with(w, &value, &by_limits))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_with_options(w, &value, &emitter_opts)),
        capture(&|w| ayaml::to_writer_with(w, &value, &by_options))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_value_with_limits(w, &value, &limits)),
        capture(&|w| ayaml::to_writer_value_with(w, &value, &by_limits))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_value_with_options(w, &value, &emitter_opts)),
        capture(&|w| ayaml::to_writer_value_with(w, &value, &by_options))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_value_with_styles(w, &value, &styles)),
        capture(&|w| ayaml::to_writer_value_with(w, &value, &by_styles))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_value_with_limits_and_styles(w, &value, &limits, &styles)),
        capture(&|w| ayaml::to_writer_value_with(w, &value, &by_limits_styles))
    );
    assert_eq!(
        capture(&|w| {
            ayaml::to_writer_value_with_options_and_styles(w, &value, &emitter_opts, &styles)
        }),
        capture(&|w| ayaml::to_writer_value_with(w, &value, &by_options_styles))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_documents_with_limits(w, &docs, &limits)),
        capture(&|w| ayaml::to_writer_documents_with(w, &docs, &by_limits))
    );
    assert_eq!(
        capture(&|w| ayaml::to_writer_events_with_limits(w, &events, &limits)),
        capture(&|w| ayaml::to_writer_events_with(w, &events, &by_limits))
    );
    assert_eq!(
        capture(&|w| {
            ayaml::to_writer_event_stream_with_limits(
                w,
                ayaml::parse_events(input).unwrap(),
                &limits,
            )
        }),
        capture(&|w| {
            ayaml::to_writer_event_stream_with(w, ayaml::parse_events(input).unwrap(), &by_limits)
        })
    );
}

struct FailingSerialize;

impl serde::Serialize for FailingSerialize {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        Err(serde::ser::Error::custom("serializer should not run"))
    }
}

/// Every `_with` entry point rejects invalid `Limits` (`max_depth > MAX_DEPTH_CAP` and
/// `node_cost_bytes == 0`) with `ErrorKind::Limit` and `limit() == None` — including compat
/// readers (without misclassifying invalid `max_depth` as `ErrorKind::Serde`) and all emitters
/// (validating `limits` before `indent` and before running `Serialize`).
#[test]
fn every_with_entry_point_rejects_invalid_limits_consistently() {
    let input = "a: 1\n";
    let value = ayaml::parse_value(input).expect("value");
    let docs = std::slice::from_ref(&value);
    let events: Vec<Event<'_>> = ayaml::parse_events(input)
        .expect("events")
        .collect::<Result<_, _>>()
        .expect("events");

    let bad_limits = [
        Limits::new().with_max_depth(MAX_DEPTH_CAP + 1),
        Limits::new().with_node_cost_bytes(0),
    ];

    let assert_config_limit = |name: &str, error: ayaml::Error| {
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        assert_eq!(error.limit(), None, "{name}: {error}");
    };

    for limits in &bad_limits {
        let parse_opts = ParseOptions::new().with_limits(limits.clone());
        assert_config_limit(
            "parse_events_with",
            ayaml::parse_events_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "parse_value_with",
            ayaml::parse_value_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "parse_documents_with",
            ayaml::parse_documents_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "from_str_with",
            ayaml::from_str_with::<Value>(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "from_str_seed_with",
            ayaml::from_str_seed_with(input, &parse_opts, PhantomData::<Value>).unwrap_err(),
        );
        assert_config_limit(
            "from_slice_with",
            ayaml::from_slice_with::<Value>(input.as_bytes(), &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "from_reader_with",
            ayaml::from_reader_with::<_, Value>(input.as_bytes(), &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "from_str_with_limits",
            ayaml::from_str_with_limits::<Value>(input, limits).unwrap_err(),
        );
        assert_config_limit(
            "from_slice_with_limits",
            ayaml::from_slice_with_limits::<Value>(input.as_bytes(), limits).unwrap_err(),
        );
        assert_config_limit(
            "from_slice_borrowed_with_limits",
            ayaml::from_slice_borrowed_with_limits::<Value>(input.as_bytes(), limits).unwrap_err(),
        );
        assert_config_limit(
            "from_reader_with_limits",
            ayaml::from_reader_with_limits::<_, Value>(input.as_bytes(), limits).unwrap_err(),
        );
        assert_config_limit(
            "Deserializer::from_str_with",
            Deserializer::from_str_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "SpannedTree::parse_with",
            SpannedTree::parse_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "split_frontmatter_with",
            ayaml::split_frontmatter_with(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "parse_frontmatter_with",
            ayaml::parse_frontmatter_with::<Value>(input, &parse_opts).unwrap_err(),
        );
        assert_config_limit(
            "parse_frontmatter_value_with",
            ayaml::parse_frontmatter_value_with(input, &parse_opts).unwrap_err(),
        );

        #[cfg(feature = "libyaml-compat")]
        {
            use serde::de::IgnoredAny;

            let compat_opts = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
            assert_config_limit(
                "compat::from_str_with",
                ayaml::compat::from_str_with::<Value>(input, &compat_opts).unwrap_err(),
            );
            assert_config_limit(
                "compat::from_str_seed_with",
                ayaml::compat::from_str_seed_with(input, &compat_opts, PhantomData::<Value>)
                    .unwrap_err(),
            );
            assert_config_limit(
                "compat::deserialize_any_with",
                ayaml::compat::deserialize_any_with(input, &compat_opts, IgnoredAny).unwrap_err(),
            );
            assert_config_limit(
                "compat::parse_events_with",
                ayaml::compat::parse_events_with(input, &compat_opts).unwrap_err(),
            );
            assert_config_limit(
                "compat::parse_events_eager_with",
                ayaml::compat::parse_events_eager_with(input, &compat_opts).unwrap_err(),
            );
            assert_config_limit(
                "SpannedTree::parse_compat_with",
                SpannedTree::parse_compat_with(input, &compat_opts).unwrap_err(),
            );
        }

        // Also pair invalid limits with an invalid indent to prove limits validate first on every emitter.
        let emit_opts = EmitOptions::new()
            .with_limits(limits.clone())
            .with_indent(0);
        assert_config_limit(
            "to_string_with",
            ayaml::to_string_with(&FailingSerialize, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_writer_with",
            ayaml::to_writer_with(&mut Vec::new(), &FailingSerialize, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_string_value_with",
            ayaml::to_string_value_with(&value, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_writer_value_with",
            ayaml::to_writer_value_with(&mut Vec::new(), &value, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_string_documents_with",
            ayaml::to_string_documents_with(docs, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_writer_documents_with",
            ayaml::to_writer_documents_with(&mut Vec::new(), docs, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_string_events_with",
            ayaml::to_string_events_with(&events, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_writer_events_with",
            ayaml::to_writer_events_with(&mut Vec::new(), &events, &emit_opts).unwrap_err(),
        );
        assert_config_limit(
            "to_writer_event_stream_with",
            ayaml::to_writer_event_stream_with(
                &mut Vec::new(),
                ayaml::parse_events(input).expect("stream"),
                &emit_opts,
            )
            .unwrap_err(),
        );
    }

    // Invalid indent also fails fast before running Serialize on to_string_with / to_writer_with.
    let bad_indent = EmitOptions::new().with_indent(1);
    assert_eq!(
        ayaml::to_string_with(&FailingSerialize, &bad_indent)
            .unwrap_err()
            .kind(),
        ErrorKind::Emitter
    );
    assert_eq!(
        ayaml::to_writer_with(&mut Vec::new(), &FailingSerialize, &bad_indent)
            .unwrap_err()
            .kind(),
        ErrorKind::Emitter
    );
}
