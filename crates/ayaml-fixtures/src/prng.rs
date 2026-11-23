//! Deterministic `xorshift64*` generator used by every fixture builder.

use std::fmt::Write as _;

const WORDS: [&str; 16] = [
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa",
    "lambda", "sigma", "omega", "proxy", "worker", "gateway",
];

const ENVIRONMENTS: [&str; 4] = ["dev", "staging", "canary", "prod"];

/// Fixed-seed `xorshift64*` pseudo-random generator for reproducible fixtures.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Rng(u64);

impl Rng {
    /// Construct a generator from a non-zero seed (zero maps to a fixed fallback constant).
    pub(crate) const fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    pub(crate) fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub(crate) fn below(&mut self, bound: usize) -> usize {
        let value = usize::try_from(self.next_u64() >> 33).unwrap_or(0);
        value % bound
    }

    pub(crate) fn word(&mut self) -> &'static str {
        WORDS[self.below(WORDS.len())]
    }

    pub(crate) fn env_name(&mut self) -> &'static str {
        ENVIRONMENTS[self.below(ENVIRONMENTS.len())]
    }

    pub(crate) fn semver(&mut self) -> String {
        format!(
            "{}.{}.{}",
            1 + self.below(5),
            self.below(20),
            self.below(40)
        )
    }

    pub(crate) fn hex_token(&mut self, bytes: usize) -> String {
        let mut out = String::with_capacity(bytes * 2);
        for _ in 0..bytes {
            let byte = self.next_u64().to_le_bytes()[5];
            let _ = write!(out, "{byte:02x}");
        }
        out
    }
}
