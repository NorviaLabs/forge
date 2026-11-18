//! Checking that a tool's schema, as sent to the model, accepts and refuses
//! exactly the calls the schema it was generated from does.
//!
//! The validator is passed in (`errors(schema, arguments)`, the paths of the
//! errors found, none when the arguments are valid), so this works with
//! whatever validation the caller's tools use.

use serde_json::{json, Map, Value};

/// Strings an `enum` or `const` anywhere in `schema` allows: values worth
/// trying for a property.
fn allowed_values(schema: &Value, found: &mut Vec<Value>) {
    match schema {
        Value::Object(map) => {
            for (key, value) in map {
                match (key.as_str(), value) {
                    ("enum", Value::Array(items)) => found.extend(items.iter().cloned()),
                    ("const", item) => found.push(item.clone()),
                    _ => allowed_values(value, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| allowed_values(item, found)),
        _ => {}
    }
}

/// Arguments to try against `schema`: none; the required fields filled in
/// with values the schema accepts; each property set to `null` and to each
/// kind of JSON value; the same one level down, inside a property holding
/// an object or an array of objects; an unknown property.
pub fn argument_samples(
    schema: &Value,
    errors: &dyn Fn(&Value, &Value) -> Vec<String>,
) -> Vec<Value> {
    let mut candidates = vec![
        Value::Null,
        json!("x"),
        json!(""),
        json!(1),
        json!(0),
        json!(-1),
        json!(1.5),
        json!(true),
        json!([]),
        json!(["x"]),
        json!([{}]),
        json!({}),
    ];
    allowed_values(schema, &mut candidates);
    let properties: Vec<String> = schema["properties"]
        .as_object()
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    let required: Vec<String> = schema["required"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();

    // The required fields, each with the first value the schema accepts
    // for it.
    let mut filled = Map::new();
    for field in &required {
        let pointer = format!("/{field}");
        let accepted = candidates.iter().find(|candidate| {
            let mut trial = filled.clone();
            trial.insert(field.clone(), (*candidate).clone());
            !errors(schema, &Value::Object(trial))
                .iter()
                .any(|path| path.starts_with(&pointer))
        });
        filled.insert(field.clone(), accepted.cloned().unwrap_or(Value::Null));
    }

    let mut samples = vec![json!({}), Value::Object(filled.clone())];
    for field in &properties {
        for candidate in &candidates {
            let mut sample = filled.clone();
            sample.insert(field.clone(), candidate.clone());
            samples.push(Value::Object(sample));
        }
    }
    // One level deeper: a property holding an object (or an array of
    // objects) gets that object's fields tried the same way.
    for field in &properties {
        let property = &schema["properties"][field];
        if let Some(object) = object_of(property, schema) {
            nested_samples(
                schema,
                &filled,
                field,
                false,
                object,
                &candidates,
                errors,
                &mut samples,
            );
        }
        if let Some(object) = resolve(property, schema)
            .get("items")
            .and_then(|items| object_of(items, schema))
        {
            nested_samples(
                schema,
                &filled,
                field,
                true,
                object,
                &candidates,
                errors,
                &mut samples,
            );
        }
    }

    let mut unknown = filled;
    unknown.insert("not_a_field".into(), json!(1));
    samples.push(Value::Object(unknown));
    samples
}

/// `schema` with a `$ref` into the root's `$defs` followed.
fn resolve<'a>(schema: &'a Value, root: &'a Value) -> &'a Value {
    schema
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix("#/$defs/"))
        .and_then(|name| root.get("$defs").and_then(|defs| defs.get(name)))
        .unwrap_or(schema)
}

/// The object schema (one with `properties`) `schema` is, or one of its
/// `anyOf`/`oneOf`/`allOf` branches is.
fn object_of<'a>(schema: &'a Value, root: &'a Value) -> Option<&'a Value> {
    let schema = resolve(schema, root);
    if schema.get("properties").is_some_and(Value::is_object) {
        return Some(schema);
    }
    ["anyOf", "oneOf", "allOf"]
        .iter()
        .filter_map(|keyword| schema.get(*keyword).and_then(Value::as_array))
        .flatten()
        .find_map(|branch| object_of(branch, root))
}

/// Samples with the top-level `filled` arguments and `field` holding an
/// `object` (inside a one-item array when `array`): its required fields
/// filled, then each of its fields set to each candidate.
#[allow(clippy::too_many_arguments)]
fn nested_samples(
    schema: &Value,
    filled: &Map<String, Value>,
    field: &str,
    array: bool,
    object: &Value,
    candidates: &[Value],
    errors: &dyn Fn(&Value, &Value) -> Vec<String>,
    samples: &mut Vec<Value>,
) {
    let wrap = |inner: Map<String, Value>| -> Value {
        let mut sample = filled.clone();
        let inner = Value::Object(inner);
        sample.insert(
            field.to_string(),
            if array { json!([inner]) } else { inner },
        );
        Value::Object(sample)
    };
    let prefix = if array {
        format!("/{field}/0")
    } else {
        format!("/{field}")
    };
    let mut inner = Map::new();
    for nested in object["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        let pointer = format!("{prefix}/{nested}");
        let accepted = candidates.iter().find(|candidate| {
            let mut trial = inner.clone();
            trial.insert(nested.to_string(), (*candidate).clone());
            !errors(schema, &wrap(trial))
                .iter()
                .any(|path| path.starts_with(&pointer))
        });
        inner.insert(nested.to_string(), accepted.cloned().unwrap_or(Value::Null));
    }
    samples.push(wrap(inner.clone()));
    for nested in object["properties"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(name, _)| name)
    {
        for candidate in candidates {
            let mut trial = inner.clone();
            trial.insert(nested.clone(), candidate.clone());
            samples.push(wrap(trial));
        }
    }
}

/// Panic unless `reported` accepts and refuses exactly the samples
/// `generated` does, with at least one of each (so the comparison is not
/// vacuous).
pub fn assert_same_validation(
    name: &str,
    generated: &Value,
    reported: &Value,
    errors: &dyn Fn(&Value, &Value) -> Vec<String>,
) {
    let samples = argument_samples(generated, errors);
    let mut valid = 0;
    for sample in &samples {
        let expected = errors(generated, sample).is_empty();
        assert_eq!(
            errors(reported, sample).is_empty(),
            expected,
            "{name}: {sample} was {} before",
            if expected { "accepted" } else { "refused" }
        );
        valid += usize::from(expected);
    }
    assert!(
        valid > 0,
        "{name}: no sample is valid, so nothing was compared"
    );
    assert!(valid < samples.len(), "{name}: every sample is valid");
}
