//! Regression tests for the merging and path attachment of `ValidationErrors`.
//!
//! These tests pin down how field errors, list indices and struct-level
//! (`__all__`) errors coexist in one error tree, independently of the derive
//! macro. See `docs/validation-errors-merge.md` for the design tradeoffs.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

/// A struct whose validation fails (when `valid` is false) with one field
/// error and one struct-level (schema) error, mimicking what
/// `#[derive(Validate)]` generates for a struct with a failing field
/// validator and a failing schema function.
struct Item(bool);

impl Validate for Item {
    fn validate(&self) -> Result<(), ValidationErrors> {
        if self.0 {
            return Ok(());
        }
        let mut errors = ValidationErrors::new();
        let mut field_err = ValidationError::new("length");
        field_err.add_param(Cow::Borrowed("min"), &5u64);
        field_err.add_param(Cow::Borrowed("value"), &"bad");
        errors.add("name", field_err);
        errors.add("__all__", ValidationError::new("schema_meh"));
        Err(errors)
    }
}

fn item_errors() -> ValidationErrors {
    Item(false).validate().unwrap_err()
}

fn expected_item_kind() -> ValidationErrorsKind {
    ValidationErrorsKind::Struct(Box::new(item_errors()))
}

/// Serializing must never leak the internal `_tmp_validator` marker key.
fn assert_no_tmp_marker(errors: &ValidationErrors) {
    let json = serde_json::to_string(errors).unwrap();
    assert!(
        !json.contains("_tmp_validator"),
        "internal `_tmp_validator` marker leaked into serialization: {json}"
    );
}

#[test]
fn merge_attaches_collection_errors_to_the_field_path() {
    let items = vec![Item(true), Item(false)];
    let res = ValidationErrors::merge(Ok(()), "items", items.validate());
    let errs = res.unwrap_err();

    assert_eq!(errs.errors().len(), 1);
    match &errs.errors()["items"] {
        ValidationErrorsKind::List(map) => {
            assert_eq!(map.len(), 1);
            // The schema error stays attached to the element index, it is not
            // hoisted to the container or detached behind an internal key.
            assert_eq!(map[&1], Box::new(expected_item_kind()));
        }
        other => panic!("Expected list validation errors, got {other:?}"),
    }
    assert_no_tmp_marker(&errs);
}

#[test]
fn merge_self_attaches_collection_errors_to_the_field_path() {
    let items = vec![Item(true), Item(false)];
    let mut errs = ValidationErrors::new();
    errs.merge_self("items", items.validate());

    match &errs.errors()["items"] {
        ValidationErrorsKind::List(map) => {
            assert_eq!(map.len(), 1);
            assert_eq!(map[&1], Box::new(expected_item_kind()));
        }
        other => panic!("Expected list validation errors, got {other:?}"),
    }
    assert_no_tmp_marker(&errs);
}

#[test]
fn merge_all_keeps_field_and_schema_errors_at_the_element_index() {
    let items = vec![Item(true), Item(false)];
    let results: Vec<_> = items
        .iter()
        .map(|item| ValidationErrors::merge(Ok(()), "items", item.validate()))
        .collect();
    let errs = ValidationErrors::merge_all(Ok(()), "items", results).unwrap_err();

    let mut expected_list = BTreeMap::new();
    expected_list.insert(1usize, Box::new(expected_item_kind()));
    let expected = ValidationErrors(HashMap::from_iter([(
        Cow::Borrowed("items"),
        ValidationErrorsKind::List(expected_list),
    )]));
    assert_eq!(errs, expected);
    assert_no_tmp_marker(&errs);
}

#[test]
fn merge_all_preserves_nested_list_kinds() {
    // Elements that are themselves collections must keep their own indices
    // instead of being dropped or flattened into the wrong position.
    let rows = vec![vec![Item(false)]];
    let results: Vec<_> =
        rows.iter().map(|row| ValidationErrors::merge(Ok(()), "rows", row.validate())).collect();
    let errs = ValidationErrors::merge_all(Ok(()), "rows", results).unwrap_err();

    match &errs.errors()["rows"] {
        ValidationErrorsKind::List(map) => match &*map[&0] {
            ValidationErrorsKind::List(inner) => {
                assert_eq!(inner[&0], Box::new(expected_item_kind()));
            }
            other => panic!("Expected nested list validation errors, got {other:?}"),
        },
        other => panic!("Expected list validation errors, got {other:?}"),
    }
    assert_no_tmp_marker(&errs);
}

