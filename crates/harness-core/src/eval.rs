use std::collections::BTreeMap;
use std::fmt;

use serde::Deserialize;

use crate::decision::{Answer, DecisionModel, DecisionRequest, DecisionResponse, Question};
use crate::error::HarnessError;
use crate::event::Usage;
use crate::future::BoxFuture;
use crate::machine::{Machine, Run, RunStatus};
use crate::node::{Node, NodeId};
use crate::policy::{ConfidenceAction, ConfidencePolicy};
use crate::state::State;
use crate::workflow::{Next, Outcome, Workflow};

/// A fixture did not parse or did not match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixtureError {
    pub message: String,
}

impl FixtureError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FixtureError {}

#[derive(Debug, Deserialize)]
struct AnswerFixture {
    question: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
    expect: String,
    min_confidence: f64,
    max_confidence: f64,
}

#[derive(Debug, Deserialize)]
struct BranchFixture {
    question: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
    policy: ConfidencePolicy,
    expect: ConfidenceAction,
}

/// Checks that the highest-probability label is `expect` and that `confidence`
/// lies in the inclusive range. Does not call a model.
pub fn check_answer_fixture(json: &str) -> Result<(), FixtureError> {
    let fixture: AnswerFixture = parse(json)?;
    if !fixture.min_confidence.is_finite()
        || !fixture.max_confidence.is_finite()
        || fixture.min_confidence > fixture.max_confidence
    {
        return Err(FixtureError::new(format!(
            "question `{}` has an empty confidence range {} to {}",
            fixture.question, fixture.min_confidence, fixture.max_confidence
        )));
    }
    let answer = Answer {
        probabilities: fixture.probabilities,
        confidence: fixture.confidence,
    };
    let chosen = answer.top().ok_or_else(|| {
        FixtureError::new(format!(
            "question `{}` has no chosen answer",
            fixture.question
        ))
    })?;
    if chosen != fixture.expect {
        return Err(FixtureError::new(format!(
            "question `{}` chose `{chosen}`, expected `{}`",
            fixture.question, fixture.expect
        )));
    }
    if !answer.confidence.is_finite()
        || answer.confidence < fixture.min_confidence
        || answer.confidence > fixture.max_confidence
    {
        return Err(FixtureError::new(format!(
            "question `{}` confidence {} is outside {} to {}",
            fixture.question, answer.confidence, fixture.min_confidence, fixture.max_confidence
        )));
    }
    Ok(())
}

/// Runs the recorded answer through the machine and checks the branch it takes.
/// `Continue` is the transition to `next`, including a low score whose policy
/// says `Continue`. `SafeDefault` and `Escalate` are the low-confidence handlers.
pub fn check_branch_fixture(json: &str) -> Result<(), FixtureError> {
    let fixture: BranchFixture = parse(json)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|err| FixtureError::new(format!("runtime: {err}")))?;
    let taken = runtime.block_on(branch_taken(&fixture))?;
    if taken != fixture.expect {
        return Err(FixtureError::new(format!(
            "question `{}` took {taken:?}, expected {:?}",
            fixture.question, fixture.expect
        )));
    }
    Ok(())
}

fn parse<'a, T: Deserialize<'a>>(json: &'a str) -> Result<T, FixtureError> {
    serde_json::from_str(json).map_err(|err| FixtureError::new(format!("fixture json: {err}")))
}

async fn branch_taken(fixture: &BranchFixture) -> Result<ConfidenceAction, FixtureError> {
    let workflow = branch_workflow(fixture)?;
    let model = FixedDecision {
        question: fixture.question.clone(),
        probabilities: fixture.probabilities.clone(),
        confidence: fixture.confidence,
    };
    let run = Machine::builder()
        .decision(model)
        .build()
        .run(&workflow, State::new())
        .await
        .map_err(|err| FixtureError::new(err.to_string()))?;
    taken(&run)
}

fn branch_workflow(fixture: &BranchFixture) -> Result<Workflow, FixtureError> {
    let step = |id: &str| Node::Deterministic {
        id: id.into(),
        step: noop,
    };
    let nodes = vec![
        Node::Decide {
            id: "decide".into(),
            reads: vec![],
            questions: BTreeMap::from([(fixture.question.clone(), Question::noul("fixture"))]),
            policy: fixture.policy,
        },
        step("next"),
        step("safe"),
        step("escalate"),
    ];
    Workflow::new("decide".into(), nodes, to_next)
        .and_then(|workflow| workflow.on_low(ConfidenceAction::SafeDefault, "safe".into()))
        .and_then(|workflow| workflow.on_low(ConfidenceAction::Escalate, "escalate".into()))
        .map_err(|err| FixtureError::new(err.to_string()))
}

fn noop(_state: &mut State) -> Result<(), HarnessError> {
    Ok(())
}

fn to_next(node: &NodeId, _outcome: &Outcome, _state: &State) -> Next {
    if node.0 == "decide" {
        Next::Goto("next".into())
    } else {
        Next::End
    }
}

fn taken(run: &Run) -> Result<ConfidenceAction, FixtureError> {
    if let RunStatus::Halted(action) = run.status {
        return Ok(action);
    }
    match run.path.get(1).map(|id| id.0.as_str()) {
        Some("next") => Ok(ConfidenceAction::Continue),
        Some("safe") => Ok(ConfidenceAction::SafeDefault),
        Some("escalate") => Ok(ConfidenceAction::Escalate),
        Some(other) => Err(FixtureError::new(format!(
            "branch left the decide node for `{other}`"
        ))),
        None => Err(FixtureError::new(
            "run ended on the decide node without taking a branch",
        )),
    }
}

struct FixedDecision {
    question: String,
    probabilities: BTreeMap<String, f64>,
    confidence: f64,
}

impl DecisionModel for FixedDecision {
    fn evaluate(
        &self,
        _req: DecisionRequest,
    ) -> BoxFuture<'_, Result<DecisionResponse, HarnessError>> {
        let question = self.question.clone();
        let probabilities = self.probabilities.clone();
        let confidence = self.confidence;
        Box::pin(async move {
            Ok(DecisionResponse {
                answers: BTreeMap::from([(
                    question,
                    Answer {
                        probabilities,
                        confidence,
                    },
                )]),
                usage: Usage::default(),
            })
        })
    }
}
