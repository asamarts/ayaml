use super::RedefinedHandles;
use super::tags::{TagFormRef, needs_directive};
use crate::tag::Tag;

/// `needs_directive` is `TagFormRef::of` without building a token, so the two agree on every tag.
#[test]
fn needs_directive_agrees_with_the_tag_form() {
    let tags = [
        Tag::local("x"),
        Tag::local("a b"),
        Tag::local(""),
        Tag::new("!", "x"),
        Tag::new("!e-", "x"),
        Tag::new("tag:yaml.org,2002:", "str"),
        Tag::new("tag:yaml.org,2002:", ""),
        Tag::new("tag:yaml.org,2002:", "a b"),
        Tag::new("tag:x:", "y"),
        Tag::new("tag:x:", "50%25"),
        Tag::new("tag:x:", "50%"),
        Tag::new("tag:x:", "50%2"),
        Tag::new("tag:x:", "50%zz"),
        Tag::new("tag:x:", "a b"),
        Tag::new("tag:x:", "é"),
        Tag::new("tag:x:", "[a],b"),
        Tag::new("", "foobar"),
        Tag::new("", "tag:x:y"),
        Tag::new("", "1abc:x"),
        Tag::new("", "a+b-c.d:x"),
        Tag::new("", "a_b:x"),
        Tag::new("foo", "bar"),
        Tag::new("x", "y"),
        Tag::new("x", ""),
        Tag::new("", "x"),
        Tag::new("", ""),
    ];
    for tag in tags {
        let form = TagFormRef::of(&tag, RedefinedHandles::default());
        let directive = matches!(form, Ok(TagFormRef::Directive { .. }) | Err(_));
        assert_eq!(needs_directive(&tag), directive, "{tag:?}: {form:?}");
    }
}
