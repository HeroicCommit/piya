//! Normalize MCP JSON Schemas for OpenAI function calling.

use serde_json::{json, Map, Value};

const MAX_SCHEMA_BYTES: usize = 6_000;

/// Strip unsupported / oversized schema fields before sending to OpenAI.
pub fn sanitize_openai_schema(schema: &Value) -> Value {
    let bytes = serde_json::to_string(schema).map(|s| s.len()).unwrap_or(0);
    if bytes > MAX_SCHEMA_BYTES {
        return generic_object_schema();
    }

    let mut out = simplify_value(schema, 0);
    if !out.is_object() {
        return generic_object_schema();
    }
    ensure_object_schema(&mut out);
    out
}

fn generic_object_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "additionalProperties": true
    })
}

fn ensure_object_schema(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        *value = generic_object_schema();
        return;
    };
    if !obj.contains_key("type") {
        obj.insert("type".into(), json!("object"));
    }
    if !obj.contains_key("properties") {
        obj.insert("properties".into(), json!({}));
    }
}

fn simplify_value(value: &Value, depth: usize) -> Value {
    if depth > 8 {
        return json!({});
    }

    match value {
        Value::Object(map) => {
            let mut out = Map::new();
            for (key, val) in map {
                if should_drop_key(key) {
                    continue;
                }
                match key.as_str() {
                    "properties" => {
                        if let Some(props) = val.as_object() {
                            let mut cleaned = Map::new();
                            for (pk, pv) in props {
                                cleaned.insert(pk.clone(), simplify_property(pv, depth + 1));
                            }
                            out.insert(key.clone(), Value::Object(cleaned));
                        }
                    }
                    "items" => {
                        out.insert(key.clone(), simplify_property(val, depth + 1));
                    }
                    "required" => {
                        if val.is_array() {
                            out.insert(key.clone(), val.clone());
                        }
                    }
                    "type" | "description" | "enum" | "minimum" | "maximum" | "minItems"
                    | "maxItems" | "pattern" | "format" => {
                        out.insert(key.clone(), val.clone());
                    }
                    "anyOf" | "oneOf" => {
                        if let Some(first) = val.as_array().and_then(|a| a.first()) {
                            out.insert("type".into(), json!(first.get("type").cloned().unwrap_or(json!("string"))));
                        }
                    }
                    _ => {}
                }
            }
            if !out.contains_key("type") && out.contains_key("properties") {
                out.insert("type".into(), json!("object"));
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| simplify_value(v, depth + 1))
                .collect(),
        ),
        _ => value.clone(),
    }
}

fn simplify_property(value: &Value, depth: usize) -> Value {
    if let Some(desc) = value.get("description").and_then(|d| d.as_str()) {
        if value.as_object().is_some_and(|o| o.len() == 1) {
            return json!({ "type": "string", "description": desc });
        }
    }
    simplify_value(value, depth)
}

fn should_drop_key(key: &str) -> bool {
    matches!(
        key,
        "$schema"
            | "$defs"
            | "definitions"
            | "$ref"
            | "outputSchema"
            | "annotations"
            | "title"
            | "examples"
            | "default"
            | "const"
            | "allOf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_schema_falls_back() {
        let huge = json!({ "type": "object", "properties": { "x": { "type": "string" } } });
        let mut big = huge.clone();
        for i in 0..500 {
            big["properties"][format!("field_{i}")] = json!({
                "type": "string",
                "description": "x".repeat(200)
            });
        }
        let out = sanitize_openai_schema(&big);
        assert_eq!(out["additionalProperties"], json!(true));
    }
}
