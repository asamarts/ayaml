//! Shared benchmark scenarios, typed Serde models, and mandatory pre-timing semantic
//! validation for `ayaml-bench`.
//!
//! Every benchmark group in `benches/throughput.rs` calls [`validate_all_scenarios`] (or the
//! per-group validators) before Criterion begins timing so broken parses, silent empty outputs,
//! or mismatched error codes can never masquerade as fast timings.

use std::borrow::Cow;
use std::collections::BTreeMap;

use ayaml::{ColumnEncoding, Deserializer, EmitOptions, ErrorCode, LineIndex, SpannedTree, Value};
use ayaml_fixtures::{Fixture, bulk_automation_files, generate};
use serde::{Deserialize, Serialize};

/// Owned typed Serde target matching `frontmatter-tiny`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct FrontmatterOwned {
    /// Task identifier.
    pub id: String,
    /// Human-readable title.
    pub title: String,
    /// Numeric phase.
    pub phase: u32,
    /// Lifecycle status string.
    pub status: String,
    /// Effort estimate in days.
    pub effort_days: f64,
    /// Blocking task identifiers.
    pub blocked_by: Vec<String>,
    /// Owned repository paths.
    pub paths: Vec<String>,
    /// Classification tags.
    pub tags: Vec<String>,
}

/// Zero-copy borrowed Serde target matching `frontmatter-tiny`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct FrontmatterBorrowed<'a> {
    /// Task identifier borrowed directly from input.
    pub id: &'a str,
    /// Human-readable title borrowed directly from input.
    pub title: &'a str,
    /// Numeric phase.
    pub phase: u32,
    /// Lifecycle status string borrowed directly from input.
    pub status: &'a str,
    /// Effort estimate in days.
    pub effort_days: f64,
    /// Blocking task identifiers borrowed from input.
    #[serde(borrow)]
    pub blocked_by: Vec<&'a str>,
    /// Owned repository paths borrowed from input.
    #[serde(borrow)]
    pub paths: Vec<&'a str>,
    /// Classification tags borrowed from input.
    #[serde(borrow)]
    pub tags: Vec<&'a str>,
}

/// Owned typed Serde target matching `boundary-*` fixtures straddling the 4 KiB threshold.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct BoundaryOwned {
    /// Service name header.
    pub service: String,
    /// Key-value entries map.
    pub entries: BTreeMap<String, String>,
}

/// Borrowed typed Serde target matching `boundary-*` fixtures straddling the 4 KiB threshold.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct BoundaryBorrowed<'a> {
    /// Service name header borrowed from input.
    pub service: &'a str,
    /// Key-value entries borrowed from input where unescaped.
    #[serde(borrow)]
    pub entries: BTreeMap<&'a str, Cow<'a, str>>,
}

/// Owned typed Serde target matching individual documents in `cluster-manifests`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClusterDocumentOwned {
    /// API version header.
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    /// Resource kind.
    pub kind: String,
    /// Resource metadata block.
    pub metadata: ClusterMetadataOwned,
}

/// Resource metadata sub-struct for [`ClusterDocumentOwned`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ClusterMetadataOwned {
    /// Resource name.
    pub name: String,
    /// Optional namespace.
    pub namespace: Option<String>,
}

/// Borrowed typed Serde target matching individual documents in `cluster-manifests`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ClusterDocumentBorrowed<'a> {
    /// API version header.
    #[serde(rename = "apiVersion")]
    pub api_version: &'a str,
    /// Resource kind.
    pub kind: &'a str,
    /// Resource metadata block.
    #[serde(borrow)]
    pub metadata: ClusterMetadataBorrowed<'a>,
}

/// Borrowed metadata sub-struct for [`ClusterDocumentBorrowed`].
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ClusterMetadataBorrowed<'a> {
    /// Resource name.
    pub name: &'a str,
    /// Optional namespace.
    pub namespace: Option<&'a str>,
}

