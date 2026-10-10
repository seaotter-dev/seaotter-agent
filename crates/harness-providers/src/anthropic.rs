//! Anthropic Messages API (`/v1/messages`) generative client.
//!
//! One request, one response. The request never carries tools, and a
//! response with tool use is an error. Schema-bound output is validated
//! against the original schema before it is returned.

use std::fmt;
use std::time::Duration;

use harness_core::{
    BoxFuture, GenOutput, GenRequest, GenResponse, GenerativeModel, HarnessError, JsonSchema,
    OutputSpec, Usage, validate_output,
};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::http::send_json;
use crate::openai::SYSTEM_PROMPT;

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const DEFAULT_VERSION: &str = "2023-06-01";

/// Constraints `output_config.format` rejects. They move into `description`.
const UNSUPPORTED_KEYWORDS: &[&str] = &[
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "minLength",
    "maxLength",
    "maxItems",
    "uniqueItems",
    "minProperties",
    "maxProperties",
];

const SUPPORTED_FORMATS: &[&str] = &[
    "date-time",
    "time",
    "date",
    "duration",
    "email",
    "hostname",
    "uri",
    "ipv4",
    "ipv6",
    "uuid",
];

/// Rewrites a schema into the subset `output_config.format` accepts, the way
/// the Anthropic SDKs do: unsupported constraints and formats move into
/// `description`, and every object gets `additionalProperties: false`.
pub fn transform_schema(schema: &Value) -> Value {
    let mut out = schema.clone();
    transform_node(&mut out);
    out
}

fn transform_node(node: &mut Value) {
    let Value::Object(map) = node else {
        return;
    };

    let mut moved = Map::new();
    for key in UNSUPPORTED_KEYWORDS {
        if let Some(value) = map.remove(*key) {
            moved.insert((*key).into(), value);
        }
    }
    if map
        .get("minItems")
        .and_then(Value::as_u64)
        .is_some_and(|n| n > 1)
        && let Some(value) = map.remove("minItems")
    {
        moved.insert("minItems".into(), value);
    }
    let unsupported_format = map
        .get("format")
        .and_then(Value::as_str)
        .is_some_and(|f| !SUPPORTED_FORMATS.contains(&f));
    if unsupported_format && let Some(value) = map.remove("format") {
        moved.insert("format".into(), value);
    }
    if !moved.is_empty() {
        let note = Value::Object(moved).to_string();
        let description = match map.get("description").and_then(Value::as_str) {
            Some(d) if !d.is_empty() => format!("{d} {note}"),
            _ => note,
        };
        map.insert("description".into(), json!(description));
    }

    let is_object = map.contains_key("properties")
        || match map.get("type") {
            Some(Value::String(t)) => t == "object",
            Some(Value::Array(types)) => types.iter().any(|t| t == "object"),
            _ => false,
        };
    if is_object {
        map.insert("additionalProperties".into(), json!(false));
    }

    for key in ["properties", "$defs", "definitions"] {
        if let Some(Value::Object(children)) = map.get_mut(key) {
            children.values_mut().for_each(transform_node);
        }
    }
    for key in ["items", "prefixItems", "anyOf", "allOf", "oneOf"] {
        match map.get_mut(key) {
            Some(Value::Array(children)) => children.iter_mut().for_each(transform_node),
            Some(child) => transform_node(child),
            None => {}
        }
    }
}

/// How a schema-bound request asks Claude for JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AnthropicOutputMode {
    /// `output_config.format` with the transformed schema.
    #[default]
    OutputConfig,
    /// The schema in the system prompt, for endpoints or models without
    /// structured outputs.
    Prompt,
}

/// Where and how to reach an Anthropic-compatible endpoint.
#[derive(Clone)]
pub struct AnthropicConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub version: String,
    pub timeout: Duration,
    pub mode: AnthropicOutputMode,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            api_key: api_key.into(),
            model: model.into(),
            version: DEFAULT_VERSION.into(),
            timeout: Duration::from_secs(60),
            mode: AnthropicOutputMode::default(),
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
        let mut config = Self::new(required("API_KEY")?, required("MODEL")?);
        if let Some(base_url) = var("BASE_URL") {
            config.base_url = base_url;
        }
        Ok(config)
    }
}

impl fmt::Debug for AnthropicConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnthropicConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("version", &self.version)
            .field("timeout", &self.timeout)
            .field("mode", &self.mode)
            .finish()
    }
}

/// A [`GenerativeModel`] backed by the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct Anthropic {
    config: AnthropicConfig,
    client: reqwest::Client,
}

