//! Safety-gated generation.
//!
//! A System One yes/no gate decides whether a reply may be drafted. Rust
//! then runs one schema-bound generative call, or takes the safe default.
//! The generative client is selected outside [`workflow`], so swapping
//! OpenAI-compatible and Anthropic does not change the graph.

use std::collections::BTreeMap;

use harness_core::{
    Answer, BoxFuture, ConfidenceAction, ConfidencePolicy, DecisionModel, DecisionRequest,
    DecisionResponse, EventKind, Gate, GenOutput, GenRequest, GenResponse, GenerativeModel,
    HarnessError, JsonSchema, Machine, Next, Node, NodeId, Outcome, OutputSpec, Question, Run,
    State, Usage, Workflow,
};
use harness_providers::{
    Anthropic, AnthropicConfig, OpenAiCompatible, OpenAiCompatibleConfig, TypeSafeConfig,
    TypeSafeDecision,
};
use serde::Deserialize;
use serde_json::{Value, json};

/// Which live generative client to use. The workflow graph stays the same.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveClient {
    OpenAi,
    Anthropic,
}

/// Checked-in request, recorded gate answer, and recorded draft.
#[derive(Debug, Clone, Deserialize)]
pub struct Fixture {
    pub request: String,
    pub answer: Answer,
    pub decision_usage: Usage,
    pub output: Value,
    pub generation_usage: Usage,
}

/// Parses a generation fixture. A bad document is a workflow error.
pub fn load_fixture(json: &str) -> Result<Fixture, HarnessError> {
    serde_json::from_str(json)
        .map_err(|err| HarnessError::Workflow(format!("generation fixture: {err}")))
}

/// The gated graph. It does not name a model.
pub fn workflow() -> Result<Workflow, HarnessError> {
    Workflow::new(
        "normalize".into(),
        vec![
            Node::Deterministic {
                id: "normalize".into(),
                step: normalize,
            },
            Node::Decide {
                id: "gate".into(),
                reads: vec!["request".into()],
                questions: BTreeMap::from([(
                    "allow".into(),
                    Question::noul("Is it safe to draft a reply to this request?"),
                )]),
                policy: ConfidencePolicy {
                    min_confidence: 0.7,
                    on_low: ConfidenceAction::SafeDefault,
                },
            },
            Node::Generate {
                id: "draft".into(),
                task: "Draft a short reply to this support request.".into(),
                reads: vec!["request".into()],
                output: reply_schema(),
                max_tokens: 128,
                gate: Some(Gate {
                    decide: "gate".into(),
                    question: "allow".into(),
                    allow: "true".into(),
                }),
            },
            Node::Deterministic {
                id: "accept".into(),
                step: to_drafted,
            },
            Node::Deterministic {
                id: "safe".into(),
                step: to_safe,
            },
        ],
        transition,
    )?
    .on_low(ConfidenceAction::SafeDefault, "safe".into())
}

/// Runs `fixture`. `live` calls TypeSafe for the gate and the named generative
/// client, and ignores the recorded answer and draft.
pub async fn run(fixture: &Fixture, live: Option<LiveClient>) -> Result<Run, HarnessError> {
    let wf = workflow()?;
    let mut state = State::new();
    state.insert("request".into(), json!(fixture.request));
    let machine = match live {
        None => Machine::builder()
            .decision(FixtureDecision {
                answer: fixture.answer.clone(),
                usage: fixture.decision_usage.clone(),
            })
            .generative(FixtureGenerate {
                output: fixture.output.clone(),
                usage: fixture.generation_usage.clone(),
            })
            .build(),
        Some(LiveClient::OpenAi) => Machine::builder()
            .decision(TypeSafeDecision::new(TypeSafeConfig::from_env()?)?)
            .generative(OpenAiCompatible::new(OpenAiCompatibleConfig::from_env(
                "OPENAI",
            )?)?)
            .build(),
        Some(LiveClient::Anthropic) => Machine::builder()
            .decision(TypeSafeDecision::new(TypeSafeConfig::from_env()?)?)
            .generative(Anthropic::new(AnthropicConfig::from_env("ANTHROPIC")?)?)
            .build(),
    };
    machine.run(&wf, state).await
}

