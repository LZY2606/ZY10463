//! Regression tests for the merging and path attachment of `ValidationErrors`.
//!
//! The scenario under test: the second element of a `Vec` fails field-level and
//! schema-level (`__all__`) validation at the same time. Merging the results
//! must keep field errors, list indices and struct-level errors side by side;
//! the schema error must neither be disguised as a field nor detached from the
//! element index.

use std::borrow::Cow;
use std::collections::BTreeMap;

use serde_json::{json, Value};
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

/// Mirrors what `#[derive(Validate)]` generates for a struct with a field rule
/// and a schema rule (`skip_on_field_errors = false`): both errors are produced
/// by the same `validate()` call.
#[derive(Debug)]
struct Child {
    name: String,
}

impl Validate for Child {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        if self.name.is_empty() {
            let mut err = ValidationError::new("length");
            err.add_param(Cow::Borrowed("min"), &1u64);
            err.add_param(Cow::Borrowed("value"), &self.name);
            errors.add("name", err);
            // schema-level error, attached to the element itself
            errors.add("__all__", ValidationError::new("child_schema"));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }
}

/// A struct whose validation only produces a schema-level error.
#[derive(Debug)]
struct SchemaOnlyChild;

impl Validate for SchemaOnlyChild {
    fn validate(&self) -> Result<(), ValidationErrors> {
        let mut errors = ValidationErrors::new();
        errors.add("__all__", ValidationError::new("child_schema"));
        Err(errors)
    }
}

fn children() -> Vec<Child> {
    vec![Child { name: "ok".to_string() }, Child { name: String::new() }]
}

fn expect_list<'a>(
    errors: &'a ValidationErrors,
    field: &str,
) -> &'a BTreeMap<usize, Box<ValidationErrors>> {
    match errors.errors().get(field) {
        Some(ValidationErrorsKind::List(list)) => list,
        other => panic!("expected `{field}` to be a List, got {other:?}"),
    }
}

fn expect_second_element(list: &BTreeMap<usize, Box<ValidationErrors>>) -> &ValidationErrors {
    assert!(!list.contains_key(&0), "the valid first element must not appear");
    let element = list.get(&1).expect("the index of the second element must be kept");
    assert!(
        matches!(element.errors().get("name"), Some(ValidationErrorsKind::Field(_))),
        "field error of element 1 must stay attached to the index"
    );
    assert!(
        matches!(element.errors().get("__all__"), Some(ValidationErrorsKind::Field(_))),
        "schema error of element 1 must stay attached to the index"
    );
    element
}

#[test]
fn merge_attaches_collection_indices_to_the_field() {
    let result = ValidationErrors::merge(Ok(()), "children", children().validate());
    let errors = result.unwrap_err();
    expect_second_element(expect_list(&errors, "children"));

    // The internal carrier key of collection `Validate` impls must not leak.
    let serialized = serde_json::to_string(&errors).unwrap();
    assert!(!serialized.contains("_tmp_validator"), "got: {serialized}");
}

#[test]
fn merge_and_merge_self_agree_on_collections() {
    let via_merge = ValidationErrors::merge(Ok(()), "children", children().validate()).unwrap_err();

    let mut via_merge_self = ValidationErrors::new();
    via_merge_self.merge_self("children", children().validate());

    assert_eq!(via_merge, via_merge_self);
}

#[test]
fn merge_all_keeps_raw_element_results() {
    // Negative regression test: raw `element.validate()` results used to be
    // silently dropped, losing field and schema errors together with the index.
    let results: Vec<_> = children().iter().map(|child| child.validate()).collect();
    let result = ValidationErrors::merge_all(Ok(()), "children", results);
    let errors = result.expect_err("element errors must not be dropped");
    expect_second_element(expect_list(&errors, "children"));
}

#[test]
fn merge_all_keeps_wrapped_element_results() {
    // The documented pattern of wrapping each element with `merge` first.
    let results: Vec<_> = children()
        .iter()
        .map(|child| ValidationErrors::merge(Ok(()), "children", child.validate()))
        .collect();
    let result = ValidationErrors::merge_all(Ok(()), "children", results);
    let errors = result.expect_err("element errors must not be dropped");
    expect_second_element(expect_list(&errors, "children"));
}

#[test]
fn merge_all_raw_and_wrapped_results_agree() {
    let raw: Vec<_> = children().iter().map(|child| child.validate()).collect();
    let wrapped: Vec<_> = children()
        .iter()
        .map(|child| ValidationErrors::merge(Ok(()), "children", child.validate()))
        .collect();

    let raw = ValidationErrors::merge_all(Ok(()), "children", raw).unwrap_err();
    let wrapped = ValidationErrors::merge_all(Ok(()), "children", wrapped).unwrap_err();
    assert_eq!(raw, wrapped);
}

#[test]
fn merge_all_keeps_schema_only_elements() {
    // An element whose only error is a schema-level (`__all__`) error must keep
    // its index instead of vanishing.
    let elements = [SchemaOnlyChild, SchemaOnlyChild];
    let results: Vec<_> = elements.iter().map(|child| child.validate()).collect();
    let errors = ValidationErrors::merge_all(Ok(()), "children", results).unwrap_err();

    let list = expect_list(&errors, "children");
    for index in [0, 1] {
        let element = list.get(&index).expect("schema-only element must keep its index");
        assert!(
            matches!(element.errors().get("__all__"), Some(ValidationErrorsKind::Field(_))),
            "schema error must stay attached to index {index}"
        );
        assert_eq!(element.errors().len(), 1, "schema error must not be disguised as a field");
    }
}

