//! The memory-bounded default count limits and the profile that reads any valid input within
//! `MAX_INPUT_BYTES`.

use std::fmt::Write as _;

#[cfg(feature = "libyaml-compat")]
use ayaml::Value;
use ayaml::{ErrorKind, ParseOptions};

fn dense_inputs() -> Vec<(&'static str, String)> {
    let mut mapping = String::with_capacity(7 * 1024 * 1024);
    for index in 0..600_000 {
        writeln!(mapping, "k{index}: v").expect("write to a string");
    }

    let mut scalars = String::with_capacity(2 * 1024 * 1024);
    scalars.push('[');
    for index in 0..1_000_001 {
        if index > 0 {
            scalars.push(',');
        }
        scalars.push('0');
    }
    scalars.push_str("]\n");

    let mut anchors = String::with_capacity(2 * 1024 * 1024);
    anchors.push('[');
    for index in 0..100_001 {
        if index > 0 {
            anchors.push(',');
        }
        write!(anchors, "&a{index} 0").expect("write to a string");
    }
    anchors.push_str("]\n");

    let mut aliases = String::with_capacity(512 * 1024);
    aliases.push_str("a: &x 1\nb: [");
    for index in 0..100_001 {
        if index > 0 {
            aliases.push(',');
        }
        aliases.push_str("*x");
    }
    aliases.push_str("]\n");

    vec![
        ("block mapping", mapping),
        ("flow scalars", scalars),
        ("anchors", anchors),
        ("aliases", aliases),
    ]
}

/// The default count profile is deliberately lower than the input limit. Each reported dense
/// document is refused by a count limit, not by a syntax error or the input limit.
#[test]
fn default_count_limits_refuse_dense_documents() {
    for (name, input) in dense_inputs() {
        let error = ayaml::parse_value_with(&input, &ParseOptions::new()).expect_err(name);
        assert_eq!(error.kind(), ErrorKind::Limit, "{name}: {error}");
        assert!(error.limit().is_some(), "{name}: {error}");
    }
}

/// The max-input count profile reads the same four documents. Every reader that takes options can
/// use the profile; the alias shape is also checked through the compatibility reader.
#[test]
fn max_input_count_profile_reads_dense_documents() {
    let options = ParseOptions::for_max_input();
    #[cfg(feature = "libyaml-compat")]
    let compat_options = ayaml::compat::ParseOptions::for_max_input();
    for (name, input) in dense_inputs() {
        ayaml::parse_value_with(&input, &options).unwrap_or_else(|error| panic!("{name}: {error}"));
        #[cfg(feature = "libyaml-compat")]
        if name == "aliases" {
            ayaml::compat::from_str_with::<Value>(&input, &compat_options)
                .unwrap_or_else(|error| panic!("{name} in compat: {error}"));
        }
    }
}
