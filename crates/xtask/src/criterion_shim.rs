//! Counting stand-in for the subset of `criterion` used by `ayaml-bench`.
//!
//! Embedded by `crates/xtask/src/det.rs` and written to a temporary crate patched over
//! `crates.io`'s `criterion` during `cargo xtask det` and `cargo xtask profile`. This lets
//! Valgrind Callgrind and DHAT run directly against the standard Criterion `ayaml-bench`
//! benchmarks while keeping setup and fixture generation completely outside the measured region.
#![allow(
    clippy::all,
    clippy::pedantic,
    missing_docs,
    missing_debug_implementations,
    dead_code,
    unused_variables,
    reason = "standalone throwaway Criterion shim compiled only under cargo xtask det/profile"
)]

use std::borrow::Cow;
use std::fmt::{self, Display};
use std::marker::PhantomData;
use std::time::Duration;

pub use std::hint::black_box;

pub mod measurement {
    #[derive(Clone, Copy, Debug, Default)]
    pub struct WallTime;
}

/// Non-inlined measurement boundary targeted by Callgrind `--toggle-collect=*__ayaml_det_measure*`
/// and by DHAT stack frame scoping. Dropping `black_box(routine())` inside this boundary ensures
/// full parse+drop cost is accounted for, matching Criterion's `Bencher::iter`.
#[inline(never)]
pub fn __ayaml_det_measure<O, R: FnMut() -> O>(routine: &mut R) {
    drop(black_box(routine()));
}

#[derive(Clone)]
pub struct Criterion {
    filter: Option<String>,
    k: usize,
    warmup: usize,
    list: bool,
}

impl Default for Criterion {
    fn default() -> Self {
        let list = std::env::var_os("AYAML_DET_LIST").is_some();
        Self {
            filter: std::env::var("AYAML_DET_FILTER").ok(),
            k: std::env::var("AYAML_DET_K")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(1),
            warmup: std::env::var("AYAML_DET_WARMUP")
                .ok()
                .and_then(|raw| raw.parse().ok())
                .unwrap_or(0),
            list,
        }
    }
}

impl Criterion {
    pub fn benchmark_group<S: Into<String>>(
        &mut self,
        name: S,
    ) -> BenchmarkGroup<'_, measurement::WallTime> {
        BenchmarkGroup {
            c: self,
            name: name.into(),
            _m: PhantomData,
        }
    }

    pub fn bench_function<ID: IntoBenchmarkId, F: FnMut(&mut Bencher)>(
        &mut self,
        id: ID,
        mut f: F,
    ) -> &mut Self {
        let full = id.into_id();
        self.run(&full, |b| f(b));
        self
    }

    pub fn bench_with_input<ID: IntoBenchmarkId, I: ?Sized, F: FnMut(&mut Bencher, &I)>(
        &mut self,
        id: ID,
        input: &I,
        mut f: F,
    ) -> &mut Self {
        let full = id.into_id();
        self.run(&full, |b| f(b, input));
        self
    }

    pub fn sample_size(self, _: usize) -> Self {
        self
    }
    pub fn measurement_time(self, _: Duration) -> Self {
        self
    }
    pub fn warm_up_time(self, _: Duration) -> Self {
        self
    }
    pub fn noise_threshold(self, _: f64) -> Self {
        self
    }
    pub fn significance_level(self, _: f64) -> Self {
        self
    }
    pub fn confidence_level(self, _: f64) -> Self {
        self
    }
    pub fn nresamples(self, _: usize) -> Self {
        self
    }
    pub fn without_plots(self) -> Self {
        self
    }
    pub fn configure_from_args(self) -> Self {
        self
    }
    pub fn final_summary(&mut self) {}

    fn run(&self, full: &str, mut f: impl FnMut(&mut Bencher)) {
        if self.list {
            println!("AYAML_DET_ID\t{full}");
            return;
        }
        if self.filter.as_deref() != Some(full) {
            return;
        }
        let mut b = Bencher {
            k: self.k,
            warmup: self.warmup,
        };
        f(&mut b);
    }
}

pub struct BenchmarkGroup<'a, M = measurement::WallTime> {
    c: &'a mut Criterion,
    name: String,
    _m: PhantomData<M>,
}

