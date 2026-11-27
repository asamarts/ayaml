//! Source-scanning helpers for [`super::SpannedTree`] entry spans and missing-value positions.

use super::{
    AfterBlockScalar, BlankRunSummary, Entry, Kind, LineScanCache, SpannedNode, split_first_line,
};
use crate::event::ScalarStyle;
use crate::span::Span;

/// Where a pair with no braces ends, which starts at `start` and has the token after it at
/// `next`: with its value, where the source writes one. Where it doesn't, the pair ends with the
/// last thing written before `next`: its key, a `?` or `:`, a tag, or an anchor. Blanks, line
/// breaks, and comments after that are not the pair's.
pub(super) fn pair_end(
    input: &str,
    start: usize,
    key_content_end: Option<usize>,
    val_content_end: Option<usize>,
    next: usize,
) -> usize {
    if let Some(end) = val_content_end {
        return end;
    }
    let from = key_content_end.unwrap_or(start).min(next);
    let bytes = input.as_bytes();
    let blank = |at: usize| matches!(bytes.get(at), Some(b' ' | b'\t' | b'\n' | b'\r'));
    // The compatibility reader reads past a byte-order mark that begins a line, so one is no
    // more written text than the line's spaces are, and a comment may follow it. A mark
    // anywhere else is text to it, which would be the pair's value, so one that stands here,
    // in a pair with no value, begins its line, after the break that separates what follows.
    let mark = '\u{feff}';
    // Whether a `#` here would start a comment: something separates it from the pair's text.
    let (mut end, mut at, mut separated) = (from, from, false);
    while at < next {
        if blank(at) {
            at += 1;
            separated = true;
        } else if input[at..].starts_with(mark) {
            at += mark.len_utf8();
        } else if bytes[at] == b'#' && separated {
            while at < next && !matches!(bytes[at], b'\n' | b'\r') {
                at += 1;
            }
        } else {
            while at < next && !blank(at) {
                at += 1;
            }
            end = at;
            separated = false;
        }
    }
    end
}

pub(super) fn is_merge_key(node: &SpannedNode<'_>) -> bool {
    matches!(&node.kind, Kind::PlainScalar("<<"))
        || matches!(
            &node.kind,
            Kind::Scalar(data) if data.style == ScalarStyle::Plain && data.value == "<<"
        )
}

/// What stands before a block entry on its first line.
#[derive(Clone, Copy)]
pub(super) enum Lead {
    /// A mapping's key, which ends at `end`: indentation alone before it, and the key and its
    /// `:` on that one line. A key that follows a `?`, on that line or an earlier one, has its
    /// `:` on a later line or has none. A key with no text has no known end, whether a `?`
    /// came before it or not.
    Key { end: Option<usize> },
    /// A sequence's item: indentation, its `-`, and spaces.
    Item,
}

/// Whether `before` is indentation followed by one or more block-sequence `- ` indicators, as
/// precedes the first key of a compact mapping (`- a: 1` or `- - a: 1`).
fn is_compact_item_prefix(before: &str) -> bool {
    let mut rest = before.trim_start_matches(' ');
    let mut seen = false;
    while let Some(after) = rest.strip_prefix('-') {
        let spaces = after.len() - after.trim_start_matches(' ').len();
        if spaces == 0 {
            return false;
        }
        rest = &after[spaces..];
        seen = true;
    }
    seen && rest.is_empty()
}

