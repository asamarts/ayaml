#![allow(
    clippy::items_after_statements,
    reason = "test fixtures favor compact setup, generated inputs, and literal YAML braces"
)]

use super::*;

/// A block collection starts after a line break (`s-l+block-collection`), so none starts on a
/// `---` line, after another document or not, on any entry point, as libyaml refuses them. A
/// scalar or a flow collection may sit there, and a block collection on the next line.
#[test]
fn a_block_collection_cannot_start_on_a_document_start_line() {
    for (input, at) in [
        ("--- a: b", "1 column 5"),
        ("--- a: b\nc: d", "1 column 5"),
        ("--- a:\tb", "1 column 5"),
        ("--- - a", "1 column 5"),
        ("--- -", "1 column 5"),
        ("--- ? a", "1 column 5"),
        ("--- ?", "1 column 5"),
        ("--- : a", "1 column 5"),
        ("--- :", "1 column 5"),
        ("--- \"a\": b", "1 column 5"),
        ("--- 'a': b", "1 column 5"),
        ("--- [a]: b", "1 column 5"),
        ("--- --- :", "1 column 5"),
        // After a block collection, which a `---` line ends.
        ("a: 1\n--- b: c", "2 column 5"),
        ("- a\n--- - b", "2 column 5"),
        ("k: \n---  : ", "2 column 6"),
        ("a:\n  b: 1\n--- c: d", "3 column 5"),
    ] {
        for (entry, read) in entry_points(input) {
            assert_eq!(
                read,
                Err(format!(
                    "a block collection cannot start on a `---` line at line {at}"
                )),
                "{input:?} on {entry}"
            );
        }
    }
    for input in [
        "--- a",
        "--- a:b",
        "--- -a",
        "--- ?a",
        "--- [a]",
        "--- {a: b}",
        "--- |\n a",
        "--- # c\n- a",
        "--- !t\na: b",
        "---\na: b",
    ] {
        for (entry, read) in entry_points(input) {
            assert_eq!(read, Ok(()), "{input:?} on {entry}");
        }
    }
    // A stream reader reads the next document's scalar on its `---` line, and its collection on
    // the next line.
    let streams = [
        "parse_documents",
        "Deserializer",
        "parse_events",
        "compat::parse_events_with_limits",
    ];
    for input in ["a: 1\n--- b", "- a\n--- [b]", "a: 1\n...\n---\nb: c"] {
        for (entry, read) in entry_points(input) {
            if streams.contains(&entry) {
                assert_eq!(read, Ok(()), "{input:?} on {entry}");
            }
        }
    }
    // A `...` line ends a block collection too, and then holds nothing.
    for (entry, read) in entry_points("a: 1\n... b: c") {
        assert_eq!(
            read,
            Err("content after document end marker at line 2 column 5".to_owned()),
            "on {entry}"
        );
    }
}

/// A flow collection's `?` before a flow indicator is neither a key's indicator, which needs a
/// space or a line break after it, nor a plain scalar, so it's an error in both modes. libyaml
/// reads `{?}`, `{?,a}` and `[?[a]]`, and refuses `[?]` only because it drops the `]`. Where it
/// refuses the input too, as in `[?}`, compat gives this error, not libyaml's. A `?` before other
/// text keeps each mode's reading, and in block context a flow indicator doesn't end one.
#[test]
fn a_flow_collection_s_question_mark_is_separated_from_a_flow_indicator() {
    for (input, column) in [
        ("[?]", 2),
        ("{?}", 2),
        ("[?,a]", 2),
        ("{?,a}", 2),
        ("[a, ?]", 5),
        ("{a, ?}", 5),
        ("[?[a]]", 2),
        ("{?[a]}", 2),
        ("[?{a: b}]", 2),
        ("{?{a: b}}", 2),
        ("[[?]]", 3),
        ("{a: [?]}", 6),
        ("- [?]", 4),
        ("[?}", 2),
        ("[?,", 2),
        ("[a, ?}", 5),
    ] {
        for (mode, read) in readings(input) {
            assert_eq!(
                read,
                Err(format!(
                    "a flow collection's `?` must be separated from what follows it at line 1 \
                     column {column}"
                )),
                "{input:?} in {mode}"
            );
        }
    }
    for (input, same) in [
        ("[? ]", "[{~: ~}]"),
        ("[?\n]", "[{~: ~}]"),
        ("[?\t]", "[{~: ~}]"),
        ("[? , a]", "[{~: ~}, a]"),
        ("{? }", "{~: ~}"),
        ("{? a: b}", "{a: b}"),
        ("?]", "'?]'"),
        ("- ?,", "['?,']"),
    ] {
        let expected = ayaml::parse_value(same).expect("the same value");
        for (mode, read) in readings(input) {
            assert_eq!(read.as_ref(), Ok(&expected), "{input:?} in {mode}");
        }
    }
    assert_eq!(parse("[?x]"), json!(["?x"]));
    #[cfg(feature = "libyaml-compat")]
    assert_eq!(
        ayaml::compat::from_str::<Json>("[?x]").expect("compat"),
        json!([{ "x": null }])
    );
}

