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
use crate::position::LineStarts;
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
        if let Some(close) = match_delimiter_line(rest, false, None) {
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
        let remaining = max_bytes - pos;
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

    let limit = max_len.unwrap_or(bytes.len()).min(bytes.len());
    let mut i = 3;
    let mut saw_whitespace = false;
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
                // Skip comment through end of line (bounded when max_len is set).
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    if max_len.is_some() && i > limit {
                        return Some(MatchedDelimiter {
                            kind,
                            next_offset: i,
                            has_newline: false,
                            exceeded_limit: true,
                        });
                    }
                    i += 1;
                }
                break;
            }
            b'\n' | b'\r' => break,
            _ => return None,
        }
    }

    if i == bytes.len() {
        return Some(MatchedDelimiter {
            kind,
            next_offset: i,
            has_newline: false,
            exceeded_limit: false,
        });
    }
    if bytes[i] == b'\n' {
        return Some(MatchedDelimiter {
            kind,
            next_offset: i + 1,
            has_newline: true,
            exceeded_limit: false,
        });
    }
    if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
        return Some(MatchedDelimiter {
            kind,
            next_offset: i + 2,
            has_newline: true,
            exceeded_limit: false,
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

/// Byte length of the first line of `s` (including its `\n`), bounded to at most `max_len` bytes.
///
/// Returns `None` if the line exceeds `max_len` bytes without reaching `\n` or EOF.
fn next_line_len_bounded(s: &str, max_len: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let limit = bytes.len().min(max_len);
    let mut i = 0;
    while i < limit {
        if bytes[i] == b'\n' {
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
    let index = LineStarts::new(prefix);
    Error::coded(
        ErrorKind::Parser,
        ErrorCode::UnexpectedToken,
        "unterminated frontmatter: expected closing `---` or `...` delimiter",
    )
    .with_span(open_span)
    .with_location_from(&index, prefix)
}