/// Destination, gate label, and the recorded cost and latency of each call.
pub fn report(run: &Run) -> String {
    let destination = run
        .state
        .get("destination")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let Some(decide) = run
        .events
        .iter()
        .find(|event| event.kind == EventKind::Decide)
    else {
        return format!("destination: {destination}");
    };
    let allowed = serde_json::from_value::<BTreeMap<String, Answer>>(decide.output.clone())
        .ok()
        .and_then(|answers| {
            answers
                .get("allow")
                .and_then(Answer::top)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let decision_cost = money(decide.usage.cost_usd);
    let mut lines = vec![
        format!("destination: {destination}"),
        format!("allowed: {allowed}"),
        format!("confidence: {}", decide.confidence.unwrap_or(0.0)),
        format!("decision_latency_ms: {}", decide.usage.latency_ms),
        format!("decision_cost_usd: {decision_cost}"),
    ];
    if let Some(generate) = run
        .events
        .iter()
        .find(|event| event.kind == EventKind::Generate)
    {
        lines.push(format!(
            "generation_latency_ms: {}",
            generate.usage.latency_ms
        ));
        lines.push(format!(
            "generation_cost_usd: {}",
            money(generate.usage.cost_usd)
        ));
        if let Some(reply) = generate.output.get("reply").and_then(Value::as_str) {
            lines.push(format!("reply: {reply}"));
        }
    }
    lines.join("\n")
}

fn reply_schema() -> OutputSpec {
    OutputSpec::Schema(JsonSchema(json!({
        "type": "object",
        "required": ["reply"],
        "properties": { "reply": { "type": "string" } }
    })))
}

fn normalize(state: &mut State) -> Result<(), HarnessError> {
    let request = state
        .get("request")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_owned();
    state.insert("request".into(), json!(request));
    Ok(())
}

fn to_drafted(state: &mut State) -> Result<(), HarnessError> {
    state.insert("destination".into(), json!("drafted"));
    Ok(())
}

fn to_safe(state: &mut State) -> Result<(), HarnessError> {
    state.insert("destination".into(), json!("safe"));
    Ok(())
}

/// A confident gate always continues to `draft`. The gate on that node skips
/// the model unless the top label is `true`. Low confidence never reaches
/// this match: the machine sends it to `safe`.
fn transition(node: &NodeId, outcome: &Outcome, _: &State) -> Next {
    match (node.0.as_str(), outcome) {
        ("normalize", _) => Next::Goto("gate".into()),
        ("gate", Outcome::Decided(_)) => Next::Goto("draft".into()),
        ("draft", Outcome::Generated(_)) => Next::Goto("accept".into()),
        ("draft", Outcome::Skipped) => Next::Goto("safe".into()),
        _ => Next::End,
    }
}

fn money(cost: Option<f64>) -> String {
    cost.map(|cost| cost.to_string())
        .unwrap_or_else(|| "none".to_owned())
}

struct FixtureDecision {
    answer: Answer,
    usage: Usage,
}

impl DecisionModel for FixtureDecision {
    fn evaluate(
        &self,
        req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let answer = self.answer.clone();
        let usage = self.usage.clone();
        Box::pin(async move {
            let answers = req
                .questions
                .into_keys()
                .map(|key| (key, answer.clone()))
                .collect();
            Ok(DecisionResponse { answers, usage })
        })
    }
}

struct FixtureGenerate {
    output: Value,
    usage: Usage,
}

impl GenerativeModel for FixtureGenerate {
    fn complete(&self, _req: GenRequest) -> BoxFuture<'_, Result<GenResponse, HarnessError>> {
        let output = GenOutput::Json(self.output.clone());
        let usage = self.usage.clone();
        Box::pin(async move { Ok(GenResponse { output, usage }) })
    }
}
