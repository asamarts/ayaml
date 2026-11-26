//! Frontmatter and prefix-document splitting and parsing.
//!
//! Documentation tools, task trackers, and static site generators commonly embed a leading
//! YAML document delimited by `---` and `---` (or `...`) ahead of arbitrary non-YAML body text.
//! This module splits and parses that leading YAML header while preserving exact byte offsets
//! into the original input for [`LineIndex`](crate::LineIndex), [`Span`], and
//! [`SpannedTree`](crate::SpannedTree).

use crate::error::{Error, ErrorCode, ErrorKind};
use crate::limits::Limits;
use crate::options::ParseOptions;
use crate::span::{Location, Span};
use crate::value::Value;

const UTF8_BOM: &str = "\u{feff}";

/// The closing delimiter that terminated a frontmatter block.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrontmatterDelimiter {
    /// Closed by a `---` document-start marker line.
    DocumentStart,
    /// Closed by a `...` document-end marker line.
    DocumentEnd,
}

/// A leading YAML frontmatter block split from the remaining non-YAML body.
///
/// [`yaml`](Self::yaml) always starts at byte offset `0` of the original input (preserving any
/// leading UTF-8 BOM and opening `---` line), so every [`Span`] produced when parsing it with
/// [`parse_events`](crate::parse_events), [`SpannedTree::parse`](crate::SpannedTree::parse), or
/// any error aligns directly with [`LineIndex::new(input)`](crate::LineIndex::new).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Frontmatter<'a> {
    yaml: &'a str,
    yaml_span: Span,
    closing_delimiter: FrontmatterDelimiter,
    delimiter_span: Span,
    body: &'a str,
    body_offset: usize,
}

impl<'a> Frontmatter<'a> {
    /// The leading YAML frontmatter slice (`&input[..yaml_span.end()]`), starting at byte `0`
    /// of the original input and ending immediately before the closing delimiter line.
    #[must_use]
    pub const fn yaml(&self) -> &'a str {
        self.yaml
    }

    /// The byte span of [`yaml`](Self::yaml) in the original input (`0..closing_start`).
    #[must_use]
    pub const fn yaml_span(&self) -> Span {
        self.yaml_span
    }

    /// Which delimiter (`---` or `...`) closed the frontmatter block.
    #[must_use]
    pub const fn closing_delimiter(&self) -> FrontmatterDelimiter {
        self.closing_delimiter
    }

    /// The 3-byte span of the closing `---` or `...` delimiter in the original input.
    #[must_use]
    pub const fn delimiter_span(&self) -> Span {
        self.delimiter_span
    }

    /// The remaining non-YAML body slice immediately after the closing delimiter line.
    #[must_use]
    pub const fn body(&self) -> &'a str {
        self.body
    }

    /// Exact byte offset of [`body`](Self::body) in the original input.
    ///
    /// `&input[frontmatter.body_offset()..]` is always identical to `frontmatter.body()`.
    #[must_use]
    pub const fn body_offset(&self) -> usize {
        self.body_offset
    }
}

/// A decoded leading YAML frontmatter value paired with the remaining non-YAML body slice.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct ParsedFrontmatter<'a, T> {
    /// The decoded frontmatter value.
    pub frontmatter: T,
    /// The remaining non-YAML body slice after the closing delimiter line.
    pub body: &'a str,
    /// Exact byte offset of `body` in the original input.
    pub body_offset: usize,
    /// Byte span of the YAML frontmatter slice (`0..closing_start`) in the original input.
    pub yaml_span: Span,
    /// Which delimiter (`---` or `...`) closed the frontmatter block.
    pub closing_delimiter: FrontmatterDelimiter,
    /// The 3-byte span of the closing `---` or `...` delimiter in the original input.
    pub delimiter_span: Span,
}

