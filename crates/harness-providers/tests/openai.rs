mod common;

use common::serve_once;
use harness_core::{GenOutput, GenRequest, GenerativeModel, HarnessError, JsonSchema, OutputSpec};
use harness_providers::openai::{from_wire, to_wire};
use harness_providers::{MaxTokensField, OpenAiCompatible, OpenAiCompatibleConfig, StructuredMode};
use serde_json::{Value, json};

const VALID: &str = include_str!("fixtures/chat_schema_valid.json");
const INVALID: &str = include_str!("fixtures/chat_schema_invalid.json");
const FREE_TEXT: &str = include_str!("fixtures/chat_free_text.json");
const TOOL_CALLS: &str = include_str!("fixtures/chat_tool_calls.json");

fn schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "properties": {
            "summary": {"type": "string"},
            "priority": {"type": "string", "enum": ["low", "normal", "high"]}
        },
        "required": ["summary", "priority"],
        "additionalProperties": false
    })))
}

fn request(output: OutputSpec) -> GenRequest {
    GenRequest {
        task: "Summarize the ticket and set a priority.".into(),
        context: json!({"ticket": "My blender arrived broken. I want to return it."}),
        output,
        max_tokens: 200,
    }
}

fn config() -> OpenAiCompatibleConfig {
    OpenAiCompatibleConfig::new("https://api.x.ai/v1", "test-key", "grok-4")
}

fn parse(fixture: &str) -> Value {
    serde_json::from_str(fixture).unwrap()
}

/// A fixture with one field of `choices[0]` changed.
fn with_choice(fixture: &str, edit: impl FnOnce(&mut Value)) -> Value {
    let mut body = parse(fixture);
    edit(&mut body["choices"][0]);
    body
}

fn assert_err<T: std::fmt::Debug>(
    result: Result<T, HarnessError>,
    expected: fn(&HarnessError) -> bool,
) {
    match result {
        Err(ref e) if expected(e) => {}
        other => panic!("unexpected result: {other:?}"),
    }
}

fn is_provider(e: &HarnessError) -> bool {
    matches!(e, HarnessError::Provider(_))
}

fn is_schema_invalid(e: &HarnessError) -> bool {
    matches!(e, HarnessError::SchemaInvalid(_))
}

#[test]
fn schema_valid_body_passes() {
    let resp = from_wire(parse(VALID), &schema(), 17).unwrap();
    assert_eq!(
        resp.output,
        GenOutput::Json(json!({
            "summary": "Customer wants to return a damaged blender.",
            "priority": "high"
        }))
    );
    assert_eq!(resp.usage.latency_ms, 17);
    assert_eq!(resp.usage.input_tokens, Some(112));
    assert_eq!(resp.usage.output_tokens, Some(21));
    assert_eq!(resp.usage.cost_usd, None);
}

#[test]
fn schema_invalid_body_fails() {
    assert_err(from_wire(parse(INVALID), &schema(), 0), is_schema_invalid);
}

#[test]
fn non_json_content_fails_a_schema_bound_request() {
    assert_err(from_wire(parse(FREE_TEXT), &schema(), 0), is_schema_invalid);
}

#[test]
fn free_text_opt_out_returns_text() {
    let resp = from_wire(parse(FREE_TEXT), &OutputSpec::FreeText, 0).unwrap();
    assert_eq!(
        resp.output,
        GenOutput::Text("Thanks for reaching out. We'll send a prepaid return label today.".into())
    );
    assert_eq!(resp.usage.output_tokens, Some(16));
}

#[test]
fn tool_calls_are_rejected() {
    assert_err(from_wire(parse(TOOL_CALLS), &schema(), 0), is_provider);
    assert_err(
        from_wire(parse(TOOL_CALLS), &OutputSpec::FreeText, 0),
        is_provider,
    );
    let legacy = with_choice(FREE_TEXT, |c| {
        c["message"]["function_call"] = json!({"name": "issue_refund", "arguments": "{}"});
    });
    assert_err(from_wire(legacy, &OutputSpec::FreeText, 0), is_provider);
}

#[test]
fn refusal_is_a_provider_error() {
    let body = with_choice(VALID, |c| {
        c["message"]["content"] = Value::Null;
        c["message"]["refusal"] = json!("I can't help with that.");
    });
    assert_err(from_wire(body, &schema(), 0), is_provider);
}

#[test]
fn truncated_schema_output_is_a_provider_error() {
    let body = with_choice(VALID, |c| c["finish_reason"] = json!("length"));
    assert_err(from_wire(body, &schema(), 0), is_provider);
}

