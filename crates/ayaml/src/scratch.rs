//! Reusable opaque parser scratch buffer ([`ParserBuf`]).

use std::fmt;

use serde::Deserialize;
use serde::de::DeserializeSeed;

use crate::composer::ComposerScratch;
use crate::error::Error;
use crate::options::ParseOptions;
use crate::parser::{Cont, WindowScratch};
use crate::tape::{RecycledTape, Tape, TapeDocuments, TapeScratch, TapeStorage};
use crate::value::Value;

/// Pure capacity retention predicate verified symbolically by Kani.
#[inline]
pub(crate) const fn clamp_retained_capacity(capacity: usize, max_retain: usize) -> bool {
    capacity <= max_retain
}

/// Reusable opaque scratch buffer for high-throughput bulk YAML parsing.
///
/// Reusing one `ParserBuf` across repeated parses eliminates internal scanner,
/// line-window, continuation-stack, duplicate-key, and composer scratch
/// allocations after the first warm-up document.
///
/// After every parse (whether it succeeds, fails with an [`Error`], or unwinds
/// from a custom [`serde::Deserialize`] implementation), internal buffers are
/// cleared and any capacity grown past an internal retention ceiling (for
/// example by a single large outlier document) is automatically released so a
/// long-lived worker buffer does not pin unbounded memory.
///
/// Pairing [`ParserBuf::parse_tape`] with [`Tape::recycle_into`] additionally
/// recycles the tape's own backing buffers for zero-allocation steady-state
/// bulk loops.
///
/// ```
/// use ayaml::ParserBuf;
///
/// let mut buf = ParserBuf::new();
/// for doc in ["a: 1\n", "a: 2\n", "a: 3\n"] {
///     let tape = buf.parse_tape(doc)?;
///     assert!(tape.get("a").and_then(|n| n.as_u64()).is_some());
///     tape.recycle_into(&mut buf);
/// }
/// # Ok::<(), ayaml::Error>(())
/// ```
#[derive(Default)]
pub struct ParserBuf {
    window: WindowScratch,
    cont_overflow: Vec<Cont>,
    composer: ComposerScratch,
    tape: TapeScratch,
    recycled_tape: RecycledTape,
    stream: crate::typed::StreamScratch,
}

impl fmt::Debug for ParserBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParserBuf").finish_non_exhaustive()
    }
}

impl ParserBuf {
    /// Create a new empty `ParserBuf` with zero initial heap allocations.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Release all retained scratch capacity back to the global allocator.
    pub fn shrink_to_fit(&mut self) {
        self.window.release_all();
        self.cont_overflow = Vec::new();
        self.composer.release_all();
        self.tape.release_all();
        self.recycled_tape.release_all();
        self.stream.release_all();
    }

