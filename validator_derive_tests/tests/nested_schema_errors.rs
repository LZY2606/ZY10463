//! Regression tests for "nested collection indices are lost in schema errors".
//!
//! Injection scenario: the second element of a `Vec` fails field validation
//! and schema validation at the same time; after merging, the schema error
//! must stay attached to the element index instead of overriding the field
//! error, being hoisted to the container, or being detached behind the
//! internal `_tmp_validator` marker. See `docs/validation-errors-merge.md`.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};

use serde::Serialize;
use validator::{Validate, ValidationError, ValidationErrors, ValidationErrorsKind};

fn check_inner(inner: &Inner) -> Result<(), ValidationError> {
    if inner.name.len() < 5 { Err(ValidationError::new("schema_meh")) } else { Ok(()) }
}

#[derive(Validate, Debug, Serialize)]
#[validate(schema(function = "check_inner", skip_on_field_errors = false))]
struct Inner {
    #[validate(length(min = 5))]
    name: String,
}

fn bad_inner() -> Inner {
    Inner { name: "bad".to_string() }
}

fn good_inner() -> Inner {
    Inner { name: "valid_name".to_string() }
}

fn expected_length_error() -> ValidationError {
    let mut error = ValidationError::new("length");
    error.add_param(Cow::Borrowed("min"), &5u64);
    error.add_param(Cow::Borrowed("value"), &"bad");
    error
}

/// The error tree of one invalid `Inner`, as attached below a list index.
fn expected_inner_kind() -> ValidationErrorsKind {
    ValidationErrorsKind::Struct(Box::new(ValidationErrors(HashMap::from_iter([
        (Cow::Borrowed("name"), ValidationErrorsKind::Field(vec![expected_length_error()])),
        (
            Cow::Borrowed("__all__"),
            ValidationErrorsKind::Field(vec![ValidationError::new("schema_meh")]),
        ),
    ]))))
}

fn expected_outer_errors() -> ValidationErrors {
    ValidationErrors(HashMap::from_iter([(
        Cow::Borrowed("items"),
        ValidationErrorsKind::List(BTreeMap::from([(1usize, Box::new(expected_inner_kind()))])),
    )]))
}

fn expected_outer_json() -> serde_json::Value {
    serde_json::json!({
        "items": {
            "1": {
                "name": [{
                    "code": "length",
                    "message": null,
                    "params": {"min": 5, "value": "bad"},
                }],
                "__all__": [{
                    "code": "schema_meh",
                    "message": null,
                    "params": {},
                }],
            },
        },
    })
}

fn assert_no_tmp_marker(errors: &ValidationErrors) {
    let json = serde_json::to_string(errors).unwrap();
    assert!(
        !json.contains("_tmp_validator"),
        "internal `_tmp_validator` marker leaked into serialization: {json}"
    );
}

#[test]
fn vec_element_field_and_schema_errors_keep_their_index() {
    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Vec<Inner>,
    }

    let outer = Outer { items: vec![good_inner(), bad_inner()] };
    let errs = outer.validate().unwrap_err();

    // Full error tree comparison.
    assert_eq!(errs, expected_outer_errors());

    // Negative assertions: the schema error is neither hoisted to the
    // container nor disguised as a field of it, and the valid element does
    // not show up in the tree.
    assert!(!errs.errors().contains_key("__all__"));
    match &errs.errors()["items"] {
        ValidationErrorsKind::List(map) => {
            assert!(!map.contains_key(&0));
            let element = match &*map[&1] {
                ValidationErrorsKind::Struct(errors) => errors,
                other => panic!("Expected struct validation errors, got {other:?}"),
            };
            assert!(element.errors().contains_key("name"));
            assert!(element.errors().contains_key("__all__"));
        }
        other => panic!("Expected list validation errors, got {other:?}"),
    }

    // Stable serialization of the same tree.
    assert_eq!(serde_json::to_value(&errs).unwrap(), expected_outer_json());
    assert_no_tmp_marker(&errs);
}

#[test]
fn option_vec_keeps_element_indices() {
    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Option<Vec<Inner>>,
    }

    let outer = Outer { items: Some(vec![good_inner(), bad_inner()]) };
    let errs = outer.validate().unwrap_err();
    assert_eq!(errs, expected_outer_errors());
    assert_eq!(serde_json::to_value(&errs).unwrap(), expected_outer_json());

    // `None` and empty collections produce no errors at all.
    assert!(Outer { items: None }.validate().is_ok());
    assert!(Outer { items: Some(Vec::new()) }.validate().is_ok());
}

#[test]
fn empty_vec_produces_no_error_entry() {
    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Vec<Inner>,
    }

    assert!(Outer { items: Vec::new() }.validate().is_ok());
}

#[test]
fn multi_layer_vec_keeps_nested_indices() {
    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Vec<Vec<Inner>>,
    }

    let outer = Outer { items: vec![vec![good_inner()], vec![good_inner(), bad_inner()]] };
    let errs = outer.validate().unwrap_err();

    let json = serde_json::to_value(&errs).unwrap();
    assert_eq!(json["items"]["1"]["1"]["name"][0]["code"], "length");
    assert_eq!(json["items"]["1"]["1"]["__all__"][0]["code"], "schema_meh");
    // The first row and its valid element stay out of the tree.
    assert!(json["items"].get("0").is_none() || json["items"]["0"].is_null());
    assert_no_tmp_marker(&errs);

    // The same nesting is visible through the typed tree.
    match &errs.errors()["items"] {
        ValidationErrorsKind::List(rows) => match &*rows[&1] {
            ValidationErrorsKind::List(cells) => {
                assert_eq!(cells[&1], Box::new(expected_inner_kind()));
            }
            other => panic!("Expected nested list validation errors, got {other:?}"),
        },
        other => panic!("Expected list validation errors, got {other:?}"),
    }
}