/// A tag names an enum's variant by its whole name on every typed path. `!<tag:x:B>` and `!e!B`
/// under `%TAG !e! tag:x:` are one tag, so they select the same variant, named `<tag:x:B>`, and
/// not `B`. A local tag's name is all of it after its first `!`, as libyaml-based readers name it,
/// whichever handle wrote it, so `!<!!bang>` selects `!bang`. A single-entry mapping still names a
/// variant, and a variant written under the local tag of its name reads back, whatever the name.
#[test]
fn a_tag_names_an_enum_s_variant_by_its_whole_name() {
    #[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
    enum Pick {
        B(u32),
        C {
            x: u32,
        },
        #[serde(rename = "<tag:x:B>")]
        Global(u32),
        #[serde(rename = "pre-B")]
        Prefixed(u32),
        #[serde(rename = "<tag:x:T>")]
        Pair(u32, u32),
        #[serde(rename = "<tag:x:S>")]
        Shape {
            y: u32,
        },
        #[serde(rename = "!bang")]
        Bang(u32),
        #[serde(rename = "<!x>")]
        Bracketed(u32),
        #[serde(rename = "<>")]
        Empty(u32),
        #[serde(rename = "<tag:yaml.org,2002:D>")]
        Core(u32),
    }
    for (input, expected) in [
        ("!B 5\n", Pick::B(5)),
        ("!<!B> 5\n", Pick::B(5)),
        ("%TAG !e! !\n--- !e!B 5\n", Pick::B(5)),
        ("%TAG !e! tag:x:\n--- !e!B 5\n", Pick::Global(5)),
        ("%TAG ! tag:x:\n--- !B 5\n", Pick::Global(5)),
        ("!<tag:x:B> 5\n", Pick::Global(5)),
        ("%TAG !e! !pre-\n--- !e!B 5\n", Pick::Prefixed(5)),
        ("!<!!bang> 5\n", Pick::Bang(5)),
        ("%TAG !e! !!\n--- !e!bang 5\n", Pick::Bang(5)),
        ("!%3C%21x%3E 5\n", Pick::Bracketed(5)),
        // A local name in angle brackets is the name of the tag it spells.
        ("!%3Ctag:x:B%3E 5\n", Pick::Global(5)),
        ("!C {x: 1}\n", Pick::C { x: 1 }),
        ("B: 5\n", Pick::B(5)),
    ] {
        for result in every_path::<Pick>(input) {
            assert_eq!(result.as_ref(), Ok(&expected), "{input:?}");
        }
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<Pick>(input)
                .map_err(|error| error.to_string())
                .as_ref(),
            Ok(&expected),
            "{input:?} in compat"
        );
    }
    // A variant is written under the local tag of its name, which selects it. A scalar under
    // `!!D` would be a string.
    for (variant, text) in [
        (Pick::B(5), "!B 5\n"),
        (Pick::C { x: 1 }, "!C {x: 1}\n"),
        (Pick::Global(5), "!%3Ctag:x:B%3E 5\n"),
        (Pick::Prefixed(5), "!pre-B 5\n"),
        (Pick::Pair(1, 2), "!%3Ctag:x:T%3E [1, 2]\n"),
        (Pick::Shape { y: 1 }, "!%3Ctag:x:S%3E {y: 1}\n"),
        (Pick::Bang(5), "!%21bang 5\n"),
        (Pick::Bracketed(5), "!%3C%21x%3E 5\n"),
        (Pick::Empty(5), "!%3C%3E 5\n"),
        (Pick::Core(5), "!%3Ctag:yaml.org%2C2002:D%3E 5\n"),
    ] {
        let written = ayaml::to_string(&variant).expect("write");
        assert_eq!(written, text, "{variant:?}");
        for result in every_path::<Pick>(&written) {
            assert_eq!(result.as_ref(), Ok(&variant), "{written:?}");
        }
    }
    // A `Value` names that local tag as it names `!pre-B`.
    let value = ayaml::parse_value("%TAG !e! !pre-\n--- !e!B 5\n").expect("parse");
    assert_eq!(
        serde_json::to_value(&value).expect("json"),
        json!({ "!pre-B": 5 })
    );
    // A core tag names no variant but by its whole name.
    for input in ["!!C {x: 1}\n", "!<tag:x:ZZZ> 5\n"] {
        for result in every_path::<Pick>(input) {
            assert!(
                result
                    .as_ref()
                    .is_err_and(|error| error.contains("unknown variant `<")),
                "{input:?}: {result:?}"
            );
        }
    }
}

