use serde::{Deserialize, Serialize};

/// The control-flow edge a `Decide` node takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfidenceAction {
    Escalate,
    RetryStronger,
    SafeDefault,
    Human,
    Continue,
}

/// Below `min_confidence`, the machine takes `on_low` instead of continuing.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConfidencePolicy {
    pub min_confidence: f64,
    pub on_low: ConfidenceAction,
}
