use super::{
    ELIDED, Error, ErrorCode, ErrorKind, MAX_KEY_CHARS, hides_text, path_bytes, quoted_key,
};
use crate::limits::{LimitExceeded, LimitKind};
use crate::span::{Location, Span};
use std::collections::HashSet;
use std::error::Error as _;

const ALL_CODES: &[ErrorCode] = &[
    ErrorCode::InvalidUtf8,
    ErrorCode::NonPrintableChar,
    ErrorCode::InvalidBom,
    ErrorCode::ReservedIndicator,
    ErrorCode::InvalidTabIndentation,
    ErrorCode::UnterminatedQuotedScalar,
    ErrorCode::InvalidQuotedContinuation,
    ErrorCode::InvalidEscape,
    ErrorCode::InvalidBlockScalarHeader,
    ErrorCode::InvalidBlockScalarIndentation,
    ErrorCode::InvalidFlowIndentation,
    ErrorCode::DocumentMarkerInFlow,
    ErrorCode::InvalidFlowKey,
    ErrorCode::MissingCommentWhitespace,
    ErrorCode::InvalidDirective,
    ErrorCode::DuplicateDirective,
    ErrorCode::InvalidAnchor,
    ErrorCode::InvalidTag,
    ErrorCode::UndefinedTagHandle,
    ErrorCode::DirectiveWithoutDocument,
    ErrorCode::InvalidDocumentStart,
    ErrorCode::ContentAfterDocumentEnd,
    ErrorCode::TrailingContent,
    ErrorCode::InvalidBlockIndentation,
    ErrorCode::MissingMappingColon,
    ErrorCode::TrailingDocuments,
    ErrorCode::UnexpectedToken,
    ErrorCode::UnexpectedMappingValue,
    ErrorCode::UnterminatedFlowCollection,
    ErrorCode::InvalidFlowCollection,
    ErrorCode::DuplicateNodeProperties,
    ErrorCode::MisplacedNodeProperties,
    ErrorCode::InvalidImplicitKey,
    ErrorCode::AliasWithProperties,
    ErrorCode::UnknownAnchor,
    ErrorCode::CyclicAlias,
    ErrorCode::DuplicateKey,
    ErrorCode::InvalidMergeOperand,
    ErrorCode::MalformedEventStream,
    ErrorCode::InvalidIntScalar,
    ErrorCode::InvalidFloatScalar,
    ErrorCode::InvalidBoolScalar,
    ErrorCode::InvalidNullScalar,
    ErrorCode::InvalidScalar,
    ErrorCode::LimitExceeded,
    ErrorCode::InvalidLimits,
    ErrorCode::InputSizeLimit,
    ErrorCode::DepthLimit,
    ErrorCode::DocumentCountLimit,
    ErrorCode::ScalarSizeLimit,
    ErrorCode::CollectionEntriesLimit,
    ErrorCode::EventCountLimit,
    ErrorCode::AnchorCountLimit,
    ErrorCode::AliasDereferenceLimit,
    ErrorCode::AliasExpansionLimit,
    ErrorCode::OutputSizeLimit,
    ErrorCode::TagExpansionLimit,
    ErrorCode::InvalidType,
    ErrorCode::InvalidValue,
    ErrorCode::InvalidLength,
    ErrorCode::UnknownVariant,
    ErrorCode::UnknownField,
    ErrorCode::MissingField,
    ErrorCode::DuplicateField,
    ErrorCode::InvalidEnumRepresentation,
    ErrorCode::MapValueBeforeKey,
    ErrorCode::SerdeCustom,
    ErrorCode::InvalidEmitterOptions,
    ErrorCode::UnsupportedTag,
    ErrorCode::InvalidEmitEvent,
    ErrorCode::Io,
];

