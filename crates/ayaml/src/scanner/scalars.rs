use std::borrow::Cow;

use super::ScalarToken;
use super::line::{
    Continuation, LineShape, Owner, is_blank, is_line_break, leading_spaces, line_shape,
    strip_comment,
};
use super::{
    BOM_FIRST_BYTE, EMPTY_LINE_TAB, Error, ErrorCode, Limits, ScalarStyle, Scanner, Token,
    TokenKind, starts_with_document_marker,
};
use crate::position::{
    is_forbidden_control_byte, is_quoted_only_non_printable, line_start_of, location_at,
};

const LO_BYTES: u64 = 0x0101_0101_0101_0101;
const HI_BYTES: u64 = 0x8080_8080_8080_8080;

/// Whether any byte in `word` is `0x00`.
#[inline]
const fn has_zero_byte(word: u64) -> bool {
    (word.wrapping_sub(LO_BYTES) & !word & HI_BYTES) != 0
}

/// Whether any byte in `word` equals `byte`.
#[inline]
const fn has_byte(word: u64, byte: u8) -> bool {
    has_zero_byte(word ^ (LO_BYTES * byte as u64))
}

/// Whether any byte in `word` is non-ASCII (`>= 0x80`) or an ASCII control (`< 0x20`).
#[inline]
pub(super) const fn has_non_ascii_or_ctrl(word: u64) -> bool {
    ((word | word.wrapping_sub(0x20 * LO_BYTES)) & HI_BYTES) != 0
}

/// Whether any byte in `word` is non-ASCII (`>= 0x80`), an ASCII control (`< 0x20`), or DEL (`0x7F`).
#[inline]
pub(super) const fn has_non_ascii_or_ctrl_or_del(word: u64) -> bool {
    ((word | word.wrapping_sub(0x20 * LO_BYTES) | word.wrapping_add(LO_BYTES)) & HI_BYTES) != 0
}

/// Advance `pos` in 8-byte SWAR chunks over printable ASCII bytes (`0x20..=0x7E`).
#[inline]
pub(super) fn skip_printable_ascii_swar(bytes: &[u8], mut pos: usize) -> usize {
    for &raw in bytes[pos..].as_chunks::<8>().0 {
        if has_non_ascii_or_ctrl_or_del(u64::from_ne_bytes(raw)) {
            break;
        }
        pos += 8;
    }
    pos
}

/// Advance `pos` in 8-byte SWAR chunks over block plain-scalar safe ASCII bytes (`0x20..=0x7E`,
/// excluding `#` and `:`).
#[inline]
fn skip_plain_block_swar(bytes: &[u8], mut pos: usize) -> usize {
    for &raw in bytes[pos..].as_chunks::<8>().0 {
        let word = u64::from_ne_bytes(raw);
        if has_non_ascii_or_ctrl_or_del(word) || has_byte(word, b'#') || has_byte(word, b':') {
            break;
        }
        pos += 8;
    }
    pos
}

/// Advance `pos` in 8-byte SWAR chunks over flow plain-scalar safe ASCII bytes (`0x20..=0x7E`,
/// excluding `#`, `:`, `,`, `[`, `]`, `{`, `}`).
#[inline]
fn skip_plain_flow_swar(bytes: &[u8], mut pos: usize) -> usize {
    for &raw in bytes[pos..].as_chunks::<8>().0 {
        let word = u64::from_ne_bytes(raw);
        if has_non_ascii_or_ctrl_or_del(word)
            || has_byte(word, b'#')
            || has_byte(word, b':')
            || has_byte(word, b',')
            || has_byte(word, b'[')
            || has_byte(word, b']')
            || has_byte(word, b'{')
            || has_byte(word, b'}')
        {
            break;
        }
        pos += 8;
    }
    pos
}

/// Advance `pos` in 8-byte SWAR chunks over single-quoted safe ASCII bytes (`0x20..=0x7F`,
/// excluding `'`).
#[inline]
fn skip_single_quoted_swar(bytes: &[u8], mut pos: usize) -> usize {
    for &raw in bytes[pos..].as_chunks::<8>().0 {
        let word = u64::from_ne_bytes(raw);
        if has_non_ascii_or_ctrl(word) || has_byte(word, b'\'') {
            break;
        }
        pos += 8;
    }
    pos
}

/// Advance `pos` in 8-byte SWAR chunks over double-quoted safe ASCII bytes (`0x20..=0x7F`,
/// excluding `"` and `\`).
#[inline]
fn skip_double_quoted_swar(bytes: &[u8], mut pos: usize) -> usize {
    for &raw in bytes[pos..].as_chunks::<8>().0 {
        let word = u64::from_ne_bytes(raw);
        if has_non_ascii_or_ctrl(word) || has_byte(word, b'"') || has_byte(word, b'\\') {
            break;
        }
        pos += 8;
    }
    pos
}

const PLAIN_STOP_BLOCK: [bool; 256] = {
    let mut table = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        table[i] = true;
        i += 1;
    }
    table[b'\t' as usize] = false;
    table[b'#' as usize] = true;
    table[b':' as usize] = true;
    table[0x7f] = true;
    table[0xC2] = true;
    table[BOM_FIRST_BYTE as usize] = true;
    table
};

const PLAIN_STOP_FLOW: [bool; 256] = {
    let mut table = PLAIN_STOP_BLOCK;
    table[b'[' as usize] = true;
    table[b']' as usize] = true;
    table[b'{' as usize] = true;
    table[b'}' as usize] = true;
    table[b',' as usize] = true;
    table
};

const SINGLE_QUOTE_STOP: [bool; 256] = {
    let mut table = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        table[i] = true;
        i += 1;
    }
    table[b'\t' as usize] = false;
    table[b'\'' as usize] = true;
    table
};

const DOUBLE_QUOTE_STOP: [bool; 256] = {
    let mut table = [false; 256];
    let mut i = 0;
    while i < 0x20 {
        table[i] = true;
        i += 1;
    }
    table[b'\t' as usize] = false;
    table[b'"' as usize] = true;
    table[b'\\' as usize] = true;
    table
};

impl<'a> Scanner<'a> {
    /// Fail immediately with `LimitKind::ScalarBytes` (or `LimitKind::Events` if this token
    /// already exceeds `max_events`, matching [`Scanner::make`] precedence) when `observed`
    /// exceeds `limits.max_scalar_bytes`.
    #[inline]
    fn check_scalar_bytes_incremental(
        &self,
        start: usize,
        observed: usize,
        limits: &Limits,
    ) -> Result<(), Box<Error>> {
        if observed > limits.max_scalar_bytes {
            return Err(self.scalar_limit_error(start, observed, limits));
        }
        Ok(())
    }

    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    fn scalar_limit_error(&self, start: usize, observed: usize, limits: &Limits) -> Box<Error> {
        if let Err(events_limit) = limits.check_events(self.tokens + 1) {
            return self.limit_at(start, events_limit);
        }
        let Err(limit) = limits.check_scalar_bytes(observed) else {
            unreachable!("checked observed > max_scalar_bytes");
        };
        self.limit_at(start, limit)
    }

    pub(super) fn scan_single_quoted(
        &mut self,
        limits: &Limits,
    ) -> Result<Option<Token<'a>>, Box<Error>> {
        let start = self.pos;
        self.pos += 1;
        let content_start = self.pos;
        let bytes = self.input.as_bytes();
        let mut index = content_start;
        let warmup_end = (index + 8).min(bytes.len());
        while index < warmup_end && !SINGLE_QUOTE_STOP[bytes[index] as usize] {
            index += 1;
        }
        if index == warmup_end {
            index = skip_single_quoted_swar(bytes, index);
            while index < bytes.len() && !SINGLE_QUOTE_STOP[bytes[index] as usize] {
                index += 1;
            }
        }
        if index >= bytes.len() {
            self.pos = index;
            return Err(self.error_at(
                start,
                ErrorCode::UnterminatedQuotedScalar,
                "unterminated single-quoted scalar",
            ));
        }
        match bytes[index] {
            b'\'' => {
                if bytes.get(index + 1) != Some(&b'\'') {
                    self.pos = index + 1;
                    return self.make(TokenKind::SingleQuotedScalar, start, self.pos, limits);
                }
            }
            b'\n' | b'\r' => {}
            _ => {
                self.pos = index;
                return Err(self.non_printable_error_at(index));
            }
        }

