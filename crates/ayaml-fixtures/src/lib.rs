//! Deterministic, SHA-256-verified YAML fixture catalog for `ayaml` benchmarks, allocation
//! budgets, and instruction-count gates.
//!
//! Every generator uses fixed `xorshift64*` seeds and pure-Rust FIPS 180-4 SHA-256 digests so
//! Criterion timing runs, counting-allocator measurements, and Callgrind instruction gates
//! exercise byte-for-byte identical inputs across machines and commits.

mod domains;
mod prng;
pub mod sha256;
mod shapes;

use std::fmt::Write as _;

/// Broad workload classification for a catalog fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FixtureCategory {
    /// Realistic production YAML domain workload.
    Domain,
    /// Sizes bracketing internal parser/reader strategy boundaries (such as 4 KiB).
    Boundary,
    /// Multi-order-of-magnitude (1x / 10x / 100x) linear scaling ladders.
    Scaling,
    /// Focused structural stress shape (depth, aliases, large scalars, dense short entries).
    Structural,
    /// Intentionally invalid or limit-exceeding input for error-path measurement.
    ErrorPath,
}

impl FixtureCategory {
    /// Stable lowercase identifier for serialization and tables.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Domain => "domain",
            Self::Boundary => "boundary",
            Self::Scaling => "scaling",
            Self::Structural => "structural",
            Self::ErrorPath => "error-path",
        }
    }
}

/// Static specification of one fixture in the catalog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FixtureSpec {
    /// Stable kebab-case fixture identifier.
    pub id: &'static str,
    /// Workload category.
    pub category: FixtureCategory,
    /// Short human-readable description.
    pub description: &'static str,
    /// Whether the fixture is a multi-document YAML stream.
    pub multi_document: bool,
    /// Whether the fixture is expected to parse cleanly under default YAML 1.2.2 mode.
    pub expect_valid: bool,
    /// Fixed generator seed (0 when the generator is purely structural).
    pub seed: u64,
    /// Pinned expected lowercase hex SHA-256 digest of the generated UTF-8 bytes.
    pub expected_sha256: &'static str,
}

/// A materialized fixture with its exact UTF-8 payload and SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fixture {
    /// Stable fixture identifier.
    pub id: String,
    /// Workload category.
    pub category: FixtureCategory,
    /// Short human-readable description.
    pub description: &'static str,
    /// Whether the fixture is a multi-document YAML stream.
    pub multi_document: bool,
    /// Whether default YAML 1.2.2 parsing should succeed.
    pub expect_valid: bool,
    /// Fixed generator seed.
    pub seed: u64,
    /// Generated UTF-8 YAML content.
    pub content: String,
    /// Byte length (`content.len()`).
    pub bytes: usize,
    /// Lowercase hex SHA-256 digest of `content.as_bytes()`.
    pub sha256: String,
}

/// Metadata entry for one fixture inside a [`CatalogManifest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
    /// Stable fixture identifier.
    pub id: String,
    /// Workload category.
    pub category: FixtureCategory,
    /// Whether the fixture is multi-document.
    pub multi_document: bool,
    /// Whether default YAML 1.2.2 parsing should succeed.
    pub expect_valid: bool,
    /// Fixed generator seed.
    pub seed: u64,
    /// Byte length of generated content.
    pub bytes: usize,
    /// Lowercase hex SHA-256 digest of generated content.
    pub sha256: String,
}

/// Deterministic manifest summarizing the entire catalog with a combined SHA-256 digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogManifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Combined SHA-256 over all `(id, bytes, sha256)` entries in catalog order.
    pub manifest_sha256: String,
    /// Ordered per-fixture entries.
    pub entries: Vec<ManifestEntry>,
}

impl CatalogManifest {
    /// Render the manifest as deterministic pretty-printed JSON without external crates.
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut out = String::with_capacity(self.entries.len() * 140 + 160);
        let _ = writeln!(
            out,
            "{{\n  \"schema_version\": {},\n  \"manifest_sha256\": \"{}\",\n  \"fixtures\": [",
            self.schema_version, self.manifest_sha256
        );
        for (idx, entry) in self.entries.iter().enumerate() {
            let comma = if idx + 1 == self.entries.len() {
                ""
            } else {
                ","
            };
            let _ = writeln!(
                out,
                "    {{\"id\": \"{}\", \"category\": \"{}\", \"multi_document\": {}, \"expect_valid\": {}, \"seed\": {}, \"bytes\": {}, \"sha256\": \"{}\"}}{comma}",
                entry.id,
                entry.category.as_str(),
                entry.multi_document,
                entry.expect_valid,
                entry.seed,
                entry.bytes,
                entry.sha256,
            );
        }
        out.push_str("  ]\n}\n");
        out
    }
}