/// Exhaustive match so adding an `ErrorCode` variant without adding it to `ALL_CODES` fails
/// to compile.
fn is_listed(code: ErrorCode) -> bool {
    match code {
        ErrorCode::InvalidUtf8
        | ErrorCode::NonPrintableChar
        | ErrorCode::InvalidBom
        | ErrorCode::ReservedIndicator
        | ErrorCode::InvalidTabIndentation
        | ErrorCode::UnterminatedQuotedScalar
        | ErrorCode::InvalidQuotedContinuation
        | ErrorCode::InvalidEscape
        | ErrorCode::InvalidBlockScalarHeader
        | ErrorCode::InvalidBlockScalarIndentation
        | ErrorCode::InvalidFlowIndentation
        | ErrorCode::DocumentMarkerInFlow
        | ErrorCode::InvalidFlowKey
        | ErrorCode::MissingCommentWhitespace
        | ErrorCode::InvalidDirective
        | ErrorCode::DuplicateDirective
        | ErrorCode::InvalidAnchor
        | ErrorCode::InvalidTag
        | ErrorCode::UndefinedTagHandle
        | ErrorCode::DirectiveWithoutDocument
        | ErrorCode::InvalidDocumentStart
        | ErrorCode::ContentAfterDocumentEnd
        | ErrorCode::TrailingContent
        | ErrorCode::InvalidBlockIndentation
        | ErrorCode::MissingMappingColon
        | ErrorCode::TrailingDocuments
        | ErrorCode::UnexpectedToken
        | ErrorCode::UnexpectedMappingValue
        | ErrorCode::UnterminatedFlowCollection
        | ErrorCode::InvalidFlowCollection
        | ErrorCode::DuplicateNodeProperties
        | ErrorCode::MisplacedNodeProperties
        | ErrorCode::InvalidImplicitKey
        | ErrorCode::AliasWithProperties
        | ErrorCode::UnknownAnchor
        | ErrorCode::CyclicAlias
        | ErrorCode::DuplicateKey
        | ErrorCode::InvalidMergeOperand
        | ErrorCode::MalformedEventStream
        | ErrorCode::InvalidIntScalar
        | ErrorCode::InvalidFloatScalar
        | ErrorCode::InvalidBoolScalar
        | ErrorCode::InvalidNullScalar
        | ErrorCode::InvalidScalar
        | ErrorCode::LimitExceeded
        | ErrorCode::InvalidLimits
        | ErrorCode::InputSizeLimit
        | ErrorCode::DepthLimit
        | ErrorCode::DocumentCountLimit
        | ErrorCode::ScalarSizeLimit
        | ErrorCode::CollectionEntriesLimit
        | ErrorCode::EventCountLimit
        | ErrorCode::AnchorCountLimit
        | ErrorCode::AliasDereferenceLimit
        | ErrorCode::AliasExpansionLimit
        | ErrorCode::OutputSizeLimit
        | ErrorCode::TagExpansionLimit
        | ErrorCode::InvalidType
        | ErrorCode::InvalidValue
        | ErrorCode::InvalidLength
        | ErrorCode::UnknownVariant
        | ErrorCode::UnknownField
        | ErrorCode::MissingField
        | ErrorCode::DuplicateField
        | ErrorCode::InvalidEnumRepresentation
        | ErrorCode::MapValueBeforeKey
        | ErrorCode::SerdeCustom
        | ErrorCode::InvalidEmitterOptions
        | ErrorCode::UnsupportedTag
        | ErrorCode::InvalidEmitEvent
        | ErrorCode::Io => ALL_CODES.contains(&code),
    }
}

#[test]
fn every_error_code_has_a_unique_yaml_kebab_name() {
    let mut seen = HashSet::new();
    for &code in ALL_CODES {
        assert!(is_listed(code));
        let text = code.as_str();
        assert_eq!(code.to_string(), text);
        assert_eq!(code.as_ref(), text);
        let rest = text
            .strip_prefix("yaml.")
            .unwrap_or_else(|| panic!("missing yaml. prefix: {text}"));
        assert!(!rest.is_empty(), "empty suffix: {text}");
        assert!(
            !rest.starts_with('-') && !rest.ends_with('-') && !rest.contains("--"),
            "{text}"
        );
        assert!(
            rest.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "not kebab-case: {text}"
        );
        assert!(seen.insert(text), "duplicate error code string: {text}");
    }
}

