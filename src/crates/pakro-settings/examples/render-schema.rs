//! Regenerate repository configuration schemas without Node.js.

use eyre::{Context, Result, bail, eyre};
use serde_json::{Map, Value, json};
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
        serde_json::from_str(&fs::read_to_string(root.join("src/schema/pakro.json"))?)?;
    let settings_schema = schema
        .pointer_mut("/$defs/settings")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| eyre!("src/schema/pakro.json is missing $defs.settings"))?;
    settings_schema.insert(
        "properties".into(),
        Value::Object(build_settings(settings, "")?),
    );

    let rc_schema = build_rc_schema(settings)?;
    for (path, value) in [
        ("src/schema/pakro.json", schema),
        ("src/schema/pakrorc.json", rc_schema),
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
        "title": "pakro rc config",
        "description": "Early initialization settings for pakro. These settings are loaded before the main config files.",
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
    fn unknown_types_and_missing_descriptions_fail_before_writing_schemas() {
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
