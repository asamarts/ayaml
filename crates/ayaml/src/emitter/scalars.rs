use std::fmt::Write as _;

/// YAML's printable character set (production 1, `c-printable`).
#[inline]
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
#[inline]
pub(crate) fn is_raw_char(ch: char) -> bool {
    is_printable(ch)
        && !matches!(
            ch,
            '\n' | '\r' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\u{feff}'
        )
}

#[inline]
fn is_flow_indicator_byte(byte: u8) -> bool {
    matches!(byte, b',' | b'[' | b']' | b'{' | b'}')
}

const PLAIN_SIMPLE: u8 = 0;
const PLAIN_SPACE: u8 = 1;
const PLAIN_FLOW: u8 = 2;
const PLAIN_COLON: u8 = 3;
const PLAIN_HASH: u8 = 4;
const PLAIN_BAD: u8 = 5;
const PLAIN_NON_ASCII: u8 = 6;

const PLAIN_CLASS: [u8; 256] = {
    let mut table = [PLAIN_SIMPLE; 256];
    let mut i = 0;
    while i < 0x20 {
        table[i] = PLAIN_BAD;
        i += 1;
    }
    table[b' ' as usize] = PLAIN_SPACE;
    table[b',' as usize] = PLAIN_FLOW;
    table[b'[' as usize] = PLAIN_FLOW;
    table[b']' as usize] = PLAIN_FLOW;
    table[b'{' as usize] = PLAIN_FLOW;
    table[b'}' as usize] = PLAIN_FLOW;
    table[b':' as usize] = PLAIN_COLON;
    table[b'#' as usize] = PLAIN_HASH;
    table[0x7f] = PLAIN_BAD;
    let mut j = 0x80;
    while j < 256 {
        table[j] = PLAIN_NON_ASCII;
        j += 1;
    }
    table
};