#[test]
fn merge_all_with_no_failing_children_keeps_parent() {
    assert!(ValidationErrors::merge_all(Ok(()), "children", vec![]).is_ok());

    let ok_children = [Child { name: "ok".to_string() }, Child { name: "fine".to_string() }];
    let results: Vec<_> = ok_children.iter().map(|child| child.validate()).collect();
    assert!(ValidationErrors::merge_all(Ok(()), "children", results).is_ok());
}

#[test]
fn merge_order_reversal_yields_the_same_tree() {
    // Field errors merged into the parent before vs after the nested collection
    // must produce the same error tree.
    let parent_field_error = || {
        let mut errors = ValidationErrors::new();
        errors.add("token", ValidationError::new("required"));
        errors
    };

    let results: Vec<_> = children().iter().map(|child| child.validate()).collect();
    let nested_first =
        ValidationErrors::merge_all(Err(parent_field_error()), "children", results).unwrap_err();

    let results: Vec<_> = children().iter().map(|child| child.validate()).collect();
    let mut nested_last = ValidationErrors::merge_all(Ok(()), "children", results).unwrap_err();
    nested_last.add("token", ValidationError::new("required"));

    assert_eq!(nested_first, nested_last);
    assert_eq!(
        serde_json::to_value(&nested_first).unwrap(),
        serde_json::to_value(&nested_last).unwrap(),
        "serialization must be stable across merge orders"
    );
}

#[test]
fn merge_self_order_reversal_yields_the_same_tree() {
    let mut ab = ValidationErrors::new();
    ab.merge_self("children", children().validate());
    ab.merge_self("parents", children().validate());

    let mut ba = ValidationErrors::new();
    ba.merge_self("parents", children().validate());
    ba.merge_self("children", children().validate());

    assert_eq!(ab, ba);
    assert_eq!(serde_json::to_value(&ab).unwrap(), serde_json::to_value(&ba).unwrap());
}

#[test]
fn duplicate_codes_keep_separate_entries() {
    let mut errors = ValidationErrors::new();
    errors.add("name", ValidationError::new("dup"));
    errors.add("name", ValidationError::new("dup"));
    errors.add("__all__", ValidationError::new("dup"));
    errors.add("__all__", ValidationError::new("dup"));

    let field = match errors.errors().get("name") {
        Some(ValidationErrorsKind::Field(errors)) => errors,
        other => panic!("expected field errors, got {other:?}"),
    };
    assert_eq!(field.len(), 2);
    assert!(field.iter().all(|err| err.code == "dup"));

    let schema = match errors.errors().get("__all__") {
        Some(ValidationErrorsKind::Field(errors)) => errors,
        other => panic!("expected schema errors, got {other:?}"),
    };
    assert_eq!(schema.len(), 2);
    assert!(schema.iter().all(|err| err.code == "dup"));
}

#[test]
fn list_indices_survive_a_serialization_roundtrip() {
    let errors = ValidationErrors::merge(Ok(()), "children", children().validate()).unwrap_err();
    let json = serde_json::to_string(&errors).unwrap();
    let back: ValidationErrors = serde_json::from_str(&json).unwrap();
    assert_eq!(errors, back, "collection indices must survive a roundtrip");
    expect_second_element(expect_list(&back, "children"));
}

#[test]
fn old_error_json_deserializes_to_the_expected_positions() {
    // Error JSON produced by older versions: field errors are arrays, schema
    // errors live under `__all__`, nested structs are objects and collection
    // indices are objects keyed by the index.
    let old_json = json!({
        "__all__": [{"code": "parent_schema", "message": null, "params": {}}],
        "token": [{"code": "required", "message": null, "params": {}}],
        "contact": {"mail": [{"code": "email", "message": null, "params": {}}]},
        "children": {
            "1": {
                "name": [{"code": "length", "message": null, "params": {"min": 1, "value": ""}}],
                "__all__": [{"code": "child_schema", "message": null, "params": {}}]
            }
        }
    });

    let errors: ValidationErrors = serde_json::from_value(old_json).unwrap();

    assert!(
        matches!(errors.errors().get("__all__"), Some(ValidationErrorsKind::Field(_))),
        "schema errors keep the `__all__` key"
    );
    assert!(matches!(errors.errors().get("token"), Some(ValidationErrorsKind::Field(_))));
    assert!(
        matches!(errors.errors().get("contact"), Some(ValidationErrorsKind::Struct(_))),
        "nested structs keep their shape"
    );

    let list = expect_list(&errors, "children");
    let element = list.get(&1).expect("the collection index must be recovered");
    assert!(element.errors().contains_key("name"));
    assert!(element.errors().contains_key("__all__"));
}

#[test]
fn serialized_tree_matches_the_expected_shape() {
    let errors = ValidationErrors::merge(Ok(()), "children", children().validate()).unwrap_err();
    // Compare as `Value`: the iteration order of the error map is deliberately
    // not part of the stable output, only keys and positions are.
    let value: Value = serde_json::to_value(&errors).unwrap();
    assert_eq!(
        value,
        json!({
            "children": {
                "1": {
                    "name": [{
                        "code": "length",
                        "message": null,
                        "params": {"min": 1, "value": ""}
                    }],
                    "__all__": [{
                        "code": "child_schema",
                        "message": null,
                        "params": {}
                    }]
                }
            }
        })
    );
}
