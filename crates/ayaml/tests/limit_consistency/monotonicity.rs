use ayaml::{Deserializer, Error, ErrorKind, Limits, ParseOptions, SpannedTree, Value};

fn is_limit_err(err: &Error) -> bool {
    err.kind() == ErrorKind::Limit || err.limit().is_some()
}

fn assert_monotonic_eq<T: PartialEq + std::fmt::Debug>(
    name: &str,
    reader: &str,
    lo: Result<T, Error>,
    hi: Result<T, Error>,
) {
    match (lo, hi) {
        (Ok(left), Ok(right)) => {
            assert_eq!(
                left, right,
                "{name} ({reader}): raising limit changed parsed value"
            );
        }
        (Ok(val), Err(err)) => {
            panic!(
                "{name} ({reader}): tighter limit succeeded ({val:?}) while looser failed ({err:?})"
            );
        }
        (Err(lo_err), Ok(_)) => {
            assert!(
                is_limit_err(&lo_err),
                "{name} ({reader}): raising limit turned non-limit error into Ok: {lo_err:?}"
            );
        }
        (Err(lo_err), Err(hi_err)) => {
            if !is_limit_err(&lo_err) {
                assert_eq!(
                    (lo_err.kind(), lo_err.code(), lo_err.span()),
                    (hi_err.kind(), hi_err.code(), hi_err.span()),
                    "{name} ({reader}): tightening limit swapped non-limit error: lo={lo_err:?} hi={hi_err:?}"
                );
            }
        }
    }
}

fn assert_monotonic_result<T: std::fmt::Debug>(
    name: &str,
    reader: &str,
    lo: Result<T, Error>,
    hi: Result<T, Error>,
) {
    match (lo, hi) {
        (Ok(_), Ok(_)) => {}
        (Ok(val), Err(err)) => {
            panic!(
                "{name} ({reader}): tighter limit succeeded ({val:?}) while looser failed ({err:?})"
            );
        }
        (Err(lo_err), Ok(_)) => {
            assert!(
                is_limit_err(&lo_err),
                "{name} ({reader}): raising limit turned non-limit error into Ok: {lo_err:?}"
            );
        }
        (Err(lo_err), Err(hi_err)) => {
            if !is_limit_err(&lo_err) {
                assert_eq!(
                    (lo_err.kind(), lo_err.code(), lo_err.span()),
                    (hi_err.kind(), hi_err.code(), hi_err.span()),
                    "{name} ({reader}): tightening limit swapped non-limit error: lo={lo_err:?} hi={hi_err:?}"
                );
            }
        }
    }
}