/// Split a leading YAML frontmatter block from `input` under the given limits.
///
/// Returns `Ok(None)` when `input` does not begin with a standalone `---` opening line
/// (optionally preceded by a UTF-8 BOM). Returns `Err` when `limits` is invalid, when the
/// frontmatter exceeds `limits.max_input_bytes`, or when an opening `---` line has no matching
/// closing `---` or `...` line before end of input.
pub(crate) fn split_frontmatter_with_limits<'a>(
    input: &'a str,
    limits: &Limits,
) -> Result<Option<Frontmatter<'a>>, Error> {
    limits.validate()?;

    let bom_len = if input.starts_with(UTF8_BOM) {
        UTF8_BOM.len()
    } else {
        0
    };
    let after_bom = &input[bom_len..];

    let max_bytes = limits.max_input_bytes;
    let Some(open_line) =
        match_delimiter_line(after_bom, true, Some(max_bytes.saturating_sub(bom_len)))
    else {
        return Ok(None);
    };
    if open_line.exceeded_limit {
        return Err(input_limit_error(limits, max_bytes.saturating_add(1)));
    }
    let open_span = Span::new(bom_len, bom_len + 3);
    let mut pos = bom_len + open_line.next_offset;

    if !open_line.has_newline {
        // An opening `---` at EOF with no closing delimiter is unclosed frontmatter.
        limits.check_input(input.len()).map_err(limit_at_start)?;
        return Err(missing_closing_delimiter(input, open_span));
    }

    while pos <= input.len() {
        limits.check_input(pos).map_err(limit_at_start)?;
        if pos == input.len() {
            break;
        }
        let rest = &input[pos..];
        let remaining = max_bytes - pos;
        if let Some(close) = match_delimiter_line(rest, false, Some(remaining)) {
            if close.exceeded_limit {
                return Err(input_limit_error(
                    limits,
                    pos.saturating_add(close.next_offset),
                ));
            }
            let body_offset = pos + close.next_offset;
            return Ok(Some(Frontmatter {
                yaml: &input[..pos],
                yaml_span: Span::new(0, pos),
                closing_delimiter: close.kind,
                delimiter_span: Span::new(pos, pos + 3),
                body: &input[body_offset..],
                body_offset,
            }));
        }
        let Some(step) = next_line_len_bounded(rest, remaining) else {
            return Err(input_limit_error(
                limits,
                pos.saturating_add(remaining).saturating_add(1),
            ));
        };
        pos += step;
    }

    Err(missing_closing_delimiter(input, open_span))
}

/// Parse a leading YAML frontmatter block into a typed Serde value.
pub(crate) fn parse_frontmatter_with<'de, T: serde::Deserialize<'de>>(
    input: &'de str,
    options: &ParseOptions,
) -> Result<Option<ParsedFrontmatter<'de, T>>, Error> {
    let Some(split) = split_frontmatter_with_limits(input, &options.limits)? else {
        return Ok(None);
    };
    let frontmatter = crate::from_str_with(split.yaml(), options)?;
    Ok(Some(ParsedFrontmatter {
        frontmatter,
        body: split.body(),
        body_offset: split.body_offset(),
        yaml_span: split.yaml_span(),
        closing_delimiter: split.closing_delimiter(),
        delimiter_span: split.delimiter_span(),
    }))
}

/// Parse a leading YAML frontmatter block into a generic [`Value`].
pub(crate) fn parse_frontmatter_value_with<'a>(
    input: &'a str,
    options: &ParseOptions,
) -> Result<Option<ParsedFrontmatter<'a, Value>>, Error> {
    let Some(split) = split_frontmatter_with_limits(input, &options.limits)? else {
        return Ok(None);
    };
    let frontmatter = crate::parse_value_with(split.yaml(), options)?;
    Ok(Some(ParsedFrontmatter {
        frontmatter,
        body: split.body(),
        body_offset: split.body_offset(),
        yaml_span: split.yaml_span(),
        closing_delimiter: split.closing_delimiter(),
        delimiter_span: split.delimiter_span(),
    }))
}

struct MatchedDelimiter {
    kind: FrontmatterDelimiter,
    next_offset: usize,
    has_newline: bool,
    exceeded_limit: bool,
}