/// The span that removes the first entry of a block mapping starting on a sequence's `-` line
/// (`- a: 1\n  b: 2\n`), leaving `- ` and shifting `next` into `first`'s column (`- b: 2\n`).
pub(super) fn compact_first_entry_span(
    input: &str,
    first_entry: &Entry<'_>,
    next_entry: &Entry<'_>,
    first_key_end: Option<usize>,
    first_val_end: Option<usize>,
    next_key_end: Option<usize>,
) -> Option<Span> {
    let first = first_entry.key.span.start();
    let key_end = first_key_end?;
    let content_end = first_val_end?;
    let mut line0 = line_start(input, first);
    if line0 == 0 {
        line0 = input.len() - input.trim_start_matches('\u{feff}').len();
    }
    let before = input.get(line0..first)?;
    if !is_compact_item_prefix(before) {
        return None;
    }
    let colon = after_key(input, key_end);
    if colon <= key_end || input.get(first..colon)?.contains(['\n', '\r']) {
        return None;
    }
    let end = compute_entry_end(input, content_end)?;
    let next_first = next_entry.key.span.start();
    let next_key_end = next_key_end?;
    let next_line = line_start(input, next_first);
    if end > next_line {
        return None;
    }
    let next_before = input.get(next_line..next_first)?;
    if !next_before.bytes().all(|byte| byte == b' ') || next_before.len() != before.len() {
        return None;
    }
    let next_colon = after_key(input, next_key_end);
    if next_colon <= next_key_end || input.get(next_first..next_colon)?.contains(['\n', '\r']) {
        return None;
    }
    if end < next_line {
        let between = input.get(end..next_line)?;
        // Refuse if any comment or non-whitespace line stands between the first entry and the
        // next key; deleting up to `next_first` would silently delete that intervening comment.
        if !between
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\n' | b'\r'))
        {
            return None;
        }
        // `between` is purely whitespace and `next_before` is purely spaces, so the first
        // non-blank line after `end` is guaranteed to begin at `next_first`.
        if input
            .get(next_first..)
            .and_then(|rest| rest.strip_prefix(':'))
            .is_some_and(|after| after.is_empty() || after.starts_with([' ', '\t', '\n', '\r']))
        {
            return None;
        }
    }
    (next_first > first).then(|| Span::new(first, next_first))
}

/// The span whose removal removes one block entry and nothing else, or `None` when the entry
/// has no such span (see [`SpannedNode::entry_span`]). The entry begins at `first`, its last
/// character ends at `content_end`, when that is known, `anchored` says whether it defines an
/// anchor, stands inside a collection that has one, or is in an input with a byte-order mark
/// past the marks that open it, and `after_block_scalar` whether the last thing written before
/// it is a block scalar.
pub(super) fn block_entry_span(
    cache: &mut LineScanCache<'_>,
    style: crate::event::CollectionStyle,
    lead: Lead,
    first: usize,
    content_end: Option<usize>,
    anchored: bool,
    after_block_scalar: AfterBlockScalar,
) -> Option<Span> {
    if style == crate::event::CollectionStyle::Flow || anchored {
        return None;
    }
    let input = cache.input;
    let content_end = content_end?;
    let mut start = line_start(input, first);
    // The byte-order marks that open the input are no part of the first entry.
    if start == 0 {
        start = input.len() - input.trim_start_matches('\u{feff}').len();
    }
    let before = input.get(start..first)?;
    let begins_its_line = match lead {
        Lead::Key { end } => {
            let end = end?;
            let colon = after_key(input, end);
            before.bytes().all(|byte| byte == b' ')
                && colon > end
                && !input.get(first..colon)?.contains(['\n', '\r'])
        }
        Lead::Item => before
            .trim_start_matches(' ')
            .strip_prefix('-')
            .is_some_and(|blanks| !blanks.is_empty() && blanks.bytes().all(|byte| byte == b' ')),
    };
    if !begins_its_line {
        return None;
    }
    let end = cache.entry_end(content_end)?;
    // A line that begins with `:` gives a value to the key before it. With this entry gone,
    // that could be an explicit key from before the entry.
    if cache.value_indicator_follows(end) {
        return None;
    }
    // A block scalar that is the last thing written before this entry ends where the entry
    // begins. With the entry gone it could absorb blank or comment lines that follow unless
    // content follows or every intervening blank/comment line is outside the scalar.
    if after_block_scalar.is_some() && !cache.block_scalar_ignores_after(end, after_block_scalar) {
        return None;
    }
    (end > start).then(|| Span::new(start, end))
}

