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

use ayaml::{
    Deserializer, EmitOptions, Error, ErrorKind, LimitKind, Limits, ParseOptions, Span,
    SpannedTree, Value,
};
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

fn parse_options(limits: &Limits) -> ParseOptions {
    ParseOptions::new().with_limits(limits.clone())
}

fn cursor(
    input: &str,
    limits: &Limits,
    next: fn(&mut Deserializer<'_>) -> Result<bool, Error>,
) -> Result<(), Error> {
    let mut cursor = Deserializer::from_str_with(input, &parse_options(limits))?;
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
            name: "parse_events_with",
            read: |input, limits| {
                for event in ayaml::parse_events_with(input, &parse_options(limits))? {
                    event?;
                }
                Ok(())
            },
            values: false,
            stream: true,
        },
        Reader {
            name: "parse_value_with",
            read: |input, limits| ayaml::parse_value_with(input, &parse_options(limits)).map(drop),
            values: true,
            stream: false,
        },
        Reader {
            name: "parse_documents_with",
            read: |input, limits| {
                ayaml::parse_documents_with(input, &parse_options(limits)).map(drop)
            },
            values: true,
            stream: true,
        },
        Reader {
            name: "from_str_with (Value)",
            read: |input, limits| {
                ayaml::from_str_with::<Value>(input, &parse_options(limits)).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_str_with (IgnoredAny)",
            read: |input, limits| {
                ayaml::from_str_with::<IgnoredAny>(input, &parse_options(limits)).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_slice_with (Value)",
            read: |input, limits| {
                ayaml::from_slice_with::<Value>(input.as_bytes(), &parse_options(limits)).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_slice_with (IgnoredAny)",
            read: |input, limits| {
                ayaml::from_slice_with::<IgnoredAny>(input.as_bytes(), &parse_options(limits))
                    .map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_str_seed_with (Value)",
            read: |input, limits| {
                ayaml::from_str_seed_with(input, &parse_options(limits), PhantomData::<Value>)
                    .map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "SpannedTree::parse_with",
            read: |input, limits| SpannedTree::parse_with(input, &parse_options(limits)).map(drop),
            values: false,
            stream: true,
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
    readers.extend([
        Reader {
            name: "compat::parse_events_with",
            read: |input, limits| {
                let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
                for event in ayaml::compat::parse_events_with(input, &options)? {
                    event?;
                }
                Ok(())
            },
            values: false,
            stream: true,
        },
        Reader {
            name: "compat::parse_events_eager_with",
            read: |input, limits| {
                let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
                ayaml::compat::parse_events_eager_with(input, &options).map(drop)
            },
            values: false,
            stream: true,
        },
        Reader {
            name: "SpannedTree::parse_compat_with",
            read: |input, limits| {
                let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
                SpannedTree::parse_compat_with(input, &options).map(drop)
            },
            values: false,
            stream: true,
        },
    ]);
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
    readers.extend(extra_typed_readers());
    readers
}

#[allow(deprecated, reason = "tests deprecated 0.x *_with_limits shims")]
fn extra_typed_readers() -> [Reader; 7] {
    [
        Reader {
            name: "from_str_with_limits (Value)",
            read: |input, limits| ayaml::from_str_with_limits::<Value>(input, limits).map(drop),
            values: true,
            stream: false,
        },
        Reader {
            name: "from_slice_with_limits (Value)",
            read: |input, limits| {
                ayaml::from_slice_with_limits::<Value>(input.as_bytes(), limits).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_slice_borrowed_with_limits (IgnoredAny)",
            read: |input, limits| {
                ayaml::from_slice_borrowed_with_limits::<IgnoredAny>(input.as_bytes(), limits)
                    .map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_reader_with (Value)",
            read: |input, limits| {
                ayaml::from_reader_with::<_, Value>(input.as_bytes(), &parse_options(limits))
                    .map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "from_reader_with_limits (Value)",
            read: |input, limits| {
                ayaml::from_reader_with_limits::<_, Value>(input.as_bytes(), limits).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "DocumentDeserializer::from_str_with (Value)",
            read: |input, limits| {
                use serde::Deserialize as _;
                let de = ayaml::DocumentDeserializer::from_str_with(input, &parse_options(limits))?;
                Value::deserialize(de).map(drop)
            },
            values: true,
            stream: false,
        },
        Reader {
            name: "DocumentDeserializer::from_str_with_limits (IgnoredAny)",
            read: |input, limits| {
                use serde::Deserialize as _;
                let de = ayaml::DocumentDeserializer::from_str_with_limits(input, limits)?;
                IgnoredAny::deserialize(de).map(drop)
            },
            values: true,
            stream: false,
        },
    ]
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
    /// The case is asked after each of these lengths of comment lines. One whose least value
    /// depends on the input's length is asked of its own input alone.
    comments: &'static [usize],
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
        comments: &COMMENT_BYTES,
    }
}

/// A case whose least value depends on how long the input is.
const fn by_length(case: Case) -> Case {
    Case {
        comments: &[0],
        ..case
    }
}

/// Comment lines before a case's input, in bytes. A typed reader takes one of two paths by the
/// input's length, at most 4 KiB and above, and a comment adds no event, token, or node, so
/// each case has the same least value on each path. The third length is one more above 4 KiB.
const COMMENT_BYTES: [usize; 3] = [0, 4_200, 8_300];

/// `input` after at least `bytes` bytes of comment lines.
fn after_comments(input: &str, bytes: usize) -> String {
    const LINE: &str = "# a comment line that holds no token\n";
    let mut text = LINE.repeat(bytes.div_ceil(LINE.len()));
    text.push_str(input);
    text
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
        // A scalar with no value still has its text: two quotes.
        case("\"\"\n", LimitKind::ScalarBytes, scalar_bytes, 2),
        case("''\n", LimitKind::ScalarBytes, scalar_bytes, 2),
        // The quotes, and three escaped line breaks of two bytes each, which add no value.
        case(
            "\"\\\n\\\n\\\n\"\n",
            LimitKind::ScalarBytes,
            scalar_bytes,
            8,
        ),
        // `|`, its line break, and three blank lines, which a clipped scalar leaves out of its
        // value.
        case(
            "a: |\n\n\n\nb: c\n",
            LimitKind::ScalarBytes,
            scalar_bytes,
            5,
        ),
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
        by_length(values_only(case(
            "a: &x 1\nb: *x\n",
            LimitKind::AliasExpansion,
            alias_per_byte,
            19,
        ))),
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
        by_length(case("a: 1\n", LimitKind::InputBytes, input_bytes, 5)),
    ]
}

#[test]
fn every_reader_gives_the_same_answer_at_the_same_limits() {
    let readers = readers();
    let mut failures = Vec::new();
    let cases = cases();
    let asked = cases
        .iter()
        .flat_map(|case| case.comments.iter().map(move |bytes| (case, *bytes)));
    for (case, comment_bytes) in asked {
        let input = after_comments(case.input, comment_bytes);
        let input = input.as_str();
        let shown = if comment_bytes == 0 {
            format!("{:?}", case.input)
        } else {
            format!(
                "{:?} after {} bytes of comments",
                case.input,
                input.len() - case.input.len()
            )
        };
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
            let accepts = |value| (reader.read)(input, &(case.set)(Limits::default(), value));
            if case.decreasing {
                // At and below the bound accept; values above it refuse with this limit.
                for value in [boundary.saturating_sub(1), boundary] {
                    if let Err(error) = accepts(value) {
                        failures.push(format!(
                            "{} {:?}: {} refused {value}: {error}",
                            shown, case.kind, reader.name
                        ));
                    }
                }
                for value in [boundary + 1, boundary + 2] {
                    match accepts(value) {
                        Ok(()) => failures.push(format!(
                            "{} {:?}: {} accepted {value}",
                            shown, case.kind, reader.name
                        )),
                        Err(error) => {
                            let kind = error.limit().map(ayaml::LimitExceeded::kind);
                            if error.kind() != ErrorKind::Limit || kind != Some(case.kind) {
                                failures.push(format!(
                                    "{} {:?}: {} refused {value} with {error}",
                                    shown, case.kind, reader.name
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
                        "{} {:?}: {} refused {value}: {error}",
                        shown, case.kind, reader.name
                    ));
                }
            }
            if boundary == 0 {
                continue;
            }
            // Every value below one less refuses the input for the same limit, wherever. After
            // comments, where the plain input has already been asked every value, two are asked.
            let below: Vec<usize> = if comment_bytes == 0 {
                (0..boundary - 1).collect()
            } else {
                let mut below = vec![0, (boundary - 1) / 2];
                below.dedup();
                below.retain(|value| *value < boundary - 1);
                below
            };
            for value in below {
                match accepts(value) {
                    Err(error)
                        if error.limit().map(ayaml::LimitExceeded::kind) == Some(case.kind) => {}
                    other => failures.push(format!(
                        "{} {:?}: {} at {value}: {other:?}",
                        shown, case.kind, reader.name
                    )),
                }
            }
            match accepts(boundary - 1) {
                Ok(()) => failures.push(format!(
                    "{} {:?}: {} accepted {}",
                    shown,
                    case.kind,
                    reader.name,
                    boundary - 1
                )),
                Err(error) => {
                    let kind = error.limit().map(ayaml::LimitExceeded::kind);
                    if error.kind() != ErrorKind::Limit || kind != Some(case.kind) {
                        failures.push(format!(
                            "{} {:?}: {} refused {} with {error}",
                            shown,
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
                    "{} {:?}: {name} refused at {span:?}, {first} at {expected:?}",
                    shown, case.kind
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
        ("to_string_value_with", |value, _, limits| {
            ayaml::to_string_value_with(value, &EmitOptions::new().with_limits(limits.clone()))
                .map(|text| text.len())
        }),
        ("to_string_with", |value, _, limits| {
            ayaml::to_string_with(value, &EmitOptions::new().with_limits(limits.clone()))
                .map(|text| text.len())
        }),
        ("to_string_documents_with", |value, _, limits| {
            ayaml::to_string_documents_with(
                std::slice::from_ref(value),
                &EmitOptions::new().with_limits(limits.clone()),
            )
            .map(|text| text.len())
        }),
        ("to_string_events_with", |_, events, limits| {
            ayaml::to_string_events_with(events, &EmitOptions::new().with_limits(limits.clone()))
                .map(|text| text.len())
        }),
        ("to_writer_value_with", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_value_with(
                &mut output,
                value,
                &EmitOptions::new().with_limits(limits.clone()),
            )
            .map(|()| output.len())
        }),
        ("to_writer_documents_with", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_documents_with(
                &mut output,
                std::slice::from_ref(value),
                &EmitOptions::new().with_limits(limits.clone()),
            )
            .map(|()| output.len())
        }),
        ("to_writer_with", |value, _, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_with(
                &mut output,
                value,
                &EmitOptions::new().with_limits(limits.clone()),
            )
            .map(|()| output.len())
        }),
        ("to_writer_events_with", |_, events, limits| {
            let mut output = Vec::new();
            ayaml::to_writer_events_with(
                &mut output,
                events,
                &EmitOptions::new().with_limits(limits.clone()),
            )
            .map(|()| output.len())
        }),
        ("to_writer_event_stream_with", |_, _, limits| {
            let mut output = Vec::new();
            let stream = ayaml::parse_events("a\n").expect("events");
            ayaml::to_writer_event_stream_with(
                &mut output,
                stream,
                &EmitOptions::new().with_limits(limits.clone()),
            )
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
        let options = ParseOptions::new();
        let eager = ayaml::internals::parse_events(input, &options.limits);
        let lazy = ayaml::parse_events_with(input, &options)
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

/// Compatibility typed and visitor readers enforce the same single-document limit boundaries
/// through `compat::ParseOptions` (mapping depth failures to `ErrorKind::Serde` as documented).
#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_typed_readers_honor_every_single_document_limit() {
    let readers: [(&str, Read); 3] = [
        ("compat::from_str_with", |input, limits| {
            let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
            ayaml::compat::from_str_with::<Value>(input, &options).map(drop)
        }),
        ("compat::from_str_seed_with", |input, limits| {
            let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
            ayaml::compat::from_str_seed_with(input, &options, PhantomData::<Value>).map(drop)
        }),
        ("compat::deserialize_any_with", |input, limits| {
            let options = ayaml::compat::ParseOptions::new().with_limits(limits.clone());
            ayaml::compat::deserialize_any_with(input, &options, IgnoredAny).map(drop)
        }),
    ];
    for case in cases().into_iter().filter(|case| !case.several) {
        let boundary = case.least;
        for (name, read) in readers {
            let run = |value| read(case.input, &(case.set)(Limits::default(), value));
            if case.decreasing {
                assert!(run(boundary).is_ok(), "{name} {:?}: {boundary}", case.kind);
                let error = run(boundary + 1).expect_err("above bound");
                assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
                assert_eq!(
                    error.limit().map(ayaml::LimitExceeded::kind),
                    Some(case.kind),
                    "{name}: {error}"
                );
            } else {
                assert!(run(boundary).is_ok(), "{name} {:?}: {boundary}", case.kind);
                if boundary > 0 {
                    let error = run(boundary - 1).expect_err("below bound");
                    if case.kind == LimitKind::Depth {
                        assert_eq!(error.kind(), ErrorKind::Serde, "{name}: {error}");
                    } else {
                        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
                        assert_eq!(
                            error.limit().map(ayaml::LimitExceeded::kind),
                            Some(case.kind),
                            "{name}: {error}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn invalid_limits_take_precedence_over_input_size_and_root_size_hint_is_clamped() {
    #[derive(Debug)]
    struct HintProbe;
    impl<'de> serde::Deserialize<'de> for HintProbe {
        fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            struct V;
            impl<'de> serde::de::Visitor<'de> for V {
                type Value = HintProbe;
                fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    f.write_str("seq")
                }
                fn visit_seq<A: serde::de::SeqAccess<'de>>(
                    self,
                    mut seq: A,
                ) -> Result<Self::Value, A::Error> {
                    if let Some(hint) = seq.size_hint() {
                        assert!(
                            hint <= 4,
                            "root flow size_hint must be clamped to max_collection_entries (got {hint})"
                        );
                    }
                    while seq.next_element::<IgnoredAny>()?.is_some() {}
                    Ok(HintProbe)
                }
            }
            deserializer.deserialize_seq(V)
        }
    }

    let bad = Limits::new()
        .with_max_depth(ayaml::MAX_DEPTH_CAP + 1)
        .with_max_input_bytes(1);
    let opts = ParseOptions::new().with_limits(bad);
    let over = "a: 1\nb: 2\n";
    for err in [
        ayaml::parse_value_with(over, &opts).unwrap_err(),
        ayaml::from_str_with::<Value>(over, &opts).unwrap_err(),
        ayaml::from_slice_with::<Value>(over.as_bytes(), &opts).unwrap_err(),
        ayaml::from_reader_with::<_, Value>(over.as_bytes(), &opts).unwrap_err(),
        ayaml::DocumentDeserializer::from_str_with(over, &opts).unwrap_err(),
    ] {
        assert_eq!(err.code(), ayaml::ErrorCode::InvalidLimits);
    }

    // Streaming typed path (> 4 KiB) clamps root flow collection size_hint() to
    // `max_collection_entries` so a custom Deserialize visitor never pre-allocates past the limit.
    let wide_flow = format!("[{}1]", "1,".repeat(3_000));
    assert!(wide_flow.len() > 4096);
    let tight = ParseOptions::new().with_limits(Limits::new().with_max_collection_entries(4));
    let err = ayaml::from_str_with::<HintProbe>(&wide_flow, &tight).expect_err("over entries");
    assert_eq!(err.code(), ayaml::ErrorCode::CollectionEntriesLimit);
}

/// Raising `max_depth` must never swap an earlier non-limit parser error for a later scanner
/// error (regression from scheduled proptest `invariants::limits_are_monotonic`).
#[test]
fn raising_max_depth_keeps_earlier_parser_error_before_later_scanner_error() {
    // At `max_depth = 0`, the scanner stops at `{` before `![bad` and the parser reports `}}-x`.
    // Raising `max_depth` to 2 must still report `}}-x` rather than swapping to `InvalidTag`.
    let default_input = "}}-x\n{ ![bad }\n";
    let lo_opts = ParseOptions::new().with_limits(Limits::default().with_max_depth(0));
    let hi_opts = ParseOptions::new().with_limits(Limits::default().with_max_depth(2));
    for (label, lo, hi) in [
        (
            "parse_value_with",
            ayaml::parse_value_with(default_input, &lo_opts).unwrap_err(),
            ayaml::parse_value_with(default_input, &hi_opts).unwrap_err(),
        ),
        (
            "from_str_with",
            ayaml::from_str_with::<Value>(default_input, &lo_opts).unwrap_err(),
            ayaml::from_str_with::<Value>(default_input, &hi_opts).unwrap_err(),
        ),
        (
            "parse_documents_with",
            ayaml::parse_documents_with(default_input, &lo_opts).unwrap_err(),
            ayaml::parse_documents_with(default_input, &hi_opts).unwrap_err(),
        ),
        (
            "parse_events_with",
            ayaml::parse_events_with(default_input, &lo_opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .unwrap_err(),
            ayaml::parse_events_with(default_input, &hi_opts)
                .and_then(Iterator::collect::<Result<Vec<_>, _>>)
                .unwrap_err(),
        ),
    ] {
        assert_eq!(lo.kind(), hi.kind(), "{label}: {lo:?} -> {hi:?}");
        assert_eq!(lo.code(), hi.code(), "{label}: {lo:?} -> {hi:?}");
        assert_eq!(lo.span(), hi.span(), "{label}: {lo:?} -> {hi:?}");
    }

    #[cfg(feature = "libyaml-compat")]
    {
        let compat_input = "\u{feff}---\n!local\na: {'café 日本語': !<tag:example.com,2000:node> {'explicit_a': ![custom \"plain line\\n  indented line\\n- list looking\"}, name: !<tag:example.com,2000:node> [!custom \"- list looking\\nplain line\"]}\n";
        let compat_lo =
            ayaml::compat::ParseOptions::new().with_limits(Limits::default().with_max_depth(0));
        let compat_hi =
            ayaml::compat::ParseOptions::new().with_limits(Limits::default().with_max_depth(2));
        let lo = ayaml::compat::from_str_with::<Value>(compat_input, &compat_lo).unwrap_err();
        let hi = ayaml::compat::from_str_with::<Value>(compat_input, &compat_hi).unwrap_err();
        assert_eq!(lo.kind(), hi.kind(), "{lo:?} -> {hi:?}");
        assert_eq!(lo.code(), hi.code(), "{lo:?} -> {hi:?}");
        assert_eq!(lo.span(), hi.span(), "{lo:?} -> {hi:?}");
    }
}