/// Match a standalone `---` (or `...` when `opening_only` is false) line at the start of `s`.
///
/// Only optional horizontal whitespace (`' '` or `'\t'`) and an optional `# ...` comment (when
/// preceded by whitespace) are permitted before `\r\n`, `\n`, or EOF. When `max_len` is `Some`,
/// scanning stops once the line length exceeds `max_len` bytes and reports `exceeded_limit`.
fn match_delimiter_line(
    s: &str,
    opening_only: bool,
    max_len: Option<usize>,
) -> Option<MatchedDelimiter> {
    let bytes = s.as_bytes();
    let kind = if bytes.starts_with(b"---") {
        FrontmatterDelimiter::DocumentStart
    } else if !opening_only && bytes.starts_with(b"...") {
        FrontmatterDelimiter::DocumentEnd
    } else {
        return None;
    };
    if let Some(&next) = bytes.get(3)
        && !matches!(next, b' ' | b'\t' | b'\n' | b'\r')
    {
        return None;
    }

    let limit = max_len.unwrap_or(bytes.len()).min(bytes.len());
    let mut i = 3;
    let mut saw_whitespace = false;
    let mut saw_comment = false;
    while i < bytes.len() {
        if max_len.is_some() && i > limit {
            return Some(MatchedDelimiter {
                kind,
                next_offset: i,
                has_newline: false,
                exceeded_limit: true,
            });
        }
        match bytes[i] {
            b' ' | b'\t' => {
                saw_whitespace = true;
                i += 1;
            }
            b'#' if saw_whitespace => {
                saw_comment = true;
                break;
            }
            b'\n' | b'\r' => break,
            _ => return None,
        }
    }
    if saw_comment {
        // Skip comment through end of line (bounded when max_len is set).
        while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
            if max_len.is_some() && i > limit {
                // Advance past at most 3 UTF-8 continuation bytes so `next_offset` is
                // always a valid UTF-8 character boundary into `s`.
                if (bytes[i] & 0b1100_0000) == 0b1000_0000 {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
                    i += 1;
                }
                if i < bytes.len() && (bytes[i] & 0b1100_0000) == 0b1000_0000 {
                    i += 1;
                }
                return Some(MatchedDelimiter {
                    kind,
                    next_offset: i,
                    has_newline: false,
                    exceeded_limit: true,
                });
            }
            i += 1;
        }
    }

    if i == bytes.len() {
        let exceeded_limit = max_len.is_some_and(|max| i > max);
        return Some(MatchedDelimiter {
            kind,
            next_offset: i,
            has_newline: false,
            exceeded_limit,
        });
    }
    if bytes[i] == b'\n' {
        let next_offset = i + 1;
        let exceeded_limit = max_len.is_some_and(|max| next_offset > max);
        return Some(MatchedDelimiter {
            kind,
            next_offset,
            has_newline: true,
            exceeded_limit,
        });
    }
    if bytes[i] == b'\r' {
        let step = if bytes.get(i + 1) == Some(&b'\n') {
            2
        } else {
            1
        };
        let next_offset = i + step;
        let exceeded_limit = max_len.is_some_and(|max| next_offset > max);
        return Some(MatchedDelimiter {
            kind,
            next_offset,
            has_newline: true,
            exceeded_limit,
        });
    }
    None
}

fn limit_at_start(error: crate::limits::LimitExceeded) -> Error {
    Error::from(error)
        .with_span(Span::empty(0))
        .with_location(Location::new(1, 1))
}

fn input_limit_error(limits: &Limits, actual: usize) -> Error {
    match limits.check_input(actual) {
        Err(error) => limit_at_start(error),
        Ok(()) => Error::coded(
            ErrorKind::Limit,
            ErrorCode::InputSizeLimit,
            "input exceeds max_input_bytes",
        )
        .with_span(Span::empty(0))
        .with_location(Location::new(1, 1)),
    }
}

/// Byte length of the first line of `s` (including its `\r\n`, `\n`, or `\r`), bounded to at most
/// `max_len` bytes.
///
/// Returns `None` if the line exceeds `max_len` bytes without reaching a line break or EOF.
fn next_line_len_bounded(s: &str, max_len: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let limit = bytes.len().min(max_len);
    let mut i = 0;
    while i < limit {
        if bytes[i] == b'\n' {
            return Some(i + 1);
        }
        if bytes[i] == b'\r' {
            if bytes.get(i + 1) == Some(&b'\n') {
                return (i + 2 <= max_len).then_some(i + 2);
            }
            return Some(i + 1);
        }
        i += 1;
    }
    if bytes.len() <= max_len {
        Some(bytes.len())
    } else {
        None
    }
}

fn missing_closing_delimiter(input: &str, open_span: Span) -> Error {
    let prefix = &input[..open_span.end()];
    Error::coded(
        ErrorKind::Parser,
        ErrorCode::UnexpectedToken,
        "unterminated frontmatter: expected closing `---` or `...` delimiter",
    )
    .with_span(open_span)
    .with_input_location(prefix, false)
}

#[cfg(kani)]
mod kani_proofs {
    use super::{FrontmatterDelimiter, match_delimiter_line, next_line_len_bounded};

    #[kani::proof]
    #[kani::unwind(8)]
    fn verify_next_line_len_bounded_invariants() {
        // Constant-offset symbolic fill with an optional 2-byte UTF-8 `'é'` (`0xC3, 0xA9`)
        // at any valid interior position so `is_char_boundary` is checked non-vacuously.
        let mut buf = [0_u8; 6];
        for slot in &mut buf {
            *slot = match kani::any::<u8>() & 3 {
                0 => b'a',
                1 => b'\n',
                2 => b'\r',
                _ => b'-',
            };
        }
        let len: usize = kani::any();
        kani::assume(len <= buf.len());
        if kani::any::<bool>() && len >= 2 {
            let mb_pos: usize = kani::any();
            kani::assume(mb_pos <= len - 2);
            buf[mb_pos] = 0xC3;
            buf[mb_pos + 1] = 0xA9;
        }
        let max_len: usize = kani::any();
        let Ok(s) = std::str::from_utf8(&buf[..len]) else {
            return;
        };

        match next_line_len_bounded(s, max_len) {
            Some(step) => {
                assert!(step <= len);
                assert!(step <= max_len);
                assert!(s.is_char_boundary(step));
                if len > 0 {
                    assert!(step > 0);
                }
                // Never split a `\r\n` line ending.
                if step > 0 && step < len {
                    assert!(!(buf[step - 1] == b'\r' && buf[step] == b'\n'));
                }
            }
            None => {
                assert!(len > max_len);
                // No complete line ending can finish within `max_len` bytes.
                let mut i = 0;
                while i < max_len && i < len {
                    assert!(buf[i] != b'\n');
                    if buf[i] == b'\r' {
                        assert!(i + 1 < len && buf[i + 1] == b'\n' && i + 2 > max_len);
                    }
                    i += 1;
                }
            }
        }
    }

