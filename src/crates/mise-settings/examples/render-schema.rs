//! Regenerate repository configuration schemas without Node.js.

use eyre::{Context, Result, bail, eyre};
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::path::Path;

fn main() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let settings: toml::Table = fs::read_to_string(root.join(".config/settings.toml"))
        .wrap_err("read settings metadata")?
        .parse()
        .wrap_err("parse settings metadata")?;
    let settings = serde_json::to_value(settings)?;
    let settings = settings
        .as_object()
        .ok_or_else(|| eyre!("settings metadata must be an object"))?;
    let mut schema: Value =
        serde_json::from_str(&fs::read_to_string(root.join("src/schema/mise.json"))?)?;
    let settings_schema = schema
        .pointer_mut("/$defs/settings")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("src/schema/mise.json is missing $defs.settings"))?;
    settings_schema.insert(
        "properties".into(),
        Value::Object(build_settings(settings, "")?),
    );

    let task_schema = build_task_schema(&schema)?;
    let rc_schema = build_rc_schema(settings)?;
    for (path, value) in [
        ("src/schema/mise.json", schema),
        ("src/schema/mise-task.json", task_schema),
        ("src/schema/miserc.json", rc_schema),
    ] {
        let output = format!("{}\n", serde_json::to_string_pretty(&value)?);
        fs::write(root.join(path), output).wrap_err_with(|| format!("write {path}"))?;
    }
    Ok(())
}

fn build_element(name: &str, props: &Map<String, Value>) -> Result<Value> {
    let setting_type = props
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let schema_type = match setting_type {
        "String" | "Path" | "Url" | "Duration" => "string",
        "Bool" => "boolean",
        "Integer" => "integer",
        "ListString" | "ListPath" | "SetString" => "array",
        "IndexMap<String, String>" => "object",
        "BoolOrString" => "union",
        _ => bail!("Unknown type for {name}: {setting_type}"),
    };
    let description = props
        .get("description")
        .and_then(Value::as_str)
        .filter(|description| !description.is_empty())
        .ok_or_else(|| eyre!("Missing description for {name}"))?;
    let mut element = Map::new();
    element.insert("description".into(), json!(description));
    if let Some(default) = props.get("default") {
        element.insert("default".into(), default.clone());
    }
    if props
        .get("deprecated")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.is_empty())
    {
        element.insert("deprecated".into(), json!(true));
    }
    if schema_type == "integer"
        && props
            .get("rust_type")
            .and_then(Value::as_str)
            .is_some_and(|value| value.starts_with('u'))
    {
        element.insert("minimum".into(), json!(0));
    }
    let enum_values = props.get("enum").and_then(Value::as_array).map(|values| {
        values
            .iter()
            .map(|value| value.get("value").unwrap_or(value).clone())
            .collect::<Vec<_>>()
    });
    match schema_type {
        "array" => {
            let mut items = json!({ "type": "string" });
            if let Some(values) = enum_values {
                items["enum"] = json!(values);
            }
            element.insert("items".into(), items);
        }
        _ => {
            if let Some(values) = enum_values {
                element.insert("enum".into(), json!(values));
            }
        }
    }
    if schema_type == "object" {
        element.insert("additionalProperties".into(), json!({ "type": "string" }));
    }
    if schema_type == "union" {
        element.insert(
            "oneOf".into(),
            json!([{ "type": "boolean" }, { "type": "string" }]),
        );
    } else {
        element.insert("type".into(), json!(schema_type));
    }
    Ok(Value::Object(element))
}

fn build_settings(table: &Map<String, Value>, prefix: &str) -> Result<Map<String, Value>> {
    let mut settings = Map::new();
    for (key, node) in table {
        let name = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        let props = node
            .as_object()
            .ok_or_else(|| eyre!("Setting {name} must be an object"))?;
        let element = if props.contains_key("type") {
            build_element(&name, props)?
        } else {
            json!({
                "type": "object",
                "unevaluatedProperties": false,
                "properties": build_settings(props, &name)?,
            })
        };
        settings.insert(key.clone(), element);
    }
    Ok(settings)
}

fn referenced_defs(schema: &Value, root: &Value) -> Result<Map<String, Value>> {
    let defs = schema
        .get("$defs")
        .and_then(Value::as_object)
        .ok_or_else(|| eyre!("src/schema/mise.json is missing $defs"))?;
    let mut queued = VecDeque::from([root]);
    let mut seen = BTreeSet::new();
    let mut picked = Map::new();
    while let Some(value) = queued.pop_front() {
        match value {
            Value::Array(values) => queued.extend(values),
            Value::Object(object) => {
                if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                    if let Some(path) = reference.strip_prefix("#/$defs/") {
                        let key = path.split('/').next().unwrap_or_default();
                        let definition = defs
                            .get(key)
                            .ok_or_else(|| eyre!("src/schema/mise.json is missing $defs.{key}"))?;
                        if seen.insert(key.to_owned()) {
                            picked.insert(key.to_owned(), definition.clone());
                            queued.push_back(definition);
                        }
                    } else if reference.starts_with("#/") {
                        bail!("unsupported local JSON schema ref: {reference}");
                    }
                }
                queued.extend(object.values());
            }
            _ => {}
        }
    }
    Ok(picked)
}