impl Anthropic {
    pub fn new(config: AnthropicConfig) -> Result<Self, HarnessError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| HarnessError::Provider(format!("anthropic client: {e}")))?;
        Ok(Self { config, client })
    }

    fn url(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }
}

impl GenerativeModel for Anthropic {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        Box::pin(async move {
            let body = to_wire(&req, &self.config);
            let http = self
                .client
                .post(self.url())
                .header("x-api-key", &self.config.api_key)
                .header("anthropic-version", &self.config.version)
                .json(&body);
            let (text, latency_ms) = send_json("anthropic", http).await?;
            let wire: Value = serde_json::from_str(&text)
                .map_err(|e| HarnessError::Provider(format!("anthropic response json: {e}")))?;
            from_wire(wire, &req.output, latency_ms)
        })
    }
}

/// Builds the `/v1/messages` body. Never includes `tools` or `tool_choice`.
pub fn to_wire(req: &GenRequest, config: &AnthropicConfig) -> Value {
    let mut system = SYSTEM_PROMPT.to_string();
    let mut output_config = None;
    if let OutputSpec::Schema(JsonSchema(schema)) = &req.output {
        match config.mode {
            AnthropicOutputMode::OutputConfig => {
                output_config = Some(json!({
                    "format": {"type": "json_schema", "schema": transform_schema(schema)}
                }));
            }
            AnthropicOutputMode::Prompt => {
                system.push_str(&format!(
                    " Respond with a single JSON object matching this JSON Schema, and nothing \
                     else:\n{schema}"
                ));
            }
        }
    }
    let user = format!("Task:\n{}\n\nContext (JSON):\n{}", req.task, req.context);

    let mut body = Map::new();
    body.insert("model".into(), json!(config.model));
    body.insert("max_tokens".into(), json!(req.max_tokens));
    body.insert("system".into(), json!(system));
    body.insert(
        "messages".into(),
        json!([{"role": "user", "content": user}]),
    );
    if let Some(output_config) = output_config {
        body.insert("output_config".into(), output_config);
    }
    Value::Object(body)
}

#[derive(Deserialize)]
struct WireResponse {
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Option<WireUsage>,
}

#[derive(Deserialize)]
struct WireUsage {
    #[serde(default)]
    input_tokens: Option<u32>,
    #[serde(default)]
    output_tokens: Option<u32>,
}

/// Maps a `/v1/messages` response onto `spec`, validating schema-bound
/// output against the original schema. Tool use, refusals, and missing text
/// are provider errors.
pub fn from_wire(
    body: Value,
    spec: &OutputSpec,
    latency_ms: u64,
) -> Result<GenResponse, HarnessError> {
    let provider = |msg: &str| HarnessError::Provider(format!("anthropic: {msg}"));
    let resp: WireResponse = serde_json::from_value(body)
        .map_err(|e| provider(&format!("unexpected response shape: {e}")))?;

    let block_type = |block: &Value| block.get("type").and_then(Value::as_str).map(str::to_owned);
    let tool_block = resp.content.iter().any(|b| {
        matches!(
            block_type(b).as_deref(),
            Some("tool_use" | "server_tool_use")
        )
    });
    let stop_reason = resp.stop_reason.as_deref();
    if tool_block || stop_reason == Some("tool_use") {
        return Err(provider("tool use is not allowed"));
    }
    if stop_reason == Some("refusal") {
        return Err(provider("model refused the request"));
    }
    if matches!(spec, OutputSpec::Schema(_)) && stop_reason == Some("max_tokens") {
        return Err(provider("output was cut off at max_tokens"));
    }

    let texts: Vec<&str> = resp
        .content
        .iter()
        .filter(|b| block_type(b).as_deref() == Some("text"))
        .filter_map(|b| b.get("text").and_then(Value::as_str))
        .collect();
    if texts.is_empty() {
        return Err(provider("response has no text"));
    }
    let text = texts.concat();

    let output = match spec {
        OutputSpec::Schema(_) => {
            let value: Value = serde_json::from_str(&text).map_err(|e| {
                HarnessError::SchemaInvalid(format!("expected JSON output, got: {e}"))
            })?;
            GenOutput::Json(value)
        }
        OutputSpec::FreeText => GenOutput::Text(text),
    };
    validate_output(spec, &output)?;

    let usage = resp.usage;
    Ok(GenResponse {
        output,
        usage: Usage {
            latency_ms,
            input_tokens: usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: usage.as_ref().and_then(|u| u.output_tokens),
            cost_usd: None,
        },
    })
}
