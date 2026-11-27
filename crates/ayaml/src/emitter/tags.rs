use std::borrow::Cow;
use std::fmt::Write as _;
use std::io::Write;

use super::{Output, STACK_MEDIUM, STACK_SMALL, TieredStack};
use crate::error::{Error, ErrorCode, ErrorKind};
use crate::tag::{Tag, TaggedValue, is_uri_byte, verbatim_reads_back};
use crate::value::Value;

/// Which of the default tag handles a document redefines with `%TAG`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RedefinedHandles {
    /// `%TAG ! ...` is in effect.
    pub(crate) primary: bool,
    /// `%TAG !! ...` is in effect.
    pub(crate) secondary: bool,
}

/// Borrowed classification of how a tag is written (D82), avoiding string allocations before
/// output limits are checked.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TagFormRef<'a> {
    /// Non-specific `!`.
    NonSpecific,
    /// Primary `!` shorthand whose suffix is `rest` followed by `suffix` (both `%`-escaped).
    Primary { rest: &'a str, suffix: &'a str },
    /// Secondary `!!` shorthand whose suffix is `%`-escaped.
    Secondary { suffix: &'a str },
    /// Verbatim `!<handle + suffix>`.
    Verbatim { handle: &'a str, suffix: &'a str },
    /// Named `%TAG` handle shorthand whose directive maps to `prefix` and uses `suffix`.
    Directive { prefix: &'a str, suffix: &'a str },
}

impl<'a> TagFormRef<'a> {
    /// Classify `tag` in a document that redefines the handles in `redefined` without allocating.
    pub(crate) fn of(tag: &'a Tag, redefined: RedefinedHandles) -> Result<Self, Error> {
        let (handle, suffix) = (tag.handle(), tag.suffix());
        // The non-specific tag `!` is not a shorthand.
        if tag.is_non_specific() {
            return Ok(Self::NonSpecific);
        }
        if let Some(rest) = handle.strip_prefix('!')
            && !redefined.primary
        {
            return Ok(Self::Primary { rest, suffix });
        }
        if tag.is_core() && !suffix.is_empty() && !redefined.secondary {
            return Ok(Self::Secondary { suffix });
        }
        if verbatim_reads_back(handle, suffix) {
            return Ok(Self::Verbatim { handle, suffix });
        }
        if !handle.is_empty() && !suffix.is_empty() {
            return Ok(Self::Directive {
                prefix: handle,
                suffix,
            });
        }
        // A name kept whole: split it after its last `:`, `/` or `#`, or else before its last
        // character, so that both parts hold one.
        let name = if handle.is_empty() { suffix } else { handle };
        match name.char_indices().next_back() {
            Some((last, _)) if last > 0 => {
                let cut = name[..last]
                    .rfind([':', '/', '#'])
                    .map_or(last, |index| index + 1);
                Ok(Self::Directive {
                    prefix: &name[..cut],
                    suffix: &name[cut..],
                })
            }
            _ => Err(Error::coded(
                ErrorKind::Emitter,
                ErrorCode::UnsupportedTag,
                format!("YAML has no way to write the tag {name:?}"),
            )),
        }
    }
}

/// Whether `tag` takes a `%TAG` directive, or has no form, in a document that redefines no handle:
/// [`TagFormRef::of`] without building the token.
pub(super) fn needs_directive(tag: &Tag) -> bool {
    let (handle, suffix) = (tag.handle(), tag.suffix());
    !(handle.starts_with('!')
        || (tag.is_core() && !suffix.is_empty())
        || verbatim_reads_back(handle, suffix))
}

/// %-escape `text` for a tag shorthand's suffix or a `%TAG` prefix, whose escapes the parser
/// decodes: every byte but [`is_uri_byte`]'s, so a literal `%` is written `%25`.
fn escape_tag(text: &str, output: &mut String) {
    for byte in text.bytes() {
        if is_uri_byte(byte) {
            output.push(char::from(byte));
        } else {
            let _ = write!(output, "%{byte:02X}");
        }
    }
}

