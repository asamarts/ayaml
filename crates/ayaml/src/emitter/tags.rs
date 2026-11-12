use std::fmt::Write as _;

use crate::error::{Error, ErrorKind};
use crate::tag::{is_uri_byte, verbatim_reads_back};
use crate::value::Value;

/// Which of the default tag handles a document redefines with `%TAG`.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct RedefinedHandles {
    /// `%TAG ! ...` is in effect.
    pub(crate) primary: bool,
    /// `%TAG !! ...` is in effect.
    pub(crate) secondary: bool,
}

/// How a tag is written (D82).
#[derive(Debug)]
pub(crate) enum TagForm {
    /// As it stands: `!`, a `!` or `!!` shorthand, or a verbatim tag.
    Token(String),
    /// As a shorthand under a `%TAG` directive that the writer adds to the document: the prefix
    /// that the directive's handle stands for, and the suffix after the handle.
    Directive { prefix: String, suffix: String },
}

impl TagForm {
    /// The form of `tag` in a document that redefines the handles in `redefined`.
    ///
    /// A local tag uses `!` and a core tag `!!`, unless the document redefines that handle. Any
    /// other tag is written verbatim (`!<...>`) when that reads back as the same name: a verbatim
    /// tag is read as written, so it can hold only URI characters, and only a `%` that starts an
    /// escape, and only a local name or one with a URI scheme is a tag name there. Every other tag
    /// gets a `%TAG` directive, whose prefix and shorthand suffix are read with their escapes
    /// decoded, so they can hold any text. A tag of fewer than two characters that is neither local
    /// nor written verbatim has no form: both parts of a directive's shorthand hold a character.
    pub(crate) fn of(tag: &crate::tag::Tag, redefined: RedefinedHandles) -> Result<Self, Error> {
        let (handle, suffix) = (tag.handle(), tag.suffix());
        // The non-specific tag `!` is not a shorthand.
        if tag.is_non_specific() {
            return Ok(Self::Token("!".to_owned()));
        }
        if let Some(rest) = handle.strip_prefix('!')
            && !redefined.primary
        {
            let mut output = String::with_capacity(handle.len() + suffix.len());
            output.push('!');
            escape_tag(rest, &mut output);
            escape_tag(suffix, &mut output);
            return Ok(Self::Token(output));
        }
        if tag.is_core() && !suffix.is_empty() && !redefined.secondary {
            let mut output = String::with_capacity(suffix.len() + 2);
            output.push_str("!!");
            escape_tag(suffix, &mut output);
            return Ok(Self::Token(output));
        }
        if verbatim_reads_back(handle, suffix) {
            return Ok(Self::Token(format!("!<{handle}{suffix}>")));
        }
        if !handle.is_empty() && !suffix.is_empty() {
            return Ok(Self::Directive {
                prefix: handle.to_owned(),
                suffix: suffix.to_owned(),
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
                    prefix: name[..cut].to_owned(),
                    suffix: name[cut..].to_owned(),
                })
            }
            _ => Err(Error::new(
                ErrorKind::Emitter,
                format!("YAML has no way to write the tag {name:?}"),
            )),
        }
    }
}