#[test]
fn every_error_code_is_documented_in_reference_page() {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = manifest.join("../..");
    let path = root.join("docs/error-codes.md");
    if !path.is_file() {
        assert!(
            manifest.join("Cargo.toml.orig").is_file(),
            "{} is neither in the repository nor an unpacked package",
            manifest.display()
        );
        return;
    }
    let doc = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    let mut documented = HashSet::new();
    for line in doc.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        if cells.len() >= 4
            && let Some(variant) = cells[1].strip_prefix('`').and_then(|s| s.strip_suffix('`'))
            && let Some(code_str) = cells[2].strip_prefix('`').and_then(|s| s.strip_suffix('`'))
            && code_str.starts_with("yaml.")
        {
            assert!(
                documented.insert((variant.to_owned(), code_str.to_owned())),
                "duplicate code in docs/error-codes.md: ({variant}, {code_str})"
            );
        }
    }
    let expected: HashSet<(String, String)> = ALL_CODES
        .iter()
        .map(|code| (format!("{code:?}"), code.as_str().to_owned()))
        .collect();
    let missing: Vec<_> = expected.difference(&documented).collect();
    let extra: Vec<_> = documented.difference(&expected).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "docs/error-codes.md drift: missing={missing:?}, extra={extra:?}"
    );
}

#[test]
fn error_supports_clone_and_partial_eq() {
    let a = Error::new(ErrorKind::Parser, "bad")
        .with_code(ErrorCode::TrailingDocuments)
        .with_span(Span::new(1, 3))
        .with_location(Location::new(1, 2));
    let b = a.clone();
    assert_eq!(a, b);
    assert_ne!(a, a.clone().with_code(ErrorCode::UnexpectedToken));

    let with_limit = Error::new(ErrorKind::Limit, "too deep").with_limit(LimitExceeded::new(
        LimitKind::Depth,
        4,
        5,
    ));
    assert_eq!(with_limit.code(), ErrorCode::DepthLimit);
    assert_eq!(with_limit, with_limit.clone());
    let replaced_limit = with_limit.with_limit(LimitExceeded::new(LimitKind::InputBytes, 8, 9));
    assert_eq!(replaced_limit.code(), ErrorCode::InputSizeLimit);
    let explicit_code = Error::new(ErrorKind::Limit, "custom")
        .with_code(ErrorCode::InvalidLimits)
        .with_limit(LimitExceeded::new(LimitKind::Depth, 4, 5));
    assert_eq!(explicit_code.code(), ErrorCode::InvalidLimits);

    let s1 =
        Error::new(ErrorKind::Io, "write failed").with_source(std::io::Error::other("broken pipe"));
    let s2 =
        Error::new(ErrorKind::Io, "write failed").with_source(std::io::Error::other("broken pipe"));
    let s3 =
        Error::new(ErrorKind::Io, "write failed").with_source(std::io::Error::other("disk full"));
    assert_eq!(s1, s1.clone());
    assert_eq!(s1, s2);
    assert_ne!(s1, s3);
    assert_ne!(s1, Error::new(ErrorKind::Io, "write failed"));
}

#[test]
fn default_codes_for_every_error_kind_are_pinned() {
    let defaults = [
        (ErrorKind::InvalidEncoding, ErrorCode::InvalidUtf8),
        (ErrorKind::Scanner, ErrorCode::UnexpectedToken),
        (ErrorKind::Parser, ErrorCode::UnexpectedToken),
        (ErrorKind::DuplicateKey, ErrorCode::DuplicateKey),
        (ErrorKind::Tag, ErrorCode::InvalidTag),
        (ErrorKind::Resolution, ErrorCode::InvalidScalar),
        (ErrorKind::Limit, ErrorCode::LimitExceeded),
        (ErrorKind::Composition, ErrorCode::MalformedEventStream),
        (ErrorKind::Serde, ErrorCode::SerdeCustom),
        (ErrorKind::Emitter, ErrorCode::InvalidEmitEvent),
        (ErrorKind::Io, ErrorCode::Io),
    ];
    for (kind, expected) in defaults {
        assert_eq!(Error::new(kind, "msg").code(), expected, "{kind:?}");
    }
}

#[test]
fn kind_and_message_are_exposed() {
    let error = Error::new(ErrorKind::Parser, "unexpected token");
    assert_eq!(error.kind(), ErrorKind::Parser);
    assert_eq!(error.code(), ErrorCode::UnexpectedToken);
    assert_eq!(error.message(), "unexpected token");
    assert_eq!(error.to_string(), "unexpected token");
    assert!(error.span().is_none());
    assert!(error.location().is_none());
}