impl<M> BenchmarkGroup<'_, M> {
    pub fn throughput(&mut self, _: Throughput) -> &mut Self {
        self
    }
    pub fn sample_size(&mut self, _: usize) -> &mut Self {
        self
    }
    pub fn measurement_time(&mut self, _: Duration) -> &mut Self {
        self
    }
    pub fn warm_up_time(&mut self, _: Duration) -> &mut Self {
        self
    }
    pub fn noise_threshold(&mut self, _: f64) -> &mut Self {
        self
    }
    pub fn sampling_mode(&mut self, _: SamplingMode) -> &mut Self {
        self
    }

    pub fn bench_with_input<ID: IntoBenchmarkId, I: ?Sized, F: FnMut(&mut Bencher, &I)>(
        &mut self,
        id: ID,
        input: &I,
        mut f: F,
    ) -> &mut Self {
        let full = format!("{}/{}", self.name, id.into_id());
        self.c.run(&full, |b| f(b, input));
        self
    }

    pub fn bench_function<ID: IntoBenchmarkId, F: FnMut(&mut Bencher)>(
        &mut self,
        id: ID,
        mut f: F,
    ) -> &mut Self {
        let full = format!("{}/{}", self.name, id.into_id());
        self.c.run(&full, |b| f(b));
        self
    }

    pub fn finish(self) {}
}

pub struct Bencher {
    k: usize,
    warmup: usize,
}

impl Bencher {
    pub fn iter<O, R: FnMut() -> O>(&mut self, mut routine: R) {
        for _ in 0..self.warmup {
            drop(black_box(routine()));
        }
        for _ in 0..self.k {
            __ayaml_det_measure(&mut routine);
        }
    }

    pub fn iter_batched<I, O, S: FnMut() -> I, R: FnMut(I) -> O>(
        &mut self,
        mut setup: S,
        mut routine: R,
        _: BatchSize,
    ) {
        for _ in 0..self.warmup {
            let input = setup();
            drop(black_box(routine(input)));
        }
        for _ in 0..self.k {
            let input = setup();
            let mut slot = Some(input);
            let mut step = || {
                let value = slot.take().expect("batch input present");
                routine(value)
            };
            __ayaml_det_measure(&mut step);
        }
    }

    pub fn iter_batched_ref<I, O, S: FnMut() -> I, R: FnMut(&mut I) -> O>(
        &mut self,
        mut setup: S,
        mut routine: R,
        _: BatchSize,
    ) {
        for _ in 0..self.warmup {
            let mut input = setup();
            drop(black_box(routine(&mut input)));
        }
        for _ in 0..self.k {
            let mut input = setup();
            let mut step = || routine(&mut input);
            __ayaml_det_measure(&mut step);
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Throughput {
    Bytes(u64),
    BytesDecimal(u64),
    Elements(u64),
}

#[derive(Clone, Copy, Debug)]
pub enum BatchSize {
    SmallInput,
    LargeInput,
    PerIteration,
    NumBatches(u64),
    NumIterations(u64),
}

#[derive(Clone, Copy, Debug)]
pub enum SamplingMode {
    Auto,
    Linear,
    Flat,
}

#[derive(Clone, Debug)]
pub struct BenchmarkId {
    id: String,
}

impl BenchmarkId {
    pub fn new<S: Into<String>, P: Display>(function_name: S, parameter: P) -> Self {
        Self {
            id: format!("{}/{}", function_name.into(), parameter),
        }
    }

    pub fn from_parameter<P: Display>(parameter: P) -> Self {
        Self {
            id: parameter.to_string(),
        }
    }
}

impl Display for BenchmarkId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.id)
    }
}

pub trait IntoBenchmarkId {
    fn into_id(self) -> String;
}

impl IntoBenchmarkId for BenchmarkId {
    fn into_id(self) -> String {
        self.id
    }
}

impl IntoBenchmarkId for &BenchmarkId {
    fn into_id(self) -> String {
        self.id.clone()
    }
}

impl IntoBenchmarkId for &str {
    fn into_id(self) -> String {
        self.to_owned()
    }
}

impl IntoBenchmarkId for String {
    fn into_id(self) -> String {
        self
    }
}

impl IntoBenchmarkId for &String {
    fn into_id(self) -> String {
        self.clone()
    }
}

impl IntoBenchmarkId for Cow<'_, str> {
    fn into_id(self) -> String {
        self.into_owned()
    }
}

#[macro_export]
macro_rules! criterion_group {
    (name = $name:ident; config = $config:expr; targets = $($target:path),+ $(,)?) => {
        pub fn $name() {
            let mut criterion = $config;
            $( $target(&mut criterion); )+
        }
    };
    ($name:ident, $($target:path),+ $(,)?) => {
        pub fn $name() {
            let mut criterion = $crate::Criterion::default().configure_from_args();
            $( $target(&mut criterion); )+
        }
    };
}

#[macro_export]
macro_rules! criterion_main {
    ($($group:path),+ $(,)?) => {
        fn main() {
            $( $group(); )+
        }
    };
}
