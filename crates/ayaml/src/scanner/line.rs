use std::collections::HashMap;

use crate::error::{Error, ErrorKind};

/// The block structure that owns a node, as far as the context-free scanner
/// can tell from the text around it.
///
/// YAML measures the indentation of a node's continuation lines (plain
/// scalars) and content lines (block scalars) against the indentation `n` of
/// the block collection that contains the node, not against the line the node
/// starts on. In `- - a` the inner sequence is indented by 2 even though the
/// line is indented by 0.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    /// A root node (the spec's indentation -1).
    Root,
    /// A node inside a block collection indented by this many columns.
    Block(usize),
    /// The surrounding text does not determine the owner.
    Unknown,
}

/// How far a plain scalar's continuation lines must be indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Continuation {
    /// Inside a flow collection.
    Flow,
    /// At least this many spaces.
    Indent(usize),
    /// Fallback when the owner is unknown: measured against the scalar's own
    /// line. Standalone scalars may continue at their own indentation.
    Line {
        start_indent: usize,
        standalone: bool,
        allow_block_indicator: bool,
    },
}

/// What a line (or the part of a line before a node) says about ownership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LineShape {
    /// The text ends with structure that owns what follows it.
    Owner(Owner),
    /// Only whitespace or node properties: look at the previous line.
    Open,
    /// The text ends with content, so it doesn't own what follows.
    Content,
}

/// The number of spaces that `text` starts with.
pub(super) fn leading_spaces(text: &str) -> usize {
    text.len() - text.trim_start_matches(' ').len()
}

/// Where a line's key or node starts: `text` without a mark it starts with, the offset in that of
/// what follows its leading spaces, an optional `---`, and block indicators (`- `, `? `, `: `), and
/// the owner those say it has.
pub(super) fn block_prefix(text: &str, compat: bool) -> (&str, usize, Option<Owner>) {
    // In compat, a `---` after a mark is text, as the scanner reads it.
    let (text, marker_possible) = if compat {
        text.strip_prefix('\u{feff}')
            .map_or((text, true), |rest| (rest, false))
    } else {
        (text.trim_start_matches('\u{feff}'), true)
    };
    let bytes = text.as_bytes();
    let separator = |index: usize| index >= bytes.len() || matches!(bytes[index], b' ' | b'\t');
    let skip_blanks = |mut index: usize| {
        while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
            index += 1;
        }
        index
    };
    let mut index = 0;
    while index < bytes.len() && bytes[index] == b' ' {
        index += 1;
    }
    let mut owner = None;
    if marker_possible && index == 0 && text.starts_with("---") && separator(3) {
        owner = Some(Owner::Root);
        index = skip_blanks(3);
    }
    while index < bytes.len() && matches!(bytes[index], b'-' | b'?' | b':') && separator(index + 1)
    {
        // Indicators are ASCII, and so is everything before them on the line,
        // so the byte offset is the column.
        owner = Some(Owner::Block(index));
        index = skip_blanks(index + 1);
    }
    (text, index, owner)
}

/// Classify `text`, which starts at a line start: what `block_prefix` reads, an optional implicit
/// key with its `:`, and node properties. The last indicator or key decides the owner. `names`
/// ends anchor and alias names where the scanner does.
pub(super) fn line_shape(text: &str, names: NameRules<'_, '_>) -> LineShape {
    let (text, index, owner) = block_prefix(text, names.compat);
    let rest = &text[index..];
    if let LineStop::Value(colon) = walk_line(rest, names) {
        return if only_properties(&rest[colon + 1..], names.compat) {
            LineShape::Owner(Owner::Block(text[..index].chars().count()))
        } else {
            LineShape::Content
        };
    }
    if only_properties(rest, names.compat) {
        owner.map_or(LineShape::Open, LineShape::Owner)
    } else {
        LineShape::Content
    }
}

/// What decides where an anchor or alias name ends on a line that isn't being scanned.
#[derive(Clone, Copy)]
pub(super) struct NameRules<'s, 'a> {
    pub(super) compat: bool,
    pub(super) colon_anchors: Option<&'s HashMap<&'a str, usize>>,
    /// The text the names are part of, so a name's place in it says which anchors come before it.
    pub(super) input: &'a str,
}

impl NameRules<'_, '_> {
    /// Whether an alias whose name ends in `:` keeps it. By default every name does. In compat,
    /// only one that an anchor before it already has does. `name` is part of the input: a line is
    /// read again after anchors later on it, or on later lines, have been scanned, and those
    /// don't count.
    pub(super) fn alias_keeps_colon(self, name: &str) -> bool {
        !self.compat
            || self.colon_anchors.is_some_and(|names| {
                names.get(name).is_some_and(|&defined| {
                    name.as_ptr()
                        .addr()
                        .checked_sub(self.input.as_ptr().addr())
                        .is_some_and(|at| at <= self.input.len() && defined < at)
                })
            })
    }
}