#[test]
fn span_and_location_are_one_based() {
    let span = Span::new(4, 5);
    let error = Error::new(ErrorKind::Scanner, "bad token")
        .with_span(span)
        .with_location(Location::new(2, 3));
    assert_eq!(error.span(), Some(span));
    let location = error.location().expect("location");
    assert_eq!((location.line(), location.column()), (2, 3));
}

#[test]
fn display_and_masked_format_location_consistently_with_or_without_span() {
    let loc_only = Error::new(ErrorKind::Parser, "bad input").with_location(Location::new(2, 4));
    assert_eq!(loc_only.span(), None);
    assert_eq!(loc_only.location(), Some(Location::new(2, 4)));
    assert_eq!(loc_only.to_string(), "bad input at line 2 column 4");
    assert_eq!(
        loc_only.masked().to_string(),
        "parser error (yaml.unexpected-token) at line 2 column 4"
    );

    let loc_with_context = Error::new(ErrorKind::Parser, "unterminated")
        .with_context(", opened at line 1 column 1")
        .with_location(Location::new(2, 4));
    assert_eq!(
        loc_with_context.to_string(),
        "unterminated at line 2 column 4, opened at line 1 column 1"
    );
    assert_eq!(
        loc_with_context.masked().to_string(),
        "parser error (yaml.unexpected-token) at line 2 column 4"
    );

    let span_only = Error::new(ErrorKind::Scanner, "bad token").with_span(Span::new(1, 5));
    assert_eq!(span_only.to_string(), "bad token");
    assert_eq!(
        span_only.masked().to_string(),
        "scanner error (yaml.unexpected-token)"
    );

    // Calling `with_span` after `with_location` clears the prior location consistently for both
    // `Display` and `MaskedError`.
    let cleared = Error::new(ErrorKind::Parser, "bad input")
        .with_location(Location::new(2, 4))
        .with_span(Span::new(1, 3));
    assert_eq!(cleared.location(), None);
    assert_eq!(cleared.to_string(), "bad input");
    assert_eq!(
        cleared.masked().to_string(),
        "parser error (yaml.unexpected-token)"
    );

    // Path prefix + location without a span prints both on `Error` and redacts the key on `MaskedError`.
    let path_loc_only = Error::new(ErrorKind::Serde, "bad value")
        .with_location(Location::new(4, 7))
        .inside_key("secret_field");
    assert_eq!(
        path_loc_only.to_string(),
        "secret_field: bad value at line 4 column 7"
    );
    assert_eq!(
        path_loc_only.masked().to_string(),
        "serde error (yaml.serde-custom) at line 4 column 7"
    );
}

#[test]
fn source_is_chained() {
    let inner = std::io::Error::other("inner");
    let error = Error::new(ErrorKind::Io, "outer").with_source(inner);
    assert!(error.source().is_some());
}

#[test]
fn a_path_joins_its_steps_and_stands_before_the_message() {
    let error = Error::new(ErrorKind::Serde, "bad")
        .inside_key("c")
        .inside_index(2)
        .inside_key("b")
        .inside_key("a b")
        .inside_index(0);
    assert_eq!(error.message(), "[0][\"a b\"].b[2].c: bad");
    assert_eq!(error.kind(), ErrorKind::Serde);
    let error = Error::new(ErrorKind::Serde, "bad").inside_collection_key();
    assert_eq!(error.message(), "[?]: bad");
}