/// All 11 realistic domain workload fixture IDs from `ayaml-fixtures`.
pub const DOMAIN_IDS: &[&str] = &[
    "cluster-manifests",
    "openapi-spec",
    "ci-workflow",
    "container-compose",
    "automation-bulk",
    "lockfile-wide",
    "custom-tags",
    "escape-routes",
    "frontmatter-tiny",
    "json-in-yaml",
    "unicode-multibyte",
];

/// All 4 strategy-boundary fixture IDs straddling 4 KiB (`SMALL_INPUT_BYTES`).
pub const BOUNDARY_IDS: &[&str] = &[
    "boundary-3kb",
    "boundary-4kb-below",
    "boundary-4kb-above",
    "boundary-8kb",
];

/// Scaling ladder fixture IDs (10 KiB / 100 KiB / 1 MiB across map, sequence, and `OpenAPI`).
pub const SCALING_IDS: &[&str] = &[
    "scale-map-10kb",
    "scale-map-100kb",
    "scale-map-1mb",
    "scale-seq-10kb",
    "scale-seq-100kb",
    "scale-seq-1mb",
    "scale-openapi-10kb",
    "scale-openapi-100kb",
    "scale-openapi-1mb",
];

/// Structural stress fixture IDs.
pub const STRUCTURAL_IDS: &[&str] = &[
    "depth-64",
    "depth-128",
    "aliases-1000",
    "large-scalar-1mb",
    "flow-map-256kb",
    "flow-seq-256kb",
    "short-seq-20000",
    "short-map-20000",
    "short-explicit-20000",
    "nested-keys",
];

/// Error/rejection fixture IDs.
pub const ERROR_IDS: &[&str] = &[
    "err-syntax-early",
    "err-syntax-late",
    "err-duplicate-key-late",
    "err-depth-limit",
    "err-invalid-escape-late",
];

/// Summary counts returned by [`validate_all_scenarios`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ValidationSummary {
    /// Validated domain workload fixtures.
    pub domains_checked: usize,
    /// Validated 4 KiB boundary fixtures.
    pub boundaries_checked: usize,
    /// Validated scaling ladder fixtures.
    pub scaling_checked: usize,
    /// Validated structural stress fixtures.
    pub structural_checked: usize,
    /// Validated error-path fixtures (including late typed decode).
    pub errors_checked: usize,
    /// Validated multi-document stream documents decoded.
    pub stream_docs_decoded: usize,
}

/// Load a required fixture by catalog ID.
///
/// # Panics
///
/// Panics if `id` is not present in the `ayaml-fixtures` catalog.
#[must_use]
pub fn must_fixture(id: &str) -> Fixture {
    generate(id).unwrap_or_else(|| panic!("missing required catalog fixture: {id}"))
}

/// Stream-decode every document in `input` into [`ClusterDocumentOwned`] via [`Deserializer`].
///
/// # Errors
///
/// Propagates any parser or Serde error from [`Deserializer`].
pub fn stream_cluster_owned(input: &str) -> Result<Vec<ClusterDocumentOwned>, ayaml::Error> {
    let mut cursor = Deserializer::from_str(input)?;
    let mut docs = Vec::new();
    while let Some(doc) = cursor.next::<ClusterDocumentOwned>()? {
        docs.push(doc);
    }
    Ok(docs)
}

/// Stream-decode every document in `input` into [`ClusterDocumentBorrowed`] via [`Deserializer`].
///
/// # Errors
///
/// Propagates any parser or Serde error from [`Deserializer`].
pub fn stream_cluster_borrowed(
    input: &str,
) -> Result<Vec<ClusterDocumentBorrowed<'_>>, ayaml::Error> {
    let mut cursor = Deserializer::from_str(input)?;
    let mut docs = Vec::new();
    while let Some(doc) = cursor.next_borrowed::<ClusterDocumentBorrowed<'_>>()? {
        docs.push(doc);
    }
    Ok(docs)
}

