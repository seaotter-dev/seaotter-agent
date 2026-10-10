use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::HarnessError;
use crate::event::Usage;
use crate::future::BoxFuture;

/// A JSON Schema document that a generative response must match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JsonSchema(pub Value);

/// What a generative call must return. Free text is an explicit opt-out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputSpec {
    Schema(JsonSchema),
    FreeText,
}

/// A single-shot generative request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenRequest {
    pub task: String,
    pub context: Value,
    pub output: OutputSpec,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenOutput {
    Json(Value),
    Text(String),
}

/// The completed output. Carries no tool calls and no next-step plan.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GenResponse {
    pub output: GenOutput,
    pub usage: Usage,
}

/// Completes exactly one request. Cannot choose tools or the next step.
pub trait GenerativeModel: Send + Sync {
    fn complete(&self, req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>>;
}