/// Whether `text` holds nothing but node properties (anchors and tags).
pub(super) fn only_properties(text: &str, compat: bool) -> bool {
    text.split([' ', '\t'])
        .filter(|token| !token.is_empty())
        .all(|token| {
            token.starts_with('!')
                || (token.starts_with('&')
                    && !(compat
                        && (1..token.len())
                            .any(|index| compat_name_stop(token.as_bytes(), index, 1))))
        })
}

/// Whether the compat scanner, outside a flow collection, ends the anchor or alias name that
/// starts at `name_start` before `bytes[index]`: libyaml stops at a `?`, and at a `:` followed by
/// more name text (see `scan_anchor`).
fn compat_name_stop(bytes: &[u8], index: usize, name_start: usize) -> bool {
    match bytes[index] {
        b'?' => bytes
            .get(index + 1)
            .is_some_and(|next| !matches!(next, b'[' | b']' | b'{' | b'}' | b',')),
        b':' => {
            if index <= name_start {
                return false;
            }
            let rest = std::str::from_utf8(&bytes[index + 1..]).unwrap_or("");
            rest.chars().next().is_some_and(|next| {
                !matches!(
                    next,
                    ' ' | '\t' | '[' | ']' | '{' | '}' | ',' | '\u{85}' | '\u{2028}' | '\u{2029}'
                )
            })
        }
        _ => false,
    }
}

/// Where walking a line's tokens stopped, as a byte offset into it.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum LineStop {
    /// A block mapping-value indicator: a `:` followed by whitespace or the end.
    Value(usize),
    /// A `#` that starts a comment.
    Comment(usize),
    /// The line ended first, or a flow collection ran past it.
    End,
}

/// Walk `text` from the start of a token to its first value indicator or comment, skipping quoted
/// scalars and flow collections.
pub(super) fn walk_line(text: &str, names: NameRules<'_, '_>) -> LineStop {
    let bytes = text.as_bytes();
    let mut index = 0;
    // Inside a plain scalar, quotes, flow indicators, and property indicators are content:
    // `a [b: |` has the key `a [b`.
    let mut in_plain = false;
    while index < bytes.len() {
        let byte = bytes[index];
        match byte {
            b'#' if index == 0 || matches!(bytes[index - 1], b' ' | b'\t') => {
                return LineStop::Comment(index);
            }
            b':' if bytes
                .get(index + 1)
                .is_none_or(|next| matches!(next, b' ' | b'\t')) =>
            {
                return LineStop::Value(index);
            }
            _ if in_plain => {}
            b'"' | b'\'' => index = closing_quote(bytes, index),
            // A tag, anchor, or alias runs to the next blank; a `:` in it, as in `!: |` or
            // `&a: x`, is part of the name. In compat an anchor or alias name ends where libyaml
            // ends it, and plain text starts there: `&a:: |` is the key `:` anchored `a`.
            b'!' | b'&' | b'*' => {
                let name_start = index + 1;
                while index + 1 < bytes.len() && !matches!(bytes[index + 1], b' ' | b'\t') {
                    if names.compat
                        && byte != b'!'
                        && compat_name_stop(bytes, index + 1, name_start)
                    {
                        break;
                    }
                    index += 1;
                }
                // An alias that gives up its last `:` leaves it as this line's indicator.
                if byte == b'*'
                    && index > name_start
                    && bytes[index] == b':'
                    && !names.alias_keeps_colon(&text[name_start..=index])
                {
                    return LineStop::Value(index);
                }
            }
            b'[' | b'{' => match flow_collection_end(bytes, index, names.compat) {
                Some(end) => index = end,
                None => return LineStop::End,
            },
            b' ' | b'\t' => {}
            _ => in_plain = true,
        }
        index += 1;
    }
    LineStop::End
}

