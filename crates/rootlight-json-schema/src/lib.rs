//! First-party JSON Schema validation for the checked Rootlight contracts.
//!
//! The supported vocabulary is intentionally explicit. Schema compilation
//! rejects unsupported assertions so a future generator change cannot silently
//! turn input or output validation into a permissive no-op.

mod pattern;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use pattern::Pattern;
use serde_json::{Number, Value};

const MAX_VALIDATION_DEPTH: usize = 256;

/// One invalid instance location and its failed schema assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    instance_path: String,
    message: String,
}

impl ValidationError {
    /// Returns the failing instance location as an RFC 6901 pointer.
    pub fn instance_path(&self) -> &str {
        &self.instance_path
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.message, self.instance_path)
    }
}

impl std::error::Error for ValidationError {}

/// A compiled checked-contract validator.
#[derive(Debug)]
pub struct Validator {
    schema: Value,
    patterns: BTreeMap<String, Pattern>,
}

impl Validator {
    /// Compiles one schema, rejecting unsupported or malformed assertions.
    ///
    /// # Errors
    /// Returns a descriptive error if a keyword, reference, or pattern cannot
    /// be validated by this first-party implementation.
    pub fn compile(schema: &Value) -> Result<Self, String> {
        let mut patterns = BTreeMap::new();
        check_schema(schema, schema, &mut patterns)?;
        Ok(Self {
            schema: schema.clone(),
            patterns,
        })
    }

    /// Returns whether an instance satisfies this schema.
    pub fn is_valid(&self, instance: &Value) -> bool {
        self.validate(instance).is_ok()
    }

    /// Validates an instance and returns the first failing location.
    ///
    /// # Errors
    /// Returns the failed assertion and an RFC 6901 instance path.
    pub fn validate(&self, instance: &Value) -> Result<(), ValidationError> {
        evaluate(&self.schema, &self.schema, &self.patterns, instance, "", 0).map(|_| ())
    }
}

/// Checked subset of JSON Schema Draft 2020-12 used by Rootlight contracts.
pub mod draft202012 {
    use serde_json::Value;

    use crate::Validator;

    /// Compiles one checked contract using the supported Draft 2020-12 vocabulary.
    ///
    /// # Errors
    /// Rejects malformed schemas and assertions that Rootlight cannot enforce.
    pub fn new(schema: &Value) -> Result<Validator, String> {
        Validator::compile(schema)
    }
}

fn invalid(path: &str, message: &str) -> ValidationError {
    ValidationError {
        instance_path: path.to_owned(),
        message: message.to_owned(),
    }
}

fn child_path(parent: &str, segment: &str) -> String {
    format!("{parent}/{}", segment.replace('~', "~0").replace('/', "~1"))
}