const CATALOG: &[FixtureSpec] = &[
    // -- Realistic domain workloads (11 families) --
    FixtureSpec {
        id: "cluster-manifests",
        category: FixtureCategory::Domain,
        description: "Multi-document cluster Deployments, Services, and CRD schema",
        multi_document: true,
        expect_valid: true,
        seed: 0x0001_c1a5_7e40_0001,
        expected_sha256: "1835e9e70495561c218b4649b15837452ced87973b2a1494d81847b4eeba2c4b",
    },
    FixtureSpec {
        id: "openapi-spec",
        category: FixtureCategory::Domain,
        description: "OpenAPI 3.1 service specification with nested paths and flow schemas",
        multi_document: false,
        expect_valid: true,
        seed: 0x0002_0be4_a910_0002,
        expected_sha256: "171af420cc6c70f1f488e734beed00b7602e2dbd5e1825a4783f625224a644a2",
    },
    FixtureSpec {
        id: "ci-workflow",
        category: FixtureCategory::Domain,
        description: "CI workflow with matrix strategies and literal/folded script blocks",
        multi_document: false,
        expect_valid: true,
        seed: 0x0003_c100_f100_0003,
        expected_sha256: "77b59e2ace49a28e84eb0798b8acfb33c1a35da6e1cf210e41f923b150f142b4",
    },
    FixtureSpec {
        id: "container-compose",
        category: FixtureCategory::Domain,
        description: "Container orchestration spec with extension anchors, aliases, and merge keys",
        multi_document: false,
        expect_valid: true,
        seed: 0x0004_c0a9_05e0_0004,
        expected_sha256: "b8b8b3added7e39cd22baf8b3478ed8940882060d72464b4cddc53267d4c0ea5",
    },
    FixtureSpec {
        id: "automation-bulk",
        category: FixtureCategory::Domain,
        description: "Multi-document stream of small automation playbooks",
        multi_document: true,
        expect_valid: true,
        seed: 0x0005_a070_b01c_0005,
        expected_sha256: "5d5d82f2cc0b707a2405c363077e37e69a982fd6eb1d1101528899a8d2eac602",
    },
    FixtureSpec {
        id: "lockfile-wide",
        category: FixtureCategory::Domain,
        description: "Wide lockfile mapping with quoted scoped package keys and integrity hashes",
        multi_document: false,
        expect_valid: true,
        seed: 0x0006_10c4_f11e_0006,
        expected_sha256: "be8f326ada7343a37bb96db7e91fa118d7cad21f93f20420584b3979ef5f54c3",
    },
    FixtureSpec {
        id: "custom-tags",
        category: FixtureCategory::Domain,
        description: "Template heavy in local custom YAML tags and intrinsic expressions",
        multi_document: false,
        expect_valid: true,
        seed: 0x0007_7a65_c0de_0007,
        expected_sha256: "c912e0bb870cc73f88718ff075da573ea7e97f3e741bdae28a0e4e44a8423127",
    },
    FixtureSpec {
        id: "escape-routes",
        category: FixtureCategory::Domain,
        description: "Escape-heavy proxy route and regex config with double-quoted escapes",
        multi_document: false,
        expect_valid: true,
        seed: 0x0008_e5ca_9e00_0008,
        expected_sha256: "8bd9eaebcad60903d775532e13a5eb1a9151fd699dee7578398bc76f798503d2",
    },
    FixtureSpec {
        id: "frontmatter-tiny",
        category: FixtureCategory::Domain,
        description: "Tiny ~340-byte YAML metadata frontmatter header",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "40478a17c8b3d2ab7ae0d5748351b54ad8bd34f0daeb76f16752371f91b7d978",
    },
    FixtureSpec {
        id: "json-in-yaml",
        category: FixtureCategory::Domain,
        description: "Strict RFC 8259 JSON payload parsed directly as YAML 1.2.2",
        multi_document: false,
        expect_valid: true,
        seed: 0x000a_1504_da7a_000a,
        expected_sha256: "67708bd63b72cd686fd1523db82804837791da731dc6f56d0889499da148a0e1",
    },
    FixtureSpec {
        id: "unicode-multibyte",
        category: FixtureCategory::Domain,
        description: "Multi-byte UTF-8 document with accents, CJK, Cyrillic, Arabic, and emoji",
        multi_document: false,
        expect_valid: true,
        seed: 0x000b_007f_8000_000b,
        expected_sha256: "6bcceb5c2fd5d826436ec28566a44235e3b1db466f378a0dbb5cd478b14bb379",
    },
    // -- Strategy boundary fixtures (4 KiB typed tree-vs-stream boundary) --
    FixtureSpec {
        id: "boundary-3kb",
        category: FixtureCategory::Boundary,
        description: "Structured mapping comfortably below the 4 KiB strategy boundary",
        multi_document: false,
        expect_valid: true,
        seed: 0x0010_b000_3000_0010,
        expected_sha256: "ca14510004d08ed8e3e98f61049b63efb8633eb060192df2e78021bf717fad50",
    },
    FixtureSpec {
        id: "boundary-4kb-below",
        category: FixtureCategory::Boundary,
        description: "Structured mapping just under 4,096 bytes (~4,020 B)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0011_b000_4000_0011,
        expected_sha256: "0a7087645f60a04da352a4c9ef785bd85c280ea63e2849b4ec3b31b34a9e7f27",
    },
    FixtureSpec {
        id: "boundary-4kb-above",
        category: FixtureCategory::Boundary,
        description: "Structured mapping just above 4,096 bytes (~4,350 B)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0012_b000_4400_0012,
        expected_sha256: "2aa5270e8528d7a914b849f5a319529ccf389ceeab385135f860797b3d421892",
    },
    FixtureSpec {
        id: "boundary-8kb",
        category: FixtureCategory::Boundary,
        description: "Structured mapping comfortably above the 4 KiB strategy boundary",
        multi_document: false,
        expect_valid: true,
        seed: 0x0013_b000_8000_0013,
        expected_sha256: "b039b9a5137c4b52535d9fcc9f43bec620a95c9585d5a78f22ecd917c93637c4",
    },
    // -- Scaling ladders (1x / 10x / 100x linear complexity verification) --
    FixtureSpec {
        id: "scale-map-10kb",
        category: FixtureCategory::Scaling,
        description: "10 KiB block mapping scaling point (1x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0020_5ca1_e010_0020,
        expected_sha256: "8f64940a96008ec3320097795e1533ef19b5e52f5e6a68d481431672c4e19059",
    },
    FixtureSpec {
        id: "scale-map-100kb",
        category: FixtureCategory::Scaling,
        description: "100 KiB block mapping scaling point (10x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0020_5ca1_e010_0020,
        expected_sha256: "296d2114d8d017ebb38ee96ee30bd0a3202bb95f69a2ec125c86b053d470ed3d",
    },
    FixtureSpec {
        id: "scale-map-1mb",
        category: FixtureCategory::Scaling,
        description: "1 MiB block mapping scaling point (100x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0020_5ca1_e010_0020,
        expected_sha256: "67d08f7037c67f5d75a2bc68fc7ec3ad7351a9848a779de3a883467cf8faf090",
    },
    FixtureSpec {
        id: "scale-seq-10kb",
        category: FixtureCategory::Scaling,
        description: "10 KiB block sequence scaling point (1x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0021_5ca1_e010_0021,
        expected_sha256: "122f38e05ca8ddf7f08ef8e9d5795bf7bb440050904125d3662824f94c23827d",
    },
    FixtureSpec {
        id: "scale-seq-100kb",
        category: FixtureCategory::Scaling,
        description: "100 KiB block sequence scaling point (10x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0021_5ca1_e010_0021,
        expected_sha256: "7911b7f934bfc664f18aa519dc83d06383f0bd39a984accb23f0dd24e88227a4",
    },
    FixtureSpec {
        id: "scale-seq-1mb",
        category: FixtureCategory::Scaling,
        description: "1 MiB block sequence scaling point (100x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0021_5ca1_e010_0021,
        expected_sha256: "538d49bf825683a653f8459b31b4f47d29f708cbe58ea972041a7dd6aa842f73",
    },
    FixtureSpec {
        id: "scale-openapi-10kb",
        category: FixtureCategory::Scaling,
        description: "~10 KiB nested OpenAPI scaling point (1x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0022_5ca1_e010_0022,
        expected_sha256: "8ffd56b6c155555b3b26d7d163b667fc4efd390d91243b1b4e7fdfb0b93a95ed",
    },
    FixtureSpec {
        id: "scale-openapi-100kb",
        category: FixtureCategory::Scaling,
        description: "~100 KiB nested OpenAPI scaling point (10x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0022_5ca1_e010_0022,
        expected_sha256: "691f3ed1b3d1bb44b61e4a0ae6eaea47435667ed59eb194c8fe1e6aed680e33c",
    },
    FixtureSpec {
        id: "scale-openapi-1mb",
        category: FixtureCategory::Scaling,
        description: "~1 MiB nested OpenAPI scaling point (100x)",
        multi_document: false,
        expect_valid: true,
        seed: 0x0022_5ca1_e010_0022,
        expected_sha256: "141e36b67fe1afff3a8415983f8aa48b210fdd07fac442f381d00dc023f44645",
    },
    // -- Structural stress shapes --
    FixtureSpec {
        id: "depth-64",
        category: FixtureCategory::Structural,
        description: "Nested flow sequence of depth 64",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "f8e98d56449d4c90792a9368b4558e287a6e96fc9cd94059b42f99577f7c38a8",
    },
    FixtureSpec {
        id: "depth-128",
        category: FixtureCategory::Structural,
        description: "Nested flow sequence at maximum default depth 128",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "c2eba8bfb879e5c1079b88c7daebec8ddbb294ef4f2c4472c501f2a8802f7ba6",
    },
    FixtureSpec {
        id: "aliases-1000",
        category: FixtureCategory::Structural,
        description: "Anchored mapping dereferenced 1,000 times via aliases",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "728f99b4b83d68f18104a9661cc193f4c39042c1ed828648ccf8668a4419a611",
    },
    FixtureSpec {
        id: "large-scalar-1mb",
        category: FixtureCategory::Structural,
        description: "Single 1,000,000-byte plain scalar",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "0c75012d2d17dadeac27f5cd1f5217ab0e96199ed04cb40b156a7a0189ba0de8",
    },
    FixtureSpec {
        id: "flow-map-256kb",
        category: FixtureCategory::Structural,
        description: "256 KiB single-line flow mapping",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "2d18ca9af5bc8e0a1b96f98c6d9cd21ee1e092edd76947a0761fd32322912809",
    },
    FixtureSpec {
        id: "flow-seq-256kb",
        category: FixtureCategory::Structural,
        description: "256 KiB multi-line flow sequence of integers",
        multi_document: false,
        expect_valid: true,
        seed: 0x0030_f10a_5e90_0030,
        expected_sha256: "0c7166edf585c1489570a3e8d31bed6c845cf132d943beb0bb3dfa6eda0f1a24",
    },
    FixtureSpec {
        id: "short-seq-20000",
        category: FixtureCategory::Structural,
        description: "20,000 short block sequence entries (- a)",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "7844bc3b3ebf9ee08b7e312c4fa898a3b213adf2c321ebb7ba702bd85209b6c1",
    },
    FixtureSpec {
        id: "short-map-20000",
        category: FixtureCategory::Structural,
        description: "20,000 short implicit block mapping entries (kN: vN)",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "3fcaa88304cf085cc204acaee3b11c41235c3c525211075d7c52e05c2ebd9fa9",
    },
    FixtureSpec {
        id: "short-explicit-20000",
        category: FixtureCategory::Structural,
        description: "20,000 short explicit block mapping entries (? kN / : vN)",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "62c707c898ccf3634bab5346421ea758b43abd6e042fe0f1e4f60893fa7284cb",
    },
    FixtureSpec {
        id: "nested-keys",
        category: FixtureCategory::Structural,
        description: "100-deep nested complex mapping key wrapping a 1,000-entry flow mapping",
        multi_document: false,
        expect_valid: true,
        seed: 0,
        expected_sha256: "d041f772f0087cc93508b70ce1f3da57ab7814a16e7810ee0c59dbdd9f7e70b8",
    },
    FixtureSpec {
        id: "typed-mismatch-late",
        category: FixtureCategory::Structural,
        description: "Valid 64 KiB integer mapping with a trailing string value for late typed-decode errors",
        multi_document: false,
        expect_valid: true,
        seed: 0x0031_79be_d001_0031,
        expected_sha256: "850e4962dce2ab4fa7ced51115711e05500cedb3b3bfd71c27416c732de1aac6",
    },
    // -- Error-path fixtures --
    FixtureSpec {
        id: "err-syntax-early",
        category: FixtureCategory::ErrorPath,
        description: "Malformed flow bracket at start of a 64 KiB document",
        multi_document: false,
        expect_valid: false,
        seed: 0x0040_e440_0001_0040,
        expected_sha256: "af65315ec5d58467508180c33186cfa2ff3a3007d8a14e1da8eb78948e6f5a9d",
    },
    FixtureSpec {
        id: "err-syntax-late",
        category: FixtureCategory::ErrorPath,
        description: "Unterminated flow bracket at end of a 64 KiB document",
        multi_document: false,
        expect_valid: false,
        seed: 0x0041_e440_0002_0041,
        expected_sha256: "0a374c26c1c81ea748dfee4bb0340de0c9aec7dffe946376249962d6d2a6399a",
    },
    FixtureSpec {
        id: "err-duplicate-key-late",
        category: FixtureCategory::ErrorPath,
        description: "Duplicate root key at end of a 64 KiB mapping",
        multi_document: false,
        expect_valid: false,
        seed: 0x0042_e440_0003_0042,
        expected_sha256: "d63ae7cc890c9f5f63ba7d3232d4b8e9aed11adc91f6feaebf3225e0f9e104d3",
    },
    FixtureSpec {
        id: "err-depth-limit",
        category: FixtureCategory::ErrorPath,
        description: "Flow nesting depth 129 exceeding MAX_DEPTH",
        multi_document: false,
        expect_valid: false,
        seed: 0,
        expected_sha256: "3a1ac7b6c0fbb56d743d2e2d916a53795af15cdab2cb0cef48517412af2020bf",
    },
    FixtureSpec {
        id: "err-invalid-escape-late",
        category: FixtureCategory::ErrorPath,
        description: "Invalid escape sequence at end of an escape-heavy route config",
        multi_document: false,
        expect_valid: false,
        seed: 0x0044_e440_0005_0044,
        expected_sha256: "013cf214036dc29746537c5660cafbe15634f7356edf5a432593926c6783deda",
    },
];