#[test]
fn a_key_is_bare_only_if_it_is_a_short_name() {
    for bare in ["a", "_a", "runs-on", "a_1", &"k".repeat(64)] {
        let error = Error::new(ErrorKind::Serde, "bad").inside_key(bare);
        assert_eq!(error.message(), format!("{bare}: bad"));
    }
    for (key, shown) in [
        ("", "[\"\"]"),
        ("80", "[\"80\"]"),
        ("-a", "[\"-a\"]"),
        ("a.b", "[\"a.b\"]"),
        ("名前", "[\"名前\"]"),
        ("a\"b\\c", "[\"a\\\"b\\\\c\"]"),
        ("a\nb\u{1b}", "[\"a\\u{a}b\\u{1b}\"]"),
        ("\u{85}\u{2028}\u{2029}", "[\"\\u{85}\\u{2028}\\u{2029}\"]"),
        ("\u{200e}\u{200f}", "[\"\\u{200e}\\u{200f}\"]"),
        ("a\u{61c}b", "[\"a\\u{61c}b\"]"),
        (
            "\u{202e}\u{2066}\u{feff}",
            "[\"\\u{202e}\\u{2066}\\u{feff}\"]",
        ),
    ] {
        let error = Error::new(ErrorKind::Serde, "bad").inside_key(key);
        assert_eq!(error.message(), format!("{shown}: bad"));
    }
}

#[test]
fn a_long_key_and_a_long_path_are_cut_short() {
    let error = Error::new(ErrorKind::Serde, "bad").inside_key(&"é".repeat(65));
    assert_eq!(error.message(), format!("[\"{}…\"]: bad", "é".repeat(64)));

    let mut error = Error::new(ErrorKind::Serde, "bad");
    let name = "k".repeat(64);
    for _ in 0..100 {
        error = error.inside_key(&name);
    }
    let (path, rest) = error.message().split_once(": ").expect("a path");
    assert_eq!(rest, "bad");
    assert!(path.starts_with("….k"), "{path}");
    assert!(path.len() <= 1024 + 4, "{}", path.len());
    let before = error.message().to_owned();
    let error = error.inside_key("outer").inside_index(1);
    assert_eq!(error.message(), before);

    let mut error = Error::new(ErrorKind::Serde, "bad").inside_index(7);
    for _ in 0..200 {
        error = error.inside_index(1_234_567);
    }
    assert!(error.message().starts_with("…["), "{}", error.message());
}

/// Text that reads like a location, in a key or in the message, never stands for the error's.
#[test]
fn a_location_in_the_text_does_not_stand_for_the_error_s() {
    let span = Span::new(4, 5);
    let error = Error::new(ErrorKind::Serde, "bad")
        .with_span(span)
        .with_location(Location::new(2, 3))
        .inside_key("at line 9");
    assert_eq!(error.to_string(), "[\"at line 9\"]: bad at line 2 column 3");
    let error = Error::new(ErrorKind::Serde, "invalid type: string \"at line 9\"")
        .with_span(span)
        .with_location(Location::new(2, 3))
        .inside_key("a");
    assert_eq!(
        error.to_string(),
        "a: invalid type: string \"at line 9\" at line 2 column 3"
    );
}

#[cfg(target_pointer_width = "64")]
#[test]
fn an_error_stays_small() {
    assert_eq!(size_of::<Error>(), 8);
    assert_eq!(size_of::<Result<crate::Value, Error>>(), 32);
}

/// A path's steps are as long as `inside` lets them be, and a context too long to keep its
/// length still shows.
#[test]
fn a_long_path_or_context_keeps_its_text() {
    let long = "k".repeat(70_000);
    let error = Error::new(ErrorKind::Serde, "bad").inside_key(&long);
    assert!(
        error.message().ends_with("…\"]: bad"),
        "{}",
        error.message()
    );
    let error = (0..2_000).fold(Error::new(ErrorKind::Serde, "bad"), |error, index| {
        error.inside_index(index)
    });
    let start: String = error.message().chars().take(40).collect();
    assert!(error.message().starts_with('…'), "{start}");
    assert!(error.message().len() < 2_048);
    let span = Span::new(4, 5);
    let context = format!(", {}", "c".repeat(70_000));
    let error = Error::new(ErrorKind::Parser, "bad")
        .with_context(&context)
        .with_span(span)
        .with_location(Location::new(2, 3));
    assert_eq!(
        error.to_string(),
        format!("bad{context} at line 2 column 3")
    );
}