#[test]
fn custom_codes_and_duplicate_codes_stay_distinguishable() {
    fn schema_a(_inner: &Coded) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup_code"))
    }

    fn schema_b(_inner: &Coded) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup_code"))
    }

    fn custom_a(_name: &str) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup_code"))
    }

    fn custom_b(_name: &str) -> Result<(), ValidationError> {
        Err(ValidationError::new("dup_code"))
    }

    #[derive(Validate)]
    #[validate(schema(function = "schema_a", skip_on_field_errors = false))]
    #[validate(schema(function = "schema_b", skip_on_field_errors = false))]
    struct Coded {
        #[validate(custom(function = "custom_a", use_context = false))]
        #[validate(custom(function = "custom_b", use_context = false))]
        name: String,
    }

    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Vec<Coded>,
    }

    let outer = Outer { items: vec![Coded { name: "x".to_string() }] };
    let errs = outer.validate().unwrap_err();

    let json = serde_json::to_value(&errs).unwrap();
    // Duplicate codes are preserved in order, per element, both for field
    // errors and for struct-level (`__all__`) errors.
    let field_codes: Vec<_> = json["items"]["0"]["name"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert_eq!(field_codes, ["dup_code", "dup_code"]);
    let schema_codes: Vec<_> = json["items"]["0"]["__all__"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["code"].as_str().unwrap())
        .collect();
    assert_eq!(schema_codes, ["dup_code", "dup_code"]);
}

#[test]
fn custom_schema_code_is_kept_at_the_element_index() {
    fn coded_schema(_inner: &InnerWithCodedSchema) -> Result<(), ValidationError> {
        Err(ValidationError::new("my_custom_code"))
    }

    #[derive(Validate)]
    #[validate(schema(
        function = "coded_schema",
        code = "overridden_code",
        skip_on_field_errors = false
    ))]
    struct InnerWithCodedSchema {
        #[validate(length(min = 5))]
        name: String,
    }

    #[derive(Validate)]
    struct Outer {
        #[validate(nested)]
        items: Vec<InnerWithCodedSchema>,
    }

    let outer = Outer { items: vec![InnerWithCodedSchema { name: "bad".to_string() }] };
    let errs = outer.validate().unwrap_err();

    let json = serde_json::to_value(&errs).unwrap();
    assert_eq!(json["items"]["0"]["__all__"][0]["code"], "overridden_code");
    assert_no_tmp_marker(&errs);
}

#[test]
fn renamed_fields_keep_the_rust_field_name_as_error_path() {
    #[derive(Validate, Serialize)]
    struct Outer {
        #[serde(rename = "renamedItems")]
        #[validate(nested)]
        items: Vec<Inner>,
    }

    let outer = Outer { items: vec![bad_inner()] };
    let errs = outer.validate().unwrap_err();

    // The validator error path follows the Rust field name; serde's rename
    // only affects serialization of `Outer` itself.
    assert!(errs.errors().contains_key("items"));
    assert!(!errs.errors().contains_key("renamedItems"));
    assert!(matches!(errs.errors()["items"], ValidationErrorsKind::List(_)));
}

#[test]
fn merge_order_reversal_mutation_yields_the_same_tree() {
    // Mutation of the injection scenario with the merge order reversed: the
    // parent field error is recorded before vs. after the nested collection
    // is merged. Both orders must produce the identical tree and the same
    // serialized value.
    #[derive(Validate)]
    struct Outer {
        #[validate(length(min = 5))]
        title: String,
        #[validate(nested)]
        items: Vec<Inner>,
    }

    let outer = Outer { title: "bad".to_string(), items: vec![good_inner(), bad_inner()] };
    let derived = outer.validate().unwrap_err();

    // Order A: parent field error first, nested collection second.
    let mut order_a = ValidationErrors::new();
    let mut title_err = ValidationError::new("length");
    title_err.add_param(Cow::Borrowed("min"), &5u64);
    title_err.add_param(Cow::Borrowed("value"), &"bad");
    order_a.add("title", title_err.clone());
    order_a.merge_self("items", vec![good_inner(), bad_inner()].validate());

    // Order B: nested collection first, parent field error second.
    let mut order_b = ValidationErrors::new();
    order_b.merge_self("items", vec![good_inner(), bad_inner()].validate());
    order_b.add("title", title_err);

    assert_eq!(order_a, order_b);
    assert_eq!(derived, order_a);
    assert_eq!(serde_json::to_value(&order_a).unwrap(), serde_json::to_value(&order_b).unwrap());

    // The derived tree contains both the parent field error and the indexed
    // element errors at the same time.
    let json = serde_json::to_value(&derived).unwrap();
    assert_eq!(json["title"][0]["code"], "length");
    assert_eq!(json["items"]["1"]["__all__"][0]["code"], "schema_meh");
    assert_no_tmp_marker(&derived);
}