        self.check_scalar_bytes_incremental(start, index - start, limits)?;
        let mut held = None;
        let mut value = String::with_capacity(
            (index - content_start)
                .saturating_add(16)
                .min(limits.max_scalar_bytes),
        );
        value.push_str(&self.input[content_start..index]);
        self.pos = index;
        loop {
            let run_start = self.pos;
            let mut i = run_start;
            let warmup_end = (i + 8).min(bytes.len());
            while i < warmup_end && !SINGLE_QUOTE_STOP[bytes[i] as usize] {
                i += 1;
            }
            if i == warmup_end {
                i = skip_single_quoted_swar(bytes, i);
                while i < bytes.len() && !SINGLE_QUOTE_STOP[bytes[i] as usize] {
                    i += 1;
                }
            }
            if i > run_start {
                self.check_scalar_bytes_incremental(start, i - start, limits)?;
                value.push_str(&self.input[run_start..i]);
                self.pos = i;
            }
            let Some(&byte) = bytes.get(self.pos) else {
                return Err(self.error_at(
                    start,
                    ErrorCode::UnterminatedQuotedScalar,
                    "unterminated single-quoted scalar",
                ));
            };
            match byte {
                b'\'' => {
                    if bytes.get(self.pos + 1) == Some(&b'\'') {
                        self.pos += 2;
                        self.check_scalar_bytes_incremental(start, self.pos - start, limits)?;
                        value.push('\'');
                    } else {
                        self.pos += 1;
                        break;
                    }
                }
                b'\n' | b'\r' => {
                    self.scan_quoted_line_break(
                        &mut value, '\'', start, &mut held, 0, false, limits,
                    )?;
                }
                _ => return Err(self.non_printable_error_at(self.pos)),
            }
        }
        let raw_text = &self.input[start..self.pos];
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar(Box::new(ScalarToken {
                value,
                style: ScalarStyle::SingleQuoted,
            })),
            start,
            self.pos,
            limits,
        )
    }

    /// Whether the lines of a flow node starting here may start at column 0. That holds in a
    /// document's root node and whatever it nests. Under a block collection they must be
    /// indented (YAML 1.2.2 productions 69, 137, and 141).
    pub(super) fn may_continue_at_column_zero(&self) -> bool {
        self.flow_allow_zero_indent
            .last()
            .copied()
            .unwrap_or(!self.node_seen)
    }

    /// The indent the lines of a quoted scalar that starts at `start` are held to, and whether they
    /// may sit at exactly it. The block collection that holds the scalar, or the outermost flow
    /// collection it is in, sets it: in `k:\n  'a\n b'` and `k: [\n    'a\n b']` the lines need only
    /// pass `k`'s column (YAML 1.2.2 production 69). After a compact key or entry the collection
    /// sits right of the line's indent, so by default in `- k: 'a\n   b'` and `- - 'a\n   b'` the
    /// lines need three spaces. Where the collection isn't known, or isn't left of the scalar's
    /// line, or after a compact key in compat, the line sets it.
    pub(super) fn quoted_indent(&mut self, start: usize) -> (usize, bool) {
        if self.may_continue_at_column_zero() {
            return (0, true);
        }
        let line_start = self.line_start(start);
        let prefix = self.input[line_start..start].trim_end_matches(is_blank);
        let prefix = prefix.strip_prefix('\u{feff}').unwrap_or(prefix);
        let (counted_line, mut indent) = self.counted_indent;
        if counted_line != line_start {
            // A mark's column doesn't count. libyaml has no such check, so it shouldn't get
            // stricter because of a mark.
            indent = leading_spaces(self.after_line_boms(line_start, self.input.len()));
            self.counted_indent = (line_start, indent);
        }
        let (mut owner, compact) = if self.flow_level > 0 {
            if self.flow_owner.0 != self.flow_start {
                self.flow_owner = (
                    self.flow_start,
                    self.owner_of(self.flow_start),
                    self.compact_column(self.flow_start),
                );
            }
            (self.flow_owner.1, self.flow_owner.2)
        } else {
            (self.owner_of(start), self.compact_column(start))
        };
        if self.compat_flow
            && self.previous_line_bom_takes_column(line_start)
            && let Owner::Block(parent) = owner
        {
            // A mark at the start of the key's line is a column in libyaml, but libyaml has no
            // check on a quoted scalar's continuation lines. Use the parent without that column.
            owner = Owner::Block(parent.saturating_sub(1));
        }
        // Compat holds the lines to the line's indent there, as libyaml has no such check and
        // real files align them with the key, as in `- k: "a\n  b"`.
        if let Some(column) = compact
            && column > indent
            && !self.compat_flow
        {
            return (column, false);
        }
        match owner {
            Owner::Block(parent) if parent < indent => (parent, false),
            _ => (indent, prefix.is_empty()),
        }
    }

    /// The column of the block collection that a compact key or entry before `at` on its line
    /// opens, as `k` in `- k: 'a` or the inner `-` in `- - 'a`. Only default mode holds a quoted
    /// scalar to it, and there no mark takes a column.
    pub(super) fn compact_column(&self, at: usize) -> Option<usize> {
        let line_start = self.line_start(at);
        match self.shape_of_line(line_start, &self.input[line_start..at]) {
            LineShape::Owner(Owner::Block(column)) => Some(column),
            _ => None,
        }
    }

    /// The spaces that a line of the outermost open flow collection needs before its content: one
    /// more than the column of the innermost block collection holding it, as `k` in
    /// `a:\n  k: [b,\n   c]` or in `- k: [b,\n   c]` (YAML 1.2.2 production 69). `None` where its
    /// lines may start at column 0, at the root, or where that collection isn't known.
    pub(super) fn flow_line_indent(&mut self) -> Option<usize> {
        if self.may_continue_at_column_zero() {
            return None;
        }
        if self.flow_owner.0 != self.flow_start {
            self.flow_owner = (
                self.flow_start,
                self.owner_of(self.flow_start),
                self.compact_column(self.flow_start),
            );
        }
        match self.flow_owner.1 {
            Owner::Block(column) => Some(column + 1),
            _ => None,
        }
    }

    /// The error for a line of the innermost open flow collection, starting its content at `at`,
    /// that isn't indented enough. Such a line most often follows a collection left open, so the
    /// error names where that collection opened, as the parser's flow errors do.
    #[cold]
    #[inline(never)]
    #[allow(
        clippy::unnecessary_box_returns,
        reason = "hot-path callers propagate Box<Error> to keep Result<_, Box<Error>> pointer-sized"
    )]
    pub(super) fn flow_indent_error(&self, at: usize) -> Box<Error> {
        let error = self.error_at(
            at,
            ErrorCode::InvalidFlowIndentation,
            "flow collection entries must be indented",
        );
        let Some(&(open, opened, _)) = self.flow_stack.last() else {
            return error;
        };
        let name = if open == '[' { "sequence" } else { "mapping" };
        match location_at(self.input, opened, self.compat_flow) {
            Some(there) => Box::new((*error).with_context(&format!(
                ", in the flow {name} opened at line {} column {}",
                there.line(),
                there.column()
            ))),
            None => error,
        }
    }

    /// Fold the line breaks at the current position into a quoted scalar's `value`, and check the
    /// next line's indentation. `held` holds the scalar's `quoted_indent` once a line break has
    /// asked for it, so a scalar that ends on its line never works it out.
    #[allow(
        clippy::too_many_arguments,
        reason = "internal helper carries the active scalar's incremental scan state"
    )]
    pub(super) fn scan_quoted_line_break(
        &mut self,
        value: &mut String,
        quote: char,
        start: usize,
        held: &mut Option<(usize, bool)>,
        protected_len: usize,
        escaped: bool,
        limits: &Limits,
    ) -> Result<(), Box<Error>> {
        // White space before an escaped line break is content.
        while !escaped && value.len() > protected_len && value.ends_with([' ', '\t']) {
            value.pop();
        }
        let mut breaks = 0_usize;
        loop {
            let line_start = self.pos;
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.pos += 1;
            }
            if !matches!(self.input.as_bytes().get(self.pos), Some(b'\n' | b'\r')) {
                break;
            }
            // By default an empty line can't hold a tab before the scalar's indentation
            // (`l-empty`, YAML 1.2.2 production 70), as in `k: "a\n\t\n b"`. libyaml reads it, and
            // so does compat.
            if breaks > 0
                && !self.compat_flow
                && let Some(tab) = self.input[line_start..self.pos].find('\t')
            {
                let (start_indent, allow_same_indent) = match *held {
                    Some(known) => known,
                    None => *held.insert(self.quoted_indent(start)),
                };
                if tab < start_indent + usize::from(!allow_same_indent) {
                    return Err(self.error_at(
                        line_start + tab,
                        ErrorCode::InvalidTabIndentation,
                        EMPTY_LINE_TAB,
                    ));
                }
            }
            self.consume_line_break();
            breaks += 1;
        }
        if breaks == 0 {
            return Ok(());
        }
        let (start_indent, allow_same_indent) = match *held {
            Some(known) => known,
            None => *held.insert(self.quoted_indent(start)),
        };
        let line_start = self.current_line_start;
        let indentation = &self.input[line_start..self.pos];
        // Only spaces indent a quoted scalar's continuation. A tab after them is separation, but
        // a tab before any space is not indentation, whatever follows it.
        let indent = indentation.chars().take_while(|ch| *ch == ' ').count();
        // A closing quote may sit at any indentation, as a closing `]` or `}` may, so tabs alone
        // can come before it. Only this scalar's quote closes it, and in a single-quoted scalar
        // `''` is a quote in the text.
        let closes =
            self.peek() == Some(quote) && (quote == '"' || self.peek_second() != Some('\''));
        if !allow_same_indent && !closes && indentation.starts_with('\t') {
            return Err(self.error_at(
                line_start,
                ErrorCode::InvalidTabIndentation,
                "tabs cannot be the only quoted-scalar indentation",
            ));
        }
        let rest = &self.input[self.pos..];
        if indentation.is_empty()
            && (rest.starts_with("---") || rest.starts_with("..."))
            && rest
                .get(3..)
                .is_none_or(|tail| tail.chars().next().is_none_or(is_blank))
        {
            return Err(self.error_at(
                self.pos,
                ErrorCode::InvalidQuotedContinuation,
                "document marker inside quoted scalar",
            ));
        }
        // Only the closing quote is exempt: a line that starts with the other kind of quote, or
        // with `''` in a single-quoted scalar, is text and is held to the indent.
        if !closes && (indent < start_indent || (!allow_same_indent && indent == start_indent)) {
            // A short continuation line can be the consequence of an unclosed quote. In that
            // case name the opening, where the reader has to add the quote, rather than the
            // indentation rule that noticed it first.
            if !self.quoted_closes_from(self.pos, quote) {
                let name = if quote == '"' { "double" } else { "single" };
                return Err(self.error_at(
                    start,
                    ErrorCode::UnterminatedQuotedScalar,
                    format!("unterminated {name}-quoted scalar"),
                ));
            }
            return Err(self.error_at(
                self.pos,
                ErrorCode::InvalidQuotedContinuation,
                "quoted scalar continuation must be indented more than its parent",
            ));
        }
        // Check raw byte progress before pushing folded line breaks so runs of blank lines cannot
        // allocate past `max_scalar_bytes`.
        self.check_scalar_bytes_incremental(start, self.pos - start, limits)?;
        // A folded break is a space; each empty line after it, or after an
        // escaped break, is a line feed (YAML 1.2.2 productions 112, 73).
        if breaks == 1 && !escaped {
            value.push(' ');
        } else {
            let added = breaks.saturating_sub(1);
            if value.len().saturating_add(added) > limits.max_scalar_bytes {
                // `'\n'` is never trimmed by a later line break, so `value.len() + added` is
                // permanently committed to the decoded scalar length.
                self.check_scalar_bytes_incremental(
                    start,
                    value.len().saturating_add(added),
                    limits,
                )?;
            }
            for _ in 1..breaks {
                value.push('\n');
            }
        }
        Ok(())
    }

    pub(super) fn scan_double_quoted(
        &mut self,
        limits: &Limits,
    ) -> Result<Option<Token<'a>>, Box<Error>> {
        let start = self.pos;
        self.pos += 1;
        let content_start = self.pos;
        let bytes = self.input.as_bytes();
        let mut index = content_start;
        let warmup_end = (index + 8).min(bytes.len());
        while index < warmup_end && !DOUBLE_QUOTE_STOP[bytes[index] as usize] {
            index += 1;
        }
        if index == warmup_end {
            index = skip_double_quoted_swar(bytes, index);
            while index < bytes.len() && !DOUBLE_QUOTE_STOP[bytes[index] as usize] {
                index += 1;
            }
        }
        if index >= bytes.len() {
            self.pos = index;
            return Err(self.error_at(
                start,
                ErrorCode::UnterminatedQuotedScalar,
                "unterminated double-quoted scalar",
            ));
        }
        match bytes[index] {
            b'"' => {
                self.pos = index + 1;
                return self.make(TokenKind::DoubleQuotedScalar, start, self.pos, limits);
            }
            b'\\' | b'\n' | b'\r' => {}
            _ => {
                self.pos = index;
                return Err(self.non_printable_error_at(index));
            }
        }

        self.check_scalar_bytes_incremental(start, index - start, limits)?;
        let mut held = None;
        let mut value = String::with_capacity(
            (index - content_start)
                .saturating_add(16)
                .min(limits.max_scalar_bytes),
        );
        value.push_str(&self.input[content_start..index]);
        self.pos = index;
        let mut protected_len = 0;
        loop {
            let run_start = self.pos;
            let mut i = run_start;
            let warmup_end = (i + 8).min(bytes.len());
            while i < warmup_end && !DOUBLE_QUOTE_STOP[bytes[i] as usize] {
                i += 1;
            }
            if i == warmup_end {
                i = skip_double_quoted_swar(bytes, i);
                while i < bytes.len() && !DOUBLE_QUOTE_STOP[bytes[i] as usize] {
                    i += 1;
                }
            }
            if i > run_start {
                self.check_scalar_bytes_incremental(start, i - start, limits)?;
                let run = &self.input[run_start..i];
                if value.len().saturating_add(run.len()) > limits.max_scalar_bytes {
                    let trimmed_run = run.trim_end_matches([' ', '\t']);
                    let committed = if trimmed_run.is_empty() {
                        value.trim_end_matches([' ', '\t']).len().max(protected_len)
                    } else {
                        value.len().saturating_add(trimmed_run.len())
                    };
                    self.check_scalar_bytes_incremental(start, committed, limits)?;
                }
                value.push_str(run);
                self.pos = i;
            }
            let Some(&byte) = bytes.get(self.pos) else {
                return Err(self.error_at(
                    start,
                    ErrorCode::UnterminatedQuotedScalar,
                    "unterminated double-quoted scalar",
                ));
            };
            match byte {
                b'"' => {
                    self.pos += 1;
                    break;
                }
                b'\\' => {
                    let escape_start = self.pos;
                    self.pos += 1;
                    if matches!(bytes.get(self.pos), Some(b'\n' | b'\r')) {
                        self.scan_quoted_line_break(
                            &mut value,
                            '"',
                            start,
                            &mut held,
                            protected_len,
                            true,
                            limits,
                        )?;
                        continue;
                    }
                    let Some(escape) = self.peek() else {
                        return Err(self.error_at(
                            escape_start,
                            ErrorCode::InvalidEscape,
                            "unterminated escape",
                        ));
                    };
                    if escape.is_ascii() && is_forbidden_control_byte(escape as u8) {
                        return Err(self.non_printable_error_at(self.pos));
                    }
                    self.pos += escape.len_utf8();
                    let decoded = match escape {
                        '0' => '\0',
                        'a' => '\u{0007}',
                        'b' => '\u{0008}',
                        't' | '\t' => '\t',
                        'n' => '\n',
                        'v' => '\u{000b}',
                        'f' => '\u{000c}',
                        'r' => '\r',
                        'e' => '\u{001b}',
                        ' ' => ' ',
                        '"' => '"',
                        '/' => '/',
                        '\\' => '\\',
                        'N' => '\u{0085}',
                        '_' => '\u{00a0}',
                        'L' => '\u{2028}',
                        'P' => '\u{2029}',
                        'x' => self.scan_hex_escape(2, escape_start)?,
                        'u' => self.scan_hex_escape(4, escape_start)?,
                        'U' => self.scan_hex_escape(8, escape_start)?,
                        other => {
                            return Err(self.error_at(
                                escape_start,
                                ErrorCode::InvalidEscape,
                                format!("unknown escape \\{other}"),
                            ));
                        }
                    };
                    let observed =
                        (self.pos - start).max(value.len().saturating_add(decoded.len_utf8()));
                    self.check_scalar_bytes_incremental(start, observed, limits)?;
                    value.push(decoded);
                    if matches!(decoded, ' ' | '\t') {
                        protected_len = value.len();
                    }
                }
                b'\n' | b'\r' => {
                    self.scan_quoted_line_break(
                        &mut value,
                        '"',
                        start,
                        &mut held,
                        protected_len,
                        false,
                        limits,
                    )?;
                }
                _ => return Err(self.non_printable_error_at(self.pos)),
            }
        }
        let raw_text = &self.input[start..self.pos];
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar(Box::new(ScalarToken {
                value,
                style: ScalarStyle::DoubleQuoted,
            })),
            start,
            self.pos,
            limits,
        )
    }

    pub(super) fn scan_hex_escape(
        &mut self,
        digits: usize,
        escape_start: usize,
    ) -> Result<char, Box<Error>> {
        let mut value = 0_u32;
        for _ in 0..digits {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(
                    escape_start,
                    ErrorCode::InvalidEscape,
                    "short hexadecimal escape",
                ));
            };
            if ch.is_ascii() && is_forbidden_control_byte(ch as u8) {
                return Err(self.non_printable_error_at(self.pos));
            }
            let Some(digit) = ch.to_digit(16) else {
                return Err(self.error_at(
                    self.pos,
                    ErrorCode::InvalidEscape,
                    "invalid hexadecimal escape",
                ));
            };
            value = value * 16 + digit;
            self.pos += ch.len_utf8();
        }
        char::from_u32(value).ok_or_else(|| {
            self.error_at(
                escape_start,
                ErrorCode::InvalidEscape,
                "escape is not a Unicode scalar value",
            )
        })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the routine is one linear state machine; splitting it would obscure its state"
    )]
    pub(super) fn scan_block_scalar(
        &mut self,
        start: usize,
        literal: bool,
        limits: &Limits,
    ) -> Result<Option<Token<'a>>, Box<Error>> {
        self.pos += 1;
        // At most one chomping and one indentation indicator, in either order, right after the
        // `|` or `>` (YAML 1.2.2 production 162). Only blanks and a comment may follow them.
        let mut chomping = None;
        let mut explicit_indent = None;
        loop {
            match self.peek() {
                Some(ch @ ('+' | '-')) if chomping.is_none() => {
                    chomping = Some(if ch == '+' {
                        Chomping::Keep
                    } else {
                        Chomping::Strip
                    });
                    self.pos += 1;
                }
                Some(ch @ '1'..='9') if explicit_indent.is_none() => {
                    explicit_indent = Some(ch.to_digit(10).unwrap_or(0) as usize);
                    self.pos += 1;
                }
                _ => break,
            }
        }
        let chomping = chomping.unwrap_or(Chomping::Clip);
        loop {
            match self.peek() {
                Some(' ' | '\t') => self.pos += 1,
                Some('#') => {
                    let previous = self.input[..self.pos].chars().next_back();
                    if !previous.is_some_and(is_blank) {
                        return Err(self.error_at(
                            self.pos,
                            ErrorCode::MissingCommentWhitespace,
                            "a block scalar header comment requires preceding whitespace",
                        ));
                    }
                    let bytes = self.input.as_bytes();
                    while let Some(ch) = self.peek() {
                        if is_line_break(ch) {
                            break;
                        }
                        if is_forbidden_control_byte(bytes[self.pos])
                            || is_quoted_only_non_printable(bytes, self.pos)
                        {
                            return Err(self.non_printable_error_at(self.pos));
                        }
                        // A comment holds no mark (production 75), here as elsewhere. Compat
                        // reads one, as libyaml does.
                        if ch == '\u{feff}' && !self.compat_flow {
                            return Err(self.error_at(
                                self.pos,
                                ErrorCode::InvalidBom,
                                "a byte order mark can only start a document",
                            ));
                        }
                        self.pos += ch.len_utf8();
                    }
                    break;
                }
                Some(ch) if is_line_break(ch) => break,
                Some(ch) => {
                    let bytes = self.input.as_bytes();
                    if is_forbidden_control_byte(bytes[self.pos])
                        || is_quoted_only_non_printable(bytes, self.pos)
                    {
                        return Err(self.non_printable_error_at(self.pos));
                    }
                    return Err(self.error_at(
                        self.pos,
                        ErrorCode::InvalidBlockScalarHeader,
                        format!("unexpected {ch:?} in block scalar header"),
                    ));
                }
                None => break,
            }
        }
        if self.peek().is_some_and(|ch| !is_line_break(ch)) {
            return Err(self.error_at(
                start,
                ErrorCode::InvalidBlockScalarHeader,
                "block scalar header must end at a line break",
            ));
        }
        let line_start = self.current_line_start;
        let base_indent = self.line_indent_at(line_start);
        let prefix = self.input[line_start..start].trim_matches(is_blank);
        let root_context = prefix.is_empty() || prefix == "---" || prefix == "...";
        if self.peek().is_some() {
            self.consume_line_break();
        }
        let content_start = self.pos;
        // Content indentation is relative to the block collection that owns
        // the scalar: in `- key: |2` that is the mapping at column 2, not the
        // line's indentation. Root scalars count from column 0, as libyaml
        // does.
        let owner = self.owner_of(start);
        let header_indent = base_indent;
        // A header on the same line as its key or a root marker is already owned by that
        // line's structure; only a header on a line of its own needs the parent check.
        if line_start > 0
            && prefix.is_empty()
            && !self.input[line_start..start].contains('\t')
            && let Owner::Block(parent) = owner
        {
            let previous = self.input[..line_start]
                .trim_end_matches(is_line_break)
                .rsplit(['\n', '\r'])
                .next()
                .unwrap_or_default();
            let anchored_colon_key = self.compat_flow
                && parent == 0
                && header_indent == 0
                && previous.trim().starts_with('&')
                && previous.trim_end().ends_with("::");
            if header_indent <= parent && !anchored_colon_key {
                return Err(self.error_at(
                    start,
                    ErrorCode::InvalidBlockScalarIndentation,
                    "a block scalar is not indented past its parent",
                ));
            }
        }
        let root_like = matches!(&owner, Owner::Root)
            || (matches!(&owner, Owner::Unknown) && prefix.is_empty());
        let (detect_base, allow_base, explicit_base) = match owner {
            Owner::Root => (0, true, 0),
            Owner::Block(n) => (n, false, n),
            Owner::Unknown => (
                base_indent,
                root_context,
                if prefix.is_empty() { 0 } else { base_indent },
            ),
        };
        let indent = match explicit_indent {
            Some(digits) => Ok(if root_like && !self.compat_flow {
                // A root node's indentation is the spec's -1, so an indicator of 1 keeps a
                // column-0 line's first space (productions 170 and 174). libyaml counts from 0.
                digits - 1
            } else {
                explicit_base + digits
            }),
            None => self.detect_block_indent(content_start, detect_base, allow_base),
        }?;

        // When the indentation is detected, leading empty lines may not be
        // indented more than the first non-empty content line (YAML 1.2.2
        // section 8.1.1.1). With an indentation indicator, a "blank" line
        // with more spaces than that is content. Tabs on a leading
        // empty line are ambiguous and rejected as scalar indentation.
        let mut probe = content_start;
        let mut first_content = None;
        while probe < self.input.len() {
            let spaces = self.count_spaces(probe);
            let after_spaces = probe + spaces;
            let next = self.input[after_spaces..].chars().next();
            match next {
                None => break,
                Some(ch) if is_line_break(ch) => {
                    probe = after_spaces + ch.len_utf8();
                    if ch == '\r' && self.input[probe..].starts_with('\n') {
                        probe += 1;
                    }
                }
                Some(_) => {
                    // A document marker at column 0 ends a root scalar, so it is outside it,
                    // not the first content line that sets the leading-blank check.
                    if spaces == 0 && self.document_marker_starts_line(probe) {
                        break;
                    }
                    // A dedented line is outside the scalar, so it is neither content nor a
                    // leading empty line. A tab after the indentation is content, not space.
                    if spaces < indent {
                        let line_end = self.input[after_spaces..]
                            .find(['\n', '\r'])
                            .map_or(self.input.len(), |offset| after_spaces + offset);
                        let rest = &self.input[after_spaces..line_end];
                        if spaces == 0 && !rest.is_empty() && rest.chars().all(|ch| ch == '\t') {
                            return Err(self.error_at(
                                probe,
                                ErrorCode::InvalidTabIndentation,
                                "tabs are not allowed as leading block-scalar indentation",
                            ));
                        }
                        break;
                    }
                    first_content = Some((probe, spaces));
                    break;
                }
            }
        }
        if let Some((_, first_spaces)) = first_content.filter(|_| explicit_indent.is_none()) {
            let mut leading = content_start;
            while leading < probe {
                let line_end = self.input[leading..]
                    .find(['\n', '\r'])
                    .map_or(self.input.len(), |offset| leading + offset);
                let spaces = self.count_spaces(leading);
                if !self.input[leading..line_end].contains('\t') && spaces > first_spaces {
                    return Err(self.error_at(
                        leading,
                        ErrorCode::InvalidBlockScalarIndentation,
                        "leading empty block-scalar lines are more indented than the content",
                    ));
                }
                leading = line_end;
                if leading < self.input.len()
                    && let Some(ch) = self.input[leading..].chars().next()
                {
                    leading += ch.len_utf8();
                    if ch == '\r' && self.input[leading..].starts_with('\n') {
                        leading += 1;
                    }
                }
            }
        }

        // Reject headers that already exceed `max_scalar_bytes` before allocating body storage.
        self.check_scalar_bytes_incremental(start, content_start - start, limits)?;

        // The scalar's lines, each followed by its line break if it has one, built as they are
        // read. Holding each line in a string of its own cost dozens of times the input on short
        // lines, before the scalar limit could see the scalar.
        let mut text = String::new();
        // Whether the last line with any text has a line break, which clipping keeps.
        let mut final_break = false;
        let mut seen_content = false;
        let mut end = content_start;
        loop {
            if self.pos >= self.input.len() {
                break;
            }
            let line_start = self.pos;
            let spaces = self.count_spaces(line_start);
            let after_spaces = line_start + spaces;
            let next = self.input[after_spaces..].chars().next();
            let blank = next.is_none_or(is_line_break);
            if blank {
                if next.is_some() {
                    self.pos = after_spaces;
                    self.consume_line_break();
                } else {
                    self.pos = after_spaces;
                }
                end = self.pos;
                self.check_scalar_bytes_incremental(start, end - start, limits)?;
                // Spaces past the indentation are content, on a leading line
                // too when an indicator fixes the indentation.
                let content_spaces =
                    if (seen_content || explicit_indent.is_some()) && spaces > indent {
                        spaces - indent
                    } else {
                        0
                    };
                // The suite treats a whitespace-only last line at end of input
                // as a complete line (JEF9, L24T); libyaml drops its line
                // break, and the compatibility scanner keeps that.
                let at_eof_line = next.is_none() && spaces > 0 && !self.compat_flow;
                let had_break = next.is_some() || at_eof_line;
                text.extend(std::iter::repeat_n(' ', content_spaces));
                if had_break {
                    text.push('\n');
                }
                if content_spaces > 0 {
                    final_break = had_break;
                }
                continue;
            }
            if spaces < indent {
                let rest = &self.input[line_start..];
                if rest.starts_with('\t') && rest.trim_start_matches([' ', '\t']).starts_with('#') {
                    return Err(self.error_at(
                        line_start,
                        ErrorCode::InvalidTabIndentation,
                        "a tab cannot start a comment after a block scalar",
                    ));
                }
                break;
            }
            if indent == 0 {
                // Document markers at column 0 end any block scalar, including
                // one whose header follows `---` on the same line. A column-0
                // `#` line is content in YAML 1.2.2 (DK3J). libyaml indents a
                // root block scalar by at least one space, so there it is a
                // comment ending the scalar before any content; the compat
                // scanner keeps that wherever libyaml accepts the input.
                // A column-0 byte order mark always ends it there: libyaml
                // reads the BOM's line outside the scalar, so what follows
                // the BOM is a comment or an error, never lost text.
                let rest = &self.input[line_start..];
                if (rest.starts_with("---") && self.marker_follows(line_start, 3))
                    || (rest.starts_with("...") && self.marker_follows(line_start, 3))
                    || (self.compat_flow
                        && (rest.starts_with('\u{feff}')
                            || (!seen_content && rest.starts_with('#'))))
                {
                    break;
                }
            }
            if !self.compat_flow && self.input[line_start..].starts_with('\u{feff}') {
                // A BOM line ends the scalar when it starts a document prefix;
                // otherwise the BOM is an error below.
                if self.bom_starts_prefix(line_start) {
                    break;
                }
            }
            self.pos = line_start + indent;
            let text_start = self.pos;
            let bytes = self.input.as_bytes();
            let check_bom = !self.compat_flow;
            let mut position = text_start;
            while position < bytes.len() {
                position = skip_printable_ascii_swar(bytes, position);
                if position >= bytes.len() {
                    break;
                }
                match bytes[position] {
                    b'\n' | b'\r' => break,
                    b'\t' => position += 1,
                    0x00..=0x1f | 0x7f => {
                        self.pos = position;
                        return Err(self.non_printable_error_at(position));
                    }
                    0xC2 if is_quoted_only_non_printable(bytes, position) => {
                        self.pos = position;
                        return Err(self.non_printable_error_at(position));
                    }
                    BOM_FIRST_BYTE => {
                        if check_bom
                            && bytes.get(position + 1) == Some(&0xBB)
                            && bytes.get(position + 2) == Some(&0xBF)
                        {
                            self.pos = position;
                            return Err(self.error_at(
                                position,
                                ErrorCode::InvalidBom,
                                "a byte order mark can only start a document",
                            ));
                        }
                        if is_quoted_only_non_printable(bytes, position) {
                            self.pos = position;
                            return Err(self.non_printable_error_at(position));
                        }
                        position += 1;
                    }
                    _ => {
                        position += 1;
                        while position < bytes.len()
                            && bytes[position] >= 0x80
                            && !matches!(bytes[position], 0xC2 | BOM_FIRST_BYTE)
                        {
                            position += 1;
                        }
                    }
                }
            }
            let line_content_end = position;
            self.pos = position;
            let had_break = matches!(bytes.get(self.pos), Some(b'\n' | b'\r'));
            if had_break {
                self.consume_line_break();
            }
            end = self.pos;
            self.check_scalar_bytes_incremental(start, end - start, limits)?;
            // A content line holds at least the character that made it one.
            text.push_str(&self.input[text_start..line_content_end]);
            if had_break {
                text.push('\n');
            }
            final_break = had_break;
            seen_content = true;
        }
        self.pos = end;
        self.current_line_start = line_start_of(self.input.as_bytes(), self.pos);
        self.line_has_block_prefix = false;
        self.current_line_indent = None;

        let value = build_block_scalar(text, final_break, literal, chomping);
        self.make(
            TokenKind::Scalar(Box::new(ScalarToken {
                value: Cow::Owned(value),
                style: if literal {
                    ScalarStyle::Literal
                } else {
                    ScalarStyle::Folded
                },
            })),
            start,
            end,
            limits,
        )
    }

    /// The block structure owning the node that starts at `start` (see
    /// [`Owner`]). Only meaningful in block context.
    pub(super) fn owner_of(&self, start: usize) -> Owner {
        let line_start = self.line_start(start);
        match self.shape_of_line(line_start, &self.input[line_start..start]) {
            LineShape::Owner(owner) => owner,
            LineShape::Open => self.owner_from_previous_lines(line_start),
            LineShape::Content => Owner::Unknown,
        }
    }

    /// What `text`, which starts the line at `line_start`, says about ownership. An owner after
    /// a mark that takes up a column is one deeper.
    pub(super) fn shape_of_line(&self, line_start: usize, text: &str) -> LineShape {
        // The stream-start run of marks is skipped, so it cannot make a later mark look like
        // content. Keep the first mark in the slice: compat's block-prefix rule reads a marker
        // after it as text, and a mark on a later line still counts as a column here.
        let text = if self.compat_flow && line_start == 0 && self.initial_bom {
            let marks = self.initial_bom_end();
            let last = marks - '\u{feff}'.len_utf8();
            let end = line_start + text.len();
            if marks > '\u{feff}'.len_utf8() && end >= last {
                &self.input[last..end]
            } else {
                text
            }
        } else {
            text
        };
        let shape = line_shape(text, self.names());
        match shape {
            LineShape::Owner(Owner::Block(indent))
                if text
                    .strip_prefix('\u{feff}')
                    .is_some_and(|rest| self.mark_takes_column(line_start, rest)) =>
            {
                LineShape::Owner(Owner::Block(indent + 1))
            }
            shape => shape,
        }
    }

    /// Whether the mark that `after_mark` follows, at the start of the line at `line_start`,
    /// takes up a column. In compat a later line's does, as libyaml reads it. The first line's
    /// doesn't, unless a document marker follows: that's text only because the mark counts, so
    /// the rest of the line reads that way too. No mark takes up a column by default.
    pub(super) fn mark_takes_column(&self, line_start: usize, after_mark: &str) -> bool {
        self.compat_flow && (line_start > 0 || starts_with_document_marker(after_mark))
    }

    /// For a node that starts its line (after optional properties), the
    /// owner is whatever the previous non-empty line opened.
    pub(super) fn owner_from_previous_lines(&self, line_start: usize) -> Owner {
        let mut end = line_start;
        loop {
            if end == 0 || (self.initial_bom && end == self.initial_bom_end()) {
                return Owner::Root;
            }
            let mut previous_end = end;
            if self.input[..previous_end].ends_with('\n') {
                previous_end -= 1;
            }
            if self.input[..previous_end].ends_with('\r') {
                previous_end -= 1;
            }
            if previous_end == end {
                // A Unicode line break; find where it starts.
                let start = self.line_start(end.saturating_sub(1));
                previous_end = self.input[start..end]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| !is_line_break(*ch))
                    .map_or(start, |(offset, ch)| start + offset + ch.len_utf8());
            }
            let previous_start = self.line_start(previous_end);
            let line = self.after_line_boms(previous_start, previous_end);
            // In compat, the line can still hold its mark, and a comment may follow that. The
            // stream's first line can hold a run of them, which reads as one mark does.
            let comment_after_mark = line
                .strip_prefix('\u{feff}')
                .map(|rest| {
                    if previous_start == 0 {
                        rest.trim_start_matches('\u{feff}')
                    } else {
                        rest
                    }
                })
                .is_some_and(|rest| rest.trim_start_matches(is_blank).starts_with('#'));
            let code = if comment_after_mark {
                ""
            } else {
                strip_comment(line, self.names())
            };
            if code.trim_matches(is_blank).is_empty() {
                end = previous_start;
                continue;
            }
            if code.starts_with('%') {
                return Owner::Unknown;
            }
            if code.starts_with("...") && code[3..].trim_matches(is_blank).is_empty() {
                return Owner::Root;
            }
            match self.shape_of_line(previous_start, code) {
                LineShape::Owner(owner) => return owner,
                LineShape::Open => end = previous_start,
                LineShape::Content => return Owner::Unknown,
            }
        }
    }

    pub(super) fn line_indent(&self, position: usize) -> usize {
        self.line_indent_at(self.line_start(position))
    }

    pub(super) fn line_indent_at(&self, line_start: usize) -> usize {
        let text = self.after_line_boms(line_start, self.input.len());
        // The spaces after a mark count only if it takes up a column. Otherwise, in compat, this
        // stops at the mark. By default, no mark is left here.
        match text.strip_prefix('\u{feff}') {
            Some(rest) if self.mark_takes_column(line_start, rest) => 1 + leading_spaces(rest),
            _ => leading_spaces(text),
        }
    }

    /// Whether the nearest non-empty line before `line_start` starts with a byte order mark that
    /// takes up a column.
    pub(super) fn previous_line_bom_takes_column(&self, line_start: usize) -> bool {
        let mut end = line_start;
        while end > 0 {
            let previous_start = self.line_start(end.saturating_sub(1));
            let line = &self.input[previous_start..end];
            let trimmed = line.trim_start_matches(is_blank);
            if trimmed.is_empty() {
                end = previous_start;
                continue;
            }
            let bom_start = previous_start + (line.len() - trimmed.len());
            return trimmed
                .strip_prefix('\u{feff}')
                .is_some_and(|rest| self.mark_takes_column(bom_start, rest));
        }
        false
    }

    /// The text of `start..end`, a line's start onward, past any byte order marks there. They
    /// take up no column, except in compat, which keeps libyaml's reading of them as a column.
    pub(super) fn after_line_boms(&self, start: usize, end: usize) -> &'a str {
        // Most lines hold no mark, and this runs for many of them, so the rest stays out of line.
        if !self.compat_flow && self.input.as_bytes().get(start) == Some(&BOM_FIRST_BYTE) {
            return self.after_bom_run(start, end);
        }
        &self.input[start..end]
    }

    /// The same for a line that may start with marks, reading a run of them once.
    #[cold]
    pub(super) fn after_bom_run(&self, start: usize, end: usize) -> &'a str {
        let (known_start, known_content) = self.bom_run.get();
        let content = if known_start == start {
            known_content
        } else {
            let trimmed = self.input[start..].trim_start_matches('\u{feff}');
            let content = self.input.len() - trimmed.len();
            self.bom_run.set((start, content));
            content
        };
        &self.input[content.min(end)..end]
    }

    pub(super) fn count_spaces(&self, position: usize) -> usize {
        self.input.as_bytes()[position..]
            .iter()
            .take_while(|&&byte| byte == b' ')
            .count()
    }

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the caller uses the shared fallible scanner API"
    )]
    pub(super) fn detect_block_indent(
        &self,
        content_start: usize,
        base_indent: usize,
        allow_base: bool,
    ) -> Result<usize, Box<Error>> {
        let mut position = content_start;
        while position < self.input.len() {
            let spaces = self.count_spaces(position);
            let after = position + spaces;
            let next = self.input[after..].chars().next();
            if next.is_none() {
                return Ok(base_indent + 1);
            }
            if let Some(ch) = next.filter(|ch| is_line_break(*ch)) {
                position = after + ch.len_utf8();
                continue;
            }
            if spaces < base_indent || (!allow_base && spaces == base_indent) {
                // A dedented line ends the scalar; it is not an error. This is
                // how an empty block scalar is represented.
                return Ok(base_indent + 1);
            }
            return Ok(spaces);
        }
        Ok(base_indent + 1)
    }

    pub(super) fn scan_plain(&mut self, limits: &Limits) -> Result<Option<Token<'a>>, Box<Error>> {
        let start = self.pos;
        let start_line_start = self.current_line_start;
        let start_has_block_prefix =
            !self.compat_flow && self.flow_level == 0 && self.line_has_block_prefix;
        let mut cached_base_indent = self.current_line_indent;
        self.short_flow_scalar = None;
        if self.input.as_bytes().get(start) == Some(&b'#') {
            return Err(self.error_at(
                start,
                ErrorCode::MissingCommentWhitespace,
                "a comment must be separated from the preceding token",
            ));
        }
        // Most plain scalars end on their own line. Defer the owner analysis
        // (which scans the line prefix) until a line break proves the scalar
        // may continue.
        let mut continuation = None;
        // Keep the common single-line scalar borrowed: only create an owned
        // value when a continuation is actually accepted.
        let mut value: Option<String> = None;
        loop {
            let content_start = self.pos;
            // This loop runs for every byte of a plain scalar, using a 256-byte stop table for
            // short runs and switching to 8-byte SWAR after an 8-byte warmup.
            let bytes = self.input.as_bytes();
            let in_flow = self.flow_level > 0;
            let stop = if in_flow {
                &PLAIN_STOP_FLOW
            } else {
                &PLAIN_STOP_BLOCK
            };
            let check_bom = !self.compat_flow;
            let mut position = content_start;
            while position < bytes.len() {
                let warmup_end = (position + 8).min(bytes.len());
                while position < warmup_end && !stop[bytes[position] as usize] {
                    position += 1;
                }
                if position == warmup_end {
                    position = if in_flow {
                        skip_plain_flow_swar(bytes, position)
                    } else {
                        skip_plain_block_swar(bytes, position)
                    };
                    while position < bytes.len() && !stop[bytes[position] as usize] {
                        position += 1;
                    }
                }
                if position >= bytes.len() {
                    break;
                }
                match bytes[position] {
                    b'\n' | b'\r' | b'[' | b']' | b'{' | b'}' | b',' => break,
                    b'#' => {
                        if self.is_blank_before(position) {
                            break;
                        }
                        position += 1;
                    }
                    b':' => {
                        if self.colon_indicator(position) {
                            break;
                        }
                        position += 1;
                    }
                    0x00..=0x1f | 0x7f => {
                        self.pos = position;
                        return Err(self.non_printable_error_at(position));
                    }
                    0xC2 => {
                        if is_quoted_only_non_printable(bytes, position) {
                            self.pos = position;
                            return Err(self.non_printable_error_at(position));
                        }
                        position += 1;
                    }
                    BOM_FIRST_BYTE => {
                        if check_bom
                            && bytes.get(position + 1) == Some(&0xBB)
                            && bytes.get(position + 2) == Some(&0xBF)
                        {
                            self.pos = position;
                            return Err(self.error_at(
                                position,
                                ErrorCode::InvalidBom,
                                "a byte order mark can only start a document",
                            ));
                        }
                        if is_quoted_only_non_printable(bytes, position) {
                            self.pos = position;
                            return Err(self.non_printable_error_at(position));
                        }
                        position += 1;
                    }
                    _ => {
                        position += 1;
                        while position < bytes.len()
                            && bytes[position] >= 0x80
                            && !matches!(bytes[position], 0xC2 | BOM_FIRST_BYTE)
                        {
                            position += 1;
                        }
                    }
                }
            }
            self.pos = position;
            if let Some(value) = value.as_mut() {
                let trimmed_end = self.trim_trailing_whitespace(content_start, self.pos);
                self.check_scalar_bytes_incremental(start, trimmed_end - start, limits)?;
                value.push_str(&self.input[content_start..trimmed_end]);
                while value.ends_with([' ', '\t']) {
                    value.pop();
                }
            }
            if !matches!(self.input.as_bytes().get(self.pos), Some(b'\n' | b'\r')) {
                break;
            }
            // A scalar that starts after a block prefix cannot continue on a
            // line at or below the prefix's indentation. Reject that common
            // case before scanning the line prefix for an owner.
            #[cfg(test)]
            let fast_continuation = self.fast_continuation;
            #[cfg(not(test))]
            let fast_continuation = true;
            if fast_continuation
                && start_has_block_prefix
                && self.next_content_indent().is_some_and(|indent| {
                    let base = *cached_base_indent
                        .get_or_insert_with(|| self.line_indent_at(start_line_start));
                    indent <= base
                })
            {
                break;
            }
            let continuation = if let Some(continuation) = continuation {
                continuation
            } else {
                let computed = self.plain_continuation(start);
                continuation = Some(computed);
                computed
            };
            let first_end = self.pos;
            let Some(breaks) = self.try_plain_continuation(continuation)? else {
                break;
            };
            self.check_scalar_bytes_incremental(start, self.pos - start, limits)?;
            if value.is_none() {
                let trimmed_first_end = self.trim_trailing_whitespace(start, first_end);
                let mut first = String::with_capacity(
                    trimmed_first_end
                        .saturating_sub(start)
                        .saturating_add(1)
                        .min(limits.max_scalar_bytes),
                );
                first.push_str(&self.input[start..trimmed_first_end]);
                value = Some(first);
            }
            let Some(value) = value.as_mut() else {
                break;
            };
            if breaks == 1 {
                value.push(' ');
            } else {
                for _ in 1..breaks {
                    value.push('\n');
                }
            }
        }
        // A plain scalar ends at its last non-blank character. The scanner still stops at the
        // separation, comment, line break, or flow indicator that ended it, so trimming here
        // changes only what the token and event report, not what comes next.
        let end = self.trim_trailing_whitespace(start, self.pos);
        if end == start {
            return Err(self.error_at(start, ErrorCode::UnexpectedToken, "expected a token"));
        }
        if let Some((_, line)) = self.short_flow_scalar {
            self.short_flow_scalar = Some((end, line));
        }
        let kind = match value {
            Some(value) => TokenKind::Scalar(Box::new(ScalarToken {
                value: Cow::Owned(value),
                style: ScalarStyle::Plain,
            })),
            None => TokenKind::PlainScalar,
        };
        self.make(kind, start, end, limits)
    }

    /// The indentation of the next line after the current position that holds more than white
    /// space, without changing scanner state. An empty line, whatever spaces or tabs it holds,
    /// doesn't end a scalar, so `a:\n  k: x\n \n   y` goes on to `y`.
    pub(super) fn next_content_indent(&self) -> Option<usize> {
        let bytes = self.input.as_bytes();
        let mut index = self.pos;
        let mut line_start = index;
        while index < bytes.len() {
            match bytes[index] {
                b'\n' => {
                    index += 1;
                    line_start = index;
                }
                b'\r' => {
                    index += 1;
                    if bytes.get(index) == Some(&b'\n') {
                        index += 1;
                    }
                    line_start = index;
                }
                b' ' | b'\t' => index += 1,
                _ => {
                    let mut spaces = 0;
                    while line_start + spaces < index && bytes[line_start + spaces] == b' ' {
                        spaces += 1;
                    }
                    return Some(spaces);
                }
            }
        }
        None
    }

    /// How a plain scalar starting at `start` may continue on later lines.
    pub(super) fn plain_continuation(&self, start: usize) -> Continuation {
        if self.flow_level > 0 {
            // Continuation lines inside flow collections are not measured
            // against block indentation. Analyzing the line prefix per flow
            // item would also be quadratic on a long single-line collection.
            return Continuation::Flow;
        }
        match self.owner_of(start) {
            // A root plain scalar continues at any indentation.
            Owner::Root => Continuation::Indent(0),
            // Inside a block collection indented by `n`, continuation lines
            // need at least `n + 1` spaces.
            Owner::Block(n) => Continuation::Indent(n + 1),
            Owner::Unknown => {
                let line_start = self.line_start(start);
                let prefix = &self.input[line_start..start];
                Continuation::Line {
                    start_indent: self.line_indent(start),
                    standalone: prefix.chars().all(|ch| is_blank(ch) || ch == '\u{feff}'),
                    allow_block_indicator: !prefix.contains(':'),
                }
            }
        }
    }

    pub(super) fn try_plain_continuation(
        &mut self,
        continuation: Continuation,
    ) -> Result<Option<usize>, Box<Error>> {
        let saved = self.pos;
        let saved_line_start = self.current_line_start;
        let saved_block_prefix = self.line_has_block_prefix;
        let saved_line_indent = self.current_line_indent;
        let restore = |scanner: &mut Self| {
            scanner.pos = saved;
            scanner.current_line_start = saved_line_start;
            scanner.line_has_block_prefix = saved_block_prefix;
            scanner.current_line_indent = saved_line_indent;
        };
        let mut breaks = 0_usize;
        // In a flow collection under a block collection, a line needs one more space than that
        // collection's column, and a tab doesn't count. Compat refuses only a tab left of that
        // column on a line that the scalar goes on to, as libyaml does, so `k: [a\n\tb]` is an
        // error in both modes and `k: [a\nb]` by default only. A closing line, as in `k: [a\n\t]`,
        // still reads.
        let flow_indent = match continuation {
            Continuation::Flow => self.flow_line_indent(),
            _ => None,
        };
        // An empty line that the scalar goes on past can't hold a tab before its indentation
        // (`l-empty`, YAML 1.2.2 production 70), as in `k:\n  a\n\t\n  b` and `k: [a\n\t\n b]`.
        // libyaml refuses it too.
        let empty_line_indent = match continuation {
            Continuation::Indent(minimum) => Some(minimum),
            Continuation::Flow => flow_indent,
            Continuation::Line { .. } => None,
        };
        let mut empty_line_tab = None;
        loop {
            if !matches!(self.input.as_bytes().get(self.pos), Some(b'\n' | b'\r')) {
                break;
            }
            self.consume_line_break();
            breaks += 1;
            let line_start = self.pos;
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.pos += 1;
            }
            if self.peek().is_some_and(is_line_break) {
                if empty_line_tab.is_none()
                    && let Some(required) = empty_line_indent
                    && let Some(tab) = self.input[line_start..self.pos].find('\t')
                    && tab < required
                {
                    empty_line_tab = Some(line_start + tab);
                }
                continue;
            }
            break;
        }
        if breaks == 0 || self.pos >= self.input.len() {
            restore(self);
            return Ok(None);
        }
        // In a flow collection, an indicator on the next line ends the scalar, and the line
        // breaks before it aren't part of the value.
        // A document marker line ends it too, and the flow collection then refuses the marker, as
        // in `[a\n--- b]`: no node holds one (YAML 1.2.2 production 206).
        if self.flow_level > 0
            && (matches!(self.peek(), Some(',' | ']' | '}'))
                || (self.peek() == Some(':') && self.colon_indicator(self.pos))
                || self.document_marker_starts_line(self.pos))
        {
            restore(self);
            return Ok(None);
        }
        let line_start = self.line_start(self.pos);
        let indent = self.input[line_start..self.pos]
            .chars()
            .take_while(|ch| *ch == ' ')
            .count();
        let bom_line = self.input[self.pos..].starts_with('\u{feff}');
        let (allowed, allow_block_indicator) = match continuation {
            // By default a short line ends the scalar, and the flow collection then refuses it.
            Continuation::Flow => (
                self.compat_flow || flow_indent.is_none_or(|required| indent >= required),
                false,
            ),
            Continuation::Indent(minimum) => (indent >= minimum, true),
            Continuation::Line {
                start_indent,
                standalone,
                allow_block_indicator,
            } => (
                if standalone {
                    indent >= start_indent
                } else {
                    indent > start_indent
                },
                allow_block_indicator,
            ),
        };
        // A line starting with a BOM ends the scalar by default; the BOM is
        // then a document prefix or an error. libyaml continues a scalar onto
        // it wherever the indentation allows, whatever the line holds, as
        // compat does: a colon on the line then makes a multi-line key, which
        // is an error, and a `#` after the BOM isn't a comment.
        if !allowed
            || (bom_line && !self.compat_flow)
            || (!bom_line
                && self.flow_level == 0
                && self.looks_like_block_start(self.pos, allow_block_indicator))
            || self.peek() == Some('#')
        {
            restore(self);
            return Ok(None);
        }
        if let Some(tab) = empty_line_tab {
            return Err(self.error_at(tab, ErrorCode::InvalidTabIndentation, EMPTY_LINE_TAB));
        }
        if let Some(required) = flow_indent
            && let Some(tab) = self.input[line_start..self.pos].find('\t')
            && tab < required
        {
            // Only compat gets here.
            return Err(self.error_at(
                line_start + tab,
                ErrorCode::InvalidTabIndentation,
                "a tab cannot indent a flow collection's line under a block collection",
            ));
        }
        if flow_indent.is_some_and(|required| indent < required) {
            // Only compat gets here, and `scan_plain` sets where the scalar ends.
            self.short_flow_scalar = Some((usize::MAX, self.pos));
        }
        Ok(Some(breaks))
    }

    /// Whether the line that `position` starts the content of ends a plain scalar rather than
    /// continuing it: a document marker, or block structure. A `---` or `...` is a marker only at
    /// column 0 with whitespace or the line's end after it, and a comment's `:` isn't an indicator.
    pub(super) fn looks_like_block_start(
        &self,
        position: usize,
        allow_block_indicator: bool,
    ) -> bool {
        let rest = &self.input[position..];
        if self.document_marker_starts_line(position) {
            return true;
        }
        if rest.starts_with(": ") {
            return true;
        }
        if !allow_block_indicator && (rest.starts_with("- ") || rest.starts_with("? ")) {
            return true;
        }
        let mut chars = rest.chars().peekable();
        let mut previous = self.input[..position].chars().next_back();
        while let Some(ch) = chars.next() {
            if is_line_break(ch) || (ch == '#' && previous.is_some_and(is_blank)) {
                break;
            }
            if ch == ':' && chars.peek().is_none_or(|next| is_blank(*next)) {
                return true;
            }
            previous = Some(ch);
        }
        false
    }

    pub(super) fn is_blank_before(&self, position: usize) -> bool {
        self.input[..position]
            .chars()
            .next_back()
            .is_some_and(is_blank)
    }

    /// Whether a closing `quote` appears in the remaining input, with escapes and `''` skipped.
    fn quoted_closes_from(&self, from: usize, quote: char) -> bool {
        let mut chars = self.input[from..].chars();
        while let Some(ch) = chars.next() {
            if quote == '"' && ch == '\\' {
                chars.next();
                continue;
            }
            if ch == quote {
                if quote == '\'' && chars.clone().next() == Some('\'') {
                    chars.next();
                    continue;
                }
                return true;
            }
        }
        false
    }

    pub(super) fn trim_trailing_whitespace(&self, start: usize, end: usize) -> usize {
        let bytes = self.input.as_bytes();
        let mut trimmed = end;
        while trimmed > start && matches!(bytes[trimmed - 1], b' ' | b'\t') {
            trimmed -= 1;
        }
        trimmed
    }
}
fn scalar_cow(value: String, candidate: &str) -> Cow<'_, str> {
    if value == candidate {
        Cow::Borrowed(candidate)
    } else {
        Cow::Owned(value)
    }
}

