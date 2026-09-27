//! Regression tests for "nested collection indices are lost in schema errors".
//!
//! Injection scenario: the second object of a `Vec` fails field validation and
//! schema-level validation at the same time. After merging, the schema error
//! must stay attached to the element index: field errors, list indices and
//! struct-level (`__all__`) errors coexist in one tree, schema errors are not
//! disguised as fields and the historical serialization keys are unchanged.

use serde::Serialize;
use serde_json::{Value, json};
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

fn child_schema(child: &Child) -> Result<(), ValidationError> {
    if child.name.is_empty() { Err(ValidationError::new("child_schema")) } else { Ok(()) }
}

fn parent_schema<T>(_parent: &T) -> Result<(), ValidationError> {
    Err(ValidationError::new("parent_schema"))
}

#[derive(Debug, Validate, Serialize)]
#[validate(schema(function = child_schema, skip_on_field_errors = false))]
struct Child {
    #[validate(length(min = 1, code = "name_too_short"))]
    name: String,
}

#[derive(Debug, Validate)]
#[validate(schema(function = parent_schema, skip_on_field_errors = false))]
struct Parent {
    #[validate(nested)]
    children: Vec<Child>,
}

fn good_child() -> Child {
    Child { name: "ok".to_string() }
}

fn bad_child() -> Child {
    Child { name: String::new() }
}