#[test]
fn the_location_goes_before_the_context() {
    let span = Span::new(4, 5);
    let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
        .with_context(", opened at line 1 column 1")
        .with_span(span)
        .with_location(Location::new(2, 3));
    assert_eq!(
        error.message(),
        "unterminated flow sequence, opened at line 1 column 1"
    );
    assert_eq!(
        error.to_string(),
        "unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
    );
    let error = error.inside_index(0).inside_key("k");
    assert_eq!(
        error.to_string(),
        "k[0]: unterminated flow sequence at line 2 column 3, opened at line 1 column 1"
    );
    // With no location, the context follows the problem.
    let error = Error::new(ErrorKind::Parser, "unterminated flow sequence")
        .with_context(", opened at line 1 column 1");
    assert_eq!(
        error.to_string(),
        "unterminated flow sequence, opened at line 1 column 1"
    );
}

#[test]
fn categories_display_stably() {
    assert_eq!(ErrorKind::DuplicateKey.to_string(), "duplicate key");
    assert_eq!(ErrorKind::Limit.to_string(), "resource limit");
}

#[test]
fn masked_error_shows_kind_code_and_location_without_message_or_source() {
    fn assert_masked_traits<
        T: Copy
            + Clone
            + PartialEq
            + Eq
            + Send
            + Sync
            + std::fmt::Display
            + std::fmt::Debug
            + std::error::Error,
    >(
        _: T,
    ) {
    }

    let secret = "TOP_SECRET_VALUE_991";
    let error = Error::new(
        ErrorKind::Serde,
        format!("invalid type: string \"{secret}\""),
    )
    .with_code(ErrorCode::InvalidType)
    .with_span(Span::new(10, 20))
    .with_location(Location::new(3, 5))
    .with_context(", in flow mapping")
    .inside_key("secret_key_991")
    .with_source(std::io::Error::other(format!("inner {secret}")));

    assert!(error.to_string().contains(secret));
    assert!(error.to_string().contains("secret_key_991"));

    let masked = error.masked();
    let display = masked.to_string();
    let debug = format!("{masked:?}");
    let pretty_debug = format!("{masked:#?}");

    assert_eq!(
        display,
        "serde error (yaml.invalid-type) at line 3 column 5"
    );
    assert!(!display.contains(secret), "leaked in display: {display}");
    assert!(
        !display.contains("secret_key_991"),
        "leaked key in display: {display}"
    );
    assert!(!debug.contains(secret), "leaked in debug: {debug}");
    assert!(
        !debug.contains("secret_key_991"),
        "leaked key in debug: {debug}"
    );
    assert!(
        !pretty_debug.contains(secret) && !pretty_debug.contains("secret_key_991"),
        "leaked in pretty debug: {pretty_debug}"
    );
    assert!(debug.contains("Serde") && debug.contains("InvalidType"));
    assert!(masked.source().is_none());
    assert_eq!(masked.kind(), ErrorKind::Serde);
    assert_eq!(masked.code(), ErrorCode::InvalidType);
    assert_eq!(masked.span(), Some(Span::new(10, 20)));
    assert_eq!(masked.location(), Some(Location::new(3, 5)));
    assert_eq!(masked.limit(), None);

    let no_loc = Error::new(ErrorKind::Io, format!("write {secret}"))
        .with_source(std::io::Error::other(secret));
    assert_eq!(no_loc.masked().to_string(), "I/O error (yaml.io)");
    assert!(!format!("{:?}", no_loc.masked()).contains(secret));
    assert!(no_loc.masked().source().is_none());

    let with_limit = Error::new(ErrorKind::Limit, format!("too deep {secret}"))
        .with_limit(LimitExceeded::new(LimitKind::Depth, 4, 5));
    assert_eq!(
        with_limit.masked().to_string(),
        "resource limit (yaml.depth-limit)"
    );
    assert_eq!(
        with_limit.masked().limit(),
        Some(LimitExceeded::new(LimitKind::Depth, 4, 5))
    );
    assert!(!format!("{:?}", with_limit.masked()).contains(secret));

    let loc_without_span =
        Error::new(ErrorKind::Parser, format!("bad {secret}")).with_location(Location::new(2, 3));
    assert_eq!(
        loc_without_span.masked().to_string(),
        "parser error (yaml.unexpected-token) at line 2 column 3"
    );
    assert_eq!(loc_without_span.masked().span(), None);
    assert_eq!(
        loc_without_span.masked().location(),
        Some(Location::new(2, 3))
    );

    let span_without_loc =
        Error::new(ErrorKind::Scanner, format!("bad {secret}")).with_span(Span::new(1, 5));
    assert_eq!(
        span_without_loc.masked().to_string(),
        "scanner error (yaml.unexpected-token)"
    );
    assert_eq!(span_without_loc.masked().span(), Some(Span::new(1, 5)));
    assert_eq!(span_without_loc.masked().location(), None);
    assert!(!format!("{:?}", span_without_loc.masked()).contains(secret));
    assert!(!format!("{:#?}", span_without_loc.masked()).contains(secret));

    assert_masked_traits(masked);
}