fn evaluate(
    schema: &Value,
    root: &Value,
    patterns: &BTreeMap<String, Pattern>,
    instance: &Value,
    path: &str,
    depth: usize,
) -> Result<BTreeSet<String>, ValidationError> {
    if depth >= MAX_VALIDATION_DEPTH {
        return Err(invalid(path, "schema validation depth exceeded"));
    }
    if let Some(boolean) = schema.as_bool() {
        return if boolean {
            Ok(BTreeSet::new())
        } else {
            Err(invalid(path, "false schema"))
        };
    }
    let object = schema
        .as_object()
        .ok_or_else(|| invalid(path, "invalid schema"))?;
    if let Some(expected) = object.get("type") {
        let accepted = expected.as_array().map_or_else(
            || type_matches(expected.as_str().unwrap_or(""), instance),
            |choices| {
                choices
                    .iter()
                    .any(|choice| type_matches(choice.as_str().unwrap_or(""), instance))
            },
        );
        if !accepted {
            return Err(invalid(path, "type mismatch"));
        }
    }
    if let Some(expected) = object.get("const")
        && !json_equal(expected, instance)
    {
        return Err(invalid(path, "constant mismatch"));
    }
    if let Some(actual) = instance.as_number() {
        if let Some(minimum) = object.get("minimum").and_then(Value::as_number)
            && matches!(compare_numbers(actual, minimum), Some(Ordering::Less))
        {
            return Err(invalid(path, "below minimum"));
        }
        if let Some(maximum) = object.get("maximum").and_then(Value::as_number)
            && matches!(compare_numbers(actual, maximum), Some(Ordering::Greater))
        {
            return Err(invalid(path, "above maximum"));
        }
    }
    if let Some(text) = instance.as_str() {
        let length = text.chars().count() as u64;
        if object
            .get("minLength")
            .and_then(Value::as_u64)
            .is_some_and(|min| length < min)
        {
            return Err(invalid(path, "string below minimum length"));
        }
        if object
            .get("maxLength")
            .and_then(Value::as_u64)
            .is_some_and(|max| length > max)
        {
            return Err(invalid(path, "string above maximum length"));
        }
        if let Some(source) = object.get("pattern").and_then(Value::as_str)
            && !patterns
                .get(source)
                .is_some_and(|pattern| pattern.matches(text))
        {
            return Err(invalid(path, "pattern mismatch"));
        }
    }
    if let Some(items) = instance.as_array() {
        if object
            .get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|min| (items.len() as u64) < min)
        {
            return Err(invalid(path, "array below minimum length"));
        }
        if object
            .get("maxItems")
            .and_then(Value::as_u64)
            .is_some_and(|max| (items.len() as u64) > max)
        {
            return Err(invalid(path, "array above maximum length"));
        }
        if object.get("uniqueItems") == Some(&Value::Bool(true)) {
            for (index, value) in items.iter().enumerate() {
                if items[..index]
                    .iter()
                    .any(|previous| json_equal(previous, value))
                {
                    return Err(invalid(path, "array items are not unique"));
                }
            }
        }
        if let Some(item_schema) = object.get("items") {
            for (index, item) in items.iter().enumerate() {
                evaluate(
                    item_schema,
                    root,
                    patterns,
                    item,
                    &child_path(path, &index.to_string()),
                    depth + 1,
                )?;
            }
        }
    }

    let mut evaluated = BTreeSet::new();
    if let Some(members) = instance.as_object() {
        let properties = object.get("properties").and_then(Value::as_object);
        let pattern_properties = object.get("patternProperties").and_then(Value::as_object);
        for (name, value) in members {
            let member_path = child_path(path, name);
            if let Some(name_schema) = object.get("propertyNames") {
                evaluate(
                    name_schema,
                    root,
                    patterns,
                    &Value::String(name.clone()),
                    &member_path,
                    depth + 1,
                )?;
            }
            let mut matched = false;
            if let Some(property_schema) = properties.and_then(|properties| properties.get(name)) {
                evaluate(
                    property_schema,
                    root,
                    patterns,
                    value,
                    &member_path,
                    depth + 1,
                )?;
                matched = true;
            }
            if let Some(pattern_properties) = pattern_properties {
                for (source, property_schema) in pattern_properties {
                    if patterns
                        .get(source)
                        .is_some_and(|pattern| pattern.matches(name))
                    {
                        evaluate(
                            property_schema,
                            root,
                            patterns,
                            value,
                            &member_path,
                            depth + 1,
                        )?;
                        matched = true;
                    }
                }
            }
            if matched {
                evaluated.insert(name.clone());
            } else if let Some(additional) = object.get("additionalProperties") {
                evaluate(additional, root, patterns, value, &member_path, depth + 1)?;
                evaluated.insert(name.clone());
            }
        }
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            for name in required.iter().filter_map(Value::as_str) {
                if !members.contains_key(name) {
                    return Err(invalid(path, "required property missing"));
                }
            }
        }
    }

    if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
        let pointer = reference
            .strip_prefix('#')
            .ok_or_else(|| invalid(path, "invalid reference"))?;
        let resolved = root
            .pointer(pointer)
            .ok_or_else(|| invalid(path, "unresolved reference"))?;
        evaluated.extend(evaluate(
            resolved,
            root,
            patterns,
            instance,
            path,
            depth + 1,
        )?);
    }
    if let Some(choices) = object.get("allOf").and_then(Value::as_array) {
        for choice in choices {
            evaluated.extend(evaluate(choice, root, patterns, instance, path, depth + 1)?);
        }
    }
    if let Some(choices) = object.get("anyOf").and_then(Value::as_array) {
        let mut accepted = false;
        let mut first_error = None;
        for choice in choices {
            match evaluate(choice, root, patterns, instance, path, depth + 1) {
                Ok(keys) => {
                    accepted = true;
                    evaluated.extend(keys);
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if !accepted {
            return Err(first_error.unwrap_or_else(|| invalid(path, "anyOf mismatch")));
        }
    }
    if let Some(choices) = object.get("oneOf").and_then(Value::as_array) {
        let mut selected = None;
        let mut first_error = None;
        for choice in choices {
            match evaluate(choice, root, patterns, instance, path, depth + 1) {
                Ok(keys) => {
                    if selected.replace(keys).is_some() {
                        return Err(invalid(path, "multiple oneOf alternatives matched"));
                    }
                }
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        evaluated.extend(
            selected
                .ok_or_else(|| first_error.unwrap_or_else(|| invalid(path, "oneOf mismatch")))?,
        );
    }
    if let Some(negated) = object.get("not")
        && evaluate(negated, root, patterns, instance, path, depth + 1).is_ok()
    {
        return Err(invalid(path, "negated schema matched"));
    }
    if let Some(members) = instance.as_object()
        && let Some(unevaluated) = object.get("unevaluatedProperties")
    {
        for (name, value) in members {
            if !evaluated.contains(name) {
                evaluate(
                    unevaluated,
                    root,
                    patterns,
                    value,
                    &child_path(path, name),
                    depth + 1,
                )?;
                evaluated.insert(name.clone());
            }
        }
    }
    Ok(evaluated)
}

fn type_matches(expected: &str, instance: &Value) -> bool {
    match expected {
        "null" => instance.is_null(),
        "boolean" => instance.is_boolean(),
        "object" => instance.is_object(),
        "array" => instance.is_array(),
        "number" => instance.is_number(),
        "integer" => instance.as_number().is_some_and(|number| {
            number.is_i64()
                || number.is_u64()
                || number.as_f64().is_some_and(|value| value.fract() == 0.0)
        }),
        "string" => instance.is_string(),
        _ => false,
    }
}

fn compare_numbers(left: &Number, right: &Number) -> Option<Ordering> {
    let integer = |number: &Number| {
        number
            .as_i64()
            .map(i128::from)
            .or_else(|| number.as_u64().map(i128::from))
    };
    match (integer(left), integer(right)) {
        (Some(left), Some(right)) => Some(left.cmp(&right)),
        (Some(left), None) => compare_integer_float(left, right.as_f64()?),
        (None, Some(right)) => compare_integer_float(right, left.as_f64()?).map(Ordering::reverse),
        (None, None) => left.as_f64()?.partial_cmp(&right.as_f64()?),
    }
}

fn compare_integer_float(integer: i128, float: f64) -> Option<Ordering> {
    if !float.is_finite() {
        return None;
    }
    if float < -9_223_372_036_854_775_808.0 {
        return Some(Ordering::Greater);
    }
    if float >= 18_446_744_073_709_551_616.0 {
        return Some(Ordering::Less);
    }
    match integer.cmp(&truncated_float_integer(float)?) {
        Ordering::Equal if float.fract() > 0.0 => Some(Ordering::Less),
        Ordering::Equal if float.fract() < 0.0 => Some(Ordering::Greater),
        order => Some(order),
    }
}

fn truncated_float_integer(float: f64) -> Option<i128> {
    let bits = float.to_bits();
    let exponent = i32::from(u16::try_from((bits >> 52) & 0x7ff).ok()?) - 1023;
    if exponent < 0 {
        return Some(0);
    }
    let significand = (1_u64 << 52) | (bits & ((1_u64 << 52) - 1));
    // The caller bounds finite values to [-2^63, 2^64), so this shift
    // retains every integer bit without f64-rounding an adjacent u64.
    let magnitude = if exponent >= 52 {
        i128::from(significand).checked_shl(u32::try_from(exponent - 52).ok()?)?
    } else {
        i128::from(significand) >> u32::try_from(52 - exponent).ok()?
    };
    Some(if bits >> 63 == 0 {
        magnitude
    } else {
        -magnitude
    })
}

fn json_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            compare_numbers(left, right) == Some(Ordering::Equal)
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(left, right)| json_equal(left, right))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(name, value)| {
                    right
                        .get(name)
                        .is_some_and(|other| json_equal(value, other))
                })
        }
        _ => left == right,
    }
}