/// Whether `value` reads back as the same plain scalar text, in block
/// context or, with `flow`, inside a flow collection. This checks syntax
/// only; callers that write strings must also check how the text resolves.
///
/// Where libyaml's emitter is stricter than the spec, these rules follow it,
/// so the output also reads back under libyaml-based readers: a leading `?` or
/// `:`, and any `:` inside a flow collection, are quoted.
pub(crate) fn plain_text_ok(value: &str, flow: bool) -> bool {
    let bytes = value.as_bytes();
    let Some(&first) = bytes.first() else {
        return false;
    };
    let leading_ws = if first < 0x80 {
        first.is_ascii_whitespace()
    } else {
        value.chars().next().is_some_and(char::is_whitespace)
    };
    let last = bytes[bytes.len() - 1];
    let trailing_ws = if last < 0x80 {
        last.is_ascii_whitespace()
    } else {
        value.ends_with(char::is_whitespace)
    };
    if leading_ws
        || trailing_ws
        || (first == b'-' && value.starts_with("---"))
        || (first == b'.' && value.starts_with("..."))
    {
        return false;
    }
    let second = bytes.get(1).copied();
    let leading_ok = match first {
        b'-' => second.is_some_and(|next| next != b' ' && !(flow && is_flow_indicator_byte(next))),
        b'?' | b':' => !flow && second.is_some_and(|next| next != b' '),
        b',' | b'[' | b']' | b'{' | b'}' | b'#' | b'&' | b'*' | b'!' | b'|' | b'>' | b'\''
        | b'"' | b'%' | b'@' | b'`' => false,
        _ => true,
    };
    if !leading_ok {
        return false;
    }
    let mut index = 0;
    while index < bytes.len() {
        // Fast path over runs of ordinary ASCII bytes (the overwhelming majority of scalar text).
        while index < bytes.len() && PLAIN_CLASS[bytes[index] as usize] == PLAIN_SIMPLE {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        match PLAIN_CLASS[bytes[index] as usize] {
            PLAIN_SPACE => index += 1,
            PLAIN_FLOW => {
                if flow {
                    return false;
                }
                index += 1;
            }
            PLAIN_COLON => {
                if flow || bytes.get(index + 1).is_none_or(|&next| next == b' ') {
                    return false;
                }
                index += 1;
            }
            PLAIN_HASH => {
                if index > 0 && bytes[index - 1] == b' ' {
                    return false;
                }
                index += 1;
            }
            PLAIN_BAD => return false,
            _ => {
                let Some(ch) = value[index..].chars().next() else {
                    break;
                };
                if !is_raw_char(ch) {
                    return false;
                }
                index += ch.len_utf8();
            }
        }
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
    let bytes = value.as_bytes();
    let digits = match bytes.first() {
        Some(b'-' | b'+') => &bytes[1..],
        _ => bytes,
    };
    digits.len() > 1
        && digits[0] == b'0'
        && digits[1..].iter().all(|&b| b.is_ascii_digit() || b == b'_')
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
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < 0x80 {
            if !((0x20..=0x7e).contains(&byte) || byte == b'\t') {
                return false;
            }
            index += 1;
        } else {
            let Some(ch) = value[index..].chars().next() else {
                break;
            };
            if !is_raw_char(ch) {
                return false;
            }
            index += ch.len_utf8();
        }
    }
    true
}

/// Whether a string can be written as a literal or folded block scalar. It
/// must hold no characters that need escapes and no whitespace-only lines,
/// because a block scalar can lose their spaces.
pub(crate) fn block_scalar_ok(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut index = 0;
    let mut line_non_empty = false;
    let mut line_all_ws = true;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == b'\n' {
            if line_non_empty && line_all_ws {
                return false;
            }
            line_non_empty = false;
            line_all_ws = true;
            index += 1;
        } else if byte < 0x80 {
            if byte == b' ' || byte == b'\t' {
                line_non_empty = true;
                index += 1;
            } else if (0x21..=0x7e).contains(&byte) {
                line_non_empty = true;
                line_all_ws = false;
                index += 1;
                // Once a line has non-whitespace content, blast through printable ASCII bytes.
                while index < bytes.len() && (0x20..=0x7e).contains(&bytes[index]) {
                    index += 1;
                }
            } else {
                return false;
            }
        } else {
            let Some(ch) = value[index..].chars().next() else {
                break;
            };
            if !is_raw_char(ch) {
                return false;
            }
            line_non_empty = true;
            line_all_ws = false;
            index += ch.len_utf8();
        }
    }
    !(line_non_empty && line_all_ws)
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

#[cfg(test)]
pub(crate) fn double_quoted(value: &str) -> String {
    let mut output = String::with_capacity(value.len() + 2);
    emit_double_quoted(value, &mut output);
    output
}

/// Character count of `double_quoted(value)` capped at `cap`, without allocating.
pub(crate) fn double_quoted_char_len_capped(value: &str, cap: usize) -> usize {
    let mut count = 2usize;
    if count >= cap {
        return count;
    }
    for ch in value.chars() {
        let step = match ch {
            '\\' | '"' | '\0' | '\x07' | '\x08' | '\t' | '\n' | '\x0b' | '\x0c' | '\r' | '\x1b'
            | '\u{0085}' | '\u{2028}' | '\u{2029}' => 2,
            ch if !is_printable(ch) || ch == '\u{feff}' => {
                let code = ch as u32;
                if code <= 0xFF {
                    4
                } else if code <= 0xFFFF {
                    6
                } else {
                    10
                }
            }
            _ => 1,
        };
        count = count.saturating_add(step);
        if count >= cap {
            return count;
        }
    }
    count
}

/// Write a double-quoted scalar. Every character outside the printable set,
/// every line break, and the byte order mark are escaped, using libyaml's
/// escape choices, so the output always fits on one line.
pub(crate) fn emit_double_quoted(value: &str, output: &mut String) {
    output.push('"');
    let bytes = value.as_bytes();
    let mut run_start = 0;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if (0x20..=0x7e).contains(&byte) && byte != b'"' && byte != b'\\' {
            index += 1;
            continue;
        }
        if index > run_start {
            output.push_str(&value[run_start..index]);
        }
        if byte < 0x80 {
            match byte {
                b'\\' => output.push_str("\\\\"),
                b'"' => output.push_str("\\\""),
                b'\0' => output.push_str("\\0"),
                0x07 => output.push_str("\\a"),
                0x08 => output.push_str("\\b"),
                b'\t' => output.push_str("\\t"),
                b'\n' => output.push_str("\\n"),
                0x0b => output.push_str("\\v"),
                0x0c => output.push_str("\\f"),
                b'\r' => output.push_str("\\r"),
                0x1b => output.push_str("\\e"),
                _ => {
                    let _ = write!(output, "\\x{byte:02X}");
                }
            }
            index += 1;
            run_start = index;
        } else {
            let Some(ch) = value[index..].chars().next() else {
                break;
            };
            match ch {
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
            index += ch.len_utf8();
            run_start = index;
        }
    }
    if run_start < bytes.len() {
        output.push_str(&value[run_start..]);
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::{
        block_scalar_ok, double_quoted, double_quoted_char_len_capped, plain_text_ok,
        single_quoted_ok,
    };

    #[test]
    fn byte_table_scalar_predicates_and_double_quoting_handle_edge_cases() {
        assert!(plain_text_ok("service-a", false));
        assert!(plain_text_ok("a:b", false));
        assert!(!plain_text_ok("a:b", true));
        assert!(!plain_text_ok("a: b", false));
        assert!(!plain_text_ok("a:", false));
        assert!(plain_text_ok("a#b", false));
        assert!(!plain_text_ok("a #b", false));
        assert!(plain_text_ok("a,b", false));
        assert!(!plain_text_ok("a,b", true));
        assert!(plain_text_ok("café", false));
        assert!(!plain_text_ok("a\x7fb", false));
        assert!(!plain_text_ok("a\tb", false));
        assert!(!plain_text_ok("a\u{2028}b", false));

        assert!(single_quoted_ok("plain\tcafé"));
        assert!(!single_quoted_ok("a\nb"));
        assert!(!single_quoted_ok("a\x7fb"));
        assert!(!single_quoted_ok("a\u{feff}b"));

        assert!(block_scalar_ok("line1\n\nline2\n"));
        assert!(!block_scalar_ok("line1\n  \nline2\n"));
        assert!(!block_scalar_ok("line1\n\t"));
        assert!(!block_scalar_ok("line1\x7f\n"));

        let complex = "a\"b\\c\n\t\x7f\u{0085}\u{2028}\u{2029}\u{feff}é\u{1f680}\u{10ffff}";
        assert_eq!(
            double_quoted("a\"b\\c\n\t\x7f\u{0085}\u{2028}\u{2029}\u{feff}é"),
            "\"a\\\"b\\\\c\\n\\t\\x7F\\N\\L\\P\\uFEFFé\""
        );
        assert_eq!(
            double_quoted_char_len_capped(complex, usize::MAX),
            double_quoted(complex).chars().count()
        );
        assert!(double_quoted_char_len_capped(complex, 10) >= 10);
    }
}
