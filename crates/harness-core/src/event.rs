use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::node::NodeId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    Decide,
    Generate,
    Effect,
}

/// Latency and cost reported for one call. Unknown values are `None`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub latency_ms: u64,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
    pub cost_usd: Option<f64>,
}

/// One append-only record of a decide, generate, or effect step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub node: NodeId,
    pub kind: EventKind,
    pub input_hash: String,
    pub output: Value,
    pub confidence: Option<f64>,
    pub usage: Usage,
}
