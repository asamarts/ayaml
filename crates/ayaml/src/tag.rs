//! YAML tags and tagged values.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

use crate::value::Value;

const CORE_PREFIX: &str = "tag:yaml.org,2002:";

/// A tag, stored as the prefix its handle resolved to plus a suffix.
///
/// Two tags are equal when they name the same tag: equality, ordering, and
/// hashing use the full tag (`handle` followed by `suffix`), so
/// `!<tag:yaml.org,2002:str>` equals `!!str`, and a tag written with a
/// `%TAG` handle equals the same tag written verbatim.
#[derive(Clone, Debug)]
pub struct Tag {
    handle: String,
    suffix: String,
}

impl Tag {
    /// Create a tag from its handle prefix and suffix.
    ///
    /// A full name that starts with the core prefix `tag:yaml.org,2002:` is
    /// split at that prefix whatever `handle` holds, so
    /// `Tag::new("", "tag:yaml.org,2002:str")` and
    /// `Tag::new("tag:yaml.org,2002:s", "tr")` are both the core tag `!!str`.
    /// A verbatim tag (empty `handle`) that starts with `!` is split there,
    /// so `Tag::new("", "!x")` is the local tag `!x`.
    #[must_use]
    pub fn new(handle: impl Into<String>, suffix: impl Into<String>) -> Self {
        let handle = handle.into();
        let suffix = suffix.into();
        if handle != CORE_PREFIX
            && (CORE_PREFIX.starts_with(handle.as_str()) || handle.starts_with(CORE_PREFIX))
        {
            let full = format!("{handle}{suffix}");
            if let Some(rest) = full.strip_prefix(CORE_PREFIX) {
                return Self {
                    handle: CORE_PREFIX.to_owned(),
                    suffix: rest.to_owned(),
                };
            }
        }
        if handle.is_empty()
            && let Some(rest) = suffix.strip_prefix('!')
        {
            return Self {
                handle: "!".to_owned(),
                suffix: rest.to_owned(),
            };
        }
        Self { handle, suffix }
    }

    /// Whether this is a core collection tag (`!!seq`, `!!map`, `!!set`,
    /// `!!omap`, or `!!pairs`). It only restates the node kind, so the
    /// composers drop it.
    pub(crate) fn is_core_collection(&self) -> bool {
        self.is_core() && matches!(self.suffix(), "seq" | "map" | "set" | "omap" | "pairs")
    }

    /// The tag's name in the Serde data model: a local tag's name after its
    /// `!` (`Foo` for `!Foo`, and for `!e!Foo` under `%TAG !e! !`) or, for
    /// any other tag, its full name in angle brackets
    /// (`<tag:yaml.org,2002:set>` for `!!set`). A local name that is empty
    /// or starts with `!` or `<` is bracketed too, so that
    /// [`Tag::from_serde_name`] reads every name back.
    pub(crate) fn serde_name(&self) -> String {
        if let Some(prefix) = self.handle.strip_prefix('!') {
            let name = format!("{prefix}{}", self.suffix);
            if bare_serde_name(&name) {
                return name;
            }
        }
        format!("<{}{}>", self.handle, self.suffix)
    }

    /// The name of the enum variant this tag selects: a local tag's name after its first `!`, as
    /// readers built on libyaml name it, so `!<!!x>` selects `!x`, or any other tag's full name in
    /// angle brackets, as in [`Tag::serde_name`]. Unlike a Serde name, a local name is never
    /// bracketed, so `Tag::local(name)` selects the variant `name`, which is how a variant is
    /// written, and a local tag spelled `!%3Ctag:x:B%3E` selects the variant that `!<tag:x:B>`
    /// selects.
    pub(crate) fn variant_name(&self) -> String {
        match self.handle.strip_prefix('!') {
            Some(prefix) => format!("{prefix}{}", self.suffix),
            None => format!("<{}{}>", self.handle, self.suffix),
        }
    }