    #[kani::proof]
    #[kani::unwind(9)]
    fn verify_match_delimiter_line_bounds() {
        // Constant-offset symbolic buffer up to 7 bytes with optional 2-byte UTF-8 `'é'`
        // (`0xC3, 0xA9`) placed at any valid position >= 3, covering `--- #é`, `----\n`,
        // `...\t#é`, and arbitrary non-delimiter prefixes without nested loop blowup.
        let mut buf = [0_u8; 7];
        match kani::any::<u8>() & 3 {
            0 => buf[0..3].copy_from_slice(b"---"),
            1 => buf[0..3].copy_from_slice(b"..."),
            2 => buf[0..3].copy_from_slice(b"--x"),
            _ => buf[0..3].copy_from_slice(b"xyz"),
        }
        for slot in &mut buf[3..] {
            *slot = match kani::any::<u8>() & 7 {
                0 => b'-',
                1 => b'.',
                2 => b' ',
                3 => b'\t',
                4 => b'#',
                5 => b'\r',
                6 => b'\n',
                _ => b'x',
            };
        }
        let len: usize = kani::any();
        kani::assume(len <= buf.len());
        if kani::any::<bool>() && len >= 5 {
            let mb_pos: usize = kani::any();
            kani::assume((3..=len - 2).contains(&mb_pos));
            buf[mb_pos] = 0xC3;
            buf[mb_pos + 1] = 0xA9;
        }
        let opening_only: bool = kani::any();
        let max_len: Option<usize> = if kani::any() {
            let max: usize = kani::any();
            kani::assume(max <= buf.len() + 2 || max == usize::MAX);
            Some(max)
        } else {
            None
        };
        let Ok(s) = std::str::from_utf8(&buf[..len]) else {
            return;
        };

        if let Some(matched) = match_delimiter_line(s, opening_only, max_len) {
            if opening_only {
                assert_eq!(matched.kind, FrontmatterDelimiter::DocumentStart);
            }
            assert!(matched.next_offset <= len);
            assert!(s.is_char_boundary(matched.next_offset));
            if len > 3 {
                assert!(matches!(buf[3], b' ' | b'\t' | b'\n' | b'\r'));
            }
            if !matched.exceeded_limit {
                assert!(matched.next_offset >= 3);
                if let Some(max) = max_len {
                    assert!(matched.next_offset <= max);
                }
                if matched.has_newline {
                    assert!(matched.next_offset >= 4);
                    if matched.next_offset < len {
                        assert!(
                            !(buf[matched.next_offset - 1] == b'\r'
                                && buf[matched.next_offset] == b'\n')
                        );
                    }
                } else {
                    assert_eq!(matched.next_offset, len);
                }
            } else {
                assert!(max_len.is_some());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::match_delimiter_line;

    #[test]
    fn match_delimiter_line_keeps_utf8_char_boundary_on_over_limit_comment() {
        let input = "--- #é\n";
        let matched = match_delimiter_line(input, true, Some(5)).expect("delimiter match");
        assert!(matched.exceeded_limit);
        assert!(input.is_char_boundary(matched.next_offset));
        assert_eq!(matched.next_offset, 7);

        // 4-byte UTF-8 character (`🦀`) at every interior continuation offset.
        let emoji = "--- #🦀\n";
        for max in [5, 6, 7] {
            let m = match_delimiter_line(emoji, true, Some(max)).expect("delimiter match");
            assert!(m.exceeded_limit);
            assert!(emoji.is_char_boundary(m.next_offset), "max={max}");
            assert_eq!(m.next_offset, 9, "max={max}");
        }
    }

    #[test]
    fn match_delimiter_line_rejects_four_hyphens_even_under_tiny_limit() {
        assert!(match_delimiter_line("----\nbody\n", true, Some(2)).is_none());
        assert!(match_delimiter_line("...x\nbody\n", false, Some(2)).is_none());
        assert!(match_delimiter_line("---#comment\n", true, Some(2)).is_none());
    }
}