#[derive(Clone, Copy)]
pub(super) enum Chomping {
    Strip,
    Clip,
    Keep,
}

/// Fold the lines of a folded block scalar, given as `raw`'s lines between line breaks, without
/// collecting them: only the line before a run of empty lines and the one after it matter.
fn fold_block_scalar(raw: &str) -> String {
    let mut value = String::with_capacity(raw.len());
    let mut lines = raw.split('\n');
    let Some(mut line) = lines.next() else {
        return value;
    };
    loop {
        value.push_str(line);
        // The lines between this one and the next with text, and that line if there is one.
        let mut empty = 0;
        let mut next = None;
        let mut last = true;
        for candidate in lines.by_ref() {
            last = false;
            if candidate.is_empty() {
                empty += 1;
            } else {
                next = Some(candidate);
                break;
            }
        }
        if last {
            break;
        }
        let Some(next) = next else {
            value.extend(std::iter::repeat_n('\n', empty));
            break;
        };
        let breaks = empty + 1;
        let more_indented = line.starts_with([' ', '\t']) || next.starts_with([' ', '\t']);
        if line.is_empty() || more_indented {
            value.extend(std::iter::repeat_n('\n', breaks));
        } else if breaks == 1 {
            value.push(' ');
        } else {
            value.extend(std::iter::repeat_n('\n', breaks - 1));
        }
        line = next;
    }
    value
}