/// Tightening any scanner limit (`max_events`, `max_depth`, `max_scalar_bytes`,
/// `max_tag_expansion_bytes`, `max_anchors`) on a demanded line must never turn `Ok` into a
/// non-limit error or swap one non-limit error for another (D110).
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "table-driven regression suite covering all six D110 limit-truncation monotonicity classes across every reader"
)]
fn limit_truncated_lines_preserve_monotonicity_across_all_readers() {
    let long_key_scalar = format!("[a: b: c, {}, {}]: 1\n", "y".repeat(500), "z".repeat(520));
    let long_key_events = format!("[a: b: c, {}]: 1\n", "1, ".repeat(350));
    let long_key_depth = format!("}}}} [[1], {}]: 1\n", "x".repeat(1020));
    let deep_flow_colon = format!(
        "{}!a !b 1234567890 : 1{}\n",
        "[".repeat(128),
        "]".repeat(128)
    );

    let cases: &[(&str, &str, Limits, Limits)] = &[
        (
            "mapping exit truncated by EventCountLimit",
            "!Ref\n'a: b # not comment': !local [!custom http://example.com/a, !Ref 0, fallback]\n'  spaced  ': &a0 !custom |\n    plain line\n]    A,000A\n",
            Limits::default().with_max_events(19),
            Limits::default().with_max_events(20),
        ),
        (
            "mapping exit unclosed flow truncated by TagExpansionLimit",
            "%YAML 1.2\r\n---\r\n'it''s quoted': true\r[\n[0]: []\r\na: {a: !local \"- list looking\\nplain line\\nplain line\"}\r\n",
            Limits::default().with_max_tag_expansion_bytes(0),
            Limits::default().with_max_tag_expansion_bytes(1),
        ),
        (
            "mapping exit flow without colon truncated by ScalarSizeLimit",
            "a: 1\n[ !a !b 1234567890 ]\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "mapping exit flow with colon truncated by ScalarSizeLimit",
            "a: 1\n[ !a !b 1234567890 ]: 2\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "root multiline flow key truncated by ScalarSizeLimit",
            "[\n[a: b: c]\n1234567890\n]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "sequence-item multiline flow key truncated by ScalarSizeLimit",
            "- [\n  [a: b: c]\n  1234567890\n  ]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "duplicate property ambiguity on limit-truncated flow key",
            "!outer\n!inner [ 1234567890 ]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "document-start line flow truncated by ScalarSizeLimit",
            "--- !a [ !b !c 1234567890 ]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        // Inherited property followed by a limit-truncated key property on valid YAML.
        (
            "inherited property followed by limit-truncated key property (scalar limit)",
            "a: []\nb:\n  !outer\n  !inner 1234567890: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "inherited property followed by limit-truncated key property (events limit)",
            "a: []\nb:\n  !outer\n  !inner k: 1\n",
            Limits::default().with_max_events(9),
            Limits::default().with_max_events(20),
        ),
        (
            "inherited property followed by limit-truncated key property (anchors limit)",
            "a: []\nb:\n  !outer\n  !inner &x k: 1\n",
            Limits::default().with_max_anchors(0),
            Limits::default().with_max_anchors(1),
        ),
        // Inherited property must not attach to alias or scalar key when `:` is truncated.
        (
            "inherited tag before limit-truncated alias mapping key",
            "a: []\nb: &x 1\nc:\n  !outer\n  *x : 2\n",
            Limits::default().with_max_events(12),
            Limits::default().with_max_events(100),
        ),
        (
            "inherited core scalar tag before limit-truncated mapping key",
            "a: []\nb:\n  !!int\n  'bad' : 1\n",
            Limits::default().with_max_events(8),
            Limits::default().with_max_events(100),
        ),
        // Line break or >1024-char implicit key completed after scanner limit truncation.
        (
            "post-truncation newline flow key (scalar limit)",
            "[a: b: c\n1234567890\n]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "post-truncation newline flow key (depth limit)",
            "[ !a !b [1]\n]: 1\n",
            Limits::default().with_max_depth(1),
            Limits::default().with_max_depth(2),
        ),
        (
            "post-truncation newline flow key (events limit)",
            "[ !a !b c\n]: 1\n",
            Limits::default().with_max_events(3),
            Limits::default().with_max_events(20),
        ),
        (
            "post-truncation newline flow key (anchors limit)",
            "[ !a !b &a 1\n]: 1\n",
            Limits::default().with_max_anchors(0),
            Limits::default().with_max_anchors(1),
        ),
        (
            "post-truncation newline flow key (tag expansion limit)",
            "[ 'a' 'b' !t\n]: 1\n",
            Limits::default().with_max_tag_expansion_bytes(0),
            Limits::default().with_max_tag_expansion_bytes(1),
        ),
        (
            "post-truncation >1024-char flow key (scalar limit)",
            &long_key_scalar,
            Limits::default().with_max_scalar_bytes(510),
            Limits::default().with_max_scalar_bytes(1000),
        ),
        (
            "post-truncation >1024-char flow key (events limit)",
            &long_key_events,
            Limits::default().with_max_events(20),
            Limits::default().with_max_events(2000),
        ),
        (
            "post-truncation >1024-char flow key (depth limit)",
            &long_key_depth,
            Limits::default().with_max_depth(1),
            Limits::default().with_max_depth(2),
        ),
        // Compact mapping inside an explicit mapping value truncated before `:`.
        (
            "compact mapping value truncated before colon (single line)",
            "? k\n: [1] 2 1234567890: 3\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "compact mapping value truncated before colon (multiline key)",
            "? k\n: [\n  [a: b: c]\n  1234567890\n  ]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        // Compat misindented trailing mapping line truncated before `:`.
        (
            "compat misindented trailing line truncated by scalar limit",
            "a: []\n [1234567890]: 1\n",
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "compat misindented trailing line truncated by events limit",
            "a: [1, 2, 3, 4, 5]\n b: 1\n",
            Limits::default().with_max_events(14),
            Limits::default().with_max_events(100),
        ),
        // +1 depth boundary when an implicit flow mapping colon is hidden by a scalar limit.
        (
            "flow sequence implicit mapping colon hidden at default max_depth boundary",
            &deep_flow_colon,
            Limits::default().with_max_scalar_bytes(5),
            Limits::default().with_max_scalar_bytes(100),
        ),
        (
            "flow sequence implicit mapping colon hidden at max_depth=1",
            "[ !a !b 1234567890 : 1 ]\n",
            Limits::default().with_max_depth(1).with_max_scalar_bytes(5),
            Limits::default()
                .with_max_depth(1)
                .with_max_scalar_bytes(100),
        ),
    ];

    for (name, input, lo_limits, hi_limits) in cases {
        let lo_opts = ParseOptions::new().with_limits(lo_limits.clone());
        let hi_opts = ParseOptions::new().with_limits(hi_limits.clone());

        assert_monotonic_eq(
            name,
            "parse_value_with",
            ayaml::parse_value_with(input, &lo_opts),
            ayaml::parse_value_with(input, &hi_opts),
        );
        assert_monotonic_eq(
            name,
            "parse_documents_with",
            ayaml::parse_documents_with(input, &lo_opts),
            ayaml::parse_documents_with(input, &hi_opts),
        );
        assert_monotonic_eq(
            name,
            "from_str_with",
            ayaml::from_str_with::<Value>(input, &lo_opts),
            ayaml::from_str_with::<Value>(input, &hi_opts),
        );
        let de_res = |opts: &ParseOptions| {
            Deserializer::from_str_with(input, opts).and_then(|mut de| {
                let mut docs = Vec::new();
                while let Some(doc) = de.next::<Value>()? {
                    docs.push(doc);
                }
                Ok(docs)
            })
        };
        assert_monotonic_eq(name, "Deserializer", de_res(&lo_opts), de_res(&hi_opts));
        let ev_res = |opts: &ParseOptions| {
            ayaml::parse_events_with(input, opts).and_then(Iterator::collect::<Result<Vec<_>, _>>)
        };
        assert_monotonic_eq(
            name,
            "parse_events_with",
            ev_res(&lo_opts),
            ev_res(&hi_opts),
        );
        assert_monotonic_result(
            name,
            "SpannedTree::parse_with",
            SpannedTree::parse_with(input, &lo_opts),
            SpannedTree::parse_with(input, &hi_opts),
        );

        #[cfg(feature = "libyaml-compat")]
        {
            let lo_compat = ayaml::compat::ParseOptions::new().with_limits(lo_limits.clone());
            let hi_compat = ayaml::compat::ParseOptions::new().with_limits(hi_limits.clone());
            assert_monotonic_eq(
                name,
                "compat::from_str_with",
                ayaml::compat::from_str_with::<Value>(input, &lo_compat),
                ayaml::compat::from_str_with::<Value>(input, &hi_compat),
            );
        }
    }
}
