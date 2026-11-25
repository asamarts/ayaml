# Error codes

Every [`Error`](https://docs.rs/ayaml/latest/ayaml/struct.Error.html) returned by `ayaml` carries
an `ErrorKind` (pipeline stage) and a stable `ErrorCode` (`yaml.<kebab-name>`).

Call `Error::kind()` for the coarse category and `Error::code()` for the machine-readable variant,
whose `as_str()` method and `Display` / `AsRef<str>` implementations return the same
`yaml.<kebab-name>` string.

Match on `Error::code()` or `Error::kind()` rather than `Error::message()` text. Message wording
may be clarified across releases; `ErrorCode` variants and their `yaml.<kebab-name>` strings are
part of the crate's stability contract (see [Stability and versioning](stability.md)). For what
unmasked errors may contain and guidance on untrusted input, see
[Security and untrusted input](security.md).

## Masked error output

Normal `Display`, `Debug`, `Error::message()`, and `std::error::Error::source()` output on `Error`
may include text from the YAML document (for example, scalar values, mapping keys, typed error
paths, `%TAG` handles or prefixes, anchor and alias names, unexpected characters, or underlying
I/O error text).

When logging, recording telemetry, or surfacing errors from untrusted or sensitive YAML files
(such as configuration files containing credentials, tokens, or personal data), format
`error.masked()` (`MaskedError`) instead:

```rust
use std::collections::HashMap;

let err = ayaml::from_str::<HashMap<String, u32>>("api_key: secret-value\n").unwrap_err();
// Normal Display may include document text:
assert!(err.to_string().contains("api_key"));
assert!(err.to_string().contains("secret-value"));

// Masked Display and Debug never include document text or source error strings:
let masked = err.masked();
assert_eq!(masked.to_string(), "serde error (yaml.invalid-type) at line 1 column 10");
assert!(!format!("{masked:?}").contains("secret-value"));
```

`MaskedError` formats only the coarse `ErrorKind`, the stable `ErrorCode`, and the one-based
`Location` (when present) in `Display`, plus the byte `Span` and numeric `LimitExceeded` metadata
in `Debug`. Its `std::error::Error::source()` implementation always returns `None` so an unmasked
underlying cause cannot leak through error-chain formatters.

## `ErrorKind` and `ErrorCode`

`ErrorKind` answers which layer failed (encoding, scanner, parser, aliases, duplicate keys, tags,
resolution, resource limits, composition, Serde, emitter, or I/O). `ErrorCode` answers *why* it
failed.

- Multiple `ErrorCode` variants share one `ErrorKind`. For example, scanner syntax failures all
  report `ErrorKind::Scanner` while distinguishing `yaml.invalid-escape`,
  `yaml.unterminated-quoted-scalar`, `yaml.invalid-directive`, `yaml.undefined-tag-handle`, and so
  on.
- A few codes can appear under more than one `ErrorKind` depending on where the failure is caught:
  - `yaml.invalid-tab-indentation` and `yaml.invalid-block-scalar-indentation` normally report
    `ErrorKind::Scanner`, and report `ErrorKind::Parser` when the parser detects a misindented line
    after closing a block collection.
  - `yaml.duplicate-key` reports `ErrorKind::DuplicateKey` during YAML decoding and composition, and
    `ErrorKind::Serde` when a `Serialize` implementation emits a duplicate mapping key into
    `to_value` or `to_string`.
  - `yaml.unexpected-token` reports `ErrorKind::Parser` for grammar failures and is also the
    default code for `Error::new(ErrorKind::Scanner, ...)` and `Error::new(ErrorKind::Parser, ...)`.
- In `libyaml-compat` mode (`ayaml::compat`), a few failures remap `Error::kind()` to preserve
  libyaml-compatible behavior while keeping their specific `ErrorCode`:
  - `yaml.depth-limit` reports `ErrorKind::Serde` (instead of `ErrorKind::Limit`) when remapped by
    compatibility deserializers.
  - `yaml.document-count-limit` reports `ErrorKind::Parser` (instead of `ErrorKind::Limit`) when
    remapped by single-document compatibility deserializers.
- `Error::new(kind, message)` assigns the default `ErrorCode` for `kind`. Calling
  `Error::with_limit(limit)` refines `ErrorCode::LimitExceeded` to the specific limit code for
  `limit.kind()`, and `Error::with_code(code)` sets any `ErrorCode` explicitly.
- Both `ErrorKind` and `ErrorCode` are `#[non_exhaustive]`. New codes may be added in minor
  releases; existing codes are not renamed or removed within a major version.

## Encoding

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `InvalidUtf8` | `yaml.invalid-utf8` | `InvalidEncoding` | Input bytes passed to `from_slice*` or `from_reader*` are not valid UTF-8. |

## Scanner

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `NonPrintableChar` | `yaml.non-printable-char` | `Scanner` | Input contains a disallowed non-printable character (or a character allowed only inside quoted scalars appears outside quotes). |
| `InvalidBom` | `yaml.invalid-bom` | `Scanner` | A byte order mark (`U+FEFF`) appears where YAML forbids one (for example, inside a document, comment, scalar, directive, or anchor name). |
| `ReservedIndicator` | `yaml.reserved-indicator` | `Scanner` | A reserved indicator (`%`, `@`, or `` ` ``) starts a plain scalar. |
| `InvalidTabIndentation` | `yaml.invalid-tab-indentation` | `Scanner`, `Parser` | A tab is used where YAML forbids tabs as indentation or structural separation (such as block structure, quoted or flow indentation, empty lines inside block-owned scalars, or a comment after a block scalar). |
| `UnterminatedQuotedScalar` | `yaml.unterminated-quoted-scalar` | `Scanner` | A single- or double-quoted scalar reached end of input without a closing quote. |
| `InvalidQuotedContinuation` | `yaml.invalid-quoted-continuation` | `Scanner` | A continuation line of a quoted scalar is insufficiently indented for its parent or contains a document marker at column 0. |
| `InvalidEscape` | `yaml.invalid-escape` | `Scanner` | A double-quoted scalar contains an unterminated, unknown, malformed hexadecimal, or non-Unicode-scalar escape sequence. |
| `InvalidBlockScalarHeader` | `yaml.invalid-block-scalar-header` | `Scanner` | A block scalar header (`\|` or `>`) has unexpected characters or does not end at a line break. |
| `InvalidBlockScalarIndentation` | `yaml.invalid-block-scalar-indentation` | `Scanner`, `Parser` | A block scalar is not indented past its parent, or its leading empty lines are indented more than its first content line. |
| `InvalidFlowIndentation` | `yaml.invalid-flow-indentation` | `Scanner` | A continuation line inside a flow collection under a block collection is not indented past its enclosing block collection. |
| `DocumentMarkerInFlow` | `yaml.document-marker-in-flow` | `Scanner` | A document start (`---`) or document end (`...`) marker appears at column 0 inside a flow collection. |
| `InvalidFlowKey` | `yaml.invalid-flow-key` | `Scanner` | A `?` explicit-key indicator inside a flow collection is immediately followed by a flow indicator without separating whitespace. |
| `MissingCommentWhitespace` | `yaml.missing-comment-whitespace` | `Scanner` | A `#` comment indicator is not separated from the preceding token or block scalar header by whitespace. |
| `InvalidDirective` | `yaml.invalid-directive` | `Scanner` | A directive lacks a name or has malformed `%YAML` or `%TAG` syntax, handle, prefix, or percent-escapes. |
| `DuplicateDirective` | `yaml.duplicate-directive` | `Scanner` | A document declares more than one `%YAML` directive or redeclares the same `%TAG` handle. |
| `InvalidAnchor` | `yaml.invalid-anchor` | `Scanner` | An anchor (`&`) or alias (`*`) has an empty name or is not separated from a following flow collection by whitespace. |
| `InvalidTag` | `yaml.invalid-tag` | `Scanner`, `Tag` | A tag is malformed (unterminated or invalid verbatim URI, empty tag name, missing shorthand suffix, missing trailing whitespace, or invalid percent-escapes). Also the default code for `ErrorKind::Tag`. |
| `UndefinedTagHandle` | `yaml.undefined-tag-handle` | `Scanner` | A shorthand tag references a handle not declared by a `%TAG` directive for the current document. |

## Parser

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `DirectiveWithoutDocument` | `yaml.directive-without-document` | `Parser` | One or more directives are not followed by an explicit `---` document start marker. |
| `InvalidDocumentStart` | `yaml.invalid-document-start` | `Parser` | A block collection or document-start property before a same-line block mapping key appears on a `---` line. |
| `ContentAfterDocumentEnd` | `yaml.content-after-document-end` | `Parser` | Non-comment tokens appear on the same line after a `...` document end marker. |
| `TrailingContent` | `yaml.trailing-content` | `Parser` | Unexpected tokens appear after the document root without a new document start marker. |
| `InvalidBlockIndentation` | `yaml.invalid-block-indentation` | `Parser` | A block sequence or mapping entry is indented inconsistently with its enclosing block structure. |
| `MissingMappingColon` | `yaml.missing-mapping-colon` | `Parser` | A line inside a block mapping has no `:` value indicator. |
| `TrailingDocuments` | `yaml.trailing-documents` | `Parser` | A single-document reader (`parse_value`, `from_str`, `from_slice`, `from_reader`, `from_str_seed`, `DocumentDeserializer`, `parse_frontmatter*`, or their `_with` and `compat` forms) encountered a second document in the stream. |
| `UnexpectedToken` | `yaml.unexpected-token` | `Parser`, `Scanner` | An unexpected token appeared in node, mapping key, or post-node position (or frontmatter was missing a closing `---`/`...` delimiter). Also the default code for `ErrorKind::Parser` and `ErrorKind::Scanner`. |
| `UnexpectedMappingValue` | `yaml.unexpected-mapping-value` | `Parser` | An unexpected token or nested mapping value (`:`) appeared where a single mapping value was expected. |
| `UnterminatedFlowCollection` | `yaml.unterminated-flow-collection` | `Parser` | A flow sequence (`[...]`) or flow mapping (`{...}`) reached end of input without its closing bracket. |
| `InvalidFlowCollection` | `yaml.invalid-flow-collection` | `Parser` | A flow collection is missing a `,` or closing bracket, has an empty entry, or has a mismatched bracket. |
| `DuplicateNodeProperties` | `yaml.duplicate-node-properties` | `Parser` | A single node carries more than one anchor or more than one tag. |
| `MisplacedNodeProperties` | `yaml.misplaced-node-properties` | `Parser` | Node properties precede a compact block sequence or an explicit `?` mapping key on the same line. |
| `InvalidImplicitKey` | `yaml.invalid-implicit-key` | `Parser` | An implicit mapping key spans multiple lines or exceeds 1,024 characters before its `:`. |
| `AliasWithProperties` | `yaml.alias-with-properties` | `Parser` | An alias node (`*name`) carries a tag or anchor property. |

## Composition, aliases, and duplicate keys

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `UnknownAnchor` | `yaml.unknown-anchor` | `Composition`, `Alias` | An alias refers to an anchor name that was not defined earlier in the same document. Also the default code for `ErrorKind::Alias`. |
| `CyclicAlias` | `yaml.cyclic-alias` | `Composition`, `Alias` | An alias refers to an enclosing collection whose anchor has not yet closed. |
| `DuplicateKey` | `yaml.duplicate-key` | `DuplicateKey`, `Serde` | A mapping contains two semantically equal keys during YAML composition or Serde serialization. |
| `InvalidMergeOperand` | `yaml.invalid-merge-operand` | `Composition` | A merge key (`<<`) evaluated by `Value::apply_merge_keys*` or `ParseOptions::with_merge_keys(true)` has a value that is neither a mapping nor a sequence of mappings. |
| `MalformedEventStream` | `yaml.malformed-event-stream` | `Composition` | An event stream handed to composition is structurally unbalanced or incomplete. Also the default code for `ErrorKind::Composition`. |

## Scalar resolution

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `InvalidIntScalar` | `yaml.invalid-int-scalar` | `Resolution` | A scalar explicitly tagged `!!int` does not match the YAML 1.2 Core Schema integer grammar or overflows `i128` / `u128`. |
| `InvalidFloatScalar` | `yaml.invalid-float-scalar` | `Resolution` | A scalar explicitly tagged `!!float` does not match the YAML 1.2 Core Schema float grammar. |
| `InvalidBoolScalar` | `yaml.invalid-bool-scalar` | `Resolution` | A scalar explicitly tagged `!!bool` does not match the YAML 1.2 Core Schema boolean grammar. |
| `InvalidNullScalar` | `yaml.invalid-null-scalar` | `Resolution` | A scalar explicitly tagged `!!null` does not match the YAML 1.2 Core Schema null grammar. |
| `InvalidScalar` | `yaml.invalid-scalar` | `Resolution` | Generic scalar resolution failure (the default code for `ErrorKind::Resolution`). |

## Resource limits

All limit-exceeded errors also attach a structured `LimitExceeded` value accessible through
`Error::limit()` (except where `libyaml-compat` remaps `DepthLimit` or `DocumentCountLimit`). See
[`docs/limits.md`](limits.md) for defaults and accounting rules.

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `LimitExceeded` | `yaml.limit-exceeded` | `Limit` | Generic resource-limit failure (the default code for `ErrorKind::Limit` before `Error::with_limit` refines it). |
| `InvalidLimits` | `yaml.invalid-limits` | `Limit` | A `Limits` configuration failed validation (for example, `max_depth > MAX_DEPTH_CAP` or `node_cost_bytes == 0`). |
| `InputSizeLimit` | `yaml.input-size-limit` | `Limit` | Input byte length exceeded `Limits::max_input_bytes`. |
| `DepthLimit` | `yaml.depth-limit` | `Limit`, `Serde` (`compat`) | Collection nesting or recursion depth exceeded `Limits::max_depth`. |
| `DocumentCountLimit` | `yaml.document-count-limit` | `Limit`, `Parser` (`compat`) | Stream document count exceeded `Limits::max_documents`. |
| `ScalarSizeLimit` | `yaml.scalar-size-limit` | `Limit` | A scalar's raw or decoded byte length exceeded `Limits::max_scalar_bytes`. |
| `CollectionEntriesLimit` | `yaml.collection-entries-limit` | `Limit` | A single sequence or mapping exceeded `Limits::max_collection_entries`. |
| `EventCountLimit` | `yaml.event-count-limit` | `Limit` | Token or event count exceeded `Limits::max_events`. |
| `AnchorCountLimit` | `yaml.anchor-count-limit` | `Limit` | Anchor definition count exceeded `Limits::max_anchors`. |
| `AliasDereferenceLimit` | `yaml.alias-dereference-limit` | `Limit` | Alias dereference count exceeded `Limits::max_alias_dereferences`. |
| `AliasExpansionLimit` | `yaml.alias-expansion-limit` | `Limit` | Cumulative byte-weighted alias expansion exceeded `Limits::max_alias_expansion_bytes`. |
| `OutputSizeLimit` | `yaml.output-size-limit` | `Limit` | Emitted byte count exceeded `Limits::max_output_bytes`. |
| `TagExpansionLimit` | `yaml.tag-expansion-limit` | `Limit` | Cumulative expanded `%TAG` handle bytes exceeded `Limits::max_tag_expansion_bytes`. |

## Serde

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `InvalidType` | `yaml.invalid-type` | `Serde` | A YAML node's type does not match what the Serde target expected (via `serde::de::Error::invalid_type`, including non-float radix integers read as `f64`). |
| `InvalidValue` | `yaml.invalid-value` | `Serde` | A YAML value has the expected type category but is out of range or otherwise unacceptable to the Serde target (`serde::de::Error::invalid_value`). |
| `InvalidLength` | `yaml.invalid-length` | `Serde` | A sequence, tuple, or map has the wrong number of elements (`serde::de::Error::invalid_length`). |
| `UnknownVariant` | `yaml.unknown-variant` | `Serde` | An enum variant name is not among the target enum's variants (`serde::de::Error::unknown_variant`). |
| `UnknownField` | `yaml.unknown-field` | `Serde` | A struct field name is not recognized by a target that rejects unknown fields (`serde::de::Error::unknown_field`). |
| `MissingField` | `yaml.missing-field` | `Serde` | A required struct field is absent (`serde::de::Error::missing_field`). |
| `DuplicateField` | `yaml.duplicate-field` | `Serde` | A struct field was supplied more than once (for example, via a Serde field alias or in `libyaml-compat` mode; `serde::de::Error::duplicate_field`). |
| `InvalidEnumRepresentation` | `yaml.invalid-enum-representation` | `Serde` | A YAML node cannot represent the requested Serde enum (for example, a multi-entry or empty mapping, a non-string variant key, or a non-unit payload for a unit variant). |
| `MapValueBeforeKey` | `yaml.map-value-before-key` | `Serde` | A `MapAccess` or `SerializeMap` caller requested or serialized a map value before its key. |
| `SerdeCustom` | `yaml.serde-custom` | `Serde` | A custom Serde deserialization or serialization error (via `serde::de::Error::custom`, `serde::ser::Error::custom`, or a failing `Display` in `collect_str`). Also the default code for `ErrorKind::Serde`. |

## Emitter and I/O

| Variant | Code | Usual `ErrorKind` | Meaning |
| --- | --- | --- | --- |
| `InvalidEmitterOptions` | `yaml.invalid-emitter-options` | `Emitter` | Emitter options failed validation (for example, `indent` outside `2..=9`). |
| `UnsupportedTag` | `yaml.unsupported-tag` | `Emitter` | A tag cannot be written as valid YAML, wraps an already-tagged payload, or conflicts with the scalar value's YAML 1.2 Core Schema rule. |
| `InvalidEmitEvent` | `yaml.invalid-emit-event` | `Emitter` | An event stream handed to `to_string_events*`, `to_writer_events*`, or `to_writer_event_stream*` is malformed, has an unwritable anchor/alias/directive, or references an undefined alias. Also the default code for `ErrorKind::Emitter`. |
| `Io` | `yaml.io` | `Io` | Reading YAML input from a `std::io::Read` source (`from_reader*`) or writing YAML output to a `std::io::Write` sink returned an I/O error. |