#[test]
fn masked_error_equality_ignores_redacted_message_and_source() {
    let a = Error::new(ErrorKind::Serde, "secret A")
        .with_code(ErrorCode::InvalidType)
        .with_span(Span::new(0, 4))
        .with_location(Location::new(1, 1))
        .with_source(std::io::Error::other("cause A"));
    let b = Error::new(ErrorKind::Serde, "secret B")
        .with_code(ErrorCode::InvalidType)
        .with_span(Span::new(0, 4))
        .with_location(Location::new(1, 1))
        .with_source(std::io::Error::other("cause B"));
    let c = Error::new(ErrorKind::Serde, "secret A")
        .with_code(ErrorCode::InvalidValue)
        .with_span(Span::new(0, 4))
        .with_location(Location::new(1, 1));

    assert_ne!(a, b);
    assert_eq!(a.masked(), b.masked());
    assert_ne!(a.masked(), c.masked());
}

#[test]
fn path_truncation_and_key_quoting_preserve_utf8_and_bounds() {
    assert_eq!(path_bytes(0), 0);
    assert_eq!(path_bytes(usize::MAX), u16::MAX);

    // Long 4-byte emoji key truncates at MAX_KEY_CHARS without splitting UTF-8.
    let emoji_key = "🚀".repeat(MAX_KEY_CHARS + 10);
    let quoted = quoted_key(&emoji_key);
    assert!(quoted.starts_with("[\""));
    assert!(quoted.ends_with(&format!("{ELIDED}\"]")));
    assert_eq!(quoted.chars().filter(|&c| c == '🚀').count(), MAX_KEY_CHARS);

    // Control chars and BiDi override characters are always escaped in quoted keys.
    let hostile = "a\u{0000}\u{202e}\u{feff}\"\\z";
    let quoted_hostile = quoted_key(hostile);
    assert!(quoted_hostile.chars().all(|c| !hides_text(c)));
    assert!(quoted_hostile.contains("\\u{0}"));
    assert!(quoted_hostile.contains("\\u{202e}"));
    assert!(quoted_hostile.contains("\\u{feff}"));
    assert!(quoted_hostile.contains("\\\""));
    assert!(quoted_hostile.contains("\\\\"));

    // Repeated `inside_key` calls cap path length and become idempotent once elided.
    let mut err = Error::new(ErrorKind::Serde, "bad value");
    for i in 0..40 {
        err = err.inside_key(&format!("segment_with_long_name_{i:02}"));
    }
    assert!(err.message().starts_with(ELIDED));
    let once_elided = err.message().to_owned();
    let after_more = err.inside_key("outer_ignored");
    assert_eq!(after_more.message(), once_elided);
}

#[test]
fn error_debug_formats_public_fields_without_exposing_inner_box() {
    let err = Error::from(LimitExceeded::new(LimitKind::Depth, 4, 5))
        .with_span(Span::new(2, 6))
        .with_location(Location::new(3, 4))
        .with_source(std::io::Error::other("io cause"));

    let rendered = format!("{err:?}");
    assert!(
        rendered.starts_with("Error {"),
        "expected struct Debug prefix, got: {rendered}"
    );
    for field in [
        "kind: Limit",
        "code: DepthLimit",
        "span: Some(",
        "location: Some(",
        "message: ",
        "limit: Some(",
        "source: Some(",
        "io cause",
    ] {
        assert!(
            rendered.contains(field),
            "expected Debug output to contain `{field}`, got: {rendered}"
        );
    }
    assert!(
        !rendered.contains("inner:") && !rendered.contains("ErrorInner"),
        "Debug output must not expose internal Box<ErrorInner> wrapper: {rendered}"
    );
}