    /// Parse a single YAML document into a [`Tape`], reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_tape`].
    pub fn parse_tape<'a>(&mut self, input: &'a str) -> Result<Tape<'a>, Error> {
        Tape::parse_in(input, self)
    }

    /// Parse a single YAML document into a [`Tape`] with explicit options,
    /// reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_tape_with`].
    pub fn parse_tape_with<'a>(
        &mut self,
        input: &'a str,
        options: &ParseOptions,
    ) -> Result<Tape<'a>, Error> {
        Tape::parse_with_in(input, options, self)
    }

    /// Parse every YAML document in a stream into [`TapeDocuments`], reusing
    /// internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_tape_documents`].
    pub fn parse_tape_documents<'a>(&mut self, input: &'a str) -> Result<TapeDocuments<'a>, Error> {
        TapeDocuments::parse_in(input, self)
    }

    /// Parse every YAML document in a stream into [`TapeDocuments`] with
    /// explicit options, reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_tape_documents_with`].
    pub fn parse_tape_documents_with<'a>(
        &mut self,
        input: &'a str,
        options: &ParseOptions,
    ) -> Result<TapeDocuments<'a>, Error> {
        TapeDocuments::parse_with_in(input, options, self)
    }

    /// Parse a single YAML document into a [`Value`], reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_value`].
    pub fn parse_value(&mut self, input: &str) -> Result<Value, Error> {
        crate::parse_value_in(input, self)
    }

    /// Parse a single YAML document into a [`Value`] with explicit options,
    /// reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_value_with`].
    pub fn parse_value_with(
        &mut self,
        input: &str,
        options: &ParseOptions,
    ) -> Result<Value, Error> {
        crate::parse_value_with_in(input, options, self)
    }

    /// Parse every YAML document in a stream into `Vec<Value>`, reusing
    /// internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_documents`].
    pub fn parse_documents(&mut self, input: &str) -> Result<Vec<Value>, Error> {
        crate::parse_documents_in(input, self)
    }

    /// Parse every YAML document in a stream into `Vec<Value>` with explicit
    /// options, reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::parse_documents_with`].
    pub fn parse_documents_with(
        &mut self,
        input: &str,
        options: &ParseOptions,
    ) -> Result<Vec<Value>, Error> {
        crate::parse_documents_with_in(input, options, self)
    }

    /// Deserialize a single YAML document into `T`, reusing internal scratch buffers.
    ///
    /// Accepts both owned types (`DeserializeOwned`) and borrowed types
    /// (`Deserialize<'de>`).
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::from_str`].
    pub fn from_str<'de, T: Deserialize<'de>>(&mut self, input: &'de str) -> Result<T, Error> {
        crate::from_str_in(input, self)
    }

    /// Deserialize a single YAML document into `T` with explicit options,
    /// reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::from_str_with`].
    pub fn from_str_with<'de, T: Deserialize<'de>>(
        &mut self,
        input: &'de str,
        options: &ParseOptions,
    ) -> Result<T, Error> {
        crate::from_str_with_in(input, options, self)
    }

    /// Deserialize a single YAML document using a stateful [`DeserializeSeed`],
    /// reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::from_str_seed`].
    pub fn from_str_seed<'de, T: DeserializeSeed<'de>>(
        &mut self,
        input: &'de str,
        seed: T,
    ) -> Result<T::Value, Error> {
        crate::from_str_seed_in(input, seed, self)
    }

    /// Deserialize a single YAML document using a stateful [`DeserializeSeed`]
    /// and explicit options, reusing internal scratch buffers.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`crate::from_str_seed_with`].
    pub fn from_str_seed_with<'de, T: DeserializeSeed<'de>>(
        &mut self,
        input: &'de str,
        options: &ParseOptions,
        seed: T,
    ) -> Result<T::Value, Error> {
        crate::from_str_seed_with_in(input, options, seed, self)
    }

    pub(crate) fn take_window_scratch(&mut self) -> WindowScratch {
        std::mem::take(&mut self.window)
    }

    pub(crate) fn put_window_scratch(&mut self, mut scratch: WindowScratch) {
        scratch.clear_and_clamp();
        self.window = scratch;
    }

    pub(crate) fn take_cont_overflow(&mut self) -> Vec<Cont> {
        std::mem::take(&mut self.cont_overflow)
    }

    pub(crate) fn put_cont_overflow(&mut self, mut overflow: Vec<Cont>) {
        overflow.clear();
        if clamp_retained_capacity(overflow.capacity(), 256) {
            self.cont_overflow = overflow;
        } else {
            self.cont_overflow = Vec::new();
        }
    }

    pub(crate) fn take_composer_scratch(&mut self) -> ComposerScratch {
        std::mem::take(&mut self.composer)
    }

    pub(crate) fn put_composer_scratch(&mut self, mut scratch: ComposerScratch) {
        scratch.clear_and_clamp();
        self.composer = scratch;
    }

    pub(crate) fn take_tape_scratch(&mut self) -> TapeScratch {
        std::mem::take(&mut self.tape)
    }

    pub(crate) fn put_tape_scratch(&mut self, mut scratch: TapeScratch) {
        scratch.clear_and_clamp();
        self.tape = scratch;
    }

    pub(crate) fn take_recycled_tape(&mut self) -> RecycledTape {
        std::mem::take(&mut self.recycled_tape)
    }

    pub(crate) fn recycle_storage(&mut self, storage: TapeStorage<'_>, roots: Vec<u32>) {
        self.recycled_tape.accept_storage(storage, roots);
    }

    pub(crate) fn take_stream_scratch(&mut self) -> crate::typed::StreamScratch {
        std::mem::take(&mut self.stream)
    }

    pub(crate) fn put_stream_scratch(&mut self, mut scratch: crate::typed::StreamScratch) {
        scratch.clear_and_clamp();
        self.stream = scratch;
    }

    /// Expose retained capacity summary for white-box retention-clamping tests.
    #[cfg(test)]
    pub(crate) fn retained_capacities(&self) -> (usize, usize, usize, usize) {
        (
            self.window.tokens.capacity(),
            self.window.lines.capacity(),
            self.recycled_tape.nodes.capacity(),
            self.recycled_tape.decoded.capacity(),
        )
    }
}