/// Return the static ordered specification slice for every fixture in the catalog.
#[must_use]
pub const fn catalog_specs() -> &'static [FixtureSpec] {
    CATALOG
}

fn generate_content(spec: &FixtureSpec) -> String {
    match spec.id {
        "cluster-manifests" => domains::cluster_manifests(spec.seed, 24),
        "openapi-spec" => domains::openapi_spec(spec.seed, 120),
        "ci-workflow" => domains::ci_workflow(spec.seed, 24),
        "container-compose" => domains::container_compose(spec.seed, 48),
        "automation-bulk" => domains::automation_bulk_stream(spec.seed, 48),
        "lockfile-wide" => domains::lockfile_wide(spec.seed, 240),
        "custom-tags" => domains::custom_tags(spec.seed, 60),
        "escape-routes" => domains::escape_routes(spec.seed, 80),
        "frontmatter-tiny" => domains::frontmatter_tiny(),
        "json-in-yaml" => domains::json_in_yaml(spec.seed, 240),
        "unicode-multibyte" => domains::unicode_multibyte(spec.seed, 120),
        "boundary-3kb" => shapes::boundary_mapping(spec.seed, 3_072),
        "boundary-4kb-below" => shapes::boundary_mapping(spec.seed, 4_040),
        "boundary-4kb-above" => shapes::boundary_mapping(spec.seed, 4_400),
        "boundary-8kb" => shapes::boundary_mapping(spec.seed, 8_192),
        "scale-map-10kb" => shapes::block_mapping_sized(spec.seed, 10_000),
        "scale-map-100kb" => shapes::block_mapping_sized(spec.seed, 100_000),
        "scale-map-1mb" => shapes::block_mapping_sized(spec.seed, 1_000_000),
        "scale-seq-10kb" => shapes::block_sequence_sized(spec.seed, 10_000),
        "scale-seq-100kb" => shapes::block_sequence_sized(spec.seed, 100_000),
        "scale-seq-1mb" => shapes::block_sequence_sized(spec.seed, 1_000_000),
        "scale-openapi-10kb" => shapes::openapi_scaled(spec.seed, 10),
        "scale-openapi-100kb" => shapes::openapi_scaled(spec.seed, 105),
        "scale-openapi-1mb" => shapes::openapi_scaled(spec.seed, 1_050),
        "depth-64" => shapes::nested_flow_depth(64),
        "depth-128" => shapes::nested_flow_depth(128),
        "aliases-1000" => shapes::aliases_repeated(1_000),
        "large-scalar-1mb" => shapes::large_scalar(1_000_000),
        "flow-map-256kb" => shapes::flow_mapping_sized(256 * 1024),
        "flow-seq-256kb" => shapes::flow_sequence_sized(spec.seed, 256 * 1024),
        "short-seq-20000" => shapes::short_sequence(20_000),
        "short-map-20000" => shapes::short_mapping(20_000),
        "short-explicit-20000" => shapes::short_explicit_mapping(20_000),
        "nested-keys" => shapes::nested_complex_keys(100, 1_000),
        "typed-mismatch-late" => shapes::typed_mismatch_late(spec.seed),
        "err-syntax-early" => shapes::err_syntax_early(spec.seed),
        "err-syntax-late" => shapes::err_syntax_late(spec.seed),
        "err-duplicate-key-late" => shapes::err_duplicate_key_late(spec.seed),
        "err-depth-limit" => shapes::err_depth_limit(),
        "err-invalid-escape-late" => shapes::err_invalid_escape_late(spec.seed),
        other => unreachable!("unhandled catalog fixture id: {other}"),
    }
}

