//! Compile-time trait, auto-trait, and error-contract freeze assertions for `ayaml` 1.0 (AY-149).

use std::error::Error as StdError;
use std::fmt::{Debug, Display};
use std::hash::Hash;

use ayaml::{
    Anchor, Chomping, CollectionStyle, ColumnEncoding, Deserializer, DocumentDeserializer,
    EmitOptions, EmitterOptions, EmitterStyles, Error, ErrorCode, ErrorKind, Event, EventStream,
    Frontmatter, FrontmatterDelimiter, LimitExceeded, LimitKind, Limits, LineIndex, Location,
    Mapping, MaskedError, NodeStyle, Number, ParseOptions, ParsedFrontmatter, PathSegment,
    Position, PositionError, ScalarStyle, Span, Spanned, SpannedNode, SpannedTree, Tag,
    TaggedValue, Value,
};

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}
fn assert_unpin<T: Unpin>() {}
fn assert_value_traits<T: Clone + Debug + PartialEq + Eq>() {}
fn assert_hash_traits<T: Clone + Debug + PartialEq + Eq + Hash>() {}
fn assert_error_traits<T: StdError + Send + Sync + Clone + Debug + Display + PartialEq + Eq>() {}

#[test]
fn public_api_auto_traits_and_standard_derives_are_frozen_for_v1() {
    // Error and masked error contracts.
    assert_error_traits::<Error>();
    assert_send_sync::<MaskedError<'_>>();
    assert_unpin::<Error>();
    assert_unpin::<MaskedError<'_>>();
    assert_hash_traits::<ErrorKind>();
    assert_hash_traits::<ErrorCode>();
    assert_hash_traits::<LimitKind>();
    assert_value_traits::<LimitExceeded>();
    assert_send_sync::<LimitExceeded>();

    // Core data model types.
    assert_send_sync::<Value>();
    assert_send_sync::<Mapping>();
    assert_send_sync::<Number>();
    assert_send_sync::<Tag>();
    assert_send_sync::<TaggedValue>();
    assert_hash_traits::<Value>();
    assert_hash_traits::<Mapping>();
    assert_hash_traits::<Number>();
    assert_hash_traits::<Tag>();
    assert_hash_traits::<TaggedValue>();

    // Options, limits, and emitter configuration.
    assert_send_sync::<Limits>();
    assert_send_sync::<ParseOptions>();
    assert_send_sync::<EmitOptions>();
    assert_send_sync::<EmitterOptions>();
    assert_send_sync::<EmitterStyles>();
    assert_send_sync::<NodeStyle>();
    assert_send_sync::<PathSegment>();
    assert_send_sync::<Chomping>();
    assert_value_traits::<Limits>();
    assert_value_traits::<ParseOptions>();
    assert_value_traits::<EmitOptions>();
    assert_value_traits::<EmitterOptions>();
    assert_value_traits::<EmitterStyles>();
    assert_value_traits::<NodeStyle>();
    assert_hash_traits::<PathSegment>();
    assert_hash_traits::<Chomping>();

    // Spans, positions, frontmatter, and events.
    assert_send_sync::<Span>();
    assert_send_sync::<Position>();
    assert_send_sync::<Location>();
    assert_send_sync::<Spanned<String>>();
    assert_send_sync::<Frontmatter<'_>>();
    assert_send_sync::<FrontmatterDelimiter>();
    assert_send_sync::<ParsedFrontmatter<'_, String>>();
    assert_send_sync::<Event<'_>>();
    assert_send_sync::<Anchor<'_>>();
    assert_send_sync::<ScalarStyle>();
    assert_send_sync::<CollectionStyle>();
    assert_send_sync::<ColumnEncoding>();
    assert_send_sync::<PositionError>();
    assert_send_sync::<SpannedTree<'_>>();
    assert_send_sync::<SpannedNode<'_>>();
    assert_hash_traits::<Span>();
    assert_hash_traits::<Position>();
    assert_hash_traits::<Location>();
    assert_hash_traits::<Spanned<String>>();
    assert_hash_traits::<Frontmatter<'_>>();
    assert_hash_traits::<FrontmatterDelimiter>();
    assert_hash_traits::<ParsedFrontmatter<'_, String>>();
    assert_hash_traits::<Anchor<'_>>();
    assert_hash_traits::<ScalarStyle>();
    assert_hash_traits::<CollectionStyle>();
    assert_hash_traits::<ColumnEncoding>();
    assert_hash_traits::<PositionError>();

    // Stateful / Cell-bearing readers, emitters, and iterators.
    assert_send::<LineIndex<'_>>();
    assert_unpin::<LineIndex<'_>>();
    assert_send::<EventStream<'_>>();
    assert_unpin::<EventStream<'_>>();
    assert_send::<Deserializer<'_>>();
    assert_unpin::<Deserializer<'_>>();
    assert_send::<DocumentDeserializer<'_>>();
    assert_unpin::<DocumentDeserializer<'_>>();
    assert_send::<ayaml::Emitter<'_>>();
    assert_unpin::<ayaml::Emitter<'_>>();
    assert_send_sync::<ayaml::mapping::IntoIter>();

    // Spanned<T> supports DerefMut as well as Deref/AsRef/AsMut.
    let mut spanned = Spanned::new(Span::new(0, 1), 10_u32);
    *spanned += 5;
    assert_eq!(*spanned, 15);
}

#[cfg(feature = "libyaml-compat")]
#[test]
fn compat_public_api_traits_are_frozen_for_v1() {
    assert_send_sync::<ayaml::compat::ParseOptions>();
    assert_send_sync::<ayaml::compat::GlobalTags>();
    assert_value_traits::<ayaml::compat::ParseOptions>();
    assert_hash_traits::<ayaml::compat::GlobalTags>();
}
