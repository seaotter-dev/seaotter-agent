use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::HarnessError;
use crate::event::Usage;
use crate::future::BoxFuture;
use crate::state::State;

/// A typed question for a System One decision model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Question {
    /// Pick one of the labels in `criteria` (label to description).
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    /// Score against the labels in `criteria` (label to description).
    Score {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    Noul {
        instructions: String,
    },
}

/// The state slice a `Decide` node is allowed to see, plus its questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub state: State,
    pub questions: BTreeMap<String, Question>,
}

/// Probabilities over answer labels and the model's confidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Answer {
    pub probabilities: BTreeMap<String, f64>,
    pub confidence: f64,
}

impl Answer {
    /// The highest-probability label, if any.
    pub fn top(&self) -> Option<&str> {
        self.probabilities
            .iter()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(label, _)| label.as_str())
    }
}

/// One answer per question key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionResponse {
    pub answers: BTreeMap<String, Answer>,
    pub usage: Usage,
}

/// An atomic, typed decision call. Returns answers, never a next step.
pub trait DecisionModel: Send + Sync {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>>;
}