fn expected_parent_tree() -> Value {
    json!({
        "__all__": [{
            "code": "parent_schema",
            "message": null,
            "params": {}
        }],
        "children": {
            "1": {
                "name": [{
                    "code": "name_too_short",
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
}

#[test]
fn nested_vec_schema_errors_keep_the_element_index() {
    let parent = Parent { children: vec![good_child(), bad_child()] };
    let errors = parent.validate().unwrap_err();

    // Compare as `Value`: the iteration order of the error map is deliberately
    // not part of the stable output, only keys and positions are.
    assert_eq!(serde_json::to_value(&errors).unwrap(), expected_parent_tree());

    // The index belongs to the *second* element: it must not be renumbered.
    let list = match errors.errors().get("children") {
        Some(ValidationErrorsKind::List(list)) => list,
        other => panic!("expected `children` to be a List, got {other:?}"),
    };
    assert!(!list.contains_key(&0));
    let element = list.get(&1).expect("the element index must be kept");
    assert!(element.errors().contains_key("name"));
    assert!(element.errors().contains_key("__all__"));
}

#[test]
fn schema_errors_are_not_disguised_as_fields() {
    let parent = Parent { children: vec![good_child(), bad_child()] };
    let errors = parent.validate().unwrap_err();

    // The parent schema error stays at the parent level under `__all__`...
    match errors.errors().get("__all__") {
        Some(ValidationErrorsKind::Field(schema_errors)) => {
            assert_eq!(schema_errors.len(), 1);
            assert_eq!(schema_errors[0].code, "parent_schema");
        }
        other => panic!("expected parent schema errors under `__all__`, got {other:?}"),
    }
    // ...and the collection is a `List`, not a fabricated field entry.
    assert!(matches!(errors.errors().get("children"), Some(ValidationErrorsKind::List(_))));
    assert_eq!(errors.errors().len(), 2, "no extra keys may be invented");

    // The internal carrier key of collection validation must never leak.
    let serialized = serde_json::to_string(&errors).unwrap();
    assert!(!serialized.contains("_tmp_validator"), "got: {serialized}");
}

#[test]
fn nested_errors_survive_a_serialization_roundtrip() {
    let parent = Parent { children: vec![good_child(), bad_child()] };
    let errors = parent.validate().unwrap_err();

    let json = serde_json::to_string(&errors).unwrap();
    let back: ValidationErrors = serde_json::from_str(&json).unwrap();
    assert_eq!(errors, back);
    assert!(
        matches!(back.errors().get("children"), Some(ValidationErrorsKind::List(_))),
        "the List shape must survive the roundtrip"
    );
}

#[test]
fn option_vec_nested_schema_errors_keep_the_element_index() {
    #[derive(Debug, Validate)]
    #[validate(schema(function = parent_schema, skip_on_field_errors = false))]
    struct OptParent {
        #[validate(nested)]
        children: Option<Vec<Child>>,
    }

    let some = OptParent { children: Some(vec![good_child(), bad_child()]) };
    let errors = some.validate().unwrap_err();
    assert_eq!(serde_json::to_value(&errors).unwrap(), expected_parent_tree());

    let none = OptParent { children: None };
    let errors = none.validate().unwrap_err();
    assert!(!errors.errors().contains_key("children"));
    assert!(errors.errors().contains_key("__all__"));
}

#[test]
fn multi_level_vec_keeps_distinguishable_positions() {
    #[derive(Debug, Validate)]
    struct Matrix {
        #[validate(nested)]
        matrix: Vec<Vec<Child>>,
    }

    let input = Matrix { matrix: vec![vec![good_child()], vec![good_child(), bad_child()]] };
    let errors = input.validate().unwrap_err();

    // Anonymous inner collections have no field name to attach to; each nesting
    // level keeps its own index map so positions stay distinguishable.
    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "matrix": {
                "1": {
                    "_tmp_validator": {
                        "1": {
                            "name": [{
                                "code": "name_too_short",
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
                }
            }
        })
    );
}

#[test]
fn empty_collection_does_not_invent_indices() {
    let parent = Parent { children: vec![] };
    let errors = parent.validate().unwrap_err();

    assert_eq!(
        serde_json::to_value(&errors).unwrap(),
        json!({
            "__all__": [{
                "code": "parent_schema",
                "message": null,
                "params": {}
            }]
        }),
        "an empty collection must not produce a `children` key"
    );
}

#[test]
fn renamed_fields_keep_the_rust_error_key() {
    #[derive(Debug, Validate, Serialize)]
    struct Renamed {
        #[validate(nested)]
        #[serde(rename = "childList")]
        children: Vec<Child>,
    }

    let input = Renamed { children: vec![bad_child()] };
    let errors = input.validate().unwrap_err();

    // The historical error key is the Rust field name, not the serde rename.
    assert!(errors.errors().contains_key("children"));
    assert!(!errors.errors().contains_key("childList"));
}

#[test]
fn duplicate_codes_keep_separate_entries() {
    fn first_schema(_input: &Duplicates) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup"))
    }
    fn second_schema(_input: &Duplicates) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup"))
    }
    fn custom_dup(_value: &String) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup"))
    }

    #[derive(Debug, Validate)]
    #[validate(schema(function = first_schema, skip_on_field_errors = false))]
    #[validate(schema(function = second_schema, skip_on_field_errors = false))]
    struct Duplicates {
        #[validate(length(min = 1, code = "dup"), custom(function = custom_dup, code = "dup"))]
        name: String,
    }

    let errors = (Duplicates { name: String::new() }).validate().unwrap_err();

    match errors.errors().get("name") {
        Some(ValidationErrorsKind::Field(field_errors)) => {
            assert_eq!(field_errors.len(), 2);
            assert!(field_errors.iter().all(|err| err.code == "dup"));
        }
        other => panic!("expected field errors, got {other:?}"),
    }
    match errors.errors().get("__all__") {
        Some(ValidationErrorsKind::Field(schema_errors)) => {
            assert_eq!(schema_errors.len(), 2);
            assert!(schema_errors.iter().all(|err| err.code == "dup"));
        }
        other => panic!("expected schema errors, got {other:?}"),
    }
}

#[test]
fn merge_order_reversal_mutation_yields_the_same_tree() {
    // The derive merges nested fields first and schema errors last. A manual
    // implementation merging in the reversed order must produce the same tree.
    struct ManualParent {
        children: Vec<Child>,
    }

    impl Validate for ManualParent {
        fn validate(&self) -> Result<(), ValidationErrors> {
            let mut errors = ValidationErrors::new();
            // schema-level error merged *before* the nested collection
            errors.add("__all__", ValidationError::new("parent_schema"));
            errors.merge_self("children", self.children.validate());
            if errors.is_empty() { Ok(()) } else { Err(errors) }
        }
    }

    let derived = (Parent { children: vec![good_child(), bad_child()] }).validate().unwrap_err();
    let manual =
        (ManualParent { children: vec![good_child(), bad_child()] }).validate().unwrap_err();

    assert_eq!(derived, manual);
    assert_eq!(serde_json::to_value(&derived).unwrap(), serde_json::to_value(&manual).unwrap());
    assert_eq!(serde_json::to_value(&manual).unwrap(), expected_parent_tree());
}