/// Whether `line`, from where a node starts to its line's end, starts a block collection: a block
/// indicator, or an implicit key, which a `:` indicator follows on the line. The walk reads one
/// node, after any properties, so that a line with many nodes is read about once in all. A quoted
/// scalar, a flow collection, or an alias is a key only if the `:` comes right after it, and a
/// plain scalar ends at its `:`. A block scalar's header takes the rest of its line.
pub(super) fn starts_block_collection(line: &str, names: NameRules<'_, '_>) -> bool {
    let bytes = line.as_bytes();
    let separated = |index: usize| {
        bytes
            .get(index + 1)
            .is_none_or(|next| matches!(next, b' ' | b'\t'))
    };
    let skip_blanks = |mut index: usize| {
        while matches!(bytes.get(index), Some(b' ' | b'\t')) {
            index += 1;
        }
        index
    };
    if matches!(bytes.first(), Some(b'-' | b'?')) && separated(0) {
        return true;
    }
    // Node properties, each running to a blank, or in compat to where libyaml ends an anchor's
    // name. What follows a name that ends early is plain text.
    let mut index = 0;
    while let Some(&property @ (b'!' | b'&')) = bytes.get(index) {
        let name_start = index + 1;
        index = name_start;
        while index < bytes.len()
            && !matches!(bytes[index], b' ' | b'\t')
            && !(names.compat && property == b'&' && compat_name_stop(bytes, index, name_start))
        {
            index += 1;
        }
        index = skip_blanks(index);
    }
    let end = match bytes.get(index) {
        // An empty key, as in `: a` or `&a : b`.
        Some(b':') if separated(index) => return true,
        // A block indicator after properties is the parser's to refuse, and the rest of these
        // can't start a plain scalar. Walking on from any of them would read on to the line's
        // end, and again for each tab after the next indicator.
        Some(b'-' | b'?') if separated(index) => return false,
        None | Some(b'#' | b'|' | b'>' | b',' | b']' | b'}' | b'%' | b'@' | b'`') => return false,
        Some(b'"' | b'\'') => {
            let close = closing_quote(bytes, index);
            if close >= bytes.len() {
                // It goes on past this line, so it can't be an implicit key.
                return false;
            }
            close + 1
        }
        Some(b'[' | b'{') => match flow_collection_end(bytes, index, names.compat) {
            Some(close) => close + 1,
            None => return false,
        },
        Some(b'*') => {
            let name_end = (index..bytes.len())
                .find(|&at| matches!(bytes[at], b' ' | b'\t'))
                .unwrap_or(bytes.len());
            // In compat an alias may give up its last `:`, which is then the key's indicator.
            if let LineStop::Value(_) = walk_line(&line[index..name_end], names) {
                return true;
            }
            name_end
        }
        Some(_) => return matches!(walk_line(&line[index..], names), LineStop::Value(_)),
    };
    let after = skip_blanks(end);
    bytes.get(after) == Some(&b':') && separated(after)
}

/// The offset of what closes the flow collection that opens at `open`, reading its tokens by the
/// scanner's rules, or `None` if it doesn't close on this line. What's quoted, named, or plain in
/// it can hold any of the indicators, so it takes all of these to find the end of a key. A `:` or
/// `?` before a flow indicator is an indicator to the scanner too, but plain text would stop right
/// there anyway, so only a blank after one counts here.
pub(super) fn flow_collection_end(bytes: &[u8], open: usize, compat: bool) -> Option<usize> {
    let blank_follows = |index: usize| matches!(bytes.get(index + 1), Some(b' ' | b'\t'));
    let mut depth = 0_usize;
    let mut index = open;
    // A `:` after a quoted scalar or a closed collection is an indicator whatever follows it, past
    // any blanks: in `{"a" :"b"}`, `"b"` is quoted.
    let mut after_json_like = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if matches!(byte, b' ' | b'\t') {
            index += 1;
            continue;
        }
        let mut json_like = false;
        match byte {
            // A comment, because without a blank before it this is an error.
            b'#' => return None,
            b'[' | b'{' => depth += 1,
            b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(index);
                }
                json_like = true;
            }
            b',' => {}
            b'"' | b'\'' => {
                index = closing_quote(bytes, index);
                json_like = true;
            }
            // A verbatim tag can hold flow indicators.
            b'!' if bytes.get(index + 1) == Some(&b'<') => {
                while index + 1 < bytes.len() && bytes[index] != b'>' {
                    index += 1;
                }
            }
            // Here no `:` ends a name. Libyaml's stop at a `?` in compat leaves an indicator that
            // a name can't come before, so that's an error whichever way it's read.
            b'!' | b'&' | b'*' => {
                while bytes
                    .get(index + 1)
                    .is_some_and(|next| !matches!(next, b' ' | b'\t' | b',' | b']' | b'}'))
                {
                    index += 1;
                }
            }
            b'?' if compat || blank_follows(index) => {}
            b':' if after_json_like || blank_follows(index) => {}
            // A plain scalar, in which quotes and names are content. A `:` ends it only if the
            // arm above takes it, and otherwise this arm goes on from there.
            _ => {
                while let Some(&next) = bytes.get(index + 1) {
                    if matches!(next, b',' | b'[' | b']' | b'{' | b'}' | b':')
                        || (next == b'#' && matches!(bytes[index], b' ' | b'\t'))
                    {
                        break;
                    }
                    index += 1;
                }
            }
        }
        after_json_like = json_like;
        index += 1;
    }
    None
}

