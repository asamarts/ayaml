//! Run one operation repeatedly, for `tools/check_instructions.py` to count under
//! callgrind.
//!
//! usage: probe events|value|typed|emit|walk FILE ITERATIONS

use std::hint::black_box;

/// Visit every node of a value, a mapping's entries through `&Mapping` and once more collected.
/// This crate is not the library's, so the count shows what a caller's loop costs: a mapping's
/// iterators hand on to the map's, and once cost twice as much from outside as from inside.
fn walk(value: &ayaml::Value) -> usize {
    match value {
        ayaml::Value::Sequence(items) => 1 + items.iter().map(walk).sum::<usize>(),
        ayaml::Value::Mapping(mapping) => {
            let mut nodes = 1;
            for (key, value) in mapping {
                nodes += walk(key) + walk(value);
            }
            let entries: Vec<_> = mapping.into_iter().collect();
            nodes + black_box(entries).len()
        }
        ayaml::Value::Tagged(tagged) => 1 + walk(&tagged.value),
        _ => 1,
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, operation, path, iterations] = args.as_slice() else {
        panic!("usage: probe events|value|typed|emit|walk FILE ITERATIONS");
    };
    let input = std::fs::read_to_string(path).expect("read the input");
    let iterations: usize = iterations.parse().expect("parse the iteration count");

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
                black_box(ayaml::from_str::<ayaml::Value>(&input).expect("decode a value"));
            }
        }
        "emit" => {
            let value = ayaml::parse_value(&input).expect("parse a value");
            for _ in 0..iterations {
                black_box(ayaml::to_string_value(&value).expect("emit a value"));
            }
        }
        "walk" => {
            let value = ayaml::parse_value(&input).expect("parse a value");
            for _ in 0..iterations {
                black_box(walk(black_box(&value)));
            }
        }
        other => panic!("unknown operation {other}"),
    }
}