#[test]
fn multi_layer_collections_keep_their_indices() {
    let matrix = vec![vec![Item(true)], vec![Item(true), Item(false)]];
    let mut errs = ValidationErrors::new();
    errs.merge_self("matrix", matrix.validate());

    let json = serde_json::to_value(&errs).unwrap();
    assert_eq!(json["matrix"]["1"]["1"]["name"][0]["code"], "length");
    assert_eq!(json["matrix"]["1"]["1"]["__all__"][0]["code"], "schema_meh");
    assert!(json["matrix"].get("0").is_none() || json["matrix"]["0"].is_null());
    assert_no_tmp_marker(&errs);
}

#[test]
fn empty_collections_produce_no_errors() {
    let empty: Vec<Item> = Vec::new();
    assert!(empty.validate().is_ok());

    let mut errs = ValidationErrors::new();
    errs.merge_self("items", empty.validate());
    assert!(errs.is_empty());

    let res = ValidationErrors::merge(Ok(()), "items", empty.validate());
    assert!(res.is_ok());

    let res = ValidationErrors::merge_all(Ok(()), "items", vec![Ok(()), Ok(())]);
    assert!(res.is_ok());
}

#[test]
fn duplicate_codes_are_preserved_in_order() {
    struct DuplicateCodes;

    impl Validate for DuplicateCodes {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            errors.add("name", ValidationError::new("same_code"));
            errors.add("name", ValidationError::new("same_code"));
            errors.add("__all__", ValidationError::new("same_code"));
            Err(errors)
        }
    }

    let items = vec![DuplicateCodes, DuplicateCodes];
    let mut errs = ValidationErrors::new();
    errs.merge_self("items", items.validate());

    let json = serde_json::to_value(&errs).unwrap();
    for index in ["0", "1"] {
        let field_codes: Vec<_> = json["items"][index]["name"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["code"].as_str().unwrap())
            .collect();
        assert_eq!(field_codes, ["same_code", "same_code"]);
        assert_eq!(json["items"][index]["__all__"][0]["code"], "same_code");
    }
}

#[test]
fn merge_order_reversal_yields_the_same_tree() {
    // Mutation: build the same logical errors with the merge order reversed.
    // The resulting trees and their serialization must be identical.
    fn parent_field_error() -> ValidationError {
        ValidationError::new("parent_code")
    }

    // Order A: parent field error first, nested collection merged second.
    let mut order_a = ValidationErrors::new();
    order_a.add("name", parent_field_error());
    order_a.merge_self("items", vec![Item(true), Item(false)].validate());

    // Order B: nested collection merged first, parent field error second.
    let mut order_b = ValidationErrors::new();
    order_b.merge_self("items", vec![Item(true), Item(false)].validate());
    order_b.add("name", parent_field_error());

    assert_eq!(order_a, order_b);
    assert_eq!(serde_json::to_value(&order_a).unwrap(), serde_json::to_value(&order_b).unwrap());

    // Same mutation through the `merge`/`merge_all` combinators: the parent
    // result may carry errors before or after the children are merged in.
    let results: Vec<_> = [Item(true), Item(false)]
        .iter()
        .map(|item| ValidationErrors::merge(Ok(()), "items", item.validate()))
        .collect();
    let mut parent = ValidationErrors::new();
    parent.add("name", parent_field_error());
    let combined = ValidationErrors::merge_all(Err(parent), "items", results).unwrap_err();
    assert_eq!(combined.errors()["items"], order_a.errors()["items"]);
    assert!(matches!(combined.errors()["name"], ValidationErrorsKind::Field(_)));
}

#[test]
fn serialization_is_stable_for_the_same_tree() {
    let mut errs = ValidationErrors::new();
    errs.merge_self("items", vec![Item(true), Item(false)].validate());
    errs.add("name", ValidationError::new("parent_code"));

    // Repeated serialization of the same tree is byte-stable.
    let first = serde_json::to_string(&errs).unwrap();
    let second = serde_json::to_string(&errs).unwrap();
    assert_eq!(first, second);

    // The serialized shape is the stable, documented one: field names and
    // list indices as keys, `__all__` for struct-level errors.
    let value = serde_json::to_value(&errs).unwrap();
    assert_eq!(value["items"]["1"]["name"][0]["code"], "length");
    assert_eq!(value["items"]["1"]["__all__"][0]["code"], "schema_meh");
    assert_eq!(value["name"][0]["code"], "parent_code");
}
