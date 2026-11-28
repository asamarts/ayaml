//! Compile-time trait, auto-trait, and error-contract freeze assertions for `ayaml` 1.0 (AY-149).

use std::error::Error as StdError;
use std::fmt::{Debug, Display};
use std::hash::Hash;
use std::panic::{RefUnwindSafe, UnwindSafe};

use ayaml::{
    Anchor, Chomping, CollectionStyle, ColumnEncoding, Deserializer, DocumentDeserializer,
    EmitOptions, EmitterOptions, EmitterStyles, Error, ErrorCode, ErrorKind, Event, EventStream,
    Frontmatter, FrontmatterDelimiter, LimitExceeded, LimitKind, Limits, LineIndex, Location,
    Mapping, MaskedError, NodeStyle, Number, ParseOptions, ParsedFrontmatter, ParserBuf,
    PathSegment, Position, PositionError, ScalarStyle, Span, Spanned, SpannedNode, SpannedTree,
    Tag, TaggedValue, Tape, TapeDocuments, TapeDocumentsIter, TapeKind, TapeMapIter, TapeRef,
    TapeSeqIter, Value,
};

fn assert_send<T: Send>() {}
fn assert_send_sync<T: Send + Sync>() {}
fn assert_unpin<T: Unpin>() {}
fn assert_unwind_safe<T: UnwindSafe>() {}
fn assert_ref_unwind_safe<T: RefUnwindSafe>() {}
fn assert_copy<T: Copy>() {}
fn assert_default<T: Default>() {}
fn assert_ord<T: Ord>() {}
fn assert_debug<T: Debug>() {}
fn assert_display<T: Display>() {}
fn assert_std_error<T: StdError>() {}
fn assert_as_ref_str<T: AsRef<str>>() {}
fn assert_debug_clone<T: Clone + Debug>() {}
fn assert_value_traits<T: Clone + Debug + PartialEq + Eq>() {}
fn assert_hash_traits<T: Clone + Debug + PartialEq + Eq + Hash>() {}
fn assert_error_traits<T: StdError + Send + Sync + Clone + Debug + Display + PartialEq + Eq>() {}
fn assert_deserializer<'de, D: serde::Deserializer<'de>>() {}
fn assert_exact_fused_iter<I: ExactSizeIterator + std::iter::FusedIterator>() {}
fn assert_double_ended_exact_fused_iter<
    I: DoubleEndedIterator + ExactSizeIterator + std::iter::FusedIterator,
>() {
}
fn assert_value_index<I: ayaml::value::Index>() {}
fn assert_mapping_index<I: ayaml::mapping::Index>() {}
fn assert_tape_index<I: ayaml::tape::Index>() {}

