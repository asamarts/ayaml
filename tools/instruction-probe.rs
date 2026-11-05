//! Run one operation repeatedly, for `tools/check_instructions.py` to count under
//! callgrind.
//!
//! usage: probe events|value|typed|emit FILE ITERATIONS

use std::hint::black_box;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, operation, path, iterations] = args.as_slice() else {
        panic!("usage: probe events|value|typed|emit FILE ITERATIONS");
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
        other => panic!("unknown operation {other}"),
    }
}
