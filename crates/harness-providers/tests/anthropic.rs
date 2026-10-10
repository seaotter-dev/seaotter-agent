mod common;

use common::serve_once;
use harness_core::{GenOutput, GenRequest, GenerativeModel, HarnessError, JsonSchema, OutputSpec};
use harness_providers::anthropic::{from_wire, to_wire, transform_schema};
use harness_providers::{Anthropic, AnthropicConfig, AnthropicOutputMode};
use serde_json::{Value, json};

const VALID: &str = include_str!("fixtures/messages_schema_valid.json");
const INVALID: &str = include_str!("fixtures/messages_schema_invalid.json");
const FREE_TEXT: &str = include_str!("fixtures/messages_free_text.json");
const TOOL_USE: &str = include_str!("fixtures/messages_tool_use.json");

fn schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string", "maxLength": 80},
            "priority": {"type": "string", "enum": ["low", "normal", "high"]}
        },
        "required": ["summary", "priority"]
    })
}

fn schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(schema_value()))
}

fn request(output: OutputSpec) -> GenRequest {
    GenRequest {
        task: "Summarize the ticket and set a priority.".into(),
        context: json!({"ticket": "My blender arrived broken. I want to return it."}),
        output,
        max_tokens: 200,
    }
}

fn config() -> AnthropicConfig {
    AnthropicConfig::new("test-key", "claude-sonnet-5-5")
}

fn parse(fixture: &str) -> Value {
    serde_json::from_str(fixture).unwrap()
}

fn assert_provider<T: std::fmt::Debug>(result: Result<T, HarnessError>) {
    assert!(
        matches!(result, Err(HarnessError::Provider(_))),
        "{result:?}"
    );
}

fn assert_schema_invalid<T: std::fmt::Debug>(result: Result<T, HarnessError>) {
    assert!(
        matches!(result, Err(HarnessError::SchemaInvalid(_))),
        "{result:?}"
    );
}

#[test]
fn schema_valid_response_passes() {
    let resp = from_wire(parse(VALID), &schema(), 9).unwrap();
    assert_eq!(
        resp.output,
        GenOutput::Json(json!({
            "summary": "Customer wants to return a damaged blender.",
            "priority": "high"
        }))
    );
    assert_eq!(resp.usage.latency_ms, 9);
    assert_eq!(resp.usage.input_tokens, Some(140));
    assert_eq!(resp.usage.output_tokens, Some(24));
    assert_eq!(resp.usage.cost_usd, None);
}

#[test]
fn schema_invalid_response_fails() {
    assert_schema_invalid(from_wire(parse(INVALID), &schema(), 0));
}

#[test]
fn free_text_is_accepted_only_under_the_opt_out() {
    let resp = from_wire(parse(FREE_TEXT), &OutputSpec::FreeText, 0).unwrap();
    assert_eq!(
        resp.output,
        GenOutput::Text("Thanks for reaching out. We'll send a prepaid return label today.".into())
    );
    assert_schema_invalid(from_wire(parse(FREE_TEXT), &schema(), 0));
}

#[test]
fn tool_use_is_rejected() {
    assert_provider(from_wire(parse(TOOL_USE), &schema(), 0));
    assert_provider(from_wire(parse(TOOL_USE), &OutputSpec::FreeText, 0));
    let mut stop_only = parse(FREE_TEXT);
    stop_only["stop_reason"] = json!("tool_use");
    assert_provider(from_wire(stop_only, &OutputSpec::FreeText, 0));
}

#[test]
fn refusal_and_truncation_are_provider_errors() {
    let mut refusal = parse(VALID);
    refusal["stop_reason"] = json!("refusal");
    assert_provider(from_wire(refusal, &schema(), 0));

    let mut truncated = parse(VALID);
    truncated["stop_reason"] = json!("max_tokens");
    assert_provider(from_wire(truncated.clone(), &schema(), 0));
    truncated["content"][0]["text"] = json!("A partial reply");
    assert!(from_wire(truncated, &OutputSpec::FreeText, 0).is_ok());
}

#[test]
fn missing_text_is_a_provider_error() {
    let mut body = parse(VALID);
    body["content"] = json!([{"type": "thinking", "thinking": "...", "signature": "x"}]);
    assert_provider(from_wire(body, &schema(), 0));
}

#[test]
fn text_blocks_are_joined_and_thinking_is_ignored() {
    let mut body = parse(VALID);
    body["content"] = json!([
        {"type": "thinking", "thinking": "Considering priority.", "signature": "x"},
        {"type": "text", "text": "{\"summary\":\"Broken blender.\","},
        {"type": "text", "text": "\"priority\":\"high\"}"}
    ]);
    let resp = from_wire(body, &schema(), 0).unwrap();
    assert_eq!(
        resp.output,
        GenOutput::Json(json!({"summary": "Broken blender.", "priority": "high"}))
    );
}

#[test]
fn transform_moves_constraints_into_description() {
    let out = transform_schema(&json!({
        "type": "object",
        "properties": {
            "count": {"type": "integer", "minimum": 1, "maximum": 10, "description": "How many"},
            "tags": {"type": "array", "items": {"type": "string"}, "minItems": 2, "maxItems": 5},
            "maybe": {"type": "array", "items": {"type": "string"}, "minItems": 1}
        }
    }));
    let count = &out["properties"]["count"];
    assert!(count.get("minimum").is_none() && count.get("maximum").is_none());
    let description = count["description"].as_str().unwrap();
    assert!(description.starts_with("How many "), "{description}");
    assert!(description.contains("\"minimum\":1"), "{description}");
    assert!(description.contains("\"maximum\":10"), "{description}");

    let tags = &out["properties"]["tags"];
    assert!(tags.get("minItems").is_none() && tags.get("maxItems").is_none());
    assert!(
        tags["description"]
            .as_str()
            .unwrap()
            .contains("\"minItems\":2")
    );

    assert_eq!(out["properties"]["maybe"]["minItems"], 1);
    assert!(out["properties"]["maybe"].get("description").is_none());
}

