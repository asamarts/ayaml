# Cookbook

Self-contained recipes for common production patterns in `ayaml`. Every Rust
example on this page is compiled and executed as part of the workspace test
suite.

## Multi-document streaming

Use `Deserializer::from_str_with` to decode a multi-document stream lazily one
document at a time without materializing every document into memory at once:

```rust
use ayaml::{Deserializer, Limits, ParseOptions};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Manifest {
    kind: String,
    name: String,
}

# fn main() -> Result<(), ayaml::Error> {
let stream = "\
kind: Service
name: api
---
kind: Deployment
name: worker
";

let opts = ParseOptions::new().with_limits(Limits::new().with_max_documents(100));
let mut de = Deserializer::from_str_with(stream, &opts)?;
let mut manifests = Vec::new();
while let Some(doc) = de.next::<Manifest>()? {
    manifests.push(doc);
}

assert_eq!(manifests.len(), 2);
assert_eq!(manifests[0].name, "api");
assert_eq!(manifests[1].kind, "Deployment");
# Ok(())
# }
```

## Custom local tags with Rust enums

Local tags such as `!Env` or `!Secret` deserialize directly into Rust enum
variants and serialize back with matching tags:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, PartialEq, Deserialize, Serialize)]
enum ValueSource {
    Literal(String),
    Env(String),
    Secret { vault: String, key: String },
}

#[derive(Debug, PartialEq, Deserialize, Serialize)]
struct Config {
    db_url: ValueSource,
    token: ValueSource,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "\
db_url: !Env DATABASE_URL
token: !Secret
  vault: prod
  key: api_token
";

let cfg: Config = ayaml::from_str(input)?;
assert_eq!(cfg.db_url, ValueSource::Env("DATABASE_URL".into()));
assert_eq!(
    cfg.token,
    ValueSource::Secret {
        vault: "prod".into(),
        key: "api_token".into(),
    }
);

let yaml = ayaml::to_string(&cfg)?;
assert_eq!(ayaml::from_str::<Config>(&yaml)?, cfg);
# Ok(())
# }
```

## Opt-in YAML 1.1 merge keys

Enable `ParseOptions::new().with_merge_keys(true)` to expand plain `<<:`
merges (including chained merges and sequence operands with first-wins
precedence) while preserving quoted `"<<"` keys, borrowed strings, and source
spans:

```rust
use ayaml::{ParseOptions, from_str_with};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Job<'a> {
    image: &'a str,
    retries: u8,
    command: &'a str,
}

#[derive(Debug, PartialEq, Deserialize)]
struct Pipeline<'a> {
    #[serde(borrow)]
    build: Job<'a>,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "\
base: &base
  image: rust:1.88
  retries: 2
fast: &fast
  retries: 0
build:
  <<: [*fast, *base]
  command: cargo test
";

let opts = ParseOptions::new().with_merge_keys(true);
let pipeline: Pipeline<'_> = from_str_with(input, &opts)?;
assert_eq!(
    pipeline.build,
    Job {
        image: "rust:1.88",
        retries: 0,
        command: "cargo test",
    }
);
# Ok(())
# }
```

## Span-preserving typed decoding with `Spanned<T>`

Wrap fields in `Spanned<T>` when you want typed Serde decoding plus exact byte
spans for semantic validation and diagnostics:

```rust
use ayaml::{ColumnEncoding, LineIndex, Location, Spanned};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct ServerConfig {
    host: Spanned<String>,
    port: Spanned<u16>,
}

# fn main() -> Result<(), ayaml::Error> {
let input = "host: 0.0.0.0\nport: 80\n";
let cfg: ServerConfig = ayaml::from_str(input)?;

assert_eq!(*cfg.host, "0.0.0.0");
assert_eq!(cfg.host.span().slice(input), Some("0.0.0.0"));

if *cfg.port < 1024 {
    let index = LineIndex::new(input);
    let pos = index
        .to_position(cfg.port.span().start(), ColumnEncoding::Scalar)
        .expect("port position");
    let loc = Location::from(pos);
    assert_eq!((loc.line(), loc.column()), (2, 7));
}
# Ok(())
# }
```

## Bounded I/O reading with `from_reader_with`

When reading from a file, stdin, or an HTTP body, use `from_reader_with` so
`ayaml` reads at most `max_input_bytes + 1` bytes before returning
`ErrorKind::Limit`:

```rust
use std::io::Cursor;
use ayaml::{ErrorKind, LimitKind, Limits, ParseOptions, from_reader_with};
use serde::Deserialize;

#[derive(Debug, PartialEq, Deserialize)]
struct Payload {
    ok: bool,
}

# fn main() -> Result<(), ayaml::Error> {
let opts = ParseOptions::new().with_limits(Limits::new().with_max_input_bytes(64));
let ok: Payload = from_reader_with(Cursor::new(b"ok: true\n"), &opts)?;
assert!(ok.ok);

let oversized = vec![b'a'; 128];
let err = from_reader_with::<_, Payload>(Cursor::new(oversized), &opts).unwrap_err();
assert_eq!(err.kind(), ErrorKind::Limit);
assert_eq!(err.limit().map(|l| l.kind()), Some(LimitKind::InputBytes));
# Ok(())
# }
```

## Frontmatter splitting and parsing

Parse YAML frontmatter from Markdown or static-site documents while keeping
exact byte offsets into the original file for both frontmatter errors and the
remaining body:

```rust
use ayaml::{ParseOptions, Spanned, parse_frontmatter_with};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PostMeta<'a> {
    #[serde(borrow)]
    title: Spanned<&'a str>,
    draft: bool,
}