/// Exercise `LineIndex` position/offset conversions across UTF-8, UTF-16, and scalar columns
/// using a pre-built [`LineIndex`].
///
/// # Panics
///
/// Panics if a valid UTF-8 boundary offset fails to round-trip through `LineIndex`.
#[must_use]
pub fn probe_line_index_queries(input: &str, index: &LineIndex<'_>) -> usize {
    let mut checksum = 0_usize;
    let step = (input.len() / 64).max(1);
    let mut offset = 0_usize;
    while offset < input.len() {
        if input.is_char_boundary(offset)
            && !(input.as_bytes()[offset] == b'\n'
                && offset > 0
                && input.as_bytes()[offset - 1] == b'\r')
        {
            for encoding in [
                ColumnEncoding::Utf8,
                ColumnEncoding::Utf16,
                ColumnEncoding::Scalar,
            ] {
                let pos = index
                    .to_position(offset, encoding)
                    .expect("valid char boundary converts to position");
                let back = index
                    .to_offset(pos, encoding)
                    .expect("valid position converts back to offset");
                debug_assert_eq!(back, offset, "LineIndex round-trip mismatch");
                checksum = checksum.wrapping_add(back);
            }
        }
        offset = offset.saturating_add(step);
    }
    checksum
}

/// Construct a [`LineIndex`] and exercise position/offset conversions across all encodings.
#[must_use]
pub fn probe_line_index(input: &str) -> usize {
    let index = LineIndex::new(input);
    probe_line_index_queries(input, &index)
}