/// A block scalar's value from its lines, `text`, each with its line break if it had one, and
/// whether the last line with any text had one.
pub(super) fn build_block_scalar(
    text: String,
    final_break: bool,
    literal: bool,
    chomping: Chomping,
) -> String {
    let mut value = if literal {
        text
    } else {
        fold_block_scalar(&text)
    };
    match chomping {
        Chomping::Strip => {
            while value.ends_with('\n') {
                value.pop();
            }
        }
        Chomping::Clip => {
            while value.ends_with('\n') {
                value.pop();
            }
            if final_break && !value.is_empty() {
                value.push('\n');
            }
        }
        Chomping::Keep => {}
    }
    value
}

#[cfg(kani)]
mod kani_proofs {
    use super::{has_byte, has_non_ascii_or_ctrl, has_non_ascii_or_ctrl_or_del, has_zero_byte};

    #[kani::proof]
    fn verify_swar_byte_predicates() {
        let word: u64 = kani::any();
        let needle: u8 = kani::any();
        let bytes = word.to_ne_bytes();

        let ref_zero = bytes.contains(&0);
        let ref_needle = bytes.contains(&needle);
        let ref_non_ascii_or_ctrl = bytes.iter().any(|&b| b < 0x20 || b >= 0x80);
        let ref_non_ascii_or_ctrl_or_del = bytes.iter().any(|&b| b < 0x20 || b >= 0x7f);

        assert_eq!(has_zero_byte(word), ref_zero);
        assert_eq!(has_byte(word, needle), ref_needle);
        assert_eq!(has_non_ascii_or_ctrl(word), ref_non_ascii_or_ctrl);
        assert_eq!(
            has_non_ascii_or_ctrl_or_del(word),
            ref_non_ascii_or_ctrl_or_del
        );
    }
}
