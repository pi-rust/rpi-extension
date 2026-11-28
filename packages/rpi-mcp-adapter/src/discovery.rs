use crate::Tool;
use serde_json::{json, Value};
use std::collections::HashSet;

const PAGE_SIZE: usize = 30;
const MAX_SELECTED: usize = 20;
const SIGNATURE_BYTES: usize = 360;

pub(crate) struct Discovery<'a> {
    selected: Option<&'a [Value]>,
    pub(crate) offset: usize,
    pub(crate) probe: bool,
    full: bool,
}

impl<'a> Discovery<'a> {
    pub(crate) fn new(params: &'a Value) -> Result<Self, String> {
        let selected = params
            .get("tools")
            .map(|value| {
                value
                    .as_array()
                    .filter(|tools| {
                        (1..=MAX_SELECTED).contains(&tools.len())
                            && tools.iter().all(|tool| {
                                tool.as_str().is_some_and(|name| {
                                    !name.trim().is_empty() && name.len() <= 200
                                })
                            })
                    })
                    .ok_or("tools must contain 1-20 exact tool names")
            })
            .transpose()?
            .map(Vec::as_slice);
        let full = match params.get("detail").and_then(Value::as_str) {
            Some("full") => true,
            None if params.get("detail").is_none() => false,
            _ => return Err("detail must be full when supplied".into()),
        };
        let offset = params
            .get("offset")
            .map(|value| {
                value
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .filter(|n| *n <= 16_384)
                    .ok_or("offset must be an integer from 0 to 16384")
            })
            .transpose()?
            .unwrap_or(0);
        let probe = match params.get("probe") {
            None => false,
            Some(value) => value.as_bool().ok_or("probe must be a boolean")?,
        };
        let has_target = params.get("server").is_some() || params.get("url").is_some();
        if probe && (has_target || selected.is_some() || full || params["action"] == "close") {
            return Err("probe is only available when listing configured servers".into());
        }
        if selected.is_some() && params.get("offset").is_some() {
            return Err("offset cannot be combined with tools".into());
        }
        if (selected.is_some() || full || (params.get("offset").is_some() && !probe))
            && (params["action"] == "close" || !has_target)
        {
            return Err(
                "tools, detail and offset require tool discovery with server or url".into(),
            );
        }
        Ok(Self {
            selected,
            offset,
            full,
            probe,
        })
    }

    pub(crate) fn project(self, mut result: Value) -> Result<Value, String> {
        let Some(tools) = result.get_mut("tools").and_then(Value::as_array_mut) else {
            return Ok(result);
        };
        let mut catalog = std::mem::take(tools);
        let total = catalog.len();
        let page = if let Some(names) = self.selected {
            let mut selected = Vec::with_capacity(names.len());
            for name in names {
                if selected.iter().any(|tool: &Value| tool["name"] == *name) {
                    continue;
                }
                let position = catalog
                    .iter()
                    .position(|tool| tool["name"] == *name)
                    .ok_or_else(|| format!("MCP server does not expose tool {name}"))?;
                selected.push(catalog.swap_remove(position));
            }
            selected
        } else {
            catalog
                .into_iter()
                .skip(self.offset)
                .take(PAGE_SIZE)
                .collect()
        };
        result["tools"] = Value::Array(if self.full {
            page
        } else {
            page.iter()
                .map(|tool| -> Result<Value, String> {
                    let name = tool["name"]
                        .as_str()
                        .filter(|name| !name.trim().is_empty() && name.len() <= 200)
                        .ok_or(
                            "MCP server returned a tool name outside the 1–200 UTF-8 byte limit",
                        )?;
                    let description = tool["description"].as_str().unwrap_or_default();
                    let schema = &tool["inputSchema"];
                    if self.selected.is_some() {
                        Ok(signature(name, description, schema))
                    } else {
                        Ok(summary(name, description, schema))
                    }
                })
                .collect::<Result<Vec<_>, String>>()?
        });
        if !self.full {
            if let Some(info) = result["serverInfo"].as_object_mut() {
                for value in info.values_mut() {
                    if let Some(text) = value.as_str() {
                        *value = json!(short_description(text));
                    }
                }
            }
        }
        result["toolCount"] = json!(total);
        if self.selected.is_none() && self.offset + PAGE_SIZE < total {
            result["nextOffset"] = json!(self.offset + PAGE_SIZE);
        }
        Ok(result)
    }
}

fn short_description(description: &str) -> &str {
    let line = description.lines().next().unwrap_or_default().trim();
    let end = line
        .char_indices()
        .map(|(i, _)| i)
        .chain(std::iter::once(line.len()))
        .take_while(|&i| i <= 80)
        .last()
        .unwrap_or(0);
    line[..end].trim_end()
}