fn check_schema(
    schema: &Value,
    root: &Value,
    patterns: &mut BTreeMap<String, Pattern>,
) -> Result<(), String> {
    let object = schema.as_object().ok_or("expected a schema object")?;
    for (key, value) in object {
        match key.as_str() {
            "$id" | "title" | "description" | "default" | "x-rootlight-max-utf8-bytes" => {}
            "$schema" => {
                if value.as_str() != Some("https://json-schema.org/draft/2020-12/schema") {
                    return Err("unsupported JSON Schema dialect".to_owned());
                }
            }
            "format" => {
                if !matches!(
                    value.as_str(),
                    Some("uint" | "uint8" | "uint16" | "uint32" | "uint64" | "int64" | "double")
                ) {
                    return Err("unsupported schema format".to_owned());
                }
            }
            "$ref" => {
                let reference = value.as_str().ok_or("$ref must be a string")?;
                let pointer = reference
                    .strip_prefix('#')
                    .ok_or("only local $ref is supported")?;
                root.pointer(pointer)
                    .ok_or_else(|| format!("unresolved $ref {reference}"))?;
            }
            "pattern" => {
                let source = value.as_str().ok_or("pattern must be a string")?;
                patterns.insert(source.to_owned(), Pattern::compile(source)?);
            }
            "$defs" | "properties" | "patternProperties" => {
                let entries = value.as_object().ok_or("expected a schema map")?;
                for (name, child) in entries {
                    if key == "patternProperties" {
                        patterns.insert(name.clone(), Pattern::compile(name)?);
                    }
                    check_schema(child, root, patterns)?;
                }
            }
            "anyOf" | "oneOf" | "allOf" => {
                let children = value.as_array().ok_or("expected a schema array")?;
                if children.is_empty() {
                    return Err(format!("{key} must not be empty"));
                }
                for child in children {
                    check_schema(child, root, patterns)?;
                }
            }
            "items"
            | "additionalProperties"
            | "unevaluatedProperties"
            | "not"
            | "propertyNames" => {
                if !value.is_boolean() {
                    check_schema(value, root, patterns)?;
                }
            }
            "type" => {
                let valid = |item: &Value| {
                    matches!(
                        item.as_str(),
                        Some(
                            "null"
                                | "boolean"
                                | "object"
                                | "array"
                                | "number"
                                | "integer"
                                | "string"
                        )
                    )
                };
                if !valid(value)
                    && !value
                        .as_array()
                        .is_some_and(|items| !items.is_empty() && items.iter().all(valid))
                {
                    return Err("invalid schema type".to_owned());
                }
            }
            "required" => {
                if !value
                    .as_array()
                    .is_some_and(|items| items.iter().all(|item| item.is_string()))
                {
                    return Err("required must be an array of strings".to_owned());
                }
            }
            "minimum" | "maximum" => {
                if !value.is_number() {
                    return Err(format!("{key} must be numeric"));
                }
            }
            "maxLength" | "minLength" | "maxItems" | "minItems" => {
                if value.as_u64().is_none() {
                    return Err(format!("{key} must be a nonnegative integer"));
                }
            }
            "uniqueItems" => {
                if !value.is_boolean() {
                    return Err("uniqueItems must be boolean".to_owned());
                }
            }
            "const" => {}
            _ => return Err(format!("unsupported JSON Schema keyword: {key}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Validator;
    use serde_json::{Value, json};

    #[test]
    fn compiles_every_generated_schema_without_ignoring_assertions() {
        let root =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schemas/generated/json");
        let mut count = 0;
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let schema: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            Validator::compile(&schema)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            count += 1;
        }
        assert!(count > 0, "no generated schemas were checked");
    }

    #[test]
    fn validates_composed_contract_assertions() {
        let schema = json!({
            "$defs": {
                "name": {"type": "string", "minLength": 2, "pattern": "^[a-z0-9_-]+$"},
                "member": {
                    "type": "object",
                    "properties": {"name": {"$ref": "#/$defs/name"}},
                    "required": ["name"],
                    "additionalProperties": false
                }
            },
            "type": "object",
            "anyOf": [
                {"properties": {"item": {"$ref": "#/$defs/member"}}, "required": ["item"]},
                {"properties": {"items": {"type": "array", "items": {"$ref": "#/$defs/member"}, "uniqueItems": true}}, "required": ["items"]}
            ],
            "unevaluatedProperties": false
        });
        let native = Validator::compile(&schema).unwrap();
        for (instance, expected) in [
            (json!({"item": {"name": "ab"}}), true),
            (json!({"items": [{"name": "ab"}, {"name": "cd"}]}), true),
            (json!({"items": []}), true),
            (json!({"item": {"name": "a"}}), false),
            (json!({"item": {"name": "ab", "extra": true}}), false),
            (json!({"item": {"name": "ab"}, "unknown": true}), false),
            (json!({"items": [{"name": "ab"}, {"name": "ab"}]}), false),
            (json!({"item": {"name": "ab"}, "items": []}), true),
            (json!({}), false),
        ] {
            assert_eq!(native.is_valid(&instance), expected, "{instance}");
        }
    }

    #[test]
    fn rejects_unsupported_assertions_at_compile_time() {
        for schema in [
            json!({"contains": {"type": "string"}}),
            json!({"pattern": "^[^a]$"}),
            json!({"$ref": "https://example.invalid/schema"}),
            json!({"format": "email"}),
        ] {
            assert!(Validator::compile(&schema).is_err(), "{schema}");
        }
    }

    #[test]
    fn reports_escaped_nested_instance_paths() {
        let schema = json!({
            "type": "object",
            "properties": {
                "a/b": {
                    "type": "object",
                    "properties": {"~": {"type": "integer"}},
                    "additionalProperties": false
                }
            },
            "additionalProperties": false
        });
        let validator = Validator::compile(&schema).unwrap();
        let error = validator
            .validate(&json!({"a/b": {"~": "wrong"}}))
            .unwrap_err();
        assert_eq!(error.instance_path(), "/a~1b/~0");
        let error = validator
            .validate(&json!({"a/b": {"other/key": 1}}))
            .unwrap_err();
        assert_eq!(error.instance_path(), "/a~1b/other~1key");
    }

    #[test]
    fn one_of_and_numeric_equality_preserve_json_semantics() {
        let validator = Validator::compile(&json!({
            "oneOf": [
                {"type": "integer", "minimum": 1},
                {"type": "string", "const": "one"}
            ]
        }))
        .unwrap();
        assert!(validator.is_valid(&json!(1.0)));
        assert!(validator.is_valid(&json!("one")));
        assert!(!validator.is_valid(&json!(0)));
        assert!(!validator.is_valid(&json!("two")));

        let unique = Validator::compile(&json!({"type": "array", "uniqueItems": true})).unwrap();
        assert!(!unique.is_valid(&json!([1, 1.0])));

        let maximum = Validator::compile(&json!({"type": "number", "maximum": u64::MAX})).unwrap();
        assert!(maximum.is_valid(&json!(u64::MAX)));
        assert!(!maximum.is_valid(&json!(18_446_744_073_709_551_616.0)));
        let constant = Validator::compile(&json!({"const": u64::MAX})).unwrap();
        assert!(!constant.is_valid(&json!(18_446_744_073_709_551_616.0)));
    }
}
