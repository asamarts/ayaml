//! Workspace-only conformance helpers and documentation book doctests.
//!
//! `ayaml-conformance` is not published to crates.io, so doctesting `README.md`
//! and the mdBook pages under `docs/` here keeps every Rust example verified in
//! `cargo test --workspace --all-features` without coupling the packaged `ayaml`
//! `.crate` tarball to repository-level Markdown files.

/// Compile and execute every Rust example in `README.md` and the mdBook under `docs/`.
///
/// Every page in `docs/` (other than `docs/SUMMARY.md`) must be listed here; `tools/check_docs.py`
/// and `crates/ayaml/tests/repository_policy.rs` enforce that no page is omitted.
#[cfg(doctest)]
pub mod book_doctests {
    #[doc = include_str!("../../../README.md")]
    pub struct Readme;

    #[doc = include_str!("../../../docs/index.md")]
    pub struct Index;

    #[doc = include_str!("../../../docs/getting-started.md")]
    pub struct GettingStarted;

    #[doc = include_str!("../../../docs/interface.md")]
    pub struct Interface;

    #[cfg_attr(
        feature = "compat",
        doc = include_str!("../../../docs/compatibility.md")
    )]
    pub struct Compatibility;

    #[doc = include_str!("../../../docs/error-codes.md")]
    pub struct ErrorCodes;

    #[doc = include_str!("../../../docs/security.md")]
    pub struct Security;

    #[doc = include_str!("../../../docs/stability.md")]
    pub struct Stability;

    #[doc = include_str!("../../../docs/limits.md")]
    pub struct Limits;

    #[doc = include_str!("../../../docs/design.md")]
    pub struct Design;

    #[doc = include_str!("../../../docs/dependencies.md")]
    pub struct Dependencies;

    #[doc = include_str!("../../../docs/benchmarks.md")]
    pub struct Benchmarks;

    #[doc = include_str!("../../../docs/decisions.md")]
    pub struct Decisions;

    #[doc = include_str!("../../../docs/issue-priorities.md")]
    pub struct IssuePriorities;
}