/// Exact byte (and char, since output is pure ASCII) length of `escape_tag(text)`.
fn escaped_tag_len(text: &str) -> usize {
    let mut len = 0usize;
    for byte in text.bytes() {
        len = len.saturating_add(if is_uri_byte(byte) { 1 } else { 3 });
    }
    len
}

/// Exact byte (and char) length of `escape_tag(text)`, capped at `cap`.
fn escaped_tag_len_capped(text: &str, cap: usize) -> usize {
    let mut len = 0usize;
    for byte in text.bytes() {
        len = len.saturating_add(if is_uri_byte(byte) { 1 } else { 3 });
        if len >= cap {
            return cap;
        }
    }
    len
}

/// Stream %-escaped `text` directly to `output` without allocating an intermediate `String`.
fn write_escaped_tag<W: Write>(text: &str, output: &mut Output<'_, W>) -> Result<(), Error> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let bytes = text.as_bytes();
    let mut run_start = 0usize;
    let mut index = 0usize;
    let mut esc_buf = [b'%', 0, 0];
    while index < bytes.len() {
        let byte = bytes[index];
        if is_uri_byte(byte) {
            index += 1;
            continue;
        }
        if index > run_start {
            output.push_str(&text[run_start..index])?;
        }
        esc_buf[1] = HEX[(byte >> 4) as usize];
        esc_buf[2] = HEX[(byte & 0x0f) as usize];
        let esc = std::str::from_utf8(&esc_buf).unwrap_or("");
        output.push_str(esc)?;
        index += 1;
        run_start = index;
    }
    if run_start < bytes.len() {
        output.push_str(&text[run_start..])?;
    }
    Ok(())
}

/// Exact byte length of one `%TAG <handle> <escaped_prefix>\n` directive line.
fn directive_line_byte_len(handle: &str, prefix: &str) -> usize {
    let prefix_len = match prefix.strip_prefix('!') {
        Some(rest) => 1usize.saturating_add(escaped_tag_len(rest)),
        None => escaped_tag_len(prefix),
    };
    // "%TAG " (5) + handle + " " (1) + prefix_len + "\n" (1)
    7usize
        .saturating_add(handle.len())
        .saturating_add(prefix_len)
}

/// The `%TAG` directives that the writer adds to one document, for the tags whose form is a
/// directive. Each handle is `!tN!`, numbered in order of first use, skipping any handle that the
/// document declares itself.
#[derive(Debug, Default)]
pub(crate) struct TagDirectives {
    /// The handles added so far, kept apart so that a document that adds none allocates nothing.
    added: Option<Box<AddedHandles>>,
    /// Named handles that the document's own directives declare.
    declared: std::collections::HashSet<String, crate::mapping::GlobalHasher>,
}

#[derive(Debug, Default)]
struct AddedHandles {
    /// Each added handle and the prefix it stands for, in order.
    handles: Vec<(String, String)>,
    /// The index in `handles` of each prefix.
    by_prefix: std::collections::BTreeMap<String, usize>,
    /// The number of the next handle to try: one past the last one added, which may have skipped
    /// declared handles.
    next: usize,
    /// Cumulative byte length of all `%TAG` lines in `handles`.
    lines_bytes: usize,
}

enum ScanFrame<'v> {
    Sequence(std::slice::Iter<'v, Value>),
    Mapping {
        iter: crate::mapping::Iter<'v>,
        pending_value: Option<&'v Value>,
    },
}

