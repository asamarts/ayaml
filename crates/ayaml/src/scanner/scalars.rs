use std::borrow::Cow;

use super::line::*;
use super::*;

impl<'a> Scanner<'a> {
    pub(super) fn scan_single_quoted(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        let mut held = None;
        self.pos += 1;
        let mut value = String::new();
        loop {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(start, "unterminated single-quoted scalar"));
            };
            match ch {
                '\'' => {
                    if self.peek_second() == Some('\'') {
                        value.push('\'');
                        self.pos += 2;
                    } else {
                        self.pos += 1;
                        break;
                    }
                }
                ch if is_line_break(ch) => {
                    self.scan_quoted_line_break(&mut value, '\'', start, &mut held, 0, false)?;
                }
                ch => {
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let raw_text = &self.input[start..self.pos];
        let raw = Cow::Borrowed(raw_text);
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::SingleQuoted,
            },
            start,
            self.pos,
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
        let line_start = self.line_index.line_start(start);
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
            && self.previous_line_starts_with_bom(line_start)
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
        let line_start = self.line_index.line_start(at);
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
    pub(super) fn flow_indent_error(&self, at: usize) -> Error {
        let error = self.error_at(at, "flow collection entries must be indented");
        let Some(&(open, opened, _)) = self.flow_stack.last() else {
            return error;
        };
        let name = if open == '[' { "sequence" } else { "mapping" };
        match self.line_index.marker(self.input, opened) {
            Some(there) => error.with_context(&format!(
                ", in the flow {name} opened at line {} column {}",
                there.line(),
                there.column()
            )),
            None => error,
        }
    }

    /// Fold the line breaks at the current position into a quoted scalar's `value`, and check the
    /// next line's indentation. `held` holds the scalar's `quoted_indent` once a line break has
    /// asked for it, so a scalar that ends on its line never works it out.
    pub(super) fn scan_quoted_line_break(
        &mut self,
        value: &mut String,
        quote: char,
        start: usize,
        held: &mut Option<(usize, bool)>,
        protected_len: usize,
        escaped: bool,
    ) -> Result<(), Error> {
        // White space before an escaped line break is content.
        while !escaped && value.len() > protected_len && value.ends_with([' ', '\t']) {
            value.pop();
        }
        let mut breaks = 0;
        loop {
            let line_start = self.pos;
            while matches!(self.peek(), Some(' ' | '\t')) {
                self.pos += 1;
            }
            if !self.peek().is_some_and(is_line_break) {
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
                    return Err(self.error_at(line_start + tab, EMPTY_LINE_TAB));
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
        let line_start = self.input[..self.pos]
            .rfind(['\n', '\r'])
            .map_or(0, |index| index + 1);
        let indentation = &self.input[line_start..self.pos];
        let indent = indentation
            .chars()
            .take_while(|ch| *ch == ' ' || *ch == '\t')
            .count();
        // A closing quote may sit at any indentation, as a closing `]` or `}` may, so tabs alone
        // can come before it. Only this scalar's quote closes it, and in a single-quoted scalar
        // `''` is a quote in the text.
        let closes =
            self.peek() == Some(quote) && (quote == '"' || self.peek_second() != Some('\''));
        if !allow_same_indent && !closes && indentation.contains('\t') && !indentation.contains(' ')
        {
            return Err(self.error_at(
                line_start,
                "tabs cannot be the only quoted-scalar indentation",
            ));
        }
        let rest = &self.input[self.pos..];
        if indent == 0
            && (rest.starts_with("---") || rest.starts_with("..."))
            && rest
                .get(3..)
                .is_none_or(|tail| tail.chars().next().is_none_or(is_blank))
        {
            return Err(self.error_at(self.pos, "document marker inside quoted scalar"));
        }
        // Only the closing quote is exempt: a line that starts with the other kind of quote, or
        // with `''` in a single-quoted scalar, is text and is held to the indent.
        if !closes && (indent < start_indent || (!allow_same_indent && indent == start_indent)) {
            return Err(self.error_at(
                self.pos,
                "quoted scalar continuation must be indented more than its parent",
            ));
        }
        // A folded break is a space; each empty line after it, or after an
        // escaped break, is a line feed (YAML 1.2.2 productions 112, 73).
        if breaks == 1 && !escaped {
            value.push(' ');
        } else {
            for _ in 1..breaks {
                value.push('\n');
            }
        }
        Ok(())
    }

    pub(super) fn scan_double_quoted(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        let mut held = None;
        self.pos += 1;
        let mut value = String::new();
        let mut protected_len = 0;
        loop {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(start, "unterminated double-quoted scalar"));
            };
            match ch {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    let escape_start = self.pos;
                    self.pos += 1;
                    if self.peek().is_some_and(is_line_break) {
                        self.scan_quoted_line_break(
                            &mut value,
                            '"',
                            start,
                            &mut held,
                            protected_len,
                            true,
                        )?;
                        continue;
                    }
                    let Some(escape) = self.peek() else {
                        return Err(self.error_at(escape_start, "unterminated escape"));
                    };
                    self.pos += escape.len_utf8();
                    let before = value.len();
                    match escape {
                        '0' => value.push('\0'),
                        'a' => value.push('\u{0007}'),
                        'b' => value.push('\u{0008}'),
                        't' | '\t' => value.push('\t'),
                        'n' => value.push('\n'),
                        'v' => value.push('\u{000b}'),
                        'f' => value.push('\u{000c}'),
                        'r' => value.push('\r'),
                        'e' => value.push('\u{001b}'),
                        ' ' => value.push(' '),
                        '"' => value.push('"'),
                        '/' => value.push('/'),
                        '\\' => value.push('\\'),
                        'N' => value.push('\u{0085}'),
                        '_' => value.push('\u{00a0}'),
                        'L' => value.push('\u{2028}'),
                        'P' => value.push('\u{2029}'),
                        'x' => value.push(self.scan_hex_escape(2, escape_start)?),
                        'u' => value.push(self.scan_hex_escape(4, escape_start)?),
                        'U' => value.push(self.scan_hex_escape(8, escape_start)?),
                        other => {
                            return Err(
                                self.error_at(escape_start, format!("unknown escape \\{other}"))
                            );
                        }
                    }
                    if value.len() > before && value.ends_with([' ', '\t']) {
                        protected_len = value.len();
                    }
                }
                ch if is_line_break(ch) => {
                    self.scan_quoted_line_break(
                        &mut value,
                        '"',
                        start,
                        &mut held,
                        protected_len,
                        false,
                    )?;
                }
                ch => {
                    value.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        let raw_text = &self.input[start..self.pos];
        let raw = Cow::Borrowed(raw_text);
        let value = scalar_cow(
            value,
            raw_text
                .get(1..raw_text.len().saturating_sub(1))
                .unwrap_or(""),
        );
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::DoubleQuoted,
            },
            start,
            self.pos,
        )
    }

    pub(super) fn scan_hex_escape(
        &mut self,
        digits: usize,
        escape_start: usize,
    ) -> Result<char, Error> {
        let mut value = 0_u32;
        for _ in 0..digits {
            let Some(ch) = self.peek() else {
                return Err(self.error_at(escape_start, "short hexadecimal escape"));
            };
            let Some(digit) = ch.to_digit(16) else {
                return Err(self.error_at(self.pos, "invalid hexadecimal escape"));
            };
            value = value * 16 + digit;
            self.pos += ch.len_utf8();
        }
        char::from_u32(value)
            .ok_or_else(|| self.error_at(escape_start, "escape is not a Unicode scalar value"))
    }

    pub(super) fn scan_block_scalar(
        &mut self,
        start: usize,
        literal: bool,
    ) -> Result<Option<Token<'a>>, Error> {
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
                    explicit_indent = Some(ch.to_digit(10).expect("digit") as usize);
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
                            "a block scalar header comment requires preceding whitespace",
                        ));
                    }
                    while let Some(ch) = self.peek() {
                        if is_line_break(ch) {
                            break;
                        }
                        // A comment holds no mark (production 75), here as elsewhere. Compat
                        // reads one, as libyaml does.
                        if ch == '\u{feff}' && !self.compat_flow {
                            return Err(self.error_at(
                                self.pos,
                                "a byte order mark can only start a document",
                            ));
                        }
                        self.pos += ch.len_utf8();
                    }
                    break;
                }
                Some(ch) if is_line_break(ch) => break,
                Some(ch) => {
                    return Err(self.error_at(
                        self.pos,
                        format!("unexpected {ch:?} in block scalar header"),
                    ));
                }
                None => break,
            }
        }
        if self.peek().is_some_and(|ch| !is_line_break(ch)) {
            return Err(self.error_at(start, "block scalar header must end at a line break"));
        }
        let base_indent = self.line_indent(start);
        let line_start = self.input[..start]
            .rfind(['\n', '\r'])
            .map_or(0, |index| index + 1);
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
                        "leading empty block-scalar lines are more indented than the content",
                    ));
                }
                let line_end = self.input[leading..]
                    .find(['\n', '\r'])
                    .map_or(self.input.len(), |offset| leading + offset);
                leading = line_end;
                if leading < self.input.len() {
                    let ch = self.input[leading..].chars().next().expect("line break");
                    leading += ch.len_utf8();
                    if ch == '\r' && self.input[leading..].starts_with('\n') {
                        leading += 1;
                    }
                }
            }
        }

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
                if let Some(ch) = next {
                    self.pos = after_spaces + ch.len_utf8();
                    if ch == '\r' && self.peek() == Some('\n') {
                        self.pos += 1;
                    }
                } else {
                    self.pos = after_spaces;
                }
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
                end = self.pos;
                continue;
            }
            if spaces < indent {
                let rest = &self.input[line_start..];
                if rest.starts_with('\t') && rest.trim_start_matches([' ', '\t']).starts_with('#') {
                    return Err(self.error_at(
                        line_start,
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
            while let Some(ch) = self.peek() {
                if is_line_break(ch) {
                    break;
                }
                if ch == '\u{feff}' && !self.compat_flow {
                    return Err(
                        self.error_at(self.pos, "a byte order mark can only start a document")
                    );
                }
                self.pos += ch.len_utf8();
            }
            // A content line holds at least the character that made it one.
            text.push_str(&self.input[text_start..self.pos]);
            let had_break = self.peek().is_some_and(is_line_break);
            if had_break {
                self.consume_line_break();
                text.push('\n');
            }
            final_break = had_break;
            seen_content = true;
            end = self.pos;
        }
        self.pos = end;

        let value = build_block_scalar(text, final_break, literal, chomping);
        let raw = Cow::Borrowed(&self.input[start..end]);
        self.make(
            TokenKind::Scalar {
                value: Cow::Owned(value),
                raw,
                style: if literal {
                    ScalarStyle::Literal
                } else {
                    ScalarStyle::Folded
                },
            },
            start,
            end,
        )
    }

    /// The block structure owning the node that starts at `start` (see
    /// [`Owner`]). Only meaningful in block context.
    pub(super) fn owner_of(&self, start: usize) -> Owner {
        let line_start = self.line_index.line_start(start);
        match self.shape_of_line(line_start, &self.input[line_start..start]) {
            LineShape::Owner(owner) => owner,
            LineShape::Open => self.owner_from_previous_lines(line_start),
            LineShape::Content => Owner::Unknown,
        }
    }

    /// What `text`, which starts the line at `line_start`, says about ownership. An owner after
    /// a mark that takes up a column is one deeper.
    pub(super) fn shape_of_line(&self, line_start: usize, text: &str) -> LineShape {
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
            if end == 0 || (end == 3 && self.initial_bom) {
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
                // A Unicode line break; the line index knows where it starts.
                let start = self.line_index.line_start(end.saturating_sub(1));
                previous_end = self.input[start..end]
                    .char_indices()
                    .rev()
                    .find(|(_, ch)| !is_line_break(*ch))
                    .map_or(start, |(offset, ch)| start + offset + ch.len_utf8());
            }
            let previous_start = self.line_index.line_start(previous_end);
            let line = self.after_line_boms(previous_start, previous_end);
            // In compat, the line can still hold its mark, and a comment may follow that.
            let comment_after_mark = line
                .strip_prefix('\u{feff}')
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
        let line_start = self.line_index.line_start(position);
        let text = self.after_line_boms(line_start, self.input.len());
        // The spaces after a mark count only if it takes up a column. Otherwise, in compat, this
        // stops at the mark. By default, no mark is left here.
        match text.strip_prefix('\u{feff}') {
            Some(rest) if self.mark_takes_column(line_start, rest) => 1 + leading_spaces(rest),
            _ => leading_spaces(text),
        }
    }

    /// Whether the nearest non-empty line before `line_start` starts with a byte order mark.
    pub(super) fn previous_line_starts_with_bom(&self, line_start: usize) -> bool {
        let mut end = line_start;
        while end > 0 {
            let previous_start = self.line_index.line_start(end.saturating_sub(1));
            let line = &self.input[previous_start..end];
            if line.trim_matches(is_blank).is_empty() {
                end = previous_start;
                continue;
            }
            return line.trim_start_matches(is_blank).starts_with('\u{feff}');
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
        self.input[position..]
            .chars()
            .take_while(|ch| *ch == ' ')
            .count()
    }

    pub(super) fn detect_block_indent(
        &self,
        content_start: usize,
        base_indent: usize,
        allow_base: bool,
    ) -> Result<usize, Error> {
        let mut position = content_start;
        while position < self.input.len() {
            let spaces = self.count_spaces(position);
            let after = position + spaces;
            let next = self.input[after..].chars().next();
            if next.is_none() {
                return Ok(base_indent + 1);
            }
            if next.is_some_and(is_line_break) {
                position = after + next.expect("checked").len_utf8();
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

    pub(super) fn scan_plain(&mut self) -> Result<Option<Token<'a>>, Error> {
        let start = self.pos;
        self.short_flow_scalar = None;
        if self.peek() == Some('#') {
            return Err(self.error_at(
                start,
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
            // This loop runs for every character of a plain scalar, so it walks the text once
            // rather than slicing the input again for each character.
            let text = &self.input[content_start..];
            let in_flow = self.flow_level > 0;
            let mut content_end = self.input.len();
            for (offset, ch) in text.char_indices() {
                let position = content_start + offset;
                if is_line_break(ch)
                    || (ch == '#' && self.is_blank_before(position))
                    || (in_flow && matches!(ch, '[' | ']' | '{' | '}' | ','))
                    || (ch == ':' && self.colon_indicator(position))
                {
                    content_end = position;
                    break;
                }
                if ch == '\u{feff}' && !self.compat_flow {
                    self.pos = position;
                    return Err(
                        self.error_at(position, "a byte order mark can only start a document")
                    );
                }
            }
            self.pos = content_end;
            if let Some(value) = value.as_mut() {
                value.push_str(&self.input[content_start..self.pos]);
                while value.ends_with([' ', '\t']) {
                    value.pop();
                }
            }
            if !self.peek().is_some_and(is_line_break) {
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
                && self.has_block_prefix(start)
                && self
                    .next_content_indent()
                    .is_some_and(|indent| indent <= self.line_indent(start))
            {
                break;
            }
            let continuation = match continuation {
                Some(continuation) => continuation,
                None => {
                    let computed = self.plain_continuation(start);
                    continuation = Some(computed);
                    computed
                }
            };
            let first_end = self.pos;
            let Some(breaks) = self.try_plain_continuation(continuation)? else {
                break;
            };
            if value.is_none() {
                let mut first = String::with_capacity(first_end.saturating_sub(start) + 1);
                first.push_str(&self.input[start..first_end]);
                while first.ends_with([' ', '\t']) {
                    first.pop();
                }
                value = Some(first);
            }
            let value = value.as_mut().expect("initialized");
            if breaks == 1 {
                value.push(' ');
            } else {
                for _ in 1..breaks {
                    value.push('\n');
                }
            }
        }
        let end = self.pos;
        if end == start {
            return Err(self.error_at(start, "expected a token"));
        }
        if let Some((_, line)) = self.short_flow_scalar {
            self.short_flow_scalar = Some((end, line));
        }
        let raw_text = &self.input[start..end];
        let raw = Cow::Borrowed(raw_text);
        let value = match value {
            Some(value) => Cow::Owned(value),
            None => Cow::Borrowed(&self.input[start..self.trim_trailing_whitespace(start, end)]),
        };
        self.make(
            TokenKind::Scalar {
                value,
                raw,
                style: ScalarStyle::Plain,
            },
            start,
            end,
        )
    }

    /// The indentation of the next line after the current position that holds more than white
    /// space, without changing scanner state. An empty line, whatever spaces or tabs it holds,
    /// doesn't end a scalar, so `a:\n  k: x\n \n   y` goes on to `y`.
    pub(super) fn next_content_indent(&self) -> Option<usize> {
        let mut index = self.pos;
        let mut line_start = index;
        while let Some(ch) = self.input[index..].chars().next() {
            if is_line_break(ch) {
                index += ch.len_utf8();
                if ch == '\r' && self.input[index..].starts_with('\n') {
                    index += 1;
                }
                line_start = index;
            } else if is_blank(ch) {
                index += 1;
            } else {
                return Some(leading_spaces(&self.input[line_start..index]));
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
                let line_start = self.line_index.line_start(start);
                let prefix = &self.input[line_start..start];
                Continuation::Line {
                    start_indent: self.line_indent(start),
                    standalone: prefix.chars().all(|ch| is_blank(ch) || ch == '\u{feff}'),
                    allow_block_indicator: !prefix.contains(':'),
                }
            }
        }
    }

    /// Whether a block indicator or a `:` value indicator comes before `start` on its line,
    /// outside compat and flow collections. A `: ` in a node property, as in `&a: b`, is part of
    /// its name. A scalar asks once for each of its lines, and this reads the text once.
    pub(super) fn has_block_prefix(&mut self, start: usize) -> bool {
        if self.compat_flow || self.flow_level > 0 {
            return false;
        }
        if self.block_prefix.0 != start {
            let prefix = &self.input[self.line_index.line_start(start)..start];
            let (text, index, owner) = block_prefix(prefix, false);
            let found = matches!(owner, Some(Owner::Block(_)))
                || matches!(walk_line(&text[index..], self.names()), LineStop::Value(_));
            self.block_prefix = (start, found);
        }
        self.block_prefix.1
    }

    pub(super) fn try_plain_continuation(
        &mut self,
        continuation: Continuation,
    ) -> Result<Option<usize>, Error> {
        let saved = self.pos;
        let mut breaks = 0;
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
            if !self.peek().is_some_and(is_line_break) {
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
            self.pos = saved;
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
            self.pos = saved;
            return Ok(None);
        }
        let line_start = self.line_index.line_start(self.pos);
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
            self.pos = saved;
            return Ok(None);
        }
        if let Some(tab) = empty_line_tab {
            return Err(self.error_at(tab, EMPTY_LINE_TAB));
        }
        if let Some(required) = flow_indent
            && let Some(tab) = self.input[line_start..self.pos].find('\t')
            && tab < required
        {
            // Only compat gets here.
            return Err(self.error_at(
                line_start + tab,
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

    pub(super) fn trim_trailing_whitespace(&self, start: usize, end: usize) -> usize {
        let mut trimmed = end;
        while trimmed > start {
            let ch = self.input[..trimmed].chars().next_back().expect("nonempty");
            if ch == ' ' || ch == '\t' {
                trimmed -= ch.len_utf8();
            } else {
                break;
            }
        }
        trimmed
    }
}
fn scalar_cow<'a>(value: String, candidate: &'a str) -> Cow<'a, str> {
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