/// Pre-timing semantic validation covering every benchmarked read, emit, span, boundary, scaling,
/// and error path.
///
/// # Errors
///
/// Returns a descriptive diagnostic if any fixture fails digest verification, parses to unexpected
/// values, loses semantic fidelity on round-trip emission, or returns the wrong error code.
pub fn validate_all_scenarios() -> Result<ValidationSummary, String> {
    ayaml_fixtures::verify_manifest_digests()?;

    // 1. Domain workloads + multi-document streaming + bulk small files.
    for &id in DOMAIN_IDS {
        let fixture = must_fixture(id);
        let docs = ayaml::parse_documents(&fixture.content)
            .map_err(|e| format!("domain `{id}` parse_documents failed: {e}"))?;
        if fixture.multi_document {
            if docs.len() < 2 {
                return Err(format!(
                    "domain `{id}` expected >1 docs, got {}",
                    docs.len()
                ));
            }
        } else if docs.len() != 1 {
            return Err(format!("domain `{id}` expected 1 doc, got {}", docs.len()));
        }
        let tree = SpannedTree::parse(&fixture.content)
            .map_err(|e| format!("domain `{id}` SpannedTree::parse failed: {e}"))?;
        if tree.documents().len() != docs.len() {
            return Err(format!(
                "domain `{id}` SpannedTree doc count {} != Value doc count {}",
                tree.documents().len(),
                docs.len()
            ));
        }
    }

    let cluster = must_fixture("cluster-manifests");
    let owned_stream = stream_cluster_owned(&cluster.content)
        .map_err(|e| format!("cluster-manifests stream_owned failed: {e}"))?;
    let borrowed_stream = stream_cluster_borrowed(&cluster.content)
        .map_err(|e| format!("cluster-manifests stream_borrowed failed: {e}"))?;
    if owned_stream.len() != borrowed_stream.len() || owned_stream.len() != 49 {
        return Err(format!(
            "cluster-manifests stream count mismatch: owned={}, borrowed={} (expected 49)",
            owned_stream.len(),
            borrowed_stream.len()
        ));
    }
    for (o, b) in owned_stream.iter().zip(&borrowed_stream) {
        if o.api_version != b.api_version
            || o.kind != b.kind
            || o.metadata.name != b.metadata.name
            || o.metadata.namespace.as_deref() != b.metadata.namespace
        {
            return Err("cluster-manifests owned vs borrowed stream mismatch".to_owned());
        }
    }

    let bulk = bulk_automation_files(32);
    for file in &bulk {
        let val = ayaml::parse_value(&file.content)
            .map_err(|e| format!("bulk file `{}` parse failed: {e}", file.id))?;
        if !val.is_sequence() {
            return Err(format!("bulk file `{}` root must be sequence", file.id));
        }
    }

    // 2. Typed owned vs borrowed decoding & Serde round-trip emission on `frontmatter-tiny`.
    let front = must_fixture("frontmatter-tiny");
    let fm_owned: FrontmatterOwned = ayaml::from_str(&front.content)
        .map_err(|e| format!("frontmatter owned decode failed: {e}"))?;
    let fm_borrowed: FrontmatterBorrowed<'_> = ayaml::from_str_borrowed(&front.content)
        .map_err(|e| format!("frontmatter borrowed decode failed: {e}"))?;
    if fm_owned.id != fm_borrowed.id
        || fm_owned.title != fm_borrowed.title
        || fm_owned.phase != fm_borrowed.phase
        || fm_owned.tags.len() != fm_borrowed.tags.len()
    {
        return Err("frontmatter owned vs borrowed mismatch".to_owned());
    }
    let fm_emitted =
        ayaml::to_string(&fm_owned).map_err(|e| format!("frontmatter to_string failed: {e}"))?;
    let fm_roundtrip: FrontmatterOwned =
        ayaml::from_str(&fm_emitted).map_err(|e| format!("frontmatter round-trip failed: {e}"))?;
    if fm_roundtrip != fm_owned {
        return Err("frontmatter Serde round-trip value drift".to_owned());
    }

    // 3. Emission styles (block, flow, events, multi-document).
    let openapi = must_fixture("openapi-spec");
    let openapi_val =
        ayaml::parse_value(&openapi.content).map_err(|e| format!("openapi parse failed: {e}"))?;
    let emitted_block = ayaml::to_string_value(&openapi_val)
        .map_err(|e| format!("openapi block emit failed: {e}"))?;
    let emitted_flow = ayaml::to_string_value_with(&openapi_val, &EmitOptions::flow())
        .map_err(|e| format!("openapi flow emit failed: {e}"))?;
    if ayaml::parse_value(&emitted_block).map_err(|e| e.to_string())? != openapi_val {
        return Err("openapi block emit round-trip mismatch".to_owned());
    }
    if ayaml::parse_value(&emitted_flow).map_err(|e| e.to_string())? != openapi_val {
        return Err("openapi flow emit round-trip mismatch".to_owned());
    }
    let openapi_events =
        ayaml::internals::parse_events(&openapi.content, &ayaml::Limits::default())
            .map_err(|e| format!("openapi events failed: {e}"))?;
    let emitted_events = ayaml::to_string_events(&openapi_events)
        .map_err(|e| format!("openapi event emit failed: {e}"))?;
    if ayaml::parse_value(&emitted_events).map_err(|e| e.to_string())? != openapi_val {
        return Err("openapi event emit round-trip mismatch".to_owned());
    }
    let cluster_vals = ayaml::parse_documents(&cluster.content)
        .map_err(|e| format!("cluster parse_documents failed: {e}"))?;
    let emitted_docs = ayaml::to_string_documents(&cluster_vals)
        .map_err(|e| format!("cluster to_string_documents failed: {e}"))?;
    if ayaml::parse_documents(&emitted_docs).map_err(|e| e.to_string())? != cluster_vals {
        return Err("cluster multi-doc emit round-trip mismatch".to_owned());
    }

    // 4. 4 KiB strategy boundary owned vs borrowed equivalence.
    for &id in BOUNDARY_IDS {
        let fixture = must_fixture(id);
        let owned: BoundaryOwned = ayaml::from_str(&fixture.content)
            .map_err(|e| format!("boundary `{id}` owned decode failed: {e}"))?;
        let borrowed: BoundaryBorrowed<'_> = ayaml::from_str_borrowed(&fixture.content)
            .map_err(|e| format!("boundary `{id}` borrowed decode failed: {e}"))?;
        if owned.service != borrowed.service || owned.entries.len() != borrowed.entries.len() {
            return Err(format!("boundary `{id}` owned vs borrowed mismatch"));
        }
        for (k, v) in &owned.entries {
            if borrowed.entries.get(k.as_str()).map(Cow::as_ref) != Some(v.as_str()) {
                return Err(format!("boundary `{id}` key `{k}` mismatch"));
            }
        }
    }

    // 5. Scaling ladders & structural shapes.
    for &id in SCALING_IDS {
        let fixture = must_fixture(id);
        let val = ayaml::parse_value(&fixture.content)
            .map_err(|e| format!("scaling `{id}` parse failed: {e}"))?;
        if val == Value::Null {
            return Err(format!("scaling `{id}` parsed to Null"));
        }
    }
    for &id in STRUCTURAL_IDS {
        let fixture = must_fixture(id);
        let val = ayaml::parse_value(&fixture.content)
            .map_err(|e| format!("structural `{id}` parse failed: {e}"))?;
        if val == Value::Null {
            return Err(format!("structural `{id}` parsed to Null"));
        }
    }

    // 6. LineIndex round-trip sanity on multi-byte Unicode.
    let unicode = must_fixture("unicode-multibyte");
    if probe_line_index(&unicode.content) == 0 {
        return Err("unicode-multibyte LineIndex probe returned 0".to_owned());
    }

    // 7. Error/rejection paths + late typed mismatch.
    for &id in ERROR_IDS {
        let fixture = must_fixture(id);
        let err = ayaml::parse_value(&fixture.content)
            .expect_err(&format!("error fixture `{id}` unexpectedly parsed"));
        match id {
            "err-duplicate-key-late" if err.code() != ErrorCode::DuplicateKey => {
                return Err(format!(
                    "`{id}` returned {:?}, expected DuplicateKey",
                    err.code()
                ));
            }
            "err-depth-limit" if err.code() != ErrorCode::DepthLimit => {
                return Err(format!(
                    "`{id}` returned {:?}, expected DepthLimit",
                    err.code()
                ));
            }
            "err-invalid-escape-late" if err.code() != ErrorCode::InvalidEscape => {
                return Err(format!(
                    "`{id}` returned {:?}, expected InvalidEscape",
                    err.code()
                ));
            }
            _ => {}
        }
    }
    let late_typed = must_fixture("typed-mismatch-late");
    if ayaml::from_str::<BTreeMap<String, u64>>(&late_typed.content).is_ok() {
        return Err("typed-mismatch-late unexpectedly decoded as BTreeMap<String, u64>".to_owned());
    }

    Ok(ValidationSummary {
        domains_checked: DOMAIN_IDS.len(),
        boundaries_checked: BOUNDARY_IDS.len(),
        scaling_checked: SCALING_IDS.len(),
        structural_checked: STRUCTURAL_IDS.len(),
        errors_checked: ERROR_IDS.len() + 1,
        stream_docs_decoded: owned_stream.len(),
    })
}

#[cfg(test)]
mod tests {
    use crate::validate_all_scenarios;

    #[test]
    fn semantic_validation_passes_for_every_benchmark_scenario() {
        let summary = validate_all_scenarios().unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(summary.domains_checked, 11);
        assert_eq!(summary.boundaries_checked, 4);
        assert_eq!(summary.scaling_checked, 9);
        assert_eq!(summary.structural_checked, 10);
        assert_eq!(summary.errors_checked, 6);
        assert_eq!(summary.stream_docs_decoded, 49);
    }
}