impl TagDirectives {
    /// No directives yet, in a document whose own directives declare `declared`.
    pub(crate) fn declaring(declared: Vec<String>) -> Self {
        Self {
            declared: declared.into_iter().collect(),
            ..Self::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_none()
    }

    /// Exact byte length of all added `%TAG` directive lines.
    pub(crate) fn lines_byte_len(&self) -> usize {
        self.added.as_ref().map_or(0, |added| added.lines_bytes)
    }

    /// The handle that stands for `prefix`, added if there is none yet, verifying that the
    /// cumulative directive lines fit within `output`'s remaining `max_output_bytes` before
    /// copying `prefix`.
    fn handle_for_checked<W: Write>(
        &mut self,
        prefix: &str,
        output: &Output<'_, W>,
    ) -> Result<&str, Error> {
        if let Some(index) = self
            .added
            .as_ref()
            .and_then(|added| added.by_prefix.get(prefix).copied())
        {
            return Ok(self
                .added
                .as_ref()
                .and_then(|added| added.handles.get(index))
                .map_or("", |(handle, _)| handle.as_str()));
        }
        let existing_bytes = self.lines_byte_len();
        let mut next = self.added.as_ref().map_or(0, |added| added.next);
        let handle = loop {
            let candidate = format!("!t{next}!");
            next += 1;
            if !self.declared.contains(&candidate) {
                break candidate;
            }
        };
        let line_len = directive_line_byte_len(&handle, prefix);
        output.check_additional(existing_bytes.saturating_add(line_len))?;

        let added = self.added.get_or_insert_with(Box::default);
        added.next = next;
        added.lines_bytes = added.lines_bytes.saturating_add(line_len);
        added.handles.push((handle, prefix.to_owned()));
        let index = added.handles.len() - 1;
        added.by_prefix.insert(prefix.to_owned(), index);
        Ok(&added.handles[index].0)
    }

    /// Stream `tag` directly to `output` in its form, adding a directive for it if it needs one.
    pub(crate) fn write_tag<W: Write>(
        &mut self,
        tag: &Tag,
        redefined: RedefinedHandles,
        output: &mut Output<'_, W>,
    ) -> Result<(), Error> {
        match TagFormRef::of(tag, redefined)? {
            TagFormRef::NonSpecific => output.push_str("!"),
            TagFormRef::Primary { rest, suffix } => {
                output.push_str("!")?;
                write_escaped_tag(rest, output)?;
                write_escaped_tag(suffix, output)
            }
            TagFormRef::Secondary { suffix } => {
                output.push_str("!!")?;
                write_escaped_tag(suffix, output)
            }
            TagFormRef::Verbatim { handle, suffix } => {
                output.push_str("!<")?;
                output.push_str(handle)?;
                output.push_str(suffix)?;
                output.push_str(">")
            }
            TagFormRef::Directive { prefix, suffix } => {
                // `handle_for_checked` borrows `output` immutably and returns a handle slice tied
                // to `self`, so copy the short `!tN!` handle (typically 4..24 bytes) before
                // writing to `output`.
                let handle = self.handle_for_checked(prefix, output)?.to_owned();
                output.push_str(&handle)?;
                write_escaped_tag(suffix, output)
            }
        }
    }

    /// Character length of `tag`'s written form capped at `cap`, without allocating the tag token,
    /// while validating the tag and registering any required `%TAG` directive within `output`'s
    /// remaining byte budget.
    pub(crate) fn token_char_len_capped<W: Write>(
        &mut self,
        tag: &Tag,
        redefined: RedefinedHandles,
        cap: usize,
        output: &Output<'_, W>,
    ) -> Result<usize, Error> {
        let len = match TagFormRef::of(tag, redefined)? {
            TagFormRef::NonSpecific => 1,
            TagFormRef::Primary { rest, suffix } => {
                let first = 1usize.saturating_add(escaped_tag_len_capped(rest, cap));
                if first >= cap {
                    cap
                } else {
                    first.saturating_add(escaped_tag_len_capped(suffix, cap - first))
                }
            }
            TagFormRef::Secondary { suffix } => {
                2usize.saturating_add(escaped_tag_len_capped(suffix, cap.saturating_sub(2)))
            }
            TagFormRef::Verbatim { handle, suffix } => {
                let mut total = 3usize;
                for ch in handle.chars().chain(suffix.chars()) {
                    let _ = ch;
                    total = total.saturating_add(1);
                    if total >= cap {
                        return Ok(cap);
                    }
                }
                total
            }
            TagFormRef::Directive { prefix, suffix } => {
                let handle_len = self.handle_for_checked(prefix, output)?.len();
                if handle_len >= cap {
                    cap
                } else {
                    handle_len.saturating_add(escaped_tag_len_capped(suffix, cap - handle_len))
                }
            }
        };
        Ok(len.min(cap))
    }

    /// Stream the directives' lines directly to `output` without allocating an intermediate string.
    pub(crate) fn write_lines<W: Write>(&self, output: &mut Output<'_, W>) -> Result<(), Error> {
        for (handle, prefix) in self.added.iter().flat_map(|added| &added.handles) {
            output.push_str("%TAG ")?;
            output.push_str(handle)?;
            output.push_str(" ")?;
            // A local prefix keeps its `!`; a global one may start with any other character,
            // escaped.
            match prefix.strip_prefix('!') {
                Some(rest) => {
                    output.push_str("!")?;
                    write_escaped_tag(rest, output)?;
                }
                None => write_escaped_tag(prefix, output)?,
            }
            output.push_str("\n")?;
        }
        Ok(())
    }

    /// Append the directives' lines into a pre-budgeted `String` (used when splicing held document
    /// headers in `event_emitter`).
    pub(crate) fn push_lines_into(&self, output: &mut String) {
        for (handle, prefix) in self.added.iter().flat_map(|added| &added.handles) {
            output.push_str("%TAG ");
            output.push_str(handle);
            output.push(' ');
            match prefix.strip_prefix('!') {
                Some(rest) => {
                    output.push('!');
                    escape_tag(rest, output);
                }
                None => escape_tag(prefix, output),
            }
            output.push('\n');
        }
    }

    /// Add a directive for each tag in `value` that needs one, in the order the writer meets them,
    /// or fail at the first tag that can't be written, as the writer would: one with no form,
    /// and a tag of the core schema on a scalar that it doesn't take. The walk stops where the
    /// writer does, at a collection past `max_depth` and at a tag on a tagged value, using an
    /// explicit O(depth) heap stack rather than native recursion. It runs before a document's
    /// first line, so a tag is refused before any of the document is written.
    pub(super) fn scan<W: Write>(
        &mut self,
        value: &Value,
        max_depth: usize,
        output: &Output<'_, W>,
    ) -> Result<(), Error> {
        if may_hold_a_tag(value) {
            self.scan_iterative(value, max_depth, output)?;
        }
        Ok(())
    }

    /// Add the directive that a tag needs, apart from the walk so that the walk's loop stays small.
    #[cold]
    #[inline(never)]
    fn add_tag<W: Write>(
        &mut self,
        tagged: &TaggedValue,
        output: &Output<'_, W>,
    ) -> Result<(), Error> {
        match TagFormRef::of(&tagged.tag, RedefinedHandles::default())? {
            TagFormRef::Directive { prefix, .. } => {
                self.handle_for_checked(prefix, output)?;
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// Iterative depth-first traversal matching the writer's tag order and `max_depth` cutoff,
    /// using a tight small-stack fast path for typical depths (`<= STACK_SMALL`), promoting once
    /// to a medium inline buffer up to `MAX_DEPTH_CAP`, and spilling to the heap beyond.
    fn scan_iterative<W: Write>(
        &mut self,
        root: &Value,
        max_depth: usize,
        output: &Output<'_, W>,
    ) -> Result<(), Error> {
        let mut small = [const { None }; STACK_SMALL];
        let mut len = 0_usize;
        let mut current = root;
        let mut depth_left = max_depth;
        loop {
            loop {
                match current {
                    Value::Tagged(tagged) => {
                        if matches!(tagged.value, Value::Tagged(_)) {
                            break;
                        }
                        if tagged.tag.is_core()
                            && let Some(refusal) = core_tag_refusal(&tagged.tag, &tagged.value)
                        {
                            return Err(refusal);
                        }
                        if needs_directive(&tagged.tag) {
                            self.add_tag(tagged, output)?;
                        }
                        current = &tagged.value;
                    }
                    Value::Sequence(items) if depth_left > 0 => {
                        let mut iter = items.iter();
                        if let Some(next) = iter.by_ref().find(|item| may_hold_a_tag(item)) {
                            let frame = ScanFrame::Sequence(iter);
                            current = next;
                            depth_left -= 1;
                            if len < STACK_SMALL {
                                small[len] = Some(frame);
                                len += 1;
                                continue;
                            }
                            return self.scan_iterative_deep(
                                current, depth_left, &mut small, len, frame, output,
                            );
                        }
                        break;
                    }
                    Value::Mapping(mapping) if depth_left > 0 => {
                        let mut iter = mapping.iter();
                        let mut found = None;
                        for (key, val) in iter.by_ref() {
                            if may_hold_a_tag(key) {
                                found = Some((key, may_hold_a_tag(val).then_some(val)));
                                break;
                            }
                            if may_hold_a_tag(val) {
                                found = Some((val, None));
                                break;
                            }
                        }
                        if let Some((next, pending_value)) = found {
                            let frame = ScanFrame::Mapping {
                                iter,
                                pending_value,
                            };
                            current = next;
                            depth_left -= 1;
                            if len < STACK_SMALL {
                                small[len] = Some(frame);
                                len += 1;
                                continue;
                            }
                            return self.scan_iterative_deep(
                                current, depth_left, &mut small, len, frame, output,
                            );
                        }
                        break;
                    }
                    _ => break,
                }
            }
            let mut next_node = None;
            while len > 0 {
                if let Some(frame) = small[len - 1].as_mut()
                    && let Some(child) = advance_scan_frame(frame)
                {
                    next_node = Some(child);
                    break;
                }
                len -= 1;
                small[len] = None;
                depth_left += 1;
            }
            let Some(next) = next_node else {
                return Ok(());
            };
            current = next;
        }
    }

    #[cold]
    #[inline(never)]
    fn scan_iterative_deep<'v, W: Write>(
        &mut self,
        current: &'v Value,
        depth_left: usize,
        small: &mut [Option<ScanFrame<'v>>],
        small_len: usize,
        pending: ScanFrame<'v>,
        output: &Output<'_, W>,
    ) -> Result<(), Error> {
        let mut medium = [const { None }; STACK_MEDIUM];
        let mut stack = TieredStack::from_promoted(small, &mut medium, small_len, pending);
        self.scan_loop(current, depth_left, &mut stack, output)
    }

    fn scan_loop<'v, W: Write>(
        &mut self,
        mut current: &'v Value,
        mut depth_left: usize,
        stack: &mut TieredStack<'_, '_, ScanFrame<'v>>,
        output: &Output<'_, W>,
    ) -> Result<(), Error> {
        loop {
            loop {
                match current {
                    Value::Tagged(tagged) => {
                        if matches!(tagged.value, Value::Tagged(_)) {
                            break;
                        }
                        if tagged.tag.is_core()
                            && let Some(refusal) = core_tag_refusal(&tagged.tag, &tagged.value)
                        {
                            return Err(refusal);
                        }
                        if needs_directive(&tagged.tag) {
                            self.add_tag(tagged, output)?;
                        }
                        current = &tagged.value;
                    }
                    Value::Sequence(items) if depth_left > 0 => {
                        let mut iter = items.iter();
                        if let Some(next) = iter.by_ref().find(|item| may_hold_a_tag(item)) {
                            stack.push(ScanFrame::Sequence(iter));
                            current = next;
                            depth_left -= 1;
                            continue;
                        }
                        break;
                    }
                    Value::Mapping(mapping) if depth_left > 0 => {
                        let mut iter = mapping.iter();
                        let mut found = None;
                        for (key, val) in iter.by_ref() {
                            if may_hold_a_tag(key) {
                                found = Some((key, may_hold_a_tag(val).then_some(val)));
                                break;
                            }
                            if may_hold_a_tag(val) {
                                found = Some((val, None));
                                break;
                            }
                        }
                        if let Some((next, pending_value)) = found {
                            stack.push(ScanFrame::Mapping {
                                iter,
                                pending_value,
                            });
                            current = next;
                            depth_left -= 1;
                            continue;
                        }
                        break;
                    }
                    _ => break,
                }
            }
            let Some(next) = next_scan_child(stack, &mut depth_left) else {
                return Ok(());
            };
            current = next;
        }
    }
}

#[inline]
fn advance_scan_frame<'v>(frame: &mut ScanFrame<'v>) -> Option<&'v Value> {
    match frame {
        ScanFrame::Sequence(iter) => iter.by_ref().find(|item| may_hold_a_tag(item)),
        ScanFrame::Mapping {
            iter,
            pending_value,
        } => {
            if let Some(val) = pending_value.take() {
                return Some(val);
            }
            for (key, val) in iter.by_ref() {
                if may_hold_a_tag(key) {
                    *pending_value = may_hold_a_tag(val).then_some(val);
                    return Some(key);
                }
                if may_hold_a_tag(val) {
                    return Some(val);
                }
            }
            None
        }
    }
}

fn next_scan_child<'v>(
    stack: &mut TieredStack<'_, '_, ScanFrame<'v>>,
    depth_left: &mut usize,
) -> Option<&'v Value> {
    while let Some(frame) = stack.last_mut() {
        if let Some(child) = advance_scan_frame(frame) {
            return Some(child);
        }
        stack.pop();
        *depth_left += 1;
    }
    None
}

