use std::collections::{BTreeMap, BTreeSet};

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
    /// Score along ordered levels; level `i` is `criteria[i]`, low to high.
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    /// Yes or no, optionally with what each answer means.
    Noul {
        instructions: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

impl Question {
    /// A `Choice` question. A repeated label is an error.
    pub fn choice<I, A, B>(
        instructions: impl Into<String>,
        criteria: I,
    ) -> Result<Self, HarnessError>
    where
        I: IntoIterator<Item = (A, B)>,
        A: Into<String>,
        B: Into<String>,
    {
        let mut labels = BTreeMap::new();
        for (label, description) in criteria {
            let label = label.into();
            if labels.insert(label.clone(), description.into()).is_some() {
                return Err(HarnessError::Workflow(format!(
                    "choice label `{label}` is repeated"
                )));
            }
        }
        Ok(Self::Choice {
            instructions: instructions.into(),
            criteria: labels,
        })
    }

    /// A `Score` question. `criteria` is ordered low to high.
    pub fn score<I, S>(instructions: impl Into<String>, criteria: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Score {
            instructions: instructions.into(),
            criteria: criteria.into_iter().map(Into::into).collect(),
        }
    }

    /// A yes-or-no question with no description of either answer.
    pub fn noul(instructions: impl Into<String>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    /// A yes-or-no question that says what each answer means.
    pub fn noul_meanings(
        instructions: impl Into<String>,
        yes: impl Into<String>,
        no: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                yes: yes.into(),
                no: no.into(),
            }),
        }
    }
}

/// What a yes and a no mean for a `Noul` question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoulCriteria {
    #[serde(rename = "true")]
    pub yes: String,
    #[serde(rename = "false")]
    pub no: String,
}

/// The state slice a `Decide` node is allowed to see, plus its questions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DecisionRequest {
    pub state: State,
    pub questions: BTreeMap<String, Question>,
}

impl DecisionRequest {
    /// Starts a request whose state may contain only `reads`.
    pub fn builder<I, S>(reads: I) -> DecisionRequestBuilder
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        DecisionRequestBuilder {
            reads: reads.into_iter().map(Into::into).collect(),
            state: State::new(),
            questions: BTreeMap::new(),
        }
    }
}

/// Builds a [`DecisionRequest`]. `set` rejects a key that is not in `reads`.
#[derive(Debug, Clone)]
pub struct DecisionRequestBuilder {
    reads: BTreeSet<String>,
    state: State,
    questions: BTreeMap<String, Question>,
}

impl DecisionRequestBuilder {
    /// Stores `value` under `key`. A key the node did not read is an error and
    /// is not stored.
    pub fn set(
        mut self,
        key: impl Into<String>,
        value: impl Serialize,
    ) -> Result<Self, HarnessError> {
        let key = key.into();
        if !self.reads.contains(&key) {
            return Err(HarnessError::Workflow(format!(
                "state key `{key}` is not read by this decision"
            )));
        }
        let value = serde_json::to_value(value)
            .map_err(|e| HarnessError::Workflow(format!("state value for `{key}`: {e}")))?;
        self.state.insert(key, value);
        Ok(self)
    }

    /// Adds a question. A repeated key is an error.
    pub fn ask(mut self, key: impl Into<String>, question: Question) -> Result<Self, HarnessError> {
        let key = key.into();
        if self.questions.contains_key(&key) {
            return Err(HarnessError::Workflow(format!(
                "question `{key}` is already set"
            )));
        }
        self.questions.insert(key, question);
        Ok(self)
    }

    pub fn build(self) -> DecisionRequest {
        DecisionRequest {
            state: self.state,
            questions: self.questions,
        }
    }
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
