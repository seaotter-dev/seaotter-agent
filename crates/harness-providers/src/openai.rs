//! OpenAI-compatible `/chat/completions` generative client.
//!
//! One request, one response. The request never carries tools, and a
//! response that comes back with tool calls is an error. Schema-bound output
//! is validated before it is returned.

use std::fmt;
use std::time::Duration;

use harness_core::{
    BoxFuture, GenOutput, GenRequest, GenResponse, GenerativeModel, HarnessError, JsonSchema,
    OutputSpec, Usage, validate_output,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::http::post_json;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

const SYSTEM_PROMPT: &str = "Complete the task in a single response. Return only the requested \
output. Do not ask follow-up questions, call tools, or plan further steps.";

/// How a schema-bound request asks the endpoint for JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructuredMode {
    /// `response_format: {type: "json_schema", ...}`.
    JsonSchema { strict: bool },
    /// `response_format: {type: "json_object"}` with the schema in the
    /// system message, for endpoints without `json_schema` support.
    JsonObject,
}

impl Default for StructuredMode {
    fn default() -> Self {
        Self::JsonSchema { strict: true }
    }
}

/// Which request field carries `GenRequest::max_tokens`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MaxTokensField {
    #[default]
    MaxCompletionTokens,
    MaxTokens,
}

impl MaxTokensField {
    fn key(self) -> &'static str {
        match self {
            Self::MaxCompletionTokens => "max_completion_tokens",
            Self::MaxTokens => "max_tokens",
        }
    }
}

/// Where and how to reach an OpenAI-compatible endpoint.
#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
    pub structured: StructuredMode,
    pub max_tokens_field: MaxTokensField,
}

impl OpenAiCompatibleConfig {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            timeout: Duration::from_secs(60),
            structured: StructuredMode::default(),
            max_tokens_field: MaxTokensField::default(),
        }
    }

    /// Reads `{prefix}_API_KEY` and `{prefix}_MODEL` (both required) and
    /// `{prefix}_BASE_URL` (defaults to [`DEFAULT_BASE_URL`]).
    pub fn from_env(prefix: &str) -> Result<Self, HarnessError> {
        let var = |suffix: &str| {
            std::env::var(format!("{prefix}_{suffix}"))
                .ok()
                .filter(|v| !v.is_empty())
        };
        let required = |suffix: &str| {
            var(suffix)
                .ok_or_else(|| HarnessError::Provider(format!("{prefix}_{suffix} is not set")))
        };
        let api_key = required("API_KEY")?;
        let model = required("MODEL")?;
        let base_url = var("BASE_URL").unwrap_or_else(|| DEFAULT_BASE_URL.into());
        Ok(Self::new(base_url, api_key, model))
    }
}

impl fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenAiCompatibleConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("structured", &self.structured)
            .field("max_tokens_field", &self.max_tokens_field)
            .finish()
    }
}

/// A [`GenerativeModel`] backed by an OpenAI-compatible endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiCompatible {
    config: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

impl OpenAiCompatible {
    pub fn new(config: OpenAiCompatibleConfig) -> Result<Self, HarnessError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| HarnessError::Provider(format!("openai client: {e}")))?;
        Ok(Self { config, client })
    }

    fn url(&self) -> String {
        format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

impl GenerativeModel for OpenAiCompatible {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            let body = to_wire(&req, &self.config);
            let (text, latency_ms) = post_json(
                &self.client,
                "openai",
                &self.url(),
                &self.config.api_key,
                &body,
            )
            .await?;
            let wire: Value = serde_json::from_str(&text)
                .map_err(|e| HarnessError::Provider(format!("openai response json: {e}")))?;
            from_wire(wire, &req.output, latency_ms)
        })
    }
}

/// Builds the `/chat/completions` body. Never includes `tools`,
/// `tool_choice`, or `functions`.
pub fn to_wire(req: &GenRequest, config: &OpenAiCompatibleConfig) -> Value {
    let mut system = SYSTEM_PROMPT.to_string();
    let mut response_format = None;
    if let OutputSpec::Schema(JsonSchema(schema)) = &req.output {
        match config.structured {
            StructuredMode::JsonSchema { strict } => {
                system.push_str(" Respond with JSON that matches the provided schema.");
                response_format = Some(json!({
                    "type": "json_schema",
                    "json_schema": {"name": "output", "schema": schema, "strict": strict},
                }));
            }
            StructuredMode::JsonObject => {
                system.push_str(&format!(
                    " Respond with a single JSON object that matches this JSON Schema:\n{schema}"
                ));
                response_format = Some(json!({"type": "json_object"}));
            }
        }
    }
    let user = format!("Task:\n{}\n\nContext (JSON):\n{}", req.task, req.context);

    let mut body = Map::new();
    body.insert("model".into(), json!(config.model));
    body.insert(
        "messages".into(),
        json!([
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ]),
    );
    body.insert(config.max_tokens_field.key().into(), json!(req.max_tokens));
    if let Some(format) = response_format {
        body.insert("response_format".into(), format);
    }
    Value::Object(body)
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    choices: Vec<WireChoice>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireChoice {
    message: WireMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct WireMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    refusal: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    function_call: Option<Value>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    prompt_tokens: Option<u32>,
    #[serde(default)]
    completion_tokens: Option<u32>,
}

/// Maps a `/chat/completions` response onto `spec`, validating schema-bound
/// output. Tool calls, refusals, and missing content are provider errors.
pub fn from_wire(
    body: Value,
    spec: &OutputSpec,
    latency_ms: u64,
) -> Result<GenResponse, HarnessError> {
    let provider = |msg: &str| HarnessError::Provider(format!("openai: {msg}"));
    let resp: WireResponse = serde_json::from_value(body)
        .map_err(|e| provider(&format!("unexpected response shape: {e}")))?;
    let choice = resp
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| provider("response has no choices"))?;
    let message = choice.message;
    if message.tool_calls.is_some_and(|calls| !calls.is_empty()) || message.function_call.is_some()
    {
        return Err(provider("tool calls are not allowed"));
    }
    if let Some(refusal) = message.refusal.filter(|r| !r.is_empty()) {
        return Err(provider(&format!("model refused: {refusal}")));
    }
    let content = message
        .content
        .ok_or_else(|| provider("response has no content"))?;

    let output = match spec {
        OutputSpec::Schema(_) => {
            if choice.finish_reason.as_deref() == Some("length") {
                return Err(provider("output was cut off at max_tokens"));
            }
            let value: Value = serde_json::from_str(&content).map_err(|e| {
                HarnessError::SchemaInvalid(format!("expected JSON output, got: {e}"))
            })?;
            GenOutput::Json(value)
        }
        OutputSpec::FreeText => GenOutput::Text(content),
    };
    validate_output(spec, &output)?;

    let usage = resp.usage;
    Ok(GenResponse {
        output,
        usage: Usage {
            latency_ms,
            input_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
            output_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
            cost_usd: None,
        },
    })
}
