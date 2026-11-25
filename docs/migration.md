# Migrating from YAML 1.1

`ayaml` follows YAML 1.2.2 by default. Teams moving from YAML 1.1 stacks or
libyaml-based pipelines usually run into five differences: scalar typing,
duplicate mapping keys, `<<:` merge keys, non-core global tags, and stricter
indentation or alias syntax.

## 1. Scalar resolution (YAML 1.2.2 Core Schema)

YAML 1.1 implicitly coerced several plain strings into booleans, octal
integers, sexagesimal numbers, and timestamps. YAML 1.2.2 keeps them as
strings:

| Plain scalar | YAML 1.1 reading | `ayaml` (YAML 1.2.2) | Migration action |
| --- | --- | --- | --- |
| `yes`, `no`, `on`, `off`, `y`, `n` | boolean | string | Write `true` / `false` for booleans; leave country codes like `NO` unquoted. |
| `0123`, `0644` | octal integer | string | Use `0o123` / `0o644` for octal integers, or quote file modes (`"0644"`). |
| `1_000` | integer `1000` | string `"1_000"` | Remove underscores from numeric literals (`1000`). |
| `1:20`, `1:20:30.5` | sexagesimal number | string | Write plain integers/floats (or keep strings for ports and durations). |
| `2026-05-01` | timestamp | string | Parse dates in application types (e.g. Serde string deserializers). |
| `null`, `Null`, `NULL`, `~`, empty | null | null | Unchanged. |

```rust
use ayaml::parse_value;

# fn main() -> Result<(), ayaml::Error> {
let doc = parse_value(
    "\
country: NO
mode: 0644
octal: 0o644
flag: true
date: 2026-05-01
",
)?;

assert_eq!(doc["country"].as_str(), Some("NO"));
assert_eq!(doc["mode"].as_str(), Some("0644"));
assert_eq!(doc["octal"].as_u64(), Some(0o644));
assert_eq!(doc["flag"].as_bool(), Some(true));
assert_eq!(doc["date"].as_str(), Some("2026-05-01"));
# Ok(())
# }
```

When emitting YAML that older YAML 1.1 consumers may read, `ayaml`'s writers
automatically quote leading-zero decimal strings such as `"0644"` so they are
not misread as octal. See
[Compatibility profile: YAML 1.1 readers](compatibility.md#yaml-11-readers)
for the full list of scalars to quote when interoperating with YAML 1.1 tools.

## 2. Duplicate mapping keys

Many YAML 1.1 loaders silently keep the last value when a mapping repeats a
key. In default mode, `ayaml` rejects duplicate semantic keys across
`parse_value`, `parse_documents`, and all typed Serde readers (`ErrorKind::DuplicateKey`,
`ErrorCode::DuplicateKey`), including keys that collide after resolution such
as `1` and `"1"`:

```rust
use ayaml::{ErrorCode, ErrorKind, parse_value};

let err = parse_value("port: 80\nport: 443\n").unwrap_err();
assert_eq!(err.kind(), ErrorKind::DuplicateKey);
assert_eq!(err.code(), ErrorCode::DuplicateKey);
```

If you need to inspect duplicate pairs (for example in a linter), use
`parse_events`, which preserves every pair as written. If you must accept
existing files with last-wins map duplicates while migrating, enable the
optional `libyaml-compat` feature and use `ayaml::compat` (see
[When to use `ayaml::compat`](#5-when-to-use-ayamlcompat)).

## 3. Merge keys (`<<:`)

YAML 1.2.2 treats `<<` as an ordinary mapping key. Container specs, CI
pipelines, and legacy configs frequently combine anchors with `<<:` merges.

`ayaml` keeps `<<` literal by default and offers two explicit opt-in paths:

1. **Event and typed merge expansion (`ParseOptions::with_merge_keys(true)`):**
   recommended for new code. Expands plain `<<` and `!!merge <<` across
   `from_str_with`, `from_slice_with`, `from_reader_with`, `parse_value_with`,
   `parse_documents_with`, `parse_events_with`, `SpannedTree::parse_with`,
   `Deserializer::from_str_with`, `DocumentDeserializer::from_str_with`, and
   `parse_frontmatter_with`. Quoted `"<<"`, `'<<'`, and `!!str <<` stay
   literal, original source spans and borrowed `&str` slices are preserved, and
   all depth, entry, event, and alias budgets are enforced.
2. **Post-composition value merge (`Value::apply_merge_keys` /
   `Value::apply_merge_keys_with_limits`):** useful when you already hold a
   `Value`. Because `Value` does not retain scalar quoting, `"<<"` and `<<` are
   treated identically there.

```rust
use ayaml::{ParseOptions, Spanned, from_str_with, parse_value};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Service<'a> {
    #[serde(borrow)]
    host: Spanned<&'a str>,
    port: u16,
    #[serde(rename = "<<")]
    literal: &'a str,
}

#[derive(Debug, Deserialize)]
struct Config<'a> {
    #[serde(borrow)]
    service: Service<'a>,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "\
defaults: &def
  host: localhost
  port: 8080
service:
  <<: *def
  \"<<\": kept-literal
  port: 9090
";

// Default YAML 1.2.2 mode keeps `<<` literal (so `<<` and `"<<"` collide).
assert!(parse_value(input).is_err());

// Opt-in merge expansion expands plain `<<` while keeping `"<<"` literal.
let opts = ParseOptions::new().with_merge_keys(true);
let cfg: Config<'_> = from_str_with(input, &opts)?;
assert_eq!(*cfg.service.host, "localhost");
assert_eq!(cfg.service.host.span().slice(input), Some("localhost"));
assert_eq!(cfg.service.port, 9090);
assert_eq!(cfg.service.literal, "kept-literal");
# Ok(())
# }
```

## 4. Stricter syntax and alias separation

Two syntax habits common in YAML 1.1 files fail under YAML 1.2.2:

- **Alias directly followed by `:` as a mapping key**: `*a: 1` parses `a:` as
  the anchor name in YAML 1.2.2. Add a space before the colon (`*a : 1`) so it
  works in both YAML 1.1 and YAML 1.2.2.
- **Unindented flow continuations inside block collections**: lines inside a
  flow sequence or mapping under a block collection must be indented past the
  enclosing block collection.

## 5. When to use `ayaml::compat`

Keep the default profile whenever you control or can validate the input. Turn
on the additive `libyaml-compat` feature only for compatibility boundaries that
must accept existing libyaml-formatted documents:

```toml
[dependencies]
ayaml = { version = "0.2.3", features = ["libyaml-compat"] }
```

Enabling the feature never changes the root `ayaml::*` functions; callers opt
in explicitly through `ayaml::compat::*` and `compat::ParseOptions` (including
`compat::GlobalTags::Drop` when legacy documents carry non-core global tags
such as `!!timestamp` or `!!binary`). See
[Compatibility profile](compatibility.md#compatibility-mode) for the complete
list of compatibility behaviors.
