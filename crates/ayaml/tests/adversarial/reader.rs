//! Bounded `from_reader` adversarial tests, executed inside the bounded `in_child!`
//! harness of `crates/ayaml/tests/adversarial.rs`.

use std::error::Error as _;
use std::io::{self, Read};

use ayaml::{ErrorKind, LimitExceeded, LimitKind, Limits, Value};

use super::in_child_named;

struct CountingReader<R> {
    inner: R,
    bytes_read: usize,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes_read += n;
        Ok(n)
    }
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "stream reset",
        ))
    }
}

/// Bounded reader entry points reject infinite and oversized streams after reading at most
/// `max_input_bytes + 1` bytes, and propagate I/O errors cleanly.
#[test]
#[allow(deprecated, reason = "tests deprecated 0.x *_with_limits shims")]
fn from_reader_rejects_infinite_and_oversized_streams_boundedly() {
    in_child!(|| {
        // Default limits bound an infinite stream to max_input_bytes + 1 bytes.
        let mut default_infinite = CountingReader {
            inner: io::repeat(b'a'),
            bytes_read: 0,
        };
        let error =
            ayaml::from_reader::<_, Value>(&mut default_infinite).expect_err("infinite default");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit().map(LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );
        assert_eq!(
            default_infinite.bytes_read,
            Limits::default().max_input_bytes + 1
        );

        // Custom limits bound both infinite and oversized finite streams to max_input_bytes + 1.
        let limits = Limits::new().with_max_input_bytes(1024);
        let mut custom_infinite = CountingReader {
            inner: io::repeat(b'x'),
            bytes_read: 0,
        };
        let error = ayaml::from_reader_with_limits::<_, Value>(&mut custom_infinite, &limits)
            .expect_err("infinite custom");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit().map(LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );
        assert_eq!(custom_infinite.bytes_read, 1025);

        let oversized = vec![b'y'; 4096];
        let mut custom_finite = CountingReader {
            inner: oversized.as_slice(),
            bytes_read: 0,
        };
        let error = ayaml::from_reader_with_limits::<_, Value>(&mut custom_finite, &limits)
            .expect_err("oversized finite");
        assert_eq!(error.kind(), ErrorKind::Limit);
        assert_eq!(
            error.limit().map(LimitExceeded::kind),
            Some(LimitKind::InputBytes)
        );
        assert_eq!(custom_finite.bytes_read, 1025);

        // I/O failures propagate as ErrorKind::Io with their underlying source preserved.
        let error = ayaml::from_reader_with_limits::<_, Value>(FailingReader, &limits)
            .expect_err("io failure");
        assert_eq!(error.kind(), ErrorKind::Io);
        assert_eq!(
            error
                .source()
                .and_then(|source| source.downcast_ref::<io::Error>())
                .map(io::Error::kind),
            Some(io::ErrorKind::ConnectionReset)
        );

        // Alias bombs are rejected through from_reader just like the slice/str readers.
        let bomb = super::alias_bomb();
        let error = ayaml::from_reader::<_, Value>(bomb.as_bytes()).expect_err("alias bomb");
        assert_eq!(error.kind(), ErrorKind::Limit);
    });
}
