use std::fmt::Write as _;

/// YAML's printable character set (production 1, `c-printable`).
pub(crate) fn is_printable(ch: char) -> bool {
    matches!(
        ch,
        '\t' | '\n'
            | '\r'
            | ' '..='~'
            | '\u{85}'
            | '\u{a0}'..='\u{d7ff}'
            | '\u{e000}'..='\u{fffd}'
            | '\u{10000}'..='\u{10ffff}'
    )
}

/// Whether a character can be written raw inside one line of a plain,
/// single-quoted, or block scalar. Line breaks, non-printable characters,
/// and the byte order mark need a double-quoted escape. NEL, LS, and PS are
/// escaped as well, because YAML 1.1 readers treat them as line breaks.
pub(crate) fn is_raw_char(ch: char) -> bool {
    is_printable(ch)
        && !matches!(
            ch,
            '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}'
        )
}

fn is_flow_indicator(ch: char) -> bool {
    matches!(ch, ',' | '[' | ']' | '{' | '}')
}

/// Whether `value` reads back as the same plain scalar text, in block
/// context or, with `flow`, inside a flow collection. This checks syntax
/// only; callers that write strings must also check how the text resolves.
///
/// Where libyaml's emitter is stricter than the spec, these rules follow it,
/// so the output also reads back under libyaml-based readers: a leading `?` or
/// `:`, and any `:` inside a flow collection, are quoted.
pub(crate) fn plain_text_ok(value: &str, flow: bool) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    let second = chars.next();
    if first.is_whitespace()
        || value.ends_with(char::is_whitespace)
        || value.starts_with("---")
        || value.starts_with("...")
    {
        return false;
    }
    let leading_ok = match first {
        '-' => second.is_some_and(|next| next != ' ' && !(flow && is_flow_indicator(next))),
        '?' | ':' => !flow && second.is_some_and(|next| next != ' '),
        ',' | '[' | ']' | '{' | '}' | '#' | '&' | '*' | '!' | '|' | '>' | '\'' | '"' | '%'
        | '@' | '`' => false,
        _ => true,
    };
    if !leading_ok {
        return false;
    }
    let mut previous = None;
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if !is_raw_char(ch) || ch == '\t' {
            return false;
        }
        if flow && (is_flow_indicator(ch) || ch == ':') {
            return false;
        }
        if ch == ':' && chars.peek().is_none_or(|next| *next == ' ') {
            return false;
        }
        if ch == '#' && previous == Some(' ') {
            return false;
        }
        previous = Some(ch);
    }
    true
}

/// Whether a plain scalar with this text resolves to a string. A resolution
/// error (an integer outside the value model) counts as "not a string", so
/// such text is quoted instead of being written in a form that cannot be
/// read back.
fn resolves_to_string(value: &str) -> bool {
    crate::resolver::plain_resolves_to_string(value)
}

/// Whether the text looks like a decimal numeral with a leading zero, as in `0644` or `-0644`.
/// YAML 1.2 keeps it a string, but YAML 1.1 readers may take it as an octal integer, so a writer
/// quotes it. A sign, digits, and `_` are the whole shape; `0` alone is already a number.
pub(crate) fn looks_like_leading_zero_numeral(value: &str) -> bool {
    let digits = value.strip_prefix(['-', '+']).unwrap_or(value);
    let mut chars = digits.chars();
    chars.next() == Some('0')
        && chars.clone().next().is_some()
        && chars.all(|ch| ch.is_ascii_digit() || ch == '_')
}

/// Whether a string can be written as a plain scalar inside a flow collection.
pub(crate) fn plain_is_safe(value: &str) -> bool {
    plain_text_ok(value, true)
        && resolves_to_string(value)
        && !looks_like_leading_zero_numeral(value)
}

pub(super) fn plain_is_block_safe(value: &str) -> bool {
    plain_text_ok(value, false)
        && resolves_to_string(value)
        && !looks_like_leading_zero_numeral(value)
}

/// Whether a string can be written single-quoted on one line.
pub(crate) fn single_quoted_ok(value: &str) -> bool {
    value.chars().all(is_raw_char)
}

/// Whether a string can be written as a literal or folded block scalar. It
/// must hold no characters that need escapes and no whitespace-only lines,
/// because a block scalar can lose their spaces.
pub(crate) fn block_scalar_ok(value: &str) -> bool {
    value.chars().all(|ch| ch == '\n' || is_raw_char(ch)) && !has_whitespace_only_line(value)
}

pub(crate) fn can_fold(body: &str) -> bool {
    if body.starts_with('\n') || body.contains(['\t', '\r']) {
        return false;
    }
    body.split('\n')
        .all(|line| !line.starts_with(' ') && !line.ends_with(' '))
}

pub(crate) fn expand_folded(body: &str) -> String {
    let mut output = String::with_capacity(body.len().saturating_add(8));
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        output.push(ch);
        if ch == '\n' {
            output.push('\n');
            while chars.peek() == Some(&'\n') {
                output.push('\n');
                chars.next();
            }
        }
    }
    output
}

/// Whether a block scalar needs an explicit indentation indicator. It does
/// when its first non-empty line starts with whitespace, because
/// auto-detection would read that whitespace as indentation.
pub(crate) fn needs_explicit_indent(text: &str) -> bool {
    let body = text.trim_end_matches('\n');
    body.split('\n')
        .find(|line| !line.is_empty())
        .is_some_and(|line| line.starts_with([' ', '\t']))
}

fn has_whitespace_only_line(text: &str) -> bool {
    text.trim_end_matches('\n')
        .split('\n')
        .any(|line| !line.is_empty() && line.chars().all(|ch| matches!(ch, ' ' | '\t')))
}

pub(crate) fn double_quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    emit_double_quoted(value, &mut output);
    output
}

/// Write a double-quoted scalar. Every character outside the printable set,
/// every line break, and the byte order mark are escaped, using libyaml's
/// escape choices, so the output always fits on one line.
pub(crate) fn emit_double_quoted(value: &str, output: &mut String) {
    output.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => output.push_str("\\\\"),
            '"' => output.push_str("\\\""),
            '\0' => output.push_str("\\0"),
            '\u{0007}' => output.push_str("\\a"),
            '\u{0008}' => output.push_str("\\b"),
            '\t' => output.push_str("\\t"),
            '\n' => output.push_str("\\n"),
            '\u{000b}' => output.push_str("\\v"),
            '\u{000c}' => output.push_str("\\f"),
            '\r' => output.push_str("\\r"),
            '\u{001b}' => output.push_str("\\e"),
            '\u{0085}' => output.push_str("\\N"),
            '\u{2028}' => output.push_str("\\L"),
            '\u{2029}' => output.push_str("\\P"),
            ch if !is_printable(ch) || ch == '\u{feff}' => {
                let code = ch as u32;
                if code <= 0xFF {
                    let _ = write!(output, "\\x{code:02X}");
                } else if code <= 0xFFFF {
                    let _ = write!(output, "\\u{code:04X}");
                } else {
                    let _ = write!(output, "\\U{code:08X}");
                }
            }
            ch => output.push(ch),
        }
    }
    output.push('"');
}