# fn main() -> Result<(), ayaml::Error> {
let doc = "---\ntitle: Release Notes\ndraft: false\n---\n# Heading\nBody text.\n";
let parsed = parse_frontmatter_with::<PostMeta<'_>>(doc, &ParseOptions::new())?
    .expect("frontmatter present");

assert_eq!(*parsed.frontmatter.title, "Release Notes");
assert_eq!(
    parsed.frontmatter.title.span().slice(doc),
    Some("Release Notes")
);
assert!(!parsed.frontmatter.draft);
assert_eq!(parsed.body, "# Heading\nBody text.\n");
assert_eq!(&doc[parsed.body_offset..], parsed.body);
# Ok(())
# }
```

## Serde middleware wrappers with `DocumentDeserializer`

`DocumentDeserializer` implements `serde::Deserializer<'de>` directly, so you
can pass it to Serde middleware crates (such as path-tracking or unknown-field
reporting wrappers) without losing `ayaml`'s spans or resource limits. The
example below shows a zero-dependency unknown-field collector and a
path-tracking `DeserializeSeed` wrapper driving `DocumentDeserializer`:

```rust
use std::collections::BTreeMap;
use ayaml::{DocumentDeserializer, ParseOptions};
use serde::de::{self, Deserialize, DeserializeSeed, MapAccess, Visitor};

struct UnknownFieldCollector<T> {
    value: T,
    unknown: Vec<String>,
}

#[derive(Debug, Default, PartialEq)]
struct StrictApp {
    name: String,
    port: u16,
}

impl<'de> Deserialize<'de> for UnknownFieldCollector<StrictApp> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AppVisitor;

        impl<'de> Visitor<'de> for AppVisitor {
            type Value = UnknownFieldCollector<StrictApp>;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an app configuration mapping")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut app = StrictApp::default();
                let mut unknown = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "name" => app.name = map.next_value()?,
                        "port" => app.port = map.next_value()?,
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                            unknown.push(key);
                        }
                    }
                }
                Ok(UnknownFieldCollector { value: app, unknown })
            }
        }

        deserializer.deserialize_map(AppVisitor)
    }
}

struct TrackKey<'a>(&'a mut Option<String>);

impl<'de> DeserializeSeed<'de> for TrackKey<'_> {
    type Value = BTreeMap<String, u16>;

    fn deserialize<D: serde::Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct MapVisitor<'a>(&'a mut Option<String>);
        impl<'de> Visitor<'de> for MapVisitor<'_> {
            type Value = BTreeMap<String, u16>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a map of port names to u16 values")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = BTreeMap::new();
                while let Some(k) = map.next_key::<String>()? {
                    *self.0 = Some(k.clone());
                    let v = map.next_value::<u16>()?;
                    out.insert(k, v);
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(MapVisitor(self.0))
    }
}

# fn main() -> Result<(), ayaml::Error> {
let input = "name: billing\nport: 8080\nlegacy_flag: true\n";
let de = DocumentDeserializer::from_str_with(input, &ParseOptions::new())?;
let collected = UnknownFieldCollector::<StrictApp>::deserialize(de)?;

assert_eq!(
    collected.value,
    StrictApp {
        name: "billing".into(),
        port: 8080,
    }
);
assert_eq!(collected.unknown, vec!["legacy_flag"]);

let mut failed_key = None;
let err = TrackKey(&mut failed_key)
    .deserialize(DocumentDeserializer::from_str("http: 80\nhttps: bad\n")?)
    .unwrap_err();
assert_eq!(failed_key.as_deref(), Some("https"));
assert!(err.span().is_some());
# Ok(())
# }
```

## Rendering source snippets with `LineIndex`

Combine `Error::span()` (or `Spanned::span()`) with `LineIndex` to render
editor-style diagnostics with line numbers and carets:

```rust
use ayaml::{ColumnEncoding, LineIndex, Location, Span};

fn render_snippet(source: &str, span: Span, message: &str) -> String {
    let index = LineIndex::new(source);
    let start = index
        .to_position(span.start(), ColumnEncoding::Scalar)
        .unwrap_or_default();
    let loc = Location::from(start);
    let line_text = source.lines().nth(start.line).unwrap_or("");
    let width = span
        .slice(source)
        .and_then(|s| s.lines().next())
        .map(|s| s.chars().count())
        .unwrap_or(1)
        .max(1);
    let pad = " ".repeat(start.column);
    let carets = "^".repeat(width);
    format!(
        "error: {message}\n --> line {}, column {}\n  |\n{} | {line_text}\n  | {pad}{carets}",
        loc.line(),
        loc.column(),
        loc.line(),
    )
}

# fn main() {
let input = "service:\n  port: nope\n";
let err = ayaml::from_str::<std::collections::HashMap<String, std::collections::HashMap<String, u16>>>(input)
    .unwrap_err();
let span = err.span().expect("error span");
let snippet = render_snippet(input, span, err.message());

assert!(snippet.contains("line 2, column 9"));
assert!(snippet.contains("2 |   port: nope"));
assert!(snippet.contains("        ^^^^"));
# }
```