/// The offset of the quote that closes the scalar opening at `open`, or past the end if none does.
pub(super) fn closing_quote(bytes: &[u8], open: usize) -> usize {
    let quote = bytes[open];
    let mut index = open + 1;
    while index < bytes.len() {
        if quote == b'"' && bytes[index] == b'\\' {
            index += 2;
            continue;
        }
        if bytes[index] == quote {
            if quote == b'\'' && bytes.get(index + 1) == Some(&b'\'') {
                index += 2;
                continue;
            }
            return index;
        }
        index += 1;
    }
    bytes.len()
}

/// `line` without a trailing comment.
pub(super) fn strip_comment<'t>(line: &'t str, names: NameRules<'_, '_>) -> &'t str {
    let (text, mut from, _) = block_prefix(line, names.compat);
    let mark = line.len() - text.len();
    loop {
        match walk_line(&text[from..], names) {
            LineStop::Value(colon) => from += colon + 1,
            LineStop::Comment(comment) => return &line[..mark + from + comment],
            LineStop::End => return line,
        }
    }
}

/// Check the stream's characters. A character outside `nb-json`
/// (a C0 control other than tab, line feed, or carriage return) is an error
/// anywhere: its offset is the `Err`. A character in `nb-json` but outside
/// `c-printable` (DEL, a C1 control other than NEL, U+FFFE, or U+FFFF) may
/// appear only inside a quoted scalar (YAML 1.2.2 section 5.1); the `Ok`
/// lists their offsets for the scanner to check in context. Escapes in
/// double-quoted scalars can still produce any character.
pub(super) fn non_printable_characters(input: &str) -> Result<Vec<usize>, usize> {
    let bytes = input.as_bytes();
    let mut quoted_only = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            if byte < 0x20 && !matches!(byte, b'\t' | b'\n' | b'\r') {
                return Err(index);
            }
            if byte == 0x7f {
                quoted_only.push(index);
            }
            index += 1;
            continue;
        }
        let Some(ch) = input[index..].chars().next() else {
            break;
        };
        let code = u32::from(ch);
        if !(code == 0x85
            || (0xa0..=0xd7ff).contains(&code)
            || (0xe000..=0xfffd).contains(&code)
            || code >= 0x1_0000)
        {
            quoted_only.push(index);
        }
        index += ch.len_utf8();
    }
    Ok(quoted_only)
}

pub(super) fn non_printable_error(input: &str, index: usize) -> Error {
    let Some(ch) = input[index..].chars().next() else {
        return Error::new(ErrorKind::Scanner, "invalid character boundary");
    };
    Error::new(
        ErrorKind::Scanner,
        format!(
            "non-printable character U+{:04X} is not allowed in YAML",
            u32::from(ch)
        ),
    )
}

/// Why [`decode_percent_escapes`] failed.
pub(super) enum PercentError {
    /// A `%` at this byte offset is not followed by two hex digits.
    Escape(usize),
    /// The decoded bytes are not UTF-8.
    Utf8,
}

/// Decode the `%XX` escapes in tag text (YAML 1.2.2 section 6.8.2).
pub(super) fn decode_percent_escapes(raw: &str) -> Result<String, PercentError> {
    if !raw.contains('%') {
        return Ok(raw.to_owned());
    }
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = raw
                .get(index + 1..index + 3)
                .filter(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
                .ok_or(PercentError::Escape(index))?;
            decoded.push(hex);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| PercentError::Utf8)
}

pub(super) fn is_uri_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
        || matches!(
            ch,
            '-' | '#'
                | ';'
                | '/'
                | '?'
                | ':'
                | '@'
                | '&'
                | '='
                | '+'
                | '$'
                | ','
                | '_'
                | '.'
                | '!'
                | '~'
                | '*'
                | '\''
                | '('
                | ')'
                | '['
                | ']'
                | '%'
        )
}

/// A tag shorthand character (production 40, `ns-tag-char`): a URI character
/// other than `!` and the flow indicators.
/// Whether `text` is URI characters, with each `%` starting a `%XX` escape.
pub(super) fn uri_text_ok(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.char_indices().all(|(index, ch)| {
        is_uri_char(ch)
            && (ch != '%'
                || (bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                    && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)))
    })
}

pub(super) fn is_tag_char(ch: char) -> bool {
    is_uri_char(ch) && !matches!(ch, '!' | ',' | '[' | ']' | '{' | '}')
}

/// YAML white space (production 33, `s-white`) or a line break. Other
/// Unicode spaces, such as U+00A0, are ordinary content characters.
pub(super) fn is_blank(ch: char) -> bool {
    matches!(ch, ' ' | '\t') || is_line_break(ch)
}

/// Whether `ch` is a YAML 1.2 line break. NEL, LS, and PS were line breaks
/// in YAML 1.1 and libyaml; in YAML 1.2 they are content.
pub(super) fn is_line_break(ch: char) -> bool {
    matches!(ch, '\n' | '\r')
}