fn build_task_schema(schema: &Value) -> Result<Value> {
    let mut task_schema = json!({
        "$id": "https://mise.jdx.dev/schema/mise-task.json",
        "$schema": schema["$schema"],
        "title": "mise-task-schema",
        "type": "object",
        "description": "Config file for included mise tasks (https://mise.jdx.dev/tasks/#task-configuration)",
        "additionalProperties": { "$ref": "#/$defs/task" },
    });
    task_schema["$defs"] = Value::Object(referenced_defs(schema, &task_schema)?);
    Ok(task_schema)
}

fn build_rc_schema(settings: &Map<String, Value>) -> Result<Value> {
    let mut properties = Map::new();
    for (key, value) in settings {
        if let Some(props) = value.as_object()
            && props.contains_key("type")
            && props.get("rc").and_then(Value::as_bool) == Some(true)
        {
            properties.insert(key.clone(), build_element(key, props)?);
        }
    }
    Ok(json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "mise rc config",
        "description": "Early initialization settings for mise. These settings are loaded before the main config files.",
        "type": "object",
        "unevaluatedProperties": false,
        "properties": properties,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn element(props: Value) -> Value {
        build_element("example", props.as_object().unwrap()).unwrap()
    }

    #[test]
    fn setting_types_preserve_bounds_enums_and_optional_defaults() {
        assert_eq!(
            element(json!({ "type": "Integer", "description": "count", "rust_type": "u64" })),
            json!({ "type": "integer", "description": "count", "minimum": 0 }),
        );
        assert_eq!(
            element(json!({
                "type": "ListString", "description": "choices", "default": [],
                "enum": ["a", { "value": "b", "description": "second" }],
            })),
            json!({
                "type": "array", "description": "choices", "default": [],
                "items": { "type": "string", "enum": ["a", "b"] },
            }),
        );
        let union = element(json!({ "type": "BoolOrString", "description": "switch" }));
        assert!(union.get("type").is_none());
        assert_eq!(
            union["oneOf"],
            json!([{ "type": "boolean" }, { "type": "string" }])
        );
    }

    #[test]
    fn nested_settings_and_rc_only_include_applicable_properties() {
        let settings = json!({
            "direct": { "type": "Bool", "description": "early", "rc": true },
            "normal": { "type": "Bool", "description": "normal" },
            "nested": { "child": { "type": "Path", "description": "path", "rc": true } },
        });
        let settings = settings.as_object().unwrap();
        let generated = build_settings(settings, "").unwrap();
        assert_eq!(generated["nested"]["unevaluatedProperties"], json!(false));
        assert_eq!(
            generated["nested"]["properties"]["child"]["type"],
            json!("string")
        );
        let rc = build_rc_schema(settings).unwrap();
        let keys = rc["properties"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["direct"]);
    }

    #[test]
    fn task_schema_closes_recursive_refs_and_excludes_unused_definitions() {
        let schema = json!({ "$defs": {
            "task": { "allOf": [{ "$ref": "#/$defs/shared" }, { "$ref": "#/$defs/shared/properties/x" }] },
            "shared": { "properties": { "x": { "$ref": "#/$defs/task" } } },
            "unused": { "type": "string" },
        } });
        let generated = build_task_schema(&schema).unwrap();
        let defs = generated["$defs"].as_object().unwrap();
        assert_eq!(defs.len(), 2);
        assert!(defs.contains_key("task") && defs.contains_key("shared"));
        assert!(!defs.contains_key("unused"));
    }

    #[test]
    fn invalid_refs_and_unknown_types_fail_before_writing_schemas() {
        let schema = json!({ "$defs": {} });
        assert!(referenced_defs(&schema, &json!({ "$ref": "#/$defs/missing" })).is_err());
        assert!(referenced_defs(&schema, &json!({ "$ref": "#/properties/task" })).is_err());
        assert!(
            build_element(
                "bad",
                json!({ "type": "Alien", "description": "bad" })
                    .as_object()
                    .unwrap()
            )
            .is_err()
        );
        assert!(build_element("bad", json!({ "type": "Bool" }).as_object().unwrap()).is_err());
    }
}
