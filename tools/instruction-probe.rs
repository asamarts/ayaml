//! Steady-state Callgrind probe for `tools/check_instructions.py` (AY-139).
//!
//! Usage:
//!   probe --dump-fixtures DIR
//!   probe OPERATION FILE ITERATIONS

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::hint::black_box;
use std::path::Path;

use ayaml::{ColumnEncoding, LineIndex, SpannedTree, Value};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
struct FrontmatterOwned {
    id: String,
    title: String,
    phase: u32,
    status: String,
    effort_days: f64,
    blocked_by: Vec<String>,
    paths: Vec<String>,
    tags: Vec<String>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during borrowed Callgrind measurement"
)]
#[derive(Clone, Debug, Deserialize)]
struct FrontmatterBorrowed<'a> {
    id: &'a str,
    title: &'a str,
    phase: u32,
    status: &'a str,
    effort_days: f64,
    #[serde(borrow)]
    blocked_by: Vec<&'a str>,
    #[serde(borrow)]
    paths: Vec<&'a str>,
    #[serde(borrow)]
    tags: Vec<&'a str>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BoundaryOwned {
    service: String,
    entries: BTreeMap<String, String>,
}

#[allow(
    dead_code,
    reason = "fields are populated by Serde during borrowed Callgrind measurement"
)]
#[derive(Clone, Debug, Deserialize)]
struct BoundaryBorrowed<'a> {
    service: &'a str,
    #[serde(borrow)]
    entries: BTreeMap<&'a str, Cow<'a, str>>,
}

const PROBE_FIXTURES: &[&str] = &[
    "frontmatter-tiny",
    "boundary-4kb-below",
    "boundary-4kb-above",
    "ci-workflow",
    "escape-routes",
    "unicode-multibyte",
    "aliases-1000",
    "nested-keys",
    "typed-mismatch-late",
];

fn walk(value: &Value) -> usize {
    match value {
        Value::Sequence(items) => 1 + items.iter().map(walk).sum::<usize>(),
        Value::Mapping(mapping) => {
            let mut nodes = 1;
            for (key, value) in mapping {
                nodes += walk(key) + walk(value);
            }
            let entries: Vec<_> = mapping.into_iter().collect();
            nodes + black_box(entries).len()
        }
        Value::Tagged(tagged) => 1 + walk(&tagged.value),
        _ => 1,
    }
}

fn dump_fixtures(dir: &Path) {
    std::fs::create_dir_all(dir).expect("create fixture dump directory");
    let mut manifest = String::from("{\n");
    for (idx, &id) in PROBE_FIXTURES.iter().enumerate() {
        let fixture = ayaml_fixtures::generate(id)
            .unwrap_or_else(|| panic!("missing catalog fixture `{id}`"));
        std::fs::write(dir.join(format!("{id}.yml")), &fixture.content)
            .expect("write fixture file");
        let comma = if idx + 1 == PROBE_FIXTURES.len() {
            ""
        } else {
            ","
        };
        manifest.push_str(&format!(
            "  \"{id}\": {{\"bytes\": {}, \"sha256\": \"{}\"}}{comma}\n",
            fixture.bytes, fixture.sha256
        ));
    }
    manifest.push_str("}\n");
    std::fs::write(dir.join("manifest.json"), manifest).expect("write manifest.json");
}

fn stem(path: &str) -> &str {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 3 && args[1] == "--dump-fixtures" {
        dump_fixtures(Path::new(&args[2]));
        return;
    }
    let [_, operation, path, iterations] = args.as_slice() else {
        panic!("usage: probe OPERATION FILE ITERATIONS | probe --dump-fixtures DIR");
    };
    let input = std::fs::read_to_string(path).expect("read the input");
    let iterations: usize = iterations.parse().expect("parse the iteration count");
    let name = stem(path);

    match operation.as_str() {
        "events" => {
            for _ in 0..iterations {
                let events = ayaml::parse_events(&input).expect("start parsing");
                black_box(events.count());
            }
        }
        "value" => {
            for _ in 0..iterations {
                black_box(ayaml::parse_value(&input).expect("parse a value"));
            }
        }
        "typed" => {
            for _ in 0..iterations {
                black_box(ayaml::from_str::<Value>(&input).expect("decode a value"));
            }
        }
        "typed_struct" => match name {
            "frontmatter-tiny" => {
                for _ in 0..iterations {
                    black_box(ayaml::from_str::<FrontmatterOwned>(&input).expect("frontmatter"));
                }
            }
            s if s.starts_with("boundary-") => {
                for _ in 0..iterations {
                    black_box(ayaml::from_str::<BoundaryOwned>(&input).expect("boundary"));
                }
            }
            _ => {
                for _ in 0..iterations {
                    black_box(
                        ayaml::from_str::<BTreeMap<String, Value>>(&input).expect("typed map"),
                    );
                }
            }
        },
        "borrowed" => match name {
            "frontmatter-tiny" => {
                for _ in 0..iterations {
                    black_box(
                        ayaml::from_str_borrowed::<FrontmatterBorrowed<'_>>(&input)
                            .expect("frontmatter borrowed"),
                    );
                }
            }
            s if s.starts_with("boundary-") => {
                for _ in 0..iterations {
                    black_box(
                        ayaml::from_str_borrowed::<BoundaryBorrowed<'_>>(&input)
                            .expect("boundary borrowed"),
                    );
                }
            }
            _ => {
                for _ in 0..iterations {
                    black_box(
                        ayaml::from_str_borrowed::<BTreeMap<&str, Value>>(&input)
                            .expect("borrowed map"),
                    );
                }
            }
        },
        "spanned" => {
            for _ in 0..iterations {
                let tree = SpannedTree::parse(&input).expect("SpannedTree::parse");
                let idx = LineIndex::new(&input);
                let pos = idx.to_position(input.len() / 2, ColumnEncoding::Utf16);
                black_box((tree, pos));
            }
        }
        "emit" => {
            let value = ayaml::parse_value(&input).expect("parse a value");
            for _ in 0..iterations {
                black_box(ayaml::to_string_value(&value).expect("emit a value"));
            }
        }
        "serialize" => match name {
            "frontmatter-tiny" => {
                let typed: FrontmatterOwned = ayaml::from_str(&input).expect("frontmatter");
                for _ in 0..iterations {
                    black_box(ayaml::to_string(&typed).expect("serialize frontmatter"));
                }
            }
            s if s.starts_with("boundary-") => {
                let typed: BoundaryOwned = ayaml::from_str(&input).expect("boundary");
                for _ in 0..iterations {
                    black_box(ayaml::to_string(&typed).expect("serialize boundary"));
                }
            }
            _ => {
                let value = ayaml::parse_value(&input).expect("parse a value");
                for _ in 0..iterations {
                    black_box(ayaml::to_string(&value).expect("serialize value"));
                }
            }
        },
        "walk" => {
            let value = ayaml::parse_value(&input).expect("parse a value");
            for _ in 0..iterations {
                black_box(walk(black_box(&value)));
            }
        }
        "late_typed_err" => {
            for _ in 0..iterations {
                let err = ayaml::from_str::<BTreeMap<String, u64>>(&input)
                    .expect_err("expected late typed decode error");
                black_box(err);
            }
        }
        other => panic!("unknown operation {other}"),
    }
}