#[test]
fn missing_content_or_choices_is_a_provider_error() {
    let no_content = with_choice(FREE_TEXT, |c| c["message"]["content"] = Value::Null);
    assert_err(from_wire(no_content, &OutputSpec::FreeText, 0), is_provider);
    let mut no_choices = parse(FREE_TEXT);
    no_choices["choices"] = json!([]);
    assert_err(from_wire(no_choices, &OutputSpec::FreeText, 0), is_provider);
}

fn assert_no_tools(body: &Value) {
    for key in ["tools", "tool_choice", "functions", "function_call"] {
        assert!(body.get(key).is_none(), "request carries `{key}`: {body}");
    }
}

#[test]
fn to_wire_json_schema_mode() {
    let body = to_wire(&request(schema()), &config());
    assert_no_tools(&body);
    assert_eq!(body["model"], "grok-4");
    assert_eq!(body["max_completion_tokens"], 200);
    assert!(body.get("max_tokens").is_none());
    let OutputSpec::Schema(JsonSchema(expected)) = schema() else {
        unreachable!()
    };
    assert_eq!(
        body["response_format"],
        json!({
            "type": "json_schema",
            "json_schema": {"name": "output", "schema": expected, "strict": true}
        })
    );
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "system");
    assert_eq!(messages[1]["role"], "user");
    let user = messages[1]["content"].as_str().unwrap();
    assert!(user.contains("Summarize the ticket and set a priority."));
    assert!(user.contains("My blender arrived broken."));
}

#[test]
fn to_wire_json_object_mode_embeds_the_schema() {
    let mut config = config();
    config.structured = StructuredMode::JsonObject;
    let body = to_wire(&request(schema()), &config);
    assert_no_tools(&body);
    assert_eq!(body["response_format"], json!({"type": "json_object"}));
    let system = body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("JSON"), "{system}");
    assert!(
        system.contains("\"required\":[\"summary\",\"priority\"]"),
        "{system}"
    );
}

#[test]
fn to_wire_free_text_has_no_response_format() {
    let body = to_wire(&request(OutputSpec::FreeText), &config());
    assert_no_tools(&body);
    assert!(body.get("response_format").is_none());
}

#[test]
fn to_wire_can_send_max_tokens() {
    let mut config = config();
    config.max_tokens_field = MaxTokensField::MaxTokens;
    let body = to_wire(&request(schema()), &config);
    assert_eq!(body["max_tokens"], 200);
    assert!(body.get("max_completion_tokens").is_none());
}

#[tokio::test]
async fn complete_posts_one_chat_completion() {
    let (base_url, server) = serve_once("200 OK", VALID).await;
    let mut config = config();
    config.base_url = format!("{base_url}/v1/");
    let model = OpenAiCompatible::new(config.clone()).unwrap();
    let resp = model.complete(request(schema())).await.unwrap();
    let captured = server.await.unwrap();

    assert_eq!(
        captured.request_line(),
        "POST /v1/chat/completions HTTP/1.1"
    );
    assert!(
        captured.has_header("authorization: Bearer test-key"),
        "{}",
        captured.head
    );
    assert_eq!(captured.body, to_wire(&request(schema()), &config));
    assert!(matches!(resp.output, GenOutput::Json(_)));
    assert_eq!(resp.usage.input_tokens, Some(112));
    assert_eq!(resp.usage.output_tokens, Some(21));
}

#[tokio::test]
async fn complete_rejects_a_schema_invalid_body() {
    let (base_url, server) = serve_once("200 OK", INVALID).await;
    let mut config = config();
    config.base_url = base_url;
    let result = OpenAiCompatible::new(config)
        .unwrap()
        .complete(request(schema()))
        .await;
    server.await.unwrap();
    assert_err(result, is_schema_invalid);
}

/// Calls a real endpoint. Set `OPENAI_COMPAT_API_KEY`, `OPENAI_COMPAT_MODEL`,
/// and optionally `OPENAI_COMPAT_BASE_URL`, then run
/// `cargo test -p harness-providers -- --ignored`.
#[tokio::test]
#[ignore = "live network call; needs OPENAI_COMPAT_API_KEY and OPENAI_COMPAT_MODEL"]
async fn live_schema_bound_call() {
    let Ok(config) = OpenAiCompatibleConfig::from_env("OPENAI_COMPAT") else {
        eprintln!("OPENAI_COMPAT_* not set; skipping");
        return;
    };
    let resp = OpenAiCompatible::new(config)
        .unwrap()
        .complete(request(schema()))
        .await
        .unwrap();
    harness_core::validate_output(&schema(), &resp.output).unwrap();
}