fn materialize(spec: &FixtureSpec) -> Fixture {
    let content = generate_content(spec);
    let bytes = content.len();
    let digest = sha256::sha256_hex(content.as_bytes());
    Fixture {
        id: spec.id.to_owned(),
        category: spec.category,
        description: spec.description,
        multi_document: spec.multi_document,
        expect_valid: spec.expect_valid,
        seed: spec.seed,
        content,
        bytes,
        sha256: digest,
    }
}

/// Generate one fixture by its stable catalog identifier.
#[must_use]
pub fn generate(id: &str) -> Option<Fixture> {
    CATALOG.iter().find(|spec| spec.id == id).map(materialize)
}

/// Generate every fixture in the catalog in deterministic order.
#[must_use]
pub fn generate_all() -> Vec<Fixture> {
    CATALOG.iter().map(materialize).collect()
}

/// Generate every fixture belonging to `category`.
#[must_use]
pub fn generate_by_category(category: FixtureCategory) -> Vec<Fixture> {
    CATALOG
        .iter()
        .filter(|spec| spec.category == category)
        .map(materialize)
        .collect()
}

/// Generate `count` individual small automation YAML files for bulk small-file benchmarks.
#[must_use]
pub fn bulk_automation_files(count: usize) -> Vec<Fixture> {
    const BASE_SEED: u64 = 0x0005_a070_b01c_0005;
    (0..count)
        .map(|idx| {
            let content = domains::automation_playbook(BASE_SEED, idx);
            let bytes = content.len();
            let digest = sha256::sha256_hex(content.as_bytes());
            Fixture {
                id: format!("automation-bulk-{idx:04}"),
                category: FixtureCategory::Domain,
                description: "Individual small automation playbook",
                multi_document: false,
                expect_valid: true,
                seed: BASE_SEED,
                content,
                bytes,
                sha256: digest,
            }
        })
        .collect()
}

