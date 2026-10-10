use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::HarnessError;
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

/// Append-only log of decide, generate, and effect steps.
///
/// Events can be read. Nothing in this type updates or removes one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EventLog {
    events: Vec<Event>,
}

impl EventLog {
    pub fn as_slice(&self) -> &[Event] {
        &self.events
    }

    pub fn iter(&self) -> std::slice::Iter<'_, Event> {
        self.events.iter()
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub(crate) fn push(&mut self, event: Event) {
        self.events.push(event);
    }
}

/// Hex SHA-256 of `value` serialized as JSON. Map keys are ordered, so the
/// same value always hashes the same.
pub fn input_hash(value: &impl Serialize) -> Result<String, HarnessError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| HarnessError::Workflow(format!("event input: {e}")))?;
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    Ok(out)
}