    /// [`Tag::variant_name`], which moves the suffix of a tag written with `!` rather than
    /// copying it.
    pub(crate) fn into_variant_name(self) -> String {
        if self.handle == "!" {
            self.suffix
        } else {
            self.variant_name()
        }
    }

    /// The tag a Serde name stands for (see [`Tag::serde_name`]). A bare
    /// name is a local tag.
    pub(crate) fn from_serde_name(name: &str) -> Self {
        match name
            .strip_prefix('<')
            .and_then(|rest| rest.strip_suffix('>'))
        {
            Some(full) => Self::new("", full),
            None if name.starts_with('!') => Self::new("", name),
            None => Self::local(name),
        }
    }

    fn full(&self) -> impl Iterator<Item = u8> + '_ {
        self.handle.bytes().chain(self.suffix.bytes())
    }

    /// Create a core-schema tag such as `!!str`.
    #[must_use]
    pub fn core(suffix: impl Into<String>) -> Self {
        Self::new(CORE_PREFIX, suffix)
    }

    /// Create a local tag such as `!Custom`.
    #[must_use]
    pub fn local(suffix: impl Into<String>) -> Self {
        Self::new("!", suffix)
    }

    /// The tag handle.
    #[must_use]
    pub fn handle(&self) -> &str {
        &self.handle
    }

    /// The bytes of text this tag holds, which every copy of it holds too.
    pub(crate) fn text_len(&self) -> usize {
        self.handle.len().saturating_add(self.suffix.len())
    }

    /// The tag suffix.
    #[must_use]
    pub fn suffix(&self) -> &str {
        &self.suffix
    }

    /// Whether this is a core-schema tag.
    #[must_use]
    pub fn is_core(&self) -> bool {
        self.handle == CORE_PREFIX
    }
}

impl PartialEq for Tag {
    fn eq(&self, other: &Self) -> bool {
        self.handle.len() + self.suffix.len() == other.handle.len() + other.suffix.len()
            && self.full().eq(other.full())
    }
}

impl Eq for Tag {}

impl Hash for Tag {
    fn hash<H: Hasher>(&self, state: &mut H) {
        // Equal tags can split their text differently, and not every hasher reads two writes as
        // one, so the split can't show.
        for byte in self.full() {
            state.write_u8(byte);
        }
        state.write_u8(0xff);
    }
}

impl PartialOrd for Tag {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Tag {
    fn cmp(&self, other: &Self) -> Ordering {
        self.full().cmp(other.full())
    }
}

impl fmt::Display for Tag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.handle == "!" {
            write!(formatter, "!{}", self.suffix)
        } else if self.handle == CORE_PREFIX {
            write!(formatter, "!!{}", self.suffix)
        } else {
            write!(formatter, "{}{}", self.handle, self.suffix)
        }
    }
}

/// A tagged value.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaggedValue {
    /// The tag.
    pub tag: Tag,
    /// The tagged value.
    pub value: Value,
}

impl TaggedValue {
    /// Create a tagged value.
    #[must_use]
    pub fn new(tag: Tag, value: Value) -> Self {
        Self { tag, value }
    }
}

/// Whether a local tag's name stands for itself in the Serde data model, unbracketed.
fn bare_serde_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with(['!', '<'])
}

#[cfg(test)]
mod tests {
    use super::{Tag, TaggedValue};
    use crate::value::Value;

    /// Equal tags hash alike under any hasher, including one that reads each write on its own.
    #[test]
    fn a_tags_hash_does_not_show_where_its_text_splits() {
        use std::hash::{Hash, Hasher};

        #[derive(Default)]
        struct Writes(Vec<Vec<u8>>);
        impl Hasher for Writes {
            fn write(&mut self, bytes: &[u8]) {
                self.0.push(bytes.to_vec());
            }
            fn finish(&self) -> u64 {
                0
            }
        }
        let writes = |tag: Tag| {
            let mut writes = Writes::default();
            tag.hash(&mut writes);
            writes.0
        };
        let (split, other) = (Tag::new("!ab", "c"), Tag::new("!", "abc"));
        assert_eq!(split, other);
        assert_eq!(writes(split), writes(other));
    }