/// Scan a contiguous run of blank and comment lines beginning at line boundary `from` in a single
/// pass, summarizing all properties needed by `value_indicator_follows`,
/// `block_scalar_ignores_after`, and block-mode `missing_value_place`.
pub(super) fn scan_blank_run(input: &str, from: usize) -> BlankRunSummary {
    let from = from.min(input.len());
    let mut offset = from;
    let mut blocks_any_scalar = false;
    let mut has_blank_before_comment = false;
    let mut seen_comment = false;
    let mut max_comment_indent: Option<usize> = None;
    #[cfg(test)]
    let mut lines_scanned = 0usize;

    while offset < input.len() {
        #[cfg(test)]
        {
            lines_scanned += 1;
        }
        let rest = &input[offset..];
        let (line, next_rest) = split_first_line(rest);
        let spaces = line.len() - line.trim_start_matches(' ').len();
        let after = &line[spaces..];
        if after.is_empty() {
            if spaces > 0 {
                blocks_any_scalar = true;
            }
            if !seen_comment {
                has_blank_before_comment = true;
            }
        } else if after.starts_with('\t') {
            let trimmed = after.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.starts_with('#') {
                blocks_any_scalar = true;
            } else {
                let next_is_value_indicator = trimmed
                    .strip_prefix(':')
                    .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t']));
                return BlankRunSummary {
                    from,
                    next_content: offset,
                    next_is_value_indicator,
                    next_content_indent: spaces,
                    blocks_any_scalar,
                    has_blank_before_comment,
                    max_comment_indent,
                    #[cfg(test)]
                    lines_scanned,
                };
            }
        } else if after.starts_with('#') {
            seen_comment = true;
            max_comment_indent = Some(max_comment_indent.map_or(spaces, |prev| prev.max(spaces)));
        } else {
            let next_is_value_indicator = after
                .strip_prefix(':')
                .is_some_and(|tail| tail.is_empty() || tail.starts_with([' ', '\t']));
            return BlankRunSummary {
                from,
                next_content: offset,
                next_is_value_indicator,
                next_content_indent: spaces,
                blocks_any_scalar,
                has_blank_before_comment,
                max_comment_indent,
                #[cfg(test)]
                lines_scanned,
            };
        }
        offset = input.len() - next_rest.len();
    }

    BlankRunSummary {
        from,
        next_content: input.len(),
        next_is_value_indicator: false,
        next_content_indent: 0,
        blocks_any_scalar,
        has_blank_before_comment,
        max_comment_indent,
        #[cfg(test)]
        lines_scanned,
    }
}

/// Where an entry whose last character ends at `content_end` ends: after its line break, or
/// `None` when more than a comment follows it on that line.
pub(super) fn compute_entry_end(input: &str, content_end: usize) -> Option<usize> {
    let (before, rest) = input.split_at_checked(content_end)?;
    // A block scalar's text runs through its last line break.
    if before.ends_with(['\n', '\r']) {
        return Some(content_end);
    }
    let bytes = rest.as_bytes();
    let mut pos = 0;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    let saw_blank = pos > 0;
    if pos < bytes.len() {
        // A comment needs a blank before its `#`.
        if bytes[pos] == b'#' && saw_blank {
            pos += 1;
            while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                pos += 1;
            }
        } else if !matches!(bytes[pos], b'\n' | b'\r') {
            return None;
        }
    }
    if pos < bytes.len() {
        if bytes[pos] == b'\r' && bytes.get(pos + 1) == Some(&b'\n') {
            pos += 2;
        } else {
            pos += 1;
        }
    }
    Some(content_end + pos)
}