/// An enum reads from a mapping of one entry whose key is a string naming the variant, as in
/// `B: 5`, on every typed reader in both modes, and from no other mapping (D80): not an empty
/// one, nor one of more entries, nor one whose key is null, a boolean, a number, a collection, or
/// under a custom tag. A quoted key, an explicit one, one under `!!str` or the non-specific `!`,
/// and an alias of a string name the variant, and so does the whole mapping through an alias.
#[test]
fn an_enum_reads_one_mapping_form() {
    #[derive(Debug, PartialEq, serde::Deserialize)]
    enum Pick {
        B(u32),
        C {
            x: u32,
        },
        #[serde(rename = "true")]
        Yes(u32),
        #[serde(rename = "1")]
        One(u32),
        D,
    }
    for (input, expected) in [
        ("B: 5\n", Pick::B(5)),
        ("{B: 5}\n", Pick::B(5)),
        ("'B': 5\n", Pick::B(5)),
        ("\"B\": 5\n", Pick::B(5)),
        ("!!str B: 5\n", Pick::B(5)),
        ("!!map {B: 5}\n", Pick::B(5)),
        ("! B: 5\n", Pick::B(5)),
        ("{! B : 5}\n", Pick::B(5)),
        ("? B\n: 5\n", Pick::B(5)),
        ("&a B: 5\n", Pick::B(5)),
        ("{\"B\":5}\n", Pick::B(5)),
        ("C: {x: 1}\n", Pick::C { x: 1 }),
        ("C:\n  x: 1\n", Pick::C { x: 1 }),
        ("'true': 5\n", Pick::Yes(5)),
        ("'1': 5\n", Pick::One(5)),
        ("D: ~\n", Pick::D),
    ] {
        for (reader, result) in every_typed_reader::<Pick>(input) {
            assert_eq!(result.as_ref(), Ok(&expected), "{input:?} on {reader}");
        }
    }
    for (input, problem) in [
        ("{}\n", "enum mapping must contain one variant"),
        ("{B: 5, D: ~}\n", "enum mapping must contain one variant"),
        ("B: 5\nC: {x: 1}\n", "enum mapping must contain one variant"),
        ("~: 5\n", "enum variant key must be a string"),
        (": 5\n", "enum variant key must be a string"),
        ("true: 5\n", "enum variant key must be a string"),
        ("1: 5\n", "enum variant key must be a string"),
        ("[B]: 5\n", "enum variant key must be a string"),
        ("? {B: 1}\n: 5\n", "enum variant key must be a string"),
        ("!x B: 5\n", "enum variant key must be a string"),
        ("! [B]: 5\n", "enum variant key must be a string"),
        ("!!int 1: 5\n", "enum variant key must be a string"),
        // A merge key is a key like any other until `apply_merge_keys` merges it.
        ("<<: {B: 5}\n", "unknown variant `<<`"),
    ] {
        for (reader, result) in every_typed_reader::<Pick>(input) {
            assert!(
                result.as_ref().is_err_and(|error| error.contains(problem)),
                "{input:?} on {reader}: {result:?}"
            );
        }
    }
    // A duplicate key is an error before the enum sees it, except in compat, which keeps both.
    for (reader, result) in every_typed_reader::<Pick>("{B: 5, B: 6}\n") {
        let problem = if reader == "compat" {
            "enum mapping must contain one variant"
        } else {
            "duplicate"
        };
        assert!(
            result.as_ref().is_err_and(|error| error.contains(problem)),
            "{reader}: {result:?}"
        );
    }
    // An alias names the variant as the string or the mapping it stands for does, and the forms
    // read the same inside a struct, a sequence, or an `Option`.
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Holder {
        name: String,
        pick: Pick,
    }
    for (input, pick) in [
        ("name: &k B\npick: {*k : 5}\n", Ok(Pick::B(5))),
        ("name: B\npick: &m {B: 5}\n", Ok(Pick::B(5))),
        (
            "name: B\npick: {B: 5, D: ~}\n",
            Err("enum mapping must contain one variant"),
        ),
        (
            "name: B\npick: {true: 5}\n",
            Err("enum variant key must be a string"),
        ),
    ] {
        for (reader, result) in every_typed_reader::<Holder>(input) {
            match pick {
                Ok(ref pick) => assert_eq!(
                    result.as_ref().map(|holder| &holder.pick),
                    Ok(pick),
                    "{input:?} on {reader}"
                ),
                Err(problem) => assert!(
                    result.as_ref().is_err_and(|error| error.contains(problem)),
                    "{input:?} on {reader}: {result:?}"
                ),
            }
        }
    }
    #[derive(Debug, PartialEq, serde::Deserialize)]
    struct Shared {
        first: Pick,
        second: Pick,
    }
    for (reader, result) in every_typed_reader::<Shared>("first: &m {C: {x: 1}}\nsecond: *m\n") {
        assert_eq!(
            result,
            Ok(Shared {
                first: Pick::C { x: 1 },
                second: Pick::C { x: 1 }
            }),
            "{reader}"
        );
    }
    for (reader, result) in every_typed_reader::<Vec<Option<Pick>>>("- B: 5\n- ~\n- {D: ~}\n") {
        assert_eq!(
            result,
            Ok(vec![Some(Pick::B(5)), None, Some(Pick::D)]),
            "{reader}"
        );
    }
    for (reader, result) in every_typed_reader::<Vec<Pick>>("- {B: 5, D: ~}\n") {
        assert!(
            result
                .as_ref()
                .is_err_and(|error| error.contains("enum mapping must contain one variant")),
            "{reader}: {result:?}"
        );
    }
}

