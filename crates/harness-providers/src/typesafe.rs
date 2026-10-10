//! TypeSafe System One (`/v1/systemone`) decision client.
//!
//! Sends only the state slice and the typed questions, and maps each answer
//! to probabilities and a confidence. Compatible endpoints work by pointing
//! [`TypeSafeConfig::base_url`] at them.

use std::collections::BTreeMap;
use std::fmt;
use std::time::{Duration, Instant};

use harness_core::{
    Answer, BoxFuture, DecisionModel, DecisionRequest, DecisionResponse, HarnessError,
    NoulCriteria, Question, State, Usage,
};
use serde::{Deserialize, Serialize};

pub const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
pub const DEFAULT_MODEL: &str = "jev-latest";
const ERROR_BODY_LIMIT: usize = 500;

/// Where and how to reach a System One endpoint.
#[derive(Clone)]
pub struct TypeSafeConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    pub timeout: Duration,
}

impl TypeSafeConfig {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.into(),
            api_key: api_key.into(),
            model: DEFAULT_MODEL.into(),
            timeout: Duration::from_secs(30),
        }
    }

    /// Reads `TYPESAFE_API_KEY` (required), `TYPESAFE_BASE_URL`, and
    /// `TYPESAFE_MODEL`.
    pub fn from_env() -> Result<Self, HarnessError> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let api_key = var("TYPESAFE_API_KEY")
            .ok_or_else(|| HarnessError::Provider("TYPESAFE_API_KEY is not set".into()))?;
        let mut config = Self::new(api_key);
        if let Some(base_url) = var("TYPESAFE_BASE_URL") {
            config.base_url = base_url;
        }
        if let Some(model) = var("TYPESAFE_MODEL") {
            config.model = model;
        }
        Ok(config)
    }
}

impl fmt::Debug for TypeSafeConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypeSafeConfig")
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// A [`DecisionModel`] backed by a System One endpoint.
#[derive(Debug, Clone)]
pub struct TypeSafeDecision {
    config: TypeSafeConfig,
    client: reqwest::Client,
}

impl TypeSafeDecision {
    pub fn new(config: TypeSafeConfig) -> Result<Self, HarnessError> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .build()
            .map_err(|e| HarnessError::Provider(format!("typesafe client: {e}")))?;
        Ok(Self { config, client })
    }

    fn url(&self) -> String {
        format!(
            "{}/v1/systemone",
            self.config.base_url.trim_end_matches('/')
        )
    }
}

impl DecisionModel for TypeSafeDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        Box::pin(async move {
            let body = to_wire(&req, &self.config.model);
            let started = Instant::now();
            let resp = self
                .client
                .post(self.url())
                .bearer_auth(&self.config.api_key)
                .json(&body)
                .send()
                .await
                .map_err(|e| HarnessError::Provider(format!("typesafe request: {e}")))?;
            let status = resp.status();
            let text = resp
                .text()
                .await
                .map_err(|e| HarnessError::Provider(format!("typesafe response: {e}")))?;
            let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
            if !status.is_success() {
                let snippet: String = text.chars().take(ERROR_BODY_LIMIT).collect();
                return Err(HarnessError::Provider(format!(
                    "typesafe returned {status}: {snippet}"
                )));
            }
            let wire: WireResponse = serde_json::from_str(&text)
                .map_err(|e| HarnessError::Provider(format!("typesafe response json: {e}")))?;
            from_wire(wire, &req.questions, latency_ms)
        })
    }
}

/// The request body: model, state slice, and questions. Nothing else.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WireRequest {
    pub model: String,
    pub state: State,
    pub questions: BTreeMap<String, WireQuestion>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireQuestion {
    Choice {
        instructions: String,
        criteria: BTreeMap<String, String>,
    },
    Score {
        instructions: String,
        criteria: Vec<String>,
    },
    Noul {
        instructions: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireResponse {
    #[serde(default)]
    pub model: Option<String>,
    pub answers: BTreeMap<String, WireAnswer>,
    #[serde(default)]
    pub usage: Option<WireUsage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WireAnswer {
    Choice {
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Noul {
        noul: f64,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct WireUsage {
    #[serde(default)]
    pub input_tokens: Option<u32>,
    #[serde(default)]
    pub output_tokens: Option<u32>,
}

pub fn to_wire(req: &DecisionRequest, model: &str) -> WireRequest {
    let questions = req
        .questions
        .iter()
        .map(|(key, q)| {
            let wire = match q.clone() {
                Question::Choice {
                    instructions,
                    criteria,
                } => WireQuestion::Choice {
                    instructions,
                    criteria,
                },
                Question::Score {
                    instructions,
                    criteria,
                } => WireQuestion::Score {
                    instructions,
                    criteria,
                },
                Question::Noul {
                    instructions,
                    criteria,
                } => WireQuestion::Noul {
                    instructions,
                    criteria,
                },
            };
            (key.clone(), wire)
        })
        .collect();
    WireRequest {
        model: model.into(),
        state: req.state.clone(),
        questions,
    }
}

/// Maps a response onto the asked questions. Noul answers become
/// `{"true": p, "false": 1 - p}` with confidence `2 * |p - 0.5|`.
pub fn from_wire(
    resp: WireResponse,
    questions: &BTreeMap<String, Question>,
    latency_ms: u64,
) -> Result<DecisionResponse, HarnessError> {
    let mut wire_answers = resp.answers;
    let mut answers = BTreeMap::new();
    for (key, question) in questions {
        let wire = wire_answers
            .remove(key)
            .ok_or_else(|| HarnessError::Provider(format!("typesafe: no answer for `{key}`")))?;
        let answer = match (question, wire) {
            (
                Question::Choice { .. },
                WireAnswer::Choice {
                    probabilities,
                    confidence,
                },
            )
            | (
                Question::Score { .. },
                WireAnswer::Score {
                    probabilities,
                    confidence,
                },
            ) => Answer {
                probabilities,
                confidence,
            },
            (Question::Noul { .. }, WireAnswer::Noul { noul }) => {
                if !(0.0..=1.0).contains(&noul) {
                    return Err(HarnessError::Provider(format!(
                        "typesafe: noul for `{key}` out of range: {noul}"
                    )));
                }
                Answer {
                    probabilities: BTreeMap::from([
                        ("true".into(), noul),
                        ("false".into(), 1.0 - noul),
                    ]),
                    confidence: 2.0 * (noul - 0.5).abs(),
                }
            }
            _ => {
                return Err(HarnessError::Provider(format!(
                    "typesafe: answer type for `{key}` does not match its question"
                )));
            }
        };
        answers.insert(key.clone(), answer);
    }
    if let Some(extra) = wire_answers.keys().next() {
        return Err(HarnessError::Provider(format!(
            "typesafe: answer for unasked question `{extra}`"
        )));
    }
    let usage = resp.usage;
    Ok(DecisionResponse {
        answers,
        usage: Usage {
            latency_ms,
            input_tokens: usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: usage.as_ref().and_then(|u| u.output_tokens),
            cost_usd: None,
        },
    })
}