/// Where a value that the source doesn't write stands, when the source shows that it wrote
/// none: after the `:` that follows its key on the key's line. `None` when no `:` follows the
/// key there, as after an explicit key, and when what comes next could be the value: anything
/// but a comment on the key's line, or a line indented past the mapping, which begins at
/// `mapping_start`. A tag that names the kind of a collection stands there with no event to
/// show for it. The mapping's column is the one to compare with, and not the key's: a tag or an
/// anchor before a key puts the key itself further right.
pub(super) fn missing_value_place(
    cache: &mut LineScanCache<'_>,
    mapping_start: usize,
    cached_column: &mut Option<usize>,
    key_end: usize,
    style: crate::event::CollectionStyle,
) -> Option<usize> {
    let input = cache.input;
    let at = after_key(input, key_end);
    if at == key_end {
        return None;
    }
    let bytes = input.as_bytes();
    if style == crate::event::CollectionStyle::Flow {
        // Flow mappings inspect only immediate bytes after `:`, skipping inline blanks/comments
        // without scanning to the end of a long single-line flow collection, and reuse the
        // memoized blank-run scanner across multi-line blank/comment runs.
        let mut pos = at;
        while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
            pos += 1;
        }
        if pos > at && pos < bytes.len() && bytes[pos] == b'#' {
            pos += 1;
            while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
                pos += 1;
            }
        }
        if pos >= bytes.len() {
            return Some(at);
        }
        match bytes[pos] {
            b',' | b'}' | b']' => return Some(at),
            b'\n' | b'\r' => {
                if bytes[pos] == b'\r' && bytes.get(pos + 1) == Some(&b'\n') {
                    pos += 2;
                } else {
                    pos += 1;
                }
                let run = cache.blank_run(pos);
                if run.next_content >= input.len() {
                    return Some(at);
                }
                let next_bytes = input.as_bytes();
                let mut cur = run.next_content;
                while cur < next_bytes.len() && matches!(next_bytes[cur], b' ' | b'\t') {
                    cur += 1;
                }
                return next_bytes
                    .get(cur)
                    .is_some_and(|b| matches!(b, b',' | b'}' | b']'))
                    .then_some(at);
            }
            _ => return None,
        }
    }

    // Block mapping: inspect the remainder of the key's line, then consult the memoized blank-run
    // scanner for any following blank/comment lines.
    let mut pos = at;
    while pos < bytes.len() && matches!(bytes[pos], b' ' | b'\t') {
        pos += 1;
    }
    if pos > at && pos < bytes.len() && bytes[pos] == b'#' {
        pos += 1;
        while pos < bytes.len() && !matches!(bytes[pos], b'\n' | b'\r') {
            pos += 1;
        }
    }
    if pos >= bytes.len() {
        return Some(at);
    }
    if !matches!(bytes[pos], b'\n' | b'\r') {
        return None;
    }
    if bytes[pos] == b'\r' && bytes.get(pos + 1) == Some(&b'\n') {
        pos += 2;
    } else {
        pos += 1;
    }

    // The column counts characters, and a byte-order mark that opens the input is none.
    let key_column = if let Some(col) = *cached_column {
        col
    } else {
        let col = input
            .get(line_start(input, mapping_start)..mapping_start)?
            .chars()
            .filter(|ch| *ch != '\u{feff}')
            .count();
        *cached_column = Some(col);
        col
    };
    let run = cache.blank_run(pos);
    if run.next_content >= input.len() {
        return Some(at);
    }
    (run.next_content_indent <= key_column).then_some(at)
}

/// The offset after the `:` that follows a key on the key's line, or the key's end when none
/// does. A key that is a block scalar ends with its last line, so a `:` after it stands on
/// another line, and is another entry's.
fn after_key(input: &str, key_end: usize) -> usize {
    let Some((before, rest)) = input.split_at_checked(key_end) else {
        return key_end;
    };
    if before.ends_with(['\n', '\r']) {
        return key_end;
    }
    let blanks = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    if rest[blanks..].starts_with(':') {
        key_end + blanks + 1
    } else {
        key_end
    }
}

fn line_start(input: &str, index: usize) -> usize {
    let bytes = input.as_bytes();
    let mut i = index;
    while i > 0 && !matches!(bytes[i - 1], b'\n' | b'\r') {
        i -= 1;
    }
    i
}
