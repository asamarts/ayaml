//! Compile-time trait and auto-trait freeze assertions for `ayaml-edit` 1.0 (AY-149).

use std::error::Error as StdError;
use std::fmt::{Debug, Display};
use std::hash::Hash;

use ayaml_edit::{
    EditError, EditErrorKind, EditOp, EditOptions, MaskedEditError, TextEdit, VerifiedEdit,
};

fn assert_send_sync_unpin<T: Send + Sync + Unpin>() {}
fn assert_copy<T: Copy>() {}
fn assert_default<T: Default>() {}
fn assert_value_traits<T: Clone + Debug + PartialEq + Eq>() {}
fn assert_hash_traits<T: Clone + Debug + PartialEq + Eq + Hash>() {}
fn assert_display<T: Display>() {}
fn assert_error_traits<T: StdError + Send + Sync + Clone + Debug + Display + PartialEq + Eq>() {}

const DEFAULT_EDIT_OPTIONS: EditOptions = EditOptions::new();

#[test]
fn ayaml_edit_public_api_traits_are_frozen_for_v1() {
    assert_send_sync_unpin::<TextEdit>();
    assert_send_sync_unpin::<VerifiedEdit>();
    assert_send_sync_unpin::<EditErrorKind>();
    assert_send_sync_unpin::<EditError>();
    assert_send_sync_unpin::<MaskedEditError<'_>>();
    assert_send_sync_unpin::<EditOptions>();
    assert_send_sync_unpin::<EditOp>();

    assert_copy::<EditErrorKind>();
    assert_display::<EditErrorKind>();
    assert_copy::<MaskedEditError<'_>>();
    assert_default::<EditOptions>();
    assert_hash_traits::<TextEdit>();
    assert_value_traits::<VerifiedEdit>();
    assert_hash_traits::<EditErrorKind>();
    assert_error_traits::<EditError>();
    assert_error_traits::<MaskedEditError<'_>>();
    assert_value_traits::<EditOptions>();
    assert_value_traits::<EditOp>();

    assert_eq!(DEFAULT_EDIT_OPTIONS, EditOptions::default());

    // Verify `MaskedEditError::source_error` borrows from `'a` (`&EditError`), not a temporary.
    let err = ayaml_edit::apply_batch("a: [1\n", &[]).unwrap_err();
    let src = err.masked().source_error();
    assert!(src.is_some());
}
