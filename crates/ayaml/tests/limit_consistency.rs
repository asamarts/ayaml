//! The same input at the same limits gets the same answer from every reader that takes limits.
//!
//! Each case names a limit and the least value of it that accepts the input. Every value below
//! it refuses the input, on every reader, with that limit's kind. At one less, where each case
//! breaks the limit at one place only, every reader refuses at that place. Further below, a
//! stream can break the limit in more than one document, and the readers that read a document
//! at a time may stop at an earlier one, as `docs/limits.md` says. The least values are worked
//! out by hand in each case's comment, from what `docs/limits.md` says each limit counts, not
//! read off the parser.

#![allow(
    clippy::needless_pass_by_value,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use std::marker::PhantomData;

use ayaml::{Deserializer, Error, ErrorKind, LimitKind, Limits, Span, Value};
use serde::de::IgnoredAny;

type Read = fn(&str, &Limits) -> Result<(), Error>;
type Emit = fn(&Value, &[ayaml::Event<'static>], &Limits) -> Result<usize, Error>;

/// A reader that takes limits.
struct Reader {
    name: &'static str,
    read: Read,
    /// It builds values, so an alias copies its target there.
    values: bool,
    /// It reads every document of a stream, not only one.
    stream: bool,
}

fn cursor(
    input: &str,
    limits: &Limits,
    next: fn(&mut Deserializer<'_>) -> Result<bool, Error>,
) -> Result<(), Error> {
    let mut cursor = Deserializer::from_str_with_limits(input, limits)?;
    while next(&mut cursor)? {}
    Ok(())
}

fn readers() -> Vec<Reader> {
    #[cfg_attr(
        not(any(feature = "libyaml-compat", feature = "bench-internals")),
        allow(unused_mut, reason = "the list is extended only by optional readers")
    )]
    let mut readers = vec![
        Reader {
            name: "parse_events_with_limits",
            read: |input, limits| {
                for event in ayaml::parse_events_with_limits(input, limits)? {
                    event?;
                }
                Ok(())
            },
            values: false,
            stream: true,
        },
        Reader {
            name: "parse_value_with_limits",
            read: |input, limits| ayaml::parse_value_with_limits(input, limits).map(drop),
            values: true,
            stream: false,
        },
        Reader {
            name: "parse_documents_with_limits",
            read: |input, limits| ayaml::parse_documents_with_limits(input, limits).map(drop),
            values: true,
            stream: true,
        },
        Reader {
            name: "from_str_borrowed_with_limits (Value)",
            read: |input, limits| {
                ayaml::from_str_borrowed_with_limits::<Value>(input, limits).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_str_borrowed_with_limits (IgnoredAny)",
            read: |input, limits| {
                ayaml::from_str_borrowed_with_limits::<IgnoredAny>(input, limits).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "Deserializer::next",
            read: |input, limits| {
                cursor(input, limits, |cursor| {
                    Ok(cursor.next::<Value>()?.is_some())
                })
            },
            values: true,
            stream: true,
        },
        Reader {
            name: "Deserializer::next_borrowed",
            read: |input, limits| {
                cursor(input, limits, |cursor| {
                    Ok(cursor.next_borrowed::<IgnoredAny>()?.is_some())
                })
            },
            values: true,
            stream: true,
        },
        Reader {
            name: "Deserializer::next_seed",
            read: |input, limits| {
                cursor(input, limits, |cursor| {
                    Ok(cursor.next_seed(PhantomData::<Value>)?.is_some())
                })
            },
            values: true,
            stream: true,
        },
        Reader {
            name: "Deserializer::next_seed_borrowed",
            read: |input, limits| {
                cursor(input, limits, |cursor| {
                    Ok(cursor
                        .next_seed_borrowed(PhantomData::<IgnoredAny>)?
                        .is_some())
                })
            },
            values: true,
            stream: true,
        },
    ];
    #[cfg(feature = "libyaml-compat")]
    readers.push(Reader {
        name: "compat::parse_events_with_limits",
        read: |input, limits| ayaml::compat::parse_events_with_limits(input, limits).map(drop),
        values: false,
        stream: true,
    });
    #[cfg(feature = "bench-internals")]
    readers.extend([
        Reader {
            name: "internals::parse_events",
            read: |input, limits| ayaml::internals::parse_events(input, limits).map(drop),
            values: false,
            stream: true,
        },
        Reader {
            name: "internals::compose_events",
            read: |input, limits| {
                let events = ayaml::internals::parse_events(input, limits)?;
                ayaml::internals::compose_events(&events, input.len(), limits).map(drop)
            },
            values: true,
            stream: true,
        },
    ]);
    readers
}

/// An input and a limit, with the least value of the limit that accepts it.
struct Case {
    input: &'static str,
    kind: LimitKind,
    set: fn(Limits, usize) -> Limits,
    /// The least accepted value on readers that build values.
    least: usize,
    /// The least accepted value on event readers, which don't copy an alias's target. It's
    /// `least` unless the limit bounds what an alias copies.
    least_events: usize,
    /// The input holds more than one document, which a reader of one document refuses
    /// whatever the limits, so only the stream readers are asked.
    several: bool,
    /// The limit is looser as its value grows, so `least` is the largest accepted value and
    /// values above it are refused.
    decreasing: bool,
    /// Event readers don't copy an alias's target, so an alias-budget limit doesn't apply to
    /// them.
    events: bool,
}

const fn case(
    input: &'static str,
    kind: LimitKind,
    set: fn(Limits, usize) -> Limits,
    least: usize,
) -> Case {
    Case {
        input,
        kind,
        set,
        least,
        least_events: least,
        several: false,
        decreasing: false,
        events: true,
    }
}

/// A case whose input holds more than one document.
const fn stream(case: Case) -> Case {
    Case {
        several: true,
        ..case
    }
}

/// A case whose boundary is a maximum: larger values are refused.
const fn decreasing(case: Case) -> Case {
    Case {
        decreasing: true,
        ..case
    }
}

/// A case that only the readers that build values are asked, because an event reader does not
/// copy what an alias names.
const fn values_only(case: Case) -> Case {
    Case {
        events: false,
        ..case
    }
}

fn events(limits: Limits, value: usize) -> Limits {
    limits.with_max_events(value)
}

fn depth(limits: Limits, value: usize) -> Limits {
    limits.with_max_depth(value)
}

fn anchors(limits: Limits, value: usize) -> Limits {
    limits.with_max_anchors(value)
}

fn scalar_bytes(limits: Limits, value: usize) -> Limits {
    limits.with_max_scalar_bytes(value)
}

/// The fixed alias allowance alone, with nothing added for the input's size.
fn alias_bytes(limits: Limits, value: usize) -> Limits {
    limits
        .with_alias_bytes_per_input_byte(0)
        .with_max_alias_expansion_bytes(value)
}

/// A node cost, with the fixed alias allowance that bounds it.
fn node_cost(limits: Limits, value: usize) -> Limits {
    limits
        .with_node_cost_bytes(value)
        .with_alias_bytes_per_input_byte(0)
        .with_max_alias_expansion_bytes(257)
}

/// The per-input-byte allowance alone, with no fixed allowance.
fn alias_per_byte(limits: Limits, value: usize) -> Limits {
    limits
        .with_alias_bytes_per_input_byte(value)
        .with_max_alias_expansion_bytes(0)
}

fn dereferences(limits: Limits, value: usize) -> Limits {
    limits.with_max_alias_dereferences(value)
}

fn documents(limits: Limits, value: usize) -> Limits {
    limits.with_max_documents(value)
}

fn entries(limits: Limits, value: usize) -> Limits {
    limits.with_max_collection_entries(value)
}

fn tag_bytes(limits: Limits, value: usize) -> Limits {
    limits.with_max_tag_expansion_bytes(value)
}

fn input_bytes(limits: Limits, value: usize) -> Limits {
    limits.with_max_input_bytes(value)
}

/// The cases. An event count includes the stream's start and end; a token count is the
/// scalars, properties, indicators, markers and directives. `max_events` bounds both.
///
/// `max_output_bytes` applies to emitters rather than readers, and is covered by
/// `every_emitter_gives_the_same_output_limit_answer`. An event reader does not copy an alias's
/// target, so `node_cost_bytes` and `alias_bytes_per_input_byte` are asked only of readers that
/// build values.
fn cases() -> Vec<Case> {
    vec![
        // Events: stream start, document start, mapping start, `a`, `1`, mapping end, document
        // end, stream end. Tokens: `a`, `:`, `1`.
        case("a: 1\n", LimitKind::Events, events, 8),
        // Blank lines add neither events nor tokens: 10 events, 6 tokens.
        case(
            "a: 1\n\n\n\n\n\n\n\n\n\n\nb: 2\n",
            LimitKind::Events,
            events,
            10,
        ),
        // Nor do comment lines, or a comment after a node.
        case(
            "a: 1 # c\n# c\n# c\n  # c\n#\nb: 2\n",
            LimitKind::Events,
            events,
            10,
        ),
        // In a flow sequence: 8 events, and `[`, `a`, `,`, `b`, `]`.
        case("[a,\n\n b]\n", LimitKind::Events, events, 8),
        // Inside a block scalar the blank lines are its text: 10 events, 6 tokens.
        case("a: |\n  x\n\n\n  y\nb: 2\n", LimitKind::Events, events, 10),
        // Tokens outnumber events here: each line is `-`, an anchor, a tag and a scalar, so 12
        // tokens, and 9 events.
        case(
            "- &a !t x\n- &b !t y\n- &c !t z\n",
            LimitKind::Events,
            events,
            12,
        ),
        // A directive is an event: stream start, directive, document start, `a`, document end,
        // stream end.
        case("%YAML 1.2\n---\na\n...\n", LimitKind::Events, events, 6),
        // Three events for each document, and the stream's two.
        stream(case("a\n---\nb\n---\nc\n", LimitKind::Events, events, 11)),
        // The first document has 12 events: its start and end, three mapping starts and ends,
        // `a`, `b`, `c`, and the empty value. The second has 6: its start and end, the
        // sequence's, `x` and `y`. With the stream's two, 20. Tokens: 6 for `a:`, `b:` and
        // `c:`, the `---`, and 8 for the entries. From 7 to 12, a reader of a document at a time
        // stops at an event of the first document, and the others at a token of the second.
        stream(case(
            "a:\n b:\n  c:\n---\n- &a !t x\n- &b !t y\n",
            LimitKind::Events,
            events,
            20,
        )),
        // Depth: the sequence, the pair's mapping inside it, and the inner sequence.
        case("[a: [1]]\n", LimitKind::Depth, depth, 3),
        // The same with an explicit key.
        case("[? a : [1]]\n", LimitKind::Depth, depth, 3),
        // A key's collection nests inside the pair's mapping, as a value's does.
        case("[[a]: 1]\n", LimitKind::Depth, depth, 3),
        case("[? [a] : 1]\n", LimitKind::Depth, depth, 3),
        // A block mapping's key is inside the mapping: the mapping, and the key's collection.
        case("[a]: c\n", LimitKind::Depth, depth, 2),
        case("{a: b}: c\n", LimitKind::Depth, depth, 2),
        case("&x [a]: c\n", LimitKind::Depth, depth, 2),
        // The sequence, the mapping in it, and the key's sequence.
        case("- [a]: c\n", LimitKind::Depth, depth, 3),
        // Two block sequences and the flow sequence.
        case("- - [1]\n", LimitKind::Depth, depth, 3),
        // The mapping, the sequence, the flow mapping, and the flow sequence.
        case("a:\n  - {b: [c]}\n", LimitKind::Depth, depth, 4),
        // The first document is 2 deep and the second 3. Below 2, a reader of a document at a
        // time stops in the first document, and the others in the second, where the scanner
        // meets a `[`.
        stream(case(
            "a:\n  - b\n---\n[[[1]]]\n",
            LimitKind::Depth,
            depth,
            3,
        )),
        // An alias where a value is built nests its target's two sequences inside the mapping and
        // `[*x]`. An event reader sees the alias alone.
        Case {
            least_events: 3,
            ..case("a: &x [[1]]\nb: [*x]\n", LimitKind::Depth, depth, 4)
        },
        // Anchors count for the whole stream.
        stream(case(
            "&a 1\n---\n&b 2\n---\n&c 3\n",
            LimitKind::Anchors,
            anchors,
            3,
        )),
        // Three anchors in one document, one of them on a key.
        case("[&a 1, &b {&c k: v}]\n", LimitKind::Anchors, anchors, 3),
        // A scalar's size is the larger of its text, quotes included, and its value: 8 bytes of
        // text, and 3 of value for each `\L`.
        case("\"\\L\\L\\L\"\n", LimitKind::ScalarBytes, scalar_bytes, 9),
        case(
            "\"\\P\\P\\P\\P\"\n",
            LimitKind::ScalarBytes,
            scalar_bytes,
            12,
        ),
        // 5 bytes of text, and 3 of value.
        case("'abc'\n", LimitKind::ScalarBytes, scalar_bytes, 5),
        case("\"a\\tb\"\n", LimitKind::ScalarBytes, scalar_bytes, 6),
        // `|`, its line break, and `  abc` with its line break.
        case("|\n  abc\n", LimitKind::ScalarBytes, scalar_bytes, 8),
        // An alias is charged 256 for each node it copies, and the text: here the tagged node,
        // `!t`, the scalar, and `abc`.
        Case {
            least_events: 0,
            ..case(
                "a: &x !t abc\nb: *x\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                256 + 2 + 256 + 3,
            )
        },
        // The tagged sequence, `!t`, the sequence, and `1`.
        Case {
            least_events: 0,
            ..case(
                "a: &x !t [1]\nb: *x\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                256 + 2 + 256 + 256,
            )
        },
        // A tag inside the copy costs the same.
        Case {
            least_events: 0,
            ..case(
                "a: &x [!t 1]\nb: *x\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                256 + 256 + 256 + 2,
            )
        },
        // A core tag on a collection is dropped, so it adds nothing.
        Case {
            least_events: 0,
            ..case(
                "a: &x !!seq [1]\nb: *x\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                256 + 256,
            )
        },
        // Two sequences and two scalars.
        Case {
            least_events: 0,
            ..case(
                "a: &x [1, [2]]\nb: *x\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                4 * 256,
            )
        },
        // `*y` copies the sequence and the two copies of `x` in it, after two aliases to `x`,
        // so `x`'s two nodes are copied four times and `y`'s own node once.
        Case {
            least_events: 0,
            ..case(
                "a: &x [1]\nb: &y [*x, *x]\nc: *y\n",
                LimitKind::AliasExpansion,
                alias_bytes,
                2 * 512 + (256 + 2 * 512),
            )
        },
        // A node's cost is charged against the alias budget. A number's text adds nothing, so
        // with a fixed allowance of 257, 257 is the largest accepted node cost.
        values_only(decreasing(case(
            "a: &x 1\nb: *x\n",
            LimitKind::AliasExpansion,
            node_cost,
            257,
        ))),
        // The input is 14 bytes and the alias copies a 256-byte node. With no fixed allowance,
        // 19 bytes per input byte is the least that reaches it.
        values_only(case(
            "a: &x 1\nb: *x\n",
            LimitKind::AliasExpansion,
            alias_per_byte,
            19,
        )),
        // Each alias is one dereference, wherever its target is.
        Case {
            least_events: 0,
            ..case(
                "a: &x {k: v}\nb: *x\nc: *x\n",
                LimitKind::AliasDereferences,
                dereferences,
                2,
            )
        },
        stream(case("a\n---\nb\n", LimitKind::Documents, documents, 2)),
        // Three entries, and two pairs.
        case("[a, b, c]\n", LimitKind::CollectionEntries, entries, 3),
        case("a: 1\nb: 2\n", LimitKind::CollectionEntries, entries, 2),
        // Each place that counts entries: flow and block mappings, flow and block sequences,
        // explicit keys, and the short-block fast paths.
        case("{a: 1, b: 2}\n", LimitKind::CollectionEntries, entries, 2),
        case("- a\n- b\n", LimitKind::CollectionEntries, entries, 2),
        case(
            "? a\n: 1\n? b\n: 2\n",
            LimitKind::CollectionEntries,
            entries,
            2,
        ),
        case(
            "a: 1\nb: 2\nc: 3\n",
            LimitKind::CollectionEntries,
            entries,
            3,
        ),
        case("- a\n- b\n- c\n", LimitKind::CollectionEntries, entries, 3),
        // `!!` stands for the 18 bytes of `tag:yaml.org,2002:`.
        case("!!str a\n", LimitKind::TagExpansion, tag_bytes, 18),
        // `!` stands for itself, once for each use.
        case("- !t a\n- !t b\n", LimitKind::TagExpansion, tag_bytes, 2),
        // Five bytes.
        case("a: 1\n", LimitKind::InputBytes, input_bytes, 5),
    ]
}

#[test]
fn every_reader_gives_the_same_answer_at_the_same_limits() {
    let readers = readers();
    let mut failures = Vec::new();
    for case in cases() {
        let mut places: Vec<(usize, &str, Option<Span>)> = Vec::new();
        for reader in readers
            .iter()
            .filter(|reader| (reader.stream || !case.several) && (case.events || reader.values))
        {
            let boundary = if reader.values {
                case.least
            } else {
                case.least_events
            };
            let accepts = |value| (reader.read)(case.input, &(case.set)(Limits::default(), value));
            if case.decreasing {
                // At and below the bound accept; values above it refuse with this limit.
                for value in [boundary.saturating_sub(1), boundary] {
                    if let Err(error) = accepts(value) {
                        failures.push(format!(
                            "{:?} {:?}: {} refused {value}: {error}",
                            case.input, case.kind, reader.name
                        ));
                    }
                }
                for value in [boundary + 1, boundary + 2] {
                    match accepts(value) {
                        Ok(()) => failures.push(format!(
                            "{:?} {:?}: {} accepted {value}",
                            case.input, case.kind, reader.name
                        )),
                        Err(error) => {
                            let kind = error.limit().map(ayaml::LimitExceeded::kind);
                            if error.kind() != ErrorKind::Limit || kind != Some(case.kind) {
                                failures.push(format!(
                                    "{:?} {:?}: {} refused {value} with {error}",
                                    case.input, case.kind, reader.name
                                ));
                            }
                            places.push((value, reader.name, error.span()));
                        }
                    }
                }
                continue;
            }
            for value in [boundary, boundary + 1] {
                if let Err(error) = accepts(value) {
                    failures.push(format!(
                        "{:?} {:?}: {} refused {value}: {error}",
                        case.input, case.kind, reader.name
                    ));
                }
            }
            if boundary == 0 {
                continue;
            }
            // Every value below one less refuses the input for the same limit, wherever.
            for value in 0..boundary - 1 {
                match accepts(value) {
                    Err(error)
                        if error.limit().map(ayaml::LimitExceeded::kind) == Some(case.kind) => {}
                    other => failures.push(format!(
                        "{:?} {:?}: {} at {value}: {other:?}",
                        case.input, case.kind, reader.name
                    )),
                }
            }
            match accepts(boundary - 1) {
                Ok(()) => failures.push(format!(
                    "{:?} {:?}: {} accepted {}",
                    case.input,
                    case.kind,
                    reader.name,
                    boundary - 1
                )),
                Err(error) => {
                    let kind = error.limit().map(ayaml::LimitExceeded::kind);
                    if error.kind() != ErrorKind::Limit || kind != Some(case.kind) {
                        failures.push(format!(
                            "{:?} {:?}: {} refused {} with {error}",
                            case.input,
                            case.kind,
                            reader.name,
                            boundary - 1
                        ));
                    }
                    places.push((boundary, reader.name, error.span()));
                }
            }
        }
        // Readers refusing at the same value refuse at the same place.
        for (least, name, span) in &places {
            let (_, first, expected) = places
                .iter()
                .find(|(other, _, _)| other == least)
                .expect("itself");
            if span != expected {
                failures.push(format!(
                    "{:?} {:?}: {name} refused at {span:?}, {first} at {expected:?}",
                    case.input, case.kind
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Every emitter that takes limits refuses one byte below the output it would write, and accepts
/// that byte and one more. The entry point that starts from an event stream parses it again, so
/// the output limit is the only limit under test.
#[test]
fn every_emitter_gives_the_same_output_limit_answer() {
    let value = ayaml::parse_value("a\n").expect("value");
    let events: Vec<ayaml::Event<'static>> = ayaml::parse_events("a\n")
        .expect("events")
        .collect::<Result<_, _>>()
        .expect("events");
    let emitters: Vec<(&str, Emit)> = vec![
        ("to_string_value_with_limits", |value, _, limits| {
            ayaml::to_string_value_with_limits(value, limits).map(|text| text.len())
        }),
        ("to_string_with_limits", |value, _, limits| {
            ayaml::to_string_with_limits(value, limits).map(|text| text.len())
        }),
        ("to_string_documents_with_limits", |value, _, limits| {
            ayaml::to_string_documents_with_limits(std::slice::from_ref(value), limits)
                .map(|text| text.len())
        }),
        ("to_string_events_with_limits", |_, events, limits| {
            ayaml::to_string_events_with_limits(events, limits).map(|text| text.len())
        }),
        ("to_writer_value_with_limits", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_value_with_limits(&mut output, value, limits).map(|()| output.len())
        }),
        ("to_writer_documents_with_limits", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_documents_with_limits(&mut output, std::slice::from_ref(value), limits)
                .map(|()| output.len())
        }),
        ("to_writer_with_limits", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_with_limits(&mut output, value, limits).map(|()| output.len())
        }),
        ("to_writer_event_stream_with_limits", |_, _, limits| {
            let mut output = Vec::new();
            let stream = ayaml::parse_events("a\n").expect("events");
            ayaml::to_writer_event_stream_with_limits(&mut output, stream, limits)
                .map(|()| output.len())
        }),
    ];
    for (name, emit) in emitters {
        let wide = Limits::default().with_max_output_bytes(usize::MAX);
        let least = emit(&value, &events, &wide)
            .unwrap_or_else(|error| panic!("{name} refused usize::MAX: {error}"));
        for allowed in [least, least + 1] {
            emit(
                &value,
                &events,
                &Limits::default().with_max_output_bytes(allowed),
            )
            .unwrap_or_else(|error| panic!("{name} refused {allowed} bytes: {error}"));
        }
        let error = emit(
            &value,
            &events,
            &Limits::default().with_max_output_bytes(least - 1),
        )
        .expect_err("a byte below the output");
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        assert_eq!(
            error.limit().map(ayaml::LimitExceeded::kind),
            Some(LimitKind::OutputBytes),
            "{name}: {error}"
        );
    }
}

/// The lazy and eager event readers give the same events, with the same places, the stream's
/// start and end included, and an empty node where a document or the input ends. An input they
/// refuse, they refuse with the same error at the same place.
#[cfg(feature = "bench-internals")]
#[test]
fn lazy_and_eager_events_match() {
    for input in [
        "a\n",
        "\u{feff}a\n",
        "a",
        "",
        "# c\n",
        "\n\n",
        "a\n---\nb\n",
        "%YAML 1.2\n---\na\n...\n",
        "- [a, b: c]\n",
        "a:",
        "a:\n",
        "a:\n--- b\n",
        "a:\n\n# c\n\n--- b\n",
        "---\n---\n",
        "- a:\n--- !t\nx:\n  - y: 1\n    z:\n",
        "a:\n...\n--- b:\n",
        // Refused.
        "%YAML 1.2\n",
        "a: [b\n",
        "a:\n---\n[\n",
        "a: 1\n---\nb: [\n",
    ] {
        let limits = Limits::default();
        let eager = ayaml::internals::parse_events(input, &limits);
        let lazy = ayaml::parse_events_with_limits(input, &limits)
            .and_then(Iterator::collect::<Result<Vec<_>, _>>);
        match (lazy, eager) {
            (Ok(lazy), Ok(eager)) => assert_eq!(lazy, eager, "{input:?}"),
            (Err(lazy), Err(eager)) => assert_eq!(
                (lazy.to_string(), lazy.span()),
                (eager.to_string(), eager.span()),
                "{input:?}"
            ),
            (lazy, eager) => panic!("{input:?}: lazy {lazy:?}, eager {eager:?}"),
        }
    }
}