/// Whether `value` is a tag or a collection, which the scan for directives looks into: every other
/// value is a scalar, which it can pass without a call.
fn may_hold_a_tag(value: &Value) -> bool {
    matches!(
        value,
        Value::Tagged(_) | Value::Sequence(_) | Value::Mapping(_)
    )
}

/// The error for a tag of the core schema on a scalar that the tag doesn't take, or `None`.
///
/// A reader resolves a scalar under `!!int`, `!!float`, `!!bool`, or `!!null` by that tag's
/// rule, whatever style the scalar is written in, and refuses one that doesn't fit: `!!int x`
/// has no reader. This asks the reader's own rule about the text that the payload is written
/// as, so the two can't drift apart. Every other pairing has a reader: `!!str` takes any
/// scalar, a collection keeps its kind beneath any core tag, and a name that the core schema
/// doesn't define is the application's.
pub(super) fn core_tag_refusal(tag: &Tag, value: &Value) -> Option<Error> {
    if !tag.is_core() || !matches!(tag.suffix(), "int" | "float" | "bool" | "null") {
        return None;
    }
    let text: Cow<'_, str> = match value {
        // A null beneath a tag is written as no text at all.
        Value::Null => Cow::Borrowed(""),
        Value::Bool(value) => Cow::Borrowed(if *value { "true" } else { "false" }),
        Value::Number(number) => Cow::Owned(number.to_string()),
        Value::String(text) => Cow::Borrowed(text),
        Value::Sequence(_) | Value::Mapping(_) | Value::Tagged(_) => return None,
    };
    crate::resolver::resolve_explicit_core(&text, tag.suffix(), tag)
        .err()
        .map(|_| {
            Error::coded(
                ErrorKind::Emitter,
                ErrorCode::UnsupportedTag,
                format!(
                    "the tag !!{} cannot be written on {}: no reader takes {text:?} as one",
                    tag.suffix(),
                    value_kind(value)
                ),
            )
        })
}

fn value_kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "a null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "this number",
        Value::String(_) => "this string",
        Value::Sequence(_) => "a sequence",
        Value::Mapping(_) => "a mapping",
        Value::Tagged(_) => "a tagged value",
    }
}