fn required(schema: &Value) -> impl Iterator<Item = &str> + Clone {
    schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

fn summary(name: &str, description: &str, schema: &Value) -> Value {
    let mut result = json!({"name":name});
    let description = short_description(description);
    if !description.is_empty() {
        result["description"] = json!(description);
    }
    let mut names = Vec::new();
    let mut bytes = 0;
    for field in required(schema) {
        bytes += json!(field).to_string().len() + 1;
        if bytes > 80 {
            result["requiredTruncated"] = json!(true);
            break;
        }
        names.push(field);
    }
    if !names.is_empty() {
        result["required"] = json!(names);
    }
    result
}

fn signature(name: &str, description: &str, schema: &Value) -> Value {
    let mut result = summary(name, description, schema);
    let mut input = json!({"type":"object"});
    if let Some(names) = result
        .as_object_mut()
        .and_then(|fields| fields.remove("required"))
    {
        input["required"] = names;
    }
    result["inputSchema"] = input;
    let Some(properties) = schema["properties"].as_object() else {
        return result;
    };
    let required: HashSet<_> = required(schema).collect();
    let fields = properties
        .iter()
        .filter(|(key, _)| required.contains(key.as_str()))
        .chain(
            properties
                .iter()
                .filter(|(key, _)| !required.contains(key.as_str())),
        );
    for (key, property) in fields {
        let mut projected = json!({});
        for field in ["type", "enum"] {
            if let Some(value) = property.get(field) {
                projected[field] = value.clone();
            }
        }
        result["inputSchema"]["properties"][key] = projected;
        if result.to_string().len() > SIGNATURE_BYTES - 24 {
            let output = result["inputSchema"]["properties"].as_object_mut().unwrap();
            output.remove(key);
            result["schemaTruncated"] = json!(true);
        }
    }
    result
}

pub(crate) fn validate_arguments(tool: &Tool, arguments: &Value) -> Result<(), String> {
    let schema = &tool.input_schema;
    let fail = |field: &str, reason: &str| {
        Err(json!({
            "code":"invalid_tool_arguments", "field":field, "reason":reason,
            "signature":signature(&tool.name, "", schema),
        })
        .to_string())
    };
    for field in required(schema) {
        if arguments.get(field).is_none() {
            return fail(field, "required argument is missing");
        }
    }
    for (field, value) in arguments.as_object().into_iter().flatten() {
        let Some(property) = schema["properties"].get(field) else {
            if schema["additionalProperties"] == false
                && schema.get("patternProperties").is_none()
                && !["allOf", "anyOf", "oneOf", "$ref"]
                    .iter()
                    .any(|key| schema.get(key).is_some())
            {
                return fail(field, "argument is not in the tool schema");
            }
            continue;
        };
        let valid_type = |kind: &str| match kind {
            "null" => value.is_null(),
            "boolean" => value.is_boolean(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "number" => value.is_number(),
            "integer" => {
                value.is_i64() || value.is_u64() || value.as_f64().is_some_and(|n| n.fract() == 0.0)
            }
            _ => true,
        };
        let valid = match &property["type"] {
            Value::String(kind) => valid_type(kind),
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).any(valid_type),
            _ => true,
        };
        if !valid {
            return fail(field, "argument type does not match");
        }
        if property["enum"]
            .as_array()
            .is_some_and(|choices| !choices.iter().any(|choice| enum_value_eq(choice, value)))
        {
            return fail(field, "argument is not an allowed enum value");
        }
    }
    Ok(())
}

// JSON Schema compares numbers by value, even when their JSON forms differ.
fn enum_value_eq(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => {
            let integer = |number: &serde_json::Number| {
                number
                    .as_i64()
                    .map(i128::from)
                    .or_else(|| number.as_u64().map(i128::from))
            };
            let (whole, decimal) = match (integer(left), integer(right)) {
                (Some(whole), None) => (whole, right),
                (None, Some(whole)) => (whole, left),
                _ => return false,
            };
            decimal
                .as_f64()
                .is_some_and(|value| value.fract() == 0.0 && value as i128 == whole)
        }
        (Value::Array(left), Value::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| enum_value_eq(a, b))
        }
        (Value::Object(left), Value::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, value)| {
                    right
                        .get(key)
                        .is_some_and(|other| enum_value_eq(value, other))
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argument_enum_uses_numeric_values_instead_of_json_number_representations() {
        let tool = Tool {
            name: "lookup".into(),
            description: String::new(),
            input_schema: json!({"properties":{"arg":{"enum":[{"items":[1, 2]}]}}}),
        };
        assert!(validate_arguments(&tool, &json!({"arg":{"items":[1.0, 2.0]}})).is_ok());
        assert!(validate_arguments(&tool, &json!({"arg":{"items":[1.0, 2.5]}})).is_err());
        assert!(!enum_value_eq(
            &json!(u64::MAX),
            &json!(18446744073709551616.0)
        ));
    }
}