/// Whether `tag` takes a `%TAG` directive, or has no form, in a document that redefines no handle:
/// [`TagForm::of`] without building the token.
pub(super) fn needs_directive(tag: &crate::tag::Tag) -> bool {
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

/// The `%TAG` directives that the writer adds to one document, for the tags whose [`TagForm`] is a
/// directive. Each handle is `!tN!`, numbered in order of first use, skipping any handle that the
/// document declares itself.
#[derive(Debug, Default)]
pub(crate) struct TagDirectives {
    /// The handles added so far, kept apart so that a document that adds none allocates nothing.
    added: Option<Box<AddedHandles>>,
    /// Named handles that the document's own directives declare.
    declared: Vec<String>,
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
}

impl TagDirectives {
    /// No directives yet, in a document whose own directives declare `declared`.
    pub(crate) fn declaring(declared: Vec<String>) -> Self {
        Self {
            declared,
            ..Self::default()
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.added.is_none()
    }

    /// The handle that stands for `prefix`, added if there is none yet.
    pub(crate) fn handle_for(&mut self, prefix: &str) -> &str {
        let added = self.added.get_or_insert_with(Box::default);
        let index = if let Some(&index) = added.by_prefix.get(prefix) {
            index
        } else {
            let handle = loop {
                let handle = format!("!t{}!", added.next);
                added.next += 1;
                if !self.declared.contains(&handle) {
                    break handle;
                }
            };
            added.handles.push((handle, prefix.to_owned()));
            added
                .by_prefix
                .insert(prefix.to_owned(), added.handles.len() - 1);
            added.handles.len() - 1
        };
        &added.handles[index].0
    }

    /// Write `tag` in its form, adding a directive for it if it needs one.
    pub(crate) fn token(
        &mut self,
        tag: &crate::tag::Tag,
        redefined: RedefinedHandles,
    ) -> Result<String, Error> {
        Ok(match TagForm::of(tag, redefined)? {
            TagForm::Token(token) => token,
            TagForm::Directive { prefix, suffix } => {
                let mut token = self.handle_for(&prefix).to_owned();
                escape_tag(&suffix, &mut token);
                token
            }
        })
    }

    /// The directives' lines.
    pub(crate) fn lines(&self) -> String {
        let mut output = String::new();
        for (handle, prefix) in self.added.iter().flat_map(|added| &added.handles) {
            output.push_str("%TAG ");
            output.push_str(handle);
            output.push(' ');
            // A local prefix keeps its `!`; a global one may start with any other character,
            // escaped.
            match prefix.strip_prefix('!') {
                Some(rest) => {
                    output.push('!');
                    escape_tag(rest, &mut output);
                }
                None => escape_tag(prefix, &mut output),
            }
            output.push('\n');
        }
        output
    }

    /// Add a directive for each tag in `value` that needs one, in the order the writer meets them,
    /// or fail at the first tag that has no form, as the writer would. The walk stops where the
    /// writer does, at a collection past `max_depth` and at a tag on a tagged value, so its
    /// recursion is no deeper than the writer's.
    pub(super) fn scan(&mut self, value: &Value, max_depth: usize) -> Result<(), Error> {
        if may_hold_a_tag(value)
            && let Err(tag) = self.scan_at(value, max_depth)
        {
            return TagForm::of(tag, RedefinedHandles::default()).map(|_| ());
        }
        Ok(())
    }

    /// Add the directive that `tag` needs, apart from the walk so that the walk's frame stays small.
    #[cold]
    #[inline(never)]
    fn add<'v>(&mut self, tag: &'v crate::tag::Tag) -> Result<(), &'v crate::tag::Tag> {
        match self.token(tag, RedefinedHandles::default()) {
            Ok(_) => Ok(()),
            Err(_) => Err(tag),
        }
    }

    /// [`TagDirectives::scan`] with `depth_left` more collections to enter. Returns the tag with
    /// no form that stopped it.
    fn scan_at<'v>(
        &mut self,
        value: &'v Value,
        depth_left: usize,
    ) -> Result<(), &'v crate::tag::Tag> {
        match value {
            Value::Tagged(tagged) => {
                if matches!(tagged.value, Value::Tagged(_)) {
                    return Ok(());
                }
                if needs_directive(&tagged.tag) {
                    self.add(&tagged.tag)?;
                }
                self.scan_at(&tagged.value, depth_left)
            }
            Value::Sequence(items) if depth_left > 0 => {
                for item in items.iter().filter(|item| may_hold_a_tag(item)) {
                    self.scan_at(item, depth_left - 1)?;
                }
                Ok(())
            }
            Value::Mapping(mapping) if depth_left > 0 => {
                for (key, value) in mapping.iter() {
                    for node in [key, value] {
                        if may_hold_a_tag(node) {
                            self.scan_at(node, depth_left - 1)?;
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Whether `value` is a tag or a collection, which the scan for directives looks into: every other
/// value is a scalar, which it can pass without a call.
fn may_hold_a_tag(value: &Value) -> bool {
    matches!(
        value,
        Value::Tagged(_) | Value::Sequence(_) | Value::Mapping(_)
    )
}