/// Build the deterministic manifest with individual and combined SHA-256 digests.
#[must_use]
pub fn manifest() -> CatalogManifest {
    let fixtures = generate_all();
    let mut combined = sha256::Sha256::new();
    let mut entries = Vec::with_capacity(fixtures.len());
    for fixture in fixtures {
        let line = format!("{} {} {}\n", fixture.id, fixture.bytes, fixture.sha256);
        combined.update(line.as_bytes());
        entries.push(ManifestEntry {
            id: fixture.id,
            category: fixture.category,
            multi_document: fixture.multi_document,
            expect_valid: fixture.expect_valid,
            seed: fixture.seed,
            bytes: fixture.bytes,
            sha256: fixture.sha256,
        });
    }
    CatalogManifest {
        schema_version: 1,
        manifest_sha256: combined.finalize_hex(),
        entries,
    }
}

/// Verify that every generated fixture matches its pinned `expected_sha256` digest in the catalog.
///
/// # Errors
///
/// Returns a descriptive mismatch summary if any fixture's generated digest differs from its
/// pinned SHA-256 constant.
pub fn verify_manifest_digests() -> Result<(), String> {
    let mut mismatches = Vec::new();
    for spec in CATALOG {
        let fixture = materialize(spec);
        if fixture.sha256 != spec.expected_sha256 {
            mismatches.push(format!(
                "{}: expected {}, got {} ({} bytes)",
                spec.id, spec.expected_sha256, fixture.sha256, fixture.bytes
            ));
        }
    }
    if mismatches.is_empty() {
        Ok(())
    } else {
        Err(mismatches.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use crate::{
        FixtureCategory, bulk_automation_files, catalog_specs, generate, generate_all,
        generate_by_category, manifest, sha256, verify_manifest_digests,
    };

    #[test]
    fn sha256_matches_nist_fips_180_4_vectors() {
        assert_eq!(
            sha256::sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256::sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
        let million_a = vec![b'a'; 1_000_000];
        assert_eq!(
            sha256::sha256_hex(&million_a),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );

        // Verify incremental chunking across 56-byte padding and 64-byte block boundaries.
        for msg_len in [0_usize, 1, 55, 56, 57, 63, 64, 65, 119, 120, 128, 193] {
            let msg: Vec<u8> = (0..msg_len)
                .map(|i| u8::try_from((i * 31 + 7) % 256).expect("byte"))
                .collect();
            let whole = sha256::sha256_hex(&msg);
            for chunk_size in [1_usize, 3, 7, 16, 56, 64] {
                let mut h = sha256::Sha256::new();
                for chunk in msg.chunks(chunk_size) {
                    h.update(chunk);
                }
                assert_eq!(
                    h.finalize_hex(),
                    whole,
                    "chunked digest mismatch for len={msg_len} chunk={chunk_size}"
                );
            }
        }
    }

    #[test]
    fn catalog_ids_are_unique_reproducible_and_sha256_pinned() {
        let mut seen = HashSet::new();
        for spec in catalog_specs() {
            assert!(
                !spec.id.is_empty()
                    && spec
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "fixture id must be ASCII kebab-case: {}",
                spec.id
            );
            assert!(seen.insert(spec.id), "duplicate fixture id: {}", spec.id);
            let first = generate(spec.id).expect("generate by id");
            let second = generate(spec.id).expect("regenerate by id");
            assert_eq!(first, second, "non-deterministic fixture: {}", spec.id);
            assert_eq!(first.bytes, first.content.len());
            assert_eq!(first.sha256.len(), 64);
        }
        assert!(generate("does-not-exist").is_none());

        verify_manifest_digests().unwrap_or_else(|msg| panic!("digest drift:\n{msg}"));

        let m1 = manifest();
        let m2 = manifest();
        assert_eq!(m1, m2);
        assert_eq!(m1.entries.len(), catalog_specs().len());
        let json = m1.to_json();
        assert!(json.contains(&m1.manifest_sha256));
        let parsed_manifest =
            ayaml::parse_value(&json).expect("manifest JSON must parse cleanly as YAML 1.2.2");
        assert!(parsed_manifest.is_mapping());
    }

    #[test]
    fn boundary_and_scaling_shapes_hit_expected_size_classes() {
        let b3 = generate("boundary-3kb").expect("3kb");
        let below = generate("boundary-4kb-below").expect("below");
        let above = generate("boundary-4kb-above").expect("above");
        let b8 = generate("boundary-8kb").expect("8kb");
        assert!(b3.bytes > 2_900 && b3.bytes <= 3_072, "b3={}", b3.bytes);
        assert!(
            below.bytes > 3_900 && below.bytes < 4_096,
            "below={}",
            below.bytes
        );
        assert!(
            above.bytes > 4_096 && above.bytes < 4_600,
            "above={}",
            above.bytes
        );
        assert!(b8.bytes > 8_000 && b8.bytes <= 8_192, "b8={}", b8.bytes);

        for (small, med, large) in [
            ("scale-map-10kb", "scale-map-100kb", "scale-map-1mb"),
            ("scale-seq-10kb", "scale-seq-100kb", "scale-seq-1mb"),
            (
                "scale-openapi-10kb",
                "scale-openapi-100kb",
                "scale-openapi-1mb",
            ),
        ] {
            let s = generate(small).expect(small).bytes;
            let m = generate(med).expect(med).bytes;
            let l = generate(large).expect(large).bytes;
            assert!(m >= s * 8 && m <= s * 12, "{small}={s} {med}={m} not ~10x");
            assert!(l >= m * 8 && l <= m * 12, "{med}={m} {large}={l} not ~10x");
        }

        assert_eq!(generate_by_category(FixtureCategory::Domain).len(), 11);
        assert_eq!(generate_by_category(FixtureCategory::Boundary).len(), 4);
        assert_eq!(generate_by_category(FixtureCategory::Scaling).len(), 9);
        assert_eq!(generate_by_category(FixtureCategory::Structural).len(), 11);
        assert_eq!(generate_by_category(FixtureCategory::ErrorPath).len(), 5);
    }

    #[test]
    fn every_catalog_fixture_parses_or_rejects_as_declared() {
        for fixture in generate_all() {
            let docs = ayaml::parse_documents(&fixture.content);
            if fixture.expect_valid {
                let parsed = docs.unwrap_or_else(|err| {
                    panic!("valid fixture '{}' failed to parse: {err}", fixture.id)
                });
                if fixture.multi_document {
                    assert!(
                        parsed.len() > 1,
                        "multi-doc fixture '{}' parsed {} doc(s)",
                        fixture.id,
                        parsed.len()
                    );
                } else {
                    assert_eq!(
                        parsed.len(),
                        1,
                        "single-doc fixture '{}' parsed {} doc(s)",
                        fixture.id,
                        parsed.len()
                    );
                }
                // `SpannedTree::parse` must also succeed on every valid fixture.
                ayaml::SpannedTree::parse(&fixture.content).unwrap_or_else(|err| {
                    panic!(
                        "valid fixture '{}' failed SpannedTree::parse: {err}",
                        fixture.id
                    )
                });
            } else {
                let err = docs.expect_err(&format!(
                    "error-path fixture '{}' unexpectedly parsed cleanly",
                    fixture.id
                ));
                match fixture.id.as_str() {
                    "err-duplicate-key-late" => {
                        assert_eq!(err.code(), ayaml::ErrorCode::DuplicateKey);
                    }
                    "err-depth-limit" => {
                        assert_eq!(err.code(), ayaml::ErrorCode::DepthLimit);
                    }
                    "err-invalid-escape-late" => {
                        assert_eq!(err.code(), ayaml::ErrorCode::InvalidEscape);
                    }
                    _ => {
                        assert!(matches!(
                            err.kind(),
                            ayaml::ErrorKind::Parser | ayaml::ErrorKind::Scanner
                        ));
                    }
                }
            }
        }

        // Verify `container-compose` merge keys (`<<:`) resolve cleanly via `apply_merge_keys`.
        let compose = generate("container-compose").expect("container-compose");
        let mut merged = ayaml::parse_value(&compose.content)
            .expect("container-compose must parse as single value");
        merged
            .apply_merge_keys()
            .expect("container-compose merge keys must resolve cleanly");
        let services = merged
            .as_mapping()
            .and_then(|root| root.get("services"))
            .and_then(ayaml::Value::as_mapping)
            .expect("services mapping");
        let first_service = services
            .iter()
            .next()
            .and_then(|(_, v)| v.as_mapping())
            .expect("first service mapping");
        assert_eq!(
            first_service.get("restart"),
            Some(&ayaml::Value::from("unless-stopped")),
            "merge key should inherit `restart` from `&service_base`"
        );
        assert!(
            first_service.get("<<").is_none(),
            "apply_merge_keys should consume `<<` merge keys"
        );

        // Verify `escape-routes` exercises real decoded double-quoted escapes (`\t`, `\n`, `\u001b`).
        let routes = generate("escape-routes").expect("escape-routes");
        let parsed_routes = ayaml::parse_value(&routes.content).expect("escape-routes parses");
        let trace_header = parsed_routes["static_resources"]["listeners"][0]["routes"][0]["match"]
            ["headers"][0]["value"]
            .as_str()
            .expect("x-trace-pattern value");
        assert!(
            trace_header.contains('\t')
                && trace_header.contains('\n')
                && trace_header.contains('\u{001b}'),
            "expected decoded tab, newline, and ESC in escape-routes: {trace_header:?}"
        );

        // Verify `typed-mismatch-late` is valid YAML yet fails late during typed `u64` map decode.
        let late_typed = generate("typed-mismatch-late").expect("typed-mismatch-late");
        let typed_err =
            ayaml::from_str::<std::collections::BTreeMap<String, u64>>(&late_typed.content)
                .expect_err("typed-mismatch-late must fail typed u64 decoding");
        assert!(
            typed_err.to_string().contains("k_mismatch")
                || typed_err.location().is_some_and(|loc| loc.line() > 3_000),
            "expected late typed error location, got: {typed_err}"
        );

        // Verify bulk small-file generator produces distinct valid playbooks.
        let bulk = bulk_automation_files(32);
        assert_eq!(bulk.len(), 32);
        let digests: HashSet<&str> = bulk.iter().map(|f| f.sha256.as_str()).collect();
        assert_eq!(digests.len(), 32);
        for file in &bulk {
            assert!(
                ayaml::parse_value(&file.content).is_ok(),
                "bulk file '{}' failed to parse",
                file.id
            );
        }
    }
}