    #[test]
    fn local_and_core_tags_display_canonically() {
        assert_eq!(Tag::local("Custom").to_string(), "!Custom");
        assert_eq!(Tag::core("str").to_string(), "!!str");
        assert_eq!(
            Tag::new("tag:example.com,2026:", "foo").to_string(),
            "tag:example.com,2026:foo"
        );
    }

    #[test]
    fn serde_names_read_back() {
        for tag in [
            Tag::local("Foo"),
            Tag::local(""),
            Tag::local("!"),
            Tag::local("<x>"),
            Tag::local("a:b"),
            Tag::core("set"),
            Tag::new("tag:example.com,2000:", "foo"),
            Tag::new("", "tag:x"),
            Tag::new("!a-", "b"),
            Tag::new("!", ""),
            Tag::new("!!", "x"),
        ] {
            assert_eq!(Tag::from_serde_name(&tag.serde_name()), tag, "{tag}");
        }
        assert_eq!(Tag::local("Foo").serde_name(), "Foo");
        // A local tag that a named handle wrote has the name it has when written with `!`.
        assert_eq!(Tag::new("!a-", "b").serde_name(), "a-b");
        assert_eq!(Tag::core("set").serde_name(), "<tag:yaml.org,2002:set>");
    }

    #[test]
    fn variant_names_read_back() {
        for (tag, name) in [
            (Tag::local("B"), "B"),
            (Tag::new("!a-", "b"), "a-b"),
            (Tag::new("", "!!x"), "!x"),
            (Tag::new("!!", "x"), "!x"),
            (Tag::local("<!x>"), "<!x>"),
            (Tag::local(""), ""),
            (Tag::new("tag:x:", "B"), "<tag:x:B>"),
            (Tag::core("C"), "<tag:yaml.org,2002:C>"),
        ] {
            assert_eq!(tag.variant_name(), name, "{tag}");
            assert_eq!(tag.clone().into_variant_name(), name, "{tag}");
            // A variant is written under the local tag of its name, which selects it.
            assert_eq!(Tag::local(name).variant_name(), name, "{tag}");
        }
    }

    #[test]
    fn tags_compare_by_full_name() {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        fn hash(tag: &Tag) -> u64 {
            let mut hasher = DefaultHasher::new();
            tag.hash(&mut hasher);
            hasher.finish()
        }
        let verbatim = Tag::new("", "tag:yaml.org,2002:str");
        assert_eq!(verbatim, Tag::core("str"));
        assert!(verbatim.is_core());
        assert_eq!(verbatim.suffix(), "str");
        assert_eq!(Tag::new("", "!x"), Tag::local("x"));
        let handle = Tag::new("tag:example.com,2000:app/", "foo");
        let full = Tag::new("", "tag:example.com,2000:app/foo");
        assert_eq!(handle, full);
        assert_eq!(hash(&handle), hash(&full));
        assert_eq!(handle.cmp(&full), std::cmp::Ordering::Equal);
        assert_ne!(Tag::local("a"), Tag::local("b"));
        assert!(Tag::local("a") < Tag::local("b"));
    }

    #[test]
    fn core_tag_is_recognized() {
        assert!(Tag::core("int").is_core());
        assert!(!Tag::local("int").is_core());
    }

    #[test]
    fn tagged_value_keeps_its_tag_and_payload() {
        let tagged = TaggedValue::new(Tag::local("Custom"), Value::from(1_u64));
        assert_eq!(tagged.tag.to_string(), "!Custom");
        assert_eq!(tagged.value, Value::from(1_u64));
    }
}
