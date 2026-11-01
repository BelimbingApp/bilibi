//! This is the deliberately small
//! input-schema dialect. Unknown keywords fail
//! discovery; unknown input fields fail dispatch, including nested objects.
use serde_json::Value;
use std::cmp::Ordering;
use std::collections::BTreeMap;

pub type FieldErrors = BTreeMap<String, Vec<String>>;
const MAX_DEPTH: usize = 32;

pub fn check(schema: &Value) -> Result<(), String> {
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        return Err("input must be an object schema".into());
    }
    check_node(schema, 0)
}
fn check_node(node: &Value, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err("schema is too deeply nested".into());
    }
    let object = node.as_object().ok_or("schema must be an object")?;
    let kind = node
        .get("type")
        .and_then(Value::as_str)
        .ok_or("schema needs type")?;
    let keywords: &[&str] = match kind {
        "object" => &["properties", "required"],
        "string" => &["enum", "maxLength", "format", "nullable"],
        "integer" | "number" => &["enum", "minimum", "maximum", "nullable"],
        "boolean" => &["nullable"],
        "array" => &["items"],
        _ => return Err(format!("unknown schema type {kind}")),
    };
    for key in object.keys() {
        if ![
            "type",
            "title",
            "description",
            "writeOnly",
            "x-bilibi-reference",
            "x-bilibi-picklist",
            "x-bilibi-money",
        ]
        .contains(&key.as_str())
            && !keywords.contains(&key.as_str())
        {
            return Err(format!("unknown {kind} keyword {key}"));
        }
    }
    if node
        .get("writeOnly")
        .is_some_and(|value| kind != "string" || !value.is_boolean())
    {
        return Err("writeOnly requires a string field and a boolean".into());
    }
    for key in ["x-bilibi-reference", "x-bilibi-picklist"] {
        if let Some(value) = node.get(key) {
            if !matches!(kind, "integer" | "string")
                || value
                    .as_str()
                    .is_none_or(|text| text.is_empty() || text.len() > 255)
            {
                return Err(format!("{key} needs a scalar field and a non-empty key"));
            }
        }
    }
    if node
        .get("x-bilibi-money")
        .is_some_and(|value| kind != "string" || value != true)
    {
        return Err("x-bilibi-money requires an exact decimal string field and true".into());
    }
    for key in ["title", "description"] {
        if node.get(key).is_some_and(|v| !v.is_string()) {
            return Err(format!("{key} must be text"));
        }
    }
    if node
        .get("nullable")
        .is_some_and(|value| !value.is_boolean())
    {
        return Err("nullable must be boolean".into());
    }
    if kind == "object" {
        let properties = match node.get("properties") {
            Some(v) => v.as_object().ok_or("properties must be an object")?.clone(),
            None => Default::default(),
        };
        for child in properties.values() {
            check_node(child, depth + 1)?;
        }
        if let Some(required) = node.get("required") {
            let required = required.as_array().ok_or("required must be an array")?;
            let mut seen = std::collections::BTreeSet::new();
            for name in required {
                let name = name.as_str().ok_or("required names must be strings")?;
                if !properties.contains_key(name) || !seen.insert(name) {
                    return Err("required names must be unique declared properties".into());
                }
            }
        }
    }
    if kind == "array" {
        check_node(node.get("items").ok_or("array needs items")?, depth + 1)?;
    }
    if let Some(values) = node.get("enum") {
        let values = values.as_array().ok_or("enum must be an array")?;
        if values.is_empty() || values.iter().any(|v| !of_type(kind, v)) {
            return Err("enum must contain values of the schema type".into());
        }
    }
    for key in ["minimum", "maximum"] {
        if node.get(key).is_some_and(|v| !of_type(kind, v)) {
            return Err(format!("{key} must have schema's numeric type"));
        }
    }
    if node
        .get("minimum")
        .zip(node.get("maximum"))
        .is_some_and(|(a, b)| compare(a, b) == Some(Ordering::Greater))
    {
        return Err("minimum exceeds maximum".into());
    }
    if node.get("maxLength").is_some_and(|v| v.as_u64().is_none()) {
        return Err("maxLength must be a nonnegative integer".into());
    }
    if node
        .get("format")
        .is_some_and(|v| !matches!(v.as_str(), Some("date" | "date-time" | "email")))
    {
        return Err("unknown string format".into());
    }
    Ok(())
}
fn of_type(kind: &str, v: &Value) -> bool {
    match kind {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        _ => false,
    }
}
// Integer schemas must not round IDs or bounds through f64 above 2^53.
fn compare(a: &Value, b: &Value) -> Option<Ordering> {
    if let (Some(a), Some(b)) = (a.as_i64(), b.as_i64()) {
        return Some(a.cmp(&b));
    }
    if let (Some(a), Some(b)) = (a.as_u64(), b.as_u64()) {
        return Some(a.cmp(&b));
    }
    if a.is_i64() && b.is_u64() {
        return Some(Ordering::Less);
    }
    if a.is_u64() && b.is_i64() {
        return Some(Ordering::Greater);
    }
    a.as_f64()?.partial_cmp(&b.as_f64()?)
}
pub fn validate(schema: &Value, input: &Value) -> Result<(), FieldErrors> {
    validate_plan(schema, input, &Default::default())
}
pub(crate) fn validate_plan(
    schema: &Value,
    input: &Value,
    references: &std::collections::BTreeSet<String>,
) -> Result<(), FieldErrors> {
    let mut errors = FieldErrors::new();
    errors_at(schema, input, "", &mut errors, references);
    if input
        .get("page_size")
        .and_then(Value::as_u64)
        .is_some_and(|v| v > 100)
    {
        add(&mut errors, "page_size", "must be at most 100".into());
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
fn add(errors: &mut FieldErrors, path: &str, message: String) {
    errors.entry(path.into()).or_default().push(message);
}
fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.into()
    } else {
        format!("{path}.{name}")
    }
}
fn errors_at(
    schema: &Value,
    input: &Value,
    path: &str,
    errors: &mut FieldErrors,
    references: &std::collections::BTreeSet<String>,
) {
    if references.contains(path) {
        if !matches!(schema["type"].as_str(), Some("integer" | "string")) {
            add(errors, path, "a record reference needs an ID field".into());
        }
        return;
    }
    let kind = schema["type"].as_str().unwrap_or("");
    if input.is_null() && schema["nullable"].as_bool() == Some(true) {
        return;
    }
    if !of_type(kind, input) {
        add(errors, path, format!("must be of type {kind}"));
        return;
    }
    if let Some(object) = input.as_object() {
        for (key, value) in object {
            let child = &schema["properties"][key];
            let child_path = join(path, key);
            if child.is_null() {
                add(
                    errors,
                    &child_path,
                    "is not a field of this operation".into(),
                );
            } else {
                errors_at(child, value, &child_path, errors, references);
            }
        }
        if let Some(required) = schema["required"].as_array() {
            for name in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(name) {
                    add(errors, &join(path, name), "is required".into());
                }
            }
        }
    }
    if let Some(items) = input.as_array() {
        for (index, item) in items.iter().enumerate() {
            errors_at(
                &schema["items"],
                item,
                &join(path, &index.to_string()),
                errors,
                references,
            );
        }
    }
    if schema
        .get("enum")
        .and_then(Value::as_array)
        .is_some_and(|values| !values.contains(input))
    {
        add(
            errors,
            path,
            "must be one of the declared enum values".into(),
        );
    }
    for (key, holds, message) in [
        ("minimum", true, "must be at least"),
        ("maximum", false, "must be at most"),
    ] {
        if let Some(bound) = schema.get(key) {
            if compare(input, bound).is_some_and(|order| {
                if holds {
                    order == Ordering::Less
                } else {
                    order == Ordering::Greater
                }
            }) {
                add(errors, path, format!("{message} {}", schema[key]));
            }
        }
    }
    if let Some(text) = input.as_str() {
        if schema["maxLength"]
            .as_u64()
            .is_some_and(|n| text.chars().count() as u64 > n)
        {
            add(
                errors,
                path,
                format!("must be at most {} characters", schema["maxLength"]),
            );
        }
        let valid = match schema["format"].as_str() {
            Some("date") => text.len() == 10 && text.parse::<jiff::civil::Date>().is_ok(),
            Some("date-time") => text.parse::<jiff::Timestamp>().is_ok(),
            Some("email") => {
                let parts: Vec<_> = text.split('@').collect();
                parts.len() == 2
                    && !parts[0].is_empty()
                    && parts[1]
                        .split_once('.')
                        .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
                    && !text.chars().any(char::is_whitespace)
            }
            _ => true,
        };
        if !valid {
            add(
                errors,
                path,
                format!("must be a valid {}", schema["format"]),
            );
        }
    }
}
