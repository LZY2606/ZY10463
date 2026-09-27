use std::borrow::Cow;
use std::collections::{hash_map::Entry::Vacant, BTreeMap, HashMap};

use serde::de::Error as DeError;
use serde::ser::Serialize;
use serde_derive::{Deserialize, Serialize};
use serde_json::{to_value, Value};

/// Internal key used by the `Validate` implementations of collection types to smuggle a
/// `ValidationErrorsKind::List` out of `validate()` before it is attached to the field path.
/// It is stripped when the errors are merged into a parent `ValidationErrors` and must never
/// appear in serialized output for a single collection level.
pub(crate) const TMP_COLLECTION_KEY: &str = "_tmp_validator";

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize)]
pub struct ValidationError {
    pub code: Cow<'static, str>,
    pub message: Option<Cow<'static, str>>,
    pub params: HashMap<Cow<'static, str>, Value>,
}

impl ValidationError {
    pub fn new(code: &'static str) -> ValidationError {
        ValidationError { code: Cow::from(code), message: None, params: HashMap::new() }
    }

    pub fn add_param<T: Serialize>(&mut self, name: Cow<'static, str>, val: &T) {
        self.params.insert(name, to_value(val).unwrap());
    }

    /// Adds a custom message to a `ValidationError` that will be used when displaying the
    /// `ValidationError`, instead of an auto-generated description.
    pub fn with_message(mut self, message: Cow<'static, str>) -> ValidationError {
        self.message = Some(message);
        self
    }
}

impl std::error::Error for ValidationError {
    fn description(&self) -> &str {
        &self.code
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        None
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(untagged)]
pub enum ValidationErrorsKind {
    Struct(Box<ValidationErrors>),
    List(BTreeMap<usize, Box<ValidationErrors>>),
    Field(Vec<ValidationError>),
}

impl<'de> serde::Deserialize<'de> for ValidationErrorsKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Go through `Value` so collection indices keep their `List` shape: a JSON object
        // whose keys all parse as indices (and whose values are all objects) is a `List`,
        // any other object is a `Struct`. Error JSON produced before this change stays
        // deserializable: field errors are arrays and struct-level objects only change
        // meaning when they carry the exact index shape a `List` serializes to.
        match serde::Deserialize::deserialize(deserializer)? {
            Value::Array(errors) => {
                <Vec<ValidationError> as serde::Deserialize>::deserialize(Value::Array(errors))
                    .map(ValidationErrorsKind::Field)
                    .map_err(D::Error::custom)
            }
            Value::Object(map) => {
                let is_list = !map.is_empty()
                    && map.keys().all(|key| key.parse::<usize>().is_ok())
                    && map.values().all(|value| value.is_object());
                if is_list {
                    <BTreeMap<usize, Box<ValidationErrors>> as serde::Deserialize>::deserialize(
                        Value::Object(map),
                    )
                    .map(ValidationErrorsKind::List)
                    .map_err(D::Error::custom)
                } else {
                    <ValidationErrors as serde::Deserialize>::deserialize(Value::Object(map))
                        .map(|errors| ValidationErrorsKind::Struct(Box::new(errors)))
                        .map_err(D::Error::custom)
                }
            }
            other => Err(D::Error::custom(format!(
                "expected an array of field errors or an object of nested errors, got {other}"
            ))),
        }
    }
}