#[test]
fn transform_closes_every_object() {
    let out = transform_schema(&json!({
        "type": "object",
        "additionalProperties": {"type": "string"},
        "properties": {
            "customer": {"type": "object", "properties": {"name": {"type": "string"}}},
            "orders": {"type": "array", "items": {"type": "object", "properties": {"id": {"type": "string"}}}},
            "note": {"anyOf": [{"type": "null"}, {"properties": {"text": {"type": "string"}}}]}
        },
        "$defs": {"addr": {"type": ["object", "null"], "properties": {"zip": {"type": "string"}}}}
    }));
    assert_eq!(out["additionalProperties"], false);
    assert_eq!(out["properties"]["customer"]["additionalProperties"], false);
    assert_eq!(
        out["properties"]["orders"]["items"]["additionalProperties"],
        false
    );
    assert_eq!(
        out["properties"]["note"]["anyOf"][1]["additionalProperties"],
        false
    );
    assert!(
        out["properties"]["note"]["anyOf"][0]
            .get("additionalProperties")
            .is_none()
    );
    assert_eq!(out["$defs"]["addr"]["additionalProperties"], false);
    assert!(
        out["properties"]["customer"]["properties"]["name"]
            .get("additionalProperties")
            .is_none()
    );
}

#[test]
fn transform_keeps_supported_formats_only() {
    let out = transform_schema(&json!({
        "type": "object",
        "properties": {
            "when": {"type": "string", "format": "date-time"},
            "site": {"type": "string", "format": "uri-reference"}
        }
    }));
    assert_eq!(out["properties"]["when"]["format"], "date-time");
    assert!(out["properties"]["site"].get("format").is_none());
    assert!(
        out["properties"]["site"]["description"]
            .as_str()
            .unwrap()
            .contains("uri-reference")
    );
}

#[test]
fn original_constraints_are_still_enforced() {
    let long = "x".repeat(81);
    let mut body = parse(VALID);
    body["content"][0]["text"] = json!(json!({"summary": long, "priority": "high"}).to_string());
    assert_schema_invalid(from_wire(body, &schema(), 0));
}

fn assert_no_tools(body: &Value) {
    for key in ["tools", "tool_choice"] {
        assert!(body.get(key).is_none(), "request carries `{key}`: {body}");
    }
}

#[test]
fn to_wire_output_config_mode() {
    let body = to_wire(&request(schema()), &config());
    assert_no_tools(&body);
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert_eq!(body["max_tokens"], 200);
    assert_eq!(
        body["output_config"],
        json!({"format": {"type": "json_schema", "schema": transform_schema(&schema_value())}})
    );
    let sent = &body["output_config"]["format"]["schema"];
    assert_eq!(sent["additionalProperties"], false);
    assert!(sent["properties"]["summary"].get("maxLength").is_none());
    assert!(body["system"].as_str().unwrap().contains("single response"));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0]["role"], "user");
    let user = messages[0]["content"].as_str().unwrap();
    assert!(user.contains("Summarize the ticket and set a priority."));
    assert!(user.contains("My blender arrived broken."));
}

#[test]
fn to_wire_prompt_mode_embeds_the_original_schema() {
    let mut config = config();
    config.mode = AnthropicOutputMode::Prompt;
    let body = to_wire(&request(schema()), &config);
    assert_no_tools(&body);
    assert!(body.get("output_config").is_none());
    let system = body["system"].as_str().unwrap();
    assert!(system.contains("JSON Schema"), "{system}");
    assert!(system.contains("\"maxLength\":80"), "{system}");
}

#[test]
fn to_wire_free_text_has_no_output_config() {
    let body = to_wire(&request(OutputSpec::FreeText), &config());
    assert_no_tools(&body);
    assert!(body.get("output_config").is_none());
}

#[tokio::test]
async fn complete_posts_one_message() {
    let (base_url, server) = serve_once("200 OK", VALID).await;
    let mut config = config();
    config.base_url = format!("{base_url}/");
    let resp = Anthropic::new(config.clone())
        .unwrap()
        .complete(request(schema()))
        .await
        .unwrap();
    let captured = server.await.unwrap();

    assert_eq!(captured.request_line(), "POST /v1/messages HTTP/1.1");
    assert!(
        captured.has_header("x-api-key: test-key"),
        "{}",
        captured.head
    );
    assert!(
        captured.has_header("anthropic-version: 2023-06-01"),
        "{}",
        captured.head
    );
    assert!(
        !captured
            .head
            .to_ascii_lowercase()
            .contains("authorization:"),
        "{}",
        captured.head
    );
    assert_eq!(captured.body, to_wire(&request(schema()), &config));
    assert_eq!(resp.usage.input_tokens, Some(140));
    assert_eq!(resp.usage.output_tokens, Some(24));
}

/// Calls the real API. Set `ANTHROPIC_API_KEY` and `ANTHROPIC_MODEL`, then
/// run `cargo test -p harness-providers -- --ignored`.
#[tokio::test]
#[ignore = "live network call; needs ANTHROPIC_API_KEY and ANTHROPIC_MODEL"]
async fn live_schema_bound_call() {
    let Ok(config) = AnthropicConfig::from_env("ANTHROPIC") else {
        eprintln!("ANTHROPIC_* not set; skipping");
        return;
    };
    let resp = Anthropic::new(config)
        .unwrap()
        .complete(request(schema()))
        .await
        .unwrap();
    harness_core::validate_output(&schema(), &resp.output).unwrap();
}