#[test]
fn public_api_auto_traits_and_standard_derives_are_frozen_for_v1() {
    // Error and masked error contracts.
    assert_error_traits::<Error>();
    assert_error_traits::<MaskedError<'_>>();
    assert_copy::<MaskedError<'_>>();
    assert_unpin::<Error>();
    assert_unpin::<MaskedError<'_>>();
    assert_eq!(size_of::<Error>(), size_of::<usize>());
    assert_eq!(size_of::<Result<Value, Error>>(), size_of::<Value>());
    assert_copy::<ErrorKind>();
    assert_copy::<ErrorCode>();
    assert_copy::<LimitKind>();
    assert_copy::<LimitExceeded>();
    assert_send_sync::<ErrorKind>();
    assert_send_sync::<ErrorCode>();
    assert_send_sync::<LimitKind>();
    assert_display::<ErrorKind>();
    assert_display::<ErrorCode>();
    assert_as_ref_str::<ErrorCode>();
    assert_display::<LimitKind>();
    assert_display::<LimitExceeded>();
    assert_std_error::<LimitExceeded>();
    assert_hash_traits::<ErrorKind>();
    assert_hash_traits::<ErrorCode>();
    assert_hash_traits::<LimitKind>();
    assert_value_traits::<LimitExceeded>();
    assert_send_sync::<LimitExceeded>();

    // Core data model types, mapping iterators, entries, and index traits.
    assert_send_sync::<Value>();
    assert_send_sync::<Mapping>();
    assert_send_sync::<Number>();
    assert_display::<Number>();
    assert_send_sync::<Tag>();
    assert_display::<Tag>();
    assert_send_sync::<TaggedValue>();
    assert_send_sync::<ayaml::Entry<'_>>();
    assert_debug::<ayaml::Entry<'_>>();
    assert_send_sync::<ayaml::mapping::Iter<'_>>();
    assert_debug_clone::<ayaml::mapping::Iter<'_>>();
    assert_double_ended_exact_fused_iter::<ayaml::mapping::Iter<'_>>();
    assert_send_sync::<ayaml::mapping::IterMut<'_>>();
    assert_debug::<ayaml::mapping::IterMut<'_>>();
    assert_double_ended_exact_fused_iter::<ayaml::mapping::IterMut<'_>>();
    assert_send_sync::<ayaml::mapping::IntoIter>();
    assert_debug_clone::<ayaml::mapping::IntoIter>();
    assert_double_ended_exact_fused_iter::<ayaml::mapping::IntoIter>();
    assert_copy::<Number>();
    assert_default::<Mapping>();
    assert_hash_traits::<Value>();
    assert_hash_traits::<Mapping>();
    assert_hash_traits::<Number>();
    assert_hash_traits::<Tag>();
    assert_hash_traits::<TaggedValue>();
    assert_value_index::<usize>();
    assert_value_index::<&str>();
    assert_mapping_index::<&str>();

    // Options, limits, and emitter configuration.
    assert_send_sync::<Limits>();
    assert_send_sync::<ParseOptions>();
    assert_send_sync::<EmitOptions>();
    assert_send_sync::<EmitterOptions>();
    assert_send_sync::<EmitterStyles>();
    assert_send_sync::<NodeStyle>();
    assert_send_sync::<PathSegment>();
    assert_send_sync::<Chomping>();
    assert_copy::<EmitterOptions>();
    assert_copy::<NodeStyle>();
    assert_copy::<Chomping>();
    assert_default::<Limits>();
    assert_default::<ParseOptions>();
    assert_default::<EmitOptions>();
    assert_default::<EmitterOptions>();
    assert_default::<EmitterStyles>();
    assert_default::<NodeStyle>();
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
    assert_copy::<Span>();
    assert_copy::<Position>();
    assert_copy::<Location>();
    assert_copy::<Frontmatter<'_>>();
    assert_copy::<FrontmatterDelimiter>();
    assert_copy::<ScalarStyle>();
    assert_copy::<CollectionStyle>();
    assert_copy::<ColumnEncoding>();
    assert_copy::<PositionError>();
    assert_display::<PositionError>();
    assert_std_error::<PositionError>();
    assert_ord::<Position>();
    assert_ord::<Location>();
    assert_hash_traits::<Event<'_>>();
    assert_debug_clone::<SpannedTree<'_>>();
    assert_debug_clone::<SpannedNode<'_>>();
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
    assert_unwind_safe::<EventStream<'_>>();
    assert_send::<Deserializer<'_>>();
    assert_unpin::<Deserializer<'_>>();
    assert_unwind_safe::<Deserializer<'_>>();
    assert_send::<DocumentDeserializer<'_>>();
    assert_unpin::<DocumentDeserializer<'_>>();
    assert_unwind_safe::<DocumentDeserializer<'_>>();
    assert_deserializer::<DocumentDeserializer<'_>>();
    assert_deserializer::<&mut DocumentDeserializer<'_>>();
    assert_send_sync::<ayaml::Emitter<'_>>();
    assert_copy::<ayaml::Emitter<'_>>();
    assert_debug::<ayaml::Emitter<'_>>();
    assert_unpin::<ayaml::Emitter<'_>>();
    assert_unwind_safe::<ayaml::Emitter<'_>>();

    // Tape DOM and reusable ParserBuf contracts (AY-177).
    assert_send_sync::<Tape<'_>>();
    assert_unpin::<Tape<'_>>();
    assert_unwind_safe::<Tape<'_>>();
    assert_ref_unwind_safe::<Tape<'_>>();
    assert_debug_clone::<Tape<'_>>();
    assert_deserializer::<Tape<'_>>();
    assert_deserializer::<&Tape<'_>>();

    assert_send_sync::<TapeDocuments<'_>>();
    assert_unpin::<TapeDocuments<'_>>();
    assert_unwind_safe::<TapeDocuments<'_>>();
    assert_ref_unwind_safe::<TapeDocuments<'_>>();
    assert_debug_clone::<TapeDocuments<'_>>();

    assert_send_sync::<TapeRef<'_, '_>>();
    assert_unpin::<TapeRef<'_, '_>>();
    assert_unwind_safe::<TapeRef<'_, '_>>();
    assert_ref_unwind_safe::<TapeRef<'_, '_>>();
    assert_copy::<TapeRef<'_, '_>>();
    assert_debug_clone::<TapeRef<'_, '_>>();
    assert_deserializer::<TapeRef<'_, '_>>();

    assert_send_sync::<TapeKind>();
    assert_unpin::<TapeKind>();
    assert_unwind_safe::<TapeKind>();
    assert_ref_unwind_safe::<TapeKind>();
    assert_copy::<TapeKind>();
    assert_hash_traits::<TapeKind>();

    assert_send_sync::<TapeSeqIter<'_, '_>>();
    assert_unpin::<TapeSeqIter<'_, '_>>();
    assert_unwind_safe::<TapeSeqIter<'_, '_>>();
    assert_ref_unwind_safe::<TapeSeqIter<'_, '_>>();
    assert_debug_clone::<TapeSeqIter<'_, '_>>();
    assert_exact_fused_iter::<TapeSeqIter<'_, '_>>();

    assert_send_sync::<TapeMapIter<'_, '_>>();
    assert_unpin::<TapeMapIter<'_, '_>>();
    assert_unwind_safe::<TapeMapIter<'_, '_>>();
    assert_ref_unwind_safe::<TapeMapIter<'_, '_>>();
    assert_debug_clone::<TapeMapIter<'_, '_>>();
    assert_exact_fused_iter::<TapeMapIter<'_, '_>>();

    assert_send_sync::<TapeDocumentsIter<'_, '_>>();
    assert_unpin::<TapeDocumentsIter<'_, '_>>();
    assert_unwind_safe::<TapeDocumentsIter<'_, '_>>();
    assert_ref_unwind_safe::<TapeDocumentsIter<'_, '_>>();
    assert_debug_clone::<TapeDocumentsIter<'_, '_>>();
    assert_exact_fused_iter::<TapeDocumentsIter<'_, '_>>();

    assert_send_sync::<ParserBuf>();
    assert_unpin::<ParserBuf>();
    assert_unwind_safe::<ParserBuf>();
    assert_ref_unwind_safe::<ParserBuf>();
    assert_default::<ParserBuf>();
    assert_debug::<ParserBuf>();

    assert_tape_index::<usize>();
    assert_tape_index::<&str>();
    assert_tape_index::<String>();
    assert_tape_index::<Value>();
    assert_tape_index::<&Value>();

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
    assert_copy::<ayaml::compat::GlobalTags>();
    assert_default::<ayaml::compat::ParseOptions>();
    assert_default::<ayaml::compat::GlobalTags>();
    assert_value_traits::<ayaml::compat::ParseOptions>();
    assert_hash_traits::<ayaml::compat::GlobalTags>();
}