#[derive(Default, Debug, Serialize, Deserialize, Clone, PartialEq)]
pub struct ValidationErrors(pub HashMap<Cow<'static, str>, ValidationErrorsKind>);

impl ValidationErrors {
    pub fn new() -> ValidationErrors {
        ValidationErrors(HashMap::new())
    }

    /// Returns a boolean indicating whether a validation result includes validation errors for a
    /// given field. May be used as a condition for performing nested struct validations on a field
    /// in the absence of field-level validation errors.
    #[must_use]
    pub fn has_error(result: &Result<(), ValidationErrors>, field: &'static str) -> bool {
        match result {
            Ok(()) => false,
            Err(ref errs) => errs.contains_key(field),
        }
    }

    pub fn merge_self(
        &mut self,
        field: &'static str,
        child: Result<(), ValidationErrors>,
    ) -> &mut ValidationErrors {
        match child {
            Ok(()) => self,
            Err(errors) => {
                self.add_nested(field, errors.into_kind());
                self
            }
        }
    }

    /// Returns the combined outcome of a struct's validation result along with the nested
    /// validation result for one of its fields.
    ///
    /// If the child result comes from a collection of validating structs, the collection
    /// indices are attached directly to `field` as a `ValidationErrorsKind::List`, exactly
    /// like [`ValidationErrors::merge_self`] does.
    pub fn merge(
        parent: Result<(), ValidationErrors>,
        field: &'static str,
        child: Result<(), ValidationErrors>,
    ) -> Result<(), ValidationErrors> {
        match child {
            Ok(()) => parent,
            Err(errors) => {
                parent.and_then(|_| Err(ValidationErrors::new())).map_err(|mut parent_errors| {
                    parent_errors.add_nested(field, errors.into_kind());
                    parent_errors
                })
            }
        }
    }

    /// Returns the combined outcome of a struct's validation result along with the nested
    /// validation result for one of its fields where that field is a vector of validating structs.
    ///
    /// Each child result keeps its own index in the resulting `ValidationErrorsKind::List`.
    /// Two child shapes are recognised:
    ///
    /// - the wrapped shape produced by `ValidationErrors::merge(.., field, ..)`, i.e. a sole
    ///   `{field: Struct(errors)}` entry, which is unwrapped one level;
    /// - a raw `element.validate()` result, which is used as-is so that field errors,
    ///   schema-level (`__all__`) errors and nested errors of the element all stay attached
    ///   to the element's index instead of being silently dropped.
    pub fn merge_all(
        parent: Result<(), ValidationErrors>,
        field: &'static str,
        children: Vec<Result<(), ValidationErrors>>,
    ) -> Result<(), ValidationErrors> {
        let errors = children
            .into_iter()
            .enumerate()
            .filter_map(|(i, res)| res.err().map(|err| (i, err)))
            .map(|(i, err)| (i, Box::new(err.into_element(field))))
            .collect::<BTreeMap<_, _>>();

        if errors.is_empty() {
            parent
        } else {
            parent.and_then(|_| Err(ValidationErrors::new())).map_err(|mut parent_errors| {
                parent_errors.add_nested(field, ValidationErrorsKind::List(errors));
                parent_errors
            })
        }
    }

    /// Converts the outcome of a nested `validate()` call into the `ValidationErrorsKind`
    /// to attach to a field path, stripping the internal carrier entry that collection
    /// `Validate` implementations use to smuggle a `List` out of `validate()`.
    fn into_kind(mut self) -> ValidationErrorsKind {
        if self.0.len() == 1 {
            if let Some(kind) = self.0.remove(TMP_COLLECTION_KEY) {
                return kind;
            }
        }
        ValidationErrorsKind::Struct(Box::new(self))
    }

    /// Extracts the element-level errors of a single collection member's validation result,
    /// see [`ValidationErrors::merge_all`] for the recognised shapes.
    fn into_element(mut self, field: &str) -> ValidationErrors {
        let is_wrapped =
            self.0.len() == 1 && matches!(self.0.get(field), Some(ValidationErrorsKind::Struct(_)));
        if is_wrapped {
            if let Some(ValidationErrorsKind::Struct(inner)) = self.0.remove(field) {
                return *inner;
            }
        }
        self
    }

    /// Returns a map of field-level validation errors found for the struct that was validated and
    /// any of it's nested structs that are tagged for validation.
    pub fn errors(&self) -> &HashMap<Cow<'static, str>, ValidationErrorsKind> {
        &self.0
    }

    /// Returns a mutable map of field-level validation errors found for the struct that was validated and
    /// any of it's nested structs that are tagged for validation.
    pub fn errors_mut(&mut self) -> &mut HashMap<Cow<'static, str>, ValidationErrorsKind> {
        &mut self.0
    }

    /// Consume the struct, returning the validation errors found
    pub fn into_errors(self) -> HashMap<Cow<'static, str>, ValidationErrorsKind> {
        self.0
    }

    /// Returns a map of only field-level validation errors found for the struct that was validated.
    pub fn field_errors(&self) -> HashMap<Cow<'static, str>, &Vec<ValidationError>> {
        self.0
            .iter()
            .filter_map(|(k, v)| {
                if let ValidationErrorsKind::Field(errors) = v {
                    Some((k.clone(), errors))
                } else {
                    None
                }
            })
            .collect::<HashMap<_, _>>()
    }

    pub fn add(&mut self, field: &'static str, error: ValidationError) {
        if let ValidationErrorsKind::Field(ref mut vec) = self
            .0
            .entry(Cow::Borrowed(field))
            .or_insert_with(|| ValidationErrorsKind::Field(vec![]))
        {
            vec.push(error);
        } else {
            panic!("Attempt to add field validation to a non-Field ValidationErrorsKind instance");
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn add_nested(&mut self, field: &'static str, errors: ValidationErrorsKind) {
        if let Vacant(entry) = self.0.entry(Cow::Borrowed(field)) {
            entry.insert(errors);
        } else {
            panic!("Attempt to replace non-empty ValidationErrors entry");
        }
    }

    #[must_use]
    fn contains_key(&self, field: &'static str) -> bool {
        self.0.contains_key(field)
    }
}

impl std::error::Error for ValidationErrors {
    fn description(&self) -> &str {
        "Validation failed"
    }
    fn cause(&self) -> Option<&dyn std::error::Error> {
        None
    }
}