/// An empty line inside a multi-line scalar under a block collection can't hold a tab before the
/// scalar's indentation (`l-empty`, YAML 1.2.2 production 70). Compat refuses it in a plain
/// scalar, as libyaml does, and reads it in a quoted one, as libyaml does. An empty line after
/// the scalar, or with spaces enough before the tab, or at the root, reads.
#[test]
fn an_empty_line_inside_a_scalar_holds_no_tab_before_its_indentation() {
    let refused = |at: &str| {
        Err(format!(
            "a tab cannot indent an empty line inside a scalar under a block collection at line {at}"
        ))
    };
    for (input, at) in [
        ("k:\n  a\n\t\n  b", "3 column 1"),
        ("k: a\n\t\n b", "2 column 1"),
        ("k: a\n\t\n\t\n b", "2 column 1"),
        ("k: a\r\n\t\r\n b", "2 column 1"),
        ("- a\n\t\n  b", "2 column 1"),
        ("- k:\n    a\n  \t\n    b", "3 column 3"),
        ("a:\n  k: x\n  \t\n   y", "3 column 3"),
        ("k: [a\n\t\n b]", "2 column 1"),
        ("- k: [a\n  \t\n   b]", "2 column 3"),
    ] {
        for (mode, read) in readings(input) {
            assert_eq!(read, refused(at), "{input:?} in {mode}");
        }
    }
    for (input, at, value) in [
        ("k: \"a\n\t\n b\"", "2 column 1", json!({ "k": "a\nb" })),
        ("k: 'a\n\t\n b'", "2 column 1", json!({ "k": "a\nb" })),
        ("k:\n  \"a\n\t\n  b\"", "3 column 1", json!({ "k": "a\nb" })),
        ("k: \"a\\\n\t\n b\"", "2 column 1", json!({ "k": "a\nb" })),
        ("k: [\"a\n\t\n b\"]", "2 column 1", json!({ "k": ["a\nb"] })),
        ("k: \"a\n\t\n\t\"", "2 column 1", json!({ "k": "a\n" })),
        ("- \"a\n\t\n  b\"", "2 column 1", json!(["a\nb"])),
    ] {
        assert_eq!(
            ayaml::parse_value(input).map_err(|error| error.to_string()),
            refused(at),
            "{input:?}"
        );
        #[cfg(feature = "libyaml-compat")]
        assert_eq!(
            ayaml::compat::from_str::<Json>(input).expect("compat"),
            value,
            "{input:?} in compat"
        );
        #[cfg(not(feature = "libyaml-compat"))]
        drop(value);
    }
    for (input, value) in [
        ("k:\n  a\n \t\n  b", json!({ "k": "a\nb" })),
        ("k: a\n \t\n b", json!({ "k": "a\nb" })),
        ("- k:\n    a\n   \t\n    b", json!([{ "k": "a\nb" }])),
        ("k: [a\n \t\n b]", json!({ "k": ["a\nb"] })),
        ("k: \"a\n \t\n b\"", json!({ "k": "a\nb" })),
        ("- \"a\n \t\n  b\"", json!(["a\nb"])),
        ("a\n\t\nb", json!("a\nb")),
        ("[a\n\t\nb]", json!(["a\nb"])),
        ("\"a\n\t\nb\"", json!("a\nb")),
        ("k: a\n\t\nj: b", json!({ "k": "a", "j": "b" })),
        ("k: [a\n\t\n ]", json!({ "k": ["a"] })),
        // A tab at the end of the scalar's own line is no empty line.
        ("k: a\t\n b", json!({ "k": "a b" })),
        ("k: \"a\t\n b\"", json!({ "k": "a b" })),
    ] {
        for (mode, read) in readings(input) {
            let read = read.map(|value| serde_json::to_value(value).expect("json"));
            assert_eq!(read.as_ref(), Ok(&value), "{input:?} in {mode}");
        }
    }
}

/// An empty line inside a plain scalar doesn't end it, whatever its spaces, so a line of fewer
/// spaces than the key's line, as in `a:\n  k: x\n \n   y`, comes before more of the value.
#[test]
fn an_empty_line_of_few_spaces_does_not_end_a_key_s_value() {
    for (input, value) in [
        ("a:\n  k: x\n \n   y", json!({ "a": { "k": "x\ny" } })),
        ("a:\n  k: x\n  \n   y", json!({ "a": { "k": "x\ny" } })),
        ("  k: a\n \n   b", json!({ "k": "a\nb" })),
        (
            "a:\n  k: x\n  \nj: y",
            json!({ "a": { "k": "x" }, "j": "y" }),
        ),
        (
            "a:\n  k: x\n  \n  j: y",
            json!({ "a": { "k": "x", "j": "y" } }),
        ),
    ] {
        for (mode, read) in readings(input) {
            let read = read.map(|value| serde_json::to_value(value).expect("json"));
            assert_eq!(read.as_ref(), Ok(&value), "{input:?} in {mode}");
        }
    }
}
